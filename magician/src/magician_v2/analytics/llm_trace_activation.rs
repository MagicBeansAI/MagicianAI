//! Runtime activation bridge for the canonical LLM fact pipeline.
//!
//! Production call sites publish one scoped `LLMResponseReceived` event at
//! their response boundary. Structured callers additionally label explicit
//! validation success/failure; transport-only callers leave validation
//! unattempted rather than having success inferred. This bridge is the
//! compatibility adapter that turns that event into journal-backed immutable
//! call and final-provider-attempt revisions. Earlier physical attempts are
//! never invented: when the receipt says they occurred but their lifecycle is
//! unavailable, an exact capture-gap count is persisted beside the facts.
//!
//! The legacy `llm_calls` sink remains a compatibility mirror during the
//! migration window. Scoped `llm_dispatch` rows remain the auxiliary source
//! for queue/local-prep timing and are joined to canonical facts only by stable
//! dispatch identity. Canonical call/attempt identity, usage and economics use
//! the durable journal/materializer datasets activated here.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use magicllm::{
    dispatch::{
        ErrorClass as DispatchErrorClass, JobMeta, LlmQueueEvent, LocalPrepCallStat,
        TombstoneReason,
    },
    trace::normalized_activity_id,
    LlmCallRole, LlmParentRelation, LlmScope, LlmScopeResolution, LlmTraceContext,
    LlmWorkloadClass,
};
use serde::Serialize;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::{
    llm_pricing_identity::provider_kind_for_pricing,
    llm_tool_lineage::LLM_TOOL_LINEAGE_EVENT_TYPE,
    llm_trace_journal::{
        LlmTraceDurablePipeline, LlmTraceJournalConfig, LlmTraceJournalError,
        LlmTraceShutdownReport,
    },
    llm_trace_materializer::build_canonical_llm_trace_pipeline,
    llm_trace_recorder::{
        is_content_free_machine_category, LlmAttemptTerminalState, LlmCallCompleted,
        LlmCallStarted, LlmCallTerminalState, LlmCaptureFact, LlmCaptureGap, LlmCostSource,
        LlmImmediateValidationFact, LlmPricingFact, LlmProviderAttemptEvent,
        LlmProviderAttemptPhase, LlmTimingFact, LlmTokenUsageFact, LlmToolLineageRecord,
        LlmTraceRecord, LlmTraceRecorder, LLM_HARNESS_AGGREGATE_RESPONSE_KIND,
        LLM_LOGICAL_CHUNK_SUMMARY_RESPONSE_KIND, LLM_TRACE_FACT_SCHEMA_VERSION,
    },
};
use crate::magician_v2::{
    artifact_v2::workspace::{
        ArtifactV2Workspace, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
    },
    realtime_events::{
        AgentEventEnvelope, LlmEventCorrelation, RuntimeTransportBroadcaster, RuntimeTransportEvent,
    },
};

const UNPRICED_VERSION: &str = "runtime-pricing-miss@call-time";
const INVALID_PRICING_VERSION: &str = "runtime-pricing-invalid-value@call-time";
const USAGE_UNREPORTED_VERSION: &str = "provider-usage-unreported@call-time";
const LOGICAL_CHUNK_SUMMARY_VERSION: &str = "logical-chunk-summary-non-billing-v1";
const BROADCAST_LAGGED: &str = "runtime_transport_events_unclassified_due_broadcast_lag";
const DISPATCH_BROADCAST_LAGGED: &str = "llm_dispatch_events_unclassified_due_broadcast_lag";
const MAPPING_REJECTED: &str = "runtime_event_mapping_rejected";
const DISPATCH_MAPPING_REJECTED: &str = "llm_dispatch_mapping_rejected";
const EARLIER_ATTEMPTS_UNAVAILABLE: &str = "earlier_provider_attempt_lifecycle_unavailable";
const TERMINAL_ATTEMPTS_UNAVAILABLE: &str = "terminal_provider_attempt_lifecycle_unavailable";
const INVALID_START: &str = "missing_or_invalid_start_timestamp";
const INVALID_TTFT: &str = "invalid_ttft_exceeds_total_latency";
const INVALID_COST: &str = "invalid_cost_omitted";
const INVALID_USAGE: &str = "invalid_provider_usage_omitted";
const SUPPLIED_COST_MISMATCH: &str = "producer_cost_mismatch_recomputed";
const USAGE_UNREPORTED: &str = "provider_usage_unreported";

#[derive(Debug, Clone)]
pub struct LlmTraceActivationConfig {
    pub journal: LlmTraceJournalConfig,
}

impl Default for LlmTraceActivationConfig {
    fn default() -> Self {
        Self {
            journal: LlmTraceJournalConfig::default(),
        }
    }
}

#[derive(Debug, Default)]
struct ActivationMetrics {
    response_events_seen: AtomicU64,
    tool_lineage_events_seen: AtomicU64,
    reused_responses_ignored: AtomicU64,
    external_aggregate_events_ignored: AtomicU64,
    records_accepted: AtomicU64,
    records_rejected: AtomicU64,
    gap_records_emitted: AtomicU64,
    broadcast_events_lost: AtomicU64,
    dispatch_terminal_events_seen: AtomicU64,
    dispatch_events_lost: AtomicU64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LlmTraceActivationStats {
    pub response_events_seen: u64,
    pub tool_lineage_events_seen: u64,
    pub reused_responses_ignored: u64,
    pub external_aggregate_events_ignored: u64,
    pub records_accepted: u64,
    pub records_rejected: u64,
    pub gap_records_emitted: u64,
    pub broadcast_events_lost: u64,
    pub dispatch_terminal_events_seen: u64,
    pub dispatch_events_lost: u64,
}

impl ActivationMetrics {
    fn snapshot(&self) -> LlmTraceActivationStats {
        LlmTraceActivationStats {
            response_events_seen: self.response_events_seen.load(Ordering::Relaxed),
            tool_lineage_events_seen: self.tool_lineage_events_seen.load(Ordering::Relaxed),
            reused_responses_ignored: self.reused_responses_ignored.load(Ordering::Relaxed),
            external_aggregate_events_ignored: self
                .external_aggregate_events_ignored
                .load(Ordering::Relaxed),
            records_accepted: self.records_accepted.load(Ordering::Relaxed),
            records_rejected: self.records_rejected.load(Ordering::Relaxed),
            gap_records_emitted: self.gap_records_emitted.load(Ordering::Relaxed),
            broadcast_events_lost: self.broadcast_events_lost.load(Ordering::Relaxed),
            dispatch_terminal_events_seen: self
                .dispatch_terminal_events_seen
                .load(Ordering::Relaxed),
            dispatch_events_lost: self.dispatch_events_lost.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LlmTraceActivationShutdownReport {
    pub activation: LlmTraceActivationStats,
    pub durable_pipeline: LlmTraceShutdownReport,
}

/// Process-owned canonical capture runtime. It subscribes before returning,
/// drains its receiver first on shutdown, then asks the durable pipeline to
/// journal/materialize every accepted record within its bounded deadline.
pub struct LlmTraceActivation {
    cancel: CancellationToken,
    response_bridge: tokio::task::JoinHandle<()>,
    dispatch_bridges: Vec<tokio::task::JoinHandle<()>>,
    recorder: Arc<dyn LlmTraceRecorder>,
    pipeline: LlmTraceDurablePipeline,
    metrics: Arc<ActivationMetrics>,
    /// Retained so dispatch bridges attached after `start` can publish the
    /// activity-cost view of a completed call. `start` only borrowed it to
    /// subscribe; the bridges need it for the life of the activation.
    broadcaster: Arc<RuntimeTransportBroadcaster>,
}

impl LlmTraceActivation {
    pub fn start(
        broadcaster: Arc<RuntimeTransportBroadcaster>,
        workspace: ArtifactV2Workspace,
        config: LlmTraceActivationConfig,
    ) -> Result<Self, LlmTraceJournalError> {
        // Subscribe before the synchronous recovery barrier. Recovery can scan
        // and materialize an existing durable prefix; events emitted by an
        // already-alive producer during that interval must wait in this
        // receiver rather than falling into a boot-time blind spot.
        let mut receiver = broadcaster.subscribe();
        let recovery_scopes = discover_recovery_scopes(&workspace)?;
        let pipeline =
            build_canonical_llm_trace_pipeline(workspace, config.journal, recovery_scopes.clone())?;
        let recorder: Arc<dyn LlmTraceRecorder> = pipeline.recorder();
        let cancel = CancellationToken::new();
        let bridge_cancel = cancel.clone();
        let metrics = Arc::new(ActivationMetrics::default());
        let bridge_metrics = Arc::clone(&metrics);
        // The response bridge is where SUCCESSFUL calls are mapped, so it is
        // the bridge that has to be able to publish a cost. It was previously
        // given no broadcaster at all, which is why the cost event could never
        // fire for the ordinary case.
        let bridge_broadcaster = Arc::clone(&broadcaster);
        let response_bridge = tokio::spawn(async move {
            run_activation_bridge(
                &mut receiver,
                recorder,
                bridge_cancel,
                bridge_metrics,
                bridge_broadcaster,
            )
            .await;
        });

        tracing::info!(
            target: "analytics::llm_trace_activation",
            recovered_scopes = recovery_scopes.len(),
            "canonical LLM trace journal and materializer activated"
        );
        Ok(Self {
            cancel,
            response_bridge,
            dispatch_bridges: Vec::new(),
            recorder: pipeline.recorder(),
            pipeline,
            metrics,
            broadcaster,
        })
    }

    /// Attach the exact terminal stream from a dispatch queue. Failed and
    /// tombstoned jobs have no successful response receipt, so this bridge is
    /// the authoritative source of their logical-call terminal fact. Completed
    /// jobs remain owned by the richer response event to avoid dual writers.
    pub fn attach_dispatch_events(&mut self, mut receiver: broadcast::Receiver<LlmQueueEvent>) {
        let recorder = Arc::clone(&self.recorder);
        let cancel = self.cancel.clone();
        let metrics = Arc::clone(&self.metrics);
        let broadcaster = Arc::clone(&self.broadcaster);
        self.dispatch_bridges.push(tokio::spawn(async move {
            run_dispatch_bridge(&mut receiver, recorder, cancel, metrics, broadcaster).await;
        }));
    }

    pub fn stats(&self) -> LlmTraceActivationStats {
        self.metrics.snapshot()
    }

    /// Canonical fact recorder. Phase 3 uses it only for content-free capture
    /// gaps; sanitized payloads have a separate restricted replay journal.
    pub fn recorder(&self) -> Arc<dyn LlmTraceRecorder> {
        Arc::clone(&self.recorder)
    }

    pub async fn shutdown(self) -> LlmTraceActivationShutdownReport {
        self.cancel.cancel();
        if let Err(error) = self.response_bridge.await {
            tracing::warn!(
                target: "analytics::llm_trace_activation",
                error = %error,
                "canonical LLM event bridge failed while shutting down"
            );
        }
        for bridge in self.dispatch_bridges {
            if let Err(error) = bridge.await {
                tracing::warn!(
                    target: "analytics::llm_trace_activation",
                    error = %error,
                    "canonical LLM dispatch bridge failed while shutting down"
                );
            }
        }
        let activation = self.metrics.snapshot();
        let durable_pipeline = self.pipeline.shutdown().await;
        LlmTraceActivationShutdownReport {
            activation,
            durable_pipeline,
        }
    }
}

async fn run_dispatch_bridge(
    receiver: &mut broadcast::Receiver<LlmQueueEvent>,
    recorder: Arc<dyn LlmTraceRecorder>,
    cancel: CancellationToken,
    metrics: Arc<ActivationMetrics>,
    broadcaster: Arc<RuntimeTransportBroadcaster>,
) {
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                loop {
                    match receiver.try_recv() {
                        Ok(event) => record_dispatch_event(event, recorder.as_ref(), metrics.as_ref(), broadcaster.as_ref()),
                        Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                            record_dispatch_lag(skipped, recorder.as_ref(), metrics.as_ref());
                        },
                        Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => break,
                    }
                }
                break;
            }
            event = receiver.recv() => match event {
                Ok(event) => record_dispatch_event(event, recorder.as_ref(), metrics.as_ref(), broadcaster.as_ref()),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    record_dispatch_lag(skipped, recorder.as_ref(), metrics.as_ref());
                },
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }
}

fn record_dispatch_lag(skipped: u64, recorder: &dyn LlmTraceRecorder, metrics: &ActivationMetrics) {
    metrics
        .dispatch_events_lost
        .fetch_add(skipped, Ordering::Relaxed);
    let occurred_at_ms = chrono::Utc::now().timestamp_millis();
    submit_mapped_record(
        recorder,
        LlmTraceRecord::CaptureGap(capture_gap(
            LlmScope::new(DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE),
            "unknown".to_string(),
            DISPATCH_BROADCAST_LAGGED,
            skipped,
            occurred_at_ms,
            &ulid::Ulid::new().to_string(),
        )),
        metrics,
    );
}

fn record_dispatch_event(
    event: LlmQueueEvent,
    recorder: &dyn LlmTraceRecorder,
    metrics: &ActivationMetrics,
    broadcaster: &RuntimeTransportBroadcaster,
) {
    let (meta, terminal) = match event {
        LlmQueueEvent::Completed { meta } => (Some(meta), None),
        LlmQueueEvent::Failed { meta, error_class } => (
            Some(meta),
            Some((LlmCallTerminalState::Failed, Some(error_class))),
        ),
        LlmQueueEvent::Tombstoned { meta, reason } => (
            Some(meta),
            Some((
                LlmCallTerminalState::Tombstoned,
                tombstone_error_class(&reason),
            )),
        ),
        _ => (None, None),
    };
    let Some(meta) = meta else {
        return;
    };
    if let Some(local_prep) = meta.local_prep.as_ref() {
        for call in &local_prep.calls {
            match map_local_prep_call(call) {
                // Through the funnel as well. `map_local_prep_call` is the
                // only mapper that produces `cost_source: Local`, so if this
                // path does not publish, the `local` commodity can never
                // appear in the view and free-on-this-machine work is
                // indistinguishable from unpriced work.
                Ok(records) => submit_mapped_records(records, recorder, metrics, broadcaster),
                Err(reason) => {
                    submit_mapped_record(
                        recorder,
                        LlmTraceRecord::CaptureGap(capture_gap(
                            valid_or_diagnostic_scope(&call.trace_context.scope),
                            "local_prep_summarise".to_string(),
                            &format!(
                                "{DISPATCH_MAPPING_REJECTED}:local_prep_{}",
                                mapping_reason_code(&reason)
                            ),
                            1,
                            call.completed_at_ms.max(0),
                            &call.trace_context.llm_call_id,
                        )),
                        metrics,
                    );
                },
            }
        }
    }
    let Some((terminal_state, error_class)) = terminal else {
        return;
    };
    metrics
        .dispatch_terminal_events_seen
        .fetch_add(1, Ordering::Relaxed);
    match map_terminal_dispatch(&meta, terminal_state, error_class) {
        Ok(records) => submit_mapped_records(records, recorder, metrics, broadcaster),
        Err(reason) => submit_dispatch_mapping_gap(&meta, &reason, recorder, metrics),
    }
}

/// The one funnel that both persists a mapped record and publishes its cost.
///
/// **Every path that can produce a priced `CallCompleted` must call this.**
/// There are three, and for a long time only the least likely one did: the
/// queue's *terminal-failure* bridge. Failed and tombstoned jobs have no priced
/// response — `map_terminal_dispatch` says so with a default `LlmPricingFact` —
/// so the emit was reachable in principle and dead in practice, and the tests
/// that called `emit_activity_cost` with hand-built structs could not notice.
///
/// The three are: the response bridge (successful calls, the only path with a
/// provider price), the local-prep loop (the only path with
/// `LlmCostSource::Local`), and the terminal-dispatch bridge. Adding a fourth
/// producer of `CallCompleted` means routing it through here too.
fn submit_mapped_records(
    records: Vec<LlmTraceRecord>,
    recorder: &dyn LlmTraceRecorder,
    metrics: &ActivationMetrics,
    broadcaster: &RuntimeTransportBroadcaster,
) {
    for record in records {
        // The durable record is the source of truth and is submitted first.
        // The transport event is a view of it, emitted afterwards and
        // fire-and-forget: a broadcast with no subscribers is the normal case
        // (nobody has the activity view open), and it must never affect
        // whether the analytical row lands.
        //
        // The order was inverted here for a while — emit, then submit — which
        // is not what the paragraph above describes. It cannot be dismissed as
        // cosmetic: publishing first means a subscriber can be told a call cost
        // something before anything durable says it happened, so the live view
        // leads the record it is supposed to be a view *of*. Submitting first
        // makes the ordering match the claim.
        //
        // Cloning the completed record is what lets the durable submission take
        // ownership and still leave something to emit from. It happens only on
        // the `CallCompleted` variant — one per completed call, not per event.
        let completed = match &record {
            LlmTraceRecord::CallCompleted(completed) => Some(completed.clone()),
            _ => None,
        };
        submit_mapped_record(recorder, record, metrics);
        if let Some(completed) = completed {
            emit_activity_cost(&completed, broadcaster);
        }
    }
}

/// Publish what a completed call cost, for the live activity view.
///
/// Emitted from [`submit_mapped_records`] rather than from the mapping
/// functions that build `LlmCallCompleted`: those are pure, returning
/// `Result<Vec<LlmTraceRecord>, String>`, and giving one a side effect would
/// make every mapping test publish onto the bus.
///
/// **Do not test this function directly.** Its guards are simple and its
/// reachability is not: calling it with a hand-built `LlmCallCompleted` proves
/// the guards and proves nothing about whether anything calls it, which is the
/// exact way this event spent its whole life unreachable. The test that matters
/// drives a real response event through `record_runtime_event` — see
/// `a_successful_dispatch_publishes_a_cost_through_the_real_entry_point`.
///
/// # THIS SEND IS NOT JOURNALLED BY THE LOOP OUTBOX, AND THAT IS A REFUSAL
///
/// Recorded here on 2026-08-28 because
/// `execution::agentic::run_loop::phases::outbox`'s census of *phase-reachable
/// emission sites* is the gate on turning that outbox on, and this site was in
/// neither half of it — neither journalled nor written down as refused. It was
/// invisible to the sweep by construction: that sweep is a call-graph closure
/// over `executor.rs` and the modules the loop's phase files name, and **no
/// phase calls this function**.
///
/// A phase *causes* it. The `Decide` phase's provider call becomes a job on the
/// LLM queue; when that job terminates the queue broadcasts an `LlmQueueEvent`;
/// the task `LlmTraceActivation::start` spawned is subscribed to that broadcast,
/// maps the meta into records, and [`submit_mapped_records`] calls this. So by
/// the time this runs, the phase that caused it is on another thread and has
/// usually already committed.
///
/// **Refused rather than owed a diff**, for three reasons in increasing order of
/// how much they would cost to remove:
///
/// 1. **There is no address to record.** A journal record is keyed by
///    `(execution_id, iteration, phase, ordinal)`. Nothing on this path carries
///    an iteration or a phase: `LlmTraceContext` has an `execution_id` and a
///    `plan_id`, and the queue message has neither of the other two. Threading
///    them would mean widening the queue's own message type.
/// 2. **The phase is over.** Even with an address, the record would arrive after
///    the phase's boundary had committed, so it would either be appended to a
///    closed phase or re-stamped onto a later one — and a projector replaying it
///    would place a cost in the middle of a phase that did not incur it.
/// 3. **It is telemetry ABOUT a run, not an event OF one.** The outbox replays a
///    run's timeline so a crashed phase's events are not lost. A cost row is an
///    observation of the process that ran the phase; its absence from a replayed
///    timeline is not a hole in that timeline.
///
/// Nothing here is claiming the event is unimportant or that it never reaches a
/// user — it goes out on the bus exactly as before. What is claimed is that it
/// does not belong to the outbox's population, and that the census is now
/// complete about it either way.
fn emit_activity_cost(completed: &LlmCallCompleted, broadcaster: &RuntimeTransportBroadcaster) {
    // No activity means the call was issued outside any instrumented span.
    // There is nothing in the view to attach a cost to, so nothing is said.
    let Some(activity_id) = completed.context.activity_id.clone() else {
        return;
    };
    // An unpriced call emits NOTHING, never zero. Zero reads as "this was
    // free"; absent reads as "we do not know what this cost". Conflating them
    // would make a pricing outage look like a windfall.
    let Some(cost_usd) = completed.pricing.cost_usd else {
        return;
    };
    if !cost_usd.is_finite() || cost_usd < 0.0 {
        return;
    }

    // Local work is reported in its own commodity, never as zero dollars.
    // Both are "0" on screen, but they answer different questions: `usd 0`
    // says a vendor charged nothing this time, `local 0` says there was no
    // vendor. Read from `cost_source` rather than inferred from a zero cost —
    // inferring would relabel any genuinely free paid call as local.
    let local = completed.pricing.cost_source == Some(LlmCostSource::Local);
    broadcaster.emit_transport_only(RuntimeTransportEvent::ActivityCost {
        activity_id,
        cost_microunits: if local {
            0
        } else {
            (cost_usd * 1_000_000.0).round() as u64
        },
        commodity: if local { "local" } else { "usd" }.to_string(),
        input_tokens: completed.usage.input_tokens,
        output_tokens: completed.usage.output_tokens,
        principal: Some(completed.context.scope.principal.clone()),
        workspace: Some(completed.context.scope.workspace.clone()),
        dropped: 0,
        // Stable per logical call and unique across the family, so the
        // backfill/live overlap dedupes on it exactly like the layer's own
        // rows do.
        seq: completed.context.llm_call_id.clone(),
        timestamp: completed.observed_at_ms,
    });
}

fn submit_dispatch_mapping_gap(
    meta: &JobMeta,
    reason: &str,
    recorder: &dyn LlmTraceRecorder,
    metrics: &ActivationMetrics,
) {
    let supplied_operation =
        nonempty(&meta.origin.operation).unwrap_or_else(|| "unknown".to_string());
    let operation = if is_content_free_machine_category(&supplied_operation) {
        supplied_operation
    } else {
        "unknown".to_string()
    };
    let occurred_at_ms = meta
        .completed_at_ms
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis())
        .max(0);
    submit_mapped_record(
        recorder,
        LlmTraceRecord::CaptureGap(capture_gap(
            valid_or_diagnostic_scope(&meta.trace_context.scope),
            operation.clone(),
            &format!(
                "{DISPATCH_MAPPING_REJECTED}:{}",
                mapping_reason_code(reason)
            ),
            1,
            occurred_at_ms,
            &format!("{}:{reason}", meta.job_id),
        )),
        metrics,
    );
}

fn map_local_prep_call(call: &LocalPrepCallStat) -> Result<Vec<LlmTraceRecord>, String> {
    if !call.trace_context.is_valid() {
        return Err("invalid trace context".to_string());
    }
    if call.provider_attempt_count == 0
        || call.provider_attempt_id
            != call
                .trace_context
                .provider_attempt_id(call.provider_attempt_count)
    {
        return Err("invalid provider attempt identity".to_string());
    }
    if call.started_at_ms <= 0 || call.completed_at_ms < call.started_at_ms {
        return Err("invalid timestamps".to_string());
    }
    if call.provider.trim().is_empty() || call.model.trim().is_empty() {
        return Err("missing provider or model".to_string());
    }
    let operation = "local_prep_summarise".to_string();
    let capture = LlmCaptureFact::default();
    let mut started = LlmCallStarted::new(
        call.trace_context.clone(),
        operation.clone(),
        call.started_at_ms,
    );
    started.observed_at_ms = call.completed_at_ms;
    started.operation_family = Some("local_prep".to_string());
    started.capability = Some(call.purpose.clone());
    started.source_surface = Some(source_surface(call.trace_context.workload_class).to_string());
    started.capture = capture.clone();
    let timing = LlmTimingFact {
        created_at_ms: Some(call.started_at_ms),
        started_at_ms: Some(call.started_at_ms),
        completed_at_ms: Some(call.completed_at_ms),
        provider_execution_ms: Some(call.latency_ms),
        latency_ms: Some(call.latency_ms),
        ..LlmTimingFact::default()
    };
    let attempt_timing = terminal_attempt_timing(&timing, false, call.provider_attempt_count);
    let usage = call
        .tokens
        .as_ref()
        .map_or_else(LlmTokenUsageFact::default, |tokens| {
            let input_tokens = u64::from(tokens.prompt_tokens);
            let output_tokens = u64::from(tokens.completion_tokens);
            LlmTokenUsageFact {
                input_tokens: Some(input_tokens),
                output_tokens: Some(output_tokens),
                reasoning_tokens: Some(u64::from(tokens.reasoning_tokens)),
                cache_read_tokens: Some(u64::from(tokens.cached_tokens)),
                total_tokens: input_tokens.checked_add(output_tokens),
                ..LlmTokenUsageFact::default()
            }
        });
    let pricing = LlmPricingFact {
        pricing_version: Some("local-runtime-zero-cost-v1".to_string()),
        // `Local`, not `Computed`. Nothing was computed from a rate card here:
        // the call ran on local hardware and money was never the unit. The
        // zero below is authoritative rather than a fallback, and consumers
        // need to tell it from a vendor cost that happened to be zero.
        cost_source: Some(LlmCostSource::Local),
        cost_usd: Some(0.0),
        ..LlmPricingFact::default()
    };
    let cancelled = call.error_class.as_deref() == Some("cancelled");
    let attempt_terminal_state = if call.success {
        LlmAttemptTerminalState::Succeeded
    } else if cancelled {
        LlmAttemptTerminalState::Cancelled
    } else {
        LlmAttemptTerminalState::Failed
    };
    let call_terminal_state = if call.success {
        LlmCallTerminalState::Succeeded
    } else if cancelled {
        LlmCallTerminalState::Cancelled
    } else {
        LlmCallTerminalState::Failed
    };
    let attempt = LlmProviderAttemptEvent {
        schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
        context: call.trace_context.clone(),
        dispatch_job_id: None,
        provider_attempt_id: call.provider_attempt_id.clone(),
        provider_attempt_index: call.provider_attempt_count,
        phase: LlmProviderAttemptPhase::Completed,
        occurred_at_ms: call.completed_at_ms,
        observed_at_ms: call.completed_at_ms,
        operation: operation.clone(),
        effective_profile: None,
        provider: call.provider.clone(),
        model: call.model.clone(),
        model_revision: None,
        timing: attempt_timing,
        terminal_state: Some(attempt_terminal_state),
        error_class: call.error_class.clone(),
        error_code: None,
        finish_reason: None,
        refusal: None,
        truncated: None,
        usage: usage.clone(),
        pricing: pricing.clone(),
        capture: capture.clone(),
    };
    let completed = LlmCallCompleted {
        schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
        context: call.trace_context.clone(),
        dispatch_job_id: None,
        occurred_at_ms: call.completed_at_ms,
        observed_at_ms: call.completed_at_ms,
        operation,
        terminal_state: call_terminal_state,
        provider_attempt_count: call.provider_attempt_count,
        provider_response_id: None,
        response_kind: Some("text".to_string()),
        error_class: call.error_class.clone(),
        error_code: None,
        finish_reason: None,
        refusal: None,
        truncated: None,
        timing,
        usage,
        pricing,
        validation: LlmImmediateValidationFact {
            response_present: call.success,
            ..LlmImmediateValidationFact::default()
        },
        capture,
    };
    validate_mapped_records(vec![
        LlmTraceRecord::CallStarted(started),
        LlmTraceRecord::ProviderAttempt(attempt),
        LlmTraceRecord::CallCompleted(completed),
    ])
}

fn map_terminal_dispatch(
    meta: &JobMeta,
    terminal_state: LlmCallTerminalState,
    dispatch_error_class: Option<DispatchErrorClass>,
) -> Result<Vec<LlmTraceRecord>, String> {
    if !meta.trace_context.is_valid() {
        return Err("invalid trace context".to_string());
    }
    let operation =
        nonempty(&meta.origin.operation).ok_or_else(|| "missing operation".to_string())?;
    let completed_at_ms = meta
        .completed_at_ms
        .filter(|value| *value > 0)
        .ok_or_else(|| "missing completion timestamp".to_string())?;
    if meta.submitted_at_ms <= 0 || meta.submitted_at_ms > completed_at_ms {
        return Err("invalid submission timestamp".to_string());
    }
    if meta
        .dispatched_at_ms
        .is_some_and(|value| value < meta.submitted_at_ms || value > completed_at_ms)
    {
        return Err("invalid dispatch timestamp".to_string());
    }
    if meta.provider_attempt_count > 0 {
        let expected = meta
            .trace_context
            .provider_attempt_id(meta.provider_attempt_count);
        if meta.provider_attempt_id.as_deref() != Some(expected.as_str()) {
            return Err("missing or inconsistent final provider attempt identity".to_string());
        }
    } else if meta.provider_attempt_id.is_some() {
        return Err("provider attempt id exists with a zero attempt count".to_string());
    }

    let capture = LlmCaptureFact::default();
    let terminal_route_known = terminal_state == LlmCallTerminalState::Failed
        && meta.provider_attempt_count > 0
        && meta.provider.is_some()
        && meta.model.as_deref().and_then(nonempty).is_some();
    let mut started = LlmCallStarted::new(
        meta.trace_context.clone(),
        operation.clone(),
        meta.submitted_at_ms,
    );
    started.observed_at_ms = completed_at_ms;
    started.priority_lane = Some(meta.priority.as_str().to_string());
    started.source_surface = Some(source_surface(meta.trace_context.workload_class).to_string());
    started.origin_channel = meta.origin.caller.clone();
    if terminal_route_known {
        started.selected_profile = meta.profile.clone();
    } else {
        started.requested_profile = meta.profile.clone();
    }
    started.capture = capture.clone();

    let timing = LlmTimingFact {
        created_at_ms: Some(meta.submitted_at_ms),
        submitted_at_ms: Some(meta.submitted_at_ms),
        started_at_ms: meta.dispatched_at_ms,
        completed_at_ms: Some(completed_at_ms),
        queue_wait_ms: meta.wait_ms,
        local_prep_ms: meta.local_prep.as_ref().map(|value| value.duration_ms),
        provider_execution_ms: meta.execution_ms,
        latency_ms: u64::try_from(completed_at_ms.saturating_sub(meta.submitted_at_ms)).ok(),
        ..LlmTimingFact::default()
    };
    // Dispatch metadata aggregates provider execution across the whole job.
    // It cannot identify the final physical attempt after fallback/retry, so
    // keep the attempt's terminal identity and leave its timing unknown.
    let attempt_timing = terminal_attempt_timing(&timing, true, meta.provider_attempt_count);
    let usage = meta
        .tokens
        .as_ref()
        .map_or_else(LlmTokenUsageFact::default, |tokens| {
            let input_tokens = u64::from(tokens.prompt_tokens);
            let output_tokens = u64::from(tokens.completion_tokens);
            LlmTokenUsageFact {
                input_tokens: Some(input_tokens),
                output_tokens: Some(output_tokens),
                reasoning_tokens: Some(u64::from(tokens.reasoning_tokens)),
                cache_read_tokens: Some(u64::from(tokens.cached_tokens)),
                total_tokens: input_tokens.checked_add(output_tokens),
                ..LlmTokenUsageFact::default()
            }
        });
    let error_class = dispatch_error_class
        .map(|value| value.name().to_string())
        .or_else(|| {
            (terminal_state == LlmCallTerminalState::Tombstoned).then(|| "cancelled".to_string())
        });
    let completed = LlmCallCompleted {
        schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
        context: meta.trace_context.clone(),
        dispatch_job_id: Some(meta.job_id.to_string()),
        occurred_at_ms: completed_at_ms,
        observed_at_ms: completed_at_ms,
        operation: operation.clone(),
        terminal_state,
        provider_attempt_count: meta.provider_attempt_count,
        provider_response_id: None,
        response_kind: None,
        error_class: error_class.clone(),
        error_code: None,
        finish_reason: None,
        refusal: None,
        truncated: None,
        timing: timing.clone(),
        usage: usage.clone(),
        // Queue terminal metadata does not carry a priced response. Omitting
        // economics is truthful; a fake zero-cost/computed row is not.
        pricing: LlmPricingFact::default(),
        validation: LlmImmediateValidationFact::default(),
        capture: capture.clone(),
    };
    // Failed queue metadata now retains the concrete terminal route returned
    // by magicllm. Materialize that observed attempt; tombstones and older
    // metadata without a complete route remain explicit gaps.
    let mut records = vec![LlmTraceRecord::CallStarted(started)];
    if terminal_route_known {
        let attempt_terminal_state = match error_class.as_deref() {
            Some("timeout") | Some("worker_watchdog") => LlmAttemptTerminalState::TimedOut,
            Some("cancelled") => LlmAttemptTerminalState::Cancelled,
            _ => LlmAttemptTerminalState::Failed,
        };
        records.push(LlmTraceRecord::ProviderAttempt(LlmProviderAttemptEvent {
            schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
            context: meta.trace_context.clone(),
            dispatch_job_id: Some(meta.job_id.to_string()),
            provider_attempt_id: meta
                .provider_attempt_id
                .clone()
                .expect("attempt identity was validated above"),
            provider_attempt_index: meta.provider_attempt_count,
            phase: LlmProviderAttemptPhase::Completed,
            occurred_at_ms: completed_at_ms,
            observed_at_ms: completed_at_ms,
            operation: operation.clone(),
            effective_profile: meta.profile.clone(),
            provider: meta
                .provider
                .as_ref()
                .map(ToString::to_string)
                .expect("terminal route was checked above"),
            model: meta
                .model
                .clone()
                .expect("terminal route was checked above"),
            model_revision: None,
            timing: attempt_timing,
            terminal_state: Some(attempt_terminal_state),
            error_class: error_class.clone(),
            error_code: None,
            finish_reason: None,
            refusal: None,
            truncated: None,
            usage,
            pricing: LlmPricingFact::default(),
            capture: capture.clone(),
        }));
    }
    records.push(LlmTraceRecord::CallCompleted(completed));
    if terminal_route_known && meta.provider_attempt_count > 1 {
        records.push(LlmTraceRecord::CaptureGap(capture_gap_for_call(
            meta.trace_context.scope.clone(),
            operation.clone(),
            EARLIER_ATTEMPTS_UNAVAILABLE,
            u64::from(meta.provider_attempt_count - 1),
            completed_at_ms,
            &meta.trace_context.llm_call_id,
        )));
    } else if meta.provider_attempt_count > 0 && !terminal_route_known {
        records.push(LlmTraceRecord::CaptureGap(capture_gap_for_call(
            meta.trace_context.scope.clone(),
            operation,
            TERMINAL_ATTEMPTS_UNAVAILABLE,
            u64::from(meta.provider_attempt_count),
            completed_at_ms,
            &meta.trace_context.llm_call_id,
        )));
    }
    validate_mapped_records(records)
}

fn tombstone_error_class(reason: &TombstoneReason) -> Option<DispatchErrorClass> {
    Some(match reason {
        TombstoneReason::DeadlineExceeded => DispatchErrorClass::Timeout,
        TombstoneReason::QueueFull | TombstoneReason::ProcessRestart => DispatchErrorClass::Unknown,
        TombstoneReason::TaskCancelled { .. }
        | TombstoneReason::TaskCancelledInFlight { .. }
        | TombstoneReason::TaskMissing
        | TombstoneReason::ChatSessionEnded { .. }
        | TombstoneReason::ExplicitCancel { .. }
        | TombstoneReason::QueueShutdown => DispatchErrorClass::Cancelled,
    })
}

fn terminal_attempt_timing(
    call_timing: &LlmTimingFact,
    queued: bool,
    provider_attempt_count: u32,
) -> LlmTimingFact {
    if !queued && provider_attempt_count == 1 {
        return LlmTimingFact {
            started_at_ms: call_timing.started_at_ms,
            first_token_at_ms: call_timing.first_token_at_ms,
            completed_at_ms: call_timing.completed_at_ms,
            provider_execution_ms: call_timing.provider_execution_ms,
            ttft_ms: call_timing.ttft_ms,
            generation_after_ttft_ms: call_timing.generation_after_ttft_ms,
            latency_ms: call_timing.latency_ms,
            ..LlmTimingFact::default()
        };
    }
    LlmTimingFact {
        completed_at_ms: call_timing.completed_at_ms,
        ..LlmTimingFact::default()
    }
}

fn valid_or_diagnostic_scope(scope: &LlmScope) -> LlmScope {
    if scope.is_valid() {
        scope.clone()
    } else {
        LlmScope::new(DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE)
    }
}

fn discover_recovery_scopes(
    workspace: &ArtifactV2Workspace,
) -> Result<Vec<LlmScope>, LlmTraceJournalError> {
    let mut recovery = Vec::new();
    for (principal, workspace_name) in workspace.list_scope_segments_sync()? {
        let scope = LlmScope::new(principal, workspace_name);
        if !scope.is_valid() {
            return Err(LlmTraceJournalError::Sequence(format!(
                "invalid on-disk scope component during LLM journal recovery: {}/{}",
                scope.principal, scope.workspace
            )));
        }
        if workspace
            .analytics_llm_trace_journal_root(&scope.principal, &scope.workspace)
            .exists()
        {
            recovery.push(scope);
        }
    }
    Ok(recovery)
}

async fn run_activation_bridge(
    receiver: &mut broadcast::Receiver<RuntimeTransportEvent>,
    recorder: Arc<dyn LlmTraceRecorder>,
    cancel: CancellationToken,
    metrics: Arc<ActivationMetrics>,
    broadcaster: Arc<RuntimeTransportBroadcaster>,
) {
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                // A broadcast receiver has no synchronous drain API. Consume
                // every event already queued before stopping the bridge.
                loop {
                    match receiver.try_recv() {
                        Ok(event) => {
                            record_runtime_event(event, recorder.as_ref(), metrics.as_ref(), broadcaster.as_ref());
                        },
                        Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                            metrics.broadcast_events_lost.fetch_add(skipped, Ordering::Relaxed);
                            submit_mapped_record(
                                recorder.as_ref(),
                                LlmTraceRecord::CaptureGap(broadcast_lag_gap(skipped)),
                                metrics.as_ref(),
                            );
                        },
                        Err(
                            broadcast::error::TryRecvError::Empty
                            | broadcast::error::TryRecvError::Closed,
                        ) => break,
                    }
                }
                break;
            }
            event = receiver.recv() => match event {
                Ok(event) => record_runtime_event(
                    event,
                    recorder.as_ref(),
                    metrics.as_ref(),
                    broadcaster.as_ref(),
                ),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    metrics.broadcast_events_lost.fetch_add(skipped, Ordering::Relaxed);
                    let gap = broadcast_lag_gap(skipped);
                    submit_mapped_record(
                        recorder.as_ref(),
                        LlmTraceRecord::CaptureGap(gap),
                        metrics.as_ref(),
                    );
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }
}

fn record_runtime_event(
    event: RuntimeTransportEvent,
    recorder: &dyn LlmTraceRecorder,
    metrics: &ActivationMetrics,
    broadcaster: &RuntimeTransportBroadcaster,
) {
    if let RuntimeTransportEvent::AgentEvent { event: envelope } = &event {
        if envelope.event_type != LLM_TOOL_LINEAGE_EVENT_TYPE {
            return;
        }
        metrics
            .tool_lineage_events_seen
            .fetch_add(1, Ordering::Relaxed);
        match map_tool_lineage_event(envelope) {
            Ok(record) => {
                submit_mapped_record(recorder, LlmTraceRecord::ToolLineage(record), metrics)
            },
            Err(gap) => {
                tracing::warn!(
                    target: "analytics::llm_trace_activation",
                    reason = %gap.reason,
                    principal = %gap.scope.principal,
                    workspace = %gap.scope.workspace,
                    "tool-lineage event could not be mapped; recording an explicit capture gap"
                );
                submit_mapped_record(recorder, LlmTraceRecord::CaptureGap(gap), metrics);
            },
        }
        return;
    }
    if !matches!(&event, RuntimeTransportEvent::LLMResponseReceived { .. }) {
        return;
    }
    metrics.response_events_seen.fetch_add(1, Ordering::Relaxed);
    match map_response_event(&event) {
        Ok(records) => {
            if records.is_empty() && response_was_reused(&event) {
                metrics
                    .reused_responses_ignored
                    .fetch_add(1, Ordering::Relaxed);
            }
            if records.is_empty() && response_was_external_aggregate(&event) {
                metrics
                    .external_aggregate_events_ignored
                    .fetch_add(1, Ordering::Relaxed);
            }
            // Through the cost-publishing funnel, not straight to the
            // recorder. This is the path a SUCCESSFUL call takes, and it is
            // the only path that ever carries a price — `map_terminal_dispatch`
            // has no priced response to report and says so with a default
            // `LlmPricingFact`. Bypassing the funnel here is what made the
            // whole cost event unreachable in production.
            submit_mapped_records(records, recorder, metrics, broadcaster);
        },
        Err(failure) => {
            tracing::warn!(
                target: "analytics::llm_trace_activation",
                reason = %failure.reason,
                operation = %failure.operation,
                principal = %failure.scope.principal,
                workspace = %failure.scope.workspace,
                "LLM response event could not be mapped; recording an explicit capture gap"
            );
            submit_mapped_record(
                recorder,
                LlmTraceRecord::CaptureGap(failure.into_gap()),
                metrics,
            );
        },
    }
}

fn map_tool_lineage_event(
    envelope: &AgentEventEnvelope,
) -> Result<LlmToolLineageRecord, LlmCaptureGap> {
    let now = chrono::Utc::now().timestamp_millis();
    let principal = envelope
        .principal
        .as_deref()
        .unwrap_or(DEFAULT_SCOPE_PRINCIPAL);
    let workspace = envelope
        .workspace
        .as_deref()
        .unwrap_or(DEFAULT_SCOPE_WORKSPACE);
    let scope = LlmScope::new(principal, workspace);
    let call_id = envelope
        .payload
        .get("context")
        .and_then(|value| value.get("llm_call_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let failure = |reason: &'static str| {
        let seed = format!(
            "{}:{}:{}:{}:{}",
            principal, workspace, envelope.agent_id, envelope.timestamp, reason
        );
        LlmCaptureGap {
            schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
            gap_id: format!(
                "tool-lineage-gap-{}",
                blake3::hash(seed.as_bytes()).to_hex()
            ),
            scope: scope.clone(),
            llm_call_id: call_id.clone(),
            operation: "tool_lineage".to_string(),
            reason: reason.to_string(),
            missing_record_count: 1,
            first_observed_at_ms: envelope.timestamp.max(0),
            last_observed_at_ms: envelope.timestamp.max(0),
            emitted_at_ms: now.max(envelope.timestamp.max(0)),
        }
    };
    let declared_mapping_gap = match envelope
        .payload
        .get("mapping_gap")
        .and_then(serde_json::Value::as_str)
    {
        Some("tool_lineage_assistant_turn_missing_at_dispatch") => {
            "tool_lineage_assistant_turn_missing_at_dispatch"
        },
        Some("tool_lineage_producing_call_trace_receipt_missing") => {
            "tool_lineage_producing_call_trace_receipt_missing"
        },
        Some("tool_lineage_provider_tool_call_index_missing") => {
            "tool_lineage_provider_tool_call_index_missing"
        },
        _ => "tool_lineage_event_invalid",
    };
    let record: LlmToolLineageRecord = serde_json::from_value(envelope.payload.clone())
        .map_err(|_| failure(declared_mapping_gap))?;
    if record.context.scope != scope {
        return Err(failure("tool_lineage_scope_mismatch"));
    }
    LlmTraceRecord::ToolLineage(record.clone())
        .validate()
        .map_err(|_| failure("tool_lineage_contract_invalid"))?;
    Ok(record)
}

/// Keep the activation counter tied to every attempted gap submission. Gap
/// sources include mapper rejection, provider-attempt incompleteness and both
/// transport broadcast rails; centralizing the increment prevents a new gap
/// path from silently disappearing from process health statistics.
fn submit_mapped_record(
    recorder: &dyn LlmTraceRecorder,
    record: LlmTraceRecord,
    metrics: &ActivationMetrics,
) {
    if matches!(&record, LlmTraceRecord::CaptureGap(_)) {
        metrics.gap_records_emitted.fetch_add(1, Ordering::Relaxed);
    }
    submit_record(recorder, record, metrics);
}

fn response_was_reused(event: &RuntimeTransportEvent) -> bool {
    matches!(
        event,
        RuntimeTransportEvent::LLMResponseReceived {
            correlation: Some(LlmEventCorrelation {
                response_reused: true,
                ..
            }),
            ..
        }
    )
}

fn response_was_external_aggregate(event: &RuntimeTransportEvent) -> bool {
    matches!(
        event,
        RuntimeTransportEvent::LLMResponseReceived {
            capability,
            operation,
            response_kind,
            correlation: Some(LlmEventCorrelation {
                provider_attempt_count: 0,
                provider_attempt_id: None,
                dispatch_job_id: None,
                ..
            }),
            ..
        } if capability == "coding" && operation == "coding" && response_kind == "external_ai_run"
    )
}

fn submit_record(
    recorder: &dyn LlmTraceRecorder,
    record: LlmTraceRecord,
    metrics: &ActivationMetrics,
) {
    let result = match record {
        LlmTraceRecord::CallStarted(value) => recorder.begin_call(value),
        LlmTraceRecord::ProviderAttempt(value) => recorder.record_attempt(value),
        LlmTraceRecord::CallCompleted(value) => recorder.complete_call(value),
        LlmTraceRecord::CaptureGap(value) => recorder.record_gap(value),
        LlmTraceRecord::ToolLineage(value) => recorder.record_tool_lineage(value),
        LlmTraceRecord::CallIo(value) => recorder.record_call_io(value),
        LlmTraceRecord::ContextBlock(value) => recorder.record_context_block(value),
        LlmTraceRecord::ContentTombstone(value) => recorder.record_content_tombstone(value),
        LlmTraceRecord::ContentAccessAudit(value) => recorder.record_content_access_audit(value),
    };
    match result {
        Ok(_) => {
            metrics.records_accepted.fetch_add(1, Ordering::Relaxed);
        },
        Err(error) => {
            metrics.records_rejected.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                target: "analytics::llm_trace_activation",
                error = %error,
                "canonical LLM fact enqueue was rejected; pipeline gap accounting remains authoritative"
            );
        },
    }
}

#[derive(Debug)]
struct MappingFailure {
    scope: LlmScope,
    operation: String,
    occurred_at_ms: i64,
    reason: String,
    seed: String,
}

impl MappingFailure {
    fn into_gap(self) -> LlmCaptureGap {
        let reason = format!("{MAPPING_REJECTED}:{}", mapping_reason_code(&self.reason));
        capture_gap(
            self.scope,
            self.operation,
            &reason,
            1,
            self.occurred_at_ms,
            &format!("{}:{}", self.reason, self.seed),
        )
    }
}

fn map_response_event(
    event: &RuntimeTransportEvent,
) -> Result<Vec<LlmTraceRecord>, MappingFailure> {
    let RuntimeTransportEvent::LLMResponseReceived {
        execution_id,
        principal,
        workspace,
        correlation,
        plan_id,
        step_id,
        capability,
        success,
        cost,
        latency_ms,
        error,
        provider,
        model,
        usage_reported,
        input_tokens,
        output_tokens,
        reasoning_tokens,
        cache_read_tokens,
        cache_creation_tokens,
        audio_input_tokens,
        audio_output_tokens,
        audio_cached_tokens,
        search_calls,
        ttft_ms,
        task_id,
        chat_session_id,
        operation,
        profile,
        response_kind,
        started_at_ms,
        timestamp,
        ..
    } = event
    else {
        return Ok(Vec::new());
    };

    let supplied_scope = principal
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .zip(
            workspace
                .as_deref()
                .filter(|value| !value.trim().is_empty()),
        )
        .map(|(principal, workspace)| LlmScope::new(principal, workspace));
    // Mapping failures still need durable loss evidence, but a partially
    // supplied scope must never determine its storage path. Use the explicit
    // diagnostic scope unless both authoritative components are present.
    let scope = supplied_scope
        .as_ref()
        .filter(|scope| scope.is_valid())
        .cloned()
        .unwrap_or_else(|| LlmScope::new(DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE));
    let operation = operation.trim().to_string();
    // Mapping failures have no trustworthy call id. Give every rejected
    // transport envelope its own diagnostic identity so two malformed calls
    // with the same operation/timestamp cannot collapse into one gap row.
    let seed = format!(
        "{execution_id}:{timestamp}:{operation}:{provider}:{model}:{}",
        ulid::Ulid::new()
    );
    let fail = |reason: &str| MappingFailure {
        scope: scope.clone(),
        // A rejected producer field must not be copied into the diagnostic
        // fact. Keeping only a bounded machine category prevents an oversized
        // or content-bearing operation label from making the gap itself
        // unpersistable (or leaking the source value).
        operation: if is_content_free_machine_category(&operation) {
            operation.clone()
        } else {
            "unknown".to_string()
        },
        occurred_at_ms: (*timestamp).max(0),
        reason: reason.to_string(),
        seed: seed.clone(),
    };
    if supplied_scope
        .as_ref()
        .is_none_or(|scope| !scope.is_valid())
    {
        return Err(fail("missing authoritative scope"));
    }
    if operation.is_empty() {
        return Err(fail("missing operation"));
    }
    if *timestamp <= 0 {
        return Err(fail("missing completion timestamp"));
    }
    let correlation = correlation
        .as_ref()
        .ok_or_else(|| fail("missing stable call correlation"))?;
    let mut context = context_from_correlation(correlation, scope.clone())
        .map_err(|reason| fail(reason.as_str()))?;
    if !context.is_valid() {
        return Err(fail("invalid typed call correlation"));
    }
    let external_aggregate =
        capability == "coding" && operation == "coding" && response_kind == "external_ai_run";
    if external_aggregate {
        if correlation.provider_attempt_count != 0
            || correlation.provider_attempt_id.is_some()
            || correlation.dispatch_job_id.is_some()
            || correlation.response_reused
        {
            return Err(fail(
                "external aggregate must not claim provider or dispatch attempts",
            ));
        }
        return Ok(Vec::new());
    }
    // A managed CLI reports one logical aggregate without physical receipts.
    let harness_aggregate =
        provider.starts_with("harness:") && correlation.usage_availability.is_some();
    if harness_aggregate
        && (correlation.provider_attempt_count != 0
            || correlation.provider_attempt_id.is_some()
            || correlation.dispatch_job_id.is_some()
            || correlation.response_reused)
    {
        return Err(fail("harness aggregate must not claim physical attempts"));
    }
    let logical_chunk_summary = response_kind == LLM_LOGICAL_CHUNK_SUMMARY_RESPONSE_KIND;
    let provider_attempt_index = correlation.provider_attempt_count;
    let expected_attempt_id =
        (provider_attempt_index > 0).then(|| context.provider_attempt_id(provider_attempt_index));
    match (
        expected_attempt_id.as_deref(),
        correlation.provider_attempt_id.as_deref(),
    ) {
        (Some(expected), Some(provided)) if expected == provided => {},
        (Some(_), None) => return Err(fail("missing provider attempt id")),
        (Some(_), Some(_)) => {
            return Err(fail(
                "provider attempt id does not match its one-based ordinal",
            ));
        },
        (None, Some(_)) => {
            return Err(fail("provider attempt id exists with a zero attempt count"));
        },
        (None, None) if *success && !logical_chunk_summary && !harness_aggregate => {
            return Err(fail(
                "successful response requires one or more provider attempts",
            ));
        },
        (None, None) => {},
    }

    // Reused subscribers did not invoke a provider, but an invalid reuse flag
    // must not suppress a malformed event without gap evidence. Validate the
    // authoritative scope/correlation and owner dispatch identity first, then
    // skip provider-only payload fields that legitimately belong to the owner
    // delivery.
    if correlation.response_reused {
        if !*success {
            return Err(fail(
                "failed response cannot claim successful response reuse",
            ));
        }
        if correlation
            .dispatch_job_id
            .as_deref()
            .is_none_or(|job_id| job_id.trim().is_empty())
        {
            return Err(fail("reused response requires an owner dispatch job id"));
        }
        return Ok(Vec::new());
    }
    // Top-level event fields are historical compatibility assertions for the
    // subscriber which actually invoked the provider. A reused subscriber can
    // legitimately have different execution/task/session lineage from the
    // owner receipt, so it is deliberately handled above after validating only
    // the authoritative owner identity and dispatch job.
    assert_or_fill_lineage(
        &mut context.execution_id,
        nonempty(execution_id),
        "execution_id",
    )
    .map_err(|reason| fail(&reason))?;
    assert_or_fill_lineage(&mut context.plan_id, nonempty(plan_id), "plan_id")
        .map_err(|reason| fail(&reason))?;
    assert_or_fill_lineage(
        &mut context.step_id,
        step_id.as_deref().and_then(nonempty),
        "step_id",
    )
    .map_err(|reason| fail(&reason))?;
    assert_or_fill_lineage(
        &mut context.task_id,
        task_id.as_deref().and_then(nonempty),
        "task_id",
    )
    .map_err(|reason| fail(&reason))?;
    assert_or_fill_lineage(
        &mut context.chat_session_id,
        chat_session_id.as_deref().and_then(nonempty),
        "chat_session_id",
    )
    .map_err(|reason| fail(&reason))?;
    if !context.is_valid() {
        return Err(fail("invalid typed call correlation after lineage merge"));
    }
    if *started_at_ms <= 0 || *started_at_ms > *timestamp {
        return Err(fail(INVALID_START));
    }
    let validation_error_class = response_kind
        .strip_prefix("validation_error:")
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let validation_success_class = response_kind
        .strip_prefix("validation_success:")
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if response_kind.starts_with("validation_error") && validation_error_class.is_none() {
        return Err(fail("validation error response requires a non-empty class"));
    }
    if response_kind.starts_with("validation_success") && validation_success_class.is_none() {
        return Err(fail(
            "validation success response requires a non-empty class",
        ));
    }
    if !is_content_free_machine_category(response_kind)
        || validation_error_class.is_some_and(|value| !is_content_free_machine_category(value))
        || validation_success_class.is_some_and(|value| !is_content_free_machine_category(value))
    {
        return Err(fail(
            "response kind or validation class is not a content-free machine category",
        ));
    }
    if (validation_error_class.is_some() || validation_success_class.is_some()) && !*success {
        return Err(fail(
            "validation outcome response must report successful provider transport",
        ));
    }
    if *success && error.as_deref().and_then(nonempty).is_some() && validation_error_class.is_none()
    {
        return Err(fail(
            "successful response error is only valid for explicit contract validation failure",
        ));
    }
    if validation_error_class.is_some() && error.as_deref().and_then(nonempty).is_none() {
        return Err(fail("validation error response requires a non-empty error"));
    }
    if validation_success_class.is_some() && error.as_deref().and_then(nonempty).is_some() {
        return Err(fail("validation success response cannot carry an error"));
    }
    if !*success && error.as_deref().and_then(nonempty).is_none() {
        return Err(fail("failed response requires a non-empty error"));
    }

    let completed_at_ms = *timestamp;
    let started_at_ms = *started_at_ms;
    let elapsed_ms = u64::try_from(completed_at_ms - started_at_ms).unwrap_or_default();
    let valid_ttft = (*ttft_ms).filter(|value| *value <= *latency_ms && *value <= elapsed_ms);
    let first_token_at_ms = valid_ttft
        .map(|value| started_at_ms.saturating_add(i64::try_from(value).unwrap_or(i64::MAX)));
    let capture = LlmCaptureFact::default();
    let (error_class, attempt_terminal_state, call_terminal_state) =
        classify_terminal(*success, error.as_deref());
    let error_class =
        if !*success && response_kind == super::decision_model_telemetry::RESPONSE_KIND {
            error
                .as_deref()
                .filter(|value| is_content_free_machine_category(value))
                .map(str::to_string)
                .or(error_class)
        } else {
            error_class
        };
    let is_queued = correlation.dispatch_job_id.is_some();
    let timing = LlmTimingFact {
        created_at_ms: Some(started_at_ms),
        started_at_ms: Some(started_at_ms),
        first_token_at_ms,
        completed_at_ms: Some(completed_at_ms),
        // Direct response latency is provider execution. Queued response
        // latency includes admission/wait and must be enriched from the
        // dispatch stream by stable job id instead of being mislabeled here.
        provider_execution_ms: (!is_queued && provider_attempt_index > 0).then_some(*latency_ms),
        ttft_ms: valid_ttft,
        generation_after_ttft_ms: valid_ttft.map(|value| (*latency_ms).saturating_sub(value)),
        latency_ms: Some(*latency_ms),
        ..LlmTimingFact::default()
    };

    let mut started = LlmCallStarted::new(context.clone(), operation.clone(), started_at_ms);
    started.observed_at_ms = completed_at_ms;
    started.capability = nonempty(capability);
    if response_kind == super::decision_model_telemetry::RESPONSE_KIND {
        started.operation_family = Some("decision_model".into());
    }
    started.source_surface = Some(source_surface(context.workload_class).to_string());
    started.prompt_projection_mode = correlation.prompt_projection_mode.clone();
    // Successful responses and attempted failures with concrete route fields
    // prove the selected profile. A zero-attempt failure only has a requested
    // hint and must not be promoted into an effective route.
    let terminal_route_known =
        provider_attempt_index > 0 && nonempty(provider).is_some() && nonempty(model).is_some();
    if logical_chunk_summary {
        started.requested_profile = profile.clone();
    } else if terminal_route_known {
        started.selected_profile = profile.clone();
    } else {
        started.requested_profile = profile.clone();
    }
    started.capture = capture.clone();

    if logical_chunk_summary && *success {
        if *usage_reported
            || *input_tokens != 0
            || *output_tokens != 0
            || *reasoning_tokens != 0
            || *cache_read_tokens != 0
            || *cache_creation_tokens != 0
            || audio_input_tokens.is_some()
            || audio_output_tokens.is_some()
            || audio_cached_tokens.is_some()
        {
            return Err(fail(
                "logical chunk summary must not duplicate physical child usage",
            ));
        }
        if !cost.is_finite() || *cost != 0.0 {
            return Err(fail(
                "logical chunk summary must carry zero incremental cost",
            ));
        }
        let completed = LlmCallCompleted {
            schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
            context,
            dispatch_job_id: None,
            occurred_at_ms: completed_at_ms,
            observed_at_ms: completed_at_ms,
            operation: operation.clone(),
            terminal_state: LlmCallTerminalState::Succeeded,
            provider_attempt_count: 0,
            provider_response_id: None,
            response_kind: Some(LLM_LOGICAL_CHUNK_SUMMARY_RESPONSE_KIND.to_string()),
            error_class: None,
            error_code: None,
            finish_reason: None,
            refusal: None,
            truncated: None,
            timing,
            usage: LlmTokenUsageFact::default(),
            pricing: LlmPricingFact {
                pricing_version: Some(LOGICAL_CHUNK_SUMMARY_VERSION.to_string()),
                cost_source: Some(LlmCostSource::Unknown),
                ..LlmPricingFact::default()
            },
            validation: LlmImmediateValidationFact {
                response_present: true,
                ..LlmImmediateValidationFact::default()
            },
            capture,
        };
        return validate_mapped_records(vec![
            LlmTraceRecord::CallStarted(started),
            LlmTraceRecord::CallCompleted(completed),
        ])
        .map_err(|reason| fail(&format!("invalid canonical records: {reason}")));
    }

    let usage_valid = !*usage_reported
        || runtime_usage_is_consistent(
            *input_tokens,
            *output_tokens,
            *reasoning_tokens,
            *cache_read_tokens,
            *cache_creation_tokens,
            *audio_input_tokens,
            *audio_output_tokens,
            *audio_cached_tokens,
        );
    let usage = if *usage_reported && usage_valid {
        let total_tokens = u64::from(*input_tokens).saturating_add(u64::from(*output_tokens));
        LlmTokenUsageFact {
            input_tokens: Some(u64::from(*input_tokens)),
            output_tokens: Some(u64::from(*output_tokens)),
            reasoning_tokens: Some(u64::from(*reasoning_tokens)),
            cache_read_tokens: correlation
                .usage_availability
                .map_or(true, |u| u.cache_read)
                .then_some(u64::from(*cache_read_tokens)),
            cache_creation_tokens: correlation
                .usage_availability
                .map_or(true, |u| u.cache_write)
                .then_some(u64::from(*cache_creation_tokens)),
            audio_input_tokens: (*audio_input_tokens).map(u64::from),
            audio_output_tokens: (*audio_output_tokens).map(u64::from),
            audio_cached_tokens: (*audio_cached_tokens).map(u64::from),
            total_tokens: Some(total_tokens),
        }
    } else {
        LlmTokenUsageFact::default()
    };
    let decision_pricing = (response_kind == super::decision_model_telemetry::RESPONSE_KIND
        && provider.starts_with("decision:"))
    .then(|| {
        let availability = correlation.usage_availability.unwrap_or_default();
        super::decision_model_telemetry::pricing(
            provider,
            model,
            started_at_ms,
            &magicllm::TokenUsage {
                prompt_tokens: (*usage_reported && usage_valid).then_some(*input_tokens),
                completion_tokens: (*usage_reported && usage_valid).then_some(*output_tokens),
                cached_tokens: availability.cache_read.then_some(*cache_read_tokens),
                cache_creation_tokens: availability.cache_write.then_some(*cache_creation_tokens),
                ..Default::default()
            },
        )
    });
    if !*success {
        let completed = LlmCallCompleted {
            schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
            context: context.clone(),
            dispatch_job_id: correlation.dispatch_job_id.clone(),
            occurred_at_ms: completed_at_ms,
            observed_at_ms: completed_at_ms,
            operation: operation.clone(),
            terminal_state: call_terminal_state,
            provider_attempt_count: correlation.provider_attempt_count,
            provider_response_id: None,
            response_kind: if harness_aggregate {
                Some(LLM_HARNESS_AGGREGATE_RESPONSE_KIND.into())
            } else {
                nonempty(response_kind)
            },
            error_class: error_class.clone(),
            error_code: None,
            finish_reason: None,
            refusal: error
                .as_deref()
                .map(|value| contains_any(value, &["refusal", "content policy", "safety"])),
            truncated: None,
            timing: timing.clone(),
            // Native transport errors carry placeholders. A CLI may report
            // metered work before a failed turn; preserve only explicit facts.
            usage: if correlation.usage_availability.is_some_and(|u| u.tokens) {
                usage.clone()
            } else {
                LlmTokenUsageFact::default()
            },
            pricing: decision_pricing.clone().unwrap_or_else(|| {
                aggregate_harness_pricing(
                    correlation.usage_availability,
                    *cost,
                    usage_valid && *usage_reported,
                )
            }),
            validation: LlmImmediateValidationFact::default(),
            capture: capture.clone(),
        };
        let mut records = vec![LlmTraceRecord::CallStarted(started)];
        if terminal_route_known {
            records.push(LlmTraceRecord::ProviderAttempt(LlmProviderAttemptEvent {
                schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
                context: context.clone(),
                dispatch_job_id: correlation.dispatch_job_id.clone(),
                provider_attempt_id: expected_attempt_id
                    .clone()
                    .expect("attempt identity was validated above"),
                provider_attempt_index,
                phase: LlmProviderAttemptPhase::Completed,
                occurred_at_ms: completed_at_ms,
                observed_at_ms: completed_at_ms,
                operation: operation.clone(),
                effective_profile: profile.clone(),
                provider: provider.clone(),
                model: model.clone(),
                model_revision: None,
                timing: terminal_attempt_timing(&timing, is_queued, provider_attempt_index),
                terminal_state: Some(attempt_terminal_state),
                error_class: error_class.clone(),
                error_code: None,
                finish_reason: None,
                refusal: error
                    .as_deref()
                    .map(|value| contains_any(value, &["refusal", "content policy", "safety"])),
                truncated: None,
                usage: if decision_pricing.is_some() {
                    completed.usage.clone()
                } else {
                    LlmTokenUsageFact::default()
                },
                pricing: if decision_pricing.is_some() {
                    completed.pricing.clone()
                } else {
                    LlmPricingFact::default()
                },
                capture: capture.clone(),
            }));
        }
        records.push(LlmTraceRecord::CallCompleted(completed));
        if terminal_route_known && provider_attempt_index > 1 {
            records.push(LlmTraceRecord::CaptureGap(capture_gap_for_call(
                scope.clone(),
                operation.clone(),
                EARLIER_ATTEMPTS_UNAVAILABLE,
                u64::from(provider_attempt_index - 1),
                completed_at_ms,
                &context.llm_call_id,
            )));
        } else if provider_attempt_index > 0 && !terminal_route_known {
            records.push(LlmTraceRecord::CaptureGap(capture_gap_for_call(
                scope.clone(),
                operation.clone(),
                TERMINAL_ATTEMPTS_UNAVAILABLE,
                u64::from(provider_attempt_index),
                completed_at_ms,
                &context.llm_call_id,
            )));
        }
        return validate_mapped_records(records)
            .map_err(|reason| fail(&format!("invalid canonical records: {reason}")));
    }

    // A successful transport proves the logical call completed, even if an
    // older or incomplete producer omitted the effective route. Preserve the
    // stable call identity and provider-reported usage, but do not attach the
    // configured/requested profile to a physical attempt. The exact missing
    // attempt count remains call-owned gap evidence for reconciliation.
    if !terminal_route_known {
        let completed = LlmCallCompleted {
            schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
            context: context.clone(),
            dispatch_job_id: correlation.dispatch_job_id.clone(),
            occurred_at_ms: completed_at_ms,
            observed_at_ms: completed_at_ms,
            operation: operation.clone(),
            terminal_state: call_terminal_state,
            provider_attempt_count: correlation.provider_attempt_count,
            provider_response_id: None,
            response_kind: if harness_aggregate {
                Some(LLM_HARNESS_AGGREGATE_RESPONSE_KIND.into())
            } else {
                nonempty(response_kind)
            },
            error_class,
            error_code: None,
            finish_reason: None,
            refusal: None,
            truncated: None,
            timing,
            usage,
            pricing: aggregate_harness_pricing(
                correlation.usage_availability,
                *cost,
                usage_valid && *usage_reported,
            ),
            validation: LlmImmediateValidationFact {
                response_present: true,
                ..LlmImmediateValidationFact::default()
            },
            capture,
        };
        let mut records = vec![
            LlmTraceRecord::CallStarted(started),
            LlmTraceRecord::CallCompleted(completed),
        ];
        if provider_attempt_index > 0 {
            records.push(LlmTraceRecord::CaptureGap(capture_gap_for_call(
                scope.clone(),
                operation.clone(),
                TERMINAL_ATTEMPTS_UNAVAILABLE,
                u64::from(provider_attempt_index),
                completed_at_ms,
                &context.llm_call_id,
            )));
        }
        if !usage_reported {
            records.push(LlmTraceRecord::CaptureGap(capture_gap_for_call(
                scope.clone(),
                operation.clone(),
                USAGE_UNREPORTED,
                1,
                completed_at_ms,
                &context.llm_call_id,
            )));
        } else if !usage_valid {
            records.push(LlmTraceRecord::CaptureGap(capture_gap_for_call(
                scope.clone(),
                operation.clone(),
                INVALID_USAGE,
                1,
                completed_at_ms,
                &context.llm_call_id,
            )));
        }
        if !cost.is_finite() || *cost < 0.0 {
            records.push(LlmTraceRecord::CaptureGap(capture_gap_for_call(
                scope.clone(),
                operation.clone(),
                INVALID_COST,
                1,
                completed_at_ms,
                &context.llm_call_id,
            )));
        }
        return validate_mapped_records(records)
            .map_err(|reason| fail(&format!("invalid canonical records: {reason}")));
    }
    let (pricing, supplied_cost_mismatch) = pricing_for_runtime_event(
        provider,
        model,
        started_at_ms,
        *cost,
        *usage_reported,
        usage_valid,
        *input_tokens,
        *output_tokens,
        *reasoning_tokens,
        *cache_read_tokens,
        *cache_creation_tokens,
        *audio_input_tokens,
        *audio_output_tokens,
        *audio_cached_tokens,
        *search_calls,
    );

    let (pricing, supplied_cost_mismatch) = decision_pricing
        .map(|pricing| (pricing, false))
        .unwrap_or((pricing, supplied_cost_mismatch));

    let attempt = LlmProviderAttemptEvent {
        schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
        context: context.clone(),
        dispatch_job_id: correlation.dispatch_job_id.clone(),
        provider_attempt_id: expected_attempt_id
            .expect("successful response attempt identity was validated above"),
        provider_attempt_index,
        phase: LlmProviderAttemptPhase::Completed,
        occurred_at_ms: completed_at_ms,
        observed_at_ms: completed_at_ms,
        operation: operation.clone(),
        effective_profile: profile.clone(),
        provider: provider.clone(),
        model: model.clone(),
        model_revision: None,
        timing: terminal_attempt_timing(&timing, is_queued, provider_attempt_index),
        terminal_state: Some(attempt_terminal_state),
        error_class: error_class.clone(),
        error_code: None,
        finish_reason: None,
        refusal: error
            .as_deref()
            .map(|value| contains_any(value, &["refusal", "content policy", "safety"])),
        truncated: None,
        usage: usage.clone(),
        pricing: pricing.clone(),
        capture: capture.clone(),
    };
    let completed = LlmCallCompleted {
        schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
        context: context.clone(),
        dispatch_job_id: correlation.dispatch_job_id.clone(),
        occurred_at_ms: completed_at_ms,
        observed_at_ms: completed_at_ms,
        operation: operation.clone(),
        terminal_state: call_terminal_state,
        provider_attempt_count: correlation.provider_attempt_count,
        provider_response_id: None,
        response_kind: nonempty(response_kind),
        error_class,
        error_code: None,
        finish_reason: None,
        refusal: error
            .as_deref()
            .map(|value| contains_any(value, &["refusal", "content policy", "safety"])),
        truncated: None,
        timing,
        usage,
        pricing,
        validation: validation_error_class.map_or_else(
            || {
                validation_success_class.map_or_else(
                    || LlmImmediateValidationFact {
                        response_present: true,
                        ..LlmImmediateValidationFact::default()
                    },
                    |_| LlmImmediateValidationFact {
                        response_present: true,
                        contract_validation_attempted: true,
                        contract_validation_success: Some(true),
                        ..LlmImmediateValidationFact::default()
                    },
                )
            },
            |validation_error_class| LlmImmediateValidationFact {
                response_present: true,
                contract_validation_attempted: true,
                contract_validation_success: Some(false),
                validation_error_class: Some(validation_error_class.to_string()),
                discarded_before_use: true,
                discard_reason: Some("execution_native_contract_rejected".to_string()),
                ..LlmImmediateValidationFact::default()
            },
        ),
        capture,
    };

    let mut records = vec![
        LlmTraceRecord::CallStarted(started),
        LlmTraceRecord::ProviderAttempt(attempt),
        LlmTraceRecord::CallCompleted(completed),
    ];
    if provider_attempt_index > 1 {
        records.push(LlmTraceRecord::CaptureGap(capture_gap_for_call(
            scope.clone(),
            operation.clone(),
            EARLIER_ATTEMPTS_UNAVAILABLE,
            u64::from(provider_attempt_index - 1),
            completed_at_ms,
            &context.llm_call_id,
        )));
    }
    if ttft_ms.is_some() && valid_ttft.is_none() {
        records.push(LlmTraceRecord::CaptureGap(capture_gap_for_call(
            scope.clone(),
            operation.clone(),
            INVALID_TTFT,
            1,
            completed_at_ms,
            &context.llm_call_id,
        )));
    }
    if !usage_reported {
        records.push(LlmTraceRecord::CaptureGap(capture_gap_for_call(
            scope.clone(),
            operation.clone(),
            USAGE_UNREPORTED,
            1,
            completed_at_ms,
            &context.llm_call_id,
        )));
    }
    if *usage_reported && !usage_valid {
        records.push(LlmTraceRecord::CaptureGap(capture_gap_for_call(
            scope.clone(),
            operation.clone(),
            INVALID_USAGE,
            1,
            completed_at_ms,
            &context.llm_call_id,
        )));
    }
    if !cost.is_finite() || *cost < 0.0 {
        records.push(LlmTraceRecord::CaptureGap(capture_gap_for_call(
            scope.clone(),
            operation.clone(),
            INVALID_COST,
            1,
            completed_at_ms,
            &context.llm_call_id,
        )));
    }
    if supplied_cost_mismatch {
        records.push(LlmTraceRecord::CaptureGap(capture_gap_for_call(
            scope.clone(),
            operation.clone(),
            SUPPLIED_COST_MISMATCH,
            1,
            completed_at_ms,
            &context.llm_call_id,
        )));
    }
    validate_mapped_records(records)
        .map_err(|reason| fail(&format!("invalid canonical records: {reason}")))
}

#[cfg(any(test, feature = "test-fixtures"))]
#[test]
fn decision_model_receipts_map_to_priced_canonical_calls_and_attempts_without_duplicate_aggregates()
{
    use super::decision_model_telemetry::{event, fixture_call};
    use decision_engine_contract::telemetry::DecisionCallStatus;
    let mut context = LlmTraceContext::new(
        LlmScope::new("owner", "default"),
        LlmWorkloadClass::ForegroundChat,
    );
    context.chat_session_id = Some("session-1".into());
    context.chat_turn_id = Some("turn-1".into());
    context.execution_id = Some("turn-1".into());
    for status in [
        DecisionCallStatus::Succeeded,
        DecisionCallStatus::Failed,
        DecisionCallStatus::Cancelled,
    ] {
        let mut call = fixture_call();
        call.status = status.clone();
        call.error_class = match status {
            DecisionCallStatus::Succeeded => None,
            DecisionCallStatus::Failed => Some("invalid_response".into()),
            DecisionCallStatus::Cancelled => Some("cancelled".into()),
        };
        let records =
            map_response_event(&event(&context, &call)).unwrap_or_else(|e| panic!("{}", e.reason));
        let completed: Vec<_> = records
            .iter()
            .filter_map(|r| match r {
                LlmTraceRecord::CallCompleted(v) => Some(v),
                _ => None,
            })
            .collect();
        let attempts: Vec<_> = records
            .iter()
            .filter_map(|r| match r {
                LlmTraceRecord::ProviderAttempt(v) => Some(v),
                _ => None,
            })
            .collect();
        assert_eq!(completed.len(), 1);
        assert_eq!(attempts.len(), 1);
        assert_eq!(completed[0].context.chat_turn_id.as_deref(), Some("turn-1"));
        assert_eq!(attempts[0].model, "jev-1.13.0");
        assert_eq!(completed[0].usage.input_tokens, Some(1000));
        assert_eq!(completed[0].usage.cache_read_tokens, None);
        assert_eq!(completed[0].usage.cache_creation_tokens, None);
        assert!((completed[0].pricing.cost_usd.unwrap() - 0.000042).abs() < 1e-12);
        assert_eq!(attempts[0].pricing, completed[0].pricing);
        assert!(!records
            .iter()
            .any(|r| matches!(r, LlmTraceRecord::CaptureGap(_))));
    }
    let mut local = fixture_call();
    local.provider = "decision:laya-mlx".into();
    local.model = "laya".into();
    local.input_tokens = None;
    local.output_tokens = None;
    local.status = DecisionCallStatus::Failed;
    local.error_class = Some("model_error".into());
    let records =
        map_response_event(&event(&context, &local)).unwrap_or_else(|e| panic!("{}", e.reason));
    let completed = records
        .iter()
        .find_map(|r| match r {
            LlmTraceRecord::CallCompleted(v) => Some(v),
            _ => None,
        })
        .unwrap();
    assert_eq!(completed.pricing.cost_usd, Some(0.0));
    assert_eq!(completed.pricing.cost_source, Some(LlmCostSource::Local));
    assert_eq!(completed.usage.input_tokens, None);
}

fn validate_mapped_records(records: Vec<LlmTraceRecord>) -> Result<Vec<LlmTraceRecord>, String> {
    for record in &records {
        record
            .validate()
            .map_err(|error| format!("{}: {error}", record.key().idempotency_key()))?;
    }
    Ok(records)
}

fn context_from_correlation(
    correlation: &LlmEventCorrelation,
    scope: LlmScope,
) -> Result<LlmTraceContext, String> {
    if correlation.schema_version != 1 {
        return Err(format!(
            "unsupported correlation schema {}",
            correlation.schema_version
        ));
    }
    let scope_resolution = match correlation.scope_resolution.as_str() {
        "explicit" => LlmScopeResolution::Explicit,
        "inherited" => LlmScopeResolution::Inherited,
        "system_default" => LlmScopeResolution::SystemDefault,
        "legacy_default" => LlmScopeResolution::LegacyDefault,
        other => return Err(format!("unknown scope resolution {other}")),
    };
    let workload_class = match correlation.workload_class.as_str() {
        "foreground_chat" => LlmWorkloadClass::ForegroundChat,
        "interactive_task" => LlmWorkloadClass::InteractiveTask,
        "autonomous_task" => LlmWorkloadClass::AutonomousTask,
        "scheduled" => LlmWorkloadClass::Scheduled,
        "ambient" => LlmWorkloadClass::Ambient,
        "comms_assist" => LlmWorkloadClass::CommsAssist,
        "memory" => LlmWorkloadClass::Memory,
        "evaluation" => LlmWorkloadClass::Evaluation,
        "system" => LlmWorkloadClass::System,
        other => return Err(format!("unknown workload class {other}")),
    };
    let call_role = match correlation.call_role.as_str() {
        "primary" => LlmCallRole::Primary,
        "supporting" => LlmCallRole::Supporting,
        "summarizer" => LlmCallRole::Summarizer,
        "classifier" => LlmCallRole::Classifier,
        "validator" => LlmCallRole::Validator,
        "verifier" => LlmCallRole::Verifier,
        "judge" => LlmCallRole::Judge,
        "recovery" => LlmCallRole::Recovery,
        "title" => LlmCallRole::Title,
        "memory" => LlmCallRole::Memory,
        other => return Err(format!("unknown call role {other}")),
    };
    let parent_relation = match correlation.parent_relation.as_deref() {
        None => None,
        Some("cascade") => Some(LlmParentRelation::Cascade),
        Some("verifies") => Some(LlmParentRelation::Verifies),
        Some("judges") => Some(LlmParentRelation::Judges),
        Some("summarizes") => Some(LlmParentRelation::Summarizes),
        Some("supports") => Some(LlmParentRelation::Supports),
        Some("chunk_map") => Some(LlmParentRelation::ChunkMap),
        Some("chunk_repair") => Some(LlmParentRelation::ChunkRepair),
        Some("chunk_fallback") => Some(LlmParentRelation::ChunkFallback),
        Some("chunk_reduce") => Some(LlmParentRelation::ChunkReduce),
        Some(other) => return Err(format!("unknown parent relation {other}")),
    };
    if correlation.parent_call_id.is_some() != parent_relation.is_some() {
        return Err("parent call and relation must be populated together".to_string());
    }
    Ok(LlmTraceContext {
        trace_id: correlation.trace_id.clone(),
        llm_call_id: correlation.llm_call_id.clone(),
        // The join key, carried on the correlation for exactly this
        // reconstruction. Hard-coding `None` here silently emptied it for
        // every successful call: the durable `LlmCallCompleted` that owns the
        // price knew what the call cost and not which unit of work it belonged
        // to, so the parquet join and the live cost event both had a number
        // and no owner.
        //
        // Normalised rather than copied, and deliberately not a rejection. This
        // reconstruction runs over a call that has already completed and been
        // paid for; returning `Err` for a malformed id would turn the whole
        // record into a `CaptureGap`, discarding the price to punish the label.
        // Dropping the label keeps the cost row and loses only the join.
        activity_id: normalized_activity_id(correlation.activity_id.as_deref()),
        parent_call_id: correlation.parent_call_id.clone(),
        parent_relation,
        retry_group_id: correlation.retry_group_id.clone(),
        route_decision_id: correlation.route_decision_id.clone(),
        scope,
        scope_resolution,
        task_id: correlation.task_id.clone(),
        root_execution_id: correlation.root_execution_id.clone(),
        execution_id: correlation.execution_id.clone(),
        plan_id: correlation.plan_id.clone(),
        step_id: correlation.step_id.clone(),
        iteration_id: correlation.iteration_id.clone(),
        chat_session_id: correlation.chat_session_id.clone(),
        chat_turn_id: correlation.chat_turn_id.clone(),
        user_message_id: correlation.user_message_id.clone(),
        workload_class,
        call_role,
    })
}

fn classify_terminal(
    success: bool,
    error: Option<&str>,
) -> (
    Option<String>,
    LlmAttemptTerminalState,
    LlmCallTerminalState,
) {
    if success {
        return (
            None,
            LlmAttemptTerminalState::Succeeded,
            LlmCallTerminalState::Succeeded,
        );
    }
    let error = error.unwrap_or_default();
    if contains_any(error, &["cancel", "tombstone"]) {
        return (
            Some("cancelled".to_string()),
            LlmAttemptTerminalState::Cancelled,
            LlmCallTerminalState::Cancelled,
        );
    }
    if contains_any(error, &["timeout", "timed out", "deadline"]) {
        return (
            Some("timeout".to_string()),
            LlmAttemptTerminalState::TimedOut,
            LlmCallTerminalState::Failed,
        );
    }
    let class = if contains_any(
        error,
        &[
            "configuration error",
            "missing api key",
            "no router configured",
        ],
    ) {
        "configuration"
    } else if contains_any(error, &["unsupported capability", "not supported by"]) {
        "unsupported_capability"
    } else if contains_any(
        error,
        &["validation error", "context budget", "invalid request"],
    ) {
        "validation"
    } else if contains_any(error, &["queue lane", "queue full", "receiver dropped"]) {
        "dispatch_error"
    } else if contains_any(
        error,
        &["transport error", "connection error", "network error"],
    ) {
        "transport"
    } else if contains_any(error, &["rate limit", "429"]) {
        "rate_limit"
    } else if contains_any(error, &["refusal", "content policy", "safety"]) {
        "content_policy"
    } else {
        "provider_error"
    };
    (
        Some(class.to_string()),
        LlmAttemptTerminalState::Failed,
        LlmCallTerminalState::Failed,
    )
}

/// Aggregate CLI estimates remain call-owned; never invent physical provider attempts.
fn aggregate_harness_pricing(
    availability: Option<magicllm::types::UsageAvailability>,
    cost: f64,
    usage_valid: bool,
) -> LlmPricingFact {
    let Some(availability) = availability else {
        return LlmPricingFact::default();
    };
    let known =
        availability.tokens && availability.cost && usage_valid && cost.is_finite() && cost >= 0.0;
    LlmPricingFact {
        pricing_version: Some("harness-reported".to_string()),
        cost_source: Some(if known {
            LlmCostSource::Estimated
        } else {
            LlmCostSource::Unknown
        }),
        cost_usd: known.then_some(cost),
        ..LlmPricingFact::default()
    }
}

#[allow(clippy::too_many_arguments)]
fn pricing_for_runtime_event(
    provider: &str,
    model: &str,
    started_at_ms: i64,
    supplied_cost_usd: f64,
    usage_reported: bool,
    usage_valid: bool,
    input_tokens: u32,
    output_tokens: u32,
    reasoning_tokens: u32,
    cache_read_tokens: u32,
    cache_creation_tokens: u32,
    audio_input_tokens: Option<u32>,
    audio_output_tokens: Option<u32>,
    audio_cached_tokens: Option<u32>,
    search_calls: u32,
) -> (LlmPricingFact, bool) {
    let provider_kind = provider_kind_for_pricing(provider);
    let realtime_usage = audio_input_tokens.is_some()
        || audio_output_tokens.is_some()
        || audio_cached_tokens.is_some();
    let pricing_version = if realtime_usage {
        // The realtime table is keyed by model and holds every vendor's
        // realtime rows, so gating it on one vendor left the others priceless:
        // every Gemini Live row read a pricing miss and cost Unknown while the
        // voice trace beside it carried a real computed price. What the gate is
        // actually for is the rule in `llm_pricing_identity` — an
        // OpenAI-compatible private endpoint is its own billing route and must
        // never inherit vendor pricing by looking like it. That is a question
        // about custom providers, not about which vendor this is.
        (!matches!(provider_kind, magicllm::LLMProviderKind::Custom(_)))
            .then(|| magicllm::active_table().realtime_pricing_version_at(model, started_at_ms))
            .flatten()
    } else {
        magicllm::active_table().pricing_version_at(&provider_kind, model, started_at_ms)
    };
    if !usage_reported {
        return (
            LlmPricingFact {
                pricing_version: Some(USAGE_UNREPORTED_VERSION.to_string()),
                cost_source: Some(LlmCostSource::Unknown),
                ..LlmPricingFact::default()
            },
            false,
        );
    }
    if !usage_valid {
        return (
            LlmPricingFact {
                pricing_version: Some(INVALID_PRICING_VERSION.to_string()),
                cost_source: Some(LlmCostSource::Unknown),
                ..LlmPricingFact::default()
            },
            false,
        );
    }
    if let Some(pricing_version) = pricing_version {
        // A model billed by the clock reports no tokens of its own, so its
        // buckets carry nothing to recompute from and a recompute would price
        // a real session at an authoritative $0.00. Its duration is measured
        // by the producer and appears nowhere else on the event, so the
        // producer's figure is the only evidence there is. The producer may
        // have measured the session itself rather than been told, so the
        // strongest claim that stays true is an estimate; an unusable figure
        // stays unknown rather than zero.
        if realtime_usage && magicllm::realtime_is_duration_billed_at(model, started_at_ms) {
            let usable = supplied_cost_usd.is_finite() && supplied_cost_usd >= 0.0;
            return (
                LlmPricingFact {
                    pricing_version: Some(pricing_version),
                    cost_source: Some(if usable {
                        LlmCostSource::Estimated
                    } else {
                        LlmCostSource::Unknown
                    }),
                    cost_usd: usable.then_some(supplied_cost_usd),
                    ..LlmPricingFact::default()
                },
                false,
            );
        }
        // The compatibility event's dollar field is an assertion made by the
        // producer, not authoritative pricing evidence. Recompute from the
        // provider-reported token buckets and the effective-dated table so a
        // stale producer cannot silently poison the canonical training facts.
        let computed_cost = if realtime_usage {
            let cache_read = u64::from(cache_read_tokens);
            let audio_cached = u64::from(audio_cached_tokens.unwrap_or_default());
            let uncached_input = u64::from(input_tokens)
                .saturating_sub(cache_read)
                .saturating_sub(u64::from(cache_creation_tokens));
            let realtime = magicllm::types::RealtimeUsage {
                text_input_tokens: uncached_input
                    .saturating_sub(u64::from(audio_input_tokens.unwrap_or_default())),
                text_cached_input_tokens: cache_read.saturating_sub(audio_cached),
                text_output_tokens: u64::from(output_tokens)
                    .saturating_sub(u64::from(audio_output_tokens.unwrap_or_default())),
                audio_input_tokens: u64::from(audio_input_tokens.unwrap_or_default()),
                audio_cached_input_tokens: audio_cached,
                audio_output_tokens: u64::from(audio_output_tokens.unwrap_or_default()),
                billed_seconds: 0.0,
            };
            magicllm::compute_realtime_cost_at(model, &realtime, started_at_ms)
        } else {
            let usage = magicllm::TokenUsage {
                prompt_tokens: Some(input_tokens),
                completion_tokens: Some(output_tokens),
                total_tokens: Some(input_tokens.saturating_add(output_tokens)),
                reasoning_tokens: Some(reasoning_tokens),
                cached_tokens: Some(cache_read_tokens),
                cache_creation_tokens: Some(cache_creation_tokens),
            };
            // Include the per-search charges the producer's cost carries, or
            // every searched call would flag a false cost-mismatch gap.
            magicllm::compute_cost_with_server_web_search_at(
                &provider_kind,
                model,
                &usage,
                search_calls as usize,
                started_at_ms,
            )
        };
        let supplied_cost_mismatch = supplied_cost_usd.is_finite()
            && supplied_cost_usd >= 0.0
            && (supplied_cost_usd - computed_cost).abs() > (computed_cost.abs() * 1e-7).max(1e-9);
        return (
            LlmPricingFact {
                pricing_version: Some(pricing_version),
                cost_source: Some(LlmCostSource::Computed),
                cost_usd: Some(computed_cost),
                ..LlmPricingFact::default()
            },
            supplied_cost_mismatch,
        );
    }
    // The legacy event carries `0.0` both for genuinely free calls and
    // for a missing price row. Never turn an unpriced online/model pair
    // into authoritative zero spend; retain explicit provenance instead.
    (
        LlmPricingFact {
            pricing_version: Some(UNPRICED_VERSION.to_string()),
            cost_source: Some(LlmCostSource::Unknown),
            ..LlmPricingFact::default()
        },
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn runtime_usage_is_consistent(
    input_tokens: u32,
    output_tokens: u32,
    reasoning_tokens: u32,
    cache_read_tokens: u32,
    cache_creation_tokens: u32,
    audio_input_tokens: Option<u32>,
    audio_output_tokens: Option<u32>,
    audio_cached_tokens: Option<u32>,
) -> bool {
    let cache_total = u64::from(cache_read_tokens) + u64::from(cache_creation_tokens);
    if cache_total > u64::from(input_tokens) || reasoning_tokens > output_tokens {
        return false;
    }
    let realtime_usage = audio_input_tokens.is_some()
        || audio_output_tokens.is_some()
        || audio_cached_tokens.is_some();
    if !realtime_usage {
        return true;
    }
    // RealtimeUsage has no cache-write bucket. Once a modality split is
    // present, every audio bucket is required: absence means unknown, not
    // authoritative zero.
    let (Some(audio_input_tokens), Some(audio_output_tokens), Some(audio_cached_tokens)) =
        (audio_input_tokens, audio_output_tokens, audio_cached_tokens)
    else {
        return false;
    };
    cache_creation_tokens == 0
        && audio_cached_tokens <= cache_read_tokens
        && u64::from(audio_input_tokens) <= u64::from(input_tokens).saturating_sub(cache_total)
        && audio_output_tokens <= output_tokens
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    let value = value.to_ascii_lowercase();
    needles.iter().any(|needle| value.contains(needle))
}

/// Collapse diagnostic detail to a finite content-free vocabulary before it
/// reaches a persisted capture-gap fact. Transliteration is not redaction:
/// arbitrary prose with spaces changed to underscores can still reveal the
/// source text.
fn mapping_reason_code(value: &str) -> &'static str {
    let value = value.to_ascii_lowercase();
    if value.starts_with("missing authoritative scope") {
        "missing_authoritative_scope"
    } else if value.starts_with("missing operation") {
        "missing_operation"
    } else if value.starts_with("missing completion timestamp") {
        "missing_completion_timestamp"
    } else if value.starts_with("missing stable call correlation") {
        "missing_stable_call_correlation"
    } else if value.starts_with("unsupported correlation schema") {
        "unsupported_correlation_schema"
    } else if value.starts_with("unknown scope resolution") {
        "unknown_scope_resolution"
    } else if value.starts_with("unknown workload class") {
        "unknown_workload_class"
    } else if value.starts_with("unknown call role") {
        "unknown_call_role"
    } else if value.starts_with("unknown parent relation") {
        "unknown_parent_relation"
    } else if value.starts_with("parent call and relation") {
        "invalid_parent_relation"
    } else if value.starts_with("invalid typed call correlation")
        || value.starts_with("invalid trace context")
    {
        "invalid_trace_context"
    } else if value.contains("conflicts with the runtime event") {
        "lineage_conflict"
    } else if value.starts_with("external aggregate") {
        "invalid_external_aggregate"
    } else if value.starts_with("missing provider attempt id") {
        "missing_provider_attempt_id"
    } else if value.starts_with("provider attempt id does not match")
        || value.starts_with("invalid provider attempt identity")
        || value.starts_with("missing or inconsistent final provider attempt identity")
    {
        "invalid_provider_attempt_identity"
    } else if value.starts_with("provider attempt id exists with a zero") {
        "unexpected_provider_attempt_id"
    } else if value.starts_with("successful response requires one or more provider attempts") {
        "missing_provider_attempts"
    } else if value.starts_with("failed response cannot claim successful response reuse") {
        "invalid_response_reuse"
    } else if value.starts_with("reused response requires an owner dispatch job id") {
        "missing_owner_dispatch_job_id"
    } else if value.starts_with(INVALID_START) || value.starts_with("invalid timestamps") {
        INVALID_START
    } else if value.starts_with("validation error response requires a non-empty class") {
        "missing_validation_error_class"
    } else if value.starts_with("validation success response requires a non-empty class") {
        "missing_validation_success_class"
    } else if value.starts_with("response kind or validation class") {
        "invalid_response_category"
    } else if value.starts_with("validation outcome response must report") {
        "invalid_validation_transport_state"
    } else if value.starts_with("successful response error is only valid") {
        "unexpected_success_error"
    } else if value.starts_with("validation error response requires a non-empty error") {
        "missing_validation_error"
    } else if value.starts_with("validation success response cannot carry an error") {
        "unexpected_validation_error"
    } else if value.starts_with("failed response requires a non-empty error") {
        "missing_failure_error"
    } else if value.starts_with("logical chunk summary") {
        "invalid_logical_chunk_summary"
    } else if value.starts_with("missing effective provider or model")
        || value.starts_with("missing provider or model")
    {
        "missing_effective_route"
    } else if value.starts_with("invalid submission timestamp") {
        "invalid_submission_timestamp"
    } else if value.starts_with("invalid dispatch timestamp") {
        "invalid_dispatch_timestamp"
    } else if value.contains("serialized record exceeds") {
        "record_oversize"
    } else if value.starts_with("invalid canonical records") {
        "invalid_canonical_records"
    } else {
        "mapping_validation_failed"
    }
}

fn source_surface(workload: LlmWorkloadClass) -> &'static str {
    match workload {
        LlmWorkloadClass::ForegroundChat => "chat",
        LlmWorkloadClass::InteractiveTask | LlmWorkloadClass::AutonomousTask => "task",
        LlmWorkloadClass::Scheduled => "scheduled",
        LlmWorkloadClass::Ambient => "ambient",
        LlmWorkloadClass::CommsAssist => "comms_assist",
        LlmWorkloadClass::Memory => "memory",
        LlmWorkloadClass::Evaluation => "evaluation",
        LlmWorkloadClass::System => "system",
    }
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn assert_or_fill_lineage(
    target: &mut Option<String>,
    compatibility_value: Option<String>,
    field: &str,
) -> Result<(), String> {
    let Some(compatibility_value) = compatibility_value else {
        return Ok(());
    };
    match target {
        Some(existing) if existing != &compatibility_value => Err(format!(
            "typed correlation {field} conflicts with the runtime event"
        )),
        Some(_) => Ok(()),
        None => {
            *target = Some(compatibility_value);
            Ok(())
        },
    }
}

fn capture_gap(
    scope: LlmScope,
    operation: String,
    reason: &str,
    missing_record_count: u64,
    occurred_at_ms: i64,
    seed: &str,
) -> LlmCaptureGap {
    let identity = format!(
        "{}\0{}\0{}\0{}\0{}",
        scope.principal, scope.workspace, operation, reason, seed
    );
    LlmCaptureGap {
        schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
        gap_id: format!("gap-{}", blake3::hash(identity.as_bytes()).to_hex()),
        scope,
        llm_call_id: None,
        operation,
        reason: reason.to_string(),
        missing_record_count,
        first_observed_at_ms: occurred_at_ms,
        last_observed_at_ms: occurred_at_ms,
        emitted_at_ms: occurred_at_ms,
    }
}

fn capture_gap_for_call(
    scope: LlmScope,
    operation: String,
    reason: &str,
    missing_record_count: u64,
    occurred_at_ms: i64,
    llm_call_id: &str,
) -> LlmCaptureGap {
    let mut gap = capture_gap(
        scope,
        operation,
        reason,
        missing_record_count,
        occurred_at_ms,
        llm_call_id,
    );
    gap.llm_call_id = Some(llm_call_id.to_string());
    gap
}

fn broadcast_lag_gap(skipped: u64) -> LlmCaptureGap {
    let occurred_at_ms = chrono::Utc::now().timestamp_millis();
    capture_gap(
        LlmScope::new(DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE),
        "unknown".to_string(),
        BROADCAST_LAGGED,
        skipped,
        occurred_at_ms,
        &ulid::Ulid::new().to_string(),
    )
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::{sync::Mutex, time::Duration};

    async fn wait_for_activation_stats(
        activation: &LlmTraceActivation,
        predicate: impl Fn(&LlmTraceActivationStats) -> bool,
    ) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let stats = activation.stats();
            if predicate(&stats) {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("activation stats did not settle before shutdown: {stats:?}");
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    use super::*;
    use crate::magician_v2::analytics::llm_trace_recorder::{
        LlmToolBranchState, LlmToolLineageOutcome, LlmToolLineageStage, LlmToolSideEffectState,
        LlmTraceRecordError, LlmTraceRecordSink, TypedLlmTraceRecorder,
    };

    /// Every vendor's realtime models live in one model-keyed table, so
    /// pricing them only when the provider happened to be OpenAI left every
    /// Gemini Live call unpriced — a pricing miss and cost Unknown next to a
    /// voice trace that had already computed a real price. A private endpoint
    /// that merely looks like a vendor still inherits nothing.
    #[test]
    fn a_realtime_call_is_priced_by_its_model_not_by_which_vendor_it_came_from() {
        let (gemini, _) = pricing_for_runtime_event(
            "gemini",
            "gemini-3.8-live",
            0,
            0.0,
            true,
            true,
            900,
            200,
            0,
            0,
            0,
            Some(700),
            Some(150),
            Some(0),
            0,
        );
        assert_ne!(
            gemini.pricing_version.as_deref(),
            Some(UNPRICED_VERSION),
            "a Gemini Live call has a realtime row; it must be priced"
        );
        assert_eq!(gemini.cost_source, Some(LlmCostSource::Computed));

        let (private, _) = pricing_for_runtime_event(
            "openai-compatible-private",
            "gemini-3.8-live",
            0,
            0.0,
            true,
            true,
            900,
            200,
            0,
            0,
            0,
            Some(700),
            Some(150),
            Some(0),
            0,
        );
        assert_eq!(
            private.pricing_version.as_deref(),
            Some(UNPRICED_VERSION),
            "a private endpoint never inherits a vendor's realtime price"
        );
    }

    /// A voice model billed by the clock reports seconds and no tokens of its
    /// own. The canonical facts recompute realtime cost from the event's token
    /// buckets — which for such a model carry no information at all, so the
    /// recompute priced a real GPT-Live session at an authoritative $0.00, the
    /// exact false zero the unknown-never-zero rule exists to prevent. The
    /// duration is measured by the producer and nowhere else on the event, so
    /// a duration-billed model keeps the producer's figure — as an estimate,
    /// since the producer may have measured the session itself rather than
    /// been told — and an unusable figure stays unknown.
    #[test]
    fn a_duration_billed_realtime_call_keeps_the_price_its_producer_measured() {
        let (priced, mismatch) = pricing_for_runtime_event(
            "openai",
            "gpt-live-1",
            0,
            0.05,
            true,
            true,
            0,
            0,
            0,
            0,
            0,
            Some(0),
            Some(0),
            Some(0),
            0,
        );
        assert_eq!(priced.cost_source, Some(LlmCostSource::Estimated));
        assert_eq!(priced.cost_usd, Some(0.05));
        assert!(!mismatch, "there is nothing to disagree with");

        for unusable in [f64::NAN, -1.0] {
            let (priced, _) = pricing_for_runtime_event(
                "openai",
                "gpt-live-1",
                0,
                unusable,
                true,
                true,
                0,
                0,
                0,
                0,
                0,
                Some(0),
                Some(0),
                Some(0),
                0,
            );
            assert_eq!(priced.cost_source, Some(LlmCostSource::Unknown));
            assert_eq!(priced.cost_usd, None, "unknown, never zero");
        }

        // A token-priced realtime model still recomputes, and still disagrees
        // with a producer that made a figure up.
        let (tokens, mismatch) = pricing_for_runtime_event(
            "openai",
            "gpt-realtime-2.1",
            0,
            999.0,
            true,
            true,
            0,
            0,
            0,
            0,
            0,
            Some(0),
            Some(0),
            Some(0),
            0,
        );
        assert_eq!(tokens.cost_source, Some(LlmCostSource::Computed));
        assert_eq!(tokens.cost_usd, Some(0.0));
        assert!(mismatch);
    }

    #[derive(Default)]
    struct RecordingSink(Mutex<Vec<LlmTraceRecord>>);

    impl LlmTraceRecordSink for RecordingSink {
        fn try_record(&self, record: LlmTraceRecord) -> Result<(), LlmTraceRecordError> {
            self.0.lock().expect("recording sink lock").push(record);
            Ok(())
        }
    }

    fn response_event(attempts: u32) -> RuntimeTransportEvent {
        let context = LlmTraceContext::new(
            LlmScope::new("owner", "workspace"),
            LlmWorkloadClass::ForegroundChat,
        );
        let receipt = magicllm::LlmTraceReceipt::direct_with_attempt_count(context, attempts);
        let usage = magicllm::TokenUsage {
            prompt_tokens: Some(100),
            completion_tokens: Some(25),
            total_tokens: Some(125),
            reasoning_tokens: Some(3),
            cached_tokens: Some(50),
            cache_creation_tokens: Some(0),
        };
        let cost = magicllm::compute_cost_at(
            &magicllm::LLMProviderKind::OpenAI,
            "gpt-5.6-terra",
            &usage,
            1_000,
        );
        RuntimeTransportEvent::LLMResponseReceived {
            execution_id: "execution-1".to_string(),
            principal: Some("owner".to_string()),
            workspace: Some("workspace".to_string()),
            correlation: Some(LlmEventCorrelation::from(&receipt)),
            plan_id: String::new(),
            step_id: None,
            step_index: None,
            capability: "chat".to_string(),
            success: true,
            decision_summary: String::new(),
            cost,
            latency_ms: 800,
            error: None,
            provider: "openai".to_string(),
            model: "gpt-5.6-terra".to_string(),
            usage_reported: true,
            input_tokens: 100,
            output_tokens: 25,
            reasoning_tokens: 3,
            reasoning_summary: None,
            cache_read_tokens: 50,
            cache_creation_tokens: 0,
            audio_input_tokens: None,
            audio_output_tokens: None,
            audio_cached_tokens: None,
            search_calls: 0,
            ttft_ms: Some(200),
            task_id: None,
            agent_id: None,
            delegated_agent_id: None,
            chat_session_id: Some("session-1".to_string()),
            operation: "chat_completion".to_string(),
            profile: Some("fast".to_string()),
            attempt: 1,
            response_kind: "text".to_string(),
            started_at_ms: 1_000,
            timestamp: 1_800,
        }
    }

    /// The same successful response, issued under a named activity span.
    ///
    /// Built by stamping the trace context BEFORE the receipt is taken, which
    /// is how production reaches it: `LlmJob::new` fills the context from the
    /// origin and the worker builds the receipt from that context. Setting the
    /// field on the correlation afterwards would test a shape no producer
    /// creates.
    fn response_event_under_activity(activity_id: &str) -> RuntimeTransportEvent {
        let mut context = LlmTraceContext::new(
            LlmScope::new("owner", "workspace"),
            LlmWorkloadClass::ForegroundChat,
        );
        context.activity_id = Some(activity_id.to_string());
        let receipt = magicllm::LlmTraceReceipt::direct_with_attempt_count(context, 1);
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived { correlation, .. } = &mut event {
            *correlation = Some(LlmEventCorrelation::from(&receipt));
        }
        event
    }

    fn test_broadcaster() -> RuntimeTransportBroadcaster {
        RuntimeTransportBroadcaster::new(64)
    }

    /// A `Completed` queue event whose job did local prep under `activity_id`.
    ///
    /// `Completed` on purpose: it is the branch that returns before the
    /// terminal mapping, so it exercises local prep and nothing else — which is
    /// exactly the case the old wiring dropped on the floor.
    fn local_prep_completed_event(activity_id: &str) -> LlmQueueEvent {
        use magicllm::dispatch::{JobOrigin, LlmJob, LocalPrepStat, TokenSummary};

        let origin =
            JobOrigin::op("local_prep_host").with_activity_id(Some(activity_id.to_string()));
        let (job, _rx) = LlmJob::new(magicllm::LLMRequest::default(), origin);
        let mut meta = JobMeta::pending_from(&job);

        let mut call_context = LlmTraceContext::new(
            LlmScope::new("owner", "workspace"),
            LlmWorkloadClass::System,
        );
        call_context.call_role = LlmCallRole::Summarizer;
        call_context.activity_id = Some(activity_id.to_string());
        let provider_attempt_id = call_context.provider_attempt_id(1);
        meta.local_prep = Some(LocalPrepStat {
            blocks_processed: 1,
            chars_in: 4_096,
            chars_out: 512,
            model: "qwen3:8b".to_string(),
            duration_ms: 40,
            calls: vec![LocalPrepCallStat {
                trace_context: call_context,
                provider_attempt_id,
                provider_attempt_count: 1,
                provider: "ollama".to_string(),
                model: "qwen3:8b".to_string(),
                purpose: "tool_result".to_string(),
                started_at_ms: 1_000,
                completed_at_ms: 1_040,
                latency_ms: 40,
                success: true,
                error_class: None,
                tokens: Some(TokenSummary {
                    prompt_tokens: 900,
                    completion_tokens: 80,
                    cached_tokens: 0,
                    reasoning_tokens: 0,
                }),
            }],
        });
        LlmQueueEvent::Completed { meta }
    }

    fn correlation_context(correlation: &LlmEventCorrelation) -> LlmTraceContext {
        context_from_correlation(correlation, LlmScope::new("owner", "workspace"))
            .expect("valid test correlation")
    }

    fn drain_activity_cost(
        receiver: &mut broadcast::Receiver<RuntimeTransportEvent>,
    ) -> Option<RuntimeTransportEvent> {
        while let Ok(event) = receiver.try_recv() {
            if matches!(event, RuntimeTransportEvent::ActivityCost { .. }) {
                return Some(event);
            }
        }
        None
    }

    /// THE TEST THAT WAS MISSING. A successful call publishes a cost carrying
    /// the `activity_id` it was issued under — **through the real entry
    /// point**.
    ///
    /// The four tests this replaces called `emit_activity_cost` directly with
    /// hand-built structs. They passed for the whole time the event was
    /// unreachable, because nothing they touched could tell them that the only
    /// caller of the emit was the terminal-FAILURE bridge, which never has a
    /// price. A test that constructs the thing under test cannot observe
    /// whether production reaches it.
    ///
    /// So this drives `record_runtime_event` — the function the response
    /// bridge actually calls — with a broadcaster subscribed, and asserts the
    /// cost arrives. It fails if the emit is ever orphaned again: unhook it
    /// from the successful path and nothing lands on the bus.
    ///
    /// It asserts the ID, not the number. The number is the provider's and will
    /// move with the rate card; the id is the only thing that makes a cost
    /// attributable, and when it drifts the failure is silent — the view shows
    /// lanes that group correctly and costs that attach to nothing, which looks
    /// exactly like "no calls happened".
    #[test]
    fn a_successful_dispatch_publishes_a_cost_through_the_real_entry_point() {
        let broadcaster = test_broadcaster();
        let mut receiver = broadcaster.subscribe();
        let sink = Arc::new(RecordingSink::default());
        let recorder = TypedLlmTraceRecorder::new(sink.clone());
        let metrics = ActivationMetrics::default();

        record_runtime_event(
            response_event_under_activity("activity-77"),
            &recorder,
            &metrics,
            &broadcaster,
        );

        let event = drain_activity_cost(&mut receiver)
            .expect("a successful priced call must publish a cost on the bus");
        let RuntimeTransportEvent::ActivityCost {
            activity_id,
            commodity,
            cost_microunits,
            principal,
            workspace,
            ..
        } = event
        else {
            unreachable!("filtered to ActivityCost above");
        };
        assert_eq!(
            activity_id, "activity-77",
            "the join key must survive the correlation round trip — it is the \
             only thing that says which unit of work this spend belongs to"
        );
        assert_eq!(commodity, "usd");
        assert!(
            cost_microunits > 0,
            "a priced call publishes what the rate card computed, never a zero"
        );
        // Scope rides along so the websocket router can decide who may see it;
        // without it the only options are everyone or nobody.
        assert_eq!(principal.as_deref(), Some("owner"));
        assert_eq!(workspace.as_deref(), Some("workspace"));
    }

    /// The durable record carries the join key too, not only the live event.
    ///
    /// `context_from_correlation` hard-coded `activity_id: None`, so every
    /// successful call's `LlmCallCompleted` — the row that owns the price in
    /// parquet — knew what the call cost and not which unit of work it was.
    /// The retrospective join was as broken as the live one, and only this
    /// assertion notices, because the live event would still look right if the
    /// id were re-derived somewhere else on the way out.
    #[test]
    fn the_durable_completed_record_carries_the_activity_too() {
        let sink = Arc::new(RecordingSink::default());
        let recorder = TypedLlmTraceRecorder::new(sink.clone());
        let metrics = ActivationMetrics::default();

        record_runtime_event(
            response_event_under_activity("activity-77"),
            &recorder,
            &metrics,
            &test_broadcaster(),
        );

        let recorded = sink.0.lock().expect("recording sink lock");
        let completed = recorded
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CallCompleted(completed) => Some(completed),
                _ => None,
            })
            .expect("a successful response records a completed call");
        assert_eq!(
            completed.context.activity_id.as_deref(),
            Some("activity-77")
        );
    }

    /// A malformed join key costs the join key, not the record.
    ///
    /// `context.is_valid()` is checked immediately after
    /// `context_from_correlation`, and a `false` there makes the whole envelope
    /// a `MappingFailure` — a `CaptureGap` row instead of the priced
    /// `LlmCallCompleted`. For an already-completed, already-paid-for call that
    /// discards the *price* in order to punish the *label*.
    ///
    /// So the id is normalised on reconstruction and an unusable one becomes
    /// absent, which every consumer already handles. Asserting the completed
    /// record lands is the point: asserting only that `activity_id` is `None`
    /// would pass just as well if the record had been dropped entirely.
    #[test]
    fn a_malformed_activity_id_on_the_wire_keeps_the_priced_record() {
        let broadcaster = test_broadcaster();
        let mut receiver = broadcaster.subscribe();
        let sink = Arc::new(RecordingSink::default());
        let recorder = TypedLlmTraceRecorder::new(sink.clone());
        let metrics = ActivationMetrics::default();

        record_runtime_event(
            response_event_under_activity("   "),
            &recorder,
            &metrics,
            &broadcaster,
        );

        let recorded = sink.0.lock().expect("recording sink lock");
        let completed = recorded
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CallCompleted(completed) => Some(completed),
                _ => None,
            })
            .expect("a malformed activity id must not cost the priced completion record");
        assert_eq!(
            completed.context.activity_id, None,
            "an id that joins against nothing must read as absent, not as present"
        );
        assert!(
            completed.pricing.cost_usd.is_some(),
            "the price is what the record exists for and must survive the bad label"
        );
        assert!(
            !recorded
                .iter()
                .any(|record| matches!(record, LlmTraceRecord::CaptureGap(_))),
            "a telemetry field must degrade, never turn a successful call into a gap"
        );
        drop(recorded);
        assert!(
            drain_activity_cost(&mut receiver).is_none(),
            "and with no activity to attach it to, no cost is published"
        );
    }

    /// A call outside any instrumented span says nothing.
    ///
    /// There is no row in the view to attach a cost to. Emitting with an empty
    /// or invented id would attach spend to unrelated work.
    #[test]
    fn a_call_with_no_activity_publishes_no_cost() {
        let broadcaster = test_broadcaster();
        let mut receiver = broadcaster.subscribe();
        let sink = Arc::new(RecordingSink::default());
        let recorder = TypedLlmTraceRecorder::new(sink);
        let metrics = ActivationMetrics::default();

        // `response_event` declares no activity — the ordinary shape for work
        // issued outside any instrumented span.
        record_runtime_event(response_event(1), &recorder, &metrics, &broadcaster);

        assert!(
            drain_activity_cost(&mut receiver).is_none(),
            "a call with no activity has nothing to attach spend to"
        );
    }

    /// An unpriced call emits NOTHING — never a zero.
    ///
    /// Zero reads as "this was free"; absent reads as "we do not know what this
    /// cost". A pricing outage that reported zeros would look like a windfall.
    ///
    /// Driven end to end by withholding the provider's usage: without usage
    /// there is nothing to price from, so `pricing_for_runtime_event` returns
    /// `LlmCostSource::Unknown` with no `cost_usd`. That is the real production
    /// shape of "unpriced", not a struct literal standing in for one.
    #[test]
    fn an_unpriced_call_publishes_nothing_rather_than_zero() {
        let broadcaster = test_broadcaster();
        let mut receiver = broadcaster.subscribe();
        let sink = Arc::new(RecordingSink::default());
        let recorder = TypedLlmTraceRecorder::new(sink);
        let metrics = ActivationMetrics::default();

        let mut event = response_event_under_activity("activity-78");
        if let RuntimeTransportEvent::LLMResponseReceived {
            usage_reported,
            input_tokens,
            output_tokens,
            reasoning_tokens,
            cache_read_tokens,
            cost,
            ..
        } = &mut event
        {
            *usage_reported = false;
            *input_tokens = 0;
            *output_tokens = 0;
            *reasoning_tokens = 0;
            *cache_read_tokens = 0;
            *cost = 0.0;
        }

        record_runtime_event(event, &recorder, &metrics, &broadcaster);

        assert!(
            drain_activity_cost(&mut receiver).is_none(),
            "an unknown cost must not be published as zero dollars"
        );
    }

    /// Local work is reported in its own commodity, not as zero dollars.
    ///
    /// Both render as "0", but `usd 0` says a vendor charged nothing this time
    /// while `local` says there was no vendor. Read from `cost_source`, never
    /// inferred from the zero itself — inferring would relabel a genuinely free
    /// paid call as local.
    ///
    /// Driven through `record_dispatch_event`, because `map_local_prep_call` is
    /// the ONLY producer of `LlmCostSource::Local` and its records used to go
    /// out through a third submit function with no broadcaster at all. If that
    /// wiring is undone, the `local` commodity silently disappears from the
    /// view and locally-run work becomes indistinguishable from unpriced work.
    #[test]
    fn local_prep_publishes_its_own_commodity_through_the_real_entry_point() {
        let broadcaster = test_broadcaster();
        let mut receiver = broadcaster.subscribe();
        let sink = Arc::new(RecordingSink::default());
        let recorder = TypedLlmTraceRecorder::new(sink);
        let metrics = ActivationMetrics::default();

        record_dispatch_event(
            local_prep_completed_event("activity-79"),
            &recorder,
            &metrics,
            &broadcaster,
        );

        let event = drain_activity_cost(&mut receiver)
            .expect("a local-prep call still publishes — in its own commodity");
        let RuntimeTransportEvent::ActivityCost {
            activity_id,
            commodity,
            cost_microunits,
            ..
        } = event
        else {
            unreachable!("filtered to ActivityCost above");
        };
        assert_eq!(activity_id, "activity-79");
        assert_eq!(commodity, "local");
        assert_eq!(cost_microunits, 0);
    }

    fn tool_lineage_event() -> RuntimeTransportEvent {
        let context = LlmTraceContext::new(
            LlmScope::new("owner", "workspace"),
            LlmWorkloadClass::InteractiveTask,
        );
        let record = LlmToolLineageRecord {
            schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
            tool_execution_id: format!("{}:tool:call_1", context.llm_call_id),
            context,
            model_tool_call_id: "call_1".to_string(),
            branch_id: "branch_1".to_string(),
            operation: "agentic_decision".to_string(),
            source_surface: "interactive_task".to_string(),
            tool_name: "browser__click".to_string(),
            tool_family: Some("browser".to_string()),
            stage: LlmToolLineageStage::Proposed,
            stage_index: 0,
            occurred_at_ms: 10,
            observed_at_ms: 11,
            arguments_fingerprint: Some("a".repeat(64)),
            result_ref: None,
            canonical_event_ref: None,
            related_execution_ids: Vec::new(),
            consumed_by_call_id: None,
            name_known: None,
            arguments_parsed: None,
            schema_matched: None,
            policy_allowed: None,
            approval_required: None,
            approval_obtained: None,
            transport_ran: None,
            tool_reported_success: None,
            result_validation_success: None,
            outcome: LlmToolLineageOutcome::Pending,
            failure_owner: None,
            failure_code: None,
            side_effect_state: LlmToolSideEffectState::None,
            branch_state: LlmToolBranchState::Active,
            on_successful_path: None,
            same_tool_arguments_count: 1,
            observation_action_cycle_count: 0,
            recovered_after_failure: false,
            linkage_gap: None,
        };
        RuntimeTransportEvent::AgentEvent {
            event: AgentEventEnvelope::new_scoped(
                LLM_TOOL_LINEAGE_EVENT_TYPE,
                "personal-assistant",
                "owner",
                "workspace",
                serde_json::to_value(record).expect("lineage payload"),
            ),
        }
    }

    #[test]
    fn tool_lineage_transport_maps_to_one_valid_canonical_record() {
        let sink = Arc::new(RecordingSink::default());
        let recorder = TypedLlmTraceRecorder::new(sink.clone());
        let metrics = ActivationMetrics::default();
        record_runtime_event(
            tool_lineage_event(),
            &recorder,
            &metrics,
            &test_broadcaster(),
        );
        let recorded = sink.0.lock().expect("recording sink lock");
        assert!(matches!(
            recorded.as_slice(),
            [LlmTraceRecord::ToolLineage(_)]
        ));
        assert_eq!(metrics.tool_lineage_events_seen.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.records_accepted.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn tool_lineage_scope_mismatch_becomes_an_explicit_capture_gap() {
        let mut event = tool_lineage_event();
        if let RuntimeTransportEvent::AgentEvent { event } = &mut event {
            event.workspace = Some("other-workspace".to_string());
        }
        let sink = Arc::new(RecordingSink::default());
        let recorder = TypedLlmTraceRecorder::new(sink.clone());
        let metrics = ActivationMetrics::default();
        record_runtime_event(event, &recorder, &metrics, &test_broadcaster());
        let recorded = sink.0.lock().expect("recording sink lock");
        assert!(matches!(
            recorded.as_slice(),
            [LlmTraceRecord::CaptureGap(gap)] if gap.reason == "tool_lineage_scope_mismatch"
        ));
    }

    #[test]
    fn declared_agentic_mapping_gap_preserves_machine_reason_and_call_owner() {
        let mut event = tool_lineage_event();
        let expected_call_id = if let RuntimeTransportEvent::AgentEvent { event } = &mut event {
            let context = event.payload["context"].clone();
            let llm_call_id = context["llm_call_id"]
                .as_str()
                .expect("fixture call id")
                .to_string();
            event.payload = serde_json::json!({
                "mapping_gap": "tool_lineage_provider_tool_call_index_missing",
                "context": context,
            });
            llm_call_id
        } else {
            unreachable!("tool-lineage fixture")
        };
        let sink = Arc::new(RecordingSink::default());
        let recorder = TypedLlmTraceRecorder::new(sink.clone());
        let metrics = ActivationMetrics::default();
        record_runtime_event(event, &recorder, &metrics, &test_broadcaster());
        let recorded = sink.0.lock().expect("recording sink lock");
        assert!(matches!(
            recorded.as_slice(),
            [LlmTraceRecord::CaptureGap(gap)]
                if gap.reason == "tool_lineage_provider_tool_call_index_missing"
                    && gap.llm_call_id.as_deref() == Some(expected_call_id.as_str())
        ));
        assert_eq!(metrics.gap_records_emitted.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn completed_response_maps_to_call_and_final_attempt_revisions() {
        let records = map_response_event(&response_event(1)).expect("map response");
        assert_eq!(records.len(), 3);
        assert!(matches!(records[0], LlmTraceRecord::CallStarted(_)));
        let LlmTraceRecord::ProviderAttempt(attempt) = &records[1] else {
            panic!("expected provider attempt");
        };
        assert_eq!(attempt.provider_attempt_index, 1);
        assert_eq!(attempt.timing.ttft_ms, Some(200));
        assert_eq!(attempt.timing.generation_after_ttft_ms, Some(600));
        assert_eq!(attempt.usage.total_tokens, Some(125));
        let expected_cost = magicllm::compute_cost_at(
            &magicllm::LLMProviderKind::OpenAI,
            "gpt-5.6-terra",
            &magicllm::TokenUsage {
                prompt_tokens: Some(100),
                completion_tokens: Some(25),
                total_tokens: Some(125),
                reasoning_tokens: Some(3),
                cached_tokens: Some(50),
                cache_creation_tokens: Some(0),
            },
            1_000,
        );
        assert_eq!(attempt.pricing.cost_usd, Some(expected_cost));
        assert!(matches!(records[2], LlmTraceRecord::CallCompleted(_)));
        for record in records {
            record.validate().expect("valid canonical revision");
        }
    }

    #[test]
    fn successful_response_without_effective_route_keeps_call_and_gaps_attempt() {
        let mut event = response_event(1);
        let expected_call_id = if let RuntimeTransportEvent::LLMResponseReceived {
            correlation,
            provider,
            model,
            ..
        } = &mut event
        {
            provider.clear();
            model.clear();
            correlation
                .as_ref()
                .expect("correlation")
                .llm_call_id
                .clone()
        } else {
            unreachable!("response fixture")
        };

        let records = map_response_event(&event).expect("route-missing success");
        assert!(!records
            .iter()
            .any(|record| matches!(record, LlmTraceRecord::ProviderAttempt(_))));
        let call = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CallCompleted(value) => Some(value),
                _ => None,
            })
            .expect("completed logical call");
        assert_eq!(call.context.llm_call_id, expected_call_id);
        assert_eq!(call.terminal_state, LlmCallTerminalState::Succeeded);
        assert_eq!(call.usage.total_tokens, Some(125));
        assert_eq!(call.pricing, LlmPricingFact::default());
        let gap = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CaptureGap(value) => Some(value),
                _ => None,
            })
            .expect("call-owned terminal attempt gap");
        assert_eq!(gap.llm_call_id.as_deref(), Some(expected_call_id.as_str()));
        assert_eq!(gap.reason, TERMINAL_ATTEMPTS_UNAVAILABLE);
        assert_eq!(gap.missing_record_count, 1);
        for record in records {
            record.validate().expect("valid route-missing revisions");
        }
    }

    #[test]
    fn execution_native_contract_failure_preserves_successful_attempt_and_usage() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived {
            error,
            response_kind,
            ..
        } = &mut event
        {
            *error = Some("Execution-native response contained zero tool calls".to_string());
            *response_kind = "validation_error:zero_tool_calls".to_string();
        }

        let records = map_response_event(&event).expect("contract validation response");
        let attempt = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::ProviderAttempt(value) => Some(value),
                _ => None,
            })
            .expect("successful provider attempt");
        let call = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CallCompleted(value) => Some(value),
                _ => None,
            })
            .expect("completed call");
        assert_eq!(
            attempt.terminal_state,
            Some(LlmAttemptTerminalState::Succeeded)
        );
        assert_eq!(attempt.usage.total_tokens, Some(125));
        assert_eq!(call.terminal_state, LlmCallTerminalState::Succeeded);
        assert!(call.validation.contract_validation_attempted);
        assert_eq!(call.validation.contract_validation_success, Some(false));
        assert_eq!(
            call.validation.validation_error_class.as_deref(),
            Some("zero_tool_calls")
        );
        assert!(call.validation.discarded_before_use);
        assert!(!records
            .iter()
            .any(|record| matches!(record, LlmTraceRecord::CaptureGap(_))));
        for record in records {
            record.validate().expect("valid canonical revision");
        }
    }

    #[test]
    fn explicit_validation_success_is_not_inferred_from_transport_alone() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived { response_kind, .. } = &mut event {
            *response_kind = "validation_success:query_analysis_json".to_string();
        }
        let records = map_response_event(&event).expect("mapped");
        let completed = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CallCompleted(call) => Some(call),
                _ => None,
            })
            .expect("completed call");
        assert!(completed.validation.contract_validation_attempted);
        assert_eq!(completed.validation.contract_validation_success, Some(true));
        assert!(completed.validation.validation_error_class.is_none());
    }

    #[test]
    fn successful_transport_cannot_hide_an_unclassified_error() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived { error, .. } = &mut event {
            *error = Some("unexpected response problem".to_string());
        }
        let gap = map_response_event(&event)
            .expect_err("successful error needs validation classification")
            .into_gap();
        assert_eq!(
            gap.reason,
            format!("{MAPPING_REJECTED}:unexpected_success_error")
        );
    }

    #[test]
    fn response_categories_reject_content_and_mapping_gap_reasons_are_fixed() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived { response_kind, .. } = &mut event {
            *response_kind = "validation_error:private user text".to_string();
        }
        let failure = map_response_event(&event)
            .expect_err("response category prose must fail at the mapping boundary");
        let gap = failure.into_gap();

        assert_eq!(
            gap.reason,
            format!("{MAPPING_REJECTED}:invalid_response_category")
        );
        LlmTraceRecord::CaptureGap(gap.clone())
            .validate()
            .expect("bounded content-free mapping gap");
        assert_eq!(
            mapping_reason_code(&"private ".repeat(1_000)),
            "mapping_validation_failed"
        );
        assert_eq!(
            mapping_reason_code(
                "invalid canonical records: serialized record exceeds 16384-byte limit"
            ),
            "record_oversize"
        );

        let mut oversized_operation = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived { operation, .. } =
            &mut oversized_operation
        {
            *operation = "private ".repeat(4_000);
        }
        let gap = map_response_event(&oversized_operation)
            .expect_err("content-bearing operation must be rejected")
            .into_gap();
        assert_eq!(gap.operation, "unknown");
        LlmTraceRecord::CaptureGap(gap)
            .validate()
            .expect("fallback gap remains bounded and persistable");
    }

    #[test]
    fn unavailable_earlier_attempts_are_counted_not_fabricated() {
        let records = map_response_event(&response_event(3)).expect("map response");
        assert_eq!(
            records
                .iter()
                .filter(|record| matches!(record, LlmTraceRecord::ProviderAttempt(_)))
                .count(),
            1
        );
        let gap = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CaptureGap(gap) if gap.reason == EARLIER_ATTEMPTS_UNAVAILABLE => {
                    Some(gap)
                },
                _ => None,
            })
            .expect("attempt gap");
        assert_eq!(gap.missing_record_count, 2);
        assert!(gap
            .llm_call_id
            .as_deref()
            .is_some_and(|call_id| gap.gap_id.contains("gap-") && !call_id.is_empty()));
    }

    #[test]
    fn reused_response_does_not_materialize_a_second_lifecycle() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived {
            correlation,
            provider,
            started_at_ms,
            execution_id,
            plan_id,
            step_id,
            task_id,
            chat_session_id,
            ..
        } = &mut event
        {
            let mut owner = correlation_context(correlation.as_ref().expect("correlation"));
            owner.execution_id = Some("owner-execution".to_string());
            owner.plan_id = Some("owner-plan".to_string());
            owner.step_id = Some("owner-step".to_string());
            owner.task_id = Some("owner-task".to_string());
            owner.chat_session_id = Some("owner-session".to_string());
            let mut receipt = magicllm::LlmTraceReceipt::queued(owner, "owner-job", 1);
            receipt.response_reused = true;
            *correlation = Some(LlmEventCorrelation::from(&receipt));
            // Subscriber lineage is intentionally unrelated to the owner. A
            // reused response must not be rejected as a cross-lineage call.
            *execution_id = "subscriber-execution".to_string();
            *plan_id = "subscriber-plan".to_string();
            *step_id = Some("subscriber-step".to_string());
            *task_id = Some("subscriber-task".to_string());
            *chat_session_id = Some("subscriber-session".to_string());
            provider.clear();
            *started_at_ms = 0;
        }
        assert!(map_response_event(&event)
            .expect("reused response is valid")
            .is_empty());

        let sink = Arc::new(RecordingSink::default());
        let recorder = TypedLlmTraceRecorder::new(sink.clone());
        let metrics = ActivationMetrics::default();
        record_runtime_event(event, &recorder, &metrics, &test_broadcaster());
        assert!(sink.0.lock().expect("recording sink lock").is_empty());
        assert_eq!(metrics.response_events_seen.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.reused_responses_ignored.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn reused_response_without_owner_dispatch_identity_is_rejected() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived { correlation, .. } = &mut event {
            correlation.as_mut().expect("correlation").response_reused = true;
        }
        let gap = map_response_event(&event)
            .expect_err("direct receipt cannot represent reuse")
            .into_gap();
        assert!(gap.reason.contains("owner_dispatch_job_id"));
    }

    #[test]
    fn opaque_external_run_does_not_fabricate_a_provider_attempt() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived {
            capability,
            operation,
            response_kind,
            correlation,
            ..
        } = &mut event
        {
            *capability = "coding".to_string();
            *operation = "coding".to_string();
            *response_kind = "external_ai_run".to_string();
            let context = context_from_correlation(
                correlation.as_ref().expect("correlation"),
                LlmScope::new("owner", "workspace"),
            )
            .expect("typed context");
            *correlation = Some(LlmEventCorrelation::from(
                &magicllm::LlmTraceReceipt::direct_with_attempt_count(context, 0),
            ));
        }
        assert!(map_response_event(&event)
            .expect("external aggregate is intentionally excluded")
            .is_empty());
        let sink = Arc::new(RecordingSink::default());
        let recorder = TypedLlmTraceRecorder::new(sink.clone());
        let metrics = ActivationMetrics::default();
        record_runtime_event(event, &recorder, &metrics, &test_broadcaster());
        assert!(sink.0.lock().expect("recording sink lock").is_empty());
        assert_eq!(
            metrics
                .external_aggregate_events_ignored
                .load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn external_run_claiming_a_physical_attempt_fails_closed() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived {
            capability,
            operation,
            response_kind,
            ..
        } = &mut event
        {
            *capability = "coding".to_string();
            *operation = "coding".to_string();
            *response_kind = "external_ai_run".to_string();
        }
        assert!(map_response_event(&event)
            .expect_err("opaque aggregate cannot claim an attempt")
            .into_gap()
            .reason
            .contains("invalid_external_aggregate"));
    }

    #[test]
    fn malformed_or_unscoped_events_fail_to_one_explicit_gap() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived { correlation, .. } = &mut event {
            *correlation = None;
        }
        let failure = map_response_event(&event).expect_err("missing correlation");
        let gap = failure.into_gap();
        LlmTraceRecord::CaptureGap(gap.clone())
            .validate()
            .expect("valid mapping gap");
        assert!(gap.reason.starts_with(MAPPING_REJECTED));
        assert_eq!(gap.missing_record_count, 1);
    }

    #[test]
    fn missing_or_zero_attempt_identity_is_rejected_instead_of_invented() {
        let mut missing_id = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived { correlation, .. } = &mut missing_id {
            correlation
                .as_mut()
                .expect("correlation")
                .provider_attempt_id = None;
        }
        assert!(map_response_event(&missing_id)
            .expect_err("attempt id is mandatory")
            .into_gap()
            .reason
            .contains("missing_provider_attempt_id"));

        let mut zero_count = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived { correlation, .. } = &mut zero_count {
            correlation
                .as_mut()
                .expect("correlation")
                .provider_attempt_count = 0;
        }
        assert!(map_response_event(&zero_count)
            .expect_err("attempt count is mandatory")
            .into_gap()
            .reason
            .contains("unexpected_provider_attempt_id"));
    }

    #[test]
    fn missing_start_timestamp_is_rejected_instead_of_derived_from_latency() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived { started_at_ms, .. } = &mut event {
            *started_at_ms = 0;
        }
        let gap = map_response_event(&event)
            .expect_err("start timestamp is mandatory")
            .into_gap();
        assert!(gap.reason.contains(INVALID_START));
        assert_eq!(gap.missing_record_count, 1);
    }

    #[test]
    fn direct_and_queued_events_preserve_dispatch_identity_with_fact_parity() {
        let direct = map_response_event(&response_event(1)).expect("direct response");
        let mut queued_event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived { correlation, .. } = &mut queued_event {
            let context = LlmTraceContext::new(
                LlmScope::new("owner", "workspace"),
                LlmWorkloadClass::InteractiveTask,
            );
            *correlation = Some(LlmEventCorrelation::from(
                &magicllm::LlmTraceReceipt::queued(context, "job-1", 1),
            ));
        }
        let queued = map_response_event(&queued_event).expect("queued response");
        let direct_call = direct
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CallCompleted(value) => Some(value),
                _ => None,
            })
            .expect("direct call");
        let queued_call = queued
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CallCompleted(value) => Some(value),
                _ => None,
            })
            .expect("queued call");
        let direct_attempt = direct
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::ProviderAttempt(value) => Some(value),
                _ => None,
            })
            .expect("direct attempt");
        let queued_attempt = queued
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::ProviderAttempt(value) => Some(value),
                _ => None,
            })
            .expect("queued attempt");
        assert_eq!(direct_call.dispatch_job_id, None);
        assert_eq!(queued_call.dispatch_job_id.as_deref(), Some("job-1"));
        assert_eq!(direct_call.usage, queued_call.usage);
        assert_eq!(direct_call.pricing, queued_call.pricing);
        assert_eq!(direct_call.timing.provider_execution_ms, Some(800));
        assert_eq!(queued_call.timing.provider_execution_ms, None);
        assert_eq!(direct_call.timing.latency_ms, queued_call.timing.latency_ms);
        assert_eq!(direct_attempt.timing.provider_execution_ms, Some(800));
        assert_eq!(direct_attempt.timing.latency_ms, Some(800));
        assert_eq!(queued_attempt.timing.started_at_ms, None);
        assert_eq!(queued_attempt.timing.provider_execution_ms, None);
        assert_eq!(queued_attempt.timing.latency_ms, None);
    }

    #[test]
    fn prompt_projection_mode_is_preserved_without_mutating_iteration_identity() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived { correlation, .. } = &mut event {
            let correlation = correlation.as_mut().expect("correlation");
            correlation.iteration_id = Some("execution-1:step-1:7".to_string());
            correlation.prompt_projection_mode = Some("rebootstrap".to_string());
        }

        let records = map_response_event(&event).expect("mapped response");
        let started = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CallStarted(started) => Some(started),
                _ => None,
            })
            .expect("call start");
        assert_eq!(
            started.context.iteration_id.as_deref(),
            Some("execution-1:step-1:7")
        );
        assert_eq!(
            started.prompt_projection_mode.as_deref(),
            Some("rebootstrap")
        );
    }

    #[test]
    fn failure_cancel_and_timeout_states_are_distinct() {
        for (message, expected_call, expected_attempt, expected_class) in [
            (
                "configuration error: missing API key",
                LlmCallTerminalState::Failed,
                LlmAttemptTerminalState::Failed,
                "configuration",
            ),
            (
                "provider rejected request",
                LlmCallTerminalState::Failed,
                LlmAttemptTerminalState::Failed,
                "provider_error",
            ),
            (
                "request cancelled by owner",
                LlmCallTerminalState::Cancelled,
                LlmAttemptTerminalState::Cancelled,
                "cancelled",
            ),
            (
                "provider timed out",
                LlmCallTerminalState::Failed,
                LlmAttemptTerminalState::TimedOut,
                "timeout",
            ),
        ] {
            let mut event = response_event(1);
            if let RuntimeTransportEvent::LLMResponseReceived { success, error, .. } = &mut event {
                *success = false;
                *error = Some(message.to_string());
            }
            let records = map_response_event(&event).expect("failed response");
            let attempt = records
                .iter()
                .find_map(|record| match record {
                    LlmTraceRecord::ProviderAttempt(value) => Some(value),
                    _ => None,
                })
                .expect("known terminal attempt");
            assert_eq!(attempt.terminal_state, Some(expected_attempt));
            assert_eq!(attempt.error_class.as_deref(), Some(expected_class));
            assert_eq!(attempt.provider, "openai");
            assert_eq!(attempt.model, "gpt-5.6-terra");
            let call = records
                .iter()
                .find_map(|record| match record {
                    LlmTraceRecord::CallCompleted(value) => Some(value),
                    _ => None,
                })
                .expect("call");
            assert_eq!(call.terminal_state, expected_call);
            assert_eq!(call.error_class.as_deref(), Some(expected_class));
            assert_eq!(call.usage, LlmTokenUsageFact::default());
            assert_eq!(call.pricing, LlmPricingFact::default());
            assert!(!records.iter().any(|record| matches!(
                record,
                LlmTraceRecord::CaptureGap(gap)
                    if gap.reason == TERMINAL_ATTEMPTS_UNAVAILABLE
                        || gap.reason == EARLIER_ATTEMPTS_UNAVAILABLE
            )));
        }
    }

    #[test]
    fn failed_response_materializes_only_known_terminal_attempt_and_gaps_earlier_ones() {
        let mut event = response_event(3);
        if let RuntimeTransportEvent::LLMResponseReceived { success, error, .. } = &mut event {
            *success = false;
            *error = Some("provider rejected request".to_string());
        }

        let records = map_response_event(&event).expect("failed response");
        let attempt = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::ProviderAttempt(value) => Some(value),
                _ => None,
            })
            .expect("terminal attempt");
        assert_eq!(attempt.provider_attempt_index, 3);
        assert_eq!(attempt.timing.started_at_ms, None);
        assert_eq!(attempt.timing.provider_execution_ms, None);
        assert_eq!(attempt.timing.latency_ms, None);
        let gap = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CaptureGap(value)
                    if value.reason == EARLIER_ATTEMPTS_UNAVAILABLE =>
                {
                    Some(value)
                },
                _ => None,
            })
            .expect("earlier attempts gap");
        assert_eq!(
            gap.llm_call_id.as_deref(),
            Some(attempt.context.llm_call_id.as_str())
        );
        assert_eq!(gap.missing_record_count, 2);
    }

    #[test]
    fn cancelled_dropped_stream_keeps_call_and_marks_unknown_attempt_route_as_gap() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived {
            success,
            error,
            provider,
            model,
            response_kind,
            ..
        } = &mut event
        {
            *success = false;
            *error = Some("cancelled".to_string());
            provider.clear();
            model.clear();
            *response_kind = "cancelled".to_string();
        }

        let records = map_response_event(&event).expect("cancelled response");
        assert!(records.iter().any(|record| matches!(
            record,
            LlmTraceRecord::CallCompleted(call)
                if call.terminal_state == LlmCallTerminalState::Cancelled
                    && call.provider_attempt_count == 1
        )));
        assert!(!records
            .iter()
            .any(|record| matches!(record, LlmTraceRecord::ProviderAttempt(_))));
        assert!(records.iter().any(|record| matches!(
            record,
            LlmTraceRecord::CaptureGap(gap)
                if gap.reason == TERMINAL_ATTEMPTS_UNAVAILABLE
                    && gap.missing_record_count == 1
        )));
    }

    #[test]
    fn failed_dispatch_materializes_effective_terminal_attempt() {
        let mut request = magicllm::LLMRequest::default();
        request.model = "requested-model".to_string();
        request.metadata.operation = "query_analysis".to_string();
        request.metadata.set_trace_context(LlmTraceContext::new(
            LlmScope::new("owner", "workspace"),
            LlmWorkloadClass::InteractiveTask,
        ));
        let (job, _response) = magicllm::dispatch::LlmJob::new(
            request,
            magicllm::dispatch::JobOrigin::op("query_analysis"),
        );
        let mut meta = JobMeta::pending_from(&job);
        meta.state = magicllm::dispatch::JobState::Failed;
        meta.dispatched_at_ms = Some(meta.submitted_at_ms + 10);
        meta.completed_at_ms = Some(meta.submitted_at_ms + 70);
        meta.wait_ms = Some(10);
        meta.execution_ms = Some(60);
        meta.provider_attempt_count = 2;
        meta.provider_attempt_id = Some(meta.trace_context.provider_attempt_id(2));
        meta.profile = Some("fallback-profile".to_string());
        meta.provider = Some(magicllm::LLMProviderKind::Anthropic);
        meta.model = Some("fallback-model".to_string());
        meta.error_class = Some(DispatchErrorClass::Provider4xx);

        let records = map_terminal_dispatch(
            &meta,
            LlmCallTerminalState::Failed,
            Some(DispatchErrorClass::Provider4xx),
        )
        .expect("failed dispatch");
        let attempt = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::ProviderAttempt(value) => Some(value),
                _ => None,
            })
            .expect("known final attempt");
        assert_eq!(attempt.provider_attempt_index, 2);
        assert_eq!(
            attempt.effective_profile.as_deref(),
            Some("fallback-profile")
        );
        assert_eq!(attempt.provider, "anthropic");
        assert_eq!(attempt.model, "fallback-model");
        assert_eq!(attempt.error_class.as_deref(), Some("provider_4xx"));
        assert_eq!(attempt.timing.started_at_ms, None);
        assert_eq!(attempt.timing.provider_execution_ms, None);
        assert_eq!(attempt.timing.latency_ms, None);
        assert!(records.iter().any(|record| matches!(
            record,
            LlmTraceRecord::CaptureGap(gap)
                if gap.reason == EARLIER_ATTEMPTS_UNAVAILABLE
                    && gap.missing_record_count == 1
        )));
        assert!(!records.iter().any(|record| matches!(
            record,
            LlmTraceRecord::CaptureGap(gap)
                if gap.reason == TERMINAL_ATTEMPTS_UNAVAILABLE
        )));
    }

    #[test]
    fn pre_provider_failure_materializes_call_without_attempt_or_gap() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived {
            success,
            error,
            provider,
            model,
            correlation,
            ..
        } = &mut event
        {
            *success = false;
            *error = Some("configuration error: missing API key".to_string());
            provider.clear();
            model.clear();
            let context = correlation_context(correlation.as_ref().expect("correlation"));
            *correlation = Some(LlmEventCorrelation::from(
                &magicllm::LlmTraceReceipt::direct_with_attempt_count(context, 0),
            ));
        }

        let records = map_response_event(&event).expect("pre-provider failure");
        assert_eq!(records.len(), 2);
        assert!(matches!(records[0], LlmTraceRecord::CallStarted(_)));
        assert!(!records.iter().any(|record| matches!(
            record,
            LlmTraceRecord::ProviderAttempt(_) | LlmTraceRecord::CaptureGap(_)
        )));
        let call = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CallCompleted(value) => Some(value),
                _ => None,
            })
            .expect("completed call");
        assert_eq!(call.provider_attempt_count, 0);
        assert_eq!(call.terminal_state, LlmCallTerminalState::Failed);
        assert_eq!(call.usage, LlmTokenUsageFact::default());
        assert_eq!(call.pricing, LlmPricingFact::default());
        for record in records {
            record.validate().expect("valid canonical revision");
        }
    }

    #[test]
    fn failed_response_without_error_is_rejected_instead_of_inventing_classification() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived { success, error, .. } = &mut event {
            *success = false;
            *error = None;
        }
        let gap = map_response_event(&event)
            .expect_err("failure requires error evidence")
            .into_gap();
        assert!(gap.reason.contains("missing_failure_error"));
    }

    #[test]
    fn missing_runtime_scope_never_materializes_under_a_fabricated_call_scope() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived {
            principal,
            workspace,
            ..
        } = &mut event
        {
            *principal = None;
            *workspace = None;
        }
        let gap = map_response_event(&event)
            .expect_err("scope is mandatory")
            .into_gap();
        assert!(gap.reason.contains("missing_authoritative_scope"));
        assert_eq!(gap.scope.principal, DEFAULT_SCOPE_PRINCIPAL);
        assert_eq!(gap.scope.workspace, DEFAULT_SCOPE_WORKSPACE);
    }

    #[test]
    fn duplicate_delivery_has_identical_idempotency_keys_and_payloads() {
        let event = response_event(1);
        let first = map_response_event(&event).expect("first map");
        let second = map_response_event(&event).expect("second map");
        assert_eq!(first, second);
        assert_eq!(
            first.iter().map(LlmTraceRecord::key).collect::<Vec<_>>(),
            second.iter().map(LlmTraceRecord::key).collect::<Vec<_>>()
        );
    }

    #[test]
    fn recorder_bridge_preserves_order_and_acceptance_accounting() {
        let sink = Arc::new(RecordingSink::default());
        let recorder = TypedLlmTraceRecorder::new(sink.clone());
        let metrics = ActivationMetrics::default();
        record_runtime_event(response_event(1), &recorder, &metrics, &test_broadcaster());
        let recorded = sink.0.lock().expect("recording sink lock");
        assert_eq!(recorded.len(), 3);
        assert_eq!(metrics.response_events_seen.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.records_accepted.load(Ordering::Relaxed), 3);
        assert_eq!(metrics.records_rejected.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn mapped_record_validation_fails_as_one_gap_without_partial_lifecycle() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived { profile, .. } = &mut event {
            *profile = Some(" invalid profile ".to_string());
        }
        let sink = Arc::new(RecordingSink::default());
        let recorder = TypedLlmTraceRecorder::new(sink.clone());
        let metrics = ActivationMetrics::default();

        record_runtime_event(event, &recorder, &metrics, &test_broadcaster());

        let recorded = sink.0.lock().expect("recording sink lock");
        assert_eq!(recorded.len(), 1);
        assert!(matches!(
            &recorded[0],
            LlmTraceRecord::CaptureGap(gap)
                if gap.reason.contains("invalid_canonical_records")
        ));
        assert_eq!(metrics.records_accepted.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.records_rejected.load(Ordering::Relaxed), 0);
        assert_eq!(metrics.gap_records_emitted.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn shutdown_drain_accounts_for_lagged_broadcast_events() {
        let (sender, mut receiver) = broadcast::channel(1);
        let sink = Arc::new(RecordingSink::default());
        let recorder: Arc<dyn LlmTraceRecorder> =
            Arc::new(TypedLlmTraceRecorder::new(sink.clone()));
        let metrics = Arc::new(ActivationMetrics::default());
        sender.send(response_event(1)).expect("first event");
        sender.send(response_event(1)).expect("second event");
        let cancel = CancellationToken::new();
        cancel.cancel();

        run_activation_bridge(
            &mut receiver,
            recorder,
            cancel,
            metrics.clone(),
            Arc::new(RuntimeTransportBroadcaster::new(16)),
        )
        .await;

        assert_eq!(metrics.broadcast_events_lost.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.gap_records_emitted.load(Ordering::Relaxed), 1);
        let recorded = sink.0.lock().expect("recording sink lock");
        assert!(recorded.iter().any(|record| matches!(
            record,
            LlmTraceRecord::CaptureGap(gap) if gap.reason == BROADCAST_LAGGED
                && gap.missing_record_count == 1
        )));
        assert_eq!(
            recorded
                .iter()
                .filter(|record| matches!(record, LlmTraceRecord::CallCompleted(_)))
                .count(),
            1
        );
    }

    #[test]
    fn dispatch_lag_is_counted_as_both_transport_loss_and_an_emitted_gap() {
        let sink = Arc::new(RecordingSink::default());
        let recorder = TypedLlmTraceRecorder::new(sink.clone());
        let metrics = ActivationMetrics::default();

        record_dispatch_lag(3, &recorder, &metrics);

        assert_eq!(metrics.dispatch_events_lost.load(Ordering::Relaxed), 3);
        assert_eq!(metrics.gap_records_emitted.load(Ordering::Relaxed), 1);
        let recorded = sink.0.lock().expect("recording sink lock");
        assert!(matches!(
            recorded.as_slice(),
            [LlmTraceRecord::CaptureGap(gap)]
                if gap.reason == DISPATCH_BROADCAST_LAGGED
                    && gap.missing_record_count == 3
        ));
    }

    #[tokio::test]
    async fn activation_restart_duplicate_and_shutdown_are_durable() {
        let temp = crate::magician_v2::analytics::llm_trace_journal::canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let config = LlmTraceActivationConfig {
            journal: LlmTraceJournalConfig {
                flush_interval: Duration::from_secs(60),
                batch_row_threshold: 100,
                ..LlmTraceJournalConfig::default()
            },
            ..LlmTraceActivationConfig::default()
        };
        let activation =
            LlmTraceActivation::start(Arc::clone(&broadcaster), workspace.clone(), config.clone())
                .expect("first activation");
        let event = response_event(1);
        broadcaster.emit_transport_only(event.clone());
        broadcaster.emit_transport_only(event);
        wait_for_activation_stats(&activation, |stats| {
            stats.response_events_seen >= 2 && stats.records_accepted > 0
        })
        .await;
        let first = activation.shutdown().await;
        assert!(!first.durable_pipeline.timed_out);
        assert_eq!(first.durable_pipeline.remaining_buffered_records, 0);

        let restarted =
            LlmTraceActivation::start(broadcaster, workspace, config).expect("restart activation");
        let second = restarted.shutdown().await;
        assert!(!second.durable_pipeline.timed_out);
        assert_eq!(second.durable_pipeline.remaining_buffered_records, 0);
    }

    #[test]
    fn terminal_usage_harness_estimates_preserve_unknown_buckets_without_attempts() {
        for (reported, succeeded) in [(false, true), (true, true), (true, false)] {
            let mut event = response_event(1);
            if let RuntimeTransportEvent::LLMResponseReceived {
                correlation,
                provider,
                model,
                cost,
                input_tokens,
                output_tokens,
                reasoning_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                success,
                error,
                ..
            } = &mut event
            {
                *success = succeeded;
                *error = (!succeeded).then(|| "harness turn failed after metered work".into());
                let c = correlation.as_mut().unwrap();
                c.provider_attempt_count = 0;
                c.provider_attempt_id = None;
                c.usage_availability = Some(magicllm::types::UsageAvailability {
                    tokens: true,
                    cache_read: true,
                    cache_write: false,
                    cost: reported,
                });
                *provider = "harness:claude_code".into();
                *model = "fixture-model".into();
                *cost = 0.125;
                *input_tokens = 100;
                *output_tokens = 4;
                *reasoning_tokens = 0;
                *cache_read_tokens = 80;
                *cache_creation_tokens = 0;
            }
            let records = map_response_event(&event).expect("aggregate usage maps");
            assert!(!records
                .iter()
                .any(|r| matches!(r, LlmTraceRecord::ProviderAttempt(_))));
            let call = records
                .iter()
                .find_map(|r| match r {
                    LlmTraceRecord::CallCompleted(c) => Some(c),
                    _ => None,
                })
                .unwrap();
            assert_eq!(call.usage.cache_read_tokens, Some(80));
            assert_eq!(call.usage.cache_creation_tokens, None);
            assert_eq!(call.pricing.cost_usd, reported.then_some(0.125));
            assert_eq!(
                call.pricing.cost_source,
                Some(if reported {
                    LlmCostSource::Estimated
                } else {
                    LlmCostSource::Unknown
                })
            );
        }
    }

    #[test]
    fn terminal_usage_direct_harness_chat_enters_canonical_analytics() {
        use crate::magician_v2::execution::plane::{
            harness_chat_usage_event, HarnessStopReason, HarnessTurnSettled, HarnessUsage,
        };
        let mut trace = LlmTraceContext::new(
            LlmScope::new("owner", "workspace"),
            LlmWorkloadClass::ForegroundChat,
        );
        trace.execution_id = Some("turn-1".into());
        trace.chat_turn_id = Some("turn-1".into());
        trace.chat_session_id = Some("session-1".into());
        let settled = HarnessTurnSettled {
            assistant_text: "fixture".into(),
            stop_reason: HarnessStopReason::Settled,
            native_session_id: None,
            usage: Some(HarnessUsage {
                input_tokens: 100,
                output_tokens: 4,
                cached_input_tokens: 80,
                cache_read_reported: true,
                cost_usd: Some(0.125),
                model: Some("fixture-model".into()),
                ..Default::default()
            }),
        };
        let event = harness_chat_usage_event(
            trace,
            "claude_code",
            "personal-assistant",
            None,
            &settled,
            1000,
            1100,
            100,
        );
        let records = map_response_event(&event).expect("producer maps to canonical facts");
        let completed = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CallCompleted(call) => Some(call),
                _ => None,
            })
            .unwrap();
        assert_eq!(completed.context.chat_turn_id.as_deref(), Some("turn-1"));
        assert_eq!(completed.provider_attempt_count, 0);
        assert_eq!(completed.pricing.cost_usd, Some(0.125));
        assert_eq!(completed.usage.cache_read_tokens, Some(80));
        assert_eq!(completed.usage.cache_creation_tokens, None);
    }

    #[test]
    fn invalid_ttft_is_omitted_and_visible_as_a_gap() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived { ttft_ms, .. } = &mut event {
            *ttft_ms = Some(801);
        }
        let records = map_response_event(&event).expect("map invalid timing");
        let attempt = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::ProviderAttempt(value) => Some(value),
                _ => None,
            })
            .expect("attempt");
        assert_eq!(attempt.timing.ttft_ms, None);
        assert!(records.iter().any(|record| matches!(
            record,
            LlmTraceRecord::CaptureGap(gap) if gap.reason == INVALID_TTFT
        )));
    }

    #[test]
    fn invalid_cost_is_omitted_and_visible_as_a_gap() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived { cost, .. } = &mut event {
            *cost = -0.01;
        }
        let records = map_response_event(&event).expect("map invalid cost");
        let call = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CallCompleted(value) => Some(value),
                _ => None,
            })
            .expect("call");
        assert!(call.pricing.cost_usd.is_some());
        assert_eq!(call.pricing.cost_source, Some(LlmCostSource::Computed));
        assert!(call
            .pricing
            .pricing_version
            .as_deref()
            .is_some_and(|version| version.starts_with("pricing-row-v1:")));
        assert!(records.iter().any(|record| matches!(
            record,
            LlmTraceRecord::CaptureGap(gap) if gap.reason == INVALID_COST
        )));
    }

    #[test]
    fn producer_cost_mismatch_is_recomputed_and_visible_as_a_gap() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived { cost, .. } = &mut event {
            *cost = 99.0;
        }
        let records = map_response_event(&event).expect("map stale producer cost");
        let call = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CallCompleted(value) => Some(value),
                _ => None,
            })
            .expect("call");
        assert_ne!(call.pricing.cost_usd, Some(99.0));
        assert_eq!(call.pricing.cost_source, Some(LlmCostSource::Computed));
        assert!(records.iter().any(|record| matches!(
            record,
            LlmTraceRecord::CaptureGap(gap) if gap.reason == SUPPLIED_COST_MISMATCH
        )));
    }

    #[test]
    fn custom_provider_prefix_never_inherits_vendor_pricing() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived { provider, .. } = &mut event {
            *provider = "openai-compatible-private".to_string();
        }
        let records = map_response_event(&event).expect("map custom provider");
        let call = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CallCompleted(value) => Some(value),
                _ => None,
            })
            .expect("call");
        assert_eq!(call.pricing.cost_usd, None);
        assert_eq!(call.pricing.cost_source, Some(LlmCostSource::Unknown));
        assert_eq!(
            call.pricing.pricing_version.as_deref(),
            Some(UNPRICED_VERSION)
        );
    }

    #[test]
    fn custom_provider_cannot_inherit_openai_realtime_pricing_by_model_name() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived {
            provider,
            model,
            input_tokens,
            output_tokens,
            reasoning_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            audio_input_tokens,
            audio_output_tokens,
            audio_cached_tokens,
            ..
        } = &mut event
        {
            *provider = "private-realtime".to_string();
            *model = "gpt-realtime-2.1".to_string();
            *input_tokens = 500;
            *output_tokens = 200;
            *reasoning_tokens = 0;
            *cache_read_tokens = 100;
            *cache_creation_tokens = 0;
            *audio_input_tokens = Some(300);
            *audio_output_tokens = Some(150);
            *audio_cached_tokens = Some(80);
        }
        let records = map_response_event(&event).expect("map custom realtime provider");
        let call = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CallCompleted(value) => Some(value),
                _ => None,
            })
            .expect("call");
        assert_eq!(call.pricing.cost_usd, None);
        assert_eq!(call.pricing.cost_source, Some(LlmCostSource::Unknown));
    }

    #[test]
    fn inconsistent_usage_is_omitted_instead_of_saturating_into_a_price() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived {
            input_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            ..
        } = &mut event
        {
            *input_tokens = 10;
            *cache_read_tokens = 8;
            *cache_creation_tokens = 8;
        }
        let records = map_response_event(&event).expect("map inconsistent usage");
        let call = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CallCompleted(value) => Some(value),
                _ => None,
            })
            .expect("call");
        assert_eq!(call.usage, LlmTokenUsageFact::default());
        assert_eq!(call.pricing.cost_usd, None);
        assert_eq!(call.pricing.cost_source, Some(LlmCostSource::Unknown));
        assert!(records.iter().any(|record| matches!(
            record,
            LlmTraceRecord::CaptureGap(gap) if gap.reason == INVALID_USAGE
        )));
    }

    #[test]
    fn partial_realtime_modality_split_is_unknown_not_assumed_zero() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived {
            model,
            audio_input_tokens,
            audio_output_tokens,
            audio_cached_tokens,
            ..
        } = &mut event
        {
            *model = "gpt-realtime-2.1".to_string();
            *audio_input_tokens = Some(10);
            *audio_output_tokens = None;
            *audio_cached_tokens = Some(0);
        }
        let records = map_response_event(&event).expect("map partial realtime usage");
        let call = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CallCompleted(value) => Some(value),
                _ => None,
            })
            .expect("call");
        assert_eq!(call.usage, LlmTokenUsageFact::default());
        assert_eq!(call.pricing.cost_usd, None);
        assert!(records.iter().any(|record| matches!(
            record,
            LlmTraceRecord::CaptureGap(gap) if gap.reason == INVALID_USAGE
        )));
    }

    #[test]
    fn realtime_pricing_reconstructs_uncached_and_cached_modalities_once() {
        let mut event = response_event(1);
        let expected_usage = magicllm::types::RealtimeUsage {
            text_input_tokens: 100,
            text_cached_input_tokens: 20,
            text_output_tokens: 50,
            audio_input_tokens: 300,
            audio_cached_input_tokens: 80,
            audio_output_tokens: 150,
            billed_seconds: 0.0,
        };
        let expected_cost =
            magicllm::compute_realtime_cost_at("gpt-realtime-2.1", &expected_usage, 1_000);
        if let RuntimeTransportEvent::LLMResponseReceived {
            provider,
            model,
            cost,
            input_tokens,
            output_tokens,
            reasoning_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            audio_input_tokens,
            audio_output_tokens,
            audio_cached_tokens,
            ..
        } = &mut event
        {
            *provider = "openai".to_string();
            *model = "gpt-realtime-2.1".to_string();
            *cost = expected_cost;
            *input_tokens = 500;
            *output_tokens = 200;
            *reasoning_tokens = 0;
            *cache_read_tokens = 100;
            *cache_creation_tokens = 0;
            *audio_input_tokens = Some(300);
            *audio_output_tokens = Some(150);
            *audio_cached_tokens = Some(80);
        }
        let records = map_response_event(&event).expect("map realtime usage");
        let call = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CallCompleted(value) => Some(value),
                _ => None,
            })
            .expect("call");
        assert_eq!(call.pricing.cost_usd, Some(expected_cost));
        assert!(!records.iter().any(|record| matches!(
            record,
            LlmTraceRecord::CaptureGap(gap) if gap.reason == SUPPLIED_COST_MISMATCH
        )));
    }

    #[test]
    fn missing_provider_usage_stays_null_and_cannot_be_priced_as_zero() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived {
            usage_reported,
            input_tokens,
            output_tokens,
            reasoning_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            ..
        } = &mut event
        {
            *usage_reported = false;
            *input_tokens = 0;
            *output_tokens = 0;
            *reasoning_tokens = 0;
            *cache_read_tokens = 0;
            *cache_creation_tokens = 0;
        }

        let records = map_response_event(&event).expect("map usage-unreported response");
        let call = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CallCompleted(value) => Some(value),
                _ => None,
            })
            .expect("call");
        assert_eq!(call.usage, LlmTokenUsageFact::default());
        assert_eq!(call.pricing.cost_usd, None);
        assert_eq!(call.pricing.cost_source, Some(LlmCostSource::Unknown));
        assert_eq!(
            call.pricing.pricing_version.as_deref(),
            Some(USAGE_UNREPORTED_VERSION)
        );
        assert!(records.iter().any(|record| matches!(
            record,
            LlmTraceRecord::CaptureGap(gap) if gap.reason == USAGE_UNREPORTED
        )));
    }

    #[test]
    fn logical_chunk_parent_is_a_non_billing_zero_attempt_summary() {
        let mut event = response_event(1);
        if let RuntimeTransportEvent::LLMResponseReceived {
            correlation,
            response_kind,
            usage_reported,
            input_tokens,
            output_tokens,
            reasoning_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            cost,
            provider,
            model,
            ..
        } = &mut event
        {
            let context = correlation_context(correlation.as_ref().expect("correlation"));
            *correlation = Some(LlmEventCorrelation::from(
                &magicllm::LlmTraceReceipt::direct_with_attempt_count(context, 0),
            ));
            *response_kind = LLM_LOGICAL_CHUNK_SUMMARY_RESPONSE_KIND.to_string();
            *usage_reported = false;
            *input_tokens = 0;
            *output_tokens = 0;
            *reasoning_tokens = 0;
            *cache_read_tokens = 0;
            *cache_creation_tokens = 0;
            *cost = 0.0;
            provider.clear();
            model.clear();
        }

        let records = map_response_event(&event).expect("map logical summary");
        assert_eq!(records.len(), 2);
        assert!(!records
            .iter()
            .any(|record| matches!(record, LlmTraceRecord::ProviderAttempt(_))));
        let call = records
            .iter()
            .find_map(|record| match record {
                LlmTraceRecord::CallCompleted(value) => Some(value),
                _ => None,
            })
            .expect("summary call");
        assert_eq!(call.provider_attempt_count, 0);
        assert_eq!(call.usage, LlmTokenUsageFact::default());
        assert_eq!(call.pricing.cost_usd, None);
        assert_eq!(
            call.pricing.pricing_version.as_deref(),
            Some(LOGICAL_CHUNK_SUMMARY_VERSION)
        );
    }
}

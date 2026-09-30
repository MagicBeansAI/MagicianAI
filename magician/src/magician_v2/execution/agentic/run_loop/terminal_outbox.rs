//! Bounded lifecycle projection for terminal executions.
//!
//! A terminal can commit successfully and lose its process before the boundary
//! projector saves the outbox cursor. No execution host is needed to finish
//! that delivery: the journal already contains the exact event, routing decision
//! and named-rail arguments. This module owns the narrower recovery lifecycle so
//! terminal debt does not depend on somebody being able to reconstruct a whole
//! [`super::driver_worker::WorkerHost`].
//!
//! The pass is deliberately one cursored page and one key at a time. The page
//! bounds execution entries examined, including non-matches, and sequential
//! projection is bounded concurrency of one. A lifecycle owner retains
//! [`TerminalOutboxProjectionReport::resume`] between cadence calls and resets
//! it to `None` after reaching the end, producing a round-robin walk without an
//! unbounded task fan-out.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::magician_v2::artifact_v2::{
    ArtifactV2Service, CanonicalEventScope, RuntimeCanonicalEventReceipt,
};
use crate::magician_v2::realtime_events::RuntimeTransportBroadcaster;

use super::journal::{
    EmitRefused, EventKey, ProjectedEventSink, ProjectorCursor, RecordedEventRouting,
};
use super::phases::outbox;
use super::state::WorkerId;
use super::store::{ExecutionKey, LoopStateStore, ScanCursor, StoreError, StoreResult};

/// Long enough to cover one bounded journal read and synchronous broadcaster
/// drain without needing lease renewal in the middle of an emit-before-mark
/// sequence. Expiry is safe: the fenced cursor save fails and durable debt stays
/// discoverable for a later pass.
const TERMINAL_PROJECTION_LEASE_TTL: Duration = Duration::from_secs(5 * 60);
/// Leave a full minute for the fenced cursor save and lease release. A wedged
/// canonical writer must not hold this lifecycle page forever, and timing out
/// is safe because the durable loop cursor remains below every admitted fact.
const TERMINAL_CANONICAL_ACK_TIMEOUT: Duration = Duration::from_secs(4 * 60);
/// Artifact/runtime settlement is a distinct durable owner from canonical
/// event persistence. Give it a fresh budget after renewing the exact lease;
/// otherwise a legal slow canonical append can consume this step's entire
/// deadline on every replay and permanently starve settlement.
const TERMINAL_RUNTIME_SETTLEMENT_TIMEOUT: Duration = Duration::from_secs(4 * 60);

/// What one bounded lifecycle page did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalOutboxProjectionReport {
    pub discovered: usize,
    pub claimed: usize,
    pub projected_events: usize,
    pub deduped_events: usize,
    pub unmappable_events: usize,
    pub no_longer_terminal: usize,
    pub lease_conflicts: usize,
    pub lease_renew_failures: usize,
    pub store_failures: usize,
    pub cursor_save_failures: usize,
    /// Canonical queue admission succeeded, but the durable append failed (or
    /// its worker disappeared) before the loop projector cursor was saved.
    pub canonical_persistence_failures: usize,
    /// A reconciler/deadline terminal was projected, but its Artifact/runtime
    /// lifecycle did not durably accept continuation or failure settlement.
    pub runtime_settlement_failures: usize,
    /// A committed terminal steer receipt could not be acknowledged or cleared.
    /// The state receipt remains discoverable and the next bounded pass retries.
    pub steer_ack_failures: usize,
    pub release_failures: usize,
    pub cancelled: bool,
    /// The next scan position. A lifecycle owner should retain this between
    /// calls and reset it to `None` once it is `None`, starting the next sweep.
    pub resume: Option<ScanCursor>,
}

/// Retains blocking terminal-projection work across cancellation of its async
/// caller.
///
/// Tokio cannot abort a `spawn_blocking` closure after it has started. The
/// production lifecycle owner therefore registers every such closure here and
/// closes/joins this registry before canonical sinks are torn down. Refusing a
/// late registration is safe: the loop projector cursor remains below the
/// unprojected record and the next process will rediscover the debt.
#[derive(Clone, Default)]
pub struct TerminalProjectionJobRegistry {
    inner: Arc<TerminalProjectionJobRegistryInner>,
}

#[derive(Default)]
struct TerminalProjectionJobRegistryInner {
    state: Mutex<TerminalProjectionJobRegistryState>,
    drained: tokio::sync::Notify,
}

struct TerminalProjectionJobRegistryState {
    accepting: bool,
    active_jobs: usize,
}

impl Default for TerminalProjectionJobRegistryState {
    fn default() -> Self {
        Self {
            accepting: true,
            active_jobs: 0,
        }
    }
}

impl TerminalProjectionJobRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn register(&self) -> Option<TerminalProjectionJobRegistration> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.accepting {
            return None;
        }
        state.active_jobs = state.active_jobs.saturating_add(1);
        Some(TerminalProjectionJobRegistration {
            inner: Arc::clone(&self.inner),
        })
    }

    /// Refuse new projection closures and wait until every already-started
    /// closure has stopped touching its broadcaster/canonical sink.
    pub async fn quiesce(&self) -> usize {
        let observed = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.accepting = false;
            state.active_jobs
        };
        loop {
            // Create the notification future before checking the count so a
            // last-job drop between the check and await cannot be missed.
            let drained = self.inner.drained.notified();
            let active_jobs = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .active_jobs;
            if active_jobs == 0 {
                return observed;
            }
            drained.await;
        }
    }
}

struct TerminalProjectionJobRegistration {
    inner: Arc<TerminalProjectionJobRegistryInner>,
}

impl Drop for TerminalProjectionJobRegistration {
    fn drop(&mut self) {
        let became_empty = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.active_jobs = state.active_jobs.saturating_sub(1);
            state.active_jobs == 0
        };
        if became_empty {
            self.inner.drained.notify_waiters();
        }
    }
}

/// Project one bounded, cursored page of terminal outbox debt.
///
/// Discovery ignores placement because it never grants phase authority. Every
/// candidate is claimed and its committed journal terminal is re-derived under
/// that lease before any event is emitted. Journal/cursor corruption and store
/// failures fail closed: the durable mark is not moved, so the debt remains.
///
/// Emit precedes the fenced cursor save. A crash or save failure may therefore
/// duplicate delivery, but cannot mark an event emitted by nobody; the durable
/// projector dedupe window absorbs the ordinary immediate retry.
pub async fn project_terminal_outbox_debt(
    store: &dyn LoopStateStore,
    broadcaster: &RuntimeTransportBroadcaster,
    artifact_service: Option<&Arc<ArtifactV2Service>>,
    worker: &WorkerId,
    after: Option<&ScanCursor>,
    max_visits: NonZeroUsize,
    cancellation: &CancellationToken,
) -> StoreResult<TerminalOutboxProjectionReport> {
    project_terminal_outbox_debt_inner(
        store,
        broadcaster,
        artifact_service,
        worker,
        after,
        max_visits,
        cancellation,
        None,
    )
    .await
}

/// Production variant whose blocking projection work is retained through
/// shutdown even if the async lifecycle task itself has to be aborted.
#[allow(clippy::too_many_arguments)]
pub async fn project_terminal_outbox_debt_registered(
    store: &dyn LoopStateStore,
    broadcaster: &RuntimeTransportBroadcaster,
    artifact_service: Option<&Arc<ArtifactV2Service>>,
    worker: &WorkerId,
    after: Option<&ScanCursor>,
    max_visits: NonZeroUsize,
    cancellation: &CancellationToken,
    projection_jobs: &TerminalProjectionJobRegistry,
) -> StoreResult<TerminalOutboxProjectionReport> {
    project_terminal_outbox_debt_inner(
        store,
        broadcaster,
        artifact_service,
        worker,
        after,
        max_visits,
        cancellation,
        Some(projection_jobs),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn project_terminal_outbox_debt_inner(
    store: &dyn LoopStateStore,
    broadcaster: &RuntimeTransportBroadcaster,
    artifact_service: Option<&Arc<ArtifactV2Service>>,
    worker: &WorkerId,
    after: Option<&ScanCursor>,
    max_visits: NonZeroUsize,
    cancellation: &CancellationToken,
    projection_jobs: Option<&TerminalProjectionJobRegistry>,
) -> StoreResult<TerminalOutboxProjectionReport> {
    let mut report = TerminalOutboxProjectionReport::default();
    if cancellation.is_cancelled() {
        report.cancelled = true;
        report.resume = after.cloned();
        return Ok(report);
    }

    let page = store.scan_terminal_outbox_debt(max_visits, after).await?;
    report.discovered = page.keys.len();

    for key in page.keys {
        if cancellation.is_cancelled() {
            // Restart this page on the next call. Already projected keys are
            // cheap durable-cursor no-ops; advancing to `page.resume` here would
            // skip candidates in this page that cancellation prevented us from
            // visiting.
            report.cancelled = true;
            report.resume = after.cloned();
            return Ok(report);
        }

        let mut lease = match store
            .claim(&key, worker, TERMINAL_PROJECTION_LEASE_TTL)
            .await
        {
            Ok(lease) => lease,
            Err(StoreError::LeaseHeld { .. }) => {
                report.lease_conflicts = report.lease_conflicts.saturating_add(1);
                continue;
            },
            Err(error) => {
                report.store_failures = report.store_failures.saturating_add(1);
                tracing::warn!(
                    execution = %key,
                    %error,
                    "[LOOP_OUTBOX] terminal debt could not be claimed; its durable cursor is \
                     unchanged and a later lifecycle pass will retry"
                );
                continue;
            },
        };
        report.claimed = report.claimed.saturating_add(1);

        project_claimed_terminal(
            store,
            broadcaster,
            artifact_service,
            &key,
            &mut lease,
            cancellation,
            &mut report,
            projection_jobs,
        )
        .await;

        if let Err(error) = store.release(lease).await {
            report.release_failures = report.release_failures.saturating_add(1);
            tracing::warn!(
                execution = %key,
                %error,
                "[LOOP_OUTBOX] terminal projection finished but its lease could not be released; \
                 it remains excluded until the bounded lease expires"
            );
        }
    }

    report.resume = page.resume;
    Ok(report)
}

async fn project_claimed_terminal(
    store: &dyn LoopStateStore,
    broadcaster: &RuntimeTransportBroadcaster,
    artifact_service: Option<&Arc<ArtifactV2Service>>,
    key: &ExecutionKey,
    lease: &mut super::store::Lease,
    cancellation: &CancellationToken,
    report: &mut TerminalOutboxProjectionReport,
    projection_jobs: Option<&TerminalProjectionJobRegistry>,
) {
    let mut committed = match store.load(key).await {
        Ok(Some(committed)) => committed,
        Ok(None) => {
            report.no_longer_terminal = report.no_longer_terminal.saturating_add(1);
            return;
        },
        Err(error) => {
            record_store_failure(report, key, "load its committed state", &error);
            return;
        },
    };

    // Receipt retirement is independent lifecycle debt. Do it before asking
    // for an ended marker or a terminal journal: an external cancellation can
    // leave an eventless nonterminal LoopState carrying the receipt while the
    // runtime FSM is already terminal and startup phase recovery skips it.
    if let Some(receipt) = committed.state.steer_consume_receipt.clone() {
        let Some(service) = artifact_service else {
            report.steer_ack_failures = report.steer_ack_failures.saturating_add(1);
            tracing::warn!(
                execution = %key,
                "[LOOP_OUTBOX] operator steer debt requires the Artifact workspace; the committed receipt remains discoverable"
            );
            return;
        };
        let Some(runtime_execution_id) = committed.state.identity.execution_id.as_deref() else {
            report.steer_ack_failures = report.steer_ack_failures.saturating_add(1);
            tracing::warn!(
                execution = %key,
                "[LOOP_OUTBOX] operator steer debt has no independently committed runtime execution identity; no inbox is mutated"
            );
            return;
        };
        let scope_matches = committed.state.identity.principal.as_deref() == Some(key.principal())
            && committed.state.identity.workspace.as_deref() == Some(key.workspace());
        let segment_matches = committed
            .state
            .segment_binding
            .as_ref()
            .map(|binding| {
                binding.base_execution_id == runtime_execution_id
                    && binding.exact_segment_id == key.execution_id()
            })
            .unwrap_or(true);
        if !scope_matches || !segment_matches {
            report.steer_ack_failures = report.steer_ack_failures.saturating_add(1);
            tracing::warn!(
                execution = %key,
                "[LOOP_OUTBOX] operator steer debt conflicts with its committed scope/segment authority; no inbox is mutated"
            );
            return;
        }
        if let Err(error) = super::steer_inbox::acknowledge(
            service.workspace(),
            key.principal(),
            key.workspace(),
            runtime_execution_id,
            key.execution_id(),
            &receipt,
        )
        .await
        {
            report.steer_ack_failures = report.steer_ack_failures.saturating_add(1);
            tracing::warn!(
                execution = %key,
                %error,
                "[LOOP_OUTBOX] operator steer acknowledgement failed; the committed receipt remains discoverable"
            );
            return;
        }
        committed.state.steer_consume_receipt = None;
        match store
            .commit_fenced(key, &committed.state, committed.revision, lease)
            .await
        {
            Ok(revision) => committed.revision = revision,
            Err(error) => {
                report.steer_ack_failures = report.steer_ack_failures.saturating_add(1);
                tracing::warn!(
                    execution = %key,
                    %error,
                    "[LOOP_OUTBOX] operator steer was acknowledged but its receipt could not be cleared; the next pass will repeat the idempotent acknowledgement"
                );
                return;
            },
        }
    }
    let watermark = committed.state.journal_seq;
    let replayed = match store.replay_committed_journal(key, watermark).await {
        Ok(replayed) => replayed,
        Err(error) => {
            record_store_failure(report, key, "verify its committed journal", &error);
            return;
        },
    };
    let receipt_terminal_is_exact = committed
        .state
        .terminal_settlement_receipt
        .as_ref()
        .is_some_and(|receipt| {
            receipt.descriptor.terminal_seq == watermark
                && replayed.terminal.is_some_and(|terminal| {
                    receipt.descriptor.terminal_kind == terminal.settlement_label()
                })
        });
    let terminal_is_exact = replayed.seq == watermark
        && replayed
            .terminal
            .is_some_and(|terminal| !terminal.is_resumable() || receipt_terminal_is_exact);
    if !terminal_is_exact {
        // The scan is intentionally only a cheap marker read. A concurrent
        // repair or restore can invalidate it before this claim; the journal
        // under the committed watermark is the authority and wins.
        report.no_longer_terminal = report.no_longer_terminal.saturating_add(1);
        tracing::warn!(
            execution = %key,
            replayed_seq = replayed.seq,
            watermark,
            terminal = ?replayed.terminal,
            "[LOOP_OUTBOX] a debt-scan candidate is not an exact committed terminal with \
             either final-segment or receipt authority; projection is refused"
        );
        return;
    }

    let mut cursor = match store.load_projector_cursor(key).await {
        Ok(Some(cursor)) => cursor,
        Ok(None) => ProjectorCursor::new(),
        Err(error) => {
            record_store_failure(report, key, "read its projector cursor", &error);
            return;
        },
    };
    let runtime_settlement_mark_before = cursor.runtime_settled_terminal_seq();
    if runtime_settlement_mark_before.is_some_and(|settled| settled > watermark) {
        report.store_failures = report.store_failures.saturating_add(1);
        tracing::warn!(
            execution = %key,
            settled_terminal_seq = ?runtime_settlement_mark_before,
            watermark,
            "[LOOP_OUTBOX] runtime-settlement receipt is beyond the committed terminal watermark; nothing is emitted or saved"
        );
        return;
    }
    // Cross-layer settlement is the admission barrier for terminal HITL
    // events. Converge runtime + Artifact first, but keep the exact FullPause
    // generation staged until the canonical request below is durably accepted.
    // Response APIs recognize that durable staging marker and return retryable
    // without mutating state in the short request-publication window.
    let terminal = replayed
        .terminal
        .expect("terminal exactness was checked before projection");
    let runtime_settlement_required = terminal
        != crate::magician_v2::execution::agentic::run_loop::journal::TerminalKind::HandedOff
        && (committed.state.identity.task_id.is_some()
            || committed.state.identity.execution_id.is_some())
        && (terminal
            == crate::magician_v2::execution::agentic::run_loop::journal::TerminalKind::CannotProceed
            || receipt_terminal_is_exact);
    let pause_publication_required = committed
        .state
        .terminal_settlement_receipt
        .as_ref()
        .is_some_and(|receipt| receipt.descriptor.pause_key.is_some());
    let runtime_settlement_acceptance_required = runtime_settlement_required
        && (cursor.runtime_settled_terminal_seq() != Some(watermark) || pause_publication_required);
    if runtime_settlement_required && artifact_service.is_none() {
        report.runtime_settlement_failures = report.runtime_settlement_failures.saturating_add(1);
        tracing::warn!(
            execution = %key,
            "[LOOP_OUTBOX] exact runtime-backed terminal requires an Artifact/runtime settlement owner; settlement debt remains durable"
        );
        return;
    }
    // Keep every first-time cross-layer settlement—not only pause publication—
    // inside the same agent->execution lifecycle fence as cancellation,
    // deletion, and owner changes. The live worker held this fence through the
    // LoopState CAS, but a process can die before this host-free projector runs;
    // reacquiring it closes the post-CAS gap for ordinary and pipeline finals.
    // Receiptless CannotProceed compatibility uses the same fence. Ordinary
    // delegated failure is the one deliberate agent-only case selected by the
    // seal-validating service helper, because its acceptance path acquires the
    // control-tree/execution lock to force-fail that exact child. Governed
    // callable-agent failure retains the full fence through its exact status
    // CAS because that specialized leaf owner does not reacquire it.
    let mut terminal_settlement_lifecycle_exclusion = if runtime_settlement_required {
        let service = artifact_service.expect("runtime settlement requires Artifact owner");
        match store.renew(lease, TERMINAL_PROJECTION_LEASE_TTL).await {
            Ok(renewed) => *lease = renewed,
            Err(error) => {
                report.lease_renew_failures = report.lease_renew_failures.saturating_add(1);
                tracing::warn!(
                    execution = %key,
                    %error,
                    "[LOOP_OUTBOX] terminal projection lost its lease before lifecycle exclusion"
                );
                return;
            },
        }
        let exclusion_deadline = tokio::time::Instant::now() + TERMINAL_RUNTIME_SETTLEMENT_TIMEOUT;
        let exclusion = tokio::select! {
            _ = cancellation.cancelled() => {
                report.cancelled = true;
                return;
            },
            exclusion = tokio::time::timeout_at(
                exclusion_deadline,
                service.acquire_terminal_loop_receipt_lifecycle_exclusion(
                    key,
                    &committed.state.identity,
                    committed.state.segment_binding.as_ref(),
                    terminal,
                    watermark,
                    committed.state.terminal_settlement_receipt.as_ref(),
                ),
            ) => exclusion
                .map_err(|_| ArtifactRuntimeSettlementFailure::TimedOut)
                .and_then(|exclusion| exclusion.map_err(|error| {
                    ArtifactRuntimeSettlementFailure::Rejected(error.to_string())
                })),
        };
        match exclusion {
            Ok(guard) => guard,
            Err(error) => {
                report.runtime_settlement_failures =
                    report.runtime_settlement_failures.saturating_add(1);
                tracing::warn!(
                    execution = %key,
                    %error,
                    "[LOOP_OUTBOX] terminal settlement lifecycle exclusion failed; durable debt remains unacknowledged"
                );
                return;
            },
        }
    } else {
        None
    };
    if matches!(
        terminal_settlement_lifecycle_exclusion.as_ref(),
        Some(crate::magician_v2::artifact_v2::service::TerminalLoopReceiptLifecycleExclusion::DeletedTask { .. })
    ) {
        // The exact journal identity/receipt was validated before deletion
        // exclusion. Preserve the journal, emit nothing, and acknowledge only
        // this watermark while retaining the task deletion transaction guard.
        suppress_deleted_task_terminal(store, key, watermark, cursor, lease, report).await;
        return;
    }
    if !runtime_settlement_acceptance_required {
        // Runtime settlement can precede a crash before event delivery. Check
        // deletion on that replay too, then release a live execution's fence.
        drop(terminal_settlement_lifecycle_exclusion.take());
    }
    // A pause receipt is re-accepted even after its runtime cursor mark was
    // saved. Cancellation may have won while an earlier pass was stopped below
    // the canonical request; re-reading that durable disposition under the
    // lifecycle exclusion prevents a later pass from publishing a stale HITL
    // request merely because cross-layer settlement had once completed.
    if runtime_settlement_acceptance_required {
        let service = artifact_service.expect("required Artifact settlement owner checked above");
        match store.renew(lease, TERMINAL_PROJECTION_LEASE_TTL).await {
            Ok(renewed) => *lease = renewed,
            Err(error) => {
                report.lease_renew_failures = report.lease_renew_failures.saturating_add(1);
                tracing::warn!(
                    execution = %key,
                    %error,
                    "[LOOP_OUTBOX] terminal projection lost its lease before Artifact/runtime settlement; the durable cursor remains unchanged"
                );
                return;
            },
        }
        let settlement_deadline = tokio::time::Instant::now() + TERMINAL_RUNTIME_SETTLEMENT_TIMEOUT;
        let accepted = tokio::select! {
            _ = cancellation.cancelled() => {
                report.cancelled = true;
                return;
            },
            accepted = tokio::time::timeout_at(
                settlement_deadline,
                // Keep the large Artifact terminal future out of this
                // projector's select/timeout state machine. It still polls
                // inline under the same lifecycle exclusion and cancellation.
                Box::pin(service.accept_terminal_loop_settlement(
                    key,
                    &committed.state.identity,
                    committed.state.segment_binding.as_ref(),
                    terminal,
                    watermark,
                    committed.state.terminal_settlement_receipt.as_ref(),
                )),
            ) => accepted
                .map_err(|_| ArtifactRuntimeSettlementFailure::TimedOut)
                .and_then(|accepted| accepted.map_err(|error| {
                    ArtifactRuntimeSettlementFailure::Rejected(error.to_string())
                })),
        };
        let acceptance = match accepted {
            Ok(acceptance) => acceptance,
            Err(error) => {
                report.runtime_settlement_failures =
                    report.runtime_settlement_failures.saturating_add(1);
                tracing::warn!(
                    execution = %key,
                    %error,
                    "[LOOP_OUTBOX] terminal journal projection was not accepted by its Artifact/runtime lifecycle; the durable cursor remains below the terminal"
                );
                return;
            },
        };
        if matches!(
            acceptance,
            crate::magician_v2::artifact_v2::service::TerminalLoopSettlementAcceptance::SupersededByCancellation
                | crate::magician_v2::artifact_v2::service::TerminalLoopSettlementAcceptance::SuppressedByCanonicalPolicy
        ) {
            // Cancellation and canonical delegated-child force-failure are
            // stronger durable lifecycle dispositions. Skip the immutable
            // receipt's now-stale HITL request/event batch and acknowledge the
            // exact watermark under the current loop lease; publishing it later
            // would resurrect response authority after terminal settlement.
            cursor.suppress_terminal_batch_through(watermark);
            if let Err(error) = store
                .save_projector_cursor_fenced(key, &cursor, lease)
                .await
            {
                report.cursor_save_failures = report.cursor_save_failures.saturating_add(1);
                tracing::warn!(
                    execution = %key,
                    watermark,
                    %error,
                    "[LOOP_OUTBOX] cancelled terminal receipt could not save its suppressed cursor; exact debt remains"
                );
            }
            return;
        }
        cursor.mark_runtime_terminal_settled(watermark);
        if !pause_publication_required {
            // Ordinary and pipeline finals need the fence only through their
            // canonical Artifact/runtime acceptance. Once that durable
            // disposition wins, event replay is non-authoritative and must not
            // hold an agent-wide lifecycle lock across broadcaster latency.
            drop(terminal_settlement_lifecycle_exclusion.take());
        }
    }
    let mark_before = cursor.emitted_through_seq();
    let require_complete_history = cursor.requires_complete_history(key.execution_id());
    let records = match store
        .read_journal_projection(
            key,
            mark_before.saturating_add(1),
            watermark,
            require_complete_history,
        )
        .await
    {
        Ok(records) => records,
        Err(error) => {
            record_store_failure(report, key, "read its committed projection window", &error);
            return;
        },
    };
    // Journal/cursor verification may consume a material part of the original
    // lease. Renew immediately before the first externally visible emit so the
    // four-minute canonical-ack budget starts under a fresh five-minute fence.
    // If another holder has already won, this stale projector emits nothing.
    match store.renew(lease, TERMINAL_PROJECTION_LEASE_TTL).await {
        Ok(renewed) => *lease = renewed,
        Err(error) => {
            report.lease_renew_failures = report.lease_renew_failures.saturating_add(1);
            tracing::warn!(
                execution = %key,
                %error,
                "[LOOP_OUTBOX] terminal projection lost its lease before emission; the durable cursor remains unchanged"
            );
            return;
        },
    }
    // Rejoining and broadcasting is normally CPU-only, but scoped HITL replay
    // must synchronously fsync its authoritative lifecycle journal before
    // visibility. Keep that bounded blocking contract off the Tokio worker.
    let projection_broadcaster = broadcaster.clone();
    let runtime_execution_id = committed.state.identity.execution_id.clone();
    let loop_execution_id = key.execution_id().to_string();
    let projection_registration = match projection_jobs {
        Some(registry) => match registry.register() {
            Some(registration) => Some(registration),
            None => {
                report.cancelled = true;
                tracing::debug!(
                    execution = %key,
                    "[LOOP_OUTBOX] terminal projection refused after its blocking-work registry quiesced"
                );
                return;
            },
        },
        None => None,
    };
    let projected = tokio::task::spawn_blocking(move || {
        let _projection_registration = projection_registration;
        let mut cursor = cursor;
        let mut sink =
            BroadcasterEventSink::new(&projection_broadcaster, runtime_execution_id.as_deref());
        let projection = cursor.project_records(&records, &loop_execution_id, watermark, &mut sink);
        (cursor, projection, sink.unmappable, sink.canonical_receipts)
    })
    .await;
    let (cursor, projection, unmappable, canonical_receipts) = match projected {
        Ok(projected) => projected,
        Err(error) => {
            report.store_failures = report.store_failures.saturating_add(1);
            tracing::warn!(
                execution = %key,
                %error,
                "[LOOP_OUTBOX] terminal broadcast worker failed; the durable cursor remains unchanged"
            );
            return;
        },
    };
    if projection.mark_beyond_watermark {
        report.store_failures = report.store_failures.saturating_add(1);
        tracing::warn!(
            execution = %key,
            mark = mark_before,
            watermark,
            "[LOOP_OUTBOX] a terminal projector cursor is beyond its committed watermark; \
             nothing is saved until an operator reconciles the cursor and journal"
        );
        return;
    }
    if let Some((seq, refused)) = &projection.stopped_at {
        // Exact canonical delivery refuses a missing sink, missing identity,
        // route mismatch, or HITL admission/journal failure. The mark stays
        // below that exact record, so process loss cannot erase the retry.
        tracing::warn!(
            execution = %key,
            stopped_at_seq = *seq,
            reason = %refused,
            "[LOOP_OUTBOX] terminal projection stopped at a refused emit; the durable cursor \
             remains below that record"
        );
    }
    report.projected_events = report.projected_events.saturating_add(projection.emitted);
    report.deduped_events = report.deduped_events.saturating_add(projection.deduped);
    report.unmappable_events = report.unmappable_events.saturating_add(unmappable);

    // The runtime canonical sink is an asynchronously drained bounded queue.
    // Queue admission is not delivery: a process loss or append failure after
    // enqueue would otherwise let this projector save a false durable mark and
    // permanently lose the event. Await every append accepted during this
    // projection before publishing the loop cursor. Failure leaves the old
    // cursor intact, so the next lifecycle pass retries at-least-once.
    let canonical_deadline = tokio::time::Instant::now() + TERMINAL_CANONICAL_ACK_TIMEOUT;
    for receipt in canonical_receipts {
        let persisted = tokio::select! {
            _ = cancellation.cancelled() => {
                report.cancelled = true;
                return;
            },
            persisted = tokio::time::timeout_at(
                canonical_deadline,
                receipt.wait_persisted(),
            ) => persisted
                .map_err(|_| "canonical append acknowledgement timed out".to_string())
                .and_then(std::convert::identity),
        };
        if let Err(error) = persisted {
            report.canonical_persistence_failures =
                report.canonical_persistence_failures.saturating_add(1);
            tracing::warn!(
                execution = %key,
                %error,
                "[LOOP_OUTBOX] canonical append was not acknowledged; the durable projector \
                 cursor remains below the replayed record"
            );
            return;
        }
    }

    // Publish the response authority last. Waiting until the complete terminal
    // batch is durably canonical prevents both prompt-before-admission and the
    // inverse consume-before-prompt race. If projection stopped early, save its
    // partial cursor below while leaving the pause staged for the next pass.
    if pause_publication_required && cursor.emitted_through_seq() == watermark {
        let service = artifact_service.expect("pause publication requires Artifact owner");
        match store.renew(lease, TERMINAL_PROJECTION_LEASE_TTL).await {
            Ok(renewed) => *lease = renewed,
            Err(error) => {
                report.lease_renew_failures = report.lease_renew_failures.saturating_add(1);
                tracing::warn!(
                    execution = %key,
                    %error,
                    "[LOOP_OUTBOX] terminal projection lost its lease before pause publication; the exact staged generation remains hidden"
                );
                return;
            },
        }
        let publication_deadline =
            tokio::time::Instant::now() + TERMINAL_RUNTIME_SETTLEMENT_TIMEOUT;
        let published = tokio::select! {
            _ = cancellation.cancelled() => {
                report.cancelled = true;
                return;
            },
            published = tokio::time::timeout_at(
                publication_deadline,
                service.publish_terminal_loop_pause(
                    key,
                    &committed.state.identity,
                    committed.state.segment_binding.as_ref(),
                    terminal,
                    watermark,
                    committed.state.terminal_settlement_receipt.as_ref(),
                ),
            ) => published
                .map_err(|_| ArtifactRuntimeSettlementFailure::TimedOut)
                .and_then(|published| published.map_err(|error| {
                    ArtifactRuntimeSettlementFailure::Rejected(error.to_string())
                })),
        };
        if let Err(error) = published {
            report.runtime_settlement_failures =
                report.runtime_settlement_failures.saturating_add(1);
            tracing::warn!(
                execution = %key,
                %error,
                "[LOOP_OUTBOX] canonical terminal request is durable but its exact pause generation remains staged; cursor acknowledgement is withheld for retry"
            );
            return;
        }
    }

    if cursor.emitted_through_seq() == mark_before
        && cursor.runtime_settled_terminal_seq() == runtime_settlement_mark_before
    {
        return;
    }
    if let Err(error) = store
        .save_projector_cursor_fenced(key, &cursor, lease)
        .await
    {
        report.cursor_save_failures = report.cursor_save_failures.saturating_add(1);
        tracing::warn!(
            execution = %key,
            mark = cursor.emitted_through_seq(),
            %error,
            "[LOOP_OUTBOX] terminal events were emitted but their cursor could not be saved; \
             durable debt remains and a later pass may deliver them again"
        );
    }
}

async fn suppress_deleted_task_terminal(
    store: &dyn LoopStateStore,
    key: &ExecutionKey,
    watermark: u64,
    mut cursor: ProjectorCursor,
    lease: &super::store::Lease,
    report: &mut TerminalOutboxProjectionReport,
) {
    cursor.suppress_terminal_batch_through(watermark);
    match store
        .save_projector_cursor_fenced(key, &cursor, lease)
        .await
    {
        Ok(()) => tracing::info!(execution = %key, watermark,
            "[LOOP_OUTBOX] deleted task terminal debt retired; journal retained"),
        Err(error) => {
            report.cursor_save_failures = report.cursor_save_failures.saturating_add(1);
            tracing::warn!(execution = %key, watermark, %error,
                "[LOOP_OUTBOX] deleted task terminal cursor save failed; debt remains");
        },
    }
}

#[derive(Debug)]
enum ArtifactRuntimeSettlementFailure {
    TimedOut,
    Rejected(String),
}

impl std::fmt::Display for ArtifactRuntimeSettlementFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut => formatter.write_str("Artifact/runtime settlement timed out"),
            Self::Rejected(error) => formatter.write_str(error),
        }
    }
}

fn record_store_failure(
    report: &mut TerminalOutboxProjectionReport,
    key: &ExecutionKey,
    operation: &'static str,
    error: &StoreError,
) {
    report.store_failures = report.store_failures.saturating_add(1);
    tracing::warn!(
        execution = %key,
        %error,
        "[LOOP_OUTBOX] terminal projection could not {operation}; nothing is emitted and the \
         durable cursor is unchanged"
    );
}

/// A host-free version of `driver_worker::HostEventSink`.
///
/// The Artifact service installs its canonical sink on this broadcaster before
/// invoking the lifecycle pass, so `emit` preserves canonical runtime facts
/// while `emit_transport_only` preserves a producer's explicit non-fact route.
struct BroadcasterEventSink<'a> {
    broadcaster: &'a RuntimeTransportBroadcaster,
    /// The runtime execution identity from the committed state, never the
    /// loop-state key (which may carry resume/nesting/refinement suffixes).
    runtime_execution_id: Option<&'a str>,
    unmappable: usize,
    canonical_receipts: Vec<RuntimeCanonicalEventReceipt>,
}

impl<'a> BroadcasterEventSink<'a> {
    fn new(
        broadcaster: &'a RuntimeTransportBroadcaster,
        runtime_execution_id: Option<&'a str>,
    ) -> Self {
        Self {
            broadcaster,
            runtime_execution_id,
            unmappable: 0,
            canonical_receipts: Vec::new(),
        }
    }

    fn deliver(
        &mut self,
        key: &EventKey,
        event_type: &str,
        payload: &serde_json::Value,
        routing: &RecordedEventRouting,
    ) -> Result<(), EmitRefused> {
        let event = match outbox::rejoin(event_type, payload.clone()) {
            Ok(event) => event,
            Err(error) => {
                self.unmappable = self.unmappable.saturating_add(1);
                let reason = format!(
                    "terminal journal event {event_type:?} cannot be rejoined by this build: {error}"
                );
                tracing::warn!(
                    address = %key,
                    event_type,
                    %error,
                    "[LOOP_OUTBOX] a terminal journal event cannot be rejoined by this build; \
                     projection stops below it rather than marking an undelivered fact"
                );
                return Err(EmitRefused { reason });
            },
        };
        match routing {
            RecordedEventRouting::CanonicalRuntimeFact { scope } => {
                for (field, expected) in [
                    ("principal", scope.principal.as_str()),
                    ("workspace", scope.workspace.as_str()),
                    ("task_id", scope.task_id.as_str()),
                ] {
                    if let Some(actual) = payload.get(field).and_then(serde_json::Value::as_str) {
                        if actual != expected {
                            return Err(EmitRefused {
                                reason: format!(
                                    "terminal journal event {event_type:?} carries {field}={actual:?}, which conflicts with its recorded canonical scope {expected:?}"
                                ),
                            });
                        }
                    }
                }
                let Some(execution_id) = self.runtime_execution_id else {
                    return Err(EmitRefused {
                        reason: "the committed run identity has no runtime execution id; a canonical scope cannot be reconstructed from the suffixed loop-state key"
                            .to_string(),
                    });
                };
                let (principal, workspace, task_id, ui_thread_id) =
                    scope.clone().into_scope_fields();
                let receipt = self
                    .broadcaster
                    .emit_recorded_runtime_fact_with_receipt(
                        event,
                        CanonicalEventScope {
                            principal,
                            workspace,
                            task_id,
                            execution_id: execution_id.to_string(),
                            ui_thread_id,
                        },
                    )
                    .map_err(|error| EmitRefused {
                        reason: error.to_string(),
                    })?;
                self.canonical_receipts.push(receipt);
            },
            RecordedEventRouting::TransportOnly => self.broadcaster.emit_transport_only(event),
            RecordedEventRouting::Unrecorded => {
                // A live-rule fallback may choose canonical persistence from a
                // process-local scope registration, but the loop cursor cannot
                // obtain a durability receipt for that implicit branch. It may
                // also choose transport-only after a cold restart. Advancing a
                // durable cursor across a route that changes with process state
                // is a false acknowledgement, so legacy unknown routing stops
                // here for explicit migration/operator handling.
                return Err(EmitRefused {
                    reason: format!(
                        "terminal journal event {event_type:?} predates recorded routing; a cold projector cannot prove whether its original delivery was canonical or transport-only"
                    ),
                });
            },
        }
        Ok(())
    }
}

impl ProjectedEventSink for BroadcasterEventSink<'_> {
    fn emit(
        &mut self,
        key: &EventKey,
        event_type: &str,
        payload: &serde_json::Value,
    ) -> Result<(), EmitRefused> {
        self.deliver(key, event_type, payload, &RecordedEventRouting::Unrecorded)
    }

    fn emit_routed(
        &mut self,
        key: &EventKey,
        event_type: &str,
        payload: &serde_json::Value,
        routing: &RecordedEventRouting,
    ) -> Result<(), EmitRefused> {
        self.deliver(key, event_type, payload, routing)
    }

    fn emit_named(
        &mut self,
        _key: &EventKey,
        name: &str,
        agent_id: &str,
        principal: Option<&str>,
        workspace: Option<&str>,
        payload: &serde_json::Value,
    ) -> Result<(), EmitRefused> {
        self.broadcaster
            .emit_named(name, agent_id, principal, workspace, payload.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::agentic::run_loop::journal::{
        JournalAppend, RecordedCanonicalScope, RecordedEventRouting, RecordedStep, TerminalKind,
    };
    use crate::magician_v2::execution::agentic::run_loop::outcome::Phase;
    use crate::magician_v2::execution::agentic::run_loop::store::contract::{fresh_state, key};
    use crate::magician_v2::execution::agentic::run_loop::store::memory::MemoryLoopStateStore;
    use crate::magician_v2::execution::agentic::run_loop::store::Revision;
    use crate::magician_v2::realtime_events::RuntimeTransportEvent;

    #[tokio::test]
    async fn blocking_projection_registry_refuses_late_work_and_waits_for_active_jobs() {
        let registry = TerminalProjectionJobRegistry::new();
        let registration = registry
            .register()
            .expect("registry accepts work before shutdown");

        assert!(
            tokio::time::timeout(Duration::from_millis(10), registry.quiesce())
                .await
                .is_err(),
            "quiescence must not report completion while blocking projection work is active"
        );
        assert!(registry.register().is_none());

        drop(registration);
        assert_eq!(registry.quiesce().await, 0);
    }

    struct FailingReceiptSink;

    impl crate::magician_v2::artifact_v2::RuntimeCanonicalEventSink for FailingReceiptSink {
        fn emit(
            &self,
            _scope: CanonicalEventScope,
            _event_type: crate::magician_v2::artifact_v2::ArtifactV2EventType,
            _payload: serde_json::Value,
        ) {
        }

        fn emit_with_receipt(
            &self,
            _scope: CanonicalEventScope,
            _event_type: crate::magician_v2::artifact_v2::ArtifactV2EventType,
            _payload: serde_json::Value,
        ) -> Result<RuntimeCanonicalEventReceipt, String> {
            let (completion, completed) = tokio::sync::oneshot::channel();
            completion
                .send(Err("fixture canonical append failure".to_string()))
                .expect("receipt receiver remains alive");
            Ok(RuntimeCanonicalEventReceipt::new(completed))
        }
    }

    fn status_append(execution_id: &str, routing: RecordedEventRouting) -> JournalAppend {
        let event = RuntimeTransportEvent::ExecutionStatusChanged {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            task_id: Some("task-1".to_string()),
            root_execution_id: Some(execution_id.to_string()),
            previous_status: "running".to_string(),
            new_status: "completed".to_string(),
            reason: None,
            timestamp: 1,
        };
        let mut encoded = serde_json::to_value(event).expect("encode status event");
        let object = encoded.as_object_mut().expect("tagged status event object");
        let event_type = object
            .remove("event_type")
            .and_then(|value| value.as_str().map(str::to_string))
            .expect("status event tag");
        let payload = object.remove("data").expect("status event payload");
        JournalAppend::event(1, Phase::Prepare, event_type, payload, routing)
            .expect("bounded status event")
    }

    fn canonical_status_append(execution_id: &str) -> JournalAppend {
        status_append(
            execution_id,
            RecordedEventRouting::CanonicalRuntimeFact {
                scope: RecordedCanonicalScope {
                    principal: "owner".to_string(),
                    workspace: "default".to_string(),
                    task_id: "task-1".to_string(),
                    ui_thread_id: "general".to_string(),
                },
            },
        )
    }

    async fn commit_terminal_with_event(
        store: &MemoryLoopStateStore,
        key: &ExecutionKey,
        event: JournalAppend,
    ) {
        let watermark = store
            .append_journal(
                key,
                &[
                    event,
                    JournalAppend::phase_completed(
                        1,
                        Phase::Prepare,
                        RecordedStep::RunEnded {
                            terminal: TerminalKind::Success,
                        },
                    ),
                ],
            )
            .await
            .expect("append terminal journal");
        let mut state = fresh_state(key);
        state.journal_seq = watermark;
        store
            .commit(key, &state, Revision::INITIAL)
            .await
            .expect("commit terminal state");
    }

    /// Regression for the production wire this module exists to provide: no
    /// phase host is composed, the exact terminal journal is delivered, and its
    /// durable cursor removes the key from the next debt scan.
    #[tokio::test]
    async fn a_host_free_pass_projects_and_retires_exact_terminal_debt() {
        let store = MemoryLoopStateStore::new();
        let key = key("terminal-projector");
        let watermark = store
            .append_journal(
                &key,
                &[
                    JournalAppend::named_event(
                        1,
                        Phase::Prepare,
                        "plan.step.finished",
                        "agent-1",
                        Some("owner".to_owned()),
                        Some("default".to_owned()),
                        serde_json::json!({
                            "execution_id": key.execution_id(),
                            "step_id": "last",
                        }),
                    )
                    .expect("bounded named event"),
                    JournalAppend::phase_completed(
                        1,
                        Phase::Prepare,
                        RecordedStep::RunEnded {
                            terminal: TerminalKind::Success,
                        },
                    ),
                ],
            )
            .await
            .expect("append terminal journal");
        let mut state = fresh_state(&key);
        state.journal_seq = watermark;
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit terminal state");

        let broadcaster = RuntimeTransportBroadcaster::new(8);
        let mut events = broadcaster.subscribe();
        let report = project_terminal_outbox_debt(
            &store,
            &broadcaster,
            None,
            &WorkerId::new("terminal-projector"),
            None,
            NonZeroUsize::new(8).expect("non-zero budget"),
            &CancellationToken::new(),
        )
        .await
        .expect("project terminal debt");

        assert_eq!(report.discovered, 1);
        assert_eq!(report.claimed, 1);
        assert_eq!(report.projected_events, 1);
        assert_eq!(report.cursor_save_failures, 0);
        let event = events.try_recv().expect("named event was broadcast");
        assert!(matches!(
            event,
            RuntimeTransportEvent::AgentEvent { event }
                if event.event_type == "plan.step.finished"
        ));

        let debt = store
            .scan_terminal_outbox_debt(NonZeroUsize::new(8).expect("non-zero budget"), None)
            .await
            .expect("rescan debt");
        assert!(debt.keys.is_empty(), "the saved cursor retires the debt");
    }

    #[tokio::test]
    async fn terminal_steer_receipt_is_discoverable_without_event_debt() {
        let store = MemoryLoopStateStore::new();
        let key = key("terminal-steer-receipt");
        let watermark = store
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Decide,
                    RecordedStep::RunEnded {
                        terminal: TerminalKind::Success,
                    },
                )],
            )
            .await
            .expect("append eventless terminal");
        let mut state = fresh_state(&key);
        state.journal_seq = watermark;
        state.steer_consume_receipt = Some(super::super::steer_inbox::SteerConsumeReceipt {
            schema_version: 1,
            execution_id: key.execution_id().to_string(),
            source_segment: key.execution_id().to_string(),
            iteration: 1,
            entries: vec![super::super::steer_inbox::SteerReceiptEntry {
                entry_id: "steer-1".to_string(),
                generation: 1,
                control_generation: "controls-1".to_string(),
                content_digest: "digest-1".to_string(),
            }],
        });
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit terminal receipt debt");

        let debt = store
            .scan_terminal_outbox_debt(NonZeroUsize::new(8).expect("non-zero budget"), None)
            .await
            .expect("scan receipt debt");
        assert_eq!(debt.keys, vec![key.clone()]);

        let report = project_terminal_outbox_debt(
            &store,
            &RuntimeTransportBroadcaster::new(8),
            None,
            &WorkerId::new("terminal-projector"),
            None,
            NonZeroUsize::new(8).expect("non-zero budget"),
            &CancellationToken::new(),
        )
        .await
        .expect("project without Artifact workspace");
        assert_eq!(report.steer_ack_failures, 1);
        assert!(
            store
                .load(&key)
                .await
                .expect("load terminal state")
                .expect("terminal state")
                .state
                .steer_consume_receipt
                .is_some(),
            "a missing acknowledgement owner must leave the debt discoverable"
        );
    }

    #[tokio::test]
    async fn nonterminal_eventless_steer_receipt_is_discoverable_without_an_ending_marker() {
        let store = MemoryLoopStateStore::new();
        let key = key("cancelled-nonterminal-steer-receipt");
        let mut state = fresh_state(&key);
        state.steer_consume_receipt = Some(super::super::steer_inbox::SteerConsumeReceipt {
            schema_version: 1,
            execution_id: key.execution_id().to_string(),
            source_segment: key.execution_id().to_string(),
            iteration: 1,
            entries: vec![super::super::steer_inbox::SteerReceiptEntry {
                entry_id: "steer-1".to_string(),
                generation: 1,
                control_generation: "controls-1".to_string(),
                content_digest: "digest-1".to_string(),
            }],
        });
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit receipt-only state");

        let debt = store
            .scan_terminal_outbox_debt(NonZeroUsize::new(8).expect("non-zero budget"), None)
            .await
            .expect("scan receipt-only debt");
        assert_eq!(debt.keys, vec![key]);
    }

    #[test]
    fn receipt_debt_is_retired_before_the_terminal_journal_gate() {
        let source = include_str!("terminal_outbox.rs");
        let claimed = source
            .split("async fn project_claimed_terminal")
            .nth(1)
            .and_then(|tail| tail.split("struct BroadcasterSink").next())
            .expect("claimed projector source window");
        let acknowledge = claimed
            .find("super::steer_inbox::acknowledge")
            .expect("receipt acknowledgement");
        let journal = claimed
            .find("store.replay_committed_journal")
            .expect("terminal journal gate");
        let projection = claimed
            .find(".read_journal_projection(")
            .expect("bounded projection window");
        assert!(acknowledge < journal);
        assert!(journal < projection);
        assert!(
            !claimed.contains("store.read_journal_verified"),
            "terminal lifecycle recovery must use the indexed replay/projection seams instead of reparsing the lifetime journal"
        );
    }

    #[tokio::test]
    async fn a_failed_canonical_append_does_not_advance_the_loop_cursor() {
        let store = MemoryLoopStateStore::new();
        let key = key("terminal-canonical-append-fails");
        commit_terminal_with_event(&store, &key, canonical_status_append(key.execution_id())).await;

        let broadcaster = RuntimeTransportBroadcaster::new(8);
        broadcaster.set_runtime_canonical_event_sink(std::sync::Arc::new(FailingReceiptSink));
        let report = project_terminal_outbox_debt(
            &store,
            &broadcaster,
            None,
            &WorkerId::new("terminal-projector"),
            None,
            NonZeroUsize::new(8).expect("non-zero budget"),
            &CancellationToken::new(),
        )
        .await
        .expect("project terminal debt");

        assert_eq!(report.canonical_persistence_failures, 1);
        assert!(
            store
                .load_projector_cursor(&key)
                .await
                .expect("read cursor")
                .is_none(),
            "queue admission or transport broadcast must not count as durable canonical delivery"
        );
        let debt = store
            .scan_terminal_outbox_debt(NonZeroUsize::new(8).expect("non-zero budget"), None)
            .await
            .expect("rescan debt");
        assert_eq!(debt.keys, vec![key]);
    }

    #[tokio::test]
    async fn an_unmappable_cold_event_stays_below_the_durable_cursor() {
        let store = MemoryLoopStateStore::new();
        let key = key("terminal-unmappable-event");
        let event = JournalAppend::event(
            1,
            Phase::Prepare,
            "runtime.event.from.a.newer.build",
            serde_json::json!({"execution_id": key.execution_id()}),
            RecordedEventRouting::TransportOnly,
        )
        .expect("bounded unknown event");
        commit_terminal_with_event(&store, &key, event).await;

        let report = project_terminal_outbox_debt(
            &store,
            &RuntimeTransportBroadcaster::new(8),
            None,
            &WorkerId::new("terminal-projector"),
            None,
            NonZeroUsize::new(8).expect("non-zero budget"),
            &CancellationToken::new(),
        )
        .await
        .expect("project terminal debt");

        assert_eq!(report.unmappable_events, 1);
        assert_eq!(report.projected_events, 0);
        assert!(store
            .load_projector_cursor(&key)
            .await
            .expect("read cursor")
            .is_none());
        let debt = store
            .scan_terminal_outbox_debt(NonZeroUsize::new(8).expect("non-zero budget"), None)
            .await
            .expect("rescan debt");
        assert_eq!(debt.keys, vec![key]);
    }

    #[tokio::test]
    async fn legacy_unrecorded_routing_is_not_falsely_acknowledged() {
        let store = MemoryLoopStateStore::new();
        let key = key("terminal-unrecorded-route");
        commit_terminal_with_event(
            &store,
            &key,
            status_append(key.execution_id(), RecordedEventRouting::Unrecorded),
        )
        .await;

        let report = project_terminal_outbox_debt(
            &store,
            &RuntimeTransportBroadcaster::new(8),
            None,
            &WorkerId::new("terminal-projector"),
            None,
            NonZeroUsize::new(8).expect("non-zero budget"),
            &CancellationToken::new(),
        )
        .await
        .expect("project terminal debt");

        assert_eq!(report.projected_events, 0);
        assert!(
            store
                .load_projector_cursor(&key)
                .await
                .expect("read cursor")
                .is_none(),
            "an ambient live scope must not turn an unknown legacy route into a durable acknowledgement"
        );
    }

    #[tokio::test]
    async fn eventless_cannot_proceed_requires_runtime_acceptance_before_receipt() {
        let store = MemoryLoopStateStore::new();
        let key = key("eventless-cannot-proceed");
        let watermark = store
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Epilogue,
                    RecordedStep::RunEnded {
                        terminal: TerminalKind::CannotProceed,
                    },
                )],
            )
            .await
            .expect("append reconciler-style retirement");
        let mut state = fresh_state(&key);
        state.journal_seq = watermark;
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit retirement");

        let report = project_terminal_outbox_debt(
            &store,
            &RuntimeTransportBroadcaster::new(8),
            None,
            &WorkerId::new("terminal-projector"),
            None,
            NonZeroUsize::new(8).expect("non-zero budget"),
            &CancellationToken::new(),
        )
        .await
        .expect("project eventless retirement");
        assert_eq!(report.discovered, 1);
        assert_eq!(report.projected_events, 0);
        assert_eq!(report.runtime_settlement_failures, 1);
        assert!(
            store
                .load_projector_cursor(&key)
                .await
                .expect("load cursor")
                .is_none(),
            "missing runtime acceptance cannot publish a false receipt"
        );
        assert_eq!(
            store
                .scan_terminal_outbox_debt(NonZeroUsize::new(8).expect("non-zero budget"), None,)
                .await
                .expect("rescan debt")
                .keys,
            vec![key]
        );
    }

    #[tokio::test]
    async fn deleted_task_terminal_debt_requires_complete_deletion_and_preserves_journal() {
        use crate::magician_v2::auth::ScopeRef;
        let temp = tempfile::tempdir().unwrap();
        let service = crate::magician_v2::test_support::build_test_artifact_v2_service(temp.path());
        let workspace = service.workspace();
        let scope = ScopeRef::system_internal_unauthenticated("owner", "default");
        let task_id = "task_deleted_terminal";
        let key = key("exec-deleted-terminal");
        let store = MemoryLoopStateStore::new();
        let watermark = store
            .append_journal(
                &key,
                &[
                    status_append(key.execution_id(), RecordedEventRouting::TransportOnly),
                    JournalAppend::phase_completed(
                        1,
                        Phase::Epilogue,
                        RecordedStep::RunEnded {
                            terminal: TerminalKind::CannotProceed,
                        },
                    ),
                ],
            )
            .await
            .unwrap();
        let mut state = fresh_state(&key);
        state.identity.task_id = Some(task_id.to_owned());
        state.identity.agent_id = Some("researcher".to_owned());
        state.segment_binding = Some(super::super::state::LoopSegmentBinding {
            base_execution_id: key.execution_id().to_owned(),
            exact_segment_id: key.execution_id().to_owned(),
            preseed_admission_token: None,
        });
        state.journal_seq = watermark;
        store.commit(&key, &state, Revision::INITIAL).await.unwrap();
        let before = store
            .read_journal_projection(&key, 1, watermark, true)
            .await
            .unwrap();
        let marker = workspace.task_deletion_marker_path("owner", "default", task_id);
        let user_dir = workspace.user_visible_task_dir("owner", "default", task_id);
        let internal_dir = workspace.internal_task_dir("owner", "default", task_id);
        let broadcaster = RuntimeTransportBroadcaster::new(8);
        let mut events = broadcaster.subscribe();
        let worker = WorkerId::new("deleted-terminal-projector");
        let cancellation = CancellationToken::new();
        let budget = NonZeroUsize::new(8).unwrap();

        // Absence alone, partial deletion in either root, and an invalid
        // deletion marker must all retain the exact recovery debt.
        for case in 0..4 {
            if case == 1 {
                workspace
                    .create_dir_all_path(marker.parent().unwrap())
                    .await
                    .unwrap();
                workspace
                    .write_path(&marker, chrono::Utc::now().to_rfc3339().as_bytes())
                    .await
                    .unwrap();
                workspace.create_dir_all_path(&user_dir).await.unwrap();
                workspace
                    .write_path(user_dir.join("task.json"), b"surviving authority")
                    .await
                    .unwrap();
            } else if case == 2 {
                workspace.remove_dir_all_path(&user_dir).await.unwrap();
                workspace.create_dir_all_path(&internal_dir).await.unwrap();
                workspace
                    .write_path(internal_dir.join("task.json"), b"surviving authority")
                    .await
                    .unwrap();
            } else if case == 3 {
                workspace.remove_dir_all_path(&internal_dir).await.unwrap();
                workspace.write_path(&marker, b"invalid").await.unwrap();
            }
            let report = project_terminal_outbox_debt(
                &store,
                &broadcaster,
                Some(&service),
                &worker,
                None,
                budget,
                &cancellation,
            )
            .await
            .unwrap();
            assert_eq!(report.runtime_settlement_failures, 1, "case {case}");
            assert!(store.load_projector_cursor(&key).await.unwrap().is_none());
            assert!(events.try_recv().is_err());
        }
        service
            .archive_task_with_options(&scope, task_id, true)
            .await
            .unwrap();
        let mut wrong_identity = state.identity.clone();
        wrong_identity.workspace = Some("other".to_owned());
        assert!(service
            .acquire_terminal_loop_receipt_lifecycle_exclusion(
                &key,
                &wrong_identity,
                state.segment_binding.as_ref(),
                TerminalKind::CannotProceed,
                watermark,
                None
            )
            .await
            .is_err());
        assert!(service
            .acquire_terminal_loop_receipt_lifecycle_exclusion(
                &key,
                &state.identity,
                None,
                TerminalKind::CannotProceed,
                watermark,
                None
            )
            .await
            .is_err());

        let report = project_terminal_outbox_debt(
            &store,
            &broadcaster,
            Some(&service),
            &worker,
            None,
            budget,
            &cancellation,
        )
        .await
        .unwrap();
        assert_eq!(report.runtime_settlement_failures, 0);
        assert_eq!(report.cursor_save_failures, 0);
        assert_eq!(report.projected_events, 0);
        assert!(events.try_recv().is_err());
        let cursor = store.load_projector_cursor(&key).await.unwrap().unwrap();
        assert_eq!(cursor.emitted_through_seq(), watermark);
        assert_eq!(cursor.runtime_settled_terminal_seq(), Some(watermark));
        assert!(store
            .scan_terminal_outbox_debt(budget, None)
            .await
            .unwrap()
            .keys
            .is_empty());
        let after = store
            .read_journal_projection(&key, 1, watermark, true)
            .await
            .unwrap();
        assert_eq!(
            before, after,
            "retirement must preserve the immutable journal"
        );
        assert!(!user_dir.exists() && !internal_dir.exists());

        // A process can settle runtime first, then lose the event delivery.
        // Deletion must also supersede that remaining batch on a later pass.
        let replay_store = MemoryLoopStateStore::new();
        let replay_watermark = replay_store
            .append_journal(
                &key,
                &[
                    status_append(key.execution_id(), RecordedEventRouting::TransportOnly),
                    JournalAppend::phase_completed(
                        1,
                        Phase::Epilogue,
                        RecordedStep::RunEnded {
                            terminal: TerminalKind::CannotProceed,
                        },
                    ),
                ],
            )
            .await
            .unwrap();
        state.journal_seq = replay_watermark;
        replay_store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .unwrap();
        let mut replay_cursor = ProjectorCursor::new();
        replay_cursor.mark_runtime_terminal_settled(replay_watermark);
        replay_store
            .save_projector_cursor(&key, &replay_cursor)
            .await
            .unwrap();
        let report = project_terminal_outbox_debt(
            &replay_store,
            &broadcaster,
            Some(&service),
            &worker,
            None,
            budget,
            &cancellation,
        )
        .await
        .unwrap();
        assert_eq!(report.runtime_settlement_failures, 0);
        assert_eq!(report.cursor_save_failures, 0);
        assert_eq!(report.projected_events, 0);
        assert!(events.try_recv().is_err());
        assert_eq!(
            replay_store
                .load_projector_cursor(&key)
                .await
                .unwrap()
                .unwrap()
                .emitted_through_seq(),
            replay_watermark
        );
    }

    /// Runtime/outer-pipeline settlement is part of terminal delivery, not a
    /// best-effort side effect after acknowledgement. A crash or refusal at
    /// that bridge must leave the loop cursor behind the terminal watermark.
    #[test]
    fn artifact_settlement_precedes_terminal_projector_cursor_save() {
        let source = include_str!("terminal_outbox.rs");
        let claimed = source
            .split("async fn project_claimed_terminal")
            .nth(1)
            .and_then(|tail| tail.split("struct BroadcasterSink").next())
            .expect("claimed terminal projector source window");
        let settlement = claimed
            .find("accept_terminal_loop_settlement")
            .expect("Artifact/runtime settlement bridge");
        let bounded_settlement = claimed[..settlement]
            .rfind("let accepted = tokio::select!")
            .expect("bounded cancellable Artifact/runtime settlement");
        let save = claimed
            .find("save_projector_cursor_fenced")
            .expect("durable terminal projector acknowledgement");
        assert!(bounded_settlement < settlement && settlement < save);
        assert!(claimed[settlement..save].contains("return;"));
        assert!(claimed[bounded_settlement..settlement].contains("timeout_at"));
        assert!(claimed[bounded_settlement..settlement].contains("cancellation.cancelled()"));
    }

    /// Runtime/Artifact settlement and canonical persistence are independent
    /// durable transactions, each with a fresh bounded deadline.
    #[test]
    fn artifact_settlement_renews_its_lease_and_uses_a_fresh_deadline() {
        let source = include_str!("terminal_outbox.rs");
        let claimed = source
            .split("async fn project_claimed_terminal")
            .nth(1)
            .and_then(|tail| tail.split("struct BroadcasterSink").next())
            .expect("claimed terminal projector source window");
        let canonical_deadline = claimed
            .find("let canonical_deadline")
            .expect("canonical persistence deadline");
        let settlement_deadline = claimed
            .find("let settlement_deadline")
            .expect("fresh Artifact/runtime settlement deadline");
        let settlement = claimed
            .find("accept_terminal_loop_settlement")
            .expect("Artifact/runtime settlement bridge");
        let renew = claimed[..settlement]
            .rfind("store.renew(lease, TERMINAL_PROJECTION_LEASE_TTL)")
            .expect("lease renewal immediately before settlement");

        assert!(settlement_deadline < canonical_deadline);
        assert!(renew < settlement_deadline && settlement_deadline < settlement);
        assert!(claimed[settlement_deadline..settlement]
            .contains("TERMINAL_RUNTIME_SETTLEMENT_TIMEOUT"));
        assert!(claimed[settlement_deadline..settlement]
            .contains("timeout_at(\n                settlement_deadline"));
    }

    #[test]
    fn receipt_pause_is_published_only_after_canonical_terminal_batch_is_durable() {
        let source = include_str!("terminal_outbox.rs");
        let claimed = source
            .split("async fn project_claimed_terminal")
            .nth(1)
            .and_then(|tail| tail.split("struct BroadcasterSink").next())
            .expect("claimed terminal projector source window");
        let settlement = claimed
            .find("accept_terminal_loop_settlement")
            .expect("runtime and Artifact settlement");
        let canonical_ack = claimed
            .find("receipt.wait_persisted()")
            .expect("canonical event durability");
        let publication = claimed
            .find("publish_terminal_loop_pause")
            .expect("exact staged pause publication");
        let save = claimed[publication..]
            .find("save_projector_cursor_fenced")
            .map(|offset| publication + offset)
            .expect("combined projector receipt");
        assert!(settlement < canonical_ack);
        assert!(canonical_ack < publication);
        assert!(publication < save);
        assert!(claimed[..publication].contains("cursor.emitted_through_seq() == watermark"));
    }

    /// Ordinary runtime-backed terminals need the same cross-layer receipt as
    /// a reconciler retirement. The exact watermark and LoopState seal must
    /// reach the runtime/Artifact owner before the projector suppresses debt.
    #[test]
    fn ordinary_terminal_settlement_is_receipt_and_watermark_bound() {
        let source = include_str!("terminal_outbox.rs");
        let claimed = source
            .split("async fn project_claimed_terminal")
            .nth(1)
            .and_then(|tail| tail.split("struct BroadcasterSink").next())
            .expect("claimed terminal projector source window");
        let normalized = claimed.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(normalized.contains("terminal != crate::magician_v2::execution::agentic::run_loop::journal::TerminalKind::HandedOff"));
        assert!(claimed.contains("committed.state.terminal_settlement_receipt.as_ref()"));
        let settlement = claimed
            .find("accept_terminal_loop_settlement")
            .expect("Artifact settlement");
        let receipt = claimed[settlement..]
            .find("terminal_settlement_receipt")
            .map(|offset| settlement + offset)
            .expect("bounded exact receipt argument");
        let save = claimed
            .find("save_projector_cursor_fenced")
            .expect("cursor save");
        assert!(settlement < receipt && receipt < save);
    }

    #[test]
    fn every_first_time_runtime_settlement_reacquires_lifecycle_exclusion() {
        let source = include_str!("terminal_outbox.rs");
        let claimed = source
            .split("async fn project_claimed_terminal")
            .nth(1)
            .and_then(|tail| tail.split("struct BroadcasterSink").next())
            .expect("claimed terminal projector source window");
        let requirement = claimed
            .find("let runtime_settlement_acceptance_required")
            .expect("first-time settlement predicate");
        let exclusion = claimed
            .find("acquire_terminal_loop_receipt_lifecycle_exclusion")
            .expect("cross-layer lifecycle exclusion");
        let settlement = claimed
            .find("accept_terminal_loop_settlement")
            .expect("Artifact settlement");
        assert!(requirement < exclusion && exclusion < settlement);
        assert!(claimed[requirement..exclusion]
            .contains("cursor.runtime_settled_terminal_seq() != Some(watermark)"));
        assert!(claimed[..settlement].contains("if runtime_settlement_acceptance_required"));
    }

    #[test]
    fn taskless_runtime_and_receipt_backed_hitl_keep_settlement_debt() {
        let source = include_str!("terminal_outbox.rs");
        let claimed = source
            .split("async fn project_claimed_terminal")
            .nth(1)
            .and_then(|tail| tail.split("struct BroadcasterSink").next())
            .expect("claimed terminal projector");
        assert!(claimed.contains("identity.execution_id.is_some()"));
        assert!(claimed.contains("receipt_terminal_is_exact"));
        assert!(claimed.contains("terminal_settlement_receipt"));
    }

    #[test]
    fn partially_saved_pause_receipt_rechecks_cancellation_before_event_publication() {
        let source = include_str!("terminal_outbox.rs");
        let claimed = source
            .split("async fn project_claimed_terminal")
            .nth(1)
            .and_then(|tail| tail.split("struct BroadcasterSink").next())
            .expect("claimed terminal projector");
        let settlement = claimed
            .find("accept_terminal_loop_settlement")
            .expect("repeatable lifecycle acceptance");
        let projection = claimed
            .find("read_journal_projection")
            .expect("canonical event projection");
        let cancellation = claimed[settlement..projection]
            .find("SupersededByCancellation")
            .map(|offset| settlement + offset)
            .expect("durable cancellation disposition");
        assert!(claimed[..settlement].contains("|| pause_publication_required"));
        assert!(settlement < cancellation && cancellation < projection);
        assert!(claimed[cancellation..projection].contains("suppress_terminal_batch_through"));
        assert!(claimed[cancellation..projection].contains("return;"));
    }
}

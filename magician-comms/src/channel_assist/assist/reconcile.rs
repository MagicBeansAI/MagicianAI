//! Provider-neutral follow-up reconciliation.
//!
//! This worker does not know about Gmail, WhatsApp, AgentMail, or future
//! adapters. It reads the normalized channel-assist store and retires active
//! follow-up annotations when newer locally-distilled channel evidence proves
//! the work was handled or replaced.

use std::{
    sync::{
        atomic::{AtomicI64, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Duration,
};

use anyhow::Result;
use serde_json::json;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use magician::magician_v2::artifact_v2::workspace::{
    DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};

use super::store::{AnnotationTransitionResult, MailAssistStore};
use super::types::{
    derive_channel_required_action, ChannelRequiredActionSource, MailAnnotationState,
    MailAssistActor, MailMessageMeta, MailThreadAnnotation, MessageDirection, ProviderThreadChange,
    ProviderThreadChangeKind,
};

const LOG_TARGET: &str = "magician::channel_assist::reconcile";
/// The one resolution that proves the owner performed the work: the follow-up
/// said the owner owed an action, and an outbound message followed the
/// evidence. Named so the emitter and the learning sink cannot drift apart.
const OWNER_COMPLETION_REASON: &str = "owner_or_agent_sent_after_evidence";
const DEFAULT_INTERVAL_SECS: u64 = 60;
const DEFAULT_STARTUP_DELAY_SECS: u64 = 180;
const DEFAULT_BATCH: usize = 100;
const DEFAULT_NEWER_MESSAGE_LIMIT: usize = 20;
/// Retire untouched active work after a month by default. Setting
/// `CHANNEL_RECONCILE_STALE_DAYS=0` remains an explicit opt-out.
const DEFAULT_STALE_DAYS: u64 = 30;
static METRICS: OnceLock<ChannelReconcileMetrics> = OnceLock::new();

#[derive(Debug)]
struct ChannelReconcileMetrics {
    last_started_at: AtomicI64,
    last_completed_at: AtomicI64,
    total_passes: AtomicU64,
    total_scanned: AtomicU64,
    total_completed: AtomicU64,
    total_superseded: AtomicU64,
    total_stale: AtomicU64,
    total_kept_active: AtomicU64,
    total_blocked_by_claim: AtomicU64,
    total_unexpected_state: AtomicU64,
    total_errors: AtomicU64,
    last_scanned: AtomicU64,
    last_completed: AtomicU64,
    last_superseded: AtomicU64,
    last_stale: AtomicU64,
    last_kept_active: AtomicU64,
    last_blocked_by_claim: AtomicU64,
    last_unexpected_state: AtomicU64,
    last_errors: AtomicU64,
    last_error: Mutex<Option<String>>,
}

impl ChannelReconcileMetrics {
    fn new() -> Self {
        Self {
            last_started_at: AtomicI64::new(0),
            last_completed_at: AtomicI64::new(0),
            total_passes: AtomicU64::new(0),
            total_scanned: AtomicU64::new(0),
            total_completed: AtomicU64::new(0),
            total_superseded: AtomicU64::new(0),
            total_stale: AtomicU64::new(0),
            total_kept_active: AtomicU64::new(0),
            total_blocked_by_claim: AtomicU64::new(0),
            total_unexpected_state: AtomicU64::new(0),
            total_errors: AtomicU64::new(0),
            last_scanned: AtomicU64::new(0),
            last_completed: AtomicU64::new(0),
            last_superseded: AtomicU64::new(0),
            last_stale: AtomicU64::new(0),
            last_kept_active: AtomicU64::new(0),
            last_blocked_by_claim: AtomicU64::new(0),
            last_unexpected_state: AtomicU64::new(0),
            last_errors: AtomicU64::new(0),
            last_error: Mutex::new(None),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChannelReconcileConfig {
    pub enabled: bool,
    pub interval: Duration,
    pub startup_delay: Duration,
    pub batch: usize,
    pub newer_message_limit: usize,
    pub stale_after: Duration,
}

impl Default for ChannelReconcileConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: Duration::from_secs(DEFAULT_INTERVAL_SECS),
            startup_delay: Duration::from_secs(DEFAULT_STARTUP_DELAY_SECS),
            batch: DEFAULT_BATCH,
            newer_message_limit: DEFAULT_NEWER_MESSAGE_LIMIT,
            stale_after: Duration::from_secs(DEFAULT_STALE_DAYS * 24 * 60 * 60),
        }
    }
}

impl ChannelReconcileConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Ok(raw) = std::env::var("CHANNEL_RECONCILE_ENABLED") {
            config.enabled = !matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            );
        }
        if let Some(secs) = env_parse::<u64>("CHANNEL_RECONCILE_INTERVAL_SECS") {
            if secs > 0 {
                config.interval = Duration::from_secs(secs);
            }
        }
        if let Some(secs) = env_parse::<u64>("CHANNEL_RECONCILE_STARTUP_DELAY_SECS") {
            config.startup_delay = Duration::from_secs(secs);
        }
        if let Some(batch) = env_parse::<usize>("CHANNEL_RECONCILE_BATCH") {
            if batch > 0 {
                config.batch = batch;
            }
        }
        if let Some(limit) = env_parse::<usize>("CHANNEL_RECONCILE_NEWER_MESSAGE_LIMIT") {
            if limit > 0 {
                config.newer_message_limit = limit;
            }
        }
        if let Some(days) = env_parse::<u64>("CHANNEL_RECONCILE_STALE_DAYS") {
            config.stale_after = Duration::from_secs(days.saturating_mul(24 * 60 * 60));
        }
        config
    }
}

fn env_parse<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

fn metrics() -> &'static ChannelReconcileMetrics {
    METRICS.get_or_init(ChannelReconcileMetrics::new)
}

fn timestamp_or_null(value: i64) -> serde_json::Value {
    if value > 0 {
        json!(value)
    } else {
        serde_json::Value::Null
    }
}

pub fn runtime_snapshot_json() -> serde_json::Value {
    let metrics = metrics();
    let last_error = metrics
        .last_error
        .lock()
        .expect("mail reconcile metrics mutex poisoned")
        .clone();
    json!({
        "last_started_at": timestamp_or_null(metrics.last_started_at.load(Ordering::Relaxed)),
        "last_completed_at": timestamp_or_null(metrics.last_completed_at.load(Ordering::Relaxed)),
        "total": {
            "passes": metrics.total_passes.load(Ordering::Relaxed),
            "scanned": metrics.total_scanned.load(Ordering::Relaxed),
            "completed": metrics.total_completed.load(Ordering::Relaxed),
            "superseded": metrics.total_superseded.load(Ordering::Relaxed),
            "stale": metrics.total_stale.load(Ordering::Relaxed),
            "kept_active": metrics.total_kept_active.load(Ordering::Relaxed),
            "blocked_by_claim": metrics.total_blocked_by_claim.load(Ordering::Relaxed),
            "unexpected_state": metrics.total_unexpected_state.load(Ordering::Relaxed),
            "errors": metrics.total_errors.load(Ordering::Relaxed),
        },
        "last": {
            "scanned": metrics.last_scanned.load(Ordering::Relaxed),
            "completed": metrics.last_completed.load(Ordering::Relaxed),
            "superseded": metrics.last_superseded.load(Ordering::Relaxed),
            "stale": metrics.last_stale.load(Ordering::Relaxed),
            "kept_active": metrics.last_kept_active.load(Ordering::Relaxed),
            "blocked_by_claim": metrics.last_blocked_by_claim.load(Ordering::Relaxed),
            "unexpected_state": metrics.last_unexpected_state.load(Ordering::Relaxed),
            "errors": metrics.last_errors.load(Ordering::Relaxed),
            "error": last_error,
        }
    })
}

fn record_pass_started(started_at: i64) {
    metrics()
        .last_started_at
        .store(started_at, Ordering::Relaxed);
}

fn record_pass_success(completed_at: i64, outcome: &ReconcilePassOutcome) {
    let metrics = metrics();
    metrics.total_passes.fetch_add(1, Ordering::Relaxed);
    metrics
        .total_scanned
        .fetch_add(outcome.scanned as u64, Ordering::Relaxed);
    metrics
        .total_completed
        .fetch_add(outcome.completed as u64, Ordering::Relaxed);
    metrics
        .total_superseded
        .fetch_add(outcome.superseded as u64, Ordering::Relaxed);
    metrics
        .total_stale
        .fetch_add(outcome.stale as u64, Ordering::Relaxed);
    metrics
        .total_kept_active
        .fetch_add(outcome.kept_active as u64, Ordering::Relaxed);
    metrics
        .total_blocked_by_claim
        .fetch_add(outcome.blocked_by_claim as u64, Ordering::Relaxed);
    metrics
        .total_unexpected_state
        .fetch_add(outcome.unexpected_state as u64, Ordering::Relaxed);
    metrics
        .total_errors
        .fetch_add(outcome.errors as u64, Ordering::Relaxed);
    metrics
        .last_completed_at
        .store(completed_at, Ordering::Relaxed);
    metrics
        .last_scanned
        .store(outcome.scanned as u64, Ordering::Relaxed);
    metrics
        .last_completed
        .store(outcome.completed as u64, Ordering::Relaxed);
    metrics
        .last_superseded
        .store(outcome.superseded as u64, Ordering::Relaxed);
    metrics
        .last_stale
        .store(outcome.stale as u64, Ordering::Relaxed);
    metrics
        .last_kept_active
        .store(outcome.kept_active as u64, Ordering::Relaxed);
    metrics
        .last_blocked_by_claim
        .store(outcome.blocked_by_claim as u64, Ordering::Relaxed);
    metrics
        .last_unexpected_state
        .store(outcome.unexpected_state as u64, Ordering::Relaxed);
    metrics
        .last_errors
        .store(outcome.errors as u64, Ordering::Relaxed);
    *metrics
        .last_error
        .lock()
        .expect("mail reconcile metrics mutex poisoned") = None;
}

fn record_pass_failure(completed_at: i64, error: &anyhow::Error) {
    let metrics = metrics();
    metrics.total_passes.fetch_add(1, Ordering::Relaxed);
    metrics.total_errors.fetch_add(1, Ordering::Relaxed);
    metrics
        .last_completed_at
        .store(completed_at, Ordering::Relaxed);
    metrics.last_scanned.store(0, Ordering::Relaxed);
    metrics.last_completed.store(0, Ordering::Relaxed);
    metrics.last_superseded.store(0, Ordering::Relaxed);
    metrics.last_stale.store(0, Ordering::Relaxed);
    metrics.last_kept_active.store(0, Ordering::Relaxed);
    metrics.last_blocked_by_claim.store(0, Ordering::Relaxed);
    metrics.last_unexpected_state.store(0, Ordering::Relaxed);
    metrics.last_errors.store(1, Ordering::Relaxed);
    *metrics
        .last_error
        .lock()
        .expect("mail reconcile metrics mutex poisoned") = Some(error.to_string());
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReconcilePassOutcome {
    pub scanned: usize,
    pub completed: usize,
    pub superseded: usize,
    pub stale: usize,
    pub kept_active: usize,
    pub routing_retracted: usize,
    pub blocked_by_claim: usize,
    pub unexpected_state: usize,
    pub errors: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileDecision {
    KeepActive {
        reason: &'static str,
    },
    Complete {
        reason: &'static str,
        resolution_message_id: String,
        resolution_message_at: i64,
    },
    CompleteProviderChange {
        reason: &'static str,
        change_id: String,
        change_at: i64,
    },
    Supersede {
        reason: &'static str,
        resolution_message_id: String,
        resolution_message_at: i64,
    },
    Stale {
        reason: &'static str,
    },
    /// The deterministic derivation no longer promotes this thread. Used when a
    /// routing rule changes under annotations that were already written: the
    /// evidence is unchanged, so nothing here is "resolved" or "stale" — the
    /// promotion was simply never warranted.
    RoutingRetracted {
        reason: &'static str,
    },
}

impl ReconcileDecision {
    /// The state a decision transitions its annotation to (`None` = keep the
    /// current state). `pub` since plan 3.1 so the assist seam's rule-matrix
    /// tests can pin the retire/supersede/require-review mapping; production
    /// callers remain in-module.
    pub fn target_state(&self, current: MailAnnotationState) -> Option<MailAnnotationState> {
        match self {
            Self::KeepActive { .. } => None,
            Self::Complete { .. } | Self::CompleteProviderChange { .. } => Some(match current {
                MailAnnotationState::DraftReady | MailAnnotationState::Inserted => {
                    MailAnnotationState::SentDetected
                },
                MailAnnotationState::SentDetected => MailAnnotationState::Completed,
                _ => MailAnnotationState::Completed,
            }),
            Self::Supersede { .. } => Some(MailAnnotationState::Superseded),
            // Stale rather than Dismissed: the owner never judged this, and the
            // stale path already offers Review & re-open if the retraction is
            // wrong.
            Self::Stale { .. } | Self::RoutingRetracted { .. } => Some(MailAnnotationState::Stale),
        }
    }

    /// The decision's reason token (persisted in the audit detail and
    /// matched by the completion-port gate). `pub` since plan 3.1 for the
    /// seam's rule-matrix tests; production callers remain in-module.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::KeepActive { reason }
            | Self::Complete { reason, .. }
            | Self::CompleteProviderChange { reason, .. }
            | Self::Supersede { reason, .. }
            | Self::Stale { reason }
            | Self::RoutingRetracted { reason } => reason,
        }
    }
}

pub fn reconcile_annotation(
    annotation: &MailThreadAnnotation,
    newer_messages: &[MailMessageMeta],
    now_ms: i64,
    stale_after: Duration,
) -> ReconcileDecision {
    reconcile_annotation_with_provider_changes(annotation, newer_messages, &[], now_ms, stale_after)
}

pub fn reconcile_annotation_with_provider_changes(
    annotation: &MailThreadAnnotation,
    newer_messages: &[MailMessageMeta],
    provider_changes: &[ProviderThreadChange],
    now_ms: i64,
    stale_after: Duration,
) -> ReconcileDecision {
    if !is_active_state(annotation.state) {
        return ReconcileDecision::KeepActive {
            reason: "terminal_or_quiet_state",
        };
    }

    let follow_up_kind = action_string(annotation, "follow_up_kind")
        .or_else(|| action_string(annotation, "action_kind"));
    let action_owner = action_string(annotation, "action_owner");
    let owner_owes = annotation.label.as_deref() == Some("needs_reply")
        || matches!(
            follow_up_kind.as_deref(),
            Some("needs_reply" | "owner_owes" | "schedule")
        )
        || matches!(action_owner.as_deref(), Some("owner" | "agent"));
    let counterparty_owes = matches!(
        follow_up_kind.as_deref(),
        Some("other_owes" | "waiting_on" | "check_back")
    ) || matches!(action_owner.as_deref(), Some("counterparty"));

    if owner_owes {
        if let Some(message) = newer_messages
            .iter()
            .find(|message| message.direction == Some(MessageDirection::Outbound))
        {
            return ReconcileDecision::Complete {
                reason: OWNER_COMPLETION_REASON,
                resolution_message_id: message.message_id.clone(),
                resolution_message_at: message.internal_date,
            };
        }
    }

    if let Some(change) = provider_changes
        .iter()
        .find(|change| change.closes_active_work())
    {
        let reason = match change.kind {
            ProviderThreadChangeKind::MessageDeleted => "provider_message_deleted",
            ProviderThreadChangeKind::LabelsAdded => "provider_thread_trashed_or_spam",
            ProviderThreadChangeKind::LabelsRemoved => "provider_thread_archived",
        };
        return ReconcileDecision::CompleteProviderChange {
            reason,
            change_id: change.id.clone(),
            change_at: change.observed_at,
        };
    }

    // An outbound message is handled above as a manual/agent send. Any other
    // newer evidence invalidates the assumptions behind a pending or inserted
    // draft and must be reviewed rather than silently superseded.
    if matches!(
        annotation.state,
        MailAnnotationState::DraftRequested
            | MailAnnotationState::DraftReady
            | MailAnnotationState::Inserted
    ) && newer_messages
        .iter()
        .any(|message| message.direction != Some(MessageDirection::Outbound))
    {
        return ReconcileDecision::Stale {
            reason: "newer_message_requires_draft_review",
        };
    }

    if counterparty_owes {
        if let Some(message) = newer_messages
            .iter()
            .find(|message| message.direction == Some(MessageDirection::Inbound))
        {
            if inbound_message_resolves_waiting_on(message) {
                return ReconcileDecision::Complete {
                    reason: "counterparty_replied_to_waiting_on",
                    resolution_message_id: message.message_id.clone(),
                    resolution_message_at: message.internal_date,
                };
            }
            return ReconcileDecision::Supersede {
                reason: "counterparty_message_changes_followup",
                resolution_message_id: message.message_id.clone(),
                resolution_message_at: message.internal_date,
            };
        }
    }

    if let Some(message) = newer_messages.first() {
        return ReconcileDecision::Supersede {
            reason: "newer_evidence_supersedes",
            resolution_message_id: message.message_id.clone(),
            resolution_message_at: message.internal_date,
        };
    }

    let basis = annotation
        .evidence_message_at
        .unwrap_or(annotation.created_at);
    let stale_after_ms = duration_millis_i64(stale_after);
    if stale_after_ms > 0 && now_ms.saturating_sub(basis) >= stale_after_ms {
        return ReconcileDecision::Stale {
            reason: "stale_window_elapsed",
        };
    }

    ReconcileDecision::KeepActive {
        reason: "no_newer_reconciling_evidence",
    }
}

/// Whether an annotation promoted purely by the information brief would still
/// be promoted by the current derivation.
///
/// A routing rule can change under annotations that were already written — the
/// audience-addressed promotion/event suppression did exactly that to roughly
/// 1.7k stored rows. Re-deriving from the message's own persisted brief costs
/// no model call, so the lane can be repaired locally instead of waiting for
/// every thread to be re-classified.
///
/// Deliberately narrow: only annotations whose recorded
/// `required_action_source` is the information brief are eligible. An
/// annotation promoted by an explicit reply hint, follow-up hint, or declared
/// intent is left alone even if the brief no longer implies work, because that
/// evidence is stronger than the brief and is not what changed.
fn routing_retraction_reason(
    annotation: &MailThreadAnnotation,
    evidence: &MailMessageMeta,
) -> Option<&'static str> {
    if action_string(annotation, "required_action_source").as_deref()
        != Some(ChannelRequiredActionSource::InformationBrief.as_str())
    {
        return None;
    }
    derive_channel_required_action(
        evidence.intent.as_deref(),
        evidence.needs_reply_hint,
        evidence.follow_up_hint.as_ref(),
        evidence.distill_brief.as_ref(),
    )
    .is_none()
    .then_some("required_action_no_longer_derived")
}

fn is_active_state(state: MailAnnotationState) -> bool {
    matches!(
        state,
        MailAnnotationState::NeedsApproval
            | MailAnnotationState::Approved
            | MailAnnotationState::Scheduled
            | MailAnnotationState::DraftRequested
            | MailAnnotationState::DraftReady
            | MailAnnotationState::Inserted
            | MailAnnotationState::SentDetected
    )
}

fn action_string(annotation: &MailThreadAnnotation, key: &str) -> Option<String> {
    annotation
        .proposed_action
        .as_ref()
        .and_then(|value| value.as_object())
        .and_then(|object| object.get(key))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn inbound_message_resolves_waiting_on(message: &MailMessageMeta) -> bool {
    if message.needs_reply_hint {
        return false;
    }
    if message.follow_up_hint.is_some() {
        return false;
    }
    matches!(
        message.intent.as_deref(),
        Some("fyi" | "transactional" | "social" | "newsletter")
    )
}

fn duration_millis_i64(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

pub async fn run_reconcile_pass(
    store: &MailAssistStore,
    principal: &str,
    workspace: &str,
    config: &ChannelReconcileConfig,
) -> Result<ReconcilePassOutcome> {
    run_reconcile_pass_with_completion_sink(store, principal, workspace, config, None).await
}

pub async fn run_reconcile_pass_with_completion_sink(
    store: &MailAssistStore,
    principal: &str,
    workspace: &str,
    config: &ChannelReconcileConfig,
    completions: Option<&dyn ReconcileCompletionSink>,
) -> Result<ReconcilePassOutcome> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let stale_after_ms = duration_millis_i64(config.stale_after);
    let stale_before = (stale_after_ms > 0).then(|| now_ms.saturating_sub(stale_after_ms));
    let annotations = store
        .list_active_annotations_for_reconcile(principal, workspace, stale_before, config.batch)
        .await?;
    let mut outcome = ReconcilePassOutcome {
        scanned: annotations.len(),
        ..ReconcilePassOutcome::default()
    };

    for annotation in annotations {
        let newer_messages = match store
            .list_reconcile_messages_after(
                principal,
                workspace,
                &annotation,
                config.newer_message_limit,
            )
            .await
        {
            Ok(messages) => messages,
            Err(error) => {
                outcome.errors += 1;
                warn!(
                    target: LOG_TARGET,
                    annotation_id = annotation.id.as_str(),
                    error = %error,
                    "failed to load reconciliation messages"
                );
                continue;
            },
        };
        let provider_changes = match store
            .list_provider_changes_after(
                principal,
                workspace,
                &annotation,
                config.newer_message_limit,
            )
            .await
        {
            Ok(changes) => changes,
            Err(error) => {
                outcome.errors += 1;
                warn!(
                    target: LOG_TARGET,
                    annotation_id = annotation.id.as_str(),
                    error = %error,
                    "failed to load reconciliation provider changes"
                );
                continue;
            },
        };
        let mut decision = reconcile_annotation_with_provider_changes(
            &annotation,
            &newer_messages,
            &provider_changes,
            now_ms,
            config.stale_after,
        );
        // Evidence-based reconciliation runs first; only a thread it would keep
        // active is re-checked against the current derivation rule, so a routing
        // change can never override a real resolution.
        if matches!(decision, ReconcileDecision::KeepActive { .. }) {
            match store
                .load_annotation_evidence_message(principal, workspace, &annotation)
                .await
            {
                Ok(Some(evidence)) => {
                    if let Some(reason) = routing_retraction_reason(&annotation, &evidence) {
                        decision = ReconcileDecision::RoutingRetracted { reason };
                    }
                },
                Ok(None) => {},
                Err(error) => {
                    outcome.errors += 1;
                    warn!(
                        target: LOG_TARGET,
                        annotation_id = annotation.id.as_str(),
                        error = %error,
                        "failed to load annotation evidence for routing repair"
                    );
                },
            }
        }
        if let ReconcileDecision::Supersede {
            resolution_message_id,
            ..
        } = &decision
        {
            match store
                .annotation_exists_for_evidence(
                    principal,
                    workspace,
                    &annotation.provider,
                    &annotation.account_alias,
                    &annotation.thread_id,
                    resolution_message_id,
                )
                .await
            {
                Ok(true) => {},
                Ok(false) => {
                    outcome.kept_active += 1;
                    debug!(
                        target: LOG_TARGET,
                        annotation_id = annotation.id.as_str(),
                        resolution_message_id = resolution_message_id.as_str(),
                        "reconciliation kept old annotation active until newer evidence is classified"
                    );
                    continue;
                },
                Err(error) => {
                    outcome.errors += 1;
                    warn!(
                        target: LOG_TARGET,
                        annotation_id = annotation.id.as_str(),
                        resolution_message_id = resolution_message_id.as_str(),
                        error = %error,
                        "failed to check classified newer evidence before supersession"
                    );
                    continue;
                },
            }
        }
        let Some(target_state) = decision.target_state(annotation.state) else {
            outcome.kept_active += 1;
            continue;
        };
        let detail = reconciliation_detail(&annotation, &decision);
        match store
            .transition_annotation_if_state(
                principal,
                workspace,
                &annotation.id,
                annotation.state,
                target_state,
                MailAssistActor::Worker,
                Some(detail),
                None,
                now_ms,
            )
            .await
        {
            Ok(AnnotationTransitionResult::Applied(_)) => match target_state {
                MailAnnotationState::Completed | MailAnnotationState::SentDetected => {
                    outcome.completed += 1;
                    // Only the owner-acted resolution is a learning signal. A
                    // thread closed because the counterparty replied, the
                    // provider deleted it, or it was superseded proves nothing
                    // about whether this item required the owner to act, and
                    // labelling those as completion would teach the opposite of
                    // what happened.
                    // Compared, not pattern-matched: a const in pattern position
                    // binds a fresh variable instead of comparing, which would
                    // make this arm accept every completion.
                    if matches!(decision, ReconcileDecision::Complete { .. })
                        && decision.reason() == OWNER_COMPLETION_REASON
                    {
                        if let Some(sink) = completions {
                            sink.record_owner_completion(
                                principal,
                                workspace,
                                &annotation.id,
                                now_ms,
                            )
                            .await;
                        }
                    }
                },
                MailAnnotationState::Superseded => outcome.superseded += 1,
                // Both land in Stale, but a retraction is a rule change rather
                // than an expiry; counting them together would hide the size of
                // a repair sweep inside the ordinary stale rate.
                MailAnnotationState::Stale
                    if matches!(decision, ReconcileDecision::RoutingRetracted { .. }) =>
                {
                    outcome.routing_retracted += 1
                },
                MailAnnotationState::Stale => outcome.stale += 1,
                _ => outcome.kept_active += 1,
            },
            Ok(AnnotationTransitionResult::ActionInProgress { action, .. }) => {
                outcome.blocked_by_claim += 1;
                debug!(
                    target: LOG_TARGET,
                    annotation_id = annotation.id.as_str(),
                    action = action.as_str(),
                    "reconciliation skipped active action claim"
                );
            },
            Ok(AnnotationTransitionResult::UnexpectedState { current, .. }) => {
                outcome.unexpected_state += 1;
                debug!(
                    target: LOG_TARGET,
                    annotation_id = annotation.id.as_str(),
                    state = current.state.as_db_str(),
                    "reconciliation skipped changed annotation state"
                );
            },
            Ok(AnnotationTransitionResult::NotFound) => {
                outcome.unexpected_state += 1;
            },
            Err(error) => {
                outcome.errors += 1;
                warn!(
                    target: LOG_TARGET,
                    annotation_id = annotation.id.as_str(),
                    error = %error,
                    "reconciliation transition failed"
                );
            },
        }
    }

    // Repair sweep. The scan above only reaches threads with newer evidence, a
    // provider change, or 30 days of age, so a brief-routed annotation on a
    // quiet thread would never be revisited — and a routing-rule change needs
    // to reach precisely those. Each repaired row leaves the active set, so
    // repeated passes drain the backlog a batch at a time.
    let brief_routed = match store
        .list_active_brief_routed_annotations(principal, workspace, config.batch)
        .await
    {
        Ok(annotations) => annotations,
        Err(error) => {
            outcome.errors += 1;
            warn!(
                target: LOG_TARGET,
                error = %error,
                "failed to list brief-routed annotations for routing repair"
            );
            Vec::new()
        },
    };
    for annotation in brief_routed {
        let evidence = match store
            .load_annotation_evidence_message(principal, workspace, &annotation)
            .await
        {
            Ok(Some(evidence)) => evidence,
            Ok(None) => continue,
            Err(error) => {
                outcome.errors += 1;
                warn!(
                    target: LOG_TARGET,
                    annotation_id = annotation.id.as_str(),
                    error = %error,
                    "failed to load evidence for routing repair"
                );
                continue;
            },
        };
        let Some(reason) = routing_retraction_reason(&annotation, &evidence) else {
            continue;
        };
        let decision = ReconcileDecision::RoutingRetracted { reason };
        let detail = reconciliation_detail(&annotation, &decision);
        match store
            .transition_annotation_if_state(
                principal,
                workspace,
                &annotation.id,
                annotation.state,
                MailAnnotationState::Stale,
                MailAssistActor::Worker,
                Some(detail),
                None,
                now_ms,
            )
            .await
        {
            Ok(AnnotationTransitionResult::Applied(_)) => outcome.routing_retracted += 1,
            Ok(AnnotationTransitionResult::ActionInProgress { .. }) => {
                outcome.blocked_by_claim += 1
            },
            Ok(
                AnnotationTransitionResult::UnexpectedState { .. }
                | AnnotationTransitionResult::NotFound,
            ) => outcome.unexpected_state += 1,
            Err(error) => {
                outcome.errors += 1;
                warn!(
                    target: LOG_TARGET,
                    annotation_id = annotation.id.as_str(),
                    error = %error,
                    "routing repair transition failed"
                );
            },
        }
    }

    Ok(outcome)
}

fn reconciliation_detail(
    annotation: &MailThreadAnnotation,
    decision: &ReconcileDecision,
) -> serde_json::Value {
    let mut detail = json!({
        "action": "reconcile",
        "rule_version": 1,
        "verdict": decision.reason(),
        "prior_state": annotation.state.as_db_str(),
        "prior_evidence_message_id": annotation.evidence_message_id.clone(),
        "prior_evidence_message_at": annotation.evidence_message_at,
    });
    match decision {
        ReconcileDecision::Complete {
            resolution_message_id,
            resolution_message_at,
            ..
        }
        | ReconcileDecision::Supersede {
            resolution_message_id,
            resolution_message_at,
            ..
        } => {
            if let Some(object) = detail.as_object_mut() {
                object.insert(
                    "resolution_message_id".to_string(),
                    json!(resolution_message_id),
                );
                object.insert(
                    "resolution_message_at".to_string(),
                    json!(resolution_message_at),
                );
            }
        },
        ReconcileDecision::CompleteProviderChange {
            change_id,
            change_at,
            ..
        } => {
            if let Some(object) = detail.as_object_mut() {
                object.insert("provider_change_id".to_string(), json!(change_id));
                object.insert("provider_change_at".to_string(), json!(change_at));
            }
        },
        ReconcileDecision::KeepActive { .. }
        | ReconcileDecision::Stale { .. }
        | ReconcileDecision::RoutingRetracted { .. } => {},
    }
    detail
}

/// The completion port reconciliation reports through — plan workstream 3.1
/// prerequisite (a). Extracted verbatim to
/// [`super::completion_port::ReconcileCompletionSink`] so the port is a
/// named seam artifact instead of a trait buried in the worker file. This
/// module imports it privately for its own signatures; implementers (the
/// `magician-api` sink, the `magician-bin` wiring) name the port at
/// [`super::completion_port`] directly (the Phase 5 batch-4 shim removal
/// repointed them off the old `reconcile::ReconcileCompletionSink` path).
use super::completion_port::ReconcileCompletionSink;

#[derive(Debug)]
pub struct ChannelReconcileWorker {
    handle: JoinHandle<()>,
    cancel: CancellationToken,
}

impl ChannelReconcileWorker {
    pub fn spawn(store: MailAssistStore, config: ChannelReconcileConfig) -> Self {
        Self::spawn_with_completion_sink(store, config, None)
    }

    pub fn spawn_with_completion_sink(
        store: MailAssistStore,
        config: ChannelReconcileConfig,
        completions: Option<Arc<dyn ReconcileCompletionSink>>,
    ) -> Self {
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let handle = tokio::spawn(async move {
            run_periodic(store, config, completions, cancel_for_task).await;
        });
        Self { handle, cancel }
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = self.handle.await;
    }
}

async fn run_periodic(
    store: MailAssistStore,
    config: ChannelReconcileConfig,
    completions: Option<Arc<dyn ReconcileCompletionSink>>,
    cancel: CancellationToken,
) {
    if !magician::magician_v2::runtime::startup::wait_for_http_or_cancel(&cancel).await {
        return;
    }

    if !config.startup_delay.is_zero() {
        tokio::select! {
            _ = tokio::time::sleep(config.startup_delay) => {},
            _ = cancel.cancelled() => return,
        }
    }

    loop {
        record_pass_started(chrono::Utc::now().timestamp_millis());
        match run_reconcile_pass_with_completion_sink(
            &store,
            DEFAULT_SCOPE_PRINCIPAL,
            DEFAULT_SCOPE_WORKSPACE,
            &config,
            completions.as_deref(),
        )
        .await
        {
            Ok(outcome) => {
                record_pass_success(chrono::Utc::now().timestamp_millis(), &outcome);
                if outcome.scanned > 0 {
                    debug!(
                        target: LOG_TARGET,
                        scanned = outcome.scanned,
                        completed = outcome.completed,
                        superseded = outcome.superseded,
                        stale = outcome.stale,
                        kept_active = outcome.kept_active,
                        blocked_by_claim = outcome.blocked_by_claim,
                        unexpected_state = outcome.unexpected_state,
                        errors = outcome.errors,
                        "reconciliation pass complete"
                    );
                }
            },
            Err(error) => {
                record_pass_failure(chrono::Utc::now().timestamp_millis(), &error);
                warn!(target: LOG_TARGET, error = %error, "reconciliation pass failed");
            },
        }

        tokio::select! {
            _ = tokio::time::sleep(config.interval) => {},
            _ = cancel.cancelled() => return,
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::super::types::{ChannelFollowUpHint, MailRecordOrigin, MAIL_ASSIST_SCHEMA_VERSION};
    use super::*;

    fn annotation(
        label: &str,
        follow_up_kind: &str,
        owner: &str,
        evidence_at: i64,
    ) -> MailThreadAnnotation {
        MailThreadAnnotation {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            id: "a1".to_string(),
            provider: "gmail".to_string(),
            account_alias: "business".to_string(),
            thread_id: "t1".to_string(),
            lane: Default::default(),
            state: MailAnnotationState::NeedsApproval,
            label: Some(label.to_string()),
            confidence: Some(0.9),
            reason: Some("Needs attention".to_string()),
            evidence_refs: vec!["thread:t1".to_string(), "message:m1".to_string()],
            evidence_message_id: Some("m1".to_string()),
            evidence_message_at: Some(evidence_at),
            classification_input_revision: Some(1),
            semantic_features: None,
            proposed_action: Some(json!({
                "follow_up_kind": follow_up_kind,
                "action_owner": owner,
            })),
            provenance: Some("test".to_string()),
            created_at: evidence_at,
            updated_at: evidence_at,
        }
    }

    fn brief_routed_annotation(source: &str) -> MailThreadAnnotation {
        let mut annotation = annotation("follow_up", "check_back", "unknown", 1_000);
        annotation.proposed_action = Some(json!({
            "required_action": "follow_up",
            "required_action_source": source,
        }));
        annotation
    }

    fn evidence_with_brief(
        information_type: super::super::types::ChannelInformationType,
        stated_action: Option<&str>,
    ) -> MailMessageMeta {
        let mut evidence = message("m1", 1_000, MessageDirection::Inbound);
        evidence.intent = Some("fyi".to_string());
        evidence.needs_reply_hint = false;
        evidence.follow_up_hint = None;
        evidence.distill_brief = Some(super::super::types::ChannelInformationBrief {
            schema_version: 1,
            information_type,
            summary: "Safe summary".to_string(),
            key_facts: Vec::new(),
            changes: Vec::new(),
            temporal_facts: Vec::new(),
            stated_action: stated_action.map(str::to_string),
            detail_status: Default::default(),
            missing_details: Vec::new(),
        });
        evidence
    }

    /// The repair exists because a routing rule changed under rows that were
    /// already written, so it must retract exactly what the rule no longer
    /// derives — and nothing else.
    #[test]
    fn routing_repair_retracts_only_what_the_rule_no_longer_derives() {
        let event = evidence_with_brief(
            super::super::types::ChannelInformationType::Event,
            Some("Take the online quiz"),
        );
        assert_eq!(
            routing_retraction_reason(&brief_routed_annotation("information_brief"), &event),
            Some("required_action_no_longer_derived")
        );

        // Still derived — a deadline is owner-addressed, so it stays.
        let deadline =
            evidence_with_brief(super::super::types::ChannelInformationType::Deadline, None);
        assert_eq!(
            routing_retraction_reason(&brief_routed_annotation("information_brief"), &deadline),
            None
        );
    }

    /// Explicit evidence is stronger than the brief and is not what changed, so
    /// a hint-routed annotation is never touched by the repair.
    #[test]
    fn routing_repair_leaves_hint_routed_annotations_alone() {
        let event = evidence_with_brief(
            super::super::types::ChannelInformationType::Event,
            Some("Take the online quiz"),
        );
        for source in ["follow_up_hint", "needs_reply_hint", "intent"] {
            assert_eq!(
                routing_retraction_reason(&brief_routed_annotation(source), &event),
                None,
                "{source} routed annotations must not be retracted"
            );
        }
    }

    /// A retraction is a rule change, not an expiry or a resolution.
    #[test]
    fn routing_retraction_retires_to_stale_and_keeps_its_reason() {
        let decision = ReconcileDecision::RoutingRetracted {
            reason: "required_action_no_longer_derived",
        };
        assert_eq!(
            decision.target_state(MailAnnotationState::NeedsApproval),
            Some(MailAnnotationState::Stale)
        );
        assert_eq!(decision.reason(), "required_action_no_longer_derived");
    }

    fn message(id: &str, at: i64, direction: MessageDirection) -> MailMessageMeta {
        MailMessageMeta {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: "gmail".to_string(),
            account_alias: "business".to_string(),
            account_email: None,
            thread_id: "t1".to_string(),
            message_id: id.to_string(),
            provider_cursor: None,
            label_ids: Vec::new(),
            subject: Some("Follow-up".to_string()),
            from_name: None,
            from_address: None,
            to_domains: Vec::new(),
            cc_domains: Vec::new(),
            internal_date: at,
            observed_at: at,
            direction: Some(direction),
            summary: Some("A newer message arrived.".to_string()),
            intent: Some("reply".to_string()),
            needs_reply_hint: false,
            follow_up_hint: None,
            distill_brief: None,
            distill_contract_version: None,
            distilled_at: None,
            distill_revision: None,
            distill_state: Default::default(),
            distill_attempts: 0,
            sensitive_suppressed: false,
            origin: MailRecordOrigin::MetadataSync,
        }
    }

    #[test]
    fn outbound_after_needs_reply_completes() {
        let a = annotation("needs_reply", "needs_reply", "owner", 1000);
        let decision = reconcile_annotation(
            &a,
            &[message("m2", 2000, MessageDirection::Outbound)],
            2000,
            Duration::from_secs(60),
        );
        assert!(matches!(
            decision,
            ReconcileDecision::Complete {
                reason: "owner_or_agent_sent_after_evidence",
                ..
            }
        ));
    }

    #[test]
    fn only_an_owner_send_is_labelled_as_the_owner_completing_the_work() {
        // ActionCompleted is the single positive the actionability model can
        // learn from, so this predicate decides what that model believes an
        // owner action looks like. A resolution the owner did not perform must
        // never reach it: a counterparty reply or a superseding message closes
        // the card while proving nothing about whether the owner owed work.
        let owner_acted = reconcile_annotation(
            &annotation("needs_reply", "needs_reply", "owner", 1000),
            &[message("m2", 2000, MessageDirection::Outbound)],
            2000,
            Duration::from_secs(60),
        );
        assert_eq!(owner_acted.reason(), OWNER_COMPLETION_REASON);

        let mut inbound = message("m2", 2000, MessageDirection::Inbound);
        inbound.intent = Some("fyi".to_string());
        let counterparty_acted = reconcile_annotation(
            &annotation("follow_up", "waiting_on", "counterparty", 1000),
            &[inbound],
            2000,
            Duration::from_secs(60),
        );
        assert_ne!(counterparty_acted.reason(), OWNER_COMPLETION_REASON);

        let expired = reconcile_annotation(
            &annotation("needs_reply", "needs_reply", "owner", 1000),
            &[],
            5_000_000,
            Duration::from_secs(3),
        );
        assert_ne!(
            expired.reason(),
            OWNER_COMPLETION_REASON,
            "an item that simply aged out was never acted on"
        );
    }

    #[test]
    fn counterparty_update_completes_waiting_on() {
        let a = annotation("follow_up", "waiting_on", "counterparty", 1000);
        let mut newer = message("m2", 2000, MessageDirection::Inbound);
        newer.intent = Some("fyi".to_string());
        let decision = reconcile_annotation(&a, &[newer], 2000, Duration::from_secs(60));
        assert!(matches!(
            decision,
            ReconcileDecision::Complete {
                reason: "counterparty_replied_to_waiting_on",
                ..
            }
        ));
    }

    #[test]
    fn ambiguous_counterparty_update_supersedes_waiting_on() {
        let a = annotation("follow_up", "waiting_on", "counterparty", 1000);
        let mut newer = message("m2", 2000, MessageDirection::Inbound);
        newer.intent = Some("other".to_string());
        let decision = reconcile_annotation(&a, &[newer], 2000, Duration::from_secs(60));
        assert!(matches!(
            decision,
            ReconcileDecision::Supersede {
                reason: "counterparty_message_changes_followup",
                ..
            }
        ));
    }

    #[test]
    fn ambiguous_newer_message_supersedes() {
        let a = annotation("follow_up", "waiting_on", "counterparty", 1000);
        let mut newer = message("m2", 2000, MessageDirection::Inbound);
        newer.needs_reply_hint = true;
        newer.follow_up_hint = Some(ChannelFollowUpHint {
            kind: "needs_reply".to_string(),
            actor: Some("owner".to_string()),
            ..ChannelFollowUpHint::default()
        });
        let decision = reconcile_annotation(&a, &[newer], 2000, Duration::from_secs(60));
        assert!(matches!(
            decision,
            ReconcileDecision::Supersede {
                reason: "counterparty_message_changes_followup",
                ..
            }
        ));
    }

    #[test]
    fn old_item_without_newer_evidence_becomes_stale() {
        let a = annotation("follow_up", "owner_owes", "owner", 1000);
        let decision = reconcile_annotation(&a, &[], 5000, Duration::from_secs(3));
        assert_eq!(
            decision,
            ReconcileDecision::Stale {
                reason: "stale_window_elapsed"
            }
        );
    }

    #[test]
    fn incoming_message_marks_pending_draft_stale_for_review() {
        let mut a = annotation("needs_reply", "needs_reply", "owner", 1000);
        a.state = MailAnnotationState::DraftReady;
        let decision = reconcile_annotation(
            &a,
            &[message("m2", 2000, MessageDirection::Inbound)],
            2000,
            Duration::from_secs(60),
        );
        assert_eq!(
            decision,
            ReconcileDecision::Stale {
                reason: "newer_message_requires_draft_review"
            }
        );
    }

    #[test]
    fn outbound_message_still_completes_pending_draft() {
        let mut a = annotation("needs_reply", "needs_reply", "owner", 1000);
        a.state = MailAnnotationState::DraftRequested;
        let decision = reconcile_annotation(
            &a,
            &[message("m2", 2000, MessageDirection::Outbound)],
            2000,
            Duration::from_secs(60),
        );
        assert!(matches!(
            decision,
            ReconcileDecision::Complete {
                reason: "owner_or_agent_sent_after_evidence",
                ..
            }
        ));
    }

    #[test]
    fn archive_history_change_completes_active_work_without_new_message() {
        let a = annotation("needs_reply", "needs_reply", "owner", 1000);
        let change = ProviderThreadChange {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            id: "provider-change:gmail:business:2:labels_removed:t1:m1".to_string(),
            provider: "gmail".to_string(),
            account_alias: "business".to_string(),
            thread_id: "t1".to_string(),
            message_id: Some("m1".to_string()),
            kind: ProviderThreadChangeKind::LabelsRemoved,
            thread_removed: false,
            label_ids: vec!["INBOX".to_string()],
            current_label_ids: Vec::new(),
            provider_cursor: Some("2".to_string()),
            observed_at: 2000,
        };
        let decision = reconcile_annotation_with_provider_changes(
            &a,
            &[],
            &[change],
            2000,
            Duration::from_secs(60),
        );
        assert!(matches!(
            decision,
            ReconcileDecision::CompleteProviderChange {
                reason: "provider_thread_archived",
                ..
            }
        ));
    }

    #[test]
    fn unrelated_label_change_does_not_close_active_work() {
        let a = annotation("needs_reply", "needs_reply", "owner", 1000);
        let change = ProviderThreadChange {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            id: "provider-change:gmail:business:2:labels_added:t1:m1".to_string(),
            provider: "gmail".to_string(),
            account_alias: "business".to_string(),
            thread_id: "t1".to_string(),
            message_id: Some("m1".to_string()),
            kind: ProviderThreadChangeKind::LabelsAdded,
            thread_removed: false,
            label_ids: vec!["STARRED".to_string()],
            current_label_ids: vec!["INBOX".to_string(), "STARRED".to_string()],
            provider_cursor: Some("2".to_string()),
            observed_at: 2000,
        };
        let decision = reconcile_annotation_with_provider_changes(
            &a,
            &[],
            &[change],
            2000,
            Duration::from_secs(60),
        );
        assert_eq!(
            decision,
            ReconcileDecision::KeepActive {
                reason: "no_newer_reconciling_evidence"
            }
        );
    }

    #[test]
    fn transient_archive_delta_does_not_close_a_thread_that_is_back_in_inbox() {
        let a = annotation("needs_reply", "needs_reply", "owner", 1000);
        let change = ProviderThreadChange {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            id: "provider-change:gmail:business:2:labels_removed:t1:m1".to_string(),
            provider: "gmail".to_string(),
            account_alias: "business".to_string(),
            thread_id: "t1".to_string(),
            message_id: Some("m1".to_string()),
            kind: ProviderThreadChangeKind::LabelsRemoved,
            thread_removed: false,
            label_ids: vec!["INBOX".to_string()],
            current_label_ids: vec!["INBOX".to_string()],
            provider_cursor: Some("2".to_string()),
            observed_at: 2000,
        };
        assert_eq!(
            reconcile_annotation_with_provider_changes(
                &a,
                &[],
                &[change],
                2000,
                Duration::from_secs(60),
            ),
            ReconcileDecision::KeepActive {
                reason: "no_newer_reconciling_evidence"
            }
        );
    }

    #[test]
    fn current_trash_label_completes_active_work() {
        let a = annotation("needs_reply", "needs_reply", "owner", 1000);
        let change = ProviderThreadChange {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            id: "provider-change:gmail:business:2:labels_added:t1:m1".to_string(),
            provider: "gmail".to_string(),
            account_alias: "business".to_string(),
            thread_id: "t1".to_string(),
            message_id: Some("m1".to_string()),
            kind: ProviderThreadChangeKind::LabelsAdded,
            thread_removed: false,
            label_ids: vec!["TRASH".to_string()],
            current_label_ids: vec!["TRASH".to_string()],
            provider_cursor: Some("2".to_string()),
            observed_at: 2000,
        };
        assert!(matches!(
            reconcile_annotation_with_provider_changes(
                &a,
                &[],
                &[change],
                2000,
                Duration::from_secs(60),
            ),
            ReconcileDecision::CompleteProviderChange {
                reason: "provider_thread_trashed_or_spam",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn reconcile_pass_applies_a_persisted_provider_change_once() {
        let temporary = tempfile::TempDir::new().unwrap();
        let store = MailAssistStore::open(temporary.path()).unwrap();
        let annotation = annotation("needs_reply", "needs_reply", "owner", 1_000);
        store
            .create_annotation(
                "principal",
                "workspace",
                annotation,
                MailAssistActor::Worker,
            )
            .await
            .unwrap();
        store
            .append_provider_changes(
                "principal",
                "workspace",
                vec![ProviderThreadChange {
                    schema_version: MAIL_ASSIST_SCHEMA_VERSION,
                    id: "provider-change:gmail:business:2:labels_removed:t1:m1".to_string(),
                    provider: "gmail".to_string(),
                    account_alias: "business".to_string(),
                    thread_id: "t1".to_string(),
                    message_id: Some("m1".to_string()),
                    kind: ProviderThreadChangeKind::LabelsRemoved,
                    thread_removed: false,
                    label_ids: vec!["INBOX".to_string()],
                    current_label_ids: Vec::new(),
                    provider_cursor: Some("2".to_string()),
                    observed_at: 2_000,
                }],
            )
            .await
            .unwrap();
        let config = ChannelReconcileConfig {
            stale_after: Duration::ZERO,
            ..ChannelReconcileConfig::default()
        };

        let first = run_reconcile_pass(&store, "principal", "workspace", &config)
            .await
            .unwrap();
        assert_eq!(first.completed, 1);
        assert_eq!(
            store
                .get_annotation("principal", "workspace", "a1")
                .await
                .unwrap()
                .unwrap()
                .state,
            MailAnnotationState::Completed
        );

        let second = run_reconcile_pass(&store, "principal", "workspace", &config)
            .await
            .unwrap();
        assert_eq!(second.scanned, 0);
        assert_eq!(second.completed, 0);
    }

    #[tokio::test]
    async fn reconcile_pass_detects_manual_send_before_distillation_finishes() {
        let temporary = tempfile::TempDir::new().unwrap();
        let store = MailAssistStore::open(temporary.path()).unwrap();
        store
            .create_annotation(
                "principal",
                "workspace",
                annotation("needs_reply", "needs_reply", "owner", 1_000),
                MailAssistActor::Worker,
            )
            .await
            .unwrap();
        let mut outbound = message("m2", 2_000, MessageDirection::Outbound);
        outbound.summary = None;
        outbound.distill_state = super::super::types::DistillState::Pending;
        store
            .append_messages("principal", "workspace", vec![outbound])
            .await
            .unwrap();
        let config = ChannelReconcileConfig {
            stale_after: Duration::ZERO,
            ..ChannelReconcileConfig::default()
        };

        let outcome = run_reconcile_pass(&store, "principal", "workspace", &config)
            .await
            .unwrap();
        assert_eq!(outcome.completed, 1);
        assert_eq!(
            store
                .get_annotation("principal", "workspace", "a1")
                .await
                .unwrap()
                .unwrap()
                .state,
            MailAnnotationState::Completed
        );
    }

    #[tokio::test]
    async fn reconcile_pass_stales_a_draft_on_pending_inbound_metadata() {
        let temporary = tempfile::TempDir::new().unwrap();
        let store = MailAssistStore::open(temporary.path()).unwrap();
        let mut draft = annotation("needs_reply", "needs_reply", "owner", 1_000);
        draft.state = MailAnnotationState::DraftReady;
        store
            .create_annotation("principal", "workspace", draft, MailAssistActor::Worker)
            .await
            .unwrap();
        let mut inbound = message("m2", 2_000, MessageDirection::Inbound);
        inbound.summary = None;
        inbound.distill_state = super::super::types::DistillState::Pending;
        store
            .append_messages("principal", "workspace", vec![inbound])
            .await
            .unwrap();
        let config = ChannelReconcileConfig {
            stale_after: Duration::ZERO,
            ..ChannelReconcileConfig::default()
        };

        let outcome = run_reconcile_pass(&store, "principal", "workspace", &config)
            .await
            .unwrap();
        assert_eq!(outcome.stale, 1);
        assert_eq!(
            store
                .get_annotation("principal", "workspace", "a1")
                .await
                .unwrap()
                .unwrap()
                .state,
            MailAnnotationState::Stale
        );
    }
}

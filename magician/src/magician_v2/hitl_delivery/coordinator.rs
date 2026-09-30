//! The delivery coordinator (plan §6.1): one subscription to the canonical
//! `hitl.requested` / `hitl.resolved` lifecycle, one decision per request,
//! one record per destination written before any send, bounded retries that
//! recheck the request first, claims and reports from channel bots, and
//! retirement of everything outstanding when the request resolves.
use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::{
    alert::{AlertCard, AlertKind, Criticality},
    policy::{ChannelDestination, DeliveryPolicy},
    records::{
        latency_summary, ClaimBinding, DeliveryRecord, DeliveryState, DeliveryStore, Destination,
        LatencySummary,
    },
};
use crate::magician_v2::realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent};

/// Attempts per destination before `failed`.
pub const MAX_ATTEMPTS: u32 = 3;
/// Backoff before the second and third attempt.
pub const RETRY_BACKOFF: [Duration; 2] = [Duration::from_secs(5), Duration::from_secs(30)];
/// How long an offered alert waits for a bot to claim it before the
/// destination is `unavailable` (no bot connected).
pub const CLAIM_WINDOW: Duration = Duration::from_secs(10);
/// How long a claimed delivery waits for the bot's report before it is
/// `ambiguous` (the send may have happened; no provider idempotency).
pub const REPORT_WINDOW: Duration = Duration::from_secs(30);
/// Cadence at which a waiting delivery task re-reads its record.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Resolved correlations remembered so a late `hitl.requested` replay never
/// re-alerts.
const RESOLVED_MEMORY: usize = 4_096;
const MAX_STATUS_ROWS: usize = 200;

/// Wall clock, injectable for tests.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        Utc::now().timestamp_millis()
    }
}

/// Which request a delivery is for, as the lifecycle event named it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestIdentity {
    pub principal: String,
    pub workspace: String,
    pub correlation_id: String,
    /// `user_request`, `agentic`, `chat`, … or `test`.
    pub source: String,
    pub execution_id: Option<String>,
}

impl RequestIdentity {
    fn key(&self) -> String {
        scope_key(&self.principal, &self.workspace, &self.correlation_id)
    }
}

/// What the coordinator asks the request owners before every send.
#[async_trait]
pub trait RequestOracle: Send + Sync {
    /// Whether the request is still open (pending, same scope).
    async fn still_pending(&self, request: &RequestIdentity) -> bool;
    /// The origin channel of the chat session the request belongs to, when
    /// it belongs to one: `(channel_type, address)`.
    async fn origin_channel(
        &self,
        request: &RequestIdentity,
        input_schema: Option<&Value>,
    ) -> Option<(String, String)>;
}

/// Cards kept in memory for claims — never on disk, where records are
/// routing metadata only.
const MAX_LIVE_CARDS: usize = 4_096;

/// The push lane as the coordinator drives it.
#[async_trait]
pub trait PushSink: Send + Sync {
    /// Send the attention push to the scope's registered devices; returns
    /// how many registrations the wave addressed and how many accepted.
    async fn attention_requested(
        &self,
        principal: &str,
        workspace: &str,
        correlation_id: &str,
        timestamp: i64,
    ) -> PushWave;
    async fn attention_resolved(
        &self,
        principal: &str,
        workspace: &str,
        correlation_id: &str,
        timestamp: i64,
    );
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PushWave {
    pub registrations: usize,
    pub accepted: usize,
    /// Registrations whose platform has no provider configured on this
    /// runtime. Nothing was sent and nothing is retryable, so these are
    /// neither an acceptance nor a failure — a wave that is all of them is
    /// `unavailable`, once, instead of three attempts ending `failed`.
    #[allow(dead_code)]
    pub not_configured: usize,
}

/// Whether automatic verification-code retrieval (§6.2) would run for a
/// scope right now — the delivery coordinator's cue for a short grace
/// before channel alerts. Installed by the composition root.
#[async_trait]
pub trait RetrievalOracle: Send + Sync {
    async fn retrieval_expected(&self, principal: &str, workspace: &str) -> bool;

    /// The same, for one request: false once the resolver already knows
    /// the challenge and has decided it will not deliver (ambiguous,
    /// unavailable) — its status may have gone out before the grace was
    /// armed.
    async fn retrieval_expected_for(
        &self,
        principal: &str,
        workspace: &str,
        _correlation_id: &str,
    ) -> bool {
        self.retrieval_expected(principal, workspace).await
    }
}

/// How alerts reach the channel bots: the scoped realtime feed.
pub trait ChannelTransport: Send + Sync {
    fn offer(&self, event: RuntimeTransportEvent);
}

impl ChannelTransport for RuntimeTransportBroadcaster {
    fn offer(&self, event: RuntimeTransportEvent) {
        self.emit(event);
    }
}

/// What a claiming bot receives: the address it may send to and the card.
#[derive(Debug, Clone, Serialize)]
pub struct ClaimGrant {
    pub delivery_id: String,
    pub correlation_id: String,
    pub kind: String,
    pub channel_type: String,
    pub address: String,
    pub alert: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimError {
    UnknownDelivery,
    WrongScope,
    WrongChannel,
    /// Already claimed, or no longer queued (retired, expired, failed).
    NotClaimable(DeliveryState),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportError {
    UnknownDelivery,
    WrongScope,
    /// The reporting bot is not the one that claimed.
    NotTheClaimant,
    NotReportable(DeliveryState),
}

/// A bot's report of one send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    ProviderAccepted { provider_message_id: Option<String> },
    ConfirmedDelivered,
    Failed { reason: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    pub deliveries: Vec<Value>,
    pub latency: LatencySummary,
    /// channel type → last claim time, so the settings surface can say when a
    /// bot last showed up.
    pub channels_last_claimed_ms: HashMap<String, i64>,
}

struct Timings {
    claim_window: Duration,
    report_window: Duration,
    backoff: Vec<Duration>,
}

pub struct DeliveryCoordinator {
    policy: RwLock<DeliveryPolicy>,
    store: Arc<DeliveryStore>,
    push: Option<Arc<dyn PushSink>>,
    transport: Arc<dyn ChannelTransport>,
    oracle: Arc<dyn RequestOracle>,
    clock: Arc<dyn Clock>,
    timings: Timings,
    /// Live delivery tasks per `principal|workspace|correlation`, cancelled
    /// on resolution.
    active: Mutex<HashMap<String, CancellationToken>>,
    /// Retrieval graces in progress, per request key: cut short the moment
    /// automatic retrieval says it will not deliver.
    graces: Mutex<HashMap<String, CancellationToken>>,
    /// Recently resolved correlations, newest last.
    resolved: Mutex<Vec<String>>,
    /// channel type → last claim time.
    last_claim: Mutex<HashMap<String, i64>>,
    /// delivery id → the card a claim hands over.
    /// delivery id → (when it was minted, the value-free card). The time is
    /// what makes eviction honest: a full map drops the OLDEST cards, never
    /// everyone's.
    cards: Mutex<HashMap<String, (i64, Value)>>,
    /// Automatic retrieval, when the runtime has it.
    retrieval: RwLock<Option<Arc<dyn RetrievalOracle>>>,
}

fn scope_key(principal: &str, workspace: &str, correlation_id: &str) -> String {
    format!("{principal}|{workspace}|{correlation_id}")
}

impl DeliveryCoordinator {
    pub fn new(
        policy: DeliveryPolicy,
        store: Arc<DeliveryStore>,
        push: Option<Arc<dyn PushSink>>,
        transport: Arc<dyn ChannelTransport>,
        oracle: Arc<dyn RequestOracle>,
    ) -> Self {
        Self::with_clock(
            policy,
            store,
            push,
            transport,
            oracle,
            Arc::new(SystemClock),
        )
    }

    pub fn with_clock(
        policy: DeliveryPolicy,
        store: Arc<DeliveryStore>,
        push: Option<Arc<dyn PushSink>>,
        transport: Arc<dyn ChannelTransport>,
        oracle: Arc<dyn RequestOracle>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            policy: RwLock::new(policy),
            store,
            push,
            transport,
            oracle,
            clock,
            timings: Timings {
                claim_window: CLAIM_WINDOW,
                report_window: REPORT_WINDOW,
                backoff: RETRY_BACKOFF.to_vec(),
            },
            active: Mutex::new(HashMap::new()),
            graces: Mutex::new(HashMap::new()),
            resolved: Mutex::new(Vec::new()),
            last_claim: Mutex::new(HashMap::new()),
            cards: Mutex::new(HashMap::new()),
            retrieval: RwLock::new(None),
        }
    }

    /// Attach automatic verification-code retrieval, so a code ask it is
    /// watching gives it a short grace before channel alerts.
    pub async fn set_retrieval_oracle(&self, oracle: Arc<dyn RetrievalOracle>) {
        *self.retrieval.write().await = Some(oracle);
    }

    /// Shorter windows for tests.
    #[cfg(test)]
    pub(crate) fn with_timings(
        mut self,
        claim_window: Duration,
        report_window: Duration,
        backoff: Vec<Duration>,
    ) -> Self {
        self.timings = Timings {
            claim_window,
            report_window,
            backoff,
        };
        self
    }

    pub async fn policy(&self) -> DeliveryPolicy {
        self.policy.read().await.clone()
    }

    /// Settings changed (a config reload): the next request uses them.
    pub async fn reload(&self, policy: DeliveryPolicy) {
        *self.policy.write().await = policy;
    }

    pub fn store(&self) -> &Arc<DeliveryStore> {
        &self.store
    }

    /// Subscribe to the broadcaster for the lifetime of the process.
    pub fn start(self: Arc<Self>, broadcaster: Arc<RuntimeTransportBroadcaster>) {
        let mut events = broadcaster.subscribe();
        tokio::spawn(async move {
            loop {
                let event = match events.recv().await {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        warn!(count, "[HITL-DELIVERY] event subscriber lagged");
                        continue;
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                let coordinator = Arc::clone(&self);
                tokio::spawn(async move { coordinator.handle(event).await });
            }
        });
    }

    /// Re-drive the deliveries a previous run left live (secure HITL §5).
    ///
    /// A restart used to mark every non-terminal row `failed` and stop there,
    /// so an alert that was still owed when the runtime went down was never
    /// offered again: the row read `failed`, Attention still showed the ask, and
    /// the owner was never reached on any channel. Waiting for the announcement
    /// to come round again recovers nothing — the realtime feed replays nothing,
    /// and a republished `hitl.requested` for a key already on the pending shard
    /// is admitted without a re-broadcast. The coordinator's OWN rows are the
    /// record, so recovery reads them.
    ///
    /// Per correlation still live: ask whether the request is open; if it is,
    /// retire the stale rows and offer a fresh set with the generic card (the
    /// prompt itself is never persisted, by design, so the card cannot name the
    /// service — the claim path already falls back to the same card). If the
    /// request is closed, or the row predates the fields that name the ask's
    /// lane, retire it as before.
    pub async fn recover_after_restart(self: &Arc<Self>) {
        let live = self.store.live_rows().await;
        if live.is_empty() {
            return;
        }
        let now = self.clock.now_ms();
        let mut groups: Vec<(RequestIdentity, Option<i64>, Vec<String>)> = Vec::new();
        for row in live {
            if let Some((_, _, ids)) = groups.iter_mut().find(|(identity, _, _)| {
                identity.principal == row.principal
                    && identity.workspace == row.workspace
                    && identity.correlation_id == row.correlation_id
            }) {
                ids.push(row.id);
                continue;
            }
            groups.push((
                RequestIdentity {
                    principal: row.principal.clone(),
                    workspace: row.workspace.clone(),
                    correlation_id: row.correlation_id.clone(),
                    source: row.source.clone(),
                    execution_id: row.execution_id.clone(),
                },
                row.deadline_ms,
                vec![row.id],
            ));
        }
        for (request, deadline_ms, ids) in groups {
            // The announcement can come round again before recovery runs (the
            // request service republishes its restored pending rows, and this
            // coordinator subscribes just before recovery is spawned). A
            // correlation this process is already delivering is NOT recovered:
            // re-offering it would send the owner a second card for one ask.
            if self.active.lock().await.contains_key(&request.key()) {
                continue;
            }
            if request.source.is_empty() {
                self.store
                    .retire(
                        &ids,
                        "the runtime restarted and the row does not name the ask's lane",
                        now,
                    )
                    .await;
                continue;
            }
            if !self.request_live(&request, deadline_ms).await {
                self.store
                    .retire(&ids, "the request closed while the runtime was down", now)
                    .await;
                continue;
            }
            self.store
                .retire(&ids, "re-offered after the runtime restarted", now)
                .await;
            let policy = self.policy().await;
            let mut card = AlertCard::for_request(
                None,
                &request.correlation_id,
                policy.public_origin.as_deref(),
            );
            // The card cannot name the service (no prompt is persisted) but it
            // can still carry the ask's own expiry, which the bot renders.
            card.deadline_ms = deadline_ms;
            info!(
                correlation_id = %request.correlation_id,
                source = %request.source,
                "[HITL-DELIVERY] re-offering an alert the restart interrupted"
            );
            self.fan_out(
                request,
                card,
                deadline_ms,
                deadline_ms.is_some(),
                None,
                now,
                None,
            )
            .await;
        }
    }

    /// One lifecycle event.
    pub async fn handle(self: &Arc<Self>, event: RuntimeTransportEvent) {
        if crate::magician_v2::realtime_events::is_app_owner_notification_transport_event(&event) {
            // Workflow briefing/escalation notifications are explicitly
            // no-push (app.notify.v1) and are not credential asks.
            return;
        }
        match event {
            RuntimeTransportEvent::HitlRequested {
                correlation_id,
                source,
                input_schema,
                execution_id,
                principal: Some(principal),
                workspace: Some(workspace),
                timestamp,
                ..
            } => {
                let request = RequestIdentity {
                    principal,
                    workspace,
                    correlation_id,
                    source,
                    execution_id,
                };
                self.on_requested(request, input_schema, timestamp).await;
            },
            RuntimeTransportEvent::HitlResolved {
                correlation_id,
                outcome,
                principal: Some(principal),
                workspace: Some(workspace),
                timestamp,
                ..
            } => {
                self.on_resolved(&principal, &workspace, &correlation_id, &outcome, timestamp)
                    .await;
            },
            RuntimeTransportEvent::VerificationRetrievalStatus {
                correlation_id,
                status,
                principal: Some(principal),
                workspace: Some(workspace),
                ..
            } if matches!(status.as_str(), "ambiguous" | "unavailable") => {
                // Retrieval will not deliver: the alert goes out now, not at
                // the end of the grace.
                if let Some(grace) = self.graces.lock().await.remove(&scope_key(
                    &principal,
                    &workspace,
                    &correlation_id,
                )) {
                    grace.cancel();
                }
            },
            _ => {},
        }
    }

    async fn on_requested(
        self: &Arc<Self>,
        request: RequestIdentity,
        input_schema: Option<Value>,
        timestamp: i64,
    ) {
        let key = request.key();
        if self.resolved.lock().await.iter().any(|seen| *seen == key) {
            return;
        }
        // One authoritative delivery set per request: a second announcement
        // of a correlation whose deliveries are still running (a republish,
        // a duplicate emit) adds nothing.
        if self.active.lock().await.contains_key(&key) {
            return;
        }
        let policy = self.policy().await;
        let Some(criticality) = Criticality::of(input_schema.as_ref()) else {
            // Ordinary attention: today's push behaviour, nothing else.
            if policy.settings.push_enabled {
                if let Some(push) = &self.push {
                    let _ = push
                        .attention_requested(
                            &request.principal,
                            &request.workspace,
                            &request.correlation_id,
                            timestamp,
                        )
                        .await;
                }
            }
            return;
        };
        let card = AlertCard::for_request(
            input_schema.as_ref(),
            &request.correlation_id,
            policy.public_origin.as_deref(),
        );
        let origin = self
            .oracle
            .origin_channel(&request, input_schema.as_ref())
            .await;
        // Retrieval first, alert second — briefly (§6.2): a code ask that
        // automatic retrieval is watching gives the sources a short grace
        // before the channel alerts go out. Push is never delayed, and a
        // short deadline skips the grace.
        let channel_delay = self
            .retrieval_grace(
                &request,
                input_schema.as_ref(),
                criticality.deadline_ms,
                &policy,
            )
            .await;
        self.fan_out(
            request,
            card,
            criticality.deadline_ms,
            criticality.time_bound(),
            origin,
            timestamp,
            channel_delay,
        )
        .await;
    }

    /// The deadline below which an alert must not wait for retrieval.
    const SHORT_DEADLINE_MS: i64 = 90_000;

    async fn retrieval_grace(
        &self,
        request: &RequestIdentity,
        input_schema: Option<&Value>,
        deadline_ms: Option<i64>,
        policy: &DeliveryPolicy,
    ) -> Option<Duration> {
        if policy.retrieval_grace_secs == 0 {
            return None;
        }
        let is_code_ask = input_schema
            .and_then(|schema| schema.get("sensitive"))
            .and_then(|spec| spec.get("kind"))
            .and_then(Value::as_str)
            == Some("otp");
        if !is_code_ask {
            return None;
        }
        if deadline_ms
            .is_some_and(|deadline| deadline - self.clock.now_ms() < Self::SHORT_DEADLINE_MS)
        {
            return None;
        }
        let retrieval = self.retrieval.read().await.clone()?;
        retrieval
            .retrieval_expected_for(
                &request.principal,
                &request.workspace,
                &request.correlation_id,
            )
            .await
            .then(|| Duration::from_secs(policy.retrieval_grace_secs))
    }

    /// The owner's test from the settings surface: a real delivery through
    /// every enabled destination, never held by quiet hours, recorded as
    /// `test`.
    pub async fn send_test(
        self: &Arc<Self>,
        principal: &str,
        workspace: &str,
    ) -> Vec<DeliveryRecord> {
        let correlation_id = format!("test-{}", uuid::Uuid::new_v4().simple());
        let policy = self.policy().await;
        let card = AlertCard::test(policy.public_origin.as_deref(), &correlation_id);
        let now = self.clock.now_ms();
        let request = RequestIdentity {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            correlation_id: correlation_id.clone(),
            source: "test".to_string(),
            execution_id: None,
        };
        self.fan_out(request, card, None, true, None, now, None)
            .await;
        self.store
            .for_correlation(principal, workspace, &correlation_id)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn fan_out(
        self: &Arc<Self>,
        request: RequestIdentity,
        card: AlertCard,
        deadline_ms: Option<i64>,
        time_bound: bool,
        origin: Option<(String, String)>,
        requested_at_ms: i64,
        channel_delay: Option<Duration>,
    ) {
        let policy = self.policy().await;
        let now = self.clock.now_ms();
        let kind = card.kind.as_str().to_string();
        let (principal, workspace, correlation_id) = (
            request.principal.clone(),
            request.workspace.clone(),
            request.correlation_id.clone(),
        );
        let held_until = if card.kind == AlertKind::Test {
            None
        } else {
            let now_utc = Utc
                .timestamp_millis_opt(now)
                .single()
                .unwrap_or_else(Utc::now);
            if policy.quiet_hours_hold(now_utc, time_bound) {
                policy.quiet_hours_end_ms(now_utc)
            } else {
                None
            }
        };
        let initial_state = if held_until.is_some() {
            DeliveryState::Held
        } else {
            DeliveryState::Queued
        };
        let new_record = |destination: Destination| DeliveryRecord {
            id: uuid::Uuid::new_v4().simple().to_string(),
            principal: principal.clone(),
            workspace: workspace.clone(),
            correlation_id: correlation_id.clone(),
            revision: 1,
            kind: kind.clone(),
            destination,
            state: initial_state,
            attempts: 0,
            requested_at_ms,
            enqueued_at_ms: now,
            claimed_at_ms: None,
            accepted_at_ms: None,
            updated_at_ms: now,
            provider_message_id: None,
            reason: held_until.map(|_| "held by quiet hours".to_string()),
            claimed_by: None,
            registrations: None,
            // Carried so a restart can re-drive this row instead of retiring it.
            source: request.source.clone(),
            execution_id: request.execution_id.clone(),
            deadline_ms,
        };

        // Every record exists before any send.
        let mut push_record = None;
        if policy.settings.push_enabled && self.push.is_some() {
            let record = new_record(Destination::Push);
            self.store.insert(record.clone()).await;
            push_record = Some(record);
        }
        let mut channel_records = Vec::new();
        for destination in policy.channel_destinations() {
            let mut record = new_record(Destination::Channel {
                channel_type: destination.channel_type.clone(),
                address: destination.address.clone(),
            });
            // The origin relay already carries the notice to this exact
            // destination (P3): one deduplication decision, taken here.
            if origin.as_ref().is_some_and(|(channel_type, address)| {
                channel_type.eq_ignore_ascii_case(&destination.channel_type)
                    && address == &destination.address
            }) {
                record.state = DeliveryState::RelayedByOrigin;
                record.set_reason("the request's own chat channel carries the notice");
                self.store.insert(record).await;
                continue;
            }
            self.store.insert(record.clone()).await;
            channel_records.push((destination, record));
        }
        if push_record.is_none() && channel_records.is_empty() {
            return;
        }
        {
            let alert = card.to_value(now);
            let mut cards = self.cards.lock().await;
            // Clearing the whole map threw away every OTHER request's card, and
            // a claim that arrived afterwards handed the bot the generic card —
            // "a service", no reason, no deadline — for a real credential ask.
            // Drop the oldest instead, just enough to fit.
            let over = (cards.len() + channel_records.len()).saturating_sub(MAX_LIVE_CARDS);
            if over > 0 {
                let mut by_age: Vec<(i64, String)> = cards
                    .iter()
                    .map(|(id, (at, _))| (*at, id.clone()))
                    .collect();
                by_age.sort_unstable();
                for (_, id) in by_age.into_iter().take(over) {
                    cards.remove(&id);
                }
            }
            for (_, record) in &channel_records {
                cards.insert(record.id.clone(), (now, alert.clone()));
            }
        }

        let cancel = CancellationToken::new();
        let key = request.key();
        self.active.lock().await.insert(key.clone(), cancel.clone());

        let coordinator = Arc::clone(self);
        let alert = card.to_value(now);
        let staged = policy.staged();
        let staged_fallback = Duration::from_secs(policy.settings.staged_fallback_secs.max(1));
        let request = Arc::new(request);
        tokio::spawn(async move {
            if let Some(held_until) = held_until {
                let wait =
                    Duration::from_millis((held_until - coordinator.clock.now_ms()).max(0) as u64);
                // Both exits reclaim the `active` entry, like every other exit
                // of this task. Leaving it behind held the key — and its token
                // — for the process's life, and `on_requested`'s
                // `contains_key` guard then suppressed every later
                // announcement of that correlation.
                tokio::select! {
                    _ = cancel.cancelled() => {
                        coordinator.active.lock().await.remove(&key);
                        return;
                    },
                    _ = tokio::time::sleep(wait) => {},
                }
                if !coordinator.request_live(&request, deadline_ms).await {
                    coordinator
                        .expire_all(&request, "the request closed during quiet hours")
                        .await;
                    coordinator.active.lock().await.remove(&key);
                    return;
                }
                if let Some(record) = &push_record {
                    coordinator
                        .store
                        .update(&record.id, |r| {
                            r.state = DeliveryState::Queued;
                            r.reason = None;
                        })
                        .await;
                }
                for (_, record) in &channel_records {
                    coordinator
                        .store
                        .update(&record.id, |r| {
                            r.state = DeliveryState::Queued;
                            r.reason = None;
                        })
                        .await;
                }
            }
            let mut tasks = Vec::new();
            if let Some(record) = push_record {
                let c = Arc::clone(&coordinator);
                let request = Arc::clone(&request);
                let cancel = cancel.clone();
                tasks.push(tokio::spawn(async move {
                    c.run_push_delivery(record, &request, deadline_ms, requested_at_ms, cancel)
                        .await
                }));
            }
            if let Some(delay) = channel_delay.filter(|_| !channel_records.is_empty()) {
                let now = coordinator.clock.now_ms();
                for (_, record) in &channel_records {
                    coordinator
                        .store
                        .update(&record.id, |r| {
                            r.set_reason("waiting briefly for automatic code retrieval");
                            r.updated_at_ms = now;
                        })
                        .await;
                }
                let grace = CancellationToken::new();
                coordinator
                    .graces
                    .lock()
                    .await
                    .insert(key.clone(), grace.clone());
                tokio::select! {
                    _ = cancel.cancelled() => {
                        coordinator.graces.lock().await.remove(&key);
                        for task in tasks {
                            let _ = task.await;
                        }
                        coordinator.active.lock().await.remove(&key);
                        return;
                    },
                    _ = grace.cancelled() => {},
                    _ = tokio::time::sleep(delay) => {},
                }
                coordinator.graces.lock().await.remove(&key);
                if !coordinator.request_live(&request, deadline_ms).await {
                    coordinator
                        .expire_all(&request, "the request closed during the retrieval grace")
                        .await;
                    for task in tasks {
                        let _ = task.await;
                    }
                    coordinator.active.lock().await.remove(&key);
                    return;
                }
                let now = coordinator.clock.now_ms();
                for (_, record) in &channel_records {
                    coordinator
                        .store
                        .update(&record.id, |r| {
                            r.reason = None;
                            r.updated_at_ms = now;
                        })
                        .await;
                }
            }
            if staged {
                let c = Arc::clone(&coordinator);
                let request = Arc::clone(&request);
                let alert = alert.clone();
                let cancel = cancel.clone();
                tasks.push(tokio::spawn(async move {
                    c.run_staged(
                        channel_records,
                        &request,
                        alert,
                        deadline_ms,
                        staged_fallback,
                        cancel,
                    )
                    .await
                }));
            } else {
                for (destination, record) in channel_records {
                    let c = Arc::clone(&coordinator);
                    let request = Arc::clone(&request);
                    let alert = alert.clone();
                    let cancel = cancel.clone();
                    tasks.push(tokio::spawn(async move {
                        c.run_channel_delivery(
                            record,
                            destination,
                            &request,
                            alert,
                            deadline_ms,
                            cancel,
                        )
                        .await;
                    }));
                }
            }
            for task in tasks {
                let _ = task.await;
            }
            coordinator.active.lock().await.remove(&key);
        });
    }

    /// Recheck before a send: the request is still open, its deadline has
    /// not passed, and it has not resolved meanwhile.
    async fn request_live(&self, request: &RequestIdentity, deadline_ms: Option<i64>) -> bool {
        if request.source == "test" {
            return true;
        }
        if deadline_ms.is_some_and(|deadline| deadline <= self.clock.now_ms()) {
            return false;
        }
        if self
            .resolved
            .lock()
            .await
            .iter()
            .any(|seen| *seen == request.key())
        {
            return false;
        }
        self.oracle.still_pending(request).await
    }

    async fn expire_all(&self, request: &RequestIdentity, reason: &str) {
        for record in self
            .store
            .for_correlation(
                &request.principal,
                &request.workspace,
                &request.correlation_id,
            )
            .await
        {
            if !record.state.is_terminal() {
                let now = self.clock.now_ms();
                self.store
                    .update(&record.id, |r| {
                        r.state = DeliveryState::Expired;
                        r.set_reason(reason);
                        r.updated_at_ms = now;
                    })
                    .await;
            }
        }
    }

    async fn run_push_delivery(
        &self,
        record: DeliveryRecord,
        request: &RequestIdentity,
        deadline_ms: Option<i64>,
        requested_at_ms: i64,
        cancel: CancellationToken,
    ) {
        let Some(push) = &self.push else { return };
        let (principal, workspace, correlation_id) = (
            &request.principal,
            &request.workspace,
            &request.correlation_id,
        );
        for attempt in 1..=MAX_ATTEMPTS {
            if attempt > 1 {
                let delay = self
                    .timings
                    .backoff
                    .get(attempt as usize - 2)
                    .copied()
                    .unwrap_or(Duration::from_secs(30));
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(delay) => {},
                }
            }
            if cancel.is_cancelled() {
                return;
            }
            if !self.request_live(request, deadline_ms).await {
                let now = self.clock.now_ms();
                self.store
                    .update(&record.id, |r| {
                        if !matches!(r.state, DeliveryState::Resolved | DeliveryState::Skipped) {
                            r.state = DeliveryState::Expired;
                            r.set_reason("the request closed before the push");
                            r.updated_at_ms = now;
                        }
                    })
                    .await;
                return;
            }
            let now = self.clock.now_ms();
            self.store
                .update(&record.id, |r| {
                    r.attempts = attempt;
                    r.updated_at_ms = now;
                })
                .await;
            let wave = push
                .attention_requested(principal, workspace, correlation_id, requested_at_ms)
                .await;
            let now = self.clock.now_ms();
            if wave.registrations == 0 {
                self.store
                    .update(&record.id, |r| {
                        r.state = DeliveryState::Unavailable;
                        r.registrations = Some(0);
                        r.set_reason("no registered device");
                        r.updated_at_ms = now;
                    })
                    .await;
                return;
            }
            if wave.accepted > 0 {
                self.store
                    .update(&record.id, |r| {
                        r.state = DeliveryState::ProviderAccepted;
                        r.registrations = Some(wave.registrations);
                        r.accepted_at_ms = Some(now);
                        r.updated_at_ms = now;
                    })
                    .await;
                return;
            }
            if wave.not_configured >= wave.registrations {
                // No push provider on this runtime. Retrying cannot change
                // that, and three attempts of backoff would end `failed` on
                // the owner's status for every critical ask.
                self.store
                    .update(&record.id, |r| {
                        r.state = DeliveryState::Unavailable;
                        r.registrations = Some(wave.registrations);
                        r.set_reason("no push provider is configured on this runtime");
                        r.updated_at_ms = now;
                    })
                    .await;
                return;
            }
            self.store
                .update(&record.id, |r| {
                    r.registrations = Some(wave.registrations);
                    r.set_reason("the push provider did not accept");
                    r.updated_at_ms = now;
                })
                .await;
        }
        let now = self.clock.now_ms();
        self.store
            .update(&record.id, |r| {
                r.state = DeliveryState::Failed;
                r.updated_at_ms = now;
            })
            .await;
    }

    async fn run_staged(
        self: &Arc<Self>,
        records: Vec<(ChannelDestination, DeliveryRecord)>,
        request: &Arc<RequestIdentity>,
        alert: Value,
        deadline_ms: Option<i64>,
        fallback: Duration,
        cancel: CancellationToken,
    ) {
        let mut remaining = records.into_iter();
        let mut running: Vec<(String, tokio::task::JoinHandle<()>)> = Vec::new();
        while let Some((destination, record)) = remaining.next() {
            if cancel.is_cancelled() {
                break;
            }
            let c = Arc::clone(self);
            let request = Arc::clone(request);
            let (alert, cancel_child) = (alert.clone(), cancel.clone());
            let record_id = record.id.clone();
            running.push((
                record_id.clone(),
                tokio::spawn(async move {
                    c.run_channel_delivery(
                        record,
                        destination,
                        &request,
                        alert,
                        deadline_ms,
                        cancel_child,
                    )
                    .await;
                }),
            ));
            // Wait for this destination to be accepted, or the fallback
            // delay, before offering the next.
            let accepted = tokio::select! {
                _ = cancel.cancelled() => break,
                accepted = self.wait_for(&record_id, fallback, |state| matches!(state, DeliveryState::ProviderAccepted | DeliveryState::ConfirmedDelivered)) => accepted,
            };
            if accepted {
                let now = self.clock.now_ms();
                for (_, record) in remaining.by_ref() {
                    self.store
                        .update(&record.id, |r| {
                            r.state = DeliveryState::Skipped;
                            r.set_reason("a preferred destination accepted");
                            r.updated_at_ms = now;
                        })
                        .await;
                }
                break;
            }
        }
        for (_, task) in running {
            let _ = task.await;
        }
    }

    /// Poll a record until `done(state)` or the deadline; `true` when done.
    async fn wait_for(
        &self,
        record_id: &str,
        limit: Duration,
        done: impl Fn(DeliveryState) -> bool,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            match self.store.get(record_id).await {
                Some(record) if done(record.state) => return true,
                Some(record) if record.state.is_terminal() => return false,
                None => return false,
                _ => {},
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Every attempt for one channel, and then the card is reclaimed: once
    /// this returns the row can no longer be claimed (a claim needs `queued`),
    /// so keeping the card only grew the live map until something wiped it.
    async fn run_channel_delivery(
        &self,
        record: DeliveryRecord,
        destination: ChannelDestination,
        request: &RequestIdentity,
        alert: Value,
        deadline_ms: Option<i64>,
        cancel: CancellationToken,
    ) {
        let card_id = record.id.clone();
        self.run_channel_delivery_attempts(
            record,
            destination,
            request,
            alert,
            deadline_ms,
            cancel,
        )
        .await;
        self.cards.lock().await.remove(&card_id);
    }

    async fn run_channel_delivery_attempts(
        &self,
        record: DeliveryRecord,
        destination: ChannelDestination,
        request: &RequestIdentity,
        alert: Value,
        deadline_ms: Option<i64>,
        cancel: CancellationToken,
    ) {
        let (principal, workspace, correlation_id) = (
            &request.principal,
            &request.workspace,
            &request.correlation_id,
        );
        for attempt in 1..=MAX_ATTEMPTS {
            if attempt > 1 {
                let delay = self
                    .timings
                    .backoff
                    .get(attempt as usize - 2)
                    .copied()
                    .unwrap_or(Duration::from_secs(30));
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(delay) => {},
                }
            }
            if cancel.is_cancelled() {
                return;
            }
            if !self.request_live(request, deadline_ms).await {
                // A row awaiting its retry still reads `failed` from the last
                // report; the request closing supersedes that. Only a
                // resolution recorded meanwhile stands.
                let now = self.clock.now_ms();
                self.store
                    .update(&record.id, |r| {
                        if !matches!(r.state, DeliveryState::Resolved | DeliveryState::Skipped) {
                            r.state = DeliveryState::Expired;
                            r.set_reason("the request closed before the send");
                            r.updated_at_ms = now;
                        }
                    })
                    .await;
                return;
            }
            let now = self.clock.now_ms();
            // A resolution that landed while this attempt was deciding owns the
            // row. Re-queueing unconditionally overwrote `Resolved` — the only
            // write in this file that did not guard the terminal states — and
            // the card then went out for a request that was already answered,
            // with no retirement ever emitted (the resolution had already run).
            // A row this attempt did not manage to queue is not this attempt's
            // to offer.
            let requeued = self
                .store
                .update(&record.id, |r| {
                    // `Failed` is the ONE terminal state this loop may resume:
                    // it is the report this very attempt is retrying. Guarding
                    // on `is_terminal()` swallowed it too, which killed the
                    // retry altogether — attempts 2 and 3 never happened after
                    // a transient provider failure.
                    if !matches!(
                        r.state,
                        DeliveryState::Queued | DeliveryState::Held | DeliveryState::Failed
                    ) {
                        return;
                    }
                    r.state = DeliveryState::Queued;
                    r.attempts = attempt;
                    r.claimed_by = None;
                    r.claimed_at_ms = None;
                    r.updated_at_ms = now;
                })
                .await;
            if requeued.map(|row| row.state) != Some(DeliveryState::Queued) {
                return;
            }
            self.transport
                .offer(RuntimeTransportEvent::CriticalRequestAlert {
                    delivery_id: record.id.clone(),
                    correlation_id: correlation_id.to_string(),
                    channel_type: destination.channel_type.clone(),
                    kind: record.kind.clone(),
                    alert: alert.clone(),
                    revision: record.revision,
                    deadline_ms,
                    principal: Some(principal.to_string()),
                    workspace: Some(workspace.to_string()),
                    timestamp: now,
                });
            // A bot must claim within the window, else nobody is connected.
            let claimed = tokio::select! {
                _ = cancel.cancelled() => return,
                claimed = self.wait_for(&record.id, self.timings.claim_window, |state| state != DeliveryState::Queued) => claimed,
            };
            if !claimed {
                let now = self.clock.now_ms();
                self.store
                    .update(&record.id, |r| {
                        if r.state == DeliveryState::Queued {
                            r.state = DeliveryState::Unavailable;
                            r.set_reason("no bot claimed the alert");
                            r.updated_at_ms = now;
                        }
                    })
                    .await;
                return;
            }
            // Then the bot must report.
            let reported = tokio::select! {
                _ = cancel.cancelled() => return,
                reported = self.wait_for(&record.id, self.timings.report_window, |state| state != DeliveryState::Claimed) => reported,
            };
            let Some(current) = self.store.get(&record.id).await else {
                return;
            };
            if !reported {
                if current.state == DeliveryState::Claimed {
                    let now = self.clock.now_ms();
                    self.store
                        .update(&record.id, |r| {
                            r.state = DeliveryState::Ambiguous;
                            r.set_reason(
                                "the bot claimed but never reported; the send may have happened",
                            );
                            r.updated_at_ms = now;
                        })
                        .await;
                }
                return;
            }
            match current.state {
                DeliveryState::ProviderAccepted | DeliveryState::ConfirmedDelivered => return,
                DeliveryState::Failed if attempt < MAX_ATTEMPTS => continue,
                _ => return,
            }
        }
    }

    /// A channel bot claims a queued delivery for its channel type.
    pub async fn claim(
        &self,
        delivery_id: &str,
        principal: &str,
        workspace: &str,
        channel_type: &str,
        bot_identity: &str,
        connection_generation: &str,
    ) -> Result<ClaimGrant, ClaimError> {
        let record = self
            .store
            .get(delivery_id)
            .await
            .ok_or(ClaimError::UnknownDelivery)?;
        if record.principal != principal || record.workspace != workspace {
            return Err(ClaimError::WrongScope);
        }
        let Destination::Channel {
            channel_type: record_channel,
            address,
        } = &record.destination
        else {
            return Err(ClaimError::WrongChannel);
        };
        if !record_channel.eq_ignore_ascii_case(channel_type) {
            return Err(ClaimError::WrongChannel);
        }
        if record.state != DeliveryState::Queued {
            return Err(ClaimError::NotClaimable(record.state));
        }
        let now = self.clock.now_ms();
        let binding = ClaimBinding {
            bot_identity: bot_identity.to_string(),
            connection_generation: connection_generation.to_string(),
        };
        // The winner is whoever performed the transition — not whoever matches
        // the binding afterwards. Comparing bindings let two claimants through
        // whenever their bindings were equal, and a bot's
        // `connection_generation` is a per-process counter, so two processes of
        // one channel (an orphan beside its replacement) both start at the same
        // generation, both claimed, and the owner got the same credential card
        // twice.
        let mut won = false;
        let claimed = self
            .store
            .update(delivery_id, |r| {
                if r.state == DeliveryState::Queued {
                    won = true;
                    r.state = DeliveryState::Claimed;
                    r.claimed_at_ms = Some(now);
                    r.claimed_by = Some(binding.clone());
                    r.updated_at_ms = now;
                }
            })
            .await
            .ok_or(ClaimError::UnknownDelivery)?;
        if !won {
            return Err(ClaimError::NotClaimable(claimed.state));
        }
        self.last_claim
            .lock()
            .await
            .insert(record_channel.clone(), now);
        // The card lives in memory only; a record without one (a runtime
        // restart between offer and claim) hands over the generic card.
        let alert = match self.cards.lock().await.get(delivery_id) {
            Some((_, alert)) => alert.clone(),
            None => {
                let policy = self.policy().await;
                AlertCard::for_request(
                    None,
                    &record.correlation_id,
                    policy.public_origin.as_deref(),
                )
                .to_value(now)
            },
        };
        let deadline_ms = alert.get("deadline_ms").and_then(Value::as_i64);
        Ok(ClaimGrant {
            delivery_id: record.id,
            correlation_id: record.correlation_id,
            kind: record.kind,
            channel_type: record_channel.clone(),
            address: address.clone(),
            alert,
            deadline_ms,
        })
    }

    /// The claiming bot reports the provider's answer.
    pub async fn report(
        &self,
        delivery_id: &str,
        principal: &str,
        workspace: &str,
        bot_identity: &str,
        connection_generation: Option<&str>,
        outcome: DeliveryOutcome,
    ) -> Result<DeliveryRecord, ReportError> {
        let record = self
            .store
            .get(delivery_id)
            .await
            .ok_or(ReportError::UnknownDelivery)?;
        if record.principal != principal || record.workspace != workspace {
            return Err(ReportError::WrongScope);
        }
        let claimed_by = record.claimed_by.as_ref();
        if claimed_by.map(|b| b.bot_identity.as_str()) != Some(bot_identity) {
            return Err(ReportError::NotTheClaimant);
        }
        // The same bot NAME can be two processes — an orphan beside its
        // replacement. Only the connection that claimed may say what the
        // provider did with the send; an orphan's report would take a delivery
        // terminal on behalf of a send its replacement is still making. A bot
        // that names no connection is an older SDK: admitted, as before, since
        // the field it does not send cannot be checked.
        if let (Some(claimed), Some(reporting)) = (
            claimed_by
                .map(|b| b.connection_generation.as_str())
                .filter(|g| !g.is_empty()),
            connection_generation
                .map(str::trim)
                .filter(|g| !g.is_empty()),
        ) {
            if claimed != reporting {
                return Err(ReportError::NotTheClaimant);
            }
        }
        let now = self.clock.now_ms();
        // Decided INSIDE the mutate closure, under the store's write lock: the
        // snapshot above is already stale by the time the write runs, and a
        // resolution that landed in between owns the row. Reading the state
        // outside let a report overwrite a `Resolved` whose retirement had
        // already gone out.
        let mut reportable = false;
        let updated = self
            .store
            .update(delivery_id, |r| {
                reportable = match (&r.state, &outcome) {
                    (DeliveryState::Claimed, _) => true,
                    (DeliveryState::ProviderAccepted, DeliveryOutcome::ConfirmedDelivered) => true,
                    _ => false,
                };
                if !reportable {
                    return;
                }
                match &outcome {
                    DeliveryOutcome::ProviderAccepted {
                        provider_message_id,
                    } => {
                        r.state = DeliveryState::ProviderAccepted;
                        r.accepted_at_ms = Some(now);
                        r.provider_message_id = provider_message_id.clone();
                        r.reason = None;
                    },
                    DeliveryOutcome::ConfirmedDelivered => {
                        r.state = DeliveryState::ConfirmedDelivered;
                        if r.accepted_at_ms.is_none() {
                            r.accepted_at_ms = Some(now);
                        }
                    },
                    DeliveryOutcome::Failed { reason } => {
                        r.state = DeliveryState::Failed;
                        r.set_reason(reason);
                    },
                }
                r.updated_at_ms = now;
            })
            .await
            .ok_or(ReportError::UnknownDelivery)?;
        if !reportable {
            return Err(ReportError::NotReportable(updated.state));
        }
        Ok(updated)
    }

    async fn on_resolved(
        &self,
        principal: &str,
        workspace: &str,
        correlation_id: &str,
        outcome: &str,
        timestamp: i64,
    ) {
        let key = scope_key(principal, workspace, correlation_id);
        {
            let mut resolved = self.resolved.lock().await;
            resolved.push(key.clone());
            if resolved.len() > RESOLVED_MEMORY {
                let excess = resolved.len() - RESOLVED_MEMORY;
                resolved.drain(..excess);
            }
        }
        if let Some(cancel) = self.active.lock().await.remove(&key) {
            cancel.cancel();
        }
        let policy = self.policy().await;
        if policy.settings.push_enabled {
            if let Some(push) = &self.push {
                push.attention_resolved(principal, workspace, correlation_id, timestamp)
                    .await;
            }
        }
        let now = self.clock.now_ms();
        let records = self
            .store
            .for_correlation(principal, workspace, correlation_id)
            .await;
        {
            let mut cards = self.cards.lock().await;
            for record in &records {
                cards.remove(&record.id);
            }
        }
        for record in records {
            // Decide "was a card sent" from the row as it is *inside* the
            // locked update, not from the snapshot above: a bot's claim can
            // land between the two, and that card must still be retired.
            let mut prior = record.state;
            let updated = self
                .store
                .update(&record.id, |r| {
                    prior = r.state;
                    if !r.state.is_terminal() {
                        r.state = DeliveryState::Resolved;
                        r.set_reason(&format!("request {outcome}"));
                    }
                    r.updated_at_ms = now;
                })
                .await;
            if updated.is_none() {
                continue;
            }
            let sent = matches!(
                prior,
                DeliveryState::Claimed
                    | DeliveryState::ProviderAccepted
                    | DeliveryState::ConfirmedDelivered
                    | DeliveryState::Ambiguous
            );
            if sent {
                if let Destination::Channel { channel_type, .. } = &record.destination {
                    self.transport
                        .offer(RuntimeTransportEvent::CriticalRequestRetired {
                            delivery_id: record.id.clone(),
                            correlation_id: correlation_id.to_string(),
                            channel_type: channel_type.clone(),
                            outcome: outcome.to_string(),
                            principal: Some(principal.to_string()),
                            workspace: Some(workspace.to_string()),
                            timestamp: now,
                        });
                }
            }
        }
        info!(
            correlation_id,
            outcome, "[HITL-DELIVERY] request resolved; outstanding deliveries retired"
        );
    }

    /// The owner's value-free status: rows (addresses masked), latency, and
    /// when each channel's bot last claimed.
    pub async fn status(
        &self,
        principal: &str,
        workspace: &str,
        correlation_id: Option<&str>,
    ) -> StatusReport {
        let rows = match correlation_id {
            Some(correlation_id) => {
                self.store
                    .for_correlation(principal, workspace, correlation_id)
                    .await
            },
            None => {
                self.store
                    .for_scope(principal, workspace, MAX_STATUS_ROWS)
                    .await
            },
        };
        let latency = latency_summary(
            &self
                .store
                .for_scope(principal, workspace, MAX_STATUS_ROWS)
                .await,
        );
        StatusReport {
            deliveries: rows.iter().map(DeliveryRecord::status_view).collect(),
            latency,
            channels_last_claimed_ms: self.last_claim.lock().await.clone(),
        }
    }
}

#[cfg(test)]
#[path = "coordinator_tests.rs"]
mod tests;

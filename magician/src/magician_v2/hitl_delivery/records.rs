//! The delivery log (plan §6.1): routing and status metadata only, keyed by
//! scope, correlation id, revision and destination, with every state
//! tracked separately — provider acceptance is not proof the owner saw the
//! alert. Bounded and durable beside the push registration store; never the
//! alert body, never a prompt, never an answer.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tracing::warn;

use crate::magician_v2::artifact_v2::io::write_bytes_durably_with_mode;

/// The most rows kept on disk; the oldest terminal rows go first.
pub const MAX_RECORDS: usize = 2_048;
const MAX_REASON_BYTES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    /// Written before any send; an alert may be on the wire.
    Queued,
    /// Held by quiet hours until the window ends.
    Held,
    /// A channel bot claimed the delivery and holds the address.
    Claimed,
    /// The provider accepted the send (not proof the owner saw it).
    ProviderAccepted,
    /// The provider reported delivery, where a provider does.
    ConfirmedDelivered,
    /// Every attempt failed.
    Failed,
    /// No bot claimed the alert, or no device is registered.
    Unavailable,
    /// The bot claimed but never reported: the send may or may not have
    /// happened, and the provider offers no idempotency — not retried.
    Ambiguous,
    /// The request's deadline passed before a send.
    Expired,
    /// The request resolved (answered, cancelled, expired upstream) — every
    /// queued retry stopped, cards retired.
    Resolved,
    /// The request's own origin channel already carries the notice.
    RelayedByOrigin,
    /// A preferred destination accepted first (staged policy).
    Skipped,
}

impl DeliveryState {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Held | Self::Claimed)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Held => "held",
            Self::Claimed => "claimed",
            Self::ProviderAccepted => "provider_accepted",
            Self::ConfirmedDelivered => "confirmed_delivered",
            Self::Failed => "failed",
            Self::Unavailable => "unavailable",
            Self::Ambiguous => "ambiguous",
            Self::Expired => "expired",
            Self::Resolved => "resolved",
            Self::RelayedByOrigin => "relayed_by_origin",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Destination {
    /// Every registered device of the scope, through the push dispatcher.
    Push,
    /// One verified owner address on one channel type.
    Channel {
        channel_type: String,
        address: String,
    },
}

impl Destination {
    pub fn channel_type(&self) -> Option<&str> {
        match self {
            Self::Push => None,
            Self::Channel { channel_type, .. } => Some(channel_type),
        }
    }

    /// The destination as the status surface shows it: the channel type and a
    /// masked address (last two characters), never the full address.
    pub fn label(&self) -> String {
        match self {
            Self::Push => "push".to_string(),
            Self::Channel {
                channel_type,
                address,
            } => {
                let tail: String = address
                    .chars()
                    .rev()
                    .take(2)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                format!("{channel_type}:…{tail}")
            },
        }
    }
}

/// Who claimed a channel delivery: the bot's token identity and the realtime
/// connection generation it claimed under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimBinding {
    pub bot_identity: String,
    pub connection_generation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryRecord {
    pub id: String,
    pub principal: String,
    pub workspace: String,
    pub correlation_id: String,
    pub revision: u32,
    /// `request` or `test`.
    pub kind: String,
    pub destination: Destination,
    pub state: DeliveryState,
    pub attempts: u32,
    /// The request's own timestamp (`hitl.requested`).
    pub requested_at_ms: i64,
    /// When the coordinator wrote this row — request→enqueue latency.
    pub enqueued_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_at_ms: Option<i64>,
    /// When a provider accepted — enqueue→acceptance latency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_at_ms: Option<i64>,
    pub updated_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_message_id: Option<String>,
    /// A bounded, value-free reason for a non-success state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_by: Option<ClaimBinding>,
    /// For a push row: how many registrations the wave addressed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registrations: Option<usize>,
    /// The lane that owns the ask (`agentic`, `user_request`, …) and the
    /// execution it belongs to. Persisted so a RESTART can ask the oracle
    /// whether the request is still open instead of retiring every live row
    /// blind: an alert that was still owed when the runtime went down was
    /// marked `failed` on the way back up and nothing ever re-offered it.
    /// Absent on a row written before this field existed — such a row can
    /// only be retired, as before.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    /// The ask's own deadline, so a recovered offer keeps its expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<i64>,
}

impl DeliveryRecord {
    pub fn set_reason(&mut self, reason: &str) {
        self.reason = Some(bounded(reason, MAX_REASON_BYTES));
    }

    /// The row as the owner's status surface returns it: addresses masked.
    pub fn status_view(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "correlation_id": self.correlation_id,
            "revision": self.revision,
            "kind": self.kind,
            "destination": self.destination.label(),
            "channel_type": self.destination.channel_type(),
            "state": self.state.as_str(),
            "attempts": self.attempts,
            "requested_at_ms": self.requested_at_ms,
            "enqueued_at_ms": self.enqueued_at_ms,
            "claimed_at_ms": self.claimed_at_ms,
            "accepted_at_ms": self.accepted_at_ms,
            "updated_at_ms": self.updated_at_ms,
            "provider_message_id": self.provider_message_id,
            "reason": self.reason,
            "registrations": self.registrations,
        })
    }
}

fn bounded(text: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if out.len() + ch.len_utf8() > max {
            break;
        }
        out.push(ch);
    }
    out
}

/// Bounded, durable, private (`0600`) delivery log.
pub struct DeliveryStore {
    path: Option<PathBuf>,
    rows: RwLock<Vec<DeliveryRecord>>,
    write_lock: Mutex<()>,
}

impl DeliveryStore {
    /// Open the log under the data root. An unreadable log is set aside with
    /// a warning and a fresh one started: a routing log must never keep the
    /// runtime from booting.
    pub async fn open(base_root: &Path) -> Self {
        let path = base_root.join("system").join("hitl-deliveries.json");
        let rows = match tokio::fs::read(&path).await {
            Ok(bytes) => match serde_json::from_slice::<Vec<DeliveryRecord>>(&bytes) {
                Ok(rows) => rows,
                Err(error) => {
                    warn!(%error, path = %path.display(), "[HITL-DELIVERY] delivery log unreadable; starting a fresh one");
                    Vec::new()
                },
            },
            Err(_) => Vec::new(),
        };
        Self {
            path: Some(path),
            rows: RwLock::new(rows),
            write_lock: Mutex::new(()),
        }
    }

    /// An in-memory log for tests and for runtimes without a data root.
    pub fn ephemeral() -> Self {
        Self {
            path: None,
            rows: RwLock::new(Vec::new()),
            write_lock: Mutex::new(()),
        }
    }

    /// After a restart no delivery task is running: every row still live is
    /// closed honestly rather than left `queued` forever.
    /// Every row that is not in a terminal state, in insertion order.
    pub async fn live_rows(&self) -> Vec<DeliveryRecord> {
        self.rows
            .read()
            .await
            .iter()
            .filter(|row| !row.state.is_terminal())
            .cloned()
            .collect()
    }

    /// Retire the named rows with one reason.
    pub async fn retire(&self, ids: &[String], reason: &str, now_ms: i64) -> usize {
        let mut retired = 0;
        self.commit(|rows| {
            for row in rows
                .iter_mut()
                .filter(|row| ids.iter().any(|id| *id == row.id) && !row.state.is_terminal())
            {
                row.state = DeliveryState::Failed;
                row.set_reason(reason);
                row.updated_at_ms = now_ms;
                retired += 1;
            }
        })
        .await;
        retired
    }

    pub async fn insert(&self, record: DeliveryRecord) {
        self.commit(|rows| {
            rows.retain(|row| row.id != record.id);
            rows.push(record);
        })
        .await;
    }

    /// Apply `mutate` to one row; returns the row after the change, or
    /// `None` when the id is unknown.
    pub async fn update(
        &self,
        id: &str,
        mutate: impl FnOnce(&mut DeliveryRecord),
    ) -> Option<DeliveryRecord> {
        let mut changed = None;
        self.commit(|rows| {
            if let Some(row) = rows.iter_mut().find(|row| row.id == id) {
                mutate(row);
                changed = Some(row.clone());
            }
        })
        .await;
        changed
    }

    pub async fn get(&self, id: &str) -> Option<DeliveryRecord> {
        self.rows
            .read()
            .await
            .iter()
            .find(|row| row.id == id)
            .cloned()
    }

    pub async fn for_correlation(
        &self,
        principal: &str,
        workspace: &str,
        correlation_id: &str,
    ) -> Vec<DeliveryRecord> {
        self.rows
            .read()
            .await
            .iter()
            .filter(|row| {
                row.principal == principal
                    && row.workspace == workspace
                    && row.correlation_id == correlation_id
            })
            .cloned()
            .collect()
    }

    /// The scope's rows, newest first, at most `limit`.
    pub async fn for_scope(
        &self,
        principal: &str,
        workspace: &str,
        limit: usize,
    ) -> Vec<DeliveryRecord> {
        let rows = self.rows.read().await;
        let mut out: Vec<DeliveryRecord> = rows
            .iter()
            .filter(|row| row.principal == principal && row.workspace == workspace)
            .cloned()
            .collect();
        out.sort_by(|a, b| b.enqueued_at_ms.cmp(&a.enqueued_at_ms));
        out.truncate(limit);
        out
    }

    async fn commit(&self, mutate: impl FnOnce(&mut Vec<DeliveryRecord>)) {
        let _guard = self.write_lock.lock().await;
        let mut next = self.rows.read().await.clone();
        mutate(&mut next);
        evict(&mut next);
        if let Some(path) = &self.path {
            match serde_json::to_vec_pretty(&next) {
                Ok(bytes) => {
                    if let Err(error) =
                        write_bytes_durably_with_mode(path, &bytes, Some(0o600)).await
                    {
                        warn!(%error, "[HITL-DELIVERY] delivery log write failed; the in-memory log continues");
                    }
                },
                Err(error) => warn!(%error, "[HITL-DELIVERY] delivery log could not be serialised"),
            }
        }
        *self.rows.write().await = next;
    }
}

/// Keep the log bounded: drop the oldest terminal rows first, then — only
/// if a pathological number of live rows exist — the oldest live ones.
fn evict(rows: &mut Vec<DeliveryRecord>) {
    if rows.len() <= MAX_RECORDS {
        return;
    }
    rows.sort_by(|a, b| a.enqueued_at_ms.cmp(&b.enqueued_at_ms));
    let mut excess = rows.len() - MAX_RECORDS;
    let mut kept = Vec::with_capacity(MAX_RECORDS);
    for row in rows.drain(..) {
        if excess > 0 && row.state.is_terminal() {
            excess -= 1;
            continue;
        }
        kept.push(row);
    }
    if kept.len() > MAX_RECORDS {
        kept.drain(..kept.len() - MAX_RECORDS);
    }
    *rows = kept;
}

/// p50/p95 over the recorded latencies, per leg.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Percentiles {
    pub samples: usize,
    pub p50_ms: Option<i64>,
    pub p95_ms: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatencySummary {
    /// `hitl.requested` timestamp → the row written.
    pub request_to_enqueue: Percentiles,
    /// The row written → a provider accepted.
    pub enqueue_to_acceptance: Percentiles,
}

pub fn latency_summary(rows: &[DeliveryRecord]) -> LatencySummary {
    let mut to_enqueue: Vec<i64> = rows
        .iter()
        .filter(|row| row.kind == "request")
        .map(|row| (row.enqueued_at_ms - row.requested_at_ms).max(0))
        .collect();
    let mut to_accept: Vec<i64> = rows
        .iter()
        .filter_map(|row| {
            row.accepted_at_ms
                .map(|accepted| (accepted - row.enqueued_at_ms).max(0))
        })
        .collect();
    LatencySummary {
        request_to_enqueue: percentiles(&mut to_enqueue),
        enqueue_to_acceptance: percentiles(&mut to_accept),
    }
}

fn percentiles(samples: &mut [i64]) -> Percentiles {
    if samples.is_empty() {
        return Percentiles::default();
    }
    samples.sort_unstable();
    let at = |fraction: f64| {
        let index = ((samples.len() - 1) as f64 * fraction).round() as usize;
        samples[index.min(samples.len() - 1)]
    };
    Percentiles {
        samples: samples.len(),
        p50_ms: Some(at(0.5)),
        p95_ms: Some(at(0.95)),
    }
}

/// Group rows by correlation for a status read.
pub fn by_correlation(rows: Vec<DeliveryRecord>) -> HashMap<String, Vec<DeliveryRecord>> {
    let mut out: HashMap<String, Vec<DeliveryRecord>> = HashMap::new();
    for row in rows {
        out.entry(row.correlation_id.clone()).or_default().push(row);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, state: DeliveryState, enqueued: i64) -> DeliveryRecord {
        DeliveryRecord {
            id: id.into(),
            principal: "owner".into(),
            workspace: "ws".into(),
            correlation_id: "req".into(),
            revision: 1,
            kind: "request".into(),
            destination: Destination::Channel {
                channel_type: "telegram".into(),
                address: "123456789".into(),
            },
            state,
            attempts: 1,
            requested_at_ms: enqueued - 100,
            enqueued_at_ms: enqueued,
            claimed_at_ms: None,
            accepted_at_ms: Some(enqueued + 1_000),
            updated_at_ms: enqueued,
            provider_message_id: None,
            reason: None,
            claimed_by: None,
            registrations: None,
            source: "user_request".into(),
            execution_id: None,
            deadline_ms: None,
        }
    }

    #[test]
    fn the_status_view_masks_the_address_and_carries_no_body() {
        let mut record = row("d1", DeliveryState::Failed, 10);
        record.set_reason(&"x".repeat(500));
        let view = record.status_view();
        assert_eq!(view["destination"], "telegram:…89");
        assert!(!view.to_string().contains("123456789"));
        assert_eq!(view["reason"].as_str().unwrap().len(), MAX_REASON_BYTES);
        assert_eq!(Destination::Push.label(), "push");
    }

    #[test]
    fn eviction_drops_the_oldest_terminal_rows_first() {
        let mut rows: Vec<DeliveryRecord> = (0..MAX_RECORDS as i64 + 10)
            .map(|i| {
                row(
                    &format!("d{i}"),
                    if i < 5 {
                        DeliveryState::Queued
                    } else {
                        DeliveryState::Resolved
                    },
                    i,
                )
            })
            .collect();
        evict(&mut rows);
        assert_eq!(rows.len(), MAX_RECORDS);
        assert!(
            rows.iter()
                .take(5)
                .all(|r| r.state == DeliveryState::Queued),
            "live rows survive"
        );
        assert!(
            !rows.iter().any(|r| r.id == "d5"),
            "the oldest terminal row went first"
        );
    }

    #[test]
    fn latency_percentiles_measure_the_two_legs_separately() {
        let rows: Vec<DeliveryRecord> = (1..=20)
            .map(|i| row(&format!("d{i}"), DeliveryState::ProviderAccepted, i * 100))
            .collect();
        let summary = latency_summary(&rows);
        assert_eq!(summary.request_to_enqueue.samples, 20);
        assert_eq!(summary.request_to_enqueue.p50_ms, Some(100));
        assert_eq!(summary.enqueue_to_acceptance.p95_ms, Some(1_000));
        assert_eq!(
            latency_summary(&[]).enqueue_to_acceptance,
            Percentiles::default()
        );
    }

    #[tokio::test]
    async fn the_store_round_trips_durably_and_updates_in_place() {
        let dir = std::env::temp_dir().join(format!("hitl-deliveries-{}", uuid::Uuid::new_v4()));
        let store = DeliveryStore::open(&dir).await;
        store.insert(row("d1", DeliveryState::Queued, 1)).await;
        let updated = store
            .update("d1", |r| r.state = DeliveryState::Claimed)
            .await
            .unwrap();
        assert_eq!(updated.state, DeliveryState::Claimed);
        assert!(store.update("missing", |_| {}).await.is_none());
        let reopened = DeliveryStore::open(&dir).await;
        assert_eq!(
            reopened.get("d1").await.unwrap().state,
            DeliveryState::Claimed
        );
        assert_eq!(
            reopened.for_correlation("owner", "ws", "req").await.len(),
            1
        );
        assert!(reopened
            .for_correlation("other", "ws", "req")
            .await
            .is_empty());
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}

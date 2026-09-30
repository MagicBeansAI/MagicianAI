//! Append-only observability store for the attention-routing funnel.
//!
//! This is intentionally separate from channel-assist and resurfacing storage:
//! route events are adapter-neutral diagnostics that should span Gmail,
//! WhatsApp, memory, tasks, meetings, screen observations, and future sources.

use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::{
    attention_funnel::{AttentionRouteEvent, AttentionScope, RouteOutcome},
    blocking_admission::spawn_blocking_admitted,
};

const ROUTE_EVENT_RETENTION_DAYS: i64 = 30;
const ROUTE_EVENT_SCOPE_CAP: usize = 50_000;
const MILLIS_PER_DAY: i64 = 86_400_000;

async fn spawn_store_blocking<T, F>(
    label: &'static str,
    io_gate: &tokio::sync::Mutex<()>,
    f: F,
) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    #[cfg(test)]
    let _admission = crate::blocking_admission::ensure_blocking_admission_test_lock().await;
    // Serialize on the async side first. The SQLite connection is a 1-lock
    // mutex; occupying a process-wide blocking permit while waiting for it
    // would starve parent-dir fsync.
    let _gate = io_gate.lock().await;
    spawn_blocking_admitted(f)
        .await
        .with_context(|| format!("attention funnel {label} task panicked"))?
}

const BOOTSTRAP_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS attention_route_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    provider TEXT,
    account_alias TEXT,
    source_family TEXT NOT NULL,
    candidate_key TEXT NOT NULL,
    stage TEXT NOT NULL,
    outcome TEXT NOT NULL,
    lane TEXT,
    route_reason TEXT,
    drop_reason TEXT,
    priority TEXT,
    confidence REAL,
    occurred_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    metadata_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS attention_route_events_scope_created_idx
    ON attention_route_events(principal, workspace, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS attention_route_events_scope_stage_idx
    ON attention_route_events(principal, workspace, stage);
CREATE INDEX IF NOT EXISTS attention_route_events_scope_source_kind_idx
    ON attention_route_events(principal, workspace, source_kind);
CREATE INDEX IF NOT EXISTS attention_route_events_scope_source_family_idx
    ON attention_route_events(principal, workspace, source_family);
CREATE INDEX IF NOT EXISTS attention_route_events_scope_lane_idx
    ON attention_route_events(principal, workspace, lane);
CREATE INDEX IF NOT EXISTS attention_route_events_scope_drop_reason_idx
    ON attention_route_events(principal, workspace, drop_reason);
"#;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttentionRouteEventSummary {
    pub event_id: String,
    pub source_kind: String,
    pub source_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_alias: Option<String>,
    pub source_family: String,
    pub candidate_key: String,
    pub stage: String,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    pub occurred_at: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttentionFunnelObservability {
    pub scope: AttentionScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<i64>,
    pub total_events: u64,
    pub routed_events: u64,
    pub dropped_events: u64,
    pub by_outcome: BTreeMap<String, u64>,
    pub by_stage: BTreeMap<String, u64>,
    pub by_source_kind: BTreeMap<String, u64>,
    pub by_source_family: BTreeMap<String, u64>,
    pub by_lane: BTreeMap<String, u64>,
    pub by_route_reason: BTreeMap<String, u64>,
    pub by_drop_reason: BTreeMap<String, u64>,
    pub recent_events: Vec<AttentionRouteEventSummary>,
}

#[derive(Clone)]
pub struct AttentionFunnelStore {
    conn: Arc<Mutex<Connection>>,
    io_gate: Arc<tokio::sync::Mutex<()>>,
}

impl AttentionFunnelStore {
    pub fn open(base_root: &Path) -> Result<Self> {
        Self::open_at(&base_root.join("attention_funnel.db"))
    }

    /// Open the cataloged host database file. Production callers pass
    /// `host_database_path(..., DatabaseOwner::AttentionFunnel)`.
    pub fn open_at(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "creating attention funnel store directory: {}",
                    parent.display()
                )
            })?;
        }
        let conn = Connection::open(db_path)
            .with_context(|| format!("opening attention funnel store at {}", db_path.display()))?;
        conn.query_row("PRAGMA journal_mode=WAL", [], |_row| Ok(()))
            .context("enabling WAL journal mode on attention funnel store")?;
        conn.execute_batch(BOOTSTRAP_DDL)
            .context("bootstrapping attention funnel store schema")?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            io_gate: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    /// Append one route event. Duplicate `event_id`s are ignored so producers can
    /// retry a best-effort observability write without double-counting.
    pub async fn append_event(&self, event: AttentionRouteEvent) -> Result<()> {
        let store = self.clone();
        spawn_store_blocking("append_event", &self.io_gate, move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction()
                .context("beginning attention funnel append_event transaction")?;
            write_event(&tx, &event)?;
            tx.commit()
                .context("committing attention funnel append_event transaction")?;
            retention_sweep_blocking(
                &conn,
                &event.scope.principal,
                &event.scope.workspace,
                event.created_at,
                ROUTE_EVENT_RETENTION_DAYS,
                ROUTE_EVENT_SCOPE_CAP,
            )?;
            Ok(())
        })
        .await
    }

    /// Append one route event and report whether THIS call inserted it.
    ///
    /// Same `INSERT OR IGNORE` + UNIQUE `event_id` semantics as
    /// [`Self::append_event`], but the caller learns whether the row was new
    /// (`true`) or already durable (`false`). Recurring Monitors Phase 3 uses
    /// this as the durable per-channel notification dedupe (plan §7.4): the
    /// `event_id` is a hash of the `(scope, monitor_task_id, monitor_revision,
    /// change_fingerprint, channel)` dedupe key, so a retried acceptance or a
    /// process restart can never emit the same notification twice.
    pub async fn append_event_returning_new(&self, event: AttentionRouteEvent) -> Result<bool> {
        let store = self.clone();
        spawn_store_blocking("append_event_returning_new", &self.io_gate, move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction()
                .context("beginning attention funnel append_event_returning_new transaction")?;
            let inserted = write_event(&tx, &event)?;
            tx.commit()
                .context("committing attention funnel append_event_returning_new transaction")?;
            retention_sweep_blocking(
                &conn,
                &event.scope.principal,
                &event.scope.workspace,
                event.created_at,
                ROUTE_EVENT_RETENTION_DAYS,
                ROUTE_EVENT_SCOPE_CAP,
            )?;
            Ok(inserted)
        })
        .await
    }

    /// Append multiple route events in a single transaction and retention sweep.
    /// Duplicate `event_id`s are ignored so producers can retry a best-effort
    /// observability write without double-counting.
    pub async fn append_events(&self, events: Vec<AttentionRouteEvent>) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let store = self.clone();
        spawn_store_blocking("append_events", &self.io_gate, move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction()
                .context("beginning attention funnel append_events transaction")?;
            for event in &events {
                write_event(&tx, event)?;
            }
            tx.commit()
                .context("committing attention funnel append_events transaction")?;
            if let Some(event) = events.first() {
                retention_sweep_blocking(
                    &conn,
                    &event.scope.principal,
                    &event.scope.workspace,
                    event.created_at,
                    ROUTE_EVENT_RETENTION_DAYS,
                    ROUTE_EVENT_SCOPE_CAP,
                )?;
            }
            Ok(())
        })
        .await
    }

    pub async fn observability(
        &self,
        principal: &str,
        workspace: &str,
        since_ms: Option<i64>,
        recent_limit: usize,
    ) -> Result<AttentionFunnelObservability> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let recent_limit = recent_limit.min(100);
        spawn_store_blocking("observability", &self.io_gate, move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let (total_events, routed_events, dropped_events) =
                totals(&conn, &principal, &workspace, since_ms)?;
            Ok(AttentionFunnelObservability {
                scope: AttentionScope {
                    principal: principal.clone(),
                    workspace: workspace.clone(),
                },
                since_ms,
                total_events,
                routed_events,
                dropped_events,
                by_outcome: counter_map(&conn, "outcome", &principal, &workspace, since_ms, true)?,
                by_stage: counter_map(&conn, "stage", &principal, &workspace, since_ms, true)?,
                by_source_kind: counter_map(
                    &conn,
                    "source_kind",
                    &principal,
                    &workspace,
                    since_ms,
                    true,
                )?,
                by_source_family: counter_map(
                    &conn,
                    "source_family",
                    &principal,
                    &workspace,
                    since_ms,
                    true,
                )?,
                by_lane: counter_map(&conn, "lane", &principal, &workspace, since_ms, false)?,
                by_route_reason: counter_map(
                    &conn,
                    "route_reason",
                    &principal,
                    &workspace,
                    since_ms,
                    false,
                )?,
                by_drop_reason: counter_map(
                    &conn,
                    "drop_reason",
                    &principal,
                    &workspace,
                    since_ms,
                    false,
                )?,
                recent_events: recent_events(
                    &conn,
                    &principal,
                    &workspace,
                    since_ms,
                    recent_limit,
                )?,
            })
        })
        .await
    }

    /// Explicit retention hook for maintenance callers and focused tests.
    pub async fn retention_sweep(
        &self,
        principal: &str,
        workspace: &str,
        now_ms: i64,
        retention_days: i64,
        scope_cap: usize,
    ) -> Result<usize> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        spawn_store_blocking("retention_sweep", &self.io_gate, move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            retention_sweep_blocking(
                &conn,
                &principal,
                &workspace,
                now_ms,
                retention_days,
                scope_cap,
            )
        })
        .await
    }
}

/// Insert one route event. Returns `true` when the row was inserted, `false`
/// when the UNIQUE `event_id` already existed (the `INSERT OR IGNORE` path).
fn write_event(conn: &Connection, event: &AttentionRouteEvent) -> Result<bool> {
    let (outcome, lane, route_reason, drop_reason, priority) = match &event.outcome {
        RouteOutcome::Routed {
            lane,
            reason,
            priority,
        } => (
            "routed",
            Some(lane.as_str()),
            Some(reason.as_str()),
            None,
            Some(priority.as_str()),
        ),
        RouteOutcome::Dropped { reason } => ("dropped", None, None, Some(reason.as_str()), None),
        RouteOutcome::Traced { status } => (status.as_str(), None, None, None, None),
    };
    let confidence = event
        .confidence
        .filter(|value| value.is_finite())
        .map(|value| value as f64);
    let metadata_json = serde_json::to_string(&sanitize_metadata(&event.metadata))
        .context("serializing attention route event metadata")?;
    let inserted = conn
        .execute(
            "INSERT OR IGNORE INTO attention_route_events (
            event_id, principal, workspace, source_kind, source_ref, provider,
            account_alias, source_family, candidate_key, stage, outcome, lane,
            route_reason, drop_reason, priority, confidence, occurred_at,
            created_at, metadata_json
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                event.event_id.as_str(),
                event.scope.principal.as_str(),
                event.scope.workspace.as_str(),
                event.source.kind.as_str(),
                event.source.source_ref.as_str(),
                event.source.provider.as_deref(),
                event.source.account_alias.as_deref(),
                event.source_family.as_str(),
                event.candidate_key.as_str(),
                event.stage.as_str(),
                outcome,
                lane,
                route_reason,
                drop_reason,
                priority,
                confidence,
                event.occurred_at,
                event.created_at,
                metadata_json,
            ],
        )
        .context("inserting attention route event")?;
    Ok(inserted > 0)
}

fn retention_sweep_blocking(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    now_ms: i64,
    retention_days: i64,
    scope_cap: usize,
) -> Result<usize> {
    let mut pruned = 0usize;
    let cutoff = now_ms.saturating_sub(retention_days.max(0).saturating_mul(MILLIS_PER_DAY));
    pruned += conn
        .execute(
            "DELETE FROM attention_route_events \
             WHERE principal = ? AND workspace = ? AND created_at < ?",
            params![principal, workspace, cutoff],
        )
        .context("age-pruning attention route events")?;
    pruned += cap_prune_blocking(conn, principal, workspace, scope_cap)?;
    Ok(pruned)
}

/// Trim one scope down to its newest `scope_cap` rows.
///
/// ## Why this is not `id NOT IN (SELECT … LIMIT cap)`
///
/// The cap prune runs inside every `append_event` / `append_events`, and those
/// are on the `/today` and `/feed/attention` request paths — the latter mounted
/// in the app shell, so it polls from every page. A scope that has reached the
/// cap therefore re-runs this on the order of a few hundred times an hour, and
/// reaching the cap is the steady state, not the exception: at the observed
/// ~2.6k rows/hour a scope fills 50k in under a day, far inside the 30-day age
/// prune above, and then stays there forever.
///
/// The `NOT IN` form paid for that by materialising `cap` ids into an ephemeral
/// index on every single call and probing the whole scope against it. The two
/// statements below walk `attention_route_events_scope_created_idx` instead:
/// one index-ordered seek to the boundary row, then one index-ordered range
/// delete of everything below it. No ephemeral structure, and the delete
/// touches only the rows it actually removes.
///
/// ## Tie-break
///
/// The order is the composite `(created_at, id)`, which is what the index and
/// the read path (`recent_events`) already use — NOT `created_at` alone.
/// `created_at` is nowhere near unique here because producers append in
/// batches: a live scope was measured holding 50,000 rows across only 582
/// distinct `created_at` values, with 2,065 rows sharing the busiest one. A
/// boundary expressed on `created_at` alone would take or leave such a batch
/// whole and miss the cap by up to its size, and which rows survived would
/// depend on scan order. `id` is `INTEGER PRIMARY KEY AUTOINCREMENT`, so
/// `(created_at, id)` is a total order and the boundary is exact: the delete
/// predicate is precisely the complement of the `cap` rows the boundary seek
/// walked past, giving the same surviving set the `NOT IN` form produced.
fn cap_prune_blocking(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    scope_cap: usize,
) -> Result<usize> {
    let cap = scope_cap.max(1) as i64;
    // The cap-th newest row. `None` means the scope holds fewer than `cap`
    // rows, so there is nothing to trim — and that is the whole cost of the
    // sweep for an under-cap scope: one index seek, no table access.
    let boundary: Option<(i64, i64)> = conn
        .query_row(
            "SELECT created_at, id FROM attention_route_events \
             WHERE principal = ? AND workspace = ? \
             ORDER BY created_at DESC, id DESC \
             LIMIT 1 OFFSET ?",
            params![principal, workspace, cap - 1],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .context("locating the attention route event scope cap boundary")?;
    let Some((boundary_created_at, boundary_id)) = boundary else {
        return Ok(0);
    };
    // Everything that sorts strictly below the boundary under the same
    // `(created_at DESC, id DESC)` order. The boundary row itself is the
    // cap-th newest and is kept.
    let pruned = conn
        .execute(
            "DELETE FROM attention_route_events \
             WHERE principal = ? AND workspace = ? \
             AND (created_at < ? OR (created_at = ? AND id < ?))",
            params![
                principal,
                workspace,
                boundary_created_at,
                boundary_created_at,
                boundary_id,
            ],
        )
        .context("cap-pruning attention route events")?;
    Ok(pruned)
}

fn totals(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    since_ms: Option<i64>,
) -> Result<(u64, u64, u64)> {
    let base = "SELECT COUNT(*), \
        COALESCE(SUM(CASE WHEN outcome = 'routed' THEN 1 ELSE 0 END), 0), \
        COALESCE(SUM(CASE WHEN outcome = 'dropped' THEN 1 ELSE 0 END), 0) \
        FROM attention_route_events WHERE principal = ? AND workspace = ?";
    let (total, routed, dropped): (i64, i64, i64) = if let Some(since_ms) = since_ms {
        conn.query_row(
            &format!("{base} AND created_at >= ?"),
            params![principal, workspace, since_ms],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
    } else {
        conn.query_row(base, params![principal, workspace], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
    }
    .context("reading attention route event totals")?;
    Ok((
        total.max(0) as u64,
        routed.max(0) as u64,
        dropped.max(0) as u64,
    ))
}

fn counter_map(
    conn: &Connection,
    column: &str,
    principal: &str,
    workspace: &str,
    since_ms: Option<i64>,
    include_null: bool,
) -> Result<BTreeMap<String, u64>> {
    debug_assert!(matches!(
        column,
        "outcome"
            | "stage"
            | "source_kind"
            | "source_family"
            | "lane"
            | "route_reason"
            | "drop_reason"
    ));
    let null_filter = if include_null {
        String::new()
    } else {
        format!(" AND {column} IS NOT NULL AND {column} <> ''")
    };
    let base = format!(
        "SELECT {column}, COUNT(*) FROM attention_route_events \
         WHERE principal = ? AND workspace = ?{null_filter}"
    );
    let sql = if since_ms.is_some() {
        format!("{base} AND created_at >= ? GROUP BY {column} ORDER BY {column}")
    } else {
        format!("{base} GROUP BY {column} ORDER BY {column}")
    };
    let mut stmt = conn
        .prepare(&sql)
        .with_context(|| format!("preparing attention counter for {column}"))?;
    let mut out = BTreeMap::new();
    if let Some(since_ms) = since_ms {
        let rows = stmt.query_map(params![principal, workspace, since_ms], |row| {
            Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (key, count) = row?;
            out.insert(
                key.unwrap_or_else(|| "unknown".to_string()),
                count.max(0) as u64,
            );
        }
    } else {
        let rows = stmt.query_map(params![principal, workspace], |row| {
            Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (key, count) = row?;
            out.insert(
                key.unwrap_or_else(|| "unknown".to_string()),
                count.max(0) as u64,
            );
        }
    }
    Ok(out)
}

fn recent_events(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    since_ms: Option<i64>,
    limit: usize,
) -> Result<Vec<AttentionRouteEventSummary>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let base = "SELECT event_id, source_kind, source_ref, provider, account_alias, \
        source_family, candidate_key, stage, outcome, lane, route_reason, drop_reason, \
        priority, confidence, occurred_at, created_at \
        FROM attention_route_events WHERE principal = ? AND workspace = ?";
    let sql = if since_ms.is_some() {
        format!("{base} AND created_at >= ? ORDER BY created_at DESC, id DESC LIMIT ?")
    } else {
        format!("{base} ORDER BY created_at DESC, id DESC LIMIT ?")
    };
    let mut stmt = conn
        .prepare(&sql)
        .context("preparing recent attention route events query")?;
    let map_row = |row: &rusqlite::Row<'_>| {
        let confidence = row
            .get::<_, Option<f64>>(13)?
            .filter(|value| value.is_finite())
            .map(|value| value as f32);
        Ok(AttentionRouteEventSummary {
            event_id: row.get(0)?,
            source_kind: row.get(1)?,
            source_ref: row.get(2)?,
            provider: row.get(3)?,
            account_alias: row.get(4)?,
            source_family: row.get(5)?,
            candidate_key: row.get(6)?,
            stage: row.get(7)?,
            outcome: row.get(8)?,
            lane: row.get(9)?,
            route_reason: row.get(10)?,
            drop_reason: row.get(11)?,
            priority: row.get(12)?,
            confidence,
            occurred_at: row.get(14)?,
            created_at: row.get(15)?,
        })
    };
    let mut out = Vec::new();
    if let Some(since_ms) = since_ms {
        let rows = stmt.query_map(
            params![principal, workspace, since_ms, limit as i64],
            map_row,
        )?;
        for row in rows {
            out.push(row?);
        }
    } else {
        let rows = stmt.query_map(params![principal, workspace, limit as i64], map_row)?;
        for row in rows {
            out.push(row?);
        }
    }
    Ok(out)
}

fn sanitize_metadata(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut sanitized = serde_json::Map::new();
            for (key, value) in map {
                if is_raw_content_key(key) {
                    sanitized.insert(
                        key.clone(),
                        serde_json::Value::String("[redacted]".to_string()),
                    );
                } else {
                    sanitized.insert(key.clone(), sanitize_metadata(value));
                }
            }
            serde_json::Value::Object(sanitized)
        },
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(sanitize_metadata).collect())
        },
        serde_json::Value::String(text) if text.chars().count() > 2048 => {
            let prefix: String = text.chars().take(2048).collect();
            serde_json::Value::String(format!("{prefix}..."))
        },
        other => other.clone(),
    }
}

fn is_raw_content_key(key: &str) -> bool {
    matches!(
        key,
        "body"
            | "raw_body"
            | "rawBody"
            | "html_body"
            | "htmlBody"
            | "plain_body"
            | "plainBody"
            | "message_body"
            | "messageBody"
            | "raw_content"
            | "rawContent"
            | "content"
            | "text"
            | "html"
    )
}

impl AttentionFunnelStore {
    /// Test/eval convenience: open a store in a fresh temp dir. Kept out of
    /// `cfg(test)` so dependent crates' test code can construct one.
    pub fn open_in_temp() -> Self {
        let dir = tempfile::TempDir::new().expect("creating attention funnel temp dir");
        let path = dir.keep();
        Self::open(&path).expect("opening attention funnel store in temp dir")
    }
}

#[cfg(test)]
mod tests {
    use crate::attention_funnel::{
        AttentionFunnelStage, AttentionLane, AttentionSource, AttentionSourceFamily,
        AttentionSourceKind, AttentionTraceStatus, DropReason, RoutePriority, RouteReason,
    };

    use super::*;

    fn event(
        event_id: &str,
        stage: AttentionFunnelStage,
        outcome: RouteOutcome,
    ) -> AttentionRouteEvent {
        AttentionRouteEvent {
            event_id: event_id.to_string(),
            scope: AttentionScope {
                principal: "p".to_string(),
                workspace: "w".to_string(),
            },
            source: AttentionSource {
                kind: AttentionSourceKind::Comm,
                source_ref: "thread-1".to_string(),
                provider: Some("gmail".to_string()),
                account_alias: Some("work".to_string()),
            },
            source_family: AttentionSourceFamily::Promise,
            candidate_key: "candidate-1".to_string(),
            stage,
            outcome,
            occurred_at: 1_000,
            created_at: 2_000,
            confidence: Some(0.8),
            metadata: serde_json::json!({ "body": "raw", "hint": "safe" }),
        }
    }

    #[tokio::test]
    async fn append_and_observability_group_route_events() {
        let store = AttentionFunnelStore::open_in_temp();
        store
            .append_event(event(
                "e1",
                AttentionFunnelStage::Routed,
                RouteOutcome::Routed {
                    lane: AttentionLane::FollowUp,
                    reason: RouteReason::PromiseOrObligation,
                    priority: RoutePriority::Normal,
                },
            ))
            .await
            .unwrap();
        store
            .append_event(event(
                "e2",
                AttentionFunnelStage::Dropped,
                RouteOutcome::Dropped {
                    reason: DropReason::WeakSignal,
                },
            ))
            .await
            .unwrap();

        let obs = store.observability("p", "w", None, 10).await.unwrap();
        assert_eq!(obs.total_events, 2);
        assert_eq!(obs.routed_events, 1);
        assert_eq!(obs.dropped_events, 1);
        assert_eq!(obs.by_stage["routed"], 1);
        assert_eq!(obs.by_stage["dropped"], 1);
        assert_eq!(obs.by_source_family["promise"], 2);
        assert_eq!(obs.by_lane["follow_up"], 1);
        assert_eq!(obs.by_drop_reason["weak_signal"], 1);
        assert_eq!(obs.recent_events.len(), 2);
    }

    #[tokio::test]
    async fn trace_events_count_by_stage_and_status_without_route_totals() {
        let store = AttentionFunnelStore::open_in_temp();
        store
            .append_event(event(
                "trace-1",
                AttentionFunnelStage::Distilled,
                RouteOutcome::Traced {
                    status: AttentionTraceStatus::Succeeded,
                },
            ))
            .await
            .unwrap();

        let obs = store.observability("p", "w", None, 10).await.unwrap();
        assert_eq!(obs.total_events, 1);
        assert_eq!(obs.routed_events, 0);
        assert_eq!(obs.dropped_events, 0);
        assert_eq!(obs.by_stage["distilled"], 1);
        assert_eq!(obs.by_outcome["succeeded"], 1);
        assert!(obs.by_lane.is_empty());
        assert!(obs.by_route_reason.is_empty());
        assert!(obs.by_drop_reason.is_empty());
    }

    #[tokio::test]
    async fn append_event_returning_new_reports_first_insert_only() {
        // Recurring Monitors Phase 3 dedupe seam: the first write of a
        // dedupe-key-derived event id is `true`; every replay (retry,
        // restart) is `false` without erroring — the UNIQUE row IS the
        // durable per-channel notification dedupe.
        let store = AttentionFunnelStore::open_in_temp();
        let notify = event(
            "monitor-notify:abc123",
            AttentionFunnelStage::Routed,
            RouteOutcome::Routed {
                lane: AttentionLane::Changed,
                reason: RouteReason::ObservedChange,
                priority: RoutePriority::Normal,
            },
        );
        assert!(store
            .append_event_returning_new(notify.clone())
            .await
            .unwrap());
        assert!(!store
            .append_event_returning_new(notify.clone())
            .await
            .unwrap());
        // Even via the fire-and-forget append, the row stays single.
        store.append_event(notify).await.unwrap();
        assert_eq!(
            store
                .observability("p", "w", None, 10)
                .await
                .unwrap()
                .total_events,
            1
        );
    }

    #[tokio::test]
    async fn duplicate_event_ids_are_idempotent_and_retention_is_scoped() {
        let store = AttentionFunnelStore::open_in_temp();
        let routed = event(
            "same",
            AttentionFunnelStage::Routed,
            RouteOutcome::Routed {
                lane: AttentionLane::WorthALook,
                reason: RouteReason::NonActionableUsefulContext,
                priority: RoutePriority::Low,
            },
        );
        store.append_event(routed.clone()).await.unwrap();
        store.append_event(routed).await.unwrap();
        assert_eq!(
            store
                .observability("p", "w", None, 10)
                .await
                .unwrap()
                .total_events,
            1
        );

        store
            .append_event(AttentionRouteEvent {
                event_id: "other-scope".to_string(),
                scope: AttentionScope {
                    principal: "other".to_string(),
                    workspace: "w".to_string(),
                },
                ..event(
                    "ignored",
                    AttentionFunnelStage::Dropped,
                    RouteOutcome::Dropped {
                        reason: DropReason::RecencyOnly,
                    },
                )
            })
            .await
            .unwrap();
        let pruned = store.retention_sweep("p", "w", 2_000, 30, 1).await.unwrap();
        assert_eq!(pruned, 0);
        assert_eq!(
            store
                .observability("other", "w", None, 10)
                .await
                .unwrap()
                .total_events,
            1
        );
    }

    /// Rows shaped the way producers actually write them: batched, so a small
    /// number of `created_at` values each carry a large block of rows, and
    /// insertion order (hence `id` order) is decorrelated from timestamp order.
    /// 240 rows over 6 timestamps, 40 per timestamp.
    fn tie_heavy_batch(count: usize, distinct_timestamps: i64) -> Vec<AttentionRouteEvent> {
        (0..count)
            .map(|index| AttentionRouteEvent {
                created_at: 1_000_000 + (index as i64 % distinct_timestamps) * 1_000,
                ..event(
                    &format!("cap-{index}"),
                    AttentionFunnelStage::Routed,
                    RouteOutcome::Routed {
                        lane: AttentionLane::FollowUp,
                        reason: RouteReason::PromiseOrObligation,
                        priority: RoutePriority::Normal,
                    },
                )
            })
            .collect()
    }

    fn scope_ids(store: &AttentionFunnelStore, extra_sql: &str) -> Vec<i64> {
        let conn = store
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = conn
            .prepare(&format!(
                "SELECT id FROM attention_route_events \
                 WHERE principal = 'p' AND workspace = 'w' {extra_sql} ORDER BY id"
            ))
            .expect("preparing scope id query");
        stmt.query_map([], |row| row.get(0))
            .expect("running scope id query")
            .collect::<rusqlite::Result<Vec<i64>>>()
            .expect("collecting scope ids")
    }

    /// The boundary-row prune must leave EXACTLY the rows the previous
    /// `id NOT IN (SELECT … ORDER BY created_at DESC, id DESC LIMIT cap)`
    /// form left — including when the cap falls in the middle of a block of
    /// rows that all share one `created_at`.
    #[tokio::test]
    async fn cap_prune_keeps_exactly_the_cap_and_the_same_rows_as_the_not_in_form() {
        const TOTAL: usize = 240;
        const CAP: usize = 100;
        let store = AttentionFunnelStore::open_in_temp();
        store
            .append_events(tie_heavy_batch(TOTAL, 6))
            .await
            .unwrap();
        assert_eq!(scope_ids(&store, "").len(), TOTAL);

        // The survivor set the old `NOT IN` delete would have produced is the
        // positive form of the very same subquery.
        let expected = scope_ids(
            &store,
            "AND id IN ( \
                 SELECT id FROM attention_route_events \
                 WHERE principal = 'p' AND workspace = 'w' \
                 ORDER BY created_at DESC, id DESC \
                 LIMIT 100 \
             )",
        );
        assert_eq!(expected.len(), CAP);

        let pruned = store
            .retention_sweep("p", "w", 1_005_000, 30, CAP)
            .await
            .unwrap();
        assert_eq!(pruned, TOTAL - CAP);
        assert_eq!(scope_ids(&store, ""), expected);

        // A second sweep is a no-op: the scope now sits exactly on the cap.
        assert_eq!(
            store
                .retention_sweep("p", "w", 1_005_000, 30, CAP)
                .await
                .unwrap(),
            0
        );
        assert_eq!(scope_ids(&store, "").len(), CAP);
    }

    /// The cap deliberately lands inside a tie block here: 6 timestamps × 40
    /// rows with a cap of 100 keeps the two newest timestamps whole (80 rows)
    /// and must then keep the 20 HIGHEST-`id` rows of the third-newest
    /// timestamp — not an arbitrary 20 of them. A `created_at`-only boundary
    /// would have to take all 40 or none.
    #[tokio::test]
    async fn cap_prune_splits_a_shared_timestamp_by_id_descending() {
        const CAP: usize = 100;
        let store = AttentionFunnelStore::open_in_temp();
        store.append_events(tie_heavy_batch(240, 6)).await.unwrap();
        let boundary_ts = 1_000_000 + 3 * 1_000;
        let in_boundary_batch = scope_ids(&store, &format!("AND created_at = {boundary_ts}"));
        assert_eq!(in_boundary_batch.len(), 40);

        store
            .retention_sweep("p", "w", 1_005_000, 30, CAP)
            .await
            .unwrap();

        // Newest two timestamps survive whole.
        for offset in [5_i64, 4] {
            let ts = 1_000_000 + offset * 1_000;
            assert_eq!(
                scope_ids(&store, &format!("AND created_at = {ts}")).len(),
                40
            );
        }
        // Third-newest is split at the cap, keeping the top 20 ids.
        let survivors = scope_ids(&store, &format!("AND created_at = {boundary_ts}"));
        assert_eq!(survivors, in_boundary_batch[20..].to_vec());
        // Everything older is gone.
        for offset in [2_i64, 1, 0] {
            let ts = 1_000_000 + offset * 1_000;
            assert!(scope_ids(&store, &format!("AND created_at = {ts}")).is_empty());
        }
        assert_eq!(scope_ids(&store, "").len(), CAP);
    }

    /// A scope under the cap must not be touched, and the boundary seek must
    /// stay scoped — another principal's rows are invisible to it.
    #[tokio::test]
    async fn cap_prune_is_a_no_op_under_the_cap_and_stays_inside_its_scope() {
        let store = AttentionFunnelStore::open_in_temp();
        store.append_events(tie_heavy_batch(40, 4)).await.unwrap();
        store
            .append_event(AttentionRouteEvent {
                event_id: "foreign".to_string(),
                scope: AttentionScope {
                    principal: "other".to_string(),
                    workspace: "w".to_string(),
                },
                created_at: 1_000_000,
                ..event(
                    "ignored",
                    AttentionFunnelStage::Dropped,
                    RouteOutcome::Dropped {
                        reason: DropReason::RecencyOnly,
                    },
                )
            })
            .await
            .unwrap();

        assert_eq!(
            store
                .retention_sweep("p", "w", 1_005_000, 30, 100)
                .await
                .unwrap(),
            0
        );
        assert_eq!(scope_ids(&store, "").len(), 40);
        assert_eq!(
            store
                .observability("other", "w", None, 10)
                .await
                .unwrap()
                .total_events,
            1
        );
    }
}

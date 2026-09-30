use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Utc};
use duckdb::Connection;
use serde_json::Value as JsonValue;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use super::duckdb_safety::{analytics_duckdb_guard, configure_analytics_connection_checked};
use super::pool::DuckDbPool;

// ---------------------------------------------------------------------------
// AnalyticsEvent
// ---------------------------------------------------------------------------

/// A single analytics event ready for ingestion.
#[derive(Debug, Clone)]
pub struct AnalyticsEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: String,
    pub source: String,
    pub principal: Option<String>,
    pub workspace: Option<String>,
    pub payload: JsonValue,
}

impl AnalyticsEvent {
    pub fn in_scope(mut self, principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        let principal = principal.into();
        let workspace = workspace.into();
        self.principal = Some(principal.clone());
        self.workspace = Some(workspace.clone());
        if let Some(payload) = self.payload.as_object_mut() {
            payload.insert("principal".to_string(), serde_json::json!(principal));
            payload.insert("workspace".to_string(), serde_json::json!(workspace));
        }
        self
    }

    /// A log line captured from the tracing subsystem.
    pub fn log(level: &str, message: &str, target: &str) -> Self {
        Self {
            timestamp: Utc::now(),
            event_type: "log".into(),
            source: target.into(),
            principal: None,
            workspace: None,
            payload: serde_json::json!({
                "level": level,
                "message": message,
                "target": target,
            }),
        }
    }

    /// A line of output from a supervised bot process.
    pub fn bot_log(bot_name: &str, stream: &str, line: &str) -> Self {
        Self {
            timestamp: Utc::now(),
            event_type: "bot_log".into(),
            source: bot_name.into(),
            principal: None,
            workspace: None,
            payload: serde_json::json!({
                "bot_name": bot_name,
                "stream": stream,
                "line": line,
            }),
        }
    }

    /// A chat message sent or received.
    pub fn chat_message(
        session_id: &str,
        principal: &str,
        workspace: &str,
        direction: &str,
        content_text: &str,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            event_type: "chat_message".into(),
            source: session_id.into(),
            principal: Some(principal.to_string()),
            workspace: Some(workspace.to_string()),
            payload: serde_json::json!({
                "session_id": session_id,
                "principal": principal,
                "workspace": workspace,
                "direction": direction,
                "content_text": content_text,
            }),
        }
    }

    /// Chat session lifecycle event (created, active, closed, etc.).
    pub fn chat_session(
        session_id: &str,
        principal: &str,
        workspace: &str,
        agent_id: &str,
        status: &str,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            event_type: "chat_session".into(),
            source: session_id.into(),
            principal: Some(principal.to_string()),
            workspace: Some(workspace.to_string()),
            payload: serde_json::json!({
                "session_id": session_id,
                "principal": principal,
                "workspace": workspace,
                "agent_id": agent_id,
                "status": status,
            }),
        }
    }

    /// Task execution lifecycle event.
    pub fn task_execution(
        task_id: &str,
        execution_id: &str,
        principal: &str,
        workspace: &str,
        status: &str,
        duration_ms: Option<u64>,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            event_type: "task_execution".into(),
            source: task_id.into(),
            principal: Some(principal.to_string()),
            workspace: Some(workspace.to_string()),
            payload: serde_json::json!({
                "task_id": task_id,
                "execution_id": execution_id,
                "principal": principal,
                "workspace": workspace,
                "status": status,
                "duration_ms": duration_ms,
            }),
        }
    }

    /// Individual step within a task execution.
    pub fn task_step(execution_id: &str, step_number: u32, step_name: &str, status: &str) -> Self {
        Self {
            timestamp: Utc::now(),
            event_type: "task_step".into(),
            source: execution_id.into(),
            principal: None,
            workspace: None,
            payload: serde_json::json!({
                "execution_id": execution_id,
                "step_number": step_number,
                "step_name": step_name,
                "status": status,
            }),
        }
    }

    /// An artifact was registered in the artifact store.
    pub fn artifact_registered(artifact_uid: &str, namespace: &str, name: &str) -> Self {
        Self {
            timestamp: Utc::now(),
            event_type: "artifact_registered".into(),
            source: namespace.into(),
            principal: None,
            workspace: None,
            payload: serde_json::json!({
                "artifact_uid": artifact_uid,
                "namespace": namespace,
                "name": name,
            }),
        }
    }

    /// An artifact transitioned between lifecycle states.
    pub fn artifact_transition(artifact_uid: &str, from_state: &str, to_state: &str) -> Self {
        Self {
            timestamp: Utc::now(),
            event_type: "artifact_transition".into(),
            source: artifact_uid.into(),
            principal: None,
            workspace: None,
            payload: serde_json::json!({
                "artifact_uid": artifact_uid,
                "from_state": from_state,
                "to_state": to_state,
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// AnalyticsEventSink
// ---------------------------------------------------------------------------

/// Async event sink that buffers analytics events and flushes them to DuckDB
/// in batches via a background task.
///
/// All tracing from within this module uses `target: "analytics"` so that the
/// tracing subscriber layer can filter these out to avoid feedback loops.
#[derive(Clone)]
pub struct AnalyticsEventSink {
    tx: mpsc::Sender<AnalyticsEvent>,
    dropped: Arc<AtomicU64>,
}

impl AnalyticsEventSink {
    /// Maximum events buffered in the channel before back-pressure kicks in.
    const CHANNEL_CAPACITY: usize = 10_000;
    /// Maximum events per DuckDB batch insert.
    const BATCH_SIZE: usize = 100;
    /// Maximum time to wait before flushing a partial batch.
    const FLUSH_INTERVAL: Duration = Duration::from_millis(500);
    /// Retention sweep cadence for the schemaless events table.
    const RETENTION_SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);
    /// Per-event-type count floor. Rows newer than 24h are always kept,
    /// and at least this many newest rows survive even when older.
    const RETENTION_MIN_ROWS_PER_EVENT_TYPE: i64 = 1_000;

    /// Create the sink and spawn its background ingestion task.
    ///
    /// The background task runs until `shutdown` is cancelled, at which point
    /// it drains any remaining events from the channel and flushes them.
    pub fn start(pool: Arc<DuckDbPool>, shutdown: CancellationToken) -> Self {
        let (tx, rx) = mpsc::channel(Self::CHANNEL_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));

        let sink = Self {
            tx,
            dropped: Arc::clone(&dropped),
        };

        tokio::spawn(Self::run_background(pool, rx, dropped, shutdown));

        sink
    }

    /// Try to enqueue an event.  If the channel is full the event is dropped
    /// and a counter is incremented (no allocation, no blocking).
    pub fn emit(&self, event: AnalyticsEvent) {
        match self.tx.try_send(event) {
            Ok(()) => {},
            Err(mpsc::error::TrySendError::Full(_)) => {
                let prev = self.dropped.fetch_add(1, Ordering::Relaxed);
                // Log at most once every 1000 drops to avoid spam.
                if prev.is_multiple_of(1000) {
                    warn!(
                        target: "analytics",
                        dropped = prev + 1,
                        "analytics channel full — dropping events"
                    );
                }
            },
            Err(mpsc::error::TrySendError::Closed(_)) => {
                // Background task has shut down; silently discard.
            },
        }
    }

    /// Return the cumulative number of events dropped due to back-pressure.
    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    // -----------------------------------------------------------------------
    // Background ingestion task
    // -----------------------------------------------------------------------

    async fn run_background(
        pool: Arc<DuckDbPool>,
        mut rx: mpsc::Receiver<AnalyticsEvent>,
        dropped: Arc<AtomicU64>,
        shutdown: CancellationToken,
    ) {
        debug!(target: "analytics", "background ingestion task started");

        let mut batch: Vec<AnalyticsEvent> = Vec::with_capacity(Self::BATCH_SIZE);
        let mut interval = tokio::time::interval(Self::FLUSH_INTERVAL);
        let mut next_retention_sweep = tokio::time::Instant::now();
        // The first tick completes immediately — consume it so we start
        // waiting for the real interval.
        interval.tick().await;

        loop {
            tokio::select! {
                biased;

                // Prefer draining events over ticking the timer.
                maybe_event = rx.recv() => {
                    match maybe_event {
                        Some(event) => {
                            batch.push(event);
                            if batch.len() >= Self::BATCH_SIZE {
                                Self::flush_batch(&pool, &mut batch, &dropped).await;
                                Self::sweep_retention_if_due(&pool, &mut next_retention_sweep)
                                    .await;
                                interval.reset();
                            }
                        }
                        None => {
                            // Channel closed (all senders dropped).
                            break;
                        }
                    }
                }
                _ = interval.tick() => {
                    if !batch.is_empty() {
                        Self::flush_batch(&pool, &mut batch, &dropped).await;
                        Self::sweep_retention_if_due(&pool, &mut next_retention_sweep).await;
                    }
                }
                _ = shutdown.cancelled() => {
                    debug!(target: "analytics", "shutdown signal received — draining channel");
                    break;
                }
            }
        }

        // Drain remaining events from the channel.
        rx.close();
        while let Ok(event) = rx.try_recv() {
            batch.push(event);
            if batch.len() >= Self::BATCH_SIZE {
                Self::flush_batch(&pool, &mut batch, &dropped).await;
            }
        }
        if !batch.is_empty() {
            Self::flush_batch(&pool, &mut batch, &dropped).await;
            Self::sweep_retention_if_due(&pool, &mut next_retention_sweep).await;
        }

        debug!(target: "analytics", "background ingestion task stopped");
    }

    async fn sweep_retention_if_due(
        pool: &Arc<DuckDbPool>,
        next_retention_sweep: &mut tokio::time::Instant,
    ) {
        let now = tokio::time::Instant::now();
        if now < *next_retention_sweep {
            return;
        }
        *next_retention_sweep = now + Self::RETENTION_SWEEP_INTERVAL;
        Self::sweep_retention(pool).await;
    }

    async fn sweep_retention(pool: &Arc<DuckDbPool>) {
        let pool = Arc::clone(pool);
        let result = tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let conn = pool.write_connection();
            trim_events_retention(&conn, Self::RETENTION_MIN_ROWS_PER_EVENT_TYPE)
        })
        .await;

        match result {
            Ok(Ok(deleted)) if deleted > 0 => {
                debug!(
                    target: "analytics",
                    deleted,
                    "trimmed analytics events retention"
                );
            },
            Ok(Ok(_)) => {},
            Ok(Err(error)) => {
                warn!(
                    target: "analytics",
                    error = %error,
                    "analytics retention sweep failed"
                );
            },
            Err(error) => {
                warn!(
                    target: "analytics",
                    error = %error,
                    "analytics retention sweep task panicked"
                );
            },
        }
    }

    /// Flush a batch of events to DuckDB via the Appender API.
    ///
    /// Runs on a blocking thread because DuckDB is synchronous.
    async fn flush_batch(
        pool: &Arc<DuckDbPool>,
        batch: &mut Vec<AnalyticsEvent>,
        _dropped: &Arc<AtomicU64>,
    ) {
        let events = std::mem::take(batch);
        let count = events.len();
        let pool = Arc::clone(pool);
        let pool_for_reconnect = Arc::clone(&pool);

        let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            {
                let conn = pool.write_connection();

                // Use Appender with explicit columns so the `id` column picks up
                // its DEFAULT (nextval from the sequence).
                let mut appender = conn
                    .appender_with_columns(
                        "events",
                        &["timestamp", "event_type", "source", "payload"],
                    )
                    .map_err(|e| anyhow::anyhow!("failed to create appender: {}", e))?;

                for event in &events {
                    let ts_str = event
                        .timestamp
                        .format("%Y-%m-%d %H:%M:%S%.6f+00")
                        .to_string();
                    let payload_str = event.payload.to_string();

                    appender
                        .append_row(duckdb::params![
                            ts_str,
                            event.event_type,
                            event.source,
                            payload_str,
                        ])
                        .map_err(|e| anyhow::anyhow!("appender row error: {}", e))?;
                }

                appender
                    .flush()
                    .map_err(|e| anyhow::anyhow!("appender flush error: {}", e))?;
            } // drop the write-connection guard before the mirror write

            // Version-stable Parquet mirror of the same batch. The events table in
            // analytics.duckdb becomes a rebuildable cache: a DuckDB storage-format
            // change can quarantine the DB and rebuild events from this mirror with
            // zero loss. Best-effort — a mirror failure must never fail the flush.
            if let Some(analytics_root) = pool.db_path().parent() {
                if let Err(err) = write_events_parquet(analytics_root, &events) {
                    warn!(
                        target: "analytics",
                        error = %err,
                        "events Parquet mirror write failed (non-fatal; DuckDB events table still written)"
                    );
                }
            }

            Ok(())
        })
        .await;

        match result {
            Ok(Ok(())) => {
                debug!(target: "analytics", count, "flushed batch to DuckDB");
            },
            Ok(Err(e)) => {
                let err_str = e.to_string();
                error!(target: "analytics", error = %e, count, "failed to flush batch to DuckDB");

                // Auto-heal: if DuckDB entered a fatal/invalidated state (e.g., disk full
                // followed by recovery), reconnect so subsequent flushes can succeed.
                if err_str.contains("invalidated") || err_str.contains("fatal") {
                    warn!(target: "analytics", "DuckDB connection invalidated — attempting reconnect");
                    let pool_ref = pool_for_reconnect.clone();
                    let reconnect_result =
                        tokio::task::spawn_blocking(move || pool_ref.reconnect_write()).await;
                    match reconnect_result {
                        Ok(Ok(())) => {
                            info!(target: "analytics", "DuckDB reconnect succeeded — analytics will resume");
                        },
                        Ok(Err(e)) => {
                            error!(target: "analytics", error = %e, "DuckDB reconnect failed — analytics disabled until restart");
                        },
                        Err(e) => {
                            error!(target: "analytics", error = %e, "DuckDB reconnect task panicked");
                        },
                    }
                }
            },
            Err(e) => {
                error!(target: "analytics", error = %e, "blocking task panicked during flush");
            },
        }
    }
}

/// Append a batch of events to the version-stable Parquet mirror at
/// `<analytics_root>/events/dt=YYYY-MM-DD/batch_<ulid>.parquet`. Mirrors the
/// llm_calls / memory_events lakehouse sinks so the `events` table can be rebuilt
/// across DuckDB storage-format changes without data loss (see
/// `DuckDbPool::open` quarantine + `rebuild_events_from_parquet`). Parquet is
/// independent of DuckDB's on-disk format, so it survives version bumps.
fn write_events_parquet(analytics_root: &Path, events: &[AnalyticsEvent]) -> anyhow::Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let partition_dt = events[0].timestamp.format("%Y-%m-%d").to_string();
    let dir = analytics_root
        .join("events")
        .join(format!("dt={partition_dt}"));
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating events parquet partition {}", dir.display()))?;
    let path = dir.join(format!("batch_{}.parquet", ulid::Ulid::new()));

    let _guard = analytics_duckdb_guard();
    let conn =
        Connection::open_in_memory().context("opening in-memory DuckDB for events parquet")?;
    configure_analytics_connection_checked(&conn, "events_parquet_write")
        .context("configuring conservative DuckDB limits for events parquet write")?;
    conn.execute_batch(
        "CREATE TABLE events_batch (timestamp_ms BIGINT NOT NULL, event_type VARCHAR NOT NULL, source VARCHAR NOT NULL, payload VARCHAR NOT NULL);",
    )
    .context("creating events_batch temp table")?;
    {
        let mut app = conn
            .appender("events_batch")
            .context("opening events_batch appender")?;
        for event in events {
            app.append_row(duckdb::params![
                event.timestamp.timestamp_millis(),
                event.event_type,
                event.source,
                event.payload.to_string(),
            ])
            .context("appending events_batch row")?;
        }
        app.flush().context("flushing events_batch appender")?;
    }
    let path_sql = path.display().to_string().replace('\'', "''");
    conn.execute_batch(&format!(
        "COPY events_batch TO '{path_sql}' (FORMAT PARQUET, COMPRESSION 'zstd');"
    ))
    .with_context(|| format!("copying events_batch to {}", path.display()))?;
    crate::magician_v2::dataset_owners::publish_written_parquet(&path)
        .with_context(|| format!("publishing events parquet {}", path.display()))?;
    Ok(())
}

fn trim_events_retention(
    conn: &duckdb::Connection,
    min_rows_per_event_type: i64,
) -> anyhow::Result<usize> {
    let cutoff = Utc::now() - chrono::Duration::hours(24);
    let cutoff_sql = cutoff.format("%Y-%m-%d %H:%M:%S%.6f+00").to_string();
    let sql = format!(
        r#"
DELETE FROM events
WHERE id IN (
    SELECT id
    FROM (
        SELECT
            id,
            timestamp,
            row_number() OVER (
                PARTITION BY event_type
                ORDER BY timestamp DESC, id DESC
            ) AS rn
        FROM events
    ) ranked
    WHERE ranked.timestamp < TIMESTAMPTZ '{cutoff_sql}'
      AND ranked.rn > {min_rows_per_event_type}
);
"#
    );
    let deleted = conn
        .execute(&sql, [])
        .map_err(|e| anyhow::anyhow!("deleting old analytics events: {e}"))?;
    if deleted > 0 {
        if let Err(error) = conn.execute_batch("CHECKPOINT;") {
            warn!(
                target: "analytics",
                error = %error,
                "analytics retention checkpoint failed"
            );
        }
    }
    Ok(deleted)
}

impl std::fmt::Debug for AnalyticsEventSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnalyticsEventSink")
            .field("dropped", &self.dropped.load(Ordering::Relaxed))
            .finish()
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use duckdb::Connection;

    fn bootstrap(conn: &Connection) {
        conn.execute_batch(
            r#"
CREATE SEQUENCE IF NOT EXISTS event_id_seq;
CREATE TABLE IF NOT EXISTS events (
    id BIGINT DEFAULT nextval('event_id_seq'),
    timestamp TIMESTAMPTZ NOT NULL,
    event_type VARCHAR NOT NULL,
    source VARCHAR NOT NULL,
    payload JSON NOT NULL
);
"#,
        )
        .unwrap();
    }

    fn insert_event(conn: &Connection, event_type: &str, timestamp: chrono::DateTime<Utc>) {
        let timestamp = timestamp.format("%Y-%m-%d %H:%M:%S%.6f+00").to_string();
        conn.execute(
            "INSERT INTO events (timestamp, event_type, source, payload) VALUES (?, ?, ?, ?)",
            duckdb::params![timestamp, event_type, "test", "{}"],
        )
        .unwrap();
    }

    #[test]
    fn trim_events_retention_keeps_newer_window_and_count_floor_per_type() {
        let conn = Connection::open_in_memory().unwrap();
        bootstrap(&conn);

        let old = Utc::now() - chrono::Duration::hours(48);
        let recent = Utc::now() - chrono::Duration::minutes(5);
        for _ in 0..5 {
            insert_event(&conn, "bot_log", old);
        }
        for _ in 0..2 {
            insert_event(&conn, "log", recent);
        }

        let deleted = trim_events_retention(&conn, 3).unwrap();
        assert_eq!(deleted, 2);

        let bot_log_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM events WHERE event_type = 'bot_log'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let log_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM events WHERE event_type = 'log'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(bot_log_count, 3);
        assert_eq!(log_count, 2);
    }
}

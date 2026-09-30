//! The activity spine's durable writer — one Parquet row per completed span.
//!
//! ```text
//! <scope>/analytics/activity_rows/dt=YYYY-MM-DD/hour=HH/batch_<ulid>.parquet
//! ```
//!
//! [`super::runtime_activity_layer`] already sees every first-party span and
//! publishes it live. That live view holds a 24h tail and answers "what is
//! running right now". This sink answers the other question — "what ran last
//! month, and how much of it was `ambient`" — which nothing could answer
//! before, because outside the LLM path no unit of work produced a durable row
//! at all.
//!
//! ## One row, written once, on close
//!
//! A row is written from [`ActivityRecord::Finished`] and never from
//! `Started`. A span that is still open has no duration and no outcome; a row
//! that carried placeholders for them would have to be updated later, and this
//! is an append-only columnar store where "later" means rewriting a file.
//! Writing only completed spans means a row is never half-populated, and the
//! cost is that a span open across a restart is never recorded — honest, and
//! visible as a `Started` on the live stream with no matching row here.
//!
//! ## No cost column
//!
//! Cost lives in `llm_dispatch_batch` and is reached by joining on
//! `activity_id`. Copying it here would create two numbers that can disagree,
//! and would force this store to understand commodities it cannot price — an
//! `exa` call bills in `usd`, a `tavily` call in credits, an `ollama` call in
//! nothing at all.
//!
//! ## Backpressure
//!
//! The submit path is non-blocking and bounded, like the layer's own queue.
//! Observability must never stall the thing it observes, so a stalled disk
//! costs rows and a counter, never latency.
//!
//! Rows are lost in three distinct ways and all three are counted separately
//! ([`ActivityRowsMetrics`]): the submit queue is full, the writer task
//! panicked, or a partition's Parquet write failed. The sink reports any
//! counter that moved on its flush cadence and again at shutdown, so a loss is
//! visible without a per-span log line at exactly the moment the process is
//! already struggling.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use duckdb::{params, Connection};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::duckdb_safety::{analytics_duckdb_guard, configure_analytics_connection_checked};
use super::runtime_activity_layer::ActivityRecord;
use crate::magician_v2::artifact_v2::workspace::{
    ArtifactV2Workspace, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};

/// Flush when this many rows are buffered across every scope.
///
/// The count is a **memory** bound, not a file-size target: a row is a handful
/// of short strings, so ten thousand of them is a few megabytes held between
/// flushes. Compaction, not this threshold, is what produces one file per day.
///
/// In practice it never fires. It beats the sixty-second timer only above
/// ~167 spans/sec (10,000 ÷ 60), and the measured rate is ~11.4.
pub const FLUSH_ROW_THRESHOLD: usize = 10_000;

/// Flush at least this often, so a crash costs at most a minute of spans.
///
/// A note on arithmetic, because the number looks like it fights the file
/// count. This timer, not the row threshold, is what fires at any realistic
/// span rate — about one file per minute per busy scope, ~1,440 a day
/// (86,400 ÷ 60). That is more than the ~290/day a five-minute batched writer
/// would produce, and vastly fewer than a genuinely unbuffered writer, which
/// at ~11.4 spans/sec would emit a file per span: ~985,000 a day.
///
/// 1,440 is still not survivable over months, and that is why compaction is
/// not optional here: hourly compaction folds those ~60 files into one, and
/// daily compaction folds the 24 hours into one. Lengthening this timer
/// instead would trade a smaller transient file count for a larger window of
/// rows lost on a crash, which is the worse trade for a store whose whole
/// purpose is retrospective truth.
pub const FLUSH_INTERVAL: Duration = Duration::from_secs(60);

/// Depth of the queue between the activity drain and this sink's task.
///
/// Sized so a several-minute stall on the writer side is absorbed rather than
/// dropped, while still refusing to grow without bound behind a wedged disk.
const SUBMIT_QUEUE_DEPTH: usize = 8192;
const INCOMPLETE_PARTITION_MARKER_FILE: &str = ".activity_rows_partition_incomplete";

#[derive(Debug, Clone, Default)]
pub struct PartitionQuerySet {
    pub files: Vec<PathBuf>,
    pub complete: bool,
}

// ─────────────────────────────────────────────────────────────────────
// The row
// ─────────────────────────────────────────────────────────────────────

/// One completed span.
///
/// The column order here is load-bearing: it is repeated by the staging DDL in
/// [`write_rows`], by the positional appender that fills it, and by the
/// projection that publishes it. All three must agree, and there is a test
/// that round-trips a row through Parquet to prove they do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityRow {
    /// The layer's process-monotonic counter, in the same decimal form the
    /// live event and `llm_dispatch_batch.activity_id` carry. This is the join
    /// key; it is never re-derived.
    pub activity_id: String,
    /// The tree edge. `None` at a root.
    pub parent_activity_id: Option<String>,
    /// The top of this span's subtree, denormalised. An orphan is its own
    /// root, so `GROUP BY root_activity_id` is total.
    pub root_activity_id: String,
    pub name: String,
    pub target: String,
    pub kind: String,
    /// One of the nine dispatch workload classes, or `None`.
    ///
    /// **`None` means undeclared and stays undeclared.** Never default it to a
    /// real class: an uninstrumented root must show up as a gap to go fix, not
    /// borrow a plausible-looking answer. Same discipline as `outcome` being
    /// `closed` rather than `success`.
    pub workload_class: Option<String>,
    /// Dispatch priority lane.
    ///
    /// **Always `None` today.** The column exists because the spine's schema
    /// specifies it and a Parquet column added later is a schema migration,
    /// whereas an absent value in an existing column is just an absent value —
    /// `union_by_name` readers already tolerate it.
    ///
    /// Filling it means declaring a `priority` field on the span and narrowing
    /// it in the layer's visitor onto the closed set exactly as
    /// `workload_class` is narrowed. Narrow onto
    /// [`magicllm::dispatch::Priority::as_str`]'s output — `high` / `normal` /
    /// `background` — and not onto the Rust variant names. That distinction is
    /// not cosmetic: the same mistake with `workload_class` put `ForegroundChat`
    /// on the wire beside a stored `foreground_chat`, which grouped correctly
    /// in the view and joined against nothing, with nothing erroring.
    pub priority: Option<String>,
    pub principal: String,
    pub workspace: String,
    pub agent_id: Option<String>,
    pub thread_id: Option<String>,
    pub task_id: Option<String>,
    pub model: Option<String>,
    pub started_at_ms: i64,
    pub duration_ms: i64,
    /// `success` | `error` | `cancelled` | `closed`. `closed` is not success —
    /// the layer watched the span close, it did not watch it succeed.
    pub outcome: String,
    /// Partition key, `YYYY-MM-DD`, from `started_at_ms`.
    pub dt: String,
    /// Partition key, `0..=23`, from `started_at_ms`.
    pub hour: i32,
}

impl ActivityRow {
    /// Build a row from a drained record, or `None` if the record is not a
    /// close.
    ///
    /// The one place `Started` and `Progress` are turned away, so "a row is
    /// only written on `Finished`" is a property of one function rather than a
    /// convention spread over the sink.
    pub fn from_record(record: &ActivityRecord) -> Option<Self> {
        let ActivityRecord::Finished {
            activity_id,
            parent_activity_id,
            root_activity_id,
            name,
            target,
            kind,
            workload_class,
            agent_id,
            thread_id,
            task_id,
            model,
            started_at_ms,
            duration_ms,
            outcome,
            principal,
            workspace,
            ..
        } = record
        else {
            return None;
        };

        let (dt, hour) = partition_key_for(*started_at_ms);
        Some(Self {
            activity_id: activity_id.to_string(),
            parent_activity_id: parent_activity_id.map(|id| id.to_string()),
            root_activity_id: root_activity_id.to_string(),
            name: name.to_string(),
            target: target.to_string(),
            kind: kind.to_string(),
            workload_class: workload_class.map(str::to_string),
            priority: None,
            // The layer resolves every span to a scope, defaulting to
            // `anonymous`/`default` when nothing declared one, so these are in
            // practice always `Some`. The fallback repeats that default rather
            // than inventing a third bucket, because the schema declares both
            // columns NOT NULL and a row filed nowhere is a row nobody finds.
            principal: principal
                .clone()
                .unwrap_or_else(|| DEFAULT_SCOPE_PRINCIPAL.to_string()),
            workspace: workspace
                .clone()
                .unwrap_or_else(|| DEFAULT_SCOPE_WORKSPACE.to_string()),
            agent_id: agent_id.as_deref().map(str::to_string),
            thread_id: thread_id.as_deref().map(str::to_string),
            task_id: task_id.as_deref().map(str::to_string),
            model: model.as_deref().map(str::to_string),
            started_at_ms: *started_at_ms,
            duration_ms: i64::try_from(*duration_ms).unwrap_or(i64::MAX),
            outcome: outcome.to_string(),
            dt,
            hour,
        })
    }

    fn scope(&self) -> (String, String) {
        (self.principal.clone(), self.workspace.clone())
    }
}

/// The `(dt, hour)` a span is filed under, taken from when it **started**.
///
/// Start, not finish, because the analytical question is "what was this
/// machine doing at 03:00", and a span that ran from 03:00 to 07:00 was doing
/// it at 03:00. Filing at finish would leave that hour looking idle and then
/// dump four hours of work into 07:00.
///
/// The consequence is that a long span writes into an hour that may already
/// have been compacted. That is handled rather than avoided: the compaction
/// manifest recognises a raw file it has not seen and folds it into a new
/// generation, which is the same late-arrival path every other dataset uses.
fn partition_key_for(started_at_ms: i64) -> (String, i32) {
    let started = DateTime::<Utc>::from_timestamp_millis(started_at_ms).unwrap_or_else(Utc::now);
    (
        started.format("%Y-%m-%d").to_string(),
        started.format("%H").to_string().parse().unwrap_or(0),
    )
}

// ─────────────────────────────────────────────────────────────────────
// The handle
// ─────────────────────────────────────────────────────────────────────

/// Every way a row can fail to reach Parquet, counted separately.
///
/// Separately because the three have different causes and different fixes: a
/// full queue means the writer is behind, a join failure means it died, and a
/// partition failure means the disk or DuckDB refused one write. Folding them
/// into one number would make "rows are being lost" visible and "why" not.
///
/// All counters are process-cumulative and monotonic.
#[derive(Debug, Default)]
pub struct ActivityRowsMetrics {
    submit_dropped: AtomicU64,
    writer_task_failures: AtomicU64,
    partition_write_failures: AtomicU64,
    rows_lost_to_write_failures: AtomicU64,
}

impl ActivityRowsMetrics {
    /// Rows lost to a full submit queue.
    pub fn submit_dropped(&self) -> u64 {
        self.submit_dropped.load(Ordering::Relaxed)
    }

    /// Flushes whose blocking writer task panicked or was cancelled.
    pub fn writer_task_failures(&self) -> u64 {
        self.writer_task_failures.load(Ordering::Relaxed)
    }

    /// Individual `(dt, hour)` Parquet writes that failed.
    pub fn partition_write_failures(&self) -> u64 {
        self.partition_write_failures.load(Ordering::Relaxed)
    }

    /// Rows that reached the writer and still did not land.
    pub fn rows_lost_to_write_failures(&self) -> u64 {
        self.rows_lost_to_write_failures.load(Ordering::Relaxed)
    }

    /// Every row this process failed to make durable, by any route.
    pub fn rows_lost(&self) -> u64 {
        self.submit_dropped()
            .saturating_add(self.rows_lost_to_write_failures())
    }

    fn snapshot(&self) -> [u64; 4] {
        [
            self.submit_dropped(),
            self.writer_task_failures(),
            self.partition_write_failures(),
            self.rows_lost_to_write_failures(),
        ]
    }
}

/// The non-blocking submit end, held by the activity drain.
///
/// Cloneable and cheap. [`submit`](Self::submit) never awaits and never
/// blocks; a full queue costs a counted drop.
#[derive(Clone)]
pub struct ActivityRowsHandle {
    tx: mpsc::Sender<ActivityRow>,
    metrics: Arc<ActivityRowsMetrics>,
}

impl ActivityRowsHandle {
    /// Offer a drained record to the durable store.
    ///
    /// Ignores everything that is not a close, so the caller does not have to
    /// know the rule. Takes `&ActivityRecord` rather than the record itself
    /// because the drain still needs it for the live wire.
    pub fn submit(&self, record: &ActivityRecord) {
        let Some(row) = ActivityRow::from_record(record) else {
            return;
        };
        if self.tx.try_send(row).is_err() {
            // Full, or the sink task is gone. Either way the row is lost and
            // the count is the only honest record of that. Nothing is logged
            // here: this runs once per span, so a warn would be a per-span log
            // line at exactly the moment the process is already struggling.
            // The sink reports the counter on its flush cadence instead.
            self.metrics.submit_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Process-cumulative rows lost to a full queue. Monotonic; never reset.
    pub fn dropped(&self) -> u64 {
        self.metrics.submit_dropped()
    }

    /// The full loss ledger, for anything that wants more than the headline.
    pub fn metrics(&self) -> &Arc<ActivityRowsMetrics> {
        &self.metrics
    }
}

// ─────────────────────────────────────────────────────────────────────
// The sink
// ─────────────────────────────────────────────────────────────────────

/// Owns the buffering task. Dropping it aborts; [`shutdown`](Self::shutdown)
/// flushes.
pub struct ActivityRowsSink {
    handle: Option<tokio::task::JoinHandle<()>>,
    cancel: CancellationToken,
    metrics: Arc<ActivityRowsMetrics>,
}

impl ActivityRowsSink {
    /// Start the sink and return it with the handle to feed it.
    ///
    /// The handle is separate from the sink so the drain can hold a submit end
    /// without holding the task's lifetime: shutdown order is then "stop the
    /// producer, then flush the sink", not a refcount race.
    pub fn spawn(workspace: ArtifactV2Workspace) -> (Self, ActivityRowsHandle) {
        let (tx, rx) = mpsc::channel(SUBMIT_QUEUE_DEPTH);
        let metrics = Arc::new(ActivityRowsMetrics::default());
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(run_sink(
            workspace,
            rx,
            cancel.clone(),
            Arc::clone(&metrics),
        ));
        (
            Self {
                handle: Some(handle),
                cancel,
                metrics: Arc::clone(&metrics),
            },
            ActivityRowsHandle { tx, metrics },
        )
    }

    /// Rows lost to a full queue over the process's life.
    pub fn dropped(&self) -> u64 {
        self.metrics.submit_dropped()
    }

    /// The full loss ledger.
    pub fn metrics(&self) -> &Arc<ActivityRowsMetrics> {
        &self.metrics
    }

    /// Stop accepting and await the final flush.
    ///
    /// Call this on shutdown **after** the activity forwarder has stopped, so
    /// the last rows it drained are in the queue before the sink drains it.
    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle.take() {
            if let Err(error) = handle.await {
                warn!(
                    target: "analytics::activity_rows_sink",
                    error = %error,
                    "activity rows sink join failed on shutdown; the final flush may be incomplete"
                );
            }
        }
    }
}

impl Drop for ActivityRowsSink {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

async fn run_sink(
    workspace: ArtifactV2Workspace,
    mut rx: mpsc::Receiver<ActivityRow>,
    cancel: CancellationToken,
    metrics: Arc<ActivityRowsMetrics>,
) {
    let mut buffers: HashMap<(String, String), Vec<ActivityRow>> = HashMap::new();
    let mut buffered = 0_usize;
    let mut reported = metrics.snapshot();
    let mut flush_timer = tokio::time::interval(FLUSH_INTERVAL);
    flush_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick of a tokio interval completes immediately; consume it so
    // the first real flush is a minute in rather than instant-and-empty.
    flush_timer.tick().await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            maybe_row = rx.recv() => {
                let Some(row) = maybe_row else { break };
                buffers.entry(row.scope()).or_default().push(row);
                buffered += 1;
                if buffered >= FLUSH_ROW_THRESHOLD {
                    flush_all(&workspace, &mut buffers, &metrics).await;
                    buffered = 0;
                }
            },
            _ = flush_timer.tick() => {
                flush_all(&workspace, &mut buffers, &metrics).await;
                buffered = 0;
                // On the flush cadence, not per row: a loss happens when the
                // process is already under pressure, and a per-row warn would
                // add load exactly then. Reported only when a counter actually
                // moved, so a healthy sink logs nothing.
                report_losses(&metrics, &mut reported, "activity rows lost since the last report");
            },
        }
    }

    // Shutdown flush. Drain whatever the producer already handed over —
    // `try_recv` rather than `recv` so a producer that is still running cannot
    // hold shutdown open indefinitely.
    while let Ok(row) = rx.try_recv() {
        buffers.entry(row.scope()).or_default().push(row);
    }
    flush_all(&workspace, &mut buffers, &metrics).await;
    // The last word on this process's losses, including anything the final
    // flush itself failed to write.
    report_losses(
        &metrics,
        &mut reported,
        "activity rows lost before shutdown completed",
    );
}

/// Warn when any loss counter has moved since the last report.
///
/// The module's contract is that a dropped row is never silent. Nothing outside
/// tests read these counters before this existed, which made that claim
/// aspirational: a wedged disk could quietly cost a day of spans and the only
/// evidence was an atomic nobody loaded.
fn report_losses(metrics: &ActivityRowsMetrics, reported: &mut [u64; 4], message: &'static str) {
    let current = metrics.snapshot();
    if current == *reported {
        return;
    }
    warn!(
        target: "analytics::activity_rows_sink",
        submit_dropped = current[0] - reported[0],
        writer_task_failures = current[1] - reported[1],
        partition_write_failures = current[2] - reported[2],
        rows_lost_to_write_failures = current[3] - reported[3],
        submit_dropped_total = current[0],
        rows_lost_total = metrics.rows_lost(),
        "{message}"
    );
    *reported = current;
}

async fn flush_all(
    workspace: &ArtifactV2Workspace,
    buffers: &mut HashMap<(String, String), Vec<ActivityRow>>,
    metrics: &ActivityRowsMetrics,
) {
    for ((principal, scope), rows) in buffers.drain() {
        if rows.is_empty() {
            continue;
        }
        let root = workspace.analytics_activity_rows_root(&principal, &scope);
        let offered = rows.len();
        // `spawn_blocking`: the write is DuckDB plus a Parquet encode behind a
        // process-wide mutex, which is exactly the shape that must not run on
        // a reactor thread.
        match tokio::task::spawn_blocking(move || write_rows(&root, &rows)).await {
            // `write_rows` used to return `()`, so this arm could only ever see
            // a `JoinError` — a panicked task. A partition whose Parquet write
            // failed warned and vanished, uncounted, with the caller told
            // nothing. Both routes are now counted.
            Ok(outcome) => {
                metrics
                    .partition_write_failures
                    .fetch_add(outcome.partitions_failed as u64, Ordering::Relaxed);
                metrics
                    .rows_lost_to_write_failures
                    .fetch_add(outcome.rows_lost as u64, Ordering::Relaxed);
                debug!(
                    target: "analytics::activity_rows_sink",
                    principal = %principal,
                    workspace = %scope,
                    rows_written = outcome.rows_written,
                    rows_lost = outcome.rows_lost,
                    "flushed activity rows"
                );
            },
            Err(error) => {
                metrics.writer_task_failures.fetch_add(1, Ordering::Relaxed);
                metrics
                    .rows_lost_to_write_failures
                    .fetch_add(offered as u64, Ordering::Relaxed);
                warn!(
                    target: "analytics::activity_rows_sink",
                    error = %error,
                    rows = offered,
                    "activity rows writer task failed; this flush's rows are lost"
                );
            },
        }
    }
}

/// What one flush actually managed to make durable.
///
/// Returned rather than swallowed because the caller is the only place that can
/// count a loss: a partition failure is per-`(dt, hour)`, and the caller owns
/// the process-wide ledger.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ActivityWriteOutcome {
    pub rows_written: usize,
    pub rows_lost: usize,
    pub partitions_failed: usize,
}

fn set_partition_incomplete_marker(partition_dir: &Path, incomplete: bool) -> Result<()> {
    let marker = partition_dir.join(INCOMPLETE_PARTITION_MARKER_FILE);
    if incomplete {
        std::fs::write(&marker, b"1").with_context(|| {
            format!(
                "marking activity rows partition incomplete at {}",
                marker.display()
            )
        })?;
    } else if marker.is_file() {
        std::fs::remove_file(&marker).with_context(|| {
            format!(
                "clearing activity rows partition incomplete marker at {}",
                marker.display()
            )
        })?;
    }
    Ok(())
}

/// Write a flush, one file per `(dt, hour)` the batch touches.
///
/// Splitting by partition rather than filing the whole batch under the first
/// row's timestamp matters at a 60-second flush: a batch straddles an hour
/// boundary roughly once an hour, and filing those rows under the wrong hour
/// would make the boundary hour of every day quietly wrong.
pub fn write_rows(root: &Path, rows: &[ActivityRow]) -> ActivityWriteOutcome {
    let mut by_partition: HashMap<(String, i32), Vec<&ActivityRow>> = HashMap::new();
    for row in rows {
        by_partition
            .entry((row.dt.clone(), row.hour))
            .or_default()
            .push(row);
    }
    let mut outcome = ActivityWriteOutcome::default();
    for ((dt, hour), partition_rows) in by_partition {
        let partition_dir = root
            .join(format!("dt={dt}"))
            .join(format!("hour={hour:02}"));
        match write_partition(&partition_dir, &partition_rows) {
            Ok(()) => {
                outcome.rows_written += partition_rows.len();
                if let Err(error) = set_partition_incomplete_marker(&partition_dir, false) {
                    warn!(
                        target: "analytics::activity_rows_sink",
                        error = %error,
                        partition = %partition_dir.display(),
                        "activity rows partition incompleteness marker could not be cleared"
                    );
                }
            },
            Err(error) => {
                outcome.partitions_failed += 1;
                outcome.rows_lost += partition_rows.len();
                warn!(
                    target: "analytics::activity_rows_sink",
                    error = %error,
                    partition = %partition_dir.display(),
                    rows = partition_rows.len(),
                    "activity rows Parquet write failed"
                );
                if let Err(error) = set_partition_incomplete_marker(&partition_dir, true) {
                    warn!(
                        target: "analytics::activity_rows_sink",
                        error = %error,
                        partition = %partition_dir.display(),
                        "activity rows partition could not be marked incomplete"
                    );
                }
            },
        }
    }
    outcome
}

fn write_partition(partition_dir: &Path, rows: &[&ActivityRow]) -> Result<()> {
    // `batch_` is the raw-file convention `PartitionedDataset` recognises;
    // renaming it would make every file invisible to compaction.
    let parquet_path = partition_dir.join(format!("batch_{}.parquet", ulid::Ulid::new()));

    // The guard is taken *before* the directory is created, not after. Both
    // retention passes hold this same guard while they `remove_dir_all` a day
    // partition, so creating the directory outside it opened a window in which
    // this write could materialise `dt=D/hour=HH` a moment before retention
    // removed all of `dt=D` — and the `COPY` below would then fail into a
    // counted row loss, or worse, recreate a directory a sweep had just
    // finished expiring. Ordering it this way makes the create-and-write one
    // critical section against maintenance.
    let _duckdb_guard = analytics_duckdb_guard();

    std::fs::create_dir_all(partition_dir)
        .with_context(|| format!("creating activity rows dir {}", partition_dir.display()))?;

    let conn =
        Connection::open_in_memory().context("opening in-memory DuckDB for activity rows")?;
    configure_analytics_connection_checked(&conn, "activity_rows_write")
        .context("configuring conservative DuckDB limits for the activity rows write")?;

    // The staging table uses only types the appender is known to bind —
    // VARCHAR, BIGINT, INTEGER — and the published types (DATE, SMALLINT) are
    // produced by the CAST in the projection below. Binding a date through the
    // appender would make the on-disk schema depend on a driver conversion
    // rather than on SQL written here.
    conn.execute_batch(
        r#"
        CREATE TABLE activity_rows_batch (
            activity_id VARCHAR NOT NULL,
            parent_activity_id VARCHAR,
            root_activity_id VARCHAR NOT NULL,
            name VARCHAR NOT NULL,
            target VARCHAR NOT NULL,
            kind VARCHAR NOT NULL,
            workload_class VARCHAR,
            priority VARCHAR,
            principal VARCHAR NOT NULL,
            workspace VARCHAR NOT NULL,
            agent_id VARCHAR,
            thread_id VARCHAR,
            task_id VARCHAR,
            model VARCHAR,
            started_at_ms BIGINT NOT NULL,
            duration_ms BIGINT NOT NULL,
            outcome VARCHAR NOT NULL,
            dt_text VARCHAR NOT NULL,
            hour_value INTEGER NOT NULL
        );
        "#,
    )
    .context("creating activity_rows_batch staging table")?;

    {
        let mut appender = conn
            .appender("activity_rows_batch")
            .context("opening activity_rows_batch appender")?;
        for row in rows {
            appender
                .append_row(params![
                    row.activity_id,
                    row.parent_activity_id,
                    row.root_activity_id,
                    row.name,
                    row.target,
                    row.kind,
                    row.workload_class,
                    row.priority,
                    row.principal,
                    row.workspace,
                    row.agent_id,
                    row.thread_id,
                    row.task_id,
                    row.model,
                    row.started_at_ms,
                    row.duration_ms,
                    row.outcome,
                    row.dt,
                    row.hour,
                ])
                .context("appending row to activity_rows_batch")?;
        }
        appender
            .flush()
            .context("flushing activity_rows_batch appender")?;
    }

    let path_sql = parquet_path.display().to_string().replace('\'', "''");
    conn.execute_batch(&format!(
        "COPY (SELECT {ACTIVITY_ROWS_PUBLISHED_PROJECTION} FROM activity_rows_batch)
           TO '{path_sql}' (FORMAT PARQUET, COMPRESSION 'zstd');"
    ))
    .with_context(|| format!("copying activity rows to {}", parquet_path.display()))?;
    crate::magician_v2::dataset_owners::publish_written_parquet(&parquet_path).with_context(
        || {
            format!(
                "publishing activity rows parquet {}",
                parquet_path.display()
            )
        },
    )?;

    debug!(
        target: "analytics::activity_rows_sink",
        path = %parquet_path.display(),
        rows = rows.len(),
        "wrote activity rows Parquet batch"
    );
    Ok(())
}

/// The published column list, in schema order.
///
/// Shared between the write and the round-trip test so the on-disk schema has
/// exactly one definition. `dt` and `hour` are cast here rather than staged as
/// their final types — see the staging DDL.
pub const ACTIVITY_ROWS_PUBLISHED_PROJECTION: &str = "activity_id, \
     parent_activity_id, \
     root_activity_id, \
     name, \
     target, \
     kind, \
     workload_class, \
     priority, \
     principal, \
     workspace, \
     agent_id, \
     thread_id, \
     task_id, \
     model, \
     started_at_ms, \
     duration_ms, \
     outcome, \
     CAST(dt_text AS DATE) AS dt, \
     CAST(hour_value AS SMALLINT) AS hour";

// ─────────────────────────────────────────────────────────────────────
// Read side
// ─────────────────────────────────────────────────────────────────────

/// The dataset root a query should read for a scope.
pub fn query_root_for_scope(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> PathBuf {
    workspace_layout.analytics_activity_rows_root(principal, workspace)
}

/// Every Parquet object under a dataset root, deduplicated against the
/// compaction manifests.
///
/// Walks `dt=*/` and `dt=*/hour=*/`, asking
/// [`super::parquet_maintenance::partition_sources`] for each directory rather
/// than globbing. Globbing would double-count: an interrupted prune leaves raw
/// batches beside the compacted object that already contains them, and only
/// the manifest knows which. This is the same rule the `memory_events` reader
/// follows, extended one level deeper for the hour partitions.
pub fn query_parquet_files(root: &Path) -> PartitionQuerySet {
    use super::parquet_maintenance::{partition_sources, PartitionedDataset};

    let mut files = Vec::new();
    let mut complete = true;
    let Ok(days) = std::fs::read_dir(root) else {
        return PartitionQuerySet { files, complete };
    };
    for day in days.flatten() {
        let day_path = day.path();
        if !day.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        if !day_path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("dt="))
        {
            continue;
        }
        // The day level holds the compacted object once a day has been folded.
        if let Ok(sources) = partition_sources(&day_path, PartitionedDataset::ActivityRows) {
            files.extend(sources);
        }
        if day_path.join(INCOMPLETE_PARTITION_MARKER_FILE).is_file() {
            complete = false;
        }
        let Ok(hours) = std::fs::read_dir(&day_path) else {
            continue;
        };
        for hour in hours.flatten() {
            let hour_path = hour.path();
            if !hour.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                continue;
            }
            if !hour_path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("hour="))
            {
                continue;
            }
            if hour_path.join(INCOMPLETE_PARTITION_MARKER_FILE).is_file() {
                complete = false;
            }
            if let Ok(sources) = partition_sources(&hour_path, PartitionedDataset::ActivityRows) {
                files.extend(sources);
            }
        }
    }
    files.sort();
    files.dedup();
    PartitionQuerySet { files, complete }
}

/// The rollup root's Parquet objects. Flat `dt=` partitions, no hour level —
/// the hour survives as a *column* in a rollup, not as a directory.
///
/// Selected from each partition's rollup marker rather than by globbing
/// `*.parquet`, for the same reason [`query_parquet_files`] consults the
/// compaction manifests: a partition can hold more than one object summarising
/// overlapping sources, and only the marker knows which one is committed. A
/// glob counted both, and because `count` and `sum_duration_ms` are additive
/// across generations the day simply reported double — permanently, since the
/// detail that could have disproved it is deleted at seven days. See
/// `ActivityRollupManifest` for how that state was reached.
///
/// Globbing survives only as the fallback for a partition with no valid marker
/// at all, so a lost marker costs a possible over-read rather than a certain
/// silent zero.
///
/// What makes that fallback usually right is what the *last successful publish*
/// left behind, not any future sweep: `quarantine_unaccounted_rollup_objects`
/// ran immediately before it, so at the moment the marker was last written the
/// partition held only marker-named objects, and a listing and the marker agree.
///
/// It is **not** protected by a later quarantine, and the earlier claim that it
/// was is wrong. Once a day's detail is deleted, `apply_activity_retention`'s
/// first loop iterates `dt=` directories under the *detail* root; that directory
/// is gone, so `roll_up_activity_day` — and therefore the sweep — is never
/// called for that day again. The rollup partition is only ever revisited by the
/// second loop, which deletes it whole at thirteen months.
///
/// So the over-read is real and residual: an object added after that last
/// publish — restored by hand, or published by a pass that crashed before its
/// marker write — is counted by the fallback, and `count`/`sum_duration_ms` are
/// additive. That is the deliberate trade (reading long beats a silent zero) and
/// `the_rollup_reader_follows_the_marker_and_falls_back_only_without_one`
/// asserts it.
pub fn rollup_parquet_files(root: &Path) -> PartitionQuerySet {
    let mut files = Vec::new();
    let mut complete = true;
    let Ok(days) = std::fs::read_dir(root) else {
        return PartitionQuerySet { files, complete };
    };
    for day in days.flatten() {
        let day_path = day.path();
        if !day.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        if let Some((committed, partition_complete)) =
            super::parquet_maintenance::rollup_generation_files(&day_path)
        {
            if !partition_complete {
                complete = false;
            }
            files.extend(committed);
            continue;
        }
        complete = false;
        let Ok(entries) = std::fs::read_dir(&day_path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry
                .file_type()
                .map(|kind| kind.is_file())
                .unwrap_or(false)
                && path.extension().and_then(|ext| ext.to_str()) == Some("parquet")
            {
                files.push(path);
            }
        }
    }
    files.sort();
    files.dedup();
    PartitionQuerySet { files, complete }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn finished(activity_id: u64, started_at_ms: i64) -> ActivityRecord {
        ActivityRecord::Finished {
            activity_id,
            parent_activity_id: Some(7),
            root_activity_id: 7,
            name: "unit_of_work",
            target: "magician::activity_rows_test",
            kind: "background",
            workload_class: Some("ambient"),
            agent_id: Some(Arc::from("agent-1")),
            thread_id: Some(Arc::from("thread-1")),
            task_id: None,
            model: Some(Arc::from("model-x")),
            started_at_ms,
            duration_ms: 1234,
            outcome: "success",
            principal: Some("owner".to_string()),
            workspace: Some("default".to_string()),
            timestamp: started_at_ms + 1234,
        }
    }

    fn started(activity_id: u64) -> ActivityRecord {
        ActivityRecord::Started {
            activity_id,
            parent_activity_id: None,
            name: "unit_of_work",
            target: "magician::activity_rows_test",
            kind: "background",
            principal: Some("owner".to_string()),
            workspace: Some("default".to_string()),
            workload_class: Some("ambient"),
            agent_id: None,
            thread_id: None,
            task_id: None,
            model: None,
            operation: None,
            timestamp: 0,
        }
    }

    /// The rule the whole store rests on: a row exists only for work that
    /// finished, so no row is ever half-populated.
    #[test]
    fn only_a_finished_record_becomes_a_row() {
        assert!(
            ActivityRow::from_record(&started(1)).is_none(),
            "an open span has no duration and no outcome; it must not produce a row"
        );
        assert!(
            ActivityRow::from_record(&ActivityRecord::Progress {
                activity_id: Some(1),
                level: "info",
                message: "working".to_string(),
                target: "magician::activity_rows_test",
                principal: None,
                workspace: None,
                timestamp: 0,
            })
            .is_none(),
            "a log line inside a span is not a unit of work"
        );
        assert!(
            ActivityRow::from_record(&finished(2, 1_760_000_000_000)).is_some(),
            "a close must produce exactly one row"
        );
    }

    /// The same rule seen through the handle the drain actually calls, so a
    /// future refactor cannot reintroduce start-time writes by bypassing
    /// `from_record`.
    #[tokio::test]
    async fn the_submit_path_forwards_only_closes() {
        let (tx, mut rx) = mpsc::channel(8);
        let handle = ActivityRowsHandle {
            tx,
            metrics: Arc::new(ActivityRowsMetrics::default()),
        };

        handle.submit(&started(1));
        handle.submit(&finished(2, 1_760_000_000_000));

        let row = rx.try_recv().expect("the close must be queued");
        assert_eq!(row.activity_id, "2");
        assert!(
            rx.try_recv().is_err(),
            "the open must not have been queued at all"
        );
    }

    #[test]
    fn a_full_queue_drops_and_counts_rather_than_blocking() {
        let (tx, _rx) = mpsc::channel(1);
        let handle = ActivityRowsHandle {
            tx,
            metrics: Arc::new(ActivityRowsMetrics::default()),
        };

        handle.submit(&finished(1, 1_760_000_000_000));
        handle.submit(&finished(2, 1_760_000_000_000));
        handle.submit(&finished(3, 1_760_000_000_000));

        assert_eq!(
            handle.dropped(),
            2,
            "a full queue must lose rows visibly, never stall the drain"
        );
        assert_eq!(
            handle.metrics().rows_lost(),
            2,
            "a queue-full drop is a lost row, and the total must say so"
        );
    }

    /// A partition that cannot be written must be counted, not just logged.
    ///
    /// `write_rows` returned `()`, so `flush_all` could only ever see a
    /// `JoinError` — a panicked task. A real write failure warned once and
    /// disappeared, and the caller was told nothing at all.
    #[test]
    fn a_failed_partition_write_is_counted_not_merely_logged() {
        let temporary = tempfile::tempdir().expect("tempdir");
        // A regular file where the dataset root must be a directory: every
        // partition under it fails `create_dir_all`.
        let root = temporary.path().join("activity_rows");
        std::fs::write(&root, b"not a directory").expect("blocking file");

        let rows = vec![
            ActivityRow::from_record(&finished(1, 1_786_852_799_000)).expect("row"),
            ActivityRow::from_record(&finished(2, 1_786_852_801_000)).expect("row"),
        ];
        let outcome = write_rows(&root, &rows);

        assert_eq!(outcome.rows_written, 0);
        assert_eq!(outcome.rows_lost, 2, "both rows failed to land");
        assert_eq!(
            outcome.partitions_failed, 2,
            "the two rows straddle an hour boundary, so two partitions failed"
        );
    }

    /// The loss report fires only when a counter has actually moved. A healthy
    /// sink must not warn every minute forever.
    #[test]
    fn losses_are_reported_once_per_increase() {
        let metrics = ActivityRowsMetrics::default();
        let mut reported = metrics.snapshot();

        report_losses(&metrics, &mut reported, "first");
        assert_eq!(reported, [0, 0, 0, 0], "nothing moved, nothing reported");

        metrics.submit_dropped.fetch_add(3, Ordering::Relaxed);
        report_losses(&metrics, &mut reported, "second");
        assert_eq!(
            reported,
            [3, 0, 0, 0],
            "an increase must be picked up and acknowledged"
        );

        report_losses(&metrics, &mut reported, "third");
        assert_eq!(
            reported,
            [3, 0, 0, 0],
            "and the same loss must not be re-reported on every later tick"
        );
    }

    /// A span is filed under the hour it STARTED in, not the hour it ended.
    #[test]
    fn a_row_is_partitioned_by_its_start() {
        // 2026-08-16T03:59:59Z
        let started_at_ms = 1_786_852_799_000;
        let (dt, hour) = partition_key_for(started_at_ms);
        assert_eq!(dt, "2026-08-16");
        assert_eq!(hour, 3);
    }

    /// The schema round-trip: every column the sink writes comes back with the
    /// value and the type the schema declares. This is what holds the staging
    /// DDL, the positional appender and the published projection together —
    /// they are three hand-maintained lists and nothing else checks them.
    #[test]
    fn a_row_round_trips_through_parquet_with_its_declared_types() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let root = temporary.path().join("activity_rows");
        let row = ActivityRow::from_record(&finished(42, 1_786_852_799_000)).expect("a row");

        write_rows(&root, std::slice::from_ref(&row));

        let partition = root.join("dt=2026-08-16").join("hour=03");
        let files: Vec<PathBuf> = std::fs::read_dir(&partition)
            .expect("partition directory")
            .flatten()
            .map(|entry| entry.path())
            .collect();
        assert_eq!(files.len(), 1, "one flush of one partition is one file");
        assert!(
            files[0]
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("batch_")),
            "the file must use the raw-batch name compaction recognises, got {:?}",
            files[0].file_name()
        );

        let _guard = analytics_duckdb_guard();
        let conn = Connection::open_in_memory().expect("duckdb");
        configure_analytics_connection_checked(&conn, "activity_rows_round_trip")
            .expect("configure DuckDB");
        let source = files[0].display().to_string().replace('\'', "''");

        let (
            activity_id,
            parent_activity_id,
            root_activity_id,
            name,
            kind,
            workload_class,
            priority,
            agent_id,
            task_id,
            model,
            started_at_ms,
            duration_ms,
            outcome,
            dt,
            hour,
        ): (
            String,
            Option<String>,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            i64,
            i64,
            String,
            String,
            i32,
        ) = conn
            .query_row(
                &format!(
                    "SELECT activity_id, parent_activity_id, root_activity_id, name, kind,
                            workload_class, priority, agent_id, task_id, model, started_at_ms,
                            duration_ms, outcome, CAST(dt AS VARCHAR), CAST(hour AS INTEGER)
                     FROM read_parquet('{source}', hive_partitioning = false)"
                ),
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                        r.get(9)?,
                        r.get(10)?,
                        r.get(11)?,
                        r.get(12)?,
                        r.get(13)?,
                        r.get(14)?,
                    ))
                },
            )
            .expect("reading the published row");

        assert_eq!(activity_id, "42");
        assert_eq!(parent_activity_id.as_deref(), Some("7"));
        assert_eq!(root_activity_id, "7");
        assert_eq!(name, "unit_of_work");
        assert_eq!(kind, "background");
        assert_eq!(workload_class.as_deref(), Some("ambient"));
        assert_eq!(
            priority, None,
            "priority is declared and unfed; it must be absent, never a default"
        );
        assert_eq!(agent_id.as_deref(), Some("agent-1"));
        assert_eq!(
            task_id, None,
            "an undeclared identifier stays absent rather than becoming an empty string"
        );
        assert_eq!(model.as_deref(), Some("model-x"));
        assert_eq!(started_at_ms, 1_786_852_799_000);
        assert_eq!(duration_ms, 1234);
        assert_eq!(outcome, "success");
        assert_eq!(dt, "2026-08-16", "dt must publish as a real DATE");
        assert_eq!(hour, 3);

        let (dt_type, hour_type): (String, String) = conn
            .query_row(
                &format!(
                    "SELECT
                       max(CASE WHEN column_name = 'dt' THEN column_type END),
                       max(CASE WHEN column_name = 'hour' THEN column_type END)
                     FROM (DESCRIBE SELECT * FROM read_parquet('{source}', hive_partitioning = false))"
                ),
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("describing the published schema");
        assert_eq!(dt_type, "DATE");
        assert_eq!(hour_type, "SMALLINT");
    }

    #[test]
    fn a_query_set_becomes_incomplete_if_a_partition_is_marked() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let root = temporary.path().join("activity_rows");
        let partition = root.join("dt=2026-08-16").join("hour=03");
        std::fs::create_dir_all(&partition).expect("partition");
        std::fs::write(partition.join(INCOMPLETE_PARTITION_MARKER_FILE), b"1").expect("marker");

        let fileset = query_parquet_files(&root);
        assert!(
            !fileset.complete,
            "a marker file should flow into inventory completeness"
        );
        assert!(fileset.files.is_empty(), "no file means nothing to scan");
    }

    #[test]
    fn query_set_stays_complete_without_incomplete_markers() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let root = temporary.path().join("activity_rows");
        let row = ActivityRow::from_record(&finished(1, 1_786_852_799_000)).expect("row");
        write_rows(&root, &[row]);

        let fileset = query_parquet_files(&root);
        assert!(
            fileset.complete,
            "a healthy partition has no incomplete marker"
        );
        assert!(
            !fileset.files.is_empty(),
            "the flush should emit one row file"
        );
    }

    /// A flush that straddles an hour boundary must not file both hours under
    /// the first row's partition.
    #[test]
    fn a_flush_spanning_an_hour_boundary_writes_both_partitions() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let root = temporary.path().join("activity_rows");
        let rows = vec![
            ActivityRow::from_record(&finished(1, 1_786_852_799_000)).expect("row"),
            ActivityRow::from_record(&finished(2, 1_786_852_801_000)).expect("row"),
        ];

        write_rows(&root, &rows);

        assert!(root.join("dt=2026-08-16/hour=03").is_dir());
        assert!(root.join("dt=2026-08-16/hour=04").is_dir());
    }
}

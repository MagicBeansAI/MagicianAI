//! Memory analytics Parquet writer.
//!
//! This module writes small, scoped, date-partitioned Parquet batches for
//! memory retrieval and consolidation observability:
//!
//! ```text
//! <scope_root>/analytics/memory_events/dt=YYYY-MM-DD/batch_<ulid>.parquet
//! ```
//!
//! The write API is intentionally fire-and-forget. Memory rendering and
//! consolidation are correctness paths; analytics failures must not block or
//! change agent behavior.

use std::{collections::HashMap, path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use duckdb::{params, Connection};
use once_cell::sync::OnceCell;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::magician_v2::{
    agents::storage::AgentStorage,
    analytics::duckdb_safety::{analytics_duckdb_guard, configure_analytics_connection_checked},
    artifact_v2::workspace::ArtifactV2Workspace,
};

const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
const FLUSH_ROW_THRESHOLD: usize = 100;

static MEMORY_PARQUET_TX: OnceCell<mpsc::UnboundedSender<MemoryParquetJob>> = OnceCell::new();

#[derive(Debug)]
struct MemoryParquetJob {
    root: PathBuf,
    rows: Vec<MemoryAnalyticsRow>,
}

#[derive(Debug, Clone)]
pub struct MemoryAnalyticsRow {
    pub timestamp_ms: i64,
    pub principal: String,
    pub workspace: String,
    pub event_kind: String,
    pub source: String,
    pub agent_id: Option<String>,
    pub goal_id: Option<String>,
    pub scope: Option<String>,
    pub tier_name: Option<String>,
    pub item_key: Option<String>,
    pub selected: Option<bool>,
    pub score: Option<u32>,
    pub confidence: Option<f64>,
    pub query_excerpt: Option<String>,
    pub max_entries: Option<u32>,
    pub max_chars: Option<u32>,
    pub candidate_count: Option<u32>,
    pub selected_count: Option<u32>,
    pub dropped_count: Option<u32>,
    pub output_chars: Option<u32>,
    pub rule_name: Option<String>,
    pub target: Option<String>,
    pub source_kind: Option<String>,
    pub input_count: Option<u32>,
    pub output_count: Option<u32>,
    pub skipped_count: Option<u32>,
    pub eval_suite: Option<String>,
    pub eval_case_id: Option<String>,
    pub eval_query: Option<String>,
    pub eval_pass: Option<bool>,
    pub expected_count: Option<u32>,
    pub matched_count: Option<u32>,
    pub best_rank: Option<u32>,
    pub retrieval_backend: Option<String>,
    pub selected_item_keys: Option<String>,
    pub status: String,
    pub payload_json: Option<String>,
}

impl MemoryAnalyticsRow {
    pub fn now(event_kind: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            timestamp_ms: Utc::now().timestamp_millis(),
            principal: String::new(),
            workspace: String::new(),
            event_kind: event_kind.into(),
            source: source.into(),
            agent_id: None,
            goal_id: None,
            scope: None,
            tier_name: None,
            item_key: None,
            selected: None,
            score: None,
            confidence: None,
            query_excerpt: None,
            max_entries: None,
            max_chars: None,
            candidate_count: None,
            selected_count: None,
            dropped_count: None,
            output_chars: None,
            rule_name: None,
            target: None,
            source_kind: None,
            input_count: None,
            output_count: None,
            skipped_count: None,
            eval_suite: None,
            eval_case_id: None,
            eval_query: None,
            eval_pass: None,
            expected_count: None,
            matched_count: None,
            best_rank: None,
            retrieval_backend: None,
            selected_item_keys: None,
            status: "ok".to_string(),
            payload_json: None,
        }
    }
}

/// Resolve a scoped analytics root from an agent storage handle. Returns
/// `None` for legacy/unscoped memory stores so tests and old fixtures don't
/// write telemetry.
pub fn scoped_memory_events_root(storage: &AgentStorage) -> Option<(PathBuf, String, String)> {
    let (principal, workspace) = storage.scope_segments()?;
    let scope_root = storage.root().parent()?.to_path_buf();
    Some((
        scope_root.join("analytics").join("memory_events"),
        principal,
        workspace,
    ))
}

pub fn emit_rows_for_storage(storage: &AgentStorage, mut rows: Vec<MemoryAnalyticsRow>) {
    let Some((root, principal, workspace)) = scoped_memory_events_root(storage) else {
        return;
    };
    if rows.is_empty() {
        return;
    }
    for row in &mut rows {
        row.principal = principal.clone();
        row.workspace = workspace.clone();
    }
    emit_rows(root, rows);
}

pub fn emit_rows(root: PathBuf, rows: Vec<MemoryAnalyticsRow>) {
    if rows.is_empty() {
        return;
    }
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            let tx = MEMORY_PARQUET_TX.get_or_init(|| spawn_memory_parquet_worker(&handle));
            if let Err(error) = tx.send(MemoryParquetJob { root, rows }) {
                let job = error.0;
                handle.spawn_blocking(move || write_rows_logged(&job.root, &job.rows));
            }
        },
        Err(_) => {
            write_rows_logged(&root, &rows);
        },
    }
}

fn spawn_memory_parquet_worker(
    handle: &tokio::runtime::Handle,
) -> mpsc::UnboundedSender<MemoryParquetJob> {
    let (tx, rx) = mpsc::unbounded_channel();
    handle.spawn(run_memory_parquet_worker(rx));
    tx
}

async fn run_memory_parquet_worker(mut rx: mpsc::UnboundedReceiver<MemoryParquetJob>) {
    let mut buffers = HashMap::<PathBuf, Vec<MemoryAnalyticsRow>>::new();
    let mut flush_timer = tokio::time::interval(FLUSH_INTERVAL);
    flush_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            maybe_job = rx.recv() => {
                let Some(job) = maybe_job else {
                    break;
                };
                let buffer = buffers.entry(job.root.clone()).or_default();
                buffer.extend(job.rows);
                if buffer.len() >= FLUSH_ROW_THRESHOLD {
                    flush_buffer(&mut buffers, &job.root).await;
                }
            },
            _ = flush_timer.tick() => {
                flush_all_buffers(&mut buffers).await;
            },
        }
    }
    flush_all_buffers(&mut buffers).await;
}

async fn flush_all_buffers(buffers: &mut HashMap<PathBuf, Vec<MemoryAnalyticsRow>>) {
    let roots = buffers.keys().cloned().collect::<Vec<_>>();
    for root in roots {
        flush_buffer(buffers, &root).await;
    }
}

async fn flush_buffer(buffers: &mut HashMap<PathBuf, Vec<MemoryAnalyticsRow>>, root: &PathBuf) {
    let Some(rows) = buffers.get_mut(root) else {
        return;
    };
    if rows.is_empty() {
        return;
    }
    let rows = std::mem::take(rows);
    let root = root.clone();
    match tokio::task::spawn_blocking(move || write_rows_logged(&root, &rows)).await {
        Ok(()) => {},
        Err(error) => {
            warn!(
                target: "analytics::memory_parquet",
                error = %error,
                "memory analytics Parquet writer task failed"
            );
        },
    }
}

fn write_rows_logged(root: &PathBuf, rows: &[MemoryAnalyticsRow]) {
    if let Err(err) = write_rows(root, rows) {
        warn!(
            target: "analytics::memory_parquet",
            error = %err,
            rows = rows.len(),
            "memory analytics Parquet write failed"
        );
    }
}

pub fn query_root_for_scope(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> PathBuf {
    workspace_layout.analytics_memory_events_root(principal, workspace)
}

fn write_rows(root: &PathBuf, rows: &[MemoryAnalyticsRow]) -> Result<()> {
    let partition_dt = partition_date_for(rows[0].timestamp_ms);
    let partition_dir = root.join(format!("dt={partition_dt}"));
    std::fs::create_dir_all(&partition_dir)
        .with_context(|| format!("creating memory analytics dir {}", partition_dir.display()))?;

    let batch_id = ulid::Ulid::new().to_string();
    let parquet_path = partition_dir.join(format!("batch_{batch_id}.parquet"));
    let _duckdb_guard = analytics_duckdb_guard();
    let conn =
        Connection::open_in_memory().context("opening in-memory DuckDB for memory analytics")?;
    configure_analytics_connection_checked(&conn, "memory_events_parquet_write")
        .context("configuring conservative DuckDB limits for memory_events parquet write")?;
    conn.execute_batch(
        r#"
        CREATE TABLE memory_events_batch (
            timestamp_ms BIGINT NOT NULL,
            principal VARCHAR NOT NULL,
            workspace VARCHAR NOT NULL,
            event_kind VARCHAR NOT NULL,
            source VARCHAR NOT NULL,
            agent_id VARCHAR,
            goal_id VARCHAR,
            scope VARCHAR,
            tier_name VARCHAR,
            item_key VARCHAR,
            selected BOOLEAN,
            score INTEGER,
            confidence DOUBLE,
            query_excerpt VARCHAR,
            max_entries INTEGER,
            max_chars INTEGER,
            candidate_count INTEGER,
            selected_count INTEGER,
            dropped_count INTEGER,
            output_chars INTEGER,
            rule_name VARCHAR,
            target VARCHAR,
            source_kind VARCHAR,
            input_count INTEGER,
            output_count INTEGER,
            skipped_count INTEGER,
            eval_suite VARCHAR,
            eval_case_id VARCHAR,
            eval_query VARCHAR,
            eval_pass BOOLEAN,
            expected_count INTEGER,
            matched_count INTEGER,
            best_rank INTEGER,
            retrieval_backend VARCHAR,
            selected_item_keys VARCHAR,
            status VARCHAR NOT NULL,
            payload_json VARCHAR
        );
        "#,
    )
    .context("creating memory_events_batch temp table")?;

    {
        let mut app = conn
            .appender("memory_events_batch")
            .context("opening memory_events_batch appender")?;
        for row in rows {
            app.append_row(params![
                row.timestamp_ms,
                row.principal,
                row.workspace,
                row.event_kind,
                row.source,
                row.agent_id,
                row.goal_id,
                row.scope,
                row.tier_name,
                row.item_key,
                row.selected,
                row.score.map(|value| value as i32),
                row.confidence,
                row.query_excerpt,
                row.max_entries.map(|value| value as i32),
                row.max_chars.map(|value| value as i32),
                row.candidate_count.map(|value| value as i32),
                row.selected_count.map(|value| value as i32),
                row.dropped_count.map(|value| value as i32),
                row.output_chars.map(|value| value as i32),
                row.rule_name,
                row.target,
                row.source_kind,
                row.input_count.map(|value| value as i32),
                row.output_count.map(|value| value as i32),
                row.skipped_count.map(|value| value as i32),
                row.eval_suite,
                row.eval_case_id,
                row.eval_query,
                row.eval_pass,
                row.expected_count.map(|value| value as i32),
                row.matched_count.map(|value| value as i32),
                row.best_rank.map(|value| value as i32),
                row.retrieval_backend,
                row.selected_item_keys,
                row.status,
                row.payload_json,
            ])
            .context("appending row to memory_events_batch")?;
        }
        app.flush()
            .context("flushing memory_events_batch appender")?;
    }

    let path_sql = parquet_path.display().to_string().replace('\'', "''");
    let copy_sql =
        format!("COPY memory_events_batch TO '{path_sql}' (FORMAT PARQUET, COMPRESSION 'zstd');");
    conn.execute_batch(&copy_sql)
        .with_context(|| format!("copying memory_events_batch to {}", parquet_path.display()))?;
    crate::magician_v2::dataset_owners::publish_written_parquet(&parquet_path).with_context(
        || {
            format!(
                "publishing memory_events parquet {}",
                parquet_path.display()
            )
        },
    )?;

    debug!(
        target: "analytics::memory_parquet",
        path = %parquet_path.display(),
        rows = rows.len(),
        "wrote memory analytics Parquet batch"
    );
    Ok(())
}

fn partition_date_for(timestamp_ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(timestamp_ms)
        .unwrap_or_else(Utc::now)
        .format("%Y-%m-%d")
        .to_string()
}

pub fn json_payload(value: &Value) -> Option<String> {
    serde_json::to_string(value).ok()
}

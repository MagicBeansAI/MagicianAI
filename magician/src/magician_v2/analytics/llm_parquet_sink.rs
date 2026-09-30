//! LLM-call Parquet sink — lakehouse persistence for `LLMResponseReceived` events.
//!
//! Subscribes to the runtime broadcaster, filters to `LLMResponseReceived`
//! envelopes, buffers rows per-(principal, workspace) scope, flushes every
//! 30s or 100 rows (whichever fires first) to date-partitioned Parquet files:
//!
//! ```text
//! <scope_root>/analytics/llm_calls/dt=YYYY-MM-DD/batch_<ulid>.parquet
//! ```
//!
//! DuckDB's `COPY ... TO ... (FORMAT PARQUET, COMPRESSION 'zstd')` produces
//! standard Parquet files readable by any analytics tool. Schema documented in
//! [`docs/plans/2026-05-12-llm-calls-lakehouse.md`](../../../../../../docs/plans/2026-05-12-llm-calls-lakehouse.md).
//!
//! The sink is a tokio task spawned from `bin/magician.rs` startup. It owns a
//! `JoinHandle` so shutdown can `await` a final flush before process exit.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use duckdb::{params, Connection};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::magician_v2::analytics::duckdb_safety::{
    analytics_duckdb_guard, configure_analytics_connection_checked,
};
use crate::magician_v2::analytics::llm_scoped_path::ensure_real_scoped_directory_chain;
use crate::magician_v2::analytics::llm_trace_recorder::is_content_free_machine_category;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent};

/// Flush triggers (whichever fires first).
const FLUSH_INTERVAL: Duration = Duration::from_secs(30);
const FLUSH_ROW_THRESHOLD: usize = 100;

/// One row in the Parquet sink, mirrors the schema in the lakehouse plan.
#[derive(Debug, Clone)]
struct LlmCallRow {
    timestamp_ms: i64,
    started_at_ms: i64,
    latency_ms: u64,
    principal: String,
    workspace: String,
    schema_version: u16,
    trace_id: Option<String>,
    llm_call_id: Option<String>,
    provider_attempt_id: Option<String>,
    dispatch_job_id: Option<String>,
    parent_call_id: Option<String>,
    parent_relation: Option<String>,
    retry_group_id: Option<String>,
    route_decision_id: Option<String>,
    scope_resolution: Option<String>,
    root_execution_id: Option<String>,
    iteration_id: Option<String>,
    prompt_projection_mode: Option<String>,
    chat_turn_id: Option<String>,
    workload_class: Option<String>,
    call_role: Option<String>,
    provider_attempt_count: u32,
    response_reused: bool,
    execution_id: String,
    task_id: Option<String>,
    plan_id: Option<String>,
    step_id: Option<String>,
    step_index: Option<i64>,
    agent_id: Option<String>,
    delegated_agent_id: Option<String>,
    chat_session_id: Option<String>,
    operation: String,
    profile: Option<String>,
    provider: String,
    model: String,
    capability: String,
    response_kind: String,
    attempt: u32,
    success: bool,
    error: Option<String>,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    reasoning_tokens: u32,
    reasoning_summary: Option<String>,
    cache_read_tokens: Option<u32>,
    cache_creation_tokens: Option<u32>,
    /// Realtime (voice) audio-modality token split; None for text calls.
    audio_input_tokens: Option<u32>,
    audio_output_tokens: Option<u32>,
    audio_cached_tokens: Option<u32>,
    /// Time-to-first-token in ms when the caller instrumented streaming.
    /// `None` for non-streaming calls (most agentic / autonomous-loop
    /// invocations) and for tool-call-only responses with no text.
    ttft_ms: Option<u64>,
    cost_usd: Option<f64>,
}

/// Public handle for spawning + shutting down the Parquet sink.
pub struct LlmParquetSink {
    handle: tokio::task::JoinHandle<()>,
    cancel: CancellationToken,
}

impl LlmParquetSink {
    /// Spawn the sink. Subscribes to the broadcaster immediately so events
    /// emitted between spawn and the first flush are not lost.
    pub fn spawn(
        broadcaster: Arc<RuntimeTransportBroadcaster>,
        workspace_layout: ArtifactV2Workspace,
    ) -> Self {
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let rx = broadcaster.subscribe();
        let handle = tokio::spawn(async move {
            run_sink(rx, workspace_layout, cancel_for_task).await;
        });
        Self { handle, cancel }
    }

    /// Cancel the sink and await its final flush. Call on shutdown so the
    /// last 30s of telemetry doesn't get lost.
    pub async fn shutdown(self) {
        self.cancel.cancel();
        if let Err(err) = self.handle.await {
            warn!(
                target: "analytics::llm_parquet_sink",
                error = %err,
                "llm_parquet_sink join failed on shutdown"
            );
        }
    }
}

async fn run_sink(
    mut rx: broadcast::Receiver<RuntimeTransportEvent>,
    workspace_layout: ArtifactV2Workspace,
    cancel: CancellationToken,
) {
    // Per-scope buffer. Each (principal, workspace) flushes to its own directory.
    let mut buffers: HashMap<(String, String), Vec<LlmCallRow>> = HashMap::new();

    // Channel that carries flush jobs to a blocking-pool worker. DuckDB write
    // is synchronous and CPU-bound; running it on a tokio worker would stall
    // the broadcaster receiver during long batches. Use a small bounded queue
    // — if flush workers fall behind, we'd rather drop a flush than block the
    // broadcaster.
    let (flush_tx, flush_rx) = mpsc::channel::<FlushJob>(8);
    let workspace_for_worker = workspace_layout.clone();
    let worker = tokio::task::spawn_blocking(move || flush_worker(workspace_for_worker, flush_rx));

    let mut flush_timer = tokio::time::interval(FLUSH_INTERVAL);
    flush_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            event = rx.recv() => match event {
                Ok(event) => {
                    if let Some(row) = row_from_event(&event) {
                        let key = (row.principal.clone(), row.workspace.clone());
                        let entry = buffers.entry(key).or_default();
                        entry.push(row);
                        if entry.len() >= FLUSH_ROW_THRESHOLD {
                            let key2 = entry[0].principal.clone();
                            let key3 = entry[0].workspace.clone();
                            let rows = std::mem::take(entry);
                            try_send_flush(&flush_tx, FlushJob { principal: key2, workspace: key3, rows });
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(
                        target: "analytics::llm_parquet_sink",
                        skipped,
                        "broadcaster lagged; dropped {skipped} events from Parquet sink"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = flush_timer.tick() => {
                for ((principal, workspace), rows) in buffers.iter_mut() {
                    if rows.is_empty() {
                        continue;
                    }
                    let rows = std::mem::take(rows);
                    try_send_flush(&flush_tx, FlushJob {
                        principal: principal.clone(),
                        workspace: workspace.clone(),
                        rows,
                    });
                }
            }
            _ = cancel.cancelled() => break,
        }
    }

    // Final flush — drain everything before dropping the channel sender.
    for ((principal, workspace), rows) in buffers.into_iter() {
        if rows.is_empty() {
            continue;
        }
        try_send_flush(
            &flush_tx,
            FlushJob {
                principal,
                workspace,
                rows,
            },
        );
    }
    drop(flush_tx);
    if let Err(err) = worker.await {
        warn!(
            target: "analytics::llm_parquet_sink",
            error = %err,
            "flush worker join failed"
        );
    }
}

struct FlushJob {
    principal: String,
    workspace: String,
    rows: Vec<LlmCallRow>,
}

fn try_send_flush(tx: &mpsc::Sender<FlushJob>, job: FlushJob) {
    if let Err(err) = tx.try_send(job) {
        warn!(
            target: "analytics::llm_parquet_sink",
            error = %err,
            "flush queue saturated; dropping batch"
        );
    }
}

fn flush_worker(workspace_layout: ArtifactV2Workspace, mut rx: mpsc::Receiver<FlushJob>) {
    while let Some(job) = rx.blocking_recv() {
        if let Err(err) = write_batch(&workspace_layout, &job) {
            warn!(
                target: "analytics::llm_parquet_sink",
                principal = %job.principal,
                workspace = %job.workspace,
                rows = job.rows.len(),
                error = %err,
                "llm_parquet_sink write_batch failed"
            );
        }
    }
}

fn write_batch(workspace_layout: &ArtifactV2Workspace, job: &FlushJob) -> Result<()> {
    if job.rows.is_empty() {
        return Ok(());
    }
    let partition_dt = partition_date_for(job.rows[0].timestamp_ms);
    let partition_dir = workspace_layout
        .analytics_llm_calls_root(&job.principal, &job.workspace)
        .join(format!("dt={partition_dt}"));
    ensure_real_scoped_directory_chain(workspace_layout.base_root(), &partition_dir)?;
    std::fs::create_dir_all(&partition_dir)
        .with_context(|| format!("creating partition dir {}", partition_dir.display()))?;
    ensure_real_scoped_directory_chain(workspace_layout.base_root(), &partition_dir)?;

    let batch_id = ulid::Ulid::new().to_string();
    let parquet_path = partition_dir.join(format!("batch_{batch_id}.parquet"));

    // In-memory DuckDB. Single connection. Build a temp table, append rows,
    // COPY to Parquet, drop temp table.
    let _duckdb_guard = analytics_duckdb_guard();
    let conn =
        Connection::open_in_memory().context("opening in-memory DuckDB for parquet write")?;
    configure_analytics_connection_checked(&conn, "llm_calls_parquet_write")
        .context("configuring conservative DuckDB limits for llm_calls parquet write")?;
    conn.execute_batch(
        r#"
        CREATE TABLE llm_calls_batch (
            timestamp_ms BIGINT NOT NULL,
            started_at_ms BIGINT NOT NULL,
            latency_ms BIGINT NOT NULL,
            principal VARCHAR NOT NULL,
            workspace VARCHAR NOT NULL,
            schema_version INTEGER NOT NULL,
            trace_id VARCHAR,
            llm_call_id VARCHAR,
            provider_attempt_id VARCHAR,
            dispatch_job_id VARCHAR,
            parent_call_id VARCHAR,
            parent_relation VARCHAR,
            retry_group_id VARCHAR,
            route_decision_id VARCHAR,
            scope_resolution VARCHAR,
            root_execution_id VARCHAR,
            iteration_id VARCHAR,
            prompt_projection_mode VARCHAR,
            chat_turn_id VARCHAR,
            workload_class VARCHAR,
            call_role VARCHAR,
            provider_attempt_count INTEGER NOT NULL,
            response_reused BOOLEAN NOT NULL,
            execution_id VARCHAR NOT NULL,
            task_id VARCHAR,
            plan_id VARCHAR,
            step_id VARCHAR,
            step_index BIGINT,
            agent_id VARCHAR,
            delegated_agent_id VARCHAR,
            chat_session_id VARCHAR,
            operation VARCHAR NOT NULL,
            profile VARCHAR,
            provider VARCHAR NOT NULL,
            model VARCHAR NOT NULL,
            capability VARCHAR NOT NULL,
            response_kind VARCHAR NOT NULL,
            attempt INTEGER NOT NULL,
            success BOOLEAN NOT NULL,
            error VARCHAR,
            input_tokens INTEGER,
            output_tokens INTEGER,
            reasoning_tokens INTEGER NOT NULL,
            reasoning_summary VARCHAR,
            cache_read_tokens INTEGER,
            cache_creation_tokens INTEGER,
            audio_input_tokens INTEGER,
            audio_output_tokens INTEGER,
            audio_cached_tokens INTEGER,
            ttft_ms BIGINT,
            cost_usd DOUBLE
        );
        "#,
    )
    .context("creating llm_calls_batch temp table")?;

    // Use the appender API — the fastest row-insert path in DuckDB.
    {
        let mut app = conn
            .appender("llm_calls_batch")
            .context("opening llm_calls_batch appender")?;
        for row in &job.rows {
            app.append_row(params![
                row.timestamp_ms,
                row.started_at_ms,
                row.latency_ms as i64,
                row.principal,
                row.workspace,
                row.schema_version as i32,
                row.trace_id,
                row.llm_call_id,
                row.provider_attempt_id,
                row.dispatch_job_id,
                row.parent_call_id,
                row.parent_relation,
                row.retry_group_id,
                row.route_decision_id,
                row.scope_resolution,
                row.root_execution_id,
                row.iteration_id,
                row.prompt_projection_mode,
                row.chat_turn_id,
                row.workload_class,
                row.call_role,
                row.provider_attempt_count as i32,
                row.response_reused,
                row.execution_id,
                row.task_id,
                row.plan_id,
                row.step_id,
                row.step_index,
                row.agent_id,
                row.delegated_agent_id,
                row.chat_session_id,
                row.operation,
                row.profile,
                row.provider,
                row.model,
                row.capability,
                row.response_kind,
                row.attempt as i32,
                row.success,
                row.error,
                row.input_tokens.map(|v| v as i32),
                row.output_tokens.map(|v| v as i32),
                row.reasoning_tokens as i32,
                row.reasoning_summary.clone(),
                row.cache_read_tokens.map(|v| v as i32),
                row.cache_creation_tokens.map(|v| v as i32),
                row.audio_input_tokens.map(|v| v as i32),
                row.audio_output_tokens.map(|v| v as i32),
                row.audio_cached_tokens.map(|v| v as i32),
                row.ttft_ms.map(|v| v as i64),
                row.cost_usd,
            ])
            .context("appending row to llm_calls_batch")?;
        }
        app.flush().context("flushing llm_calls_batch appender")?;
    }

    let path_sql = parquet_path.display().to_string().replace('\'', "''");
    let copy_sql = format!(
        "COPY llm_calls_batch TO '{}' (FORMAT PARQUET, COMPRESSION 'zstd');",
        path_sql
    );
    conn.execute_batch(&copy_sql)
        .with_context(|| format!("copying llm_calls_batch to {}", parquet_path.display()))?;
    crate::magician_v2::dataset_owners::publish_written_parquet(&parquet_path)
        .with_context(|| format!("publishing llm_calls parquet {}", parquet_path.display()))?;

    debug!(
        target: "analytics::llm_parquet_sink",
        path = %parquet_path.display(),
        rows = job.rows.len(),
        "wrote Parquet batch"
    );

    Ok(())
}

fn partition_date_for(timestamp_ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(timestamp_ms)
        .unwrap_or_else(Utc::now)
        .format("%Y-%m-%d")
        .to_string()
}

/// Retention sweep — drop date partitions older than `retention_days` across
/// every (principal, workspace) scope under the storage root.
///
/// Spawn this as a long-running tokio task at startup. Ticks every 6h.
/// Files older than the cutoff are deleted directly with `std::fs::remove_dir_all`
/// — no DB ops needed because the partitions are pure Parquet files.
pub struct LlmParquetRetention {
    handle: tokio::task::JoinHandle<()>,
    cancel: CancellationToken,
}

impl LlmParquetRetention {
    pub fn spawn(workspace_layout: ArtifactV2Workspace, retention_days: u32) -> Self {
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let handle = tokio::spawn(async move {
            run_retention(workspace_layout, retention_days, cancel_for_task).await;
        });
        Self { handle, cancel }
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = self.handle.await;
    }
}

async fn run_retention(
    workspace_layout: ArtifactV2Workspace,
    retention_days: u32,
    cancel: CancellationToken,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(6 * 60 * 60));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = interval.tick() => {
                let layout = workspace_layout.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    if let Err(err) = sweep_old_partitions(&layout, retention_days) {
                        warn!(
                            target: "analytics::llm_parquet_sink",
                            error = %err,
                            "llm_parquet retention sweep failed"
                        );
                    }
                })
                .await;
            }
            _ = cancel.cancelled() => break,
        }
    }
}

fn sweep_old_partitions(workspace_layout: &ArtifactV2Workspace, retention_days: u32) -> Result<()> {
    let cutoff = Utc::now() - chrono::Duration::days(retention_days as i64);
    let cutoff_date = cutoff.date_naive();
    let scopes_root = workspace_layout.scopes_root();
    if !scopes_root.exists() {
        return Ok(());
    }
    // Layout: <storage_root>/scopes/<principal>/<workspace>/analytics/llm_calls/dt=YYYY-MM-DD/
    let principals = match std::fs::read_dir(&scopes_root) {
        Ok(dir) => dir,
        Err(_) => return Ok(()),
    };
    for principal in principals.flatten() {
        if !principal
            .file_type()
            .is_ok_and(|file_type| file_type.is_dir())
        {
            continue;
        }
        let principal_path = principal.path();
        let workspaces = match std::fs::read_dir(&principal_path) {
            Ok(dir) => dir,
            Err(_) => continue,
        };
        for workspace in workspaces.flatten() {
            if !workspace
                .file_type()
                .is_ok_and(|file_type| file_type.is_dir())
            {
                continue;
            }
            let workspace_path = workspace.path();
            // Sweep every Parquet lakehouse stream's date partitions. The
            // canonical call/attempt/gap datasets share the same policy as the
            // legacy compatibility call mirror and the `events` rebuild
            // source. The durable journal is intentionally excluded: only its
            // own committed-segment compactor may remove replay state.
            let analytics_dir = workspace_path.join("analytics");
            for stream in [
                "llm_calls",
                "llm_provider_attempts",
                "llm_tool_calls",
                "llm_capture_gaps",
                "events",
            ] {
                let stream_dir = analytics_dir.join(stream);
                if ensure_real_scoped_directory_chain(workspace_layout.base_root(), &stream_dir)
                    .is_ok()
                    && std::fs::symlink_metadata(&stream_dir)
                        .is_ok_and(|metadata| metadata.file_type().is_dir())
                {
                    sweep_one_scope(&stream_dir, cutoff_date);
                }
            }
        }
    }
    Ok(())
}

fn sweep_one_scope(partition_root: &PathBuf, cutoff_date: NaiveDate) {
    let entries = match std::fs::read_dir(partition_root) {
        Ok(d) => d,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
            continue;
        }
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        let Some(date_str) = name.strip_prefix("dt=") else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d") else {
            warn!(
                target: "analytics::llm_parquet_sink",
                path = %path.display(),
                "llm_parquet retention: ignored malformed date partition"
            );
            continue;
        };
        if date < cutoff_date {
            if let Err(err) = std::fs::remove_dir_all(&path) {
                warn!(
                    target: "analytics::llm_parquet_sink",
                    path = %path.display(),
                    error = %err,
                    "llm_parquet retention: remove_dir_all failed"
                );
            } else {
                debug!(
                    target: "analytics::llm_parquet_sink",
                    path = %path.display(),
                    cutoff = %cutoff_date,
                    "llm_parquet retention: dropped old partition"
                );
            }
        }
    }
}

fn row_from_event(event: &RuntimeTransportEvent) -> Option<LlmCallRow> {
    match event {
        RuntimeTransportEvent::LLMResponseReceived {
            execution_id,
            principal,
            workspace,
            correlation,
            plan_id,
            step_id,
            step_index,
            capability,
            success,
            cost,
            latency_ms,
            error,
            provider,
            model,
            input_tokens,
            output_tokens,
            reasoning_tokens,
            reasoning_summary: _reasoning_summary,
            cache_read_tokens,
            cache_creation_tokens,
            audio_input_tokens,
            audio_output_tokens,
            audio_cached_tokens,
            ttft_ms,
            task_id,
            agent_id,
            delegated_agent_id,
            chat_session_id,
            operation,
            profile,
            attempt,
            response_kind,
            started_at_ms,
            timestamp,
            ..
        } => {
            // Scope is required for partitioning. Drop unscoped emits — they
            // would land in an ambiguous bucket and pollute query results.
            let principal = principal.clone()?;
            let workspace = workspace.clone()?;
            let availability = correlation
                .as_ref()
                .and_then(|value| value.usage_availability);
            Some(LlmCallRow {
                timestamp_ms: *timestamp,
                started_at_ms: *started_at_ms,
                latency_ms: *latency_ms,
                principal,
                workspace,
                schema_version: correlation.as_ref().map_or(0, |value| value.schema_version),
                trace_id: correlation.as_ref().map(|value| value.trace_id.clone()),
                llm_call_id: correlation.as_ref().map(|value| value.llm_call_id.clone()),
                provider_attempt_id: correlation
                    .as_ref()
                    .and_then(|value| value.provider_attempt_id.clone()),
                dispatch_job_id: correlation
                    .as_ref()
                    .and_then(|value| value.dispatch_job_id.clone()),
                parent_call_id: correlation
                    .as_ref()
                    .and_then(|value| value.parent_call_id.clone()),
                parent_relation: correlation
                    .as_ref()
                    .and_then(|value| value.parent_relation.clone()),
                retry_group_id: correlation
                    .as_ref()
                    .and_then(|value| value.retry_group_id.clone()),
                route_decision_id: correlation
                    .as_ref()
                    .and_then(|value| value.route_decision_id.clone()),
                scope_resolution: correlation
                    .as_ref()
                    .map(|value| value.scope_resolution.clone()),
                root_execution_id: correlation
                    .as_ref()
                    .and_then(|value| value.root_execution_id.clone()),
                iteration_id: correlation
                    .as_ref()
                    .and_then(|value| value.iteration_id.clone()),
                prompt_projection_mode: correlation
                    .as_ref()
                    .and_then(|value| value.prompt_projection_mode.clone()),
                chat_turn_id: correlation
                    .as_ref()
                    .and_then(|value| value.chat_turn_id.clone()),
                workload_class: correlation
                    .as_ref()
                    .map(|value| value.workload_class.clone()),
                call_role: correlation.as_ref().map(|value| value.call_role.clone()),
                provider_attempt_count: correlation
                    .as_ref()
                    .map_or(0, |value| value.provider_attempt_count),
                response_reused: correlation
                    .as_ref()
                    .is_some_and(|value| value.response_reused),
                execution_id: execution_id.clone(),
                task_id: task_id.clone(),
                // The transport event predates typed optional lineage and uses
                // an empty string for "no plan". Persist absence as SQL NULL:
                // blank identifiers are neither joinable nor canonical.
                plan_id: (!plan_id.is_empty()).then(|| plan_id.clone()),
                step_id: step_id.clone(),
                step_index: step_index.map(|i| i as i64),
                agent_id: agent_id.clone(),
                delegated_agent_id: delegated_agent_id.clone(),
                chat_session_id: chat_session_id.clone(),
                operation: operation.clone(),
                profile: profile.clone(),
                provider: provider.clone(),
                model: model.clone(),
                capability: capability.clone(),
                response_kind: if is_content_free_machine_category(response_kind) {
                    response_kind.clone()
                } else {
                    "invalid_category_redacted".to_string()
                },
                attempt: *attempt,
                success: *success,
                // New compatibility rows must not extend the legacy raw-error
                // leak. Preserve only a fixed failure-presence marker;
                // canonical facts carry the typed error class.
                error: error.as_ref().map(|_| "legacy_error_redacted".to_string()),
                input_tokens: availability
                    .map_or(true, |value| value.tokens)
                    .then_some(*input_tokens),
                output_tokens: availability
                    .map_or(true, |value| value.tokens)
                    .then_some(*output_tokens),
                reasoning_tokens: *reasoning_tokens,
                // Provider reasoning is restricted content owned by Phase 3,
                // never an ordinary Phase 1/2 compatibility fact.
                reasoning_summary: None,
                cache_read_tokens: availability
                    .map_or(true, |value| value.cache_read)
                    .then_some(*cache_read_tokens),
                cache_creation_tokens: availability
                    .map_or(true, |value| value.cache_write)
                    .then_some(*cache_creation_tokens),
                audio_input_tokens: *audio_input_tokens,
                audio_output_tokens: *audio_output_tokens,
                audio_cached_tokens: *audio_cached_tokens,
                ttft_ms: *ttft_ms,
                cost_usd: (availability.map_or(true, |value| value.cost)
                    && cost.is_finite()
                    && *cost >= 0.0)
                    .then_some(*cost),
            })
        },
        // Request-start events intentionally do not belong to this temporary
        // compatibility mirror. The canonical lifecycle recorder owns starts,
        // attempts, completion and explicit capture-gap evidence.
        _ => None,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    /// A minimal `LLMResponseReceived` — `audio` is `Some((in, out, cached))` for
    /// realtime rows, `None` for text; `reasoning` sets the reasoning summary.
    fn llm_event(
        execution_id: &str,
        model: &str,
        reasoning: Option<&str>,
        audio: Option<(u32, u32, u32)>,
    ) -> RuntimeTransportEvent {
        RuntimeTransportEvent::LLMResponseReceived {
            execution_id: execution_id.to_string(),
            principal: Some("p".to_string()),
            workspace: Some("w".to_string()),
            correlation: Some(
                crate::magician_v2::realtime_events::LlmEventCorrelation::direct(
                    "p",
                    "w",
                    magicllm::LlmWorkloadClass::ForegroundChat,
                ),
            ),
            plan_id: String::new(),
            step_id: None,
            step_index: None,
            capability: "chat.inline".to_string(),
            success: true,
            decision_summary: String::new(),
            cost: 1.5,
            latency_ms: 120,
            error: None,
            provider: "openai".to_string(),
            model: model.to_string(),
            usage_reported: true,
            input_tokens: 1000,
            output_tokens: 500,
            reasoning_tokens: 42,
            reasoning_summary: reasoning.map(str::to_string),
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            audio_input_tokens: audio.map(|a| a.0),
            audio_output_tokens: audio.map(|a| a.1),
            audio_cached_tokens: audio.map(|a| a.2),
            search_calls: 0,
            ttft_ms: Some(30),
            task_id: None,
            agent_id: None,
            delegated_agent_id: None,
            chat_session_id: None,
            operation: "chat_completion".to_string(),
            profile: None,
            attempt: 1,
            response_kind: "text".to_string(),
            started_at_ms: 1_700_000_000_000,
            timestamp: 1_700_000_000_000,
        }
    }

    #[test]
    fn phase1_call_row_maps_identity_audio_and_redacts_restricted_content() {
        let mut realtime_event = llm_event("e1", "gpt-realtime-2.1", None, Some((1000, 500, 100)));
        if let RuntimeTransportEvent::LLMResponseReceived { correlation, .. } = &mut realtime_event
        {
            correlation
                .as_mut()
                .expect("correlation")
                .prompt_projection_mode = Some("rebootstrap".to_string());
        }
        let realtime = row_from_event(&realtime_event).expect("scoped realtime event yields a row");
        assert_eq!(realtime.audio_input_tokens, Some(1000));
        assert_eq!(
            realtime.prompt_projection_mode.as_deref(),
            Some("rebootstrap")
        );
        assert_eq!(realtime.audio_output_tokens, Some(500));
        assert_eq!(realtime.audio_cached_tokens, Some(100));
        assert_eq!(realtime.reasoning_summary, None);
        assert_eq!(realtime.schema_version, 1);
        assert!(realtime
            .trace_id
            .as_deref()
            .is_some_and(|value| !value.is_empty()));
        assert!(realtime
            .llm_call_id
            .as_deref()
            .is_some_and(|value| !value.is_empty()));
        assert_eq!(realtime.scope_resolution.as_deref(), Some("explicit"));
        assert_eq!(realtime.workload_class.as_deref(), Some("foreground_chat"));
        assert_eq!(realtime.provider_attempt_count, 1);
        assert_eq!(realtime.plan_id, None);

        let chat = row_from_event(&llm_event("e2", "gpt-5.6-sol", Some("reasoned"), None))
            .expect("scoped chat event yields a row");
        assert_eq!(chat.audio_input_tokens, None);
        assert_eq!(chat.reasoning_summary, None);

        let mut failed = llm_event("e3", "gpt-5.6-sol", None, None);
        if let RuntimeTransportEvent::LLMResponseReceived { success, error, .. } = &mut failed {
            *success = false;
            *error = Some("private provider response".to_string());
        }
        let failed = row_from_event(&failed).expect("scoped failed event yields a row");
        assert_eq!(failed.error.as_deref(), Some("legacy_error_redacted"));

        let mut malformed = llm_event("e4", "gpt-5.6-sol", None, None);
        if let RuntimeTransportEvent::LLMResponseReceived { response_kind, .. } = &mut malformed {
            *response_kind = "private user text".to_string();
        }
        let malformed = row_from_event(&malformed).expect("scoped malformed event yields a row");
        assert_eq!(malformed.response_kind, "invalid_category_redacted");
    }

    #[test]
    fn phase1_write_batch_round_trips_identity_audio_without_restricted_content() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = ArtifactV2Workspace::new(tmp.path());
        let rows = vec![
            row_from_event(&llm_event(
                "e1",
                "gpt-realtime-2.1",
                None,
                Some((1000, 500, 100)),
            ))
            .unwrap(),
            row_from_event(&llm_event("e2", "gpt-5.6-sol", Some("reasoned"), None)).unwrap(),
        ];
        write_batch(
            &ws,
            &FlushJob {
                principal: "p".to_string(),
                workspace: "w".to_string(),
                rows,
            },
        )
        .expect("write_batch");

        let root = ws.analytics_llm_calls_root("p", "w");
        let glob = crate::magician_v2::dataset_owners::family_read_glob(
            &root,
            crate::magician_v2::dataset_owners::DatasetFamily::LlmCalls,
        );
        let conn = Connection::open_in_memory().unwrap();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT execution_id, reasoning_summary, audio_input_tokens, \
                 audio_output_tokens, audio_cached_tokens \
                 FROM read_parquet('{glob}', union_by_name = true) ORDER BY execution_id"
            ))
            .expect("prepare");
        let out: Vec<(
            String,
            Option<String>,
            Option<i32>,
            Option<i32>,
            Option<i32>,
        )> = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(out.len(), 2);
        // e1 realtime: audio split persisted, reasoning NULL.
        assert_eq!(out[0].0, "e1");
        assert_eq!(out[0].1, None);
        assert_eq!(out[0].2, Some(1000));
        assert_eq!(out[0].3, Some(500));
        assert_eq!(out[0].4, Some(100));
        // e2 chat: restricted reasoning omitted, audio NULL.
        assert_eq!(out[1].0, "e2");
        assert_eq!(out[1].1, None);
        assert_eq!(out[1].2, None);

        let phase1_rows: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM read_parquet('{glob}', union_by_name = true) \
                     WHERE schema_version = 1 AND trace_id IS NOT NULL \
                       AND llm_call_id IS NOT NULL AND provider_attempt_count = 1"
                ),
                [],
                |row| row.get(0),
            )
            .expect("query phase 1 identity columns");
        assert_eq!(phase1_rows, 2);

        let blank_plan_ids: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM read_parquet('{glob}', union_by_name = true) \
                     WHERE plan_id IS NOT NULL AND trim(plan_id) = ''"
                ),
                [],
                |row| row.get(0),
            )
            .expect("query blank plan identities");
        assert_eq!(blank_plan_ids, 0);
    }

    #[test]
    fn terminal_usage_legacy_parquet_keeps_unknown_harness_buckets_null() {
        let mut event = llm_event("harness", "model", None, None);
        if let RuntimeTransportEvent::LLMResponseReceived {
            correlation, cost, ..
        } = &mut event
        {
            correlation.as_mut().unwrap().usage_availability =
                Some(magicllm::types::UsageAvailability {
                    tokens: true,
                    cache_read: false,
                    cache_write: true,
                    cost: false,
                });
            *cost = 0.0;
        }
        let row = row_from_event(&event).unwrap();
        assert_eq!(row.input_tokens, Some(1000));
        assert_eq!(row.cache_read_tokens, None);
        assert_eq!(row.cache_creation_tokens, Some(0));
        assert_eq!(row.cost_usd, None);
        let temp = tempfile::tempdir().unwrap();
        let ws = ArtifactV2Workspace::new(temp.path());
        write_batch(
            &ws,
            &FlushJob {
                principal: "p".into(),
                workspace: "w".into(),
                rows: vec![row],
            },
        )
        .unwrap();
        let glob = crate::magician_v2::dataset_owners::family_read_glob(
            &ws.analytics_llm_calls_root("p", "w"),
            crate::magician_v2::dataset_owners::DatasetFamily::LlmCalls,
        );
        let conn = Connection::open_in_memory().unwrap();
        let values: (Option<i32>, Option<i32>, Option<i32>, Option<f64>) = conn.query_row(
            &format!("SELECT input_tokens, cache_read_tokens, cache_creation_tokens, cost_usd FROM read_parquet('{glob}')"),
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).unwrap();
        assert_eq!(values, (Some(1000), None, Some(0), None));
        if let RuntimeTransportEvent::LLMResponseReceived { correlation, .. } = &mut event {
            correlation.as_mut().unwrap().usage_availability =
                Some(magicllm::types::UsageAvailability::default());
        }
        let unknown = row_from_event(&event).unwrap();
        assert_eq!(unknown.input_tokens, None);
        assert_eq!(unknown.output_tokens, None);
        assert_eq!(unknown.cache_creation_tokens, None);
    }

    #[test]
    fn retention_covers_canonical_fact_streams_but_never_journal_segments() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let analytics = workspace.analytics_root("owner", "default");
        let old = (Utc::now() - chrono::Duration::days(120))
            .format("%Y-%m-%d")
            .to_string();
        let current = Utc::now().format("%Y-%m-%d").to_string();
        for stream in [
            "llm_calls",
            "llm_provider_attempts",
            "llm_tool_calls",
            "llm_capture_gaps",
            "events",
        ] {
            std::fs::create_dir_all(analytics.join(stream).join(format!("dt={old}")))
                .expect("old partition");
            std::fs::create_dir_all(analytics.join(stream).join(format!("dt={current}")))
                .expect("current partition");
        }
        let journal_old = workspace
            .analytics_llm_trace_journal_root("owner", "default")
            .join(format!("dt={old}"));
        std::fs::create_dir_all(&journal_old).expect("journal sentinel");

        sweep_old_partitions(&workspace, 90).expect("retention sweep");
        for stream in [
            "llm_calls",
            "llm_provider_attempts",
            "llm_tool_calls",
            "llm_capture_gaps",
            "events",
        ] {
            assert!(!analytics.join(stream).join(format!("dt={old}")).exists());
            assert!(analytics
                .join(stream)
                .join(format!("dt={current}"))
                .exists());
        }
        assert!(
            journal_old.exists(),
            "date sweep must not remove journal replay state"
        );
    }

    #[cfg(unix)]
    #[test]
    fn retention_never_follows_scope_stream_or_partition_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let external = tempfile::tempdir().expect("external");
        let external_analytics = tempfile::tempdir().expect("external analytics");
        let sentinel = external.path().join("sentinel");
        std::fs::write(&sentinel, b"keep").expect("sentinel");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let stream = workspace.analytics_llm_calls_root("owner", "default");
        std::fs::create_dir_all(&stream).expect("stream");
        let old = (Utc::now() - chrono::Duration::days(120))
            .format("%Y-%m-%d")
            .to_string();
        symlink(external.path(), stream.join(format!("dt={old}"))).expect("partition symlink");
        let redirected_partition = external_analytics
            .path()
            .join("llm_calls")
            .join(format!("dt={old}"));
        std::fs::create_dir_all(&redirected_partition).expect("redirected partition");
        let redirected_sentinel = redirected_partition.join("sentinel");
        std::fs::write(&redirected_sentinel, b"keep").expect("redirected sentinel");
        let redirected_workspace = workspace
            .scopes_root()
            .join("owner-redirected")
            .join("default");
        std::fs::create_dir_all(&redirected_workspace).expect("redirected workspace");
        symlink(
            external_analytics.path(),
            redirected_workspace.join("analytics"),
        )
        .expect("analytics symlink");

        sweep_old_partitions(&workspace, 90).expect("retention sweep");
        assert!(sentinel.exists());
        assert!(redirected_sentinel.exists());
        assert!(stream.join(format!("dt={old}")).is_symlink());
    }
}

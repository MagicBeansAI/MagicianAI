//! LLM dispatch-queue telemetry sink — lakehouse persistence for the queue's
//! per-job terminal outcomes.
//!
//! Subscribes to the `LlmDispatchQueue` event bus (`subscribe_events()`),
//! filters to the three TERMINAL events (`Completed` / `Failed` / `Tombstoned`)
//! — each carries a full `JobMeta` with lane, provider, timing, tokens,
//! attempts, and (when present) local-prep stats — and persists one row per
//! outcome to date-partitioned Parquet:
//!
//! ```text
//! <scope>/analytics/llm_dispatch/dt=YYYY-MM-DD/batch_<ulid>.parquet
//! ```
//!
//! Mirrors [`super::llm_parquet_sink`] (the `LLMResponseReceived` Parquet sink)
//! but consumes the queue's own event stream so the rows reflect the *queue's*
//! view — lane assignment, retry attempts, tombstone reasons, queue-wait vs
//! provider-execution split, and local-prep savings — none of which the
//! response-level `LLMResponseReceived` telemetry sees. Dispatch telemetry is
//! system-wide (the queue is not per-workspace), but every job carries an
//! authoritative scope and is physically partitioned with that scope.
//!
//! Spawned from `bin/magician.rs` right after the queue starts. Flushes every
//! 30s or 200 rows, whichever fires first. Best-effort: a lost final batch on
//! shutdown costs at most ~30s of queue telemetry.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use duckdb::{params, Connection};
use magicllm::dispatch::{LlmQueueEvent, Priority, TombstoneReason};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::llm_scoped_path::ensure_real_scoped_directory_chain;

use crate::magician_v2::analytics::duckdb_safety::{
    analytics_duckdb_guard, configure_analytics_connection_checked,
};
use crate::magician_v2::artifact_v2::workspace::{
    ArtifactV2Workspace, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};

/// Flush triggers (whichever fires first).
const FLUSH_INTERVAL: Duration = Duration::from_secs(30);
const FLUSH_ROW_THRESHOLD: usize = 200;

/// One row per terminal job outcome. Mirrors the queryable `JobMeta` surface.
#[derive(Debug, Clone)]
struct DispatchRow {
    /// Partition / event time (completed → dispatched → submitted, first set).
    timestamp_ms: i64,
    job_id: String,
    trace_id: String,
    llm_call_id: String,
    provider_attempt_id: Option<String>,
    provider_attempt_count: u32,
    parent_call_id: Option<String>,
    parent_relation: Option<String>,
    retry_group_id: Option<String>,
    route_decision_id: Option<String>,
    /// Live runtime-activity row this job was submitted from — the join key
    /// back to the watchable view. `None` for a call made outside any
    /// instrumented span, which is a real case rather than missing data.
    activity_id: Option<String>,
    principal: String,
    workspace: String,
    scope_resolution: String,
    root_execution_id: Option<String>,
    execution_id: Option<String>,
    plan_id: Option<String>,
    step_id: Option<String>,
    iteration_id: Option<String>,
    chat_turn_id: Option<String>,
    workload_class: String,
    call_role: String,
    response_reused: bool,
    operation: String,
    caller: Option<String>,
    /// Lane: `high` / `normal` / `background`.
    priority: String,
    /// Terminal state: `completed` / `failed` / `tombstoned`.
    state: String,
    success: bool,
    profile: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    error: Option<String>,
    /// Error class name for `failed` rows (e.g. `rate_limit`, `timeout`).
    error_class: Option<String>,
    /// Tombstone reason label for `tombstoned` rows.
    tombstone_reason: Option<String>,
    /// Queue dwell — submit → worker pickup.
    wait_ms: Option<i64>,
    /// Provider-capacity dwell — worker pickup → semaphore admission.
    provider_wait_ms: Option<i64>,
    /// Provider execution — pickup → response.
    execution_ms: Option<i64>,
    attempts: u32,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    cached_tokens: Option<i64>,
    reasoning_tokens: Option<i64>,
    /// Local-prep stats (present only when summarisable blocks were processed).
    local_prep_blocks: Option<i64>,
    local_prep_chars_in: Option<i64>,
    local_prep_chars_out: Option<i64>,
    local_prep_ms: Option<i64>,
    local_prep_model: Option<String>,
    task_id: Option<String>,
    agent_id: Option<String>,
    chat_session_id: Option<String>,
    idempotency_key: Option<String>,
    submitted_at_ms: i64,
    dispatched_at_ms: Option<i64>,
    completed_at_ms: Option<i64>,
}

/// Public handle for spawning + shutting down the dispatch telemetry sink.
pub struct LlmDispatchTelemetrySink {
    handle: tokio::task::JoinHandle<()>,
    cancel: CancellationToken,
}

impl LlmDispatchTelemetrySink {
    /// Spawn the sink against a fresh queue-event subscription. Subscribe
    /// immediately (before the first await elsewhere) so events emitted between
    /// queue start and the first flush aren't missed.
    pub fn spawn(
        rx: broadcast::Receiver<LlmQueueEvent>,
        workspace_layout: ArtifactV2Workspace,
    ) -> Self {
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let handle = tokio::spawn(async move {
            run_sink(rx, workspace_layout, cancel_for_task).await;
        });
        Self { handle, cancel }
    }

    /// Cancel the sink and await its final flush.
    pub async fn shutdown(self) {
        self.cancel.cancel();
        if let Err(err) = self.handle.await {
            warn!(
                target: "analytics::llm_dispatch_rows",
                error = %err,
                "llm_dispatch telemetry sink join failed on shutdown"
            );
        }
    }
}

async fn run_sink(
    mut rx: broadcast::Receiver<LlmQueueEvent>,
    workspace_layout: ArtifactV2Workspace,
    cancel: CancellationToken,
) {
    let mut buffer: Vec<DispatchRow> = Vec::new();

    // DuckDB writes are synchronous + CPU-bound; offload to a blocking worker
    // via a small bounded channel so a long batch can't stall the event
    // receiver. Drop a flush over blocking the receiver if the worker lags.
    let (flush_tx, flush_rx) = tokio::sync::mpsc::channel::<Vec<DispatchRow>>(8);
    let workspace_for_worker = workspace_layout.clone();
    let worker = tokio::task::spawn_blocking(move || flush_worker(workspace_for_worker, flush_rx));

    let mut flush_timer = tokio::time::interval(FLUSH_INTERVAL);
    flush_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            event = rx.recv() => match event {
                Ok(event) => {
                    if let Some(row) = row_from_event(&event) {
                        buffer.push(row);
                        if buffer.len() >= FLUSH_ROW_THRESHOLD {
                            try_send_flush(&flush_tx, std::mem::take(&mut buffer));
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(
                        target: "analytics::llm_dispatch_rows",
                        skipped,
                        "queue event bus lagged; dropped {skipped} events from dispatch telemetry"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = flush_timer.tick() => {
                if !buffer.is_empty() {
                    try_send_flush(&flush_tx, std::mem::take(&mut buffer));
                }
            }
            _ = cancel.cancelled() => break,
        }
    }

    // Final flush before dropping the channel sender.
    if !buffer.is_empty() {
        try_send_flush(&flush_tx, buffer);
    }
    drop(flush_tx);
    if let Err(err) = worker.await {
        warn!(
            target: "analytics::llm_dispatch_rows",
            error = %err,
            "dispatch telemetry flush worker join failed"
        );
    }
}

fn try_send_flush(tx: &tokio::sync::mpsc::Sender<Vec<DispatchRow>>, rows: Vec<DispatchRow>) {
    if let Err(err) = tx.try_send(rows) {
        warn!(
            target: "analytics::llm_dispatch_rows",
            error = %err,
            "dispatch telemetry flush queue saturated; dropping batch"
        );
    }
}

fn flush_worker(
    workspace_layout: ArtifactV2Workspace,
    mut rx: tokio::sync::mpsc::Receiver<Vec<DispatchRow>>,
) {
    while let Some(rows) = rx.blocking_recv() {
        let mut scoped: BTreeMap<(String, String, String), Vec<DispatchRow>> = BTreeMap::new();
        for row in rows {
            let key = dispatch_partition_key(&row);
            scoped.entry(key).or_default().push(row);
        }
        for ((principal, workspace, partition_dt), rows) in scoped {
            if let Err(err) = write_batch(
                &workspace_layout,
                &principal,
                &workspace,
                &partition_dt,
                &rows,
            ) {
                warn!(
                    target: "analytics::llm_dispatch_rows",
                    principal,
                    workspace,
                    rows = rows.len(),
                    error = %err,
                    "llm_dispatch write_batch failed"
                );
            }
        }
    }
}

fn dispatch_partition_key(row: &DispatchRow) -> (String, String, String) {
    (
        row.principal.clone(),
        row.workspace.clone(),
        partition_date_for(row.timestamp_ms),
    )
}

fn write_batch(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    partition_dt: &str,
    rows: &[DispatchRow],
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let partition_dir = workspace_layout
        .analytics_root(principal, workspace)
        .join("llm_dispatch")
        .join(format!("dt={partition_dt}"));
    ensure_real_scoped_directory_chain(workspace_layout.base_root(), &partition_dir)?;
    std::fs::create_dir_all(&partition_dir)
        .with_context(|| format!("creating partition dir {}", partition_dir.display()))?;
    ensure_real_scoped_directory_chain(workspace_layout.base_root(), &partition_dir)?;

    let batch_id = ulid::Ulid::new().to_string();
    let parquet_path = partition_dir.join(format!("batch_{batch_id}.parquet"));

    let _duckdb_guard = analytics_duckdb_guard();
    let conn = Connection::open_in_memory()
        .context("opening in-memory DuckDB for dispatch telemetry write")?;
    configure_analytics_connection_checked(&conn, "llm_dispatch_parquet_write")
        .context("configuring conservative DuckDB limits for dispatch telemetry write")?;
    conn.execute_batch(
        r#"
        CREATE TABLE llm_dispatch_batch (
            timestamp_ms BIGINT NOT NULL,
            job_id VARCHAR NOT NULL,
            trace_id VARCHAR NOT NULL,
            llm_call_id VARCHAR NOT NULL,
            provider_attempt_id VARCHAR,
            provider_attempt_count INTEGER NOT NULL,
            parent_call_id VARCHAR,
            parent_relation VARCHAR,
            retry_group_id VARCHAR,
            route_decision_id VARCHAR,
            activity_id VARCHAR,
            principal VARCHAR NOT NULL,
            workspace VARCHAR NOT NULL,
            scope_resolution VARCHAR NOT NULL,
            root_execution_id VARCHAR,
            execution_id VARCHAR,
            plan_id VARCHAR,
            step_id VARCHAR,
            iteration_id VARCHAR,
            chat_turn_id VARCHAR,
            workload_class VARCHAR NOT NULL,
            call_role VARCHAR NOT NULL,
            response_reused BOOLEAN NOT NULL,
            operation VARCHAR NOT NULL,
            caller VARCHAR,
            priority VARCHAR NOT NULL,
            state VARCHAR NOT NULL,
            success BOOLEAN NOT NULL,
            profile VARCHAR,
            provider VARCHAR,
            model VARCHAR,
            error VARCHAR,
            error_class VARCHAR,
            tombstone_reason VARCHAR,
            wait_ms BIGINT,
            provider_wait_ms BIGINT,
            execution_ms BIGINT,
            attempts INTEGER NOT NULL,
            prompt_tokens BIGINT,
            completion_tokens BIGINT,
            cached_tokens BIGINT,
            reasoning_tokens BIGINT,
            local_prep_blocks BIGINT,
            local_prep_chars_in BIGINT,
            local_prep_chars_out BIGINT,
            local_prep_ms BIGINT,
            local_prep_model VARCHAR,
            task_id VARCHAR,
            agent_id VARCHAR,
            chat_session_id VARCHAR,
            idempotency_key VARCHAR,
            submitted_at_ms BIGINT NOT NULL,
            dispatched_at_ms BIGINT,
            completed_at_ms BIGINT
        );
        "#,
    )
    .context("creating llm_dispatch_batch temp table")?;

    {
        let mut app = conn
            .appender("llm_dispatch_batch")
            .context("opening llm_dispatch_batch appender")?;
        for row in rows {
            app.append_row(params![
                row.timestamp_ms,
                row.job_id,
                row.trace_id,
                row.llm_call_id,
                row.provider_attempt_id,
                row.provider_attempt_count as i32,
                row.parent_call_id,
                row.parent_relation,
                row.retry_group_id,
                row.route_decision_id,
                row.activity_id,
                row.principal,
                row.workspace,
                row.scope_resolution,
                row.root_execution_id,
                row.execution_id,
                row.plan_id,
                row.step_id,
                row.iteration_id,
                row.chat_turn_id,
                row.workload_class,
                row.call_role,
                row.response_reused,
                row.operation,
                row.caller,
                row.priority,
                row.state,
                row.success,
                row.profile,
                row.provider,
                row.model,
                row.error,
                row.error_class,
                row.tombstone_reason,
                row.wait_ms,
                row.provider_wait_ms,
                row.execution_ms,
                row.attempts as i32,
                row.prompt_tokens,
                row.completion_tokens,
                row.cached_tokens,
                row.reasoning_tokens,
                row.local_prep_blocks,
                row.local_prep_chars_in,
                row.local_prep_chars_out,
                row.local_prep_ms,
                row.local_prep_model,
                row.task_id,
                row.agent_id,
                row.chat_session_id,
                row.idempotency_key,
                row.submitted_at_ms,
                row.dispatched_at_ms,
                row.completed_at_ms,
            ])
            .context("appending row to llm_dispatch_batch")?;
        }
        app.flush()
            .context("flushing llm_dispatch_batch appender")?;
    }

    let path_sql = parquet_path.display().to_string().replace('\'', "''");
    let copy_sql = format!(
        "COPY llm_dispatch_batch TO '{}' (FORMAT PARQUET, COMPRESSION 'zstd');",
        path_sql
    );
    conn.execute_batch(&copy_sql)
        .with_context(|| format!("copying llm_dispatch_batch to {}", parquet_path.display()))?;
    crate::magician_v2::dataset_owners::publish_written_parquet(&parquet_path)
        .with_context(|| format!("publishing llm_dispatch parquet {}", parquet_path.display()))?;

    debug!(
        target: "analytics::llm_dispatch_rows",
        path = %parquet_path.display(),
        rows = rows.len(),
        "wrote dispatch telemetry Parquet batch"
    );

    Ok(())
}

fn partition_date_for(timestamp_ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(timestamp_ms)
        .unwrap_or_else(Utc::now)
        .format("%Y-%m-%d")
        .to_string()
}

/// Retention sweep — drop `dt=*` partitions older than `retention_days`.
/// Pure Parquet directories, so deletion is a plain `remove_dir_all`.
pub struct LlmDispatchRetention {
    handle: tokio::task::JoinHandle<()>,
    cancel: CancellationToken,
}

impl LlmDispatchRetention {
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
                            target: "analytics::llm_dispatch_rows",
                            error = %err,
                            "llm_dispatch retention sweep failed"
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
    let cutoff_str = cutoff.format("%Y-%m-%d").to_string();
    let mut scopes = workspace_layout.list_scopes();
    let default_scope = (
        DEFAULT_SCOPE_PRINCIPAL.to_string(),
        DEFAULT_SCOPE_WORKSPACE.to_string(),
    );
    if !scopes.contains(&default_scope) {
        scopes.push(default_scope);
    }
    for (principal, workspace) in scopes {
        let dispatch_dir = workspace_layout
            .analytics_root(&principal, &workspace)
            .join("llm_dispatch");
        if dispatch_dir.exists() {
            sweep_one_dir(&dispatch_dir, &cutoff_str);
        }
    }
    Ok(())
}

fn sweep_one_dir(dispatch_dir: &PathBuf, cutoff_str: &str) {
    let entries = match std::fs::read_dir(dispatch_dir) {
        Ok(d) => d,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(date_str) = name.strip_prefix("dt=") else {
            continue;
        };
        if date_str < cutoff_str {
            if let Err(err) = std::fs::remove_dir_all(&path) {
                warn!(
                    target: "analytics::llm_dispatch_rows",
                    path = %path.display(),
                    error = %err,
                    "llm_dispatch retention: remove_dir_all failed"
                );
            } else {
                debug!(
                    target: "analytics::llm_dispatch_rows",
                    path = %path.display(),
                    cutoff = cutoff_str,
                    "llm_dispatch retention: dropped old partition"
                );
            }
        }
    }
}

/// Stable lane label for the lane dimension.
fn priority_label(priority: Priority) -> &'static str {
    match priority {
        Priority::High => "high",
        Priority::Normal => "normal",
        Priority::Background => "background",
    }
}

/// Stable label for a tombstone reason (drops the inner free-text reason — that
/// goes in the `error` column when present).
fn tombstone_label(reason: &TombstoneReason) -> String {
    match reason {
        TombstoneReason::TaskCancelled { .. } => "task_cancelled",
        TombstoneReason::TaskCancelledInFlight { .. } => "task_cancelled_in_flight",
        TombstoneReason::TaskMissing => "task_missing",
        TombstoneReason::ChatSessionEnded { .. } => "chat_session_ended",
        TombstoneReason::ExplicitCancel { .. } => "explicit_cancel",
        TombstoneReason::QueueShutdown => "queue_shutdown",
        TombstoneReason::DeadlineExceeded => "deadline_exceeded",
        TombstoneReason::QueueFull => "queue_full",
        TombstoneReason::ProcessRestart => "process_restart",
    }
    .to_string()
}

/// Free-text reason carried on cancellation tombstones, surfaced in `error`.
fn tombstone_reason_text(reason: &TombstoneReason) -> Option<String> {
    match reason {
        TombstoneReason::TaskCancelled { reason }
        | TombstoneReason::TaskCancelledInFlight { reason }
        | TombstoneReason::ChatSessionEnded { reason }
        | TombstoneReason::ExplicitCancel { reason } => reason.clone(),
        _ => None,
    }
}

fn row_from_event(event: &LlmQueueEvent) -> Option<DispatchRow> {
    let (meta, state, success, error_class, tombstone_reason, tombstone_text) = match event {
        LlmQueueEvent::Completed { meta } => (meta, "completed", true, None, None, None),
        LlmQueueEvent::Failed { meta, error_class } => (
            meta,
            "failed",
            false,
            Some(error_class.name().to_string()),
            None,
            None,
        ),
        LlmQueueEvent::Tombstoned { meta, reason } => (
            meta,
            "tombstoned",
            false,
            None,
            Some(tombstone_label(reason)),
            tombstone_reason_text(reason),
        ),
        // Non-terminal / provider-state / local-prep-skip events aren't rows.
        _ => return None,
    };

    let timestamp_ms = meta
        .completed_at_ms
        .or(meta.dispatched_at_ms)
        .unwrap_or(meta.submitted_at_ms);

    let tokens = meta.tokens.as_ref();
    let local_prep = meta.local_prep.as_ref();
    let task_ref = meta.task_ref.as_ref();
    let trace = &meta.trace_context;

    Some(DispatchRow {
        timestamp_ms,
        job_id: meta.job_id.to_string(),
        trace_id: trace.trace_id.clone(),
        llm_call_id: trace.llm_call_id.clone(),
        provider_attempt_id: meta.provider_attempt_id.clone(),
        provider_attempt_count: meta.provider_attempt_count,
        parent_call_id: trace.parent_call_id.clone(),
        parent_relation: trace
            .parent_relation
            .map(|value| value.as_str().to_string()),
        retry_group_id: trace.retry_group_id.clone(),
        route_decision_id: trace.route_decision_id.clone(),
        // Stamped by the submit site, where the span was live. This sink runs
        // in its own task draining a broadcast channel, so the submitting
        // span is long out of scope by the time the row is built — the id has
        // to ride the job here rather than be read here.
        activity_id: meta.origin.activity_id.clone(),
        principal: trace.scope.principal.clone(),
        workspace: trace.scope.workspace.clone(),
        scope_resolution: trace.scope_resolution.as_str().to_string(),
        root_execution_id: trace.root_execution_id.clone(),
        execution_id: trace.execution_id.clone(),
        plan_id: trace.plan_id.clone(),
        step_id: trace.step_id.clone(),
        iteration_id: trace.iteration_id.clone(),
        chat_turn_id: trace.chat_turn_id.clone(),
        workload_class: trace.workload_class.as_str().to_string(),
        call_role: trace.call_role.as_str().to_string(),
        response_reused: meta.response_reused,
        operation: meta.origin.operation.clone(),
        caller: meta.origin.caller.clone(),
        priority: priority_label(meta.priority).to_string(),
        state: state.to_string(),
        success,
        profile: meta.profile.clone(),
        provider: meta.provider.as_ref().map(|p| format!("{p:?}")),
        model: meta.model.clone(),
        // Prefer an explicit provider error string; fall back to a cancellation
        // reason text on tombstones so the row isn't a bare label.
        error: meta.error.clone().or(tombstone_text),
        error_class,
        tombstone_reason,
        wait_ms: meta.wait_ms.map(|v| v as i64),
        provider_wait_ms: meta.provider_wait_ms.map(|v| v as i64),
        execution_ms: meta.execution_ms.map(|v| v as i64),
        attempts: meta.attempts,
        prompt_tokens: tokens.map(|t| t.prompt_tokens as i64),
        completion_tokens: tokens.map(|t| t.completion_tokens as i64),
        cached_tokens: tokens.map(|t| t.cached_tokens as i64),
        reasoning_tokens: tokens.map(|t| t.reasoning_tokens as i64),
        local_prep_blocks: local_prep.map(|s| s.blocks_processed as i64),
        local_prep_chars_in: local_prep.map(|s| s.chars_in as i64),
        local_prep_chars_out: local_prep.map(|s| s.chars_out as i64),
        local_prep_ms: local_prep.map(|s| s.duration_ms as i64),
        local_prep_model: local_prep.map(|s| s.model.clone()),
        task_id: task_ref.map(|t| t.task_id.clone()),
        agent_id: task_ref.and_then(|t| t.agent_id.clone()),
        chat_session_id: task_ref.and_then(|t| t.chat_session_id.clone()),
        idempotency_key: meta.idempotency_key.clone(),
        submitted_at_ms: meta.submitted_at_ms,
        dispatched_at_ms: meta.dispatched_at_ms,
        completed_at_ms: meta.completed_at_ms,
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use magicllm::dispatch::{JobId, JobMeta, JobOrigin, JobState, TaskRef};
    use magicllm::{LlmTraceContext, LlmWorkloadClass};

    fn base_meta(state: JobState) -> JobMeta {
        let trace_context = LlmTraceContext::legacy(None, LlmWorkloadClass::Memory);
        let provider_attempt_id = trace_context.provider_attempt_id(1);
        JobMeta {
            job_id: JobId("01HJOB".to_string()),
            trace_context,
            provider_attempt_id: Some(provider_attempt_id),
            provider_attempt_count: 1,
            response_reused: false,
            priority: Priority::Background,
            task_ref: Some(TaskRef {
                task_id: "exec-1".to_string(),
                agent_id: Some("agent-1".to_string()),
                chat_session_id: Some("sess-1".to_string()),
                scope: None,
                root_execution_id: Some("exec-1".to_string()),
                execution_id: Some("exec-1".to_string()),
                plan_id: None,
                step_id: None,
                chat_turn_id: None,
                iteration_id: None,
                user_message_id: None,
                survives_terminal_task: false,
            }),
            origin: JobOrigin {
                operation: "memory_entity_extraction".to_string(),
                caller: Some("consolidator".to_string()),
                activity_id: None,
            },
            profile: Some("memory-fast".to_string()),
            provider: None,
            model: Some("test-model".to_string()),
            state,
            submitted_at_ms: 1000,
            dispatched_at_ms: Some(1200),
            completed_at_ms: Some(1500),
            wait_ms: Some(200),
            provider_wait_ms: Some(125),
            execution_ms: Some(300),
            tokens: None,
            tombstone: None,
            error: None,
            error_class: None,
            attempts: 1,
            local_prep: None,
            idempotency_key: None,
        }
    }

    #[test]
    fn phase1_completed_event_maps_identity_and_lifecycle_to_row() {
        let row = row_from_event(&LlmQueueEvent::Completed {
            meta: base_meta(JobState::Completed),
        })
        .expect("completed -> row");
        assert_eq!(row.state, "completed");
        assert!(row.success);
        assert_eq!(row.priority, "background");
        assert_eq!(row.profile.as_deref(), Some("memory-fast"));
        assert_eq!(row.operation, "memory_entity_extraction");
        assert_eq!(row.task_id.as_deref(), Some("exec-1"));
        assert_eq!(row.chat_session_id.as_deref(), Some("sess-1"));
        assert_eq!(row.timestamp_ms, 1500); // completed_at wins
        assert_eq!(row.wait_ms, Some(200));
        assert_eq!(row.provider_wait_ms, Some(125));
        assert!(!row.llm_call_id.is_empty());
        let expected_attempt_id = format!("{}:a1", row.llm_call_id);
        assert_eq!(
            row.provider_attempt_id.as_deref(),
            Some(expected_attempt_id.as_str())
        );
    }

    #[test]
    fn tombstoned_event_carries_reason_label_and_text() {
        let row = row_from_event(&LlmQueueEvent::Tombstoned {
            meta: base_meta(JobState::Tombstoned),
            reason: TombstoneReason::TaskCancelledInFlight {
                reason: Some("user aborted".to_string()),
            },
        })
        .expect("tombstoned -> row");
        assert_eq!(row.state, "tombstoned");
        assert!(!row.success);
        assert_eq!(
            row.tombstone_reason.as_deref(),
            Some("task_cancelled_in_flight")
        );
        assert_eq!(row.error.as_deref(), Some("user aborted"));
    }

    #[test]
    fn an_llm_call_outside_any_span_records_a_null_activity_id() {
        // `base_meta` carries no activity, which is what a submit site made
        // below the span floor produces. The column must be NULL — not "",
        // not "0" — so a join simply does not match it.
        let row = row_from_event(&LlmQueueEvent::Completed {
            meta: base_meta(JobState::Completed),
        })
        .expect("completed -> row");
        assert_eq!(row.activity_id, None);
    }

    #[test]
    fn the_activity_id_survives_the_parquet_round_trip() {
        // The join only exists if the id reaches disk in the same decimal
        // form the live `ActivityStarted` event carries.
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_layout = ArtifactV2Workspace::new(temp.path());
        let mut meta = base_meta(JobState::Completed);
        meta.trace_context.scope = magicllm::LlmScope::new("principal-a", "workspace-a");
        meta.origin.activity_id = Some(4242u64.to_string());
        let row = row_from_event(&LlmQueueEvent::Completed { meta }).expect("row");
        assert_eq!(row.activity_id.as_deref(), Some("4242"));

        let partition_dt = partition_date_for(row.timestamp_ms);
        write_batch(
            &workspace_layout,
            "principal-a",
            "workspace-a",
            &partition_dt,
            &[row],
        )
        .expect("write dispatch parquet");

        let partition_dir = workspace_layout
            .analytics_root("principal-a", "workspace-a")
            .join("llm_dispatch")
            .join(format!("dt={partition_dt}"));
        let parquet_path = std::fs::read_dir(&partition_dir)
            .expect("partition directory")
            .map(|entry| entry.expect("partition entry").path())
            .find(|path| path.extension().and_then(|value| value.to_str()) == Some("parquet"))
            .expect("parquet batch");
        let path_sql = parquet_path.display().to_string().replace('\'', "''");
        let conn = Connection::open_in_memory().expect("query connection");
        configure_analytics_connection_checked(&conn, "llm_dispatch_activity_id_test")
            .expect("configure query connection");
        let activity_id: String = conn
            .query_row(
                &format!("SELECT activity_id FROM read_parquet('{path_sql}')"),
                [],
                |record| record.get(0),
            )
            .expect("read activity id");
        assert_eq!(activity_id, "4242");
    }

    #[test]
    fn non_terminal_events_are_not_rows() {
        assert!(row_from_event(&LlmQueueEvent::Dispatched {
            meta: base_meta(JobState::InFlight)
        })
        .is_none());
    }

    #[test]
    fn phase1_dispatch_partition_key_uses_typed_scope_not_global_default() {
        let mut meta = base_meta(JobState::Completed);
        meta.trace_context.scope = magicllm::LlmScope::new("principal-a", "workspace-a");
        meta.trace_context.scope_resolution = magicllm::LlmScopeResolution::Inherited;
        let row = row_from_event(&LlmQueueEvent::Completed { meta }).expect("row");
        let (principal, workspace, _) = dispatch_partition_key(&row);
        assert_eq!(principal, "principal-a");
        assert_eq!(workspace, "workspace-a");
    }

    #[test]
    fn dispatch_parquet_round_trip_preserves_effective_profile_and_scope() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_layout = ArtifactV2Workspace::new(temp.path());
        let mut meta = base_meta(JobState::Completed);
        meta.trace_context.scope = magicllm::LlmScope::new("principal-a", "workspace-a");
        meta.trace_context.scope_resolution = magicllm::LlmScopeResolution::Inherited;
        meta.profile = Some("effective-profile".to_string());
        let row = row_from_event(&LlmQueueEvent::Completed { meta }).expect("row");
        let partition_dt = partition_date_for(row.timestamp_ms);
        write_batch(
            &workspace_layout,
            "principal-a",
            "workspace-a",
            &partition_dt,
            &[row.clone()],
        )
        .expect("write dispatch parquet");

        let partition_dir = workspace_layout
            .analytics_root("principal-a", "workspace-a")
            .join("llm_dispatch")
            .join(format!("dt={partition_dt}"));
        let parquet_path = std::fs::read_dir(&partition_dir)
            .expect("partition directory")
            .map(|entry| entry.expect("partition entry").path())
            .find(|path| path.extension().and_then(|value| value.to_str()) == Some("parquet"))
            .expect("parquet batch");
        let path_sql = parquet_path.display().to_string().replace('\'', "''");
        let conn = Connection::open_in_memory().expect("query connection");
        configure_analytics_connection_checked(&conn, "llm_dispatch_parquet_test")
            .expect("configure query connection");
        let query = format!(
            "SELECT profile, principal, workspace, llm_call_id, provider_wait_ms FROM read_parquet('{path_sql}')"
        );
        let (profile, principal, workspace, llm_call_id, provider_wait_ms): (
            String,
            String,
            String,
            String,
            i64,
        ) = conn
            .query_row(&query, [], |record| {
                Ok((
                    record.get(0)?,
                    record.get(1)?,
                    record.get(2)?,
                    record.get(3)?,
                    record.get(4)?,
                ))
            })
            .expect("read dispatch parquet row");

        assert_eq!(profile, "effective-profile");
        assert_eq!(principal, "principal-a");
        assert_eq!(workspace, "workspace-a");
        assert_eq!(llm_call_id, row.llm_call_id);
        assert_eq!(provider_wait_ms, 125);
    }

    #[cfg(unix)]
    #[test]
    fn dispatch_writer_rejects_symlinked_scope() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let external = tempfile::tempdir().expect("external");
        let workspace_layout = ArtifactV2Workspace::new(temp.path());
        let principal_root = workspace_layout.scopes_root().join("principal-a");
        std::fs::create_dir_all(&principal_root).expect("principal root");
        symlink(external.path(), principal_root.join("workspace-a")).expect("workspace symlink");
        let mut meta = base_meta(JobState::Completed);
        meta.trace_context.scope = magicllm::LlmScope::new("principal-a", "workspace-a");
        let row = row_from_event(&LlmQueueEvent::Completed { meta }).expect("row");
        let error = write_batch(
            &workspace_layout,
            "principal-a",
            "workspace-a",
            &partition_date_for(row.timestamp_ms),
            &[row],
        )
        .expect_err("scope symlink must fail closed");
        assert!(error.to_string().contains("real directory"));
        assert_eq!(
            std::fs::read_dir(external.path())
                .expect("external")
                .count(),
            0
        );
    }
}

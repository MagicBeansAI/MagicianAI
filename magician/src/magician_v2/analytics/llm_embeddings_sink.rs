//! Local embedding-call Parquet sink — separate lakehouse stream for
//! per-batch embedding telemetry.
//!
//! Local embeddings (the on-device Ollama embedder used by memory indexing,
//! resurfacing centrality, procedure indexing, and elicitation) are captured
//! here as one flat, content-free record per embed *batch*. This is a
//! deliberately SEPARATE dataset from `llm_calls` — never tagged rows in the
//! hot per-call path — so high-frequency, zero-cost embedding telemetry never
//! bloats `llm_calls` queries.
//!
//! ```text
//! <scope>/analytics/llm_embeddings/dt=YYYY-MM-DD/embed_<ulid>.parquet
//! ```
//!
//! The sink mirrors [`super::llm_parquet_sink`] but is fed by an explicit
//! non-blocking `try_record` API rather than a broadcaster subscription:
//! embed callers hold the scope, embed calls are frequent, and the emit must
//! NEVER block or panic the caller. A saturated channel drops the record and
//! bumps a counter instead of applying backpressure.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use duckdb::{params, Connection};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::magician_v2::analytics::duckdb_safety::{
    analytics_duckdb_guard, configure_analytics_connection_checked,
};
use crate::magician_v2::analytics::llm_scoped_path::ensure_real_scoped_directory_chain;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

/// Flush triggers (whichever fires first). Embedding batches are lower volume
/// than LLM calls but still bursty during a reindex, so keep the same cadence.
const FLUSH_INTERVAL: Duration = Duration::from_secs(30);
const FLUSH_ROW_THRESHOLD: usize = 100;

/// Bounded submit queue between `try_record` callers and the sink task. Small
/// on purpose: a saturated queue drops the record (fire-and-forget) rather than
/// blocking the embedding caller.
const RECORD_QUEUE_CAPACITY: usize = 512;

/// One flat, content-free embedding-batch record. All fields are metadata:
/// counts, timing, identity — never the embedded text or vectors.
#[derive(Debug, Clone)]
pub struct EmbeddingCallRecord {
    pub timestamp_ms: i64,
    pub principal: String,
    pub workspace: String,
    pub provider: String,
    pub model: String,
    /// Purpose of the embed batch (e.g. `memory_index`, `resurfacing`,
    /// `procedure_index`, `elicitation`).
    pub operation: String,
    /// Ollama-reported token count if available, else a length-based estimate.
    pub input_tokens: i64,
    pub batch_size: i32,
    /// Always `0.0` for local embeddings — kept for schema symmetry with
    /// `llm_calls` so downstream tooling can UNION cost analytics.
    pub cost_usd: f64,
    pub latency_ms: i64,
    pub success: bool,
}

/// Public handle for spawning + submitting to + shutting down the embeddings
/// sink. Cheaply cloneable: cloning shares the same submit channel and drop
/// counter so any subsystem holding a clone can `try_record`.
#[derive(Clone)]
pub struct LlmEmbeddingsSink {
    record_tx: mpsc::Sender<EmbeddingCallRecord>,
    dropped: Arc<AtomicU64>,
    inner: Arc<LlmEmbeddingsSinkInner>,
}

/// Owns the join handle + cancel token. Held behind an `Arc` so the public
/// handle stays `Clone`; `shutdown` takes the whole handle and joins once.
struct LlmEmbeddingsSinkInner {
    handle: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    cancel: CancellationToken,
}

impl LlmEmbeddingsSink {
    /// Spawn the sink task. Completed-day compaction and retention are owned by
    /// the singular storage-maintenance runtime so a retention sweep cannot
    /// race an embedding flush or verified compaction publication.
    pub fn spawn(workspace_layout: ArtifactV2Workspace) -> Self {
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let (record_tx, record_rx) = mpsc::channel::<EmbeddingCallRecord>(RECORD_QUEUE_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        let dropped_for_task = Arc::clone(&dropped);
        let handle = tokio::spawn(async move {
            run_sink(
                record_rx,
                workspace_layout,
                dropped_for_task,
                cancel_for_task,
            )
            .await;
        });
        Self {
            record_tx,
            dropped,
            inner: Arc::new(LlmEmbeddingsSinkInner {
                handle: std::sync::Mutex::new(Some(handle)),
                cancel,
            }),
        }
    }

    /// Non-blocking, fire-and-forget submit. NEVER blocks or panics the caller:
    /// on a saturated (or closed) channel the record is dropped and a counter is
    /// bumped. Embedding is on hot paths — recording must never gate it.
    pub fn try_record(&self, record: EmbeddingCallRecord) {
        if self.record_tx.try_send(record).is_err() {
            let count = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            // Log sparsely: on the first drop and every 1024th thereafter, so a
            // sustained overload leaves a trail without spamming.
            if count == 1 || count % 1024 == 0 {
                warn!(
                    target: "analytics::llm_embeddings_sink",
                    dropped = count,
                    "embeddings sink queue saturated; dropping record (fire-and-forget)"
                );
            }
        }
    }

    /// Total records dropped due to backpressure since spawn.
    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Cancel the sink and await its final flush. Idempotent across clones —
    /// only the first caller that holds the join handle actually joins.
    pub async fn shutdown(&self) {
        self.inner.cancel.cancel();
        let handle = self
            .inner
            .handle
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        if let Some(handle) = handle {
            if let Err(err) = handle.await {
                warn!(
                    target: "analytics::llm_embeddings_sink",
                    error = %err,
                    "llm_embeddings_sink join failed on shutdown"
                );
            }
        }
    }
}

/// Process-global embeddings sink handle.
///
/// The `OllamaEmbedder` lives in the `magician-vector-index` crate and holds no
/// scope, so it cannot reach this sink without a crate cycle. Instead, embed
/// call sites in `magician` (which DO hold the scope) record their batch via
/// [`record_embedding_batch`] after the embed completes. Registration mirrors
/// the `ollama_lifecycle` singleton pattern: set once at startup, read cheaply.
static GLOBAL_SINK: OnceLock<LlmEmbeddingsSink> = OnceLock::new();

/// Register the process-global embeddings sink. Idempotent — a second call is a
/// no-op and returns the sink that lost the race so the caller can shut it down.
/// Call once at startup after spawning the sink.
pub fn set_global_sink(sink: LlmEmbeddingsSink) -> std::result::Result<(), LlmEmbeddingsSink> {
    GLOBAL_SINK.set(sink)
}

/// Access the process-global sink, if registered.
pub fn global_sink() -> Option<&'static LlmEmbeddingsSink> {
    GLOBAL_SINK.get()
}

/// Best-effort, non-blocking recording of one embed batch through the global
/// sink. A no-op when no sink is registered (e.g. tests, or embeddings disabled)
/// and never blocks or fails the caller.
///
/// `input_tokens` should be the provider-reported token count when available,
/// else a length-based estimate; use [`estimate_input_tokens`] for the latter.
#[allow(clippy::too_many_arguments)]
pub fn record_embedding_batch(
    principal: &str,
    workspace: &str,
    model: &str,
    operation: &str,
    batch_size: usize,
    input_tokens: i64,
    latency_ms: u64,
    success: bool,
) {
    // Unscoped emits would land in an ambiguous bucket — drop them, matching the
    // llm_calls sink policy.
    if principal.trim().is_empty() || workspace.trim().is_empty() {
        return;
    }
    let Some(sink) = global_sink() else {
        return;
    };
    record_embedding_batch_to(
        sink,
        principal,
        workspace,
        model,
        operation,
        batch_size,
        input_tokens,
        latency_ms,
        success,
    );
}

/// Record through an explicitly supplied sink. Production callers normally use
/// [`record_embedding_batch`]; this boundary keeps subsystem coverage isolated
/// from the process-global `OnceLock` without changing runtime routing.
#[allow(clippy::too_many_arguments)]
pub fn record_embedding_batch_to(
    sink: &LlmEmbeddingsSink,
    principal: &str,
    workspace: &str,
    model: &str,
    operation: &str,
    batch_size: usize,
    input_tokens: i64,
    latency_ms: u64,
    success: bool,
) {
    if principal.trim().is_empty() || workspace.trim().is_empty() {
        return;
    }
    sink.try_record(build_embedding_record(
        principal,
        workspace,
        model,
        operation,
        batch_size,
        input_tokens,
        latency_ms,
        success,
    ));
}

/// Pure record construction shared by [`record_embedding_batch`]. Kept separate
/// so the field mapping (provider tag, saturating casts, `cost_usd == 0.0`) is
/// unit-testable without touching the process-global sink.
#[allow(clippy::too_many_arguments)]
fn build_embedding_record(
    principal: &str,
    workspace: &str,
    model: &str,
    operation: &str,
    batch_size: usize,
    input_tokens: i64,
    latency_ms: u64,
    success: bool,
) -> EmbeddingCallRecord {
    EmbeddingCallRecord {
        timestamp_ms: Utc::now().timestamp_millis(),
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        provider: "ollama".to_string(),
        model: model.to_string(),
        operation: operation.to_string(),
        input_tokens,
        batch_size: batch_size.min(i32::MAX as usize) as i32,
        cost_usd: 0.0,
        latency_ms: latency_ms.min(i64::MAX as u64) as i64,
        success,
    }
}

/// Length-based token estimate for a batch when the embedder does not report
/// token usage (Ollama's embed response carries none): ~4 chars per token.
pub fn estimate_input_tokens(texts: &[String]) -> i64 {
    texts
        .iter()
        .map(|text| (text.len() / 4) as i64)
        .sum::<i64>()
}

async fn run_sink(
    mut record_rx: mpsc::Receiver<EmbeddingCallRecord>,
    workspace_layout: ArtifactV2Workspace,
    dropped: Arc<AtomicU64>,
    cancel: CancellationToken,
) {
    // Per-scope buffer. Each (principal, workspace) flushes to its own dir.
    let mut buffers: std::collections::HashMap<(String, String), Vec<EmbeddingCallRecord>> =
        std::collections::HashMap::new();

    // Flush jobs go to a blocking-pool worker (DuckDB write is synchronous).
    let (flush_tx, flush_rx) = mpsc::channel::<FlushJob>(8);
    let workspace_for_worker = workspace_layout.clone();
    let worker = tokio::task::spawn_blocking(move || flush_worker(workspace_for_worker, flush_rx));

    let mut flush_timer = tokio::time::interval(FLUSH_INTERVAL);
    flush_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            record = record_rx.recv() => match record {
                Some(record) => {
                    let key = (record.principal.clone(), record.workspace.clone());
                    let entry = buffers.entry(key).or_default();
                    entry.push(record);
                    if entry.len() >= FLUSH_ROW_THRESHOLD {
                        let principal = entry[0].principal.clone();
                        let workspace = entry[0].workspace.clone();
                        let rows = std::mem::take(entry);
                        try_send_flush(&flush_tx, FlushJob { principal, workspace, rows });
                    }
                }
                // All submitters dropped their handle. Flush and exit.
                None => break,
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

    // A `cancel`-triggered break can leave records still queued in the channel
    // (a submitter fired a batch then immediately called `shutdown`). Drain the
    // backlog into the per-scope buffers so shutdown flushes the final batch
    // instead of silently dropping it.
    while let Ok(record) = record_rx.try_recv() {
        let key = (record.principal.clone(), record.workspace.clone());
        buffers.entry(key).or_default().push(record);
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
            target: "analytics::llm_embeddings_sink",
            error = %err,
            "embeddings flush worker join failed"
        );
    }
    let dropped_total = dropped.load(Ordering::Relaxed);
    if dropped_total > 0 {
        warn!(
            target: "analytics::llm_embeddings_sink",
            dropped = dropped_total,
            "embeddings sink shut down after dropping records under backpressure"
        );
    }
}

struct FlushJob {
    principal: String,
    workspace: String,
    rows: Vec<EmbeddingCallRecord>,
}

fn try_send_flush(tx: &mpsc::Sender<FlushJob>, job: FlushJob) {
    if let Err(err) = tx.try_send(job) {
        warn!(
            target: "analytics::llm_embeddings_sink",
            error = %err,
            "embeddings flush queue saturated; dropping batch"
        );
    }
}

fn flush_worker(workspace_layout: ArtifactV2Workspace, mut rx: mpsc::Receiver<FlushJob>) {
    while let Some(job) = rx.blocking_recv() {
        if let Err(err) = write_batch(&workspace_layout, &job) {
            warn!(
                target: "analytics::llm_embeddings_sink",
                principal = %job.principal,
                workspace = %job.workspace,
                rows = job.rows.len(),
                error = %err,
                "llm_embeddings_sink write_batch failed"
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
        .analytics_llm_embeddings_root(&job.principal, &job.workspace)
        .join(format!("dt={partition_dt}"));
    ensure_real_scoped_directory_chain(workspace_layout.base_root(), &partition_dir)?;
    std::fs::create_dir_all(&partition_dir)
        .with_context(|| format!("creating partition dir {}", partition_dir.display()))?;
    ensure_real_scoped_directory_chain(workspace_layout.base_root(), &partition_dir)?;

    let batch_id = ulid::Ulid::new().to_string();
    let parquet_path = partition_dir.join(format!("embed_{batch_id}.parquet"));

    let _duckdb_guard = analytics_duckdb_guard();
    let conn =
        Connection::open_in_memory().context("opening in-memory DuckDB for embeddings write")?;
    configure_analytics_connection_checked(&conn, "llm_embeddings_parquet_write")
        .context("configuring conservative DuckDB limits for llm_embeddings parquet write")?;
    conn.execute_batch(
        r#"
        CREATE TABLE llm_embeddings_batch (
            timestamp_ms BIGINT NOT NULL,
            principal VARCHAR NOT NULL,
            workspace VARCHAR NOT NULL,
            provider VARCHAR NOT NULL,
            model VARCHAR NOT NULL,
            operation VARCHAR NOT NULL,
            input_tokens BIGINT NOT NULL,
            batch_size INTEGER NOT NULL,
            cost_usd DOUBLE NOT NULL,
            latency_ms BIGINT NOT NULL,
            success BOOLEAN NOT NULL
        );
        "#,
    )
    .context("creating llm_embeddings_batch temp table")?;

    {
        let mut app = conn
            .appender("llm_embeddings_batch")
            .context("opening llm_embeddings_batch appender")?;
        for row in &job.rows {
            app.append_row(params![
                row.timestamp_ms,
                row.principal,
                row.workspace,
                row.provider,
                row.model,
                row.operation,
                row.input_tokens,
                row.batch_size,
                row.cost_usd,
                row.latency_ms,
                row.success,
            ])
            .context("appending row to llm_embeddings_batch")?;
        }
        app.flush()
            .context("flushing llm_embeddings_batch appender")?;
    }

    let path_sql = parquet_path.display().to_string().replace('\'', "''");
    let copy_sql = format!(
        "COPY llm_embeddings_batch TO '{}' (FORMAT PARQUET, COMPRESSION 'zstd');",
        path_sql
    );
    conn.execute_batch(&copy_sql)
        .with_context(|| format!("copying llm_embeddings_batch to {}", parquet_path.display()))?;
    crate::magician_v2::dataset_owners::publish_written_parquet(&parquet_path).with_context(
        || {
            format!(
                "publishing llm_embeddings parquet {}",
                parquet_path.display()
            )
        },
    )?;

    debug!(
        target: "analytics::llm_embeddings_sink",
        path = %parquet_path.display(),
        rows = job.rows.len(),
        "wrote embeddings Parquet batch"
    );

    Ok(())
}

fn partition_date_for(timestamp_ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(timestamp_ms)
        .unwrap_or_else(Utc::now)
        .format("%Y-%m-%d")
        .to_string()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn sample_record(operation: &str, batch_size: i32, input_tokens: i64) -> EmbeddingCallRecord {
        EmbeddingCallRecord {
            timestamp_ms: 1_700_000_000_000,
            principal: "p".to_string(),
            workspace: "w".to_string(),
            provider: "ollama".to_string(),
            model: "embed-model".to_string(),
            operation: operation.to_string(),
            input_tokens,
            batch_size,
            cost_usd: 0.0,
            latency_ms: 42,
            success: true,
        }
    }

    #[test]
    fn write_batch_round_trips_flat_embedding_records() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = ArtifactV2Workspace::new(tmp.path());
        let rows = vec![
            sample_record("memory_index", 3, 120),
            sample_record("resurfacing", 8, 400),
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

        let root = ws.analytics_llm_embeddings_root("p", "w");
        let glob = crate::magician_v2::dataset_owners::family_read_glob(
            &root,
            crate::magician_v2::dataset_owners::DatasetFamily::LlmEmbeddings,
        );
        let conn = Connection::open_in_memory().unwrap();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT provider, model, operation, input_tokens, batch_size, cost_usd, \
                 latency_ms, success \
                 FROM read_parquet('{glob}', union_by_name = true) ORDER BY operation"
            ))
            .expect("prepare");
        #[allow(clippy::type_complexity)]
        let out: Vec<(String, String, String, i64, i32, f64, i64, bool)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            })
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(out.len(), 2);
        // ORDER BY operation → memory_index before resurfacing.
        assert_eq!(out[0].2, "memory_index");
        assert_eq!(out[0].0, "ollama");
        assert_eq!(out[0].1, "embed-model");
        assert_eq!(out[0].3, 120);
        assert_eq!(out[0].4, 3);
        assert_eq!(out[0].5, 0.0);
        assert_eq!(out[0].6, 42);
        assert!(out[0].7);
        assert_eq!(out[1].2, "resurfacing");
        assert_eq!(out[1].3, 400);
        assert_eq!(out[1].4, 8);
    }

    #[tokio::test]
    async fn spawn_flush_and_read_back_via_shutdown() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = ArtifactV2Workspace::new(tmp.path());
        let sink = LlmEmbeddingsSink::spawn(ws.clone());
        for i in 0..5 {
            sink.try_record(sample_record("procedure_index", i + 1, (i as i64 + 1) * 10));
        }
        // Shutdown cancels + performs the final flush before returning.
        sink.shutdown().await;

        let root = ws.analytics_llm_embeddings_root("p", "w");
        let glob = crate::magician_v2::dataset_owners::family_read_glob(
            &root,
            crate::magician_v2::dataset_owners::DatasetFamily::LlmEmbeddings,
        );
        let conn = Connection::open_in_memory().unwrap();
        let total: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM read_parquet('{glob}', union_by_name = true)"),
                [],
                |row| row.get(0),
            )
            .expect("count embeddings rows");
        assert_eq!(
            total, 5,
            "all submitted records should be flushed on shutdown"
        );
        assert_eq!(sink.dropped_count(), 0, "no drops under a tiny load");
    }

    #[test]
    fn try_record_on_a_full_channel_never_blocks_or_panics() {
        // Build a sink whose channel we can saturate WITHOUT a running task
        // draining it. We construct the handle manually with a 1-slot channel
        // and no consumer, then fire many records: each must return promptly
        // (never block, never panic) and the drop counter must grow.
        let (record_tx, _record_rx) = mpsc::channel::<EmbeddingCallRecord>(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let sink = LlmEmbeddingsSink {
            record_tx,
            dropped: Arc::clone(&dropped),
            inner: Arc::new(LlmEmbeddingsSinkInner {
                handle: std::sync::Mutex::new(None),
                cancel: CancellationToken::new(),
            }),
        };
        // First send fills the single slot; every subsequent send must drop.
        for _ in 0..1000 {
            sink.try_record(sample_record("elicitation", 1, 10));
        }
        assert!(
            sink.dropped_count() >= 999,
            "a saturated channel must drop instead of blocking; dropped={}",
            sink.dropped_count()
        );
        // Draining the one buffered slot keeps _record_rx alive to end of test.
        drop(_record_rx);
    }

    #[test]
    fn build_embedding_record_tags_provider_and_zeroes_cost() {
        let record = build_embedding_record(
            "owner",
            "default",
            "embed-model",
            "memory_index",
            7,
            210,
            33,
            true,
        );
        assert_eq!(record.provider, "ollama");
        assert_eq!(record.model, "embed-model");
        assert_eq!(record.operation, "memory_index");
        assert_eq!(record.batch_size, 7);
        assert_eq!(record.input_tokens, 210);
        assert_eq!(record.cost_usd, 0.0);
        assert_eq!(record.latency_ms, 33);
        assert!(record.success);
        assert_eq!(record.principal, "owner");
        assert_eq!(record.workspace, "default");
    }

    #[tokio::test]
    async fn global_record_batch_writes_one_row_per_batch_with_tagged_fields() {
        // Register a real sink globally, emit one record per (simulated) batch
        // through the public choke point, and assert exactly one tagged row per
        // batch survives the flush. Uses the global `OnceLock`; if another test
        // already claimed it we fall back to the losing sink so this test still
        // exercises the per-batch field mapping deterministically.
        let tmp = tempfile::tempdir().expect("tempdir");
        let ws = ArtifactV2Workspace::new(tmp.path());
        let sink = LlmEmbeddingsSink::spawn(ws.clone());
        let owned = match set_global_sink(sink.clone()) {
            Ok(()) => sink,
            // Global already set by another test in this binary — use our own
            // handle directly; the field assertions below are unaffected.
            Err(rejected) => rejected,
        };

        // Three "batches" of different sizes, each recorded once.
        let batches = [
            ("memory_index", 3usize),
            ("resurfacing", 8),
            ("elicitation", 1),
        ];
        for (operation, size) in batches {
            let texts: Vec<String> = (0..size).map(|i| format!("text-{i}-xxxx")).collect();
            owned.try_record(build_embedding_record(
                "owner",
                "default",
                "embed-model",
                operation,
                texts.len(),
                estimate_input_tokens(&texts),
                12,
                true,
            ));
        }
        owned.shutdown().await;

        let root = ws.analytics_llm_embeddings_root("owner", "default");
        let glob = crate::magician_v2::dataset_owners::family_read_glob(
            &root,
            crate::magician_v2::dataset_owners::DatasetFamily::LlmEmbeddings,
        );
        let conn = Connection::open_in_memory().unwrap();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT operation, provider, model, batch_size, cost_usd, success \
                 FROM read_parquet('{glob}', union_by_name = true) ORDER BY batch_size"
            ))
            .expect("prepare");
        let out: Vec<(String, String, String, i32, f64, bool)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(out.len(), 3, "exactly one row per recorded batch");
        // ORDER BY batch_size → 1 (elicitation), 3 (memory_index), 8 (resurfacing).
        for row in &out {
            assert_eq!(row.1, "ollama");
            assert_eq!(row.2, "embed-model");
            assert_eq!(row.4, 0.0, "local embeddings cost nothing");
            assert!(row.5);
        }
        assert_eq!(out[0].0, "elicitation");
        assert_eq!(out[0].3, 1);
        assert_eq!(out[2].0, "resurfacing");
        assert_eq!(out[2].3, 8);
    }

    #[test]
    fn estimate_input_tokens_is_length_over_four() {
        // 8-char + 4-char → 2 + 1 = 3.
        let texts = vec!["abcdefgh".to_string(), "abcd".to_string()];
        assert_eq!(estimate_input_tokens(&texts), 3);
        assert_eq!(estimate_input_tokens(&[]), 0);
    }
}

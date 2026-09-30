/// The activity spine's durable writer — one row per completed span, the
/// retrospective counterpart to [`runtime_activity_layer`]'s live stream.
pub mod activity_rows_sink;
pub mod decision_model_telemetry;
pub mod duckdb_safety;
pub mod event_sink;
pub mod legacy_llm_compat;
pub mod llm_analytics_read_service;
pub mod llm_dispatch_rows;
pub mod llm_embeddings_sink;
pub mod llm_fact_compactor;
pub mod llm_fact_registry;
pub mod llm_parquet_sink;
pub mod llm_pricing_identity;
pub mod llm_reprice;
pub mod llm_restricted_content;
pub mod llm_scoped_path;
pub mod llm_sql_guard;
pub mod llm_tool_lineage;
pub mod llm_trace_activation;
pub mod llm_trace_content;
pub mod llm_trace_journal;
pub mod llm_trace_materializer;
pub mod llm_trace_recorder;
pub mod memory_eval_runner;
pub mod memory_events_compactor;
pub mod memory_index_maintainer;
pub mod memory_parquet;
pub mod memory_utility_batch_runner;
pub mod operation_llm_telemetry;
pub mod parquet_maintenance;
pub mod pool;
/// Spans + INFO events → the `Activity` transport family. Sibling of
/// [`tracing_layer`]; the two share a noisy-target skip list and a message
/// visitor so they cannot disagree about what is worth observing.
pub mod runtime_activity_layer;
pub mod schema_catalog;
pub mod tracing_layer;
pub mod views;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use once_cell::sync::OnceCell;
use tokio_util::sync::CancellationToken;

use crate::magician_v2::artifact_v2::workspace::{
    ArtifactV2Workspace, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};

use event_sink::{AnalyticsEvent, AnalyticsEventSink};

/// Global analytics dispatcher. Set once at startup via [`init`], then accessible
/// from anywhere via [`emit`]. If never initialized, [`emit`] is a no-op.
static GLOBAL_DISPATCHER: OnceCell<Arc<AnalyticsDispatcher>> = OnceCell::new();

#[derive(Debug)]
pub struct AnalyticsDispatcher {
    workspace_layout: ArtifactV2Workspace,
    shutdown: CancellationToken,
    sinks: Mutex<HashMap<(String, String), Arc<AnalyticsEventSink>>>,
    /// One shared read-write `DuckDbPool` per scope. The analytics DB is a single
    /// exclusive read-write handle, so EVERY accessor (the dispatcher's writes
    /// AND readers like the internal-data tools / analytics API) shares this one
    /// pool; readers take `read_connection()` (read-only, coexists with the
    /// writer). Opening a second read-write handle (the old per-reader
    /// `open_scoped`) raced the file lock and dropped events / failed queries.
    pools: Mutex<HashMap<(String, String), Arc<pool::DuckDbPool>>>,
}

impl AnalyticsDispatcher {
    pub fn new(workspace_layout: ArtifactV2Workspace, shutdown: CancellationToken) -> Self {
        Self {
            workspace_layout,
            shutdown,
            sinks: Mutex::new(HashMap::new()),
            pools: Mutex::new(HashMap::new()),
        }
    }

    /// The single shared read-write analytics pool for a scope, creating it on
    /// first use. Serialized so concurrent first-callers don't race two
    /// read-write opens. Returns `None` only if the open genuinely fails (logged).
    pub fn pool_for_scope(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Option<Arc<pool::DuckDbPool>> {
        let key = (principal.to_string(), workspace.to_string());
        let mut guard = self.pools.lock().ok()?;
        if let Some(existing) = guard.get(&key) {
            return Some(existing.clone());
        }
        let pool = match pool::DuckDbPool::open_scoped(&self.workspace_layout, principal, workspace)
        {
            Ok(pool) => Arc::new(pool),
            Err(error) => {
                tracing::warn!(
                    target: "analytics",
                    principal = principal,
                    workspace = workspace,
                    error = %error,
                    "failed to open scoped analytics database"
                );
                return None;
            },
        };
        guard.insert(key, pool.clone());
        Some(pool)
    }

    fn resolve_scope(event: &AnalyticsEvent) -> (String, String) {
        (
            event
                .principal
                .clone()
                .unwrap_or_else(|| DEFAULT_SCOPE_PRINCIPAL.to_string()),
            event
                .workspace
                .clone()
                .unwrap_or_else(|| DEFAULT_SCOPE_WORKSPACE.to_string()),
        )
    }

    fn sink_for_scope(&self, principal: &str, workspace: &str) -> Option<Arc<AnalyticsEventSink>> {
        let key = (principal.to_string(), workspace.to_string());
        // Hold the cache lock across creation so concurrent first-emits serialize
        // (otherwise several would race and all but one would be dropped). The
        // sink shares the one read-write pool for this scope; `start()` only spawns
        // the consumer task (no re-entrancy), so holding the lock is safe.
        let mut guard = self.sinks.lock().ok()?;
        if let Some(existing) = guard.get(&key) {
            return Some(existing.clone());
        }
        let pool = self.pool_for_scope(principal, workspace)?;
        let sink = Arc::new(AnalyticsEventSink::start(pool, self.shutdown.clone()));
        guard.insert(key, sink.clone());
        Some(sink)
    }

    pub fn emit(&self, event: AnalyticsEvent) {
        let (principal, workspace) = Self::resolve_scope(&event);
        if let Some(sink) = self.sink_for_scope(&principal, &workspace) {
            sink.emit(event);
        }
    }
}

/// Register the global analytics dispatcher. Called once at startup.
/// Returns `Err` if already initialized (safe to ignore).
pub fn init(dispatcher: Arc<AnalyticsDispatcher>) -> Result<(), Arc<AnalyticsDispatcher>> {
    GLOBAL_DISPATCHER.set(dispatcher)
}

/// Fire-and-forget an analytics event. No-op if the analytics layer is not
/// initialized. Safe to call from anywhere — no struct threading needed.
pub fn emit(event: AnalyticsEvent) {
    if let Some(dispatcher) = GLOBAL_DISPATCHER.get() {
        dispatcher.emit(event);
    }
}

/// Check whether the analytics layer is active.
pub fn is_active() -> bool {
    GLOBAL_DISPATCHER.get().is_some()
}

/// The shared scoped analytics `DuckDbPool` that every accessor must use so the
/// `analytics.duckdb` file has exactly ONE read-write handle process-wide.
///
/// Readers (internal-data tools, the analytics query API) call this and then use
/// `read_connection()` (read-only — coexists with the dispatcher's writer) rather
/// than opening their own read-write `open_scoped` pool, which would race the
/// file lock and intermittently fail. When the global dispatcher is not
/// initialized there is no competing handle, so a standalone pool is opened.
pub fn scoped_pool(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> anyhow::Result<Arc<pool::DuckDbPool>> {
    if let Some(dispatcher) = GLOBAL_DISPATCHER.get() {
        if let Some(shared) = dispatcher.pool_for_scope(principal, workspace) {
            return Ok(shared);
        }
    }
    Ok(Arc::new(pool::DuckDbPool::open_scoped(
        workspace_layout,
        principal,
        workspace,
    )?))
}

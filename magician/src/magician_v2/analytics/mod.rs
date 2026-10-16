/// The activity spine's durable writer — one row per completed span, the
/// retrospective counterpart to [`runtime_activity_layer`]'s live stream.
pub mod activity_rows_sink;
pub mod decision_metrics;
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
pub mod planning_usage_ledger;
pub mod pool;
/// Spans + INFO events → the `Activity` transport family. Sibling of
/// [`tracing_layer`]; the two share a noisy-target skip list and a message
/// visitor so they cannot disagree about what is worth observing.
pub mod runtime_activity_layer;
pub mod schema_catalog;
pub mod tracing_layer;
pub mod views;

use crate::magician_v2::storage_governance::idle_resource_cache::IdleResourceCache;
use std::sync::Arc;

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
    sinks: Arc<IdleResourceCache<(String, String), AnalyticsEventSink>>,
    /// One shared read-write `DuckDbPool` per scope. The analytics DB is a single
    /// exclusive read-write handle, so EVERY accessor (the dispatcher's writes
    /// AND readers like the internal-data tools / analytics API) shares this one
    /// pool; readers take `read_connection()` (read-only, coexists with the
    /// writer). Opening a second read-write handle (the old per-reader
    /// `open_scoped`) raced the file lock and dropped events / failed queries.
    pools: Arc<IdleResourceCache<(String, String), pool::DuckDbPool>>,
}

impl AnalyticsDispatcher {
    pub fn new(workspace_layout: ArtifactV2Workspace, shutdown: CancellationToken) -> Self {
        Self {
            workspace_layout,
            shutdown,
            sinks: IdleResourceCache::new(),
            pools: IdleResourceCache::new(),
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
        self.pools
            .get_or_try_insert_with(key, || {
                pool::DuckDbPool::open_scoped(&self.workspace_layout, principal, workspace)
                    .map(Arc::new)
            })
            .map_err(|error| {
                tracing::warn!(target: "analytics", principal, workspace, error = %error,
                "failed to open scoped analytics database");
            })
            .ok()
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
        // Retiring the last sender closes the channel. The worker drains its
        // accepted events before releasing its pool, so idle eviction loses no
        // queued work and never closes a database during a flush.
        self.sinks
            .get_or_try_insert_with(key, || {
                let pool = self.pool_for_scope(principal, workspace).ok_or(())?;
                Ok::<_, ()>(Arc::new(AnalyticsEventSink::start(
                    pool,
                    self.shutdown.clone(),
                )))
            })
            .ok()
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

#[cfg(test)]
mod idle_workspace_tests {
    use super::*;

    #[tokio::test]
    async fn idle_workspace_analytics_sink_drains_before_pool_retires() {
        let tmp = tempfile::tempdir().unwrap();
        let dispatcher = AnalyticsDispatcher::new(
            ArtifactV2Workspace::new(tmp.path()),
            CancellationToken::new(),
        );
        let key = ("alpha".to_string(), "prod".to_string());
        dispatcher.emit(AnalyticsEvent::log("info", "first", "idle-test").in_scope(&key.0, &key.1));
        dispatcher
            .emit(AnalyticsEvent::log("info", "second", "idle-test").in_scope(&key.0, &key.1));
        let pool = dispatcher.pool_for_scope(&key.0, &key.1).unwrap();
        assert_eq!(dispatcher.sinks.release_if_unused(&key), 1);
        assert_eq!(
            dispatcher.pools.release_if_unused(&key),
            0,
            "pending flush pins the pool"
        );
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let count = pool
                    .write_connection()
                    .query_row(
                        "SELECT count(*) FROM events WHERE source='idle-test'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap();
                if count == 2 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        drop(pool);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while dispatcher.pools.release_if_unused(&key) == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let reopened = dispatcher.pool_for_scope(&key.0, &key.1).unwrap();
        let read = reopened.read_connection().unwrap();
        assert_eq!(
            read.query_row(
                "SELECT count(*) FROM events WHERE source='idle-test'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        assert_eq!(
            read.query_row("SELECT current_setting('threads')", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(
            read.execute_batch("DELETE FROM events").is_err(),
            "reader stays read-only"
        );
    }
}

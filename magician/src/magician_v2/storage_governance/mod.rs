pub mod compaction_metrics;
pub mod database_maintenance;
pub mod duckdb_compaction;

pub use duckdb_compaction::{compact_open_database, DuckDbCompactionReport};

#[cfg(any(test, feature = "test-fixtures"))]
use std::path::{Path, PathBuf};

#[cfg(any(test, feature = "test-fixtures"))]
use anyhow::{anyhow, Context, Result};
#[cfg(any(test, feature = "test-fixtures"))]
use chrono::Utc;
#[cfg(any(test, feature = "test-fixtures"))]
use duckdb::{AccessMode, Config, Connection};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::magician_v2::{
    analytics::{self, parquet_maintenance},
    artifact_v2::workspace::ArtifactV2Workspace,
};

#[cfg(any(test, feature = "test-fixtures"))]
use crate::magician_v2::database_owners::{database_file_path, host_database_path, DatabaseOwner};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageKind {
    DuckDb,
    Sqlite,
    Parquet,
    Journal,
    Directory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageSafetyClass {
    Authoritative,
    LifecycleManaged,
    Regenerable,
    Observability,
    Restricted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageActionDescriptor {
    pub id: String,
    pub label: String,
    pub description: String,
    pub confirmation: Option<String>,
    pub destructive: bool,
}

/// Shared by the production comms inventory and legacy fixture inventory.
/// Maintenance itself remains in the encrypted App registry owner.
pub const APP_STORE_INVENTORY_POLICY: &str = "SQLCipher-encrypted app registry, records, grants and workflow receipts for this workspace. Maintenance runs through the app-store owner and coordinates active connections. It preserves app records; retention and purge remain app lifecycle operations.";

pub fn app_store_maintenance_actions() -> Vec<StorageActionDescriptor> {
    use crate::magician_v2::apps::registry::AppStoreMaintenanceAction;
    [
        (AppStoreMaintenanceAction::Verify, "verify", "Check integrity", "Verify encrypted pages and database structure."),
        (AppStoreMaintenanceAction::Optimize, "optimize", "Checkpoint and optimize", "Checkpoint the WAL and refresh planner statistics without deleting records."),
        (AppStoreMaintenanceAction::Reclaim, "reclaim", "Reclaim disk space", "Pause this workspace's app database operations while compacting encrypted pages and verifying integrity. Logical records are preserved."),
    ].into_iter().map(|(operation, id, label, description)| StorageActionDescriptor {
        id: format!("app_store_{id}"), label: label.to_owned(), description: description.to_owned(),
        confirmation: Some(operation.confirmation().to_owned()), destructive: false,
    }).collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageEntry {
    pub id: String,
    pub label: String,
    pub kind: StorageKind,
    pub safety_class: StorageSafetyClass,
    pub relative_path: String,
    pub size_bytes: u64,
    pub allocated_bytes: u64,
    pub wal_bytes: u64,
    #[serde(default)]
    pub shm_bytes: u64,
    pub file_count: usize,
    pub inventory_complete: bool,
    pub row_count: Option<u64>,
    pub oldest_partition: Option<String>,
    pub newest_partition: Option<String>,
    pub retention_days: Option<u32>,
    pub policy: String,
    pub actions: Vec<StorageActionDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageSnapshot {
    pub principal: String,
    pub workspace: String,
    pub generated_at_ms: i64,
    pub total_size_bytes: u64,
    pub total_allocated_bytes: u64,
    pub entries: Vec<StorageEntry>,
    pub compaction_metrics: compaction_metrics::CompactionMetricsSnapshot,
    pub safeguards: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuckDbTarget {
    Analytics,
    ChannelAssist,
    UiThreads,
    Social,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageMaintenanceReport {
    pub principal: String,
    pub workspace: String,
    pub started_at_ms: i64,
    pub completed_at_ms: i64,
    pub duckdb: Vec<DuckDbCompactionReport>,
    pub parquet: Option<parquet_maintenance::ParquetCompactionStats>,
    pub canonical_llm: Option<analytics::llm_fact_compactor::LlmFactCompactionStats>,
    pub retention: Option<parquet_maintenance::RetentionStats>,
}

/// The transport log's retention window, in the units `/storage` reports.
///
/// Derived from the compactor's own constant rather than written as `1`, so a
/// change to `EVENTS_RETENTION_WINDOW_MS` cannot leave this row lying. The
/// registry can only express whole days; the real floor is the *later* of a
/// rolling 24 hours and the newest 2,000 events, and only the prose can say so.
///
/// Rounded **up**, with a floor of one day. Integer division truncates toward
/// zero, so the moment anyone shortens the window below 24 hours — the only
/// direction it would plausibly move — this row would report `Some(0)`. On a
/// page whose whole job is retention policy, `0` reads as "discarded
/// immediately", which is both wrong and alarming, and the arithmetic that
/// produced it is invisible at the call site. Rounding up says "at most this
/// many days", which is the true statement a whole-day field can make about a
/// sub-day window.
#[cfg(any(test, feature = "test-fixtures"))]
const TRANSPORT_LOG_RETENTION_DAYS: u32 = {
    const DAY_MS: i64 = 24 * 60 * 60 * 1000;
    let window = crate::magician_v2::transport_log::EVENTS_RETENTION_WINDOW_MS;
    let whole_days = window / DAY_MS;
    let partial_day = window % DAY_MS != 0;
    if whole_days < 1 {
        1
    } else if partial_day {
        whole_days as u32 + 1
    } else {
        whole_days as u32
    }
};

/// Persist a bounded, content-free audit of successful physical compaction.
/// Metrics failure must never turn an already verified compaction into an
/// operation failure, so callers receive the maintenance result and a warning
/// is emitted if this auxiliary ledger cannot be updated.
pub fn record_compaction_metrics(
    layout: &ArtifactV2Workspace,
    trigger: compaction_metrics::CompactionMetricTrigger,
    report: &StorageMaintenanceReport,
) {
    use compaction_metrics::{CompactionMetricEvent, CompactionMetricKind};

    let duration_ms = report
        .completed_at_ms
        .saturating_sub(report.started_at_ms)
        .try_into()
        .unwrap_or(0);
    let mut events = Vec::new();
    for database in &report.duckdb {
        let mut event = CompactionMetricEvent::new(
            trigger,
            CompactionMetricKind::DuckDb,
            database.database.clone(),
            report.completed_at_ms,
        );
        event.files_before = 1;
        event.files_after = 1;
        event.files_compacted = 1;
        event.bytes_before = database.bytes_before;
        event.bytes_after = database.bytes_after;
        event.bytes_reclaimed = database.bytes_reclaimed;
        event.rows_compacted = database.row_count;
        event.duration_ms = duration_ms;
        events.push(event);
    }
    if let Some(parquet) = report.parquet.as_ref() {
        for area in &parquet.areas {
            let mut event = CompactionMetricEvent::new(
                trigger,
                CompactionMetricKind::Parquet,
                area.dataset.clone(),
                report.completed_at_ms,
            );
            event.partitions_compacted = area.partitions_compacted;
            event.files_before = area.files_before;
            event.files_after = area.files_after;
            event.files_compacted = area.raw_files_compacted;
            event.query_files_avoided = area.query_files_avoided;
            event.bytes_before = area.bytes_before;
            event.bytes_after = area.bytes_after;
            event.bytes_reclaimed = area.bytes_reclaimed;
            event.rows_compacted = area.rows_compacted;
            event.duration_ms = duration_ms;
            events.push(event);
        }
    }
    if let Some(canonical) = report.canonical_llm.as_ref() {
        for area in &canonical.areas {
            let mut event = CompactionMetricEvent::new(
                trigger,
                CompactionMetricKind::CanonicalLlm,
                area.dataset.clone(),
                report.completed_at_ms,
            );
            event.partitions_compacted = area.partitions_compacted;
            event.files_before = area.physical_files_before;
            event.files_after = area.physical_files_after;
            event.files_compacted = area.source_files_folded;
            event.query_files_avoided = area.query_files_avoided;
            event.bytes_before = area.bytes_before;
            event.bytes_after = area.bytes_after;
            event.bytes_reclaimed = area.bytes_reclaimed;
            event.compacted_bytes_written = area.compacted_bytes_written;
            event.rows_compacted = area.rows_compacted;
            event.duration_ms = duration_ms;
            events.push(event);
        }
    }
    if let Err(error) =
        compaction_metrics::append_events(layout, &report.principal, &report.workspace, events)
    {
        warn!(
            target: "storage_governance::compaction_metrics",
            principal = %report.principal,
            workspace = %report.workspace,
            error = %error,
            "verified compaction completed but its bounded metrics ledger could not be updated"
        );
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
fn build_snapshot(
    layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> Result<StorageSnapshot> {
    let defaults = crate::config::LlmTraceRetentionSettings::default();
    build_snapshot_with_restricted_retention(
        layout,
        principal,
        workspace,
        Some((
            defaults.sanitized_io_days,
            defaults.context_metadata_days,
            defaults.facts_days,
        )),
    )
}

#[cfg(any(test, feature = "test-fixtures"))]
fn cataloged_database_path(
    layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    owner: DatabaseOwner,
) -> PathBuf {
    if owner.is_host() {
        host_database_path(layout.base_root(), owner)
    } else {
        database_file_path(layout, principal, workspace, owner)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
fn build_snapshot_with_restricted_retention(
    layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    restricted_retention: Option<(u32, u32, u32)>,
) -> Result<StorageSnapshot> {
    let scope_root = layout.scope_root(principal, workspace);
    let mut entries = Vec::new();
    for (id, label, path, safety, policy, actions) in [
        (
            "analytics_duckdb",
            "Analytics catalog",
            cataloged_database_path(layout, principal, workspace, DatabaseOwner::AnalyticsDuckdb),
            StorageSafetyClass::Regenerable,
            "Hot query catalog; losslessly rebuilt from its Parquet mirror when necessary.",
            vec![compact_action("analytics")],
        ),
        (
            "channel_assist_duckdb",
            "Comms intelligence",
            cataloged_database_path(
                layout,
                principal,
                workspace,
                DatabaseOwner::ChannelAssistDuckdb,
            ),
            StorageSafetyClass::LifecycleManaged,
            "No age deletion. Provider reconciliation and user lifecycle state remain authoritative.",
            vec![compact_action("channel_assist")],
        ),
        (
            "ui_threads_duckdb",
            "Thread index",
            cataloged_database_path(layout, principal, workspace, DatabaseOwner::UiThreadsDuckdb),
            StorageSafetyClass::Authoritative,
            "Soft-delete tombstones preserve thread identity; physical compaction is lossless.",
            vec![compact_action("ui_threads")],
        ),
        (
            "feed_duckdb",
            "Today & feed projection",
            cataloged_database_path(layout, principal, workspace, DatabaseOwner::FeedDuckdb),
            StorageSafetyClass::Regenerable,
            "Derived read model. Orphan rows can be purged from authoritative task and attention sources.",
            vec![StorageActionDescriptor {
                id: "purge_feed_orphans".to_string(),
                label: "Purge stale projections".to_string(),
                description: "Remove feed rows whose authoritative task or attention source no longer exists.".to_string(),
                confirmation: None,
                destructive: true,
            }],
        ),
    ] {
        entries.push(database_entry(
            &scope_root,
            id,
            label,
            &path,
            StorageKind::DuckDb,
            safety,
            None,
            policy,
            actions,
        )?);
    }

    entries.push(database_entry(
        &scope_root,
        "browser_engine_usage_sqlite",
        "Browser engine activity",
        &cataloged_database_path(
            layout,
            principal,
            workspace,
            DatabaseOwner::BrowserEngineUsage,
        ),
        StorageKind::Sqlite,
        StorageSafetyClass::Observability,
        Some(90),
        "Content-free command-attempt facts retain sanitized HTTP(S) origin/path only and are bounded to 90 days and 50,000 rows per scope.",
        Vec::new(),
    )?);

    entries.push(database_entry(
        layout.base_root(),
        "attention_learning_sqlite",
        "Attention learning",
        &cataloged_database_path(
            layout,
            principal,
            workspace,
            DatabaseOwner::AttentionLearning,
        ),
        StorageKind::Sqlite,
        StorageSafetyClass::LifecycleManaged,
        None,
        "Shared physical learning ledger with scope-owned evidence. Online optimization preserves every row; history cleanup is previewed and applied only to the selected scope; disk reclamation drains the store and verifies an exact rebuild before replacement.",
        vec![
            StorageActionDescriptor {
                id: "optimize_attention_learning".to_string(),
                label: "Optimize now".to_string(),
                description: "Refresh SQLite planner statistics online without deleting history or shrinking the file.".to_string(),
                confirmation: Some("OPTIMIZE ATTENTION".to_string()),
                destructive: false,
            },
            StorageActionDescriptor {
                id: "retain_attention_learning".to_string(),
                label: "Clean old history".to_string(),
                description: "Preview and then remove old analytical learning rows for only this principal/workspace while preserving active evidence and current models.".to_string(),
                confirmation: Some("CLEAN ATTENTION HISTORY".to_string()),
                destructive: true,
            },
            StorageActionDescriptor {
                id: "reclaim_attention_learning".to_string(),
                label: "Reclaim disk space".to_string(),
                description: "Hold attention writes while rebuilding and verifying every row; briefly drain reads only for the final atomic replacement with rollback protection.".to_string(),
                confirmation: Some("RECLAIM ATTENTION DATABASE".to_string()),
                destructive: false,
            },
        ],
    )?);

    let app_store_path =
        cataloged_database_path(layout, principal, workspace, DatabaseOwner::AppStoreSqlite);
    validate_inventory_path_components(layout.base_root(), &app_store_path)?;
    entries.push(database_entry(
        &scope_root,
        "app_store_sqlite",
        "App store",
        &app_store_path,
        StorageKind::Sqlite,
        StorageSafetyClass::Authoritative,
        None,
        APP_STORE_INVENTORY_POLICY,
        if app_store_path.try_exists()? {
            app_store_maintenance_actions()
        } else {
            Vec::new()
        },
    )?);

    for (id, label, path, safety, policy) in [
        (
            "app_packages",
            "App packages",
            layout.app_packages_root(principal, workspace),
            StorageSafetyClass::LifecycleManaged,
            "Immutable package and cache bytes are lifecycle-owned by the future app registry; generic storage maintenance cannot delete or compact them.",
        ),
        (
            "app_attachments",
            "App attachments",
            layout.app_attachments_root(principal, workspace),
            StorageSafetyClass::Restricted,
            "Retained app attachments may contain personal data and are deleted only through the future app retention and purge settlement.",
        ),
        (
            "app_exports",
            "App exports",
            layout.app_exports_root(principal, workspace),
            StorageSafetyClass::Restricted,
            "App export archives remain inspect-only until the archive, encryption and purge owners implement their load-bearing lifecycle.",
        ),
        (
            "app_captures",
            "App captures",
            layout.app_captures_root(principal, workspace),
            StorageSafetyClass::Restricted,
            "Prompt, debug and provider captures remain inspect-only and may be removed only by their future data-policy and retention owner.",
        ),
        (
            "app_evaluations",
            "App evaluations",
            layout.app_evaluations_root(principal, workspace),
            StorageSafetyClass::Restricted,
            "Evaluation artifacts retain their source labels and follow the future app data-policy and purge settlement; generic cleanup is forbidden.",
        ),
    ] {
        validate_inventory_path_components(layout.base_root(), &path)?;
        entries.push(app_directory_entry(
            &scope_root,
            id,
            label,
            &path,
            StorageKind::Directory,
            safety,
            None,
            policy,
            Vec::new(),
        )?);
    }

    // The retention column carries the window the policy prose already states.
    // Leaving it `None` while the prose says "bounded to 30 days" makes the
    // typed field and the human field disagree, and only one of them is what a
    // caller filters on.
    for (id, label, path, retention_days, policy, actions) in [
        (
            "attention_funnel_sqlite",
            "Attention funnel",
            cataloged_database_path(layout, principal, workspace, DatabaseOwner::AttentionFunnel),
            Some(30),
            "Automatically bounded to 30 days and 50,000 events per scope; clearing it would weaken durable dedupe.",
            Vec::new(),
        ),
        (
            "resurfacing_sqlite",
            "Resurfacing state",
            cataloged_database_path(layout, principal, workspace, DatabaseOwner::Resurfacing),
            Some(90),
            "Automatically bounded to 90 days with per-scope candidate and feedback caps.",
            Vec::new(),
        ),
        (
            "social_sqlite",
            "Social Feed",
            cataloged_database_path(layout, principal, workspace, DatabaseOwner::SocialSqlite),
            None,
            "Scope-isolated agent social network data.",
            vec![compact_action("social")],
        ),
    ] {
        entries.push(database_entry(
            layout.base_root(),
            id,
            label,
            &path,
            StorageKind::Sqlite,
            StorageSafetyClass::LifecycleManaged,
            retention_days,
            policy,
            actions,
        )?);
    }

    let analytics_root = layout.analytics_root(principal, workspace);
    for (id, label, relative, safety, retention, policy) in [
        ("events", "Runtime events", "events", StorageSafetyClass::Observability, Some(90), "Completed partitions compact automatically; 90-day telemetry retention."),
        ("memory_events", "Memory diagnostics", "memory_events", StorageSafetyClass::Observability, Some(90), "Recall/utility diagnostics; compacted and retained for 90 days."),
        ("activity_rows", "Activity spine", "activity_rows", StorageSafetyClass::Observability, Some(parquet_maintenance::ACTIVITY_DETAIL_RETENTION_DAYS as u32), "One row per completed span. Hours fold after 2h and days after 48h; full rows are summarised into Activity rollups after 7 days rather than deleted."),
        ("activity_rollups", "Activity rollups", "activity_rollups", StorageSafetyClass::Observability, Some(parquet_maintenance::ACTIVITY_ROLLUP_RETENTION_DAYS as u32), "Per-hour counts and latency percentiles that outlive the spans behind them. Written when spine detail expires; kept 13 months so last year's shape is still comparable."),
        ("llm_calls", "LLM calls", "llm_calls", StorageSafetyClass::Observability, Some(90), "Canonical facts retain verified fallback revisions; legacy batches compact; 90-day retention."),
        ("llm_embeddings", "Embedding batches", "llm_embeddings", StorageSafetyClass::Observability, Some(90), "Content-free local embedding telemetry; completed partitions compact and expire after 90 days."),
        ("llm_provider_attempts", "Provider attempts", "llm_provider_attempts", StorageSafetyClass::Observability, Some(90), "Canonical provider-attempt lineage with 90-day retention."),
        ("llm_tool_calls", "Tool-call lineage", "llm_tool_calls", StorageSafetyClass::Observability, Some(90), "Tool causality facts are now covered by governed 90-day retention."),
        ("llm_capture_gaps", "Capture gaps", "llm_capture_gaps", StorageSafetyClass::Observability, Some(90), "Capture-quality evidence with 90-day retention."),
        ("llm_dispatch", "LLM dispatch", "llm_dispatch", StorageSafetyClass::Observability, Some(90), "Queue timing telemetry; completed partitions compact and expire after 90 days."),
        ("llm_call_io", "Restricted LLM content", "llm_call_io", StorageSafetyClass::Restricted, restricted_retention.map(|value| value.0), "Restricted payload content uses the live configured retention lifecycle."),
        ("llm_context_blocks", "LLM context lineage", "llm_context_blocks", StorageSafetyClass::Restricted, restricted_retention.map(|value| value.1), "Restricted context provenance follows the live configured retention and tombstone lifecycle."),
        ("llm_content_tombstones", "Restricted-content tombstones", "llm_content_tombstones", StorageSafetyClass::Restricted, restricted_retention.map(|value| value.2.max(value.0)), "Deletion evidence follows the stricter of fact and sanitized-content retention and is never handled by generic telemetry cleanup."),
        ("llm_content_access_audit", "Restricted-content access audit", "llm_content_access_audit", StorageSafetyClass::Restricted, restricted_retention.map(|value| value.2), "Every restricted reveal is durably audited and retained by the live restricted-fact policy."),
    ] {
        entries.push(directory_entry(
            &scope_root,
            id,
            label,
            &analytics_root.join(relative),
            StorageKind::Parquet,
            safety,
            retention,
            policy,
            // `activity_rollups` is deliberately absent: it is produced *by*
            // maintenance, one object per day, and has nothing to fold.
            if matches!(id, "events" | "memory_events" | "llm_calls" | "llm_embeddings" | "llm_dispatch" | "activity_rows") {
                vec![compact_parquet_action()]
            } else {
                Vec::new()
            },
        )?);
    }
    // The per-scope transport log — the NDJSON tail `/runtime`, `/events` and
    // `/attention` all stream from. It has been unregistered since it was
    // written, despite having already forced its own compactor to be rewritten
    // once, which is exactly the position a store should not be in: growth
    // nobody can see until it causes an incident.
    //
    // Not to be confused with the `events` row above. That one is
    // `<scope>/analytics/events`, a Parquet dataset. The similar name is a
    // trap, and this comment is here so the next reader does not conclude one
    // of them is a duplicate of the other and delete a row.
    //
    // The path is resolved by the writer's own rule rather than assembled from
    // `scope_root`. The two sanitizers disagree: `ArtifactV2Workspace` accepts
    // any segment that is not a traversal, while `transport_log` accepts only
    // `[A-Za-z0-9_-]` up to 128 characters and routes everything else to
    // `_quarantine/_quarantine`. A principal like `user.name` therefore has a
    // `scope_root` the transport log never writes to, and this row used to stat
    // that empty path and report a confident 0 bytes for a log that was in fact
    // growing somewhere else.
    entries.push(database_entry(
        // Displayed relative to the base root, not the scope root: for a scope
        // the transport log quarantines, the two are different directories and
        // only the base-relative form says which one the bytes are in.
        layout.base_root(),
        "transport_log_jsonl",
        "Runtime transport log",
        &crate::magician_v2::transport_log::scope_log_path(
            layout.base_root(),
            principal,
            workspace,
        ),
        StorageKind::Journal,
        StorageSafetyClass::Observability,
        // The compactor's floor is a rolling 24 hours, which is one day — the
        // finest granularity `retention_days` can express. Reporting `None`
        // here said "no retention policy" about the one store in this list with
        // the tightest one.
        Some(TRANSPORT_LOG_RETENTION_DAYS),
        "Append-only live-tail transport log, bounded by its own compactor to a 24-hour / 2,000-event window per scope. Generic Parquet retention does not touch it.",
        Vec::new(),
    )?);

    entries.push(directory_entry(
        &scope_root,
        "llm_trace_journal",
        "LLM recovery journal",
        &layout.analytics_llm_trace_journal_root(principal, workspace),
        StorageKind::Journal,
        StorageSafetyClass::Authoritative,
        None,
        "Append-before-materialize recovery state. It is never removed by generic retention.",
        Vec::new(),
    )?);
    entries.push(directory_entry(
        &scope_root,
        "llm_restricted_journal",
        "Restricted-content recovery journal",
        &layout.analytics_llm_restricted_journal_root(principal, workspace),
        StorageKind::Journal,
        StorageSafetyClass::Restricted,
        None,
        "Append-before-materialize restricted recovery state; committed payload segments are pruned only by their journal owner.",
        Vec::new(),
    )?);

    let compaction_metrics = compaction_metrics::snapshot(layout, principal, workspace)?;
    let metrics_path = compaction_metrics::metrics_path(layout, principal, workspace);
    entries.push(StorageEntry {
        id: "compaction_metrics".to_string(),
        label: "Compaction metrics".to_string(),
        kind: StorageKind::Journal,
        safety_class: StorageSafetyClass::Observability,
        relative_path: relative_display(&scope_root, &metrics_path),
        size_bytes: compaction_metrics.storage_bytes,
        allocated_bytes: compaction_metrics.allocated_bytes,
        wal_bytes: 0,
        shm_bytes: 0,
        file_count: usize::from(compaction_metrics.storage_bytes > 0),
        inventory_complete: true,
        row_count: Some(compaction_metrics.event_count as u64),
        oldest_partition: None,
        newest_partition: None,
        retention_days: None,
        policy: format!(
            "Content-free compaction history is capped at {} events and can be cleared independently.",
            compaction_metrics.retained_event_limit
        ),
        actions: vec![StorageActionDescriptor {
            id: "clear_compaction_metrics".to_string(),
            label: "Clear metrics".to_string(),
            description: "Clear only the bounded compaction-history ledger; no database, Parquet object, or recovery state is touched."
                .to_string(),
            confirmation: Some("CLEAR COMPACTION METRICS".to_string()),
            destructive: true,
        }],
    });

    let total_size_bytes = entries
        .iter()
        .map(|entry| {
            entry
                .size_bytes
                .saturating_add(entry.wal_bytes)
                .saturating_add(entry.shm_bytes)
        })
        .sum();
    let total_allocated_bytes = entries.iter().map(|entry| entry.allocated_bytes).sum();
    Ok(StorageSnapshot {
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        generated_at_ms: Utc::now().timestamp_millis(),
        total_size_bytes,
        total_allocated_bytes,
        entries,
        compaction_metrics,
        safeguards: vec![
            "DuckDB files are copied, schema/row verified, then atomically swapped with rollback backup.".to_string(),
            "Parquet raw batches are deleted only after compacted row-count, checksum, manifest, and fsync verification.".to_string(),
            "Mail and comms lifecycle data is never age-truncated by storage maintenance.".to_string(),
            "Symlinked stores, partitions, and source objects are rejected rather than followed.".to_string(),
            "Compaction metrics are content-free, bounded to 256 events per scope, and clear independently of governed data.".to_string(),
        ],
    })
}

/// Everything `/storage`'s "Apply retention" action expires, in one place.
///
/// Two sweeps, because the datasets have two different lifecycles and only one
/// of them can be expressed as a day count.
///
/// `parquet_maintenance::apply_retention` is a flat `remove_dir_all` past a
/// single window, and it deliberately excludes the activity spine: deleting
/// seven-day-old spans outright is exactly what the tiering exists to prevent.
/// So the button used to do nothing at all for `activity_rows` and
/// `activity_rollups`, while the registry advertised `retention_days` of 7 and
/// 396 on those rows — an operator pressing it would reasonably conclude the
/// spine had been tiered, and it had not.
///
/// It cannot be expressed as a parameter either: the handler clamps
/// `retention_days` to `30..=3650`, so the 7-day detail tier is unreachable
/// through it. The spine needs its own call, which is what this is — the same
/// shape `compact_parquet` already uses to drive `compact_activity_rows`.
#[cfg(any(test, feature = "test-fixtures"))]
fn apply_scope_retention(
    layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    retention_days: u32,
) -> Result<parquet_maintenance::RetentionStats> {
    let mut retention =
        parquet_maintenance::apply_retention(layout, principal, workspace, retention_days)?;
    let activity = parquet_maintenance::apply_activity_retention(layout, principal, workspace)?;
    retention.partitions_scanned += activity.partitions_scanned;
    retention.partitions_removed += activity.partitions_removed;
    retention.bytes_removed = retention
        .bytes_removed
        .saturating_add(activity.bytes_removed);
    Ok(retention)
}

#[allow(clippy::too_many_arguments)]
#[cfg(any(test, feature = "test-fixtures"))]
fn database_entry(
    display_root: &Path,
    id: &str,
    label: &str,
    path: &Path,
    kind: StorageKind,
    safety_class: StorageSafetyClass,
    retention_days: Option<u32>,
    policy: &str,
    actions: Vec<StorageActionDescriptor>,
) -> Result<StorageEntry> {
    let size_bytes = regular_file_size(path)?;
    let allocated_bytes = regular_file_allocated_size(path)?;
    // Only stores that actually have a write-ahead sidecar get one looked up.
    // A journal has none: it is a plain append-only file whose durable writer
    // stages through a uniquely-named temp and renames. Statting a `.wal` for
    // it was a guaranteed miss dressed up as a measurement.
    let wal = match kind {
        StorageKind::Sqlite => Some(sqlite_wal_path(path)),
        StorageKind::DuckDb => Some(companion_wal_path(path)),
        StorageKind::Journal | StorageKind::Parquet | StorageKind::Directory => None,
    };
    let wal_bytes = match wal.as_deref() {
        Some(wal) => regular_file_size(wal)?,
        None => 0,
    };
    let allocated_bytes = match wal.as_deref() {
        Some(wal) => allocated_bytes.saturating_add(regular_file_allocated_size(wal)?),
        None => allocated_bytes,
    };
    let shm = (kind == StorageKind::Sqlite).then(|| {
        let mut name = path.as_os_str().to_os_string();
        name.push("-shm");
        PathBuf::from(name)
    });
    let shm_bytes = match shm.as_deref() {
        Some(shm) => regular_file_size(shm)?,
        None => 0,
    };
    let allocated_bytes = match shm.as_deref() {
        Some(shm) => allocated_bytes.saturating_add(regular_file_allocated_size(shm)?),
        None => allocated_bytes,
    };
    let row_count = if kind == StorageKind::DuckDb && size_bytes > 0 {
        duckdb_estimated_rows(path).unwrap_or(None)
    } else {
        None
    };
    Ok(StorageEntry {
        id: id.to_string(),
        label: label.to_string(),
        kind,
        safety_class,
        relative_path: relative_display(display_root, path),
        size_bytes,
        allocated_bytes,
        wal_bytes,
        shm_bytes,
        file_count: usize::from(size_bytes > 0)
            + usize::from(wal_bytes > 0)
            + usize::from(shm_bytes > 0),
        inventory_complete: true,
        row_count,
        oldest_partition: None,
        newest_partition: None,
        retention_days,
        policy: policy.to_string(),
        actions,
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
fn directory_entry(
    display_root: &Path,
    id: &str,
    label: &str,
    path: &Path,
    kind: StorageKind,
    safety_class: StorageSafetyClass,
    retention_days: Option<u32>,
    policy: &str,
    actions: Vec<StorageActionDescriptor>,
) -> Result<StorageEntry> {
    let inventory = inventory_directory(path)?;
    directory_entry_from_inventory(
        display_root,
        id,
        label,
        path,
        kind,
        safety_class,
        retention_days,
        policy,
        actions,
        inventory,
    )
}

#[cfg(any(test, feature = "test-fixtures"))]
fn app_directory_entry(
    display_root: &Path,
    id: &str,
    label: &str,
    path: &Path,
    kind: StorageKind,
    safety_class: StorageSafetyClass,
    retention_days: Option<u32>,
    policy: &str,
    actions: Vec<StorageActionDescriptor>,
) -> Result<StorageEntry> {
    let inventory = inventory_directory_with_limits(
        path,
        Some(DirectoryInventoryLimits {
            max_entries: APP_DIRECTORY_INVENTORY_MAX_ENTRIES,
            max_depth: APP_DIRECTORY_INVENTORY_MAX_DEPTH,
        }),
    )?;
    directory_entry_from_inventory(
        display_root,
        id,
        label,
        path,
        kind,
        safety_class,
        retention_days,
        policy,
        actions,
        inventory,
    )
}

#[allow(clippy::too_many_arguments)]
#[cfg(any(test, feature = "test-fixtures"))]
fn directory_entry_from_inventory(
    display_root: &Path,
    id: &str,
    label: &str,
    path: &Path,
    kind: StorageKind,
    safety_class: StorageSafetyClass,
    retention_days: Option<u32>,
    policy: &str,
    actions: Vec<StorageActionDescriptor>,
    inventory: DirectoryInventory,
) -> Result<StorageEntry> {
    Ok(StorageEntry {
        id: id.to_string(),
        label: label.to_string(),
        kind,
        safety_class,
        relative_path: relative_display(display_root, path),
        size_bytes: inventory.size_bytes,
        allocated_bytes: inventory.allocated_bytes,
        wal_bytes: 0,
        shm_bytes: 0,
        file_count: inventory.file_count,
        inventory_complete: inventory.complete,
        row_count: None,
        oldest_partition: inventory.partitions.first().cloned(),
        newest_partition: inventory.partitions.last().cloned(),
        retention_days,
        policy: policy.to_string(),
        actions,
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
struct DirectoryInventory {
    size_bytes: u64,
    allocated_bytes: u64,
    file_count: usize,
    partitions: Vec<String>,
    complete: bool,
}

#[cfg(any(test, feature = "test-fixtures"))]
impl Default for DirectoryInventory {
    fn default() -> Self {
        Self {
            size_bytes: 0,
            allocated_bytes: 0,
            file_count: 0,
            partitions: Vec::new(),
            complete: true,
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
#[derive(Clone, Copy)]
struct DirectoryInventoryLimits {
    max_entries: usize,
    max_depth: usize,
}

#[cfg(any(test, feature = "test-fixtures"))]
const APP_DIRECTORY_INVENTORY_MAX_ENTRIES: usize = 20_000;
#[cfg(any(test, feature = "test-fixtures"))]
const APP_DIRECTORY_INVENTORY_MAX_DEPTH: usize = 64;
#[cfg(any(test, feature = "test-fixtures"))]
const DIRECTORY_INVENTORY_MAX_ENTRIES: usize = 50_000;
#[cfg(any(test, feature = "test-fixtures"))]
const DIRECTORY_INVENTORY_MAX_DEPTH: usize = 64;

#[cfg(any(test, feature = "test-fixtures"))]
fn inventory_directory(root: &Path) -> Result<DirectoryInventory> {
    inventory_directory_with_limits(
        root,
        Some(DirectoryInventoryLimits {
            max_entries: DIRECTORY_INVENTORY_MAX_ENTRIES,
            max_depth: DIRECTORY_INVENTORY_MAX_DEPTH,
        }),
    )
}

#[cfg(any(test, feature = "test-fixtures"))]
fn inventory_directory_with_limits(
    root: &Path,
    limits: Option<DirectoryInventoryLimits>,
) -> Result<DirectoryInventory> {
    let mut inventory = DirectoryInventory::default();
    let metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(inventory),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_dir() {
        return Err(anyhow!(
            "storage inventory root is not a real directory: {}",
            root.display()
        ));
    }
    let mut visited_entries = 0usize;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    'inventory: while let Some((directory, depth)) = stack.pop() {
        for entry in std::fs::read_dir(&directory)? {
            visited_entries = visited_entries.saturating_add(1);
            if limits.is_some_and(|limits| visited_entries > limits.max_entries) {
                inventory.complete = false;
                break 'inventory;
            }
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("dt=") && name.len() == 13)
                {
                    inventory.partitions.push(
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or_default()
                            .trim_start_matches("dt=")
                            .to_string(),
                    );
                }
                if limits.is_some_and(|limits| depth >= limits.max_depth) {
                    inventory.complete = false;
                } else {
                    stack.push((path, depth.saturating_add(1)));
                }
            } else if file_type.is_file() {
                let metadata = entry.metadata()?;
                inventory.size_bytes = inventory.size_bytes.saturating_add(metadata.len());
                inventory.allocated_bytes = inventory
                    .allocated_bytes
                    .saturating_add(allocated_bytes(&metadata));
                inventory.file_count += 1;
            }
        }
    }
    inventory.partitions.sort();
    inventory.partitions.dedup();
    Ok(inventory)
}

#[cfg(any(test, feature = "test-fixtures"))]
fn duckdb_estimated_rows(path: &Path) -> Result<Option<u64>> {
    let config = Config::default()
        .access_mode(AccessMode::ReadOnly)
        .context("configuring read-only storage inventory")?;
    let connection = match Connection::open_with_flags(path, config) {
        Ok(connection) => connection,
        Err(_) => return Ok(None),
    };
    let rows: i64 = connection
        .query_row(
            "SELECT CAST(coalesce(sum(estimated_size), 0) AS BIGINT) FROM duckdb_tables() WHERE NOT internal",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    Ok(Some(u64::try_from(rows).unwrap_or(0)))
}

#[cfg(any(test, feature = "test-fixtures"))]
fn regular_file_size(path: &Path) -> Result<u64> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(metadata.len()),
        Ok(_) => Ok(0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
fn regular_file_allocated_size(path: &Path) -> Result<u64> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(allocated_bytes(&metadata)),
        Ok(_) => Ok(0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
#[cfg(any(test, feature = "test-fixtures"))]
fn allocated_bytes(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
#[cfg(any(test, feature = "test-fixtures"))]
fn allocated_bytes(metadata: &std::fs::Metadata) -> u64 {
    metadata.len()
}

#[cfg(any(test, feature = "test-fixtures"))]
fn companion_wal_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("store");
    path.with_file_name(format!("{name}.wal"))
}

#[cfg(any(test, feature = "test-fixtures"))]
fn sqlite_wal_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("store.db");
    path.with_file_name(format!("{name}-wal"))
}

#[cfg(any(test, feature = "test-fixtures"))]
fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(any(test, feature = "test-fixtures"))]
fn validate_inventory_path_components(base_root: &Path, path: &Path) -> Result<()> {
    let relative = path.strip_prefix(base_root).map_err(|_| {
        anyhow!(
            "storage inventory path escapes its trusted root: {}",
            path.display()
        )
    })?;
    let mut current = base_root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(segment) = component else {
            return Err(anyhow!(
                "storage inventory path has a non-normal component: {}",
                path.display()
            ));
        };
        current.push(segment);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(anyhow!(
                    "storage inventory path contains a symlink: {}",
                    current.display()
                ));
            },
            Ok(_) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(any(test, feature = "test-fixtures"))]
fn compact_action(target: &str) -> StorageActionDescriptor {
    StorageActionDescriptor {
        id: format!("compact_{target}"),
        label: "Compact safely".to_string(),
        description: "Copy into a fresh database, verify every table/schema/index/view, then atomically swap.".to_string(),
        confirmation: Some("COMPACT DATABASE".to_string()),
        destructive: false,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
fn compact_parquet_action() -> StorageActionDescriptor {
    StorageActionDescriptor {
        id: "compact_parquet".to_string(),
        label: "Compact analytics".to_string(),
        description: "Merge completed batch-owned partitions and roll the active canonical LLM generation forward without deleting immutable facts."
            .to_string(),
        confirmation: Some("COMPACT PARQUET".to_string()),
        destructive: false,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn inventory_is_scope_bound_and_does_not_follow_symlinks() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let external = tempfile::tempdir().expect("external");
        let layout = ArtifactV2Workspace::new(temporary.path());
        let events = layout.analytics_root("owner", "default").join("events");
        let partition = events.join("dt=2026-07-01");
        std::fs::create_dir_all(&partition).expect("partition");
        std::fs::write(partition.join("batch_1.parquet"), vec![0_u8; 128]).expect("batch");
        std::fs::write(external.path().join("secret"), vec![0_u8; 4096]).expect("external");
        #[cfg(unix)]
        std::os::unix::fs::symlink(external.path(), events.join("redirected")).expect("symlink");

        let snapshot = build_snapshot(&layout, "owner", "default").expect("snapshot");
        let events = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == "events")
            .unwrap();
        assert_eq!(events.size_bytes, 128);
        assert_eq!(events.file_count, 1);
        assert_eq!(events.oldest_partition.as_deref(), Some("2026-07-01"));
    }

    /// The transport-log row must stat the file the writer actually writes.
    ///
    /// `ArtifactV2Workspace`'s segment sanitizer accepts a `.` in a principal;
    /// `transport_log`'s does not, and routes that scope to
    /// `_quarantine/_quarantine`. The row was assembled from `scope_root`, so
    /// for any such scope it stat'd a path that does not exist and reported a
    /// confident zero for a log that was growing elsewhere.
    #[test]
    fn the_transport_log_row_follows_the_writers_own_scope_sanitizer() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temporary.path());
        // A `.` is legal for the workspace layout and illegal for the log.
        let (principal, workspace) = ("user.name", "default");
        let written = crate::magician_v2::transport_log::scope_log_path(
            layout.base_root(),
            principal,
            workspace,
        );
        assert_ne!(
            written,
            layout.scope_root(principal, workspace).join("events.jsonl"),
            "this scope must be one the two sanitizers disagree about, or the test proves nothing"
        );
        std::fs::create_dir_all(written.parent().expect("parent")).expect("log dir");
        std::fs::write(&written, vec![b'x'; 512]).expect("transport log");

        let snapshot = build_snapshot(&layout, principal, workspace).expect("snapshot");
        let entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == "transport_log_jsonl")
            .expect("transport log row");

        assert_eq!(
            entry.size_bytes, 512,
            "the row must report the bytes at the path the writer uses"
        );
        assert_eq!(
            entry.wal_bytes, 0,
            "a journal has no write-ahead sidecar; nothing should be looked up"
        );
        assert_eq!(
            entry.retention_days,
            Some(TRANSPORT_LOG_RETENTION_DAYS),
            "the tightest retention window in the registry must not report as absent"
        );
    }

    /// `/storage`'s "Apply retention" must tier the spine, not skip it.
    ///
    /// The registry advertises `retention_days` of 7 and 396 on the two spine
    /// rows, but the action ran only `parquet_maintenance::apply_retention`,
    /// which excludes the spine by design. Pressing the button therefore did
    /// nothing for the only two datasets whose retention an operator cannot
    /// express through it — `retention_days` is clamped to `30..=3650`, so the
    /// 7-day detail window is not reachable as a parameter.
    #[test]
    fn applying_retention_tiers_the_activity_spine() {
        use crate::magician_v2::analytics::activity_rows_sink::{write_rows, ActivityRow};

        let temporary = tempfile::tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temporary.path());
        let detail_root = layout.analytics_activity_rows_root("owner", "default");
        let rollup_root = layout.analytics_activity_rollups_root("owner", "default");
        let today = Utc::now().date_naive();
        let expired =
            today - chrono::Duration::days(parquet_maintenance::ACTIVITY_DETAIL_RETENTION_DAYS);

        let started_at_ms = expired
            .and_hms_opt(3, 0, 0)
            .expect("valid hour")
            .and_utc()
            .timestamp_millis();
        write_rows(
            &detail_root,
            &[ActivityRow {
                activity_id: "1".to_string(),
                parent_activity_id: None,
                root_activity_id: "1".to_string(),
                name: "unit_of_work".to_string(),
                target: "magician::storage_governance_test".to_string(),
                kind: "background".to_string(),
                workload_class: Some("ambient".to_string()),
                priority: None,
                principal: "owner".to_string(),
                workspace: "default".to_string(),
                agent_id: None,
                thread_id: None,
                task_id: None,
                model: None,
                started_at_ms,
                duration_ms: 10,
                outcome: "success".to_string(),
                dt: expired.format("%Y-%m-%d").to_string(),
                hour: 3,
            }],
        );
        assert!(detail_root.join(format!("dt={expired}")).is_dir());

        // 90 days, the value the scheduled sweep uses and well inside the
        // handler's clamp. It says nothing about the spine's 7-day tier, which
        // is the point.
        let stats = apply_scope_retention(&layout, "owner", "default", 90).expect("retention");

        assert!(
            !detail_root.join(format!("dt={expired}")).exists(),
            "expired spine detail must be tiered away by the same action that expires everything else"
        );
        assert!(
            rollup_root.join(format!("dt={expired}")).is_dir(),
            "and it must be summarised into the rollup tier, never deleted outright"
        );
        assert!(
            stats.partitions_removed >= 1,
            "the action's report must account for the partition it removed"
        );
    }

    #[test]
    fn bounded_directory_inventory_reports_partial_results_at_its_entry_ceiling() {
        let temporary = tempfile::tempdir().expect("tempdir");
        std::fs::write(temporary.path().join("one"), b"one").expect("first file");
        std::fs::write(temporary.path().join("two"), b"two").expect("second file");

        let inventory = inventory_directory_with_limits(
            temporary.path(),
            Some(DirectoryInventoryLimits {
                max_entries: 1,
                max_depth: 8,
            }),
        )
        .expect("bounded inventory");

        assert!(!inventory.complete);
        assert_eq!(inventory.file_count, 1);
    }

    #[test]
    fn inventory_declares_mail_as_lifecycle_managed_without_retention_action() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temporary.path());
        let snapshot = build_snapshot(&layout, "owner", "default").expect("snapshot");
        let mail = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == "channel_assist_duckdb")
            .expect("mail entry");
        assert_eq!(mail.safety_class, StorageSafetyClass::LifecycleManaged);
        assert_eq!(mail.actions.len(), 1);
        assert_eq!(mail.actions[0].id, "compact_channel_assist");
        assert!(mail.retention_days.is_none());
    }

    #[test]
    fn inventory_counts_sqlite_wal_and_live_restricted_retention() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temporary.path());
        std::fs::write(
            layout.base_root().join("attention_funnel.db"),
            vec![0_u8; 10],
        )
        .expect("sqlite");
        std::fs::write(
            layout.base_root().join("attention_funnel.db-wal"),
            vec![0_u8; 7],
        )
        .expect("sqlite wal");

        let snapshot = build_snapshot_with_restricted_retention(
            &layout,
            "owner",
            "default",
            Some((14, 45, 120)),
        )
        .expect("snapshot");
        let sqlite = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == "attention_funnel_sqlite")
            .expect("sqlite entry");
        assert_eq!(sqlite.size_bytes, 10);
        assert_eq!(sqlite.wal_bytes, 7);
        assert_eq!(sqlite.file_count, 2);
        assert!(snapshot.total_size_bytes >= 17);
        assert_eq!(
            snapshot
                .entries
                .iter()
                .find(|entry| entry.id == "llm_call_io")
                .and_then(|entry| entry.retention_days),
            Some(14)
        );
        assert_eq!(
            snapshot
                .entries
                .iter()
                .find(|entry| entry.id == "llm_context_blocks")
                .and_then(|entry| entry.retention_days),
            Some(45)
        );
        assert_eq!(
            snapshot
                .entries
                .iter()
                .find(|entry| entry.id == "llm_content_access_audit")
                .and_then(|entry| entry.retention_days),
            Some(120)
        );
        assert_eq!(
            snapshot
                .entries
                .iter()
                .find(|entry| entry.id == "llm_content_tombstones")
                .and_then(|entry| entry.retention_days),
            Some(120)
        );
    }

    #[test]
    fn inventory_includes_scope_owned_browser_engine_observability() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temporary.path());
        let database = layout
            .scope_root("owner", "default")
            .join("analytics/browser_engine_usage.sqlite3");
        std::fs::create_dir_all(database.parent().unwrap()).expect("analytics dir");
        std::fs::write(&database, vec![0_u8; 11]).expect("browser analytics");
        std::fs::write(
            database.with_file_name("browser_engine_usage.sqlite3-wal"),
            vec![0_u8; 7],
        )
        .expect("browser analytics wal");

        let snapshot = build_snapshot(&layout, "owner", "default").expect("snapshot");
        let entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == "browser_engine_usage_sqlite")
            .expect("browser-engine inventory entry");
        assert_eq!(entry.safety_class, StorageSafetyClass::Observability);
        assert_eq!(
            entry.relative_path,
            "analytics/browser_engine_usage.sqlite3"
        );
        assert_eq!(entry.size_bytes, 11);
        assert_eq!(entry.wal_bytes, 7);
        assert!(entry.actions.is_empty());
    }

    #[test]
    fn dormant_app_inventory_is_scope_bound_inspect_only_and_does_not_materialize_storage() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temporary.path());
        let apps_root = layout.apps_root("owner", "default");

        let snapshot = build_snapshot(&layout, "owner", "default").expect("snapshot");
        let expected = [
            (
                "app_store_sqlite",
                StorageKind::Sqlite,
                StorageSafetyClass::Authoritative,
                "apps/app_store.sqlite3",
            ),
            (
                "app_packages",
                StorageKind::Directory,
                StorageSafetyClass::LifecycleManaged,
                "apps/packages",
            ),
            (
                "app_attachments",
                StorageKind::Directory,
                StorageSafetyClass::Restricted,
                "apps/attachments",
            ),
            (
                "app_exports",
                StorageKind::Directory,
                StorageSafetyClass::Restricted,
                "apps/exports",
            ),
            (
                "app_captures",
                StorageKind::Directory,
                StorageSafetyClass::Restricted,
                "apps/captures",
            ),
            (
                "app_evaluations",
                StorageKind::Directory,
                StorageSafetyClass::Restricted,
                "apps/evaluations",
            ),
        ];

        for (id, kind, safety, relative_path) in expected {
            let entry = snapshot
                .entries
                .iter()
                .find(|entry| entry.id == id)
                .unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(entry.kind, kind, "kind for {id}");
            assert_eq!(entry.safety_class, safety, "safety class for {id}");
            assert_eq!(entry.relative_path, relative_path, "path for {id}");
            assert_eq!(entry.size_bytes, 0, "apparent size for {id}");
            assert_eq!(entry.allocated_bytes, 0, "allocated size for {id}");
            assert_eq!(entry.wal_bytes, 0, "WAL size for {id}");
            assert_eq!(entry.file_count, 0, "file count for {id}");
            assert!(entry.actions.is_empty(), "generic action exposed for {id}");
        }
        assert!(
            !apps_root.exists(),
            "read-only inventory must not create dormant app storage"
        );
    }

    #[test]
    fn dormant_app_inventory_counts_sqlite_wal_and_real_files_without_following_links() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let external = tempfile::tempdir().expect("external");
        let layout = ArtifactV2Workspace::new(temporary.path());
        let database = layout.app_store_db_path("owner", "default");
        let packages = layout.app_packages_root("owner", "default");
        std::fs::create_dir_all(&packages).expect("packages");
        std::fs::write(&database, vec![0_u8; 13]).expect("app database");
        std::fs::write(
            database.with_file_name("app_store.sqlite3-wal"),
            vec![0_u8; 5],
        )
        .expect("app database wal");
        std::fs::write(
            database.with_file_name("app_store.sqlite3-shm"),
            vec![0_u8; 7],
        )
        .expect("app database shared memory");
        std::fs::write(packages.join("package.bin"), vec![0_u8; 17]).expect("package");
        std::fs::write(external.path().join("secret"), vec![0_u8; 4096]).expect("external");
        #[cfg(unix)]
        std::os::unix::fs::symlink(external.path(), packages.join("redirected")).expect("symlink");

        let snapshot = build_snapshot(&layout, "owner", "default").expect("snapshot");
        let database = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == "app_store_sqlite")
            .expect("app store entry");
        assert_eq!(database.size_bytes, 13);
        assert_eq!(database.wal_bytes, 5);
        assert_eq!(database.shm_bytes, 7);
        assert_eq!(database.file_count, 3);
        assert_eq!(database.actions.len(), 3);
        assert!(database
            .actions
            .iter()
            .all(|action| action.id.starts_with("app_store_") && !action.destructive));
        let packages = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == "app_packages")
            .expect("app packages entry");
        assert_eq!(packages.size_bytes, 17);
        assert_eq!(packages.file_count, 1);
        assert!(packages.inventory_complete);
        assert!(packages.actions.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn dormant_app_inventory_rejects_a_symlinked_storage_root() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let external = tempfile::tempdir().expect("external");
        let layout = ArtifactV2Workspace::new(temporary.path());
        let apps_root = layout.apps_root("owner", "default");
        std::fs::create_dir_all(&apps_root).expect("apps root");
        std::fs::write(external.path().join("secret"), vec![0_u8; 4096]).expect("external");
        std::os::unix::fs::symlink(
            external.path(),
            layout.app_attachments_root("owner", "default"),
        )
        .expect("symlink");

        let error = build_snapshot(&layout, "owner", "default").expect_err("symlink root");
        assert!(
            error
                .to_string()
                .contains("storage inventory path contains a symlink"),
            "unexpected error: {error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dormant_app_inventory_rejects_a_symlinked_apps_ancestor() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let external = tempfile::tempdir().expect("external");
        let layout = ArtifactV2Workspace::new(temporary.path());
        let scope_root = layout.scope_root("owner", "default");
        std::fs::create_dir_all(&scope_root).expect("scope root");
        std::fs::create_dir_all(external.path().join("packages")).expect("external packages");
        std::fs::write(external.path().join("packages/secret"), vec![0_u8; 4096])
            .expect("external");
        std::os::unix::fs::symlink(external.path(), layout.apps_root("owner", "default"))
            .expect("apps symlink");

        let error = build_snapshot(&layout, "owner", "default").expect_err("ancestor symlink");
        assert!(
            error
                .to_string()
                .contains("storage inventory path contains a symlink"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn compaction_reports_persist_per_area_metrics_and_inventory_footprint() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temporary.path());
        let report = StorageMaintenanceReport {
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            started_at_ms: 100,
            completed_at_ms: 125,
            duckdb: Vec::new(),
            parquet: Some(parquet_maintenance::ParquetCompactionStats {
                partitions_compacted: 1,
                raw_files_compacted: 8,
                raw_files_pruned: 8,
                rows_compacted: 80,
                files_before: 9,
                files_after: 2,
                bytes_before: 4_096,
                bytes_after: 1_024,
                bytes_reclaimed: 3_072,
                areas: vec![parquet_maintenance::ParquetCompactionAreaStats {
                    dataset: "events".to_string(),
                    partitions_compacted: 1,
                    files_before: 9,
                    files_after: 2,
                    raw_files_compacted: 8,
                    raw_files_pruned: 8,
                    rows_compacted: 80,
                    bytes_before: 4_096,
                    bytes_after: 1_024,
                    bytes_reclaimed: 3_072,
                    query_files_avoided: 7,
                }],
                ..Default::default()
            }),
            canonical_llm: None,
            retention: None,
        };
        record_compaction_metrics(
            &layout,
            compaction_metrics::CompactionMetricTrigger::ScheduledFull,
            &report,
        );

        let snapshot = build_snapshot(&layout, "owner", "default").expect("snapshot");
        assert_eq!(snapshot.compaction_metrics.event_count, 1);
        assert_eq!(snapshot.compaction_metrics.total_files_compacted, 8);
        assert_eq!(snapshot.compaction_metrics.total_query_files_avoided, 7);
        assert_eq!(snapshot.compaction_metrics.total_bytes_reclaimed, 3_072);
        assert!(snapshot.compaction_metrics.storage_bytes > 0);
        let entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == "compaction_metrics")
            .expect("metrics inventory entry");
        assert_eq!(entry.size_bytes, snapshot.compaction_metrics.storage_bytes);
        assert_eq!(entry.row_count, Some(1));
        assert_eq!(entry.actions[0].id, "clear_compaction_metrics");
    }
}

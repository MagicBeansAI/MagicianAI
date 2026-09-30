//! Query-triggered compatibility wrapper around verified Parquet maintenance.
//!
//! Completed `memory_events` partitions are compacted to a generation-named
//! root-level Parquet object and their checksum/row-count-verified raw batches
//! are pruned. Keeping the active object at the partition root preserves legacy `dt=*/*.parquet`
//! readers while the explicit selector below prevents interrupted-prune double
//! counting.

use std::path::{Path, PathBuf};

use tracing::warn;

use super::parquet_maintenance::{compact_dataset_since, compacted_file, PartitionedDataset};

const COMPACT_MIN_RAW_FILES: usize = 64;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryEventsCompactionStats {
    pub partitions_scanned: usize,
    pub partitions_compacted: usize,
    pub partitions_skipped: usize,
    pub raw_files_compacted: usize,
    pub raw_files_pruned: usize,
    pub rows_compacted: usize,
    pub bytes_reclaimed: u64,
}

pub fn compacted_partition_file(partition_dir: &Path) -> PathBuf {
    compacted_file(partition_dir, PartitionedDataset::MemoryEvents)
}

pub fn memory_events_raw_partition_files(partition_dir: &Path) -> Vec<PathBuf> {
    let mut files = match std::fs::read_dir(partition_dir) {
        Ok(entries) => entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let regular = entry.file_type().ok().is_some_and(|kind| kind.is_file());
                (regular
                    && path.extension().and_then(|extension| extension.to_str()) == Some("parquet")
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("batch_")))
                .then_some(path)
            })
            .collect::<Vec<_>>(),
        Err(_) => Vec::new(),
    };
    files.sort();
    files
}

pub fn compact_completed_partitions_for_query(
    root: &Path,
    lookback_days: Option<i64>,
) -> MemoryEventsCompactionStats {
    compact_completed_partitions_for_query_with_min_files(
        root,
        lookback_days,
        COMPACT_MIN_RAW_FILES,
    )
}

pub fn compact_completed_partitions_for_query_with_min_files(
    root: &Path,
    lookback_days: Option<i64>,
    min_raw_files: usize,
) -> MemoryEventsCompactionStats {
    let Some(analytics_root) = root.parent() else {
        return MemoryEventsCompactionStats::default();
    };
    let Some(workspace_root) = analytics_root.parent() else {
        return MemoryEventsCompactionStats::default();
    };
    let Some(principal_root) = workspace_root.parent() else {
        return MemoryEventsCompactionStats::default();
    };
    let Some(scopes_root) = principal_root.parent() else {
        return MemoryEventsCompactionStats::default();
    };
    let Some(base_root) = scopes_root.parent() else {
        return MemoryEventsCompactionStats::default();
    };
    let Some(principal) = principal_root.file_name().and_then(|value| value.to_str()) else {
        return MemoryEventsCompactionStats::default();
    };
    let Some(workspace) = workspace_root.file_name().and_then(|value| value.to_str()) else {
        return MemoryEventsCompactionStats::default();
    };
    let layout = crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(base_root);
    let oldest_partition = lookback_days.map(|days| {
        chrono::Utc::now().date_naive() - chrono::Duration::days(days.max(0).saturating_add(1))
    });
    match compact_dataset_since(
        &layout,
        principal,
        workspace,
        PartitionedDataset::MemoryEvents,
        min_raw_files,
        oldest_partition,
    ) {
        Ok(stats) => MemoryEventsCompactionStats {
            partitions_scanned: stats.partitions_scanned,
            partitions_compacted: stats.partitions_compacted,
            partitions_skipped: stats
                .partitions_already_compacted
                .saturating_add(stats.partitions_below_threshold)
                .saturating_add(stats.partitions_failed),
            raw_files_compacted: stats.raw_files_compacted,
            raw_files_pruned: stats.raw_files_pruned,
            rows_compacted: usize::try_from(stats.rows_compacted).unwrap_or(usize::MAX),
            bytes_reclaimed: stats.bytes_reclaimed,
        },
        Err(error) => {
            warn!(
                target: "analytics::memory_events_compactor",
                root = %root.display(),
                error = %error,
                "query-triggered memory-event compaction failed; raw batches remain authoritative"
            );
            MemoryEventsCompactionStats::default()
        },
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use chrono::{Duration as ChronoDuration, Utc};
    use duckdb::Connection;

    use super::*;
    use crate::magician_v2::analytics::duckdb_safety::configure_analytics_connection_checked;

    fn write_memory_events_batch(path: &Path, first_id: i64, count: i64) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let connection = Connection::open_in_memory().expect("duckdb");
        configure_analytics_connection_checked(&connection, "memory_events_compactor_test")
            .expect("configure DuckDB");
        connection
            .execute_batch("CREATE TABLE memory_events (event_id BIGINT, event_kind VARCHAR);")
            .expect("create table");
        for offset in 0..count {
            connection
                .execute(
                    "INSERT INTO memory_events VALUES (?, 'retrieval');",
                    [first_id + offset],
                )
                .expect("insert row");
        }
        connection
            .execute_batch(&format!(
                "COPY memory_events TO '{}' (FORMAT PARQUET);",
                path.display().to_string().replace('\'', "''")
            ))
            .expect("write parquet");
    }

    #[test]
    fn raw_partition_files_ignore_compacted_outputs() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let partition = temporary.path().join("dt=2026-07-01");
        std::fs::create_dir_all(&partition).expect("partition");
        std::fs::write(partition.join("batch_001.parquet"), b"raw").expect("raw");
        std::fs::write(compacted_partition_file(&partition), b"compact").expect("compact");

        assert_eq!(
            memory_events_raw_partition_files(&partition),
            vec![partition.join("batch_001.parquet")]
        );
    }

    #[test]
    fn query_compaction_prunes_raw_after_verified_publication() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let layout =
            crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(temporary.path());
        let date = Utc::now().date_naive() - ChronoDuration::days(1);
        let root = layout.analytics_memory_events_root("owner", "default");
        let partition = root.join(format!("dt={date}"));
        {
            let _guard = crate::magician_v2::analytics::duckdb_safety::analytics_duckdb_guard();
            write_memory_events_batch(&partition.join("batch_001.parquet"), 1, 2);
            write_memory_events_batch(&partition.join("batch_002.parquet"), 3, 1);
        }

        let stats = compact_completed_partitions_for_query_with_min_files(&root, Some(7), 1);
        assert_eq!(stats.partitions_compacted, 1);
        assert_eq!(stats.raw_files_pruned, 2);
        assert!(compacted_partition_file(&partition).is_file());
        assert!(memory_events_raw_partition_files(&partition).is_empty());
    }

    #[test]
    fn query_triggered_compaction_respects_the_requested_lookback() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let layout =
            crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(temporary.path());
        let root = layout.analytics_memory_events_root("owner", "default");
        let recent = root.join(format!(
            "dt={}",
            Utc::now().date_naive() - ChronoDuration::days(1)
        ));
        let old = root.join(format!(
            "dt={}",
            Utc::now().date_naive() - ChronoDuration::days(30)
        ));
        {
            let _guard = crate::magician_v2::analytics::duckdb_safety::analytics_duckdb_guard();
            for partition in [&recent, &old] {
                write_memory_events_batch(&partition.join("batch_001.parquet"), 1, 1);
                write_memory_events_batch(&partition.join("batch_002.parquet"), 2, 1);
            }
        }

        let stats = compact_completed_partitions_for_query_with_min_files(&root, Some(7), 1);
        assert_eq!(stats.partitions_compacted, 1);
        assert!(memory_events_raw_partition_files(&recent).is_empty());
        assert_eq!(memory_events_raw_partition_files(&old).len(), 2);
    }
}

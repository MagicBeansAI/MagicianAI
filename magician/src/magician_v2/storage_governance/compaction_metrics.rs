//! Bounded, content-free history for physical storage compaction.
//!
//! The ledger is deliberately separate from the stores it describes. Clearing
//! it can never alter a database, Parquet partition, journal, or retention
//! policy. A single atomic JSON object keeps its own footprint inspectable and
//! caps history growth without introducing another database that itself needs
//! compaction.

use std::{collections::BTreeMap, path::PathBuf, sync::Mutex};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::magician_v2::artifact_v2::{service::ArtifactV2Error, workspace::ArtifactV2Workspace};

const LEDGER_SCHEMA_VERSION: u16 = 1;
pub const COMPACTION_METRICS_HISTORY_LIMIT: usize = 256;
pub const COMPACTION_METRICS_RECENT_LIMIT: usize = 24;
const MAX_LEDGER_BYTES: u64 = 4 * 1024 * 1024;
static METRICS_WRITE_GUARD: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionMetricTrigger {
    Manual,
    Startup,
    ScheduledFull,
    ScheduledHot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionMetricKind {
    DuckDb,
    Parquet,
    CanonicalLlm,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionMetricEvent {
    pub id: String,
    pub completed_at_ms: i64,
    pub trigger: CompactionMetricTrigger,
    pub kind: CompactionMetricKind,
    pub area: String,
    pub partitions_compacted: usize,
    pub files_before: usize,
    pub files_after: usize,
    pub files_compacted: usize,
    pub query_files_avoided: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub bytes_reclaimed: u64,
    pub compacted_bytes_written: u64,
    pub rows_compacted: u64,
    pub duration_ms: u64,
}

impl CompactionMetricEvent {
    pub fn new(
        trigger: CompactionMetricTrigger,
        kind: CompactionMetricKind,
        area: impl Into<String>,
        completed_at_ms: i64,
    ) -> Self {
        Self {
            id: ulid::Ulid::new().to_string(),
            completed_at_ms,
            trigger,
            kind,
            area: area.into(),
            partitions_compacted: 0,
            files_before: 0,
            files_after: 0,
            files_compacted: 0,
            query_files_avoided: 0,
            bytes_before: 0,
            bytes_after: 0,
            bytes_reclaimed: 0,
            compacted_bytes_written: 0,
            rows_compacted: 0,
            duration_ms: 0,
        }
    }

    pub fn changed_storage(&self) -> bool {
        self.partitions_compacted > 0
            || self.files_compacted > 0
            || self.bytes_reclaimed > 0
            || self.compacted_bytes_written > 0
            || self.kind == CompactionMetricKind::DuckDb
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct CompactionMetricsLedger {
    schema_version: u16,
    events: Vec<CompactionMetricEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionAreaMetrics {
    pub kind: CompactionMetricKind,
    pub area: String,
    pub runs: usize,
    pub partitions_compacted: usize,
    pub files_compacted: usize,
    pub query_files_avoided: usize,
    pub bytes_reclaimed: u64,
    pub compacted_bytes_written: u64,
    pub rows_compacted: u64,
    pub last_compacted_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionMetricsSnapshot {
    pub schema_version: u16,
    pub healthy: bool,
    pub warning: Option<String>,
    pub storage_bytes: u64,
    pub allocated_bytes: u64,
    pub event_count: usize,
    pub retained_event_limit: usize,
    pub total_bytes_reclaimed: u64,
    pub total_files_compacted: usize,
    pub total_query_files_avoided: usize,
    pub total_compacted_bytes_written: u64,
    pub last_compaction_at_ms: Option<i64>,
    pub areas: Vec<CompactionAreaMetrics>,
    pub recent_events: Vec<CompactionMetricEvent>,
}

pub fn metrics_path(workspace: &ArtifactV2Workspace, principal: &str, scope: &str) -> PathBuf {
    crate::magician_v2::system_owners::system_file_path(
        workspace,
        principal,
        scope,
        crate::magician_v2::system_owners::SystemOwner::CompactionMetrics,
    )
}

pub fn append_events(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    scope: &str,
    events: impl IntoIterator<Item = CompactionMetricEvent>,
) -> Result<CompactionMetricsSnapshot> {
    validate_scope(principal, scope)?;
    let mut events = events
        .into_iter()
        .filter(CompactionMetricEvent::changed_storage)
        .collect::<Vec<_>>();
    if events.is_empty() {
        return snapshot(workspace, principal, scope);
    }
    let _guard = METRICS_WRITE_GUARD
        .lock()
        .map_err(|_| anyhow!("compaction metrics writer lock poisoned"))?;
    let path = metrics_path(workspace, principal, scope);
    let (mut ledger, warning) = read_ledger(workspace, &path)?;
    if warning.is_some() {
        ledger = CompactionMetricsLedger::default();
    }
    ledger.schema_version = LEDGER_SCHEMA_VERSION;
    ledger.events.append(&mut events);
    if ledger.events.len() > COMPACTION_METRICS_HISTORY_LIMIT {
        let remove = ledger.events.len() - COMPACTION_METRICS_HISTORY_LIMIT;
        ledger.events.drain(..remove);
    }
    let bytes =
        serde_json::to_vec_pretty(&ledger).context("serialize compaction metrics ledger")?;
    if crate::magician_v2::system_owners::store_for_any_owner(workspace, &path).is_some() {
        crate::magician_v2::system_owners::persist_system_file_sync(workspace, &path, &bytes)
            .context("writing bounded compaction metrics ledger")?;
    } else {
        workspace
            .write_json_atomic_path_sync(&path, &ledger)
            .context("writing bounded compaction metrics ledger")?;
    }
    snapshot_from_ledger(&path, ledger, true, None)
}

pub fn snapshot(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    scope: &str,
) -> Result<CompactionMetricsSnapshot> {
    validate_scope(principal, scope)?;
    let path = metrics_path(workspace, principal, scope);
    let (ledger, warning) = read_ledger(workspace, &path)?;
    snapshot_from_ledger(&path, ledger, warning.is_none(), warning)
}

pub fn clear(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    scope: &str,
) -> Result<CompactionMetricsSnapshot> {
    validate_scope(principal, scope)?;
    let _guard = METRICS_WRITE_GUARD
        .lock()
        .map_err(|_| anyhow!("compaction metrics writer lock poisoned"))?;
    let path = metrics_path(workspace, principal, scope);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            std::fs::remove_file(&path)
                .with_context(|| format!("clearing compaction metrics {}", path.display()))?;
        },
        Ok(_) => {
            return Err(anyhow!(
                "refusing to clear non-regular compaction metrics path {}",
                path.display()
            ));
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(error.into()),
    }
    snapshot_from_ledger(&path, CompactionMetricsLedger::default(), true, None)
}

fn read_ledger(
    workspace: &ArtifactV2Workspace,
    path: &PathBuf,
) -> Result<(CompactionMetricsLedger, Option<String>)> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(anyhow!(
                "compaction metrics path is not a regular file: {}",
                path.display()
            ));
        },
        Ok(metadata) if metadata.len() > MAX_LEDGER_BYTES => {
            return Ok((
                CompactionMetricsLedger::default(),
                Some(format!(
                    "Metrics history exceeds the {} byte safety cap; clear it to recover.",
                    MAX_LEDGER_BYTES
                )),
            ));
        },
        Ok(_) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((CompactionMetricsLedger::default(), None));
        },
        Err(error) => return Err(error.into()),
    }
    match workspace.read_json_path_sync::<CompactionMetricsLedger, _>(path) {
        Ok(ledger) if ledger.schema_version == LEDGER_SCHEMA_VERSION => Ok((ledger, None)),
        Ok(_) => Ok((
            CompactionMetricsLedger::default(),
            Some("Unsupported metrics schema; clear history to reset it.".to_string()),
        )),
        Err(error)
            if matches!(
                &error,
                ArtifactV2Error::Serde(_) | ArtifactV2Error::InvalidRequest(_)
            ) =>
        {
            warn!(
                target: "storage_governance::compaction_metrics",
                path = %path.display(),
                error = %error,
                "compaction metrics ledger is malformed"
            );
            Ok((
                CompactionMetricsLedger::default(),
                Some(
                    "Metrics history is malformed or exceeds structural safety limits; clear it to recover."
                        .to_string(),
                ),
            ))
        },
        Err(error) => Err(error.into()),
    }
}

fn snapshot_from_ledger(
    path: &PathBuf,
    ledger: CompactionMetricsLedger,
    healthy: bool,
    warning: Option<String>,
) -> Result<CompactionMetricsSnapshot> {
    let (storage_bytes, allocated_bytes) = file_footprint(path)?;
    let mut areas = BTreeMap::<(CompactionMetricKind, String), CompactionAreaMetrics>::new();
    for event in &ledger.events {
        let area = areas
            .entry((event.kind, event.area.clone()))
            .or_insert_with(|| CompactionAreaMetrics {
                kind: event.kind,
                area: event.area.clone(),
                runs: 0,
                partitions_compacted: 0,
                files_compacted: 0,
                query_files_avoided: 0,
                bytes_reclaimed: 0,
                compacted_bytes_written: 0,
                rows_compacted: 0,
                last_compacted_at_ms: 0,
            });
        area.runs += 1;
        area.partitions_compacted += event.partitions_compacted;
        area.files_compacted += event.files_compacted;
        area.query_files_avoided += event.query_files_avoided;
        area.bytes_reclaimed = area.bytes_reclaimed.saturating_add(event.bytes_reclaimed);
        area.compacted_bytes_written = area
            .compacted_bytes_written
            .saturating_add(event.compacted_bytes_written);
        area.rows_compacted = area.rows_compacted.saturating_add(event.rows_compacted);
        area.last_compacted_at_ms = area.last_compacted_at_ms.max(event.completed_at_ms);
    }
    let mut areas = areas.into_values().collect::<Vec<_>>();
    areas.sort_by(|left, right| {
        right
            .bytes_reclaimed
            .cmp(&left.bytes_reclaimed)
            .then_with(|| right.query_files_avoided.cmp(&left.query_files_avoided))
            .then_with(|| left.area.cmp(&right.area))
    });
    let mut recent_events = ledger.events.iter().rev().cloned().collect::<Vec<_>>();
    recent_events.truncate(COMPACTION_METRICS_RECENT_LIMIT);
    Ok(CompactionMetricsSnapshot {
        schema_version: LEDGER_SCHEMA_VERSION,
        healthy,
        warning,
        storage_bytes,
        allocated_bytes,
        event_count: ledger.events.len(),
        retained_event_limit: COMPACTION_METRICS_HISTORY_LIMIT,
        total_bytes_reclaimed: ledger
            .events
            .iter()
            .map(|event| event.bytes_reclaimed)
            .sum(),
        total_files_compacted: ledger
            .events
            .iter()
            .map(|event| event.files_compacted)
            .sum(),
        total_query_files_avoided: ledger
            .events
            .iter()
            .map(|event| event.query_files_avoided)
            .sum(),
        total_compacted_bytes_written: ledger
            .events
            .iter()
            .map(|event| event.compacted_bytes_written)
            .sum(),
        last_compaction_at_ms: ledger.events.last().map(|event| event.completed_at_ms),
        areas,
        recent_events,
    })
}

fn file_footprint(path: &PathBuf) -> Result<(u64, u64)> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            Ok((metadata.len(), allocated_bytes(&metadata)))
        },
        Ok(_) => Err(anyhow!(
            "compaction metrics path is not a regular file: {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((0, 0)),
        Err(error) => Err(error.into()),
    }
}

fn validate_scope(principal: &str, scope: &str) -> Result<()> {
    let scope = magicllm::LlmScope::new(principal.to_string(), scope.to_string());
    if !scope.is_valid() {
        return Err(anyhow!("compaction metrics require a safe explicit scope"));
    }
    Ok(())
}

#[cfg(unix)]
fn allocated_bytes(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated_bytes(metadata: &std::fs::Metadata) -> u64 {
    metadata.len()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use chrono::Utc;

    fn event(index: usize) -> CompactionMetricEvent {
        let mut event = CompactionMetricEvent::new(
            CompactionMetricTrigger::ScheduledHot,
            CompactionMetricKind::CanonicalLlm,
            "llm_calls",
            Utc::now().timestamp_millis() + index as i64,
        );
        event.files_before = 65;
        event.files_after = 1;
        event.files_compacted = 64;
        event.query_files_avoided = 64;
        event.compacted_bytes_written = 1024;
        event.rows_compacted = 64;
        event
    }

    #[test]
    fn ledger_is_bounded_aggregated_and_clearable_without_touching_scope_data() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let protected = workspace
            .scope_root("owner", "default")
            .join("protected.txt");
        workspace
            .write_path_sync(&protected, b"keep")
            .expect("protected fixture");
        let snapshot = append_events(
            &workspace,
            "owner",
            "default",
            (0..COMPACTION_METRICS_HISTORY_LIMIT + 7).map(event),
        )
        .expect("append metrics");
        assert_eq!(snapshot.event_count, COMPACTION_METRICS_HISTORY_LIMIT);
        assert_eq!(
            snapshot.total_query_files_avoided,
            COMPACTION_METRICS_HISTORY_LIMIT * 64
        );
        assert_eq!(snapshot.areas.len(), 1);
        assert!(snapshot.storage_bytes > 0);

        let cleared = clear(&workspace, "owner", "default").expect("clear metrics");
        assert_eq!(cleared.event_count, 0);
        assert_eq!(cleared.storage_bytes, 0);
        assert_eq!(
            std::fs::read(protected).expect("protected remains"),
            b"keep"
        );
    }

    #[test]
    fn no_op_events_do_not_grow_the_ledger() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let event = CompactionMetricEvent::new(
            CompactionMetricTrigger::ScheduledFull,
            CompactionMetricKind::Parquet,
            "events",
            Utc::now().timestamp_millis(),
        );
        let snapshot = append_events(&workspace, "owner", "default", [event]).expect("append");
        assert_eq!(snapshot.event_count, 0);
        assert_eq!(snapshot.storage_bytes, 0);
    }

    #[test]
    fn malformed_history_is_visible_and_next_effective_write_recovers_atomically() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let path = metrics_path(&workspace, "owner", "default");
        workspace
            .write_path_sync(&path, b"{not-json")
            .expect("malformed fixture");

        let unhealthy = snapshot(&workspace, "owner", "default").expect("degraded snapshot");
        assert!(!unhealthy.healthy);
        assert!(unhealthy.warning.is_some());
        assert!(unhealthy.storage_bytes > 0);

        let recovered = append_events(&workspace, "owner", "default", [event(1)])
            .expect("effective write resets malformed auxiliary history");
        assert!(recovered.healthy);
        assert!(recovered.warning.is_none());
        assert_eq!(recovered.event_count, 1);
    }

    #[test]
    fn structurally_overdeep_history_is_degraded_without_parsing_and_recovers_on_write() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let path = metrics_path(&workspace, "owner", "default");
        let depth = crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH + 1;
        let history = format!("{}null{}", "[".repeat(depth), "]".repeat(depth));
        workspace
            .write_path_sync(&path, history.as_bytes())
            .expect("overdeep fixture");

        let unhealthy = snapshot(&workspace, "owner", "default").expect("degraded snapshot");
        assert!(!unhealthy.healthy);
        assert!(unhealthy
            .warning
            .as_deref()
            .is_some_and(|warning| warning.contains("structural safety limits")));
        assert!(unhealthy.storage_bytes > 0);

        let recovered = append_events(&workspace, "owner", "default", [event(2)])
            .expect("effective write atomically replaces overdeep auxiliary history");
        assert!(recovered.healthy);
        assert!(recovered.warning.is_none());
        assert_eq!(recovered.event_count, 1);
    }
}

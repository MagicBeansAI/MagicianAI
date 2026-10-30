//! Verified maintenance for append-only, date-partitioned analytics Parquet.
//!
//! Completed partitions are rewritten into one generation-named root-level
//! compacted object. A checksum + row-count manifest selects the new generation
//! before raw batches or the prior generation are pruned.
//! Readers that enumerate `dt=*/*.parquet` continue to see the compacted object;
//! governed readers can use the manifest to ignore any raw files left by an
//! interrupted prune.
//! The active day of the legacy LLM `batch_*` stream rolls forward the same
//! way on the five-minute lane ([`compact_open_legacy_llm_partition`]).
//!
//! The runtime also schedules the canonical LLM compactor. The active UTC
//! partition and the previous day keep their raw revisions and use a rolling
//! compacted-prefix + raw-tail read. Days older than yesterday drop the raw
//! part files after the compact is verified. The active partition follows a
//! separate five-minute path.

#[cfg(unix)]
use std::{
    collections::{HashMap, VecDeque},
    sync::{LazyLock, Mutex},
    time::SystemTime,
};

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, Utc};
use duckdb::Connection;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use super::duckdb_safety::{
    configure_analytics_connection_checked, try_analytics_duckdb_guard_for,
};
use crate::magician_v2::{
    artifact_v2::workspace::ArtifactV2Workspace,
    chat::storage::FileChatStore,
    storage_governance::{
        compaction_metrics::CompactionMetricTrigger, record_compaction_metrics,
        StorageMaintenanceReport,
    },
};

const MANIFEST_SCHEMA_VERSION: u16 = 1;
const COMPACTION_GUARD_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_ANALYTICS_RETENTION_DAYS: u32 = 90;
const FULL_STORAGE_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const HOT_LLM_COMPACTION_INTERVAL: Duration = Duration::from_secs(5 * 60);
const CHAT_TRANSCRIPT_MAINTENANCE_BATCH: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartitionedDataset {
    Events,
    MemoryEvents,
    LegacyLlmCalls,
    LlmEmbeddings,
    LlmDispatch,
    /// The activity spine. Deliberately **absent from [`Self::ALL`]** — see the
    /// note there.
    ActivityRows,
}

impl PartitionedDataset {
    /// The datasets the generic scope sweep compacts.
    ///
    /// [`Self::ActivityRows`] is not here, and that is not an oversight.
    /// Everything in this list partitions one level deep (`dt=…/*.parquet`),
    /// which is what [`compact_dataset_since`] walks. The spine partitions two
    /// levels deep (`dt=…/hour=…/*.parquet`) and folds hours before it folds
    /// days, so it has its own driver in [`compact_activity_rows`]. Adding it
    /// here would find no `batch_` files at the day level and report a clean
    /// no-op forever, which is the worst of both — silent and wrong.
    pub const ALL: [Self; 5] = [
        Self::Events,
        Self::MemoryEvents,
        Self::LegacyLlmCalls,
        Self::LlmEmbeddings,
        Self::LlmDispatch,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Events => "events",
            Self::MemoryEvents => "memory_events",
            Self::LegacyLlmCalls => "legacy_llm_calls",
            Self::LlmEmbeddings => "llm_embeddings",
            Self::LlmDispatch => "llm_dispatch",
            Self::ActivityRows => "activity_rows",
        }
    }

    fn root(self, workspace: &ArtifactV2Workspace, principal: &str, scope: &str) -> PathBuf {
        match self {
            Self::Events => workspace.analytics_root(principal, scope).join("events"),
            Self::MemoryEvents => workspace.analytics_memory_events_root(principal, scope),
            Self::LegacyLlmCalls => workspace.analytics_llm_calls_root(principal, scope),
            Self::LlmEmbeddings => workspace.analytics_llm_embeddings_root(principal, scope),
            Self::LlmDispatch => workspace
                .analytics_root(principal, scope)
                .join("llm_dispatch"),
            Self::ActivityRows => workspace.analytics_activity_rows_root(principal, scope),
        }
    }

    fn is_raw_file_name(self, name: &str) -> bool {
        match self {
            // The historical LLM-call sink predates the `batch_` convention.
            // Treat every non-canonical, non-compacted Parquet file as legacy
            // input so maintenance never silently strands older captures.
            Self::LegacyLlmCalls => {
                !name.starts_with("part-") && !self.is_compacted_file_name(name)
            },
            Self::LlmEmbeddings => name.starts_with("embed_"),
            _ => name.starts_with("batch_"),
        }
    }

    fn compacted_file_name(self) -> String {
        format!("{}.compacted.parquet", self.as_str())
    }

    fn generation_file_name(self, generation: &ulid::Ulid) -> String {
        format!("{}.compacted.{generation}.parquet", self.as_str())
    }

    fn is_generation_file_name(self, name: &str) -> bool {
        let prefix = format!("{}.compacted.", self.as_str());
        name.strip_prefix(&prefix)
            .and_then(|value| value.strip_suffix(".parquet"))
            .is_some_and(|value| value.parse::<ulid::Ulid>().is_ok())
    }

    fn is_compacted_file_name(self, name: &str) -> bool {
        name == self.compacted_file_name() || self.is_generation_file_name(name)
    }

    /// How long a fold leaves its sources and the generation it superseded
    /// on disk. Readers of these datasets choose files outside the writer
    /// guard (or across several guarded reads), so deleting at publish turned
    /// a fold into a "file not found" for whoever chose just before it. The
    /// manifest's source list keeps the leftovers out of every governed read.
    fn source_retirement_grace(self) -> Option<Duration> {
        match self {
            Self::LegacyLlmCalls => Some(LEGACY_LLM_SOURCE_RETIREMENT_GRACE),
            _ => None,
        }
    }
}

/// See [`PartitionedDataset::source_retirement_grace`]: longer than any
/// governed read takes to choose and open its files.
pub const LEGACY_LLM_SOURCE_RETIREMENT_GRACE: Duration = Duration::from_secs(15 * 60);

/// Whether the sources `manifest` folded, and the generations it superseded,
/// may be deleted now. Derived from the durable manifest alone, so a restart
/// neither forgets a pending deletion nor deletes early.
fn manifest_sources_may_retire(
    dataset: PartitionedDataset,
    manifest: &PartitionCompactionManifest,
    now: DateTime<Utc>,
) -> bool {
    dataset.source_retirement_grace().is_none_or(|grace| {
        let grace_ms = i64::try_from(grace.as_millis()).unwrap_or(i64::MAX);
        now.timestamp_millis()
            .saturating_sub(manifest.generated_at_ms)
            >= grace_ms
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SourceFingerprint {
    file_name: String,
    byte_len: u64,
    checksum_blake3: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionCompactionManifest {
    schema_version: u16,
    dataset: PartitionedDataset,
    partition_date: String,
    generated_at_ms: i64,
    sources: Vec<SourceFingerprint>,
    source_row_count: u64,
    compacted_file_name: String,
    compacted_byte_len: u64,
    compacted_checksum_blake3: String,
    compacted_row_count: u64,
    #[serde(default)]
    raw_sources_pruned: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParquetCompactionStats {
    pub partitions_scanned: usize,
    pub partitions_compacted: usize,
    pub partitions_already_compacted: usize,
    pub partitions_below_threshold: usize,
    pub partitions_failed: usize,
    pub raw_files_compacted: usize,
    pub raw_files_pruned: usize,
    pub rows_compacted: u64,
    #[serde(default)]
    pub files_before: usize,
    #[serde(default)]
    pub files_after: usize,
    #[serde(default)]
    pub bytes_before: u64,
    #[serde(default)]
    pub bytes_after: u64,
    pub bytes_reclaimed: u64,
    #[serde(default)]
    pub query_files_avoided: usize,
    /// Raw inputs that could not be read on their own and were moved into the
    /// dataset's `_quarantine/` area so the rest of their partition could fold.
    /// Non-zero is an operator signal: each one has a durable JSON record.
    #[serde(default)]
    pub inputs_quarantined: usize,
    /// Partitions skipped this sweep because their last fold failed on the
    /// same inputs and the retry backoff has not expired.
    #[serde(default)]
    pub partitions_backed_off: usize,
    #[serde(default)]
    pub areas: Vec<ParquetCompactionAreaStats>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParquetCompactionAreaStats {
    pub dataset: String,
    pub partitions_compacted: usize,
    pub files_before: usize,
    pub files_after: usize,
    pub raw_files_compacted: usize,
    pub raw_files_pruned: usize,
    pub rows_compacted: u64,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub bytes_reclaimed: u64,
    pub query_files_avoided: usize,
}

impl ParquetCompactionStats {
    /// Fold another dataset's stats into this one, keeping the per-dataset
    /// breakdown in `areas`. Public because the spine is compacted by its own
    /// driver and the governance action has to merge the two results.
    pub fn absorb(&mut self, other: Self) {
        self.partitions_scanned += other.partitions_scanned;
        self.partitions_compacted += other.partitions_compacted;
        self.partitions_already_compacted += other.partitions_already_compacted;
        self.partitions_below_threshold += other.partitions_below_threshold;
        self.partitions_failed += other.partitions_failed;
        self.raw_files_compacted += other.raw_files_compacted;
        self.raw_files_pruned += other.raw_files_pruned;
        self.rows_compacted = self.rows_compacted.saturating_add(other.rows_compacted);
        self.files_before += other.files_before;
        self.files_after += other.files_after;
        self.bytes_before = self.bytes_before.saturating_add(other.bytes_before);
        self.bytes_after = self.bytes_after.saturating_add(other.bytes_after);
        self.bytes_reclaimed = self.bytes_reclaimed.saturating_add(other.bytes_reclaimed);
        self.query_files_avoided += other.query_files_avoided;
        self.inputs_quarantined += other.inputs_quarantined;
        self.partitions_backed_off += other.partitions_backed_off;
        self.areas.extend(other.areas);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionStats {
    pub partitions_scanned: usize,
    pub partitions_removed: usize,
    pub bytes_removed: u64,
}

pub fn compact_scope(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    scope: &str,
    min_raw_files: usize,
) -> Result<ParquetCompactionStats> {
    let mut total = ParquetCompactionStats::default();
    for dataset in PartitionedDataset::ALL {
        total.absorb(compact_dataset(
            workspace,
            principal,
            scope,
            dataset,
            min_raw_files,
        )?);
    }
    Ok(total)
}

pub fn compact_dataset(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    scope: &str,
    dataset: PartitionedDataset,
    min_raw_files: usize,
) -> Result<ParquetCompactionStats> {
    compact_dataset_since(workspace, principal, scope, dataset, min_raw_files, None)
}

pub fn compact_dataset_since(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    scope: &str,
    dataset: PartitionedDataset,
    min_raw_files: usize,
    oldest_partition: Option<NaiveDate>,
) -> Result<ParquetCompactionStats> {
    compact_dataset_selected(
        workspace,
        principal,
        scope,
        dataset,
        min_raw_files,
        PartitionWindow::Closed {
            oldest: oldest_partition,
        },
    )
}

/// Raw `batch_*` files the active UTC partition of the legacy LLM stream may
/// hold before the hot lane folds it: the canonical stream's rolling bound.
pub const LEGACY_LLM_OPEN_PARTITION_MAX_RAW_FILES: usize =
    super::llm_fact_compactor::DEFAULT_LLM_FACT_ROLLING_TAIL_FILES;
const LEGACY_LLM_OPEN_PARTITION_MAX_TAIL_AGE: Duration = Duration::from_secs(60 * 60);

/// When an open (today's) partition folds its raw tail into the next
/// compacted generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenPartitionPolicy {
    /// Fold as soon as this many raw batches are uncompacted.
    pub max_raw_files: usize,
    /// Fold any tail once the last fold (or, before one, the oldest batch) is
    /// this old.
    pub max_tail_age: Duration,
}

impl Default for OpenPartitionPolicy {
    fn default() -> Self {
        Self {
            max_raw_files: LEGACY_LLM_OPEN_PARTITION_MAX_RAW_FILES,
            max_tail_age: LEGACY_LLM_OPEN_PARTITION_MAX_TAIL_AGE,
        }
    }
}

impl OpenPartitionPolicy {
    fn is_due(
        &self,
        prior: Option<&PartitionCompactionManifest>,
        new_raw: &[PathBuf],
        now: DateTime<Utc>,
    ) -> bool {
        if new_raw.is_empty() {
            return false;
        }
        if new_raw.len() >= self.max_raw_files.max(1) {
            return true;
        }
        let since_ms = match prior {
            Some(manifest) => Some(manifest.generated_at_ms),
            None => new_raw
                .iter()
                .filter_map(|path| std::fs::metadata(path).ok()?.modified().ok())
                .min()
                .map(|modified| DateTime::<Utc>::from(modified).timestamp_millis()),
        };
        let max_age_ms = i64::try_from(self.max_tail_age.as_millis()).unwrap_or(i64::MAX);
        since_ms.is_some_and(|since| now.timestamp_millis().saturating_sub(since) >= max_age_ms)
    }
}

/// Which partitions one [`compact_dataset_selected`] pass may fold.
#[derive(Debug, Clone, Copy)]
enum PartitionWindow {
    /// Closed UTC days, optionally from `oldest` on: the full sweep.
    Closed { oldest: Option<NaiveDate> },
    /// The active UTC day and yesterday, once the policy says a tail is due.
    Open(OpenPartitionPolicy),
}

/// Fold the active UTC partition (and yesterday's) of the legacy LLM
/// `batch_*` stream.
///
/// The sink writes a batch every 30 s, so a busy day reached ~1,000 raw files
/// that stayed raw until the next full sweep after midnight, and every
/// `llm_calls` read bound each footer. This rolls them into the same
/// manifest-selected generation closed days use (prior generation + new
/// batches, verified) at most hourly or at
/// [`LEGACY_LLM_OPEN_PARTITION_MAX_RAW_FILES`]. Readers select the manifest's
/// generation plus raw files outside its source set, so a batch the sink
/// publishes mid-fold stays visible as tail and nothing is counted twice. The
/// folded batches and the old generation stay on disk for
/// [`LEGACY_LLM_SOURCE_RETIREMENT_GRACE`] after the new manifest is durable, so
/// a reader that chose them just before still opens them; a later pass of
/// this lane deletes them.
pub fn compact_open_legacy_llm_partition(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    scope: &str,
    policy: OpenPartitionPolicy,
) -> Result<ParquetCompactionStats> {
    // Two batches before a first fold saves a file, as for closed days.
    compact_dataset_selected(
        workspace,
        principal,
        scope,
        PartitionedDataset::LegacyLlmCalls,
        2,
        PartitionWindow::Open(policy),
    )
}

fn compact_dataset_selected(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    scope: &str,
    dataset: PartitionedDataset,
    min_raw_files: usize,
    window: PartitionWindow,
) -> Result<ParquetCompactionStats> {
    let root = dataset.root(workspace, principal, scope);
    let bytes_before = directory_apparent_bytes(&root)?;
    let files_before = directory_regular_file_count(&root)?;
    let mut stats = ParquetCompactionStats::default();
    for (date, partition) in partition_dirs(&root)? {
        let today = Utc::now().date_naive();
        let selected = match window {
            PartitionWindow::Closed { oldest } => {
                date < today && !oldest.is_some_and(|oldest| date < oldest)
            },
            // Yesterday too: its last hour and late writes fold, and its
            // folded sources retire, without waiting for the six-hour sweep.
            PartitionWindow::Open(_) => date == today || date.succ_opt() == Some(today),
        };
        if !selected {
            continue;
        }
        stats.partitions_scanned += 1;
        let Some(_guard) = try_analytics_duckdb_guard_for(COMPACTION_GUARD_TIMEOUT) else {
            stats.partitions_failed += 1;
            warn!(
                target: "analytics::parquet_maintenance",
                dataset = dataset.as_str(),
                partition = %partition.display(),
                "timed out waiting for analytics writer guard; partition was left untouched"
            );
            continue;
        };
        // Swept on every scanned partition, not only those that compact: a
        // partition below the threshold would otherwise keep a dead writer's
        // staging file until retention removes the whole day.
        if let Err(error) = remove_orphan_staging_files(&partition) {
            warn!(
                target: "analytics::parquet_maintenance",
                partition = %partition.display(),
                error = %error,
                "could not sweep orphaned staging files; the partition is still compacted"
            );
        }
        let raw_files = raw_partition_files(&partition, dataset)?;
        let manifest = read_valid_manifest(&partition, dataset)?;
        let (known_remaining, new_raw) = split_raw_against_manifest(&raw_files, manifest.as_ref())?;
        // Folded sources and superseded generations go once the manifest
        // that replaced them is past its dataset's grace; until then the
        // manifest's source list keeps them out of governed reads.
        let retire = manifest
            .as_ref()
            .is_some_and(|manifest| manifest_sources_may_retire(dataset, manifest, Utc::now()));
        let recovered_pruned = match manifest.as_ref() {
            Some(manifest) if retire => {
                prune_verified_sources(&known_remaining, &manifest.sources)?
            },
            _ => 0,
        };
        if let Some(manifest) = manifest.as_ref().filter(|_| retire) {
            cleanup_inactive_compacted_generations(&partition, dataset, manifest)?;
        }
        stats.raw_files_pruned += recovered_pruned;

        if manifest.is_some() && new_raw.is_empty() {
            if let Some(mut manifest) = manifest {
                let sources_gone = retire || known_remaining.is_empty();
                if sources_gone && (!manifest.raw_sources_pruned || recovered_pruned > 0) {
                    manifest.raw_sources_pruned = true;
                    write_manifest_atomic(&partition, dataset, &manifest)?;
                }
            }
            stats.partitions_already_compacted += 1;
            continue;
        }
        if manifest.is_none() && new_raw.len() < min_raw_files.max(1) {
            stats.partitions_below_threshold += 1;
            continue;
        }
        if let PartitionWindow::Open(policy) = window {
            if !policy.is_due(manifest.as_ref(), &new_raw, Utc::now()) {
                stats.partitions_below_threshold += 1;
                continue;
            }
        }
        let signature = fold_input_signature(manifest.as_ref(), &new_raw);
        if fold_backoff_pending(&partition, dataset, &signature, Utc::now()).is_some() {
            stats.partitions_backed_off += 1;
            continue;
        }
        let prior_files = usize::from(manifest.is_some());
        match attempt_verified_fold(&partition, dataset, manifest.as_ref(), &new_raw, &mut stats) {
            Ok(attempt) => {
                clear_fold_backoff(&partition, dataset);
                let Some((manifest, pruned)) = attempt.published else {
                    continue;
                };
                let query_files_before = prior_files + attempt.folded_inputs.len();
                stats.partitions_compacted += 1;
                stats.raw_files_compacted += attempt.folded_inputs.len();
                stats.raw_files_pruned += pruned;
                stats.rows_compacted = stats
                    .rows_compacted
                    .saturating_add(manifest.compacted_row_count);
                stats.query_files_avoided = stats
                    .query_files_avoided
                    .saturating_add(query_files_before.saturating_sub(1));
            },
            Err(error) => {
                stats.partitions_failed += 1;
                report_fold_failure(
                    &partition,
                    dataset,
                    &signature,
                    &error,
                    "verified Parquet compaction failed; source batches were retained",
                );
            },
        }
    }
    stats.files_before = files_before;
    stats.files_after = directory_regular_file_count(&root)?;
    stats.bytes_before = bytes_before;
    stats.bytes_after = directory_apparent_bytes(&root)?;
    stats.bytes_reclaimed = stats.bytes_before.saturating_sub(stats.bytes_after);
    if stats.partitions_compacted > 0 || stats.raw_files_pruned > 0 || stats.bytes_reclaimed > 0 {
        stats.areas.push(ParquetCompactionAreaStats {
            dataset: dataset.as_str().to_string(),
            partitions_compacted: stats.partitions_compacted,
            files_before: stats.files_before,
            files_after: stats.files_after,
            raw_files_compacted: stats.raw_files_compacted,
            raw_files_pruned: stats.raw_files_pruned,
            rows_compacted: stats.rows_compacted,
            bytes_before: stats.bytes_before,
            bytes_after: stats.bytes_after,
            bytes_reclaimed: stats.bytes_reclaimed,
            query_files_avoided: stats.query_files_avoided,
        });
    }
    Ok(stats)
}

fn compact_partition(
    partition: &Path,
    dataset: PartitionedDataset,
    prior: Option<&PartitionCompactionManifest>,
    new_raw: &[PathBuf],
) -> Result<(PartitionCompactionManifest, usize)> {
    if prior.is_none() && new_raw.is_empty() {
        return Err(anyhow!("cannot compact an empty Parquet partition"));
    }
    let connection =
        Connection::open_in_memory().context("opening in-memory DuckDB for Parquet maintenance")?;
    configure_analytics_connection_checked(&connection, "parquet_maintenance")
        .context("configuring Parquet maintenance DuckDB")?;

    let mut inputs = Vec::new();
    if let Some(prior) = prior {
        inputs.push(manifest_output(partition, prior));
    }
    inputs.extend_from_slice(new_raw);
    let input_sql = parquet_source_sql(&inputs);
    connection
        .execute_batch(&format!(
            "CREATE TABLE compacted_partition AS SELECT * FROM read_parquet({input_sql}, hive_partitioning = false, union_by_name = true);"
        ))
        .context(CompactionInputsUnreadable)?;
    let source_rows: i64 = connection
        .query_row("SELECT count(*) FROM compacted_partition", [], |row| {
            row.get(0)
        })
        .context("counting Parquet compaction inputs")?;
    if source_rows < 0 {
        return Err(anyhow!("negative Parquet source row count"));
    }

    let generation = ulid::Ulid::new();
    let compacted_file_name = dataset.generation_file_name(&generation);
    let final_path = partition.join(&compacted_file_name);
    let tmp = partition.join(format!(".{compacted_file_name}.tmp"));
    // The staging name carries this run's generation, so a crash before the
    // rename below leaves a file no later run will ever write to again.
    // `cleanup_inactive_compacted_generations` does not cover it — a staging
    // name is not a compacted-generation name — so sweep it here.
    remove_orphan_staging_files(partition)?;
    connection
        .execute_batch(&format!(
            "COPY compacted_partition TO '{}' (FORMAT PARQUET, COMPRESSION 'zstd');",
            escape_sql_literal(&tmp.display().to_string())
        ))
        .context("writing compacted Parquet object")?;
    sync_file(&tmp)?;
    let compacted_rows = count_parquet_rows(&connection, &tmp)?;
    if compacted_rows != source_rows {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow!(
            "Parquet row-count guard failed: source={source_rows}, compacted={compacted_rows}"
        ));
    }
    // Hand-rolled rather than the shared durable writer, correctly: the temp's
    // contents come from DuckDB's `COPY`, not from bytes this process holds, so
    // a byte-oriented writer cannot produce it. The durability properties are
    // all here — unique generation temp, `sync_file` above, this rename, and
    // the partition-directory sync below.
    std::fs::rename(&tmp, &final_path).with_context(|| {
        format!(
            "publishing compacted Parquet {} -> {}",
            tmp.display(),
            final_path.display()
        )
    })?;
    sync_directory(partition)?;
    crate::magician_v2::dataset_owners::publish_written_parquet(&final_path).with_context(
        || {
            format!(
                "publishing compacted Parquet through DatasetAccess {}",
                final_path.display()
            )
        },
    )?;

    let mut sources = source_fingerprints(new_raw)?;
    let deferred = dataset.source_retirement_grace().is_some();
    if deferred {
        // Sources of the prior fold still waiting out their grace are inside
        // the prior generation, hence inside this one: keep listing them, or
        // readers would count them a second time as new raw.
        if let Some(prior) = prior {
            let raw = raw_partition_files(partition, dataset)?;
            let (retained, _) = split_raw_against_manifest(&raw, Some(prior))?;
            sources.extend(source_fingerprints(&retained)?);
        }
    }
    let metadata = std::fs::metadata(&final_path)?;
    let mut manifest = PartitionCompactionManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        dataset,
        partition_date: partition_date_text(partition).unwrap_or_default(),
        generated_at_ms: Utc::now().timestamp_millis(),
        sources,
        source_row_count: u64::try_from(source_rows).unwrap_or(0),
        compacted_file_name,
        compacted_byte_len: metadata.len(),
        compacted_checksum_blake3: file_checksum(&final_path)?,
        compacted_row_count: u64::try_from(compacted_rows).unwrap_or(0),
        raw_sources_pruned: false,
    };
    write_manifest_atomic(partition, dataset, &manifest)?;
    if deferred {
        // A reader that chose the prior generation or these batches just
        // before this publish still finds them; a later pass retires them.
        sync_directory(partition)?;
        return Ok((manifest, 0));
    }
    let pruned = prune_verified_sources(new_raw, &manifest.sources)?;
    manifest.raw_sources_pruned = true;
    write_manifest_atomic(partition, dataset, &manifest)?;
    cleanup_inactive_compacted_generations(partition, dataset, &manifest)?;
    sync_directory(partition)?;
    Ok((manifest, pruned))
}

// ─────────────────────────────────────────────────────────────────────
// One bad input must not wedge a partition
// ─────────────────────────────────────────────────────────────────────
//
// Every fold reads its inputs in one `read_parquet([...])`, so a single object
// DuckDB cannot open fails the whole set. That is what wedged
// `activity_rows/dt=2026-10-02/hour=02` for weeks: one zero-byte `batch_` left
// by a writer that died mid-`COPY`, read and refused every five minutes, which
// in turn held the day fold and the seven-day roll-up behind it. Two things
// close it, and both leave the readable rows exactly where the verified fold
// protocol expects them:
//
// - after a combined read fails, each raw input is probed on its own and the
//   ones that cannot be read are *moved* — with a durable record — into the
//   dataset's `_quarantine/` area, outside every `dt=` partition, where neither
//   readers nor retention ever look; the fold then runs over the rest;
// - a failure that setting inputs aside cannot fix is retried with exponential
//   backoff keyed to the exact input set, rather than every sweep forever.

/// Marks the one step of [`compact_partition`] that reads its inputs, so a
/// failure there can be told apart from a failed write or a failed guard. It
/// is the only failure a single bad input can cause, and the only one
/// [`attempt_verified_fold`] answers by examining the inputs one by one.
#[derive(Debug, Clone, Copy)]
struct CompactionInputsUnreadable;

impl std::fmt::Display for CompactionInputsUnreadable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("reading Parquet compaction inputs")
    }
}

/// Where unreadable inputs are moved, relative to the dataset root.
///
/// Beside the `dt=` partitions rather than inside one, on purpose: retention
/// removes partitions with `remove_dir_all`, so anything set aside *inside* a
/// partition would be deleted with it. `partition_dirs` and the activity
/// readers select `dt=` names only, and the moved file loses its `.parquet`
/// extension, so no glob reads it either.
const QUARANTINE_DIR_NAME: &str = "_quarantine";

/// First retry delay after a failed fold; doubles per consecutive failure on
/// the same inputs.
const FOLD_RETRY_BASE_SECS: i64 = 5 * 60;

/// Longest delay between retries of a partition whose fold keeps failing —
/// the full-sweep cadence, so a stuck partition is still looked at every
/// full sweep.
const FOLD_RETRY_CAP_SECS: i64 = 6 * 60 * 60;

/// Consecutive failures on the same inputs after which a fold failure is
/// logged at `error` (roughly half a day of backed-off retries).
const FOLD_FAILURES_BEFORE_ERROR: u32 = 8;

/// The raw inputs a fold actually consumed, and what it published.
struct FoldAttempt {
    folded_inputs: Vec<PathBuf>,
    /// `None` when every new input was unreadable and was set aside, leaving
    /// nothing to fold.
    published: Option<(PartitionCompactionManifest, usize)>,
}

/// [`compact_partition`], plus setting aside inputs that cannot be read.
///
/// The happy path is one `compact_partition` call and nothing else. Only when
/// the combined read fails are the raw inputs probed one at a time; any that
/// fail on their own are quarantined and the fold is retried once over the
/// rest. Inputs that each read fine but fail together (a type conflict
/// `union_by_name` cannot reconcile) are not "unreadable": nothing is moved
/// and the error is returned for the caller's backoff.
///
/// The caller must hold the analytics writer guard.
fn attempt_verified_fold(
    partition: &Path,
    dataset: PartitionedDataset,
    prior: Option<&PartitionCompactionManifest>,
    inputs: &[PathBuf],
    stats: &mut ParquetCompactionStats,
) -> Result<FoldAttempt> {
    let error = match compact_partition(partition, dataset, prior, inputs) {
        Ok(published) => {
            return Ok(FoldAttempt {
                folded_inputs: inputs.to_vec(),
                published: Some(published),
            })
        },
        Err(error) => error,
    };
    if error.downcast_ref::<CompactionInputsUnreadable>().is_none() {
        return Err(error);
    }
    let unreadable = unreadable_raw_inputs(dataset, inputs);
    if unreadable.is_empty() {
        return Err(error);
    }
    let mut set_aside = Vec::with_capacity(unreadable.len());
    for (path, reason) in &unreadable {
        quarantine_unreadable_input(dataset, path, reason).with_context(|| {
            format!("setting aside unreadable Parquet input {}", path.display())
        })?;
        stats.inputs_quarantined += 1;
        set_aside.push(path.clone());
    }
    let readable: Vec<PathBuf> = inputs
        .iter()
        .filter(|path| !set_aside.contains(*path))
        .cloned()
        .collect();
    if readable.is_empty() {
        return Ok(FoldAttempt {
            folded_inputs: readable,
            published: None,
        });
    }
    let published = compact_partition(partition, dataset, prior, &readable)?;
    Ok(FoldAttempt {
        folded_inputs: readable,
        published: Some(published),
    })
}

/// After a combined read outside [`attempt_verified_fold`] failed, set aside
/// the raw inputs that cannot be read and say so on the returned error. The
/// caller still fails this pass; the next one runs without them.
fn with_unreadable_inputs_set_aside(
    error: anyhow::Error,
    dataset: PartitionedDataset,
    inputs: &[PathBuf],
) -> anyhow::Error {
    let unreadable = unreadable_raw_inputs(dataset, inputs);
    if unreadable.is_empty() {
        return error;
    }
    for (path, reason) in &unreadable {
        if let Err(quarantine_error) = quarantine_unreadable_input(dataset, path, reason) {
            return error.context(format!(
                "could not set aside unreadable Parquet input {}: {quarantine_error:#}",
                path.display()
            ));
        }
    }
    error.context(format!(
        "set aside {} unreadable Parquet input(s) into {QUARANTINE_DIR_NAME}/; the rest are \
         retried on the next sweep",
        unreadable.len()
    ))
}

/// Raw inputs that fail to read on their own, with the reason.
///
/// Each probe decodes every row through a fresh connection, and a failure is
/// confirmed by a second fresh probe, so a transient (memory pressure, a
/// racing open) is not mistaken for damage. Only raw batch names are probed:
/// a manifest-selected generation was checksum-verified when it was read, and
/// moving it would orphan its manifest.
///
/// When **no** raw input reads, the inputs are probably not the problem — the
/// environment is — so only inputs whose Parquet envelope is visibly broken
/// (shorter than a footer, or missing the `PAR1` magic) are reported. That is
/// the valve that stops a broken DuckDB from quarantining a whole partition.
fn unreadable_raw_inputs(
    dataset: PartitionedDataset,
    inputs: &[PathBuf],
) -> Vec<(PathBuf, String)> {
    let mut failing = Vec::new();
    let mut any_readable = false;
    for path in inputs {
        let is_raw = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| dataset.is_raw_file_name(name));
        if !is_raw {
            continue;
        }
        let Err(first) = probe_parquet_input(path) else {
            any_readable = true;
            continue;
        };
        if probe_parquet_input(path).is_ok() {
            any_readable = true;
            continue;
        }
        let envelope_broken = parquet_envelope_is_broken(path);
        failing.push((path.clone(), format!("{first:#}"), envelope_broken));
    }
    failing
        .into_iter()
        .filter(|(_, _, envelope_broken)| any_readable || *envelope_broken)
        .map(|(path, reason, _)| (path, reason))
        .collect()
}

/// Read every row of one Parquet object, discarding them.
fn probe_parquet_input(path: &Path) -> Result<()> {
    let connection =
        Connection::open_in_memory().context("opening DuckDB to probe a Parquet input")?;
    configure_analytics_connection_checked(&connection, "parquet_input_probe")
        .context("configuring the Parquet input probe")?;
    connection
        .execute_batch(&format!(
            "CREATE TEMP TABLE parquet_input_probe AS SELECT * FROM read_parquet('{}', hive_partitioning = false);",
            escape_sql_literal(&path.display().to_string())
        ))
        .with_context(|| format!("reading Parquet input {}", path.display()))?;
    Ok(())
}

/// Whether a file cannot be Parquet at all: shorter than the 12-byte minimum
/// (`PAR1`, footer length, `PAR1`) or missing either magic. A file that cannot
/// be opened is not evidence either way and reports `false`.
fn parquet_envelope_is_broken(path: &Path) -> bool {
    const MAGIC: [u8; 4] = *b"PAR1";
    let inspect = || -> std::io::Result<bool> {
        let mut file = File::open(path)?;
        if file.metadata()?.len() < 12 {
            return Ok(true);
        }
        let mut head = [0_u8; 4];
        file.read_exact(&mut head)?;
        let mut tail = [0_u8; 4];
        file.seek(SeekFrom::End(-4))?;
        file.read_exact(&mut tail)?;
        Ok(head != MAGIC || tail != MAGIC)
    };
    inspect().unwrap_or(false)
}

/// Why one input was set aside, written beside it and kept with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct QuarantinedInputRecord {
    schema_version: u16,
    dataset: PartitionedDataset,
    /// Where the object was, relative to the dataset root.
    original_path: String,
    /// Where it is now, relative to the dataset root.
    quarantined_path: String,
    byte_len: u64,
    checksum_blake3: String,
    reason: String,
    quarantined_at_ms: i64,
}

/// The dataset root a partition file belongs to: the parent of its nearest
/// `dt=` ancestor.
fn dataset_root_of(path: &Path) -> Option<&Path> {
    path.ancestors()
        .find(|ancestor| partition_date(ancestor).is_some())?
        .parent()
}

/// Move one unreadable input out of the readable name space, never deleting
/// it, and leave a durable record of what it was and why it moved.
///
/// `<root>/dt=D/hour=H/batch_X.parquet` becomes
/// `<root>/_quarantine/dt=D/hour=H/batch_X.parquet.unreadable-<ulid>` with a
/// `….json` record beside it. The ULID suffix means a second quarantine of
/// the same name can never overwrite the first. The caller must hold the
/// analytics writer guard.
fn quarantine_unreadable_input(
    dataset: PartitionedDataset,
    path: &Path,
    reason: &str,
) -> Result<PathBuf> {
    let root = dataset_root_of(path).ok_or_else(|| {
        anyhow!(
            "{} is not inside a dt= partition, so it has no quarantine area; it was left in place",
            path.display()
        )
    })?;
    let relative = path
        .strip_prefix(root)
        .context("locating a Parquet input inside its dataset")?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("non-UTF8 Parquet input name"))?;
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspecting {}", path.display()))?;
    if !metadata.file_type().is_file() {
        return Err(anyhow!(
            "refusing to quarantine non-regular Parquet input {}",
            path.display()
        ));
    }
    let quarantine_root = root.join(QUARANTINE_DIR_NAME);
    let destination_dir = match relative.parent() {
        Some(parent) => quarantine_root.join(parent),
        None => quarantine_root.clone(),
    };
    std::fs::create_dir_all(&destination_dir)
        .with_context(|| format!("creating {}", destination_dir.display()))?;
    if !std::fs::symlink_metadata(&quarantine_root)?
        .file_type()
        .is_dir()
    {
        return Err(anyhow!(
            "quarantine area {} is not a real directory",
            quarantine_root.display()
        ));
    }
    let fingerprint = source_fingerprint(path)?;
    let quarantined_name = format!("{file_name}.unreadable-{}", ulid::Ulid::new());
    let destination = destination_dir.join(&quarantined_name);
    let record = QuarantinedInputRecord {
        schema_version: MANIFEST_SCHEMA_VERSION,
        dataset,
        original_path: relative.display().to_string(),
        quarantined_path: destination
            .strip_prefix(root)
            .unwrap_or(destination.as_path())
            .display()
            .to_string(),
        byte_len: fingerprint.byte_len,
        checksum_blake3: fingerprint.checksum_blake3,
        reason: reason.to_string(),
        quarantined_at_ms: Utc::now().timestamp_millis(),
    };
    // The record lands first. A crash between the two leaves a record naming a
    // file that is still in place — which the next failing fold sets aside
    // again under a fresh name — never a moved file nothing explains.
    let record_path = destination_dir.join(format!("{quarantined_name}.json"));
    crate::magician_v2::artifact_v2::io::write_bytes_durably_sync(
        &record_path,
        &serde_json::to_vec_pretty(&record)?,
    )
    .with_context(|| format!("writing quarantine record {}", record_path.display()))?;
    // Move the existing input within one filesystem, preserving the exact bytes
    // that failed to read. The shared byte writer cannot move a file; both
    // affected directories are synced below, after the durable record above.
    std::fs::rename(path, &destination).with_context(|| {
        format!(
            "moving unreadable Parquet input {} -> {}",
            path.display(),
            destination.display()
        )
    })?;
    sync_directory(&destination_dir)?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    error!(
        target: "analytics::parquet_maintenance",
        dataset = dataset.as_str(),
        original = %path.display(),
        quarantined_as = %destination.display(),
        byte_len = record.byte_len,
        reason = %reason,
        "an unreadable Parquet input was set aside so the rest of its partition can fold; \
         its bytes and a record of why were kept for an operator"
    );
    Ok(destination)
}

/// Durable retry state for a partition whose fold failed.
///
/// Keyed to the input set: any change — a late batch, an input set aside, a
/// file repaired by hand — produces a new signature and an immediate retry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FoldBackoffState {
    schema_version: u16,
    input_signature: String,
    consecutive_failures: u32,
    last_failure_at_ms: i64,
    next_attempt_at_ms: i64,
    last_error: String,
}

fn fold_backoff_path(partition: &Path, dataset: PartitionedDataset) -> PathBuf {
    partition
        .join("_compact")
        .join(format!("{}.fold-backoff.json", dataset.as_str()))
}

/// `base × 2^(failures − 1)`, capped at the full-sweep cadence.
fn fold_retry_delay(consecutive_failures: u32) -> ChronoDuration {
    let doublings = consecutive_failures.saturating_sub(1).min(16);
    ChronoDuration::seconds(
        FOLD_RETRY_BASE_SECS
            .saturating_mul(1_i64 << doublings)
            .min(FOLD_RETRY_CAP_SECS),
    )
}

/// A cheap identity for a fold's input set: names and lengths, not contents.
fn fold_input_signature(prior: Option<&PartitionCompactionManifest>, inputs: &[PathBuf]) -> String {
    let mut entries: Vec<(String, u64)> = inputs
        .iter()
        .map(|path| {
            let len = std::fs::metadata(path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            (path.display().to_string(), len)
        })
        .collect();
    entries.sort();
    let mut hasher = blake3::Hasher::new();
    if let Some(prior) = prior {
        hasher.update(prior.compacted_file_name.as_bytes());
    }
    hasher.update(b"\0");
    for (name, len) in entries {
        hasher.update(name.as_bytes());
        hasher.update(b"\0");
        hasher.update(&len.to_le_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

/// Best effort: an unreadable state file is simply no backoff.
fn read_fold_backoff(partition: &Path, dataset: PartitionedDataset) -> Option<FoldBackoffState> {
    let raw = std::fs::read(fold_backoff_path(partition, dataset)).ok()?;
    serde_json::from_slice::<FoldBackoffState>(&raw)
        .ok()
        .filter(|state| state.schema_version == MANIFEST_SCHEMA_VERSION)
}

/// The backoff in force for exactly these inputs, if any.
fn fold_backoff_pending(
    partition: &Path,
    dataset: PartitionedDataset,
    signature: &str,
    now: DateTime<Utc>,
) -> Option<FoldBackoffState> {
    read_fold_backoff(partition, dataset).filter(|state| {
        state.input_signature == signature && now.timestamp_millis() < state.next_attempt_at_ms
    })
}

fn record_fold_failure(
    partition: &Path,
    dataset: PartitionedDataset,
    signature: &str,
    error: &anyhow::Error,
    now: DateTime<Utc>,
) -> FoldBackoffState {
    let previous = read_fold_backoff(partition, dataset)
        .filter(|state| state.input_signature == signature)
        .map(|state| state.consecutive_failures)
        .unwrap_or(0);
    let consecutive_failures = previous.saturating_add(1);
    let state = FoldBackoffState {
        schema_version: MANIFEST_SCHEMA_VERSION,
        input_signature: signature.to_string(),
        consecutive_failures,
        last_failure_at_ms: now.timestamp_millis(),
        next_attempt_at_ms: (now + fold_retry_delay(consecutive_failures)).timestamp_millis(),
        last_error: format!("{error:#}"),
    };
    if let Err(write_error) = write_fold_backoff(partition, dataset, &state) {
        warn!(
            target: "analytics::parquet_maintenance",
            partition = %partition.display(),
            error = %format!("{write_error:#}"),
            "could not record fold backoff; the partition is retried next sweep"
        );
    }
    state
}

fn write_fold_backoff(
    partition: &Path,
    dataset: PartitionedDataset,
    state: &FoldBackoffState,
) -> Result<()> {
    let path = fold_backoff_path(partition, dataset);
    let parent = path.parent().context("fold backoff path missing parent")?;
    std::fs::create_dir_all(parent)?;
    if !std::fs::symlink_metadata(parent)?.file_type().is_dir() {
        return Err(anyhow!("fold backoff parent is not a real directory"));
    }
    crate::magician_v2::artifact_v2::io::write_bytes_durably_sync(
        &path,
        &serde_json::to_vec_pretty(state)?,
    )
    .with_context(|| format!("writing fold backoff {}", path.display()))
}

fn clear_fold_backoff(partition: &Path, dataset: PartitionedDataset) {
    let path = fold_backoff_path(partition, dataset);
    match std::fs::remove_file(&path) {
        Ok(()) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => warn!(
            target: "analytics::parquet_maintenance",
            path = %path.display(),
            error = %error,
            "could not clear fold backoff after a successful fold"
        ),
    }
}

/// Record a failed fold's backoff and log it with the full error chain —
/// `warn` while it may still be transient, `error` once it has failed
/// [`FOLD_FAILURES_BEFORE_ERROR`] times on the same inputs.
fn report_fold_failure(
    partition: &Path,
    dataset: PartitionedDataset,
    signature: &str,
    error: &anyhow::Error,
    message: &str,
) {
    let state = record_fold_failure(partition, dataset, signature, error, Utc::now());
    let retry_in_secs = fold_retry_delay(state.consecutive_failures).num_seconds();
    let chain = format!("{error:#}");
    if state.consecutive_failures >= FOLD_FAILURES_BEFORE_ERROR {
        error!(
            target: "analytics::parquet_maintenance",
            dataset = dataset.as_str(),
            partition = %partition.display(),
            error = %chain,
            consecutive_failures = state.consecutive_failures,
            retry_in_secs,
            "{message}; it keeps failing on the same inputs and needs an operator"
        );
    } else {
        warn!(
            target: "analytics::parquet_maintenance",
            dataset = dataset.as_str(),
            partition = %partition.display(),
            error = %chain,
            consecutive_failures = state.consecutive_failures,
            retry_in_secs,
            "{message}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────
// Publishing a batch: never under its readable name until it is whole
// ─────────────────────────────────────────────────────────────────────

/// How old a writer's staging file must be before maintenance may remove it.
///
/// The sinks do hold the analytics writer guard while they write, but other
/// writers stage beside the same objects (the canonical LLM materializer, the
/// repricer) and the sweep must not depend on every one of them doing so. A
/// staging file only lives for the length of one `COPY`; an hour-old one
/// belongs to a writer that is gone.
pub(crate) const WRITER_STAGING_MIN_AGE: Duration = Duration::from_secs(60 * 60);

/// The hidden name a writer `COPY`s into before publishing `final_path`:
/// `.<name>.tmp` beside it. Every reader of these datasets selects on the
/// `.parquet` extension (globs are `*.parquet`; listings check the
/// extension), and the raw-batch prefixes (`batch_`, `embed_`) never start
/// with a dot, so the staging object is invisible until it is renamed.
pub(crate) fn parquet_staging_path(final_path: &Path) -> PathBuf {
    let name = final_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    final_path.with_file_name(format!(".{name}.tmp"))
}

/// `COPY <relation> TO <final_path> (<options>)`, published atomically.
///
/// DuckDB's `COPY` creates its target before it has written a byte, so
/// copying straight to the final name published a readable name for an object
/// that might never be finished: a process that died mid-`COPY` left a
/// zero-byte batch that failed every fold of its partition. This stages under
/// [`parquet_staging_path`], then fsyncs it, renames it and fsyncs the
/// directory, so the final name only ever refers to a complete object. The
/// staging file is removed on failure; one orphaned by a crash is swept by
/// maintenance once it is older than [`WRITER_STAGING_MIN_AGE`].
///
/// `relation` is anything `COPY` accepts as a source: a table name or a
/// parenthesised `SELECT`.
pub(crate) fn copy_to_parquet_durably(
    connection: &Connection,
    relation: &str,
    options: &str,
    final_path: &Path,
) -> Result<()> {
    let staging = parquet_staging_path(final_path);
    let staged = connection
        .execute_batch(&format!(
            "COPY {relation} TO '{}' ({options});",
            escape_sql_literal(&staging.display().to_string())
        ))
        .with_context(|| format!("writing Parquet staging object {}", staging.display()))
        .and_then(|()| {
            crate::magician_v2::artifact_v2::io::publish_staged_file_durably_sync(
                &staging, final_path,
            )
            .with_context(|| {
                format!(
                    "publishing Parquet {} -> {}",
                    staging.display(),
                    final_path.display()
                )
            })
        });
    if let Err(error) = staged {
        if let Err(cleanup) = std::fs::remove_file(&staging) {
            if cleanup.kind() != std::io::ErrorKind::NotFound {
                warn!(
                    target: "analytics::parquet_maintenance",
                    path = %staging.display(),
                    error = %cleanup,
                    "could not remove a failed Parquet staging file; maintenance sweeps it once it is old"
                );
            }
        }
        return Err(error);
    }
    Ok(())
}

/// Whether a staging name belongs to this module's own publishers — a
/// compacted generation or a rollup object — which are only ever written
/// under the analytics writer guard that every sweep caller also holds, and
/// so can be removed as soon as they are seen.
///
/// Matched exactly, not by substring: a repricer staging a compacted
/// generation (`.<gen>.parquet.reprice.<ulid>.tmp`) is *not* one of these.
fn is_maintenance_staging_name(name: &str) -> bool {
    let Some(inner) = name
        .strip_prefix('.')
        .and_then(|name| name.strip_suffix(".tmp"))
        .and_then(|name| name.strip_suffix(".parquet"))
    else {
        return false;
    };
    if let Some(hash) = inner.strip_prefix("rollup_") {
        return !hash.is_empty() && hash.chars().all(|character| character.is_ascii_hexdigit());
    }
    inner
        .split_once(".compacted.")
        .is_some_and(|(dataset, generation)| {
            !dataset.is_empty()
                && !dataset.contains('.')
                && generation.parse::<ulid::Ulid>().is_ok()
        })
}

/// Whether a staging file was last written at least `minimum` ago. A file
/// whose age cannot be read is treated as young: leaving garbage is cheap,
/// removing a live writer's file is not.
fn staging_file_is_older_than(metadata: &std::fs::Metadata, minimum: Duration) -> bool {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age >= minimum)
}

pub fn compacted_file(partition: &Path, dataset: PartitionedDataset) -> PathBuf {
    read_valid_manifest(partition, dataset)
        .ok()
        .flatten()
        .map(|manifest| manifest_output(partition, &manifest))
        .unwrap_or_else(|| partition.join(dataset.compacted_file_name()))
}

/// Parquet objects in `partition` that a governed read never selects: raw
/// batches the valid manifest already folded and superseded generations, both
/// waiting out [`PartitionedDataset::source_retirement_grace`]. Tools that walk
/// a partition's files directly (repricing) must leave these alone: rewriting
/// a folded batch changes its fingerprint and makes it look new.
pub fn retired_partition_objects(
    partition: &Path,
    dataset: PartitionedDataset,
) -> Result<Vec<PathBuf>> {
    let Some(manifest) = read_valid_manifest(partition, dataset)? else {
        return Ok(Vec::new());
    };
    let raw = raw_partition_files(partition, dataset)?;
    let (mut retired, _) = split_raw_against_manifest(&raw, Some(&manifest))?;
    let active = manifest_output(partition, &manifest);
    for entry in std::fs::read_dir(partition)? {
        let path = entry?.path();
        let is_generation = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| dataset.is_compacted_file_name(name));
        if is_generation && path != active {
            retired.push(path);
        }
    }
    retired.sort();
    Ok(retired)
}

pub fn partition_sources(partition: &Path, dataset: PartitionedDataset) -> Result<Vec<PathBuf>> {
    if let Some(manifest) = read_valid_manifest(partition, dataset)? {
        let raw = raw_partition_files(partition, dataset)?;
        let (_, new_raw) = split_raw_against_manifest(&raw, Some(&manifest))?;
        let mut sources = vec![manifest_output(partition, &manifest)];
        sources.extend(new_raw);
        return Ok(sources);
    }
    raw_partition_files(partition, dataset)
}

fn read_valid_manifest(
    partition: &Path,
    dataset: PartitionedDataset,
) -> Result<Option<PartitionCompactionManifest>> {
    let path = manifest_path(partition, dataset);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return recover_unmanifested_generation(partition, dataset)
        },
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file() {
        return recover_unmanifested_generation(partition, dataset);
    }
    let manifest: PartitionCompactionManifest = match std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    {
        Some(manifest) => manifest,
        None => {
            warn!(
                target: "analytics::parquet_maintenance",
                path = %path.display(),
                "ignoring malformed compaction manifest; raw batches remain authoritative"
            );
            return recover_unmanifested_generation(partition, dataset);
        },
    };
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION
        || manifest.dataset != dataset
        || manifest.partition_date != partition_date_text(partition).unwrap_or_default()
        || !dataset.is_compacted_file_name(&manifest.compacted_file_name)
        || manifest.source_row_count != manifest.compacted_row_count
    {
        return recover_unmanifested_generation(partition, dataset);
    }
    let output = manifest_output(partition, &manifest);
    let output_metadata = match std::fs::symlink_metadata(&output) {
        Ok(metadata) if metadata.file_type().is_file() => metadata,
        _ => {
            return Err(anyhow!(
                "manifest-selected compacted generation is missing or not a regular file: {}",
                output.display()
            ))
        },
    };
    if output_metadata.len() != manifest.compacted_byte_len
        || file_checksum(&output)? != manifest.compacted_checksum_blake3
    {
        return Err(anyhow!(
            "manifest-selected compacted generation failed length/checksum verification: {}",
            output.display()
        ));
    }
    Ok(Some(manifest))
}

fn recover_unmanifested_generation(
    partition: &Path,
    dataset: PartitionedDataset,
) -> Result<Option<PartitionCompactionManifest>> {
    if !raw_partition_files(partition, dataset)?.is_empty() {
        return Ok(None);
    }
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(partition)? {
        let entry = entry?;
        let path = entry.path();
        let candidate = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| dataset.is_compacted_file_name(name));
        if !candidate {
            continue;
        }
        if !entry.file_type()?.is_file() {
            return Err(anyhow!(
                "unmanifested compacted generation is not a regular file: {}",
                path.display()
            ));
        }
        candidates.push(path);
    }
    if candidates.is_empty() {
        return Ok(None);
    }
    if candidates.len() != 1 {
        return Err(anyhow!(
            "cannot recover {} unmanifested compacted generations without an authoritative manifest",
            candidates.len()
        ));
    }
    let output = candidates.pop().expect("one compacted generation");
    let connection =
        Connection::open_in_memory().context("opening DuckDB for unmanifested Parquet recovery")?;
    configure_analytics_connection_checked(&connection, "parquet_manifest_recovery")
        .context("configuring Parquet manifest recovery")?;
    let rows = count_parquet_rows(&connection, &output)?;
    if rows < 0 {
        return Err(anyhow!("negative row count in unmanifested generation"));
    }
    let metadata = std::fs::metadata(&output)?;
    warn!(
        target: "analytics::parquet_maintenance",
        dataset = dataset.as_str(),
        path = %output.display(),
        "using the sole valid compacted generation because no manifest or raw source remains"
    );
    Ok(Some(PartitionCompactionManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        dataset,
        partition_date: partition_date_text(partition).unwrap_or_default(),
        generated_at_ms: Utc::now().timestamp_millis(),
        sources: Vec::new(),
        source_row_count: u64::try_from(rows).unwrap_or(0),
        compacted_file_name: output
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("non-UTF8 compacted generation name"))?
            .to_string(),
        compacted_byte_len: metadata.len(),
        compacted_checksum_blake3: file_checksum(&output)?,
        compacted_row_count: u64::try_from(rows).unwrap_or(0),
        // Mark false so the next guarded maintenance sweep republishes a
        // durable authoritative manifest before considering recovery complete.
        raw_sources_pruned: false,
    }))
}

fn manifest_output(partition: &Path, manifest: &PartitionCompactionManifest) -> PathBuf {
    partition.join(&manifest.compacted_file_name)
}

fn cleanup_inactive_compacted_generations(
    partition: &Path,
    dataset: PartitionedDataset,
    manifest: &PartitionCompactionManifest,
) -> Result<()> {
    let active = manifest_output(partition, manifest);
    for entry in std::fs::read_dir(partition)? {
        let entry = entry?;
        let path = entry.path();
        let is_generation = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| dataset.is_compacted_file_name(name));
        if !is_generation || path == active {
            continue;
        }
        if !entry.file_type()?.is_file() {
            return Err(anyhow!(
                "refusing to remove non-regular inactive compacted generation {}",
                path.display()
            ));
        }
        std::fs::remove_file(&path).with_context(|| {
            format!("removing inactive compacted generation {}", path.display())
        })?;
    }
    Ok(())
}

fn split_raw_against_manifest(
    raw: &[PathBuf],
    manifest: Option<&PartitionCompactionManifest>,
) -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let Some(manifest) = manifest else {
        return Ok((Vec::new(), raw.to_vec()));
    };
    let mut known = Vec::new();
    let mut new = Vec::new();
    for path in raw {
        let fingerprint = source_fingerprint(path)?;
        if manifest.sources.contains(&fingerprint) {
            known.push(path.clone());
        } else {
            new.push(path.clone());
        }
    }
    Ok((known, new))
}

fn raw_partition_files(partition: &Path, dataset: PartitionedDataset) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(partition) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let candidate = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| dataset.is_raw_file_name(name))
            && path.extension().and_then(|extension| extension.to_str()) == Some("parquet");
        if !candidate {
            continue;
        }
        if !entry.file_type()?.is_file() {
            return Err(anyhow!(
                "Parquet maintenance source must be a regular file: {}",
                path.display()
            ));
        }
        files.push(path);
    }
    files.sort();
    Ok(files)
}

fn prune_verified_sources(files: &[PathBuf], expected: &[SourceFingerprint]) -> Result<usize> {
    // Validate the complete deletion set before unlinking any member. This
    // makes a changed or replaced batch fail closed without leaving a
    // partially-pruned source generation behind.
    for path in files {
        let metadata = std::fs::symlink_metadata(path)
            .with_context(|| format!("inspecting raw Parquet {}", path.display()))?;
        if !metadata.file_type().is_file() {
            return Err(anyhow!(
                "refusing to prune non-regular Parquet source {}",
                path.display()
            ));
        }
        let actual = source_fingerprint(path)?;
        if !expected.contains(&actual) {
            return Err(anyhow!(
                "refusing to prune changed Parquet source {}: fingerprint no longer matches the verified compact input",
                path.display()
            ));
        }
    }

    let mut removed = 0;
    for path in files {
        std::fs::remove_file(path)
            .with_context(|| format!("pruning verified raw Parquet {}", path.display()))?;
        removed += 1;
    }
    Ok(removed)
}

pub fn apply_retention(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    scope: &str,
    retention_days: u32,
) -> Result<RetentionStats> {
    let Some(_guard) = try_analytics_duckdb_guard_for(COMPACTION_GUARD_TIMEOUT) else {
        return Err(anyhow!("timed out waiting for analytics retention guard"));
    };
    let cutoff = Utc::now().date_naive() - ChronoDuration::days(i64::from(retention_days));
    let roots = [
        workspace.analytics_root(principal, scope).join("events"),
        workspace.analytics_memory_events_root(principal, scope),
        workspace.analytics_llm_calls_root(principal, scope),
        workspace.analytics_llm_embeddings_root(principal, scope),
        workspace.analytics_llm_provider_attempts_root(principal, scope),
        workspace.analytics_llm_tool_calls_root(principal, scope),
        workspace.analytics_llm_capture_gaps_root(principal, scope),
        workspace
            .analytics_root(principal, scope)
            .join("llm_dispatch"),
    ];
    let mut stats = RetentionStats::default();
    for root in roots {
        // Per-root and per-partition, never fatal. One unreadable directory
        // used to abort the whole sweep, which meant no dataset after it in
        // this list — and nothing after this call — ever expired again.
        let partitions = match partition_dirs(&root) {
            Ok(partitions) => partitions,
            Err(error) => {
                warn!(
                    target: "analytics::parquet_maintenance",
                    root = %root.display(),
                    error = %error,
                    "could not list partitions for retention; the dataset was skipped"
                );
                continue;
            },
        };
        for (date, partition) in partitions {
            stats.partitions_scanned += 1;
            if date >= cutoff {
                continue;
            }
            let bytes = directory_apparent_bytes(&partition).unwrap_or(0);
            if let Err(error) = std::fs::remove_dir_all(&partition) {
                warn!(
                    target: "analytics::parquet_maintenance",
                    partition = %partition.display(),
                    error = %error,
                    "expired partition could not be removed; it will be retried next sweep"
                );
                continue;
            }
            stats.partitions_removed += 1;
            stats.bytes_removed = stats.bytes_removed.saturating_add(bytes);
        }
    }
    Ok(stats)
}

// ─────────────────────────────────────────────────────────────────────
// The activity spine
// ─────────────────────────────────────────────────────────────────────
//
// The spine is the one dataset whose compaction policy was chosen before its
// first row was written, and every parameter below is part of that choice.
//
// The thing being avoided is concrete: `llm_calls` reached 106,335 files
// because its compaction question was deferred, and it cannot be retrofitted
// cheaply. The spine writes an order of magnitude more rows than `llm_calls`
// does — at the measured ~11.4 spans/sec a genuinely unbuffered writer would
// emit a file per span, ~985,000 a day per scope. The sink's 60-second timer
// cuts that to ~1,440/day (86,400 ÷ 60), which is still not survivable over
// months, and is in fact *more* than the ~290/day a five-minute batched writer
// would produce. Buffering is a memory and crash-window control, not the file
// count answer. Two folds are: they take it to one file per day per scope in
// steady state.

/// How long a closed hour waits before its batches are folded into one file.
///
/// Two hours, not zero, because a span is filed under the hour it **started**
/// in, so an hour keeps receiving rows for as long as spans that began in it
/// are still running. Folding at the boundary would guarantee a second fold
/// for every long span. Two hours is longer than essentially every span this
/// runtime opens, and a later arrival is still handled — the manifest simply
/// recognises a raw file it has not seen and cuts a new generation.
const ACTIVITY_HOUR_COMPACTION_DELAY_HOURS: i64 = 2;

/// How long a closed day waits before its hours are folded into one file.
///
/// Forty-eight hours after the day **ends**, so a `dt=` partition is folded
/// once it is three calendar days behind. Waiting out the same late-arrival
/// window as the hourly fold, twice over, is what makes the daily object
/// terminal in practice rather than something rewritten every sweep.
const ACTIVITY_DAY_COMPACTION_DELAY_DAYS: i64 = 3;

/// Minimum raw objects in a partition before folding it is worth a rewrite.
///
/// Two: below that there is nothing to merge, and rewriting a single object
/// into a single object burns a read, a write and an fsync to change nothing.
const ACTIVITY_COMPACTION_MIN_RAW_FILES: usize = 2;

/// How long full per-span rows are kept before they are rolled up.
///
/// Seven days is how long it stays interesting *which* span did something.
/// After that the shape of the work is what matters, and keeping instances
/// would make this store grow linearly with uptime — the failure the tiering
/// exists to prevent.
///
/// The boundary is inclusive: a partition exactly seven days old rolls up.
pub const ACTIVITY_DETAIL_RETENTION_DAYS: i64 = 7;

/// How long the rolled-up tier is kept.
///
/// Thirteen months, expressed in days so the comparison is one subtraction:
/// 396 = 365 + 31, which covers a full year plus the longest extra month
/// whichever months the window happens to span. Thirteen rather than twelve so
/// "the same month last year" is always still there to compare against.
pub const ACTIVITY_ROLLUP_RETENTION_DAYS: i64 = 396;

/// Records which detail sources are already represented in the rollup tier.
///
/// This lives in the **rollup** partition, not the detail partition, and that
/// placement is the whole point. It used to sit at
/// `<detail>/dt=D/_rollup/rolled-up.json` — inside the directory the caller is
/// about to `remove_dir_all`. A crash between publishing the rollup and writing
/// the marker left the rollup published, the marker absent and the detail
/// intact, so the next pass re-aggregated the same rows into a *second* rollup
/// object. Nothing deduplicates rollup objects on read, so the day reported
/// double forever, and the detail that could have proved it was then deleted.
/// `remove_dir_all` has no defined traversal order, so unlinking the marker
/// first and then failing on a Parquet file reached the same state with no
/// crash at all.
///
/// Three properties are needed to make the double-write impossible rather than
/// merely unlikely. The first two were the original fix; on their own they are
/// **not** sufficient, and the third is what actually closes the window:
///
/// 1. The marker is in the rollup partition, which retention only ever removes
///    whole, thirteen months later.
/// 2. A rollup object's name is a hash of the sources it summarises
///    (`activity_rollup_file_name`), so a re-run over the *same* sources renames
///    over the object it already wrote instead of adding one beside it.
/// 3. Before publishing, every Parquet object in the partition that this marker
///    does not name is quarantined out of the readable name space
///    ([`quarantine_unaccounted_rollup_objects`]), and the reader consults the
///    marker rather than globbing ([`rollup_generation_files`]).
///
/// Property 2 only holds when the source set is *unchanged*. It is not:
/// publish `rollup_hash(A)`, crash before the marker write, receive a late
/// batch `B`, and the next pass computes a pending set of `[A, B]`, hashes to a
/// different name, and publishes **beside** the first object. Nothing globbing
/// `*.parquet` could tell the two apart, retention then deleted the detail that
/// could have proved it, and `A` was counted twice forever. The corrupt-marker
/// path reached the same state without a crash, because an unreadable marker
/// used to be mapped to "absent" and absent means "nothing is accounted for".
///
/// Property 3 turns that from "the second object is unlikely to exist" into
/// "the second object is unreadable, and the marker says which one is real".
/// The sweep is safe where it runs *because of what it runs on*, and both
/// halves of that are load-bearing:
///
/// - It runs only where a generation is about to be published or superseded,
///   and there the caller deletes the detail after this returns `Ok` — so every
///   row a stray could hold is still on disk in the detail tier and is about to
///   be re-aggregated. It is **not** run on the two early returns that
///   authorise a delete without aggregating; see [`roll_up_activity_day`].
/// - It runs only with a manifest that genuinely describes the partition. The
///   "at sweep time the rows are still on disk" argument is false for a
///   *committed* generation, whose detail was deleted at the end of the pass
///   that published it. Handing the sweep an empty named-set for a partition
///   that holds committed generations therefore destroys a day's summary rather
///   than a redundant copy — the exact inverse of the bug above, and why
///   [`read_activity_rollup_manifest`] refuses on an uninterpretable marker
///   instead of reporting one.
///
/// Recording the sources — rather than just "this day is done" — is what lets a
/// batch that lands after the day was rolled up be *aggregated* instead of
/// dropped. It is the same `SourceFingerprint` discipline
/// [`PartitionCompactionManifest`] uses, for the same reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityRollupManifest {
    schema_version: u16,
    partition_date: String,
    rolled_up_at_ms: i64,
    /// Every rollup object published for this day, with the detail sources each
    /// one summarises. More than one entry means a late batch arrived after the
    /// day was first rolled up; `count` and `sum_duration_ms` are additive, so
    /// readers sum across them.
    generations: Vec<ActivityRollupGeneration>,
    /// Whether this day tier currently has unresolved publication state.
    ///
    /// `false` means this partition reads as authored by the marker.
    /// `true` means the marker is known stale or incomplete and readers should
    /// report inventory uncertainty.
    #[serde(default)]
    incomplete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ActivityRollupGeneration {
    rollup_file_name: String,
    sources: Vec<SourceFingerprint>,
    detail_row_count: u64,
    rollup_row_count: u64,
}

/// Fold the spine's Parquet: closed hours first, then closed days.
///
/// Runs on both maintenance cadences. The hourly fold is on the five-minute
/// lane because waiting six hours for it would let ~360 minute-files pile up
/// in a busy scope; the daily fold rides along because by then it is a cheap
/// directory listing that almost always finds nothing to do.
pub fn compact_activity_rows(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    scope: &str,
) -> Result<ParquetCompactionStats> {
    let dataset = PartitionedDataset::ActivityRows;
    let root = dataset.root(workspace, principal, scope);
    let rollup_root = workspace.analytics_activity_rollups_root(principal, scope);
    let bytes_before = directory_apparent_bytes(&root)?;
    let files_before = directory_regular_file_count(&root)?;
    let mut stats = ParquetCompactionStats::default();
    let now = Utc::now();

    for (date, day_partition) in partition_dirs(&root)? {
        // A day already represented in the rollup tier is awaiting deletion,
        // and folding it is not merely wasted work — it is unsafe. The rollup
        // marker identifies what it summarised by source fingerprint, and a
        // fold *replaces* those objects with a new one carrying a fingerprint
        // the marker has never seen. The next retention pass would then read
        // that object as un-rolled detail and aggregate the same rows twice.
        // The rollup only gets ahead of the delete when the delete failed, but
        // "rare" is not "safe" for a store whose rows are gone afterwards.
        match day_is_already_rolled_up(&rollup_root, date) {
            Ok(true) => continue,
            Ok(false) => {},
            Err(error) => {
                stats.partitions_failed += 1;
                warn!(
                    target: "analytics::parquet_maintenance",
                    partition = %day_partition.display(),
                    error = %error,
                    "could not read the activity rollup marker; the day was left unfolded"
                );
                continue;
            },
        }
        // Per-day, never fatal. This function runs immediately before
        // `apply_retention` and `apply_activity_retention` in `maintain_scope`;
        // propagating one half-removed directory out of here used to abort the
        // whole sweep, so a single poisoned hour meant nothing in the scope —
        // no dataset at all — ever expired again.
        match activity_hour_dirs(&day_partition) {
            Ok(hour_partitions) => {
                for hour_partition in hour_partitions {
                    if !hour_is_closed_long_enough(date, &hour_partition, now) {
                        continue;
                    }
                    fold_activity_hour(&hour_partition, &mut stats);
                }
            },
            Err(error) => {
                stats.partitions_failed += 1;
                warn!(
                    target: "analytics::parquet_maintenance",
                    partition = %day_partition.display(),
                    error = %error,
                    "could not list activity hour partitions; the day was skipped"
                );
                continue;
            },
        }

        if date + ChronoDuration::days(ACTIVITY_DAY_COMPACTION_DELAY_DAYS) > now.date_naive() {
            continue;
        }
        if let Err(error) = fold_activity_day(&day_partition, &mut stats) {
            stats.partitions_failed += 1;
            warn!(
                target: "analytics::parquet_maintenance",
                dataset = dataset.as_str(),
                partition = %day_partition.display(),
                error = %error,
                "activity day fold could not be attempted; the day was left as hourly objects"
            );
        }
    }

    stats.files_before = files_before;
    stats.files_after = directory_regular_file_count(&root)?;
    stats.bytes_before = bytes_before;
    stats.bytes_after = directory_apparent_bytes(&root)?;
    stats.bytes_reclaimed = stats.bytes_before.saturating_sub(stats.bytes_after);
    if stats.partitions_compacted > 0 || stats.raw_files_pruned > 0 || stats.bytes_reclaimed > 0 {
        stats.areas.push(ParquetCompactionAreaStats {
            dataset: dataset.as_str().to_string(),
            partitions_compacted: stats.partitions_compacted,
            files_before: stats.files_before,
            files_after: stats.files_after,
            raw_files_compacted: stats.raw_files_compacted,
            raw_files_pruned: stats.raw_files_pruned,
            rows_compacted: stats.rows_compacted,
            bytes_before: stats.bytes_before,
            bytes_after: stats.bytes_after,
            bytes_reclaimed: stats.bytes_reclaimed,
            query_files_avoided: stats.query_files_avoided,
        });
    }
    Ok(stats)
}

/// Whether any of a day's detail is already summarised in the rollup tier.
///
/// Read by the fold, not just by retention: once a day has a rollup generation,
/// rewriting its detail objects would change the fingerprints the generation
/// identifies, and the rows would be aggregated a second time.
fn day_is_already_rolled_up(rollup_root: &Path, date: NaiveDate) -> Result<bool> {
    let rollup_partition = rollup_root.join(format!("dt={date}"));
    Ok(read_activity_rollup_manifest(&rollup_partition)?
        .is_some_and(|manifest| !manifest.generations.is_empty()))
}

/// Fold one aged-out day partition's hourly objects into a single day object.
///
/// Returns `Err` only for the things that stopped the attempt from being made
/// at all — an unreadable hour directory, a manifest that fails verification.
/// The caller logs those against the one day and moves on; nothing in here may
/// abort the scope's sweep, because retention runs after it.
fn fold_activity_day(day_partition: &Path, stats: &mut ParquetCompactionStats) -> Result<()> {
    let dataset = PartitionedDataset::ActivityRows;
    // The day's inputs are its hours' objects, which is why this cannot ride
    // the generic day-level walk: `raw_partition_files` looks at files in the
    // partition, and a spine day partition holds directories.
    let hour_partitions = activity_hour_dirs(day_partition)?;
    let mut hour_sources = Vec::new();
    for hour_partition in &hour_partitions {
        hour_sources.extend(partition_sources(hour_partition, dataset)?);
    }
    if hour_sources.is_empty() {
        // Nothing this module recognises is left below — but "recognises" is
        // the load-bearing word. `partition_sources` only ever returns
        // `batch_*` files and the manifest's own compacted generation, so a
        // Parquet object under any other name is invisible to it. Deleting the
        // directory on the strength of an empty list would destroy rows that
        // were merely unreadable, which is the one outcome this dataset must
        // never produce. Drop only the genuinely empty shells.
        for hour_partition in &hour_partitions {
            let unaccounted = unaccounted_parquet_objects(hour_partition, &hour_sources)?;
            if !unaccounted.is_empty() {
                stats.partitions_failed += 1;
                warn!(
                    target: "analytics::parquet_maintenance",
                    partition = %hour_partition.display(),
                    unaccounted = unaccounted.len(),
                    example = %unaccounted[0].display(),
                    "refusing to drop an hour partition holding Parquet objects maintenance cannot account for"
                );
                continue;
            }
            if let Err(error) = std::fs::remove_dir_all(hour_partition) {
                warn!(
                    target: "analytics::parquet_maintenance",
                    partition = %hour_partition.display(),
                    error = %error,
                    "emptied hour partition could not be removed; it will be retried"
                );
            }
        }
        return Ok(());
    }
    // No minimum-file threshold here, unlike the hourly fold. A day with a
    // single hour still has to become a day-level object, because the hour
    // directory is about to be removed; and a single late hour beside an
    // existing daily object must be folded in, or the day stays two files
    // forever.
    let day_manifest = read_valid_manifest(day_partition, dataset)?;
    stats.partitions_scanned += 1;
    let signature = fold_input_signature(day_manifest.as_ref(), &hour_sources);
    if fold_backoff_pending(day_partition, dataset, &signature, Utc::now()).is_some() {
        stats.partitions_backed_off += 1;
        return Ok(());
    }
    let Some(_guard) = try_analytics_duckdb_guard_for(COMPACTION_GUARD_TIMEOUT) else {
        stats.partitions_failed += 1;
        warn!(
            target: "analytics::parquet_maintenance",
            dataset = dataset.as_str(),
            partition = %day_partition.display(),
            "timed out waiting for the analytics writer guard; the day was left as hourly objects"
        );
        return Ok(());
    };
    let attempt = attempt_verified_fold(
        day_partition,
        dataset,
        day_manifest.as_ref(),
        &hour_sources,
        stats,
    );
    if attempt.is_ok() {
        clear_fold_backoff(day_partition, dataset);
    }
    // From here on the day's inputs are the ones the fold actually consumed:
    // an unreadable batch it set aside has already left its hour directory.
    let attempt = attempt.map(|attempt| {
        hour_sources = attempt.folded_inputs;
        attempt.published
    });
    match attempt {
        // Every input was unreadable and set aside; the hours hold nothing
        // the next sweep's empty-day path cannot handle.
        Ok(None) => {},
        Ok(Some((manifest, pruned))) => {
            stats.partitions_compacted += 1;
            stats.raw_files_compacted += hour_sources.len();
            stats.raw_files_pruned += pruned;
            stats.rows_compacted = stats
                .rows_compacted
                .saturating_add(manifest.compacted_row_count);
            stats.query_files_avoided = stats
                .query_files_avoided
                .saturating_add(hour_sources.len().saturating_sub(1));
            // Only now that the day's object is published and verified are the
            // hour directories redundant. Their objects were already pruned by
            // the compaction; this removes the empty shells and their
            // manifests — and still refuses any directory holding something it
            // cannot account for.
            for hour_partition in &hour_partitions {
                match unaccounted_parquet_objects(hour_partition, &hour_sources) {
                    Ok(unaccounted) if !unaccounted.is_empty() => {
                        warn!(
                            target: "analytics::parquet_maintenance",
                            partition = %hour_partition.display(),
                            unaccounted = unaccounted.len(),
                            example = %unaccounted[0].display(),
                            "folded hour partition holds Parquet objects maintenance cannot account for; it was kept"
                        );
                        continue;
                    },
                    Ok(_) => {},
                    Err(error) => {
                        warn!(
                            target: "analytics::parquet_maintenance",
                            partition = %hour_partition.display(),
                            error = %error,
                            "could not inspect a folded hour partition before removing it; it was kept"
                        );
                        continue;
                    },
                }
                if let Err(error) = std::fs::remove_dir_all(hour_partition) {
                    warn!(
                        target: "analytics::parquet_maintenance",
                        partition = %hour_partition.display(),
                        error = %error,
                        "folded hour partition could not be removed; it holds no rows and will be retried"
                    );
                }
            }
        },
        Err(error) => {
            stats.partitions_failed += 1;
            report_fold_failure(
                day_partition,
                dataset,
                &signature,
                &error,
                "verified day fold failed; the hourly objects were retained",
            );
        },
    }
    Ok(())
}

/// Parquet objects in a partition that [`partition_sources`] did not return.
///
/// The gap is real and silent: `partition_sources` recognises `batch_*` raw
/// files and the manifest's own compacted generation, and nothing else. A stale
/// generation whose manifest was lost, an object restored by hand, a file from
/// a naming scheme that has since changed — all read as "this partition is
/// empty". Callers that delete on an empty result must ask this first.
fn unaccounted_parquet_objects(partition: &Path, accounted: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(partition) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut unaccounted = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("parquet") {
            continue;
        }
        if !entry.file_type()?.is_file() || accounted.contains(&path) {
            continue;
        }
        unaccounted.push(path);
    }
    unaccounted.sort();
    Ok(unaccounted)
}

/// Fold one hour partition, recording the outcome in `stats`.
///
/// Never returns an error: one unreadable hour must not abandon the rest of
/// the sweep, and a partition left unfolded is still readable — it is just
/// more files than it needs to be.
///
/// One thing to know before debugging an hour manifest by hand: its
/// `partition_date` is the empty string. `partition_date_text` only understands
/// `dt=` directory names, and an hour partition is named `hour=HH`. That is
/// harmless because the field is only ever compared against the same function's
/// output for the same directory — write and read agree — but it does mean the
/// hour manifest does not say which day it belongs to. Its path does.
fn fold_activity_hour(partition: &Path, stats: &mut ParquetCompactionStats) {
    let dataset = PartitionedDataset::ActivityRows;
    stats.partitions_scanned += 1;
    let inputs = raw_partition_files(partition, dataset).and_then(|raw| {
        let manifest = read_valid_manifest(partition, dataset)?;
        let (_, new_raw) = split_raw_against_manifest(&raw, manifest.as_ref())?;
        Ok((manifest, new_raw))
    });
    let (manifest, new_raw) = match inputs {
        Ok(inputs) => inputs,
        Err(error) => {
            stats.partitions_failed += 1;
            warn!(
                target: "analytics::parquet_maintenance",
                partition = %partition.display(),
                error = %error,
                "could not read activity hour inputs; the partition was left untouched"
            );
            return;
        },
    };
    if new_raw.is_empty() {
        stats.partitions_already_compacted += 1;
        return;
    }
    if manifest.is_none() && new_raw.len() < ACTIVITY_COMPACTION_MIN_RAW_FILES {
        stats.partitions_below_threshold += 1;
        return;
    }
    let signature = fold_input_signature(manifest.as_ref(), &new_raw);
    if fold_backoff_pending(partition, dataset, &signature, Utc::now()).is_some() {
        stats.partitions_backed_off += 1;
        return;
    }
    let Some(_guard) = try_analytics_duckdb_guard_for(COMPACTION_GUARD_TIMEOUT) else {
        stats.partitions_failed += 1;
        warn!(
            target: "analytics::parquet_maintenance",
            dataset = dataset.as_str(),
            partition = %partition.display(),
            "timed out waiting for the analytics writer guard; partition was left untouched"
        );
        return;
    };
    match attempt_verified_fold(partition, dataset, manifest.as_ref(), &new_raw, stats) {
        Ok(attempt) => {
            clear_fold_backoff(partition, dataset);
            let Some((manifest, pruned)) = attempt.published else {
                return;
            };
            stats.partitions_compacted += 1;
            stats.raw_files_compacted += attempt.folded_inputs.len();
            stats.raw_files_pruned += pruned;
            stats.rows_compacted = stats
                .rows_compacted
                .saturating_add(manifest.compacted_row_count);
            stats.query_files_avoided = stats
                .query_files_avoided
                .saturating_add(attempt.folded_inputs.len().saturating_sub(1));
        },
        Err(error) => {
            stats.partitions_failed += 1;
            report_fold_failure(
                partition,
                dataset,
                &signature,
                &error,
                "verified hour fold failed; the source batches were retained",
            );
        },
    }
}

/// The `hour=HH` directories inside a day partition, in order.
fn activity_hour_dirs(day_partition: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(day_partition) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut partitions = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_dir() || partition_hour(&path).is_none() {
            continue;
        }
        partitions.push(path);
    }
    partitions.sort();
    Ok(partitions)
}

fn partition_hour(path: &Path) -> Option<u32> {
    let hour: u32 = path
        .file_name()?
        .to_str()?
        .strip_prefix("hour=")?
        .parse()
        .ok()?;
    (hour < 24).then_some(hour)
}

/// Whether an hour partition ended long enough ago to be worth folding.
fn hour_is_closed_long_enough(date: NaiveDate, hour_partition: &Path, now: DateTime<Utc>) -> bool {
    let Some(hour) = partition_hour(hour_partition) else {
        return false;
    };
    let Some(start) = date.and_hms_opt(hour, 0, 0) else {
        return false;
    };
    let ends_at = start.and_utc() + ChronoDuration::hours(1);
    now >= ends_at + ChronoDuration::hours(ACTIVITY_HOUR_COMPACTION_DELAY_HOURS)
}

/// Roll expired detail into the summary tier, then expire the summaries.
///
/// Kept out of [`apply_retention`] on purpose. That function is a flat
/// `remove_dir_all` of everything past a single window, and running the spine
/// through it would delete seven-day-old spans outright — the tiering exists
/// precisely so that "how much of last month was ambient" survives the loss of
/// which specific spans made it up.
pub fn apply_activity_retention(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    scope: &str,
) -> Result<RetentionStats> {
    let Some(_guard) = try_analytics_duckdb_guard_for(COMPACTION_GUARD_TIMEOUT) else {
        return Err(anyhow!(
            "timed out waiting for the activity retention guard"
        ));
    };
    let detail_root = workspace.analytics_activity_rows_root(principal, scope);
    let rollup_root = workspace.analytics_activity_rollups_root(principal, scope);
    let today = Utc::now().date_naive();
    let mut stats = RetentionStats::default();

    // Both loops are per-partition non-fatal for the same reason the compaction
    // sweep is: one day that cannot be rolled up must not stop every other day,
    // or the rollup tier that is supposed to expire never does.
    for (date, day_partition) in partition_dirs(&detail_root)? {
        stats.partitions_scanned += 1;
        if (today - date).num_days() < ACTIVITY_DETAIL_RETENTION_DAYS {
            continue;
        }
        let bytes = directory_apparent_bytes(&day_partition).unwrap_or(0);
        if let Err(error) = roll_up_activity_day(&day_partition, &rollup_root, date) {
            // A day that keeps refusing is retried every sweep and never gives
            // up, because both forms of giving up are worse than the wedge: to
            // delete is to lose rows nothing summarised, and to fold it anyway
            // is to change the fingerprints the rollup marker identifies and
            // aggregate the same rows twice. What a wedge does deserve is to
            // stop reading like routine noise once it is clearly not transient.
            let days_overdue = (today - date).num_days() - ACTIVITY_DETAIL_RETENTION_DAYS;
            if days_overdue >= ACTIVITY_DETAIL_RETENTION_DAYS {
                error!(
                    target: "analytics::parquet_maintenance",
                    partition = %day_partition.display(),
                    days_overdue,
                    error = %error,
                    "activity detail has been refusing to roll up for longer than the retention \
                     window itself; it needs an operator, not another sweep"
                );
            } else {
                warn!(
                    target: "analytics::parquet_maintenance",
                    partition = %day_partition.display(),
                    days_overdue,
                    error = %error,
                    "expired activity partition could not be rolled up; its detail was kept"
                );
            }
            continue;
        }
        // Only reached once every source is durably represented in the rollup
        // tier and the rollup partition's marker says which.
        if let Err(error) = std::fs::remove_dir_all(&day_partition) {
            warn!(
                target: "analytics::parquet_maintenance",
                partition = %day_partition.display(),
                error = %error,
                "rolled-up activity partition could not be removed; it will be retried next sweep"
            );
            continue;
        }
        stats.partitions_removed += 1;
        stats.bytes_removed = stats.bytes_removed.saturating_add(bytes);
    }

    for (date, rollup_partition) in partition_dirs(&rollup_root)? {
        stats.partitions_scanned += 1;
        if (today - date).num_days() < ACTIVITY_ROLLUP_RETENTION_DAYS {
            continue;
        }
        let bytes = directory_apparent_bytes(&rollup_partition).unwrap_or(0);
        if let Err(error) = std::fs::remove_dir_all(&rollup_partition) {
            warn!(
                target: "analytics::parquet_maintenance",
                partition = %rollup_partition.display(),
                error = %error,
                "expired activity rollup could not be removed; it will be retried next sweep"
            );
            continue;
        }
        stats.partitions_removed += 1;
        stats.bytes_removed = stats.bytes_removed.saturating_add(bytes);
    }

    Ok(stats)
}

/// Summarise one day of detail into the rollup tier, idempotently.
///
/// Returns `Ok(())` once every source under `day_partition` is represented in
/// the rollup tier and the marker says which. The caller removes the detail
/// afterwards; a crash anywhere in between leaves this function to notice what
/// is already covered and finish only what is not.
///
/// "What is already covered" is per-source, not per-day. A day whose rollup is
/// complete but which then receives a late batch — a span that started that day
/// and closed much later — gets that batch aggregated into an additional
/// generation, rather than deleted with the rest of the partition. Before this
/// was per-source, the marker's mere existence meant "done", and a late arrival
/// into an already-rolled-up day was dropped without ever being counted.
///
/// Returning `Err` is how this function withholds permission to delete. Every
/// refusal below costs a day of detail that stays on disk and is retried each
/// sweep; none of them costs a row.
///
/// [`quarantine_unaccounted_rollup_objects`] runs on every path that **publishes
/// or supersedes** an aggregation — which is not the same as every path that
/// returns `Ok`, and the doc must not claim otherwise. Two returns above it
/// authorise the caller's delete without sweeping:
///
/// - a legacy marker covering a day whose detail is already gone, and
/// - a day with no recognised sources at all.
///
/// Neither can produce an overlapping aggregation, because neither aggregates:
/// no generation is published, so no stray can be superseded by one. Sweeping
/// there would be actively wrong rather than merely redundant — neither path has
/// read a manifest that could vouch for the rollup partition's contents, so the
/// sweep would run on an empty named-set and rename the day's committed
/// generations away. That is the same failure
/// [`read_activity_rollup_manifest`] refuses for an unreadable marker.
fn roll_up_activity_day(day_partition: &Path, rollup_root: &Path, date: NaiveDate) -> Result<()> {
    let rollup_partition = rollup_root.join(format!("dt={date}"));
    let dataset = PartitionedDataset::ActivityRows;
    let mut sources = partition_sources(day_partition, dataset)?;
    for hour_partition in activity_hour_dirs(day_partition)? {
        sources.extend(partition_sources(&hour_partition, dataset)?);
    }
    // Sorted so the fingerprint list — and therefore the deterministic object
    // name derived from it — does not depend on directory-listing order.
    sources.sort();

    // The same guard the fold already applies, on the strictly more destructive
    // path. `partition_sources` recognises `batch_*` files and the manifest's
    // own compacted generation and nothing else, so an object under any other
    // name — restored by hand, left by a naming scheme that has since changed,
    // a generation whose manifest was lost — reads as absent. Rolling up
    // without it and returning `Ok` authorises the caller's `remove_dir_all`,
    // which destroys it and reports the day as a successful `partitions_removed`.
    // Refusing costs a day of detail on disk; not refusing costs the rows.
    let unaccounted = unaccounted_activity_day_objects(day_partition, &sources)?;
    if !unaccounted.is_empty() {
        return Err(anyhow!(
            "refusing to roll up a day holding {} Parquet object(s) maintenance cannot account for \
             (e.g. {}); its detail was kept",
            unaccounted.len(),
            unaccounted[0].display()
        ));
    }

    // The legacy marker records a single object name and *no* source
    // fingerprints, so it cannot answer the only question that matters here:
    // are the sources still present the ones it already summarised — survivors
    // of an interrupted `remove_dir_all` — or ones that arrived after it?
    // Aggregating them would double-count the first kind; deleting them with
    // the partition would lose the second. Short-circuiting to `Ok` before any
    // enumeration, as this used to, picked the second silently: it is the exact
    // "the marker's existence means done" behaviour this function's contract
    // says it removed.
    //
    // So a legacy marker never authorises a delete while detail remains. One
    // re-roll clears it: remove `_rollup/rolled-up.json` from the *detail*
    // partition and the normal path takes over, quarantining the legacy object
    // as unaccounted and publishing one generation over everything present.
    if legacy_rollup_marker_covers_day(day_partition, &rollup_partition)? {
        if !sources.is_empty() {
            return Err(anyhow!(
                "a pre-fix rollup marker at {} covers this day but records no source fingerprints, \
                 so the {} source(s) still present cannot be shown to be already summarised; \
                 its detail was kept — remove the marker to force one re-roll",
                day_partition.join("_rollup").join("rolled-up.json").display(),
                sources.len()
            ));
        }
        // Nothing but the marker directory is left: the legacy build's delete
        // got everything else. There is no row to lose and none to double.
        return Ok(());
    }

    if sources.is_empty() {
        return Ok(());
    }
    let fingerprints = source_fingerprints(&sources)?;

    // `?` here is a refusal, not a plumbing detail. A marker that exists but
    // cannot be interpreted propagates out, retention keeps this day's detail,
    // and the sweep below never runs with a named-set that does not describe the
    // partition — see [`read_activity_rollup_manifest`]. Only a genuinely
    // *absent* marker falls through to the empty default, and for that state an
    // empty named-set is the truth: nothing has been committed here yet.
    let mut manifest =
        read_activity_rollup_manifest(&rollup_partition)?.unwrap_or(ActivityRollupManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            partition_date: date.format("%Y-%m-%d").to_string(),
            rolled_up_at_ms: 0,
            generations: Vec::new(),
            incomplete: false,
        });
    // A generation whose object is gone — hand-deletion, partial restore — is
    // no longer evidence that its sources are represented. Forgetting it lets
    // them be rolled up again; because the name is derived from the sources,
    // the rebuild lands on exactly the same path it lost.
    manifest.generations.retain(|generation| {
        let present = rollup_partition.join(&generation.rollup_file_name).is_file();
        if !present {
            warn!(
                target: "analytics::parquet_maintenance",
                partition = %day_partition.display(),
                rollup = %generation.rollup_file_name,
                "activity rollup marker names an object that is gone; its sources will be rolled up again"
            );
        }
        present
    });

    // Everything the (pruned) marker does not name is made unreadable before a
    // single new byte is published. This is what stops a source set that
    // *changed* across a crash from leaving two overlapping aggregations of the
    // same day behind — see [`ActivityRollupManifest`] for the sequence. It runs
    // before the `pending_sources.is_empty()` return below on purpose: that is
    // the resume path that ends in the caller deleting the detail, and it is the
    // last moment at which a stray is still provably redundant.
    //
    // It is reached only with a manifest that genuinely describes this
    // partition: absent (nothing committed yet) or read whole. An
    // *uninterpretable* marker returned above rather than defaulting to an empty
    // named-set, because a sweep run on an empty named-set renames the day's
    // committed generations away — and those are not redundant, since the detail
    // they summarised was deleted at the end of the pass that published them.
    quarantine_unaccounted_rollup_objects(&rollup_partition, &manifest)?;

    let mut pending_sources = Vec::new();
    let mut pending_fingerprints = Vec::new();
    for (path, fingerprint) in sources.iter().zip(fingerprints) {
        let already_rolled = manifest
            .generations
            .iter()
            .any(|generation| generation.sources.contains(&fingerprint));
        if !already_rolled {
            pending_sources.push(path.clone());
            pending_fingerprints.push(fingerprint);
        }
    }
    if pending_sources.is_empty() {
        // Every source is already summarised. This is the resume path after a
        // crash between the marker write and the detail delete: say so, and let
        // the caller finish the delete.
        if manifest.incomplete {
            manifest.incomplete = false;
            write_activity_rollup_manifest(&rollup_partition, &manifest)?;
        }
        return Ok(());
    }

    manifest.incomplete = true;
    write_activity_rollup_manifest(&rollup_partition, &manifest)?;

    std::fs::create_dir_all(&rollup_partition)
        .with_context(|| format!("creating rollup partition {}", rollup_partition.display()))?;
    // A crash between writing the staging file and renaming it leaves the
    // staging file behind. It is not readable as a rollup — no `.parquet`
    // extension — but nothing else would ever remove it, so it would occupy
    // the partition for thirteen months.
    remove_orphan_staging_files(&rollup_partition)?;

    let connection =
        Connection::open_in_memory().context("opening in-memory DuckDB for the activity rollup")?;
    configure_analytics_connection_checked(&connection, "activity_rollup")
        .context("configuring the activity rollup DuckDB")?;
    let input_sql = parquet_source_sql(&pending_sources);
    let detail_rows: i64 = connection
        .query_row(
            &format!(
                "SELECT count(*) FROM read_parquet({input_sql}, hive_partitioning = false, union_by_name = true)"
            ),
            [],
            |row| row.get(0),
        )
        .context("counting activity rollup inputs")
        // One unreadable batch used to refuse the day forever: the detail was
        // kept, the next sweep read the same file, and the day sat past its
        // retention window logging that it needed an operator. Set the
        // unreadable batches aside (moved, never deleted) and let the next
        // sweep roll up what is left; the marker is still `incomplete`, so
        // readers report the gap until then.
        .map_err(|error| with_unreadable_inputs_set_aside(error, dataset, &pending_sources))?;

    let file_name = activity_rollup_file_name(&pending_fingerprints);
    let final_path = rollup_partition.join(&file_name);
    let tmp = rollup_partition.join(format!(".{file_name}.tmp"));
    // `p50`/`p95` are computed per group and are NOT re-aggregatable: averaging
    // two files' percentiles is not a percentile. `count` and `sum_duration_ms`
    // are additive, which is what makes a second rollup generation for the same
    // day safe to sum across. A reader that needs an exact percentile over a
    // window wider than one group has to go to the detail tier, which is
    // exactly the trade this tier is making.
    connection
        .execute_batch(&format!(
            "COPY (
               SELECT
                 dt,
                 hour,
                 principal,
                 workspace,
                 kind,
                 workload_class,
                 agent_id,
                 outcome,
                 CAST(count(*) AS BIGINT) AS \"count\",
                 CAST(round(quantile_cont(duration_ms, 0.5)) AS BIGINT) AS p50_ms,
                 CAST(round(quantile_cont(duration_ms, 0.95)) AS BIGINT) AS p95_ms,
                 CAST(sum(duration_ms) AS BIGINT) AS sum_duration_ms
               FROM read_parquet({input_sql}, hive_partitioning = false, union_by_name = true)
               GROUP BY dt, hour, principal, workspace, kind, workload_class, agent_id, outcome
             ) TO '{}' (FORMAT PARQUET, COMPRESSION 'zstd');",
            escape_sql_literal(&tmp.display().to_string())
        ))
        .context("writing the activity rollup object")
        // `count(*)` can be answered from footers alone, so a batch whose data
        // pages are damaged first fails here, when the rows are decoded.
        .map_err(|error| with_unreadable_inputs_set_aside(error, dataset, &pending_sources))?;
    sync_file(&tmp)?;
    let rollup_rows = count_parquet_rows(&connection, &tmp)?;
    if rollup_rows <= 0 && detail_rows > 0 {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow!(
            "activity rollup guard failed: {detail_rows} detail rows summarised to {rollup_rows} groups"
        ));
    }
    // Hand-rolled rather than the shared durable writer, correctly, and for the
    // same reason as the compaction publish above: the temp's contents come
    // from DuckDB's `COPY`, not from bytes this process holds, so a
    // byte-oriented writer cannot produce it. The durability properties are all
    // here — unique generation temp, `sync_file` above, this rename, and the
    // partition-directory sync below.
    std::fs::rename(&tmp, &final_path).with_context(|| {
        format!(
            "publishing activity rollup {} -> {}",
            tmp.display(),
            final_path.display()
        )
    })?;
    sync_directory(&rollup_partition)?;
    crate::magician_v2::dataset_owners::publish_written_parquet(&final_path).with_context(
        || {
            format!(
                "publishing activity rollup through DatasetAccess {}",
                final_path.display()
            )
        },
    )?;

    manifest.rolled_up_at_ms = Utc::now().timestamp_millis();
    manifest.generations.push(ActivityRollupGeneration {
        rollup_file_name: file_name,
        sources: pending_fingerprints,
        detail_row_count: u64::try_from(detail_rows).unwrap_or(0),
        rollup_row_count: u64::try_from(rollup_rows).unwrap_or(0),
    });
    manifest.incomplete = false;
    write_activity_rollup_manifest(&rollup_partition, &manifest)?;
    Ok(())
}

/// A rollup object's name, derived from the sources it summarises.
///
/// Deterministic on purpose. The crash window this closes is between the
/// publishing rename and the marker write: a re-run recomputes the same source
/// set, derives the same name, and renames over the object the interrupted run
/// already published. A random name would put a second object beside it, and
/// because nothing deduplicates rollup objects on read, every count in that day
/// would double — permanently, since the detail is deleted next.
///
/// A genuinely different source set — a late batch — hashes differently and
/// therefore lands beside the first object rather than over it, which is
/// correct: `count` and `sum_duration_ms` are additive.
fn activity_rollup_file_name(sources: &[SourceFingerprint]) -> String {
    let mut hasher = blake3::Hasher::new();
    for fingerprint in sources {
        hasher.update(fingerprint.file_name.as_bytes());
        hasher.update(b"\0");
        hasher.update(&fingerprint.byte_len.to_le_bytes());
        hasher.update(b"\0");
        hasher.update(fingerprint.checksum_blake3.as_bytes());
        hasher.update(b"\0");
    }
    let digest = hasher.finalize().to_hex();
    format!("rollup_{}.parquet", &digest[..32])
}

/// Remove staging files left by an interrupted publish.
///
/// Only `.<name>.tmp` regular files are touched. This module's own staging
/// names ([`is_maintenance_staging_name`]) go at once: the caller holds the
/// analytics writer guard, so nothing live can own one. Any other writer's
/// staging file — a sink batch, a materializer or repricer object — is removed
/// only once it is older than [`WRITER_STAGING_MIN_AGE`], so an in-flight
/// write is never touched whether or not its writer holds the guard.
fn remove_orphan_staging_files(partition: &Path) -> Result<()> {
    let entries = match std::fs::read_dir(partition) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !(name.starts_with('.') && name.ends_with(".tmp")) || !entry.file_type()?.is_file() {
            continue;
        }
        if !is_maintenance_staging_name(name)
            && !staging_file_is_older_than(&entry.metadata()?, WRITER_STAGING_MIN_AGE)
        {
            continue;
        }
        if let Err(error) = std::fs::remove_file(&path) {
            warn!(
                target: "analytics::parquet_maintenance",
                path = %path.display(),
                error = %error,
                "could not remove an orphaned staging file; it will be retried"
            );
        }
    }
    Ok(())
}

/// Parquet objects anywhere under a day partition that `partition_sources` did
/// not return, hour directories included.
///
/// [`unaccounted_parquet_objects`] reads one directory; a spine day partition is
/// two levels, and the destructive operation — `remove_dir_all` on the day — is
/// recursive. Checking only the day level would leave every hour unguarded.
fn unaccounted_activity_day_objects(
    day_partition: &Path,
    accounted: &[PathBuf],
) -> Result<Vec<PathBuf>> {
    let dataset = PartitionedDataset::ActivityRows;
    let mut directories = vec![day_partition.to_path_buf()];
    directories.extend(activity_hour_dirs(day_partition)?);
    let mut unaccounted = Vec::new();
    for directory in directories {
        // A `batch_*` file the manifest has already verified is hidden by
        // `partition_sources` on purpose — its rows are inside the compacted
        // generation, and an interrupted prune is exactly what leaves one
        // behind. It is recognised, so it is accounted for. Flagging it would
        // wedge retention on a state that heals itself on the next sweep, and
        // the guard is meant to catch objects nothing in this module *knows
        // about* — not objects it has deliberately superseded.
        //
        // Generation-named strays are not given the same pass. An unmanifested
        // generation sitting beside raw files is invisible to
        // `recover_unmanifested_generation` too, and it may hold rows nothing
        // else does; that is precisely what must not be deleted unseen.
        let superseded = raw_partition_files(&directory, dataset).unwrap_or_default();
        for path in unaccounted_parquet_objects(&directory, accounted)? {
            if !superseded.contains(&path) {
                unaccounted.push(path);
            }
        }
    }
    unaccounted.sort();
    Ok(unaccounted)
}

/// Move every rollup object the marker does not name out of the readable name
/// space, and report how many were moved.
///
/// **Precondition: `manifest` must be this partition's real commit record** —
/// either read whole, or the empty default for a partition with no marker at
/// all. A manifest that merely *stands in* for one that could not be read makes
/// every committed generation look like a stray, and this function will rename
/// them all away. That is not a hypothetical: it is what a
/// `MANIFEST_SCHEMA_VERSION` rollback would do to every day holding a late
/// batch, which is why [`read_activity_rollup_manifest`] returns `Err` for an
/// uninterpretable marker rather than `Ok(None)`.
///
/// Renamed rather than deleted, and renamed rather than left alone. Renamed
/// because a stray is redundant *given that precondition* — the detail it
/// summarised is still on disk at the moment this runs, since the caller only
/// deletes detail after the pass that publishes over it succeeds — but "by
/// construction" has been wrong here twice now, and the bytes cost nothing: the
/// whole `dt=` partition is removed at thirteen months either way. The new
/// suffix is what matters, since every reader of this tier selects on the
/// `.parquet` extension.
///
/// A `.superseded-<ulid>` suffix rather than a fixed one so a second sweep
/// cannot silently clobber the evidence left by the first.
fn quarantine_unaccounted_rollup_objects(
    rollup_partition: &Path,
    manifest: &ActivityRollupManifest,
) -> Result<usize> {
    let entries = match std::fs::read_dir(rollup_partition) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let named: Vec<&str> = manifest
        .generations
        .iter()
        .map(|generation| generation.rollup_file_name.as_str())
        .collect();
    // Enumerated to completion before anything is renamed. Mutating a directory
    // while walking it is allowed to skip or repeat entries, and a skipped entry
    // here is a stray that stays readable — the exact outcome this exists to
    // prevent.
    let mut strays = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("parquet") {
            continue;
        }
        if !entry.file_type()?.is_file() {
            continue;
        }
        let is_named = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| named.contains(&name));
        if is_named {
            continue;
        }
        strays.push(path);
    }
    strays.sort();

    let mut quarantined = 0_usize;
    for path in strays {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        let quarantined_path =
            rollup_partition.join(format!("{name}.superseded-{}", ulid::Ulid::new()));
        // Not an atomic write at all — a MOVE. The file is being set aside
        // under a fresh ULID name, not published, so there is no temp to stage
        // and no bytes to write durably; the shared writer has nothing to
        // offer here.
        match std::fs::rename(&path, &quarantined_path) {
            Ok(()) => {
                quarantined += 1;
                warn!(
                    target: "analytics::parquet_maintenance",
                    partition = %rollup_partition.display(),
                    object = %name,
                    quarantined_as = %quarantined_path.display(),
                    "a rollup object the marker does not name was made unreadable; its rows are \
                     being re-aggregated from detail that is still present"
                );
            },
            // Leaving it would put a second aggregation of the same day beside
            // the one about to be published, and the detail is deleted next.
            // Refusing the whole roll-up keeps that detail instead.
            Err(error) => {
                return Err(anyhow!(
                    "could not quarantine unaccounted rollup object {}: {error}",
                    path.display()
                ))
            },
        }
    }
    // Made durable here rather than relying on the `sync_directory` after the
    // publish below: on the resume path there is no publish, and the caller
    // deletes the detail as soon as this returns. An unsynced rename lost to a
    // power failure would restore the stray with nothing left to correct it.
    if quarantined > 0 {
        sync_directory(rollup_partition)?;
    }
    Ok(quarantined)
}

/// The rollup objects a reader should treat as this partition's contents.
///
/// `(files, complete)` when a valid marker exists.
///
/// `files` are exactly the generations the marker names that are present on disk.
/// `complete` is false when the marker says this partition is not complete, or any
/// named file is missing.
/// `None` when there is no valid marker, leaving the
/// caller to fall back to listing the directory: a partition whose marker was
/// lost must stay readable rather than silently answer zero.
///
/// "No valid marker" covers both an absent one and one this build cannot
/// interpret. The read side collapses them on purpose — the answer to both is
/// "list the directory" — while the write side keeps them apart and refuses on
/// the second ([`read_activity_rollup_manifest`]). A reader that guessed wrong
/// reports a number for one query; a writer that guesses wrong renames a day's
/// summary out of existence.
///
/// The marker is the *commit record*. An object it does not name is not part of
/// the dataset yet, which is the correct reading of the one window in which that
/// can happen — published, marker not yet updated — because the detail those
/// rows came from is still on disk and the next sweep republishes them under the
/// same deterministic name. Briefly under-reading an uncommitted object is
/// recoverable; permanently over-reading a stray one is not.
pub fn rollup_generation_files(rollup_partition: &Path) -> Option<(Vec<PathBuf>, bool)> {
    let manifest = read_activity_rollup_manifest(rollup_partition)
        .ok()
        .flatten()?;
    let mut files = Vec::new();
    let mut complete = !manifest.incomplete;
    for generation in &manifest.generations {
        let path = rollup_partition.join(&generation.rollup_file_name);
        if path.is_file() {
            files.push(path);
        } else {
            complete = false;
            warn!(
                target: "analytics::parquet_maintenance",
                partition = %rollup_partition.display(),
                rollup = %generation.rollup_file_name,
                "activity rollup marker names an object that is gone; the day reads short until \
                 it is rebuilt"
            );
        }
    }
    files.sort();
    Some((files, complete))
}

/// The marker written by the build that kept it inside the detail partition.
///
/// Kept only so an in-flight upgrade cannot re-aggregate a day that build
/// already rolled up — which would be exactly the doubling this move exists to
/// prevent. It records a single object name and no source fingerprints.
///
/// `true` means "a pre-fix build published for this day", **not** "there is
/// nothing to do". Only the caller can decide what follows, because the answer
/// depends on whether any detail is still present; see
/// [`roll_up_activity_day`].
fn legacy_rollup_marker_covers_day(day_partition: &Path, rollup_partition: &Path) -> Result<bool> {
    #[derive(Deserialize)]
    struct LegacyMarker {
        rollup_file_name: String,
    }
    let path = day_partition.join("_rollup").join("rolled-up.json");
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let Ok(marker) = serde_json::from_slice::<LegacyMarker>(&raw) else {
        return Ok(false);
    };
    Ok(rollup_partition.join(&marker.rollup_file_name).is_file())
}

fn activity_rollup_manifest_path(rollup_partition: &Path) -> PathBuf {
    rollup_partition.join("_rollup").join("rolled-up.json")
}

/// Read a rollup partition's commit record.
///
/// `Ok(None)` means **there is no marker**. `Err` means **there is one and this
/// build cannot interpret it**. The distinction is the whole point of this
/// function: these two used to collapse into `Ok(None)`, and the write path
/// reads `None` as "this partition's default manifest names no generations".
///
/// That is exactly backwards for an unreadable marker. `roll_up_activity_day`
/// then hands `quarantine_unaccounted_rollup_objects` an empty named-set, and
/// the sweep renames **every** `.parquet` in the partition — committed
/// generations included — out of the readable name space. The justification for
/// quarantining ("the rows are still on disk in the detail tier") does not hold
/// for a committed generation whose detail was deleted at the end of the pass
/// that published it. A day D that rolled up cleanly, then received a late
/// batch, then met an unreadable marker would lose its original summary
/// permanently. A `MANIFEST_SCHEMA_VERSION` bump followed by a rollback makes
/// every such day hit that path at once.
///
/// So an uninterpretable marker refuses instead: `roll_up_activity_day`
/// propagates the error, retention keeps the day's detail and retries next
/// sweep, and nothing is published or renamed. Refusing costs a day of detail on
/// disk; not refusing costs a whole day's committed summary.
///
/// The read side ([`rollup_generation_files`]) deliberately does *not* refuse on
/// this error — it falls back to listing the directory, because a partition
/// whose marker is unreadable must still read long rather than silently zero.
fn read_activity_rollup_manifest(
    rollup_partition: &Path,
) -> Result<Option<ActivityRollupManifest>> {
    let path = activity_rollup_manifest_path(rollup_partition);
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let manifest = serde_json::from_slice::<ActivityRollupManifest>(&raw).map_err(|error| {
        anyhow!(
            "activity rollup marker {} cannot be parsed ({error}); which of this partition's \
             objects are committed generations is unknown, so nothing may be published or \
             quarantined over it",
            path.display()
        )
    })?;
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
        return Err(anyhow!(
            "activity rollup marker {} declares schema version {} and this build understands {}; \
             which of this partition's objects are committed generations is unknown, so nothing \
             may be published or quarantined over it",
            path.display(),
            manifest.schema_version,
            MANIFEST_SCHEMA_VERSION
        ));
    }
    Ok(Some(manifest))
}

fn write_activity_rollup_manifest(
    rollup_partition: &Path,
    manifest: &ActivityRollupManifest,
) -> Result<()> {
    let path = activity_rollup_manifest_path(rollup_partition);
    std::fs::create_dir_all(path.parent().unwrap_or(rollup_partition)).with_context(|| {
        format!(
            "creating activity rollup marker dir for {}",
            rollup_partition.display()
        )
    })?;
    let body = serde_json::to_vec(manifest).context("serializing the activity rollup marker")?;
    crate::magician_v2::artifact_v2::io::write_bytes_durably_sync(&path, &body)
        .with_context(|| format!("writing activity rollup marker {}", path.display()))?;
    Ok(())
}

pub struct StorageMaintenanceRuntime {
    handle: Option<tokio::task::JoinHandle<()>>,
    cancel: CancellationToken,
}

impl StorageMaintenanceRuntime {
    pub fn spawn(workspace: ArtifactV2Workspace) -> Self {
        Self::spawn_inner(workspace, None)
    }

    pub fn spawn_with_chat_store(
        workspace: ArtifactV2Workspace,
        chat_store: Arc<FileChatStore>,
    ) -> Self {
        Self::spawn_inner(workspace, Some(chat_store))
    }

    fn spawn_inner(workspace: ArtifactV2Workspace, chat_store: Option<Arc<FileChatStore>>) -> Self {
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let handle = tokio::spawn(async move {
            // Perform the full sweep once at startup. Afterwards, retention and
            // completed-day work remain on the six-hour cadence while the
            // active LLM partition gets a lightweight bounded-tail check every
            // five minutes.
            let Some(startup_permit) =
                crate::magician_v2::runtime::startup::admit_backfill_or_cancel(&cancel_for_task)
                    .await
            else {
                return;
            };
            let startup_workspace = workspace.clone();
            if let Err(error) = tokio::task::spawn_blocking(move || {
                run_all_scopes(&startup_workspace, CompactionMetricTrigger::Startup)
            })
            .await
            .unwrap_or_else(|error| Err(anyhow!("storage maintenance task panicked: {error}")))
            {
                warn!(
                    target: "analytics::parquet_maintenance",
                    error = %error,
                    "startup storage maintenance sweep failed"
                );
            }
            run_chat_transcript_maintenance(chat_store.as_deref()).await;
            drop(startup_permit);

            let now = tokio::time::Instant::now();
            let mut full_interval = tokio::time::interval_at(
                now + FULL_STORAGE_MAINTENANCE_INTERVAL,
                FULL_STORAGE_MAINTENANCE_INTERVAL,
            );
            let mut hot_interval = tokio::time::interval_at(
                now + HOT_LLM_COMPACTION_INTERVAL,
                HOT_LLM_COMPACTION_INTERVAL,
            );
            full_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            hot_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = cancel_for_task.cancelled() => break,
                    _ = full_interval.tick() => {
                        let workspace = workspace.clone();
                        if let Err(error) = tokio::task::spawn_blocking(move || {
                            run_all_scopes(&workspace, CompactionMetricTrigger::ScheduledFull)
                        })
                            .await
                            .unwrap_or_else(|error| {
                                Err(anyhow!("storage maintenance task panicked: {error}"))
                            })
                        {
                            warn!(
                                target: "analytics::parquet_maintenance",
                                error = %error,
                                "background storage maintenance sweep failed"
                            );
                        }
                    },
                    _ = hot_interval.tick() => {
                        let workspace = workspace.clone();
                        if let Err(error) = tokio::task::spawn_blocking(move || {
                            run_hot_llm_compaction_all_scopes(&workspace)
                        })
                        .await
                        .unwrap_or_else(|error| {
                            Err(anyhow!("hot LLM compaction task panicked: {error}"))
                        })
                        {
                            warn!(
                                target: "analytics::parquet_maintenance",
                                error = %error,
                                "background hot LLM compaction sweep failed"
                            );
                        }
                        run_chat_transcript_maintenance(chat_store.as_deref()).await;
                    },
                }
            }
        });
        Self {
            handle: Some(handle),
            cancel,
        }
    }

    /// Stop scheduling maintenance and await any sweep that already owns the
    /// branch. Shutdown callers use this before final analytics sink drains so
    /// compaction/retention cannot race their last durable writes.
    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle.take() {
            if let Err(error) = handle.await {
                warn!(
                    target: "analytics::parquet_maintenance",
                    error = %error,
                    "storage maintenance task join failed on shutdown"
                );
            }
        }
    }
}

async fn run_chat_transcript_maintenance(chat_store: Option<&FileChatStore>) {
    let Some(chat_store) = chat_store else {
        return;
    };
    match Box::pin(
        chat_store.compact_llm_history_maintenance_batch(CHAT_TRANSCRIPT_MAINTENANCE_BATCH),
    )
    .await
    {
        Ok((inspected, compacted)) if compacted > 0 => tracing::info!(
            target: "analytics::parquet_maintenance",
            inspected,
            compacted,
            "background chat transcript compaction completed"
        ),
        Ok(_) => {},
        Err(error) => warn!(
            target: "analytics::parquet_maintenance",
            %error,
            "background chat transcript compaction batch failed"
        ),
    }
}

impl Drop for StorageMaintenanceRuntime {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

fn run_all_scopes(workspace: &ArtifactV2Workspace, trigger: CompactionMetricTrigger) -> Result<()> {
    // Compaction runs over every directory on disk, invalid system scopes
    // included: maintenance of existing data, not tenant enumeration.
    for (principal, scope) in workspace.list_all_scope_dirs_sync()? {
        if let Err(error) = maintain_scope(workspace, &principal, &scope, trigger) {
            warn!(
                target: "analytics::parquet_maintenance",
                principal,
                workspace = scope,
                error = %error,
                "scope storage maintenance failed; continuing with remaining scopes"
            );
        }
    }
    Ok(())
}

fn run_hot_llm_compaction_all_scopes(workspace: &ArtifactV2Workspace) -> Result<()> {
    // Compaction runs over every directory on disk, invalid system scopes
    // included: maintenance of existing data, not tenant enumeration.
    for (principal, scope) in workspace.list_all_scope_dirs_sync()? {
        let started_at_ms = Utc::now().timestamp_millis();
        // The spine rides the five-minute lane, not the six-hour one. At a
        // sixty-second flush a busy scope writes ~60 objects an hour; waiting
        // for the full sweep would let ~360 of them accumulate before anything
        // folded. Folding on this cadence keeps the transient file count near
        // one hour's worth. It is cheap when there is nothing to do: an hour
        // that is already folded costs one directory listing.
        if let Err(error) = compact_activity_rows(workspace, &principal, &scope) {
            warn!(
                target: "analytics::parquet_maintenance",
                principal,
                workspace = scope,
                error = %error,
                "activity spine compaction failed; continuing with remaining scopes"
            );
        }
        let legacy_llm = compact_open_legacy_llm_partition(
            workspace,
            &principal,
            &scope,
            OpenPartitionPolicy::default(),
        )
        .unwrap_or_else(|error| {
            warn!(
                target: "analytics::parquet_maintenance",
                principal,
                workspace = scope,
                error = %error,
                "open legacy LLM partition compaction failed; continuing with canonical facts"
            );
            ParquetCompactionStats::default()
        });
        if let Err(error) = maintain_hot_llm_scope(
            workspace,
            &principal,
            &scope,
            super::llm_fact_compactor::DEFAULT_LLM_FACT_ROLLING_TAIL_FILES,
        )
        .map(|canonical_llm| {
            record_compaction_metrics(
                workspace,
                CompactionMetricTrigger::ScheduledHot,
                &StorageMaintenanceReport {
                    principal: principal.clone(),
                    workspace: scope.clone(),
                    started_at_ms,
                    completed_at_ms: Utc::now().timestamp_millis(),
                    duckdb: Vec::new(),
                    parquet: Some(legacy_llm),
                    canonical_llm: Some(canonical_llm),
                    retention: None,
                },
            );
        }) {
            warn!(
                target: "analytics::parquet_maintenance",
                principal,
                workspace = scope,
                error = %error,
                "hot LLM fact compaction failed; continuing with remaining scopes"
            );
        }
    }
    Ok(())
}

fn maintain_hot_llm_scope(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    scope: &str,
    min_uncompacted_files: usize,
) -> Result<super::llm_fact_compactor::LlmFactCompactionStats> {
    let llm_scope = magicllm::LlmScope::new(principal.to_string(), scope.to_string());
    super::llm_fact_compactor::compact_current_scope(workspace, &llm_scope, min_uncompacted_files)
}

fn maintain_scope(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    scope: &str,
    trigger: CompactionMetricTrigger,
) -> Result<()> {
    let started_at_ms = Utc::now().timestamp_millis();
    // Compaction and retention are independent obligations, and this function
    // used to make the second depend on the first: any `?` here aborted the
    // scope, so a single unreadable partition anywhere in compaction meant no
    // dataset in the scope ever expired again. Compaction failing is a file
    // count that stops improving; retention failing is a disk that fills. The
    // sweep reports both and performs whichever it can.
    let mut parquet = compact_scope(workspace, principal, scope, 2).unwrap_or_else(|error| {
        warn!(
            target: "analytics::parquet_maintenance",
            principal,
            workspace = scope,
            error = %error,
            "scope Parquet compaction failed; retention still runs"
        );
        ParquetCompactionStats::default()
    });
    // The spine folds on its own two-level schedule; absorbing its stats here
    // means `/storage` reports it beside every other dataset instead of it
    // being maintenance nobody can see.
    match compact_activity_rows(workspace, principal, scope) {
        Ok(activity) => parquet.absorb(activity),
        Err(error) => warn!(
            target: "analytics::parquet_maintenance",
            principal,
            workspace = scope,
            error = %error,
            "activity spine compaction failed; retention still runs"
        ),
    }
    let llm_scope = magicllm::LlmScope::new(principal.to_string(), scope.to_string());
    let canonical_llm = super::llm_fact_compactor::compact_scope(
        workspace,
        &llm_scope,
        super::llm_fact_compactor::LlmFactCompactionPolicy::default(),
    )
    .unwrap_or_else(|error| {
        warn!(
            target: "analytics::parquet_maintenance",
            principal,
            workspace = scope,
            error = %error,
            "canonical LLM fact compaction failed; retention still runs"
        );
        super::llm_fact_compactor::LlmFactCompactionStats::default()
    });
    let mut retention = apply_retention(
        workspace,
        principal,
        scope,
        DEFAULT_ANALYTICS_RETENTION_DAYS,
    )
    .unwrap_or_else(|error| {
        warn!(
            target: "analytics::parquet_maintenance",
            principal,
            workspace = scope,
            error = %error,
            "flat analytics retention failed; the spine's tiered retention still runs"
        );
        RetentionStats::default()
    });
    // Tiered, not flat: expired detail becomes a rollup before it is removed.
    // Runs after `apply_retention` rather than inside it because that function
    // deletes outright, which for this dataset would throw away the only
    // record of what the machine spent last month doing.
    let activity_retention =
        apply_activity_retention(workspace, principal, scope).unwrap_or_else(|error| {
            warn!(
                target: "analytics::parquet_maintenance",
                principal,
                workspace = scope,
                error = %error,
                "activity spine retention failed; the flat sweep's result still stands"
            );
            RetentionStats::default()
        });
    retention.partitions_scanned += activity_retention.partitions_scanned;
    retention.partitions_removed += activity_retention.partitions_removed;
    retention.bytes_removed = retention
        .bytes_removed
        .saturating_add(activity_retention.bytes_removed);
    record_compaction_metrics(
        workspace,
        trigger,
        &StorageMaintenanceReport {
            principal: principal.to_string(),
            workspace: scope.to_string(),
            started_at_ms,
            completed_at_ms: Utc::now().timestamp_millis(),
            duckdb: Vec::new(),
            parquet: Some(parquet),
            canonical_llm: Some(canonical_llm),
            retention: Some(retention),
        },
    );
    Ok(())
}

fn partition_dirs(root: &Path) -> Result<Vec<(NaiveDate, PathBuf)>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut partitions = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let Some(date) = partition_date(&path) else {
            continue;
        };
        if !entry.file_type()?.is_dir() {
            return Err(anyhow!(
                "analytics partition must be a real directory: {}",
                path.display()
            ));
        }
        partitions.push((date, path));
    }
    partitions.sort_by_key(|(date, _)| *date);
    Ok(partitions)
}

fn partition_date(path: &Path) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(path.file_name()?.to_str()?.strip_prefix("dt=")?, "%Y-%m-%d").ok()
}

fn partition_date_text(path: &Path) -> Option<String> {
    Some(partition_date(path)?.format("%Y-%m-%d").to_string())
}

fn manifest_path(partition: &Path, dataset: PartitionedDataset) -> PathBuf {
    partition
        .join("_compact")
        .join(format!("{}.compaction-manifest.json", dataset.as_str()))
}

fn write_manifest_atomic(
    partition: &Path,
    dataset: PartitionedDataset,
    manifest: &PartitionCompactionManifest,
) -> Result<()> {
    let path = manifest_path(partition, dataset);
    let parent = path.parent().context("manifest path missing parent")?;
    std::fs::create_dir_all(parent)?;
    // The symlink guard is this store's own invariant; it stays in front of the
    // shared writer, which will happily follow a symlinked `_compact`.
    if !std::fs::symlink_metadata(parent)?.file_type().is_dir() {
        return Err(anyhow!(
            "compaction manifest parent is not a real directory"
        ));
    }
    crate::magician_v2::artifact_v2::io::write_bytes_durably_sync(
        &path,
        &serde_json::to_vec_pretty(manifest)?,
    )
    .with_context(|| format!("write compaction manifest {}", path.display()))
}

fn source_fingerprints(files: &[PathBuf]) -> Result<Vec<SourceFingerprint>> {
    files.iter().map(|path| source_fingerprint(path)).collect()
}

fn source_fingerprint(path: &Path) -> Result<SourceFingerprint> {
    Ok(SourceFingerprint {
        file_name: path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("non-UTF8 Parquet source name"))?
            .to_string(),
        byte_len: std::fs::metadata(path)?.len(),
        checksum_blake3: file_checksum(path)?,
    })
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChecksumStamp {
    len: u64,
    modified: SystemTime,
    device: u64,
    inode: u64,
    changed: (i64, i64),
}

#[cfg(unix)]
impl ChecksumStamp {
    fn read(path: &Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(path).ok()?;
        if !metadata.file_type().is_file() {
            return None;
        }
        Some(Self {
            len: metadata.len(),
            modified: metadata.modified().ok()?,
            device: metadata.dev(),
            inode: metadata.ino(),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

#[cfg(unix)]
#[derive(Default)]
struct ChecksumCache {
    entries: HashMap<PathBuf, (ChecksumStamp, String)>,
    order: VecDeque<PathBuf>,
}

#[cfg(unix)]
static VERIFIED_CHECKSUMS: LazyLock<Mutex<ChecksumCache>> =
    LazyLock::new(|| Mutex::new(ChecksumCache::default()));

fn file_checksum(path: &Path) -> Result<String> {
    // Compacted objects and their retained sources are immutable. Preserve the
    // checksum verdict only while identity, size, mtime AND ctime are unchanged;
    // even an in-place rewrite that restores mtime must be rehashed.
    #[cfg(unix)]
    let before = ChecksumStamp::read(path);
    #[cfg(unix)]
    if let Some(stamp) = before {
        if let Ok(cache) = VERIFIED_CHECKSUMS.lock() {
            if let Some((cached_stamp, checksum)) = cache.entries.get(path) {
                if *cached_stamp == stamp {
                    return Ok(checksum.clone());
                }
            }
        }
    }
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let checksum = hasher.finalize().to_hex().to_string();
    #[cfg(unix)]
    if let Some(stamp) = before.filter(|stamp| Some(*stamp) == ChecksumStamp::read(path)) {
        if let Ok(mut cache) = VERIFIED_CHECKSUMS.lock() {
            if !cache.entries.contains_key(path) {
                const MAX_CHECKSUMS: usize = 8192;
                while cache.entries.len() >= MAX_CHECKSUMS {
                    if let Some(oldest) = cache.order.pop_front() {
                        cache.entries.remove(&oldest);
                    } else {
                        break;
                    }
                }
                cache.order.push_back(path.to_path_buf());
            }
            cache
                .entries
                .insert(path.to_path_buf(), (stamp, checksum.clone()));
        }
    }
    Ok(checksum)
}

fn count_parquet_rows(connection: &Connection, path: &Path) -> Result<i64> {
    connection
        .query_row(
            &format!(
                "SELECT count(*) FROM read_parquet('{}', hive_partitioning = false)",
                escape_sql_literal(&path.display().to_string())
            ),
            [],
            |row| row.get(0),
        )
        .context("counting compacted Parquet rows")
}

fn parquet_source_sql(files: &[PathBuf]) -> String {
    let values = files
        .iter()
        .map(|path| format!("'{}'", escape_sql_literal(&path.display().to_string())))
        .collect::<Vec<_>>();
    if values.len() == 1 {
        values[0].clone()
    } else {
        format!("[{}]", values.join(", "))
    }
}

fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

fn directory_apparent_bytes(path: &Path) -> Result<u64> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {},
        Ok(metadata) if metadata.file_type().is_file() => return Ok(metadata.len()),
        Ok(_) => return Ok(0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    }
    let mut total = 0_u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_file() {
            let metadata = entry.metadata()?;
            total = total.saturating_add(metadata.len());
        } else if file_type.is_dir() {
            total = total.saturating_add(directory_apparent_bytes(&entry.path())?);
        }
    }
    Ok(total)
}

fn directory_regular_file_count(path: &Path) -> Result<usize> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_file() {
        return Ok(1);
    }
    if !metadata.file_type().is_dir() {
        return Ok(0);
    }
    let mut count = 0_usize;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            count = count.saturating_add(directory_regular_file_count(&entry.path())?);
        } else if file_type.is_file() {
            count = count.saturating_add(1);
        }
    }
    Ok(count)
}

fn sync_file(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

/// Writer test support: after a completed write the partition holds only
/// published `.parquet` objects — no staging file — and a staging file left
/// beside them by a writer that died is invisible to the manifest-aware
/// source listing and to a `*.parquet` glob, and is not swept while young.
#[cfg(any(test, feature = "test-fixtures"))]
pub(crate) fn assert_published_without_staging(
    partition: &Path,
    dataset: PartitionedDataset,
    expected_rows: i64,
) {
    let names: Vec<String> = std::fs::read_dir(partition)
        .expect("partition listing")
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .collect();
    assert!(
        names.iter().all(|name| !name.ends_with(".tmp")),
        "a completed write left a staging file behind: {names:?}"
    );
    let published = partition_sources(partition, dataset).expect("partition sources");
    assert!(
        !published.is_empty(),
        "the write published nothing: {names:?}"
    );

    let staging = parquet_staging_path(&partition.join(format!(
        "{}_01ARZ3NDEKTSV4RRFFQ69G5FAV.parquet",
        if dataset == PartitionedDataset::LlmEmbeddings {
            "embed"
        } else {
            "batch"
        }
    )));
    std::fs::write(&staging, b"a dead writer's half-written object").expect("staging file");
    assert_eq!(
        partition_sources(partition, dataset).expect("partition sources"),
        published,
        "a staging file is never a partition source"
    );
    let glob = escape_sql_literal(&partition.join("*.parquet").display().to_string());
    let connection = Connection::open_in_memory().expect("duckdb");
    let rows: i64 = connection
        .query_row(
            &format!("SELECT count(*) FROM read_parquet('{glob}', union_by_name = true)"),
            [],
            |row| row.get(0),
        )
        .expect("a *.parquet glob must not pick up the staging file");
    assert_eq!(rows, expected_rows);
    remove_orphan_staging_files(partition).expect("staging sweep");
    assert!(
        staging.exists(),
        "a young writer staging file may belong to a live write and must survive the sweep"
    );
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    #[cfg(unix)]
    #[test]
    fn checksum_cache_revalidates_rewrites_even_when_mtime_is_restored() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("object.parquet");
        std::fs::write(&path, b"first").unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let original = super::file_checksum(&path).unwrap();
        assert_eq!(original, super::file_checksum(&path).unwrap());
        std::fs::write(&path, b"other").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert_ne!(original, super::file_checksum(&path).unwrap());
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"first").unwrap();
        assert_eq!(original, super::file_checksum(&path).unwrap());
    }

    use super::*;

    #[tokio::test]
    async fn storage_maintenance_shutdown_cancels_and_joins_the_worker() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let runtime = StorageMaintenanceRuntime::spawn(ArtifactV2Workspace::new(temporary.path()));

        tokio::time::timeout(Duration::from_secs(2), runtime.shutdown())
            .await
            .expect("maintenance shutdown must not leave a detached worker");
    }

    fn write_batch(path: &Path, start: i64, count: i64) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let connection = Connection::open_in_memory().expect("duckdb");
        connection
            .execute_batch("CREATE TABLE rows(id BIGINT, value VARCHAR)")
            .expect("schema");
        for id in start..start + count {
            connection
                .execute("INSERT INTO rows VALUES (?, 'value')", [id])
                .expect("insert");
        }
        connection
            .execute_batch(&format!(
                "COPY rows TO '{}' (FORMAT PARQUET)",
                escape_sql_literal(&path.display().to_string())
            ))
            .expect("write parquet");
    }

    fn write_llm_fact(path: &Path) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let connection = Connection::open_in_memory().expect("duckdb");
        connection
            .execute_batch(&format!(
                "CREATE TABLE fixture(idempotency_key VARCHAR, journal_sequence UBIGINT, record_kind VARCHAR, principal VARCHAR, workspace VARCHAR); INSERT INTO fixture VALUES ('call:hot:r1', 1, 'call_fact', 'owner', 'default'); COPY fixture TO '{}' (FORMAT PARQUET)",
                escape_sql_literal(&path.display().to_string()),
            ))
            .expect("write LLM fact");
    }

    #[test]
    fn hot_maintenance_path_compacts_only_the_active_llm_partition() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let current = workspace
            .analytics_llm_calls_root("owner", "default")
            .join(format!("dt={}", Utc::now().format("%Y-%m-%d")));
        write_llm_fact(&current.join("part-call_fact-hot-r1.parquet"));

        let stats =
            maintain_hot_llm_scope(&workspace, "owner", "default", 1).expect("hot maintenance");
        assert_eq!(stats.partitions_scanned, 1);
        assert_eq!(stats.partitions_compacted, 1);
        assert!(current
            .join("_compact")
            .join("canonical.compaction-manifest.json")
            .is_file());
    }

    #[test]
    fn scheduled_scope_maintenance_records_effective_compaction_metrics() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let date = Utc::now().date_naive() - ChronoDuration::days(1);
        let partition = workspace
            .analytics_memory_events_root("owner", "default")
            .join(format!("dt={date}"));
        write_batch(&partition.join("batch_1.parquet"), 0, 2);
        write_batch(&partition.join("batch_2.parquet"), 2, 2);

        maintain_scope(
            &workspace,
            "owner",
            "default",
            CompactionMetricTrigger::ScheduledFull,
        )
        .expect("scheduled maintenance");

        let metrics = crate::magician_v2::storage_governance::compaction_metrics::snapshot(
            &workspace, "owner", "default",
        )
        .expect("metrics");
        assert_eq!(metrics.event_count, 1);
        assert_eq!(
            metrics.recent_events[0].trigger,
            CompactionMetricTrigger::ScheduledFull
        );
        assert_eq!(metrics.recent_events[0].area, "memory_events");
        assert_eq!(metrics.recent_events[0].files_compacted, 2);
    }

    #[test]
    fn completed_partition_compaction_prunes_verified_raw_and_preserves_rows() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let date = Utc::now().date_naive() - ChronoDuration::days(1);
        let partition = workspace
            .analytics_memory_events_root("owner", "default")
            .join(format!("dt={date}"));
        {
            let _guard = super::super::duckdb_safety::analytics_duckdb_guard();
            write_batch(&partition.join("batch_1.parquet"), 0, 2);
            write_batch(&partition.join("batch_2.parquet"), 2, 3);
        }

        let stats = compact_dataset(
            &workspace,
            "owner",
            "default",
            PartitionedDataset::MemoryEvents,
            2,
        )
        .expect("compact");
        assert_eq!(stats.partitions_compacted, 1);
        assert_eq!(stats.raw_files_pruned, 2);
        assert_eq!(stats.areas.len(), 1);
        assert_eq!(stats.areas[0].dataset, "memory_events");
        assert_eq!(stats.areas[0].raw_files_compacted, 2);
        assert_eq!(stats.areas[0].files_before, 2);
        assert!(stats.areas[0].files_after >= 2); // compacted object + manifest
        assert_eq!(stats.areas[0].bytes_reclaimed, stats.bytes_reclaimed);
        assert_eq!(stats.areas[0].query_files_avoided, 1);
        assert!(!partition.join("batch_1.parquet").exists());
        assert!(!partition.join("batch_2.parquet").exists());
        let compacted = compacted_file(&partition, PartitionedDataset::MemoryEvents);
        assert!(compacted.is_file());
        let connection = Connection::open_in_memory().expect("duckdb");
        assert_eq!(
            count_parquet_rows(&connection, &compacted).expect("rows"),
            5
        );
        assert_eq!(
            partition_sources(&partition, PartitionedDataset::MemoryEvents).expect("sources"),
            vec![compacted]
        );
    }

    #[test]
    fn embedding_batches_use_their_stream_prefix_and_compact_losslessly() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let date = Utc::now().date_naive() - ChronoDuration::days(1);
        let partition = workspace
            .analytics_llm_embeddings_root("owner", "default")
            .join(format!("dt={date}"));
        {
            let _guard = super::super::duckdb_safety::analytics_duckdb_guard();
            write_batch(&partition.join("embed_1.parquet"), 0, 2);
            write_batch(&partition.join("embed_2.parquet"), 2, 1);
        }

        let stats = compact_dataset(
            &workspace,
            "owner",
            "default",
            PartitionedDataset::LlmEmbeddings,
            2,
        )
        .expect("compact embeddings");
        assert_eq!(stats.partitions_compacted, 1);
        assert_eq!(stats.raw_files_pruned, 2);
        assert_eq!(
            count_parquet_rows(
                &Connection::open_in_memory().expect("duckdb"),
                &compacted_file(&partition, PartitionedDataset::LlmEmbeddings),
            )
            .expect("rows"),
            3
        );
    }

    #[test]
    fn late_completed_partition_batch_is_merged_without_losing_prior_compaction() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let date = Utc::now().date_naive() - ChronoDuration::days(2);
        let partition = workspace
            .analytics_root("owner", "default")
            .join("events")
            .join(format!("dt={date}"));
        {
            let _guard = super::super::duckdb_safety::analytics_duckdb_guard();
            write_batch(&partition.join("batch_1.parquet"), 0, 2);
        }
        compact_dataset(
            &workspace,
            "owner",
            "default",
            PartitionedDataset::Events,
            1,
        )
        .expect("first compact");
        {
            let _guard = super::super::duckdb_safety::analytics_duckdb_guard();
            write_batch(&partition.join("batch_late.parquet"), 2, 2);
        }
        let visible_before_merge =
            partition_sources(&partition, PartitionedDataset::Events).expect("late sources");
        assert_eq!(visible_before_merge.len(), 2);
        assert!(visible_before_merge
            .iter()
            .any(|path| path.file_name().and_then(|name| name.to_str())
                == Some("batch_late.parquet")));
        compact_dataset(
            &workspace,
            "owner",
            "default",
            PartitionedDataset::Events,
            1,
        )
        .expect("late compact");
        let connection = Connection::open_in_memory().expect("duckdb");
        assert_eq!(
            count_parquet_rows(
                &connection,
                &compacted_file(&partition, PartitionedDataset::Events)
            )
            .expect("rows"),
            4
        );
        assert_eq!(
            partition_sources(&partition, PartitionedDataset::Events)
                .expect("governed source")
                .len(),
            1
        );
        assert_eq!(
            partition
                .read_dir()
                .expect("partition")
                .flatten()
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with("events.compacted."))
                })
                .count(),
            1,
            "only the manifest-selected compacted generation remains"
        );
    }

    /// Governed rows of one legacy LLM partition, failing on any row read
    /// twice, with the number of files a reader opens.
    fn governed_legacy_llm_rows(partition: &Path) -> (i64, usize) {
        let sources =
            partition_sources(partition, PartitionedDataset::LegacyLlmCalls).expect("sources");
        let connection = Connection::open_in_memory().expect("duckdb");
        let (rows, distinct): (i64, i64) = connection
            .query_row(
                &format!(
                    "SELECT count(*), count(DISTINCT id) FROM read_parquet({}, union_by_name = true)",
                    parquet_source_sql(&sources)
                ),
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("governed rows");
        assert_eq!(rows, distinct, "a governed reader saw a row twice");
        (rows, sources.len())
    }

    fn legacy_llm_generation_count(partition: &Path) -> usize {
        partition
            .read_dir()
            .expect("partition")
            .flatten()
            .filter(|entry| {
                entry.file_name().to_str().is_some_and(|name| {
                    PartitionedDataset::LegacyLlmCalls.is_compacted_file_name(name)
                })
            })
            .count()
    }

    /// Move the manifest's publication back in time, as a later pass sees it.
    fn age_legacy_llm_manifest(partition: &Path, age: Duration) {
        let mut manifest = read_valid_manifest(partition, PartitionedDataset::LegacyLlmCalls)
            .expect("manifest")
            .expect("a fold published one");
        manifest.generated_at_ms -= i64::try_from(age.as_millis()).expect("age");
        write_manifest_atomic(partition, PartitionedDataset::LegacyLlmCalls, &manifest)
            .expect("age the manifest");
    }

    fn rows_in_files(files: &[PathBuf]) -> i64 {
        Connection::open_in_memory()
            .expect("duckdb")
            .query_row(
                &format!(
                    "SELECT count(*) FROM read_parquet({}, union_by_name = true)",
                    parquet_source_sql(files)
                ),
                [],
                |row| row.get(0),
            )
            .expect("rows")
    }

    #[test]
    fn open_legacy_llm_partition_rolls_its_tail_without_losing_or_double_counting_rows() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let partition = workspace
            .analytics_llm_calls_root("owner", "default")
            .join(format!("dt={}", Utc::now().format("%Y-%m-%d")));
        let policy = OpenPartitionPolicy {
            max_raw_files: 3,
            max_tail_age: Duration::from_secs(60 * 60),
        };
        let fold = || {
            compact_open_legacy_llm_partition(&workspace, "owner", "default", policy)
                .expect("open partition fold")
        };
        let raw = |partition: &Path| {
            raw_partition_files(partition, PartitionedDataset::LegacyLlmCalls).expect("raw")
        };
        let past_grace = LEGACY_LLM_SOURCE_RETIREMENT_GRACE + Duration::from_secs(60);

        // Young and below the bound: left raw.
        write_batch(&partition.join("batch_01.parquet"), 0, 2);
        write_batch(&partition.join("batch_02.parquet"), 2, 2);
        let stats = fold();
        assert_eq!(stats.partitions_scanned, 1);
        assert_eq!(stats.partitions_compacted, 0);
        assert_eq!(governed_legacy_llm_rows(&partition), (4, 2));

        // The bound is reached: one generation. A reader that chose the three
        // batches just before the fold still opens them during the grace, and
        // a reader choosing now gets the generation alone (no double count).
        write_batch(&partition.join("batch_03.parquet"), 4, 2);
        let chosen_before_fold =
            partition_sources(&partition, PartitionedDataset::LegacyLlmCalls).expect("sources");
        let stats = fold();
        assert_eq!(stats.partitions_compacted, 1);
        assert_eq!(stats.raw_files_compacted, 3);
        assert_eq!(stats.raw_files_pruned, 0);
        assert_eq!(
            raw(&partition).len(),
            3,
            "folded batches wait out the grace"
        );
        assert_eq!(rows_in_files(&chosen_before_fold), 6);
        assert_eq!(governed_legacy_llm_rows(&partition), (6, 1));

        // A late writer lands after the fold: visible as tail, counted once,
        // and not folded until the generation is an hour old.
        write_batch(&partition.join("batch_04.parquet"), 6, 2);
        assert_eq!(fold().partitions_compacted, 0);
        assert_eq!(raw(&partition).len(), 4, "the grace has not passed");
        assert_eq!(governed_legacy_llm_rows(&partition), (8, 2));

        // An hour later: the first fold's batches retire, and the tail folds
        // into a second generation; the first generation and batch_04 now wait
        // out their own grace, still readable by whoever chose them.
        age_legacy_llm_manifest(&partition, Duration::from_secs(2 * 60 * 60));
        let chosen_before_fold =
            partition_sources(&partition, PartitionedDataset::LegacyLlmCalls).expect("sources");
        let stats = fold();
        assert_eq!(stats.raw_files_pruned, 3);
        assert_eq!(stats.partitions_compacted, 1);
        assert_eq!(stats.raw_files_compacted, 1);
        assert_eq!(raw(&partition), vec![partition.join("batch_04.parquet")]);
        assert_eq!(legacy_llm_generation_count(&partition), 2);
        assert_eq!(rows_in_files(&chosen_before_fold), 8);
        assert_eq!(governed_legacy_llm_rows(&partition), (8, 1));

        // A fold inside the grace carries the waiting source forward, so it is
        // still never read twice.
        write_batch(&partition.join("batch_05.parquet"), 8, 2);
        write_batch(&partition.join("batch_06.parquet"), 10, 2);
        write_batch(&partition.join("batch_07.parquet"), 12, 2);
        assert_eq!(fold().partitions_compacted, 1);
        assert_eq!(raw(&partition).len(), 4);
        assert_eq!(governed_legacy_llm_rows(&partition), (14, 1));

        // Past the grace, a pass with nothing to fold sweeps every leftover.
        age_legacy_llm_manifest(&partition, past_grace);
        let stats = fold();
        assert_eq!(stats.partitions_compacted, 0);
        assert_eq!(stats.raw_files_pruned, 4);
        assert!(raw(&partition).is_empty());
        assert_eq!(legacy_llm_generation_count(&partition), 1);
        assert_eq!(governed_legacy_llm_rows(&partition), (14, 1));

        // The closed-day sweep never folds the open partition.
        write_batch(&partition.join("batch_08.parquet"), 14, 2);
        let closed = compact_dataset(
            &workspace,
            "owner",
            "default",
            PartitionedDataset::LegacyLlmCalls,
            1,
        )
        .expect("closed-day sweep");
        assert_eq!(closed.partitions_scanned, 0);
        assert_eq!(governed_legacy_llm_rows(&partition), (16, 2));
    }

    #[test]
    fn open_legacy_llm_partition_first_fold_waits_for_its_oldest_batch_to_age() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let partition = workspace
            .analytics_llm_calls_root("owner", "default")
            .join(format!("dt={}", Utc::now().format("%Y-%m-%d")));
        let first = partition.join("batch_01.parquet");
        write_batch(&first, 0, 2);
        write_batch(&partition.join("batch_02.parquet"), 2, 2);
        let fold = || {
            compact_open_legacy_llm_partition(
                &workspace,
                "owner",
                "default",
                OpenPartitionPolicy::default(),
            )
            .expect("open partition fold")
        };
        assert_eq!(fold().partitions_compacted, 0);

        backdate(&first, Duration::from_secs(2 * 60 * 60));
        assert_eq!(fold().partitions_compacted, 1);
        assert_eq!(governed_legacy_llm_rows(&partition), (4, 1));
    }

    #[test]
    fn unpublished_compacted_generation_never_supersedes_manifest_selection() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let date = Utc::now().date_naive() - ChronoDuration::days(2);
        let partition = workspace
            .analytics_root("owner", "default")
            .join("events")
            .join(format!("dt={date}"));
        {
            let _guard = super::super::duckdb_safety::analytics_duckdb_guard();
            write_batch(&partition.join("batch_1.parquet"), 0, 2);
        }
        compact_dataset(
            &workspace,
            "owner",
            "default",
            PartitionedDataset::Events,
            1,
        )
        .expect("first compact");
        let selected = compacted_file(&partition, PartitionedDataset::Events);
        let orphan =
            partition.join(PartitionedDataset::Events.generation_file_name(&ulid::Ulid::new()));
        std::fs::copy(&selected, &orphan).expect("simulate unpublished generation");

        assert_eq!(
            partition_sources(&partition, PartitionedDataset::Events)
                .expect("manifest-selected sources"),
            vec![selected.clone()],
            "a durable object without a published manifest is never selected"
        );

        let stats = compact_dataset(
            &workspace,
            "owner",
            "default",
            PartitionedDataset::Events,
            1,
        )
        .expect("recovery sweep");
        assert_eq!(stats.partitions_already_compacted, 1);
        assert!(selected.exists());
        assert!(!orphan.exists(), "recovery removes the inactive generation");
    }

    #[test]
    fn manifest_selected_generation_hides_and_recovers_verified_leftover_raw() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let date = Utc::now().date_naive() - ChronoDuration::days(2);
        let partition = workspace
            .analytics_memory_events_root("owner", "default")
            .join(format!("dt={date}"));
        let raw = partition.join("batch_1.parquet");
        {
            let _guard = super::super::duckdb_safety::analytics_duckdb_guard();
            write_batch(&raw, 0, 2);
        }
        let raw_bytes = std::fs::read(&raw).expect("raw bytes");
        compact_dataset(
            &workspace,
            "owner",
            "default",
            PartitionedDataset::MemoryEvents,
            1,
        )
        .expect("compact");
        std::fs::write(&raw, raw_bytes).expect("simulate crash before raw unlink");

        let selected = compacted_file(&partition, PartitionedDataset::MemoryEvents);
        assert_eq!(
            partition_sources(&partition, PartitionedDataset::MemoryEvents)
                .expect("governed sources"),
            vec![selected],
            "a verified source already represented by the manifest is not read twice"
        );
        let stats = compact_dataset(
            &workspace,
            "owner",
            "default",
            PartitionedDataset::MemoryEvents,
            1,
        )
        .expect("recovery sweep");
        assert_eq!(stats.raw_files_pruned, 1);
        assert!(!raw.exists());
    }

    #[test]
    fn sole_generation_remains_readable_after_manifest_corruption() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let date = Utc::now().date_naive() - ChronoDuration::days(2);
        let partition = workspace
            .analytics_root("owner", "default")
            .join("events")
            .join(format!("dt={date}"));
        {
            let _guard = super::super::duckdb_safety::analytics_duckdb_guard();
            write_batch(&partition.join("batch_1.parquet"), 0, 2);
        }
        compact_dataset(
            &workspace,
            "owner",
            "default",
            PartitionedDataset::Events,
            1,
        )
        .expect("compact");
        let selected = compacted_file(&partition, PartitionedDataset::Events);
        std::fs::write(
            manifest_path(&partition, PartitionedDataset::Events),
            b"not-json",
        )
        .expect("corrupt manifest");

        assert_eq!(
            partition_sources(&partition, PartitionedDataset::Events)
                .expect("unambiguous recovery"),
            vec![selected]
        );
    }

    #[test]
    fn retention_covers_memory_and_tool_lineage_but_not_mail_storage() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let old = (Utc::now().date_naive() - ChronoDuration::days(120)).to_string();
        for root in [
            workspace.analytics_memory_events_root("owner", "default"),
            workspace.analytics_llm_embeddings_root("owner", "default"),
            workspace.analytics_llm_tool_calls_root("owner", "default"),
        ] {
            std::fs::create_dir_all(root.join(format!("dt={old}"))).expect("partition");
        }
        let mail = workspace.channel_assist_db_path("owner", "default");
        std::fs::create_dir_all(mail.parent().expect("parent")).expect("mail dir");
        std::fs::write(&mail, b"sentinel").expect("mail sentinel");

        let stats = apply_retention(&workspace, "owner", "default", 90).expect("retention");
        assert_eq!(stats.partitions_removed, 3);
        assert!(mail.exists(), "telemetry retention must never touch mail");
    }

    #[test]
    fn prune_refuses_a_source_changed_after_verification() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let source = temporary.path().join("batch_1.parquet");
        std::fs::write(&source, b"original").expect("source");
        let expected = vec![source_fingerprint(&source).expect("fingerprint")];
        std::fs::write(&source, b"tampered").expect("tamper");

        let error = prune_verified_sources(&[source.clone()], &expected)
            .expect_err("changed source must not be pruned");
        assert!(error.to_string().contains("fingerprint"));
        assert!(source.exists());
    }

    #[test]
    fn retention_keeps_cutoff_and_current_partitions() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let root = workspace.analytics_memory_events_root("owner", "default");
        let cutoff = Utc::now().date_naive() - ChronoDuration::days(90);
        let current = Utc::now().date_naive();
        for date in [cutoff, current] {
            let partition = root.join(format!("dt={date}"));
            std::fs::create_dir_all(&partition).expect("partition");
            std::fs::write(partition.join("batch.parquet"), b"kept").expect("sentinel");
        }

        let stats = apply_retention(&workspace, "owner", "default", 90).expect("retention");
        assert_eq!(stats.partitions_removed, 0);
        assert!(root.join(format!("dt={cutoff}")).exists());
        assert!(root.join(format!("dt={current}")).exists());
    }

    // ─────────────────────────────────────────────────────────────────
    // The activity spine
    // ─────────────────────────────────────────────────────────────────

    use super::super::activity_rows_sink::{rollup_parquet_files, write_rows, ActivityRow};
    use super::super::duckdb_safety::analytics_duckdb_guard;

    fn activity_row(
        date: NaiveDate,
        hour: i32,
        activity_id: &str,
        outcome: &str,
        duration_ms: i64,
    ) -> ActivityRow {
        ActivityRow {
            activity_id: activity_id.to_string(),
            parent_activity_id: None,
            root_activity_id: activity_id.to_string(),
            name: "unit_of_work".to_string(),
            target: "magician::activity_spine_test".to_string(),
            kind: "background".to_string(),
            workload_class: Some("ambient".to_string()),
            priority: None,
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            agent_id: Some("agent-1".to_string()),
            thread_id: None,
            task_id: None,
            model: Some("model-x".to_string()),
            started_at_ms: date
                .and_hms_opt(hour as u32, 0, 0)
                .expect("valid hour")
                .and_utc()
                .timestamp_millis(),
            duration_ms,
            outcome: outcome.to_string(),
            dt: date.format("%Y-%m-%d").to_string(),
            hour,
        }
    }

    fn parquet_files_under(root: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(directory) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().and_then(|ext| ext.to_str()) == Some("parquet") {
                    found.push(path);
                }
            }
        }
        found.sort();
        found
    }

    /// The whole reason this phase was gated: without a fold, a busy scope
    /// writes a file a minute forever. One closed hour must become one object.
    #[test]
    fn a_closed_hour_folds_into_one_object() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let root = workspace.analytics_activity_rows_root("owner", "default");
        let date = Utc::now().date_naive() - ChronoDuration::days(1);

        // Three separate flushes, as a minute-timer writer would produce.
        for id in ["1", "2", "3"] {
            write_rows(&root, &[activity_row(date, 0, id, "success", 10)]);
        }
        let hour_partition = root.join(format!("dt={date}")).join("hour=00");
        assert_eq!(
            parquet_files_under(&hour_partition).len(),
            3,
            "the writer must produce one object per flush"
        );

        let stats = compact_activity_rows(&workspace, "owner", "default").expect("compaction");
        assert!(stats.partitions_compacted >= 1);
        let folded = parquet_files_under(&hour_partition);
        assert_eq!(folded.len(), 1, "a closed hour must fold to one object");
        assert!(
            folded[0]
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("activity_rows.compacted.")),
            "the survivor must be the verified compacted generation, got {:?}",
            folded[0].file_name()
        );
    }

    /// The current hour is still receiving rows; folding it would guarantee a
    /// second fold.
    #[test]
    fn the_current_hour_is_left_alone() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let root = workspace.analytics_activity_rows_root("owner", "default");
        let now = Utc::now();
        let hour = now.format("%H").to_string().parse::<i32>().expect("hour");

        for id in ["1", "2", "3"] {
            write_rows(
                &root,
                &[activity_row(now.date_naive(), hour, id, "success", 10)],
            );
        }

        compact_activity_rows(&workspace, "owner", "default").expect("compaction");
        let partition = root
            .join(format!("dt={}", now.date_naive()))
            .join(format!("hour={hour:02}"));
        assert_eq!(
            parquet_files_under(&partition).len(),
            3,
            "an hour that has not closed (let alone aged out) must not be folded"
        );
    }

    /// The second fold: a day that closed 48h ago becomes one object and its
    /// hour directories go away entirely.
    #[test]
    fn a_closed_day_folds_its_hours_into_one_object() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let root = workspace.analytics_activity_rows_root("owner", "default");
        let date =
            Utc::now().date_naive() - ChronoDuration::days(ACTIVITY_DAY_COMPACTION_DELAY_DAYS);

        for hour in [0, 1, 2] {
            write_rows(
                &root,
                &[activity_row(date, hour, &format!("{hour}0"), "success", 10)],
            );
        }

        let stats = compact_activity_rows(&workspace, "owner", "default").expect("compaction");
        assert!(stats.partitions_compacted >= 1);

        let day_partition = root.join(format!("dt={date}"));
        let objects = parquet_files_under(&day_partition);
        assert_eq!(objects.len(), 1, "a closed day must fold to one object");
        assert_eq!(
            objects[0].parent(),
            Some(day_partition.as_path()),
            "the day's object belongs at the day level, not inside an hour"
        );
        assert!(
            activity_hour_dirs(&day_partition)
                .expect("hour dirs")
                .is_empty(),
            "the folded hour directories must be removed once the day's object is verified"
        );
    }

    /// The retention boundary, exactly as specified: seven days old rolls up
    /// and its detail is dropped; six days old is untouched.
    #[test]
    fn a_seven_day_old_partition_rolls_up_and_its_detail_is_dropped() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let detail_root = workspace.analytics_activity_rows_root("owner", "default");
        let rollup_root = workspace.analytics_activity_rollups_root("owner", "default");
        let today = Utc::now().date_naive();
        let expired = today - ChronoDuration::days(ACTIVITY_DETAIL_RETENTION_DAYS);
        let kept = today - ChronoDuration::days(ACTIVITY_DETAIL_RETENTION_DAYS - 1);

        write_rows(
            &detail_root,
            &[
                activity_row(expired, 3, "1", "success", 10),
                activity_row(expired, 3, "2", "success", 30),
                activity_row(expired, 3, "3", "error", 50),
            ],
        );
        write_rows(&detail_root, &[activity_row(kept, 3, "4", "success", 70)]);

        let stats = apply_activity_retention(&workspace, "owner", "default").expect("retention");
        assert_eq!(stats.partitions_removed, 1);

        assert!(
            !detail_root.join(format!("dt={expired}")).exists(),
            "expired detail must be gone, not merely ignored"
        );
        assert!(
            detail_root.join(format!("dt={kept}")).exists(),
            "a partition one day inside the window must be untouched"
        );

        let rollups = parquet_files_under(&rollup_root.join(format!("dt={expired}")));
        assert_eq!(rollups.len(), 1, "one rollup object per expired day");

        // The detail is gone, so the rollup is now the only record. It has to
        // still answer the questions the tier exists for.
        let _guard = analytics_duckdb_guard();
        let connection = Connection::open_in_memory().expect("duckdb");
        configure_analytics_connection_checked(&connection, "activity_rollup_test")
            .expect("configure DuckDB");
        let source = escape_sql_literal(&rollups[0].display().to_string());
        let mut rows: Vec<(String, i64, i64, i64, i64)> = connection
            .prepare(&format!(
                "SELECT outcome, CAST(hour AS BIGINT), \"count\", sum_duration_ms, p95_ms
                 FROM read_parquet('{source}', hive_partitioning = false)
                 ORDER BY outcome"
            ))
            .expect("prepare")
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get::<_, i64>(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .expect("query")
            .filter_map(|row| row.ok())
            .collect();
        rows.sort();

        assert_eq!(
            rows.len(),
            2,
            "success and error are different groups and must not be summed together"
        );
        assert_eq!(rows[0].0, "error");
        assert_eq!(rows[0].2, 1, "one error span");
        assert_eq!(rows[0].3, 50);
        assert_eq!(rows[1].0, "success");
        assert_eq!(rows[1].1, 3, "the hour survives the rollup as a column");
        assert_eq!(rows[1].2, 2, "two successful spans");
        assert_eq!(rows[1].3, 40, "10ms + 30ms");
    }

    /// The crash window between publishing a rollup and deleting its detail.
    /// A second pass must finish the delete, never re-aggregate — a second
    /// rollup object would double every count in it.
    #[test]
    fn rolling_up_twice_does_not_double_count() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let detail_root = workspace.analytics_activity_rows_root("owner", "default");
        let rollup_root = workspace.analytics_activity_rollups_root("owner", "default");
        let date = Utc::now().date_naive() - ChronoDuration::days(30);
        write_rows(&detail_root, &[activity_row(date, 0, "1", "success", 10)]);

        let day_partition = detail_root.join(format!("dt={date}"));
        roll_up_activity_day(&day_partition, &rollup_root, date).expect("first rollup");
        // Simulates a crash after publication and before the delete.
        roll_up_activity_day(&day_partition, &rollup_root, date).expect("second rollup");

        assert_eq!(
            parquet_files_under(&rollup_root.join(format!("dt={date}"))).len(),
            1,
            "an interrupted retention pass must resume, not re-aggregate"
        );
    }

    /// The crash window the marker used to sit inside.
    ///
    /// The old marker lived at `<detail>/dt=D/_rollup/rolled-up.json` — in the
    /// directory retention was about to `remove_dir_all`. Crash between the
    /// publishing rename and the marker write and you had a published rollup,
    /// no marker and intact detail; the next pass re-aggregated the same rows
    /// into a *second* object, nothing deduplicates rollup objects on read, and
    /// the detail that could have proved it was then deleted. A 900k-span day
    /// reported 1.8M forever.
    ///
    /// `rolling_up_twice_does_not_double_count` cannot catch this: both of its
    /// passes complete the marker write, so it only ever exercises the side of
    /// the window that was already protected. This one deletes the marker in
    /// between, which is exactly what the crash did.
    #[test]
    fn a_rollup_published_without_its_marker_is_not_aggregated_a_second_time() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let detail_root = workspace.analytics_activity_rows_root("owner", "default");
        let rollup_root = workspace.analytics_activity_rollups_root("owner", "default");
        let date = Utc::now().date_naive() - ChronoDuration::days(30);
        write_rows(
            &detail_root,
            &[
                activity_row(date, 0, "1", "success", 10),
                activity_row(date, 0, "2", "success", 30),
            ],
        );

        let day_partition = detail_root.join(format!("dt={date}"));
        let rollup_partition = rollup_root.join(format!("dt={date}"));
        roll_up_activity_day(&day_partition, &rollup_root, date).expect("first rollup");
        let published = parquet_files_under(&rollup_partition);
        assert_eq!(published.len(), 1, "one rollup object after a clean pass");

        // The crash: the rollup is on disk and durable, the marker never got
        // written, and the detail is untouched because the delete never ran.
        std::fs::remove_dir_all(
            activity_rollup_manifest_path(&rollup_partition)
                .parent()
                .expect("marker dir"),
        )
        .expect("removing the marker simulates the crash window");
        assert!(
            day_partition.is_dir(),
            "the detail must still be present, or this is not the window under test"
        );

        roll_up_activity_day(&day_partition, &rollup_root, date).expect("resumed rollup");

        let after = parquet_files_under(&rollup_partition);
        assert_eq!(
            after.len(),
            1,
            "a rollup published without its marker must be rewritten in place, not added beside: {after:?}"
        );
        assert_eq!(
            after, published,
            "the deterministic name is what makes the rewrite land on the same object"
        );

        // And the numbers the tier exists to preserve are still the day's, not
        // twice the day's.
        let _guard = analytics_duckdb_guard();
        let connection = Connection::open_in_memory().expect("duckdb");
        configure_analytics_connection_checked(&connection, "activity_rollup_crash_window")
            .expect("configure DuckDB");
        let source = escape_sql_literal(&after[0].display().to_string());
        let (count, sum): (i64, i64) = connection
            .query_row(
                &format!(
                    "SELECT CAST(sum(\"count\") AS BIGINT), CAST(sum(sum_duration_ms) AS BIGINT)
                     FROM read_parquet('{source}', hive_partitioning = false)"
                ),
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("reading the resumed rollup");
        assert_eq!(count, 2, "two spans went in; two must come out");
        assert_eq!(sum, 40, "10ms + 30ms, counted once");
    }

    /// A batch that lands after its day was rolled up must be aggregated, not
    /// deleted with the rest of the partition.
    ///
    /// This is what keying idempotency on *which sources* were rolled up buys.
    /// Keying it on "a marker exists" made the late batch invisible: the marker
    /// said the day was done, so the rows were dropped without ever being
    /// counted. A span that starts at 23:58 and runs for two hours is filed
    /// under the day it started in, so this is a real arrival, not a
    /// hypothetical one.
    #[test]
    fn a_batch_that_lands_after_the_rollup_is_still_counted() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let detail_root = workspace.analytics_activity_rows_root("owner", "default");
        let rollup_root = workspace.analytics_activity_rollups_root("owner", "default");
        let date = Utc::now().date_naive() - ChronoDuration::days(30);
        let day_partition = detail_root.join(format!("dt={date}"));
        let rollup_partition = rollup_root.join(format!("dt={date}"));

        write_rows(&detail_root, &[activity_row(date, 0, "1", "success", 10)]);
        roll_up_activity_day(&day_partition, &rollup_root, date).expect("first rollup");
        std::fs::remove_dir_all(&day_partition).expect("retention removes the rolled-up detail");

        // The late arrival, recreating the day partition after it was removed.
        write_rows(&detail_root, &[activity_row(date, 0, "2", "success", 30)]);
        roll_up_activity_day(&day_partition, &rollup_root, date).expect("late rollup");

        let objects = parquet_files_under(&rollup_partition);
        assert_eq!(
            objects.len(),
            2,
            "a new source set is a new generation, published beside the first"
        );

        let _guard = analytics_duckdb_guard();
        let connection = Connection::open_in_memory().expect("duckdb");
        configure_analytics_connection_checked(&connection, "activity_rollup_late_arrival")
            .expect("configure DuckDB");
        let source_sql = parquet_source_sql(&objects);
        let (count, sum): (i64, i64) = connection
            .query_row(
                &format!(
                    "SELECT CAST(sum(\"count\") AS BIGINT), CAST(sum(sum_duration_ms) AS BIGINT)
                     FROM read_parquet({source_sql}, hive_partitioning = false, union_by_name = true)"
                ),
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("reading both generations");
        assert_eq!(count, 2, "both the original span and the late one");
        assert_eq!(sum, 40, "10ms + 30ms — additive across generations");
    }

    /// A crash between staging a rollup and renaming it must not leave a file
    /// nothing will ever remove. The rollup tier is kept for thirteen months.
    #[test]
    fn an_orphaned_rollup_staging_file_is_swept() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let detail_root = workspace.analytics_activity_rows_root("owner", "default");
        let rollup_root = workspace.analytics_activity_rollups_root("owner", "default");
        let date = Utc::now().date_naive() - ChronoDuration::days(30);
        let day_partition = detail_root.join(format!("dt={date}"));
        let rollup_partition = rollup_root.join(format!("dt={date}"));
        std::fs::create_dir_all(&rollup_partition).expect("rollup partition");
        let orphan = rollup_partition.join(".rollup_deadbeef.parquet.tmp");
        std::fs::write(&orphan, b"interrupted publish").expect("orphan");

        write_rows(&detail_root, &[activity_row(date, 0, "1", "success", 10)]);
        roll_up_activity_day(&day_partition, &rollup_root, date).expect("rollup");

        assert!(
            !orphan.exists(),
            "an orphaned staging file must be swept, not kept for thirteen months"
        );
        assert_eq!(
            parquet_files_under(&rollup_partition).len(),
            1,
            "and the sweep must not have taken the real object with it"
        );
    }

    /// Sum `count` and `sum_duration_ms` across whatever rollup objects a
    /// reader would actually see. Both are additive across generations, which
    /// is exactly why a stray generation doubles a day rather than corrupting
    /// it visibly.
    fn rollup_totals(objects: &[PathBuf]) -> (i64, i64) {
        assert!(!objects.is_empty(), "nothing to total");
        let _guard = analytics_duckdb_guard();
        let connection = Connection::open_in_memory().expect("duckdb");
        configure_analytics_connection_checked(&connection, "activity_rollup_totals")
            .expect("configure DuckDB");
        let source_sql = parquet_source_sql(objects);
        connection
            .query_row(
                &format!(
                    "SELECT CAST(sum(\"count\") AS BIGINT), CAST(sum(sum_duration_ms) AS BIGINT)
                     FROM read_parquet({source_sql}, hive_partitioning = false, union_by_name = true)"
                ),
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("totalling the rollup tier")
    }

    /// C2: the crash window survives a *change* to the source set.
    ///
    /// The deterministic object name only makes a re-run idempotent while the
    /// sources are unchanged. Publish `rollup_hash(A)`, lose the marker, take a
    /// late batch `B`, and the next pass hashes `[A, B]` to a different name and
    /// publishes beside the first object rather than over it. Nothing globbing
    /// `*.parquet` could tell them apart, `count` is additive, and retention
    /// then deleted the detail — so `A` was counted twice, permanently.
    ///
    /// `a_rollup_published_without_its_marker_is_not_aggregated_a_second_time`
    /// cannot catch this: its second pass sees the *same* source set, which is
    /// the only case property 2 covers.
    #[test]
    fn a_late_source_after_an_unmarked_publish_is_aggregated_exactly_once() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let detail_root = workspace.analytics_activity_rows_root("owner", "default");
        let rollup_root = workspace.analytics_activity_rollups_root("owner", "default");
        let date = Utc::now().date_naive() - ChronoDuration::days(30);
        let day_partition = detail_root.join(format!("dt={date}"));
        let rollup_partition = rollup_root.join(format!("dt={date}"));

        write_rows(&detail_root, &[activity_row(date, 0, "1", "success", 10)]);
        roll_up_activity_day(&day_partition, &rollup_root, date).expect("first rollup");
        let first = parquet_files_under(&rollup_partition);
        assert_eq!(first.len(), 1, "one object after a clean pass");

        // The crash: object durable, marker never written, detail untouched
        // because the delete never ran.
        std::fs::remove_dir_all(
            activity_rollup_manifest_path(&rollup_partition)
                .parent()
                .expect("marker dir"),
        )
        .expect("removing the marker simulates the crash window");

        // ...and now the source set changes under it. A span that started at
        // 23:58 and closed two hours later is filed under the day it began in.
        write_rows(&detail_root, &[activity_row(date, 0, "2", "success", 30)]);

        roll_up_activity_day(&day_partition, &rollup_root, date).expect("resumed rollup");

        let readable = rollup_parquet_files(&rollup_root).files;
        assert_eq!(
            readable.len(),
            1,
            "exactly one readable aggregation must remain, got {readable:?}"
        );
        assert_eq!(
            parquet_files_under(&rollup_partition).len(),
            1,
            "and the superseded object must no longer be named `.parquet` at all"
        );
        assert_ne!(
            readable[0], first[0],
            "the survivor is the one covering both sources, not the stale one"
        );

        let (count, sum) = rollup_totals(&readable);
        assert_eq!(count, 2, "two spans went in; two must come out, not three");
        assert_eq!(sum, 40, "10ms + 30ms, each counted once");
    }

    #[test]
    fn rollup_generation_files_flags_incomplete_marker() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let detail_root = workspace.analytics_activity_rows_root("owner", "default");
        let rollup_root = workspace.analytics_activity_rollups_root("owner", "default");
        let date = Utc::now().date_naive() - ChronoDuration::days(30);
        let day_partition = detail_root.join(format!("dt={date}"));
        let rollup_partition = rollup_root.join(format!("dt={date}"));

        write_rows(&detail_root, &[activity_row(date, 0, "1", "success", 10)]);
        roll_up_activity_day(&day_partition, &rollup_root, date).expect("rollup");

        let mut manifest = read_activity_rollup_manifest(&rollup_partition)
            .expect("marker read")
            .expect("marker exists");
        manifest.incomplete = true;
        write_activity_rollup_manifest(&rollup_partition, &manifest).expect("marker updated");

        let (files, complete) =
            rollup_generation_files(&rollup_partition).expect("marker now marks incompleteness");
        assert!(
            !files.is_empty(),
            "generation markers should still list readable files"
        );
        assert!(
            !complete,
            "explicit incomplete manifests should block completeness"
        );

        let set = rollup_parquet_files(&rollup_root);
        assert!(
            !set.complete,
            "incomplete marker must flow through set completeness"
        );
        assert_eq!(
            set.files.len(),
            1,
            "one committed generation remains visible"
        );
    }

    #[test]
    fn rollup_generation_files_flags_missing_objects() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let rollup_root = workspace.analytics_activity_rollups_root("owner", "default");
        let date = Utc::now().date_naive() - ChronoDuration::days(7);
        let rollup_partition = rollup_root.join(format!("dt={date}"));

        std::fs::create_dir_all(&rollup_partition).expect("rollup partition");
        let manifest = ActivityRollupManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            partition_date: date.to_string(),
            rolled_up_at_ms: 1,
            generations: vec![ActivityRollupGeneration {
                rollup_file_name: "rollup_missing.parquet".to_string(),
                sources: Vec::new(),
                detail_row_count: 0,
                rollup_row_count: 0,
            }],
            incomplete: false,
        };
        write_activity_rollup_manifest(&rollup_partition, &manifest).expect("manifest written");

        let (files, complete) =
            rollup_generation_files(&rollup_partition).expect("marker is still parseable");
        assert!(
            files.is_empty(),
            "missing generation should not be returned as source"
        );
        assert!(!complete, "missing files must produce incomplete scans");

        let set = rollup_parquet_files(&rollup_root);
        assert!(
            !set.complete,
            "missing generation should fail completeness at set level"
        );
        assert!(set.files.is_empty(), "no valid generation should be read");
    }

    /// C2, reached without a crash at all.
    ///
    /// A corrupt or future-versioned marker used to be mapped to `Ok(None)` —
    /// "nothing here is accounted for" — which is only safe if being unaccounted
    /// has a consequence. It did not: the pass simply re-aggregated every source
    /// and published beside the object the readable marker had named.
    ///
    /// The consequence exists now, and it is a refusal rather than a rebuild:
    /// see `a_committed_generation_survives_an_unreadable_marker` for why
    /// rebuilding over an unreadable marker is the *more* destructive answer.
    /// What must hold either way is the property this test is named for — a bad
    /// marker never yields two readable aggregations of the same day.
    #[test]
    fn a_corrupt_rollup_marker_cannot_produce_a_second_aggregation() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let detail_root = workspace.analytics_activity_rows_root("owner", "default");
        let rollup_root = workspace.analytics_activity_rollups_root("owner", "default");
        let date = Utc::now().date_naive() - ChronoDuration::days(30);
        let day_partition = detail_root.join(format!("dt={date}"));
        let rollup_partition = rollup_root.join(format!("dt={date}"));

        write_rows(&detail_root, &[activity_row(date, 0, "1", "success", 10)]);
        roll_up_activity_day(&day_partition, &rollup_root, date).expect("first rollup");

        std::fs::write(
            activity_rollup_manifest_path(&rollup_partition),
            b"{ this is not a rollup marker",
        )
        .expect("corrupting the marker");
        write_rows(&detail_root, &[activity_row(date, 0, "2", "success", 30)]);

        let refusal = roll_up_activity_day(&day_partition, &rollup_root, date)
            .expect_err("an uninterpretable marker must withhold permission to delete");
        assert!(
            refusal.to_string().contains("cannot be parsed"),
            "the refusal must name the marker as the reason: {refusal}"
        );

        let readable = rollup_parquet_files(&rollup_root).files;
        assert_eq!(
            readable.len(),
            1,
            "an unreadable marker must not license a second aggregation, got {readable:?}"
        );
        assert!(
            day_partition.is_dir(),
            "and the detail is kept, so the late row is aggregated once the marker is repaired"
        );
        let (count, sum) = rollup_totals(&readable);
        assert_eq!(count, 1, "only the generation the clean pass committed");
        assert_eq!(sum, 10);
    }

    /// The inverse of C2, and the more expensive half: quarantine destroying a
    /// **committed** generation.
    ///
    /// The sweep renames every `.parquet` the manifest does not name, justified
    /// by "at sweep time every row a stray could hold is still on disk in the
    /// detail tier". That is false for a committed generation, whose detail was
    /// deleted at the end of the pass that published it. Map an unreadable
    /// marker to `Ok(None)` and `roll_up_activity_day` builds a default manifest
    /// naming no generations, so `named = []` and the sweep takes the lot.
    ///
    /// The sequence needs no corruption of its own to be reachable: bump
    /// `MANIFEST_SCHEMA_VERSION`, roll the build back, and every day partition
    /// that ever took a late batch hits this at once.
    #[test]
    fn a_committed_generation_survives_an_unreadable_marker() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let detail_root = workspace.analytics_activity_rows_root("owner", "default");
        let rollup_root = workspace.analytics_activity_rollups_root("owner", "default");
        let date = Utc::now().date_naive() - ChronoDuration::days(30);
        let day_partition = detail_root.join(format!("dt={date}"));
        let rollup_partition = rollup_root.join(format!("dt={date}"));

        // Day D rolls up cleanly and retention deletes the detail. From here on
        // the committed generation is the ONLY copy of those rows.
        write_rows(&detail_root, &[activity_row(date, 0, "1", "success", 10)]);
        roll_up_activity_day(&day_partition, &rollup_root, date).expect("first rollup");
        std::fs::remove_dir_all(&day_partition).expect("retention removes the rolled-up detail");
        let committed = rollup_parquet_files(&rollup_root).files;
        assert_eq!(committed.len(), 1, "one committed generation");

        // A late batch recreates the partition — the case this function exists
        // to handle — and the marker cannot be read at that moment. Written by a
        // build from the future rather than corrupted, because that is the
        // trigger an operator can actually cause.
        write_rows(&detail_root, &[activity_row(date, 0, "2", "success", 30)]);
        let mut future = std::fs::read(activity_rollup_manifest_path(&rollup_partition))
            .map(|raw| serde_json::from_slice::<serde_json::Value>(&raw).expect("marker json"))
            .expect("marker");
        future["schema_version"] = serde_json::Value::from(u64::from(MANIFEST_SCHEMA_VERSION) + 1);
        std::fs::write(
            activity_rollup_manifest_path(&rollup_partition),
            serde_json::to_vec(&future).expect("future marker"),
        )
        .expect("writing a future-versioned marker");

        let refusal = roll_up_activity_day(&day_partition, &rollup_root, date)
            .expect_err("a marker this build cannot interpret must make the sweep refuse");
        assert!(
            refusal.to_string().contains("schema version"),
            "the refusal must name the version it could not read: {refusal}"
        );

        assert_eq!(
            parquet_files_under(&rollup_partition),
            committed,
            "the committed generation must still be named `.parquet` — its sources are gone, so \
             quarantining it destroys the day rather than superseding a redundant copy"
        );
        let (count, sum) = rollup_totals(&rollup_parquet_files(&rollup_root).files);
        assert_eq!(count, 1, "the day's original summary is still readable");
        assert_eq!(sum, 10);
        assert!(
            day_partition.is_dir(),
            "and the late batch is kept for the sweep that follows the marker being repaired"
        );
    }

    /// A day with no marker at all is a different state from one with a marker
    /// that cannot be read, and only the first may sweep with an empty
    /// named-set. Collapsing the two is what the refusal above prevents; this
    /// pins the other side, so the fix cannot be "refuse on everything" and
    /// wedge every partition whose marker was legitimately lost.
    #[test]
    fn an_absent_rollup_marker_still_rebuilds_the_day() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let detail_root = workspace.analytics_activity_rows_root("owner", "default");
        let rollup_root = workspace.analytics_activity_rollups_root("owner", "default");
        let date = Utc::now().date_naive() - ChronoDuration::days(30);
        let day_partition = detail_root.join(format!("dt={date}"));
        let rollup_partition = rollup_root.join(format!("dt={date}"));

        write_rows(&detail_root, &[activity_row(date, 0, "1", "success", 10)]);
        roll_up_activity_day(&day_partition, &rollup_root, date).expect("first rollup");
        std::fs::remove_dir_all(
            activity_rollup_manifest_path(&rollup_partition)
                .parent()
                .expect("marker dir"),
        )
        .expect("dropping the marker entirely");

        roll_up_activity_day(&day_partition, &rollup_root, date)
            .expect("an absent marker is not a refusal — there is nothing committed to protect");

        let readable = rollup_parquet_files(&rollup_root).files;
        assert_eq!(readable.len(), 1);
        let (count, sum) = rollup_totals(&readable);
        assert_eq!(count, 1);
        assert_eq!(sum, 10);
    }

    /// The read side must follow the marker, not the directory listing.
    ///
    /// This is the other half of C2: even if a stray object does appear, the
    /// reader has to have an authority to ignore it by. Globbing had none.
    #[test]
    fn the_rollup_reader_follows_the_marker_and_falls_back_only_without_one() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let detail_root = workspace.analytics_activity_rows_root("owner", "default");
        let rollup_root = workspace.analytics_activity_rollups_root("owner", "default");
        let date = Utc::now().date_naive() - ChronoDuration::days(30);
        let day_partition = detail_root.join(format!("dt={date}"));
        let rollup_partition = rollup_root.join(format!("dt={date}"));

        write_rows(&detail_root, &[activity_row(date, 0, "1", "success", 10)]);
        roll_up_activity_day(&day_partition, &rollup_root, date).expect("rollup");
        let committed = rollup_parquet_files(&rollup_root).files;
        assert_eq!(committed.len(), 1);

        // Something the marker does not name, put there after the fact.
        let stray = rollup_partition.join("rollup_restored_by_hand.parquet");
        std::fs::write(&stray, b"summary").expect("stray");
        assert_eq!(
            rollup_parquet_files(&rollup_root).files,
            committed,
            "an object the marker does not name is not part of the dataset"
        );

        // Without a marker there is no authority, so the listing is all there
        // is — a lost marker must read long, never silently empty.
        std::fs::remove_dir_all(
            activity_rollup_manifest_path(&rollup_partition)
                .parent()
                .expect("marker dir"),
        )
        .expect("dropping the marker");
        assert_eq!(
            rollup_parquet_files(&rollup_root).files.len(),
            2,
            "with no marker the reader falls back to the listing rather than answering zero"
        );
    }

    /// C3: the retention path gets the guard the fold already had.
    ///
    /// `roll_up_activity_day` returning `Ok(())` authorises the caller's
    /// `remove_dir_all` of the whole `dt=` directory. An object
    /// `partition_sources` does not recognise — restored by hand, or from a
    /// naming scheme that has since changed — was invisible to the enumeration,
    /// so it was aggregated into nothing and then destroyed, and the day was
    /// reported as a successful `partitions_removed`.
    #[test]
    fn a_day_holding_unrecognised_objects_is_never_rolled_up_and_deleted() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let detail_root = workspace.analytics_activity_rows_root("owner", "default");
        let rollup_root = workspace.analytics_activity_rollups_root("owner", "default");
        let date = Utc::now().date_naive() - ChronoDuration::days(ACTIVITY_DETAIL_RETENTION_DAYS);
        let day_partition = detail_root.join(format!("dt={date}"));

        write_rows(&detail_root, &[activity_row(date, 0, "1", "success", 10)]);
        let stray = day_partition
            .join("hour=01")
            .join("restored-by-hand.parquet");
        std::fs::create_dir_all(stray.parent().expect("hour dir")).expect("hour dir");
        std::fs::write(&stray, b"rows").expect("stray");

        let stats = apply_activity_retention(&workspace, "owner", "default")
            .expect("one refused day must not fail the sweep");

        assert_eq!(
            stats.partitions_removed, 0,
            "a day that could not be fully accounted for must not count as removed"
        );
        assert!(
            day_partition.is_dir(),
            "the detail must be kept, not deleted on the strength of an incomplete enumeration"
        );
        assert!(stray.is_file(), "and the unrecognised object must survive");
        assert!(
            !rollup_root.join(format!("dt={date}")).exists(),
            "nothing may be published for a day this pass refused to summarise"
        );
    }

    /// I3: a pre-fix marker must not authorise deleting live detail.
    ///
    /// The legacy marker records one object name and no source fingerprints, so
    /// it cannot say whether the sources still present are survivors of an
    /// interrupted delete (already summarised) or arrivals since (not). The
    /// short-circuit to `Ok(())` before any enumeration picked the second
    /// silently and let the caller delete them — the exact "the marker's
    /// existence means done" behaviour the fix claimed to have removed.
    #[test]
    fn a_legacy_rollup_marker_never_authorises_deleting_live_detail() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let detail_root = workspace.analytics_activity_rows_root("owner", "default");
        let rollup_root = workspace.analytics_activity_rollups_root("owner", "default");
        let date = Utc::now().date_naive() - ChronoDuration::days(ACTIVITY_DETAIL_RETENTION_DAYS);
        let day_partition = detail_root.join(format!("dt={date}"));
        let rollup_partition = rollup_root.join(format!("dt={date}"));

        // What the pre-fix build left behind: an object in the rollup tier and
        // a marker inside the detail partition naming it.
        std::fs::create_dir_all(&rollup_partition).expect("rollup partition");
        std::fs::write(rollup_partition.join("rollup_legacy.parquet"), b"summary")
            .expect("legacy object");
        write_rows(&detail_root, &[activity_row(date, 0, "1", "success", 10)]);
        let legacy_marker = day_partition.join("_rollup").join("rolled-up.json");
        std::fs::create_dir_all(legacy_marker.parent().expect("marker dir")).expect("marker dir");
        std::fs::write(
            &legacy_marker,
            br#"{"rollup_file_name":"rollup_legacy.parquet"}"#,
        )
        .expect("legacy marker");

        let stats = apply_activity_retention(&workspace, "owner", "default").expect("retention");

        assert_eq!(stats.partitions_removed, 0);
        assert!(
            day_partition.is_dir(),
            "detail whose coverage the legacy marker cannot vouch for must be kept"
        );
        assert_eq!(
            parquet_files_under(&day_partition).len(),
            1,
            "the batch itself must still be there"
        );
        assert_eq!(
            parquet_files_under(&rollup_partition),
            vec![rollup_partition.join("rollup_legacy.parquet")],
            "and nothing new may be published beside the legacy object"
        );
    }

    /// The other side of I3: a legacy marker over a day with nothing left to
    /// lose still lets retention finish. The pre-fix delete got everything but
    /// the marker directory, so there is no row to drop and none to double.
    #[test]
    fn a_legacy_marker_over_an_emptied_day_still_permits_the_delete() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let detail_root = workspace.analytics_activity_rows_root("owner", "default");
        let rollup_root = workspace.analytics_activity_rollups_root("owner", "default");
        let date = Utc::now().date_naive() - ChronoDuration::days(ACTIVITY_DETAIL_RETENTION_DAYS);
        let day_partition = detail_root.join(format!("dt={date}"));
        let rollup_partition = rollup_root.join(format!("dt={date}"));

        std::fs::create_dir_all(&rollup_partition).expect("rollup partition");
        std::fs::write(rollup_partition.join("rollup_legacy.parquet"), b"summary")
            .expect("legacy object");
        let legacy_marker = day_partition.join("_rollup").join("rolled-up.json");
        std::fs::create_dir_all(legacy_marker.parent().expect("marker dir")).expect("marker dir");
        std::fs::write(
            &legacy_marker,
            br#"{"rollup_file_name":"rollup_legacy.parquet"}"#,
        )
        .expect("legacy marker");

        let stats = apply_activity_retention(&workspace, "owner", "default").expect("retention");

        assert_eq!(stats.partitions_removed, 1);
        assert!(
            !day_partition.exists(),
            "a day holding only the legacy marker has nothing worth keeping"
        );
        assert!(
            rollup_partition.join("rollup_legacy.parquet").is_file(),
            "the legacy summary is the only record of that day and must survive"
        );
    }

    /// `partition_sources` only recognises `batch_*` files and the manifest's
    /// own generation. An hour directory holding anything else reads as empty,
    /// and the day fold used to delete it on that basis.
    #[test]
    fn an_hour_directory_with_unrecognised_objects_is_never_dropped() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let root = workspace.analytics_activity_rows_root("owner", "default");
        let date =
            Utc::now().date_naive() - ChronoDuration::days(ACTIVITY_DAY_COMPACTION_DELAY_DAYS);
        let day_partition = root.join(format!("dt={date}"));
        let readable = day_partition.join("hour=00");
        let unreadable = day_partition.join("hour=01");
        std::fs::create_dir_all(&readable).expect("readable hour");
        std::fs::create_dir_all(&unreadable).expect("unreadable hour");
        // Neither a `batch_` name nor a manifest-selected generation: invisible
        // to `partition_sources`, and it may well hold rows.
        std::fs::write(unreadable.join("restored-by-hand.parquet"), b"rows").expect("stray");

        let mut stats = ParquetCompactionStats::default();
        fold_activity_day(&day_partition, &mut stats).expect("day fold must not propagate");

        assert!(
            !readable.exists(),
            "a genuinely empty hour shell is still swept"
        );
        assert!(
            unreadable.join("restored-by-hand.parquet").is_file(),
            "an hour holding objects maintenance cannot account for must be kept, not deleted"
        );
    }

    /// One poisoned hour directory must not stop the scope's sweep — this
    /// function runs immediately before both retention passes, so propagating
    /// used to mean nothing in the scope ever expired again.
    #[test]
    fn a_poisoned_hour_directory_does_not_abort_the_sweep() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let root = workspace.analytics_activity_rows_root("owner", "default");
        let poisoned_date =
            Utc::now().date_naive() - ChronoDuration::days(ACTIVITY_DAY_COMPACTION_DELAY_DAYS + 1);
        let healthy_date =
            Utc::now().date_naive() - ChronoDuration::days(ACTIVITY_DAY_COMPACTION_DELAY_DAYS);

        // A half-removed hour: a compaction manifest naming an object that is
        // no longer there. `read_valid_manifest` raises on it, which is the
        // error that used to escape all the way out of the scope sweep.
        let poisoned_hour = root.join(format!("dt={poisoned_date}")).join("hour=00");
        write_rows(&root, &[activity_row(poisoned_date, 0, "1", "success", 10)]);
        write_rows(&root, &[activity_row(poisoned_date, 0, "2", "success", 10)]);
        let mut hour_stats = ParquetCompactionStats::default();
        fold_activity_hour(&poisoned_hour, &mut hour_stats);
        for object in parquet_files_under(&poisoned_hour) {
            std::fs::remove_file(object).expect("removing the compacted object poisons the hour");
        }

        write_rows(&root, &[activity_row(healthy_date, 0, "3", "success", 10)]);
        write_rows(&root, &[activity_row(healthy_date, 1, "4", "success", 10)]);

        let stats = compact_activity_rows(&workspace, "owner", "default")
            .expect("a poisoned partition must not fail the sweep");

        assert!(
            stats.partitions_failed >= 1,
            "the poisoned day must be reported as failed, not silently skipped"
        );
        let healthy_partition = root.join(format!("dt={healthy_date}"));
        let folded = parquet_files_under(&healthy_partition);
        assert_eq!(
            folded.len(),
            1,
            "the healthy day after the poisoned one must still have been folded, got {folded:?}"
        );
        assert_eq!(
            folded[0].parent(),
            Some(healthy_partition.as_path()),
            "and folded to the day level"
        );
    }

    /// The rollup tier expires too, or "storage stays flat" is a claim rather
    /// than a property.
    #[test]
    fn rollups_older_than_thirteen_months_are_removed() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let rollup_root = workspace.analytics_activity_rollups_root("owner", "default");
        let today = Utc::now().date_naive();
        let expired = today - ChronoDuration::days(ACTIVITY_ROLLUP_RETENTION_DAYS);
        let kept = today - ChronoDuration::days(ACTIVITY_ROLLUP_RETENTION_DAYS - 1);
        for date in [expired, kept] {
            let partition = rollup_root.join(format!("dt={date}"));
            std::fs::create_dir_all(&partition).expect("partition");
            std::fs::write(partition.join("rollup_x.parquet"), b"summary").expect("sentinel");
        }

        apply_activity_retention(&workspace, "owner", "default").expect("retention");

        assert!(!rollup_root.join(format!("dt={expired}")).exists());
        assert!(rollup_root.join(format!("dt={kept}")).exists());
    }

    /// The two tiers must never share a clock. If they did, either the detail
    /// would outlive its purpose or the rollups would vanish with it.
    #[test]
    fn the_two_activity_tiers_expire_on_different_clocks() {
        assert!(
            ACTIVITY_ROLLUP_RETENTION_DAYS > ACTIVITY_DETAIL_RETENTION_DAYS,
            "a rollup that expired before its detail would be pointless"
        );
        assert_eq!(ACTIVITY_DETAIL_RETENTION_DAYS, 7);
        assert_eq!(ACTIVITY_ROLLUP_RETENTION_DAYS, 396);
    }

    /// The spine must not be swept by the flat 90-day rule. That rule deletes
    /// outright, and the whole point of the tiering is that the shape of the
    /// work survives the loss of the individual spans.
    #[test]
    fn the_flat_retention_sweep_never_touches_the_spine() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let detail_root = workspace.analytics_activity_rows_root("owner", "default");
        let rollup_root = workspace.analytics_activity_rollups_root("owner", "default");
        let ancient = Utc::now().date_naive() - ChronoDuration::days(365);
        for root in [&detail_root, &rollup_root] {
            let partition = root.join(format!("dt={ancient}"));
            std::fs::create_dir_all(&partition).expect("partition");
            std::fs::write(partition.join("batch_x.parquet"), b"rows").expect("sentinel");
        }

        apply_retention(&workspace, "owner", "default", 90).expect("flat retention");

        assert!(
            detail_root.join(format!("dt={ancient}")).exists(),
            "the flat sweep must leave the spine to its own tiered policy"
        );
        assert!(
            rollup_root.join(format!("dt={ancient}")).exists(),
            "13-month rollups must survive a 90-day sweep"
        );
    }

    // ─────────────────────────────────────────────────────────────────
    // One bad input must not wedge a partition
    // ─────────────────────────────────────────────────────────────────

    /// A zero-byte `batch_`, exactly what a writer that died mid-`COPY` left
    /// in `activity_rows/dt=2026-10-02/hour=02`.
    fn write_empty_batch(directory: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(directory).expect("mkdir");
        let path = directory.join(name);
        std::fs::write(&path, b"").expect("zero-byte batch");
        path
    }

    /// The first half of a real Parquet object: right header magic, no footer.
    fn write_truncated_batch(directory: &Path, name: &str) -> (PathBuf, Vec<u8>) {
        let scratch = tempfile::tempdir().expect("scratch");
        let whole = scratch.path().join("whole.parquet");
        write_batch(&whole, 0, 50);
        let bytes = std::fs::read(&whole).expect("read whole");
        let truncated = bytes[..bytes.len() / 2].to_vec();
        std::fs::create_dir_all(directory).expect("mkdir");
        let path = directory.join(name);
        std::fs::write(&path, &truncated).expect("truncated batch");
        (path, truncated)
    }

    /// A Parquet object whose envelope is intact (`PAR1` at both ends) but
    /// whose declared footer is two bytes of the real one, so DuckDB fails to
    /// decode the metadata. Used where the envelope check alone must not
    /// decide.
    fn write_bad_footer_batch(path: &Path) {
        write_batch(path, 0, 5);
        let mut bytes = std::fs::read(path).expect("read batch");
        let len = bytes.len();
        bytes[len - 8..len - 4].copy_from_slice(&2_u32.to_le_bytes());
        std::fs::write(path, &bytes).expect("corrupt footer length");
    }

    fn quarantined_under(root: &Path) -> (Vec<PathBuf>, Vec<QuarantinedInputRecord>) {
        let mut objects = Vec::new();
        let mut records = Vec::new();
        let mut stack = vec![root.join(QUARANTINE_DIR_NAME)];
        while let Some(directory) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                    records.push(
                        serde_json::from_slice(&std::fs::read(&path).expect("record"))
                            .expect("quarantine record parses"),
                    );
                } else {
                    objects.push(path);
                }
            }
        }
        objects.sort();
        (objects, records)
    }

    fn rows_in(path: &Path) -> i64 {
        let connection = Connection::open_in_memory().expect("duckdb");
        count_parquet_rows(&connection, path).expect("rows")
    }

    /// The wedge itself: one unreadable batch among readable ones. The hour
    /// must fold everything readable and move the bad objects aside — never
    /// delete them — with a record of what they were.
    #[test]
    fn an_unreadable_batch_is_set_aside_and_the_rest_of_the_hour_folds() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let root = workspace.analytics_activity_rows_root("owner", "default");
        let date = Utc::now().date_naive() - ChronoDuration::days(1);
        for id in ["1", "2", "3"] {
            write_rows(&root, &[activity_row(date, 0, id, "success", 10)]);
        }
        let hour_partition = root.join(format!("dt={date}")).join("hour=00");
        write_empty_batch(&hour_partition, "batch_empty.parquet");
        let (_, truncated_bytes) =
            write_truncated_batch(&hour_partition, "batch_truncated.parquet");

        let stats = compact_activity_rows(&workspace, "owner", "default").expect("compaction");

        assert_eq!(stats.inputs_quarantined, 2);
        assert_eq!(stats.partitions_failed, 0, "the hour must fold, not fail");
        let folded = parquet_files_under(&hour_partition);
        assert_eq!(
            folded.len(),
            1,
            "every readable batch folds into one object"
        );
        assert_eq!(rows_in(&folded[0]), 3, "no readable row may be lost");
        assert!(
            !fold_backoff_path(&hour_partition, PartitionedDataset::ActivityRows).exists(),
            "a fold that succeeded leaves no backoff behind"
        );

        let (objects, records) = quarantined_under(&root);
        assert_eq!(objects.len(), 2, "both bad objects are kept, not deleted");
        assert!(objects.iter().all(|path| {
            path.parent()
                == Some(
                    root.join(QUARANTINE_DIR_NAME)
                        .join(format!("dt={date}"))
                        .join("hour=00")
                        .as_path(),
                )
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains(".parquet.unreadable-"))
        }));
        let truncated = objects
            .iter()
            .find(|path| path.display().to_string().contains("batch_truncated"))
            .expect("truncated object kept");
        assert_eq!(
            std::fs::read(truncated).expect("read kept bytes"),
            truncated_bytes,
            "the bytes set aside are exactly the bytes that failed"
        );
        assert_eq!(records.len(), 2, "each object carries a durable record");
        let empty = records
            .iter()
            .find(|record| record.original_path.ends_with("batch_empty.parquet"))
            .expect("record for the zero-byte batch");
        assert_eq!(
            empty.original_path,
            format!("dt={date}/hour=00/batch_empty.parquet")
        );
        assert_eq!(empty.byte_len, 0);
        assert_eq!(empty.dataset, PartitionedDataset::ActivityRows);
        assert!(!empty.reason.is_empty());

        // The readers see the folded rows and nothing from the quarantine area.
        let query = super::super::activity_rows_sink::query_parquet_files(&root);
        assert_eq!(query.files, folded);
    }

    /// The exact on-disk state that was stuck: a day several days old whose
    /// other hours folded, with one hour holding a zero-byte batch. One sweep
    /// must fold the hour and the day; retention then rolls it up and removes
    /// the detail — and the bad object survives in the quarantine area.
    #[test]
    fn a_stuck_day_with_a_zero_byte_batch_folds_rolls_up_and_keeps_the_bad_object() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let root = workspace.analytics_activity_rows_root("owner", "default");
        let rollup_root = workspace.analytics_activity_rollups_root("owner", "default");
        let date =
            Utc::now().date_naive() - ChronoDuration::days(ACTIVITY_DETAIL_RETENTION_DAYS + 3);
        for (hour, id) in [(0, "1"), (0, "2"), (2, "3"), (2, "4")] {
            write_rows(&root, &[activity_row(date, hour, id, "success", 10)]);
        }
        let day_partition = root.join(format!("dt={date}"));
        write_empty_batch(&day_partition.join("hour=02"), "batch_empty.parquet");

        let stats = compact_activity_rows(&workspace, "owner", "default").expect("compaction");
        assert_eq!(stats.inputs_quarantined, 1);
        assert_eq!(stats.partitions_failed, 0);
        let objects = parquet_files_under(&day_partition);
        assert_eq!(objects.len(), 1, "the day folds to one object");
        assert_eq!(objects[0].parent(), Some(day_partition.as_path()));
        assert_eq!(rows_in(&objects[0]), 4);

        let retention =
            apply_activity_retention(&workspace, "owner", "default").expect("retention");
        assert_eq!(
            retention.partitions_removed, 1,
            "the day is no longer refused"
        );
        assert!(!day_partition.exists());
        let rollups = rollup_parquet_files(&rollup_root);
        assert_eq!(rollup_totals(&rollups.files), (4, 40));
        let (kept, records) = quarantined_under(&root);
        assert_eq!(
            kept.len(),
            1,
            "removing the detail must not remove what was set aside"
        );
        assert_eq!(records.len(), 1);
    }

    /// The roll-up reads detail directly, so it needs the same treatment: the
    /// pass that meets an unreadable batch sets it aside and keeps the detail;
    /// the next pass rolls up the rest instead of refusing forever.
    #[test]
    fn a_rollup_sets_aside_an_unreadable_batch_instead_of_refusing_forever() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let root = workspace.analytics_activity_rows_root("owner", "default");
        let rollup_root = workspace.analytics_activity_rollups_root("owner", "default");
        let date =
            Utc::now().date_naive() - ChronoDuration::days(ACTIVITY_DETAIL_RETENTION_DAYS + 1);
        write_rows(&root, &[activity_row(date, 5, "1", "success", 25)]);
        let day_partition = root.join(format!("dt={date}"));
        write_empty_batch(&day_partition.join("hour=05"), "batch_empty.parquet");

        let first = apply_activity_retention(&workspace, "owner", "default").expect("retention");
        assert_eq!(
            first.partitions_removed, 0,
            "the failing pass keeps the detail"
        );
        assert!(day_partition.exists());
        assert_eq!(quarantined_under(&root).0.len(), 1);

        let second = apply_activity_retention(&workspace, "owner", "default").expect("retention");
        assert_eq!(second.partitions_removed, 1);
        assert_eq!(
            rollup_totals(&rollup_parquet_files(&rollup_root).files),
            (1, 25)
        );
        assert_eq!(
            quarantined_under(&root).0.len(),
            1,
            "the bad object outlives the detail"
        );
    }

    /// The generic one-level datasets share the fold, so they share the fix.
    #[test]
    fn a_flat_dataset_partition_sets_aside_its_unreadable_batch() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let root = workspace.analytics_memory_events_root("owner", "default");
        let date = Utc::now().date_naive() - ChronoDuration::days(1);
        let partition = root.join(format!("dt={date}"));
        {
            let _guard = analytics_duckdb_guard();
            write_batch(&partition.join("batch_1.parquet"), 0, 2);
            write_batch(&partition.join("batch_2.parquet"), 2, 3);
        }
        write_empty_batch(&partition, "batch_3.parquet");

        let stats = compact_dataset(
            &workspace,
            "owner",
            "default",
            PartitionedDataset::MemoryEvents,
            2,
        )
        .expect("compact");
        assert_eq!(stats.partitions_compacted, 1);
        assert_eq!(stats.inputs_quarantined, 1);
        assert_eq!(stats.raw_files_compacted, 2);
        let compacted = compacted_file(&partition, PartitionedDataset::MemoryEvents);
        assert_eq!(rows_in(&compacted), 5);
        assert_eq!(
            partition_sources(&partition, PartitionedDataset::MemoryEvents).expect("sources"),
            vec![compacted]
        );
        let (kept, records) = quarantined_under(&root);
        assert_eq!(kept.len(), 1);
        assert_eq!(
            records[0].original_path,
            format!("dt={date}/batch_3.parquet")
        );
        assert_eq!(records[0].dataset, PartitionedDataset::MemoryEvents);
    }

    /// When nothing in the set reads and every envelope looks intact, the
    /// fault is more likely the environment than the files: nothing is moved,
    /// the failure backs off, and a change to the inputs retries at once.
    #[test]
    fn a_fold_that_setting_inputs_aside_cannot_fix_backs_off_until_its_inputs_change() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let root = workspace.analytics_activity_rows_root("owner", "default");
        let date = Utc::now().date_naive() - ChronoDuration::days(1);
        let hour_partition = root.join(format!("dt={date}")).join("hour=00");
        {
            let _guard = analytics_duckdb_guard();
            write_bad_footer_batch(&hour_partition.join("batch_bad_1.parquet"));
            write_bad_footer_batch(&hour_partition.join("batch_bad_2.parquet"));
        }
        assert!(!parquet_envelope_is_broken(
            &hour_partition.join("batch_bad_1.parquet")
        ));

        let first = compact_activity_rows(&workspace, "owner", "default").expect("compaction");
        assert_eq!(first.partitions_failed, 1);
        assert_eq!(
            first.inputs_quarantined, 0,
            "no readable sibling, no broken envelope: keep"
        );
        let state = read_fold_backoff(&hour_partition, PartitionedDataset::ActivityRows)
            .expect("backoff recorded");
        assert_eq!(state.consecutive_failures, 1);
        assert!(
            state
                .last_error
                .starts_with("reading Parquet compaction inputs: "),
            "the recorded error keeps the whole chain, not only the outer context: {}",
            state.last_error
        );

        let second = compact_activity_rows(&workspace, "owner", "default").expect("compaction");
        assert_eq!(
            second.partitions_backed_off, 1,
            "the same inputs are not retried at once"
        );
        assert_eq!(second.partitions_failed, 0);
        assert_eq!(
            read_fold_backoff(&hour_partition, PartitionedDataset::ActivityRows),
            Some(state),
            "a skipped attempt does not count as a failure"
        );

        // A readable batch arrives: the input set changed, so the fold runs
        // now — and with a readable sibling, the bad pair is set aside.
        write_rows(&root, &[activity_row(date, 0, "1", "success", 10)]);
        let third = compact_activity_rows(&workspace, "owner", "default").expect("compaction");
        assert_eq!(third.partitions_backed_off, 0);
        assert_eq!(third.inputs_quarantined, 2);
        assert_eq!(third.partitions_compacted, 1);
        let folded = parquet_files_under(&hour_partition);
        assert_eq!(folded.len(), 1);
        assert_eq!(rows_in(&folded[0]), 1);
        assert!(read_fold_backoff(&hour_partition, PartitionedDataset::ActivityRows).is_none());
    }

    #[test]
    fn fold_retry_delay_doubles_and_is_capped_at_the_full_sweep() {
        assert_eq!(fold_retry_delay(1).num_seconds(), 300);
        assert_eq!(fold_retry_delay(2).num_seconds(), 600);
        assert_eq!(fold_retry_delay(3).num_seconds(), 1_200);
        assert_eq!(fold_retry_delay(7).num_seconds(), 19_200);
        assert_eq!(fold_retry_delay(8).num_seconds(), FOLD_RETRY_CAP_SECS);
        assert_eq!(
            fold_retry_delay(u32::MAX).num_seconds(),
            FOLD_RETRY_CAP_SECS
        );
        assert_eq!(fold_retry_delay(0).num_seconds(), 300);
    }

    #[test]
    fn fold_backoff_counts_failures_per_input_set_and_expires() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let partition = temporary.path().join("dt=2026-01-01");
        std::fs::create_dir_all(&partition).expect("partition");
        let dataset = PartitionedDataset::Events;
        let now = Utc::now();
        let failure = anyhow!("inner cause").context("outer context");

        let first = record_fold_failure(&partition, dataset, "inputs-a", &failure, now);
        assert_eq!(first.consecutive_failures, 1);
        assert_eq!(first.last_error, "outer context: inner cause");
        let second = record_fold_failure(&partition, dataset, "inputs-a", &failure, now);
        assert_eq!(second.consecutive_failures, 2);
        assert_eq!(
            second.next_attempt_at_ms,
            (now + ChronoDuration::seconds(600)).timestamp_millis()
        );

        assert!(fold_backoff_pending(&partition, dataset, "inputs-a", now).is_some());
        assert!(
            fold_backoff_pending(&partition, dataset, "inputs-b", now).is_none(),
            "different inputs are never held back by another set's failures"
        );
        assert!(
            fold_backoff_pending(
                &partition,
                dataset,
                "inputs-a",
                now + ChronoDuration::seconds(601)
            )
            .is_none(),
            "an expired backoff allows the retry"
        );

        let reset = record_fold_failure(&partition, dataset, "inputs-b", &failure, now);
        assert_eq!(
            reset.consecutive_failures, 1,
            "a new input set starts counting again"
        );

        clear_fold_backoff(&partition, dataset);
        assert!(read_fold_backoff(&partition, dataset).is_none());
        clear_fold_backoff(&partition, dataset);
    }

    #[test]
    fn fold_input_signature_changes_with_the_input_set() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let a = temporary.path().join("batch_a.parquet");
        let b = temporary.path().join("batch_b.parquet");
        std::fs::write(&a, b"aa").expect("a");
        std::fs::write(&b, b"bbb").expect("b");
        let both = fold_input_signature(None, &[a.clone(), b.clone()]);
        assert_eq!(
            both,
            fold_input_signature(None, &[b.clone(), a.clone()]),
            "order-free"
        );
        assert_ne!(both, fold_input_signature(None, &[a.clone()]));
        std::fs::write(&b, b"bbbb").expect("b grows");
        assert_ne!(
            both,
            fold_input_signature(None, &[a, b]),
            "a resized input is a new set"
        );
    }

    /// The writer now stages under `.batch_<ulid>.parquet.tmp`. A crash
    /// leaves that staging name, never a readable `batch_`: readers ignore it
    /// and the next fold of the hour sweeps it.
    #[test]
    fn an_orphaned_batch_staging_file_is_invisible_and_swept() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let root = workspace.analytics_activity_rows_root("owner", "default");
        let date = Utc::now().date_naive() - ChronoDuration::days(1);
        for id in ["1", "2"] {
            write_rows(&root, &[activity_row(date, 0, id, "success", 10)]);
        }
        let hour_partition = root.join(format!("dt={date}")).join("hour=00");
        let staging = hour_partition.join(".batch_01ABCDEFGHJKMNPQRSTVWXYZ00.parquet.tmp");
        std::fs::write(&staging, b"").expect("orphaned staging file");
        backdate(&staging, WRITER_STAGING_MIN_AGE + Duration::from_secs(60));
        assert!(super::super::activity_rows_sink::query_parquet_files(&root)
            .files
            .iter()
            .all(|path| path != &staging));

        let stats = compact_activity_rows(&workspace, "owner", "default").expect("compaction");
        assert_eq!(stats.partitions_compacted, 1);
        assert_eq!(
            stats.inputs_quarantined, 0,
            "a staging name is not an input"
        );
        assert!(
            !staging.exists(),
            "the fold sweeps the dead writer's staging file once it is old"
        );
    }

    fn backdate(path: &Path, age: Duration) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open for mtime")
            .set_modified(std::time::SystemTime::now() - age)
            .expect("backdate mtime");
    }

    /// A writer that does not hold the analytics guard can be mid-`COPY`
    /// while maintenance sweeps; its staging file must survive until it is
    /// old enough to be certainly dead. This module's own staging names are
    /// only written under the guard every sweep caller holds, so they go at
    /// once.
    #[test]
    fn the_staging_sweep_removes_only_dead_writer_files() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let partition = temporary.path().join("dt=2026-01-01");
        std::fs::create_dir_all(&partition).expect("partition");
        let live = partition.join(".batch_live.parquet.tmp");
        let dead = partition.join(".batch_dead.parquet.tmp");
        let reprice = partition.join(format!(
            ".events.compacted.{}.parquet.reprice.{}.tmp",
            ulid::Ulid::new(),
            ulid::Ulid::new()
        ));
        let generation = partition.join(format!(
            ".events.compacted.{}.parquet.tmp",
            ulid::Ulid::new()
        ));
        let rollup = partition.join(".rollup_0123abcd.parquet.tmp");
        let published = partition.join("batch_kept.parquet");
        for path in [&live, &dead, &reprice, &generation, &rollup, &published] {
            std::fs::write(path, b"x").expect("fixture");
        }
        backdate(&dead, WRITER_STAGING_MIN_AGE + Duration::from_secs(60));

        remove_orphan_staging_files(&partition).expect("sweep");

        assert!(
            live.exists(),
            "an in-flight writer staging file is never touched"
        );
        assert!(
            reprice.exists(),
            "a young repricer staging file is a writer's, not maintenance's"
        );
        assert!(!dead.exists(), "an hour-old writer staging file is swept");
        assert!(
            !generation.exists(),
            "maintenance's own staging goes at once"
        );
        assert!(!rollup.exists(), "maintenance's own staging goes at once");
        assert!(published.exists(), "published objects are never staging");
    }

    #[test]
    fn maintenance_staging_names_are_matched_exactly() {
        let generation = ulid::Ulid::new();
        assert!(is_maintenance_staging_name(&format!(
            ".activity_rows.compacted.{generation}.parquet.tmp"
        )));
        assert!(is_maintenance_staging_name(".rollup_deadbeef.parquet.tmp"));
        assert!(!is_maintenance_staging_name(
            ".batch_01ARZ3NDEKTSV4RRFFQ69G5FAV.parquet.tmp"
        ));
        assert!(!is_maintenance_staging_name(&format!(
            ".llm_calls.compacted.{generation}.parquet.reprice.{generation}.tmp"
        )));
        assert!(!is_maintenance_staging_name(".rollup_not-hex.parquet.tmp"));
        assert!(!is_maintenance_staging_name(".materialize-x.parquet.tmp"));
        assert_eq!(
            parquet_staging_path(Path::new("/d/dt=2026-01-01/batch_x.parquet")),
            PathBuf::from("/d/dt=2026-01-01/.batch_x.parquet.tmp")
        );
    }

    #[test]
    fn the_writer_leaves_no_staging_file_behind() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let root = temporary.path().join("activity_rows");
        let date = Utc::now().date_naive();
        let outcome = write_rows(&root, &[activity_row(date, 1, "1", "success", 10)]);
        assert_eq!(outcome.rows_written, 1);
        let hour_partition = root.join(format!("dt={date}")).join("hour=01");
        let names: Vec<String> = std::fs::read_dir(&hour_partition)
            .expect("hour dir")
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .filter(|name| !name.starts_with(".activity_rows_partition_incomplete"))
            .collect();
        assert_eq!(names.len(), 1, "exactly the published batch: {names:?}");
        assert!(names[0].starts_with("batch_") && names[0].ends_with(".parquet"));
        assert!(rows_in(&hour_partition.join(&names[0])) == 1);
        assert_published_without_staging(&hour_partition, PartitionedDataset::ActivityRows, 1);
    }
}

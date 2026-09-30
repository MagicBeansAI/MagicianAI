//! Verified maintenance for append-only, date-partitioned analytics Parquet.
//!
//! Completed partitions are rewritten into one generation-named root-level
//! compacted object. A checksum + row-count manifest selects the new generation
//! before raw batches or the prior generation are pruned.
//! Readers that enumerate `dt=*/*.parquet` continue to see the compacted object;
//! governed readers can use the manifest to ignore any raw files left by an
//! interrupted prune.
//!
//! The runtime also schedules the canonical LLM compactor. Canonical facts
//! retain immutable raw revisions and use rolling compacted-prefix + raw-tail
//! reads, so their active UTC partition follows a separate five-minute path.

use std::{
    fs::File,
    io::Read,
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
    let root = dataset.root(workspace, principal, scope);
    let bytes_before = directory_apparent_bytes(&root)?;
    let files_before = directory_regular_file_count(&root)?;
    let mut stats = ParquetCompactionStats::default();
    for (date, partition) in partition_dirs(&root)? {
        if date >= Utc::now().date_naive() || oldest_partition.is_some_and(|oldest| date < oldest) {
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
        let raw_files = raw_partition_files(&partition, dataset)?;
        let manifest = read_valid_manifest(&partition, dataset)?;
        let (known_remaining, new_raw) = split_raw_against_manifest(&raw_files, manifest.as_ref())?;
        let recovered_pruned = manifest
            .as_ref()
            .map(|manifest| prune_verified_sources(&known_remaining, &manifest.sources))
            .transpose()?
            .unwrap_or(0);
        if let Some(manifest) = manifest.as_ref() {
            cleanup_inactive_compacted_generations(&partition, dataset, manifest)?;
        }
        stats.raw_files_pruned += recovered_pruned;

        if manifest.is_some() && new_raw.is_empty() {
            if let Some(mut manifest) = manifest {
                if !manifest.raw_sources_pruned || recovered_pruned > 0 {
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
        let query_files_before = (if manifest.is_some() { 1 } else { 0 }) + new_raw.len();
        match compact_partition(&partition, dataset, manifest.as_ref(), &new_raw) {
            Ok((manifest, pruned)) => {
                stats.partitions_compacted += 1;
                stats.raw_files_compacted += new_raw.len();
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
                warn!(
                    target: "analytics::parquet_maintenance",
                    dataset = dataset.as_str(),
                    partition = %partition.display(),
                    error = %error,
                    "verified Parquet compaction failed; source batches were retained"
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
        .context("reading Parquet compaction inputs")?;
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

    let sources = source_fingerprints(new_raw)?;
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
    let pruned = prune_verified_sources(new_raw, &manifest.sources)?;
    manifest.raw_sources_pruned = true;
    write_manifest_atomic(partition, dataset, &manifest)?;
    cleanup_inactive_compacted_generations(partition, dataset, &manifest)?;
    sync_directory(partition)?;
    Ok((manifest, pruned))
}

pub fn compacted_file(partition: &Path, dataset: PartitionedDataset) -> PathBuf {
    read_valid_manifest(partition, dataset)
        .ok()
        .flatten()
        .map(|manifest| manifest_output(partition, &manifest))
        .unwrap_or_else(|| partition.join(dataset.compacted_file_name()))
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
    match compact_partition(day_partition, dataset, day_manifest.as_ref(), &hour_sources) {
        Ok((manifest, pruned)) => {
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
            warn!(
                target: "analytics::parquet_maintenance",
                dataset = dataset.as_str(),
                partition = %day_partition.display(),
                error = %error,
                "verified day fold failed; the hourly objects were retained"
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
    match compact_partition(partition, dataset, manifest.as_ref(), &new_raw) {
        Ok((manifest, pruned)) => {
            stats.partitions_compacted += 1;
            stats.raw_files_compacted += new_raw.len();
            stats.raw_files_pruned += pruned;
            stats.rows_compacted = stats
                .rows_compacted
                .saturating_add(manifest.compacted_row_count);
            stats.query_files_avoided = stats
                .query_files_avoided
                .saturating_add(new_raw.len().saturating_sub(1));
        },
        Err(error) => {
            stats.partitions_failed += 1;
            warn!(
                target: "analytics::parquet_maintenance",
                dataset = dataset.as_str(),
                partition = %partition.display(),
                error = %error,
                "verified hour fold failed; the source batches were retained"
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
        .context("counting activity rollup inputs")?;

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
        .context("writing the activity rollup object")?;
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
/// Only `.<name>.tmp` entries are touched, and only regular files: those names
/// are produced by this module alone, and the caller holds the analytics writer
/// guard, so nothing live can own one.
fn remove_orphan_staging_files(partition: &Path) -> Result<()> {
    let entries = match std::fs::read_dir(partition) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let is_staging = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with('.') && name.ends_with(".tmp"));
        if !is_staging || !entry.file_type()?.is_file() {
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
    for (principal, scope) in workspace.list_scope_segments_sync()? {
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
    for (principal, scope) in workspace.list_scope_segments_sync()? {
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
                    parquet: None,
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

fn file_checksum(path: &Path) -> Result<String> {
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
    Ok(hasher.finalize().to_hex().to_string())
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

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
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
}

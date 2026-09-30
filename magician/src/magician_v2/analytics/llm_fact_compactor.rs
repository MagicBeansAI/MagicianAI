//! Governed compaction for immutable canonical LLM fact revisions.
//!
//! Compaction never deletes or mutates source objects. A compacted object is
//! eligible for reads while its manifest describes a verified subset of the
//! current immutable raw source set and its own checksum still verifies. New
//! raw objects remain visible as a bounded tail beside that compacted prefix;
//! a later generation folds the tail in. Any interrupted or corrupt
//! compaction therefore falls back to immutable raw files without making new
//! observations wait for a whole-day partition rollover.

use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use chrono::{NaiveDate, Utc};
use duckdb::Connection;
use magicllm::LlmScope;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::{
    duckdb_safety::{configure_analytics_connection_checked, try_analytics_duckdb_guard_for},
    llm_fact_registry::LlmCanonicalDataset,
    llm_scoped_path::{ensure_real_scoped_directory_chain, ensure_regular_file_or_missing},
    llm_trace_materializer::MATERIALIZED_FACT_SCHEMA_VERSION,
};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

pub const LLM_FACT_COMPACTION_MANIFEST_SCHEMA_VERSION: u16 = 2;
const LLM_FACT_COMPACTION_LEGACY_MANIFEST_SCHEMA_VERSION: u16 = 1;
pub const DEFAULT_LLM_FACT_ROLLING_TAIL_FILES: usize = 64;
const COMPACT_DIR_NAME: &str = "_compact";
const COMPACT_FILE_NAME: &str = "canonical.compacted.parquet";
const COMPACT_MANIFEST_FILE_NAME: &str = "canonical.compaction-manifest.json";
const COMPACT_GUARD_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LlmFactCompactionPolicy {
    /// Compact an uncompacted partition once it reaches this many files, then
    /// roll a valid generation forward whenever its raw tail reaches the same
    /// bound. Governed reads remain on the previous generation plus the tail
    /// between sweeps.
    pub min_raw_files: usize,
    /// Whether the active UTC partition may use rolling compaction. Disabling
    /// this is retained for callers that explicitly require closed days only.
    pub include_current_partition: bool,
}

impl Default for LlmFactCompactionPolicy {
    fn default() -> Self {
        Self {
            min_raw_files: DEFAULT_LLM_FACT_ROLLING_TAIL_FILES,
            include_current_partition: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmFactCompactionStats {
    pub partitions_scanned: usize,
    pub partitions_compacted: usize,
    pub partitions_already_fresh: usize,
    pub partitions_with_bounded_tail: usize,
    pub partitions_below_threshold: usize,
    pub partitions_skipped_current: usize,
    pub partitions_failed: usize,
    pub raw_files_compacted: usize,
    pub raw_tail_files_visible: usize,
    pub rows_compacted: u64,
    #[serde(default)]
    pub query_files_avoided: usize,
    #[serde(default)]
    pub bytes_reclaimed: u64,
    #[serde(default)]
    pub compacted_bytes_written: u64,
    #[serde(default)]
    pub areas: Vec<LlmFactCompactionAreaStats>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmFactCompactionAreaStats {
    pub dataset: String,
    pub partitions_compacted: usize,
    pub physical_files_before: usize,
    pub physical_files_after: usize,
    pub raw_files_compacted: usize,
    /// Immutable raw-tail files newly folded by this generation. Unlike
    /// `raw_files_compacted`, this does not recount revisions already covered
    /// by a prior compacted prefix.
    pub source_files_folded: usize,
    pub query_files_before: usize,
    pub query_files_after: usize,
    pub query_files_avoided: usize,
    pub rows_compacted: u64,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub bytes_reclaimed: u64,
    pub compacted_bytes_written: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmCompactionSource {
    pub file_name: String,
    pub byte_len: u64,
    pub checksum_blake3: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmFactCompactionManifest {
    pub schema_version: u16,
    pub materialized_schema_version: u16,
    pub principal: String,
    pub workspace: String,
    pub dataset: LlmCanonicalDataset,
    pub partition_date: String,
    pub generated_at_ms: i64,
    pub source_set_checksum_blake3: String,
    pub sources: Vec<LlmCompactionSource>,
    pub source_row_count: u64,
    pub compacted_file_name: String,
    pub compacted_byte_len: u64,
    pub compacted_checksum_blake3: String,
    pub compacted_row_count: u64,
    pub minimum_journal_sequence: u64,
    pub maximum_journal_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmGovernedPartitionSource {
    pub dataset: LlmCanonicalDataset,
    pub partition_date: String,
    pub files: Vec<PathBuf>,
    pub using_compaction: bool,
    pub raw_file_count: usize,
    pub uncompacted_tail_file_count: usize,
    pub source_row_count: Option<u64>,
    pub materialized_through_sequence: Option<u64>,
}

#[derive(Debug)]
struct LlmUsableCompaction {
    manifest: LlmFactCompactionManifest,
    uncompacted_tail: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartitionSelection {
    Eligible,
    CurrentOnly,
}

pub fn compact_scope(
    workspace: &ArtifactV2Workspace,
    scope: &LlmScope,
    policy: LlmFactCompactionPolicy,
) -> Result<LlmFactCompactionStats> {
    compact_scope_selected(workspace, scope, policy, PartitionSelection::Eligible)
}

/// Roll only the active UTC partition for one scope. The background storage
/// owner uses this lightweight path frequently without rescanning every
/// historical partition or running retention on each hot-tail check.
pub fn compact_current_scope(
    workspace: &ArtifactV2Workspace,
    scope: &LlmScope,
    min_uncompacted_files: usize,
) -> Result<LlmFactCompactionStats> {
    compact_scope_selected(
        workspace,
        scope,
        LlmFactCompactionPolicy {
            min_raw_files: min_uncompacted_files,
            include_current_partition: true,
        },
        PartitionSelection::CurrentOnly,
    )
}

fn compact_scope_selected(
    workspace: &ArtifactV2Workspace,
    scope: &LlmScope,
    policy: LlmFactCompactionPolicy,
    selection: PartitionSelection,
) -> Result<LlmFactCompactionStats> {
    validate_scope(scope)?;
    let mut stats = LlmFactCompactionStats::default();
    let current_date = Utc::now().date_naive();
    let threshold = policy.min_raw_files.max(1);
    for dataset in LlmCanonicalDataset::ALL {
        let root = dataset.root(workspace, scope);
        ensure_real_scoped_directory_chain(workspace.base_root(), &root)?;
        let mut area_partitions_compacted = 0_usize;
        let mut area_raw_files_compacted = 0_usize;
        let mut area_source_files_folded = 0_usize;
        let mut area_query_files_before = 0_usize;
        let mut area_query_files_after = 0_usize;
        let mut area_rows_compacted = 0_u64;
        let mut area_compacted_bytes_written = 0_u64;
        let mut area_physical_files_before = 0_usize;
        let mut area_physical_files_after = 0_usize;
        let mut area_bytes_before = 0_u64;
        let mut area_bytes_after = 0_u64;
        for (date, partition) in canonical_partition_dirs(&root)? {
            if selection == PartitionSelection::CurrentOnly && date != current_date {
                continue;
            }
            stats.partitions_scanned += 1;
            if !policy.include_current_partition && date >= current_date {
                stats.partitions_skipped_current += 1;
                continue;
            }
            let raw_files = canonical_raw_partition_files(dataset, &partition)?;
            let usable = usable_manifest(workspace, scope, dataset, &partition, &raw_files)?;
            let (
                compaction,
                query_files_before,
                source_files_folded,
                partition_bytes_before,
                partition_physical_files_before,
            ) = if let Some(usable) = usable {
                if usable.uncompacted_tail.is_empty() {
                    stats.partitions_already_fresh += 1;
                    continue;
                }
                if usable.uncompacted_tail.len() < threshold {
                    stats.partitions_with_bounded_tail += 1;
                    stats.raw_tail_files_visible = stats
                        .raw_tail_files_visible
                        .saturating_add(usable.uncompacted_tail.len());
                    continue;
                }
                let mut inputs = Vec::with_capacity(1 + usable.uncompacted_tail.len());
                inputs.push(compacted_file(&partition));
                inputs.extend(usable.uncompacted_tail.iter().cloned());
                let source_files_folded = usable.uncompacted_tail.len();
                let partition_bytes_before = directory_apparent_bytes(&partition)?;
                let partition_physical_files_before = directory_regular_file_count(&partition)?;
                (
                    generation_sources(&raw_files, Some(&usable.manifest)).and_then(|sources| {
                        compact_partition(
                            workspace, scope, dataset, &date, &partition, &inputs, sources,
                        )
                    }),
                    inputs.len(),
                    source_files_folded,
                    partition_bytes_before,
                    partition_physical_files_before,
                )
            } else {
                if raw_files.len() < threshold {
                    stats.partitions_below_threshold += 1;
                    continue;
                }
                let partition_bytes_before = directory_apparent_bytes(&partition)?;
                let partition_physical_files_before = directory_regular_file_count(&partition)?;
                (
                    generation_sources(&raw_files, None).and_then(|sources| {
                        compact_partition(
                            workspace, scope, dataset, &date, &partition, &raw_files, sources,
                        )
                    }),
                    raw_files.len(),
                    raw_files.len(),
                    partition_bytes_before,
                    partition_physical_files_before,
                )
            };
            match compaction {
                Ok(manifest) => {
                    stats.partitions_compacted += 1;
                    stats.raw_files_compacted += manifest.sources.len();
                    stats.rows_compacted = stats
                        .rows_compacted
                        .saturating_add(manifest.compacted_row_count);
                    let avoided = query_files_before.saturating_sub(1);
                    stats.query_files_avoided = stats.query_files_avoided.saturating_add(avoided);
                    stats.compacted_bytes_written = stats
                        .compacted_bytes_written
                        .saturating_add(manifest.compacted_byte_len);
                    area_partitions_compacted += 1;
                    area_raw_files_compacted += manifest.sources.len();
                    area_source_files_folded += source_files_folded;
                    area_query_files_before += query_files_before;
                    area_query_files_after += 1;
                    area_rows_compacted =
                        area_rows_compacted.saturating_add(manifest.compacted_row_count);
                    area_compacted_bytes_written =
                        area_compacted_bytes_written.saturating_add(manifest.compacted_byte_len);
                    area_physical_files_before =
                        area_physical_files_before.saturating_add(partition_physical_files_before);
                    area_physical_files_after = area_physical_files_after
                        .saturating_add(directory_regular_file_count(&partition)?);
                    area_bytes_before = area_bytes_before.saturating_add(partition_bytes_before);
                    area_bytes_after =
                        area_bytes_after.saturating_add(directory_apparent_bytes(&partition)?);
                },
                Err(error) => {
                    stats.partitions_failed += 1;
                    warn!(
                        target: "analytics::llm_fact_compactor",
                        principal = %scope.principal,
                        workspace = %scope.workspace,
                        dataset = dataset.as_str(),
                        partition = %partition.display(),
                        error = %error,
                        "LLM fact compaction failed; governed reads will use immutable raw revisions"
                    );
                },
            }
        }
        if area_partitions_compacted > 0 {
            let bytes_reclaimed = area_bytes_before.saturating_sub(area_bytes_after);
            let query_files_avoided =
                area_query_files_before.saturating_sub(area_query_files_after);
            stats.bytes_reclaimed = stats.bytes_reclaimed.saturating_add(bytes_reclaimed);
            stats.areas.push(LlmFactCompactionAreaStats {
                dataset: dataset.as_str().to_string(),
                partitions_compacted: area_partitions_compacted,
                physical_files_before: area_physical_files_before,
                physical_files_after: area_physical_files_after,
                raw_files_compacted: area_raw_files_compacted,
                source_files_folded: area_source_files_folded,
                query_files_before: area_query_files_before,
                query_files_after: area_query_files_after,
                query_files_avoided,
                rows_compacted: area_rows_compacted,
                bytes_before: area_bytes_before,
                bytes_after: area_bytes_after,
                bytes_reclaimed,
                compacted_bytes_written: area_compacted_bytes_written,
            });
        }
    }
    Ok(stats)
}

pub fn governed_dataset_sources(
    workspace: &ArtifactV2Workspace,
    scope: &LlmScope,
    dataset: LlmCanonicalDataset,
) -> Result<Vec<LlmGovernedPartitionSource>> {
    governed_dataset_sources_in_date_range(workspace, scope, dataset, None, None)
}

pub fn governed_dataset_sources_in_date_range(
    workspace: &ArtifactV2Workspace,
    scope: &LlmScope,
    dataset: LlmCanonicalDataset,
    earliest_date: Option<NaiveDate>,
    latest_date: Option<NaiveDate>,
) -> Result<Vec<LlmGovernedPartitionSource>> {
    validate_scope(scope)?;
    let root = dataset.root(workspace, scope);
    ensure_real_scoped_directory_chain(workspace.base_root(), &root)?;
    canonical_partition_dirs(&root)?
        .into_iter()
        .filter(|(date, _)| earliest_date.is_none_or(|earliest| *date >= earliest))
        .filter(|(date, _)| latest_date.is_none_or(|latest| *date <= latest))
        .map(|(date, partition)| {
            let raw_files = canonical_raw_partition_files(dataset, &partition)?;
            if let Some(usable) =
                usable_manifest(workspace, scope, dataset, &partition, &raw_files)?
            {
                let mut files = Vec::with_capacity(1 + usable.uncompacted_tail.len());
                files.push(compacted_file(&partition));
                files.extend(usable.uncompacted_tail.iter().cloned());
                return Ok(LlmGovernedPartitionSource {
                    dataset,
                    partition_date: date.format("%Y-%m-%d").to_string(),
                    files,
                    using_compaction: true,
                    raw_file_count: raw_files.len(),
                    uncompacted_tail_file_count: usable.uncompacted_tail.len(),
                    source_row_count: Some(usable.manifest.source_row_count),
                    materialized_through_sequence: Some(usable.manifest.maximum_journal_sequence),
                });
            }
            Ok(LlmGovernedPartitionSource {
                dataset,
                partition_date: date.format("%Y-%m-%d").to_string(),
                files: raw_files.clone(),
                using_compaction: false,
                raw_file_count: raw_files.len(),
                uncompacted_tail_file_count: raw_files.len(),
                source_row_count: None,
                materialized_through_sequence: None,
            })
        })
        .collect()
}

pub fn governed_dataset_files(
    workspace: &ArtifactV2Workspace,
    scope: &LlmScope,
    dataset: LlmCanonicalDataset,
) -> Result<Vec<PathBuf>> {
    Ok(governed_dataset_sources(workspace, scope, dataset)?
        .into_iter()
        .flat_map(|source| source.files)
        .collect())
}

pub fn canonical_raw_partition_files(
    dataset: LlmCanonicalDataset,
    partition: &Path,
) -> Result<Vec<PathBuf>> {
    let prefix = dataset.raw_file_prefix();
    let entries = match std::fs::read_dir(partition) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("reading canonical partition {}", partition.display()));
        },
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("reading entry in {}", partition.display()))?;
        let path = entry.path();
        let is_candidate = path.extension().and_then(|extension| extension.to_str())
            == Some("parquet")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix));
        if is_candidate {
            let file_type = entry
                .file_type()
                .with_context(|| format!("reading canonical source type {}", path.display()))?;
            if !file_type.is_file() {
                return Err(anyhow!(
                    "canonical LLM fact source must be a regular file, not a symlink or special entry: {}",
                    path.display()
                ));
            }
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

pub fn legacy_call_partition_files(partition: &Path) -> Result<Vec<PathBuf>> {
    super::parquet_maintenance::partition_sources(
        partition,
        super::parquet_maintenance::PartitionedDataset::LegacyLlmCalls,
    )
}

pub fn canonical_partition_dirs(root: &Path) -> Result<Vec<(NaiveDate, PathBuf)>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("reading analytics dataset root {}", root.display()));
        },
    };
    let mut partitions = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("reading entry in {}", root.display()))?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(raw_date) = name.strip_prefix("dt=") else {
            continue;
        };
        let date = NaiveDate::parse_from_str(raw_date, "%Y-%m-%d")
            .with_context(|| format!("invalid canonical LLM partition name {}", path.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("reading partition type {}", path.display()))?;
        if !file_type.is_dir() {
            return Err(anyhow!(
                "canonical LLM partition must be a real directory, not a symlink or file: {}",
                path.display()
            ));
        }
        partitions.push((date, path));
    }
    partitions.sort_by_key(|(date, _)| *date);
    Ok(partitions)
}

fn compact_partition(
    workspace: &ArtifactV2Workspace,
    scope: &LlmScope,
    dataset: LlmCanonicalDataset,
    date: &NaiveDate,
    partition: &Path,
    input_files: &[PathBuf],
    sources: Vec<LlmCompactionSource>,
) -> Result<LlmFactCompactionManifest> {
    if input_files.is_empty() || sources.is_empty() {
        return Err(anyhow!("cannot compact an empty LLM fact source set"));
    }
    let source_set_checksum_blake3 = source_set_checksum(&sources)?;
    let compact_dir = partition.join(COMPACT_DIR_NAME);
    std::fs::create_dir_all(&compact_dir)
        .with_context(|| format!("creating LLM compact directory {}", compact_dir.display()))?;
    let compact_metadata = std::fs::symlink_metadata(&compact_dir)
        .with_context(|| format!("inspecting LLM compact directory {}", compact_dir.display()))?;
    if !compact_metadata.file_type().is_dir() {
        return Err(anyhow!(
            "LLM compact destination must be a real directory, not a symlink or special entry: {}",
            compact_dir.display()
        ));
    }
    let tmp = compact_dir.join(format!(".{COMPACT_FILE_NAME}.{}.tmp", ulid::Ulid::new()));

    let Some(_duckdb_guard) = try_analytics_duckdb_guard_for(COMPACT_GUARD_TIMEOUT) else {
        return Err(anyhow!("timed out waiting for the analytics DuckDB guard"));
    };
    let connection =
        Connection::open_in_memory().context("opening in-memory DuckDB for LLM fact compaction")?;
    configure_analytics_connection_checked(&connection, "llm_fact_compaction")
        .context("configuring LLM fact compaction DuckDB")?;
    let input = parquet_source_sql(input_files);
    connection
        .execute_batch(&format!(
            "CREATE TABLE llm_fact_compact AS SELECT * FROM read_parquet({input}, hive_partitioning = false, union_by_name = true);"
        ))
        .context("reading canonical LLM revision files for compaction")?;
    let (rows, unique_keys, minimum_sequence, maximum_sequence, record_kinds, principals, workspaces):
        (i64, i64, i64, i64, i64, i64, i64) = connection
        .query_row(
            "SELECT count(*), count(DISTINCT idempotency_key), CAST(min(journal_sequence) AS BIGINT), CAST(max(journal_sequence) AS BIGINT), count(DISTINCT record_kind), count(DISTINCT principal), count(DISTINCT workspace) FROM llm_fact_compact",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
        )
        .context("validating canonical LLM revision source rows")?;
    if rows <= 0 || rows != unique_keys {
        return Err(anyhow!(
            "LLM compaction requires unique idempotency keys: rows={rows}, unique={unique_keys}"
        ));
    }
    if minimum_sequence <= 0 || maximum_sequence < minimum_sequence {
        return Err(anyhow!(
            "LLM compaction requires positive ordered journal sequences: min={minimum_sequence}, max={maximum_sequence}"
        ));
    }
    if record_kinds != 1 || principals != 1 || workspaces != 1 {
        return Err(anyhow!(
            "LLM compaction source crosses a record-kind or scope boundary"
        ));
    }
    let (actual_kind, actual_principal, actual_workspace): (String, String, String) = connection
        .query_row(
            "SELECT min(record_kind), min(principal), min(workspace) FROM llm_fact_compact",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .context("reading canonical LLM source identity")?;
    if actual_kind != dataset.record_kind()
        || actual_principal != scope.principal
        || actual_workspace != scope.workspace
    {
        return Err(anyhow!(
            "LLM compaction identity mismatch: expected {}/{}/{}, found {actual_principal}/{actual_workspace}/{actual_kind}",
            scope.principal,
            scope.workspace,
            dataset.record_kind()
        ));
    }
    let copy_result = connection.execute_batch(&format!(
        "COPY llm_fact_compact TO '{}' (FORMAT PARQUET, COMPRESSION 'zstd');",
        escape_sql_literal(&tmp.display().to_string())
    ));
    if let Err(error) = copy_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(error).context("writing temporary compacted LLM fact object");
    }
    sync_file(&tmp)?;
    let compacted_rows = count_parquet_rows(&connection, &tmp)?;
    if compacted_rows != rows {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow!(
            "LLM compaction row guard failed: source={rows}, compacted={compacted_rows}"
        ));
    }
    let final_path = compacted_file(partition);
    std::fs::rename(&tmp, &final_path).with_context(|| {
        format!(
            "publishing compacted LLM fact {} -> {}",
            tmp.display(),
            final_path.display()
        )
    })?;
    sync_directory(&compact_dir)?;
    crate::magician_v2::dataset_owners::publish_written_parquet(&final_path).with_context(
        || {
            format!(
                "publishing compacted LLM fact through DatasetAccess {}",
                final_path.display()
            )
        },
    )?;
    let compacted_byte_len = std::fs::metadata(&final_path)?.len();
    let compacted_checksum_blake3 = file_checksum(&final_path)?;
    let manifest = LlmFactCompactionManifest {
        schema_version: LLM_FACT_COMPACTION_MANIFEST_SCHEMA_VERSION,
        materialized_schema_version: MATERIALIZED_FACT_SCHEMA_VERSION,
        principal: scope.principal.clone(),
        workspace: scope.workspace.clone(),
        dataset,
        partition_date: date.format("%Y-%m-%d").to_string(),
        generated_at_ms: Utc::now().timestamp_millis(),
        source_set_checksum_blake3,
        sources,
        source_row_count: rows as u64,
        compacted_file_name: COMPACT_FILE_NAME.to_string(),
        compacted_byte_len,
        compacted_checksum_blake3,
        compacted_row_count: compacted_rows as u64,
        minimum_journal_sequence: minimum_sequence as u64,
        maximum_journal_sequence: maximum_sequence as u64,
    };
    workspace
        .write_json_atomic_path_sync(compaction_manifest_path(partition), &manifest)
        .context("publishing LLM compaction manifest")?;
    Ok(manifest)
}

fn usable_manifest(
    workspace: &ArtifactV2Workspace,
    scope: &LlmScope,
    dataset: LlmCanonicalDataset,
    partition: &Path,
    raw_files: &[PathBuf],
) -> Result<Option<LlmUsableCompaction>> {
    if raw_files.is_empty() {
        return Ok(None);
    }
    let manifest_path = compaction_manifest_path(partition);
    match ensure_regular_file_or_missing(&manifest_path) {
        Ok(false) => return Ok(None),
        Ok(true) => {},
        Err(error) => {
            warn!(
                target: "analytics::llm_fact_compactor",
                path = %manifest_path.display(),
                error = %error,
                "LLM compaction manifest is not a regular file; governed reads will use raw revisions"
            );
            return Ok(None);
        },
    }
    let manifest =
        match workspace.read_json_path_sync::<LlmFactCompactionManifest, _>(&manifest_path) {
            Ok(manifest) => manifest,
            Err(crate::magician_v2::artifact_v2::service::ArtifactV2Error::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(None);
            },
            Err(error) => {
                warn!(
                    target: "analytics::llm_fact_compactor",
                    path = %manifest_path.display(),
                    error = %error,
                    "LLM compaction manifest is unreadable; governed reads will use raw revisions"
                );
                return Ok(None);
            },
        };
    if !matches!(
        manifest.schema_version,
        LLM_FACT_COMPACTION_LEGACY_MANIFEST_SCHEMA_VERSION
            | LLM_FACT_COMPACTION_MANIFEST_SCHEMA_VERSION
    ) || manifest.materialized_schema_version != MATERIALIZED_FACT_SCHEMA_VERSION
        || manifest.dataset != dataset
        || manifest.compacted_file_name != COMPACT_FILE_NAME
        || manifest.principal != scope.principal
        || manifest.workspace != scope.workspace
        || manifest.partition_date
            != partition
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix("dt="))
                .unwrap_or_default()
    {
        return Ok(None);
    }
    if !valid_manifest_structure(&manifest) {
        return Ok(None);
    }
    if manifest.source_set_checksum_blake3 != source_set_checksum(&manifest.sources)? {
        return Ok(None);
    }
    let raw_names = raw_files
        .iter()
        .filter_map(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        })
        .collect::<std::collections::HashSet<_>>();
    let covered_names = manifest
        .sources
        .iter()
        .map(|source| source.file_name.clone())
        .collect::<std::collections::HashSet<_>>();
    if !covered_names.is_subset(&raw_names) {
        return Ok(None);
    }
    let output = compacted_file(partition);
    let Ok(metadata) = std::fs::symlink_metadata(&output) else {
        return Ok(None);
    };
    if !metadata.file_type().is_file() {
        return Ok(None);
    }
    let output_checksum = match file_checksum(&output) {
        Ok(checksum) => checksum,
        Err(error) => {
            warn!(
                target: "analytics::llm_fact_compactor",
                path = %output.display(),
                error = %error,
                "compacted LLM fact is unreadable; governed reads will use raw revisions"
            );
            return Ok(None);
        },
    };
    if metadata.len() != manifest.compacted_byte_len
        || output_checksum != manifest.compacted_checksum_blake3
        || manifest.source_row_count != manifest.compacted_row_count
    {
        return Ok(None);
    }
    let uncompacted_tail = raw_files
        .iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !covered_names.contains(name))
        })
        .cloned()
        .collect();
    Ok(Some(LlmUsableCompaction {
        manifest,
        uncompacted_tail,
    }))
}

/// Build the next generation's complete source manifest. Governed reads avoid
/// rehashing covered raw objects, but the less-frequent generation roll
/// revalidates every immutable source fingerprint before carrying it forward.
/// A modified canonical fallback therefore blocks publication instead of
/// being silently blessed by a newer compacted generation.
fn generation_sources(
    raw_files: &[PathBuf],
    previous: Option<&LlmFactCompactionManifest>,
) -> Result<Vec<LlmCompactionSource>> {
    let previous = previous
        .into_iter()
        .flat_map(|manifest| manifest.sources.iter())
        .map(|source| (source.file_name.as_str(), source))
        .collect::<std::collections::HashMap<_, _>>();
    raw_files
        .iter()
        .map(|path| {
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| anyhow!("LLM source has no UTF-8 file name: {}", path.display()))?;
            let actual = source_fingerprint(path)?;
            if let Some(expected) = previous.get(file_name) {
                if *expected != &actual {
                    return Err(anyhow!(
                        "immutable LLM source changed after compaction: {}",
                        path.display()
                    ));
                }
            }
            Ok(actual)
        })
        .collect()
}

fn source_fingerprint(path: &Path) -> Result<LlmCompactionSource> {
    Ok(LlmCompactionSource {
        file_name: path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("LLM source has no UTF-8 file name: {}", path.display()))?
            .to_string(),
        byte_len: std::fs::metadata(path)?.len(),
        checksum_blake3: file_checksum(path)?,
    })
}

fn source_set_checksum(sources: &[LlmCompactionSource]) -> Result<String> {
    let bytes = serde_json::to_vec(sources).context("serializing LLM compaction source set")?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn file_checksum(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("opening {} for checksum", path.display()))?;
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

fn directory_apparent_bytes(path: &Path) -> Result<u64> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_file() {
        return Ok(metadata.len());
    }
    if !metadata.file_type().is_dir() {
        return Ok(0);
    }
    let mut total = 0_u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_file() {
            total = total.saturating_add(entry.metadata()?.len());
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
        .context("counting compacted LLM fact rows")
}

fn compacted_file(partition: &Path) -> PathBuf {
    partition.join(COMPACT_DIR_NAME).join(COMPACT_FILE_NAME)
}

fn compaction_manifest_path(partition: &Path) -> PathBuf {
    partition
        .join(COMPACT_DIR_NAME)
        .join(COMPACT_MANIFEST_FILE_NAME)
}

fn parquet_source_sql(files: &[PathBuf]) -> String {
    let paths = files
        .iter()
        .map(|path| format!("'{}'", escape_sql_literal(&path.display().to_string())))
        .collect::<Vec<_>>();
    match paths.as_slice() {
        [one] => one.clone(),
        _ => format!("[{}]", paths.join(", ")),
    }
}

pub fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

pub fn validate_scope(scope: &LlmScope) -> Result<()> {
    if !scope.is_valid() {
        return Err(anyhow!(
            "LLM analytics requires safe, explicit principal and workspace path components"
        ));
    }
    Ok(())
}

fn valid_manifest_structure(manifest: &LlmFactCompactionManifest) -> bool {
    let mut source_names = std::collections::HashSet::new();
    manifest.generated_at_ms > 0
        && is_lower_hex_64(&manifest.source_set_checksum_blake3)
        && !manifest.sources.is_empty()
        && manifest.sources.iter().all(|source| {
            source.byte_len > 0
                && is_lower_hex_64(&source.checksum_blake3)
                && !source.file_name.is_empty()
                && Path::new(&source.file_name)
                    .file_name()
                    .and_then(|name| name.to_str())
                    == Some(source.file_name.as_str())
                && source_names.insert(source.file_name.clone())
        })
        && manifest.source_row_count > 0
        && manifest.compacted_byte_len > 0
        && is_lower_hex_64(&manifest.compacted_checksum_blake3)
        && manifest.compacted_row_count == manifest.source_row_count
        && manifest.minimum_journal_sequence > 0
        && manifest.maximum_journal_sequence >= manifest.minimum_journal_sequence
}

fn is_lower_hex_64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sync_file(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("opening {} for fsync", path.display()))?
        .sync_all()
        .with_context(|| format!("syncing {}", path.display()))
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("opening directory {} for fsync", path.display()))?
        .sync_all()
        .with_context(|| format!("syncing directory {}", path.display()))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn write_canonical_fixture(
        path: &Path,
        record_kind: &str,
        principal: &str,
        workspace: &str,
        idempotency_key: &str,
        sequence: u64,
    ) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let connection = Connection::open_in_memory().expect("duckdb");
        connection
            .execute_batch(&format!(
                "CREATE TABLE fixture(idempotency_key VARCHAR, journal_sequence UBIGINT, record_kind VARCHAR, principal VARCHAR, workspace VARCHAR); INSERT INTO fixture VALUES ('{}', {}, '{}', '{}', '{}'); COPY fixture TO '{}' (FORMAT PARQUET)",
                escape_sql_literal(idempotency_key),
                sequence,
                escape_sql_literal(record_kind),
                escape_sql_literal(principal),
                escape_sql_literal(workspace),
                escape_sql_literal(&path.display().to_string()),
            ))
            .expect("write parquet fixture");
    }

    #[test]
    fn canonical_selector_never_mixes_legacy_or_compacted_objects() {
        let temp = tempfile::tempdir().expect("tempdir");
        let partition = temp.path().join("dt=2026-07-21");
        std::fs::create_dir_all(partition.join(COMPACT_DIR_NAME)).expect("mkdir");
        let canonical = partition.join("part-call_fact-a-r1.parquet");
        std::fs::write(&canonical, b"canonical").expect("canonical");
        std::fs::write(partition.join("batch_legacy.parquet"), b"legacy").expect("legacy");
        std::fs::write(compacted_file(&partition), b"compact").expect("compact");

        assert_eq!(
            canonical_raw_partition_files(LlmCanonicalDataset::Calls, &partition)
                .expect("canonical files"),
            vec![canonical]
        );
        assert_eq!(
            legacy_call_partition_files(&partition).expect("legacy files"),
            vec![partition.join("batch_legacy.parquet")]
        );
    }

    #[test]
    fn partition_discovery_is_date_validated_and_sorted() {
        let temp = tempfile::tempdir().expect("tempdir");
        for name in ["dt=2026-07-22", "dt=invalid", "dt=2026-07-20"] {
            std::fs::create_dir_all(temp.path().join(name)).expect("mkdir");
        }
        std::fs::write(temp.path().join("dt=2026-07-19"), b"file").expect("file");
        assert!(canonical_partition_dirs(temp.path())
            .expect_err("invalid date must fail closed")
            .to_string()
            .contains("invalid canonical LLM partition"));
        std::fs::remove_dir(temp.path().join("dt=invalid")).expect("remove invalid date");
        assert!(canonical_partition_dirs(temp.path())
            .expect_err("partition file must fail closed")
            .to_string()
            .contains("must be a real directory"));
        std::fs::remove_file(temp.path().join("dt=2026-07-19")).expect("remove partition file");
        let dates = canonical_partition_dirs(temp.path())
            .expect("partitions")
            .into_iter()
            .map(|(date, _)| date.format("%Y-%m-%d").to_string())
            .collect::<Vec<_>>();
        assert_eq!(dates, vec!["2026-07-20", "2026-07-22"]);
    }

    #[cfg(unix)]
    #[test]
    fn partition_and_source_symlinks_fail_closed() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let external = tempfile::tempdir().expect("external");
        symlink(external.path(), temp.path().join("dt=2026-07-20")).expect("partition symlink");
        assert!(canonical_partition_dirs(temp.path())
            .expect_err("symlink partition")
            .to_string()
            .contains("must be a real directory"));

        std::fs::remove_file(temp.path().join("dt=2026-07-20")).expect("remove partition symlink");
        let partition = temp.path().join("dt=2026-07-21");
        std::fs::create_dir_all(&partition).expect("partition");
        let outside_file = external.path().join("outside.parquet");
        std::fs::write(&outside_file, b"outside").expect("outside source");
        symlink(
            &outside_file,
            partition.join("part-call_fact-symlink-r1.parquet"),
        )
        .expect("source symlink");
        assert!(
            canonical_raw_partition_files(LlmCanonicalDataset::Calls, &partition)
                .expect_err("symlink source")
                .to_string()
                .contains("must be a regular file")
        );
    }

    #[cfg(unix)]
    #[test]
    fn compact_destination_symlink_cannot_escape_partition() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let external = tempfile::tempdir().expect("external");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        let partition = workspace
            .analytics_llm_calls_root("owner", "default")
            .join("dt=2026-07-20");
        write_canonical_fixture(
            &partition.join("part-call_fact-a-r1.parquet"),
            "call_fact",
            "owner",
            "default",
            "call:a:r1",
            1,
        );
        symlink(external.path(), partition.join(COMPACT_DIR_NAME)).expect("compact symlink");
        let stats = compact_scope(
            &workspace,
            &scope,
            LlmFactCompactionPolicy {
                min_raw_files: 1,
                include_current_partition: true,
            },
        )
        .expect("partition failure remains isolated");
        assert_eq!(stats.partitions_failed, 1);
        assert!(!external.path().join(COMPACT_FILE_NAME).exists());
        assert!(!external.path().join(COMPACT_MANIFEST_FILE_NAME).exists());
    }

    #[cfg(unix)]
    #[test]
    fn manifest_symlink_is_ignored_and_governed_read_falls_back_to_raw() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let external = tempfile::NamedTempFile::new().expect("external manifest");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        let partition = workspace
            .analytics_llm_calls_root("owner", "default")
            .join("dt=2026-07-20");
        let raw = partition.join("part-call_fact-a-r1.parquet");
        write_canonical_fixture(&raw, "call_fact", "owner", "default", "call:a:r1", 1);
        std::fs::create_dir_all(partition.join(COMPACT_DIR_NAME)).expect("compact dir");
        symlink(
            external.path(),
            partition
                .join(COMPACT_DIR_NAME)
                .join(COMPACT_MANIFEST_FILE_NAME),
        )
        .expect("manifest symlink");
        let sources = governed_dataset_sources(&workspace, &scope, LlmCanonicalDataset::Calls)
            .expect("raw fallback");
        assert_eq!(sources.len(), 1);
        assert!(!sources[0].using_compaction);
        assert_eq!(sources[0].files, vec![raw]);
    }

    #[test]
    fn source_set_checksum_is_order_sensitive_but_deterministic() {
        let first = LlmCompactionSource {
            file_name: "a.parquet".to_string(),
            byte_len: 1,
            checksum_blake3: "a".to_string(),
        };
        let second = LlmCompactionSource {
            file_name: "b.parquet".to_string(),
            byte_len: 2,
            checksum_blake3: "b".to_string(),
        };
        assert_eq!(
            source_set_checksum(&[first.clone(), second.clone()]).expect("checksum"),
            source_set_checksum(&[first.clone(), second.clone()]).expect("checksum")
        );
        assert_ne!(
            source_set_checksum(&[first.clone(), second.clone()]).expect("checksum"),
            source_set_checksum(&[second, first]).expect("checksum")
        );
    }

    #[test]
    fn scope_must_be_explicit() {
        assert!(validate_scope(&LlmScope::new("", "default")).is_err());
        assert!(validate_scope(&LlmScope::new("owner", "")).is_err());
        assert!(validate_scope(&LlmScope::new("owner", "default")).is_ok());
    }

    #[test]
    fn compaction_is_manifest_guarded_preserves_raw_and_keeps_appends_visible() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        let partition = workspace
            .analytics_llm_calls_root("owner", "default")
            .join("dt=2026-07-20");
        let first = partition.join("part-call_fact-a-r1.parquet");
        let second = partition.join("part-call_fact-b-r2.parquet");
        write_canonical_fixture(&first, "call_fact", "owner", "default", "call:a:r1", 1);
        write_canonical_fixture(&second, "call_fact", "owner", "default", "call:a:r2", 2);

        let stats = compact_scope(
            &workspace,
            &scope,
            LlmFactCompactionPolicy {
                min_raw_files: 2,
                include_current_partition: true,
            },
        )
        .expect("compact");
        assert_eq!(stats.partitions_compacted, 1);
        assert_eq!(stats.areas.len(), 1);
        assert_eq!(stats.areas[0].dataset, "llm_calls");
        assert_eq!(stats.areas[0].query_files_before, 2);
        assert_eq!(stats.areas[0].query_files_after, 1);
        assert_eq!(stats.areas[0].source_files_folded, 2);
        assert_eq!(stats.query_files_avoided, 1);
        assert!(stats.compacted_bytes_written > 0);
        assert!(first.is_file());
        assert!(second.is_file());
        let governed = governed_dataset_sources(&workspace, &scope, LlmCanonicalDataset::Calls)
            .expect("governed");
        assert!(governed[0].using_compaction);
        assert_eq!(governed[0].files, vec![compacted_file(&partition)]);

        std::fs::write(compacted_file(&partition), b"corrupt").expect("corrupt compact output");
        let corrupt = governed_dataset_sources(&workspace, &scope, LlmCanonicalDataset::Calls)
            .expect("corrupt fallback");
        assert!(!corrupt[0].using_compaction);
        assert_eq!(corrupt[0].files, vec![first.clone(), second.clone()]);
        let repaired = compact_scope(
            &workspace,
            &scope,
            LlmFactCompactionPolicy {
                min_raw_files: 2,
                include_current_partition: true,
            },
        )
        .expect("repair compaction");
        assert_eq!(repaired.partitions_compacted, 1);

        std::fs::write(compaction_manifest_path(&partition), b"{not-json")
            .expect("corrupt manifest");
        let corrupt_manifest =
            governed_dataset_sources(&workspace, &scope, LlmCanonicalDataset::Calls)
                .expect("corrupt manifest fallback");
        assert!(!corrupt_manifest[0].using_compaction);
        assert_eq!(
            corrupt_manifest[0].files,
            vec![first.clone(), second.clone()]
        );
        let repaired_manifest = compact_scope(
            &workspace,
            &scope,
            LlmFactCompactionPolicy {
                min_raw_files: 2,
                include_current_partition: true,
            },
        )
        .expect("repair manifest");
        assert_eq!(repaired_manifest.partitions_compacted, 1);

        let third = partition.join("part-call_fact-c-r1.parquet");
        write_canonical_fixture(&third, "call_fact", "owner", "default", "call:b:r1", 3);
        let rolling = governed_dataset_sources(&workspace, &scope, LlmCanonicalDataset::Calls)
            .expect("rolling sources");
        assert!(rolling[0].using_compaction);
        assert_eq!(rolling[0].uncompacted_tail_file_count, 1);
        assert_eq!(
            rolling[0].files,
            vec![compacted_file(&partition), third.clone()]
        );
        let connection = Connection::open_in_memory().expect("duckdb");
        let rolling_rows: i64 = connection
            .query_row(
                &format!(
                    "SELECT count(*) FROM read_parquet({}, hive_partitioning = false, union_by_name = true)",
                    parquet_source_sql(&rolling[0].files)
                ),
                [],
                |row| row.get(0),
            )
            .expect("rolling rows");
        assert_eq!(rolling_rows, 3);

        let bounded = compact_scope(
            &workspace,
            &scope,
            LlmFactCompactionPolicy {
                min_raw_files: 2,
                include_current_partition: true,
            },
        )
        .expect("tail remains bounded");
        assert_eq!(bounded.partitions_compacted, 0);
        assert_eq!(bounded.partitions_with_bounded_tail, 1);
        assert_eq!(bounded.raw_tail_files_visible, 1);

        let fourth = partition.join("part-call_fact-d-r1.parquet");
        write_canonical_fixture(&fourth, "call_fact", "owner", "default", "call:c:r1", 4);
        let rolled = compact_scope(
            &workspace,
            &scope,
            LlmFactCompactionPolicy {
                min_raw_files: 2,
                include_current_partition: true,
            },
        )
        .expect("roll generation");
        assert_eq!(rolled.partitions_compacted, 1);
        assert_eq!(rolled.raw_files_compacted, 4);
        let governed = governed_dataset_sources(&workspace, &scope, LlmCanonicalDataset::Calls)
            .expect("fully rolled sources");
        assert_eq!(governed[0].files, vec![compacted_file(&partition)]);
        assert_eq!(governed[0].uncompacted_tail_file_count, 0);
        let compacted_rows: i64 = connection
            .query_row(
                &format!(
                    "SELECT count(*) FROM read_parquet('{}', hive_partitioning = false)",
                    escape_sql_literal(&governed[0].files[0].display().to_string())
                ),
                [],
                |row| row.get(0),
            )
            .expect("compacted rows");
        assert_eq!(compacted_rows, 4);
        assert!(first.is_file());
        assert!(second.is_file());
        assert!(third.is_file());
        assert!(fourth.is_file());
    }

    #[test]
    fn compaction_rejects_scope_mismatch_without_publishing_manifest() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        let partition = workspace
            .analytics_llm_calls_root("owner", "default")
            .join("dt=2026-07-20");
        write_canonical_fixture(
            &partition.join("part-call_fact-a-r1.parquet"),
            "call_fact",
            "different-owner",
            "default",
            "call:a:r1",
            1,
        );
        let stats = compact_scope(
            &workspace,
            &scope,
            LlmFactCompactionPolicy {
                min_raw_files: 1,
                include_current_partition: true,
            },
        )
        .expect("scope compaction is isolated per partition failure");
        assert_eq!(stats.partitions_failed, 1);
        assert!(!compaction_manifest_path(&partition).exists());
    }

    #[test]
    fn compaction_rejects_duplicate_revision_keys() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        let partition = workspace
            .analytics_llm_calls_root("owner", "default")
            .join("dt=2026-07-20");
        write_canonical_fixture(
            &partition.join("part-call_fact-a-r1.parquet"),
            "call_fact",
            "owner",
            "default",
            "call:a:r1",
            1,
        );
        write_canonical_fixture(
            &partition.join("part-call_fact-b-r1.parquet"),
            "call_fact",
            "owner",
            "default",
            "call:a:r1",
            2,
        );
        let stats = compact_scope(
            &workspace,
            &scope,
            LlmFactCompactionPolicy {
                min_raw_files: 2,
                include_current_partition: true,
            },
        )
        .expect("duplicate-key compaction is isolated per partition failure");
        assert_eq!(stats.partitions_failed, 1);
        assert!(!compaction_manifest_path(&partition).exists());
    }

    #[test]
    fn rolling_generation_refuses_to_bless_a_changed_immutable_source() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        let partition = workspace
            .analytics_llm_calls_root("owner", "default")
            .join("dt=2026-07-20");
        let covered = partition.join("part-call_fact-a-r1.parquet");
        write_canonical_fixture(&covered, "call_fact", "owner", "default", "call:a:r1", 1);
        compact_scope(
            &workspace,
            &scope,
            LlmFactCompactionPolicy {
                min_raw_files: 1,
                include_current_partition: true,
            },
        )
        .expect("initial compact");
        let manifest_path = compaction_manifest_path(&partition);
        let before: LlmFactCompactionManifest = workspace
            .read_json_path_sync(&manifest_path)
            .expect("manifest before");

        std::fs::write(&covered, b"changed canonical source").expect("change source");
        let tail = partition.join("part-call_fact-b-r1.parquet");
        write_canonical_fixture(&tail, "call_fact", "owner", "default", "call:b:r1", 2);
        let stats = compact_scope(
            &workspace,
            &scope,
            LlmFactCompactionPolicy {
                min_raw_files: 1,
                include_current_partition: true,
            },
        )
        .expect("partition failure remains isolated");
        assert_eq!(stats.partitions_failed, 1);
        let after: LlmFactCompactionManifest = workspace
            .read_json_path_sync(&manifest_path)
            .expect("manifest after");
        assert_eq!(after, before);

        let governed = governed_dataset_sources(&workspace, &scope, LlmCanonicalDataset::Calls)
            .expect("verified compact prefix remains readable");
        assert_eq!(governed[0].files, vec![compacted_file(&partition), tail]);
    }

    #[test]
    fn default_policy_rolls_the_open_utc_partition() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        let partition = workspace
            .analytics_llm_calls_root("owner", "default")
            .join(format!("dt={}", Utc::now().format("%Y-%m-%d")));
        write_canonical_fixture(
            &partition.join("part-call_fact-a-r1.parquet"),
            "call_fact",
            "owner",
            "default",
            "call:a:r1",
            1,
        );
        let stats = compact_scope(
            &workspace,
            &scope,
            LlmFactCompactionPolicy {
                min_raw_files: 1,
                ..LlmFactCompactionPolicy::default()
            },
        )
        .expect("compact scope");
        assert_eq!(stats.partitions_skipped_current, 0);
        assert_eq!(stats.partitions_compacted, 1);
        assert!(compaction_manifest_path(&partition).exists());
    }

    #[test]
    fn explicit_closed_day_policy_still_skips_the_open_utc_partition() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        let partition = workspace
            .analytics_llm_calls_root("owner", "default")
            .join(format!("dt={}", Utc::now().format("%Y-%m-%d")));
        write_canonical_fixture(
            &partition.join("part-call_fact-a-r1.parquet"),
            "call_fact",
            "owner",
            "default",
            "call:a:r1",
            1,
        );
        let stats = compact_scope(
            &workspace,
            &scope,
            LlmFactCompactionPolicy {
                min_raw_files: 1,
                include_current_partition: false,
            },
        )
        .expect("closed-day-only compact scope");
        assert_eq!(stats.partitions_skipped_current, 1);
        assert_eq!(stats.partitions_compacted, 0);
    }

    #[test]
    fn schema_one_manifest_is_a_safe_rolling_prefix_after_upgrade() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        let partition = workspace
            .analytics_llm_calls_root("owner", "default")
            .join("dt=2026-07-20");
        let first = partition.join("part-call_fact-a-r1.parquet");
        write_canonical_fixture(&first, "call_fact", "owner", "default", "call:a:r1", 1);
        compact_scope(
            &workspace,
            &scope,
            LlmFactCompactionPolicy {
                min_raw_files: 1,
                include_current_partition: true,
            },
        )
        .expect("initial compact");
        let manifest_path = compaction_manifest_path(&partition);
        let mut manifest: LlmFactCompactionManifest = workspace
            .read_json_path_sync(&manifest_path)
            .expect("manifest");
        manifest.schema_version = LLM_FACT_COMPACTION_LEGACY_MANIFEST_SCHEMA_VERSION;
        workspace
            .write_json_atomic_path_sync(&manifest_path, &manifest)
            .expect("legacy manifest");

        let tail = partition.join("part-call_fact-0-r1.parquet");
        write_canonical_fixture(&tail, "call_fact", "owner", "default", "call:b:r1", 2);
        let governed = governed_dataset_sources(&workspace, &scope, LlmCanonicalDataset::Calls)
            .expect("legacy rolling sources");
        assert!(governed[0].using_compaction);
        assert_eq!(governed[0].files, vec![compacted_file(&partition), tail]);
    }

    #[test]
    fn current_only_compaction_does_not_rescan_or_roll_closed_partitions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        let closed = workspace
            .analytics_llm_calls_root("owner", "default")
            .join("dt=2026-07-20");
        let current = workspace
            .analytics_llm_calls_root("owner", "default")
            .join(format!("dt={}", Utc::now().format("%Y-%m-%d")));
        write_canonical_fixture(
            &closed.join("part-call_fact-a-r1.parquet"),
            "call_fact",
            "owner",
            "default",
            "call:closed:r1",
            1,
        );
        write_canonical_fixture(
            &current.join("part-call_fact-b-r1.parquet"),
            "call_fact",
            "owner",
            "default",
            "call:current:r1",
            2,
        );
        let stats = compact_current_scope(&workspace, &scope, 1).expect("hot compact");
        assert_eq!(stats.partitions_scanned, 1);
        assert_eq!(stats.partitions_compacted, 1);
        assert!(!compaction_manifest_path(&closed).exists());
        assert!(compaction_manifest_path(&current).exists());
    }
}

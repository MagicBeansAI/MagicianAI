//! Authoritative Parquet materialization for journaled LLM facts.
//!
//! Raw lifecycle revisions are immutable. Every stable record revision maps to
//! one deterministic Parquet object, making the materializer idempotent across
//! the crash window between file publication and journal-watermark advancement.
//! Phase 2D owns compaction and latest-revision views; this module preserves the
//! complete content-free source facts needed to build them.

use std::{collections::HashSet, path::Path, sync::Arc};

use anyhow::{anyhow, Context};
use chrono::{DateTime, Utc};
use duckdb::Connection;
use magicllm::{LlmScope, LlmTraceContext};
use serde::{Deserialize, Serialize};

use super::{
    duckdb_safety::{analytics_duckdb_guard, configure_analytics_connection_checked},
    llm_scoped_path::{ensure_real_scoped_directory_chain, ensure_regular_file_or_missing},
    llm_trace_journal::{
        LlmTraceBatchMaterializer, LlmTraceDurablePipeline, LlmTraceJournalConfig,
        LlmTraceJournalError, LlmTraceJournalNamespace,
    },
    llm_trace_recorder::{
        LlmCallCompleted, LlmCallIoRecord, LlmCallStarted, LlmCaptureFact, LlmContentAccessAudit,
        LlmContentTombstone, LlmContextBlockRecord, LlmImmediateValidationFact, LlmPricingFact,
        LlmProviderAttemptEvent, LlmTimingFact, LlmTokenUsageFact, LlmToolLineageRecord,
        LlmTraceJournalEnvelope, LlmTraceRecord,
    },
};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

pub const MATERIALIZED_FACT_SCHEMA_VERSION: u16 = 1;
pub const DATASET_WATERMARK_SCHEMA_VERSION: u16 = 1;
pub const DATASET_WATERMARK_FILE: &str = "_materialization-watermark.json";
const MAX_RESTRICTED_PARTITIONS_FOR_IDEMPOTENCY_LOOKUP: usize = 4_096;

/// Start the canonical journal/materializer stack without changing any runtime
/// producer. Phase 2F calls this factory when activation and reconciliation
/// gates are satisfied.
pub fn build_canonical_llm_trace_pipeline(
    workspace: ArtifactV2Workspace,
    config: LlmTraceJournalConfig,
    recovery_scopes: Vec<LlmScope>,
) -> Result<LlmTraceDurablePipeline, LlmTraceJournalError> {
    let materializer = Arc::new(ParquetLlmTraceMaterializer::new(workspace.clone()));
    LlmTraceDurablePipeline::start(workspace, config, materializer, recovery_scopes)
}

pub fn build_restricted_llm_trace_pipeline(
    workspace: ArtifactV2Workspace,
    mut config: LlmTraceJournalConfig,
    recovery_scopes: Vec<LlmScope>,
) -> Result<LlmTraceDurablePipeline, LlmTraceJournalError> {
    config.namespace = LlmTraceJournalNamespace::RestrictedContent;
    let materializer = Arc::new(ParquetLlmTraceMaterializer::new(workspace.clone()));
    LlmTraceDurablePipeline::start(workspace, config, materializer, recovery_scopes)
}

#[derive(Clone)]
pub struct ParquetLlmTraceMaterializer {
    workspace: ArtifactV2Workspace,
}

impl ParquetLlmTraceMaterializer {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        Self { workspace }
    }

    fn validate_batch(
        &self,
        scope: &LlmScope,
        records: &[LlmTraceJournalEnvelope],
    ) -> Result<Vec<CanonicalLlmFactRow>, LlmTraceJournalError> {
        let mut keys = HashSet::new();
        let rows = records
            .iter()
            .map(|envelope| {
                envelope.verify()?;
                if envelope.record.scope() != scope {
                    return Err(LlmTraceJournalError::Materialization(format!(
                        "journal record {} belongs to {}/{}, not {}/{}",
                        envelope.key.idempotency_key(),
                        envelope.record.scope().principal,
                        envelope.record.scope().workspace,
                        scope.principal,
                        scope.workspace
                    )));
                }
                if !keys.insert(envelope.key.clone()) {
                    return Err(LlmTraceJournalError::Materialization(format!(
                        "materialization batch repeats stable revision {}",
                        envelope.key.idempotency_key()
                    )));
                }
                CanonicalLlmFactRow::from_envelope(envelope)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if rows
            .windows(2)
            .any(|pair| pair[0].journal_sequence >= pair[1].journal_sequence)
        {
            return Err(LlmTraceJournalError::Materialization(
                "materialization batch journal sequences must be strictly increasing".to_string(),
            ));
        }
        for row in &rows {
            partition_date(row.occurred_at_ms).map_err(|error| {
                LlmTraceJournalError::Materialization(format!(
                    "invalid partition timestamp for {}: {error}",
                    row.idempotency_key
                ))
            })?;
            LlmFactDataset::from_record_kind(&row.record_kind)
                .map_err(|error| LlmTraceJournalError::Materialization(error.to_string()))?;
        }
        Ok(rows)
    }

    fn materialize_rows(
        &self,
        scope: &LlmScope,
        rows: &[CanonicalLlmFactRow],
    ) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let _duckdb_guard = analytics_duckdb_guard();
        let connection = Connection::open_in_memory()
            .context("opening in-memory DuckDB for canonical LLM facts")?;
        configure_analytics_connection_checked(&connection, "llm_trace_materialization")
            .context("configuring canonical LLM fact materializer")?;
        for row in rows {
            self.materialize_row(scope, row, &connection)?;
        }
        Ok(())
    }

    fn materialize_row(
        &self,
        scope: &LlmScope,
        row: &CanonicalLlmFactRow,
        connection: &Connection,
    ) -> anyhow::Result<()> {
        let date = partition_date(row.occurred_at_ms)?;
        let dataset = LlmFactDataset::from_record_kind(&row.record_kind)?;
        let dataset_root = dataset.root(&self.workspace, scope);
        let partition = dataset_root.join(format!("dt={date}"));
        ensure_real_scoped_directory_chain(self.workspace.base_root(), &partition)?;
        std::fs::create_dir_all(&partition)
            .with_context(|| format!("creating LLM fact partition {}", partition.display()))?;
        ensure_real_scoped_directory_chain(self.workspace.base_root(), &partition)?;
        let file_name = deterministic_file_name(row);
        let final_path = partition.join(&file_name);
        let existing_path = if dataset.is_restricted() {
            find_restricted_object_by_stable_name(&dataset_root, &file_name)?
        } else if canonical_file_exists(&final_path)? {
            Some(final_path.clone())
        } else {
            None
        };
        if let Some(existing_path) = existing_path {
            verify_existing_row(connection, &existing_path, row)?;
            self.advance_dataset_watermark(dataset, &dataset_root, row)?;
            return Ok(());
        }

        let nonce = ulid::Ulid::new();
        let json_path = partition.join(format!(".materialize-{nonce}.json"));
        let parquet_path = partition.join(format!(".materialize-{nonce}.parquet.tmp"));
        let result = (|| -> anyhow::Result<()> {
            let mut json = serde_json::to_vec(row).context("serializing canonical LLM fact")?;
            json.push(b'\n');
            write_synced(&json_path, &json)?;

            let columns = dataset.materialized_columns();
            let projection = columns
                .iter()
                .map(|(name, ty)| format!("CAST(\"{name}\" AS {ty}) AS \"{name}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let json_sql = sql_path(&json_path);
            let parquet_sql = sql_path(&parquet_path);
            let copy = format!(
                "COPY (SELECT {projection} FROM read_json_auto('{json_sql}', format = 'newline_delimited')) \
                 TO '{parquet_sql}' (FORMAT PARQUET, COMPRESSION 'zstd');"
            );
            connection.execute_batch(&copy).with_context(|| {
                format!("writing canonical LLM fact {}", parquet_path.display())
            })?;
            sync_file(&parquet_path)?;

            if canonical_file_exists(&final_path)? {
                verify_existing_row(connection, &final_path, row)?;
                std::fs::remove_file(&parquet_path).with_context(|| {
                    format!("removing redundant LLM fact {}", parquet_path.display())
                })?;
            } else {
                std::fs::rename(&parquet_path, &final_path).with_context(|| {
                    format!(
                        "publishing canonical LLM fact {} -> {}",
                        parquet_path.display(),
                        final_path.display()
                    )
                })?;
                sync_directory(&partition)?;
                verify_existing_row(connection, &final_path, row)?;
                crate::magician_v2::dataset_owners::publish_written_parquet(&final_path)
                    .with_context(|| {
                        format!(
                            "publishing canonical LLM fact through DatasetAccess {}",
                            final_path.display()
                        )
                    })?;
            }
            Ok(())
        })();
        let _ = std::fs::remove_file(&json_path);
        if result.is_err() {
            let _ = std::fs::remove_file(&parquet_path);
        }
        result?;
        self.advance_dataset_watermark(dataset, &dataset_root, row)
    }

    fn advance_dataset_watermark(
        &self,
        dataset: LlmFactDataset,
        dataset_root: &Path,
        row: &CanonicalLlmFactRow,
    ) -> anyhow::Result<()> {
        let path = dataset_root.join(DATASET_WATERMARK_FILE);
        ensure_real_scoped_directory_chain(self.workspace.base_root(), dataset_root)?;
        let watermark_exists = ensure_regular_file_or_missing(&path)?;
        let previous = if !watermark_exists {
            None
        } else {
            match self
                .workspace
                .read_json_path_sync::<LlmDatasetMaterializationWatermark, _>(&path)
            {
                Ok(watermark) => Some(watermark),
                Err(error) => {
                    return Err(anyhow::Error::new(error).context("reading LLM dataset watermark"));
                },
            }
        };

        if let Some(previous) = previous.as_ref() {
            previous.validate(dataset)?;
            if row.journal_sequence < previous.committed_journal_sequence {
                // Interleaved datasets can replay an older revision after a
                // later revision of this dataset was already published. The
                // immutable object was verified above; never move state back.
                return Ok(());
            }
            if row.journal_sequence == previous.committed_journal_sequence {
                if previous.committed_checksum != row.payload_checksum {
                    return Err(anyhow!(
                        "LLM dataset {} watermark checksum conflicts at journal sequence {}",
                        dataset.as_str(),
                        row.journal_sequence
                    ));
                }
                return Ok(());
            }
        }

        let watermark = LlmDatasetMaterializationWatermark {
            schema_version: DATASET_WATERMARK_SCHEMA_VERSION,
            dataset: dataset.as_str().to_string(),
            materialized_schema_version: MATERIALIZED_FACT_SCHEMA_VERSION,
            committed_journal_sequence: row.journal_sequence,
            committed_checksum: row.payload_checksum.clone(),
            published_revision_count: previous
                .as_ref()
                .map_or(1, |value| value.published_revision_count.saturating_add(1)),
            updated_at_ms: Utc::now().timestamp_millis(),
        };
        self.workspace
            .write_json_atomic_path_sync(path, &watermark)
            .context("advancing LLM dataset watermark")
    }
}

/// Restricted replay journals intentionally discard committed payloads. The
/// deterministic object is therefore the durable idempotency authority. Look
/// across bounded UTC partitions so a repeated stable revision with a changed
/// timestamp cannot escape conflict detection by selecting a different day.
fn find_restricted_object_by_stable_name(
    dataset_root: &Path,
    file_name: &str,
) -> anyhow::Result<Option<std::path::PathBuf>> {
    let entries = match std::fs::read_dir(dataset_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("reading restricted LLM dataset root"),
    };
    let mut partitions_seen = 0_usize;
    let mut found = None;
    for entry in entries {
        let entry = entry.context("reading restricted LLM dataset entry")?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("dt=") {
            continue;
        }
        let metadata = std::fs::symlink_metadata(entry.path())
            .context("inspecting restricted LLM dataset partition")?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(anyhow!(
                "restricted LLM dataset partition must be a real directory: {}",
                entry.path().display()
            ));
        }
        partitions_seen = partitions_seen.saturating_add(1);
        if partitions_seen > MAX_RESTRICTED_PARTITIONS_FOR_IDEMPOTENCY_LOOKUP {
            return Err(anyhow!(
                "restricted LLM dataset partition budget exceeded during idempotency lookup"
            ));
        }
        let candidate = entry.path().join(file_name);
        if canonical_file_exists(&candidate)? {
            if found.is_some() {
                return Err(anyhow!(
                    "restricted LLM stable revision exists in multiple partitions"
                ));
            }
            found = Some(candidate);
        }
    }
    Ok(found)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LlmFactDataset {
    Calls,
    ProviderAttempts,
    ToolCalls,
    CaptureGaps,
    CallIo,
    ContextBlocks,
    ContentTombstones,
    ContentAccessAudit,
}

impl LlmFactDataset {
    fn from_record_kind(record_kind: &str) -> anyhow::Result<Self> {
        match record_kind {
            "call_fact" => Ok(Self::Calls),
            "provider_attempt" => Ok(Self::ProviderAttempts),
            "tool_lineage" => Ok(Self::ToolCalls),
            "capture_gap" => Ok(Self::CaptureGaps),
            "call_io" => Ok(Self::CallIo),
            "context_block" => Ok(Self::ContextBlocks),
            "content_tombstone" => Ok(Self::ContentTombstones),
            "content_access_audit" => Ok(Self::ContentAccessAudit),
            other => Err(anyhow!("unsupported canonical LLM fact dataset {other}")),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Calls => "llm_calls",
            Self::ProviderAttempts => "llm_provider_attempts",
            Self::ToolCalls => "llm_tool_calls",
            Self::CaptureGaps => "llm_capture_gaps",
            Self::CallIo => "llm_call_io",
            Self::ContextBlocks => "llm_context_blocks",
            Self::ContentTombstones => "llm_content_tombstones",
            Self::ContentAccessAudit => "llm_content_access_audit",
        }
    }

    fn root(self, workspace: &ArtifactV2Workspace, scope: &LlmScope) -> std::path::PathBuf {
        match self {
            Self::Calls => workspace.analytics_llm_calls_root(&scope.principal, &scope.workspace),
            Self::ProviderAttempts => {
                workspace.analytics_llm_provider_attempts_root(&scope.principal, &scope.workspace)
            },
            Self::ToolCalls => {
                workspace.analytics_llm_tool_calls_root(&scope.principal, &scope.workspace)
            },
            Self::CaptureGaps => {
                workspace.analytics_llm_capture_gaps_root(&scope.principal, &scope.workspace)
            },
            Self::CallIo => {
                workspace.analytics_llm_call_io_root(&scope.principal, &scope.workspace)
            },
            Self::ContextBlocks => {
                workspace.analytics_llm_context_blocks_root(&scope.principal, &scope.workspace)
            },
            Self::ContentTombstones => {
                workspace.analytics_llm_content_tombstones_root(&scope.principal, &scope.workspace)
            },
            Self::ContentAccessAudit => workspace
                .analytics_llm_content_access_audit_root(&scope.principal, &scope.workspace),
        }
    }

    fn materialized_columns(self) -> Vec<(&'static str, &'static str)> {
        let mut columns = FACT_COLUMNS.to_vec();
        if matches!(
            self,
            Self::CallIo | Self::ContextBlocks | Self::ContentTombstones | Self::ContentAccessAudit
        ) {
            columns.extend_from_slice(RESTRICTED_CONTENT_COLUMNS);
        }
        columns
    }

    const fn is_restricted(self) -> bool {
        matches!(
            self,
            Self::CallIo | Self::ContextBlocks | Self::ContentTombstones | Self::ContentAccessAudit
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmDatasetMaterializationWatermark {
    pub schema_version: u16,
    pub dataset: String,
    pub materialized_schema_version: u16,
    pub committed_journal_sequence: u64,
    pub committed_checksum: String,
    pub published_revision_count: u64,
    pub updated_at_ms: i64,
}

impl LlmDatasetMaterializationWatermark {
    fn validate(&self, expected_dataset: LlmFactDataset) -> anyhow::Result<()> {
        if self.schema_version != DATASET_WATERMARK_SCHEMA_VERSION
            || self.materialized_schema_version != MATERIALIZED_FACT_SCHEMA_VERSION
            || self.dataset != expected_dataset.as_str()
            || self.committed_journal_sequence == 0
            || !is_lower_hex_64(&self.committed_checksum)
            || self.published_revision_count == 0
            || self.updated_at_ms <= 0
        {
            return Err(anyhow!(
                "invalid LLM dataset {} materialization watermark",
                expected_dataset.as_str()
            ));
        }
        Ok(())
    }
}

fn is_lower_hex_64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl LlmTraceBatchMaterializer for ParquetLlmTraceMaterializer {
    fn materialize(
        &self,
        scope: &LlmScope,
        records: &[LlmTraceJournalEnvelope],
    ) -> Result<(), LlmTraceJournalError> {
        // Validate the complete batch before publishing its first file. Storage
        // failures may still leave a valid prefix, which deterministic paths
        // make safe to verify and continue on journal replay.
        let rows = self.validate_batch(scope, records)?;
        self.materialize_rows(scope, &rows)
            .map_err(|error| LlmTraceJournalError::Materialization(format!("{error:#}")))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct CanonicalLlmFactRow {
    materialized_schema_version: u16,
    fact_schema_version: u16,
    journal_schema_version: u16,
    journal_sequence: u64,
    record_kind: String,
    stable_id: String,
    record_revision: u32,
    lifecycle_phase: String,
    idempotency_key: String,
    payload_checksum: String,
    occurred_at_ms: i64,
    observed_at_ms: i64,
    timestamp_ms: i64,
    principal: String,
    workspace: String,
    trace_id: Option<String>,
    llm_call_id: Option<String>,
    provider_attempt_id: Option<String>,
    provider_attempt_index: Option<u32>,
    dispatch_job_id: Option<String>,
    parent_call_id: Option<String>,
    parent_relation: Option<String>,
    retry_group_id: Option<String>,
    route_decision_id: Option<String>,
    scope_resolution: Option<String>,
    task_id: Option<String>,
    root_execution_id: Option<String>,
    execution_id: Option<String>,
    model_tool_call_id: Option<String>,
    tool_execution_id: Option<String>,
    branch_id: Option<String>,
    plan_id: Option<String>,
    step_id: Option<String>,
    iteration_id: Option<String>,
    #[serde(default)]
    prompt_projection_mode: Option<String>,
    chat_session_id: Option<String>,
    chat_turn_id: Option<String>,
    user_message_id: Option<String>,
    workload_class: Option<String>,
    call_role: Option<String>,
    operation: String,
    operation_family: Option<String>,
    capability: Option<String>,
    tool_name: Option<String>,
    tool_family: Option<String>,
    tool_lineage_stage: Option<String>,
    tool_lineage_stage_index: Option<u32>,
    arguments_fingerprint: Option<String>,
    result_ref: Option<String>,
    canonical_event_ref: Option<String>,
    related_execution_ids_json: Option<String>,
    consumed_by_call_id: Option<String>,
    tool_name_known: Option<bool>,
    tool_arguments_parsed: Option<bool>,
    tool_schema_matched: Option<bool>,
    tool_policy_allowed: Option<bool>,
    tool_approval_required: Option<bool>,
    tool_approval_obtained: Option<bool>,
    tool_transport_ran: Option<bool>,
    tool_reported_success: Option<bool>,
    tool_result_validation_success: Option<bool>,
    tool_outcome: Option<String>,
    tool_failure_owner: Option<String>,
    tool_failure_code: Option<String>,
    tool_side_effect_state: Option<String>,
    tool_branch_state: Option<String>,
    on_successful_path: Option<bool>,
    same_tool_arguments_count: Option<u32>,
    observation_action_cycle_count: Option<u32>,
    recovered_after_failure: Option<bool>,
    linkage_gap: Option<String>,
    priority_lane: Option<String>,
    source_surface: Option<String>,
    origin_channel: Option<String>,
    requested_profile: Option<String>,
    selected_profile: Option<String>,
    effective_profile: Option<String>,
    profile: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    model_revision: Option<String>,
    call_terminal_state: Option<String>,
    attempt_terminal_state: Option<String>,
    transport_success: Option<bool>,
    success: Option<bool>,
    provider_attempt_count: Option<u32>,
    provider_response_id: Option<String>,
    response_kind: Option<String>,
    error_class: Option<String>,
    error_code: Option<String>,
    error: Option<String>,
    finish_reason: Option<String>,
    refusal: Option<bool>,
    truncated: Option<bool>,
    created_at_ms: Option<i64>,
    submitted_at_ms: Option<i64>,
    started_at_ms: Option<i64>,
    first_token_at_ms: Option<i64>,
    completed_at_ms: Option<i64>,
    queue_wait_ms: Option<u64>,
    local_prep_ms: Option<u64>,
    provider_execution_ms: Option<u64>,
    ttft_ms: Option<u64>,
    generation_after_ttft_ms: Option<u64>,
    parse_ms: Option<u64>,
    validation_ms: Option<u64>,
    latency_ms: Option<u64>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
    cache_creation_tokens: Option<u64>,
    audio_input_tokens: Option<u64>,
    audio_output_tokens: Option<u64>,
    audio_cached_tokens: Option<u64>,
    total_tokens: Option<u64>,
    pricing_version: Option<String>,
    cost_source: Option<String>,
    input_cost_usd: Option<f64>,
    output_cost_usd: Option<f64>,
    reasoning_cost_usd: Option<f64>,
    cache_cost_usd: Option<f64>,
    cost_usd: Option<f64>,
    response_present: Option<bool>,
    parse_attempted: Option<bool>,
    parse_success: Option<bool>,
    schema_validation_attempted: Option<bool>,
    schema_validation_success: Option<bool>,
    contract_validation_attempted: Option<bool>,
    contract_validation_success: Option<bool>,
    validation_error_class: Option<String>,
    discarded_before_use: Option<bool>,
    discard_reason: Option<String>,
    superseded_by_call_id: Option<String>,
    capture_mode: String,
    capture_status: String,
    training_eligible_at_capture: bool,
    training_exclusion_reason: Option<String>,
    gap_id: Option<String>,
    gap_reason: Option<String>,
    missing_record_count: Option<u64>,
    first_missing_at_ms: Option<i64>,
    last_missing_at_ms: Option<i64>,
    #[serde(default)]
    content_phase: Option<String>,
    #[serde(default)]
    content_target_kind: Option<String>,
    #[serde(default)]
    content_target_id: Option<String>,
    #[serde(default)]
    context_block_id: Option<String>,
    #[serde(default)]
    retention_class: Option<String>,
    #[serde(default)]
    redaction_version: Option<String>,
    #[serde(default)]
    redaction_count: Option<u32>,
    #[serde(default)]
    redaction_categories_json: Option<String>,
    #[serde(default)]
    original_bytes: Option<u64>,
    #[serde(default)]
    sanitized_bytes: Option<u64>,
    #[serde(default)]
    content_fingerprint: Option<String>,
    #[serde(default)]
    restricted_payload_json: Option<String>,
    #[serde(default)]
    deletion_reason: Option<String>,
    #[serde(default)]
    actor_id: Option<String>,
    #[serde(default)]
    audit_reason: Option<String>,
    #[serde(default)]
    audit_outcome: Option<String>,
    #[serde(default)]
    bytes_returned: Option<u64>,
}

impl CanonicalLlmFactRow {
    fn from_envelope(envelope: &LlmTraceJournalEnvelope) -> Result<Self, LlmTraceJournalError> {
        let mut row = Self::empty(envelope);
        match &envelope.record {
            LlmTraceRecord::CallStarted(record) => row.apply_call_started(record),
            LlmTraceRecord::ProviderAttempt(record) => row.apply_provider_attempt(record),
            LlmTraceRecord::CallCompleted(record) => row.apply_call_completed(record),
            LlmTraceRecord::CaptureGap(record) => {
                row.fact_schema_version = record.schema_version;
                row.occurred_at_ms = record.emitted_at_ms;
                row.observed_at_ms = record.emitted_at_ms;
                row.timestamp_ms = record.emitted_at_ms;
                row.principal.clone_from(&record.scope.principal);
                row.workspace.clone_from(&record.scope.workspace);
                row.llm_call_id.clone_from(&record.llm_call_id);
                row.operation.clone_from(&record.operation);
                row.capture_mode = "metadata".to_string();
                row.capture_status = "backpressure_degraded".to_string();
                row.training_eligible_at_capture = false;
                row.training_exclusion_reason = Some("capture_gap".to_string());
                row.gap_id = Some(record.gap_id.clone());
                row.gap_reason = Some(record.reason.clone());
                row.missing_record_count = Some(record.missing_record_count);
                row.first_missing_at_ms = Some(record.first_observed_at_ms);
                row.last_missing_at_ms = Some(record.last_observed_at_ms);
            },
            LlmTraceRecord::ToolLineage(record) => row.apply_tool_lineage(record)?,
            LlmTraceRecord::CallIo(record) => row.apply_call_io(record)?,
            LlmTraceRecord::ContextBlock(record) => row.apply_context_block(record)?,
            LlmTraceRecord::ContentTombstone(record) => row.apply_content_tombstone(record),
            LlmTraceRecord::ContentAccessAudit(record) => row.apply_content_access_audit(record),
        }
        Ok(row)
    }

    fn empty(envelope: &LlmTraceJournalEnvelope) -> Self {
        Self {
            materialized_schema_version: MATERIALIZED_FACT_SCHEMA_VERSION,
            fact_schema_version: 0,
            journal_schema_version: envelope.journal_schema_version,
            journal_sequence: envelope.sequence,
            record_kind: envelope.key.record_kind.as_str().to_string(),
            stable_id: envelope.key.stable_id.clone(),
            record_revision: envelope.key.revision,
            lifecycle_phase: lifecycle_phase(&envelope.record).to_string(),
            idempotency_key: envelope.key.idempotency_key(),
            payload_checksum: envelope.payload_checksum.clone(),
            occurred_at_ms: envelope.observed_at_ms,
            observed_at_ms: envelope.observed_at_ms,
            timestamp_ms: envelope.observed_at_ms,
            principal: String::new(),
            workspace: String::new(),
            trace_id: None,
            llm_call_id: None,
            provider_attempt_id: None,
            provider_attempt_index: None,
            dispatch_job_id: None,
            parent_call_id: None,
            parent_relation: None,
            retry_group_id: None,
            route_decision_id: None,
            scope_resolution: None,
            task_id: None,
            root_execution_id: None,
            execution_id: None,
            model_tool_call_id: None,
            tool_execution_id: None,
            branch_id: None,
            plan_id: None,
            step_id: None,
            iteration_id: None,
            prompt_projection_mode: None,
            chat_session_id: None,
            chat_turn_id: None,
            user_message_id: None,
            workload_class: None,
            call_role: None,
            operation: String::new(),
            operation_family: None,
            capability: None,
            tool_name: None,
            tool_family: None,
            tool_lineage_stage: None,
            tool_lineage_stage_index: None,
            arguments_fingerprint: None,
            result_ref: None,
            canonical_event_ref: None,
            related_execution_ids_json: None,
            consumed_by_call_id: None,
            tool_name_known: None,
            tool_arguments_parsed: None,
            tool_schema_matched: None,
            tool_policy_allowed: None,
            tool_approval_required: None,
            tool_approval_obtained: None,
            tool_transport_ran: None,
            tool_reported_success: None,
            tool_result_validation_success: None,
            tool_outcome: None,
            tool_failure_owner: None,
            tool_failure_code: None,
            tool_side_effect_state: None,
            tool_branch_state: None,
            on_successful_path: None,
            same_tool_arguments_count: None,
            observation_action_cycle_count: None,
            recovered_after_failure: None,
            linkage_gap: None,
            priority_lane: None,
            source_surface: None,
            origin_channel: None,
            requested_profile: None,
            selected_profile: None,
            effective_profile: None,
            profile: None,
            provider: None,
            model: None,
            model_revision: None,
            call_terminal_state: None,
            attempt_terminal_state: None,
            transport_success: None,
            success: None,
            provider_attempt_count: None,
            provider_response_id: None,
            response_kind: None,
            error_class: None,
            error_code: None,
            error: None,
            finish_reason: None,
            refusal: None,
            truncated: None,
            created_at_ms: None,
            submitted_at_ms: None,
            started_at_ms: None,
            first_token_at_ms: None,
            completed_at_ms: None,
            queue_wait_ms: None,
            local_prep_ms: None,
            provider_execution_ms: None,
            ttft_ms: None,
            generation_after_ttft_ms: None,
            parse_ms: None,
            validation_ms: None,
            latency_ms: None,
            input_tokens: None,
            output_tokens: None,
            reasoning_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            audio_input_tokens: None,
            audio_output_tokens: None,
            audio_cached_tokens: None,
            total_tokens: None,
            pricing_version: None,
            cost_source: None,
            input_cost_usd: None,
            output_cost_usd: None,
            reasoning_cost_usd: None,
            cache_cost_usd: None,
            cost_usd: None,
            response_present: None,
            parse_attempted: None,
            parse_success: None,
            schema_validation_attempted: None,
            schema_validation_success: None,
            contract_validation_attempted: None,
            contract_validation_success: None,
            validation_error_class: None,
            discarded_before_use: None,
            discard_reason: None,
            superseded_by_call_id: None,
            capture_mode: String::new(),
            capture_status: String::new(),
            training_eligible_at_capture: false,
            training_exclusion_reason: None,
            gap_id: None,
            gap_reason: None,
            missing_record_count: None,
            first_missing_at_ms: None,
            last_missing_at_ms: None,
            content_phase: None,
            content_target_kind: None,
            content_target_id: None,
            context_block_id: None,
            retention_class: None,
            redaction_version: None,
            redaction_count: None,
            redaction_categories_json: None,
            original_bytes: None,
            sanitized_bytes: None,
            content_fingerprint: None,
            restricted_payload_json: None,
            deletion_reason: None,
            actor_id: None,
            audit_reason: None,
            audit_outcome: None,
            bytes_returned: None,
        }
    }

    fn apply_context(&mut self, context: &LlmTraceContext) {
        self.principal.clone_from(&context.scope.principal);
        self.workspace.clone_from(&context.scope.workspace);
        self.trace_id = Some(context.trace_id.clone());
        self.llm_call_id = Some(context.llm_call_id.clone());
        self.parent_call_id.clone_from(&context.parent_call_id);
        self.parent_relation = context
            .parent_relation
            .map(|value| value.as_str().to_string());
        self.retry_group_id.clone_from(&context.retry_group_id);
        self.route_decision_id
            .clone_from(&context.route_decision_id);
        self.scope_resolution = Some(context.scope_resolution.as_str().to_string());
        // A journal record written before magicllm 0.2.35 can carry a chat
        // session id as its `task_id` (the dispatcher's scheduling key leaked
        // into the trace). A session is not a task; normalizing here keeps a
        // re-materialization of that journal from reintroducing the drift the
        // governed read refuses.
        let session_keyed = context.task_id.is_some() && context.task_id == context.chat_session_id;
        self.task_id = if session_keyed {
            None
        } else {
            context.task_id.clone()
        };
        self.root_execution_id
            .clone_from(&context.root_execution_id);
        self.execution_id.clone_from(&context.execution_id);
        self.plan_id.clone_from(&context.plan_id);
        self.step_id.clone_from(&context.step_id);
        self.iteration_id.clone_from(&context.iteration_id);
        self.chat_session_id.clone_from(&context.chat_session_id);
        self.chat_turn_id.clone_from(&context.chat_turn_id);
        self.user_message_id.clone_from(&context.user_message_id);
        self.workload_class = Some(context.workload_class.as_str().to_string());
        self.call_role = Some(context.call_role.as_str().to_string());
    }

    fn apply_call_started(&mut self, record: &LlmCallStarted) {
        self.fact_schema_version = record.schema_version;
        self.apply_context(&record.context);
        self.occurred_at_ms = record.occurred_at_ms;
        self.observed_at_ms = record.observed_at_ms;
        self.timestamp_ms = record.occurred_at_ms;
        self.created_at_ms = Some(record.occurred_at_ms);
        self.operation.clone_from(&record.operation);
        self.operation_family.clone_from(&record.operation_family);
        self.capability.clone_from(&record.capability);
        self.priority_lane.clone_from(&record.priority_lane);
        self.source_surface.clone_from(&record.source_surface);
        self.origin_channel.clone_from(&record.origin_channel);
        self.requested_profile.clone_from(&record.requested_profile);
        self.selected_profile.clone_from(&record.selected_profile);
        self.profile.clone_from(&record.selected_profile);
        self.prompt_projection_mode
            .clone_from(&record.prompt_projection_mode);
        self.apply_capture(&record.capture);
    }

    fn apply_provider_attempt(&mut self, record: &LlmProviderAttemptEvent) {
        self.fact_schema_version = record.schema_version;
        self.apply_context(&record.context);
        self.occurred_at_ms = record.occurred_at_ms;
        self.observed_at_ms = record.observed_at_ms;
        self.timestamp_ms = record.occurred_at_ms;
        self.operation.clone_from(&record.operation);
        self.dispatch_job_id.clone_from(&record.dispatch_job_id);
        self.provider_attempt_id = Some(record.provider_attempt_id.clone());
        self.provider_attempt_index = Some(record.provider_attempt_index);
        self.effective_profile.clone_from(&record.effective_profile);
        self.profile.clone_from(&record.effective_profile);
        self.provider = Some(record.provider.clone());
        self.model = Some(record.model.clone());
        self.model_revision.clone_from(&record.model_revision);
        self.attempt_terminal_state = record
            .terminal_state
            .as_ref()
            .map(|value| enum_string(value));
        self.transport_success = record.terminal_state.map(|value| {
            matches!(
                value,
                super::llm_trace_recorder::LlmAttemptTerminalState::Succeeded
            )
        });
        self.success = self.transport_success;
        self.error_class.clone_from(&record.error_class);
        self.error_code.clone_from(&record.error_code);
        self.error.clone_from(&record.error_class);
        self.finish_reason.clone_from(&record.finish_reason);
        self.refusal = record.refusal;
        self.truncated = record.truncated;
        self.apply_timing(&record.timing);
        self.apply_usage(&record.usage);
        self.apply_pricing(&record.pricing);
        self.apply_capture(&record.capture);
    }

    fn apply_call_completed(&mut self, record: &LlmCallCompleted) {
        self.fact_schema_version = record.schema_version;
        self.apply_context(&record.context);
        self.occurred_at_ms = record.occurred_at_ms;
        self.observed_at_ms = record.observed_at_ms;
        self.timestamp_ms = record.occurred_at_ms;
        self.operation.clone_from(&record.operation);
        self.dispatch_job_id.clone_from(&record.dispatch_job_id);
        self.call_terminal_state = Some(enum_string(&record.terminal_state));
        self.transport_success = Some(matches!(
            record.terminal_state,
            super::llm_trace_recorder::LlmCallTerminalState::Succeeded
        ));
        self.success = self.transport_success;
        self.provider_attempt_count = Some(record.provider_attempt_count);
        self.provider_response_id
            .clone_from(&record.provider_response_id);
        self.response_kind.clone_from(&record.response_kind);
        self.error_class.clone_from(&record.error_class);
        self.error_code.clone_from(&record.error_code);
        self.error.clone_from(&record.error_class);
        self.finish_reason.clone_from(&record.finish_reason);
        self.refusal = record.refusal;
        self.truncated = record.truncated;
        self.apply_timing(&record.timing);
        self.apply_usage(&record.usage);
        self.apply_pricing(&record.pricing);
        self.apply_validation(&record.validation);
        self.apply_capture(&record.capture);
    }

    fn apply_tool_lineage(
        &mut self,
        record: &LlmToolLineageRecord,
    ) -> Result<(), LlmTraceJournalError> {
        self.fact_schema_version = record.schema_version;
        self.apply_context(&record.context);
        self.occurred_at_ms = record.occurred_at_ms;
        self.observed_at_ms = record.observed_at_ms;
        self.timestamp_ms = record.occurred_at_ms;
        self.operation.clone_from(&record.operation);
        self.source_surface = Some(record.source_surface.clone());
        self.model_tool_call_id = Some(record.model_tool_call_id.clone());
        self.tool_execution_id = Some(record.tool_execution_id.clone());
        self.branch_id = Some(record.branch_id.clone());
        self.tool_name = Some(record.tool_name.clone());
        self.tool_family.clone_from(&record.tool_family);
        self.tool_lineage_stage = Some(enum_string(&record.stage));
        self.tool_lineage_stage_index = Some(record.stage_index);
        self.arguments_fingerprint
            .clone_from(&record.arguments_fingerprint);
        self.result_ref.clone_from(&record.result_ref);
        self.canonical_event_ref
            .clone_from(&record.canonical_event_ref);
        self.related_execution_ids_json = (!record.related_execution_ids.is_empty())
            .then(|| serde_json::to_string(&record.related_execution_ids))
            .transpose()?;
        self.consumed_by_call_id
            .clone_from(&record.consumed_by_call_id);
        self.tool_name_known = record.name_known;
        self.tool_arguments_parsed = record.arguments_parsed;
        self.tool_schema_matched = record.schema_matched;
        self.tool_policy_allowed = record.policy_allowed;
        self.tool_approval_required = record.approval_required;
        self.tool_approval_obtained = record.approval_obtained;
        self.tool_transport_ran = record.transport_ran;
        self.tool_reported_success = record.tool_reported_success;
        self.tool_result_validation_success = record.result_validation_success;
        self.tool_outcome = Some(enum_string(&record.outcome));
        self.tool_failure_owner = record.failure_owner.as_ref().map(enum_string);
        self.tool_failure_code.clone_from(&record.failure_code);
        self.tool_side_effect_state = Some(enum_string(&record.side_effect_state));
        self.tool_branch_state = Some(enum_string(&record.branch_state));
        self.on_successful_path = record.on_successful_path;
        self.same_tool_arguments_count = Some(record.same_tool_arguments_count);
        self.observation_action_cycle_count = Some(record.observation_action_cycle_count);
        self.recovered_after_failure = Some(record.recovered_after_failure);
        self.linkage_gap.clone_from(&record.linkage_gap);
        self.capture_mode = "metadata".to_string();
        self.capture_status = "complete".to_string();
        self.training_eligible_at_capture = false;
        self.training_exclusion_reason = Some("lineage_requires_outcome_maturity".to_string());
        Ok(())
    }

    fn apply_call_io(&mut self, record: &LlmCallIoRecord) -> Result<(), LlmTraceJournalError> {
        self.fact_schema_version = record.schema_version;
        self.apply_context(&record.context);
        self.occurred_at_ms = record.occurred_at_ms;
        self.observed_at_ms = record.observed_at_ms;
        self.timestamp_ms = record.occurred_at_ms;
        self.operation.clone_from(&record.operation);
        self.provider_attempt_index = record.provider_attempt_index;
        self.content_phase = Some(enum_string(&record.phase));
        self.retention_class = Some(record.retention_class.clone());
        self.redaction_version = Some(record.redaction.policy_version.clone());
        self.redaction_count = Some(record.redaction.redaction_count);
        self.redaction_categories_json = Some(serde_json::to_string(&record.redaction.categories)?);
        self.original_bytes = Some(record.original_bytes);
        self.sanitized_bytes = Some(record.sanitized_bytes);
        self.content_fingerprint = match record.phase {
            super::llm_trace_recorder::LlmCallIoPhase::LogicalRequest => {
                record.logical_request_fingerprint.clone()
            },
            super::llm_trace_recorder::LlmCallIoPhase::EffectiveRequest => {
                record.effective_request_fingerprint.clone()
            },
            super::llm_trace_recorder::LlmCallIoPhase::NormalizedResponse => {
                record.response_fingerprint.clone()
            },
        };
        self.restricted_payload_json = Some(serde_json::to_string(&record.payload)?);
        self.apply_capture(&record.capture);
        Ok(())
    }

    fn apply_context_block(
        &mut self,
        record: &LlmContextBlockRecord,
    ) -> Result<(), LlmTraceJournalError> {
        self.fact_schema_version = record.schema_version;
        self.apply_context(&record.context);
        self.occurred_at_ms = record.occurred_at_ms;
        self.observed_at_ms = record.observed_at_ms;
        self.timestamp_ms = record.occurred_at_ms;
        self.operation.clone_from(&record.operation);
        self.context_block_id = Some(record.context_block_id.clone());
        self.content_target_kind = Some("context_block".to_string());
        self.content_target_id = Some(record.context_block_id.clone());
        self.original_bytes = Some(record.original_chars);
        self.sanitized_bytes = Some(record.effective_chars);
        self.content_fingerprint = Some(record.effective_fingerprint.clone());
        // This dataset is content-free: the JSON contains provenance and a
        // restricted payload reference, never the selected block's text.
        self.restricted_payload_json = Some(serde_json::to_string(record)?);
        self.capture_mode = "metadata".to_string();
        self.capture_status = "complete".to_string();
        self.training_eligible_at_capture = false;
        self.training_exclusion_reason = Some("context_metadata_only".to_string());
        Ok(())
    }

    fn apply_content_tombstone(&mut self, record: &LlmContentTombstone) {
        self.fact_schema_version = record.schema_version;
        self.occurred_at_ms = record.occurred_at_ms;
        self.observed_at_ms = record.observed_at_ms;
        self.timestamp_ms = record.occurred_at_ms;
        self.principal.clone_from(&record.scope.principal);
        self.workspace.clone_from(&record.scope.workspace);
        self.operation = "content_deletion".to_string();
        self.content_target_kind = Some(record.target_kind.clone());
        self.content_target_id = Some(record.target_id.clone());
        self.deletion_reason = Some(record.reason.clone());
        self.capture_mode = "metadata".to_string();
        self.capture_status = "metadata_only".to_string();
        self.training_eligible_at_capture = false;
        self.training_exclusion_reason = Some("deleted".to_string());
    }

    fn apply_content_access_audit(&mut self, record: &LlmContentAccessAudit) {
        self.fact_schema_version = record.schema_version;
        self.occurred_at_ms = record.occurred_at_ms;
        self.observed_at_ms = record.observed_at_ms;
        self.timestamp_ms = record.occurred_at_ms;
        self.principal.clone_from(&record.scope.principal);
        self.workspace.clone_from(&record.scope.workspace);
        self.execution_id.clone_from(&record.execution_id);
        self.operation = "restricted_content_read".to_string();
        self.content_target_kind = Some(record.target_kind.clone());
        self.content_target_id = Some(record.target_id.clone());
        self.redaction_version = Some(record.redaction_version.clone());
        self.actor_id = Some(record.actor_id.clone());
        self.audit_reason = Some(record.reason.clone());
        self.audit_outcome = Some(record.outcome.clone());
        self.bytes_returned = Some(record.bytes_returned);
        self.capture_mode = "metadata".to_string();
        self.capture_status = "metadata_only".to_string();
        self.training_eligible_at_capture = false;
        self.training_exclusion_reason = Some("audit_record".to_string());
    }

    fn apply_timing(&mut self, timing: &LlmTimingFact) {
        self.created_at_ms = timing.created_at_ms;
        self.submitted_at_ms = timing.submitted_at_ms;
        self.started_at_ms = timing.started_at_ms;
        self.first_token_at_ms = timing.first_token_at_ms;
        self.completed_at_ms = timing.completed_at_ms;
        self.queue_wait_ms = timing.queue_wait_ms;
        self.local_prep_ms = timing.local_prep_ms;
        self.provider_execution_ms = timing.provider_execution_ms;
        self.ttft_ms = timing.ttft_ms;
        self.generation_after_ttft_ms = timing.generation_after_ttft_ms;
        self.parse_ms = timing.parse_ms;
        self.validation_ms = timing.validation_ms;
        self.latency_ms = timing.latency_ms;
    }

    fn apply_usage(&mut self, usage: &LlmTokenUsageFact) {
        self.input_tokens = usage.input_tokens;
        self.output_tokens = usage.output_tokens;
        self.reasoning_tokens = usage.reasoning_tokens;
        self.cache_read_tokens = usage.cache_read_tokens;
        self.cache_creation_tokens = usage.cache_creation_tokens;
        self.audio_input_tokens = usage.audio_input_tokens;
        self.audio_output_tokens = usage.audio_output_tokens;
        self.audio_cached_tokens = usage.audio_cached_tokens;
        self.total_tokens = usage.total_tokens;
    }

    fn apply_pricing(&mut self, pricing: &LlmPricingFact) {
        self.pricing_version.clone_from(&pricing.pricing_version);
        self.cost_source = pricing.cost_source.as_ref().map(enum_string);
        self.input_cost_usd = pricing.input_cost_usd;
        self.output_cost_usd = pricing.output_cost_usd;
        self.reasoning_cost_usd = pricing.reasoning_cost_usd;
        self.cache_cost_usd = pricing.cache_cost_usd;
        self.cost_usd = pricing.cost_usd;
    }

    fn apply_validation(&mut self, validation: &LlmImmediateValidationFact) {
        self.response_present = Some(validation.response_present);
        self.parse_attempted = Some(validation.parse_attempted);
        self.parse_success = validation.parse_success;
        self.schema_validation_attempted = Some(validation.schema_validation_attempted);
        self.schema_validation_success = validation.schema_validation_success;
        self.contract_validation_attempted = Some(validation.contract_validation_attempted);
        self.contract_validation_success = validation.contract_validation_success;
        self.validation_error_class
            .clone_from(&validation.validation_error_class);
        self.discarded_before_use = Some(validation.discarded_before_use);
        self.discard_reason.clone_from(&validation.discard_reason);
        self.superseded_by_call_id
            .clone_from(&validation.superseded_by_call_id);
    }

    fn apply_capture(&mut self, capture: &LlmCaptureFact) {
        self.capture_mode = enum_string(&capture.mode);
        self.capture_status = enum_string(&capture.status);
        self.training_eligible_at_capture = capture.training_eligible_at_capture;
        self.training_exclusion_reason
            .clone_from(&capture.training_exclusion_reason);
    }
}

fn lifecycle_phase(record: &LlmTraceRecord) -> &'static str {
    match record {
        LlmTraceRecord::CallStarted(_) => "started",
        LlmTraceRecord::CallCompleted(_) => "completed",
        LlmTraceRecord::ProviderAttempt(record) => match record.phase {
            super::llm_trace_recorder::LlmProviderAttemptPhase::Started => "started",
            super::llm_trace_recorder::LlmProviderAttemptPhase::FirstToken => "first_token",
            super::llm_trace_recorder::LlmProviderAttemptPhase::Completed => "completed",
        },
        LlmTraceRecord::CaptureGap(_) => "reported",
        LlmTraceRecord::ToolLineage(record) => match record.stage {
            super::llm_trace_recorder::LlmToolLineageStage::Proposed => "proposed",
            super::llm_trace_recorder::LlmToolLineageStage::NameValidated => "name_validated",
            super::llm_trace_recorder::LlmToolLineageStage::ArgumentsParsed => "arguments_parsed",
            super::llm_trace_recorder::LlmToolLineageStage::SchemaValidated => "schema_validated",
            super::llm_trace_recorder::LlmToolLineageStage::AuthorizationResolved => {
                "authorization_resolved"
            },
            super::llm_trace_recorder::LlmToolLineageStage::ApprovalResolved => "approval_resolved",
            super::llm_trace_recorder::LlmToolLineageStage::ExecutionStarted => "execution_started",
            super::llm_trace_recorder::LlmToolLineageStage::ExecutionFinished => {
                "execution_finished"
            },
            super::llm_trace_recorder::LlmToolLineageStage::ResultValidated => "result_validated",
            super::llm_trace_recorder::LlmToolLineageStage::ResultConsumed => "result_consumed",
            super::llm_trace_recorder::LlmToolLineageStage::BranchMaterialized => {
                "branch_materialized"
            },
            super::llm_trace_recorder::LlmToolLineageStage::RollbackStarted => "rollback_started",
            super::llm_trace_recorder::LlmToolLineageStage::RollbackFinished => "rollback_finished",
            super::llm_trace_recorder::LlmToolLineageStage::LinkageGap => "linkage_gap",
        },
        LlmTraceRecord::CallIo(record) => match record.phase {
            super::llm_trace_recorder::LlmCallIoPhase::LogicalRequest => "logical_request",
            super::llm_trace_recorder::LlmCallIoPhase::EffectiveRequest => "effective_request",
            super::llm_trace_recorder::LlmCallIoPhase::NormalizedResponse => "normalized_response",
        },
        LlmTraceRecord::ContextBlock(_) => "selected",
        LlmTraceRecord::ContentTombstone(_) => "tombstoned",
        LlmTraceRecord::ContentAccessAudit(_) => "audited",
    }
}

fn enum_string<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

fn deterministic_file_name(row: &CanonicalLlmFactRow) -> String {
    let digest = blake3::hash(row.idempotency_key.as_bytes()).to_hex();
    format!(
        "part-{}-{}-r{}.parquet",
        row.record_kind,
        &digest.as_str()[..32],
        row.record_revision
    )
}

fn partition_date(timestamp_ms: i64) -> anyhow::Result<String> {
    DateTime::<Utc>::from_timestamp_millis(timestamp_ms)
        .map(|timestamp| timestamp.format("%Y-%m-%d").to_string())
        .ok_or_else(|| anyhow!("LLM fact timestamp {timestamp_ms} is outside the supported range"))
}

fn verify_existing_row(
    connection: &Connection,
    path: &Path,
    expected: &CanonicalLlmFactRow,
) -> anyhow::Result<()> {
    let path_sql = sql_path(path);
    let dataset = LlmFactDataset::from_record_kind(&expected.record_kind)?;
    let columns = dataset.materialized_columns();
    let mut describe = connection
        .prepare(&format!(
            "DESCRIBE SELECT * FROM read_parquet('{path_sql}')"
        ))
        .with_context(|| format!("describing existing LLM fact {}", path.display()))?;
    let available = describe
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<HashSet<_>, _>>()?;
    let fields = columns
        .iter()
        .map(|(name, _)| {
            if available.contains(*name) {
                format!("{name} := \"{name}\"")
            } else if *name == "prompt_projection_mode" {
                // This additive nullable field intentionally reads as NULL
                // from schema-v1 materializations during crash replay.
                format!("{name} := NULL::VARCHAR")
            } else {
                // Preserve fail-closed behavior for every other missing fact
                // column; the query will name the corrupt/mismatched field.
                format!("{name} := \"{name}\"")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("SELECT to_json(struct_pack({fields})) FROM read_parquet('{path_sql}')");
    let mut statement = connection
        .prepare(&sql)
        .with_context(|| format!("verifying existing LLM fact {}", path.display()))?;
    let mut rows = statement
        .query([])
        .with_context(|| format!("reading existing LLM fact {}", path.display()))?;
    let Some(row) = rows
        .next()
        .with_context(|| format!("reading existing LLM fact {}", path.display()))?
    else {
        return Err(anyhow!(
            "existing canonical LLM fact {} contains no row",
            path.display()
        ));
    };
    let encoded: String = row
        .get(0)
        .with_context(|| format!("decoding existing LLM fact {}", path.display()))?;
    let mut actual: CanonicalLlmFactRow = serde_json::from_str(&encoded)
        .with_context(|| format!("parsing existing LLM fact {}", path.display()))?;
    // Restricted replay segments are intentionally pruned immediately after a
    // durable materialization watermark. If a producer repeats the same stable
    // revision after restart, its new journal sequence/checksum are transport
    // metadata; compare the persisted semantic row to detect a true conflict.
    if dataset.is_restricted() {
        actual.journal_sequence = expected.journal_sequence;
        actual
            .payload_checksum
            .clone_from(&expected.payload_checksum);
    }
    if rows
        .next()
        .with_context(|| format!("checking existing LLM fact {}", path.display()))?
        .is_some()
        || !canonical_rows_semantically_equal(&actual, expected)
    {
        let differing_fields = canonical_row_differing_fields(&actual, expected);
        return Err(anyhow!(
            "existing canonical LLM fact {} conflicts with {}; differing_fields={:?}",
            path.display(),
            expected.idempotency_key,
            differing_fields
        ));
    }
    Ok(())
}

fn canonical_rows_semantically_equal(
    actual: &CanonicalLlmFactRow,
    expected: &CanonicalLlmFactRow,
) -> bool {
    if ![
        (actual.input_cost_usd, expected.input_cost_usd),
        (actual.output_cost_usd, expected.output_cost_usd),
        (actual.reasoning_cost_usd, expected.reasoning_cost_usd),
        (actual.cache_cost_usd, expected.cache_cost_usd),
        (actual.cost_usd, expected.cost_usd),
    ]
    .into_iter()
    .all(|(left, right)| monetary_values_equivalent(left, right))
    {
        return false;
    }

    let mut actual_without_money = actual.clone();
    let mut expected_without_money = expected.clone();
    for row in [&mut actual_without_money, &mut expected_without_money] {
        row.input_cost_usd = None;
        row.output_cost_usd = None;
        row.reasoning_cost_usd = None;
        row.cache_cost_usd = None;
        row.cost_usd = None;
    }
    actual_without_money == expected_without_money
}

fn monetary_values_equivalent(actual: Option<f64>, expected: Option<f64>) -> bool {
    match (actual, expected) {
        (None, None) => true,
        (Some(left), Some(right)) if left == right => true,
        (Some(left), Some(right)) if left.is_finite() && right.is_finite() => {
            // Pricing validation guarantees non-negative values, whose IEEE-754
            // bit patterns are monotonic. One ULP covers the observed JSON
            // decimal round-trip without hiding a meaningful price mutation.
            left >= 0.0 && right >= 0.0 && left.to_bits().abs_diff(right.to_bits()) <= 1
        },
        _ => false,
    }
}

/// Return field names only: diagnostics must never copy restricted values into
/// ordinary logs, even when the conflicting row belongs to a content dataset.
fn canonical_row_differing_fields(
    actual: &CanonicalLlmFactRow,
    expected: &CanonicalLlmFactRow,
) -> Vec<String> {
    let Ok(serde_json::Value::Object(actual)) = serde_json::to_value(actual) else {
        return vec!["row_serialization".to_string()];
    };
    let Ok(serde_json::Value::Object(expected)) = serde_json::to_value(expected) else {
        return vec!["row_serialization".to_string()];
    };
    actual
        .keys()
        .chain(expected.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|key| actual.get(*key) != expected.get(*key))
        .cloned()
        .collect()
}

fn canonical_file_exists(path: &Path) -> anyhow::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(anyhow!(
            "canonical LLM fact destination must be a regular file: {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("inspecting canonical LLM fact {}", path.display()))
        },
    }
}

fn write_synced(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;

    let mut file = std::fs::File::create(path)
        .with_context(|| format!("creating temporary LLM fact {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("writing temporary LLM fact {}", path.display()))?;
    file.flush()
        .with_context(|| format!("flushing temporary LLM fact {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("syncing temporary LLM fact {}", path.display()))
}

fn sync_file(path: &Path) -> anyhow::Result<()> {
    std::fs::File::open(path)
        .with_context(|| format!("opening LLM fact {} for sync", path.display()))?
        .sync_all()
        .with_context(|| format!("syncing LLM fact {}", path.display()))
}

fn sync_directory(path: &Path) -> anyhow::Result<()> {
    std::fs::File::open(path)
        .with_context(|| format!("opening LLM fact directory {}", path.display()))?
        .sync_all()
        .with_context(|| format!("syncing LLM fact directory {}", path.display()))
}

fn sql_path(path: &Path) -> String {
    path.display().to_string().replace('\'', "''")
}

/// One explicit schema is shared by raw call revisions, attempt revisions and
/// gap facts. Separate dataset roots retain clear ownership while shared column
/// names make Phase 2D union/projection deterministic.
pub const FACT_COLUMNS: &[(&str, &str)] = &[
    ("materialized_schema_version", "INTEGER"),
    ("fact_schema_version", "INTEGER"),
    ("journal_schema_version", "INTEGER"),
    ("journal_sequence", "UBIGINT"),
    ("record_kind", "VARCHAR"),
    ("stable_id", "VARCHAR"),
    ("record_revision", "INTEGER"),
    ("lifecycle_phase", "VARCHAR"),
    ("idempotency_key", "VARCHAR"),
    ("payload_checksum", "VARCHAR"),
    ("occurred_at_ms", "BIGINT"),
    ("observed_at_ms", "BIGINT"),
    ("timestamp_ms", "BIGINT"),
    ("principal", "VARCHAR"),
    ("workspace", "VARCHAR"),
    ("trace_id", "VARCHAR"),
    ("llm_call_id", "VARCHAR"),
    ("provider_attempt_id", "VARCHAR"),
    ("provider_attempt_index", "INTEGER"),
    ("dispatch_job_id", "VARCHAR"),
    ("parent_call_id", "VARCHAR"),
    ("parent_relation", "VARCHAR"),
    ("retry_group_id", "VARCHAR"),
    ("route_decision_id", "VARCHAR"),
    ("scope_resolution", "VARCHAR"),
    ("task_id", "VARCHAR"),
    ("root_execution_id", "VARCHAR"),
    ("execution_id", "VARCHAR"),
    ("model_tool_call_id", "VARCHAR"),
    ("tool_execution_id", "VARCHAR"),
    ("branch_id", "VARCHAR"),
    ("plan_id", "VARCHAR"),
    ("step_id", "VARCHAR"),
    ("iteration_id", "VARCHAR"),
    ("prompt_projection_mode", "VARCHAR"),
    ("chat_session_id", "VARCHAR"),
    ("chat_turn_id", "VARCHAR"),
    ("user_message_id", "VARCHAR"),
    ("workload_class", "VARCHAR"),
    ("call_role", "VARCHAR"),
    ("operation", "VARCHAR"),
    ("operation_family", "VARCHAR"),
    ("capability", "VARCHAR"),
    ("tool_name", "VARCHAR"),
    ("tool_family", "VARCHAR"),
    ("tool_lineage_stage", "VARCHAR"),
    ("tool_lineage_stage_index", "INTEGER"),
    ("arguments_fingerprint", "VARCHAR"),
    ("result_ref", "VARCHAR"),
    ("canonical_event_ref", "VARCHAR"),
    ("related_execution_ids_json", "VARCHAR"),
    ("consumed_by_call_id", "VARCHAR"),
    ("tool_name_known", "BOOLEAN"),
    ("tool_arguments_parsed", "BOOLEAN"),
    ("tool_schema_matched", "BOOLEAN"),
    ("tool_policy_allowed", "BOOLEAN"),
    ("tool_approval_required", "BOOLEAN"),
    ("tool_approval_obtained", "BOOLEAN"),
    ("tool_transport_ran", "BOOLEAN"),
    ("tool_reported_success", "BOOLEAN"),
    ("tool_result_validation_success", "BOOLEAN"),
    ("tool_outcome", "VARCHAR"),
    ("tool_failure_owner", "VARCHAR"),
    ("tool_failure_code", "VARCHAR"),
    ("tool_side_effect_state", "VARCHAR"),
    ("tool_branch_state", "VARCHAR"),
    ("on_successful_path", "BOOLEAN"),
    ("same_tool_arguments_count", "INTEGER"),
    ("observation_action_cycle_count", "INTEGER"),
    ("recovered_after_failure", "BOOLEAN"),
    ("linkage_gap", "VARCHAR"),
    ("priority_lane", "VARCHAR"),
    ("source_surface", "VARCHAR"),
    ("origin_channel", "VARCHAR"),
    ("requested_profile", "VARCHAR"),
    ("selected_profile", "VARCHAR"),
    ("effective_profile", "VARCHAR"),
    ("profile", "VARCHAR"),
    ("provider", "VARCHAR"),
    ("model", "VARCHAR"),
    ("model_revision", "VARCHAR"),
    ("call_terminal_state", "VARCHAR"),
    ("attempt_terminal_state", "VARCHAR"),
    ("transport_success", "BOOLEAN"),
    ("success", "BOOLEAN"),
    ("provider_attempt_count", "INTEGER"),
    ("provider_response_id", "VARCHAR"),
    ("response_kind", "VARCHAR"),
    ("error_class", "VARCHAR"),
    ("error_code", "VARCHAR"),
    ("error", "VARCHAR"),
    ("finish_reason", "VARCHAR"),
    ("refusal", "BOOLEAN"),
    ("truncated", "BOOLEAN"),
    ("created_at_ms", "BIGINT"),
    ("submitted_at_ms", "BIGINT"),
    ("started_at_ms", "BIGINT"),
    ("first_token_at_ms", "BIGINT"),
    ("completed_at_ms", "BIGINT"),
    ("queue_wait_ms", "UBIGINT"),
    ("local_prep_ms", "UBIGINT"),
    ("provider_execution_ms", "UBIGINT"),
    ("ttft_ms", "UBIGINT"),
    ("generation_after_ttft_ms", "UBIGINT"),
    ("parse_ms", "UBIGINT"),
    ("validation_ms", "UBIGINT"),
    ("latency_ms", "UBIGINT"),
    ("input_tokens", "UBIGINT"),
    ("output_tokens", "UBIGINT"),
    ("reasoning_tokens", "UBIGINT"),
    ("cache_read_tokens", "UBIGINT"),
    ("cache_creation_tokens", "UBIGINT"),
    ("audio_input_tokens", "UBIGINT"),
    ("audio_output_tokens", "UBIGINT"),
    ("audio_cached_tokens", "UBIGINT"),
    ("total_tokens", "UBIGINT"),
    ("pricing_version", "VARCHAR"),
    ("cost_source", "VARCHAR"),
    ("input_cost_usd", "DOUBLE"),
    ("output_cost_usd", "DOUBLE"),
    ("reasoning_cost_usd", "DOUBLE"),
    ("cache_cost_usd", "DOUBLE"),
    ("cost_usd", "DOUBLE"),
    ("response_present", "BOOLEAN"),
    ("parse_attempted", "BOOLEAN"),
    ("parse_success", "BOOLEAN"),
    ("schema_validation_attempted", "BOOLEAN"),
    ("schema_validation_success", "BOOLEAN"),
    ("contract_validation_attempted", "BOOLEAN"),
    ("contract_validation_success", "BOOLEAN"),
    ("validation_error_class", "VARCHAR"),
    ("discarded_before_use", "BOOLEAN"),
    ("discard_reason", "VARCHAR"),
    ("superseded_by_call_id", "VARCHAR"),
    ("capture_mode", "VARCHAR"),
    ("capture_status", "VARCHAR"),
    ("training_eligible_at_capture", "BOOLEAN"),
    ("training_exclusion_reason", "VARCHAR"),
    ("gap_id", "VARCHAR"),
    ("gap_reason", "VARCHAR"),
    ("missing_record_count", "UBIGINT"),
    ("first_missing_at_ms", "BIGINT"),
    ("last_missing_at_ms", "BIGINT"),
];

/// Restricted/materializer-only columns. They are intentionally absent from
/// `FACT_COLUMNS`, the stable fact registry, and arbitrary fact SQL.
pub const RESTRICTED_CONTENT_COLUMNS: &[(&str, &str)] = &[
    ("content_phase", "VARCHAR"),
    ("content_target_kind", "VARCHAR"),
    ("content_target_id", "VARCHAR"),
    ("context_block_id", "VARCHAR"),
    ("retention_class", "VARCHAR"),
    ("redaction_version", "VARCHAR"),
    ("redaction_count", "INTEGER"),
    ("redaction_categories_json", "VARCHAR"),
    ("original_bytes", "UBIGINT"),
    ("sanitized_bytes", "UBIGINT"),
    ("content_fingerprint", "VARCHAR"),
    ("restricted_payload_json", "VARCHAR"),
    ("deletion_reason", "VARCHAR"),
    ("actor_id", "VARCHAR"),
    ("audit_reason", "VARCHAR"),
    ("audit_outcome", "VARCHAR"),
    ("bytes_returned", "UBIGINT"),
];

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::BTreeSet;

    use magicllm::{LlmTraceContext, LlmWorkloadClass};
    use tempfile::TempDir;

    use super::*;
    use crate::magician_v2::analytics::{
        llm_trace_journal::LlmTraceJournalStore,
        llm_trace_recorder::{
            LlmAttemptTerminalState, LlmCallIoPhase, LlmCallIoRecord, LlmCallTerminalState,
            LlmCaptureGap, LlmCaptureMode, LlmCaptureStatus, LlmCostSource,
            LlmProviderAttemptPhase, LlmRedactionReport, LlmToolBranchState, LlmToolLineageOutcome,
            LlmToolLineageRecord, LlmToolLineageStage, LlmToolSideEffectState,
            LLM_RESTRICTED_CONTENT_SCHEMA_VERSION, LLM_TRACE_FACT_SCHEMA_VERSION,
        },
    };

    const DAY_ONE: i64 = 1_700_006_400_000;
    const DAY_TWO: i64 = DAY_ONE + 86_400_000;

    fn fixture() -> (TempDir, ArtifactV2Workspace, LlmScope) {
        let temp = crate::magician_v2::analytics::llm_trace_journal::canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("principal", "workspace");
        (temp, workspace, scope)
    }

    fn context(scope: &LlmScope) -> LlmTraceContext {
        let mut context = LlmTraceContext::new(scope.clone(), LlmWorkloadClass::ForegroundChat);
        context.task_id = Some("task-1".to_string());
        context.root_execution_id = Some("root-execution-1".to_string());
        context.execution_id = Some("execution-1".to_string());
        context.chat_session_id = Some("chat-session-1".to_string());
        context.chat_turn_id = Some("chat-turn-1".to_string());
        context
    }

    fn call_started(context: LlmTraceContext, at_ms: i64) -> LlmTraceRecord {
        let mut record = LlmCallStarted::new(context, "chat", at_ms);
        record.observed_at_ms = at_ms + 1;
        record.operation_family = Some("interactive_chat".to_string());
        record.capability = Some("conversation".to_string());
        record.priority_lane = Some("interactive".to_string());
        record.source_surface = Some("chat".to_string());
        record.origin_channel = Some("web".to_string());
        record.requested_profile = Some("adaptive-chat".to_string());
        record.selected_profile = Some("fast-chat".to_string());
        LlmTraceRecord::CallStarted(record)
    }

    fn attempt_completed(context: LlmTraceContext, at_ms: i64) -> LlmTraceRecord {
        let mut record = LlmProviderAttemptEvent::started(
            context,
            "chat",
            1,
            "openai",
            "gpt-5.6-terra",
            at_ms - 40,
        );
        record.phase = LlmProviderAttemptPhase::Completed;
        record.occurred_at_ms = at_ms;
        record.observed_at_ms = at_ms + 1;
        record.dispatch_job_id = Some("dispatch-1".to_string());
        record.effective_profile = Some("fast-chat".to_string());
        record.timing.created_at_ms = Some(at_ms - 55);
        record.timing.submitted_at_ms = Some(at_ms - 50);
        record.timing.first_token_at_ms = Some(at_ms - 20);
        record.timing.completed_at_ms = Some(at_ms);
        record.timing.queue_wait_ms = Some(5);
        record.timing.local_prep_ms = Some(10);
        record.timing.provider_execution_ms = Some(40);
        record.timing.ttft_ms = Some(20);
        record.timing.generation_after_ttft_ms = Some(20);
        record.timing.latency_ms = Some(55);
        record.terminal_state = Some(LlmAttemptTerminalState::Succeeded);
        record.finish_reason = Some("stop".to_string());
        record.refusal = Some(false);
        record.truncated = Some(false);
        record.usage = LlmTokenUsageFact {
            input_tokens: Some(100),
            output_tokens: Some(20),
            reasoning_tokens: Some(5),
            cache_read_tokens: Some(60),
            total_tokens: Some(120),
            ..LlmTokenUsageFact::default()
        };
        record.pricing = LlmPricingFact {
            pricing_version: Some("2026-07-22".to_string()),
            cost_source: Some(LlmCostSource::Computed),
            input_cost_usd: Some(0.0001),
            output_cost_usd: Some(0.0002),
            reasoning_cost_usd: Some(0.00005),
            cache_cost_usd: Some(0.00001),
            cost_usd: Some(0.00036),
        };
        LlmTraceRecord::ProviderAttempt(record)
    }

    fn call_completed(context: LlmTraceContext, at_ms: i64) -> LlmTraceRecord {
        LlmTraceRecord::CallCompleted(LlmCallCompleted {
            schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
            context,
            dispatch_job_id: Some("dispatch-1".to_string()),
            occurred_at_ms: at_ms,
            observed_at_ms: at_ms + 1,
            operation: "chat".to_string(),
            terminal_state: LlmCallTerminalState::Succeeded,
            provider_attempt_count: 1,
            provider_response_id: Some("response-1".to_string()),
            response_kind: Some("text".to_string()),
            error_class: None,
            error_code: None,
            finish_reason: Some("stop".to_string()),
            refusal: Some(false),
            truncated: Some(false),
            timing: LlmTimingFact {
                created_at_ms: Some(at_ms - 60),
                submitted_at_ms: Some(at_ms - 55),
                started_at_ms: Some(at_ms - 45),
                first_token_at_ms: Some(at_ms - 25),
                completed_at_ms: Some(at_ms),
                queue_wait_ms: Some(5),
                local_prep_ms: Some(10),
                provider_execution_ms: Some(40),
                ttft_ms: Some(20),
                generation_after_ttft_ms: Some(20),
                parse_ms: Some(2),
                validation_ms: Some(3),
                latency_ms: Some(60),
            },
            usage: LlmTokenUsageFact {
                input_tokens: Some(100),
                output_tokens: Some(20),
                reasoning_tokens: Some(5),
                cache_read_tokens: Some(60),
                total_tokens: Some(120),
                ..LlmTokenUsageFact::default()
            },
            pricing: LlmPricingFact {
                pricing_version: Some("2026-07-22".to_string()),
                cost_source: Some(LlmCostSource::Computed),
                cost_usd: Some(0.00036),
                ..LlmPricingFact::default()
            },
            validation: LlmImmediateValidationFact {
                response_present: true,
                parse_attempted: true,
                parse_success: Some(true),
                schema_validation_attempted: true,
                schema_validation_success: Some(true),
                contract_validation_attempted: true,
                contract_validation_success: Some(true),
                ..LlmImmediateValidationFact::default()
            },
            capture: LlmCaptureFact::default(),
        })
    }

    fn capture_gap(scope: &LlmScope, at_ms: i64) -> LlmTraceRecord {
        LlmTraceRecord::CaptureGap(LlmCaptureGap {
            schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
            gap_id: "gap-1".to_string(),
            scope: scope.clone(),
            llm_call_id: Some("call-1".to_string()),
            operation: "chat".to_string(),
            reason: "critical_buffer_saturated".to_string(),
            missing_record_count: 3,
            first_observed_at_ms: at_ms - 2,
            last_observed_at_ms: at_ms - 1,
            emitted_at_ms: at_ms,
        })
    }

    fn tool_lineage(
        trace_context: LlmTraceContext,
        stage: LlmToolLineageStage,
        stage_index: u32,
        at_ms: i64,
    ) -> LlmToolLineageRecord {
        LlmToolLineageRecord {
            schema_version: LLM_TRACE_FACT_SCHEMA_VERSION,
            tool_execution_id: format!("{}:tool:call_1", trace_context.llm_call_id),
            context: trace_context,
            model_tool_call_id: "call_1".to_string(),
            branch_id: "branch_1".to_string(),
            operation: "chat".to_string(),
            source_surface: "chat".to_string(),
            tool_name: "browser__click".to_string(),
            tool_family: Some("browser".to_string()),
            stage,
            stage_index,
            occurred_at_ms: at_ms,
            observed_at_ms: at_ms + 1,
            arguments_fingerprint: Some("a".repeat(64)),
            result_ref: None,
            canonical_event_ref: None,
            related_execution_ids: Vec::new(),
            consumed_by_call_id: None,
            name_known: None,
            arguments_parsed: None,
            schema_matched: None,
            policy_allowed: None,
            approval_required: None,
            approval_obtained: None,
            transport_ran: None,
            tool_reported_success: None,
            result_validation_success: None,
            outcome: LlmToolLineageOutcome::Pending,
            failure_owner: None,
            failure_code: None,
            side_effect_state: LlmToolSideEffectState::None,
            branch_state: LlmToolBranchState::Active,
            on_successful_path: None,
            same_tool_arguments_count: 1,
            observation_action_cycle_count: 0,
            recovered_after_failure: false,
            linkage_gap: None,
        }
    }

    fn sanitized_call_io(trace_context: LlmTraceContext, at_ms: i64) -> LlmTraceRecord {
        LlmTraceRecord::CallIo(LlmCallIoRecord {
            schema_version: LLM_RESTRICTED_CONTENT_SCHEMA_VERSION,
            context: trace_context,
            operation: "chat".to_string(),
            phase: LlmCallIoPhase::LogicalRequest,
            provider_attempt_index: None,
            occurred_at_ms: at_ms,
            observed_at_ms: at_ms + 1,
            capture: LlmCaptureFact {
                mode: LlmCaptureMode::Sanitized,
                status: LlmCaptureStatus::Complete,
                training_eligible_at_capture: false,
                training_exclusion_reason: Some("training_not_enabled".to_string()),
            },
            retention_class: "sanitized_30d".to_string(),
            redaction: LlmRedactionReport {
                policy_version: "llm-content-redaction-v1".to_string(),
                ..LlmRedactionReport::default()
            },
            logical_request_fingerprint: Some("a".repeat(64)),
            effective_request_fingerprint: None,
            response_fingerprint: None,
            original_bytes: 24,
            sanitized_bytes: 20,
            payload: serde_json::json!({
                "messages": [{"role": "user", "content": "safe fixture"}]
            }),
        })
    }

    fn envelope(sequence: u64, record: LlmTraceRecord) -> LlmTraceJournalEnvelope {
        LlmTraceJournalEnvelope::new(sequence, record).expect("valid envelope")
    }

    fn parquet_glob(root: &Path) -> String {
        crate::magician_v2::dataset_owners::family_read_glob(
            root,
            crate::magician_v2::dataset_owners::DatasetFamily::LlmCalls,
        )
    }

    fn parquet_count(root: &Path) -> i64 {
        let connection = Connection::open_in_memory().expect("DuckDB");
        connection
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM read_parquet('{}', union_by_name = true)",
                    parquet_glob(root)
                ),
                [],
                |row| row.get(0),
            )
            .expect("Parquet count")
    }

    #[test]
    fn canonical_dataset_projection_excludes_every_restricted_column() {
        let (_temp, _workspace, scope) = fixture();
        let row = CanonicalLlmFactRow::from_envelope(&envelope(
            1,
            call_completed(context(&scope), DAY_ONE),
        ))
        .expect("canonical row");
        let serialized = serde_json::to_value(row).expect("serialized row");
        let actual = serialized
            .as_object()
            .expect("row object")
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        let declared = FACT_COLUMNS
            .iter()
            .map(|(name, _)| (*name).to_string())
            .collect::<BTreeSet<_>>();
        let restricted = RESTRICTED_CONTENT_COLUMNS
            .iter()
            .map(|(name, _)| (*name).to_string())
            .collect::<BTreeSet<_>>();
        assert!(declared.is_subset(&actual));
        assert!(restricted.is_subset(&actual));
        assert!(declared.is_disjoint(&restricted));
        assert_eq!(
            LlmFactDataset::Calls
                .materialized_columns()
                .into_iter()
                .map(|(name, _)| name.to_string())
                .collect::<BTreeSet<_>>(),
            declared
        );
        assert_eq!(
            declared.len(),
            FACT_COLUMNS.len(),
            "duplicate schema column"
        );
        for forbidden in [
            "prompt",
            "response",
            "messages",
            "tool_arguments",
            "attachment",
            "transcript",
        ] {
            assert!(!declared.contains(forbidden));
        }
    }

    #[test]
    fn completed_call_maps_timing_pricing_validation_usage_and_capture_facts() {
        let (_temp, _workspace, scope) = fixture();
        let row = CanonicalLlmFactRow::from_envelope(&envelope(
            1,
            call_completed(context(&scope), DAY_ONE),
        ))
        .expect("canonical row");

        assert_eq!(row.record_kind, "call_fact");
        assert_eq!(row.lifecycle_phase, "completed");
        assert_eq!(row.call_terminal_state.as_deref(), Some("succeeded"));
        assert_eq!(row.queue_wait_ms, Some(5));
        assert_eq!(row.local_prep_ms, Some(10));
        assert_eq!(row.ttft_ms, Some(20));
        assert_eq!(row.reasoning_tokens, Some(5));
        assert_eq!(row.cache_read_tokens, Some(60));
        assert_eq!(row.cost_source.as_deref(), Some("computed"));
        assert_eq!(row.cost_usd, Some(0.00036));
        assert_eq!(row.contract_validation_success, Some(true));
        assert_eq!(row.capture_status, "metadata_only");
        assert!(!row.training_eligible_at_capture);
    }

    #[test]
    fn replay_equality_allows_only_one_ulp_drift_in_validated_money() {
        let (_temp, _workspace, scope) = fixture();
        let expected = CanonicalLlmFactRow::from_envelope(&envelope(
            1,
            call_completed(context(&scope), DAY_ONE),
        ))
        .expect("canonical row");
        let mut adjacent = expected.clone();
        let cost = expected.cost_usd.expect("fixture cost");
        adjacent.cost_usd = Some(f64::from_bits(cost.to_bits() + 1));
        assert!(canonical_rows_semantically_equal(&adjacent, &expected));

        adjacent.cost_usd = Some(f64::from_bits(cost.to_bits() + 2));
        assert!(!canonical_rows_semantically_equal(&adjacent, &expected));

        let mut non_monetary_drift = expected.clone();
        non_monetary_drift.operation = "other".to_string();
        assert!(!canonical_rows_semantically_equal(
            &non_monetary_drift,
            &expected
        ));
    }

    #[test]
    fn existing_schema_v1_fact_without_projection_mode_remains_replayable() {
        let (temp, _workspace, scope) = fixture();
        let expected = CanonicalLlmFactRow::from_envelope(&envelope(
            1,
            call_started(context(&scope), DAY_ONE),
        ))
        .expect("canonical row");
        assert_eq!(expected.prompt_projection_mode, None);

        let mut legacy_json = serde_json::to_value(&expected).expect("serialize legacy fixture");
        legacy_json
            .as_object_mut()
            .expect("row object")
            .remove("prompt_projection_mode");
        let json_path = temp.path().join("legacy-row.json");
        std::fs::write(
            &json_path,
            serde_json::to_vec(&legacy_json).expect("encode legacy fixture"),
        )
        .expect("write legacy fixture");
        let parquet_path = temp.path().join("legacy-row.parquet");
        let connection = Connection::open_in_memory().expect("DuckDB");
        connection
            .execute_batch(&format!(
                "COPY (SELECT * FROM read_json_auto('{}')) TO '{}' (FORMAT PARQUET)",
                sql_path(&json_path),
                sql_path(&parquet_path)
            ))
            .expect("write legacy parquet");

        verify_existing_row(&connection, &parquet_path, &expected)
            .expect("additive nullable column is replay-compatible");
    }

    #[test]
    fn calls_attempts_and_gaps_materialize_to_separate_scoped_datasets() {
        let (_temp, workspace, scope) = fixture();
        let trace_context = context(&scope);
        let rows = vec![
            envelope(1, call_started(trace_context.clone(), DAY_ONE - 100)),
            envelope(2, attempt_completed(trace_context.clone(), DAY_ONE - 20)),
            envelope(3, call_completed(trace_context, DAY_ONE)),
            envelope(4, capture_gap(&scope, DAY_ONE + 10)),
        ];
        let materializer = ParquetLlmTraceMaterializer::new(workspace.clone());
        materializer
            .materialize(&scope, &rows)
            .expect("materialization");

        let calls = workspace.analytics_llm_calls_root(&scope.principal, &scope.workspace);
        let attempts =
            workspace.analytics_llm_provider_attempts_root(&scope.principal, &scope.workspace);
        let gaps = workspace.analytics_llm_capture_gaps_root(&scope.principal, &scope.workspace);
        assert_eq!(parquet_count(&calls), 2);
        assert_eq!(parquet_count(&attempts), 1);
        assert_eq!(parquet_count(&gaps), 1);

        let connection = Connection::open_in_memory().expect("DuckDB");
        let attempt: (String, String, Option<u64>, Option<u64>, Option<bool>) = connection
            .query_row(
                &format!(
                    "SELECT lifecycle_phase, provider_attempt_id, queue_wait_ms, local_prep_ms, \
                     transport_success FROM read_parquet('{}')",
                    parquet_glob(&attempts)
                ),
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("attempt row");
        assert_eq!(attempt.0, "completed");
        assert!(attempt.1.ends_with(":a1"));
        assert_eq!(attempt.2, Some(5));
        assert_eq!(attempt.3, Some(10));
        assert_eq!(attempt.4, Some(true));

        let call_watermark: LlmDatasetMaterializationWatermark = workspace
            .read_json_path_sync(calls.join(DATASET_WATERMARK_FILE))
            .expect("call watermark");
        assert_eq!(call_watermark.dataset, "llm_calls");
        assert_eq!(call_watermark.committed_journal_sequence, 3);
        assert_eq!(call_watermark.published_revision_count, 2);
        let gap_watermark: LlmDatasetMaterializationWatermark = workspace
            .read_json_path_sync(gaps.join(DATASET_WATERMARK_FILE))
            .expect("gap watermark");
        assert_eq!(gap_watermark.committed_journal_sequence, 4);
    }

    #[test]
    fn tool_lifecycle_materializes_as_ordered_content_free_revisions() {
        let (_temp, workspace, scope) = fixture();
        let trace_context = context(&scope);
        let mut proposed = tool_lineage(
            trace_context.clone(),
            LlmToolLineageStage::Proposed,
            0,
            DAY_ONE - 30,
        );
        proposed.side_effect_state = LlmToolSideEffectState::None;
        let mut finished = tool_lineage(
            trace_context.clone(),
            LlmToolLineageStage::ExecutionFinished,
            1,
            DAY_ONE - 20,
        );
        finished.transport_ran = Some(true);
        finished.tool_reported_success = Some(true);
        finished.outcome = LlmToolLineageOutcome::Succeeded;
        finished.result_ref = Some("result_1".to_string());
        finished.canonical_event_ref = Some("event_1".to_string());
        finished.related_execution_ids = vec!["child_execution_1".to_string()];
        finished.side_effect_state = LlmToolSideEffectState::Unknown;
        let mut consumed = tool_lineage(
            trace_context,
            LlmToolLineageStage::ResultConsumed,
            1,
            DAY_ONE - 10,
        );
        consumed.result_ref = Some("result_1".to_string());
        consumed.consumed_by_call_id = Some("next_call_1".to_string());
        consumed.outcome = LlmToolLineageOutcome::Succeeded;
        consumed.side_effect_state = LlmToolSideEffectState::Unknown;
        let rows = vec![
            envelope(1, LlmTraceRecord::ToolLineage(proposed)),
            envelope(2, LlmTraceRecord::ToolLineage(finished)),
            envelope(3, LlmTraceRecord::ToolLineage(consumed)),
        ];
        ParquetLlmTraceMaterializer::new(workspace.clone())
            .materialize(&scope, &rows)
            .expect("tool lineage materialization");
        let root = workspace.analytics_llm_tool_calls_root(&scope.principal, &scope.workspace);
        assert_eq!(parquet_count(&root), 3);
        let connection = Connection::open_in_memory().expect("DuckDB");
        let mut statement = connection
            .prepare(&format!(
                "SELECT record_revision, tool_lineage_stage, arguments_fingerprint, result_ref, consumed_by_call_id, related_execution_ids_json \
                 FROM read_parquet('{}', union_by_name = true) ORDER BY record_revision",
                parquet_glob(&root)
            ))
            .expect("tool lifecycle query");
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i32>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })
            .expect("tool rows")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("tool values");
        assert_eq!(rows[0].0, 1);
        assert_eq!(rows[1].0, 2_001);
        assert_eq!(rows[2].0, 4_001);
        assert!(rows.iter().all(|row| row.2 == "a".repeat(64)));
        assert_eq!(rows[2].4.as_deref(), Some("next_call_1"));
        assert_eq!(rows[1].5.as_deref(), Some("[\"child_execution_1\"]"));
    }

    #[test]
    fn all_provider_attempt_lifecycle_revisions_remain_distinct_and_ordered() {
        let (_temp, workspace, scope) = fixture();
        let trace_context = context(&scope);
        let started = LlmProviderAttemptEvent::started(
            trace_context,
            "chat",
            1,
            "openai",
            "gpt-5.6-terra",
            DAY_ONE - 100,
        );
        let mut first_token = started.clone();
        first_token.phase = LlmProviderAttemptPhase::FirstToken;
        first_token.occurred_at_ms = DAY_ONE - 75;
        first_token.observed_at_ms = DAY_ONE - 74;
        first_token.timing.first_token_at_ms = Some(DAY_ONE - 75);
        first_token.timing.ttft_ms = Some(25);
        let mut completed = first_token.clone();
        completed.phase = LlmProviderAttemptPhase::Completed;
        completed.occurred_at_ms = DAY_ONE;
        completed.observed_at_ms = DAY_ONE + 1;
        completed.timing.completed_at_ms = Some(DAY_ONE);
        completed.timing.generation_after_ttft_ms = Some(75);
        completed.timing.latency_ms = Some(100);
        completed.terminal_state = Some(LlmAttemptTerminalState::Succeeded);

        let rows = vec![
            envelope(1, LlmTraceRecord::ProviderAttempt(started)),
            envelope(2, LlmTraceRecord::ProviderAttempt(first_token)),
            envelope(3, LlmTraceRecord::ProviderAttempt(completed)),
        ];
        ParquetLlmTraceMaterializer::new(workspace.clone())
            .materialize(&scope, &rows)
            .expect("attempt lifecycle");
        let attempts =
            workspace.analytics_llm_provider_attempts_root(&scope.principal, &scope.workspace);
        let connection = Connection::open_in_memory().expect("DuckDB");
        let mut statement = connection
            .prepare(&format!(
                "SELECT record_revision, lifecycle_phase FROM read_parquet('{}') \
                 ORDER BY record_revision",
                parquet_glob(&attempts)
            ))
            .expect("attempt revisions");
        let revisions = statement
            .query_map([], |row| {
                Ok((row.get::<_, i32>(0)?, row.get::<_, String>(1)?))
            })
            .expect("attempt rows")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("revision values");
        assert_eq!(
            revisions,
            vec![
                (1, "started".to_string()),
                (2, "first_token".to_string()),
                (3, "completed".to_string()),
            ]
        );
        let watermark: LlmDatasetMaterializationWatermark = workspace
            .read_json_path_sync(attempts.join(DATASET_WATERMARK_FILE))
            .expect("attempt watermark");
        assert_eq!(watermark.published_revision_count, 3);
    }

    #[test]
    fn duplicate_replay_verifies_existing_objects_without_double_counting() {
        let (_temp, workspace, scope) = fixture();
        let rows = vec![envelope(1, call_started(context(&scope), DAY_ONE))];
        let materializer = ParquetLlmTraceMaterializer::new(workspace.clone());
        materializer.materialize(&scope, &rows).expect("first pass");
        materializer
            .materialize(&scope, &rows)
            .expect("replay pass");

        let calls = workspace.analytics_llm_calls_root(&scope.principal, &scope.workspace);
        assert_eq!(parquet_count(&calls), 1);
        let watermark: LlmDatasetMaterializationWatermark = workspace
            .read_json_path_sync(calls.join(DATASET_WATERMARK_FILE))
            .expect("watermark");
        assert_eq!(watermark.published_revision_count, 1);
    }

    #[test]
    fn restricted_replay_is_exact_and_timestamp_drift_cannot_escape_to_another_partition() {
        let (_temp, workspace, scope) = fixture();
        let record = sanitized_call_io(context(&scope), DAY_ONE);
        let materializer = ParquetLlmTraceMaterializer::new(workspace.clone());
        materializer
            .materialize(&scope, &[envelope(1, record.clone())])
            .expect("first restricted materialization");
        materializer
            .materialize(&scope, &[envelope(2, record.clone())])
            .expect("same semantic revision with a new transport sequence");

        let call_io = workspace.analytics_llm_call_io_root(&scope.principal, &scope.workspace);
        assert_eq!(parquet_count(&call_io), 1);

        let mut drifted = record;
        let LlmTraceRecord::CallIo(drifted_record) = &mut drifted else {
            unreachable!();
        };
        drifted_record.occurred_at_ms = DAY_TWO;
        drifted_record.observed_at_ms = DAY_TWO + 1;
        let error = materializer
            .materialize(&scope, &[envelope(3, drifted)])
            .expect_err("same stable revision cannot move to another UTC partition");
        assert!(error.to_string().contains("conflicts"));
        assert_eq!(parquet_count(&call_io), 1);
    }

    #[test]
    fn replay_rejects_valid_parquet_with_mutated_non_key_payload() {
        let (_temp, workspace, scope) = fixture();
        let rows = vec![envelope(1, call_started(context(&scope), DAY_ONE))];
        let materializer = ParquetLlmTraceMaterializer::new(workspace.clone());
        materializer.materialize(&scope, &rows).expect("first pass");
        let calls = workspace.analytics_llm_calls_root(&scope.principal, &scope.workspace);
        let partition = calls.join(format!("dt={}", partition_date(DAY_ONE).expect("date")));
        let original = std::fs::read_dir(&partition)
            .expect("partition")
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .find(|path| path.extension().and_then(|value| value.to_str()) == Some("parquet"))
            .expect("canonical object");
        let replacement = partition.join(".mutated.parquet.tmp");
        Connection::open_in_memory()
            .expect("duckdb")
            .execute_batch(&format!(
                "COPY (SELECT * EXCLUDE (operation), 'tampered-operation' AS operation FROM read_parquet('{}')) TO '{}' (FORMAT PARQUET)",
                sql_path(&original),
                sql_path(&replacement),
            ))
            .expect("mutated but structurally valid parquet");
        std::fs::rename(&replacement, &original).expect("replace canonical object");

        let error = materializer
            .materialize(&scope, &rows)
            .expect_err("payload mutation must fail immutable replay");
        assert!(error.to_string().contains("conflicts"));
    }

    #[cfg(unix)]
    #[test]
    fn scoped_directory_symlink_cannot_redirect_materialization() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let external = tempfile::tempdir().expect("external");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "redirected");
        let principal_root = workspace.scopes_root().join("owner");
        std::fs::create_dir_all(&principal_root).expect("principal root");
        symlink(external.path(), principal_root.join("redirected")).expect("scope symlink");

        let error = ParquetLlmTraceMaterializer::new(workspace)
            .materialize(
                &scope,
                &[envelope(1, call_started(context(&scope), DAY_ONE))],
            )
            .expect_err("scope symlink must fail closed");
        assert!(error.to_string().contains("real directory"));
        assert_eq!(
            std::fs::read_dir(external.path())
                .expect("external directory")
                .count(),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn dataset_watermark_symlink_is_rejected_instead_of_followed() {
        use std::os::unix::fs::symlink;

        let (_temp, workspace, scope) = fixture();
        let calls = workspace.analytics_llm_calls_root(&scope.principal, &scope.workspace);
        std::fs::create_dir_all(&calls).expect("calls root");
        let external = tempfile::NamedTempFile::new().expect("external watermark");
        symlink(external.path(), calls.join(DATASET_WATERMARK_FILE))
            .expect("dataset watermark symlink");
        let materializer = ParquetLlmTraceMaterializer::new(workspace);
        let error = materializer
            .materialize(
                &scope,
                &[envelope(1, call_started(context(&scope), DAY_ONE))],
            )
            .expect_err("watermark symlink must fail closed");
        assert!(error.to_string().contains("regular file"));
    }

    #[test]
    fn replay_repairs_crash_window_after_object_publish_before_dataset_watermark() {
        let (_temp, workspace, scope) = fixture();
        let rows = vec![envelope(1, call_started(context(&scope), DAY_ONE))];
        let materializer = ParquetLlmTraceMaterializer::new(workspace.clone());
        materializer.materialize(&scope, &rows).expect("first pass");
        let calls = workspace.analytics_llm_calls_root(&scope.principal, &scope.workspace);
        std::fs::remove_file(calls.join(DATASET_WATERMARK_FILE)).expect("remove watermark");

        materializer
            .materialize(&scope, &rows)
            .expect("crash-window replay");
        assert_eq!(parquet_count(&calls), 1);
        let watermark: LlmDatasetMaterializationWatermark = workspace
            .read_json_path_sync(calls.join(DATASET_WATERMARK_FILE))
            .expect("repaired watermark");
        assert_eq!(watermark.committed_journal_sequence, 1);
        assert_eq!(watermark.published_revision_count, 1);
    }

    #[test]
    fn same_stable_revision_with_different_payload_fails_closed() {
        let (_temp, workspace, scope) = fixture();
        let trace_context = context(&scope);
        let original = envelope(1, call_started(trace_context.clone(), DAY_ONE));
        let materializer = ParquetLlmTraceMaterializer::new(workspace.clone());
        materializer
            .materialize(&scope, &[original])
            .expect("original materialization");

        let mut conflicting = call_started(trace_context, DAY_ONE);
        if let LlmTraceRecord::CallStarted(record) = &mut conflicting {
            record.operation = "different_operation".to_string();
        }
        let error = materializer
            .materialize(&scope, &[envelope(2, conflicting)])
            .expect_err("conflicting immutable revision must fail");
        assert!(error.to_string().contains("conflicts"));
        let calls = workspace.analytics_llm_calls_root(&scope.principal, &scope.workspace);
        assert_eq!(parquet_count(&calls), 1);
    }

    #[test]
    fn invalid_batch_is_rejected_before_any_object_is_published() {
        let (_temp, workspace, scope) = fixture();
        let other_scope = LlmScope::new("other", "workspace");
        let materializer = ParquetLlmTraceMaterializer::new(workspace.clone());
        let rows = vec![
            envelope(1, call_started(context(&scope), DAY_ONE)),
            envelope(2, call_started(context(&other_scope), DAY_ONE + 1)),
        ];
        materializer
            .materialize(&scope, &rows)
            .expect_err("mixed-scope batch must fail");
        assert!(!workspace
            .analytics_llm_calls_root(&scope.principal, &scope.workspace)
            .exists());
    }

    #[test]
    fn revisions_are_partitioned_by_utc_event_date_across_midnight() {
        let (_temp, workspace, scope) = fixture();
        let materializer = ParquetLlmTraceMaterializer::new(workspace.clone());
        let rows = vec![
            envelope(1, call_started(context(&scope), DAY_ONE)),
            envelope(2, call_started(context(&scope), DAY_TWO)),
        ];
        materializer.materialize(&scope, &rows).expect("two days");
        let calls = workspace.analytics_llm_calls_root(&scope.principal, &scope.workspace);
        let first_date = partition_date(DAY_ONE).expect("first date");
        let second_date = partition_date(DAY_TWO).expect("second date");
        assert_ne!(first_date, second_date);
        assert!(calls.join(format!("dt={first_date}")).exists());
        assert!(calls.join(format!("dt={second_date}")).exists());
    }

    #[test]
    fn real_materializer_replays_uncommitted_journal_then_commits_checksum() {
        let (_temp, workspace, scope) = fixture();
        let records = vec![call_started(context(&scope), DAY_ONE)];
        let mut journal =
            LlmTraceJournalStore::new(workspace.clone(), 1024 * 1024).expect("journal");
        let materializer = ParquetLlmTraceMaterializer::new(workspace.clone());
        journal
            .append_materialize_commit(&scope, &records, &materializer)
            .expect("append/materialize/commit");
        drop(journal);

        let mut restarted =
            LlmTraceJournalStore::new(workspace.clone(), 1024 * 1024).expect("restart");
        assert!(restarted
            .replay_uncommitted(&scope)
            .expect("replay")
            .is_empty());
        let watermark = restarted.watermark(&scope).expect("journal watermark");
        assert_eq!(watermark.committed_sequence, 1);
        assert!(watermark.committed_checksum.is_some());
        let calls = workspace.analytics_llm_calls_root(&scope.principal, &scope.workspace);
        assert_eq!(parquet_count(&calls), 1);
    }
}

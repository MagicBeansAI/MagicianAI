//! Shared governed read service for content-free LLM analytics facts.
//!
//! REST and `internal_data` are wired to this service in Phase 2E. Keeping the
//! relation registry, scope boundary, revision coalescing, legacy projection,
//! pagination and aggregate formulas here prevents those consumers from
//! drifting apart.

use std::{
    collections::HashSet,
    ops::ControlFlow,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use duckdb::{types::Value as DuckValue, Connection};
use magicllm::LlmScope;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlparser::ast::{
    visit_relations_mut, Expr, Ident, ObjectName, ObjectNamePart, Statement, TableFactor, Visit,
    Visitor,
};

use super::{
    duckdb_safety::{
        configure_analytics_connection_checked, run_analytics_query_with_interrupt_timeout,
        try_analytics_duckdb_guard_for, AnalyticsDuckDbQueryError,
        ANALYTICS_DUCKDB_MAX_RESULT_BYTES, ANALYTICS_DUCKDB_MAX_RESULT_ROWS,
    },
    legacy_llm_compat::{LEGACY_ERROR_REDACTION, LEGACY_INVALID_CATEGORY_REDACTION},
    llm_fact_compactor::{
        canonical_partition_dirs, governed_dataset_sources, governed_dataset_sources_in_date_range,
        legacy_call_partition_files, validate_scope, LlmGovernedPartitionSource,
    },
    llm_fact_registry::{
        LlmCanonicalDataset, LlmFactDefinition, LlmFactRegistry, LlmFactRelation,
        LLM_FACT_REGISTRY_SCHEMA_VERSION,
    },
    llm_scoped_path::{ensure_real_scoped_directory_chain, ensure_regular_file_or_missing},
    llm_sql_guard::{
        is_read_only_select_query, is_read_only_select_query_node, parse_analytics_sql,
    },
    llm_trace_journal::{
        read_indexed_journal_sequence, read_verified_journal_watermark, LlmTraceJournalWatermark,
    },
    llm_trace_materializer::{
        LlmDatasetMaterializationWatermark, DATASET_WATERMARK_FILE,
        DATASET_WATERMARK_SCHEMA_VERSION, FACT_COLUMNS, MATERIALIZED_FACT_SCHEMA_VERSION,
    },
    llm_trace_recorder::{LLM_TRACE_FACT_SCHEMA_VERSION, LLM_TRACE_JOURNAL_SCHEMA_VERSION},
};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

pub const LLM_ANALYTICS_READ_SCHEMA_VERSION: u16 = 1;
pub const LLM_FACT_CATALOG_SCHEMA_VERSION: u16 = 1;
/// Admission-timeout failure. Transient and retryable — the API layer maps it
/// to 503 rather than lumping it in with genuine read failures, so a contended
/// guard is never reported to a caller as a malformed request.
pub const LLM_READ_GUARD_TIMEOUT_MESSAGE: &str = "timed out waiting for the analytics DuckDB guard";
#[cfg(not(any(test, feature = "test-fixtures")))]
const READ_GUARD_TIMEOUT: Duration = Duration::from_secs(5);
// Rust runs unit tests concurrently, while every governed analytics fixture
// intentionally exercises the same process-wide embedded DuckDB guard. Give
// queued tests enough time to acquire the guard so load from sibling fixtures
// cannot mask the validation error a test is asserting. Production keeps the
// bounded five-second admission timeout above.
#[cfg(any(test, feature = "test-fixtures"))]
const READ_GUARD_TIMEOUT: Duration = Duration::from_secs(60);
const QUERY_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_PAGE_SIZE: usize = 100;
const MAX_PAGE_SIZE: usize = 1_000;
const MAX_OFFSET: usize = 1_000_000;
const DEFAULT_WINDOW_MS: i64 = 24 * 60 * 60 * 1_000;
const MAX_WINDOW_MS: i64 = 31 * DEFAULT_WINDOW_MS;
const FRESHNESS_STALE_AFTER_MS: i64 = 15 * 60 * 1_000;
const UNCLASSIFIED_TRANSPORT_LAG_REASONS_SQL: &str =
    "'runtime_transport_events_unclassified_due_broadcast_lag', 'llm_dispatch_events_unclassified_due_broadcast_lag'";
const KNOWN_MISSING_FACT_REASONS_SQL: &str =
    "'critical_buffer_saturated', 'critical_worker_unavailable', 'earlier_provider_attempt_lifecycle_unavailable', 'terminal_provider_attempt_lifecycle_unavailable'";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmReadTimeRange {
    from_ms: i64,
    to_ms: i64,
}

impl LlmReadTimeRange {
    pub fn new(from_ms: Option<i64>, to_ms: Option<i64>) -> Result<Self> {
        resolve_time_range(from_ms, to_ms)
    }

    pub const fn from_ms(self) -> i64 {
        self.from_ms
    }

    pub const fn to_ms(self) -> i64 {
        self.to_ms
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmReadFreshness {
    pub source_latest_at_ms: Option<i64>,
    pub materialized_through_sequence: Option<u64>,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmReadCoverage {
    pub eligible: Option<u64>,
    pub observed: Option<u64>,
    pub excluded: Option<u64>,
    pub censored: Option<u64>,
    pub missing: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmReadPagination {
    pub page: usize,
    pub page_size: usize,
    pub offset: usize,
    pub total: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmReadEnvelope<T> {
    pub schema_version: u16,
    pub scope: LlmScope,
    pub generated_at_ms: i64,
    pub requested_range: LlmReadTimeRange,
    pub effective_range: LlmReadTimeRange,
    pub filters: Value,
    pub freshness: LlmReadFreshness,
    pub coverage: LlmReadCoverage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pagination: Option<LlmReadPagination>,
    pub warnings: Vec<String>,
    pub data: T,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmDatasetCatalogEntry {
    pub dataset: LlmCanonicalDataset,
    pub stable_relation: String,
    pub revision_relation: String,
    pub materialized_schema_version: u16,
    pub first_partition: Option<String>,
    pub latest_partition: Option<String>,
    pub raw_revision_files: usize,
    pub governed_read_files: usize,
    pub compacted_partitions: usize,
    pub materialized_through_sequence: Option<u64>,
    pub published_revision_count: Option<u64>,
    pub watermark_updated_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmFactCatalog {
    pub schema_version: u16,
    pub registry_schema_version: u16,
    pub generated_at_ms: i64,
    pub principal: String,
    pub workspace: String,
    pub content_class: String,
    pub datasets: Vec<LlmDatasetCatalogEntry>,
    pub relations: Vec<LlmFactDefinition>,
    pub legacy_llm_call_files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmFactFilter {
    TextEquals { column: String, value: String },
    BooleanEquals { column: String, value: bool },
    IntegerAtLeast { column: String, value: i64 },
    IntegerAtMost { column: String, value: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmFactQuery {
    pub relation: String,
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default)]
    pub filters: Vec<LlmFactFilter>,
    pub order_by: Option<String>,
    #[serde(default = "default_true")]
    pub descending: bool,
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: usize,
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
}

impl LlmFactQuery {
    pub fn for_relation(relation: LlmFactRelation) -> Self {
        Self {
            relation: relation.as_str().to_string(),
            columns: Vec::new(),
            filters: Vec::new(),
            order_by: Some("timestamp_ms".to_string()),
            descending: true,
            limit: Some(DEFAULT_PAGE_SIZE),
            offset: 0,
            from_ms: None,
            to_ms: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmFactPage {
    pub schema_version: u16,
    pub principal: String,
    pub workspace: String,
    pub generated_at_ms: i64,
    pub relation: String,
    pub columns: Vec<String>,
    pub rows: Vec<Map<String, Value>>,
    pub total: u64,
    pub limit: usize,
    pub offset: usize,
    pub from_ms: i64,
    pub to_ms: i64,
    pub source_latest_at_ms: Option<i64>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmObservabilityOverview {
    pub schema_version: u16,
    pub principal: String,
    pub workspace: String,
    pub generated_at_ms: i64,
    pub logical_calls: u64,
    pub provider_attempts: u64,
    pub capture_gaps: u64,
    pub known_missing_fact_revisions: u64,
    pub unclassified_transport_events_lost: u64,
    pub captured_fact_revisions: u64,
    pub successful_calls: u64,
    pub validation_attempted_calls: u64,
    pub valid_contract_calls: u64,
    pub captured_calls: u64,
    pub training_excluded_calls: u64,
    /// Logical calls whose provider supplied at least one authoritative token
    /// bucket. A reported zero remains observed; absent usage does not become a
    /// plausible zero-token call.
    pub usage_observed_calls: u64,
    /// Logical calls with an authoritative or computed total cost, including a
    /// legitimate local zero. Unknown pricing remains outside this count.
    pub cost_observed_calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_read_tokens: u64,
    pub total_cost_usd: f64,
    pub cost_currency: String,
    pub pricing_versions: Vec<String>,
    pub cost_sources: Vec<String>,
    pub average_ttft_ms: Option<f64>,
    pub average_queue_wait_ms: Option<f64>,
    pub average_local_prep_ms: Option<f64>,
    pub average_provider_execution_ms: Option<f64>,
    pub average_validation_ms: Option<f64>,
    pub average_latency_ms: Option<f64>,
    pub latest_observed_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmCallDetail {
    pub call: Map<String, Value>,
    pub provider_attempts: Vec<Map<String, Value>>,
    pub tool_timeline: Vec<Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmTraceDetail {
    pub trace_id: String,
    pub calls: Vec<Map<String, Value>>,
    pub provider_attempts: Vec<Map<String, Value>>,
    pub tool_timeline: Vec<Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmFactSqlPage {
    pub columns: Vec<String>,
    pub rows: Vec<Map<String, Value>>,
    pub row_count: usize,
    pub limit: usize,
}

#[derive(Clone)]
pub struct LlmAnalyticsReadService {
    workspace: ArtifactV2Workspace,
    registry: LlmFactRegistry,
}

impl LlmAnalyticsReadService {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        Self {
            workspace,
            registry: LlmFactRegistry::canonical(),
        }
    }

    pub fn registry(&self) -> &LlmFactRegistry {
        &self.registry
    }

    pub fn refresh_catalog(&self, scope: &LlmScope) -> Result<LlmFactCatalog> {
        validate_scope(scope)?;
        let mut datasets = Vec::new();
        for dataset in LlmCanonicalDataset::ALL {
            let sources = governed_dataset_sources(&self.workspace, scope, dataset)?;
            let watermark = self.dataset_watermark(scope, dataset)?;
            datasets.push(dataset_catalog_entry(dataset, &sources, watermark.as_ref()));
        }
        let catalog = LlmFactCatalog {
            schema_version: LLM_FACT_CATALOG_SCHEMA_VERSION,
            registry_schema_version: LLM_FACT_REGISTRY_SCHEMA_VERSION,
            generated_at_ms: Utc::now().timestamp_millis(),
            principal: scope.principal.clone(),
            workspace: scope.workspace.clone(),
            content_class: "fact_only".to_string(),
            datasets,
            relations: LlmFactRelation::ALL
                .into_iter()
                .map(|relation| self.registry.resolve(relation).clone())
                .collect(),
            legacy_llm_call_files: self.legacy_call_files(scope)?.len(),
        };
        self.workspace
            .write_json_atomic_path_sync(
                self.workspace
                    .analytics_llm_fact_catalog_path(&scope.principal, &scope.workspace),
                &catalog,
            )
            .context("persisting scoped LLM fact catalog")?;
        Ok(catalog)
    }

    pub fn query_facts(&self, scope: &LlmScope, query: LlmFactQuery) -> Result<LlmFactPage> {
        validate_scope(scope)?;
        let relation = LlmFactRelation::parse(&query.relation).ok_or_else(|| {
            anyhow!(
                "unknown or unavailable LLM fact relation `{}`",
                query.relation
            )
        })?;
        let range = resolve_time_range(query.from_ms, query.to_ms)?;
        let definition = self.registry.resolve(relation);
        let selected_columns = if query.columns.is_empty() {
            definition
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect::<Vec<_>>()
        } else {
            validate_columns(&self.registry, relation, &query.columns)?;
            query.columns.clone()
        };
        for filter in &query.filters {
            validate_filter(&self.registry, relation, filter)?;
        }
        if let Some(order_by) = query.order_by.as_deref() {
            if !self.registry.allows_column(relation, order_by) {
                return Err(anyhow!(
                    "column `{order_by}` is not available on `{}`",
                    query.relation
                ));
            }
        }
        let limit = query
            .limit
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE);
        if query.offset > MAX_OFFSET {
            return Err(anyhow!(
                "LLM analytics offset exceeds the {MAX_OFFSET} row maximum"
            ));
        }
        let where_sql = filters_sql(&query.filters, range);
        let order_sql = query
            .order_by
            .as_deref()
            .map_or_else(String::new, |column| {
                format!(
                    " ORDER BY \"{column}\" {} NULLS LAST",
                    if query.descending { "DESC" } else { "ASC" }
                )
            });
        let projection = selected_columns
            .iter()
            .map(|column| format!("\"{column}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let relation_name = relation.as_str();
        self.with_connection(scope, range, |connection, warnings| {
            let total = query_scalar_u64(
                connection,
                &format!("SELECT count(*) FROM \"{relation_name}\"{where_sql}"),
            )?;
            let source_latest_at_ms = query_optional_i64(
                connection,
                &format!(
                    "SELECT max(observed_at_ms) FROM \"{relation_name}\"{where_sql}"
                ),
            )?;
            let sql = format!(
                "SELECT {projection} FROM \"{relation_name}\"{where_sql}{order_sql} LIMIT {limit} OFFSET {}",
                query.offset
            );
            let (columns, rows) = query_json_rows(connection, &sql, limit)?;
            Ok(LlmFactPage {
                schema_version: LLM_ANALYTICS_READ_SCHEMA_VERSION,
                principal: scope.principal.clone(),
                workspace: scope.workspace.clone(),
                generated_at_ms: Utc::now().timestamp_millis(),
                relation: relation_name.to_string(),
                columns,
                rows,
                total,
                limit,
                offset: query.offset,
                from_ms: range.from_ms,
                to_ms: range.to_ms,
                source_latest_at_ms,
                warnings,
            })
        })
    }

    pub fn query_facts_envelope(
        &self,
        scope: &LlmScope,
        query: LlmFactQuery,
    ) -> Result<LlmReadEnvelope<LlmFactPage>> {
        let filters = serde_json::to_value(&query).context("serializing LLM fact filters")?;
        let page = self.query_facts(scope, query)?;
        let range = LlmReadTimeRange {
            from_ms: page.from_ms,
            to_ms: page.to_ms,
        };
        let warnings = page.warnings.clone();
        let pagination = LlmReadPagination {
            page: page.offset / page.limit + 1,
            page_size: page.limit,
            offset: page.offset,
            total: page.total,
        };
        Ok(self.envelope(
            scope,
            range,
            filters,
            page.source_latest_at_ms,
            LlmReadCoverage {
                eligible: None,
                observed: Some(page.total),
                excluded: None,
                censored: None,
                missing: None,
            },
            Some(pagination),
            warnings,
            page,
        )?)
    }

    pub fn read_call(
        &self,
        scope: &LlmScope,
        llm_call_id: &str,
    ) -> Result<Option<Map<String, Value>>> {
        if llm_call_id.trim().is_empty() {
            return Err(anyhow!("llm_call_id must not be empty"));
        }
        let mut query = LlmFactQuery::for_relation(LlmFactRelation::Calls);
        query.filters.push(LlmFactFilter::TextEquals {
            column: "llm_call_id".to_string(),
            value: llm_call_id.to_string(),
        });
        query.limit = Some(1);
        let range = identity_time_range(llm_call_id)?;
        query.from_ms = Some(range.from_ms);
        query.to_ms = Some(range.to_ms);
        Ok(self.query_facts(scope, query)?.rows.into_iter().next())
    }

    pub fn read_provider_attempt(
        &self,
        scope: &LlmScope,
        provider_attempt_id: &str,
    ) -> Result<Option<Map<String, Value>>> {
        if provider_attempt_id.trim().is_empty() {
            return Err(anyhow!("provider_attempt_id must not be empty"));
        }
        let mut query = LlmFactQuery::for_relation(LlmFactRelation::ProviderAttempts);
        query.filters.push(LlmFactFilter::TextEquals {
            column: "provider_attempt_id".to_string(),
            value: provider_attempt_id.to_string(),
        });
        query.limit = Some(1);
        let range = identity_time_range(provider_attempt_id)?;
        query.from_ms = Some(range.from_ms);
        query.to_ms = Some(range.to_ms);
        Ok(self.query_facts(scope, query)?.rows.into_iter().next())
    }

    pub fn read_call_detail(
        &self,
        scope: &LlmScope,
        llm_call_id: &str,
    ) -> Result<Option<LlmCallDetail>> {
        let Some(call) = self.read_call(scope, llm_call_id)? else {
            return Ok(None);
        };
        let range = identity_time_range(llm_call_id)?;
        let mut attempts = LlmFactQuery::for_relation(LlmFactRelation::ProviderAttempts);
        attempts.filters.push(LlmFactFilter::TextEquals {
            column: "llm_call_id".to_string(),
            value: llm_call_id.to_string(),
        });
        attempts.order_by = Some("provider_attempt_index".to_string());
        attempts.descending = false;
        attempts.limit = Some(MAX_PAGE_SIZE);
        attempts.from_ms = Some(range.from_ms);
        attempts.to_ms = Some(range.to_ms);
        let mut tools = LlmFactQuery::for_relation(LlmFactRelation::ToolCalls);
        tools.filters.push(LlmFactFilter::TextEquals {
            column: "llm_call_id".to_string(),
            value: llm_call_id.to_string(),
        });
        tools.order_by = Some("record_revision".to_string());
        tools.descending = false;
        tools.limit = Some(MAX_PAGE_SIZE);
        tools.from_ms = Some(range.from_ms);
        tools.to_ms = Some(range.to_ms);
        Ok(Some(LlmCallDetail {
            call,
            provider_attempts: self.query_facts(scope, attempts)?.rows,
            tool_timeline: self.query_facts(scope, tools)?.rows,
        }))
    }

    pub fn read_call_detail_envelope(
        &self,
        scope: &LlmScope,
        llm_call_id: &str,
    ) -> Result<LlmReadEnvelope<Option<LlmCallDetail>>> {
        if llm_call_id.trim().is_empty() {
            return Err(anyhow!("llm_call_id must not be empty"));
        }
        let range = identity_time_range(llm_call_id)?;
        let mut call_query = LlmFactQuery::for_relation(LlmFactRelation::Calls);
        call_query.filters.push(LlmFactFilter::TextEquals {
            column: "llm_call_id".to_string(),
            value: llm_call_id.to_string(),
        });
        call_query.limit = Some(1);
        call_query.from_ms = Some(range.from_ms);
        call_query.to_ms = Some(range.to_ms);
        let call_page = self.query_facts(scope, call_query)?;
        let mut warnings = call_page.warnings;
        let mut latest = call_page.source_latest_at_ms;
        let detail = if let Some(call) = call_page.rows.into_iter().next() {
            let mut attempts = LlmFactQuery::for_relation(LlmFactRelation::ProviderAttempts);
            attempts.filters.push(LlmFactFilter::TextEquals {
                column: "llm_call_id".to_string(),
                value: llm_call_id.to_string(),
            });
            attempts.order_by = Some("provider_attempt_index".to_string());
            attempts.descending = false;
            attempts.limit = Some(MAX_PAGE_SIZE);
            attempts.from_ms = Some(range.from_ms);
            attempts.to_ms = Some(range.to_ms);
            let attempts_page = self.query_facts(scope, attempts)?;
            latest = latest.max(attempts_page.source_latest_at_ms);
            if attempts_page.total > attempts_page.rows.len() as u64 {
                warnings.push(format!(
                    "provider-attempt timeline has {} rows; returning the first {MAX_PAGE_SIZE}",
                    attempts_page.total
                ));
            }
            warnings.extend(attempts_page.warnings);
            let mut tools = LlmFactQuery::for_relation(LlmFactRelation::ToolCalls);
            tools.filters.push(LlmFactFilter::TextEquals {
                column: "llm_call_id".to_string(),
                value: llm_call_id.to_string(),
            });
            tools.order_by = Some("record_revision".to_string());
            tools.descending = false;
            tools.limit = Some(MAX_PAGE_SIZE);
            tools.from_ms = Some(range.from_ms);
            tools.to_ms = Some(range.to_ms);
            let tools_page = self.query_facts(scope, tools)?;
            latest = latest.max(tools_page.source_latest_at_ms);
            if tools_page.total > tools_page.rows.len() as u64 {
                warnings.push(format!(
                    "tool timeline has {} rows; returning the first {MAX_PAGE_SIZE}",
                    tools_page.total
                ));
            }
            warnings.extend(tools_page.warnings);
            warnings.sort();
            warnings.dedup();
            Some(LlmCallDetail {
                call,
                provider_attempts: attempts_page.rows,
                tool_timeline: tools_page.rows,
            })
        } else {
            None
        };
        let observed = u64::from(detail.is_some());
        self.envelope(
            scope,
            range,
            serde_json::json!({"llm_call_id": llm_call_id}),
            latest,
            LlmReadCoverage {
                eligible: Some(observed),
                observed: Some(observed),
                excluded: None,
                censored: None,
                missing: None,
            },
            None,
            warnings,
            detail,
        )
    }

    pub fn read_provider_attempt_envelope(
        &self,
        scope: &LlmScope,
        provider_attempt_id: &str,
    ) -> Result<LlmReadEnvelope<Option<Map<String, Value>>>> {
        if provider_attempt_id.trim().is_empty() {
            return Err(anyhow!("provider_attempt_id must not be empty"));
        }
        let range = identity_time_range(provider_attempt_id)?;
        let mut query = LlmFactQuery::for_relation(LlmFactRelation::ProviderAttempts);
        query.filters.push(LlmFactFilter::TextEquals {
            column: "provider_attempt_id".to_string(),
            value: provider_attempt_id.to_string(),
        });
        query.limit = Some(1);
        query.from_ms = Some(range.from_ms);
        query.to_ms = Some(range.to_ms);
        let page = self.query_facts(scope, query)?;
        let latest = page.source_latest_at_ms;
        let warnings = page.warnings;
        let detail = page.rows.into_iter().next();
        let observed = u64::from(detail.is_some());
        self.envelope(
            scope,
            range,
            serde_json::json!({"provider_attempt_id": provider_attempt_id}),
            latest,
            LlmReadCoverage {
                eligible: Some(observed),
                observed: Some(observed),
                excluded: None,
                censored: None,
                missing: None,
            },
            None,
            warnings,
            detail,
        )
    }

    pub fn list_traces_envelope(
        &self,
        scope: &LlmScope,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
        limit: Option<usize>,
    ) -> Result<LlmReadEnvelope<LlmFactSqlPage>> {
        self.query_fact_sql(
            scope,
            "WITH call_rollup AS (\
                 SELECT trace_id, min(observed_at_ms) AS first_observed_at_ms, \
                        max(observed_at_ms) AS last_observed_at_ms, \
                        count(DISTINCT llm_call_id) AS llm_call_count, \
                        count(*) FILTER (WHERE call_terminal_state = 'succeeded') AS succeeded_call_count, \
                        count(*) FILTER (WHERE call_terminal_state IN ('failed', 'cancelled', 'tombstoned')) AS failed_call_count \
                 FROM llm_calls WHERE trace_id IS NOT NULL GROUP BY trace_id\
             ), tool_rollup AS (\
                 SELECT trace_id, count(DISTINCT tool_execution_id) AS tool_execution_count, \
                        count(*) FILTER (WHERE tool_lineage_stage = 'linkage_gap') AS linkage_gap_count \
                 FROM llm_tool_calls WHERE trace_id IS NOT NULL GROUP BY trace_id\
             ) \
             SELECT c.trace_id, c.first_observed_at_ms, c.last_observed_at_ms, \
                    c.llm_call_count, c.succeeded_call_count, c.failed_call_count, \
                    COALESCE(t.tool_execution_count, 0) AS tool_execution_count, \
                    COALESCE(t.linkage_gap_count, 0) AS linkage_gap_count \
             FROM call_rollup c LEFT JOIN tool_rollup t ON t.trace_id = c.trace_id \
             ORDER BY c.last_observed_at_ms DESC",
            from_ms,
            to_ms,
            limit,
        )
    }

    pub fn read_trace_envelope(
        &self,
        scope: &LlmScope,
        trace_id: &str,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
    ) -> Result<LlmReadEnvelope<Option<LlmTraceDetail>>> {
        if trace_id.trim().is_empty() || trace_id.trim() != trace_id {
            return Err(anyhow!("trace_id must be a canonical nonblank identifier"));
        }
        let range = match (from_ms, to_ms) {
            (None, None) => identity_time_range(trace_id)?,
            _ => resolve_time_range(from_ms, to_ms)?,
        };
        let query = |relation: LlmFactRelation, order_by: &str| {
            let mut query = LlmFactQuery::for_relation(relation);
            query.filters.push(LlmFactFilter::TextEquals {
                column: "trace_id".to_string(),
                value: trace_id.to_string(),
            });
            query.order_by = Some(order_by.to_string());
            query.descending = false;
            query.limit = Some(MAX_PAGE_SIZE);
            query.from_ms = Some(range.from_ms);
            query.to_ms = Some(range.to_ms);
            query
        };
        let calls = self.query_facts(scope, query(LlmFactRelation::Calls, "observed_at_ms"))?;
        let attempts = self.query_facts(
            scope,
            query(LlmFactRelation::ProviderAttempts, "observed_at_ms"),
        )?;
        let tools = self.query_facts(scope, query(LlmFactRelation::ToolCalls, "observed_at_ms"))?;
        let latest = calls
            .source_latest_at_ms
            .max(attempts.source_latest_at_ms)
            .max(tools.source_latest_at_ms);
        let mut warnings = calls.warnings;
        if calls.total > calls.rows.len() as u64 {
            warnings.push(format!(
                "trace call timeline has {} rows; returning the first {MAX_PAGE_SIZE}",
                calls.total
            ));
        }
        if attempts.total > attempts.rows.len() as u64 {
            warnings.push(format!(
                "trace provider-attempt timeline has {} rows; returning the first {MAX_PAGE_SIZE}",
                attempts.total
            ));
        }
        if tools.total > tools.rows.len() as u64 {
            warnings.push(format!(
                "trace tool timeline has {} rows; returning the first {MAX_PAGE_SIZE}",
                tools.total
            ));
        }
        warnings.extend(attempts.warnings);
        warnings.extend(tools.warnings);
        warnings.sort();
        warnings.dedup();
        let observed = calls.rows.len() as u64;
        let detail = (observed > 0).then(|| LlmTraceDetail {
            trace_id: trace_id.to_string(),
            calls: calls.rows,
            provider_attempts: attempts.rows,
            tool_timeline: tools.rows,
        });
        self.envelope(
            scope,
            range,
            serde_json::json!({"trace_id": trace_id}),
            latest,
            LlmReadCoverage {
                eligible: Some(observed),
                observed: Some(observed),
                excluded: None,
                censored: None,
                missing: None,
            },
            None,
            warnings,
            detail,
        )
    }

    pub fn overview(&self, scope: &LlmScope) -> Result<LlmObservabilityOverview> {
        self.overview_with_range(scope, None, None)
    }

    pub fn overview_with_range(
        &self,
        scope: &LlmScope,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
    ) -> Result<LlmObservabilityOverview> {
        self.overview_with_range_and_warnings(scope, from_ms, to_ms)
            .map(|(overview, _)| overview)
    }

    fn overview_with_range_and_warnings(
        &self,
        scope: &LlmScope,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
    ) -> Result<(LlmObservabilityOverview, Vec<String>)> {
        validate_scope(scope)?;
        let range = resolve_time_range(from_ms, to_ms)?;
        self.with_connection(scope, range, |connection, warnings| {
            let range_predicate = time_range_predicate(range);
            let overview_row = run_analytics_query_with_interrupt_timeout(
                connection,
                QUERY_TIMEOUT,
                || connection.query_row(
                    &format!("SELECT count(*), count(*) FILTER (WHERE transport_success = true), count(*) FILTER (WHERE contract_validation_attempted = true), count(*) FILTER (WHERE contract_validation_attempted = true AND contract_validation_success = true), count(*) FILTER (WHERE capture_status IN ('complete', 'metadata_only', 'redacted')), count(*) FILTER (WHERE training_eligible_at_capture = false), count(*) FILTER (WHERE input_tokens IS NOT NULL OR output_tokens IS NOT NULL OR reasoning_tokens IS NOT NULL OR cache_read_tokens IS NOT NULL OR cache_creation_tokens IS NOT NULL), count(*) FILTER (WHERE cost_usd IS NOT NULL), CAST(COALESCE(sum(input_tokens), 0) AS BIGINT), CAST(COALESCE(sum(output_tokens), 0) AS BIGINT), CAST(COALESCE(sum(reasoning_tokens), 0) AS BIGINT), CAST(COALESCE(sum(cache_read_tokens), 0) AS BIGINT), COALESCE(sum(cost_usd), 0.0), avg(ttft_ms), avg(queue_wait_ms), avg(local_prep_ms), avg(provider_execution_ms), avg(validation_ms), avg(latency_ms), max(observed_at_ms) FROM llm_calls WHERE {range_predicate}"),
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?, row.get(10)?, row.get(11)?, row.get(12)?, row.get(13)?, row.get(14)?, row.get(15)?, row.get(16)?, row.get(17)?, row.get(18)?, row.get(19)?)),
                ),
            );
            let (logical_calls, successful_calls, validation_attempted_calls,
                valid_contract_calls, captured_calls, training_excluded_calls,
                usage_observed_calls, cost_observed_calls,
                input_tokens, output_tokens, reasoning_tokens, cache_read_tokens,
                total_cost_usd, average_ttft_ms, average_queue_wait_ms,
                average_local_prep_ms, average_provider_execution_ms,
                average_validation_ms, average_latency_ms, latest_observed_at_ms):
                (i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, f64,
                 Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<f64>,
                 Option<f64>, Option<i64>) = match overview_row {
                    Ok(row) => row,
                    Err(AnalyticsDuckDbQueryError::Query(error)) => {
                        return Err(anyhow::Error::new(error).context("reading LLM call overview"));
                    },
                    Err(AnalyticsDuckDbQueryError::TimedOut(error)) => {
                        return Err(anyhow!(
                            "LLM analytics overview timed out{}",
                            error.map(|error| format!(": {error}")).unwrap_or_default()
                        ));
                    },
                };
            let overview = LlmObservabilityOverview {
                schema_version: LLM_ANALYTICS_READ_SCHEMA_VERSION,
                principal: scope.principal.clone(),
                workspace: scope.workspace.clone(),
                generated_at_ms: Utc::now().timestamp_millis(),
                logical_calls: checked_nonnegative_u64(logical_calls, "logical_calls")?,
                provider_attempts: query_scalar_u64(connection, &format!("SELECT count(*) FROM llm_provider_attempts WHERE {range_predicate}"))?,
                capture_gaps: query_scalar_u64(connection, &format!("SELECT CAST(COALESCE(sum(missing_record_count), 0) AS BIGINT) FROM llm_capture_gaps WHERE {range_predicate} AND COALESCE(gap_reason, '') NOT IN ({UNCLASSIFIED_TRANSPORT_LAG_REASONS_SQL})"))?,
                known_missing_fact_revisions: query_scalar_u64(connection, &format!("SELECT CAST(COALESCE(sum(missing_record_count), 0) AS BIGINT) FROM llm_capture_gaps WHERE {range_predicate} AND gap_reason IN ({KNOWN_MISSING_FACT_REASONS_SQL})"))?,
                unclassified_transport_events_lost: query_scalar_u64(connection, &format!("SELECT CAST(COALESCE(sum(missing_record_count), 0) AS BIGINT) FROM llm_capture_gaps WHERE {range_predicate} AND gap_reason IN ({UNCLASSIFIED_TRANSPORT_LAG_REASONS_SQL})"))?,
                captured_fact_revisions: query_scalar_u64(connection, &format!("SELECT (SELECT count(*) FROM llm_call_revisions WHERE {range_predicate}) + (SELECT count(*) FROM llm_provider_attempt_revisions WHERE {range_predicate}) + (SELECT count(*) FROM llm_tool_call_revisions WHERE {range_predicate})"))?,
                successful_calls: checked_nonnegative_u64(successful_calls, "successful_calls")?,
                validation_attempted_calls: checked_nonnegative_u64(validation_attempted_calls, "validation_attempted_calls")?,
                valid_contract_calls: checked_nonnegative_u64(valid_contract_calls, "valid_contract_calls")?,
                captured_calls: checked_nonnegative_u64(captured_calls, "captured_calls")?,
                training_excluded_calls: checked_nonnegative_u64(training_excluded_calls, "training_excluded_calls")?,
                usage_observed_calls: checked_nonnegative_u64(usage_observed_calls, "usage_observed_calls")?,
                cost_observed_calls: checked_nonnegative_u64(cost_observed_calls, "cost_observed_calls")?,
                input_tokens: checked_nonnegative_u64(input_tokens, "input_tokens")?,
                output_tokens: checked_nonnegative_u64(output_tokens, "output_tokens")?,
                reasoning_tokens: checked_nonnegative_u64(reasoning_tokens, "reasoning_tokens")?,
                cache_read_tokens: checked_nonnegative_u64(cache_read_tokens, "cache_read_tokens")?,
                total_cost_usd: checked_nonnegative_f64(total_cost_usd, "total_cost_usd")?,
                cost_currency: "USD".to_string(),
                pricing_versions: query_distinct_text(connection, &format!("SELECT DISTINCT pricing_version AS value FROM llm_calls WHERE {range_predicate} AND pricing_version IS NOT NULL ORDER BY value"), 100)?,
                cost_sources: query_distinct_text(connection, &format!("SELECT DISTINCT cost_source AS value FROM llm_calls WHERE {range_predicate} AND cost_source IS NOT NULL ORDER BY value"), 100)?,
                average_ttft_ms: checked_optional_nonnegative_f64(
                    average_ttft_ms,
                    "average_ttft_ms",
                )?,
                average_queue_wait_ms: checked_optional_nonnegative_f64(
                    average_queue_wait_ms,
                    "average_queue_wait_ms",
                )?,
                average_local_prep_ms: checked_optional_nonnegative_f64(
                    average_local_prep_ms,
                    "average_local_prep_ms",
                )?,
                average_provider_execution_ms: checked_optional_nonnegative_f64(
                    average_provider_execution_ms,
                    "average_provider_execution_ms",
                )?,
                average_validation_ms: checked_optional_nonnegative_f64(
                    average_validation_ms,
                    "average_validation_ms",
                )?,
                average_latency_ms: checked_optional_nonnegative_f64(
                    average_latency_ms,
                    "average_latency_ms",
                )?,
                latest_observed_at_ms,
            };
            Ok((overview, warnings))
        })
    }

    pub fn overview_envelope(
        &self,
        scope: &LlmScope,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
    ) -> Result<LlmReadEnvelope<LlmObservabilityOverview>> {
        let range = resolve_time_range(from_ms, to_ms)?;
        let (overview, warnings) =
            self.overview_with_range_and_warnings(scope, Some(range.from_ms), Some(range.to_ms))?;
        let observed = overview.captured_fact_revisions;
        let missing = overview.known_missing_fact_revisions;
        let excluded = overview.training_excluded_calls;
        self.envelope(
            scope,
            range,
            Value::Object(Map::new()),
            overview.latest_observed_at_ms,
            LlmReadCoverage {
                eligible: Some(observed.saturating_add(missing)),
                observed: Some(observed),
                excluded: Some(excluded),
                censored: None,
                missing: Some(missing),
            },
            None,
            warnings,
            overview,
        )
    }

    pub fn query_fact_sql(
        &self,
        scope: &LlmScope,
        sql: &str,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
        limit: Option<usize>,
    ) -> Result<LlmReadEnvelope<LlmFactSqlPage>> {
        validate_scope(scope)?;
        let validated_sql = validate_fact_sql(&self.registry, sql)?;
        let requested_sql = sql.trim().to_string();
        let range = resolve_time_range(from_ms, to_ms)?;
        let limit = limit.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE);
        self.with_connection(scope, range, |connection, warnings| {
            let mut warnings = warnings;
            let range_predicate = time_range_predicate(range);
            for relation in &validated_sql.canonical_relations {
                connection
                    .execute_batch(&format!(
                        "CREATE OR REPLACE TEMP VIEW \"{}\" AS SELECT * FROM \"{relation}\" WHERE {range_predicate}",
                        bounded_fact_relation(relation)
                    ))
                    .with_context(|| format!("installing bounded LLM fact relation {relation}"))?;
            }
            let mut source_latest_at_ms = None;
            for relation in &validated_sql.canonical_relations {
                let latest = query_optional_i64(
                    connection,
                    &format!(
                        "SELECT max(observed_at_ms) FROM \"{}\"",
                        bounded_fact_relation(relation)
                    ),
                )?;
                source_latest_at_ms = source_latest_at_ms.max(latest);
            }
            let probe_limit = limit.saturating_add(1);
            let bounded_sql = format!(
                "SELECT * FROM ({}) AS __llm_fact_result LIMIT {probe_limit}",
                validated_sql.sql
            );
            let (columns, mut rows) = query_json_rows(connection, &bounded_sql, probe_limit)?;
            let truncated = rows.len() > limit;
            if truncated {
                rows.truncate(limit);
                warnings.push(format!(
                    "result exceeded the {limit} row limit and was truncated; narrow the range or aggregate further"
                ));
            }
            let row_count = rows.len();
            self.envelope(
                scope,
                range,
                serde_json::json!({"sql": requested_sql}),
                source_latest_at_ms,
                LlmReadCoverage {
                    eligible: None,
                    observed: Some(row_count as u64),
                    excluded: None,
                    censored: Some(u64::from(truncated)),
                    missing: None,
                },
                None,
                warnings,
                LlmFactSqlPage {
                    columns,
                    rows,
                    row_count,
                    limit,
                },
            )
        })
    }

    fn envelope<T: Serialize>(
        &self,
        scope: &LlmScope,
        range: LlmReadTimeRange,
        filters: Value,
        source_latest_at_ms: Option<i64>,
        coverage: LlmReadCoverage,
        pagination: Option<LlmReadPagination>,
        warnings: Vec<String>,
        data: T,
    ) -> Result<LlmReadEnvelope<T>> {
        // Per-dataset watermarks are not a contiguous global prefix because
        // journal sequences interleave across datasets. Only the durable
        // journal watermark can authoritatively state that every revision up
        // to a sequence has materialized.
        let materialized_through_sequence = self
            .journal_watermark(scope)?
            .map(|watermark| watermark.committed_sequence);
        let mut warnings = warnings;
        let mut catch_up_unknown = false;
        let pending = match read_indexed_journal_sequence(&self.workspace, scope) {
            Ok(sequence) => {
                sequence.map(|head| head.saturating_sub(materialized_through_sequence.unwrap_or(0)))
            },
            Err(_) => {
                catch_up_unknown = true;
                warnings.push("LLM journal catch-up status is temporarily unavailable".to_string());
                None
            },
        };
        if let Some(pending) = pending.filter(|pending| *pending > 0) {
            warnings.push(format!("LLM analytics is catching up: {pending} durable journal revisions await publication"));
        }
        let stale = catch_up_unknown
            || pending.is_some_and(|count| count > 0)
            || source_latest_at_ms
                .map(|latest| latest < range.to_ms.saturating_sub(FRESHNESS_STALE_AFTER_MS))
                .unwrap_or(true);
        let envelope = LlmReadEnvelope {
            schema_version: LLM_ANALYTICS_READ_SCHEMA_VERSION,
            scope: scope.clone(),
            generated_at_ms: Utc::now().timestamp_millis(),
            requested_range: range,
            effective_range: range,
            filters,
            freshness: LlmReadFreshness {
                source_latest_at_ms,
                materialized_through_sequence,
                stale,
            },
            coverage,
            pagination,
            warnings,
            data,
        };
        let serialized_bytes = serde_json::to_vec(&envelope)
            .context("serializing bounded LLM analytics response")?
            .len();
        if serialized_bytes > ANALYTICS_DUCKDB_MAX_RESULT_BYTES {
            return Err(anyhow!(
                "LLM analytics response exceeds the {} byte limit",
                ANALYTICS_DUCKDB_MAX_RESULT_BYTES
            ));
        }
        Ok(envelope)
    }

    fn with_connection<T>(
        &self,
        scope: &LlmScope,
        range: LlmReadTimeRange,
        operation: impl FnOnce(&Connection, Vec<String>) -> Result<T>,
    ) -> Result<T> {
        let Some(_guard) = try_analytics_duckdb_guard_for(READ_GUARD_TIMEOUT) else {
            return Err(anyhow!(LLM_READ_GUARD_TIMEOUT_MESSAGE));
        };
        let connection = Connection::open_in_memory()
            .context("opening scoped in-memory LLM analytics connection")?;
        configure_analytics_connection_checked(&connection, "llm_analytics_read_service")
            .context("configuring scoped LLM analytics connection")?;
        let warnings =
            match run_analytics_query_with_interrupt_timeout(&connection, QUERY_TIMEOUT, || {
                self.install_views(scope, range, &connection)
            }) {
                Ok(warnings) => warnings,
                Err(AnalyticsDuckDbQueryError::Query(error)) => return Err(error),
                Err(AnalyticsDuckDbQueryError::TimedOut(error)) => {
                    return Err(anyhow!(
                        "installing governed LLM analytics views timed out{}",
                        error.map(|error| format!(": {error}")).unwrap_or_default()
                    ));
                },
            };
        // Source files have already been copied into process-local temporary
        // tables. Disable every external access path before any consumer SQL
        // runs, including the AST-validated fact-query escape hatch.
        connection
            .execute_batch(
                "SET enable_external_access = false; SET autoinstall_known_extensions = false; SET autoload_known_extensions = false;",
            )
            .context("disabling external access for governed LLM fact reads")?;
        operation(&connection, warnings)
    }

    fn install_views(
        &self,
        scope: &LlmScope,
        range: LlmReadTimeRange,
        connection: &Connection,
    ) -> Result<Vec<String>> {
        let earliest_date = timestamp_date(range.from_ms)?;
        let latest_date = timestamp_date(range.to_ms.saturating_sub(1))?;
        let call_sources = governed_dataset_sources_in_date_range(
            &self.workspace,
            scope,
            LlmCanonicalDataset::Calls,
            Some(earliest_date),
            Some(latest_date),
        )?;
        let attempt_sources = governed_dataset_sources_in_date_range(
            &self.workspace,
            scope,
            LlmCanonicalDataset::ProviderAttempts,
            Some(earliest_date),
            Some(latest_date),
        )?;
        let tool_sources = governed_dataset_sources_in_date_range(
            &self.workspace,
            scope,
            LlmCanonicalDataset::ToolCalls,
            Some(earliest_date),
            Some(latest_date),
        )?;
        let gap_sources = governed_dataset_sources_in_date_range(
            &self.workspace,
            scope,
            LlmCanonicalDataset::CaptureGaps,
            Some(earliest_date),
            Some(latest_date),
        )?;
        install_raw_view(
            connection,
            "llm_call_revisions",
            flatten_files(&call_sources),
            scope,
            LlmCanonicalDataset::Calls,
        )?;
        install_raw_view(
            connection,
            "llm_provider_attempt_revisions",
            flatten_files(&attempt_sources),
            scope,
            LlmCanonicalDataset::ProviderAttempts,
        )?;
        install_raw_view(
            connection,
            "llm_tool_call_revisions",
            flatten_files(&tool_sources),
            scope,
            LlmCanonicalDataset::ToolCalls,
        )?;
        install_raw_view(
            connection,
            "llm_capture_gap_revisions",
            flatten_files(&gap_sources),
            scope,
            LlmCanonicalDataset::CaptureGaps,
        )?;
        validate_cross_dataset_lifecycle(connection)?;
        install_coalesced_view(connection, "llm_calls_canonical_base", "llm_call_revisions")?;
        install_coalesced_view(
            connection,
            "llm_provider_attempts_canonical",
            "llm_provider_attempt_revisions",
        )?;
        // A tool lifecycle is a sequence of independently meaningful stages,
        // not one latest-wins entity. Stable ingestion already de-duplicates
        // `(tool_execution_id, record_revision)`, so the public fact relation
        // intentionally preserves the complete ordered timeline.
        connection
            .execute_batch(
                "CREATE OR REPLACE VIEW llm_tool_calls AS SELECT * FROM llm_tool_call_revisions",
            )
            .context("installing stable llm_tool_calls relation")?;
        install_coalesced_view(connection, "llm_capture_gaps", "llm_capture_gap_revisions")?;
        let dispatch_files =
            self.dispatch_files_in_date_range(scope, earliest_date, latest_date)?;
        install_dispatch_timing_view(connection, &dispatch_files, scope)?;
        install_dispatch_enriched_attempts_view(connection)?;
        install_enriched_calls_view(connection)?;

        let legacy_files =
            self.legacy_call_files_in_date_range(scope, earliest_date, latest_date)?;
        let mut warnings = Vec::new();
        if legacy_files.is_empty() {
            install_typed_empty_view(connection, "llm_calls_legacy")?;
        } else {
            install_legacy_calls_view(connection, scope, &legacy_files)?;
            warnings.push(format!(
                "{} pre-canonical llm_calls file(s) were normalized with typed legacy defaults",
                legacy_files.len()
            ));
        }
        connection
            .execute_batch(
                "CREATE OR REPLACE VIEW llm_calls AS \
                 SELECT * FROM llm_calls_canonical \
                 UNION ALL BY NAME \
                 SELECT legacy.* FROM llm_calls_legacy legacy \
                 WHERE legacy.llm_call_id IS NULL \
                    OR NOT EXISTS (SELECT 1 FROM llm_calls_canonical canonical \
                                   WHERE canonical.llm_call_id = legacy.llm_call_id)",
            )
            .context("installing stable llm_calls relation")?;
        Ok(warnings)
    }

    fn legacy_call_files(&self, scope: &LlmScope) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for (_, partition) in
            canonical_partition_dirs(&LlmCanonicalDataset::Calls.root(&self.workspace, scope))?
        {
            files.extend(legacy_call_partition_files(&partition)?);
        }
        Ok(files)
    }

    fn legacy_call_files_in_date_range(
        &self,
        scope: &LlmScope,
        earliest_date: NaiveDate,
        latest_date: NaiveDate,
    ) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for (date, partition) in
            canonical_partition_dirs(&LlmCanonicalDataset::Calls.root(&self.workspace, scope))?
        {
            if date >= earliest_date && date <= latest_date {
                files.extend(legacy_call_partition_files(&partition)?);
            }
        }
        Ok(files)
    }

    fn dispatch_files_in_date_range(
        &self,
        scope: &LlmScope,
        earliest_date: NaiveDate,
        latest_date: NaiveDate,
    ) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        let root = self
            .workspace
            .analytics_root(&scope.principal, &scope.workspace)
            .join("llm_dispatch");
        ensure_real_scoped_directory_chain(self.workspace.base_root(), &root)?;
        for (date, partition) in canonical_partition_dirs(&root)? {
            if date >= earliest_date && date <= latest_date {
                files.extend(dispatch_partition_files(&partition)?);
            }
        }
        Ok(files)
    }

    fn dataset_watermark(
        &self,
        scope: &LlmScope,
        dataset: LlmCanonicalDataset,
    ) -> Result<Option<LlmDatasetMaterializationWatermark>> {
        let path = dataset
            .root(&self.workspace, scope)
            .join(DATASET_WATERMARK_FILE);
        ensure_real_scoped_directory_chain(
            self.workspace.base_root(),
            path.parent().unwrap_or(&path),
        )?;
        if !ensure_regular_file_or_missing(&path)? {
            return Ok(None);
        }
        let watermark = match self
            .workspace
            .read_json_path_sync::<LlmDatasetMaterializationWatermark, _>(&path)
        {
            Ok(watermark) => watermark,
            Err(error) => {
                return Err(anyhow::Error::new(error)
                    .context("reading scoped LLM dataset materialization watermark"));
            },
        };
        if watermark.schema_version != DATASET_WATERMARK_SCHEMA_VERSION
            || watermark.materialized_schema_version != MATERIALIZED_FACT_SCHEMA_VERSION
            || watermark.dataset != dataset.as_str()
            || watermark.committed_journal_sequence == 0
            || !is_lower_hex_64(&watermark.committed_checksum)
            || watermark.published_revision_count == 0
            || watermark.updated_at_ms <= 0
        {
            return Err(anyhow!(
                "invalid {} materialization watermark in scoped LLM catalog",
                dataset.as_str()
            ));
        }
        Ok(Some(watermark))
    }

    fn journal_watermark(&self, scope: &LlmScope) -> Result<Option<LlmTraceJournalWatermark>> {
        read_verified_journal_watermark(&self.workspace, scope).map_err(|error| {
            anyhow!("validating authoritative LLM journal materialization watermark: {error}")
        })
    }
}

fn dataset_catalog_entry(
    dataset: LlmCanonicalDataset,
    sources: &[LlmGovernedPartitionSource],
    watermark: Option<&LlmDatasetMaterializationWatermark>,
) -> LlmDatasetCatalogEntry {
    LlmDatasetCatalogEntry {
        dataset,
        stable_relation: dataset.stable_relation().as_str().to_string(),
        revision_relation: dataset.revision_relation().as_str().to_string(),
        materialized_schema_version: MATERIALIZED_FACT_SCHEMA_VERSION,
        first_partition: sources.first().map(|source| source.partition_date.clone()),
        latest_partition: sources.last().map(|source| source.partition_date.clone()),
        raw_revision_files: sources.iter().map(|source| source.raw_file_count).sum(),
        governed_read_files: sources.iter().map(|source| source.files.len()).sum(),
        compacted_partitions: sources
            .iter()
            .filter(|source| source.using_compaction)
            .count(),
        materialized_through_sequence: sources
            .iter()
            .filter_map(|source| source.materialized_through_sequence)
            .chain(watermark.map(|watermark| watermark.committed_journal_sequence))
            .max(),
        published_revision_count: watermark.map(|watermark| watermark.published_revision_count),
        watermark_updated_at_ms: watermark.map(|watermark| watermark.updated_at_ms),
    }
}

fn install_raw_view(
    connection: &Connection,
    view: &str,
    files: Vec<PathBuf>,
    scope: &LlmScope,
    dataset: LlmCanonicalDataset,
) -> Result<()> {
    if files.is_empty() {
        return install_typed_empty_view(connection, view);
    }
    let source = format!("{view}_source");
    connection
        .execute_batch(&format!(
            "CREATE TEMP TABLE \"{source}\" AS SELECT * FROM read_parquet({}, hive_partitioning = false, union_by_name = true)",
            parquet_source_sql(&files)
        ))
        .with_context(|| format!("loading governed LLM source {view}"))?;
    let available = view_columns(connection, &source)?;
    validate_required_canonical_columns(&source, &available)?;
    let projection = FACT_COLUMNS
        .iter()
        .map(|(name, ty)| {
            if available.contains(*name) {
                format!("CAST(\"{name}\" AS {ty}) AS \"{name}\"")
            } else {
                format!("CAST(NULL AS {ty}) AS \"{name}\"")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let normalized = format!("{view}_normalized");
    connection
        .execute_batch(&format!(
            "CREATE OR REPLACE TEMP VIEW \"{normalized}\" AS SELECT {projection} FROM \"{source}\"",
        ))
        .with_context(|| format!("normalizing governed LLM view {view}"))?;
    let normalized_columns = FACT_COLUMNS
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect::<HashSet<_>>();
    validate_raw_canonical_source(connection, &normalized, &normalized_columns, scope, dataset)?;
    connection
        .execute_batch(&format!(
            "CREATE OR REPLACE VIEW \"{view}\" AS SELECT * FROM \"{normalized}\"",
        ))
        .with_context(|| format!("installing governed LLM view {view}"))
}

fn validate_required_canonical_columns(source: &str, available: &HashSet<String>) -> Result<()> {
    for required in [
        "materialized_schema_version",
        "fact_schema_version",
        "journal_schema_version",
        "stable_id",
        "record_revision",
        "journal_sequence",
        "record_kind",
        "lifecycle_phase",
        "idempotency_key",
        "payload_checksum",
        "occurred_at_ms",
        "observed_at_ms",
        "timestamp_ms",
        "principal",
        "workspace",
        "capture_mode",
        "capture_status",
        "training_eligible_at_capture",
        "training_exclusion_reason",
    ] {
        if !available.contains(required) {
            return Err(anyhow!(
                "governed LLM source {source} is missing required identity column {required}"
            ));
        }
    }
    Ok(())
}

fn validate_raw_canonical_source(
    connection: &Connection,
    source: &str,
    available: &HashSet<String>,
    scope: &LlmScope,
    dataset: LlmCanonicalDataset,
) -> Result<()> {
    validate_required_canonical_columns(source, available)?;
    let invalid_identity: i64 = connection
        .query_row(
            &format!(
                "SELECT count(*) FROM \"{source}\" WHERE stable_id IS NULL OR trim(CAST(stable_id AS VARCHAR)) = '' OR trim(CAST(stable_id AS VARCHAR)) <> CAST(stable_id AS VARCHAR) OR record_kind IS NULL OR trim(CAST(record_kind AS VARCHAR)) = '' OR lifecycle_phase IS NULL OR trim(CAST(lifecycle_phase AS VARCHAR)) = '' OR principal IS NULL OR trim(CAST(principal AS VARCHAR)) = '' OR trim(CAST(principal AS VARCHAR)) <> CAST(principal AS VARCHAR) OR workspace IS NULL OR trim(CAST(workspace AS VARCHAR)) = '' OR trim(CAST(workspace AS VARCHAR)) <> CAST(workspace AS VARCHAR) OR record_revision IS NULL OR CAST(record_revision AS BIGINT) <= 0 OR journal_sequence IS NULL OR CAST(journal_sequence AS BIGINT) <= 0"
            ),
            [],
            |row| row.get(0),
        )
        .with_context(|| format!("validating governed LLM identity in {source}"))?;
    if invalid_identity != 0 {
        return Err(anyhow!(
            "governed LLM source {source} contains {invalid_identity} row(s) with invalid stable identity"
        ));
    }
    let boundary_mismatches: i64 = connection
        .query_row(
            &format!(
                "SELECT count(*) FROM \"{source}\" WHERE record_kind <> ? OR principal <> ? OR workspace <> ?"
            ),
            duckdb::params![dataset.record_kind(), &scope.principal, &scope.workspace],
            |row| row.get(0),
        )
        .with_context(|| format!("validating governed LLM scope boundary in {source}"))?;
    if boundary_mismatches != 0 {
        return Err(anyhow!(
            "governed LLM source {source} contains {boundary_mismatches} row(s) outside the requested dataset or scope"
        ));
    }
    let revision_is_invalid = match dataset {
        LlmCanonicalDataset::Calls => "record_revision NOT IN (1, 2)",
        LlmCanonicalDataset::ProviderAttempts => "record_revision NOT IN (1, 2, 3)",
        LlmCanonicalDataset::ToolCalls => {
            "record_revision NOT IN (1, 10, 20, 30, 40, 50, 3000, 5000) AND NOT (record_revision BETWEEN 1001 AND 1999 OR record_revision BETWEEN 2001 AND 2999 OR record_revision BETWEEN 4001 AND 4999 OR record_revision BETWEEN 6001 AND 6999 OR record_revision BETWEEN 7001 AND 7999 OR record_revision BETWEEN 8001 AND 8999)"
        },
        LlmCanonicalDataset::CaptureGaps => "record_revision <> 1",
    };
    let invalid_revision: i64 = connection
        .query_row(
            &format!("SELECT count(*) FROM \"{source}\" WHERE {revision_is_invalid}"),
            [],
            |row| row.get(0),
        )
        .with_context(|| format!("validating governed LLM revisions in {source}"))?;
    if invalid_revision != 0 {
        return Err(anyhow!(
            "governed LLM source {source} contains {invalid_revision} row(s) with an unsupported lifecycle revision"
        ));
    }
    let invalid_versions: i64 = connection
        .query_row(
            &format!(
                "SELECT count(*) FROM \"{source}\" WHERE materialized_schema_version IS NULL OR materialized_schema_version <> {} OR fact_schema_version IS NULL OR fact_schema_version <> {} OR journal_schema_version IS NULL OR journal_schema_version <> {}",
                MATERIALIZED_FACT_SCHEMA_VERSION,
                LLM_TRACE_FACT_SCHEMA_VERSION,
                LLM_TRACE_JOURNAL_SCHEMA_VERSION,
            ),
            [],
            |row| row.get(0),
        )
        .with_context(|| format!("validating governed LLM schema versions in {source}"))?;
    if invalid_versions != 0 {
        return Err(anyhow!(
            "governed LLM source {source} contains {invalid_versions} row(s) with unsupported schema versions"
        ));
    }
    for optional_identity in ["idempotency_key", "payload_checksum"] {
        if available.contains(optional_identity) {
            let blank: i64 = connection
                .query_row(
                    &format!(
                        "SELECT count(*) FROM \"{source}\" WHERE \"{optional_identity}\" IS NULL OR trim(CAST(\"{optional_identity}\" AS VARCHAR)) = ''"
                    ),
                    [],
                    |row| row.get(0),
                )
                .with_context(|| {
                    format!("validating governed LLM {optional_identity} in {source}")
                })?;
            if blank != 0 {
                return Err(anyhow!(
                    "governed LLM source {source} contains {blank} row(s) with blank {optional_identity}"
                ));
            }
        }
    }
    if available.contains("idempotency_key") {
        let inconsistent: i64 = connection
            .query_row(
                &format!(
                    "SELECT count(*) FROM \"{source}\" WHERE idempotency_key <> record_kind || ':' || stable_id || ':r' || CAST(record_revision AS VARCHAR)"
                ),
                [],
                |row| row.get(0),
            )
            .with_context(|| format!("validating governed LLM idempotency keys in {source}"))?;
        if inconsistent != 0 {
            return Err(anyhow!(
                "governed LLM source {source} contains {inconsistent} inconsistent idempotency key(s)"
            ));
        }
    }
    if available.contains("payload_checksum") {
        let malformed: i64 = connection
            .query_row(
                &format!(
                    "SELECT count(*) FROM \"{source}\" WHERE length(payload_checksum) <> 64 OR regexp_matches(payload_checksum, '^[0-9a-f]{{64}}$') = false"
                ),
                [],
                |row| row.get(0),
            )
            .with_context(|| format!("validating governed LLM checksums in {source}"))?;
        if malformed != 0 {
            return Err(anyhow!(
                "governed LLM source {source} contains {malformed} malformed payload checksum(s)"
            ));
        }
    }
    let invalid_semantics: i64 = connection
        .query_row(
            &format!(
                "SELECT count(*) FROM \"{source}\" WHERE occurred_at_ms IS NULL OR observed_at_ms IS NULL OR timestamp_ms IS NULL OR occurred_at_ms < 0 OR observed_at_ms < occurred_at_ms OR timestamp_ms <> occurred_at_ms OR capture_mode IS NULL OR capture_mode NOT IN ('off', 'metadata', 'sanitized', 'full_local_encrypted') OR capture_status IS NULL OR capture_status NOT IN ('complete', 'metadata_only', 'redacted', 'sampled_out', 'policy_denied', 'oversize', 'backpressure_degraded', 'write_failed') OR (capture_mode = 'off' AND (capture_status NOT IN ('policy_denied', 'sampled_out') OR training_eligible_at_capture = true)) OR training_eligible_at_capture IS NULL OR (training_eligible_at_capture = true AND training_exclusion_reason IS NOT NULL) OR (training_eligible_at_capture = false AND (training_exclusion_reason IS NULL OR trim(training_exclusion_reason) = ''))"
            ),
            [],
            |row| row.get(0),
        )
        .with_context(|| format!("validating governed LLM fact semantics in {source}"))?;
    if invalid_semantics != 0 {
        return Err(anyhow!(
            "governed LLM source {source} contains {invalid_semantics} row(s) with invalid timing, capture, or training semantics"
        ));
    }
    if dataset != LlmCanonicalDataset::CaptureGaps {
        let invalid_context: i64 = connection
            .query_row(
                &format!(
                    "SELECT count(*) FROM \"{source}\" WHERE \
                     trace_id IS NULL OR trim(trace_id) = '' OR trim(trace_id) <> trace_id OR \
                     llm_call_id IS NULL OR trim(llm_call_id) = '' OR trim(llm_call_id) <> llm_call_id OR \
                     scope_resolution IS NULL OR scope_resolution NOT IN ('explicit', 'inherited', 'system_default', 'legacy_default') OR \
                     workload_class IS NULL OR workload_class NOT IN ('foreground_chat', 'interactive_task', 'autonomous_task', 'scheduled', 'ambient', 'comms_assist', 'memory', 'evaluation', 'system') OR \
                     call_role IS NULL OR call_role NOT IN ('primary', 'supporting', 'summarizer', 'classifier', 'validator', 'verifier', 'judge', 'recovery', 'title', 'memory') OR \
                     ((parent_call_id IS NULL) <> (parent_relation IS NULL)) OR \
                     parent_call_id = llm_call_id OR \
                     (parent_relation IS NOT NULL AND parent_relation NOT IN ('cascade', 'verifies', 'judges', 'summarizes', 'supports', 'chunk_map', 'chunk_repair', 'chunk_fallback', 'chunk_reduce')) OR \
                     (parent_call_id IS NOT NULL AND trim(parent_call_id) <> parent_call_id) OR \
                     (retry_group_id IS NOT NULL AND (trim(retry_group_id) = '' OR trim(retry_group_id) <> retry_group_id OR retry_group_id = llm_call_id)) OR \
                     (route_decision_id IS NOT NULL AND (trim(route_decision_id) = '' OR trim(route_decision_id) <> route_decision_id)) OR \
                     (task_id IS NOT NULL AND (trim(task_id) = '' OR trim(task_id) <> task_id)) OR \
                     (root_execution_id IS NOT NULL AND (trim(root_execution_id) = '' OR trim(root_execution_id) <> root_execution_id)) OR \
                     (execution_id IS NOT NULL AND (trim(execution_id) = '' OR trim(execution_id) <> execution_id)) OR \
                     (plan_id IS NOT NULL AND (trim(plan_id) = '' OR trim(plan_id) <> plan_id)) OR \
                     (step_id IS NOT NULL AND (trim(step_id) = '' OR trim(step_id) <> step_id)) OR \
                     (iteration_id IS NOT NULL AND (trim(iteration_id) = '' OR trim(iteration_id) <> iteration_id)) OR \
                     (prompt_projection_mode IS NOT NULL AND prompt_projection_mode NOT IN ('bootstrap', 'continuation', 'rebootstrap')) OR \
                     (chat_session_id IS NOT NULL AND (trim(chat_session_id) = '' OR trim(chat_session_id) <> chat_session_id)) OR \
                     (chat_turn_id IS NOT NULL AND (trim(chat_turn_id) = '' OR trim(chat_turn_id) <> chat_turn_id)) OR \
                     (user_message_id IS NOT NULL AND (trim(user_message_id) = '' OR trim(user_message_id) <> user_message_id))"
                ),
                [],
                |row| row.get(0),
            )
            .with_context(|| format!("validating governed LLM trace context in {source}"))?;
        if invalid_context != 0 {
            return Err(anyhow!(
                "governed LLM source {source} contains {invalid_context} row(s) with invalid trace context or vocabulary"
            ));
        }
    }
    let invalid_usage_pricing_timing: i64 = connection
        .query_row(
            &format!(
                "SELECT count(*) FROM \"{source}\" WHERE \
                 (input_tokens IS NOT NULL AND output_tokens IS NOT NULL AND total_tokens IS NOT NULL AND input_tokens + output_tokens <> total_tokens) OR \
                 (reasoning_tokens IS NOT NULL AND output_tokens IS NOT NULL AND reasoning_tokens > output_tokens) OR \
                 (cache_read_tokens IS NOT NULL AND input_tokens IS NOT NULL AND cache_read_tokens > input_tokens) OR \
                 (cache_creation_tokens IS NOT NULL AND input_tokens IS NOT NULL AND cache_creation_tokens > input_tokens) OR \
                 (cache_read_tokens IS NOT NULL AND cache_creation_tokens IS NOT NULL AND input_tokens IS NOT NULL AND cache_read_tokens + cache_creation_tokens > input_tokens) OR \
                 (audio_input_tokens IS NOT NULL AND input_tokens IS NOT NULL AND audio_input_tokens > input_tokens) OR \
                 (audio_output_tokens IS NOT NULL AND output_tokens IS NOT NULL AND audio_output_tokens > output_tokens) OR \
                 (audio_cached_tokens IS NOT NULL AND cache_read_tokens IS NOT NULL AND audio_cached_tokens > cache_read_tokens) OR \
                 ((audio_input_tokens IS NOT NULL OR audio_output_tokens IS NOT NULL OR audio_cached_tokens IS NOT NULL) AND (audio_input_tokens IS NULL OR audio_output_tokens IS NULL OR audio_cached_tokens IS NULL OR input_tokens IS NULL OR output_tokens IS NULL OR cache_read_tokens IS NULL OR cache_creation_tokens IS NULL)) OR \
                 ((audio_input_tokens IS NOT NULL OR audio_output_tokens IS NOT NULL OR audio_cached_tokens IS NOT NULL) AND cache_creation_tokens <> 0) OR \
                 (audio_input_tokens IS NOT NULL AND CAST(audio_input_tokens AS HUGEINT) + CAST(cache_read_tokens AS HUGEINT) + CAST(cache_creation_tokens AS HUGEINT) > CAST(input_tokens AS HUGEINT)) OR \
                 ((pricing_version IS NULL) <> (cost_source IS NULL)) OR \
                 (pricing_version IS NOT NULL AND trim(pricing_version) = '') OR \
                 (pricing_version IS NOT NULL AND NOT regexp_full_match(CAST(pricing_version AS VARCHAR), '[A-Za-z0-9_./:@-]{{1,128}}')) OR \
                 (pricing_version LIKE 'pricing-row-v1:%' AND NOT regexp_full_match(CAST(pricing_version AS VARCHAR), 'pricing-row-v1:[0-9a-f]{{64}}')) OR \
                 (cost_source IS NOT NULL AND cost_source NOT IN ('provider', 'computed', 'estimated', 'local', 'unknown')) OR \
                 (cost_source = 'local' AND cost_usd IS NOT NULL AND cost_usd <> 0) OR \
                 (cost_source IS NULL AND (input_cost_usd IS NOT NULL OR output_cost_usd IS NOT NULL OR reasoning_cost_usd IS NOT NULL OR cache_cost_usd IS NOT NULL OR cost_usd IS NOT NULL)) OR \
                 (cost_source = 'unknown' AND (input_cost_usd IS NOT NULL OR output_cost_usd IS NOT NULL OR reasoning_cost_usd IS NOT NULL OR cache_cost_usd IS NOT NULL OR cost_usd IS NOT NULL)) OR \
                 (cost_source IN ('provider', 'computed', 'estimated') AND input_cost_usd IS NULL AND output_cost_usd IS NULL AND reasoning_cost_usd IS NULL AND cache_cost_usd IS NULL AND cost_usd IS NULL) OR \
                 (input_cost_usd IS NOT NULL AND (NOT isfinite(input_cost_usd) OR input_cost_usd < 0)) OR \
                 (output_cost_usd IS NOT NULL AND (NOT isfinite(output_cost_usd) OR output_cost_usd < 0)) OR \
                 (reasoning_cost_usd IS NOT NULL AND (NOT isfinite(reasoning_cost_usd) OR reasoning_cost_usd < 0)) OR \
                 (cache_cost_usd IS NOT NULL AND (NOT isfinite(cache_cost_usd) OR cache_cost_usd < 0)) OR \
                 (cost_usd IS NOT NULL AND (NOT isfinite(cost_usd) OR cost_usd < 0)) OR \
                 (created_at_ms IS NOT NULL AND created_at_ms < 0) OR \
                 (submitted_at_ms IS NOT NULL AND (submitted_at_ms < 0 OR (created_at_ms IS NOT NULL AND submitted_at_ms < created_at_ms))) OR \
                 (started_at_ms IS NOT NULL AND (started_at_ms < 0 OR (submitted_at_ms IS NOT NULL AND started_at_ms < submitted_at_ms) OR (created_at_ms IS NOT NULL AND started_at_ms < created_at_ms))) OR \
                 (first_token_at_ms IS NOT NULL AND (first_token_at_ms < 0 OR started_at_ms IS NULL OR first_token_at_ms < started_at_ms)) OR \
                 (completed_at_ms IS NOT NULL AND (completed_at_ms < 0 OR (first_token_at_ms IS NOT NULL AND completed_at_ms < first_token_at_ms) OR (started_at_ms IS NOT NULL AND completed_at_ms < started_at_ms))) OR \
                 ((first_token_at_ms IS NULL) <> (ttft_ms IS NULL)) OR \
                 (ttft_ms IS NOT NULL AND started_at_ms IS NULL) OR \
                 (generation_after_ttft_ms IS NOT NULL AND ttft_ms IS NULL) OR \
                 (ttft_ms IS NOT NULL AND (latency_ms IS NOT NULL AND ttft_ms > latency_ms OR started_at_ms IS NOT NULL AND first_token_at_ms IS NOT NULL AND ttft_ms <> first_token_at_ms - started_at_ms)) OR \
                 (ttft_ms IS NOT NULL AND generation_after_ttft_ms IS NOT NULL AND latency_ms IS NOT NULL AND CAST(ttft_ms AS HUGEINT) + CAST(generation_after_ttft_ms AS HUGEINT) > CAST(latency_ms AS HUGEINT)) OR \
                 (latency_ms IS NOT NULL AND (COALESCE(queue_wait_ms, 0) + COALESCE(local_prep_ms, 0) + COALESCE(provider_execution_ms, 0) + COALESCE(parse_ms, 0) + COALESCE(validation_ms, 0) > latency_ms)) OR \
                 (COALESCE(parse_attempted, false) <> (parse_success IS NOT NULL)) OR \
                 (COALESCE(schema_validation_attempted, false) <> (schema_validation_success IS NOT NULL)) OR \
                 (COALESCE(contract_validation_attempted, false) <> (contract_validation_success IS NOT NULL)) OR \
                 (COALESCE(response_present, false) = false AND (COALESCE(parse_attempted, false) OR COALESCE(schema_validation_attempted, false) OR COALESCE(contract_validation_attempted, false))) OR \
                 ((COALESCE(parse_success = false, false) OR COALESCE(schema_validation_success = false, false) OR COALESCE(contract_validation_success = false, false)) <> (validation_error_class IS NOT NULL)) OR \
                 (COALESCE(discarded_before_use, false) <> (discard_reason IS NOT NULL)) OR \
                 (superseded_by_call_id IS NOT NULL AND (trim(superseded_by_call_id) = '' OR trim(superseded_by_call_id) <> superseded_by_call_id)) OR \
                 (dispatch_job_id IS NOT NULL AND (trim(dispatch_job_id) = '' OR trim(dispatch_job_id) <> dispatch_job_id)) OR \
                 (requested_profile IS NOT NULL AND (trim(requested_profile) = '' OR trim(requested_profile) <> requested_profile)) OR \
                 (selected_profile IS NOT NULL AND (trim(selected_profile) = '' OR trim(selected_profile) <> selected_profile)) OR \
                 (effective_profile IS NOT NULL AND (trim(effective_profile) = '' OR trim(effective_profile) <> effective_profile)) OR \
                 (provider IS NOT NULL AND (trim(provider) = '' OR trim(provider) <> provider)) OR \
                 (model IS NOT NULL AND (trim(model) = '' OR trim(model) <> model)) OR \
                 (model_revision IS NOT NULL AND (trim(model_revision) = '' OR trim(model_revision) <> model_revision)) OR \
                 (provider_response_id IS NOT NULL AND (trim(provider_response_id) = '' OR trim(provider_response_id) <> provider_response_id)) OR \
                 (operation IS NULL OR NOT regexp_full_match(CAST(operation AS VARCHAR), '[A-Za-z0-9_./:-]{{1,128}}')) OR \
                 (operation_family IS NOT NULL AND NOT regexp_full_match(CAST(operation_family AS VARCHAR), '[A-Za-z0-9_./:-]{{1,128}}')) OR \
                 (capability IS NOT NULL AND NOT regexp_full_match(CAST(capability AS VARCHAR), '[A-Za-z0-9_./:-]{{1,128}}')) OR \
                 (priority_lane IS NOT NULL AND NOT regexp_full_match(CAST(priority_lane AS VARCHAR), '[A-Za-z0-9_./:-]{{1,128}}')) OR \
                 (source_surface IS NOT NULL AND NOT regexp_full_match(CAST(source_surface AS VARCHAR), '[A-Za-z0-9_./:-]{{1,128}}')) OR \
                 (origin_channel IS NOT NULL AND NOT regexp_full_match(CAST(origin_channel AS VARCHAR), '[A-Za-z0-9_./:-]{{1,128}}')) OR \
                 (response_kind IS NOT NULL AND NOT regexp_full_match(CAST(response_kind AS VARCHAR), '[A-Za-z0-9_./:-]{{1,128}}')) OR \
                 (error_class IS NOT NULL AND NOT regexp_full_match(CAST(error_class AS VARCHAR), '[A-Za-z0-9_./:-]{{1,128}}')) OR \
                 (error_code IS NOT NULL AND NOT regexp_full_match(CAST(error_code AS VARCHAR), '[A-Za-z0-9_./:-]{{1,128}}')) OR \
                 (finish_reason IS NOT NULL AND NOT regexp_full_match(CAST(finish_reason AS VARCHAR), '[A-Za-z0-9_./:-]{{1,128}}')) OR \
                 (validation_error_class IS NOT NULL AND NOT regexp_full_match(CAST(validation_error_class AS VARCHAR), '[A-Za-z0-9_./:-]{{1,128}}')) OR \
                 (discard_reason IS NOT NULL AND NOT regexp_full_match(CAST(discard_reason AS VARCHAR), '[A-Za-z0-9_./:-]{{1,128}}')) OR \
                 (training_exclusion_reason IS NOT NULL AND NOT regexp_full_match(CAST(training_exclusion_reason AS VARCHAR), '[A-Za-z0-9_./:-]{{1,128}}')) OR \
                 (gap_reason IS NOT NULL AND NOT regexp_full_match(CAST(gap_reason AS VARCHAR), '[A-Za-z0-9_./:-]{{1,128}}'))"
            ),
            [],
            |row| row.get(0),
        )
        .with_context(|| {
            format!("validating governed LLM usage, pricing, timing and validation in {source}")
        })?;
    if invalid_usage_pricing_timing != 0 {
        return Err(anyhow!(
            "governed LLM source {source} contains {invalid_usage_pricing_timing} row(s) with invalid usage, pricing, timing, or validation facts"
        ));
    }
    let terminal_payload = "transport_success IS NOT NULL OR success IS NOT NULL OR provider_attempt_count IS NOT NULL OR provider_response_id IS NOT NULL OR response_kind IS NOT NULL OR error_class IS NOT NULL OR error_code IS NOT NULL OR error IS NOT NULL OR finish_reason IS NOT NULL OR refusal IS NOT NULL OR truncated IS NOT NULL OR completed_at_ms IS NOT NULL OR queue_wait_ms IS NOT NULL OR local_prep_ms IS NOT NULL OR provider_execution_ms IS NOT NULL OR first_token_at_ms IS NOT NULL OR ttft_ms IS NOT NULL OR generation_after_ttft_ms IS NOT NULL OR parse_ms IS NOT NULL OR validation_ms IS NOT NULL OR latency_ms IS NOT NULL OR input_tokens IS NOT NULL OR output_tokens IS NOT NULL OR reasoning_tokens IS NOT NULL OR cache_read_tokens IS NOT NULL OR cache_creation_tokens IS NOT NULL OR audio_input_tokens IS NOT NULL OR audio_output_tokens IS NOT NULL OR audio_cached_tokens IS NOT NULL OR total_tokens IS NOT NULL OR pricing_version IS NOT NULL OR cost_source IS NOT NULL OR input_cost_usd IS NOT NULL OR output_cost_usd IS NOT NULL OR reasoning_cost_usd IS NOT NULL OR cache_cost_usd IS NOT NULL OR cost_usd IS NOT NULL OR response_present IS NOT NULL OR parse_attempted IS NOT NULL OR parse_success IS NOT NULL OR schema_validation_attempted IS NOT NULL OR schema_validation_success IS NOT NULL OR contract_validation_attempted IS NOT NULL OR contract_validation_success IS NOT NULL OR validation_error_class IS NOT NULL OR discarded_before_use IS NOT NULL OR discard_reason IS NOT NULL OR superseded_by_call_id IS NOT NULL";
    let attempt_terminal_payload = "transport_success IS NOT NULL OR success IS NOT NULL OR error_class IS NOT NULL OR error_code IS NOT NULL OR error IS NOT NULL OR finish_reason IS NOT NULL OR refusal IS NOT NULL OR truncated IS NOT NULL OR provider_execution_ms IS NOT NULL OR generation_after_ttft_ms IS NOT NULL OR parse_ms IS NOT NULL OR validation_ms IS NOT NULL OR latency_ms IS NOT NULL OR input_tokens IS NOT NULL OR output_tokens IS NOT NULL OR reasoning_tokens IS NOT NULL OR cache_read_tokens IS NOT NULL OR cache_creation_tokens IS NOT NULL OR audio_input_tokens IS NOT NULL OR audio_output_tokens IS NOT NULL OR audio_cached_tokens IS NOT NULL OR total_tokens IS NOT NULL OR pricing_version IS NOT NULL OR cost_source IS NOT NULL OR input_cost_usd IS NOT NULL OR output_cost_usd IS NOT NULL OR reasoning_cost_usd IS NOT NULL OR cache_cost_usd IS NOT NULL OR cost_usd IS NOT NULL";
    let tool_payload = "model_tool_call_id IS NOT NULL OR tool_execution_id IS NOT NULL OR branch_id IS NOT NULL OR tool_name IS NOT NULL OR tool_family IS NOT NULL OR tool_lineage_stage IS NOT NULL OR tool_lineage_stage_index IS NOT NULL OR arguments_fingerprint IS NOT NULL OR result_ref IS NOT NULL OR canonical_event_ref IS NOT NULL OR related_execution_ids_json IS NOT NULL OR consumed_by_call_id IS NOT NULL OR tool_name_known IS NOT NULL OR tool_arguments_parsed IS NOT NULL OR tool_schema_matched IS NOT NULL OR tool_policy_allowed IS NOT NULL OR tool_approval_required IS NOT NULL OR tool_approval_obtained IS NOT NULL OR tool_transport_ran IS NOT NULL OR tool_reported_success IS NOT NULL OR tool_result_validation_success IS NOT NULL OR tool_outcome IS NOT NULL OR tool_failure_owner IS NOT NULL OR tool_failure_code IS NOT NULL OR tool_side_effect_state IS NOT NULL OR tool_branch_state IS NOT NULL OR on_successful_path IS NOT NULL OR same_tool_arguments_count IS NOT NULL OR observation_action_cycle_count IS NOT NULL OR recovered_after_failure IS NOT NULL OR linkage_gap IS NOT NULL";
    let attempt_forbidden_payload = "operation_family IS NOT NULL OR capability IS NOT NULL OR priority_lane IS NOT NULL OR source_surface IS NOT NULL OR origin_channel IS NOT NULL OR requested_profile IS NOT NULL OR selected_profile IS NOT NULL OR prompt_projection_mode IS NOT NULL OR call_terminal_state IS NOT NULL OR provider_attempt_count IS NOT NULL OR provider_response_id IS NOT NULL OR response_kind IS NOT NULL OR response_present IS NOT NULL OR parse_attempted IS NOT NULL OR parse_success IS NOT NULL OR schema_validation_attempted IS NOT NULL OR schema_validation_success IS NOT NULL OR contract_validation_attempted IS NOT NULL OR contract_validation_success IS NOT NULL OR validation_error_class IS NOT NULL OR discarded_before_use IS NOT NULL OR discard_reason IS NOT NULL OR superseded_by_call_id IS NOT NULL";
    let tool_forbidden_payload = format!(
        "provider_attempt_id IS NOT NULL OR provider_attempt_index IS NOT NULL OR dispatch_job_id IS NOT NULL OR operation_family IS NOT NULL OR capability IS NOT NULL OR priority_lane IS NOT NULL OR origin_channel IS NOT NULL OR requested_profile IS NOT NULL OR selected_profile IS NOT NULL OR prompt_projection_mode IS NOT NULL OR effective_profile IS NOT NULL OR profile IS NOT NULL OR provider IS NOT NULL OR model IS NOT NULL OR model_revision IS NOT NULL OR call_terminal_state IS NOT NULL OR attempt_terminal_state IS NOT NULL OR gap_id IS NOT NULL OR gap_reason IS NOT NULL OR missing_record_count IS NOT NULL OR first_missing_at_ms IS NOT NULL OR last_missing_at_ms IS NOT NULL OR {terminal_payload}"
    );
    let gap_forbidden_payload = "trace_id IS NOT NULL OR provider_attempt_id IS NOT NULL OR provider_attempt_index IS NOT NULL OR dispatch_job_id IS NOT NULL OR parent_call_id IS NOT NULL OR parent_relation IS NOT NULL OR retry_group_id IS NOT NULL OR route_decision_id IS NOT NULL OR scope_resolution IS NOT NULL OR task_id IS NOT NULL OR root_execution_id IS NOT NULL OR execution_id IS NOT NULL OR plan_id IS NOT NULL OR step_id IS NOT NULL OR iteration_id IS NOT NULL OR prompt_projection_mode IS NOT NULL OR chat_session_id IS NOT NULL OR chat_turn_id IS NOT NULL OR user_message_id IS NOT NULL OR workload_class IS NOT NULL OR call_role IS NOT NULL OR operation_family IS NOT NULL OR capability IS NOT NULL OR priority_lane IS NOT NULL OR source_surface IS NOT NULL OR origin_channel IS NOT NULL OR requested_profile IS NOT NULL OR selected_profile IS NOT NULL OR effective_profile IS NOT NULL OR profile IS NOT NULL OR provider IS NOT NULL OR model IS NOT NULL OR model_revision IS NOT NULL OR call_terminal_state IS NOT NULL OR attempt_terminal_state IS NOT NULL";
    let dataset_identity_predicate = match dataset {
        LlmCanonicalDataset::Calls => {
            format!("llm_call_id IS NULL OR trim(llm_call_id) = '' OR stable_id <> llm_call_id OR {tool_payload} OR lifecycle_phase <> CASE record_revision WHEN 1 THEN 'started' WHEN 2 THEN 'completed' END OR provider_attempt_id IS NOT NULL OR provider_attempt_index IS NOT NULL OR effective_profile IS NOT NULL OR provider IS NOT NULL OR model IS NOT NULL OR model_revision IS NOT NULL OR attempt_terminal_state IS NOT NULL OR (record_revision = 1 AND (created_at_ms IS NULL OR created_at_ms <> occurred_at_ms OR profile IS DISTINCT FROM selected_profile OR call_terminal_state IS NOT NULL OR {terminal_payload})) OR (record_revision = 2 AND (operation_family IS NOT NULL OR capability IS NOT NULL OR priority_lane IS NOT NULL OR source_surface IS NOT NULL OR origin_channel IS NOT NULL OR requested_profile IS NOT NULL OR selected_profile IS NOT NULL OR prompt_projection_mode IS NOT NULL OR profile IS NOT NULL OR created_at_ms IS NULL OR completed_at_ms IS NULL OR completed_at_ms <> occurred_at_ms OR latency_ms IS NULL OR call_terminal_state IS NULL OR call_terminal_state NOT IN ('succeeded', 'failed', 'cancelled', 'tombstoned') OR provider_attempt_count IS NULL OR response_present IS NULL OR parse_attempted IS NULL OR schema_validation_attempted IS NULL OR contract_validation_attempted IS NULL OR discarded_before_use IS NULL OR transport_success IS DISTINCT FROM (call_terminal_state = 'succeeded') OR success IS DISTINCT FROM (call_terminal_state = 'succeeded'))) OR (record_revision = 2 AND call_terminal_state = 'succeeded' AND ((provider_attempt_count = 0 AND response_kind IS DISTINCT FROM 'logical_chunk_summary' AND response_kind IS DISTINCT FROM 'harness_aggregate') OR (response_kind = 'logical_chunk_summary' AND provider_attempt_count <> 0) OR response_present IS DISTINCT FROM true OR error_class IS NOT NULL)) OR (record_revision = 2 AND response_kind = 'harness_aggregate' AND (provider_attempt_count <> 0 OR dispatch_job_id IS NOT NULL OR provider_response_id IS NOT NULL OR cost_source IS NULL OR cost_source NOT IN ('estimated', 'unknown'))) OR (record_revision = 2 AND response_kind = 'logical_chunk_summary' AND (provider_attempt_count <> 0 OR input_tokens IS NOT NULL OR output_tokens IS NOT NULL OR reasoning_tokens IS NOT NULL OR cache_read_tokens IS NOT NULL OR cache_creation_tokens IS NOT NULL OR audio_input_tokens IS NOT NULL OR audio_output_tokens IS NOT NULL OR audio_cached_tokens IS NOT NULL OR total_tokens IS NOT NULL OR cost_usd IS NOT NULL)) OR (record_revision = 2 AND call_terminal_state IN ('failed', 'cancelled', 'tombstoned') AND (error_class IS NULL OR trim(error_class) = ''))")
        },
        LlmCanonicalDataset::ProviderAttempts => {
            format!("llm_call_id IS NULL OR trim(llm_call_id) = '' OR provider_attempt_id IS NULL OR trim(provider_attempt_id) = '' OR stable_id <> provider_attempt_id OR provider_attempt_index IS NULL OR provider_attempt_index <= 0 OR provider_attempt_id <> llm_call_id || ':a' || CAST(provider_attempt_index AS VARCHAR) OR provider IS NULL OR trim(provider) = '' OR model IS NULL OR trim(model) = '' OR profile IS DISTINCT FROM effective_profile OR {attempt_forbidden_payload} OR {tool_payload} OR lifecycle_phase <> CASE record_revision WHEN 1 THEN 'started' WHEN 2 THEN 'first_token' WHEN 3 THEN 'completed' END OR (record_revision = 1 AND (started_at_ms IS NULL OR started_at_ms <> occurred_at_ms OR first_token_at_ms IS NOT NULL OR completed_at_ms IS NOT NULL OR attempt_terminal_state IS NOT NULL OR {attempt_terminal_payload})) OR (record_revision = 2 AND (started_at_ms IS NULL OR first_token_at_ms IS NULL OR first_token_at_ms <> occurred_at_ms OR ttft_ms IS NULL OR completed_at_ms IS NOT NULL OR attempt_terminal_state IS NOT NULL OR {attempt_terminal_payload})) OR (record_revision = 3 AND (completed_at_ms IS NULL OR completed_at_ms <> occurred_at_ms OR ((first_token_at_ms IS NULL) <> (ttft_ms IS NULL)) OR ((started_at_ms IS NULL) <> (latency_ms IS NULL)) OR (started_at_ms IS NULL AND (first_token_at_ms IS NOT NULL OR provider_execution_ms IS NOT NULL OR generation_after_ttft_ms IS NOT NULL)) OR attempt_terminal_state IS NULL OR attempt_terminal_state NOT IN ('succeeded', 'failed', 'cancelled', 'timed_out') OR transport_success IS DISTINCT FROM (attempt_terminal_state = 'succeeded') OR success IS DISTINCT FROM (attempt_terminal_state = 'succeeded'))) OR (record_revision = 3 AND attempt_terminal_state = 'succeeded' AND error_class IS NOT NULL) OR (record_revision = 3 AND attempt_terminal_state IN ('failed', 'cancelled', 'timed_out') AND (error_class IS NULL OR trim(error_class) = ''))")
        },
        LlmCanonicalDataset::ToolCalls => {
            format!("llm_call_id IS NULL OR trim(llm_call_id) = '' OR model_tool_call_id IS NULL OR trim(model_tool_call_id) = '' OR tool_execution_id IS NULL OR trim(tool_execution_id) = '' OR tool_execution_id <> llm_call_id || ':tool:' || model_tool_call_id OR stable_id <> tool_execution_id OR branch_id IS NULL OR trim(branch_id) = '' OR tool_name IS NULL OR NOT regexp_full_match(CAST(tool_name AS VARCHAR), '[A-Za-z0-9_./:-]{{1,128}}') OR tool_lineage_stage IS NULL OR lifecycle_phase <> tool_lineage_stage OR tool_lineage_stage_index IS NULL OR source_surface IS NULL OR NOT regexp_full_match(CAST(source_surface AS VARCHAR), '[A-Za-z0-9_./:-]{{1,128}}') OR arguments_fingerprint IS NULL OR NOT regexp_full_match(arguments_fingerprint, '[0-9a-f]{{64}}') OR tool_outcome IS NULL OR tool_outcome NOT IN ('pending', 'succeeded', 'failed', 'denied', 'cancelled', 'timed_out', 'abandoned', 'superseded') OR (tool_failure_owner IS NOT NULL AND tool_failure_owner NOT IN ('model', 'arguments', 'schema', 'policy', 'resource_authority', 'approval', 'runtime', 'tool', 'result_validation', 'external_provider', 'cancellation', 'unknown')) OR tool_side_effect_state IS NULL OR tool_side_effect_state NOT IN ('none', 'unknown', 'pending', 'remained', 'reversed', 'rollback_failed') OR tool_branch_state IS NULL OR tool_branch_state NOT IN ('active', 'successful', 'abandoned', 'superseded', 'unknown') OR same_tool_arguments_count IS NULL OR same_tool_arguments_count <= 0 OR observation_action_cycle_count IS NULL OR recovered_after_failure IS NULL OR capture_mode <> 'metadata' OR capture_status <> 'complete' OR training_eligible_at_capture <> false OR training_exclusion_reason IS DISTINCT FROM 'lineage_requires_outcome_maturity' OR (tool_lineage_stage = 'proposed' AND (record_revision <> 1 OR tool_lineage_stage_index <> 0)) OR (tool_lineage_stage = 'name_validated' AND (record_revision <> 10 OR tool_lineage_stage_index <> 0)) OR (tool_lineage_stage = 'arguments_parsed' AND (record_revision <> 20 OR tool_lineage_stage_index <> 0)) OR (tool_lineage_stage = 'schema_validated' AND (record_revision <> 30 OR tool_lineage_stage_index <> 0)) OR (tool_lineage_stage = 'authorization_resolved' AND (record_revision <> 40 OR tool_lineage_stage_index <> 0)) OR (tool_lineage_stage = 'approval_resolved' AND (record_revision <> 50 OR tool_lineage_stage_index <> 0)) OR (tool_lineage_stage = 'execution_started' AND (record_revision <> 1000 + tool_lineage_stage_index OR tool_lineage_stage_index NOT BETWEEN 1 AND 999)) OR (tool_lineage_stage = 'execution_finished' AND (record_revision <> 2000 + tool_lineage_stage_index OR tool_lineage_stage_index NOT BETWEEN 1 AND 999)) OR (tool_lineage_stage = 'result_validated' AND (record_revision <> 3000 OR tool_lineage_stage_index <> 0)) OR (tool_lineage_stage = 'result_consumed' AND (record_revision <> 4000 + tool_lineage_stage_index OR tool_lineage_stage_index NOT BETWEEN 1 AND 999 OR consumed_by_call_id IS NULL OR consumed_by_call_id = llm_call_id OR result_ref IS NULL)) OR (tool_lineage_stage = 'branch_materialized' AND (record_revision <> 5000 OR tool_lineage_stage_index <> 0)) OR (tool_lineage_stage = 'rollback_started' AND (record_revision <> 6000 + tool_lineage_stage_index OR tool_lineage_stage_index NOT BETWEEN 1 AND 999)) OR (tool_lineage_stage = 'rollback_finished' AND (record_revision <> 7000 + tool_lineage_stage_index OR tool_lineage_stage_index NOT BETWEEN 1 AND 999)) OR (tool_lineage_stage = 'linkage_gap' AND (record_revision <> 8000 + tool_lineage_stage_index OR tool_lineage_stage_index NOT BETWEEN 1 AND 999 OR linkage_gap IS NULL)) OR (tool_lineage_stage <> 'result_consumed' AND consumed_by_call_id IS NOT NULL) OR (tool_lineage_stage <> 'linkage_gap' AND linkage_gap IS NOT NULL) OR ((tool_outcome IN ('failed', 'denied', 'cancelled', 'timed_out')) <> (tool_failure_owner IS NOT NULL)) OR (tool_failure_code IS NOT NULL AND NOT regexp_full_match(CAST(tool_failure_code AS VARCHAR), '[A-Za-z0-9_./:-]{{1,128}}')) OR (linkage_gap IS NOT NULL AND NOT regexp_full_match(CAST(linkage_gap AS VARCHAR), '[A-Za-z0-9_./:-]{{1,128}}')) OR (tool_side_effect_state IN ('reversed', 'rollback_failed') AND tool_lineage_stage <> 'rollback_finished') OR (tool_branch_state = 'successful' AND on_successful_path IS DISTINCT FROM true) OR (related_execution_ids_json IS NOT NULL AND (tool_lineage_stage NOT IN ('execution_finished', 'branch_materialized') OR NOT json_valid(related_execution_ids_json) OR json_type(related_execution_ids_json) <> 'ARRAY' OR json_array_length(related_execution_ids_json) NOT BETWEEN 1 AND 64)) OR {tool_forbidden_payload}")
        },
        LlmCanonicalDataset::CaptureGaps => {
            format!("gap_id IS NULL OR trim(gap_id) = '' OR trim(gap_id) <> gap_id OR stable_id <> gap_id OR lifecycle_phase <> 'reported' OR missing_record_count IS NULL OR missing_record_count <= 0 OR gap_reason IS NULL OR trim(gap_reason) = '' OR (llm_call_id IS NOT NULL AND (trim(llm_call_id) = '' OR trim(llm_call_id) <> llm_call_id)) OR first_missing_at_ms IS NULL OR last_missing_at_ms IS NULL OR first_missing_at_ms < 0 OR first_missing_at_ms > last_missing_at_ms OR last_missing_at_ms > occurred_at_ms OR observed_at_ms <> occurred_at_ms OR capture_mode <> 'metadata' OR capture_status <> 'backpressure_degraded' OR training_eligible_at_capture <> false OR training_exclusion_reason IS DISTINCT FROM 'capture_gap' OR {gap_forbidden_payload} OR {terminal_payload} OR {tool_payload}")
        },
    };
    let invalid_dataset_rows: i64 = connection
        .query_row(
            &format!("SELECT count(*) FROM \"{source}\" WHERE {dataset_identity_predicate}"),
            [],
            |row| row.get(0),
        )
        .with_context(|| format!("validating governed LLM dataset identities in {source}"))?;
    if invalid_dataset_rows != 0 {
        return Err(anyhow!(
            "governed LLM source {source} contains {invalid_dataset_rows} row(s) with invalid dataset identity or lifecycle semantics"
        ));
    }
    if dataset == LlmCanonicalDataset::ToolCalls {
        let invalid_stage_facts: i64 = connection
            .query_row(
                &format!(
                    "SELECT count(*) FROM \"{source}\" WHERE \
                     (tool_lineage_stage = 'name_validated' AND tool_name_known IS NULL) OR \
                     (tool_lineage_stage = 'arguments_parsed' AND tool_arguments_parsed IS NULL) OR \
                     (tool_lineage_stage = 'schema_validated' AND tool_schema_matched IS NULL) OR \
                     (tool_lineage_stage = 'authorization_resolved' AND tool_policy_allowed IS NULL) OR \
                     (tool_lineage_stage = 'approval_resolved' AND (tool_approval_required IS NULL OR tool_approval_obtained IS NULL)) OR \
                     (tool_approval_obtained = true AND tool_approval_required IS DISTINCT FROM true) OR \
                     (tool_lineage_stage = 'execution_started' AND tool_transport_ran IS NULL) OR \
                     (tool_lineage_stage = 'execution_finished' AND tool_reported_success IS NULL) OR \
                     (tool_lineage_stage = 'result_validated' AND tool_result_validation_success IS NULL)"
                ),
                [],
                |row| row.get(0),
            )
            .with_context(|| {
                format!("validating governed tool-lineage stage facts in {source}")
            })?;
        if invalid_stage_facts != 0 {
            return Err(anyhow!(
                "governed LLM source {source} contains {invalid_stage_facts} row(s) with missing or contradictory tool-stage classifications"
            ));
        }
    }
    let duplicate_revisions: i64 = connection
        .query_row(
            &format!(
                "SELECT count(*) FROM (SELECT stable_id, record_revision, count(*) AS copies FROM \"{source}\" GROUP BY stable_id, record_revision HAVING count(*) <> 1)"
            ),
            [],
            |row| row.get(0),
        )
        .with_context(|| format!("validating governed LLM revision uniqueness in {source}"))?;
    if duplicate_revisions != 0 {
        return Err(anyhow!(
            "governed LLM source {source} contains {duplicate_revisions} duplicate stable revision key(s)"
        ));
    }
    let duplicate_sequences: i64 = connection
        .query_row(
            &format!(
                "SELECT count(*) FROM (SELECT journal_sequence, count(*) AS copies FROM \"{source}\" GROUP BY journal_sequence HAVING count(*) <> 1)"
            ),
            [],
            |row| row.get(0),
        )
        .with_context(|| format!("validating governed LLM journal sequences in {source}"))?;
    if duplicate_sequences != 0 {
        return Err(anyhow!(
            "governed LLM source {source} contains {duplicate_sequences} duplicate journal sequence(s)"
        ));
    }
    validate_revision_lifecycle_consistency(connection, source, dataset)?;
    Ok(())
}

fn null_sensitive_variant_count(column: &str) -> String {
    format!(
        "count(DISTINCT \"{column}\") + CASE WHEN count(*) FILTER (WHERE \"{column}\" IS NULL) > 0 THEN 1 ELSE 0 END"
    )
}

fn nonnull_variant_count(column: &str) -> String {
    format!("count(DISTINCT \"{column}\")")
}

/// Re-apply the journal's immutable-lifecycle invariants at the governed read
/// boundary. This is intentionally independent of the writer: copied,
/// restored, or manually altered Parquet must not become trusted merely
/// because each individual revision is locally well-formed.
fn validate_revision_lifecycle_consistency(
    connection: &Connection,
    source: &str,
    dataset: LlmCanonicalDataset,
) -> Result<()> {
    if dataset == LlmCanonicalDataset::CaptureGaps {
        return Ok(());
    }
    let mut immutable = vec![
        null_sensitive_variant_count("trace_id"),
        null_sensitive_variant_count("llm_call_id"),
        null_sensitive_variant_count("scope_resolution"),
        null_sensitive_variant_count("workload_class"),
        null_sensitive_variant_count("call_role"),
        null_sensitive_variant_count("root_execution_id"),
        null_sensitive_variant_count("execution_id"),
        null_sensitive_variant_count("task_id"),
        null_sensitive_variant_count("plan_id"),
        null_sensitive_variant_count("step_id"),
        null_sensitive_variant_count("iteration_id"),
        null_sensitive_variant_count("chat_session_id"),
        null_sensitive_variant_count("chat_turn_id"),
        null_sensitive_variant_count("user_message_id"),
        null_sensitive_variant_count("parent_call_id"),
        null_sensitive_variant_count("parent_relation"),
        null_sensitive_variant_count("retry_group_id"),
        null_sensitive_variant_count("route_decision_id"),
        null_sensitive_variant_count("operation"),
    ];
    match dataset {
        LlmCanonicalDataset::Calls => {
            // A dispatch id is unavailable at call-start for direct mappings
            // and may be enriched at completion, but two non-null owners are
            // never valid. Creation time is present in both call revisions.
            immutable.push(nonnull_variant_count("dispatch_job_id"));
            immutable.push(null_sensitive_variant_count("created_at_ms"));
        },
        LlmCanonicalDataset::ProviderAttempts => {
            immutable.extend([
                null_sensitive_variant_count("provider_attempt_id"),
                null_sensitive_variant_count("provider_attempt_index"),
                null_sensitive_variant_count("provider"),
                null_sensitive_variant_count("model"),
                nonnull_variant_count("dispatch_job_id"),
                nonnull_variant_count("effective_profile"),
                nonnull_variant_count("model_revision"),
                nonnull_variant_count("started_at_ms"),
                nonnull_variant_count("first_token_at_ms"),
                nonnull_variant_count("completed_at_ms"),
            ]);
        },
        LlmCanonicalDataset::ToolCalls => {
            immutable.extend([
                null_sensitive_variant_count("model_tool_call_id"),
                null_sensitive_variant_count("tool_execution_id"),
                null_sensitive_variant_count("branch_id"),
                null_sensitive_variant_count("tool_name"),
                null_sensitive_variant_count("tool_family"),
                null_sensitive_variant_count("arguments_fingerprint"),
                null_sensitive_variant_count("source_surface"),
            ]);
        },
        LlmCanonicalDataset::CaptureGaps => unreachable!("returned above"),
    }
    let invalid: i64 = connection
        .query_row(
            &format!(
                "SELECT count(*) FROM (SELECT stable_id FROM \"{source}\" GROUP BY stable_id HAVING {})",
                immutable
                    .iter()
                    .map(|expression| format!("{expression} > 1"))
                    .collect::<Vec<_>>()
                    .join(" OR ")
            ),
            [],
            |row| row.get(0),
        )
        .with_context(|| format!("validating immutable LLM lifecycle fields in {source}"))?;
    if invalid != 0 {
        return Err(anyhow!(
            "governed LLM source {source} contains {invalid} stable lifecycle(s) with immutable identity, route, operation, or timing drift"
        ));
    }
    Ok(())
}

/// Validate relationships that span the canonical fact datasets after each
/// source has independently passed its row and revision checks.
fn validate_cross_dataset_lifecycle(connection: &Connection) -> Result<()> {
    let duplicate_sequences: i64 = connection
        .query_row(
            "SELECT count(*) FROM (\
             SELECT journal_sequence, count(*) AS copies FROM (\
               SELECT journal_sequence FROM llm_call_revisions UNION ALL \
               SELECT journal_sequence FROM llm_provider_attempt_revisions UNION ALL \
               SELECT journal_sequence FROM llm_tool_call_revisions UNION ALL \
               SELECT journal_sequence FROM llm_capture_gap_revisions\
             ) all_revisions GROUP BY journal_sequence HAVING count(*) <> 1)",
            [],
            |row| row.get(0),
        )
        .context("validating cross-dataset LLM journal sequence uniqueness")?;
    if duplicate_sequences != 0 {
        return Err(anyhow!(
            "governed LLM sources contain {duplicate_sequences} journal sequence(s) reused across canonical datasets"
        ));
    }

    let orphan_attempts: i64 = connection
        .query_row(
            "SELECT count(*) FROM (\
             SELECT DISTINCT a.llm_call_id FROM llm_provider_attempt_revisions a \
             LEFT JOIN llm_call_revisions c ON c.llm_call_id = a.llm_call_id \
             WHERE c.llm_call_id IS NULL)",
            [],
            |row| row.get(0),
        )
        .context("validating provider-attempt logical-call ownership")?;
    if orphan_attempts != 0 {
        return Err(anyhow!(
            "governed LLM sources contain {orphan_attempts} provider-attempt lifecycle(s) without a logical-call owner"
        ));
    }

    let orphan_owned_gaps: i64 = connection
        .query_row(
            "SELECT count(*) FROM (\
             SELECT DISTINCT g.llm_call_id FROM llm_capture_gap_revisions g \
             LEFT JOIN llm_call_revisions c ON c.llm_call_id = g.llm_call_id \
             WHERE g.llm_call_id IS NOT NULL AND c.llm_call_id IS NULL)",
            [],
            |row| row.get(0),
        )
        .context("validating call-owned capture-gap ownership")?;
    if orphan_owned_gaps != 0 {
        return Err(anyhow!(
            "governed LLM sources contain {orphan_owned_gaps} call-owned capture gap(s) without a logical-call owner"
        ));
    }

    let context_columns = [
        "trace_id",
        "scope_resolution",
        "workload_class",
        "call_role",
        "root_execution_id",
        "execution_id",
        "task_id",
        "plan_id",
        "step_id",
        "iteration_id",
        "chat_session_id",
        "chat_turn_id",
        "user_message_id",
        "parent_call_id",
        "parent_relation",
        "retry_group_id",
        "route_decision_id",
        "operation",
    ];
    let context_drift = context_columns
        .iter()
        .map(|column| format!("a.\"{column}\" IS DISTINCT FROM c.\"{column}\""))
        .collect::<Vec<_>>()
        .join(" OR ");
    let invalid_attempt_ownership: i64 = connection
        .query_row(
            &format!(
                "SELECT count(*) FROM llm_provider_attempt_revisions a \
                 JOIN llm_call_revisions c ON c.llm_call_id = a.llm_call_id \
                 WHERE ({context_drift}) OR \
                   (c.record_revision = 2 AND a.provider_attempt_index > c.provider_attempt_count) OR \
                   (c.record_revision = 1 AND a.started_at_ms IS NOT NULL AND a.started_at_ms < c.created_at_ms) OR \
                   (c.record_revision = 2 AND a.completed_at_ms IS NOT NULL AND a.completed_at_ms > c.completed_at_ms)"
            ),
            [],
            |row| row.get(0),
        )
        .context("validating provider-attempt ownership against logical calls")?;
    if invalid_attempt_ownership != 0 {
        return Err(anyhow!(
            "governed LLM sources contain {invalid_attempt_ownership} provider-attempt/call relationship row(s) with context, count, or timing drift"
        ));
    }

    // A bounded query may legitimately load a tool partition whose producing
    // call sits just outside the requested date window, so absence from this
    // process-local view is censoring rather than proof of an orphan. When the
    // owner is present, however, every shared context field must agree. The
    // journal and Phase 4 live audit own end-to-end missing-owner coverage.
    let tool_context_drift = context_columns
        .iter()
        .map(|column| format!("t.\"{column}\" IS DISTINCT FROM c.\"{column}\""))
        .collect::<Vec<_>>()
        .join(" OR ");
    let invalid_tool_ownership: i64 = connection
        .query_row(
            &format!(
                "SELECT count(*) FROM llm_tool_call_revisions t \
                 JOIN llm_call_revisions c ON c.llm_call_id = t.llm_call_id \
                 WHERE {tool_context_drift}"
            ),
            [],
            |row| row.get(0),
        )
        .context("validating tool-execution ownership against logical calls")?;
    if invalid_tool_ownership != 0 {
        return Err(anyhow!(
            "governed LLM sources contain {invalid_tool_ownership} tool-execution/call relationship row(s) with scope or lineage drift"
        ));
    }

    let money_drift = [
        "input_cost_usd",
        "output_cost_usd",
        "reasoning_cost_usd",
        "cache_cost_usd",
        "cost_usd",
    ]
    .into_iter()
    .map(|column| adjacent_money_drift_predicate("a", "c", column))
    .collect::<Vec<_>>()
    .join(" OR ");
    let final_attempt_fact_drift: i64 = connection
        .query_row(
            &format!(
                "SELECT count(*) FROM llm_provider_attempt_revisions a \
             JOIN llm_call_revisions c ON c.llm_call_id = a.llm_call_id \
             WHERE a.record_revision = 3 AND c.record_revision = 2 \
               AND a.provider_attempt_index = c.provider_attempt_count AND (\
                 a.input_tokens IS DISTINCT FROM c.input_tokens OR \
                 a.output_tokens IS DISTINCT FROM c.output_tokens OR \
                 a.reasoning_tokens IS DISTINCT FROM c.reasoning_tokens OR \
                 a.cache_read_tokens IS DISTINCT FROM c.cache_read_tokens OR \
                 a.cache_creation_tokens IS DISTINCT FROM c.cache_creation_tokens OR \
                 a.audio_input_tokens IS DISTINCT FROM c.audio_input_tokens OR \
                 a.audio_output_tokens IS DISTINCT FROM c.audio_output_tokens OR \
                 a.audio_cached_tokens IS DISTINCT FROM c.audio_cached_tokens OR \
                 a.total_tokens IS DISTINCT FROM c.total_tokens OR \
                 a.pricing_version IS DISTINCT FROM c.pricing_version OR \
                 a.cost_source IS DISTINCT FROM c.cost_source OR \
                 {money_drift}\
               )"
            ),
            [],
            |row| row.get(0),
        )
        .context("validating final provider-attempt economics against its logical call")?;
    if final_attempt_fact_drift != 0 {
        return Err(anyhow!(
            "governed LLM sources contain {final_attempt_fact_drift} final provider-attempt/call pair(s) with usage or pricing drift"
        ));
    }

    let terminal_outcome_drift: i64 = connection
        .query_row(
            "SELECT count(*) FROM llm_provider_attempt_revisions a \
             JOIN llm_call_revisions c ON c.llm_call_id = a.llm_call_id \
             WHERE a.record_revision = 3 AND c.record_revision = 2 \
               AND a.provider_attempt_index = c.provider_attempt_count \
               AND ((c.call_terminal_state = 'succeeded') \
                    IS DISTINCT FROM (a.attempt_terminal_state = 'succeeded'))",
            [],
            |row| row.get(0),
        )
        .context("validating final provider-attempt transport outcome against its logical call")?;
    if terminal_outcome_drift != 0 {
        return Err(anyhow!(
            "governed LLM sources contain {terminal_outcome_drift} final provider-attempt/call transport outcome mismatch(es)"
        ));
    }

    let invalid_parent_identity: i64 = connection
        .query_row(
            "SELECT count(*) FROM llm_call_revisions child \
             JOIN llm_call_revisions parent ON parent.llm_call_id = child.parent_call_id \
             WHERE child.trace_id IS DISTINCT FROM parent.trace_id \
                OR child.principal IS DISTINCT FROM parent.principal \
                OR child.workspace IS DISTINCT FROM parent.workspace",
            [],
            |row| row.get(0),
        )
        .context("validating known LLM parent identity")?;
    if invalid_parent_identity != 0 {
        return Err(anyhow!(
            "governed LLM sources contain {invalid_parent_identity} known parent edge row(s) crossing trace or scope"
        ));
    }
    let parent_cycles: i64 = connection
        .query_row(
            "WITH RECURSIVE \
               edges(child_id, parent_id) AS (\
                 SELECT DISTINCT llm_call_id, parent_call_id FROM llm_call_revisions \
                 WHERE parent_call_id IS NOT NULL\
               ), \
               reachable(start_id, node_id) AS (\
                 SELECT child_id, parent_id FROM edges \
                 UNION \
                 SELECT reachable.start_id, edges.parent_id FROM reachable \
                 JOIN edges ON edges.child_id = reachable.node_id\
               ) \
             SELECT count(*) FROM reachable WHERE start_id = node_id",
            [],
            |row| row.get(0),
        )
        .context("validating canonical LLM parent graph")?;
    if parent_cycles != 0 {
        return Err(anyhow!(
            "governed LLM sources contain {parent_cycles} cyclic logical-call parent reachability row(s)"
        ));
    }
    Ok(())
}

/// Return a SQL predicate that flags monetary drift while tolerating only the
/// single adjacent IEEE-754 value introduced by an otherwise lossless JSON
/// parse/serialize round trip. Null asymmetry and drift of two or more ULPs
/// remain failures. All canonical money fields are independently validated as
/// finite and non-negative before this cross-dataset check runs.
fn adjacent_money_drift_predicate(left: &str, right: &str, column: &str) -> String {
    format!(
        "({left}.{column} IS DISTINCT FROM {right}.{column} AND (\
           {left}.{column} IS NULL OR {right}.{column} IS NULL OR (\
             nextafter({left}.{column}, {right}.{column}) IS DISTINCT FROM {right}.{column} AND \
             nextafter({right}.{column}, {left}.{column}) IS DISTINCT FROM {left}.{column}\
           )\
         ))"
    )
}

fn install_typed_empty_view(connection: &Connection, view: &str) -> Result<()> {
    let projection = FACT_COLUMNS
        .iter()
        .map(|(name, ty)| format!("CAST(NULL AS {ty}) AS \"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    connection
        .execute_batch(&format!(
            "CREATE OR REPLACE VIEW \"{view}\" AS SELECT {projection} WHERE false"
        ))
        .with_context(|| format!("installing typed empty LLM view {view}"))
}

fn install_coalesced_view(connection: &Connection, target: &str, source: &str) -> Result<()> {
    let projection = FACT_COLUMNS
        .iter()
        .map(|(name, _)| {
            if *name == "stable_id" {
                "stable_id".to_string()
            } else if matches!(*name, "record_revision" | "journal_sequence") {
                format!("max(\"{name}\") AS \"{name}\"")
            } else {
                format!(
                    "arg_max(\"{name}\", record_revision) FILTER (WHERE \"{name}\" IS NOT NULL) AS \"{name}\""
                )
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    connection
        .execute_batch(&format!(
            "CREATE OR REPLACE VIEW \"{target}\" AS SELECT {projection} FROM \"{source}\" GROUP BY stable_id"
        ))
        .with_context(|| format!("installing latest-revision LLM view {target}"))
}

fn install_dispatch_timing_view(
    connection: &Connection,
    files: &[PathBuf],
    scope: &LlmScope,
) -> Result<()> {
    if files.is_empty() {
        return install_empty_dispatch_timing_view(connection);
    }
    connection
        .execute_batch(&format!(
            "CREATE TEMP TABLE llm_dispatch_timing_raw AS SELECT * FROM read_parquet({}, hive_partitioning = false, union_by_name = true)",
            parquet_source_sql(files)
        ))
        .context("loading scoped dispatch timing sources")?;
    let columns = view_columns(connection, "llm_dispatch_timing_raw")?;
    let has_principal = columns.contains("principal");
    let has_workspace = columns.contains("workspace");
    if has_principal != has_workspace {
        return Err(anyhow!(
            "scoped LLM dispatch timing must carry principal and workspace together"
        ));
    }
    if has_principal {
        // Historical rows in a union-by-name batch can have both values NULL.
        // Any asserted embedded scope, however, must agree with the trusted
        // path scope before it can enrich canonical facts.
        let invalid_scope: i64 = connection
            .query_row(
                "SELECT count(*) FROM llm_dispatch_timing_raw WHERE \
                 (principal IS NULL) <> (workspace IS NULL) OR \
                 (principal IS NOT NULL AND (trim(CAST(principal AS VARCHAR)) <> CAST(principal AS VARCHAR) OR trim(CAST(workspace AS VARCHAR)) <> CAST(workspace AS VARCHAR) OR principal <> ? OR workspace <> ?))",
                duckdb::params![&scope.principal, &scope.workspace],
                |row| row.get(0),
            )
            .context("validating embedded scope on LLM dispatch timing")?;
        if invalid_scope != 0 {
            return Err(anyhow!(
                "scoped LLM dispatch timing contains {invalid_scope} embedded scope mismatch(es)"
            ));
        }
    }
    let mut integrity_predicates = Vec::new();
    for column in ["job_id", "llm_call_id"] {
        if columns.contains(column) {
            integrity_predicates.push(format!(
                "(\"{column}\" IS NOT NULL AND (trim(CAST(\"{column}\" AS VARCHAR)) = '' OR trim(CAST(\"{column}\" AS VARCHAR)) <> CAST(\"{column}\" AS VARCHAR)))"
            ));
        }
    }
    for column in [
        "provider_attempt_count",
        "dispatched_at_ms",
        "wait_ms",
        "execution_ms",
        "local_prep_ms",
        "completed_at_ms",
        "timestamp_ms",
    ] {
        if columns.contains(column) {
            integrity_predicates.push(format!("(\"{column}\" IS NOT NULL AND \"{column}\" < 0)"));
        }
    }
    if !integrity_predicates.is_empty() {
        let invalid_facts: i64 = connection
            .query_row(
                &format!(
                    "SELECT count(*) FROM llm_dispatch_timing_raw WHERE {}",
                    integrity_predicates.join(" OR ")
                ),
                [],
                |row| row.get(0),
            )
            .context("validating LLM dispatch identity and timing facts")?;
        if invalid_facts != 0 {
            return Err(anyhow!(
                "scoped LLM dispatch timing contains {invalid_facts} invalid identity or timing row(s)"
            ));
        }
    }
    // Historical or manually copied dispatch files without both stable join
    // keys cannot safely enrich canonical facts. Preserve the governed schema
    // but expose no timing rows instead of attempting a partial-key join.
    if !columns.contains("job_id") || !columns.contains("llm_call_id") {
        return install_empty_dispatch_timing_view(connection);
    }
    let text = |column: &str| {
        if columns.contains(column) {
            format!("CAST(\"{column}\" AS VARCHAR)")
        } else {
            "NULL::VARCHAR".to_string()
        }
    };
    let nonnegative_duration = |column: &str| {
        if columns.contains(column) {
            format!("CASE WHEN \"{column}\" >= 0 THEN CAST(\"{column}\" AS UBIGINT) ELSE NULL END")
        } else {
            "NULL::UBIGINT".to_string()
        }
    };
    let positive_count = |column: &str| {
        if columns.contains(column) {
            format!("CASE WHEN \"{column}\" > 0 THEN CAST(\"{column}\" AS UINTEGER) ELSE NULL END")
        } else {
            "NULL::UINTEGER".to_string()
        }
    };
    let mut predicates = Vec::new();
    if columns.contains("state") {
        predicates.push("state IN ('completed', 'failed', 'tombstoned')".to_string());
    }
    if columns.contains("response_reused") {
        predicates.push("COALESCE(response_reused, false) = false".to_string());
    }
    let where_sql = if predicates.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", predicates.join(" AND "))
    };
    let order = if columns.contains("completed_at_ms") && columns.contains("timestamp_ms") {
        "COALESCE(completed_at_ms, timestamp_ms) DESC NULLS LAST"
    } else if columns.contains("completed_at_ms") {
        "completed_at_ms DESC NULLS LAST"
    } else if columns.contains("timestamp_ms") {
        "timestamp_ms DESC NULLS LAST"
    } else if columns.contains("job_id") {
        "job_id"
    } else {
        "1"
    };
    connection
        .execute_batch(&format!(
            "CREATE OR REPLACE VIEW llm_dispatch_timing AS \
             SELECT dispatch_job_id, llm_call_id, provider_attempt_count, provider_started_at_ms, queue_wait_ms, local_prep_ms, provider_execution_ms \
             FROM (SELECT {} AS dispatch_job_id, {} AS llm_call_id, \
                          {} AS provider_attempt_count, {} AS provider_started_at_ms, \
                          {} AS queue_wait_ms, {} AS local_prep_ms, {} AS provider_execution_ms, \
                          row_number() OVER (PARTITION BY {}, {} ORDER BY {}) AS __row_number \
                   FROM llm_dispatch_timing_raw{}) \
             WHERE __row_number = 1",
            text("job_id"),
            text("llm_call_id"),
            positive_count("provider_attempt_count"),
            nonnegative_duration("dispatched_at_ms"),
            nonnegative_duration("wait_ms"),
            nonnegative_duration("local_prep_ms"),
            nonnegative_duration("execution_ms"),
            text("job_id"),
            text("llm_call_id"),
            order,
            where_sql,
        ))
        .context("installing deduplicated dispatch timing view")
}

fn install_empty_dispatch_timing_view(connection: &Connection) -> Result<()> {
    connection
        .execute_batch(
            "CREATE OR REPLACE VIEW llm_dispatch_timing AS SELECT \
             NULL::VARCHAR AS dispatch_job_id, NULL::VARCHAR AS llm_call_id, \
             NULL::UINTEGER AS provider_attempt_count, NULL::UBIGINT AS provider_started_at_ms, \
             NULL::UBIGINT AS queue_wait_ms, NULL::UBIGINT AS local_prep_ms, \
             NULL::UBIGINT AS provider_execution_ms WHERE false",
        )
        .context("installing empty dispatch timing view")
}

fn install_dispatch_enriched_attempts_view(connection: &Connection) -> Result<()> {
    let projection = FACT_COLUMNS
        .iter()
        .map(|(name, _)| {
            match *name {
                "started_at_ms" => "CASE WHEN a.started_at_ms IS NOT NULL THEN a.started_at_ms WHEN a.provider_attempt_index = 1 AND d.provider_attempt_count = 1 THEN CAST(d.provider_started_at_ms AS BIGINT) ELSE NULL END AS started_at_ms".to_string(),
                "provider_execution_ms" => "CASE WHEN a.provider_execution_ms IS NOT NULL THEN a.provider_execution_ms WHEN a.provider_attempt_index = 1 AND d.provider_attempt_count = 1 AND d.provider_started_at_ms IS NOT NULL THEN d.provider_execution_ms ELSE NULL END AS provider_execution_ms".to_string(),
                "latency_ms" => "CASE WHEN a.latency_ms IS NOT NULL THEN a.latency_ms WHEN a.provider_attempt_index = 1 AND d.provider_attempt_count = 1 AND d.provider_started_at_ms IS NOT NULL THEN d.provider_execution_ms ELSE NULL END AS latency_ms".to_string(),
                _ => format!("a.\"{name}\""),
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    connection
        .execute_batch(&format!(
            "CREATE OR REPLACE VIEW llm_provider_attempts AS SELECT {projection} \
             FROM llm_provider_attempts_canonical a LEFT JOIN llm_dispatch_timing d \
               ON d.dispatch_job_id = a.dispatch_job_id \
              AND d.llm_call_id = a.llm_call_id"
        ))
        .context("installing dispatch-enriched provider-attempt view")
}

fn install_enriched_calls_view(connection: &Connection) -> Result<()> {
    let attempt_owned = HashSet::from([
        "provider_attempt_id",
        "provider_attempt_index",
        "effective_profile",
        "profile",
        "provider",
        "model",
        "model_revision",
        "attempt_terminal_state",
    ]);
    let dispatch_owned = HashSet::from(["queue_wait_ms", "local_prep_ms", "provider_execution_ms"]);
    let projection = FACT_COLUMNS
        .iter()
        .map(|(name, _)| {
            if dispatch_owned.contains(name) {
                format!("COALESCE(c.\"{name}\", d.\"{name}\") AS \"{name}\"")
            } else if attempt_owned.contains(name) {
                format!("COALESCE(c.\"{name}\", a.\"{name}\") AS \"{name}\"")
            } else {
                format!("c.\"{name}\"")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    connection
        .execute_batch(&format!(
            "CREATE OR REPLACE VIEW llm_provider_attempts_latest_for_call AS SELECT a.* FROM llm_provider_attempts a WHERE llm_call_id IS NOT NULL QUALIFY row_number() OVER (PARTITION BY llm_call_id ORDER BY provider_attempt_index DESC NULLS LAST, record_revision DESC) = 1; CREATE OR REPLACE VIEW llm_calls_canonical AS SELECT {projection} FROM llm_calls_canonical_base c LEFT JOIN llm_provider_attempts_latest_for_call a ON a.llm_call_id = c.llm_call_id LEFT JOIN llm_dispatch_timing d ON d.dispatch_job_id = c.dispatch_job_id AND d.llm_call_id = c.llm_call_id"
        ))
        .context("installing provider-enriched canonical llm_calls view")
}

fn install_legacy_calls_view(
    connection: &Connection,
    scope: &LlmScope,
    files: &[PathBuf],
) -> Result<()> {
    connection
        .execute_batch(&format!(
            "CREATE TEMP TABLE llm_calls_legacy_raw AS SELECT *, filename AS __source_file FROM read_parquet({}, hive_partitioning = true, union_by_name = true, filename = true)",
            parquet_source_sql(files)
        ))
        .context("installing legacy llm_calls raw view")?;
    let columns = view_columns(connection, "llm_calls_legacy_raw")?;
    let projection = FACT_COLUMNS
        .iter()
        .map(|(name, ty)| legacy_projection(name, ty, &columns, scope))
        .collect::<Vec<_>>()
        .join(", ");
    connection
        .execute_batch(&format!(
            "CREATE OR REPLACE VIEW llm_calls_legacy AS SELECT {projection} FROM llm_calls_legacy_raw"
        ))
        .context("installing normalized legacy llm_calls view")
}

fn legacy_projection(name: &str, ty: &str, columns: &HashSet<String>, scope: &LlmScope) -> String {
    // The path is selected from the trusted scope. Never let an old embedded
    // scope column override it in the normalized relation.
    let compatibility_owned = matches!(
        name,
        "materialized_schema_version"
            | "fact_schema_version"
            | "journal_schema_version"
            | "journal_sequence"
            | "record_kind"
            | "stable_id"
            | "record_revision"
            | "lifecycle_phase"
            | "idempotency_key"
            | "payload_checksum"
            | "principal"
            | "workspace"
            | "capture_mode"
            | "capture_status"
            | "training_eligible_at_capture"
            | "training_exclusion_reason"
    );
    if name == "error" && columns.contains(name) {
        return format!(
            "CASE WHEN error IS NULL THEN NULL ELSE '{LEGACY_ERROR_REDACTION}'::VARCHAR END AS error"
        );
    }
    if name == "response_kind" && columns.contains(name) {
        return format!(
            "CASE WHEN response_kind IS NULL THEN NULL \
             WHEN regexp_full_match(CAST(response_kind AS VARCHAR), '[A-Za-z0-9_./:-]{{1,128}}') \
             THEN CAST(response_kind AS VARCHAR) \
             ELSE '{LEGACY_INVALID_CATEGORY_REDACTION}'::VARCHAR END AS response_kind"
        );
    }
    // Blank legacy identities mean "not observed", not a stable empty id.
    // Normalizing them to NULL also prevents a blank call id from taking
    // precedence over the deterministic row fingerprint below.
    if name == "llm_call_id" && columns.contains(name) {
        return "NULLIF(trim(CAST(llm_call_id AS VARCHAR)), '') AS llm_call_id".to_string();
    }
    if columns.contains(name) && !compatibility_owned {
        return format!("CAST(\"{name}\" AS {ty}) AS \"{name}\"");
    }
    let timestamp = if columns.contains("timestamp_ms") {
        "CAST(timestamp_ms AS BIGINT)"
    } else if columns.contains("started_at_ms") {
        "CAST(started_at_ms AS BIGINT)"
    } else {
        "0::BIGINT"
    };
    let text = |value: &str| format!("'{}'::VARCHAR", escape_sql_literal(value));
    let expression = match name {
        "materialized_schema_version" | "fact_schema_version" | "journal_schema_version" => {
            "0::INTEGER".to_string()
        },
        "journal_sequence" => "0::UBIGINT".to_string(),
        "record_kind" => text("legacy_call"),
        "stable_id" => {
            let mut candidates = Vec::new();
            for candidate in ["llm_call_id", "execution_id", "trace_id"] {
                if columns.contains(candidate) {
                    candidates.push(format!("NULLIF(trim(CAST({candidate} AS VARCHAR)), '')"));
                }
            }
            candidates.push(legacy_row_fingerprint(columns));
            format!("COALESCE({})", candidates.join(", "))
        },
        "record_revision" => "1::INTEGER".to_string(),
        "lifecycle_phase" => text("legacy_completed"),
        "idempotency_key" | "payload_checksum" => {
            format!("'legacy:' || {}", legacy_row_fingerprint(columns))
        },
        "occurred_at_ms" | "observed_at_ms" | "timestamp_ms" => timestamp.to_string(),
        "principal" => text(&scope.principal),
        "workspace" => text(&scope.workspace),
        "operation" => text("unknown"),
        "scope_resolution" => text("legacy_default"),
        "workload_class" => text("system"),
        "call_role" => text("primary"),
        "capture_mode" => text("metadata"),
        "capture_status" => text("metadata_only"),
        "training_eligible_at_capture" => "false::BOOLEAN".to_string(),
        "training_exclusion_reason" => text("legacy_schema"),
        // A historical partition that predates a field cannot prove that the
        // value was zero/false/true. Preserve the observation as unknown so
        // aggregate denominators and cost/token totals are not biased by
        // compatibility projection. Known legacy columns still flow through
        // the typed cast above.
        "success" | "transport_success" => "NULL::BOOLEAN".to_string(),
        "input_tokens"
        | "output_tokens"
        | "reasoning_tokens"
        | "cache_read_tokens"
        | "cache_creation_tokens"
        | "total_tokens"
        | "missing_record_count" => "NULL::UBIGINT".to_string(),
        "cost_usd" | "input_cost_usd" | "output_cost_usd" | "reasoning_cost_usd"
        | "cache_cost_usd" => "NULL::DOUBLE".to_string(),
        _ => format!("NULL::{ty}"),
    };
    format!("{expression} AS \"{name}\"")
}

fn legacy_row_fingerprint(columns: &HashSet<String>) -> String {
    let mut parts = vec!["__source_file".to_string()];
    for column in [
        "timestamp_ms",
        "started_at_ms",
        "llm_call_id",
        "trace_id",
        "execution_id",
        "operation",
        "provider",
        "model",
        "input_tokens",
        "output_tokens",
        "cost_usd",
    ] {
        if columns.contains(column) {
            parts.push(format!("COALESCE(CAST(\"{column}\" AS VARCHAR), '<null>')"));
        }
    }
    format!("md5(concat_ws('|', {}))", parts.join(", "))
}

fn view_columns(connection: &Connection, view: &str) -> Result<HashSet<String>> {
    let mut statement = connection
        .prepare(&format!("DESCRIBE \"{view}\""))
        .with_context(|| format!("describing LLM view {view}"))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<HashSet<_>, _>>()
        .map_err(anyhow::Error::from)
}

fn validate_columns(
    registry: &LlmFactRegistry,
    relation: LlmFactRelation,
    columns: &[String],
) -> Result<Vec<String>> {
    let mut seen = HashSet::new();
    for column in columns {
        if !registry.allows_column(relation, column) {
            return Err(anyhow!(
                "column `{column}` is not available on `{}`",
                relation.as_str()
            ));
        }
        if !seen.insert(column) {
            return Err(anyhow!("column `{column}` was selected more than once"));
        }
    }
    Ok(columns.to_vec())
}

fn validate_filter(
    registry: &LlmFactRegistry,
    relation: LlmFactRelation,
    filter: &LlmFactFilter,
) -> Result<()> {
    let column = match filter {
        LlmFactFilter::TextEquals { column, .. }
        | LlmFactFilter::BooleanEquals { column, .. }
        | LlmFactFilter::IntegerAtLeast { column, .. }
        | LlmFactFilter::IntegerAtMost { column, .. } => column,
    };
    if !registry.allows_column(relation, column) {
        return Err(anyhow!(
            "filter column `{column}` is not available on `{}`",
            relation.as_str()
        ));
    }
    let definition = registry
        .resolve(relation)
        .columns
        .iter()
        .find(|definition| definition.name == *column)
        .expect("allowed column exists");
    let valid_type = match filter {
        LlmFactFilter::TextEquals { .. } => definition.duckdb_type == "VARCHAR",
        LlmFactFilter::BooleanEquals { .. } => definition.duckdb_type == "BOOLEAN",
        LlmFactFilter::IntegerAtLeast { .. } | LlmFactFilter::IntegerAtMost { .. } => matches!(
            definition.duckdb_type.as_str(),
            "INTEGER" | "BIGINT" | "UBIGINT"
        ),
    };
    if !valid_type {
        return Err(anyhow!(
            "filter kind does not match `{column}` type {}",
            definition.duckdb_type
        ));
    }
    Ok(())
}

struct ValidatedFactSql {
    sql: String,
    canonical_relations: HashSet<String>,
}

fn validate_fact_sql(registry: &LlmFactRegistry, sql: &str) -> Result<ValidatedFactSql> {
    let sql = sql.trim();
    if sql.is_empty() {
        return Err(anyhow!("LLM fact SQL must not be empty"));
    }
    let mut statements =
        parse_analytics_sql(sql).map_err(|error| anyhow!("invalid LLM fact SQL: {error}"))?;
    if statements.len() != 1 {
        return Err(anyhow!("LLM fact SQL requires exactly one statement"));
    }
    let mut statement = statements.pop().expect("one parsed statement");
    let Statement::Query(query) = &statement else {
        return Err(anyhow!("LLM fact SQL accepts one SELECT/WITH query only"));
    };
    if !is_read_only_select_query(query) {
        return Err(anyhow!(
            "LLM fact SQL permits side-effect-free SELECT query bodies only"
        ));
    }

    let mut visitor = FactSqlVisitor::new(registry);
    if let ControlFlow::Break(error) = statement.visit(&mut visitor) {
        return Err(anyhow!(error));
    }
    if visitor.canonical_relations.is_empty() {
        return Err(anyhow!(
            "LLM fact SQL must reference at least one allowlisted fact relation"
        ));
    }
    let canonical_relations = visitor.canonical_relations.clone();
    drop(visitor);
    let _ = visit_relations_mut(&mut statement, |relation| {
        let normalized = relation.to_string().trim_matches('"').to_ascii_lowercase();
        if registry.resolve_name(&normalized).is_some() {
            *relation = ObjectName(vec![ObjectNamePart::Identifier(Ident::new(
                bounded_fact_relation(&normalized),
            ))]);
        }
        ControlFlow::<()>::Continue(())
    });
    Ok(ValidatedFactSql {
        sql: statement.to_string(),
        canonical_relations,
    })
}

fn bounded_fact_relation(relation: &str) -> String {
    format!("__llm_fact_bounded_{relation}")
}

fn is_reserved_fact_relation_name(name: &str) -> bool {
    name.starts_with("__llm_fact_bounded_")
}

struct FactSqlVisitor<'a> {
    registry: &'a LlmFactRegistry,
    cte_scopes: Vec<HashSet<String>>,
    canonical_relations: HashSet<String>,
}

impl<'a> FactSqlVisitor<'a> {
    fn new(registry: &'a LlmFactRegistry) -> Self {
        Self {
            registry,
            cte_scopes: Vec::new(),
            canonical_relations: HashSet::new(),
        }
    }

    fn cte_is_in_scope(&self, relation: &str) -> bool {
        self.cte_scopes
            .iter()
            .rev()
            .any(|scope| scope.contains(relation))
    }
}

impl Visitor for FactSqlVisitor<'_> {
    type Break = String;

    fn pre_visit_query(&mut self, query: &sqlparser::ast::Query) -> ControlFlow<Self::Break> {
        if !is_read_only_select_query_node(query) {
            return ControlFlow::Break(
                "LLM fact SQL permits side-effect-free SELECT query bodies only".to_string(),
            );
        }
        let Some(with) = &query.with else {
            self.cte_scopes.push(HashSet::new());
            return ControlFlow::Continue(());
        };

        // In a non-recursive WITH, each CTE alias enters scope only after its
        // body: later CTEs may reference earlier ones, but self/forward
        // references may not. Validate each body against that incremental
        // scope. The normal traversal visits the bodies again with the final
        // scope, which is harmless because relation collection is a set and
        // this first pass already rejected invalid references.
        if !with.recursive {
            self.cte_scopes.push(HashSet::new());
            for cte in &with.cte_tables {
                if let ControlFlow::Break(error) = cte.visit(self) {
                    return ControlFlow::Break(error);
                }
                let name = cte.alias.name.value.to_ascii_lowercase();
                if self.registry.resolve_name(&name).is_some()
                    || is_reserved_fact_relation_name(&name)
                {
                    return ControlFlow::Break(format!(
                        "CTE `{name}` may not shadow an LLM fact relation or its bounded runtime projection"
                    ));
                }
                if let Some(scope) = self.cte_scopes.last_mut() {
                    if !scope.insert(name.clone()) {
                        return ControlFlow::Break(format!(
                            "CTE `{name}` is defined more than once"
                        ));
                    }
                }
            }
            return ControlFlow::Continue(());
        }

        let mut scope = HashSet::new();
        for cte in &with.cte_tables {
            let name = cte.alias.name.value.to_ascii_lowercase();
            // The bounded-relation rewrite deliberately operates only on
            // canonical base relations. Refusing a shadowing alias keeps the
            // validated and executed ASTs semantically identical.
            if self.registry.resolve_name(&name).is_some() || is_reserved_fact_relation_name(&name)
            {
                return ControlFlow::Break(format!(
                    "CTE `{name}` may not shadow an LLM fact relation or its bounded runtime projection"
                ));
            }
            if !scope.insert(name.clone()) {
                return ControlFlow::Break(format!("CTE `{name}` is defined more than once"));
            }
        }
        self.cte_scopes.push(scope);
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &sqlparser::ast::Query) -> ControlFlow<Self::Break> {
        let _ = self.cte_scopes.pop();
        ControlFlow::Continue(())
    }

    fn pre_visit_relation(
        &mut self,
        relation: &sqlparser::ast::ObjectName,
    ) -> ControlFlow<Self::Break> {
        let relation = relation.to_string();
        if relation.contains('.') {
            return ControlFlow::Break(format!(
                "qualified or external relation `{relation}` is not available to LLM fact SQL"
            ));
        }
        let normalized = relation.trim_matches('"').to_ascii_lowercase();
        if self.cte_is_in_scope(&normalized) {
            return ControlFlow::Continue(());
        }
        if self.registry.resolve_name(&normalized).is_none() {
            return ControlFlow::Break(format!(
                "relation `{relation}` is not in the LLM fact registry"
            ));
        }
        self.canonical_relations.insert(normalized);
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<Self::Break> {
        match factor {
            TableFactor::Table { args: None, .. }
            | TableFactor::Derived { .. }
            | TableFactor::NestedJoin { .. } => ControlFlow::Continue(()),
            TableFactor::Table { args: Some(_), .. } => ControlFlow::Break(
                "table-valued arguments are not available to LLM fact SQL".to_string(),
            ),
            _ => ControlFlow::Break(
                "table functions, UNNEST, PIVOT, and external scans are not available to LLM fact SQL"
                    .to_string(),
            ),
        }
    }

    fn pre_visit_expr(&mut self, expression: &Expr) -> ControlFlow<Self::Break> {
        let Expr::Function(function) = expression else {
            return ControlFlow::Continue(());
        };
        let function_name = function.name.to_string().to_ascii_lowercase();
        let unqualified_function_name = function_name
            .rsplit('.')
            .next()
            .unwrap_or(function_name.as_str())
            .trim_matches('"');
        let forbidden = [
            "getenv",
            "glob",
            "http_get",
            "http_post",
            "load_extension",
            "read_blob",
            "read_csv",
            "read_csv_auto",
            "read_json",
            "read_json_auto",
            "read_ndjson",
            "read_parquet",
            "read_text",
            "sqlite_scan",
            "postgres_scan",
        ];
        if forbidden.contains(&unqualified_function_name)
            || unqualified_function_name.starts_with("read_")
        {
            return ControlFlow::Break(format!(
                "function `{function_name}` is not available to LLM fact SQL"
            ));
        }
        ControlFlow::Continue(())
    }
}

fn filters_sql(filters: &[LlmFactFilter], range: LlmReadTimeRange) -> String {
    let mut predicates = vec![time_range_predicate(range)];
    predicates.extend(filters.iter().map(|filter| match filter {
        LlmFactFilter::TextEquals { column, value } => {
            format!("\"{column}\" = '{}'", escape_sql_literal(value))
        },
        LlmFactFilter::BooleanEquals { column, value } => format!("\"{column}\" = {value}"),
        LlmFactFilter::IntegerAtLeast { column, value } => format!("\"{column}\" >= {value}"),
        LlmFactFilter::IntegerAtMost { column, value } => format!("\"{column}\" <= {value}"),
    }));
    format!(" WHERE {}", predicates.join(" AND "))
}

fn resolve_time_range(from_ms: Option<i64>, to_ms: Option<i64>) -> Result<LlmReadTimeRange> {
    let to_ms = to_ms.unwrap_or_else(|| Utc::now().timestamp_millis());
    let from_ms = from_ms.unwrap_or_else(|| to_ms.saturating_sub(DEFAULT_WINDOW_MS));
    if from_ms < 0 || to_ms <= from_ms {
        return Err(anyhow!(
            "LLM analytics requires a positive half-open time range with to_ms > from_ms"
        ));
    }
    if to_ms.saturating_sub(from_ms) > MAX_WINDOW_MS {
        return Err(anyhow!(
            "LLM analytics range exceeds the {} day maximum",
            MAX_WINDOW_MS / DEFAULT_WINDOW_MS
        ));
    }
    timestamp_date(from_ms)?;
    timestamp_date(to_ms.saturating_sub(1))?;
    Ok(LlmReadTimeRange { from_ms, to_ms })
}

/// Phase 1 identities are ULIDs, so a point lookup can select the immutable
/// UTC partitions beginning when the call was created instead of incorrectly
/// limiting detail reads to the most recent 31 days. Provider-attempt ids use the
/// deterministic `<call-ulid>:a<n>` form and share the same timestamp. Legacy
/// or malformed ids retain the bounded recent-window fallback.
fn identity_time_range(identity: &str) -> Result<LlmReadTimeRange> {
    let now_ms = Utc::now().timestamp_millis();
    let call_id = identity
        .split_once(":a")
        .map_or(identity, |(call_id, _)| call_id);
    if let Ok(id) = call_id.parse::<ulid::Ulid>() {
        let timestamp_ms = i64::try_from(id.timestamp_ms())
            .map_err(|_| anyhow!("LLM identity timestamp is outside the supported range"))?;
        // Use the full bounded detail window. A hard-coded two-day lookup could
        // silently omit delayed terminal revisions for a long-running call.
        let date = timestamp_date(timestamp_ms)?;
        let start = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| anyhow!("LLM identity date is invalid"))?
            .and_utc()
            .timestamp_millis();
        return resolve_time_range(Some(start), Some(start.saturating_add(MAX_WINDOW_MS)));
    }
    resolve_time_range(Some(now_ms.saturating_sub(MAX_WINDOW_MS)), Some(now_ms))
}

fn is_lower_hex_64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn timestamp_date(timestamp_ms: i64) -> Result<NaiveDate> {
    DateTime::<Utc>::from_timestamp_millis(timestamp_ms)
        .map(|timestamp| timestamp.date_naive())
        .ok_or_else(|| {
            anyhow!("LLM analytics timestamp {timestamp_ms} is outside the supported range")
        })
}

fn time_range_predicate(range: LlmReadTimeRange) -> String {
    format!(
        "timestamp_ms >= {} AND timestamp_ms < {}",
        range.from_ms, range.to_ms
    )
}

fn query_scalar_u64(connection: &Connection, sql: &str) -> Result<u64> {
    let result = run_analytics_query_with_interrupt_timeout(connection, QUERY_TIMEOUT, || {
        connection.query_row(sql, [], |row| row.get::<_, i64>(0))
    });
    match result {
        Ok(value) => checked_nonnegative_u64(value, "scalar aggregate"),
        Err(AnalyticsDuckDbQueryError::Query(error)) => Err(anyhow::Error::new(error)),
        Err(AnalyticsDuckDbQueryError::TimedOut(error)) => Err(anyhow!(
            "LLM analytics query timed out{}",
            error.map(|error| format!(": {error}")).unwrap_or_default()
        )),
    }
}

fn query_optional_i64(connection: &Connection, sql: &str) -> Result<Option<i64>> {
    let result = run_analytics_query_with_interrupt_timeout(connection, QUERY_TIMEOUT, || {
        connection.query_row(sql, [], |row| row.get::<_, Option<i64>>(0))
    });
    match result {
        Ok(value) => Ok(value),
        Err(AnalyticsDuckDbQueryError::Query(error)) => Err(anyhow::Error::new(error)),
        Err(AnalyticsDuckDbQueryError::TimedOut(error)) => Err(anyhow!(
            "LLM analytics query timed out{}",
            error.map(|error| format!(": {error}")).unwrap_or_default()
        )),
    }
}

fn query_distinct_text(connection: &Connection, sql: &str, limit: usize) -> Result<Vec<String>> {
    let (_, rows) = query_json_rows(connection, sql, limit)?;
    Ok(rows
        .into_iter()
        .filter_map(|mut row| row.remove("value"))
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect())
}

fn query_json_rows(
    connection: &Connection,
    sql: &str,
    row_limit: usize,
) -> Result<(Vec<String>, Vec<Map<String, Value>>)> {
    let result = run_analytics_query_with_interrupt_timeout(connection, QUERY_TIMEOUT, || {
        let mut statement = connection.prepare(sql)?;
        let mut cursor = statement.query([])?;
        // DuckDB 1.105 resolves result metadata when `query` executes rather
        // than when the statement is prepared. Reading it before execution
        // panics inside the driver. Rows deliberately exposes the executed
        // statement so metadata remains available without a borrow conflict.
        let executed = cursor
            .as_ref()
            .expect("DuckDB query rows retain their executed statement");
        let columns = (0..executed.column_count())
            .map(|index| {
                executed
                    .column_name(index)
                    .map(|value| value.to_string())
                    .unwrap_or_else(|_| "?".to_string())
            })
            .collect::<Vec<_>>();
        let mut rows = Vec::new();
        let mut output_bytes = 0_usize;
        while rows.len() < row_limit.min(ANALYTICS_DUCKDB_MAX_RESULT_ROWS) {
            let Some(row) = cursor.next()? else { break };
            let mut object = Map::new();
            for (index, column) in columns.iter().enumerate() {
                // A decode failure is not an unknown telemetry value. Turning
                // it into JSON null would conceal schema/corruption drift and
                // make a governed result look valid, so fail the read closed.
                let raw_value = row.get::<_, DuckValue>(index)?;
                let value = duck_value_to_json(&raw_value)?;
                object.insert(column.clone(), value);
            }
            output_bytes = output_bytes.saturating_add(
                serde_json::to_vec(&object)
                    .map_err(|error| {
                        duckdb::Error::InvalidParameterName(format!(
                            "serializing LLM analytics row: {error}"
                        ))
                    })?
                    .len()
                    .saturating_add(1),
            );
            if output_bytes > ANALYTICS_DUCKDB_MAX_RESULT_BYTES {
                return Err(duckdb::Error::InvalidParameterName(
                    "LLM analytics result exceeds the serialized byte limit".to_string(),
                ));
            }
            rows.push(object);
        }
        Ok((columns, rows))
    });
    match result {
        Ok(rows) => Ok(rows),
        Err(AnalyticsDuckDbQueryError::Query(error)) => Err(anyhow::Error::new(error)),
        Err(AnalyticsDuckDbQueryError::TimedOut(error)) => Err(anyhow!(
            "LLM analytics query timed out{}",
            error.map(|error| format!(": {error}")).unwrap_or_default()
        )),
    }
}

fn duck_value_to_json(value: &DuckValue) -> duckdb::Result<Value> {
    let converted = match value {
        DuckValue::Null => Value::Null,
        DuckValue::Boolean(value) => Value::from(*value),
        DuckValue::TinyInt(value) => Value::from(*value),
        DuckValue::SmallInt(value) => Value::from(*value),
        DuckValue::Int(value) => Value::from(*value),
        DuckValue::BigInt(value) => Value::from(*value),
        DuckValue::UTinyInt(value) => Value::from(*value),
        DuckValue::USmallInt(value) => Value::from(*value),
        DuckValue::UInt(value) => Value::from(*value),
        DuckValue::UBigInt(value) => Value::from(*value),
        DuckValue::Float(value) if value.is_finite() => Value::from(*value),
        DuckValue::Double(value) if value.is_finite() => Value::from(*value),
        DuckValue::Float(_) | DuckValue::Double(_) => {
            return Err(duckdb::Error::InvalidParameterName(
                "LLM analytics result contains a non-finite floating-point value".to_string(),
            ));
        },
        DuckValue::Text(value) => Value::from(value.clone()),
        // Caller-authored aggregate SQL can deliberately produce date/list
        // values even though the registered fact columns are primitive. Keep
        // the established deterministic representation for those types; only
        // invalid floating-point values are rejected rather than becoming
        // JSON null.
        other => Value::from(format!("{other:?}")),
    };
    Ok(converted)
}

fn flatten_files(sources: &[LlmGovernedPartitionSource]) -> Vec<PathBuf> {
    sources
        .iter()
        .flat_map(|source| source.files.iter().cloned())
        .collect()
}

fn dispatch_partition_files(partition: &Path) -> Result<Vec<PathBuf>> {
    super::parquet_maintenance::partition_sources(
        partition,
        super::parquet_maintenance::PartitionedDataset::LlmDispatch,
    )
}

fn parquet_source_sql(files: &[PathBuf]) -> String {
    let files = files
        .iter()
        .map(|path| format!("'{}'", escape_sql_literal(&path.display().to_string())))
        .collect::<Vec<_>>();
    match files.as_slice() {
        [one] => one.clone(),
        _ => format!("[{}]", files.join(", ")),
    }
}

fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

fn checked_nonnegative_u64(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| anyhow!("LLM analytics {field} aggregate is negative: {value}"))
}

fn checked_nonnegative_f64(value: f64, field: &str) -> Result<f64> {
    if !value.is_finite() || value < 0.0 {
        return Err(anyhow!(
            "LLM analytics {field} aggregate must be finite and non-negative: {value}"
        ));
    }
    Ok(value)
}

fn checked_optional_nonnegative_f64(value: Option<f64>, field: &str) -> Result<Option<f64>> {
    value
        .map(|value| checked_nonnegative_f64(value, field))
        .transpose()
}

fn default_true() -> bool {
    true
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn governed_aggregates_reject_negative_values_instead_of_inventing_zero() {
        assert_eq!(
            checked_nonnegative_u64(7, "input_tokens").expect("valid aggregate"),
            7
        );
        let error = checked_nonnegative_u64(-1, "input_tokens")
            .expect_err("negative telemetry must fail closed");
        assert!(error
            .to_string()
            .contains("input_tokens aggregate is negative"));

        for invalid in [-1.0, f64::NAN, f64::INFINITY] {
            assert!(checked_nonnegative_f64(invalid, "total_cost_usd").is_err());
            assert!(checked_optional_nonnegative_f64(Some(invalid), "average_latency_ms").is_err());
        }
        assert_eq!(
            checked_optional_nonnegative_f64(None, "average_latency_ms")
                .expect("missing average is honest"),
            None
        );
    }

    #[test]
    fn governed_json_decode_rejects_nonfinite_values() {
        assert_eq!(
            duck_value_to_json(&DuckValue::Double(1.25)).expect("finite double"),
            Value::from(1.25)
        );
        assert!(duck_value_to_json(&DuckValue::Double(f64::NAN)).is_err());
        assert!(duck_value_to_json(&DuckValue::Float(f32::INFINITY)).is_err());
    }

    #[test]
    fn governed_money_reconciliation_allows_only_one_ulp_roundtrip_drift() {
        let connection = Connection::open_in_memory().expect("duckdb");
        let predicate = adjacent_money_drift_predicate("a", "c", "cost_usd");
        let has_drift = |left: Option<f64>, right: Option<f64>| {
            connection
                .query_row(
                    &format!(
                        "SELECT {predicate} FROM (SELECT ?::DOUBLE AS cost_usd) a \
                         CROSS JOIN (SELECT ?::DOUBLE AS cost_usd) c"
                    ),
                    duckdb::params![left, right],
                    |row| row.get::<_, bool>(0),
                )
                .expect("money drift predicate")
        };
        let value = 0.010969999999999999_f64;
        let adjacent = f64::from_bits(value.to_bits() + 1);
        let two_ulps = f64::from_bits(value.to_bits() + 2);

        assert!(!has_drift(Some(value), Some(value)));
        assert!(!has_drift(Some(value), Some(adjacent)));
        assert!(has_drift(Some(value), Some(two_ulps)));
        assert!(has_drift(None, Some(value)));
    }

    fn sql_text(value: &str) -> String {
        format!("'{}'", escape_sql_literal(value))
    }

    fn fixture_date() -> String {
        Utc::now().format("%Y-%m-%d").to_string()
    }

    fn fixture_anchor_ms() -> i64 {
        Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 1)
            .expect("valid fixture day anchor")
            .and_utc()
            .timestamp_millis()
    }

    fn write_fact_revision(
        workspace: &ArtifactV2Workspace,
        scope: &LlmScope,
        dataset: LlmCanonicalDataset,
        date: &str,
        file_name: &str,
        values: &[(&str, String)],
    ) -> PathBuf {
        write_fact_revision_omitting(workspace, scope, dataset, date, file_name, values, &[])
    }

    fn write_fact_revision_omitting(
        workspace: &ArtifactV2Workspace,
        scope: &LlmScope,
        dataset: LlmCanonicalDataset,
        date: &str,
        file_name: &str,
        values: &[(&str, String)],
        omitted_columns: &[&str],
    ) -> PathBuf {
        let path = dataset
            .root(workspace, scope)
            .join(format!("dt={date}"))
            .join(file_name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let connection = Connection::open_in_memory().expect("duckdb");
        let mut unique_values = std::collections::BTreeMap::new();
        for (name, value) in values {
            unique_values.insert(*name, value.clone());
        }
        let schema = FACT_COLUMNS
            .iter()
            .filter(|(name, _)| !omitted_columns.contains(name))
            .map(|(name, ty)| format!("\"{name}\" {ty}"))
            .collect::<Vec<_>>()
            .join(", ");
        let columns = unique_values
            .iter()
            .map(|(name, _)| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let values = unique_values
            .iter()
            .map(|(_, value)| value.clone())
            .collect::<Vec<_>>()
            .join(", ");
        connection
            .execute_batch(&format!(
                "CREATE TABLE fact({schema}); INSERT INTO fact ({columns}) VALUES ({values}); COPY fact TO '{}' (FORMAT PARQUET)",
                escape_sql_literal(&path.display().to_string())
            ))
            .expect("write fact revision");
        path
    }

    fn base_values<'a>(
        scope: &'a LlmScope,
        record_kind: &'a str,
        stable_id: &'a str,
        revision: u16,
        sequence: u64,
        _lifecycle: &'a str,
        _idempotency_key: &'a str,
    ) -> Vec<(&'a str, String)> {
        let anchor_ms = fixture_anchor_ms();
        let (lifecycle, mut dataset_values): (&str, Vec<(&str, String)>) = match record_kind {
            "call_fact" if revision == 1 => (
                "started",
                vec![
                    ("trace_id", sql_text(&format!("trace-{stable_id}"))),
                    ("llm_call_id", sql_text(stable_id)),
                    ("scope_resolution", sql_text("explicit")),
                    ("workload_class", sql_text("system")),
                    ("call_role", sql_text("primary")),
                    ("created_at_ms", anchor_ms.to_string()),
                ],
            ),
            "call_fact" => (
                "completed",
                vec![
                    ("trace_id", sql_text(&format!("trace-{stable_id}"))),
                    ("llm_call_id", sql_text(stable_id)),
                    ("scope_resolution", sql_text("explicit")),
                    ("workload_class", sql_text("system")),
                    ("call_role", sql_text("primary")),
                    ("created_at_ms", anchor_ms.to_string()),
                    ("completed_at_ms", (anchor_ms + 100).to_string()),
                    ("latency_ms", "100".to_string()),
                    ("call_terminal_state", sql_text("succeeded")),
                    ("transport_success", "true".to_string()),
                    ("success", "true".to_string()),
                    ("provider_attempt_count", "1".to_string()),
                    ("response_present", "true".to_string()),
                    ("parse_attempted", "false".to_string()),
                    ("schema_validation_attempted", "false".to_string()),
                    ("contract_validation_attempted", "false".to_string()),
                    ("discarded_before_use", "false".to_string()),
                ],
            ),
            "provider_attempt" if revision == 1 => (
                "started",
                vec![
                    (
                        "trace_id",
                        sql_text(&format!(
                            "trace-{}",
                            stable_id.split(":a").next().unwrap_or(stable_id)
                        )),
                    ),
                    (
                        "llm_call_id",
                        sql_text(stable_id.split(":a").next().unwrap_or(stable_id)),
                    ),
                    ("provider_attempt_id", sql_text(stable_id)),
                    ("provider_attempt_index", "1".to_string()),
                    ("scope_resolution", sql_text("explicit")),
                    ("workload_class", sql_text("system")),
                    ("call_role", sql_text("primary")),
                    ("provider", sql_text("openai")),
                    ("model", sql_text("gpt-test")),
                    ("started_at_ms", (anchor_ms + 10).to_string()),
                ],
            ),
            "provider_attempt" if revision == 2 => (
                "first_token",
                vec![
                    (
                        "trace_id",
                        sql_text(&format!(
                            "trace-{}",
                            stable_id.split(":a").next().unwrap_or(stable_id)
                        )),
                    ),
                    (
                        "llm_call_id",
                        sql_text(stable_id.split(":a").next().unwrap_or(stable_id)),
                    ),
                    ("provider_attempt_id", sql_text(stable_id)),
                    ("provider_attempt_index", "1".to_string()),
                    ("scope_resolution", sql_text("explicit")),
                    ("workload_class", sql_text("system")),
                    ("call_role", sql_text("primary")),
                    ("provider", sql_text("openai")),
                    ("model", sql_text("gpt-test")),
                    ("started_at_ms", (anchor_ms + 10).to_string()),
                    ("first_token_at_ms", (anchor_ms + 30).to_string()),
                    ("ttft_ms", "20".to_string()),
                ],
            ),
            "provider_attempt" => (
                "completed",
                vec![
                    (
                        "trace_id",
                        sql_text(&format!(
                            "trace-{}",
                            stable_id.split(":a").next().unwrap_or(stable_id)
                        )),
                    ),
                    (
                        "llm_call_id",
                        sql_text(stable_id.split(":a").next().unwrap_or(stable_id)),
                    ),
                    ("provider_attempt_id", sql_text(stable_id)),
                    ("provider_attempt_index", "1".to_string()),
                    ("scope_resolution", sql_text("explicit")),
                    ("workload_class", sql_text("system")),
                    ("call_role", sql_text("primary")),
                    ("provider", sql_text("openai")),
                    ("model", sql_text("gpt-test")),
                    ("started_at_ms", (anchor_ms + 10).to_string()),
                    ("completed_at_ms", (anchor_ms + 90).to_string()),
                    ("latency_ms", "80".to_string()),
                    ("attempt_terminal_state", sql_text("succeeded")),
                    ("transport_success", "true".to_string()),
                    ("success", "true".to_string()),
                ],
            ),
            "capture_gap" => (
                "reported",
                vec![
                    ("gap_id", sql_text(stable_id)),
                    ("gap_reason", sql_text("test_capture_gap")),
                    ("missing_record_count", "1".to_string()),
                    ("first_missing_at_ms", anchor_ms.to_string()),
                    ("last_missing_at_ms", anchor_ms.to_string()),
                    (
                        "observed_at_ms",
                        (anchor_ms + i64::try_from(sequence).unwrap_or_default()).to_string(),
                    ),
                    ("capture_status", sql_text("backpressure_degraded")),
                    ("training_eligible_at_capture", "false".to_string()),
                    ("training_exclusion_reason", sql_text("capture_gap")),
                ],
            ),
            "tool_lineage" => {
                let llm_call_id = stable_id.split(":tool:").next().unwrap_or(stable_id);
                let stage_index = match revision {
                    1 | 10 | 20 | 30 | 40 | 50 | 3000 | 5000 => 0,
                    value if value >= 8_000 => value - 8_000,
                    value if value >= 7_000 => value - 7_000,
                    value if value >= 6_000 => value - 6_000,
                    value if value >= 4_000 => value - 4_000,
                    value if value >= 2_000 => value - 2_000,
                    value if value >= 1_000 => value - 1_000,
                    _ => 0,
                };
                (
                    _lifecycle,
                    vec![
                        ("trace_id", sql_text(&format!("trace-{llm_call_id}"))),
                        ("llm_call_id", sql_text(llm_call_id)),
                        ("model_tool_call_id", sql_text("call_1")),
                        ("tool_execution_id", sql_text(stable_id)),
                        ("branch_id", sql_text("branch_1")),
                        ("scope_resolution", sql_text("explicit")),
                        ("workload_class", sql_text("system")),
                        ("call_role", sql_text("primary")),
                        ("source_surface", sql_text("system")),
                        ("tool_name", sql_text("browser__click")),
                        ("tool_family", sql_text("browser")),
                        ("tool_lineage_stage", sql_text(_lifecycle)),
                        ("tool_lineage_stage_index", stage_index.to_string()),
                        ("arguments_fingerprint", sql_text(&"a".repeat(64))),
                        ("tool_outcome", sql_text("pending")),
                        ("tool_side_effect_state", sql_text("none")),
                        ("tool_branch_state", sql_text("active")),
                        ("same_tool_arguments_count", "1".to_string()),
                        ("observation_action_cycle_count", "0".to_string()),
                        ("recovered_after_failure", "false".to_string()),
                        ("capture_mode", sql_text("metadata")),
                        ("capture_status", sql_text("complete")),
                        ("training_eligible_at_capture", "false".to_string()),
                        (
                            "training_exclusion_reason",
                            sql_text("lineage_requires_outcome_maturity"),
                        ),
                    ],
                )
            },
            _ => (_lifecycle, Vec::new()),
        };
        let occurred_at_ms = match (record_kind, revision) {
            ("call_fact", 1) => anchor_ms,
            ("call_fact", _) => anchor_ms + 100,
            ("provider_attempt", 1) => anchor_ms + 10,
            ("provider_attempt", 2) => anchor_ms + 30,
            ("provider_attempt", _) => anchor_ms + 90,
            _ => anchor_ms + i64::try_from(sequence).unwrap_or_default(),
        };
        let mut values = vec![
            ("materialized_schema_version", "1".to_string()),
            (
                "fact_schema_version",
                LLM_TRACE_FACT_SCHEMA_VERSION.to_string(),
            ),
            (
                "journal_schema_version",
                LLM_TRACE_JOURNAL_SCHEMA_VERSION.to_string(),
            ),
            ("journal_sequence", sequence.to_string()),
            ("record_kind", sql_text(record_kind)),
            ("stable_id", sql_text(stable_id)),
            ("record_revision", revision.to_string()),
            ("lifecycle_phase", sql_text(lifecycle)),
            (
                "idempotency_key",
                sql_text(&format!("{record_kind}:{stable_id}:r{revision}")),
            ),
            ("payload_checksum", sql_text(&format!("{sequence:064x}"))),
            ("occurred_at_ms", occurred_at_ms.to_string()),
            ("observed_at_ms", (occurred_at_ms + 100).to_string()),
            ("timestamp_ms", occurred_at_ms.to_string()),
            ("principal", sql_text(&scope.principal)),
            ("workspace", sql_text(&scope.workspace)),
            ("operation", sql_text("chat_response")),
            ("capture_mode", sql_text("metadata")),
            ("capture_status", sql_text("metadata_only")),
            ("training_eligible_at_capture", "true".to_string()),
        ];
        values.append(&mut dataset_values);
        values
    }

    #[test]
    fn overview_and_fact_sql_fail_closed_on_invalid_floating_point_values() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "invalid-numeric");
        let mut call = base_values(
            &scope,
            "call_fact",
            "invalid-cost-call",
            2,
            1,
            "completed",
            "call_fact:invalid-cost-call:r2",
        );
        call.push(("cost_usd", "-0.5".to_string()));
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::Calls,
            &fixture_date(),
            "part-call_fact-invalid-cost-r2.parquet",
            &call,
        );
        let service = LlmAnalyticsReadService::new(workspace);

        let overview_error = service
            .overview(&scope)
            .expect_err("negative cost aggregate must fail closed");
        assert!(format!("{overview_error:#}")
            .contains("invalid usage, pricing, timing, or validation facts"));

        let clean_temp = tempfile::tempdir().expect("clean tempdir");
        let clean_workspace = ArtifactV2Workspace::new(clean_temp.path());
        let clean_scope = LlmScope::new("owner", "nonfinite-sql");
        let clean_call = base_values(
            &clean_scope,
            "call_fact",
            "nonfinite-sql-call",
            2,
            1,
            "completed",
            "call_fact:nonfinite-sql-call:r2",
        );
        write_fact_revision(
            &clean_workspace,
            &clean_scope,
            LlmCanonicalDataset::Calls,
            &fixture_date(),
            "part-call_fact-nonfinite-sql-r2.parquet",
            &clean_call,
        );
        let sql_error = LlmAnalyticsReadService::new(clean_workspace)
            .query_fact_sql(
                &clean_scope,
                "SELECT CAST('NaN' AS DOUBLE) AS invalid FROM llm_calls",
                None,
                None,
                Some(10),
            )
            .expect_err("non-finite SQL output must fail closed");
        assert!(sql_error.to_string().contains("non-finite floating-point"));
    }

    #[test]
    fn query_contract_rejects_unknown_relations_columns_and_type_mismatches() {
        let registry = LlmFactRegistry::canonical();
        assert!(LlmFactRelation::parse("llm_calls; DROP TABLE llm_calls").is_none());
        assert!(validate_columns(
            &registry,
            LlmFactRelation::Calls,
            &["provider".to_string(), "prompt".to_string()]
        )
        .is_err());
        assert!(validate_filter(
            &registry,
            LlmFactRelation::Calls,
            &LlmFactFilter::BooleanEquals {
                column: "provider".to_string(),
                value: true,
            }
        )
        .is_err());
    }

    #[test]
    fn filters_escape_text_and_emit_typed_predicates() {
        let sql = filters_sql(
            &[
                LlmFactFilter::TextEquals {
                    column: "provider".to_string(),
                    value: "provider'quoted".to_string(),
                },
                LlmFactFilter::IntegerAtLeast {
                    column: "timestamp_ms".to_string(),
                    value: 42,
                },
            ],
            LlmReadTimeRange {
                from_ms: 1,
                to_ms: 100,
            },
        );
        assert_eq!(
            sql,
            " WHERE timestamp_ms >= 1 AND timestamp_ms < 100 AND \"provider\" = 'provider''quoted' AND \"timestamp_ms\" >= 42"
        );
    }

    #[test]
    fn empty_scope_has_typed_empty_relations_and_zero_overview() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let service = LlmAnalyticsReadService::new(workspace);
        let scope = LlmScope::new("owner", "default");
        let overview = service.overview(&scope).expect("overview");
        assert_eq!(overview.logical_calls, 0);
        assert_eq!(overview.provider_attempts, 0);
        assert_eq!(overview.capture_gaps, 0);
        let page = service
            .query_facts(&scope, LlmFactQuery::for_relation(LlmFactRelation::Calls))
            .expect("page");
        assert_eq!(page.total, 0);
        assert!(page.rows.is_empty());
        assert_eq!(page.columns.len(), FACT_COLUMNS.len());
    }

    #[test]
    fn catalog_is_scoped_content_free_and_persisted() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let service = LlmAnalyticsReadService::new(workspace.clone());
        let scope = LlmScope::new("owner", "default");
        let watermark_path = LlmCanonicalDataset::Calls
            .root(&workspace, &scope)
            .join(DATASET_WATERMARK_FILE);
        workspace
            .write_json_atomic_path_sync(
                watermark_path,
                &LlmDatasetMaterializationWatermark {
                    schema_version: DATASET_WATERMARK_SCHEMA_VERSION,
                    dataset: "llm_calls".to_string(),
                    materialized_schema_version: MATERIALIZED_FACT_SCHEMA_VERSION,
                    committed_journal_sequence: 42,
                    committed_checksum: "a".repeat(64),
                    published_revision_count: 7,
                    updated_at_ms: 100,
                },
            )
            .expect("watermark");
        let catalog = service.refresh_catalog(&scope).expect("catalog");
        assert_eq!(catalog.datasets.len(), LlmCanonicalDataset::ALL.len());
        assert_eq!(catalog.content_class, "fact_only");
        assert_eq!(catalog.datasets[0].materialized_through_sequence, Some(42));
        assert_eq!(catalog.datasets[0].published_revision_count, Some(7));
        assert!(catalog.relations.iter().all(|relation| relation
            .columns
            .iter()
            .all(|column| column.name != "prompt" && column.name != "response")));
        assert!(workspace
            .analytics_llm_fact_catalog_path("owner", "default")
            .is_file());
    }

    #[test]
    fn page_size_is_bounded_before_sql_execution() {
        let query = LlmFactQuery {
            limit: Some(usize::MAX),
            ..LlmFactQuery::for_relation(LlmFactRelation::Calls)
        };
        assert_eq!(
            query.limit.unwrap_or_default().clamp(1, MAX_PAGE_SIZE),
            MAX_PAGE_SIZE
        );
        assert!(resolve_time_range(Some(1), Some(1)).is_err());
        assert!(resolve_time_range(Some(1), Some(1 + MAX_WINDOW_MS + 1)).is_err());
        let now = Utc::now().timestamp_millis();
        let default = resolve_time_range(None, Some(now)).expect("default range");
        assert_eq!(default.to_ms - default.from_ms, DEFAULT_WINDOW_MS);

        let temp = tempfile::tempdir().expect("tempdir");
        let service = LlmAnalyticsReadService::new(ArtifactV2Workspace::new(temp.path()));
        let excessive_offset = LlmFactQuery {
            offset: MAX_OFFSET + 1,
            ..LlmFactQuery::for_relation(LlmFactRelation::Calls)
        };
        assert!(service
            .query_facts(&LlmScope::new("owner", "default"), excessive_offset)
            .is_err());
    }

    #[test]
    fn ulid_identity_lookup_targets_its_historical_partition() {
        // Canonical ULID example with a 2016 timestamp: detail lookup must not
        // silently fall back to the most recent 31-day window.
        let range =
            identity_time_range("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("historical ULID range");
        assert_eq!(range.to_ms - range.from_ms, MAX_WINDOW_MS);
        assert!(range.to_ms < Utc::now().timestamp_millis() - MAX_WINDOW_MS);

        let attempt =
            identity_time_range("01ARZ3NDEKTSV4RRFFQ69G5FAV:a7").expect("historical attempt range");
        assert_eq!(attempt.from_ms, range.from_ms);
        assert_eq!(attempt.to_ms, range.to_ms);
    }

    #[test]
    fn envelope_freshness_uses_the_contiguous_journal_prefix() {
        let temp = crate::magician_v2::analytics::llm_trace_journal::canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        let dataset_watermark_path = LlmCanonicalDataset::Calls
            .root(&workspace, &scope)
            .join(DATASET_WATERMARK_FILE);
        workspace
            .write_json_atomic_path_sync(
                dataset_watermark_path,
                &LlmDatasetMaterializationWatermark {
                    schema_version: DATASET_WATERMARK_SCHEMA_VERSION,
                    dataset: "llm_calls".to_string(),
                    materialized_schema_version: MATERIALIZED_FACT_SCHEMA_VERSION,
                    committed_journal_sequence: 42,
                    committed_checksum: "a".repeat(64),
                    published_revision_count: 1,
                    updated_at_ms: 100,
                },
            )
            .expect("dataset watermark");
        let records = (0..7)
            .map(|index| {
                let context = magicllm::LlmTraceContext::new(
                    scope.clone(),
                    magicllm::LlmWorkloadClass::System,
                );
                super::super::llm_trace_recorder::LlmTraceRecord::CallStarted(
                    super::super::llm_trace_recorder::LlmCallStarted::new(
                        context,
                        "freshness_fixture",
                        100 + index,
                    ),
                )
            })
            .collect::<Vec<_>>();
        let mut journal = super::super::llm_trace_journal::LlmTraceJournalStore::new(
            workspace.clone(),
            1024 * 1024,
        )
        .expect("journal store");
        journal
            .append_batch(&scope, &records)
            .expect("journal records");
        journal
            .commit_through(&scope, 7)
            .expect("journal watermark");

        let envelope = LlmAnalyticsReadService::new(workspace)
            .overview_envelope(&scope, Some(1), Some(2))
            .expect("overview envelope");

        assert_eq!(envelope.freshness.materialized_through_sequence, Some(7));
    }

    #[test]
    fn envelope_freshness_rejects_a_shaped_watermark_without_its_journal_envelope() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        workspace
            .write_json_atomic_path_sync(
                workspace
                    .analytics_llm_trace_journal_root("owner", "default")
                    .join("materialization-watermark.json"),
                &LlmTraceJournalWatermark {
                    schema_version:
                        super::super::llm_trace_journal::JOURNAL_WATERMARK_SCHEMA_VERSION,
                    committed_sequence: 7,
                    committed_checksum: Some("b".repeat(64)),
                    updated_at_ms: 100,
                },
            )
            .expect("forged journal watermark");

        let error = LlmAnalyticsReadService::new(workspace)
            .overview_envelope(&scope, Some(1), Some(2))
            .expect_err("absent journal envelope must fail freshness closed");

        assert!(error.to_string().contains("absent sequence"));
    }

    #[test]
    fn malformed_dataset_watermark_fails_catalog_closed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        let watermark_path = LlmCanonicalDataset::Calls
            .root(&workspace, &scope)
            .join(DATASET_WATERMARK_FILE);
        workspace
            .write_json_atomic_path_sync(
                watermark_path,
                &LlmDatasetMaterializationWatermark {
                    schema_version: DATASET_WATERMARK_SCHEMA_VERSION,
                    dataset: "llm_calls".to_string(),
                    materialized_schema_version: MATERIALIZED_FACT_SCHEMA_VERSION,
                    committed_journal_sequence: 42,
                    committed_checksum: "malformed".to_string(),
                    published_revision_count: 7,
                    updated_at_ms: 100,
                },
            )
            .expect("watermark");
        let error = LlmAnalyticsReadService::new(workspace)
            .refresh_catalog(&scope)
            .expect_err("invalid watermark must fail closed");
        assert!(error
            .to_string()
            .contains("invalid llm_calls materialization watermark"));
    }

    #[test]
    fn revisions_coalesce_and_terminal_attempt_enriches_logical_call() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");

        let mut started = base_values(&scope, "call_fact", "call-1", 1, 1, "started", "call:1:r1");
        started.extend([
            ("llm_call_id", sql_text("call-1")),
            ("trace_id", sql_text("trace-1")),
            ("requested_profile", sql_text("adaptive")),
            ("selected_profile", sql_text("chat-fast")),
            ("profile", sql_text("chat-fast")),
            ("source_surface", sql_text("chat")),
            ("prompt_projection_mode", sql_text("rebootstrap")),
        ]);
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::Calls,
            &fixture_date(),
            "part-call_fact-a-r1.parquet",
            &started,
        );

        let mut completed = base_values(
            &scope,
            "call_fact",
            "call-1",
            2,
            4,
            "completed",
            "call:1:r2",
        );
        completed.extend([
            ("llm_call_id", sql_text("call-1")),
            ("trace_id", sql_text("trace-1")),
            ("call_terminal_state", sql_text("succeeded")),
            ("transport_success", "true".to_string()),
            ("success", "true".to_string()),
            ("provider_attempt_count", "1".to_string()),
            ("input_tokens", "120".to_string()),
            ("output_tokens", "30".to_string()),
            ("total_tokens", "150".to_string()),
            ("started_at_ms", fixture_anchor_ms().to_string()),
            ("first_token_at_ms", (fixture_anchor_ms() + 80).to_string()),
            ("ttft_ms", "80".to_string()),
            ("latency_ms", "240".to_string()),
            ("cost_usd", "0.25".to_string()),
            ("pricing_version", sql_text("2026-07-22")),
            ("cost_source", sql_text("provider")),
            ("contract_validation_attempted", "true".to_string()),
            ("contract_validation_success", "true".to_string()),
        ]);
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::Calls,
            &fixture_date(),
            "part-call_fact-a-r2.parquet",
            &completed,
        );

        let mut attempt_started = base_values(
            &scope,
            "provider_attempt",
            "call-1:a1",
            1,
            2,
            "started",
            "attempt:1:r1",
        );
        attempt_started.extend([
            ("llm_call_id", sql_text("call-1")),
            ("trace_id", sql_text("trace-1")),
            ("provider_attempt_id", sql_text("call-1:a1")),
            ("provider_attempt_index", "1".to_string()),
            ("effective_profile", sql_text("chat-fallback")),
            ("profile", sql_text("chat-fallback")),
            ("provider", sql_text("openai")),
            ("model", sql_text("gpt-5.6-terra")),
        ]);
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::ProviderAttempts,
            &fixture_date(),
            "part-provider_attempt-a-r1.parquet",
            &attempt_started,
        );

        let mut attempt_completed = base_values(
            &scope,
            "provider_attempt",
            "call-1:a1",
            3,
            3,
            "completed",
            "attempt:1:r3",
        );
        attempt_completed.extend([
            ("llm_call_id", sql_text("call-1")),
            ("trace_id", sql_text("trace-1")),
            ("provider_attempt_id", sql_text("call-1:a1")),
            ("provider_attempt_index", "1".to_string()),
            ("effective_profile", sql_text("chat-fallback")),
            ("profile", sql_text("chat-fallback")),
            ("provider", sql_text("openai")),
            ("model", sql_text("gpt-5.6-terra")),
            ("attempt_terminal_state", sql_text("succeeded")),
            ("transport_success", "true".to_string()),
            ("input_tokens", "120".to_string()),
            ("output_tokens", "30".to_string()),
            ("total_tokens", "150".to_string()),
            ("cost_usd", "0.25".to_string()),
            ("pricing_version", sql_text("2026-07-22")),
            ("cost_source", sql_text("provider")),
        ]);
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::ProviderAttempts,
            &fixture_date(),
            "part-provider_attempt-a-r3.parquet",
            &attempt_completed,
        );

        let mut gap = base_values(
            &scope,
            "capture_gap",
            "gap-1",
            1,
            5,
            "completed",
            "gap:1:r1",
        );
        gap.extend([
            ("gap_id", sql_text("gap-1")),
            ("gap_reason", sql_text("critical_buffer_saturated")),
            ("missing_record_count", "3".to_string()),
        ]);
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::CaptureGaps,
            &fixture_date(),
            "part-capture_gap-a-r1.parquet",
            &gap,
        );
        let mut transport_gap = base_values(
            &scope,
            "capture_gap",
            "gap-transport",
            1,
            6,
            "completed",
            "gap:transport:r1",
        );
        transport_gap.extend([
            ("gap_id", sql_text("gap-transport")),
            (
                "gap_reason",
                sql_text("runtime_transport_events_unclassified_due_broadcast_lag"),
            ),
            ("missing_record_count", "7".to_string()),
        ]);
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::CaptureGaps,
            &fixture_date(),
            "part-capture_gap-transport-r1.parquet",
            &transport_gap,
        );

        let service = LlmAnalyticsReadService::new(workspace);
        let mut query = LlmFactQuery::for_relation(LlmFactRelation::Calls);
        query.columns = [
            "llm_call_id",
            "trace_id",
            "requested_profile",
            "selected_profile",
            "effective_profile",
            "provider",
            "model",
            "prompt_projection_mode",
            "input_tokens",
            "cost_usd",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let page = service.query_facts(&scope, query).expect("calls");
        assert_eq!(page.total, 1);
        let row = &page.rows[0];
        assert_eq!(row["trace_id"], Value::from("trace-1"));
        assert_eq!(row["requested_profile"], Value::from("adaptive"));
        assert_eq!(row["selected_profile"], Value::from("chat-fast"));
        assert_eq!(row["effective_profile"], Value::from("chat-fallback"));
        assert_eq!(row["provider"], Value::from("openai"));
        assert_eq!(row["model"], Value::from("gpt-5.6-terra"));
        assert_eq!(row["prompt_projection_mode"], Value::from("rebootstrap"));
        assert_eq!(row["input_tokens"], Value::from(120_u64));
        assert_eq!(row["cost_usd"], Value::from(0.25));

        let overview = service.overview(&scope).expect("overview");
        assert_eq!(overview.logical_calls, 1);
        assert_eq!(overview.provider_attempts, 1);
        assert_eq!(overview.captured_fact_revisions, 4);
        assert_eq!(overview.capture_gaps, 3);
        assert_eq!(overview.known_missing_fact_revisions, 3);
        assert_eq!(overview.unclassified_transport_events_lost, 7);
        assert_eq!(overview.validation_attempted_calls, 1);
        assert_eq!(overview.valid_contract_calls, 1);
        assert_eq!(overview.usage_observed_calls, 1);
        assert_eq!(overview.cost_observed_calls, 1);
        assert_eq!(overview.input_tokens, 120);
        assert_eq!(overview.total_cost_usd, 0.25);
        assert_eq!(overview.cost_currency, "USD");
        assert_eq!(overview.pricing_versions, vec!["2026-07-22".to_string()]);
        assert_eq!(overview.cost_sources, vec!["provider".to_string()]);

        let envelope = service
            .overview_envelope(&scope, None, None)
            .expect("overview envelope");
        assert_eq!(envelope.coverage.observed, Some(4));
        assert_eq!(envelope.coverage.missing, Some(3));
        assert_eq!(envelope.coverage.eligible, Some(7));
    }

    #[test]
    fn legacy_rows_are_normalized_without_trusting_embedded_scope() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        let path = workspace
            .analytics_llm_calls_root("owner", "default")
            .join(format!("dt={}", fixture_date()))
            .join("batch_legacy.parquet");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let connection = Connection::open_in_memory().expect("duckdb");
        connection
            .execute_batch(&format!(
                "CREATE TABLE legacy(timestamp_ms BIGINT, principal VARCHAR, workspace VARCHAR, llm_call_id VARCHAR, operation VARCHAR, provider VARCHAR, model VARCHAR, input_tokens BIGINT, cost_usd DOUBLE); INSERT INTO legacy VALUES ({}, 'forged', 'other', 'legacy-call', 'legacy_chat', 'openai', 'legacy-model', 20, 0.1); COPY legacy TO '{}' (FORMAT PARQUET)",
                Utc::now().timestamp_millis() - 1_000,
                escape_sql_literal(&path.display().to_string())
            ))
            .expect("legacy fixture");

        let service = LlmAnalyticsReadService::new(workspace);
        let mut query = LlmFactQuery::for_relation(LlmFactRelation::Calls);
        query.columns = [
            "llm_call_id",
            "principal",
            "workspace",
            "capture_status",
            "training_eligible_at_capture",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let page = service.query_facts(&scope, query).expect("legacy page");
        assert_eq!(page.total, 1);
        assert_eq!(page.rows[0]["principal"], Value::from("owner"));
        assert_eq!(page.rows[0]["workspace"], Value::from("default"));
        assert_eq!(page.rows[0]["capture_status"], Value::from("metadata_only"));
        assert_eq!(
            page.rows[0]["training_eligible_at_capture"],
            Value::from(false)
        );
    }

    #[test]
    fn shared_fact_view_redacts_legacy_content_and_normalizes_blank_call_identity() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        let path = workspace
            .analytics_llm_calls_root("owner", "default")
            .join(format!("dt={}", fixture_date()))
            .join("batch_private_legacy.parquet");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let connection = Connection::open_in_memory().expect("duckdb");
        connection
            .execute_batch(&format!(
                "CREATE TABLE legacy(timestamp_ms BIGINT, llm_call_id VARCHAR, execution_id VARCHAR, response_kind VARCHAR, error VARCHAR); \
                 INSERT INTO legacy VALUES ({}, '   ', 'legacy-execution', 'private response text', 'secret provider response'); \
                 COPY legacy TO '{}' (FORMAT PARQUET)",
                Utc::now().timestamp_millis() - 1_000,
                escape_sql_literal(&path.display().to_string())
            ))
            .expect("legacy fixture");

        let service = LlmAnalyticsReadService::new(workspace);
        let mut query = LlmFactQuery::for_relation(LlmFactRelation::Calls);
        query.columns = ["stable_id", "llm_call_id", "response_kind", "error"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let page = service.query_facts(&scope, query).expect("legacy page");

        assert_eq!(page.total, 1);
        assert_eq!(page.rows[0]["stable_id"], Value::from("legacy-execution"));
        assert_eq!(page.rows[0]["llm_call_id"], Value::Null);
        assert_eq!(
            page.rows[0]["response_kind"],
            Value::from(LEGACY_INVALID_CATEGORY_REDACTION)
        );
        assert_eq!(page.rows[0]["error"], Value::from(LEGACY_ERROR_REDACTION));
        let serialized = serde_json::to_string(&page.rows).expect("serialize rows");
        assert!(!serialized.contains("secret provider response"));
        assert!(!serialized.contains("private response text"));
    }

    #[test]
    fn legacy_schema_gaps_remain_unknown_instead_of_fabricating_measurements() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        let path = workspace
            .analytics_llm_calls_root("owner", "default")
            .join(format!("dt={}", fixture_date()))
            .join("batch_sparse_legacy.parquet");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let connection = Connection::open_in_memory().expect("duckdb");
        connection
            .execute_batch(&format!(
                "CREATE TABLE legacy(timestamp_ms BIGINT, llm_call_id VARCHAR); \
                 INSERT INTO legacy VALUES ({}, 'sparse-legacy-call'); \
                 COPY legacy TO '{}' (FORMAT PARQUET)",
                Utc::now().timestamp_millis() - 1_000,
                escape_sql_literal(&path.display().to_string())
            ))
            .expect("sparse legacy fixture");

        let service = LlmAnalyticsReadService::new(workspace);
        let mut query = LlmFactQuery::for_relation(LlmFactRelation::Calls);
        query.columns = [
            "llm_call_id",
            "transport_success",
            "input_tokens",
            "output_tokens",
            "cost_usd",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let page = service.query_facts(&scope, query).expect("legacy page");

        assert_eq!(page.total, 1);
        assert_eq!(
            page.rows[0]["llm_call_id"],
            Value::from("sparse-legacy-call")
        );
        for field in [
            "transport_success",
            "input_tokens",
            "output_tokens",
            "cost_usd",
        ] {
            assert_eq!(
                page.rows[0][field],
                Value::Null,
                "{field} must stay unknown"
            );
        }
    }

    #[test]
    fn terminal_usage_reader_accepts_harness_estimates_and_rejects_physical_billing() {
        for (label, source, attempts, dispatch, response, accepted) in [
            ("estimated", "estimated", 0, false, false, true),
            ("unknown", "unknown", 0, false, false, true),
            ("computed", "computed", 0, false, false, false),
            ("attempt", "estimated", 1, false, false, false),
            ("dispatch", "estimated", 0, true, false, false),
            ("response", "estimated", 0, false, true, false),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let workspace = ArtifactV2Workspace::new(temp.path());
            let scope = LlmScope::new("owner", label);
            let mut values = base_values(
                &scope,
                "call_fact",
                "harness-call",
                2,
                2,
                "completed",
                "fixture",
            );
            values.extend([
                ("response_kind", sql_text("harness_aggregate")),
                ("provider_attempt_count", attempts.to_string()),
                (
                    "dispatch_job_id",
                    if dispatch {
                        sql_text("fake-job")
                    } else {
                        "NULL".into()
                    },
                ),
                (
                    "provider_response_id",
                    if response {
                        sql_text("fake-response")
                    } else {
                        "NULL".into()
                    },
                ),
                ("pricing_version", sql_text("harness-reported")),
                ("cost_source", sql_text(source)),
                (
                    "cost_usd",
                    if source == "unknown" {
                        "NULL".into()
                    } else {
                        "0.125".into()
                    },
                ),
                ("input_tokens", "100".into()),
                ("cache_read_tokens", "NULL".into()),
            ]);
            write_fact_revision(
                &workspace,
                &scope,
                LlmCanonicalDataset::Calls,
                &fixture_date(),
                "part-call_fact-harness-r2.parquet",
                &values,
            );
            let mut query = LlmFactQuery::for_relation(LlmFactRelation::Calls);
            query.columns = ["cost_usd", "cache_read_tokens", "provider_attempt_count"]
                .into_iter()
                .map(str::to_string)
                .collect();
            let result = LlmAnalyticsReadService::new(workspace).query_facts(&scope, query);
            if accepted {
                let page = result.unwrap_or_else(|error| panic!("{label}: {error}"));
                assert_eq!(page.total, 1);
                assert_eq!(page.rows[0]["cache_read_tokens"], Value::Null);
                assert_eq!(page.rows[0]["provider_attempt_count"], Value::from(0));
                assert_eq!(
                    page.rows[0]["cost_usd"],
                    if source == "unknown" {
                        Value::Null
                    } else {
                        Value::from(0.125)
                    }
                );
            } else {
                assert!(result.is_err(), "{label} must fail closed");
            }
        }
    }

    #[test]
    fn compatibility_mirror_is_hidden_when_canonical_call_exists() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        let call_id = "mirrored-call";
        let mut canonical = base_values(
            &scope,
            "call_fact",
            call_id,
            2,
            2,
            "completed",
            "mirrored-call:r2",
        );
        canonical.extend([
            ("llm_call_id", sql_text(call_id)),
            ("input_tokens", "11".to_string()),
            ("pricing_version", sql_text("test-pricing-v1")),
            ("cost_source", sql_text("computed")),
            ("cost_usd", "0.02".to_string()),
            ("capture_status", sql_text("metadata_only")),
        ]);
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::Calls,
            &fixture_date(),
            "part-call_fact-mirror-r2.parquet",
            &canonical,
        );

        let path = workspace
            .analytics_llm_calls_root("owner", "default")
            .join(format!("dt={}", fixture_date()))
            .join("batch_mirror.parquet");
        let connection = Connection::open_in_memory().expect("duckdb");
        connection
            .execute_batch(&format!(
                "CREATE TABLE legacy(timestamp_ms BIGINT, llm_call_id VARCHAR, input_tokens BIGINT, cost_usd DOUBLE); INSERT INTO legacy VALUES ({}, '{}', 11, 0.02); COPY legacy TO '{}' (FORMAT PARQUET)",
                Utc::now().timestamp_millis() - 1_000,
                call_id,
                escape_sql_literal(&path.display().to_string())
            ))
            .expect("legacy mirror fixture");

        let service = LlmAnalyticsReadService::new(workspace);
        let mut query = LlmFactQuery::for_relation(LlmFactRelation::Calls);
        query.columns = ["llm_call_id", "record_kind", "input_tokens", "cost_usd"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let page = service.query_facts(&scope, query).expect("deduped calls");
        assert_eq!(page.total, 1);
        assert_eq!(page.rows[0]["llm_call_id"], Value::from(call_id));
        assert_eq!(page.rows[0]["record_kind"], Value::from("call_fact"));
        assert_eq!(page.rows[0]["input_tokens"], Value::from(11_u64));
        assert_eq!(page.rows[0]["cost_usd"], Value::from(0.02));
    }

    #[test]
    fn scoped_dispatch_timing_enriches_queued_attempt_and_call() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        let date = fixture_date();
        let mut call = base_values(
            &scope,
            "call_fact",
            "queued-call",
            2,
            2,
            "completed",
            "queued-call:r2",
        );
        call.extend([
            ("llm_call_id", sql_text("queued-call")),
            ("dispatch_job_id", sql_text("job-1")),
            ("latency_ms", "125".to_string()),
        ]);
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::Calls,
            &date,
            "part-call_fact-queued-r2.parquet",
            &call,
        );
        let mut attempt = base_values(
            &scope,
            "provider_attempt",
            "queued-call:a1",
            3,
            3,
            "completed",
            "queued-call:a1:r3",
        );
        attempt.extend([
            ("llm_call_id", sql_text("queued-call")),
            ("provider_attempt_id", sql_text("queued-call:a1")),
            ("provider_attempt_index", "1".to_string()),
            ("dispatch_job_id", sql_text("job-1")),
            ("started_at_ms", "NULL".to_string()),
            ("latency_ms", "NULL".to_string()),
        ]);
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::ProviderAttempts,
            &date,
            "part-provider_attempt-queued-r3.parquet",
            &attempt,
        );

        let dispatch_path = workspace
            .analytics_root("owner", "default")
            .join("llm_dispatch")
            .join(format!("dt={date}"))
            .join("batch_timing.parquet");
        std::fs::create_dir_all(dispatch_path.parent().expect("dispatch parent"))
            .expect("dispatch directory");
        Connection::open_in_memory()
            .expect("duckdb")
            .execute_batch(&format!(
                "CREATE TABLE dispatch(job_id VARCHAR, llm_call_id VARCHAR, state VARCHAR, response_reused BOOLEAN, provider_attempt_count INTEGER, dispatched_at_ms BIGINT, wait_ms BIGINT, execution_ms BIGINT, local_prep_ms BIGINT, completed_at_ms BIGINT); \
                 INSERT INTO dispatch VALUES ('job-1', 'queued-call', 'completed', false, 1, {}, 20, 80, 5, {}); \
                 COPY dispatch TO '{}' (FORMAT PARQUET)",
                Utc::now().timestamp_millis() - 80,
                Utc::now().timestamp_millis(),
                escape_sql_literal(&dispatch_path.display().to_string()),
            ))
            .expect("dispatch timing fixture");

        let service = LlmAnalyticsReadService::new(workspace);
        let mut query = LlmFactQuery::for_relation(LlmFactRelation::ProviderAttempts);
        query.columns = [
            "llm_call_id",
            "queue_wait_ms",
            "provider_execution_ms",
            "local_prep_ms",
            "latency_ms",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let page = service
            .query_facts(&scope, query)
            .expect("enriched attempt");
        assert_eq!(page.total, 1);
        assert_eq!(page.rows[0]["queue_wait_ms"], Value::Null);
        assert_eq!(page.rows[0]["provider_execution_ms"], Value::from(80_u64));
        assert_eq!(page.rows[0]["local_prep_ms"], Value::Null);
        assert_eq!(page.rows[0]["latency_ms"], Value::from(80_u64));

        let overview = service.overview(&scope).expect("enriched overview");
        assert_eq!(overview.average_queue_wait_ms, Some(20.0));
        assert_eq!(overview.average_provider_execution_ms, Some(80.0));
        assert_eq!(overview.average_local_prep_ms, Some(5.0));
        assert_eq!(overview.average_latency_ms, Some(125.0));
    }

    #[test]
    fn multi_attempt_dispatch_timing_stays_on_call_instead_of_final_attempt() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        let date = fixture_date();
        let mut call = base_values(
            &scope,
            "call_fact",
            "fallback-call",
            2,
            2,
            "completed",
            "fallback-call:r2",
        );
        call.extend([
            ("llm_call_id", sql_text("fallback-call")),
            ("dispatch_job_id", sql_text("job-fallback")),
            ("provider_attempt_count", "2".to_string()),
            ("latency_ms", "175".to_string()),
        ]);
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::Calls,
            &date,
            "part-call_fact-fallback-r2.parquet",
            &call,
        );
        let mut attempt = base_values(
            &scope,
            "provider_attempt",
            "fallback-call:a2",
            3,
            3,
            "completed",
            "fallback-call:a2:r3",
        );
        attempt.extend([
            ("llm_call_id", sql_text("fallback-call")),
            ("provider_attempt_id", sql_text("fallback-call:a2")),
            ("provider_attempt_index", "2".to_string()),
            ("dispatch_job_id", sql_text("job-fallback")),
            ("started_at_ms", "NULL".to_string()),
            ("provider_execution_ms", "NULL".to_string()),
            ("latency_ms", "NULL".to_string()),
        ]);
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::ProviderAttempts,
            &date,
            "part-provider_attempt-fallback-r3.parquet",
            &attempt,
        );

        let dispatch_path = workspace
            .analytics_root("owner", "default")
            .join("llm_dispatch")
            .join(format!("dt={date}"))
            .join("batch_fallback_timing.parquet");
        std::fs::create_dir_all(dispatch_path.parent().expect("dispatch parent"))
            .expect("dispatch directory");
        Connection::open_in_memory()
            .expect("duckdb")
            .execute_batch(&format!(
                "CREATE TABLE dispatch(job_id VARCHAR, llm_call_id VARCHAR, state VARCHAR, response_reused BOOLEAN, provider_attempt_count INTEGER, dispatched_at_ms BIGINT, wait_ms BIGINT, execution_ms BIGINT, local_prep_ms BIGINT, completed_at_ms BIGINT); \
                 INSERT INTO dispatch VALUES ('job-fallback', 'fallback-call', 'completed', false, 2, {}, 25, 130, 10, {}); \
                 COPY dispatch TO '{}' (FORMAT PARQUET)",
                Utc::now().timestamp_millis() - 130,
                Utc::now().timestamp_millis(),
                escape_sql_literal(&dispatch_path.display().to_string()),
            ))
            .expect("dispatch timing fixture");

        let service = LlmAnalyticsReadService::new(workspace);
        let mut query = LlmFactQuery::for_relation(LlmFactRelation::ProviderAttempts);
        query.columns = [
            "provider_attempt_id",
            "started_at_ms",
            "queue_wait_ms",
            "provider_execution_ms",
            "local_prep_ms",
            "latency_ms",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let page = service
            .query_facts(&scope, query)
            .expect("non-invented final attempt");
        assert_eq!(page.total, 1);
        assert_eq!(
            page.rows[0]["provider_attempt_id"],
            Value::from("fallback-call:a2")
        );
        for field in [
            "started_at_ms",
            "queue_wait_ms",
            "provider_execution_ms",
            "local_prep_ms",
            "latency_ms",
        ] {
            assert_eq!(
                page.rows[0][field],
                Value::Null,
                "{field} must stay unknown"
            );
        }

        let overview = service
            .overview(&scope)
            .expect("call-level aggregate timing");
        assert_eq!(overview.average_queue_wait_ms, Some(25.0));
        assert_eq!(overview.average_provider_execution_ms, Some(130.0));
        assert_eq!(overview.average_local_prep_ms, Some(10.0));
        assert_eq!(overview.average_latency_ms, Some(175.0));
    }

    #[test]
    fn dispatch_enrichment_rejects_embedded_scope_or_negative_timing_drift() {
        for (workspace_name, embedded_principal, wait_ms, expected) in [
            (
                "dispatch-scope-drift",
                "different-owner",
                10,
                "scope mismatch",
            ),
            (
                "dispatch-negative-timing",
                "owner",
                -1,
                "invalid identity or timing",
            ),
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            let workspace = ArtifactV2Workspace::new(temp.path());
            let scope = LlmScope::new("owner", workspace_name);
            let date = fixture_date();
            let dispatch_path = workspace
                .analytics_root(&scope.principal, &scope.workspace)
                .join("llm_dispatch")
                .join(format!("dt={date}"))
                .join("batch_invalid_timing.parquet");
            std::fs::create_dir_all(dispatch_path.parent().expect("dispatch parent"))
                .expect("dispatch directory");
            Connection::open_in_memory()
                .expect("duckdb")
                .execute_batch(&format!(
                    "CREATE TABLE dispatch(principal VARCHAR, workspace VARCHAR, job_id VARCHAR, llm_call_id VARCHAR, state VARCHAR, response_reused BOOLEAN, provider_attempt_count INTEGER, dispatched_at_ms BIGINT, wait_ms BIGINT, execution_ms BIGINT, completed_at_ms BIGINT); \
                     INSERT INTO dispatch VALUES ({}, {}, 'job-1', 'call-1', 'completed', false, 1, {}, {}, 20, {}); \
                     COPY dispatch TO '{}' (FORMAT PARQUET)",
                    sql_text(embedded_principal),
                    sql_text(workspace_name),
                    Utc::now().timestamp_millis() - 20,
                    wait_ms,
                    Utc::now().timestamp_millis(),
                    escape_sql_literal(&dispatch_path.display().to_string()),
                ))
                .expect("dispatch integrity fixture");

            let error = LlmAnalyticsReadService::new(workspace)
                .query_facts(&scope, LlmFactQuery::for_relation(LlmFactRelation::Calls))
                .expect_err("untrusted dispatch enrichment must fail closed");
            assert!(
                error.to_string().contains(expected),
                "{workspace_name} produced an unexpected error: {error}"
            );
        }
    }

    #[test]
    fn canonical_schema_evolution_projects_missing_columns_as_typed_nulls() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        let mut values = base_values(
            &scope,
            "call_fact",
            "old-call",
            1,
            1,
            "started",
            "old-call:r1",
        );
        values.push(("operation", sql_text("old_operation")));
        write_fact_revision_omitting(
            &workspace,
            &scope,
            LlmCanonicalDataset::Calls,
            &fixture_date(),
            "part-call_fact-old-r1.parquet",
            &values,
            &["model"],
        );

        let service = LlmAnalyticsReadService::new(workspace);
        let mut query = LlmFactQuery::for_relation(LlmFactRelation::Calls);
        query.columns = ["stable_id", "operation", "model", "capture_status"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let page = service
            .query_facts(&scope, query)
            .expect("schema evolution");
        assert_eq!(page.total, 1);
        assert_eq!(page.rows[0]["stable_id"], Value::from("old-call"));
        assert_eq!(page.rows[0]["operation"], Value::from("old_operation"));
        assert_eq!(page.rows[0]["model"], Value::Null);
        assert_eq!(page.rows[0]["capture_status"], Value::from("metadata_only"));
    }

    #[test]
    fn canonical_source_missing_integrity_columns_fails_closed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        let path = workspace
            .analytics_llm_calls_root("owner", "default")
            .join(format!("dt={}", fixture_date()))
            .join("part-call_fact-ungoverned-r1.parquet");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        Connection::open_in_memory()
            .expect("duckdb")
            .execute_batch(&format!(
                "CREATE TABLE ungoverned(stable_id VARCHAR, record_revision INTEGER, journal_sequence UBIGINT, record_kind VARCHAR, principal VARCHAR, workspace VARCHAR, timestamp_ms BIGINT); INSERT INTO ungoverned VALUES ('old-call', 1, 1, 'call_fact', 'owner', 'default', {}); COPY ungoverned TO '{}' (FORMAT PARQUET)",
                Utc::now().timestamp_millis() - 1_000,
                escape_sql_literal(&path.display().to_string())
            ))
            .expect("ungoverned canonical fixture");

        let error = LlmAnalyticsReadService::new(workspace)
            .query_facts(&scope, LlmFactQuery::for_relation(LlmFactRelation::Calls))
            .expect_err("missing governance columns must fail closed");
        assert!(error
            .to_string()
            .contains("missing required identity column"));
    }

    #[test]
    fn memory_decision_local_costs_are_readable_without_inventing_usage() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "local-decision");
        for (kind, stable_id, revision, sequence, dataset) in [
            ("call_fact", "local-call", 1, 1, LlmCanonicalDataset::Calls),
            (
                "provider_attempt",
                "local-call:a1",
                1,
                2,
                LlmCanonicalDataset::ProviderAttempts,
            ),
            (
                "provider_attempt",
                "local-call:a1",
                3,
                3,
                LlmCanonicalDataset::ProviderAttempts,
            ),
            ("call_fact", "local-call", 2, 4, LlmCanonicalDataset::Calls),
        ] {
            let mut values = base_values(&scope, kind, stable_id, revision, sequence, "", "");
            if kind == "provider_attempt" {
                values.extend([
                    ("provider", sql_text("decision:kev-mlx")),
                    ("model", sql_text("kev-4b-mlx")),
                ]);
            }
            if revision > 1 {
                values.extend([
                    ("pricing_version", sql_text("local-decision-v1")),
                    ("cost_source", sql_text("local")),
                    ("cost_usd", "0.0".to_string()),
                ]);
                if kind == "call_fact" {
                    values.push(("response_kind", sql_text("decision_model")));
                }
            }
            write_fact_revision(
                &workspace,
                &scope,
                dataset,
                &fixture_date(),
                &format!("part-{kind}-{sequence}-r{revision}.parquet"),
                &values,
            );
        }
        let service = LlmAnalyticsReadService::new(workspace);
        let mut query = LlmFactQuery::for_relation(LlmFactRelation::Calls);
        query.columns = [
            "model",
            "cost_source",
            "cost_usd",
            "input_tokens",
            "cache_read_tokens",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let page = service
            .query_facts(&scope, query)
            .expect("local decision facts");
        assert_eq!(page.total, 1);
        assert_eq!(page.rows[0]["model"], "kev-4b-mlx");
        assert_eq!(page.rows[0]["cost_source"], "local");
        assert_eq!(page.rows[0]["cost_usd"], 0.0);
        assert!(page.rows[0]["input_tokens"].is_null());
        assert!(page.rows[0]["cache_read_tokens"].is_null());
        let overview = service.overview(&scope).expect("local decision overview");
        assert_eq!(overview.logical_calls, 1);
        assert_eq!(overview.provider_attempts, 1);
        assert_eq!(overview.cost_observed_calls, 1);
        assert_eq!(overview.usage_observed_calls, 0);
        assert_eq!(overview.total_cost_usd, 0.0);
        assert_eq!(overview.cost_sources, ["local"]);
    }

    #[test]
    fn canonical_reader_rejects_content_usage_pricing_and_timing_integrity_drift() {
        let cases = [
            (
                "content-category",
                vec![("response_kind", sql_text("private response text"))],
            ),
            (
                "partial-realtime-usage",
                vec![
                    ("input_tokens", "10".to_string()),
                    ("output_tokens", "2".to_string()),
                    ("total_tokens", "12".to_string()),
                    ("cache_read_tokens", "1".to_string()),
                    ("cache_creation_tokens", "0".to_string()),
                    ("audio_input_tokens", "1".to_string()),
                ],
            ),
            ("unproven-cost", vec![("cost_usd", "0.5".to_string())]),
            (
                "nonzero-local-cost",
                vec![
                    ("pricing_version", sql_text("local-decision-v1")),
                    ("cost_source", sql_text("local")),
                    ("cost_usd", "0.5".to_string()),
                ],
            ),
            (
                "content-bearing-pricing-version",
                vec![
                    (
                        "pricing_version",
                        sql_text("rate derived from private agreement"),
                    ),
                    ("cost_source", sql_text("computed")),
                    ("cost_usd", "0.5".to_string()),
                ],
            ),
            (
                "malformed-pricing-row-fingerprint",
                vec![
                    (
                        "pricing_version",
                        sql_text("pricing-row-v1:not-a-fingerprint"),
                    ),
                    ("cost_source", sql_text("computed")),
                    ("cost_usd", "0.5".to_string()),
                ],
            ),
            (
                "blank-superseding-call",
                vec![("superseded_by_call_id", sql_text(" "))],
            ),
            (
                "impossible-ttft",
                vec![
                    ("started_at_ms", "10".to_string()),
                    ("first_token_at_ms", "18".to_string()),
                    ("ttft_ms", "8".to_string()),
                    ("generation_after_ttft_ms", "5".to_string()),
                    ("latency_ms", "10".to_string()),
                ],
            ),
        ];

        for (index, (case, overrides)) in cases.into_iter().enumerate() {
            let temp = tempfile::tempdir().expect("tempdir");
            let workspace = ArtifactV2Workspace::new(temp.path());
            let scope = LlmScope::new("owner", case);
            let stable_id = format!("invalid-{case}");
            let mut values = base_values(
                &scope,
                "call_fact",
                &stable_id,
                2,
                index as u64 + 1,
                "completed",
                &stable_id,
            );
            values.extend(overrides);
            write_fact_revision(
                &workspace,
                &scope,
                LlmCanonicalDataset::Calls,
                &fixture_date(),
                &format!("part-call_fact-{case}-r2.parquet"),
                &values,
            );

            let error = LlmAnalyticsReadService::new(workspace)
                .query_facts(&scope, LlmFactQuery::for_relation(LlmFactRelation::Calls))
                .expect_err("corrupt canonical fact must fail closed");
            assert!(
                error
                    .to_string()
                    .contains("invalid usage, pricing, timing, or validation facts"),
                "unexpected {case} error: {error}"
            );
        }
    }

    #[test]
    fn canonical_reader_rejects_immutable_revision_drift() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "revision-drift");
        let started = base_values(
            &scope,
            "call_fact",
            "drift-call",
            1,
            1,
            "started",
            "drift-call:r1",
        );
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::Calls,
            &fixture_date(),
            "part-call_fact-drift-r1.parquet",
            &started,
        );
        let mut completed = base_values(
            &scope,
            "call_fact",
            "drift-call",
            2,
            2,
            "completed",
            "drift-call:r2",
        );
        completed.push(("trace_id", sql_text("different-trace")));
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::Calls,
            &fixture_date(),
            "part-call_fact-drift-r2.parquet",
            &completed,
        );

        let error = LlmAnalyticsReadService::new(workspace)
            .query_facts(&scope, LlmFactQuery::for_relation(LlmFactRelation::Calls))
            .expect_err("immutable revision drift must fail closed");
        assert!(
            error.to_string().contains("immutable identity"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn canonical_reader_rejects_cross_dataset_ownership_and_sequence_drift() {
        for duplicate_sequence in [false, true] {
            let temp = tempfile::tempdir().expect("tempdir");
            let workspace = ArtifactV2Workspace::new(temp.path());
            let scope = LlmScope::new(
                "owner",
                if duplicate_sequence {
                    "sequence-drift"
                } else {
                    "attempt-owner-drift"
                },
            );
            let call = base_values(
                &scope,
                "call_fact",
                "owner-call",
                2,
                1,
                "completed",
                "owner-call:r2",
            );
            write_fact_revision(
                &workspace,
                &scope,
                LlmCanonicalDataset::Calls,
                &fixture_date(),
                "part-call_fact-owner-r2.parquet",
                &call,
            );

            if duplicate_sequence {
                let gap = base_values(
                    &scope,
                    "capture_gap",
                    "duplicate-sequence-gap",
                    1,
                    1,
                    "reported",
                    "duplicate-sequence-gap:r1",
                );
                write_fact_revision(
                    &workspace,
                    &scope,
                    LlmCanonicalDataset::CaptureGaps,
                    &fixture_date(),
                    "part-capture_gap-duplicate-sequence-r1.parquet",
                    &gap,
                );
            } else {
                let mut attempt = base_values(
                    &scope,
                    "provider_attempt",
                    "owner-call:a1",
                    3,
                    2,
                    "completed",
                    "owner-call:a1:r3",
                );
                attempt.push(("trace_id", sql_text("different-trace")));
                write_fact_revision(
                    &workspace,
                    &scope,
                    LlmCanonicalDataset::ProviderAttempts,
                    &fixture_date(),
                    "part-provider_attempt-owner-r3.parquet",
                    &attempt,
                );
            }

            let error = LlmAnalyticsReadService::new(workspace)
                .query_facts(&scope, LlmFactQuery::for_relation(LlmFactRelation::Calls))
                .expect_err("cross-dataset lifecycle drift must fail closed");
            let expected = if duplicate_sequence {
                "reused across canonical datasets"
            } else {
                "provider-attempt/call relationship"
            };
            assert!(
                error.to_string().contains(expected),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn canonical_reader_rejects_attempts_without_an_effective_route() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "missing-attempt-route");
        let attempt = base_values(
            &scope,
            "provider_attempt",
            "missing-route-call:a1",
            3,
            1,
            "completed",
            "missing-route-call:a1:r3",
        )
        .into_iter()
        .filter(|(column, _)| *column != "provider")
        .collect::<Vec<_>>();
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::ProviderAttempts,
            &fixture_date(),
            "part-provider_attempt-missing-route-r3.parquet",
            &attempt,
        );

        let error = LlmAnalyticsReadService::new(workspace)
            .query_facts(
                &scope,
                LlmFactQuery::for_relation(LlmFactRelation::ProviderAttempts),
            )
            .expect_err("attempt without provider must fail closed");
        assert!(error.to_string().contains("invalid dataset identity"));
    }

    #[test]
    fn canonical_reader_rejects_orphan_provider_attempt_lifecycles() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "orphan-attempt");
        let attempt = base_values(
            &scope,
            "provider_attempt",
            "absent-call:a1",
            3,
            1,
            "completed",
            "absent-call:a1:r3",
        );
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::ProviderAttempts,
            &fixture_date(),
            "part-provider_attempt-orphan-r3.parquet",
            &attempt,
        );

        let error = LlmAnalyticsReadService::new(workspace)
            .query_facts(
                &scope,
                LlmFactQuery::for_relation(LlmFactRelation::ProviderAttempts),
            )
            .expect_err("provider attempt without a logical owner must fail closed");
        assert!(
            error.to_string().contains("without a logical-call owner"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn canonical_reader_rejects_orphan_call_owned_gaps() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "orphan-gap");
        let mut gap = base_values(
            &scope,
            "capture_gap",
            "orphan-call-gap",
            1,
            1,
            "reported",
            "orphan-call-gap:r1",
        );
        gap.push(("llm_call_id", sql_text("absent-call")));
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::CaptureGaps,
            &fixture_date(),
            "part-capture_gap-orphan-r1.parquet",
            &gap,
        );

        let error = LlmAnalyticsReadService::new(workspace)
            .query_facts(
                &scope,
                LlmFactQuery::for_relation(LlmFactRelation::CaptureGaps),
            )
            .expect_err("call-owned gap without a logical owner must fail closed");
        assert!(
            error.to_string().contains("call-owned capture gap"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn canonical_reader_rejects_final_attempt_usage_or_pricing_drift() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "final-attempt-fact-drift");
        let mut call = base_values(
            &scope,
            "call_fact",
            "fact-drift-call",
            2,
            1,
            "completed",
            "fact-drift-call:r2",
        );
        call.extend([
            ("input_tokens", "10".to_string()),
            ("output_tokens", "2".to_string()),
            ("total_tokens", "12".to_string()),
            ("pricing_version", sql_text("test-price-v1")),
            ("cost_source", sql_text("computed")),
            ("cost_usd", "0.1".to_string()),
        ]);
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::Calls,
            &fixture_date(),
            "part-call_fact-fact-drift-r2.parquet",
            &call,
        );
        let mut attempt = base_values(
            &scope,
            "provider_attempt",
            "fact-drift-call:a1",
            3,
            2,
            "completed",
            "fact-drift-call:a1:r3",
        );
        attempt.extend([
            ("input_tokens", "11".to_string()),
            ("output_tokens", "2".to_string()),
            ("total_tokens", "13".to_string()),
            ("pricing_version", sql_text("test-price-v1")),
            ("cost_source", sql_text("computed")),
            ("cost_usd", "0.1".to_string()),
        ]);
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::ProviderAttempts,
            &fixture_date(),
            "part-provider_attempt-fact-drift-r3.parquet",
            &attempt,
        );

        let error = LlmAnalyticsReadService::new(workspace)
            .query_facts(&scope, LlmFactQuery::for_relation(LlmFactRelation::Calls))
            .expect_err("final attempt and logical call economics must agree");
        assert!(
            error.to_string().contains("usage or pricing drift"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn canonical_reader_rejects_final_attempt_transport_outcome_drift() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "terminal-outcome-drift");
        let call = base_values(
            &scope,
            "call_fact",
            "outcome-drift-call",
            2,
            1,
            "completed",
            "outcome-drift-call:r2",
        );
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::Calls,
            &fixture_date(),
            "part-call_fact-outcome-drift-r2.parquet",
            &call,
        );
        let mut attempt = base_values(
            &scope,
            "provider_attempt",
            "outcome-drift-call:a1",
            3,
            2,
            "completed",
            "outcome-drift-call:a1:r3",
        );
        attempt.extend([
            ("attempt_terminal_state", sql_text("failed")),
            ("transport_success", "false".to_string()),
            ("success", "false".to_string()),
            ("error_class", sql_text("provider_error")),
        ]);
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::ProviderAttempts,
            &fixture_date(),
            "part-provider_attempt-outcome-drift-r3.parquet",
            &attempt,
        );

        let error = LlmAnalyticsReadService::new(workspace)
            .query_facts(&scope, LlmFactQuery::for_relation(LlmFactRelation::Calls))
            .expect_err("a successful call cannot own a failed final attempt");
        assert!(
            error.to_string().contains("transport outcome mismatch"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn canonical_reader_rejects_null_required_lifecycle_values() {
        let cases = [
            ("null-schema", "call_fact", 1, "fact_schema_version"),
            ("null-lifecycle", "call_fact", 1, "lifecycle_phase"),
            ("null-capture-mode", "call_fact", 1, "capture_mode"),
            ("null-scope-resolution", "call_fact", 1, "scope_resolution"),
            ("null-call-state", "call_fact", 2, "call_terminal_state"),
            (
                "null-attempt-count",
                "call_fact",
                2,
                "provider_attempt_count",
            ),
            ("null-response-present", "call_fact", 2, "response_present"),
            ("null-call-success", "call_fact", 2, "transport_success"),
            (
                "null-attempt-state",
                "provider_attempt",
                3,
                "attempt_terminal_state",
            ),
            (
                "null-attempt-success",
                "provider_attempt",
                3,
                "transport_success",
            ),
        ];
        for (name, record_kind, revision, column) in cases {
            let temp = tempfile::tempdir().expect("tempdir");
            let workspace = ArtifactV2Workspace::new(temp.path());
            let scope = LlmScope::new("owner", name);
            let stable_id = if record_kind == "provider_attempt" {
                "required-value-call:a1"
            } else {
                "required-value-call"
            };
            let mut values = base_values(
                &scope,
                record_kind,
                stable_id,
                revision,
                1,
                "fixture",
                "fixture",
            );
            values.push((column, "NULL".to_string()));
            let dataset = if record_kind == "provider_attempt" {
                LlmCanonicalDataset::ProviderAttempts
            } else {
                LlmCanonicalDataset::Calls
            };
            write_fact_revision(
                &workspace,
                &scope,
                dataset,
                &fixture_date(),
                &format!("part-{record_kind}-{name}.parquet"),
                &values,
            );

            let relation = if record_kind == "provider_attempt" {
                LlmFactRelation::ProviderAttempts
            } else {
                LlmFactRelation::Calls
            };
            let error = LlmAnalyticsReadService::new(workspace)
                .query_facts(&scope, LlmFactQuery::for_relation(relation))
                .expect_err("canonical required values must never degrade to SQL NULL");
            assert!(
                error.to_string().contains("invalid")
                    || error.to_string().contains("unsupported schema"),
                "{name}/{column} produced an unexpected error: {error}"
            );
        }
    }

    #[test]
    fn canonical_reader_rejects_cross_record_payload_poisoning() {
        let cases = [
            (
                "call-route-poison",
                LlmCanonicalDataset::Calls,
                "call_fact",
                "poison-call",
                2,
                "provider",
                sql_text("openai"),
            ),
            (
                "attempt-validation-poison",
                LlmCanonicalDataset::ProviderAttempts,
                "provider_attempt",
                "poison-call:a1",
                3,
                "response_present",
                "true".to_string(),
            ),
            (
                "gap-route-poison",
                LlmCanonicalDataset::CaptureGaps,
                "capture_gap",
                "poison-gap",
                1,
                "model",
                sql_text("private-model"),
            ),
        ];
        for (name, dataset, record_kind, stable_id, revision, column, value) in cases {
            let temp = tempfile::tempdir().expect("tempdir");
            let workspace = ArtifactV2Workspace::new(temp.path());
            let scope = LlmScope::new("owner", name);
            let mut values = base_values(
                &scope,
                record_kind,
                stable_id,
                revision,
                1,
                "fixture",
                "fixture",
            );
            values.push((column, value));
            write_fact_revision(
                &workspace,
                &scope,
                dataset,
                &fixture_date(),
                &format!("part-{record_kind}-{name}.parquet"),
                &values,
            );
            let relation = match dataset {
                LlmCanonicalDataset::Calls => LlmFactRelation::Calls,
                LlmCanonicalDataset::ProviderAttempts => LlmFactRelation::ProviderAttempts,
                LlmCanonicalDataset::ToolCalls => LlmFactRelation::ToolCalls,
                LlmCanonicalDataset::CaptureGaps => LlmFactRelation::CaptureGaps,
            };

            let error = LlmAnalyticsReadService::new(workspace)
                .query_facts(&scope, LlmFactQuery::for_relation(relation))
                .expect_err("record-kind-specific facts must reject foreign payload columns");
            assert!(
                error.to_string().contains("invalid dataset identity"),
                "{name}/{column} produced an unexpected error: {error}"
            );
        }
    }

    #[test]
    fn canonical_reader_rejects_noncanonical_identity_whitespace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "identity-whitespace");
        let mut call = base_values(
            &scope,
            "call_fact",
            "identity-call",
            1,
            1,
            "started",
            "identity-call:r1",
        );
        call.push(("trace_id", sql_text(" trace-identity-call")));
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::Calls,
            &fixture_date(),
            "part-call_fact-identity-whitespace-r1.parquet",
            &call,
        );

        let error = LlmAnalyticsReadService::new(workspace)
            .query_facts(&scope, LlmFactQuery::for_relation(LlmFactRelation::Calls))
            .expect_err("boundary whitespace in stable identity must fail closed");
        assert!(error.to_string().contains("invalid trace context"));
    }

    #[test]
    fn canonical_reader_rejects_training_eligible_disabled_capture() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "disabled-capture-training");
        let mut call = base_values(
            &scope,
            "call_fact",
            "disabled-capture-call",
            1,
            1,
            "started",
            "disabled-capture-call:r1",
        );
        call.extend([
            ("capture_mode", sql_text("off")),
            ("capture_status", sql_text("policy_denied")),
        ]);
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::Calls,
            &fixture_date(),
            "part-call_fact-disabled-capture-r1.parquet",
            &call,
        );

        let error = LlmAnalyticsReadService::new(workspace)
            .query_facts(&scope, LlmFactQuery::for_relation(LlmFactRelation::Calls))
            .expect_err("disabled capture cannot be training eligible");
        assert!(error
            .to_string()
            .contains("invalid timing, capture, or training"));
    }

    #[test]
    fn canonical_reader_rejects_parent_cycles_even_when_each_edge_is_valid() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "parent-cycle");
        for (sequence, child, parent) in [
            (1, "cycle-call-a", "cycle-call-b"),
            (2, "cycle-call-b", "cycle-call-a"),
        ] {
            let mut call = base_values(&scope, "call_fact", child, 1, sequence, "started", child);
            call.extend([
                ("trace_id", sql_text("shared-cycle-trace")),
                ("parent_call_id", sql_text(parent)),
                ("parent_relation", sql_text("supports")),
            ]);
            write_fact_revision(
                &workspace,
                &scope,
                LlmCanonicalDataset::Calls,
                &fixture_date(),
                &format!("part-call_fact-{child}-r1.parquet"),
                &call,
            );
        }

        let error = LlmAnalyticsReadService::new(workspace)
            .query_facts(&scope, LlmFactQuery::for_relation(LlmFactRelation::Calls))
            .expect_err("cyclic parent graph must fail closed");
        assert!(error.to_string().contains("cyclic logical-call parent"));
    }

    #[test]
    fn canonical_reader_rejects_preterminal_payloads_and_malformed_gap_windows() {
        for case in ["call-start-payload", "attempt-start-payload", "gap-window"] {
            let temp = tempfile::tempdir().expect("tempdir");
            let workspace = ArtifactV2Workspace::new(temp.path());
            let scope = LlmScope::new("owner", case);
            let (dataset, file_name, mut values) = match case {
                "call-start-payload" => (
                    LlmCanonicalDataset::Calls,
                    "part-call_fact-preterminal-r1.parquet",
                    base_values(
                        &scope,
                        "call_fact",
                        "preterminal-call",
                        1,
                        1,
                        "started",
                        "preterminal-call:r1",
                    ),
                ),
                "attempt-start-payload" => (
                    LlmCanonicalDataset::ProviderAttempts,
                    "part-provider_attempt-preterminal-r1.parquet",
                    base_values(
                        &scope,
                        "provider_attempt",
                        "preterminal-call:a1",
                        1,
                        1,
                        "started",
                        "preterminal-call:a1:r1",
                    ),
                ),
                _ => (
                    LlmCanonicalDataset::CaptureGaps,
                    "part-capture_gap-window-r1.parquet",
                    base_values(
                        &scope,
                        "capture_gap",
                        "bad-gap-window",
                        1,
                        1,
                        "reported",
                        "bad-gap-window:r1",
                    ),
                ),
            };
            match case {
                "call-start-payload" => values.push(("input_tokens", "1".to_string())),
                "attempt-start-payload" => {
                    values.extend([
                        ("pricing_version", sql_text("test-price")),
                        ("cost_source", sql_text("computed")),
                        ("cost_usd", "0.1".to_string()),
                    ]);
                },
                _ => values.push((
                    "last_missing_at_ms",
                    (fixture_anchor_ms() + 10_000).to_string(),
                )),
            }
            write_fact_revision(
                &workspace,
                &scope,
                dataset,
                &fixture_date(),
                file_name,
                &values,
            );

            let relation = match dataset {
                LlmCanonicalDataset::Calls => LlmFactRelation::Calls,
                LlmCanonicalDataset::ProviderAttempts => LlmFactRelation::ProviderAttempts,
                LlmCanonicalDataset::ToolCalls => LlmFactRelation::ToolCalls,
                LlmCanonicalDataset::CaptureGaps => LlmFactRelation::CaptureGaps,
            };
            let error = LlmAnalyticsReadService::new(workspace)
                .query_facts(&scope, LlmFactQuery::for_relation(relation))
                .expect_err("invalid lifecycle payload must fail closed");
            assert!(
                error.to_string().contains("invalid dataset identity"),
                "unexpected {case} error: {error}"
            );
        }
    }

    #[test]
    fn scoped_paths_do_not_leak_other_workspace_rows() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let allowed = LlmScope::new("owner", "allowed");
        let denied = LlmScope::new("owner", "denied");
        for (scope, call, sequence) in [(&allowed, "allowed-call", 1), (&denied, "denied-call", 2)]
        {
            let mut values = base_values(scope, "call_fact", call, 1, sequence, "completed", call);
            values.push(("llm_call_id", sql_text(call)));
            write_fact_revision(
                &workspace,
                scope,
                LlmCanonicalDataset::Calls,
                &fixture_date(),
                &format!("part-call_fact-{call}-r1.parquet"),
                &values,
            );
        }
        let service = LlmAnalyticsReadService::new(workspace);
        let page = service
            .query_facts(&allowed, LlmFactQuery::for_relation(LlmFactRelation::Calls))
            .expect("allowed page");
        assert_eq!(page.total, 1);
        assert_eq!(page.rows[0]["llm_call_id"], Value::from("allowed-call"));
    }

    #[test]
    fn fact_sql_uses_ast_relation_and_table_function_allowlists() {
        let registry = LlmFactRegistry::canonical();
        assert!(validate_fact_sql(
            &registry,
            "WITH recent AS (SELECT model, cost_usd FROM llm_calls) SELECT model, sum(cost_usd) FROM recent GROUP BY model",
        )
        .is_ok());
        assert!(validate_fact_sql(
            &registry,
            "WITH recent AS (SELECT model, cost_usd FROM llm_calls), grouped AS (SELECT model, sum(cost_usd) AS cost FROM recent GROUP BY model) SELECT * FROM grouped",
        )
        .is_ok());
        for sql in [
            "SELECT 1",
            "SELECT * FROM events",
            "SELECT * FROM main.llm_calls",
            "SELECT * FROM read_parquet('/tmp/private.parquet')",
            "SELECT read_text('/tmp/private.txt') FROM llm_calls",
            "SELECT main.read_text('/tmp/private.txt') FROM llm_calls",
            "PRAGMA database_list",
            "SELECT * FROM llm_calls; SELECT * FROM llm_provider_attempts",
            "WITH llm_calls AS (SELECT * FROM llm_provider_attempts) SELECT * FROM llm_calls",
            "WITH __llm_fact_bounded_llm_calls AS (SELECT 1) SELECT * FROM llm_calls",
            "WITH RECURSIVE __llm_fact_bounded_llm_calls AS (SELECT * FROM llm_calls) SELECT * FROM __llm_fact_bounded_llm_calls",
            "WITH leaked AS (SELECT * FROM leaked UNION ALL SELECT * FROM llm_calls) SELECT * FROM leaked",
            "WITH first AS (SELECT * FROM later), later AS (SELECT * FROM llm_calls) SELECT * FROM first",
            "SELECT * FROM (WITH leaked AS (SELECT * FROM llm_calls) SELECT * FROM leaked) nested JOIN leaked ON true",
            "SELECT * INTO temporary_llm_copy FROM llm_calls",
            "WITH scoped AS (SELECT * FROM llm_calls) VALUES (1)",
            "WITH payload AS (VALUES (1)) SELECT * FROM payload",
            "SELECT * FROM (VALUES (1)) AS payload(value)",
            "TABLE llm_calls",
        ] {
            assert!(
                validate_fact_sql(&registry, sql).is_err(),
                "unsafe SQL unexpectedly accepted: {sql}"
            );
        }
    }

    #[test]
    fn common_envelopes_report_real_scope_range_coverage_and_pagination() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let service = LlmAnalyticsReadService::new(workspace);
        let scope = LlmScope::new("owner", "default");

        let overview = service
            .overview_envelope(&scope, None, None)
            .expect("overview envelope");
        assert_eq!(overview.scope, scope);
        assert_eq!(overview.coverage.eligible, Some(0));
        assert_eq!(overview.coverage.observed, Some(0));
        assert_eq!(overview.coverage.missing, Some(0));
        assert!(overview.pagination.is_none());
        assert_eq!(overview.data.cost_currency, "USD");
        assert_eq!(overview.data.usage_observed_calls, 0);
        assert_eq!(overview.data.cost_observed_calls, 0);
        assert!(overview.data.pricing_versions.is_empty());
        assert!(overview.data.cost_sources.is_empty());

        let page = service
            .query_facts_envelope(
                &scope,
                LlmFactQuery::for_relation(LlmFactRelation::ProviderAttempts),
            )
            .expect("page envelope");
        assert_eq!(page.scope, scope);
        assert_eq!(page.pagination.as_ref().map(|page| page.page), Some(1));
        assert_eq!(page.pagination.as_ref().map(|page| page.offset), Some(0));
        assert_eq!(page.pagination.as_ref().map(|page| page.total), Some(0));
        assert_eq!(page.data.relation, "llm_provider_attempts");
    }

    #[test]
    fn common_envelope_enforces_exact_serialized_response_bound() {
        let temp = tempfile::tempdir().expect("tempdir");
        let service = LlmAnalyticsReadService::new(ArtifactV2Workspace::new(temp.path()));
        let result = service.envelope(
            &LlmScope::new("owner", "default"),
            LlmReadTimeRange {
                from_ms: 1,
                to_ms: 2,
            },
            Value::Object(Map::new()),
            None,
            LlmReadCoverage {
                eligible: None,
                observed: None,
                excluded: None,
                censored: None,
                missing: None,
            },
            None,
            Vec::new(),
            "x".repeat(ANALYTICS_DUCKDB_MAX_RESULT_BYTES),
        );

        assert!(result
            .expect_err("oversized envelope must fail closed")
            .to_string()
            .contains("byte limit"));
    }

    #[test]
    fn call_and_attempt_detail_envelopes_fail_closed_to_selected_scope() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let allowed = LlmScope::new("owner", "allowed");
        let denied = LlmScope::new("owner", "denied");
        let mut call = base_values(
            &allowed,
            "call_fact",
            "allowed-call",
            1,
            1,
            "completed",
            "allowed-call:r1",
        );
        call.push(("llm_call_id", sql_text("allowed-call")));
        write_fact_revision(
            &workspace,
            &allowed,
            LlmCanonicalDataset::Calls,
            &fixture_date(),
            "part-call_fact-allowed-r1.parquet",
            &call,
        );
        let service = LlmAnalyticsReadService::new(workspace);

        assert!(service
            .read_call_detail_envelope(&allowed, "allowed-call")
            .expect("allowed detail")
            .data
            .is_some());
        assert!(service
            .read_call_detail_envelope(&denied, "allowed-call")
            .expect("denied detail")
            .data
            .is_none());
        assert!(service
            .read_provider_attempt_envelope(&denied, "attempt-in-other-scope")
            .expect("denied attempt")
            .data
            .is_none());
    }

    #[test]
    fn shared_trace_assembler_returns_ordered_calls_attempts_and_tool_stages() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "trace-workspace");
        let other_scope = LlmScope::new("owner", "other-workspace");
        let call_id = "assembled-call";
        let trace_id = "trace-assembled-call";
        let tool_id = "assembled-call:tool:call_1";

        let call = base_values(
            &scope,
            "call_fact",
            call_id,
            2,
            1,
            "completed",
            "call_fact:assembled-call:r2",
        );
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::Calls,
            &fixture_date(),
            "part-call_fact-assembled-r2.parquet",
            &call,
        );

        let proposed = base_values(
            &scope,
            "tool_lineage",
            tool_id,
            1,
            2,
            "proposed",
            "tool_lineage:assembled:r1",
        );
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::ToolCalls,
            &fixture_date(),
            "part-tool_lineage-assembled-r1.parquet",
            &proposed,
        );
        let mut finished = base_values(
            &scope,
            "tool_lineage",
            tool_id,
            2_001,
            3,
            "execution_finished",
            "tool_lineage:assembled:r2001",
        );
        finished.push(("tool_transport_ran", "true".to_string()));
        finished.push(("tool_reported_success", "true".to_string()));
        finished.push(("tool_outcome", sql_text("succeeded")));
        // `remained`, not `unknown`: this row says the transport ran and the
        // tool reported success, and under the resolution rule those two facts
        // settle the question. The old value made the fixture model a row the
        // runtime can no longer produce.
        finished.push(("tool_side_effect_state", sql_text("remained")));
        finished.push(("result_ref", sql_text("result_1")));
        finished.push(("canonical_event_ref", sql_text("event_1")));
        write_fact_revision(
            &workspace,
            &scope,
            LlmCanonicalDataset::ToolCalls,
            &fixture_date(),
            "part-tool_lineage-assembled-r2001.parquet",
            &finished,
        );

        let service = LlmAnalyticsReadService::new(workspace);
        let from_ms = fixture_anchor_ms() - 1_000;
        let to_ms = fixture_anchor_ms() + 10_000;
        let trace = service
            .read_trace_envelope(&scope, trace_id, Some(from_ms), Some(to_ms))
            .expect("assembled trace")
            .data
            .expect("trace detail");
        assert_eq!(trace.trace_id, trace_id);
        assert_eq!(trace.calls.len(), 1);
        assert_eq!(trace.tool_timeline.len(), 2);
        assert_eq!(trace.tool_timeline[0]["tool_lineage_stage"], "proposed");
        assert_eq!(
            trace.tool_timeline[1]["tool_lineage_stage"],
            "execution_finished"
        );

        let call = service
            .read_call_detail_envelope(&scope, call_id)
            .expect("call detail")
            .data
            .expect("call exists");
        assert_eq!(call.tool_timeline.len(), 2);

        assert!(service
            .read_trace_envelope(&other_scope, trace_id, Some(from_ms), Some(to_ms))
            .expect("cross-scope trace read")
            .data
            .is_none());

        let traces = service
            .list_traces_envelope(&scope, Some(from_ms), Some(to_ms), Some(10))
            .expect("trace summaries");
        assert_eq!(traces.data.row_count, 1);
        assert_eq!(traces.data.rows[0]["trace_id"], trace_id);
        assert_eq!(
            traces.data.rows[0]["tool_execution_count"],
            Value::from(1_i64)
        );
    }

    #[test]
    fn governed_tool_lineage_rejects_producing_call_context_drift() {
        let scope = LlmScope::new("owner", "tool-ownership");
        let drift_temp = tempfile::tempdir().expect("drift tempdir");
        let drift_workspace = ArtifactV2Workspace::new(drift_temp.path());
        let call_id = "drift-call";
        let call = base_values(
            &scope,
            "call_fact",
            call_id,
            2,
            1,
            "completed",
            "call-fact:drift:r2",
        );
        write_fact_revision(
            &drift_workspace,
            &scope,
            LlmCanonicalDataset::Calls,
            &fixture_date(),
            "part-call_fact-drift-r2.parquet",
            &call,
        );
        let mut drifted = base_values(
            &scope,
            "tool_lineage",
            "drift-call:tool:call_1",
            1,
            2,
            "proposed",
            "tool-lineage:drift:r1",
        );
        drifted.push(("trace_id", sql_text("trace-from-another-call")));
        write_fact_revision(
            &drift_workspace,
            &scope,
            LlmCanonicalDataset::ToolCalls,
            &fixture_date(),
            "part-tool_lineage-drift-r1.parquet",
            &drifted,
        );
        let drift_error = LlmAnalyticsReadService::new(drift_workspace)
            .query_facts(
                &scope,
                LlmFactQuery::for_relation(LlmFactRelation::ToolCalls),
            )
            .expect_err("tool/call context drift must fail closed");
        assert!(drift_error
            .to_string()
            .contains("relationship row(s) with scope or lineage drift"));
    }

    #[test]
    fn fact_sql_executes_only_over_governed_in_memory_views() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let service = LlmAnalyticsReadService::new(workspace);
        let scope = LlmScope::new("owner", "default");
        let page = service
            .query_fact_sql(
                &scope,
                "SELECT count(*) AS calls FROM llm_calls",
                None,
                None,
                Some(10),
            )
            .expect("governed SQL");
        assert_eq!(page.data.row_count, 1);
        assert_eq!(page.data.rows[0]["calls"], Value::from(0_i64));
        assert!(page.pagination.is_none());
        assert_eq!(page.coverage.censored, Some(0));
        assert!(service
            .query_fact_sql(
                &scope,
                "SELECT * FROM read_parquet('/tmp/not-allowed.parquet')",
                None,
                None,
                Some(10),
            )
            .is_err());
    }

    #[test]
    fn fact_sql_reports_truncation_without_inventing_a_total() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LlmScope::new("owner", "default");
        for (sequence, call_id) in [(1, "call-a"), (2, "call-b")] {
            let mut values = base_values(
                &scope,
                "call_fact",
                call_id,
                1,
                sequence,
                "completed",
                call_id,
            );
            values.push(("llm_call_id", sql_text(call_id)));
            write_fact_revision(
                &workspace,
                &scope,
                LlmCanonicalDataset::Calls,
                &fixture_date(),
                &format!("part-call_fact-{call_id}-r1.parquet"),
                &values,
            );
        }

        let page = LlmAnalyticsReadService::new(workspace)
            .query_fact_sql(
                &scope,
                "SELECT llm_call_id FROM llm_calls ORDER BY llm_call_id",
                None,
                None,
                Some(1),
            )
            .expect("bounded SQL");

        assert_eq!(page.data.row_count, 1);
        assert!(page.pagination.is_none());
        assert_eq!(page.coverage.observed, Some(1));
        assert_eq!(page.coverage.censored, Some(1));
        assert!(page
            .warnings
            .iter()
            .any(|warning| warning.contains("truncated")));
    }
}

use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
    thread,
    time::Duration,
};

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Duration as ChronoDuration, NaiveDate, Utc};
use magicllm::LlmScope;
use serde::{Deserialize, Serialize};

use crate::scope::resolve_required_scope;
use crate::secret_vault_api::SecretVaultApi;
use magician::magician_v2::agents::{
    compute_memory_tier_health, inspect_scope_memory_index_fast, load_memory_candidate_documents,
    load_memory_hot_projection_index, load_memory_temperature_overlay,
    maintain_memory_hot_projections, maintain_memory_temperature_overlay,
    memory_temperature_candidate_key_renames, memory_temperature_entry_is_superseded,
    memory_temperature_utility_queue_health, migrate_memory_hot_projection_keys_for_scope,
    rebuild_scope_memory_index, resync_memory_temperature_overlay_full_scope, source_text_hash,
    AgentDefinitionStore, AgentMemoryResolver, MemoryCandidateRequest, MemoryConsolidator,
    MemoryContradictionSweepSummary, MemoryHotProjectionMaintenancePolicy,
    MemoryHotProjectionRecord, MemoryIndexManifest, MemoryLanceDbWriteReport,
    MemoryTemperatureCompactionSummary, MemoryTemperatureEntry,
    MemoryTemperatureMaintenanceSummary, MemoryTemperatureRetentionPolicy,
    MemoryTemperatureUtilityQueueHealth, MemoryTierHealthMetrics, TierScope,
};
use magician::magician_v2::analytics::duckdb_safety::{
    configure_analytics_connection_checked, duckdb_value_ref_output_bytes,
    try_analytics_duckdb_guard_for, ANALYTICS_DUCKDB_MAX_RESULT_BYTES,
    ANALYTICS_DUCKDB_MAX_RESULT_ROWS,
};
use magician::magician_v2::analytics::memory_eval_runner::{
    read_scope_regression_status, run_scope_once, MemoryRegressionStatusSnapshot,
};
use magician::magician_v2::analytics::memory_events_compactor::{
    compact_completed_partitions_for_query, compacted_partition_file,
    memory_events_raw_partition_files,
};
use magician::magician_v2::analytics::memory_index_maintainer::emit_memory_index_lancedb_write_rows;
use magician::magician_v2::analytics::pool::DuckDbPool;
use magician::magician_v2::analytics::{
    legacy_llm_compat::{
        install_legacy_llm_views, install_llm_embeddings_view, validate_legacy_llm_query,
    },
    llm_analytics_read_service::{
        LlmAnalyticsReadService, LlmFactFilter, LlmFactQuery, LLM_READ_GUARD_TIMEOUT_MESSAGE,
    },
    llm_fact_registry::LlmFactRelation,
    llm_restricted_content::{LlmRestrictedContentError, LlmRestrictedContentService},
    llm_scoped_path::ensure_real_scoped_directory_chain,
    llm_sql_guard::is_one_read_only_select_statement,
};
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;
use magician::magician_v2::realtime_events::RuntimeTransportBroadcaster;

/// Shorter than the memory-events guard wait on purpose: the spine's query
/// path is retrospective, so a busy writer is a better reason to come back
/// than to queue behind it.
const ACTIVITY_ROWS_QUERY_GUARD_TIMEOUT_SECS: u64 = 5;
const ACTIVITY_ROWS_BATCH_MAX_QUERIES: usize = 8;
const MEMORY_EVENTS_BATCH_MAX_QUERIES: usize = 4;
const ANALYTICS_QUERY_GUARD_TIMEOUT_SECS: u64 = 5;
const ANALYTICS_QUERY_TIMEOUT_SECS: u64 = 20;
const LLM_CALLS_BATCH_MAX_QUERIES: usize = 8;
const LLM_CALLS_QUERY_GUARD_TIMEOUT_SECS: u64 = 5;
const LLM_CALLS_QUERY_TIMEOUT_SECS: u64 = 20;
const LLM_CALLS_BATCH_QUERY_TIMEOUT_SECS: u64 = 8;
const ACTIVITY_ROWS_BATCH_QUERY_TIMEOUT_SECS: u64 = 8;
const MEMORY_EVENTS_QUERY_GUARD_TIMEOUT_SECS: u64 = 5;
const MEMORY_EVENTS_BATCH_QUERY_TIMEOUT_SECS: u64 = 6;
const MEMORY_EVENTS_MAX_PARQUET_FILES_DEFAULT: usize = 6_000;

/// Shared state for analytics API endpoints.
pub struct AnalyticsApi {
    workspace_layout: ArtifactV2Workspace,
    llm_analytics: Arc<LlmAnalyticsReadService>,
    definition_store: Option<AgentDefinitionStore>,
    memory_resolver: Option<AgentMemoryResolver>,
    operation_router: Option<Arc<OperationLlmRouter>>,
    event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    restricted_llm_content: Option<Arc<LlmRestrictedContentService>>,
}

impl AnalyticsApi {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            llm_analytics: Arc::new(LlmAnalyticsReadService::new(workspace_layout.clone())),
            workspace_layout,
            definition_store: None,
            memory_resolver: None,
            operation_router: None,
            event_broadcaster: None,
            restricted_llm_content: None,
        }
    }

    pub fn with_llm_analytics_read_service(
        mut self,
        llm_analytics: Arc<LlmAnalyticsReadService>,
    ) -> Self {
        self.llm_analytics = llm_analytics;
        self
    }

    pub fn with_memory_eval_runtime(
        mut self,
        definition_store: AgentDefinitionStore,
        memory_resolver: AgentMemoryResolver,
    ) -> Self {
        self.definition_store = Some(definition_store);
        self.memory_resolver = Some(memory_resolver);
        self
    }

    pub fn with_operation_router(
        mut self,
        operation_router: Option<Arc<OperationLlmRouter>>,
    ) -> Self {
        self.operation_router = operation_router;
        self
    }

    pub fn with_event_broadcaster(
        mut self,
        event_broadcaster: Arc<RuntimeTransportBroadcaster>,
    ) -> Self {
        self.event_broadcaster = Some(event_broadcaster);
        self
    }

    pub fn with_restricted_llm_content(
        mut self,
        service: Arc<LlmRestrictedContentService>,
    ) -> Self {
        self.restricted_llm_content = Some(service);
        self
    }

    fn resolve_scope(&self, req: &HttpRequest) -> Result<(String, String), HttpResponse> {
        let (principal, workspace) = resolve_required_scope(req.headers(), None)?;
        if !LlmScope::new(&principal, &workspace).is_valid() {
            return Err(HttpResponse::BadRequest().json(QueryErrorResponse {
                error: "principal and workspace must be safe scope components".to_string(),
            }));
        }
        Ok((principal, workspace))
    }

    fn open_pool_for_scope(
        &self,
        principal: &str,
        workspace: &str,
    ) -> anyhow::Result<std::sync::Arc<DuckDbPool>> {
        // Share the dispatcher's single read-write pool; reads use read_connection()
        // (read-only). Opening our own read-write `open_scoped` here raced the
        // dispatcher's handle and intermittently failed analytics queries.
        magician::magician_v2::analytics::scoped_pool(&self.workspace_layout, principal, workspace)
    }
}

/// GET /analytics/schema — returns the scoped schema catalog JSON from disk.
pub async fn get_schema_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let path = api
        .workspace_layout
        .analytics_schema_catalog_path(&principal, &workspace);
    match tokio::fs::read_to_string(&path).await {
        Ok(json) => HttpResponse::Ok()
            .content_type("application/json")
            .body(json),
        Err(_) => match tokio::task::spawn_blocking({
            let api = api.clone();
            let principal = principal.clone();
            let workspace = workspace.clone();
            move || -> anyhow::Result<String> {
                let pool = api.open_pool_for_scope(&principal, &workspace)?;
                let catalog =
                    magician::magician_v2::analytics::schema_catalog::generate_schema_catalog(
                        &pool,
                    )?;
                let root = api.workspace_layout.analytics_root(&principal, &workspace);
                magician::magician_v2::analytics::schema_catalog::write_catalog_to_disk(
                    &catalog, &root,
                )?;
                Ok(serde_json::to_string(&catalog)?)
            }
        })
        .await
        {
            Ok(Ok(json)) => HttpResponse::Ok()
                .content_type("application/json")
                .body(json),
            _ => HttpResponse::Ok()
                .content_type("application/json")
                .body(r#"{"generated_at":null,"event_types":[]}"#),
        },
    }
}

#[derive(Deserialize)]
pub struct QueryRequest {
    pub sql: String,
}

#[derive(Deserialize)]
pub struct QueryBatchRequest {
    pub queries: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct QueryResponse {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub row_count: usize,
}

#[derive(Debug, Serialize)]
pub struct ActivityRowsQueryResponse {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub row_count: usize,
    pub inventory_complete: bool,
}

#[derive(Serialize)]
pub struct QueryBatchItemResponse {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub row_count: usize,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct QueryBatchResponse {
    pub results: Vec<QueryBatchItemResponse>,
}

#[derive(Serialize)]
pub struct QueryErrorResponse {
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetryableQueryErrorResponse {
    pub code: &'static str,
    pub error: String,
    pub retryable: bool,
}

const LLM_CONTENT_GRANT_HEADER: &str = "X-Magician-Content-Grant";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueLlmContentGrantRequest {
    pub llm_call_id: String,
    pub reason: String,
    pub execution_id: Option<String>,
    pub ttl_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteLlmContentRequest {
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeleteLlmContentResponse {
    pub tombstone_id: String,
    pub llm_call_id: String,
    pub deleted: bool,
}

fn restricted_content_error(error: LlmRestrictedContentError) -> HttpResponse {
    use actix_web::http::StatusCode;
    let (status, machine_code) = match error {
        LlmRestrictedContentError::InvalidTarget | LlmRestrictedContentError::InvalidReason => (
            StatusCode::BAD_REQUEST,
            "restricted_content_invalid_request",
        ),
        LlmRestrictedContentError::GrantDenied => {
            (StatusCode::UNAUTHORIZED, "restricted_content_grant_denied")
        },
        LlmRestrictedContentError::Deleted => (StatusCode::GONE, "restricted_content_deleted"),
        LlmRestrictedContentError::NotFound => {
            (StatusCode::NOT_FOUND, "restricted_content_not_found")
        },
        LlmRestrictedContentError::ResponseTooLarge => (
            StatusCode::PAYLOAD_TOO_LARGE,
            "restricted_content_response_too_large",
        ),
        LlmRestrictedContentError::RedactionUnavailable(_)
        | LlmRestrictedContentError::AuditUnavailable
        | LlmRestrictedContentError::Storage(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "restricted_content_unavailable",
        ),
    };
    // Restricted failures can contain local storage diagnostics internally;
    // expose only a fixed machine code and forbid intermediary/browser caches.
    HttpResponse::build(status)
        .insert_header(("Cache-Control", "no-store, private"))
        .insert_header(("Pragma", "no-cache"))
        .json(QueryErrorResponse {
            error: machine_code.to_string(),
        })
}

/// POST /analytics/llm/content/grants — setup-token-authenticated issuance of
/// a one-use, short-lived reveal grant bound to the active scope and call.
pub async fn issue_llm_content_grant_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    secret_vault: Option<web::Data<SecretVaultApi>>,
    body: web::Json<IssueLlmContentGrantRequest>,
) -> HttpResponse {
    let (Some(api), Some(secret_vault)) = (api, secret_vault) else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Restricted LLM content service is not initialized".to_string(),
        });
    };
    if let Err(response) = secret_vault.require_setup_token(&req) {
        return response;
    }
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let Some(service) = api.restricted_llm_content.as_ref().cloned() else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Restricted LLM content service is not initialized".to_string(),
        });
    };
    let body = body.into_inner();
    let scope = LlmScope::new(principal, workspace);
    match service.issue_call_read_grant(
        scope,
        "setup_admin",
        &body.llm_call_id,
        &body.reason,
        body.execution_id,
        body.ttl_secs,
    ) {
        Ok(grant) => HttpResponse::Created()
            .insert_header(("Cache-Control", "no-store, private"))
            .insert_header(("Pragma", "no-cache"))
            .json(grant),
        Err(error) => restricted_content_error(error),
    }
}

/// GET /analytics/llm/content/calls/{id} — reveal sanitized content with a
/// one-use grant supplied only in a dedicated header, never in model args.
pub async fn read_llm_call_content_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    llm_call_id: web::Path<String>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Restricted LLM content service is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let token = req
        .headers()
        .get(LLM_CONTENT_GRANT_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(service) = api.restricted_llm_content.as_ref().cloned() else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Restricted LLM content service is not initialized".to_string(),
        });
    };
    let scope = LlmScope::new(principal, workspace);
    match service.read_call_content_with_token(
        &scope,
        token.unwrap_or_default(),
        &llm_call_id.into_inner(),
    ) {
        Ok(content) => HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store, private"))
            .insert_header(("Pragma", "no-cache"))
            .json(content),
        Err(error) => restricted_content_error(error),
    }
}

/// DELETE /analytics/llm/content/calls/{id} — setup-token-authenticated,
/// append-only tombstone. Reads are denied immediately and across restarts.
pub async fn delete_llm_call_content_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    secret_vault: Option<web::Data<SecretVaultApi>>,
    llm_call_id: web::Path<String>,
    body: web::Json<DeleteLlmContentRequest>,
) -> HttpResponse {
    let (Some(api), Some(secret_vault)) = (api, secret_vault) else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Restricted LLM content service is not initialized".to_string(),
        });
    };
    if let Err(response) = secret_vault.require_setup_token(&req) {
        return response;
    }
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let Some(service) = api.restricted_llm_content.as_ref().cloned() else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Restricted LLM content service is not initialized".to_string(),
        });
    };
    let llm_call_id = llm_call_id.into_inner();
    match service.tombstone_call_content(
        LlmScope::new(principal, workspace),
        &llm_call_id,
        &body.reason,
    ) {
        Ok(tombstone_id) => HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store, private"))
            .insert_header(("Pragma", "no-cache"))
            .json(DeleteLlmContentResponse {
                tombstone_id,
                llm_call_id,
                deleted: true,
            }),
        Err(error) => restricted_content_error(error),
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmReadRangeQuery {
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmTraceListQuery {
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmFactListQuery {
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub operation: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub profile: Option<String>,
    pub success: Option<bool>,
    pub order_by: Option<String>,
    pub descending: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmFactSqlRequest {
    pub sql: String,
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    pub limit: Option<usize>,
}

fn llm_fact_list_query(relation: LlmFactRelation, query: LlmFactListQuery) -> LlmFactQuery {
    let mut request = LlmFactQuery::for_relation(relation);
    request.from_ms = query.from_ms;
    request.to_ms = query.to_ms;
    request.limit = query.limit;
    request.offset = query.offset.unwrap_or_default();
    request.order_by = query.order_by.or(request.order_by);
    request.descending = query.descending.unwrap_or(true);
    for (column, value) in [
        ("operation", query.operation),
        ("provider", query.provider),
        ("model", query.model),
        ("effective_profile", query.profile),
    ] {
        if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
            request.filters.push(LlmFactFilter::TextEquals {
                column: column.to_string(),
                value,
            });
        }
    }
    if let Some(value) = query.success {
        request.filters.push(LlmFactFilter::BooleanEquals {
            column: "transport_success".to_string(),
            value,
        });
    }
    request
}

/// Map a governed-reader failure to an honest status.
///
/// These are server-side conditions, not malformed requests: a contended
/// DuckDB guard is transient and retryable, and a cross-dataset integrity
/// rejection means captured facts drifted. Reporting both as 400 told the
/// caller "you sent a bad request" and buried the real cause — `/llm` capture
/// health and call/tool lineage both surfaced a bare `HTTP 400` while the
/// actual message (1112 tool rows disagreeing with their owning call on
/// `execution_id`) sat unread in the response body.
fn llm_read_failure(error: impl std::fmt::Display) -> HttpResponse {
    let message = error.to_string();
    if message.contains(LLM_READ_GUARD_TIMEOUT_MESSAGE) {
        return HttpResponse::ServiceUnavailable()
            .insert_header((actix_web::http::header::RETRY_AFTER, "1"))
            .json(RetryableQueryErrorResponse {
                code: "analytics_busy",
                error: message,
                retryable: true,
            });
    }
    HttpResponse::InternalServerError().json(QueryErrorResponse { error: message })
}

/// GET /analytics/llm/overview — canonical call/attempt economics, timing,
/// validation and telemetry coverage from the shared governed reader.
pub async fn llm_observability_overview_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    query: web::Query<LlmReadRangeQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let scope = LlmScope::new(principal, workspace);
    let service = Arc::clone(&api.llm_analytics);
    let query = query.into_inner();
    match web::block(move || service.overview_envelope(&scope, query.from_ms, query.to_ms)).await {
        Ok(Ok(response)) => HttpResponse::Ok().json(response),
        Ok(Err(error)) => llm_read_failure(error),
        Err(error) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: format!("LLM analytics task failed: {error}"),
        }),
    }
}

/// GET /analytics/llm/catalog — scoped governed datasets and stable relations.
pub async fn llm_fact_catalog_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let scope = LlmScope::new(principal, workspace);
    let service = Arc::clone(&api.llm_analytics);
    match web::block(move || service.refresh_catalog(&scope)).await {
        Ok(Ok(response)) => HttpResponse::Ok().json(response),
        Ok(Err(error)) => llm_read_failure(error),
        Err(error) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: format!("LLM analytics task failed: {error}"),
        }),
    }
}

/// GET /analytics/llm/schema — the exact content-free fact registry. Scope is
/// still required so callers cannot probe this API outside an active tenant.
pub async fn llm_fact_schema_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    HttpResponse::Ok().json(serde_json::json!({
        "schema_version": api.llm_analytics.registry().schema_version,
        "scope": {"principal": principal, "workspace": workspace},
        "content_class": "fact_only",
        "relations": &api.llm_analytics.registry().facts,
    }))
}

pub async fn list_llm_calls_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    query: web::Query<LlmFactListQuery>,
) -> HttpResponse {
    list_llm_facts(req, api, query.into_inner(), LlmFactRelation::Calls).await
}

/// GET /analytics/llm/traces — bounded, fact-only workflow summaries assembled
/// by the same governed reader used by `internal_data`.
pub async fn list_llm_traces_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    query: web::Query<LlmTraceListQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let scope = LlmScope::new(principal, workspace);
    let service = Arc::clone(&api.llm_analytics);
    let query = query.into_inner();
    match web::block(move || {
        service.list_traces_envelope(&scope, query.from_ms, query.to_ms, query.limit)
    })
    .await
    {
        Ok(Ok(response)) => HttpResponse::Ok().json(response),
        Ok(Err(error)) => llm_read_failure(error),
        Err(error) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: format!("LLM analytics task failed: {error}"),
        }),
    }
}

/// GET /analytics/llm/traces/{trace_id} — one scoped, fact-only causal trace.
pub async fn read_llm_trace_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    trace_id: web::Path<String>,
    query: web::Query<LlmReadRangeQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let scope = LlmScope::new(principal, workspace);
    let service = Arc::clone(&api.llm_analytics);
    let trace_id = trace_id.into_inner();
    let query = query.into_inner();
    match web::block(move || {
        service.read_trace_envelope(&scope, &trace_id, query.from_ms, query.to_ms)
    })
    .await
    {
        Ok(Ok(response)) if response.data.is_some() => HttpResponse::Ok().json(response),
        Ok(Ok(_)) => HttpResponse::NotFound().json(QueryErrorResponse {
            error: "LLM trace was not found in the active scope and retention window".to_string(),
        }),
        Ok(Err(error)) => llm_read_failure(error),
        Err(error) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: format!("LLM analytics task failed: {error}"),
        }),
    }
}

pub async fn list_llm_provider_attempts_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    query: web::Query<LlmFactListQuery>,
) -> HttpResponse {
    list_llm_facts(
        req,
        api,
        query.into_inner(),
        LlmFactRelation::ProviderAttempts,
    )
    .await
}

async fn list_llm_facts(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    query: LlmFactListQuery,
    relation: LlmFactRelation,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let scope = LlmScope::new(principal, workspace);
    let service = Arc::clone(&api.llm_analytics);
    let query = llm_fact_list_query(relation, query);
    match web::block(move || service.query_facts_envelope(&scope, query)).await {
        Ok(Ok(response)) => HttpResponse::Ok().json(response),
        Ok(Err(error)) => llm_read_failure(error),
        Err(error) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: format!("LLM analytics task failed: {error}"),
        }),
    }
}

pub async fn read_llm_call_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    llm_call_id: web::Path<String>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let scope = LlmScope::new(principal, workspace);
    let service = Arc::clone(&api.llm_analytics);
    let llm_call_id = llm_call_id.into_inner();
    match web::block(move || service.read_call_detail_envelope(&scope, &llm_call_id)).await {
        Ok(Ok(response)) if response.data.is_some() => HttpResponse::Ok().json(response),
        Ok(Ok(_)) => HttpResponse::NotFound().json(QueryErrorResponse {
            error: "LLM call was not found in the active scope and retention window".to_string(),
        }),
        Ok(Err(error)) => llm_read_failure(error),
        Err(error) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: format!("LLM analytics task failed: {error}"),
        }),
    }
}

pub async fn read_llm_provider_attempt_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    provider_attempt_id: web::Path<String>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let scope = LlmScope::new(principal, workspace);
    let service = Arc::clone(&api.llm_analytics);
    let provider_attempt_id = provider_attempt_id.into_inner();
    match web::block(move || service.read_provider_attempt_envelope(&scope, &provider_attempt_id))
        .await
    {
        Ok(Ok(response)) if response.data.is_some() => HttpResponse::Ok().json(response),
        Ok(Ok(_)) => HttpResponse::NotFound().json(QueryErrorResponse {
            error: "Provider attempt was not found in the active scope and retention window"
                .to_string(),
        }),
        Ok(Err(error)) => llm_read_failure(error),
        Err(error) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: format!("LLM analytics task failed: {error}"),
        }),
    }
}

/// POST /analytics/llm/facts/query — parser-validated, fact-registry-only SQL.
pub async fn query_llm_facts_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    body: web::Json<LlmFactSqlRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let scope = LlmScope::new(principal, workspace);
    let service = Arc::clone(&api.llm_analytics);
    let body = body.into_inner();
    match web::block(move || {
        service.query_fact_sql(&scope, &body.sql, body.from_ms, body.to_ms, body.limit)
    })
    .await
    {
        Ok(Ok(response)) => HttpResponse::Ok().json(response),
        Ok(Err(error)) => llm_read_failure(error),
        Err(error) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: format!("LLM analytics task failed: {error}"),
        }),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AgentLlmHealthRollup {
    pub agent_id: String,
    pub calls_7d: u64,
    pub spend_usd_7d: f64,
    pub cost_observed_calls: u64,
    pub success_rate_7d: Option<f64>,
    pub last_call_at_ms: Option<i64>,
}

/// Fixed, server-owned LLM rollup used by the crew-health projection.
///
/// Keeping this query here reuses the schema-drift-compatible `llm_calls`
/// view and partition pruning used by the public analytics endpoint while
/// avoiding arbitrary SQL in crew clients.
pub(crate) fn query_agent_llm_health_rollups(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> anyhow::Result<Vec<AgentLlmHealthRollup>> {
    let scope = LlmScope::new(principal, workspace);
    if !scope.is_valid() {
        anyhow::bail!("principal and workspace must be safe scope components");
    }
    let llm_calls_root = workspace_layout.analytics_llm_calls_root(principal, workspace);
    ensure_real_scoped_directory_chain(workspace_layout.base_root(), &llm_calls_root)?;
    if !has_partitioned_parquet(&llm_calls_root) {
        return Ok(Vec::new());
    }

    let sql = "SELECT agent_id,\
                      COUNT(*) AS calls_7d,\
                      COALESCE(SUM(cost_usd), 0) AS spend_usd_7d,\
                      COUNT(cost_usd) AS cost_observed_calls,\
                      AVG(CASE WHEN success THEN 1.0 ELSE 0.0 END) AS success_rate_7d,\
                      MAX(timestamp_ms) AS last_call_at_ms \
               FROM llm_calls \
               WHERE timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS) \
                 AND agent_id IS NOT NULL \
                 AND TRIM(agent_id) <> '' \
               GROUP BY agent_id \
               ORDER BY agent_id";
    let Some(read_source_sql) = governed_llm_calls_read_source_sql(
        workspace_layout,
        &scope,
        &llm_calls_root,
        &[sql.to_string()],
        Some(7),
    )?
    else {
        return Ok(Vec::new());
    };
    let Some(_duckdb_guard) =
        try_analytics_duckdb_guard_for(Duration::from_secs(LLM_CALLS_QUERY_GUARD_TIMEOUT_SECS))
    else {
        anyhow::bail!(
            "analytics DuckDB is busy after waiting {}s",
            LLM_CALLS_QUERY_GUARD_TIMEOUT_SECS
        );
    };
    let conn = duckdb::Connection::open_in_memory()
        .map_err(|error| anyhow::anyhow!("open in-memory DuckDB: {error}"))?;
    configure_analytics_connection_checked(&conn, "crew_health_llm_rollup")
        .map_err(|error| anyhow::anyhow!("configure conservative DuckDB limits: {error}"))?;
    let attempts_root = workspace_layout.analytics_llm_provider_attempts_root(principal, workspace);
    let attempts_source_sql = has_partitioned_parquet(&attempts_root)
        .then(|| llm_calls_read_source_sql(&attempts_root, &[sql.to_string()], Some(7)))
        .flatten();
    install_llm_calls_views(
        &conn,
        &read_source_sql,
        attempts_source_sql.as_deref(),
        principal,
        workspace,
        // INTERVAL-based bound; no numeric timestamp_ms literal to push down.
        min_llm_timestamp_ms_bound(sql),
    )?;
    let response = run_analytics_query_with_interrupt_timeout(
        &conn,
        sql,
        10_000,
        Duration::from_secs(LLM_CALLS_BATCH_QUERY_TIMEOUT_SECS),
    )?;

    let index = |name: &str| {
        response
            .columns
            .iter()
            .position(|column| column == name)
            .ok_or_else(|| anyhow::anyhow!("crew health rollup missing `{name}` column"))
    };
    let agent_index = index("agent_id")?;
    let calls_index = index("calls_7d")?;
    let spend_index = index("spend_usd_7d")?;
    let cost_observed_index = index("cost_observed_calls")?;
    let success_index = index("success_rate_7d")?;
    let last_call_index = index("last_call_at_ms")?;

    response
        .rows
        .into_iter()
        .map(|row| {
            let agent_id = json_string(row.get(agent_index))
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("crew health rollup has an empty agent id"))?;
            let calls_7d = json_u64(row.get(calls_index))
                .ok_or_else(|| anyhow::anyhow!("crew health rollup has an invalid call count"))?;
            let spend_usd_7d = json_f64(row.get(spend_index))
                .filter(|value| value.is_finite() && *value >= 0.0)
                .ok_or_else(|| anyhow::anyhow!("crew health rollup has invalid spend"))?;
            let cost_observed_calls = json_u64(row.get(cost_observed_index))
                .filter(|value| *value <= calls_7d)
                .ok_or_else(|| anyhow::anyhow!("crew health rollup has invalid cost coverage"))?;
            let success_rate_7d = json_f64(row.get(success_index))
                .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
                .ok_or_else(|| anyhow::anyhow!("crew health rollup has invalid success rate"))?;
            let last_call_at_ms = json_i64(row.get(last_call_index))
                .filter(|value| *value > 0)
                .ok_or_else(|| anyhow::anyhow!("crew health rollup has invalid last-call time"))?;
            Ok(AgentLlmHealthRollup {
                agent_id,
                calls_7d,
                spend_usd_7d,
                cost_observed_calls,
                success_rate_7d: Some(success_rate_7d),
                last_call_at_ms: Some(last_call_at_ms),
            })
        })
        .collect()
}

fn json_string(value: Option<&serde_json::Value>) -> Option<String> {
    match value? {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn json_f64(value: Option<&serde_json::Value>) -> Option<f64> {
    match value? {
        serde_json::Value::Number(value) => value.as_f64(),
        serde_json::Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

fn json_u64(value: Option<&serde_json::Value>) -> Option<u64> {
    match value? {
        serde_json::Value::Number(value) => value.as_u64(),
        serde_json::Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

fn json_i64(value: Option<&serde_json::Value>) -> Option<i64> {
    match value? {
        serde_json::Value::Number(value) => value.as_i64(),
        serde_json::Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

struct AnalyticsViewColumn {
    name: &'static str,
    ty: &'static str,
    default_sql: String,
}

#[derive(Serialize)]
pub struct MemoryEvalRunResponse {
    pub principal: String,
    pub workspace: String,
    pub suite_count: usize,
    pub case_count: usize,
    pub passed_count: usize,
    pub failed_count: usize,
    pub regression_status: MemoryRegressionStatusSnapshot,
    pub ran_at_ms: i64,
}

#[derive(Serialize)]
pub struct MemoryRegressionStatusResponse {
    pub principal: String,
    pub workspace: String,
    pub status: MemoryRegressionStatusSnapshot,
}

#[derive(Serialize)]
pub struct MemoryIndexStatusResponse {
    pub principal: String,
    pub workspace: String,
    pub stale: bool,
    pub reason: String,
    pub current_document_count: usize,
    pub current_source_count: usize,
    pub manifest: Option<MemoryIndexManifest>,
}

#[derive(Serialize)]
pub struct MemoryIndexRebuildResponse {
    pub principal: String,
    pub workspace: String,
    pub document_count: usize,
    pub source_count: usize,
    pub backend: String,
    pub backend_status: String,
    pub rebuilt_at_ms: i64,
    pub manifest_path: String,
    pub documents_path: String,
    pub lancedb_write: MemoryLanceDbWriteReport,
    pub manifest: MemoryIndexManifest,
}

#[derive(Serialize)]
pub struct MemoryTemperatureCurrentStateResponse {
    pub principal: String,
    pub workspace: String,
    pub overlay_schema_version: u32,
    pub overlay_updated_at_ms: i64,
    pub overlay_entry_count: usize,
    pub current_entry_count: usize,
    pub superseded_entry_count: usize,
    pub projection_schema_version: u32,
    pub projection_updated_at_ms: i64,
    pub projection_count: usize,
    pub active_projection_count: usize,
    pub inactive_projection_count: usize,
    pub utility_queue: MemoryTemperatureUtilityQueueHealth,
    /// Tier *effectiveness*, as opposed to the inventory counts around it: is
    /// the active tier earned, is it bounded, does it predict use. Surfaced
    /// here so the dashboard and any operator can see it without running the
    /// eval binary.
    pub tier_health: MemoryTierHealthMetrics,
    pub lane_counts: Vec<MemoryTemperatureLaneCount>,
    pub tier_counts: Vec<MemoryTemperatureTierCount>,
    pub utility_label_counts: Vec<MemoryTemperatureUtilityLabelCount>,
    pub top_entries: Vec<MemoryTemperatureEntrySummary>,
    pub superseded_entries: Vec<MemoryTemperatureEntrySummary>,
    pub supersession_chains: Vec<MemorySupersessionChainSummary>,
    pub active_projections: Vec<MemoryHotProjectionSummary>,
    pub inactive_projections: Vec<MemoryHotProjectionSummary>,
}

#[derive(Serialize)]
pub struct MemoryTemperatureLaneCount {
    pub lane: String,
    pub count: usize,
}

#[derive(Serialize)]
pub struct MemoryTemperatureTierCount {
    pub lane: String,
    pub temperature_tier: String,
    pub count: usize,
}

#[derive(Serialize)]
pub struct MemoryTemperatureUtilityLabelCount {
    pub label: String,
    pub count: usize,
}

#[derive(Serialize)]
pub struct MemoryTemperatureEntrySummary {
    pub memory_candidate_key: String,
    pub lane: String,
    pub temperature_tier: String,
    pub temperature_score: f64,
    pub confidence: Option<f64>,
    pub retrieved_count: u32,
    pub selected_count: u32,
    pub injected_count: u32,
    pub successful_use_count: u32,
    pub failed_use_count: u32,
    pub last_used_at_ms: Option<i64>,
    pub last_utility_review_label: Option<String>,
    pub last_utility_review_confidence: Option<f64>,
    pub last_utility_review_run_id: Option<String>,
    pub last_utility_review_reason: Option<String>,
    pub superseded_by: Option<String>,
    pub superseded_at_ms: Option<i64>,
    pub supersession_reason: Option<String>,
    pub supersession_confidence: Option<f64>,
    pub supersession_source: Option<String>,
    pub supersedes: Vec<String>,
}

#[derive(Serialize)]
pub struct MemorySupersessionChainSummary {
    pub root_memory_candidate_key: String,
    pub chain: Vec<MemorySupersessionChainNode>,
}

#[derive(Serialize)]
pub struct MemorySupersessionChainNode {
    pub memory_candidate_key: String,
    pub lane: String,
    pub superseded_by: Option<String>,
    pub superseded_at_ms: Option<i64>,
    pub supersession_reason: Option<String>,
    pub supersession_confidence: Option<f64>,
    pub supersession_source: Option<String>,
}

#[derive(Deserialize)]
pub struct MemoryTemperatureMaintenanceRequest {
    #[serde(default = "default_true")]
    pub maintain_temperature: bool,
    #[serde(default)]
    pub maintain_hot_projections: bool,
    #[serde(default)]
    pub resync_overlay: bool,
    #[serde(default)]
    pub contradiction_sweep: bool,
}

#[derive(Serialize)]
pub struct MemoryTemperatureMaintenanceResponse {
    pub principal: String,
    pub workspace: String,
    pub maintained_temperature: bool,
    pub maintained_hot_projections: bool,
    pub resynced_overlay: bool,
    pub ran_contradiction_sweep: bool,
    pub temperature: Option<MemoryTemperatureMaintenanceSummary>,
    pub hot_projection_changed: Option<usize>,
    pub resync_candidate_count: Option<usize>,
    pub compaction: Option<MemoryTemperatureCompactionSummary>,
    pub contradiction_sweep: Option<MemoryContradictionSweepSummary>,
}

#[derive(Serialize)]
pub struct MemoryHotProjectionSummary {
    pub source_memory_candidate_key: String,
    pub lane: String,
    pub temperature_tier: String,
    pub active: bool,
    pub compact_text_preview: String,
    pub source_text_hash: String,
    pub source_ids: Vec<String>,
    pub source_tier_name: Option<String>,
    pub source_item_key: Option<String>,
    pub review_run_id: Option<String>,
    pub review_label: Option<String>,
    pub reviewer_confidence: Option<f64>,
    pub promotion_reason: Option<String>,
    pub projection_policy_version: u32,
    pub last_regenerated_at_ms: Option<i64>,
    pub regeneration_count: u32,
    pub deactivated_at_ms: Option<i64>,
    pub deactivation_reason: Option<String>,
    pub last_lifecycle_check_at_ms: Option<i64>,
    pub injected_count: u32,
    pub last_verified_at_ms: Option<i64>,
    pub last_injected_at_ms: Option<i64>,
    pub updated_at_ms: i64,
}

/// POST /analytics/query — execute read-only SQL with 30s timeout.
pub async fn query_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    body: web::Json<QueryRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let sql = normalize_analytics_sql(&body.sql);
    if !is_safe_select(&sql) {
        return HttpResponse::BadRequest().json(QueryErrorResponse {
            error: "Exactly one SELECT / WITH read-only query is required".to_string(),
        });
    }

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::task::spawn_blocking(move || -> anyhow::Result<QueryResponse> {
            let Some(_duckdb_guard) = try_analytics_duckdb_guard_for(Duration::from_secs(
                ANALYTICS_QUERY_GUARD_TIMEOUT_SECS,
            )) else {
                return Err(anyhow::anyhow!(
                    "analytics DuckDB is busy; timed out waiting for query guard after {}s",
                    ANALYTICS_QUERY_GUARD_TIMEOUT_SECS
                ));
            };
            let pool = api.open_pool_for_scope(&principal, &workspace)?;
            let conn = pool.read_connection()?;
            configure_analytics_connection_checked(&conn, "analytics_query").map_err(|error| {
                anyhow::anyhow!("configure conservative DuckDB limits: {error}")
            })?;
            disable_llm_query_external_access(&conn, "analytics_query")?;
            run_analytics_query_with_interrupt_timeout(
                &conn,
                &sql,
                ANALYTICS_DUCKDB_MAX_RESULT_ROWS,
                Duration::from_secs(ANALYTICS_QUERY_TIMEOUT_SECS),
            )
        }),
    )
    .await;

    match result {
        Ok(Ok(Ok(response))) => HttpResponse::Ok().json(response),
        Ok(Ok(Err(e))) => HttpResponse::BadRequest().json(QueryErrorResponse {
            error: e.to_string(),
        }),
        Ok(Err(e)) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: format!("task error: {}", e),
        }),
        Err(_) => HttpResponse::GatewayTimeout().json(QueryErrorResponse {
            error: "query timed out after 30s".to_string(),
        }),
    }
}

/// POST /analytics/llm_calls/query — execute read-only SQL against the
/// per-scope LLM-call Parquet partitions. Opens a fresh in-memory DuckDB,
/// installs an `llm_calls` view over `read_parquet(...)`, then runs the
/// caller's SQL. 30s timeout, 10k row cap, SELECT-only.
pub async fn query_llm_calls_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    body: web::Json<QueryRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };

    let sql = normalize_analytics_sql(&body.sql);
    if validate_legacy_llm_query(&sql).is_err() {
        return HttpResponse::BadRequest().json(QueryErrorResponse {
            error: "Only allowlisted legacy LLM views may be queried".to_string(),
        });
    }

    let workspace_layout = api.workspace_layout.clone();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::task::spawn_blocking(move || -> anyhow::Result<QueryResponse> {
            let llm_calls_root = workspace_layout.analytics_llm_calls_root(&principal, &workspace);
            ensure_real_scoped_directory_chain(workspace_layout.base_root(), &llm_calls_root)?;
            // No partitions yet → return empty result rather than failing on
            // `read_parquet('non-existent/**/*.parquet')`.
            if !has_partitioned_parquet(&llm_calls_root) {
                return Ok(QueryResponse {
                    columns: vec![],
                    rows: vec![],
                    row_count: 0,
                });
            }

            let Some(_duckdb_guard) = try_analytics_duckdb_guard_for(Duration::from_secs(
                LLM_CALLS_QUERY_GUARD_TIMEOUT_SECS,
            )) else {
                return Err(anyhow::anyhow!(
                    "analytics DuckDB is busy; timed out waiting for query guard after {}s",
                    LLM_CALLS_QUERY_GUARD_TIMEOUT_SECS
                ));
            };
            // Fresh in-memory DuckDB. No table state — view-only. The pool's
            // file-backed conn is for the chat/task/artifact analytics; this
            // endpoint is a pure read over Parquet partitions.
            let conn = duckdb::Connection::open_in_memory()
                .map_err(|e| anyhow::anyhow!("open in-memory DuckDB: {e}"))?;
            configure_analytics_connection_checked(&conn, "llm_calls_query")
                .map_err(|e| anyhow::anyhow!("configure conservative DuckDB limits: {e}"))?;
            let scope = LlmScope::new(principal.clone(), workspace.clone());
            let Some(read_source_sql) = governed_llm_calls_read_source_sql(
                &workspace_layout,
                &scope,
                &llm_calls_root,
                &[sql.clone()],
                None,
            )?
            else {
                return Ok(QueryResponse {
                    columns: vec![],
                    rows: vec![],
                    row_count: 0,
                });
            };
            let attempts_root =
                workspace_layout.analytics_llm_provider_attempts_root(&principal, &workspace);
            let attempts_source_sql = has_partitioned_parquet(&attempts_root)
                .then(|| llm_calls_read_source_sql(&attempts_root, &[sql.clone()], None))
                .flatten();
            install_llm_calls_views(
                &conn,
                &read_source_sql,
                attempts_source_sql.as_deref(),
                &principal,
                &workspace,
                min_llm_timestamp_ms_bound(&sql),
            )?;
            disable_llm_query_external_access(&conn, "llm_calls_query")?;

            run_analytics_query_with_interrupt_timeout(
                &conn,
                &sql,
                10_000,
                Duration::from_secs(LLM_CALLS_QUERY_TIMEOUT_SECS),
            )
        }),
    )
    .await;

    match result {
        Ok(Ok(Ok(response))) => HttpResponse::Ok().json(response),
        Ok(Ok(Err(e))) => HttpResponse::BadRequest().json(QueryErrorResponse {
            error: e.to_string(),
        }),
        Ok(Err(e)) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: format!("task error: {e}"),
        }),
        Err(_) => HttpResponse::GatewayTimeout().json(QueryErrorResponse {
            error: "query timed out after 30s".to_string(),
        }),
    }
}

/// POST /analytics/llm_embeddings/query — execute read-only SQL against the
/// per-scope embedding-batch Parquet partitions. Mirrors
/// [`query_llm_calls_handler`] but installs the flat, content-free
/// `llm_embeddings` view (no reconciliation — embeddings are already one row
/// per batch). Separate dataset so the hot `llm_calls` path is never bloated.
pub async fn query_llm_embeddings_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    body: web::Json<QueryRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };

    let sql = normalize_analytics_sql(&body.sql);
    if validate_legacy_llm_query(&sql).is_err() {
        return HttpResponse::BadRequest().json(QueryErrorResponse {
            error: "Only allowlisted legacy LLM views may be queried".to_string(),
        });
    }

    let workspace_layout = api.workspace_layout.clone();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::task::spawn_blocking(move || -> anyhow::Result<QueryResponse> {
            let embeddings_root =
                workspace_layout.analytics_llm_embeddings_root(&principal, &workspace);
            ensure_real_scoped_directory_chain(workspace_layout.base_root(), &embeddings_root)?;
            // No partitions yet → empty result rather than failing on a
            // non-existent read_parquet glob.
            if !has_partitioned_parquet(&embeddings_root) {
                return Ok(QueryResponse {
                    columns: vec![],
                    rows: vec![],
                    row_count: 0,
                });
            }

            let Some(_duckdb_guard) = try_analytics_duckdb_guard_for(Duration::from_secs(
                LLM_CALLS_QUERY_GUARD_TIMEOUT_SECS,
            )) else {
                return Err(anyhow::anyhow!(
                    "analytics DuckDB is busy; timed out waiting for query guard after {}s",
                    LLM_CALLS_QUERY_GUARD_TIMEOUT_SECS
                ));
            };
            let conn = duckdb::Connection::open_in_memory()
                .map_err(|e| anyhow::anyhow!("open in-memory DuckDB: {e}"))?;
            configure_analytics_connection_checked(&conn, "llm_embeddings_query")
                .map_err(|e| anyhow::anyhow!("configure conservative DuckDB limits: {e}"))?;
            let Some(read_source_sql) =
                llm_embeddings_read_source_sql(&embeddings_root, &[sql.clone()], None)?
            else {
                return Ok(QueryResponse {
                    columns: vec![],
                    rows: vec![],
                    row_count: 0,
                });
            };
            install_llm_embeddings_view(&conn, &read_source_sql)?;
            disable_llm_query_external_access(&conn, "llm_embeddings_query")?;

            run_analytics_query_with_interrupt_timeout(
                &conn,
                &sql,
                10_000,
                Duration::from_secs(LLM_CALLS_QUERY_TIMEOUT_SECS),
            )
        }),
    )
    .await;

    match result {
        Ok(Ok(Ok(response))) => HttpResponse::Ok().json(response),
        Ok(Ok(Err(e))) => HttpResponse::BadRequest().json(QueryErrorResponse {
            error: e.to_string(),
        }),
        Ok(Err(e)) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: format!("task error: {e}"),
        }),
        Err(_) => HttpResponse::GatewayTimeout().json(QueryErrorResponse {
            error: "query timed out after 30s".to_string(),
        }),
    }
}

/// POST /analytics/llm_calls/query_batch — execute multiple read-only SQL
/// statements against the scoped LLM-call Parquet partitions with one DuckDB
/// setup. Used by dashboard widgets to avoid a page-load query stampede.
pub async fn query_llm_calls_batch_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    body: web::Json<QueryBatchRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };

    let queries: Vec<String> = body
        .queries
        .iter()
        .take(LLM_CALLS_BATCH_MAX_QUERIES)
        .map(|sql| normalize_analytics_sql(sql))
        .collect();
    if queries
        .iter()
        .any(|sql| validate_legacy_llm_query(sql).is_err())
    {
        return HttpResponse::BadRequest().json(QueryErrorResponse {
            error: "Only allowlisted legacy LLM views may be queried".to_string(),
        });
    }

    let workspace_layout = api.workspace_layout.clone();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(70),
        tokio::task::spawn_blocking(move || -> anyhow::Result<QueryBatchResponse> {
            let llm_calls_root = workspace_layout.analytics_llm_calls_root(&principal, &workspace);
            ensure_real_scoped_directory_chain(workspace_layout.base_root(), &llm_calls_root)?;
            let scope = LlmScope::new(principal.clone(), workspace.clone());
            let Some(read_source_sql) = governed_llm_calls_read_source_sql(
                &workspace_layout,
                &scope,
                &llm_calls_root,
                &queries,
                Some(30),
            )? else {
                return Ok(QueryBatchResponse {
                    results: queries.iter().map(|_| empty_batch_item()).collect(),
                });
            };

            let Some(_duckdb_guard) =
                try_analytics_duckdb_guard_for(Duration::from_secs(
                    LLM_CALLS_QUERY_GUARD_TIMEOUT_SECS,
                ))
            else {
                return Ok(QueryBatchResponse {
                    results: queries
                        .iter()
                        .map(|_| QueryBatchItemResponse {
                            columns: vec![],
                            rows: vec![],
                            row_count: 0,
                            error: Some(format!(
                                "analytics DuckDB is busy; timed out waiting for query guard after {}s",
                                LLM_CALLS_QUERY_GUARD_TIMEOUT_SECS
                            )),
                        })
                        .collect(),
                });
            };

            let conn = duckdb::Connection::open_in_memory()
                .map_err(|e| anyhow::anyhow!("open in-memory DuckDB: {e}"))?;
            configure_analytics_connection_checked(&conn, "llm_calls_query_batch")
                .map_err(|e| anyhow::anyhow!("configure conservative DuckDB limits: {e}"))?;
            let attempts_root =
                workspace_layout.analytics_llm_provider_attempts_root(&principal, &workspace);
            let attempts_source_sql = has_partitioned_parquet(&attempts_root)
                .then(|| llm_calls_read_source_sql(&attempts_root, &queries, Some(30)))
                .flatten();
            install_llm_calls_views(
                &conn,
                &read_source_sql,
                attempts_source_sql.as_deref(),
                &principal,
                &workspace,
                shared_llm_timestamp_ms_bound(&queries),
            )?;
            disable_llm_query_external_access(&conn, "llm_calls_query_batch")?;

            let mut results = Vec::with_capacity(queries.len());
            let mut batch_bytes = serde_json::to_vec(&serde_json::json!({ "results": [] }))
                .map_err(|error| anyhow::anyhow!("serialize empty LLM query batch: {error}"))?
                .len();
            for sql in queries {
                let item = match run_analytics_query_with_interrupt_timeout(
                    &conn,
                    &sql,
                    10_000,
                    Duration::from_secs(LLM_CALLS_BATCH_QUERY_TIMEOUT_SECS),
                ) {
                    Ok(response) => QueryBatchItemResponse {
                        columns: response.columns,
                        rows: response.rows,
                        row_count: response.row_count,
                        error: None,
                    },
                    Err(error) => QueryBatchItemResponse {
                        columns: vec![],
                        rows: vec![],
                        row_count: 0,
                        error: Some(error.to_string()),
                    },
                };
                push_bounded_query_batch_item(&mut results, &mut batch_bytes, item)?;
            }
            Ok(QueryBatchResponse { results })
        }),
    )
    .await;

    match result {
        Ok(Ok(Ok(response))) => HttpResponse::Ok().json(response),
        Ok(Ok(Err(e))) => HttpResponse::BadRequest().json(QueryErrorResponse {
            error: e.to_string(),
        }),
        Ok(Err(e)) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: format!("task error: {e}"),
        }),
        Err(_) => HttpResponse::GatewayTimeout().json(QueryErrorResponse {
            error: "query batch timed out after 70s".to_string(),
        }),
    }
}

fn push_bounded_query_batch_item(
    results: &mut Vec<QueryBatchItemResponse>,
    batch_bytes: &mut usize,
    item: QueryBatchItemResponse,
) -> anyhow::Result<()> {
    let item_bytes = serde_json::to_vec(&item)
        .map_err(|error| anyhow::anyhow!("serialize LLM query batch item: {error}"))?
        .len()
        .saturating_add(1);
    if batch_bytes.saturating_add(item_bytes) > ANALYTICS_DUCKDB_MAX_RESULT_BYTES {
        return Err(anyhow::anyhow!(
            "LLM query batch exceeds the {} byte output limit",
            ANALYTICS_DUCKDB_MAX_RESULT_BYTES
        ));
    }
    *batch_bytes = batch_bytes.saturating_add(item_bytes);
    results.push(item);
    Ok(())
}

fn install_llm_calls_views(
    conn: &duckdb::Connection,
    read_source_sql: &str,
    attempts_source_sql: Option<&str>,
    principal: &str,
    workspace: &str,
    timestamp_lower_bound_ms: Option<i64>,
) -> anyhow::Result<()> {
    install_legacy_llm_views(
        conn,
        read_source_sql,
        attempts_source_sql,
        principal,
        workspace,
        timestamp_lower_bound_ms,
    )
}

fn duckdb_view_columns(
    conn: &duckdb::Connection,
    view_name: &str,
) -> anyhow::Result<HashSet<String>> {
    let mut stmt = conn
        .prepare(&format!("DESCRIBE {view_name}"))
        .map_err(|e| anyhow::anyhow!("describe {view_name}: {e}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| anyhow::anyhow!("read {view_name} columns: {e}"))?;
    let mut columns = HashSet::new();
    for row in rows {
        columns.insert(row.map_err(|e| anyhow::anyhow!("read {view_name} column: {e}"))?);
    }
    Ok(columns)
}

/// Project one declared column out of a raw view, quoting the identifier.
///
/// The quoting is not decoration. `activity_rollups` declares a column called
/// `count`, which is also a DuckDB function name; unquoted, `CAST(count AS
/// BIGINT)` is a parse error, and the endpoint would start returning 400 on
/// every query the day the first rollup exists — seven days after the spine
/// goes live, long after anyone would connect the two. The writer and this
/// file's own empty-table branch both quote it already; this was the one place
/// that did not.
fn analytics_projection_expr(columns: &HashSet<String>, column: &AnalyticsViewColumn) -> String {
    if columns.contains(column.name) {
        format!(
            "COALESCE(CAST(\"{name}\" AS {ty}), {default_sql}) AS \"{name}\"",
            name = column.name,
            ty = column.ty,
            default_sql = column.default_sql.as_str()
        )
    } else {
        format!("{} AS \"{}\"", column.default_sql, column.name)
    }
}

fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

fn install_memory_events_view(
    conn: &duckdb::Connection,
    read_source_sql: &str,
    principal: &str,
    workspace: &str,
    // Pulse fix: when the caller's query has a numeric `timestamp_ms >= N`
    // window, push it INTO the `memory_events_raw` materialization so only the
    // window's rows are ever materialized. `None` preserves the prior
    // full-materialization behaviour exactly.
    timestamp_lower_bound_ms: Option<i64>,
) -> anyhow::Result<()> {
    let raw_view_sql = format!(
        "CREATE OR REPLACE TEMP TABLE memory_events_raw AS
           SELECT * FROM read_parquet({read_source_sql}, hive_partitioning = true, union_by_name = true);"
    );
    conn.execute_batch(&raw_view_sql)
        .map_err(|e| anyhow::anyhow!("create raw memory_events view: {e}"))?;

    let columns = duckdb_view_columns(conn, "memory_events_raw")?;
    // Re-materialize narrowed to the requested window when the source carries a
    // `timestamp_ms` column and a numeric lower bound was supplied. Done after
    // DESCRIBE so we only reference the column when it is actually present.
    if let Some(bound) = timestamp_lower_bound_ms {
        if columns.contains("timestamp_ms") {
            let narrowed_sql = format!(
                "CREATE OR REPLACE TEMP TABLE memory_events_raw AS
                   SELECT * FROM read_parquet({read_source_sql}, hive_partitioning = true, union_by_name = true)
                   WHERE timestamp_ms >= {bound};"
            );
            conn.execute_batch(&narrowed_sql)
                .map_err(|e| anyhow::anyhow!("create windowed raw memory_events view: {e}"))?;
        }
    }
    let principal_default = format!("'{}'::VARCHAR", escape_sql_literal(principal));
    let workspace_default = format!("'{}'::VARCHAR", escape_sql_literal(workspace));
    let column_defs = vec![
        AnalyticsViewColumn {
            name: "timestamp_ms",
            ty: "BIGINT",
            default_sql: "0::BIGINT".to_string(),
        },
        AnalyticsViewColumn {
            name: "principal",
            ty: "VARCHAR",
            default_sql: principal_default,
        },
        AnalyticsViewColumn {
            name: "workspace",
            ty: "VARCHAR",
            default_sql: workspace_default,
        },
        AnalyticsViewColumn {
            name: "event_kind",
            ty: "VARCHAR",
            default_sql: "'unknown'::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "source",
            ty: "VARCHAR",
            default_sql: "'unknown'::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "agent_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "goal_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "scope",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "tier_name",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "item_key",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "selected",
            ty: "BOOLEAN",
            default_sql: "NULL::BOOLEAN".to_string(),
        },
        AnalyticsViewColumn {
            name: "score",
            ty: "INTEGER",
            default_sql: "NULL::INTEGER".to_string(),
        },
        AnalyticsViewColumn {
            name: "confidence",
            ty: "DOUBLE",
            default_sql: "NULL::DOUBLE".to_string(),
        },
        AnalyticsViewColumn {
            name: "query_excerpt",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "max_entries",
            ty: "INTEGER",
            default_sql: "NULL::INTEGER".to_string(),
        },
        AnalyticsViewColumn {
            name: "max_chars",
            ty: "INTEGER",
            default_sql: "NULL::INTEGER".to_string(),
        },
        AnalyticsViewColumn {
            name: "candidate_count",
            ty: "INTEGER",
            default_sql: "NULL::INTEGER".to_string(),
        },
        AnalyticsViewColumn {
            name: "selected_count",
            ty: "INTEGER",
            default_sql: "NULL::INTEGER".to_string(),
        },
        AnalyticsViewColumn {
            name: "dropped_count",
            ty: "INTEGER",
            default_sql: "NULL::INTEGER".to_string(),
        },
        AnalyticsViewColumn {
            name: "output_chars",
            ty: "INTEGER",
            default_sql: "NULL::INTEGER".to_string(),
        },
        AnalyticsViewColumn {
            name: "rule_name",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "target",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "source_kind",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "input_count",
            ty: "INTEGER",
            default_sql: "NULL::INTEGER".to_string(),
        },
        AnalyticsViewColumn {
            name: "output_count",
            ty: "INTEGER",
            default_sql: "NULL::INTEGER".to_string(),
        },
        AnalyticsViewColumn {
            name: "skipped_count",
            ty: "INTEGER",
            default_sql: "NULL::INTEGER".to_string(),
        },
        AnalyticsViewColumn {
            name: "eval_suite",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "eval_case_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "eval_query",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "eval_pass",
            ty: "BOOLEAN",
            default_sql: "NULL::BOOLEAN".to_string(),
        },
        AnalyticsViewColumn {
            name: "expected_count",
            ty: "INTEGER",
            default_sql: "NULL::INTEGER".to_string(),
        },
        AnalyticsViewColumn {
            name: "matched_count",
            ty: "INTEGER",
            default_sql: "NULL::INTEGER".to_string(),
        },
        AnalyticsViewColumn {
            name: "best_rank",
            ty: "INTEGER",
            default_sql: "NULL::INTEGER".to_string(),
        },
        AnalyticsViewColumn {
            name: "retrieval_backend",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "selected_item_keys",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "status",
            ty: "VARCHAR",
            default_sql: "'unknown'::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "payload_json",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "dt",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
    ];
    let projection = column_defs
        .iter()
        .map(|column| analytics_projection_expr(&columns, column))
        .collect::<Vec<_>>()
        .join(",\n                  ");

    let view_sql = format!(
        "CREATE OR REPLACE TEMP TABLE memory_events AS
           SELECT {projection}
           FROM memory_events_raw;
         DROP TABLE memory_events_raw;"
    );
    conn.execute_batch(&view_sql)
        .map_err(|e| anyhow::anyhow!("create memory_events view: {e}"))?;
    Ok(())
}

/// Extract the smallest literal `timestamp_ms >= N` / `timestamp_ms > N`
/// lower-bound present anywhere in `sql`, in raw epoch-milliseconds. `None` when
/// the query has no such numeric bound (e.g. it uses an `INTERVAL … DAYS`
/// expression, or is unbounded). Used both to derive a partition-file lookback
/// and — for Pulse — to push the day predicate INTO the `llm_calls`
/// materialization so only the requested window's rows are ever materialized.
fn min_llm_timestamp_ms_bound(sql: &str) -> Option<i64> {
    let mut min_ms: Option<i64> = None;
    let lowered = sql.to_ascii_lowercase();
    let mut rest = lowered.as_str();
    while let Some(idx) = rest.find("timestamp_ms") {
        rest = &rest[idx + "timestamp_ms".len()..];
        let trimmed = rest.trim_start();
        if trimmed.starts_with(">=") || trimmed.starts_with(">") {
            let offset = if trimmed.starts_with(">=") { 2 } else { 1 };
            let after_op = trimmed[offset..].trim_start();
            let digits: String = after_op
                .chars()
                .take_while(|ch| ch.is_ascii_digit())
                .collect();
            if let Ok(ms) = digits.parse::<i64>() {
                min_ms = Some(min_ms.map_or(ms, |current| current.min(ms)));
            }
        }
    }
    min_ms
}

/// The lowest numeric `timestamp_ms` lower-bound shared by ALL supplied queries,
/// or `None` if any query is unbounded (in which case the window predicate must
/// not be pushed into the materialization, or bounded rows would be dropped from
/// the unbounded query). This is the safe bound to inject into the `llm_calls`
/// CREATE for a Pulse batch.
fn shared_llm_timestamp_ms_bound(queries: &[String]) -> Option<i64> {
    let mut shared: Option<i64> = None;
    for query in queries {
        match min_llm_timestamp_ms_bound(query) {
            // Any unbounded query means we cannot narrow the materialization.
            None => return None,
            Some(ms) => {
                shared = Some(shared.map_or(ms, |current| current.min(ms)));
            },
        }
    }
    shared
}

fn llm_calls_lookback_days(sql: &str) -> Option<i64> {
    if let Some(days) = memory_events_lookback_days(sql) {
        return Some(days);
    }

    if let Some(ms) = min_llm_timestamp_ms_bound(sql) {
        let now = Utc::now().timestamp_millis();
        let diff_ms = now - ms;
        let diff_days = (diff_ms as f64 / (1000.0 * 60.0 * 60.0 * 24.0)).ceil() as i64;
        Some(diff_days.max(0))
    } else {
        None
    }
}

fn effective_llm_calls_lookback_days(
    queries: &[String],
    default_recent_days: Option<i64>,
) -> Option<i64> {
    if queries
        .iter()
        .any(|query| query.contains("magician:all_llm_partitions"))
    {
        return None;
    }

    let mut max_days: Option<i64> = None;
    let mut has_unbounded_query = false;
    for query in queries {
        match llm_calls_lookback_days(query) {
            Some(days) => {
                max_days = Some(max_days.map_or(days, |current| current.max(days)));
            },
            None => {
                has_unbounded_query = true;
            },
        }
    }
    if has_unbounded_query {
        match (max_days, default_recent_days) {
            (Some(days), Some(default_days)) => Some(days.max(default_days)),
            (None, Some(default_days)) => Some(default_days),
            (days, None) => days,
        }
    } else {
        max_days
    }
}

fn llm_calls_read_source_sql(
    root: &Path,
    queries: &[String],
    default_recent_days: Option<i64>,
) -> Option<String> {
    let lookback_days = effective_llm_calls_lookback_days(queries, default_recent_days);
    let files = partitioned_parquet_files(root, lookback_days);
    if files.is_empty() {
        return None;
    }
    Some(duckdb_parquet_source_sql(&files))
}

fn governed_llm_calls_read_source_sql(
    workspace_layout: &ArtifactV2Workspace,
    scope: &LlmScope,
    root: &Path,
    queries: &[String],
    default_recent_days: Option<i64>,
) -> anyhow::Result<Option<String>> {
    let lookback_days = effective_llm_calls_lookback_days(queries, default_recent_days);
    let files = governed_llm_call_partition_files(workspace_layout, scope, root, lookback_days)?;
    if files.is_empty() {
        return Ok(None);
    }
    Ok(Some(duckdb_parquet_source_sql(&files)))
}

/// Files DuckDB should scan for `llm_calls`, per partition, newest-window first.
///
/// BOTH halves must be compaction-aware. The legacy half always was
/// (`parquet_maintenance::partition_sources` returns the manifest output plus
/// only the newer raw files). The canonical half was not: it called
/// `canonical_raw_partition_files`, which returns EVERY immutable
/// `part-call_fact-<hash>-rN.parquet` revision. Those revisions are retained
/// deliberately as a corruption fallback and they accumulate per call, so a
/// single busy day handed DuckDB thousands of files — one observed partition
/// had 4,370 raw revisions next to the one compacted object that exists to
/// replace them. `SELECT COUNT(*) FROM llm_calls` took over 30s and the `/llm`
/// page timed out, while `compact-parquet` reported everything already
/// compacted, because it was: the compacted object was being read *in addition
/// to* the revisions rather than instead of them.
///
/// `governed_dataset_sources_in_date_range` is the governed selector for this.
/// It returns `[compacted] + uncompacted_tail` only after validating the
/// manifest scope, schema, source-set checksum, that every compacted source
/// still exists, the compacted file's byte length and blake3 checksum, and
/// `source_row_count == compacted_row_count` — and falls back to the full raw
/// set on any mismatch. So this is lossless by construction and fails closed.
fn governed_llm_call_partition_files(
    workspace_layout: &ArtifactV2Workspace,
    scope: &LlmScope,
    root: &Path,
    lookback_days: Option<i64>,
) -> anyhow::Result<Vec<PathBuf>> {
    let cutoff = lookback_days.map(|days| {
        let padded_days = days.max(0) + 1;
        Utc::now().date_naive() - ChronoDuration::days(padded_days)
    });
    let mut files = Vec::new();

    // Canonical facts: compacted object + uncompacted tail, per partition.
    files.extend(
        magician::magician_v2::analytics::llm_fact_compactor::governed_dataset_sources_in_date_range(
            workspace_layout,
            scope,
            magician::magician_v2::analytics::llm_fact_registry::LlmCanonicalDataset::Calls,
            cutoff,
            None,
        )?
        .into_iter()
        .flat_map(|source| source.files),
    );

    // Legacy batch stream: already compaction-aware, but lives beside the
    // canonical files in the same partition directories.
    let partitions = match std::fs::read_dir(root) {
        Ok(partitions) => partitions,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        },
        Err(error) => return Err(error.into()),
    };
    for partition in partitions {
        let partition = partition?;
        if !partition.file_type()?.is_dir() {
            continue;
        }
        let path = partition.path();
        let Some(date) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("dt="))
            .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
        else {
            continue;
        };
        if cutoff.is_some_and(|cutoff| date < cutoff) {
            continue;
        }
        files.extend(
            magician::magician_v2::analytics::parquet_maintenance::partition_sources(
                &path,
                magician::magician_v2::analytics::parquet_maintenance::PartitionedDataset::LegacyLlmCalls,
            )?,
        );
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn llm_embeddings_read_source_sql(
    root: &Path,
    queries: &[String],
    default_recent_days: Option<i64>,
) -> anyhow::Result<Option<String>> {
    let lookback_days = effective_llm_calls_lookback_days(queries, default_recent_days);
    let files = governed_embedding_partition_files(root, lookback_days)?;
    if files.is_empty() {
        return Ok(None);
    }
    Ok(Some(duckdb_parquet_source_sql(&files)))
}

fn governed_embedding_partition_files(
    root: &Path,
    lookback_days: Option<i64>,
) -> anyhow::Result<Vec<PathBuf>> {
    let cutoff = lookback_days.map(|days| {
        let padded_days = days.max(0) + 1;
        Utc::now().date_naive() - ChronoDuration::days(padded_days)
    });
    let partitions = match std::fs::read_dir(root) {
        Ok(partitions) => partitions,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut files = Vec::new();
    for partition in partitions {
        let partition = partition?;
        if !partition.file_type()?.is_dir() {
            continue;
        }
        let path = partition.path();
        let Some(date_text) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("dt="))
        else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(date_text, "%Y-%m-%d") else {
            continue;
        };
        if cutoff.is_some_and(|cutoff| date < cutoff) {
            continue;
        }
        files.extend(
            magician::magician_v2::analytics::parquet_maintenance::partition_sources(
                &path,
                magician::magician_v2::analytics::parquet_maintenance::PartitionedDataset::LlmEmbeddings,
            )?,
        );
    }
    files.sort();
    Ok(files)
}

fn memory_events_read_source_sql(
    root: &Path,
    queries: &[String],
    default_recent_days: Option<i64>,
) -> Option<String> {
    let lookback_days = effective_memory_events_lookback_days(queries, default_recent_days);
    let stats = compact_completed_partitions_for_query(root, lookback_days);
    if stats.partitions_compacted > 0 {
        tracing::info!(
            target: "analytics::memory_events_compactor",
            partitions = stats.partitions_compacted,
            raw_files = stats.raw_files_compacted,
            rows = stats.rows_compacted,
            "compacted memory_events partitions before query"
        );
    }
    let files = memory_events_partitioned_parquet_files(root, lookback_days);
    if files.is_empty() {
        return None;
    }
    Some(duckdb_parquet_source_sql(&files))
}

fn effective_memory_events_lookback_days(
    queries: &[String],
    default_recent_days: Option<i64>,
) -> Option<i64> {
    let mut max_days: Option<i64> = None;
    let mut has_unbounded_query = false;
    for query in queries {
        match memory_events_lookback_days(query) {
            Some(days) => {
                max_days = Some(max_days.map_or(days, |current| current.max(days)));
            },
            None => {
                has_unbounded_query = true;
            },
        }
    }
    if has_unbounded_query {
        match (max_days, default_recent_days) {
            (Some(days), Some(default_days)) => Some(days.max(default_days)),
            (None, Some(default_days)) => Some(default_days),
            (days, None) => days,
        }
    } else {
        max_days
    }
}

fn memory_events_lookback_days(sql: &str) -> Option<i64> {
    let lowered = sql.to_ascii_lowercase();
    let mut rest = lowered.as_str();
    let mut max_days: Option<i64> = None;
    while let Some(interval_idx) = rest.find("interval") {
        rest = &rest[interval_idx + "interval".len()..];
        let trimmed = rest.trim_start();
        let digits: String = trimmed
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect();
        if digits.is_empty() {
            continue;
        }
        let after_digits = trimmed[digits.len()..].trim_start();
        if !after_digits.starts_with("day") {
            continue;
        }
        if let Ok(days) = digits.parse::<i64>() {
            max_days = Some(max_days.map_or(days, |current| current.max(days)));
        }
    }
    max_days
}

fn partitioned_parquet_files(root: &Path, lookback_days: Option<i64>) -> Vec<PathBuf> {
    partitioned_parquet_files_with_max(root, lookback_days, memory_events_max_parquet_files())
}

fn partitioned_parquet_files_with_max(
    root: &Path,
    lookback_days: Option<i64>,
    max_files: usize,
) -> Vec<PathBuf> {
    let cutoff = lookback_days.map(|days| {
        let padded_days = days.max(0) + 1;
        Utc::now().date_naive() - ChronoDuration::days(padded_days)
    });
    let mut files = Vec::new();
    let Ok(partitions) = std::fs::read_dir(root) else {
        return files;
    };
    for partition in partitions.flatten() {
        let Ok(file_type) = partition.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let file_name = partition.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let Some(date_text) = name.strip_prefix("dt=") else {
            continue;
        };
        if let Some(cutoff) = cutoff {
            let Ok(partition_date) = NaiveDate::parse_from_str(date_text, "%Y-%m-%d") else {
                continue;
            };
            if partition_date < cutoff {
                continue;
            }
        }
        let Ok(partition_files) = std::fs::read_dir(partition.path()) else {
            continue;
        };
        for file in partition_files.flatten() {
            let Ok(file_type) = file.file_type() else {
                continue;
            };
            if file_type.is_file()
                && file
                    .path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext == "parquet")
            {
                files.push(file.path());
            }
        }
    }
    files.sort();
    if files.len() > max_files {
        files = files.split_off(files.len() - max_files);
    }
    files
}

fn memory_events_partitioned_parquet_files(
    root: &Path,
    lookback_days: Option<i64>,
) -> Vec<PathBuf> {
    memory_events_partitioned_parquet_files_with_max(
        root,
        lookback_days,
        memory_events_max_parquet_files(),
    )
}

fn memory_events_partitioned_parquet_files_with_max(
    root: &Path,
    lookback_days: Option<i64>,
    max_files: usize,
) -> Vec<PathBuf> {
    let cutoff = lookback_days.map(|days| {
        let padded_days = days.max(0) + 1;
        Utc::now().date_naive() - ChronoDuration::days(padded_days)
    });
    let today = Utc::now().date_naive();
    let mut compacted_files = Vec::new();
    let mut raw_files = Vec::new();
    let Ok(partitions) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    for partition in partitions.flatten() {
        let Ok(file_type) = partition.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let file_name = partition.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let Some(date_text) = name.strip_prefix("dt=") else {
            continue;
        };
        let Ok(partition_date) = NaiveDate::parse_from_str(date_text, "%Y-%m-%d") else {
            continue;
        };
        if let Some(cutoff) = cutoff {
            if partition_date < cutoff {
                continue;
            }
        }
        let partition_path = partition.path();
        let compacted_file = compacted_partition_file(&partition_path);
        let governed_sources = (partition_date < today)
            .then(|| magician::magician_v2::analytics::parquet_maintenance::partition_sources(
                &partition_path,
                magician::magician_v2::analytics::parquet_maintenance::PartitionedDataset::MemoryEvents,
            ))
            .transpose()
            .ok()
            .flatten();
        if let Some(sources) = governed_sources {
            for source in sources {
                if source == compacted_file {
                    compacted_files.push(source);
                } else {
                    raw_files.push(source);
                }
            }
        } else {
            raw_files.extend(memory_events_raw_partition_files(&partition_path));
        }
    }
    compacted_files.sort();
    raw_files.sort();

    let protected_count = compacted_files.len();
    let remaining = max_files.saturating_sub(protected_count);
    if raw_files.len() > remaining {
        raw_files = raw_files.split_off(raw_files.len() - remaining);
    }
    compacted_files.extend(raw_files);
    compacted_files.sort();
    compacted_files
}

fn memory_events_max_parquet_files() -> usize {
    std::env::var("MAGICIAN_MEMORY_EVENTS_QUERY_MAX_PARQUET_FILES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(MEMORY_EVENTS_MAX_PARQUET_FILES_DEFAULT)
}

fn format_memory_events_query_error(error: &anyhow::Error) -> String {
    let details = error
        .chain()
        .map(|cause| cause.to_string())
        .collect::<Vec<_>>()
        .join(": ");
    let message = if details.is_empty() {
        error.to_string()
    } else {
        details
    };
    if message.contains("Too many open files") || message.contains("os error 24") {
        return format!(
            "memory analytics query hit the OS open-file limit while scanning Parquet batches \
             (current live-query cap: {} newest files via MAGICIAN_MEMORY_EVENTS_QUERY_MAX_PARQUET_FILES). \
             Lower that cap or compact memory_events Parquet partitions. Raw error: {message}",
            memory_events_max_parquet_files()
        );
    }
    message
}

fn duckdb_parquet_source_sql(files: &[PathBuf]) -> String {
    if files.len() == 1 {
        return format!("'{}'", escape_sql_literal(&files[0].display().to_string()));
    }
    let items = files
        .iter()
        .map(|path| format!("'{}'", escape_sql_literal(&path.display().to_string())))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{items}]")
}

fn has_partitioned_parquet(root: &Path) -> bool {
    let Ok(partitions) = std::fs::read_dir(root) else {
        return false;
    };
    for partition in partitions.flatten() {
        let Ok(file_type) = partition.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = partition.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("dt=") {
            continue;
        }
        let Ok(files) = std::fs::read_dir(partition.path()) else {
            continue;
        };
        for file in files.flatten() {
            let Ok(file_type) = file.file_type() else {
                continue;
            };
            if file_type.is_file()
                && file
                    .path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext == "parquet")
            {
                return true;
            }
        }
    }
    false
}

/// POST /analytics/memory_events/query — execute read-only SQL against the
/// per-scope memory observability Parquet partitions. Creates a `memory_events`
/// view over `read_parquet(...)`.
pub async fn query_memory_events_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    body: web::Json<QueryRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };

    let sql = normalize_analytics_sql(&body.sql);
    if !is_safe_select(&sql) {
        return HttpResponse::BadRequest().json(QueryErrorResponse {
            error: "Only SELECT / WITH read-only queries are allowed".to_string(),
        });
    }

    let workspace_layout = api.workspace_layout.clone();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::task::spawn_blocking(move || -> anyhow::Result<QueryResponse> {
            let root = magician::magician_v2::analytics::memory_parquet::query_root_for_scope(
                &workspace_layout,
                &principal,
                &workspace,
            );

            let Some(read_source_sql) = memory_events_read_source_sql(&root, &[sql.clone()], None)
            else {
                return Ok(QueryResponse {
                    columns: vec![],
                    rows: vec![],
                    row_count: 0,
                });
            };

            let Some(_duckdb_guard) = try_analytics_duckdb_guard_for(Duration::from_secs(
                MEMORY_EVENTS_QUERY_GUARD_TIMEOUT_SECS,
            )) else {
                return Err(anyhow::anyhow!(
                    "analytics DuckDB is busy; timed out waiting for query guard after {}s",
                    MEMORY_EVENTS_QUERY_GUARD_TIMEOUT_SECS
                ));
            };
            let conn = duckdb::Connection::open_in_memory()
                .map_err(|e| anyhow::anyhow!("open in-memory DuckDB: {e}"))?;
            configure_analytics_connection_checked(&conn, "memory_events_query")
                .map_err(|e| anyhow::anyhow!("configure conservative DuckDB limits: {e}"))?;
            install_memory_events_view(
                &conn,
                &read_source_sql,
                &principal,
                &workspace,
                min_llm_timestamp_ms_bound(&sql),
            )?;
            disable_llm_query_external_access(&conn, "memory_events_query")?;

            run_analytics_query_with_interrupt_timeout(&conn, &sql, 10_000, Duration::from_secs(25))
        }),
    )
    .await;

    match result {
        Ok(Ok(Ok(response))) => HttpResponse::Ok().json(response),
        Ok(Ok(Err(e))) => HttpResponse::BadRequest().json(QueryErrorResponse {
            error: format_memory_events_query_error(&e),
        }),
        Ok(Err(e)) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: format!("task error: {e}"),
        }),
        Err(_) => HttpResponse::GatewayTimeout().json(QueryErrorResponse {
            error: "query timed out after 30s".to_string(),
        }),
    }
}

/// POST /analytics/memory_events/query_batch — execute multiple read-only SQL
/// statements against the scoped memory-events Parquet partitions with one
/// DuckDB setup. Used by dashboard widgets to avoid a page-load query stampede.
pub async fn query_memory_events_batch_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    body: web::Json<QueryBatchRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };

    let queries: Vec<String> = body
        .queries
        .iter()
        .take(MEMORY_EVENTS_BATCH_MAX_QUERIES)
        .map(|sql| normalize_analytics_sql(sql))
        .collect();
    if queries.iter().any(|sql| !is_safe_select(sql)) {
        return HttpResponse::BadRequest().json(QueryErrorResponse {
            error: "Only SELECT / WITH read-only queries are allowed".to_string(),
        });
    }

    let workspace_layout = api.workspace_layout.clone();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(70),
        tokio::task::spawn_blocking(move || -> anyhow::Result<QueryBatchResponse> {
            let root = magician::magician_v2::analytics::memory_parquet::query_root_for_scope(
                &workspace_layout,
                &principal,
                &workspace,
            );

            let Some(read_source_sql) = memory_events_read_source_sql(&root, &queries, Some(30))
            else {
                return Ok(QueryBatchResponse {
                    results: queries.iter().map(|_| empty_batch_item()).collect(),
                });
            };

            let Some(_duckdb_guard) =
                try_analytics_duckdb_guard_for(Duration::from_secs(
                    MEMORY_EVENTS_QUERY_GUARD_TIMEOUT_SECS,
                ))
            else {
                return Ok(QueryBatchResponse {
                    results: queries
                        .iter()
                        .map(|_| QueryBatchItemResponse {
                            columns: vec![],
                            rows: vec![],
                            row_count: 0,
                            error: Some(format!(
                                "analytics DuckDB is busy; timed out waiting for query guard after {}s",
                                MEMORY_EVENTS_QUERY_GUARD_TIMEOUT_SECS
                            )),
                        })
                        .collect(),
                });
            };
            let conn = duckdb::Connection::open_in_memory()
                .map_err(|e| anyhow::anyhow!("open in-memory DuckDB: {e}"))?;
            configure_analytics_connection_checked(&conn, "memory_events_query_batch")
                .map_err(|e| anyhow::anyhow!("configure conservative DuckDB limits: {e}"))?;
            install_memory_events_view(
                &conn,
                &read_source_sql,
                &principal,
                &workspace,
                shared_llm_timestamp_ms_bound(&queries),
            )?;
            disable_llm_query_external_access(&conn, "memory_events_query_batch")?;

            let mut results = Vec::with_capacity(queries.len());
            let mut batch_bytes = serde_json::to_vec(&serde_json::json!({ "results": [] }))
                .map_err(|error| anyhow::anyhow!("serialize empty memory query batch: {error}"))?
                .len();
            for sql in queries {
                let item = match run_analytics_query_with_interrupt_timeout(
                    &conn,
                    &sql,
                    10_000,
                    Duration::from_secs(MEMORY_EVENTS_BATCH_QUERY_TIMEOUT_SECS),
                ) {
                    Ok(response) => QueryBatchItemResponse {
                        columns: response.columns,
                        rows: response.rows,
                        row_count: response.row_count,
                        error: None,
                    },
                    Err(error) => QueryBatchItemResponse {
                        columns: vec![],
                        rows: vec![],
                        row_count: 0,
                        error: Some(format_memory_events_query_error(&error)),
                    },
                };
                push_bounded_query_batch_item(&mut results, &mut batch_bytes, item)?;
            }
            Ok(QueryBatchResponse { results })
        }),
    )
    .await;

    match result {
        Ok(Ok(Ok(response))) => HttpResponse::Ok().json(response),
        Ok(Ok(Err(e))) => HttpResponse::BadRequest().json(QueryErrorResponse {
            error: format_memory_events_query_error(&e),
        }),
        Ok(Err(e)) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: format!("task error: {e}"),
        }),
        Err(_) => HttpResponse::GatewayTimeout().json(QueryErrorResponse {
            error: "query batch timed out after 70s".to_string(),
        }),
    }
}

fn empty_batch_item() -> QueryBatchItemResponse {
    QueryBatchItemResponse {
        columns: vec![],
        rows: vec![],
        row_count: 0,
        error: None,
    }
}

/// Install `activity_rows` and `activity_rollups` as temp tables.
///
/// Both tiers, in one setup, because a retrospective question almost always
/// crosses the seven-day boundary between them and there is no way to answer
/// it if only one is reachable. They are kept as two tables rather than
/// UNION-ed into one: a detail row and a summary row are not the same kind of
/// fact, and silently mixing per-span durations with per-group percentiles
/// would produce numbers that look fine and mean nothing.
///
/// `hive_partitioning = false` on purpose. `dt` and `hour` are real columns in
/// the files as well as directory names, so enabling Hive partitioning would
/// produce two columns of each and fail. Storing them as columns is what lets
/// a compacted day-level object — which no longer has an `hour=` directory
/// above it — still say which hour each row belongs to.
fn install_activity_rows_views(
    conn: &duckdb::Connection,
    detail_source_sql: Option<&str>,
    rollup_source_sql: Option<&str>,
    principal: &str,
    workspace: &str,
) -> anyhow::Result<()> {
    let principal_default = format!("'{}'::VARCHAR", escape_sql_literal(principal));
    let workspace_default = format!("'{}'::VARCHAR", escape_sql_literal(workspace));

    let detail_columns = vec![
        AnalyticsViewColumn {
            name: "activity_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "parent_activity_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "root_activity_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "name",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "target",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "kind",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        // No default for the dimensions: undeclared must read as NULL so an
        // instrumentation gap shows up as a gap. A `'unknown'` here would make
        // every uninstrumented span look like a real, uniform category.
        AnalyticsViewColumn {
            name: "workload_class",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "priority",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "principal",
            ty: "VARCHAR",
            default_sql: principal_default.clone(),
        },
        AnalyticsViewColumn {
            name: "workspace",
            ty: "VARCHAR",
            default_sql: workspace_default.clone(),
        },
        AnalyticsViewColumn {
            name: "agent_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "thread_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "task_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "model",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "started_at_ms",
            ty: "BIGINT",
            default_sql: "0::BIGINT".to_string(),
        },
        AnalyticsViewColumn {
            name: "duration_ms",
            ty: "BIGINT",
            default_sql: "0::BIGINT".to_string(),
        },
        AnalyticsViewColumn {
            name: "outcome",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "dt",
            ty: "DATE",
            default_sql: "NULL::DATE".to_string(),
        },
        AnalyticsViewColumn {
            name: "hour",
            ty: "SMALLINT",
            default_sql: "NULL::SMALLINT".to_string(),
        },
    ];

    let rollup_columns = vec![
        AnalyticsViewColumn {
            name: "dt",
            ty: "DATE",
            default_sql: "NULL::DATE".to_string(),
        },
        AnalyticsViewColumn {
            name: "hour",
            ty: "SMALLINT",
            default_sql: "NULL::SMALLINT".to_string(),
        },
        AnalyticsViewColumn {
            name: "principal",
            ty: "VARCHAR",
            default_sql: principal_default,
        },
        AnalyticsViewColumn {
            name: "workspace",
            ty: "VARCHAR",
            default_sql: workspace_default,
        },
        AnalyticsViewColumn {
            name: "kind",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "workload_class",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "agent_id",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "outcome",
            ty: "VARCHAR",
            default_sql: "NULL::VARCHAR".to_string(),
        },
        AnalyticsViewColumn {
            name: "count",
            ty: "BIGINT",
            default_sql: "0::BIGINT".to_string(),
        },
        AnalyticsViewColumn {
            name: "p50_ms",
            ty: "BIGINT",
            default_sql: "NULL::BIGINT".to_string(),
        },
        AnalyticsViewColumn {
            name: "p95_ms",
            ty: "BIGINT",
            default_sql: "NULL::BIGINT".to_string(),
        },
        AnalyticsViewColumn {
            name: "sum_duration_ms",
            ty: "BIGINT",
            default_sql: "0::BIGINT".to_string(),
        },
    ];

    install_activity_tier(conn, "activity_rows", detail_source_sql, &detail_columns)?;
    install_activity_tier(conn, "activity_rollups", rollup_source_sql, &rollup_columns)?;
    Ok(())
}

/// Materialise one tier, or an empty table with the right shape when the tier
/// has no objects yet.
///
/// The empty case matters: a query for a window that predates the store must
/// return no rows, not "table activity_rollups does not exist". A missing
/// table turns "nothing happened then" into an error the caller has to
/// special-case.
fn install_activity_tier(
    conn: &duckdb::Connection,
    table: &str,
    source_sql: Option<&str>,
    column_defs: &[AnalyticsViewColumn],
) -> anyhow::Result<()> {
    let Some(source_sql) = source_sql else {
        let empty = column_defs
            .iter()
            .map(|column| format!("{} AS \"{}\"", column.default_sql, column.name))
            .collect::<Vec<_>>()
            .join(", ");
        conn.execute_batch(&format!(
            "CREATE OR REPLACE TEMP TABLE {table} AS SELECT {empty} WHERE false;"
        ))
        .map_err(|e| anyhow::anyhow!("create empty {table} view: {e}"))?;
        return Ok(());
    };

    let raw = format!("{table}_raw");
    conn.execute_batch(&format!(
        "CREATE OR REPLACE TEMP TABLE {raw} AS
           SELECT * FROM read_parquet({source_sql}, hive_partitioning = false, union_by_name = true);"
    ))
    .map_err(|e| anyhow::anyhow!("create raw {table} view: {e}"))?;

    let columns = duckdb_view_columns(conn, &raw)?;
    let projection = column_defs
        .iter()
        .map(|column| analytics_projection_expr(&columns, column))
        .collect::<Vec<_>>()
        .join(",\n                  ");
    conn.execute_batch(&format!(
        "CREATE OR REPLACE TEMP TABLE {table} AS
           SELECT {projection}
           FROM {raw};
         DROP TABLE {raw};"
    ))
    .map_err(|e| anyhow::anyhow!("create {table} view: {e}"))?;
    Ok(())
}

/// POST /analytics/activity_rows/query — read-only SQL over the activity
/// spine for the calling scope.
///
/// Two tables are in scope: `activity_rows` (full rows, seven days) and
/// `activity_rollups` (per-hour summaries, thirteen months). Joining a span to
/// its LLM economics is `activity_rows JOIN llm_dispatch ... USING
/// (activity_id)` against the dispatch dataset — the spine carries model, but no
/// cost, so there is exactly one number for each.
pub async fn query_activity_rows_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    body: web::Json<QueryRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };

    let sql = normalize_analytics_sql(&body.sql);
    if !is_safe_select(&sql) {
        return HttpResponse::BadRequest().json(QueryErrorResponse {
            error: "Only SELECT / WITH read-only queries are allowed".to_string(),
        });
    }

    let workspace_layout = api.workspace_layout.clone();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::task::spawn_blocking(move || -> anyhow::Result<ActivityRowsQueryResponse> {
            let detail_root =
                magician::magician_v2::analytics::activity_rows_sink::query_root_for_scope(
                    &workspace_layout,
                    &principal,
                    &workspace,
                );
            let rollup_root =
                workspace_layout.analytics_activity_rollups_root(&principal, &workspace);
            let detail_set =
                magician::magician_v2::analytics::activity_rows_sink::query_parquet_files(
                    &detail_root,
                );
            let rollup_set =
                magician::magician_v2::analytics::activity_rows_sink::rollup_parquet_files(
                    &rollup_root,
                );
            let detail_source = (!detail_set.files.is_empty())
                .then(|| duckdb_parquet_source_sql(&detail_set.files));
            let rollup_source = (!rollup_set.files.is_empty())
                .then(|| duckdb_parquet_source_sql(&rollup_set.files));
            let inventory_complete = detail_set.complete && rollup_set.complete;

            let Some(_duckdb_guard) = try_analytics_duckdb_guard_for(Duration::from_secs(
                ACTIVITY_ROWS_QUERY_GUARD_TIMEOUT_SECS,
            )) else {
                return Err(anyhow::anyhow!(
                    "analytics DuckDB is busy; timed out waiting for query guard after {}s",
                    ACTIVITY_ROWS_QUERY_GUARD_TIMEOUT_SECS
                ));
            };
            let conn = duckdb::Connection::open_in_memory()
                .map_err(|e| anyhow::anyhow!("open in-memory DuckDB: {e}"))?;
            configure_analytics_connection_checked(&conn, "activity_rows_query")
                .map_err(|e| anyhow::anyhow!("configure conservative DuckDB limits: {e}"))?;
            install_activity_rows_views(
                &conn,
                detail_source.as_deref(),
                rollup_source.as_deref(),
                &principal,
                &workspace,
            )?;
            disable_llm_query_external_access(&conn, "activity_rows_query")?;

            run_analytics_query_with_interrupt_timeout(&conn, &sql, 10_000, Duration::from_secs(25))
                .map(|query| ActivityRowsQueryResponse {
                    columns: query.columns,
                    rows: query.rows,
                    row_count: query.row_count,
                    inventory_complete,
                })
        }),
    )
    .await;

    match result {
        Ok(Ok(Ok(response))) => HttpResponse::Ok().json(response),
        Ok(Ok(Err(e))) => HttpResponse::BadRequest().json(QueryErrorResponse {
            error: format!("{e}"),
        }),
        Ok(Err(e)) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: format!("task error: {e}"),
        }),
        Err(_) => HttpResponse::GatewayTimeout().json(QueryErrorResponse {
            error: "query timed out after 30s".to_string(),
        }),
    }
}

/// POST /analytics/activity_rows/query_batch — execute multiple read-only SQL
/// statements against `activity_rows` + `activity_rollups` with one DuckDB
/// setup. Used for the runtime backfill hydrate path to reuse one metadata
/// bootstrap across one historical and one recent query.
pub async fn query_activity_rows_batch_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    body: web::Json<QueryBatchRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };

    let queries: Vec<String> = body
        .queries
        .iter()
        .take(ACTIVITY_ROWS_BATCH_MAX_QUERIES)
        .map(|sql| normalize_analytics_sql(sql))
        .collect();
    if queries.iter().any(|sql| !is_safe_select(sql)) {
        return HttpResponse::BadRequest().json(QueryErrorResponse {
            error: "Only SELECT / WITH read-only queries are allowed".to_string(),
        });
    }

    let workspace_layout = api.workspace_layout.clone();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(70),
        tokio::task::spawn_blocking(move || -> anyhow::Result<QueryBatchResponse> {
            let detail_root = magician::magician_v2::analytics::activity_rows_sink::query_root_for_scope(
                &workspace_layout,
                &principal,
                &workspace,
            );
            let rollup_root =
                workspace_layout.analytics_activity_rollups_root(&principal, &workspace);
            let detail_set =
                magician::magician_v2::analytics::activity_rows_sink::query_parquet_files(
                    &detail_root,
                );
            let rollup_set =
                magician::magician_v2::analytics::activity_rows_sink::rollup_parquet_files(
                    &rollup_root,
                );
            let detail_source =
                (!detail_set.files.is_empty()).then(|| duckdb_parquet_source_sql(&detail_set.files));
            let rollup_source =
                (!rollup_set.files.is_empty()).then(|| duckdb_parquet_source_sql(&rollup_set.files));
            if detail_source.is_none() && rollup_source.is_none() {
                return Ok(QueryBatchResponse {
                    results: queries.iter().map(|_| empty_batch_item()).collect(),
                });
            }

            let Some(_duckdb_guard) =
                try_analytics_duckdb_guard_for(Duration::from_secs(ACTIVITY_ROWS_QUERY_GUARD_TIMEOUT_SECS))
            else {
                return Ok(QueryBatchResponse {
                    results: queries
                        .iter()
                        .map(|_| QueryBatchItemResponse {
                            columns: vec![],
                            rows: vec![],
                            row_count: 0,
                            error: Some(format!(
                                "analytics DuckDB is busy; timed out waiting for query guard after {}s",
                                ACTIVITY_ROWS_QUERY_GUARD_TIMEOUT_SECS
                            )),
                        })
                        .collect(),
                });
            };

            let conn = duckdb::Connection::open_in_memory()
                .map_err(|error| anyhow::anyhow!("open in-memory DuckDB: {error}"))?;
            configure_analytics_connection_checked(&conn, "activity_rows_query_batch")
                .map_err(|error| {
                    anyhow::anyhow!("configure conservative DuckDB limits: {error}")
                })?;
            install_activity_rows_views(
                &conn,
                detail_source.as_deref(),
                rollup_source.as_deref(),
                &principal,
                &workspace,
            )?;
            disable_llm_query_external_access(&conn, "activity_rows_query_batch")?;

            let mut results = Vec::with_capacity(queries.len());
            let mut batch_bytes = serde_json::to_vec(&serde_json::json!({ "results": [] })).map_err(
                |error| anyhow::anyhow!("serialize empty activity rows query batch: {error}"),
            )?
            .len();
            for sql in queries {
                let item = match run_analytics_query_with_interrupt_timeout(
                    &conn,
                    &sql,
                    10_000,
                    Duration::from_secs(ACTIVITY_ROWS_BATCH_QUERY_TIMEOUT_SECS),
                ) {
                    Ok(response) => QueryBatchItemResponse {
                        columns: response.columns,
                        rows: response.rows,
                        row_count: response.row_count,
                        error: None,
                    },
                    Err(error) => QueryBatchItemResponse {
                        columns: vec![],
                        rows: vec![],
                        row_count: 0,
                        error: Some(error.to_string()),
                    },
                };
                push_bounded_query_batch_item(&mut results, &mut batch_bytes, item)?;
            }
            Ok(QueryBatchResponse { results })
        }),
    )
    .await;

    match result {
        Ok(Ok(Ok(response))) => HttpResponse::Ok().json(response),
        Ok(Ok(Err(e))) => HttpResponse::BadRequest().json(QueryErrorResponse {
            error: e.to_string(),
        }),
        Ok(Err(e)) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: format!("task error: {e}"),
        }),
        Err(_) => HttpResponse::GatewayTimeout().json(QueryErrorResponse {
            error: "activity rows query batch timed out after 70s".to_string(),
        }),
    }
}

/// POST /analytics/memory_events/evals/run — run memory retrieval evals for
/// the current scope immediately and emit `eval_case` rows into the scoped
/// memory_events Parquet lake.
pub async fn run_memory_evals_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let Some(definition_store) = api.definition_store.clone() else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Memory eval runtime is not initialized".to_string(),
        });
    };
    let Some(memory_resolver) = api.memory_resolver.clone() else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Memory eval runtime is not initialized".to_string(),
        });
    };

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        run_scope_once(
            &api.workspace_layout,
            &definition_store,
            &memory_resolver,
            &principal,
            &workspace,
        ),
    )
    .await;

    match result {
        Ok(Ok(outcome)) => HttpResponse::Ok().json(MemoryEvalRunResponse {
            regression_status: outcome.regression_snapshot(principal.as_str(), workspace.as_str()),
            principal,
            workspace,
            suite_count: outcome.suite_count,
            case_count: outcome.case_count,
            passed_count: outcome.passed_count,
            failed_count: outcome.failed_count,
            ran_at_ms: chrono::Utc::now().timestamp_millis(),
        }),
        Ok(Err(error)) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: error.to_string(),
        }),
        Err(_) => HttpResponse::GatewayTimeout().json(QueryErrorResponse {
            error: "memory eval run timed out after 120s".to_string(),
        }),
    }
}

/// GET /memory/regression/status — read the latest scoped memory regression
/// status snapshot written by the periodic/manual memory eval runner.
pub async fn memory_regression_status_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match read_scope_regression_status(&api.workspace_layout, &principal, &workspace).await {
        Ok(Some(status)) => HttpResponse::Ok().json(MemoryRegressionStatusResponse {
            principal,
            workspace,
            status,
        }),
        Ok(None) => {
            let status = MemoryRegressionStatusSnapshot::unknown(
                principal.as_str(),
                workspace.as_str(),
                "No memory regression status snapshot has been written yet.",
            );
            HttpResponse::Ok().json(MemoryRegressionStatusResponse {
                principal,
                workspace,
                status,
            })
        },
        Err(error) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: error.to_string(),
        }),
    }
}

/// GET /memory/temperature/status — read the scoped temperature overlay and
/// hot projection cache for current-state operations visibility.
pub async fn memory_temperature_status_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let Some(memory_resolver) = api.memory_resolver.clone() else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Memory temperature runtime is not initialized".to_string(),
        });
    };

    let memory_service = match memory_resolver.resolve_for_scope(&principal, &workspace) {
        Ok(service) => service,
        Err(error) => {
            return HttpResponse::InternalServerError().json(QueryErrorResponse {
                error: error.to_string(),
            });
        },
    };

    let result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let overlay = load_memory_temperature_overlay(memory_service.storage()).await?;
        let projections = load_memory_hot_projection_index(memory_service.storage()).await?;
        let utility_queue = memory_temperature_utility_queue_health(&memory_service).await?;
        Ok::<_, anyhow::Error>((overlay, projections, utility_queue))
    })
    .await;

    let (overlay, projections, utility_queue) = match result {
        Ok(Ok(state)) => state,
        Ok(Err(error)) => {
            return HttpResponse::InternalServerError().json(QueryErrorResponse {
                error: error.to_string(),
            });
        },
        Err(_) => {
            return HttpResponse::GatewayTimeout().json(QueryErrorResponse {
                error: "memory temperature status timed out after 30s".to_string(),
            });
        },
    };

    let mut lane_counts = BTreeMap::<String, usize>::new();
    let mut tier_counts = BTreeMap::<(String, String), usize>::new();
    let mut utility_label_counts = BTreeMap::<String, usize>::new();
    let mut superseded_entry_count = 0usize;
    for entry in overlay.entries.values() {
        let lane = entry.semantic_memory_type.as_str().to_string();
        let tier = entry.temperature_tier.as_str().to_string();
        *lane_counts.entry(lane.clone()).or_default() += 1;
        *tier_counts.entry((lane, tier)).or_default() += 1;
        if memory_temperature_entry_is_superseded(entry) {
            superseded_entry_count += 1;
        }
        if let Some(label) = entry.last_utility_review_label {
            *utility_label_counts
                .entry(label.as_str().to_string())
                .or_default() += 1;
        }
    }

    let mut entries = overlay.entries.values().collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        left.temperature_tier
            .cmp(&right.temperature_tier)
            .then_with(|| {
                right
                    .temperature_score
                    .partial_cmp(&left.temperature_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| right.updated_at.cmp(&left.updated_at))
    });
    let mut superseded_entries = overlay
        .entries
        .values()
        .filter(|entry| memory_temperature_entry_is_superseded(entry))
        .collect::<Vec<_>>();
    superseded_entries.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));

    let mut active_projections = projections
        .projections
        .values()
        .filter(|projection| projection.active)
        .collect::<Vec<_>>();
    active_projections.sort_by(|left, right| {
        left.temperature_tier
            .cmp(&right.temperature_tier)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
    });
    let mut inactive_projections = projections
        .projections
        .values()
        .filter(|projection| !projection.active)
        .collect::<Vec<_>>();
    inactive_projections.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));

    HttpResponse::Ok().json(MemoryTemperatureCurrentStateResponse {
        principal,
        workspace,
        overlay_schema_version: overlay.schema_version,
        overlay_updated_at_ms: overlay.updated_at.timestamp_millis(),
        overlay_entry_count: overlay.entries.len(),
        current_entry_count: overlay.entries.len().saturating_sub(superseded_entry_count),
        tier_health: compute_memory_tier_health(&overlay),
        superseded_entry_count,
        projection_schema_version: projections.schema_version,
        projection_updated_at_ms: projections.updated_at.timestamp_millis(),
        projection_count: projections.projections.len(),
        active_projection_count: projections
            .projections
            .values()
            .filter(|projection| projection.active)
            .count(),
        inactive_projection_count: projections
            .projections
            .values()
            .filter(|projection| !projection.active)
            .count(),
        utility_queue,
        lane_counts: lane_counts
            .into_iter()
            .map(|(lane, count)| MemoryTemperatureLaneCount { lane, count })
            .collect(),
        tier_counts: tier_counts
            .into_iter()
            .map(
                |((lane, temperature_tier), count)| MemoryTemperatureTierCount {
                    lane,
                    temperature_tier,
                    count,
                },
            )
            .collect(),
        utility_label_counts: utility_label_counts
            .into_iter()
            .map(|(label, count)| MemoryTemperatureUtilityLabelCount { label, count })
            .collect(),
        top_entries: entries
            .into_iter()
            .filter(|entry| !memory_temperature_entry_is_superseded(entry))
            .take(100)
            .map(memory_temperature_entry_summary)
            .collect(),
        superseded_entries: superseded_entries
            .into_iter()
            .take(100)
            .map(memory_temperature_entry_summary)
            .collect(),
        supersession_chains: memory_supersession_chains(&overlay.entries),
        active_projections: active_projections
            .into_iter()
            .take(100)
            .map(memory_hot_projection_summary)
            .collect(),
        inactive_projections: inactive_projections
            .into_iter()
            .take(100)
            .map(memory_hot_projection_summary)
            .collect(),
    })
}

/// POST /memory/temperature/maintain — manually run bounded temperature,
/// projection, resync, and contradiction-sweep maintenance for the current
/// scope.
pub async fn maintain_memory_temperature_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
    body: web::Json<MemoryTemperatureMaintenanceRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let Some(memory_resolver) = api.memory_resolver.clone() else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Memory temperature runtime is not initialized".to_string(),
        });
    };
    let memory_service = match memory_resolver.resolve_for_scope(&principal, &workspace) {
        Ok(service) => service,
        Err(error) => {
            return HttpResponse::InternalServerError().json(QueryErrorResponse {
                error: error.to_string(),
            });
        },
    };

    let request = body.into_inner();
    let result = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let mut response = MemoryTemperatureMaintenanceResponse {
            principal: principal.clone(),
            workspace: workspace.clone(),
            maintained_temperature: false,
            maintained_hot_projections: false,
            resynced_overlay: false,
            ran_contradiction_sweep: false,
            temperature: None,
            hot_projection_changed: None,
            resync_candidate_count: None,
            compaction: None,
            contradiction_sweep: None,
        };

        if request.maintain_temperature {
            let summary = maintain_memory_temperature_overlay(memory_service.storage()).await?;
            response.maintained_temperature = true;
            response.temperature = Some(summary);
        }

        if request.resync_overlay || request.maintain_hot_projections {
            let candidates = collect_scope_memory_candidates_for_api(
                &api,
                &memory_service,
                &principal,
                &workspace,
            )
            .await?;
            // Projections are keyed by candidate identity, so they share the
            // overlay's key space and must migrate with it. Run this for either
            // branch: migrating the overlay alone would orphan every projection
            // until a later maintenance pass deactivated it as unverified.
            // Idempotent, and a no-op once nothing is left to rename.
            migrate_scope_hot_projection_keys(&memory_service, &candidates).await?;
            if request.resync_overlay {
                // The only caller holding the complete candidate set for the
                // scope, so the only one allowed to migrate keys and evict.
                let (_, compaction) = resync_memory_temperature_overlay_full_scope(
                    memory_service.storage(),
                    &candidates,
                    MemoryTemperatureRetentionPolicy::default(),
                )
                .await?;
                response.resynced_overlay = true;
                response.resync_candidate_count = Some(candidates.len());
                response.compaction = Some(compaction);
            }
            if request.maintain_hot_projections {
                let source_hashes = candidates
                    .iter()
                    .map(|candidate| {
                        (
                            magician::magician_v2::agents::memory_temperature_candidate_key(
                                candidate,
                            ),
                            source_text_hash(&candidate.text),
                        )
                    })
                    .collect::<BTreeMap<_, _>>();
                let (_, summary) = maintain_memory_hot_projections(
                    memory_service.storage(),
                    &source_hashes,
                    MemoryHotProjectionMaintenancePolicy::default(),
                )
                .await?;
                response.maintained_hot_projections = true;
                response.hot_projection_changed = Some(summary.deactivated);
            }
        }

        if request.contradiction_sweep {
            let Some(definition_store) = api.definition_store.clone() else {
                return Err(anyhow::anyhow!("Agent definition store is not initialized"));
            };
            let scoped_store = definition_store.for_scope(&principal, &workspace);
            let definitions = scoped_store.list_definitions().await?;
            let mut sweep = MemoryContradictionSweepSummary::default();
            for record in definitions {
                let mut consolidator = MemoryConsolidator::new(
                    memory_service.clone(),
                    api.operation_router.clone(),
                    None,
                );
                if let Some(broadcaster) = api.event_broadcaster.as_ref() {
                    consolidator =
                        consolidator.with_llm_telemetry_broadcaster(Arc::clone(broadcaster));
                }
                sweep.merge(
                    consolidator
                        .run_contradiction_sweep_for_agent(
                            &record.definition,
                            &record.definition.agent_id,
                            chrono::Utc::now(),
                        )
                        .await?,
                );
            }
            response.ran_contradiction_sweep = true;
            response.contradiction_sweep = Some(sweep);
            let candidates = collect_scope_memory_candidates_for_api(
                &api,
                &memory_service,
                &principal,
                &workspace,
            )
            .await?;
            let (_, compaction) = resync_memory_temperature_overlay_full_scope(
                memory_service.storage(),
                &candidates,
                MemoryTemperatureRetentionPolicy::default(),
            )
            .await?;
            let summary = maintain_memory_temperature_overlay(memory_service.storage()).await?;
            response.resynced_overlay = true;
            response.resync_candidate_count = Some(candidates.len());
            response.compaction = Some(compaction);
            response.maintained_temperature = true;
            response.temperature = Some(summary);
        }

        Ok::<_, anyhow::Error>(response)
    })
    .await;

    match result {
        Ok(Ok(response)) => HttpResponse::Ok().json(response),
        Ok(Err(error)) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: error.to_string(),
        }),
        Err(_) => HttpResponse::GatewayTimeout().json(QueryErrorResponse {
            error: "memory temperature maintenance timed out after 60s".to_string(),
        }),
    }
}

fn memory_temperature_entry_summary(
    entry: &MemoryTemperatureEntry,
) -> MemoryTemperatureEntrySummary {
    MemoryTemperatureEntrySummary {
        memory_candidate_key: entry.memory_candidate_key.clone(),
        lane: entry.semantic_memory_type.as_str().to_string(),
        temperature_tier: entry.temperature_tier.as_str().to_string(),
        temperature_score: entry.temperature_score,
        confidence: entry.confidence,
        retrieved_count: entry.retrieved_count,
        selected_count: entry.selected_count,
        injected_count: entry.injected_count,
        successful_use_count: entry.successful_use_count,
        failed_use_count: entry.failed_use_count,
        last_used_at_ms: entry.last_used_at.map(|value| value.timestamp_millis()),
        last_utility_review_label: entry
            .last_utility_review_label
            .map(|label| label.as_str().to_string()),
        last_utility_review_confidence: entry.last_utility_review_confidence,
        last_utility_review_run_id: entry.last_utility_review_run_id.clone(),
        last_utility_review_reason: entry.last_utility_review_reason.clone(),
        superseded_by: entry.superseded_by.clone(),
        superseded_at_ms: entry.superseded_at.map(|value| value.timestamp_millis()),
        supersession_reason: entry.supersession_reason.clone(),
        supersession_confidence: entry.supersession_confidence,
        supersession_source: entry.supersession_source.clone(),
        supersedes: entry.supersedes.clone(),
    }
}

fn memory_supersession_chains(
    entries: &BTreeMap<String, MemoryTemperatureEntry>,
) -> Vec<MemorySupersessionChainSummary> {
    let mut chains = entries
        .values()
        .filter(|entry| memory_temperature_entry_is_superseded(entry))
        .filter(|entry| {
            !entries.values().any(|candidate| {
                candidate
                    .superseded_by
                    .as_deref()
                    .is_some_and(|successor| successor == entry.memory_candidate_key)
            })
        })
        .map(|entry| {
            let mut chain = Vec::new();
            let mut current_key = Some(entry.memory_candidate_key.as_str());
            let mut seen = std::collections::BTreeSet::<String>::new();
            while let Some(key) = current_key {
                if !seen.insert(key.to_string()) || chain.len() >= 8 {
                    break;
                }
                let Some(current) = entries.get(key) else {
                    chain.push(MemorySupersessionChainNode {
                        memory_candidate_key: key.to_string(),
                        lane: "missing".to_string(),
                        superseded_by: None,
                        superseded_at_ms: None,
                        supersession_reason: Some("successor_missing_from_overlay".to_string()),
                        supersession_confidence: None,
                        supersession_source: None,
                    });
                    break;
                };
                chain.push(memory_supersession_chain_node(current));
                current_key = current.superseded_by.as_deref();
            }
            MemorySupersessionChainSummary {
                root_memory_candidate_key: entry.memory_candidate_key.clone(),
                chain,
            }
        })
        .collect::<Vec<_>>();
    chains.sort_by(|left, right| {
        left.root_memory_candidate_key
            .cmp(&right.root_memory_candidate_key)
    });
    chains.truncate(100);
    chains
}

fn memory_supersession_chain_node(entry: &MemoryTemperatureEntry) -> MemorySupersessionChainNode {
    MemorySupersessionChainNode {
        memory_candidate_key: entry.memory_candidate_key.clone(),
        lane: entry.semantic_memory_type.as_str().to_string(),
        superseded_by: entry.superseded_by.clone(),
        superseded_at_ms: entry.superseded_at.map(|value| value.timestamp_millis()),
        supersession_reason: entry.supersession_reason.clone(),
        supersession_confidence: entry.supersession_confidence,
        supersession_source: entry.supersession_source.clone(),
    }
}

fn memory_hot_projection_summary(
    projection: &MemoryHotProjectionRecord,
) -> MemoryHotProjectionSummary {
    MemoryHotProjectionSummary {
        source_memory_candidate_key: projection.source_memory_candidate_key.clone(),
        lane: projection.semantic_memory_type.as_str().to_string(),
        temperature_tier: projection.temperature_tier.as_str().to_string(),
        active: projection.active,
        compact_text_preview: truncate_status_preview(&projection.compact_text, 280),
        source_text_hash: projection.source_text_hash.clone(),
        source_ids: projection.source_ids.clone(),
        source_tier_name: projection.source_tier_name.clone(),
        source_item_key: projection.source_item_key.clone(),
        review_run_id: projection.review_run_id.clone(),
        review_label: projection
            .review_label
            .map(|label| label.as_str().to_string()),
        reviewer_confidence: projection.reviewer_confidence,
        promotion_reason: projection.promotion_reason.clone(),
        projection_policy_version: projection.projection_policy_version,
        last_regenerated_at_ms: projection
            .last_regenerated_at
            .map(|value| value.timestamp_millis()),
        regeneration_count: projection.regeneration_count,
        deactivated_at_ms: projection
            .deactivated_at
            .map(|value| value.timestamp_millis()),
        deactivation_reason: projection.deactivation_reason.clone(),
        last_lifecycle_check_at_ms: projection
            .last_lifecycle_check_at
            .map(|value| value.timestamp_millis()),
        injected_count: projection.injected_count,
        last_verified_at_ms: projection
            .last_verified_at
            .map(|value| value.timestamp_millis()),
        last_injected_at_ms: projection
            .last_injected_at
            .map(|value| value.timestamp_millis()),
        updated_at_ms: projection.updated_at.timestamp_millis(),
    }
}

/// Move hot-projection keys onto the current candidate-key encoding.
///
/// Projections are keyed by `source_memory_candidate_key`, so they live in the
/// same key space as the temperature overlay. Migrating one without the other
/// would leave every projection unable to find its source, and the next
/// maintenance pass would deactivate it as orphaned.
async fn migrate_scope_hot_projection_keys(
    memory_service: &magician::magician_v2::agents::AgentMemoryService,
    candidates: &[magician::magician_v2::agents::MemoryCandidateDocument],
) -> anyhow::Result<usize> {
    let renames = memory_temperature_candidate_key_renames(candidates);
    Ok(migrate_memory_hot_projection_keys_for_scope(memory_service.storage(), &renames).await?)
}

async fn collect_scope_memory_candidates_for_api(
    api: &AnalyticsApi,
    memory_service: &magician::magician_v2::agents::AgentMemoryService,
    principal: &str,
    workspace: &str,
) -> anyhow::Result<Vec<magician::magician_v2::agents::MemoryCandidateDocument>> {
    let Some(definition_store) = api.definition_store.clone() else {
        return Ok(Vec::new());
    };
    let definitions = definition_store
        .for_scope(principal, workspace)
        .list_definitions()
        .await?;
    let mut candidates = Vec::new();
    for record in definitions {
        for scope in [TierScope::User, TierScope::Agent] {
            let mut scoped_candidates = load_memory_candidate_documents(
                memory_service.storage(),
                &record.definition.agent_id,
                &record.definition.memory_tiers,
                &MemoryCandidateRequest {
                    scope,
                    goal_id: None,
                    recency_cutoff: None,
                    include_environment_knowledge: true,
                    // Unbound (§5A.2): an operator diagnostic over the whole
                    // scope, driven by the signed-in owner over HTTP rather
                    // than by an execution carrying an engagement authority.
                    // It is deliberately the complete picture — the owner
                    // inspecting their own memory is the one reader an
                    // engagement boundary is not drawn against.
                    retrieval_scope: magician::magician_v2::agents::RetrievalScope::Unbound,
                },
            )
            .await?;
            candidates.append(&mut scoped_candidates);
        }

        // Goal-scoped tiers live in per-goal files and are invisible to a
        // `goal_id: None` load, so they must be enumerated. Omitting them was
        // harmless while this set only fed the non-evicting sync — absence just
        // meant "not in this batch". It stopped being harmless the moment the
        // set became eviction ground truth: every goal-scoped entry would look
        // like memory whose candidate had been deleted.
        for tier in record
            .definition
            .memory_tiers
            .iter()
            .filter(|tier| matches!(&tier.scope, &TierScope::AgentGoal))
        {
            for goal_id in magician::magician_v2::agents::discover_goal_ids_for_tier(
                memory_service.storage(),
                &record.definition.agent_id,
                tier,
            )
            .await?
            {
                let mut goal_candidates = load_memory_candidate_documents(
                    memory_service.storage(),
                    &record.definition.agent_id,
                    std::slice::from_ref(tier),
                    &MemoryCandidateRequest {
                        scope: TierScope::AgentGoal,
                        goal_id: Some(&goal_id),
                        recency_cutoff: None,
                        include_environment_knowledge: true,
                        retrieval_scope: magician::magician_v2::agents::RetrievalScope::Unbound,
                    },
                )
                .await?;
                candidates.append(&mut goal_candidates);
            }
        }
    }
    Ok(candidates)
}

fn default_true() -> bool {
    true
}

fn truncate_status_preview(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut out = trimmed.chars().take(max_chars).collect::<String>();
    out.push_str("...");
    out
}

/// GET /memory/index/status — return lightweight scoped derived memory index
/// health for UI diagnostics. Exact source freshness checks run in the
/// maintainer/rebuild paths; this endpoint must stay page-load safe.
pub async fn memory_index_status_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let Some(memory_resolver) = api.memory_resolver.clone() else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Memory index runtime is not initialized".to_string(),
        });
    };

    let memory_service = match memory_resolver.resolve_for_scope(&principal, &workspace) {
        Ok(service) => service,
        Err(error) => {
            return HttpResponse::InternalServerError().json(QueryErrorResponse {
                error: error.to_string(),
            });
        },
    };
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        inspect_scope_memory_index_fast(memory_service.storage()),
    )
    .await;

    match result {
        Ok(Ok(status)) => HttpResponse::Ok().json(MemoryIndexStatusResponse {
            principal,
            workspace,
            stale: status.stale,
            reason: status.reason,
            current_document_count: status.current_document_count,
            current_source_count: status.current_source_count,
            manifest: status.manifest,
        }),
        Ok(Err(error)) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: error.to_string(),
        }),
        Err(_) => HttpResponse::GatewayTimeout().json(QueryErrorResponse {
            error: "memory index status timed out after 5s".to_string(),
        }),
    }
}

/// POST /memory/index/rebuild — rebuild the scoped derived memory index from
/// canonical memory tier JSON and current agent definitions.
pub async fn rebuild_memory_index_handler(
    req: HttpRequest,
    api: Option<web::Data<AnalyticsApi>>,
) -> HttpResponse {
    let Some(api) = api else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Analytics layer is not initialized".to_string(),
        });
    };
    let (principal, workspace) = match api.resolve_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let Some(definition_store) = api.definition_store.clone() else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Memory index runtime is not initialized".to_string(),
        });
    };
    let Some(memory_resolver) = api.memory_resolver.clone() else {
        return HttpResponse::ServiceUnavailable().json(QueryErrorResponse {
            error: "Memory index runtime is not initialized".to_string(),
        });
    };

    let scoped_store = definition_store.for_scope(&principal, &workspace);
    let memory_service = match memory_resolver.resolve_for_scope(&principal, &workspace) {
        Ok(service) => service,
        Err(error) => {
            return HttpResponse::InternalServerError().json(QueryErrorResponse {
                error: error.to_string(),
            });
        },
    };
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        rebuild_scope_memory_index(memory_service.storage(), &scoped_store),
    )
    .await;

    match result {
        Ok(Ok(outcome)) => {
            let duration_ms = started.elapsed().as_millis() as u64;
            emit_memory_index_lancedb_write_rows(
                memory_service.storage(),
                &outcome.lancedb_write,
                Some(duration_ms),
                Some("manual_api_rebuild"),
            );
            let manifest = &outcome.manifest;
            HttpResponse::Ok().json(MemoryIndexRebuildResponse {
                principal,
                workspace,
                document_count: manifest.document_count,
                source_count: manifest.source_count,
                backend: manifest.backend.clone(),
                backend_status: manifest.backend_status.clone(),
                rebuilt_at_ms: manifest.rebuilt_at.timestamp_millis(),
                manifest_path: outcome.manifest_path.display().to_string(),
                documents_path: outcome.documents_path.display().to_string(),
                lancedb_write: outcome.lancedb_write,
                manifest: outcome.manifest,
            })
        },
        Ok(Err(error)) => HttpResponse::InternalServerError().json(QueryErrorResponse {
            error: error.to_string(),
        }),
        Err(_) => HttpResponse::GatewayTimeout().json(QueryErrorResponse {
            error: "memory index rebuild timed out after 120s".to_string(),
        }),
    }
}

/// Accept exactly one parsed SELECT/WITH query.
///
/// DuckDB's Rust `prepare()` executes all intermediate statements when given
/// a multi-statement string, so a leading-token check is not a security
/// boundary. Parsing first prevents a SELECT-shaped prefix from reconfiguring
/// the connection before the final result statement runs.
fn is_safe_select(sql: &str) -> bool {
    is_one_read_only_select_statement(sql)
}

/// The compatibility LLM-call endpoint must install its server-owned Parquet
/// view before external access is disabled. User SQL runs only afterwards, so
/// a SELECT-shaped request cannot invoke `read_*`, `glob`, extension loading,
/// or another filesystem/network table function.
fn disable_llm_query_external_access(
    conn: &duckdb::Connection,
    context: &str,
) -> anyhow::Result<()> {
    conn.execute_batch(
        "SET enable_external_access = false; \
         SET autoinstall_known_extensions = false; \
         SET autoload_known_extensions = false;",
    )
    .map_err(|error| {
        anyhow::anyhow!("{context}: failed to disable DuckDB external access: {error}")
    })
}

fn normalize_analytics_sql(sql: &str) -> String {
    // DuckDB versions in the app disagree on `TIMESTAMPTZ - INTERVAL`.
    // Existing dashboards used `epoch_ms(now() - INTERVAL ...)`; normalize
    // that stable Magician idiom instead of breaking persisted dashboards.
    sql.replace(
        "epoch_ms(now() - INTERVAL ",
        "epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL ",
    )
}

fn run_analytics_query(
    conn: &duckdb::Connection,
    sql: &str,
    row_limit: usize,
) -> anyhow::Result<QueryResponse> {
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| anyhow::anyhow!("prepare error: {e}"))?;
    let mut rows_iter = stmt
        .query([])
        .map_err(|e| anyhow::anyhow!("query error: {e}"))?;
    let stmt_ref = rows_iter
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("query did not expose statement metadata"))?;
    let column_count = stmt_ref.column_count();
    let columns: Vec<String> = (0..column_count)
        .map(|i| {
            stmt_ref
                .column_name(i)
                .map_or_else(|_| "?".to_string(), |s| s.to_string())
        })
        .collect();

    let mut rows = Vec::new();
    let mut output_bytes = serde_json::to_vec(&columns)
        .map_err(|error| anyhow::anyhow!("serialize query columns: {error}"))?
        .len();
    if output_bytes > ANALYTICS_DUCKDB_MAX_RESULT_BYTES {
        return Err(anyhow::anyhow!(
            "analytics result metadata exceeds the {} byte output limit",
            ANALYTICS_DUCKDB_MAX_RESULT_BYTES
        ));
    }
    while rows.len() < row_limit.min(ANALYTICS_DUCKDB_MAX_RESULT_ROWS) {
        let Some(row) = rows_iter
            .next()
            .map_err(|e| anyhow::anyhow!("row read error: {e}"))?
        else {
            break;
        };
        let mut values = Vec::with_capacity(column_count);
        for i in 0..column_count {
            // Reject an oversized variable-width cell before cloning it into
            // the HTTP response. This is also a fail-closed decode boundary:
            // storage/schema drift must not be represented as JSON null.
            let value_ref = row
                .get_ref(i)
                .map_err(|error| anyhow::anyhow!("column {i} decode error: {error}"))?;
            if duckdb_value_ref_output_bytes(value_ref).is_some_and(|bytes| {
                output_bytes.saturating_add(bytes) > ANALYTICS_DUCKDB_MAX_RESULT_BYTES
            }) {
                return Err(anyhow::anyhow!(
                    "analytics result exceeds the {} byte output limit",
                    ANALYTICS_DUCKDB_MAX_RESULT_BYTES
                ));
            }
            values.push(duckvalue_to_json(&value_ref.to_owned()));
        }
        let row_bytes = serde_json::to_vec(&values)
            .map_err(|error| anyhow::anyhow!("serialize query row: {error}"))?
            .len()
            .saturating_add(1);
        output_bytes = output_bytes.saturating_add(row_bytes);
        if output_bytes > ANALYTICS_DUCKDB_MAX_RESULT_BYTES {
            return Err(anyhow::anyhow!(
                "analytics result exceeds the {} byte output limit",
                ANALYTICS_DUCKDB_MAX_RESULT_BYTES
            ));
        }
        rows.push(values);
    }

    let row_count = rows.len();
    bounded_query_response(QueryResponse {
        columns,
        rows,
        row_count,
    })
}

fn bounded_query_response(response: QueryResponse) -> anyhow::Result<QueryResponse> {
    let serialized_bytes = serde_json::to_vec(&response)
        .map_err(|error| anyhow::anyhow!("serialize analytics query response: {error}"))?
        .len();
    if serialized_bytes > ANALYTICS_DUCKDB_MAX_RESULT_BYTES {
        return Err(anyhow::anyhow!(
            "analytics response exceeds the {} byte output limit",
            ANALYTICS_DUCKDB_MAX_RESULT_BYTES
        ));
    }
    Ok(response)
}

fn run_analytics_query_with_interrupt_timeout(
    conn: &duckdb::Connection,
    sql: &str,
    row_limit: usize,
    timeout: Duration,
) -> anyhow::Result<QueryResponse> {
    let interrupt = conn.interrupt_handle();
    let (done_tx, done_rx) = mpsc::channel();
    let watchdog = thread::spawn(move || {
        let timed_out = done_rx.recv_timeout(timeout).is_err();
        if timed_out {
            interrupt.interrupt();
        }
        timed_out
    });

    let result = run_analytics_query(conn, sql, row_limit);
    let _ = done_tx.send(());
    let timed_out = watchdog.join().unwrap_or(false);

    match result {
        Ok(response) => Ok(response),
        Err(error) if timed_out => Err(anyhow::anyhow!(
            "query item timed out after {}s: {error}",
            timeout.as_secs()
        )),
        Err(error) => Err(error),
    }
}

fn duckvalue_to_json(val: &duckdb::types::Value) -> serde_json::Value {
    use duckdb::types::Value;
    match val {
        Value::Null => serde_json::Value::Null,
        Value::Boolean(b) => serde_json::json!(b),
        Value::TinyInt(n) => serde_json::json!(n),
        Value::SmallInt(n) => serde_json::json!(n),
        Value::Int(n) => serde_json::json!(n),
        Value::BigInt(n) => serde_json::json!(n),
        Value::Float(f) => serde_json::json!(f),
        Value::Double(f) => serde_json::json!(f),
        Value::Text(s) => serde_json::json!(s),
        _ => {
            let s = format!("{:?}", val);
            if let Some(inner) = s.strip_prefix("HugeInt(") {
                if let Some(num_str) = inner.strip_suffix(')') {
                    if let Ok(n) = num_str.parse::<f64>() {
                        return serde_json::json!(n);
                    }
                }
            }
            serde_json::json!(s)
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    fn superseded_memory_candidate(
        item_key: &str,
        metadata: serde_json::Value,
    ) -> magician::magician_v2::agents::MemoryCandidateDocument {
        serde_json::from_value(json!({
            "principal": null,
            "workspace": null,
            "agent_id": null,
            "scope": "user",
            "tier_name": "preferences.preferences",
            "semantic_memory_type": "user_preference",
            "goal_id": null,
            "item_key": item_key,
            "source_path": null,
            "json_pointer": "/fields/preferences/0",
            "content_hash": "hash",
            "last_updated": "2026-08-21T00:00:00Z",
            "confidence": null,
            "text": "preferences: a coffee preference",
            "metadata_json": metadata,
        }))
        .expect("candidate fixture")
    }

    /// The end-to-end assertion the supersession key work exists for.
    ///
    /// The consolidator names the replacement in its own item-matching
    /// namespace (`durable::…`), which the overlay could never resolve — every
    /// consolidator supersession used to terminate in
    /// `successor_missing_from_overlay`, which reads on the `/memory` dashboard
    /// like corruption. This walks the whole path: consolidator metadata →
    /// candidate → overlay entry → chain.
    #[test]
    fn a_consolidator_supersession_produces_a_chain_that_resolves() {
        let now = chrono::Utc::now();
        let superseded = superseded_memory_candidate(
            "key:coffee_old",
            json!({
                "memory_lifecycle": "superseded",
                // What the consolidator has always written: a different
                // namespace, unresolvable against the overlay.
                "superseded_by": "durable::key::coffee",
                // What it now also writes, in the candidate namespace.
                "superseded_by_item_key": "key:coffee_current",
                "supersession_reason": "memory_conflict_review_replace_existing",
            }),
        );
        let successor = superseded_memory_candidate("key:coffee_current", json!({}));

        let successor_key =
            magician::magician_v2::agents::memory_temperature_candidate_key(&successor);
        let superseded_key =
            magician::magician_v2::agents::memory_temperature_candidate_key(&superseded);

        let mut entries = BTreeMap::new();
        for candidate in [&superseded, &successor] {
            let key = magician::magician_v2::agents::memory_temperature_candidate_key(candidate);
            entries.insert(
                key.clone(),
                MemoryTemperatureEntry::from_candidate(key, candidate, now),
            );
        }

        let chains = memory_supersession_chains(&entries);
        assert_eq!(chains.len(), 1, "one chain, rooted at the superseded item");
        let chain = &chains[0];
        assert_eq!(chain.root_memory_candidate_key, superseded_key);
        assert_eq!(chain.chain.len(), 2, "root plus a resolved successor");
        assert_eq!(chain.chain[1].memory_candidate_key, successor_key);
        assert!(
            chain
                .chain
                .iter()
                .all(|node| node.supersession_reason.as_deref()
                    != Some("successor_missing_from_overlay")),
            "the successor must resolve against the overlay"
        );
    }

    /// The legacy shape, pinned: metadata carrying only the consolidator's
    /// item-matching key still cannot resolve. Kept so the improvement stays
    /// visible and a regression to legacy-only writing is loud.
    #[test]
    fn a_legacy_only_supersession_still_reports_its_successor_missing() {
        let now = chrono::Utc::now();
        let superseded = superseded_memory_candidate(
            "key:coffee_old",
            json!({
                "memory_lifecycle": "superseded",
                "superseded_by": "durable::key::coffee",
            }),
        );
        let successor = superseded_memory_candidate("key:coffee_current", json!({}));

        let mut entries = BTreeMap::new();
        for candidate in [&superseded, &successor] {
            let key = magician::magician_v2::agents::memory_temperature_candidate_key(candidate);
            entries.insert(
                key.clone(),
                MemoryTemperatureEntry::from_candidate(key, candidate, now),
            );
        }

        let chains = memory_supersession_chains(&entries);
        assert_eq!(chains.len(), 1);
        let chain = &chains[0];
        assert_eq!(chain.chain.len(), 2);
        assert_eq!(
            chain.chain[1].supersession_reason.as_deref(),
            Some("successor_missing_from_overlay")
        );
    }

    /// `activity_rollups` has a column named `count`, which is also a DuckDB
    /// function name. Unquoted, the projection is a parse error — and one that
    /// would first appear seven days after the spine went live, when the first
    /// rollup object exists and every query to the endpoint starts 400-ing.
    /// Both branches quote, so a column named after a function is inert.
    #[test]
    fn a_projected_column_named_after_a_sql_function_is_quoted() {
        let column = AnalyticsViewColumn {
            name: "count",
            ty: "BIGINT",
            default_sql: "0::BIGINT".to_string(),
        };

        let present: HashSet<String> = ["count".to_string()].into_iter().collect();
        assert_eq!(
            analytics_projection_expr(&present, &column),
            "COALESCE(CAST(\"count\" AS BIGINT), 0::BIGINT) AS \"count\""
        );

        assert_eq!(
            analytics_projection_expr(&HashSet::new(), &column),
            "0::BIGINT AS \"count\"",
            "the absent-column branch must quote too, or a tier with no objects \
             yet parses while a populated one does not"
        );
    }

    /// The projection must actually run against DuckDB, not merely look right.
    #[test]
    fn the_rollup_projection_parses_against_duckdb() {
        let _guard = magician::magician_v2::analytics::duckdb_safety::analytics_duckdb_guard();
        let conn = duckdb::Connection::open_in_memory().expect("duckdb");
        conn.execute_batch(
            "CREATE TEMP TABLE activity_rollups_raw AS
               SELECT 7::BIGINT AS \"count\", 12::BIGINT AS sum_duration_ms;",
        )
        .expect("raw table");

        let columns = duckdb_view_columns(&conn, "activity_rollups_raw").expect("columns");
        let projection = ["count", "sum_duration_ms"]
            .into_iter()
            .map(|name| {
                analytics_projection_expr(
                    &columns,
                    &AnalyticsViewColumn {
                        name,
                        ty: "BIGINT",
                        default_sql: "0::BIGINT".to_string(),
                    },
                )
            })
            .collect::<Vec<_>>()
            .join(", ");

        let (count, sum): (i64, i64) = conn
            .query_row(
                &format!("SELECT {projection} FROM activity_rollups_raw"),
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("the projection must parse and return the stored values");
        assert_eq!(count, 7);
        assert_eq!(sum, 12);
    }

    #[actix_web::test]
    async fn memory_temperature_status_exposes_durable_utility_queue_health() {
        let temp = tempfile::tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temp.path());
        let api = web::Data::new(AnalyticsApi::new(layout).with_memory_eval_runtime(
            AgentDefinitionStore::new(magician::magician_v2::agents::AgentStorage::new(
                temp.path(),
            )),
            AgentMemoryResolver::new(temp.path()),
        ));
        let request = actix_web::test::TestRequest::get()
            .insert_header(("X-Principal", "owner"))
            .insert_header(("X-Workspace", "default"))
            .to_http_request();

        let response = memory_temperature_status_handler(request, Some(api)).await;

        assert_eq!(response.status(), actix_web::http::StatusCode::OK);
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .expect("temperature status body");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("temperature JSON");
        assert_eq!(body["principal"], serde_json::json!("owner"));
        assert_eq!(body["workspace"], serde_json::json!("default"));
        assert_eq!(body["utility_queue"]["active"], serde_json::json!(0));
        assert_eq!(body["utility_queue"]["eligible"], serde_json::json!(0));
        assert_eq!(body["utility_queue"]["retrying"], serde_json::json!(0));
        assert_eq!(body["utility_queue"]["dead"], serde_json::json!(0));
        assert!(body["utility_queue"]["oldest_pending_age_secs"].is_null());
    }

    #[actix_web::test]
    async fn restricted_content_errors_are_fixed_and_never_cacheable() {
        let response = restricted_content_error(LlmRestrictedContentError::Storage(
            "/private/path/that-must-not-leak".to_string(),
        ));
        assert_eq!(
            response.status(),
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            response
                .headers()
                .get("Cache-Control")
                .and_then(|value| value.to_str().ok()),
            Some("no-store, private")
        );
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .expect("restricted error body");
        let body = String::from_utf8(body.to_vec()).expect("UTF-8 body");
        assert!(body.contains("restricted_content_unavailable"));
        assert!(!body.contains("private/path"));
    }

    #[actix_web::test]
    async fn governed_reader_contention_is_structured_and_retryable() {
        let response = llm_read_failure(LLM_READ_GUARD_TIMEOUT_MESSAGE);

        assert_eq!(
            response.status(),
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            response
                .headers()
                .get(actix_web::http::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("1")
        );
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .expect("analytics busy body");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("analytics busy JSON");
        assert_eq!(body["code"], serde_json::json!("analytics_busy"));
        assert_eq!(body["retryable"], serde_json::json!(true));
        assert!(body["error"]
            .as_str()
            .is_some_and(|message| message.contains(LLM_READ_GUARD_TIMEOUT_MESSAGE)));
    }

    #[actix_web::test]
    async fn governed_llm_overview_requires_headers_and_returns_common_envelope() {
        let temp = tempfile::tempdir().expect("tempdir");
        let api = web::Data::new(AnalyticsApi::new(ArtifactV2Workspace::new(temp.path())));
        let missing = actix_web::test::TestRequest::get().to_http_request();
        let response = llm_observability_overview_handler(
            missing,
            Some(api.clone()),
            web::Query(LlmReadRangeQuery::default()),
        )
        .await;
        assert_eq!(response.status(), actix_web::http::StatusCode::BAD_REQUEST);

        let scoped = actix_web::test::TestRequest::get()
            .insert_header(("X-Principal", "owner"))
            .insert_header(("X-Workspace", "default"))
            .to_http_request();
        let response = llm_observability_overview_handler(
            scoped,
            Some(api),
            web::Query(LlmReadRangeQuery::default()),
        )
        .await;
        assert_eq!(response.status(), actix_web::http::StatusCode::OK);
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .expect("body");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(body["scope"]["principal"], serde_json::json!("owner"));
        assert_eq!(body["scope"]["workspace"], serde_json::json!("default"));
        assert_eq!(body["data"]["logical_calls"], serde_json::json!(0));
        assert_eq!(body["data"]["provider_attempts"], serde_json::json!(0));
        assert_eq!(
            body["data"]["known_missing_fact_revisions"],
            serde_json::json!(0)
        );
        assert_eq!(
            body["data"]["unclassified_transport_events_lost"],
            serde_json::json!(0)
        );
        assert_eq!(body["coverage"]["observed"], serde_json::json!(0));
        assert!(body.get("pagination").is_none());
    }

    #[actix_web::test]
    async fn governed_analytics_rejects_scope_components_that_path_projection_would_rewrite() {
        let temp = tempfile::tempdir().expect("tempdir");
        let api = web::Data::new(AnalyticsApi::new(ArtifactV2Workspace::new(temp.path())));
        let request = actix_web::test::TestRequest::get()
            .insert_header(("X-Principal", "owner/team"))
            .insert_header(("X-Workspace", "default"))
            .to_http_request();

        let response = llm_observability_overview_handler(
            request,
            Some(api),
            web::Query(LlmReadRangeQuery::default()),
        )
        .await;

        assert_eq!(response.status(), actix_web::http::StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn governed_llm_detail_is_scope_bound_and_absence_is_not_found() {
        let temp = tempfile::tempdir().expect("tempdir");
        let api = web::Data::new(AnalyticsApi::new(ArtifactV2Workspace::new(temp.path())));
        let request = actix_web::test::TestRequest::get()
            .insert_header(("X-Principal", "other-owner"))
            .insert_header(("X-Workspace", "other-workspace"))
            .to_http_request();
        let response = read_llm_call_handler(
            request,
            Some(api),
            web::Path::from("call-in-another-scope".to_string()),
        )
        .await;
        assert_eq!(response.status(), actix_web::http::StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn governed_trace_handlers_share_scope_and_absence_semantics() {
        let temp = tempfile::tempdir().expect("tempdir");
        let api = web::Data::new(AnalyticsApi::new(ArtifactV2Workspace::new(temp.path())));
        let request = || {
            actix_web::test::TestRequest::get()
                .insert_header(("X-Principal", "owner"))
                .insert_header(("X-Workspace", "default"))
                .to_http_request()
        };
        let response = list_llm_traces_handler(
            request(),
            Some(api.clone()),
            web::Query(LlmTraceListQuery {
                from_ms: Some(1_700_000_000_000),
                to_ms: Some(1_700_003_600_000),
                limit: Some(10),
            }),
        )
        .await;
        assert_eq!(response.status(), actix_web::http::StatusCode::OK);
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .expect("trace list body");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("trace list json");
        assert_eq!(body["scope"]["principal"], serde_json::json!("owner"));
        assert_eq!(body["data"]["row_count"], serde_json::json!(0));

        let response = read_llm_trace_handler(
            request(),
            Some(api),
            web::Path::from("missing-trace".to_string()),
            web::Query(LlmReadRangeQuery {
                from_ms: Some(1_700_000_000_000),
                to_ms: Some(1_700_003_600_000),
            }),
        )
        .await;
        assert_eq!(response.status(), actix_web::http::StatusCode::NOT_FOUND);
    }

    #[test]
    fn governed_llm_list_adapter_maps_only_typed_exact_filters() {
        let request = llm_fact_list_query(
            LlmFactRelation::Calls,
            LlmFactListQuery {
                operation: Some("chat_response".to_string()),
                provider: Some("openai".to_string()),
                success: Some(false),
                limit: Some(50),
                offset: Some(100),
                ..LlmFactListQuery::default()
            },
        );
        assert_eq!(request.relation, "llm_calls");
        assert_eq!(request.limit, Some(50));
        assert_eq!(request.offset, 100);
        assert_eq!(request.filters.len(), 3);
        assert!(matches!(
            &request.filters[2],
            LlmFactFilter::BooleanEquals {
                column,
                value: false
            } if column == "transport_success"
        ));
    }

    #[test]
    fn governed_llm_request_contracts_reject_scope_and_unknown_fields() {
        assert!(
            serde_json::from_value::<LlmFactSqlRequest>(serde_json::json!({
                "sql": "SELECT * FROM llm_calls",
                "principal": "forged"
            }))
            .is_err()
        );
        assert!(serde_json::from_value::<LlmFactQuery>(serde_json::json!({
            "relation": "llm_calls",
            "columns": [],
            "filters": [],
            "order_by": "timestamp_ms",
            "descending": true,
            "limit": 10,
            "offset": 0,
            "from_ms": null,
            "to_ms": null,
            "workspace": "forged"
        }))
        .is_err());
    }

    #[actix_web::test]
    async fn legacy_llm_rest_rejects_raw_catalog_and_external_relations_before_storage() {
        let temp = tempfile::tempdir().expect("tempdir");
        let api = web::Data::new(AnalyticsApi::new(ArtifactV2Workspace::new(temp.path())));

        for sql in [
            "SELECT * FROM llm_calls_raw",
            "SELECT * FROM duckdb_tables()",
            "SELECT * FROM read_parquet('/tmp/private.parquet')",
            "SELECT * FROM main.llm_calls",
            "SELECT * INTO temporary_llm_copy FROM llm_calls",
            "WITH scoped AS (SELECT * FROM llm_calls) VALUES (1)",
            "WITH payload AS (VALUES (1)) SELECT * FROM llm_calls",
            "SELECT * FROM llm_calls WHERE EXISTS (SELECT * FROM (VALUES (1)) AS payload(value))",
            "TABLE llm_calls",
        ] {
            let request = actix_web::test::TestRequest::post()
                .insert_header(("X-Principal", "owner"))
                .insert_header(("X-Workspace", "default"))
                .to_http_request();
            let response = query_llm_calls_handler(
                request,
                Some(api.clone()),
                web::Json(QueryRequest {
                    sql: sql.to_string(),
                }),
            )
            .await;
            assert_eq!(
                response.status(),
                actix_web::http::StatusCode::BAD_REQUEST,
                "unsafe query reached storage: {sql}"
            );
        }
    }

    #[test]
    fn legacy_llm_rest_query_disables_external_functions_after_view_install() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parquet = temp.path().join("calls.parquet");
        let conn = duckdb::Connection::open_in_memory().expect("open DuckDB");
        configure_analytics_connection_checked(&conn, "legacy_llm_rest_external_access")
            .expect("configure DuckDB");
        conn.execute_batch(&format!(
            "COPY (SELECT 1::BIGINT AS call_count) TO '{}' (FORMAT PARQUET); \
             CREATE TEMP TABLE llm_calls AS SELECT * FROM read_parquet('{}');",
            escape_sql_literal(&parquet.display().to_string()),
            escape_sql_literal(&parquet.display().to_string()),
        ))
        .expect("server-owned Parquet view");
        disable_llm_query_external_access(&conn, "legacy_llm_rest_external_access")
            .expect("disable external access");

        let call_count: i64 = conn
            .query_row("SELECT call_count FROM llm_calls", [], |row| row.get(0))
            .expect("installed view remains readable");
        assert_eq!(call_count, 1);
        assert!(
            conn.prepare("SELECT * FROM glob('/tmp/*')").is_err(),
            "external table functions must fail after the server view is installed"
        );
    }

    #[test]
    fn memory_events_query_disables_external_functions_after_view_install() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parquet = temp.path().join("memory.parquet");
        let conn = duckdb::Connection::open_in_memory().expect("open DuckDB");
        configure_analytics_connection_checked(&conn, "memory_events_external_access")
            .expect("configure DuckDB");
        conn.execute_batch(&format!(
            "COPY (SELECT 1700000000000::BIGINT AS timestamp_ms,
                          'recall'::VARCHAR AS event_kind)
             TO '{}' (FORMAT PARQUET);",
            escape_sql_literal(&parquet.display().to_string()),
        ))
        .expect("server-owned Parquet fixture");
        install_memory_events_view(
            &conn,
            &format!("'{}'", escape_sql_literal(&parquet.display().to_string())),
            "owner",
            "default",
            None,
        )
        .expect("server-owned memory views");
        disable_llm_query_external_access(&conn, "memory_events_external_access")
            .expect("disable external access");

        let event_kind: String = conn
            .query_row("SELECT event_kind FROM memory_events", [], |row| row.get(0))
            .expect("installed memory view remains readable");
        assert_eq!(event_kind, "recall");
        assert!(
            conn.prepare("SELECT * FROM glob('/tmp/*')").is_err(),
            "external table functions must fail after the memory view is installed"
        );
    }

    #[test]
    fn legacy_analytics_sql_requires_exactly_one_parsed_query() {
        assert!(is_safe_select("SELECT 1"));
        assert!(is_safe_select(
            "-- retained dashboard query\nWITH sample AS (SELECT 1 AS value) SELECT * FROM sample;"
        ));
        assert!(!is_safe_select("SELECT 1; SELECT 2"));
        assert!(!is_safe_select(
            "SELECT 1; SET enable_external_access = true; SELECT 2"
        ));
        assert!(!is_safe_select(
            "WITH sample AS (SELECT 1) DELETE FROM local_table"
        ));
        assert!(!is_safe_select("SELECT * INTO copied FROM llm_calls"));
        assert!(!is_safe_select("VALUES (1)"));
        assert!(!is_safe_select(
            "WITH payload AS (VALUES (1)) SELECT * FROM payload"
        ));
        assert!(!is_safe_select(
            "SELECT * FROM (VALUES (1)) AS payload(value)"
        ));
        assert!(!is_safe_select("TABLE llm_calls"));
    }

    #[test]
    fn legacy_analytics_query_fails_closed_on_oversized_results() {
        let conn = duckdb::Connection::open_in_memory().expect("open DuckDB");
        let oversized = ANALYTICS_DUCKDB_MAX_RESULT_BYTES + 1;
        let error = run_analytics_query(
            &conn,
            &format!("SELECT repeat('x', {oversized}) AS payload"),
            1,
        )
        .expect_err("oversized result must fail closed");

        assert!(error.to_string().contains("byte output limit"));
    }

    #[test]
    fn analytics_query_budget_measures_the_complete_serialized_response() {
        let error = bounded_query_response(QueryResponse {
            columns: vec!["payload".to_string()],
            rows: vec![vec![serde_json::json!(
                "x".repeat(ANALYTICS_DUCKDB_MAX_RESULT_BYTES)
            )]],
            row_count: 1,
        })
        .expect_err("complete serialized response must be bounded");

        assert!(error.to_string().contains("byte output limit"));
    }

    #[test]
    fn analytics_query_batches_enforce_one_aggregate_serialized_byte_limit() {
        let mut results = Vec::new();
        let mut bytes = 0;
        let error = push_bounded_query_batch_item(
            &mut results,
            &mut bytes,
            QueryBatchItemResponse {
                columns: vec!["payload".to_string()],
                rows: vec![vec![serde_json::json!(
                    "x".repeat(ANALYTICS_DUCKDB_MAX_RESULT_BYTES)
                )]],
                row_count: 1,
                error: None,
            },
        )
        .expect_err("aggregate batch byte limit must fail closed");

        assert!(error.to_string().contains("byte output limit"));
        assert!(results.is_empty());
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, b"fixture").expect("write");
    }

    fn write_memory_parquet_fixture(path: &Path, id: i64) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let connection = duckdb::Connection::open_in_memory().expect("duckdb");
        connection
            .execute_batch(&format!(
                "CREATE TABLE memory_events(event_id BIGINT, event_kind VARCHAR); \
                 INSERT INTO memory_events VALUES ({id}, 'retrieval'); \
                 COPY memory_events TO '{}' (FORMAT PARQUET);",
                path.display().to_string().replace('\'', "''")
            ))
            .expect("write fixture");
    }

    #[test]
    fn memory_events_selector_preserves_compacted_partitions_under_raw_file_cap() {
        let temp = tempfile::tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temp.path());
        let root = layout.analytics_memory_events_root("owner", "default");
        let older_date = Utc::now().date_naive() - chrono::Duration::days(2);
        let newer_date = Utc::now().date_naive() - chrono::Duration::days(1);
        let older = root.join(format!("dt={older_date}"));
        let newer = root.join(format!("dt={newer_date}"));
        write_memory_parquet_fixture(&older.join("batch_001.parquet"), 1);
        write_memory_parquet_fixture(&older.join("batch_002.parquet"), 2);
        magician::magician_v2::analytics::parquet_maintenance::compact_dataset(
            &layout,
            "owner",
            "default",
            magician::magician_v2::analytics::parquet_maintenance::PartitionedDataset::MemoryEvents,
            1,
        )
        .expect("compact older partition");
        let older_compacted = compacted_partition_file(&older);
        for index in 0..8 {
            touch(&newer.join(format!("batch_{index:03}.parquet")));
        }

        let files = memory_events_partitioned_parquet_files_with_max(&root, None, 4);

        assert!(files.contains(&older_compacted));
        assert_eq!(files.len(), 4);
        assert!(files.contains(&newer.join("batch_005.parquet")));
        assert!(files.contains(&newer.join("batch_006.parquet")));
        assert!(files.contains(&newer.join("batch_007.parquet")));
        assert!(!files.contains(&older.join("batch_001.parquet")));
        assert!(!files.contains(&older.join("batch_002.parquet")));
    }

    #[test]
    fn embedding_selector_uses_manifest_generation_without_double_counting_recovered_raw() {
        let temp = tempfile::tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temp.path());
        let root = layout.analytics_llm_embeddings_root("owner", "default");
        let date = Utc::now().date_naive() - chrono::Duration::days(2);
        let partition = root.join(format!("dt={date}"));
        let original = partition.join("embed_original.parquet");
        write_memory_parquet_fixture(&original, 1);
        let original_bytes = std::fs::read(&original).expect("original bytes");
        magician::magician_v2::analytics::parquet_maintenance::compact_dataset(
            &layout,
            "owner",
            "default",
            magician::magician_v2::analytics::parquet_maintenance::PartitionedDataset::LlmEmbeddings,
            1,
        )
        .expect("compact embeddings");
        std::fs::write(&original, original_bytes).expect("restore represented raw");
        let late = partition.join("embed_late.parquet");
        write_memory_parquet_fixture(&late, 2);

        let files = governed_embedding_partition_files(&root, None).expect("embedding sources");
        let selected = magician::magician_v2::analytics::parquet_maintenance::compacted_file(
            &partition,
            magician::magician_v2::analytics::parquet_maintenance::PartitionedDataset::LlmEmbeddings,
        );
        assert_eq!(files.len(), 2);
        assert!(files.contains(&selected));
        assert!(files.contains(&late));
        assert!(!files.contains(&original));
    }

    #[test]
    fn llm_call_selector_combines_canonical_and_governed_legacy_without_double_counting() {
        let temp = tempfile::tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temp.path());
        let root = layout.analytics_llm_calls_root("owner", "default");
        let date = Utc::now().date_naive() - chrono::Duration::days(2);
        let partition = root.join(format!("dt={date}"));
        let historical = partition.join("pre-prefix-history.parquet");
        let canonical = partition.join("part-call_fact-current.parquet");
        write_memory_parquet_fixture(&historical, 1);
        write_memory_parquet_fixture(&canonical, 2);
        let historical_bytes = std::fs::read(&historical).expect("historical bytes");
        magician::magician_v2::analytics::parquet_maintenance::compact_dataset(
            &layout,
            "owner",
            "default",
            magician::magician_v2::analytics::parquet_maintenance::PartitionedDataset::LegacyLlmCalls,
            1,
        )
        .expect("compact legacy calls");
        std::fs::write(&historical, historical_bytes).expect("restore represented raw");
        let late = partition.join("late-legacy.parquet");
        write_memory_parquet_fixture(&late, 3);

        let scope = LlmScope::new("owner", "default");
        let files =
            governed_llm_call_partition_files(&layout, &scope, &root, None).expect("call sources");
        let selected = magician::magician_v2::analytics::parquet_maintenance::compacted_file(
            &partition,
            magician::magician_v2::analytics::parquet_maintenance::PartitionedDataset::LegacyLlmCalls,
        );
        assert_eq!(files.len(), 3);
        assert!(files.contains(&canonical));
        assert!(files.contains(&selected));
        assert!(files.contains(&late));
        assert!(!files.contains(&historical));
    }

    #[test]
    fn memory_events_selector_keeps_current_day_raw() {
        let temp = tempfile::tempdir().expect("tempdir");
        let current = temp
            .path()
            .join(format!("dt={}", Utc::now().date_naive().format("%Y-%m-%d")));
        let raw_file = current.join("batch_001.parquet");
        let compacted_file = compacted_partition_file(&current);
        touch(&raw_file);
        touch(&compacted_file);

        let files = memory_events_partitioned_parquet_files_with_max(temp.path(), None, 10);

        assert!(files.contains(&raw_file));
        assert!(!files.contains(&compacted_file));
    }

    #[test]
    fn generic_partition_selector_keeps_legacy_newest_file_cap_behavior() {
        let temp = tempfile::tempdir().expect("tempdir");
        let older = temp.path().join("dt=2026-07-01");
        let newer = temp.path().join("dt=2026-07-02");
        touch(&older.join("batch_001.parquet"));
        touch(&older.join("batch_002.parquet"));
        for index in 0..4 {
            touch(&newer.join(format!("batch_{index:03}.parquet")));
        }

        let files = partitioned_parquet_files_with_max(temp.path(), None, 3);

        assert_eq!(
            files,
            vec![
                newer.join("batch_001.parquet"),
                newer.join("batch_002.parquet"),
                newer.join("batch_003.parquet"),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn generic_partition_selector_never_follows_symlinked_partitions_or_parquet_files() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        touch(&outside.path().join("outside.parquet"));
        symlink(outside.path(), temp.path().join("dt=2026-07-01")).expect("symlinked partition");

        let real_partition = temp.path().join("dt=2026-07-02");
        std::fs::create_dir_all(&real_partition).expect("real partition");
        symlink(
            outside.path().join("outside.parquet"),
            real_partition.join("linked.parquet"),
        )
        .expect("symlinked parquet");
        let real_file = real_partition.join("real.parquet");
        touch(&real_file);

        assert_eq!(
            partitioned_parquet_files_with_max(temp.path(), None, 10),
            vec![real_file]
        );
        assert!(has_partitioned_parquet(temp.path()));
    }

    #[test]
    fn crew_health_llm_rollup_rejects_unsafe_scope_without_touching_storage() {
        let temp = tempfile::tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temp.path());

        let error = query_agent_llm_health_rollups(&layout, "owner/team", "default")
            .expect_err("unsafe scope must fail closed");

        assert!(error.to_string().contains("safe scope components"));
    }

    #[test]
    fn crew_health_numeric_decoders_preserve_invalid_instead_of_inventing_zero() {
        let negative = serde_json::json!("-1");
        let malformed = serde_json::json!("not-a-number");
        let non_finite = serde_json::json!("NaN");

        assert_eq!(json_u64(Some(&negative)), None);
        assert_eq!(json_f64(Some(&malformed)), None);
        assert!(json_f64(Some(&non_finite)).is_some_and(|value| !value.is_finite()));
    }

    #[test]
    fn llm_calls_lifetime_marker_overrides_default_recent_cap() {
        let queries = vec![
            "WITH scoped AS (SELECT * FROM llm_calls WHERE /* magician:all_llm_partitions */ TRUE) SELECT COUNT(*) FROM scoped".to_string(),
        ];

        assert_eq!(effective_llm_calls_lookback_days(&queries, Some(30)), None);
    }

    #[test]
    fn llm_calls_view_supplies_typed_phase1_defaults_for_historical_parquet() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parquet = temp.path().join("legacy.parquet");
        let writer = duckdb::Connection::open_in_memory().expect("writer");
        writer
            .execute_batch(&format!(
                "CREATE TABLE legacy(
                     timestamp_ms BIGINT,
                     operation VARCHAR,
                     reasoning_summary VARCHAR,
                     error VARCHAR
                 ); \
                 INSERT INTO legacy VALUES (
                     1700000000000,
                     'chat_completion',
                     'private reasoning must not cross the compatibility boundary',
                     'private provider payload must be redacted'
                 ); \
                 COPY legacy TO '{}' (FORMAT PARQUET);",
                escape_sql_literal(&parquet.to_string_lossy())
            ))
            .expect("write legacy parquet");

        let reader = duckdb::Connection::open_in_memory().expect("reader");
        install_llm_calls_views(
            &reader,
            &format!("'{}'", escape_sql_literal(&parquet.to_string_lossy())),
            None,
            "owner",
            "default",
            None,
        )
        .expect("install stable view");

        let row: (
            i32,
            String,
            String,
            Option<String>,
            String,
            String,
            i32,
            bool,
            Option<String>,
        ) = reader
            .query_row(
                "SELECT schema_version, principal, workspace, llm_call_id, \
                        scope_resolution, workload_class, provider_attempt_count, response_reused, error \
                 FROM llm_calls",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ))
                },
            )
            .expect("read stable historical row");
        assert_eq!(row.0, 0);
        assert_eq!(row.1, "owner");
        assert_eq!(row.2, "default");
        assert_eq!(row.3, None);
        assert_eq!(row.4, "legacy_default");
        assert_eq!(row.5, "system");
        assert_eq!(row.6, 0);
        assert!(!row.7);
        assert_eq!(row.8.as_deref(), Some("legacy_error_redacted"));
        assert!(reader
            .prepare("SELECT reasoning_summary FROM llm_calls")
            .is_err());
        assert!(reader.prepare("SELECT * FROM llm_calls_raw").is_err());
    }
}

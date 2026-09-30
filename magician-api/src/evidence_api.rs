//! HTTP API for the work-evidence graph read path (Phase 0).
//!
//! - `POST /api/magician/v2/evidence/review` — assemble accrued evidence over a
//!   window + facet, synthesize an impact summary, persist it as a durable
//!   artifact, and return the Markdown.
//! - `GET  /api/magician/v2/evidence` — list an agent's accrued evidence.
//! - `GET  /api/magician/v2/evidence/reviews` — list past review artifacts.
//!
//! Calls the shared `evidence::*` library + `AgentMemoryService` directly — a
//! peer of the `distill-evidence`/`review` CLI commands, not a wrapper on them.

use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse};
use serde::{Deserialize, Serialize};

use crate::scope::resolve_required_scope;
use magician::magician_v2::agents::AgentMemoryResolver;
use magician::magician_v2::artifact_v2::models::{
    PublishSurfaceInput, PublishedSurfacePlacement, TaskLifecycle, TaskOutputMode, TaskSyncMode,
};
use magician::magician_v2::artifact_v2::service::{
    CreateTaskInput, ScopeRef, WriteUserOutputBody, WriteUserOutputDirectInput,
};
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::artifact_v2::ArtifactV2Service;
use magician::magician_v2::artifacts::durable_store::{
    open_local_durable_artifacts, DurableArtifactStore, DurableFrontmatter,
};
use magician::magician_v2::evidence::{
    build_dashboard, build_review_packet, extract_cited_ids, merge_entities,
    render_dashboard_markdown, select_evidence_window, split_entity,
    synthesize_review_with_telemetry, utility, verify_review_with_telemetry, DashboardData,
    EvidenceRecord, EvidenceStatus, Facet, ReviewFeedback, ReviewFeedbackLedger, ReviewVerdict,
    VerificationReport,
};
use magician::magician_v2::prompts::PromptManager;
use magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;
use magician::magician_v2::realtime_events::RuntimeTransportBroadcaster;

/// Durable-artifact namespace reviews are written to (served by the artifact API).
const REVIEWS_NAMESPACE: &str = "evidence-reviews";
const DEFAULT_EVIDENCE_LIST_LIMIT: usize = 50;
const MAX_EVIDENCE_LIST_LIMIT: usize = 200;

#[derive(Clone)]
pub struct EvidenceApi {
    memory_resolver: AgentMemoryResolver,
    workspace_layout: ArtifactV2Workspace,
    event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
}

impl EvidenceApi {
    pub fn new(
        memory_resolver: AgentMemoryResolver,
        workspace_layout: ArtifactV2Workspace,
    ) -> Self {
        Self {
            memory_resolver,
            workspace_layout,
            event_broadcaster: None,
        }
    }

    pub fn with_event_broadcaster(
        mut self,
        event_broadcaster: Arc<RuntimeTransportBroadcaster>,
    ) -> Self {
        self.event_broadcaster = Some(event_broadcaster);
        self
    }
}

fn default_days() -> i64 {
    7
}
fn default_facet() -> String {
    "work".to_string()
}

fn err_json(status: actix_web::http::StatusCode, message: impl std::fmt::Display) -> HttpResponse {
    HttpResponse::build(status).json(serde_json::json!({ "error": message.to_string() }))
}

#[derive(Deserialize)]
pub struct EvidenceReviewRequest {
    pub agent: String,
    #[serde(default = "default_days")]
    pub days: i64,
    #[serde(default = "default_facet")]
    pub facet: String,
    /// When true, refuse to persist a review that fails the grounding gate
    /// (returns 422 with the verification report instead).
    #[serde(default)]
    pub strict: bool,
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Serialize)]
pub struct EvidenceReviewResponse {
    pub markdown: String,
    pub artifact_namespace: String,
    pub artifact_name: Option<String>,
    pub evidence_count: usize,
    pub input_evidence_ids: Vec<String>,
    pub verification: Option<VerificationReport>,
}

#[derive(Deserialize)]
pub struct EvidenceListQuery {
    pub agent: String,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub facet: Option<String>,
    #[serde(default)]
    pub days: Option<i64>,
    /// Include deleted tombstones (off by default; the inbox hides them).
    #[serde(default)]
    pub include_deleted: bool,
    /// Annotate each record with the past reviews that cited it (forward
    /// lineage). Off by default — it scans review artifacts.
    #[serde(default)]
    pub with_usage: bool,
    /// Page size for evidence inbox reads. Defaults to a bounded page so
    /// `with_usage=true` does not force review-lineage decoration for every
    /// persisted evidence record.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Zero-based record offset after filtering and newest-first sorting.
    #[serde(default)]
    pub offset: Option<usize>,
}

#[derive(Deserialize, Clone)]
pub struct FacetInput {
    pub label: String,
    #[serde(default)]
    pub confidence: Option<f64>,
}

#[derive(Deserialize)]
pub struct EvidenceCorrectRequest {
    pub agent: String,
    pub evidence_id: String,
    /// One of: `suppress` | `unsuppress` | `delete` | `set_facets`.
    pub action: String,
    /// Replacement facet set for `set_facets` (assigned_by is forced to `user`).
    #[serde(default)]
    pub facets: Option<Vec<FacetInput>>,
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Deserialize)]
pub struct ReviewsListQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

fn evidence_page_window(
    total: usize,
    limit: Option<usize>,
    offset: Option<usize>,
) -> (usize, usize, bool) {
    let requested_limit = limit.unwrap_or(DEFAULT_EVIDENCE_LIST_LIMIT);
    let page_limit = if requested_limit == 0 {
        DEFAULT_EVIDENCE_LIST_LIMIT
    } else {
        requested_limit.min(MAX_EVIDENCE_LIST_LIMIT)
    };
    let page_offset = offset.unwrap_or(0).min(total);
    let has_more = page_offset.saturating_add(page_limit) < total;
    (page_limit, page_offset, has_more)
}

/// Generate (and persist) an impact review over a caller-chosen window + facet.
pub async fn post_evidence_review_handler(
    api: web::Data<EvidenceApi>,
    operation_router: web::Data<OperationLlmRouter>,
    prompt_manager: web::Data<Arc<PromptManager>>,
    req: HttpRequest,
    body: web::Json<EvidenceReviewRequest>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(resp) => return resp,
    };
    if body.agent.trim().is_empty() {
        return err_json(
            actix_web::http::StatusCode::BAD_REQUEST,
            "agent is required",
        );
    }

    let memory = match api
        .memory_resolver
        .resolve_for_scope(&principal, &workspace)
    {
        Ok(memory) => memory,
        Err(err) => return err_json(actix_web::http::StatusCode::BAD_REQUEST, err),
    };
    let records = match memory.load_scoped_evidence(&body.agent).await {
        Ok(records) => records,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };

    let facet_opt = if body.facet.eq_ignore_ascii_case("all") {
        None
    } else {
        Some(body.facet.as_str())
    };
    let since = chrono::Utc::now() - chrono::Duration::days(body.days);
    let selected = select_evidence_window(&records, since, facet_opt);
    if selected.is_empty() {
        return HttpResponse::Ok().json(EvidenceReviewResponse {
            markdown: format!(
                "_No evidence in the last {} day(s){}._",
                body.days,
                facet_opt
                    .map(|f| format!(" for facet '{f}'"))
                    .unwrap_or_default()
            ),
            artifact_namespace: REVIEWS_NAMESPACE.to_string(),
            artifact_name: None,
            evidence_count: 0,
            input_evidence_ids: Vec::new(),
            verification: None,
        });
    }

    let (packet, evidence_ids) = build_review_packet(&selected);
    let telemetry = api.event_broadcaster.as_ref().map(|broadcaster| {
        magician::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext::new(
            Arc::clone(broadcaster),
            principal.clone(),
            workspace.clone(),
            "evidence_review",
        )
    });
    let scoped_operation_router = operation_router.with_scope_context(Some(
        magicllm::LlmScope::new(principal.clone(), workspace.clone()),
    ));
    let review = match synthesize_review_with_telemetry(
        &packet,
        body.days,
        facet_opt,
        &scoped_operation_router,
        &**prompt_manager,
        telemetry.as_ref(),
    )
    .await
    {
        Ok(review) => review,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };

    // Slice 5 verification gate: check the review is grounded in the evidence it
    // was built from. Fail-soft — a verifier error must not lose the review.
    let verification = match verify_review_with_telemetry(
        &review,
        &packet,
        &evidence_ids,
        &scoped_operation_router,
        &**prompt_manager,
        telemetry.as_ref(),
    )
    .await
    {
        Ok(report) => Some(report),
        Err(err) => {
            tracing::warn!(error = %err, "evidence review verification failed (non-fatal)");
            None
        },
    };
    let grounded = verification.as_ref().map(|v| v.grounded).unwrap_or(true);

    // Strict callers refuse an ungrounded review rather than persist it.
    if body.strict && !grounded {
        return HttpResponse::UnprocessableEntity().json(serde_json::json!({
            "error": "review failed the grounding gate",
            "markdown": review,
            "evidence_count": selected.len(),
            "input_evidence_ids": evidence_ids,
            "verification": verification,
        }));
    }

    let now = chrono::Utc::now();
    let grounding_note = match verification.as_ref() {
        Some(v) if v.grounded => format!(
            "\n\n---\n_Grounding check: passed — {covered}/{total} claim(s) cited; {n} evidence record(s) referenced._\n",
            covered = v.total_bullets.saturating_sub(v.uncited_bullets),
            total = v.total_bullets,
            n = v.cited_ids.len(),
        ),
        Some(v) => format!(
            "\n\n---\n> ⚠️ **Grounding check flagged this review.** {uncited} uncited claim(s); {flagged} claim(s) the critic could not verify against the evidence. Treat unverified statements with caution.\n",
            uncited = v.uncited_bullets,
            flagged = v.ungrounded_claims.len(),
        ),
        None => String::new(),
    };
    let artifact_body = format!(
        "# Impact review — {facet} · last {days} day(s)\n\n> Generated {generated} from {count} evidence record(s).\n\n{review}{grounding_note}\n\n<!-- input_evidence_ids: {ids} -->\n",
        facet = body.facet,
        days = body.days,
        generated = now.to_rfc3339(),
        count = selected.len(),
        review = review,
        grounding_note = grounding_note,
        ids = evidence_ids.join(", "),
    );
    let artifact_name = format!(
        "{}-{}d-{}.md",
        body.facet,
        body.days,
        now.format("%Y%m%dT%H%M%SZ")
    );
    let store = match open_local_durable_artifacts(&api.workspace_layout, &principal, &workspace) {
        Ok(store) => store,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let frontmatter = DurableFrontmatter {
        namespace: REVIEWS_NAMESPACE.to_string(),
        name: artifact_name.clone(),
        created_by: "evidence-review".to_string(),
        last_updated_by: "evidence-review".to_string(),
        last_updated: now,
        content_type: Some("text/markdown".to_string()),
        source_execution_id: None,
        source_task_id: None,
        source_workflow_instance_id: None,
        source_run_id: None,
        source_cycle_id: None,
        source_agent_id: Some(body.agent.clone()),
        producer_stage: Some("evidence_review".to_string()),
    };
    if let Err(err) = store
        .write(
            REVIEWS_NAMESPACE,
            &artifact_name,
            &artifact_body,
            frontmatter,
        )
        .await
    {
        return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err);
    }

    HttpResponse::Ok().json(EvidenceReviewResponse {
        markdown: artifact_body,
        artifact_namespace: REVIEWS_NAMESPACE.to_string(),
        artifact_name: Some(artifact_name),
        evidence_count: selected.len(),
        input_evidence_ids: evidence_ids,
        verification,
    })
}

/// List an agent's accrued evidence (optionally windowed + facet-filtered).
pub async fn list_evidence_handler(
    api: web::Data<EvidenceApi>,
    req: HttpRequest,
    query: web::Query<EvidenceListQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let memory = match api
        .memory_resolver
        .resolve_for_scope(&principal, &workspace)
    {
        Ok(memory) => memory,
        Err(err) => return err_json(actix_web::http::StatusCode::BAD_REQUEST, err),
    };
    let mut records = match memory.load_native_evidence(&query.agent).await {
        Ok(records) => records,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    // The inbox lists active + suppressed (so suppressed can be restored);
    // deleted tombstones stay hidden unless explicitly requested. Suppressed
    // records are intentionally NOT filtered out here — unlike the review path
    // (`select_evidence_window`), which only sees active records.
    if !query.include_deleted {
        records.retain(|r| r.status != EvidenceStatus::Deleted);
    }
    if let Some(facet) = query
        .facet
        .as_deref()
        .filter(|f| !f.eq_ignore_ascii_case("all"))
    {
        records.retain(|r| r.facets.iter().any(|f| f.label == facet));
    }
    if let Some(days) = query.days {
        let since = chrono::Utc::now() - chrono::Duration::days(days);
        records.retain(|r| {
            chrono::DateTime::parse_from_rfc3339(&r.last_seen_at)
                .map(|ts| ts.with_timezone(&chrono::Utc) >= since)
                .unwrap_or(true)
        });
    }
    records.sort_by(|a, b| b.last_seen_at.cmp(&a.last_seen_at));
    let total_count = records.len();
    let (limit, offset, has_more) = evidence_page_window(total_count, query.limit, query.offset);
    let page_records: Vec<EvidenceRecord> = records.into_iter().skip(offset).take(limit).collect();
    let returned_count = page_records.len();

    // Forward lineage: annotate each record with the reviews that cited it, by
    // scanning review artifacts once and mapping evidence_id -> [review names].
    if query.with_usage {
        let usage = review_usage_index(&api, &principal, &workspace).await;
        let annotated: Vec<serde_json::Value> = page_records
            .iter()
            .map(|r| {
                let mut value = serde_json::to_value(r).unwrap_or_else(|_| serde_json::json!({}));
                let used_in = usage.get(&r.evidence_id).cloned().unwrap_or_default();
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("used_in".to_string(), serde_json::json!(used_in));
                }
                value
            })
            .collect();
        return HttpResponse::Ok().json(serde_json::json!({
            "agent": query.agent,
            "count": total_count,
            "total_count": total_count,
            "returned_count": annotated.len(),
            "limit": limit,
            "offset": offset,
            "has_more": has_more,
            "evidence": annotated,
        }));
    }

    HttpResponse::Ok().json(serde_json::json!({
        "agent": query.agent,
        "count": total_count,
        "total_count": total_count,
        "returned_count": returned_count,
        "limit": limit,
        "offset": offset,
        "has_more": has_more,
        "evidence": page_records,
    }))
}

/// Build a forward-lineage index `evidence_id -> [review artifact names]` by
/// scanning the scope's review artifacts for their cited ids. Best-effort:
/// unreadable reviews are skipped.
async fn review_usage_index(
    api: &EvidenceApi,
    principal: &str,
    workspace: &str,
) -> std::collections::HashMap<String, Vec<String>> {
    let mut index: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let store = match open_local_durable_artifacts(&api.workspace_layout, principal, workspace) {
        Ok(store) => store,
        Err(_) => return index,
    };
    let entries = match store.list(Some(REVIEWS_NAMESPACE)) {
        Ok(entries) => entries,
        Err(_) => return index,
    };
    for entry in entries {
        if let Ok((_, body)) = store.read(REVIEWS_NAMESPACE, &entry.name).await {
            for id in extract_cited_ids(&body) {
                let names = index.entry(id).or_default();
                if !names.contains(&entry.name) {
                    names.push(entry.name.clone());
                }
            }
        }
    }
    index
}

/// Apply a correction to one evidence record (suppress / unsuppress / delete /
/// re-facet). Backs the evidence-inbox trust surface.
pub async fn post_evidence_correct_handler(
    api: web::Data<EvidenceApi>,
    req: HttpRequest,
    body: web::Json<EvidenceCorrectRequest>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(resp) => return resp,
    };
    if body.agent.trim().is_empty() || body.evidence_id.trim().is_empty() {
        return err_json(
            actix_web::http::StatusCode::BAD_REQUEST,
            "agent and evidence_id are required",
        );
    }

    // Resolve the mutation up front so an unknown action fails before any I/O.
    let action = body.action.trim().to_ascii_lowercase();
    let new_status = match action.as_str() {
        "suppress" => Some(EvidenceStatus::Suppressed),
        "unsuppress" | "restore" => Some(EvidenceStatus::Active),
        "delete" => Some(EvidenceStatus::Deleted),
        "set_facets" => None,
        other => {
            return err_json(
                actix_web::http::StatusCode::BAD_REQUEST,
                format!("unknown action '{other}'"),
            )
        },
    };
    let new_facets: Option<Vec<Facet>> = if action == "set_facets" {
        Some(
            body.facets
                .clone()
                .unwrap_or_default()
                .into_iter()
                .filter(|f| !f.label.trim().is_empty())
                .map(|f| Facet {
                    label: f.label.trim().to_lowercase(),
                    confidence: f.confidence.unwrap_or(1.0).clamp(0.0, 1.0),
                    assigned_by: "user".to_string(),
                })
                .collect(),
        )
    } else {
        None
    };

    let memory = match api
        .memory_resolver
        .resolve_for_scope(&principal, &workspace)
    {
        Ok(memory) => memory,
        Err(err) => return err_json(actix_web::http::StatusCode::BAD_REQUEST, err),
    };
    let corrected_at = chrono::Utc::now().to_rfc3339();
    let found = match memory
        .update_native_evidence(&body.agent, &body.evidence_id, |record| {
            if let Some(status) = new_status {
                record.status = status;
            }
            if let Some(facets) = new_facets {
                record.facets = facets;
            }
            // Stamp the correction time so reviews generated earlier go stale.
            record.last_corrected_at = Some(corrected_at);
        })
        .await
    {
        Ok(found) => found,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    if !found {
        return err_json(
            actix_web::http::StatusCode::NOT_FOUND,
            format!("no evidence record '{}'", body.evidence_id),
        );
    }
    HttpResponse::Ok().json(serde_json::json!({
        "ok": true,
        "evidence_id": body.evidence_id,
        "action": action,
    }))
}

#[derive(Deserialize)]
pub struct EntitiesListQuery {
    pub agent: String,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub facet: Option<String>,
    /// Include deleted / merged-away tombstones (off by default).
    #[serde(default)]
    pub include_deleted: bool,
}

#[derive(Deserialize)]
pub struct EntityCorrectRequest {
    pub agent: String,
    /// The subject anchor key (the absorbed key for `merge`, the merged-away key
    /// for `split`, otherwise the anchor to mutate).
    pub entity_key: String,
    /// One of: `rename` | `merge` | `split` | `suppress` | `unsuppress` | `delete`.
    pub action: String,
    /// New canonical name for `rename`.
    #[serde(default)]
    pub name: Option<String>,
    /// The surviving canonical key for `merge` (the one `entity_key` folds into).
    #[serde(default)]
    pub target_key: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// List an agent's canonical entity anchors (active + suppressed; deleted /
/// merged-away tombstones hidden unless requested).
pub async fn list_entities_handler(
    api: web::Data<EvidenceApi>,
    req: HttpRequest,
    query: web::Query<EntitiesListQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let memory = match api
        .memory_resolver
        .resolve_for_scope(&principal, &workspace)
    {
        Ok(memory) => memory,
        Err(err) => return err_json(actix_web::http::StatusCode::BAD_REQUEST, err),
    };
    let mut records = match memory.load_native_entities(&query.agent).await {
        Ok(records) => records,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    if !query.include_deleted {
        records.retain(|e| e.status != EvidenceStatus::Deleted);
    }
    if let Some(facet) = query
        .facet
        .as_deref()
        .filter(|f| !f.eq_ignore_ascii_case("all"))
    {
        records.retain(|e| e.facets.iter().any(|f| f.label == facet));
    }
    records.sort_by(|a, b| b.last_seen_at.cmp(&a.last_seen_at));
    HttpResponse::Ok().json(serde_json::json!({
        "agent": query.agent,
        "count": records.len(),
        "entities": records,
    }))
}

/// Apply a correction to the entity graph (rename / merge / split / suppress /
/// unsuppress / delete). Merges and splits are durable and reversible.
pub async fn post_entity_correct_handler(
    api: web::Data<EvidenceApi>,
    req: HttpRequest,
    body: web::Json<EntityCorrectRequest>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(resp) => return resp,
    };
    if body.agent.trim().is_empty() || body.entity_key.trim().is_empty() {
        return err_json(
            actix_web::http::StatusCode::BAD_REQUEST,
            "agent and entity_key are required",
        );
    }
    let action = body.action.trim().to_ascii_lowercase();
    if !matches!(
        action.as_str(),
        "rename" | "merge" | "split" | "suppress" | "unsuppress" | "delete"
    ) {
        return err_json(
            actix_web::http::StatusCode::BAD_REQUEST,
            format!("unknown action '{action}'"),
        );
    }
    if action == "merge" && body.target_key.as_deref().unwrap_or("").trim().is_empty() {
        return err_json(
            actix_web::http::StatusCode::BAD_REQUEST,
            "merge requires target_key (the surviving canonical anchor)",
        );
    }

    let memory = match api
        .memory_resolver
        .resolve_for_scope(&principal, &workspace)
    {
        Ok(memory) => memory,
        Err(err) => return err_json(actix_web::http::StatusCode::BAD_REQUEST, err),
    };
    let key = body.entity_key.trim().to_lowercase();
    let name = body.name.clone();
    let target = body.target_key.clone();
    let applied = match memory
        .mutate_native_entities(&body.agent, move |entities| match action.as_str() {
            "merge" => match target {
                Some(canonical) => merge_entities(entities, &canonical, &key),
                None => false,
            },
            "split" => split_entity(entities, &key),
            "rename" => match name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                Some(new_name) => match entities.iter_mut().find(|e| e.entity_key == key) {
                    Some(entity) => {
                        entity.canonical_name = new_name.to_string();
                        entity.user_curated = true;
                        true
                    },
                    None => false,
                },
                None => false,
            },
            other => match entities.iter_mut().find(|e| e.entity_key == key) {
                Some(entity) => {
                    entity.status = match other {
                        "suppress" => EvidenceStatus::Suppressed,
                        "delete" => EvidenceStatus::Deleted,
                        _ => EvidenceStatus::Active,
                    };
                    entity.user_curated = true;
                    true
                },
                None => false,
            },
        })
        .await
    {
        Ok(applied) => applied,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    if !applied {
        return err_json(
            actix_web::http::StatusCode::NOT_FOUND,
            "correction did not apply (anchor not found, or invalid for this action)",
        );
    }
    HttpResponse::Ok().json(serde_json::json!({
        "ok": true,
        "entity_key": body.entity_key,
        "action": body.action.trim().to_ascii_lowercase(),
    }))
}

#[derive(Deserialize)]
pub struct DashboardQuery {
    pub agent: String,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub facet: Option<String>,
    #[serde(default)]
    pub days: Option<i64>,
}

#[derive(Deserialize)]
pub struct DashboardPublishRequest {
    pub agent: String,
    #[serde(default = "default_facet")]
    pub facet: String,
    #[serde(default)]
    pub days: Option<i64>,
    /// Surface route (defaults to `/briefing`, the live published-surface rail).
    #[serde(default)]
    pub route: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Count past review artifacts attributed to one agent in scope.
fn count_reviews_for_agent(
    api: &EvidenceApi,
    principal: &str,
    workspace: &str,
    agent: &str,
) -> usize {
    let store = match open_local_durable_artifacts(&api.workspace_layout, principal, workspace) {
        Ok(store) => store,
        Err(_) => return 0,
    };
    store
        .list(Some(REVIEWS_NAMESPACE))
        .map(|entries| {
            entries
                .iter()
                .filter(|e| {
                    e.frontmatter
                        .as_ref()
                        .and_then(|f| f.source_agent_id.as_deref())
                        == Some(agent)
                })
                .count()
        })
        .unwrap_or(0)
}

/// Assemble the dashboard payload for a scope/agent (facet + optional window).
/// Returns the error response to forward on failure.
async fn assemble_dashboard(
    api: &EvidenceApi,
    principal: &str,
    workspace: &str,
    agent: &str,
    facet: &str,
    days: Option<i64>,
) -> Result<DashboardData, HttpResponse> {
    let memory = api
        .memory_resolver
        .resolve_for_scope(principal, workspace)
        .map_err(|err| err_json(actix_web::http::StatusCode::BAD_REQUEST, err))?;
    let mut records = memory
        .load_scoped_evidence(agent)
        .await
        .map_err(|err| err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err))?;
    let mut entities = memory
        .load_scoped_entities(agent)
        .await
        .map_err(|err| err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err))?;

    let facet_opt = if facet.eq_ignore_ascii_case("all") {
        None
    } else {
        Some(facet)
    };
    if let Some(f) = facet_opt {
        records.retain(|r| r.facets.iter().any(|x| x.label == f));
        entities.retain(|e| e.facets.iter().any(|x| x.label == f));
    }
    if let Some(days) = days {
        let since = chrono::Utc::now() - chrono::Duration::days(days);
        records.retain(|r| {
            chrono::DateTime::parse_from_rfc3339(&r.last_seen_at)
                .map(|t| t.with_timezone(&chrono::Utc) >= since)
                .unwrap_or(true)
        });
    }
    let reviews = count_reviews_for_agent(api, principal, workspace, agent);
    Ok(build_dashboard(
        agent,
        facet,
        days,
        chrono::Utc::now().to_rfc3339(),
        &records,
        &entities,
        reviews,
    ))
}

/// Impact dashboard payload (coverage / visibility / top entities / recent).
pub async fn get_evidence_dashboard_handler(
    api: web::Data<EvidenceApi>,
    req: HttpRequest,
    query: web::Query<DashboardQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let facet = query.facet.as_deref().unwrap_or("all");
    match assemble_dashboard(
        &api,
        &principal,
        &workspace,
        &query.agent,
        facet,
        query.days,
    )
    .await
    {
        Ok(data) => HttpResponse::Ok().json(data),
        Err(resp) => resp,
    }
}

/// Publish the impact dashboard as an artifact-driven surface onto `/briefing`:
/// create an internal task, write the rendered Markdown as its user-output, then
/// publish + materialize the surface. Re-publishing supersedes the prior surface
/// (stable `logical_surface_id`).
pub async fn post_evidence_dashboard_publish_handler(
    api: web::Data<EvidenceApi>,
    service: web::Data<Arc<ArtifactV2Service>>,
    req: HttpRequest,
    body: web::Json<DashboardPublishRequest>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(resp) => return resp,
    };
    if body.agent.trim().is_empty() {
        return err_json(
            actix_web::http::StatusCode::BAD_REQUEST,
            "agent is required",
        );
    }

    let data = match assemble_dashboard(
        &api,
        &principal,
        &workspace,
        &body.agent,
        &body.facet,
        body.days,
    )
    .await
    {
        Ok(data) => data,
        Err(resp) => return resp,
    };
    let markdown = render_dashboard_markdown(&data);
    let title = format!(
        "Impact dashboard — {} · {} record(s)",
        body.facet, data.total_evidence
    );
    let summary = format!(
        "{} evidence · {} entities · {} facets",
        data.total_evidence, data.active_entities, data.facets_tracked
    );

    let scope = ScopeRef::system_internal_unauthenticated(&principal.clone(), &workspace.clone());
    let task = match service
        .create_task(CreateTaskInput {
            principal: principal.clone(),
            workspace: workspace.clone(),
            title: title.clone(),
            description: "Work-evidence impact dashboard".to_string(),
            agent_id: body.agent.clone(),
            goal_id: None,
            ui_thread_id: "general".to_string(),
            priority: None,
            due_date: None,
            tags: Vec::new(),
            created_by: "evidence-dashboard".to_string(),
            depends_on: Vec::new(),
            approved: true,
            schedule: None,
            output_mode: TaskOutputMode::Overwrite,
            chat_session_id: None,
            lifecycle: TaskLifecycle::Internal,
            sync_mode: TaskSyncMode::Deferred,
        })
        .await
    {
        Ok(task) => task,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let task_id = task.manifest.task_id.clone();

    let output = match service
        .write_user_output_direct(
            &scope,
            &task_id,
            WriteUserOutputDirectInput {
                media_type: "text/markdown".to_string(),
                body: WriteUserOutputBody::Text(markdown),
                dashboard_theme: None,
            },
        )
        .await
    {
        Ok(output) => output,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };

    let route = body
        .route
        .clone()
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "/briefing".to_string());
    let input = PublishSurfaceInput {
        task_id: task_id.clone(),
        source_output_id: Some(output.output_id.clone()),
        materialize_as: Some("muij_surface".to_string()),
        // Stable across re-publishes so a fresh dashboard supersedes the old one.
        logical_surface_id: Some(format!("evidence-dashboard:{}:{}", body.agent, body.facet)),
        surface_kind: Some("dashboard".to_string()),
        route: Some(route),
        title: Some(title),
        summary: Some(summary),
        placement: Some(PublishedSurfacePlacement {
            placement_kind: "workspace".to_string(),
            placement_id: Some(workspace.clone()),
            pinned: true,
        }),
    };
    match service.publish_surface_record(&scope, input).await {
        Ok(record) => HttpResponse::Ok().json(serde_json::json!({
            "ok": true,
            "surface_id": record.surface_id,
            "route": record.route,
            "title": record.title,
            "task_id": record.task_id,
        })),
        Err(err) => err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    }
}

/// List past review artifacts in this scope (newest first).
pub async fn list_evidence_reviews_handler(
    api: web::Data<EvidenceApi>,
    req: HttpRequest,
    query: web::Query<ReviewsListQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let store = match open_local_durable_artifacts(&api.workspace_layout, &principal, &workspace) {
        Ok(store) => store,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let entries = match store.list(Some(REVIEWS_NAMESPACE)) {
        Ok(entries) => entries,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };

    // Reverse lineage → staleness: a review is stale when any evidence it cited
    // has since been corrected (suppressed/deleted, or re-faceted after the
    // review was generated). We read each review's body for its cited ids and
    // compare against current evidence, caching evidence per agent.
    let memory = api
        .memory_resolver
        .resolve_for_scope(&principal, &workspace)
        .ok();
    let mut evidence_cache: std::collections::HashMap<String, Vec<EvidenceRecord>> =
        std::collections::HashMap::new();

    let mut reviews: Vec<serde_json::Value> = Vec::with_capacity(entries.len());
    for entry in &entries {
        let agent = entry
            .frontmatter
            .as_ref()
            .and_then(|f| f.source_agent_id.clone());
        let last_updated = entry.frontmatter.as_ref().map(|f| f.last_updated);
        let cited = match store.read(REVIEWS_NAMESPACE, &entry.name).await {
            Ok((_, body)) => extract_cited_ids(&body),
            Err(_) => Vec::new(),
        };

        let mut stale = false;
        if let (Some(agent), Some(last_updated), Some(memory)) =
            (agent.as_ref(), last_updated, memory.as_ref())
        {
            if !cited.is_empty() {
                if !evidence_cache.contains_key(agent) {
                    let loaded = memory.load_scoped_evidence(agent).await.unwrap_or_default();
                    evidence_cache.insert(agent.clone(), loaded);
                }
                let records = evidence_cache.get(agent).expect("just inserted");
                stale = cited.iter().any(|id| {
                    records
                        .iter()
                        .find(|e| &e.evidence_id == id)
                        .map(|e| {
                            e.status != EvidenceStatus::Active
                                || e.last_corrected_at
                                    .as_deref()
                                    .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                                    .map(|t| t.with_timezone(&chrono::Utc) > last_updated)
                                    .unwrap_or(false)
                        })
                        .unwrap_or(false)
                });
            }
        }

        reviews.push(serde_json::json!({
            "namespace": entry.namespace,
            "name": entry.name,
            "agent": agent,
            "last_updated": last_updated.map(|t| t.to_rfc3339()),
            "cited_count": cited.len(),
            "stale": stale,
        }));
    }
    reviews.sort_by(|a, b| b["last_updated"].as_str().cmp(&a["last_updated"].as_str()));
    HttpResponse::Ok().json(serde_json::json!({ "reviews": reviews }))
}

// ─── Review acceptance feedback + utility metric (WEG eval depth) ────────────

/// Durable-artifact namespace + ledger name for review acceptance feedback.
const REVIEW_FEEDBACK_NAMESPACE: &str = "evidence-review-feedback";
const REVIEW_FEEDBACK_NAME: &str = "feedback.json";
/// `edited` reviews with an `edit_ratio` at or below this count as "light" (still
/// useful). Caller-supplied ratio; absent ratio is treated as light.
const LIGHT_EDIT_THRESHOLD: f64 = 0.3;

async fn load_feedback_ledger(store: &DurableArtifactStore) -> ReviewFeedbackLedger {
    match store
        .read(REVIEW_FEEDBACK_NAMESPACE, REVIEW_FEEDBACK_NAME)
        .await
    {
        Ok((_, body)) => serde_json::from_str(&body).unwrap_or_default(),
        Err(_) => ReviewFeedbackLedger::default(),
    }
}

#[derive(Deserialize)]
pub struct ReviewFeedbackBody {
    /// The review's durable-artifact name (from a `POST /evidence/review` or
    /// `GET /evidence/reviews` response).
    pub review: String,
    pub verdict: ReviewVerdict,
    #[serde(default)]
    pub edit_ratio: Option<f64>,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// `POST /evidence/review/feedback` — record one acceptance signal for a
/// generated review (the utility metric's intake). Appends to a scoped ledger;
/// the latest feedback per review wins when aggregated.
pub async fn post_evidence_review_feedback_handler(
    api: web::Data<EvidenceApi>,
    req: HttpRequest,
    body: web::Json<ReviewFeedbackBody>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(resp) => return resp,
    };
    if body.review.trim().is_empty() {
        return err_json(
            actix_web::http::StatusCode::BAD_REQUEST,
            "`review` (the review artifact name) is required",
        );
    }
    let store = match open_local_durable_artifacts(&api.workspace_layout, &principal, &workspace) {
        Ok(store) => store,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let now = chrono::Utc::now();
    let mut ledger = load_feedback_ledger(&store).await;
    ledger.feedback.push(ReviewFeedback {
        review: body.review.trim().to_string(),
        verdict: body.verdict,
        edit_ratio: body.edit_ratio,
        recorded_at: now.to_rfc3339(),
    });
    let serialized = match serde_json::to_string_pretty(&ledger) {
        Ok(s) => s,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let frontmatter = DurableFrontmatter {
        namespace: REVIEW_FEEDBACK_NAMESPACE.to_string(),
        name: REVIEW_FEEDBACK_NAME.to_string(),
        created_by: "evidence-review-feedback".to_string(),
        last_updated_by: "evidence-review-feedback".to_string(),
        last_updated: now,
        content_type: Some("application/json".to_string()),
        source_execution_id: None,
        source_task_id: None,
        source_workflow_instance_id: None,
        source_run_id: None,
        source_cycle_id: None,
        source_agent_id: None,
        producer_stage: Some("evidence_review_feedback".to_string()),
    };
    if let Err(err) = store
        .write(
            REVIEW_FEEDBACK_NAMESPACE,
            REVIEW_FEEDBACK_NAME,
            &serialized,
            frontmatter,
        )
        .await
    {
        return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err);
    }
    HttpResponse::Ok().json(serde_json::json!({
        "ok": true,
        "recorded": ledger.feedback.len(),
    }))
}

/// `GET /evidence/utility` — the utility metric over recorded review feedback
/// (accepted + lightly-edited / distinct reviews with feedback).
pub async fn get_evidence_utility_handler(
    api: web::Data<EvidenceApi>,
    req: HttpRequest,
    query: web::Query<ReviewsListQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let store = match open_local_durable_artifacts(&api.workspace_layout, &principal, &workspace) {
        Ok(store) => store,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let ledger = load_feedback_ledger(&store).await;
    HttpResponse::Ok().json(utility(&ledger.feedback, LIGHT_EDIT_THRESHOLD))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_page_window_defaults_to_bounded_first_page() {
        let (limit, offset, has_more) = evidence_page_window(123, None, None);

        assert_eq!(limit, DEFAULT_EVIDENCE_LIST_LIMIT);
        assert_eq!(offset, 0);
        assert!(has_more);
    }

    #[test]
    fn evidence_page_window_clamps_limit_and_offset() {
        let (limit, offset, has_more) = evidence_page_window(75, Some(999), Some(500));

        assert_eq!(limit, MAX_EVIDENCE_LIST_LIMIT);
        assert_eq!(offset, 75);
        assert!(!has_more);
    }

    #[test]
    fn evidence_page_window_treats_zero_limit_as_default() {
        let (limit, offset, has_more) = evidence_page_window(40, Some(0), Some(10));

        assert_eq!(limit, DEFAULT_EVIDENCE_LIST_LIMIT);
        assert_eq!(offset, 10);
        assert!(!has_more);
    }

    #[test]
    fn evidence_page_window_reports_more_after_partial_page() {
        let (limit, offset, has_more) = evidence_page_window(80, Some(25), Some(50));

        assert_eq!(limit, 25);
        assert_eq!(offset, 50);
        assert!(has_more);
    }
}

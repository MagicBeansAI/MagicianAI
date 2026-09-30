//! Slice-4 decision-ledger and verified-impression HTTP boundary.
//!
//! Returning a card is intentionally not an impression. The web client calls
//! the visibility endpoint only after the configured dwell rule is satisfied
//! or as cumulative dwell advances; the store independently verifies the
//! exact selected decision item, revision, and served surface.

use actix_web::{http::StatusCode, web, HttpRequest, HttpResponse};
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use magician::magician_v2::attention::learning::{
    deserialize_semantic_envelope, AttentionActionabilityTrainingWorker, AttentionImpressionError,
    AttentionLearningService, AttentionRankRecomputePauseReason, AttentionRankRecomputeQueueCounts,
    AttentionRankRecomputeRunReport, AttentionSurface, RecordAttentionImpression,
    SemanticExtractionCheckpoint, SemanticExtractionContract, SemanticExtractionWorkItem,
    SemanticExtractionWorkStatus, TrainingOutcome, ATTENTION_DECISION_ID_MAX_CHARS,
    ATTENTION_RANK_RECOMPUTE_SCHEMA_VERSION,
};
use magician::magician_v2::attention::resurfacing::store::ResurfacingStore;
use magician_comms::channel_assist::attention_learning::historical_bootstrap::{
    HISTORICAL_BOOTSTRAP_SOURCES, LIVE_MAIL_FEEDBACK_REPAIR_SOURCE,
    LIVE_WORTH_FEEDBACK_REPAIR_SOURCE,
};
use magician_comms::channel_assist::attention_learning::{
    AttentionRankRecomputeWorker, SemanticExtractionPauseReason, SemanticExtractionWorker,
};
use magician_comms::channel_assist::channel::ChannelAssistStore;

use crate::scope::resolve_required_scope;

#[derive(Debug, Deserialize)]
pub struct AttentionLearningScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttentionRankRecomputeWorkerHealth {
    pub batch_size: usize,
    pub concurrency: usize,
    pub interval_secs: u64,
    pub max_retries: u32,
    pub lease_secs: u64,
    pub retention_days: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttentionRankRecomputeHealth {
    pub schema_version: u32,
    pub enabled: bool,
    pub paused: bool,
    pub pause_reason: Option<AttentionRankRecomputePauseReason>,
    pub queue: AttentionRankRecomputeQueueCounts,
    pub worker: AttentionRankRecomputeWorkerHealth,
}

pub async fn get_attention_historical_bootstrap_status_handler(
    learning: web::Data<AttentionLearningService>,
    req: HttpRequest,
    query: web::Query<AttentionLearningScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let mut checkpoints = Vec::new();
    for source in HISTORICAL_BOOTSTRAP_SOURCES {
        match learning
            .store()
            .historical_bootstrap_checkpoint(&principal, &workspace, source)
            .await
        {
            Ok(Some(checkpoint)) => checkpoints.push(checkpoint),
            Ok(None) => {},
            Err(error) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "historical_bootstrap_health_failed",
                    &error.to_string(),
                )
            },
        }
    }
    let config = learning.historical_bootstrap_config();
    let completed = checkpoints.len() == HISTORICAL_BOOTSTRAP_SOURCES.len()
        && checkpoints.iter().all(|checkpoint| checkpoint.completed);
    let mut repair_tails = Vec::new();
    for source in [
        LIVE_MAIL_FEEDBACK_REPAIR_SOURCE,
        LIVE_WORTH_FEEDBACK_REPAIR_SOURCE,
    ] {
        match learning
            .store()
            .historical_bootstrap_checkpoint(&principal, &workspace, source)
            .await
        {
            Ok(Some(checkpoint)) => repair_tails.push(checkpoint),
            Ok(None) => {},
            Err(error) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "historical_bootstrap_health_failed",
                    &error.to_string(),
                )
            },
        }
    }
    let embedding_bind_queue = match learning
        .store()
        .embedding_bind_queue_counts(&principal, &workspace)
        .await
    {
        Ok(count) => count,
        Err(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "historical_bootstrap_health_failed",
                &error.to_string(),
            )
        },
    };
    HttpResponse::Ok().json(serde_json::json!({
        "schema_version": 3,
        "enabled": config.enabled,
        "completed": completed,
        "batch_size": config.batch_size,
        "interval_secs": config.interval_secs,
        "checkpoints": checkpoints,
        "repair_tails": repair_tails,
        "pending_embedding_binds": embedding_bind_queue.pending
            + embedding_bind_queue.in_flight
            + embedding_bind_queue.retry,
        "embedding_bind_queue": embedding_bind_queue,
    }))
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttentionRankRecomputeAdminBody {
    #[serde(default)]
    pub apply: bool,
}

pub async fn get_attention_rank_recompute_job_handler(
    learning: web::Data<AttentionLearningService>,
    req: HttpRequest,
    query: web::Query<AttentionLearningScopeQuery>,
    path: web::Path<String>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let job_id = path.into_inner();
    if job_id.trim().is_empty()
        || job_id.chars().count() > ATTENTION_DECISION_ID_MAX_CHARS
        || job_id.chars().any(char::is_control)
    {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_rank_recompute_job_id",
            "job_id is invalid",
        );
    }
    match learning
        .store()
        .get_rank_recompute_job(&principal, &workspace, &job_id)
        .await
    {
        Ok(Some(job)) => HttpResponse::Ok().json(serde_json::json!({
            "schema_version": ATTENTION_RANK_RECOMPUTE_SCHEMA_VERSION,
            "job": job,
        })),
        Ok(None) => error_response(
            StatusCode::NOT_FOUND,
            "rank_recompute_job_not_found",
            "rank recompute job was not found in this scope",
        ),
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "rank_recompute_job_read_failed",
            &error.to_string(),
        ),
    }
}

pub async fn get_attention_rank_recompute_status_handler(
    learning: web::Data<AttentionLearningService>,
    worker: web::Data<AttentionRankRecomputeWorker>,
    req: HttpRequest,
    query: web::Query<AttentionLearningScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let queue = match learning
        .store()
        .rank_recompute_queue_counts(&principal, &workspace)
        .await
    {
        Ok(queue) => queue,
        Err(error) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "rank_recompute_health_failed",
                &error.to_string(),
            )
        },
    };
    let config = worker.config();
    let pause_reason = worker.pause_reason();
    HttpResponse::Ok().json(AttentionRankRecomputeHealth {
        schema_version: ATTENTION_RANK_RECOMPUTE_SCHEMA_VERSION,
        enabled: config.enabled,
        paused: pause_reason.is_some(),
        pause_reason,
        queue,
        worker: AttentionRankRecomputeWorkerHealth {
            batch_size: config.batch_size,
            concurrency: config.concurrency,
            interval_secs: config.interval_secs,
            max_retries: config.max_retries,
            lease_secs: config.lease_secs,
            retention_days: config.retention_days,
        },
    })
}

pub async fn post_attention_rank_recompute_schedule_handler(
    worker: web::Data<AttentionRankRecomputeWorker>,
    req: HttpRequest,
    query: web::Query<AttentionLearningScopeQuery>,
    body: web::Json<AttentionRankRecomputeAdminBody>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match worker
        .schedule_missing_once(
            &principal,
            &workspace,
            chrono::Utc::now().timestamp_millis(),
            body.apply,
        )
        .await
    {
        Ok(report) => HttpResponse::Ok().json(serde_json::json!({
            "schema_version": ATTENTION_RANK_RECOMPUTE_SCHEMA_VERSION,
            "status": if body.apply { "scheduled" } else { "preview" },
            "apply": body.apply,
            "report": report,
        })),
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "rank_recompute_schedule_failed",
            &error.to_string(),
        ),
    }
}

pub async fn post_attention_rank_recompute_requeue_handler(
    worker: web::Data<AttentionRankRecomputeWorker>,
    req: HttpRequest,
    query: web::Query<AttentionLearningScopeQuery>,
    body: web::Json<AttentionRankRecomputeAdminBody>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match worker
        .requeue_wrongly_staled_once(
            &principal,
            &workspace,
            chrono::Utc::now().timestamp_millis(),
            body.apply,
        )
        .await
    {
        Ok(report) => HttpResponse::Ok().json(serde_json::json!({
            "schema_version": ATTENTION_RANK_RECOMPUTE_SCHEMA_VERSION,
            "status": if body.apply { "requeued" } else { "preview" },
            "apply": body.apply,
            "report": report,
        })),
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "rank_recompute_requeue_failed",
            &error.to_string(),
        ),
    }
}

pub async fn post_attention_rank_recompute_process_handler(
    worker: web::Data<AttentionRankRecomputeWorker>,
    req: HttpRequest,
    query: web::Query<AttentionLearningScopeQuery>,
    body: web::Json<AttentionRankRecomputeAdminBody>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let report = if body.apply {
        worker
            .run_once(
                &principal,
                &workspace,
                chrono::Utc::now().timestamp_millis(),
            )
            .await
    } else {
        Ok(AttentionRankRecomputeRunReport::default())
    };
    match report {
        Ok(report) => HttpResponse::Ok().json(serde_json::json!({
            "schema_version": ATTENTION_RANK_RECOMPUTE_SCHEMA_VERSION,
            "status": if body.apply { "processed" } else { "preview" },
            "apply": body.apply,
            "report": report,
        })),
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "rank_recompute_process_failed",
            &error.to_string(),
        ),
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SemanticExtractionHealthCounts {
    pub active_total: u64,
    /// Active sources outside this communication extractor's contract.
    pub not_applicable: u64,
    pub compatible_revision: u64,
    pub succeeded: u64,
    pub missing: u64,
    pub invalid: u64,
    pub pending: u64,
    pub in_flight: u64,
    pub retry: u64,
    pub dead: u64,
    pub coverage: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticExtractionHealthContract {
    pub semantic_schema_version: u32,
    pub extractor_contract: String,
    pub prompt_version: String,
    pub model: Option<String>,
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SemanticExtractionHealthCheckpoint {
    pub cursor: Option<String>,
    pub updated_at: Option<i64>,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticExtractionHealthSurfaces {
    pub follow_up: SemanticExtractionHealthCounts,
    pub worth_a_look: SemanticExtractionHealthCounts,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SemanticExtractionHealthQueue {
    pub pending: u64,
    pub in_flight: u64,
    pub active_in_flight: u64,
    pub expired_in_flight: u64,
    pub retry: u64,
    pub dead: u64,
    pub next_retry_at: Option<i64>,
    pub oldest_ready_at: Option<i64>,
    pub last_succeeded_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticExtractionHealth {
    pub schema_version: u32,
    pub enabled: bool,
    pub paused: bool,
    pub pause_reason: Option<String>,
    pub degradation_reason: Option<String>,
    pub contract: SemanticExtractionHealthContract,
    pub checkpoint: SemanticExtractionHealthCheckpoint,
    pub totals: SemanticExtractionHealthCounts,
    pub surfaces: SemanticExtractionHealthSurfaces,
    pub queue: SemanticExtractionHealthQueue,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticExtractionEnqueueBody {
    /// False (default) is a read-only bounded preview. True schedules durable
    /// work but still never leases work, activates config, or calls a model.
    #[serde(default)]
    pub apply: bool,
}

pub async fn get_semantic_extraction_health_handler(
    learning: web::Data<AttentionLearningService>,
    worker: web::Data<SemanticExtractionWorker>,
    mail: web::Data<ChannelAssistStore>,
    resurfacing: web::Data<ResurfacingStore>,
    req: HttpRequest,
    query: web::Query<AttentionLearningScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match build_semantic_extraction_health(
        &learning,
        &worker,
        &mail,
        &resurfacing,
        &principal,
        &workspace,
    )
    .await
    {
        Ok(health) => HttpResponse::Ok().json(serde_json::json!({
            "semantic_extraction_health": health,
        })),
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "semantic_extraction_health_failed",
            &error.to_string(),
        ),
    }
}

pub async fn post_semantic_extraction_enqueue_handler(
    worker: web::Data<SemanticExtractionWorker>,
    req: HttpRequest,
    query: web::Query<AttentionLearningScopeQuery>,
    body: web::Json<SemanticExtractionEnqueueBody>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let now = chrono::Utc::now().timestamp_millis();
    match worker
        .schedule_missing_once(&principal, &workspace, now, body.apply)
        .await
    {
        Ok(report) => HttpResponse::Ok().json(serde_json::json!({
            "status": if body.apply { "scheduled" } else { "preview" },
            "apply": body.apply,
            "eligible": report.discovered,
            "model_calls": 0,
            "activation_changed": false,
        })),
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "semantic_extraction_enqueue_failed",
            &error.to_string(),
        ),
    }
}

pub async fn post_attention_impression_handler(
    learning: web::Data<AttentionLearningService>,
    req: HttpRequest,
    query: web::Query<AttentionLearningScopeQuery>,
    body: web::Json<RecordAttentionImpression>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match learning
        .record_verified_impression(&principal, &workspace, &body)
        .await
    {
        Ok(receipt) => HttpResponse::Ok().json(receipt),
        Err(error) => impression_error_response(error),
    }
}

pub async fn get_attention_decision_handler(
    learning: web::Data<AttentionLearningService>,
    req: HttpRequest,
    query: web::Query<AttentionLearningScopeQuery>,
    path: web::Path<String>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let decision_id = path.into_inner();
    if decision_id.trim().is_empty()
        || decision_id.chars().count() > ATTENTION_DECISION_ID_MAX_CHARS
        || decision_id.chars().any(char::is_control)
    {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_decision_id",
            &format!(
                "decision_id must contain 1..={ATTENTION_DECISION_ID_MAX_CHARS} characters without controls"
            ),
        );
    }
    match learning
        .get_routing_decision(&principal, &workspace, &decision_id)
        .await
    {
        Ok(Some(detail)) => HttpResponse::Ok().json(detail),
        Ok(None) => error_response(
            StatusCode::NOT_FOUND,
            "decision_not_found",
            "attention decision was not found in this scope",
        ),
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "decision_read_failed",
            &error.to_string(),
        ),
    }
}

fn impression_error_response(error: AttentionImpressionError) -> HttpResponse {
    match error {
        AttentionImpressionError::InvalidRequest(message) => {
            error_response(StatusCode::BAD_REQUEST, "invalid_impression", &message)
        },
        AttentionImpressionError::VisibilityRuleMismatch => error_response(
            StatusCode::BAD_REQUEST,
            "visibility_rule_mismatch",
            "visibility_rule_version is not active",
        ),
        AttentionImpressionError::DecisionItemNotFound => error_response(
            StatusCode::NOT_FOUND,
            "decision_item_not_found",
            "the scoped decision item was not found",
        ),
        AttentionImpressionError::DecisionItemMismatch => error_response(
            StatusCode::CONFLICT,
            "decision_item_mismatch",
            "the revision, served surface, or selected state does not match",
        ),
        AttentionImpressionError::DeliveryItemNotFound => error_response(
            StatusCode::NOT_FOUND,
            "delivery_item_not_found",
            "the scoped delivered card exposure was not found",
        ),
        AttentionImpressionError::DeliveryBindingMismatch => error_response(
            StatusCode::CONFLICT,
            "delivery_binding_mismatch",
            "the delivery, page, position, revision, or exposure token does not match",
        ),
        AttentionImpressionError::EventIdentityConflict => error_response(
            StatusCode::CONFLICT,
            "event_identity_conflict",
            "event_id is already bound to another impression identity",
        ),
        AttentionImpressionError::Storage(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "impression_persist_failed",
            &error.to_string(),
        ),
    }
}

pub(crate) async fn build_semantic_extraction_health(
    learning: &AttentionLearningService,
    worker: &SemanticExtractionWorker,
    mail: &ChannelAssistStore,
    resurfacing: &ResurfacingStore,
    principal: &str,
    workspace: &str,
) -> anyhow::Result<SemanticExtractionHealth> {
    let work = learning
        .semantic_extraction_work_items(principal, workspace)
        .await?;
    let queue_counts = learning
        .semantic_extraction_queue_counts(principal, workspace)
        .await?;
    let checkpoints = learning
        .semantic_extraction_checkpoints(principal, workspace)
        .await?;
    let work_by_candidate = work
        .iter()
        .map(|item| ((item.surface, item.candidate_id.as_str()), item))
        .collect::<HashMap<_, _>>();
    let contract = worker.contract();

    let mut follow_up = SemanticExtractionHealthCounts::default();
    let mut cursor: Option<String> = None;
    loop {
        let page = mail
            .list_active_semantic_annotations(principal, workspace, cursor.as_deref(), 500)
            .await?;
        if page.is_empty() {
            break;
        }
        for annotation in &page {
            let Some(revision) = annotation.classification_input_revision else {
                classify_health_item_without_revision(&mut follow_up);
                continue;
            };
            let source_revision = format!("distill:{revision}");
            classify_health_item(
                &mut follow_up,
                annotation.semantic_features.as_ref(),
                &source_revision,
                revision,
                &contract,
                work_by_candidate
                    .get(&(AttentionSurface::FollowUp, annotation.id.as_str()))
                    .copied(),
            );
        }
        cursor = page.last().map(|item| item.id.clone());
        if page.len() < 500 {
            break;
        }
    }
    finish_health_counts(&mut follow_up);

    let mut worth = SemanticExtractionHealthCounts::default();
    let mut cursor: Option<String> = None;
    loop {
        let page = resurfacing
            .list_active_semantic_candidates(principal, workspace, cursor.as_deref(), 500)
            .await?;
        if page.is_empty() {
            break;
        }
        for candidate in &page {
            if !SemanticExtractionWorker::supports_worth_source(&candidate.source_kind) {
                worth.active_total += 1;
                worth.not_applicable += 1;
                continue;
            }
            let Some(source_revision) = candidate.content_revision.as_deref() else {
                classify_health_item_without_revision(&mut worth);
                continue;
            };
            classify_health_item(
                &mut worth,
                candidate.semantic_features.as_ref(),
                source_revision,
                source_revision.parse().unwrap_or(0),
                &contract,
                work_by_candidate
                    .get(&(
                        AttentionSurface::WorthALook,
                        candidate.candidate_id.as_str(),
                    ))
                    .copied(),
            );
        }
        cursor = page.last().map(|item| item.candidate_id.clone());
        if page.len() < 500 {
            break;
        }
    }
    finish_health_counts(&mut worth);

    let mut totals = sum_health_counts(&follow_up, &worth);
    finish_health_counts(&mut totals);
    let checkpoint = aggregate_checkpoint(&checkpoints);
    let pause_reason = worker.pause_reason();
    let degradation_reason = semantic_extraction_degradation_reason(
        pause_reason,
        queue_counts.expired_in_flight,
        &totals,
    )
    .map(str::to_owned);
    Ok(SemanticExtractionHealth {
        schema_version: 1,
        enabled: worker.pause_reason() != Some(SemanticExtractionPauseReason::Disabled),
        paused: pause_reason.is_some(),
        pause_reason: pause_reason.map(|reason| reason.as_str().to_string()),
        degradation_reason,
        contract: SemanticExtractionHealthContract {
            semantic_schema_version: contract.semantic_schema_version,
            extractor_contract: contract.extractor_contract,
            prompt_version: contract.prompt_version,
            model: contract.model,
            profile: contract.profile,
        },
        checkpoint,
        totals,
        surfaces: SemanticExtractionHealthSurfaces {
            follow_up,
            worth_a_look: worth,
        },
        queue: SemanticExtractionHealthQueue {
            pending: queue_counts.pending,
            in_flight: queue_counts.in_flight,
            active_in_flight: queue_counts.active_in_flight,
            expired_in_flight: queue_counts.expired_in_flight,
            retry: queue_counts.retry,
            dead: queue_counts.dead,
            next_retry_at: queue_counts.next_retry_at,
            oldest_ready_at: queue_counts.oldest_ready_at,
            last_succeeded_at: queue_counts.last_succeeded_at,
        },
    })
}

// Queue totals also include retired candidates and superseded extractors.
// Only the active cohort can degrade current coverage; historical failures
// remain visible in queue.dead for diagnosis.
fn semantic_extraction_degradation_reason(
    pause_reason: Option<SemanticExtractionPauseReason>,
    expired_leases: u64,
    totals: &SemanticExtractionHealthCounts,
) -> Option<&'static str> {
    if pause_reason == Some(SemanticExtractionPauseReason::ModelUnavailable) {
        Some("semantic_extractor_unavailable")
    } else if expired_leases > 0 {
        Some("semantic_extraction_expired_leases")
    } else if totals.dead > 0 {
        Some("semantic_extraction_dead_letter")
    } else if totals.invalid > 0 {
        Some("semantic_extraction_invalid_features")
    } else if totals.missing > 0 {
        Some("semantic_extraction_source_missing")
    } else {
        None
    }
}

fn classify_health_item(
    counts: &mut SemanticExtractionHealthCounts,
    raw_envelope: Option<&serde_json::Value>,
    source_revision: &str,
    source_revision_number: i64,
    contract: &SemanticExtractionContract,
    work: Option<&SemanticExtractionWorkItem>,
) {
    counts.active_total += 1;
    let mut source_status = None;
    if let Some(envelope) = deserialize_semantic_envelope(raw_envelope) {
        let revision_matches = envelope.input_revision == source_revision_number
            && match envelope.source_revision.as_deref() {
                Some(revision) => revision == source_revision,
                None => source_revision == format!("distill:{source_revision_number}"),
            };
        let producer_matches = envelope.schema_version == contract.semantic_schema_version
            && envelope.extractor_contract == contract.extractor_contract
            && envelope.prompt_version == contract.prompt_version
            && envelope.model == contract.model
            && envelope.profile == contract.profile;
        if revision_matches && producer_matches {
            source_status = Some(envelope.status);
            match envelope.status {
                magician::magician_v2::attention::learning::SemanticExtractionStatus::Succeeded => {
                    counts.succeeded += 1;
                    counts.compatible_revision += 1;
                },
                _ => {},
            }
            if envelope.status
                == magician::magician_v2::attention::learning::SemanticExtractionStatus::Succeeded
            {
                return;
            }
        }
    }
    let current_work = work.filter(|item| {
        item.source_revision == source_revision
            && item.source_revision_number == source_revision_number
            && &item.contract == contract
    });
    match current_work.map(|item| item.status) {
        Some(SemanticExtractionWorkStatus::InFlight) => counts.in_flight += 1,
        Some(SemanticExtractionWorkStatus::Retry) => counts.retry += 1,
        Some(SemanticExtractionWorkStatus::Dead) => counts.dead += 1,
        // A terminal queue receipt without a matching source envelope is
        // conservatively uncovered and eligible for idempotent rescheduling.
        Some(SemanticExtractionWorkStatus::Missing) => counts.missing += 1,
        Some(SemanticExtractionWorkStatus::Invalid) => counts.invalid += 1,
        Some(SemanticExtractionWorkStatus::Pending) => counts.pending += 1,
        Some(SemanticExtractionWorkStatus::Succeeded) | None => match source_status {
            Some(magician::magician_v2::attention::learning::SemanticExtractionStatus::Invalid) => {
                counts.invalid += 1
            },
            Some(magician::magician_v2::attention::learning::SemanticExtractionStatus::Missing) => {
                counts.missing += 1
            },
            _ => counts.pending += 1,
        },
    }
}

fn classify_health_item_without_revision(counts: &mut SemanticExtractionHealthCounts) {
    counts.active_total += 1;
    counts.missing += 1;
}

fn finish_health_counts(counts: &mut SemanticExtractionHealthCounts) {
    let applicable = counts.active_total.saturating_sub(counts.not_applicable);
    counts.coverage = if applicable == 0 {
        1.0
    } else {
        counts.compatible_revision as f64 / applicable as f64
    };
}

fn sum_health_counts(
    left: &SemanticExtractionHealthCounts,
    right: &SemanticExtractionHealthCounts,
) -> SemanticExtractionHealthCounts {
    SemanticExtractionHealthCounts {
        active_total: left.active_total + right.active_total,
        not_applicable: left.not_applicable + right.not_applicable,
        compatible_revision: left.compatible_revision + right.compatible_revision,
        succeeded: left.succeeded + right.succeeded,
        missing: left.missing + right.missing,
        invalid: left.invalid + right.invalid,
        pending: left.pending + right.pending,
        in_flight: left.in_flight + right.in_flight,
        retry: left.retry + right.retry,
        dead: left.dead + right.dead,
        coverage: 0.0,
    }
}

fn aggregate_checkpoint(
    checkpoints: &[SemanticExtractionCheckpoint],
) -> SemanticExtractionHealthCheckpoint {
    let Some(latest) = checkpoints
        .iter()
        .max_by_key(|checkpoint| checkpoint.updated_at)
    else {
        return SemanticExtractionHealthCheckpoint::default();
    };
    SemanticExtractionHealthCheckpoint {
        cursor: latest
            .cursor
            .as_ref()
            .map(|cursor| format!("{}:{cursor}", latest.surface.as_str())),
        updated_at: Some(latest.updated_at),
        lease_owner: latest.lease_owner.clone(),
        lease_expires_at: latest.lease_expires_at,
    }
}

pub async fn get_actionability_training_status_handler(
    learning: web::Data<AttentionLearningService>,
    req: HttpRequest,
    query: web::Query<AttentionLearningScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match learning
        .actionability_training_status(&principal, &workspace)
        .await
    {
        Ok(status) => HttpResponse::Ok().json(serde_json::json!({
            "schema_version": 1,
            "actionability_training": status,
        })),
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "actionability_training_status_failed",
            &error.to_string(),
        ),
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionabilityTrainingRunBody {
    /// False trains and writes an artifact only. True also installs, forcing
    /// the first scope install to shadow.
    #[serde(default)]
    pub apply: bool,
}

pub async fn get_routing_training_status_handler(
    learning: web::Data<AttentionLearningService>,
    req: HttpRequest,
    query: web::Query<AttentionLearningScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match learning
        .routing_training_status(&principal, &workspace)
        .await
    {
        Ok(status) => HttpResponse::Ok().json(serde_json::json!({
            "schema_version": 1,
            "routing_training": status,
        })),
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "routing_training_status_failed",
            &error.to_string(),
        ),
    }
}

pub async fn post_routing_training_run_handler(
    worker: web::Data<AttentionActionabilityTrainingWorker>,
    learning: web::Data<AttentionLearningService>,
    req: HttpRequest,
    query: web::Query<AttentionLearningScopeQuery>,
    body: web::Json<ActionabilityTrainingRunBody>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match worker
        .run_routing_scope(&principal, &workspace, body.apply)
        .await
    {
        Ok(outcome) => {
            let status = learning
                .routing_training_status(&principal, &workspace)
                .await
                .ok();
            let (run_status, reason, snapshot_id) = match &outcome {
                TrainingOutcome::Refused { reason, .. } => ("refused", Some(reason.as_str()), None),
                TrainingOutcome::Written { snapshot_id, .. } => {
                    ("written", None, Some(snapshot_id.as_str()))
                },
            };
            HttpResponse::Ok().json(serde_json::json!({
                "schema_version": 1,
                "status": run_status,
                "reason": reason,
                "snapshot_id": snapshot_id,
                "apply": body.apply,
                "activation_changed": false,
                "routing_training": status,
            }))
        },
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "routing_training_run_failed",
            &error.to_string(),
        ),
    }
}

pub async fn post_actionability_training_run_handler(
    worker: web::Data<AttentionActionabilityTrainingWorker>,
    learning: web::Data<AttentionLearningService>,
    req: HttpRequest,
    query: web::Query<AttentionLearningScopeQuery>,
    body: web::Json<ActionabilityTrainingRunBody>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match worker
        .run_scope_with_install(&principal, &workspace, body.apply)
        .await
    {
        Ok(outcome) => {
            let status = learning
                .actionability_training_status(&principal, &workspace)
                .await
                .ok();
            let (run_status, reason, snapshot_id) = match &outcome {
                TrainingOutcome::Refused { reason, .. } => ("refused", Some(reason.as_str()), None),
                TrainingOutcome::Written { snapshot_id, .. } => {
                    ("written", None, Some(snapshot_id.as_str()))
                },
            };
            HttpResponse::Ok().json(serde_json::json!({
                "schema_version": 1,
                "status": run_status,
                "reason": reason,
                "snapshot_id": snapshot_id,
                "apply": body.apply,
                "activation_changed": false,
                "actionability_training": status,
            }))
        },
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "actionability_training_run_failed",
            &error.to_string(),
        ),
    }
}

fn error_response(status: StatusCode, code: &str, message: &str) -> HttpResponse {
    HttpResponse::build(status).json(serde_json::json!({
        "error": {
            "code": code,
            "message": message,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attention_recovery_coverage_excludes_unsupported_sources_without_hiding_them() {
        let mut counts = SemanticExtractionHealthCounts {
            active_total: 13,
            not_applicable: 3,
            succeeded: 10,
            compatible_revision: 10,
            ..Default::default()
        };
        finish_health_counts(&mut counts);
        assert_eq!(counts.coverage, 1.0);
        assert_eq!(
            semantic_extraction_degradation_reason(None, 0, &counts),
            None
        );
        assert_eq!(
            counts.active_total,
            counts.succeeded + counts.not_applicable
        );
    }

    #[test]
    fn attention_recovery_health_reports_repair_instead_of_the_old_invalid_envelope() {
        use magician::magician_v2::attention::learning::{
            ChannelAttentionSemanticEnvelope, SemanticExtractorIdentity,
        };
        let identity = SemanticExtractorIdentity {
            model: Some("luna".to_owned()),
            profile: Some("remote".to_owned()),
        };
        let invalid = ChannelAttentionSemanticEnvelope::from_optional_value(
            Some(&serde_json::json!({})),
            1,
            "1.1.0",
            &identity,
        );
        let raw = serde_json::to_value(&invalid).unwrap();
        let contract = SemanticExtractionContract {
            semantic_schema_version: invalid.schema_version,
            extractor_contract: invalid.extractor_contract,
            prompt_version: "1.1.0".to_owned(),
            model: identity.model,
            profile: identity.profile,
        };
        let mut work = SemanticExtractionWorkItem {
            work_id: "work".to_owned(),
            principal: "p".to_owned(),
            workspace: "w".to_owned(),
            surface: AttentionSurface::FollowUp,
            candidate_id: "candidate".to_owned(),
            source_revision: "distill:1".to_owned(),
            source_revision_number: 1,
            contract: contract.clone(),
            status: SemanticExtractionWorkStatus::Pending,
            attempts: 0,
            next_retry_at: None,
            lease_owner: None,
            lease_expires_at: None,
            last_error_code: None,
            created_at: 0,
            updated_at: 1,
        };
        let mut counts = SemanticExtractionHealthCounts::default();
        classify_health_item(
            &mut counts,
            Some(&raw),
            "distill:1",
            1,
            &contract,
            Some(&work),
        );
        assert_eq!(
            (counts.active_total, counts.pending, counts.invalid),
            (1, 1, 0)
        );
        work.status = SemanticExtractionWorkStatus::Dead;
        let mut counts = SemanticExtractionHealthCounts::default();
        classify_health_item(
            &mut counts,
            Some(&raw),
            "distill:1",
            1,
            &contract,
            Some(&work),
        );
        assert_eq!(
            (counts.active_total, counts.dead, counts.invalid),
            (1, 1, 0)
        );
    }

    #[test]
    fn attention_recovery_health_uses_the_active_cohort() {
        let mut active = SemanticExtractionHealthCounts {
            active_total: 10,
            succeeded: 10,
            ..Default::default()
        };
        assert_eq!(
            semantic_extraction_degradation_reason(None, 0, &active),
            None
        );
        active.invalid = 1;
        assert_eq!(
            semantic_extraction_degradation_reason(None, 0, &active),
            Some("semantic_extraction_invalid_features")
        );
        active.dead = 1;
        assert_eq!(
            semantic_extraction_degradation_reason(None, 0, &active),
            Some("semantic_extraction_dead_letter")
        );
        assert_eq!(
            semantic_extraction_degradation_reason(None, 1, &active),
            Some("semantic_extraction_expired_leases")
        );
    }

    #[test]
    fn event_identity_conflict_is_a_stable_conflict_contract() {
        let response = impression_error_response(AttentionImpressionError::EventIdentityConflict);
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn missing_decision_item_is_not_misreported_as_an_impression() {
        let response = impression_error_response(AttentionImpressionError::DecisionItemNotFound);
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn revisionless_active_candidates_are_missing_not_permanently_pending() {
        let mut counts = SemanticExtractionHealthCounts::default();
        classify_health_item_without_revision(&mut counts);
        finish_health_counts(&mut counts);

        assert_eq!(counts.active_total, 1);
        assert_eq!(counts.missing, 1);
        assert_eq!(counts.pending, 0);
        assert_eq!(counts.coverage, 0.0);
    }
}

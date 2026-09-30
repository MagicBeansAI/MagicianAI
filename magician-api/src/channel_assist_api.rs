//! Channel Assist sync + annotation APIs.
//!
//! `/api/magician/v2/channel-assist/*` is the channel-neutral route surface.
//!
//! - `GET  /api/magician/v2/channel-assist/sync/status` — per-account
//!   watermark cursors, thread/message counts, last sync time and last
//!   error (design §3: the failure posture must be visible here).
//! - `POST /api/magician/v2/channel-assist/sync/run` — one manual sync pass
//!   for the scope, the SAME function the background loop runs, returning
//!   the per-account summary (validation + ops; the acceptance runbook
//!   drives sync through this).
//! - `POST /api/magician/v2/channel-assist/distill/backfill?dry_run=` — inspect
//!   or enqueue one bounded, pressure-aware historical brief repair pass.
//! - `GET  /api/magician/v2/channel-assist/annotations?provider=&account=&thread_ids=a,b`
//!   — batch thread summaries + annotation sets per requested thread id
//!   (empty annotation array is a valid answer; batch capped at
//!   [`MAX_ANNOTATION_THREAD_IDS`]).
//! - `POST /api/magician/v2/channel-assist/annotations/{id}/dismiss` —
//!   transition to `dismissed` + audit event (actor=user; never deletes).
//! - `POST /api/magician/v2/channel-assist/annotations/{id}/feedback` —
//!   append a typed [`MailAssistUserFeedback`] audit event.
//! - `POST /api/magician/v2/channel-assist/annotations/seed` — create a
//!   fixture annotation on a thread (creating a seed-origin thread row if
//!   absent) so the UI/extension can render a known annotation before any
//!   classifier exists (parent-plan First Slice #5).
//!
//! Scope-aware in the house style (`resolve_required_scope`). The API
//! shares the [`MailAssistStore`] instance with the background
//! [`sync::ChannelSyncWorker`] so both see one write connection per scope.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

use actix_web::{web, HttpRequest, HttpResponse};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::scope::resolve_required_scope;
use magician::config::{AttentionActionabilityMode, AttentionGroupingMode};
use magician::magician_v2::artifact_v2::models::{
    TaskLifecycle, TaskOutputMode, TaskSyncMode, TaskTagRecord,
};
use magician::magician_v2::artifact_v2::service::CreateTaskInput;
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::artifact_v2::CapabilityWorkspaceManager;
use magician::magician_v2::attention::learning::{
    canonical_universe_digest, deserialize_semantic_envelope, rank_universe_digest,
    singleton_grouping_metadata, ActionabilityExplanation, ActionabilityFeatureInput,
    ActionabilityScoreStatus, AttentionBanditDecisionMetadata, AttentionDecision,
    AttentionDecisionContext, AttentionDecisionItem, AttentionFeedbackReceipt,
    AttentionGroupingMetadata, AttentionGroupingResult, AttentionLabelQuality,
    AttentionLearningService, AttentionOutcomeAttribution, AttentionOutcomeKind,
    AttentionPairCandidateRef, AttentionPairFeedbackReceipt, AttentionPairLabelKind,
    AttentionPairLabelSource, AttentionRankMetadata, AttentionRoute, AttentionRoutingCandidate,
    AttentionRoutingEvaluation, AttentionRoutingHealth, AttentionSurface, GroupingFeatureInput,
    RecordAttentionOutcome, RecordAttentionPairLabel, SemanticAttentionCandidate,
    SemanticExtractionStatus,
};
use magician::magician_v2::attention_funnel::{
    AttentionLane, AttentionScope, AttentionSourceFamily,
};
use magician::magician_v2::attention_lane_facade::{AttentionLanePage, AttentionLaneQuery};
use magician_comms::channel_assist::adapter_registry::{
    self, default_channel_adapters, ChannelActionContext, ChannelActionDescriptor,
    ChannelActionRequest, ChannelCapabilities, ChannelIdentity, GMAIL_PROVIDER,
};
use magician_comms::channel_assist::assist::distill::{
    distill_backfill_cutoff_ms, distill_backfill_runtime_snapshot_json, request_distill_backfill,
    set_distill_backfill_paused, ChannelDistillConfig,
};
use magician_comms::channel_assist::assist::writing_preferences::{
    derive_edit_preferences, normalize_statement, promote_to_memory, remove_from_memory,
    sender_domain,
};
use magician_comms::channel_assist::attention_lane_bridge::list_channel_attention_lane_rows;
use magician_comms::channel_assist::attention_learning::SemanticExtractionWorker;
use magician_comms::channel_assist::channel_observe;
use magician_comms::channel_assist::ingest::IngestContext;
use magician_comms::channel_assist::live_content::{
    ChannelEvidenceResolver, ORIGINAL_BODY_MAX_CHARS, ORIGINAL_RESPONSE_MAX_CHARS,
};
use magician_comms::channel_assist::resurfacing::actions::{
    ResurfacingActionService, ResurfacingActionTargetRef, ResurfacingContextualActionRequest,
};
use magician_comms::channel_assist::store::{
    AnnotationActionClaimResult, AnnotationTransitionFeedback, AnnotationTransitionResult,
    MailAssistStore, NeedsApprovalRow, NeedsApprovalSourceSignal, WritingPreferenceScopeKind,
    WritingPreferenceStatus,
};
use magician_comms::channel_assist::sync::{self, ChannelSyncConfig};
use magician_comms::channel_assist::types::{
    ChannelLane, MailAnnotationState, MailAssistActor, MailAssistUserFeedback, MailFeedbackVerdict,
    MailRecordOrigin, MailThreadAnnotation, MailThreadRecord, MAIL_ASSIST_SCHEMA_VERSION,
};

/// Batch cap for the annotations GET — a Gmail list view asks for at most
/// a page of visible threads, so ~100 covers real callers while bounding
/// per-request store work.
pub const MAX_ANNOTATION_THREAD_IDS: usize = 100;

/// Provenance stamp on seeded fixture annotations (vs the Phase-2
/// classifier run id).
const FIXTURE_SEED_PROVENANCE: &str = "fixture_seed";

/// Subject given to a thread row materialized by `annotations/seed` when
/// the caller doesn't supply one.
const FIXTURE_SEED_SUBJECT: &str = "[fixture] seeded thread";
// The Today-projection latency budget moved behind the assist seam (plan
// 3.1) so the product-lane quality budgets live with the product
// decisions; the value and the env override are unchanged.
use magician_comms::channel_assist::assist::quality_budgets::today_projection_latency_budget_ms;

pub struct ChannelAssistApi {
    workspace_layout: ArtifactV2Workspace,
    store: MailAssistStore,
    /// Task service — used by the Phase-3 approve action to create a
    /// follow-up task linked to the thread.
    artifact_v2_service:
        std::sync::Arc<magician::magician_v2::artifact_v2::service::ArtifactV2Service>,
    /// Operation router — used by the stats endpoint to report which
    /// profile/provider/model each mail op is CONFIGURED to use (never
    /// hardcoded), and by the generic channel-action compose/commit endpoints
    /// to build the [`ChannelActionContext`] the reply-draft op dispatches
    /// through (same local-pinned path the distill/classify workers use).
    operation_router: Option<
        std::sync::Arc<
            magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter,
        >,
    >,
    /// Runtime transport broadcaster — passed into [`ChannelActionContext`] so
    /// the reply-draft op emits `LLMResponseReceived` telemetry just like the
    /// distill/classify workers (the channel-assist ops bypass the executor
    /// layer that normally emits it).
    event_broadcaster:
        Option<std::sync::Arc<magician::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,
}

impl ChannelAssistApi {
    /// The runtime workspace layout this API serves (verification-code
    /// purpose grants edit the same unified observe config).
    pub fn workspace_layout(&self) -> &ArtifactV2Workspace {
        &self.workspace_layout
    }

    pub fn new(
        workspace_layout: ArtifactV2Workspace,
        store: MailAssistStore,
        artifact_v2_service: std::sync::Arc<
            magician::magician_v2::artifact_v2::service::ArtifactV2Service,
        >,
        operation_router: Option<
            std::sync::Arc<
                magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter,
            >,
        >,
        event_broadcaster: Option<
            std::sync::Arc<magician::magician_v2::realtime_events::RuntimeTransportBroadcaster>,
        >,
    ) -> Self {
        Self {
            workspace_layout,
            store,
            artifact_v2_service,
            operation_router,
            event_broadcaster,
        }
    }
}

/// Resolve the CONFIGURED {profile, provider, model, bound} for a mail op from
/// the operation router — so the stats UI shows what's actually configured
/// (for example, `ollama · <configured model>`), never a hardcoded label.
fn op_binding_json(
    router: Option<
        &std::sync::Arc<
            magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter,
        >,
    >,
    op_name: &str,
) -> serde_json::Value {
    use magician::magician_v2::query_analysis::operation_llm_router::LLMOperation;
    let Some(router) = router else {
        return serde_json::json!({ "operation": op_name, "bound": false });
    };
    let op = LLMOperation::Other(op_name.to_string());
    let binding = router.explicit_binding_for_operation(op_name);
    let profile = binding.as_ref().map(|(p, _)| p.clone());
    let (provider, model) = match router.get_config_for_operation(&op) {
        Ok(prof) => (Some(prof.provider.to_string()), Some(prof.model.clone())),
        Err(_) => (None, None),
    };
    serde_json::json!({
        "operation": op_name,
        "bound": binding.is_some(),
        "profile": profile,
        "provider": provider,
        "model": model,
    })
}

const FOLLOW_UP_SOURCE_PROMISE: &str = "promise";
const FOLLOW_UP_SOURCE_COMMS_INGEST: &str = "comms_ingest";

fn follow_up_source_family(
    label: Option<&str>,
    proposed_action: Option<&serde_json::Value>,
) -> &'static str {
    AttentionSourceFamily::from_follow_up_route_metadata_or_signals(label, proposed_action).as_str()
}

fn follow_up_source_family_histogram(
    signals: &[NeedsApprovalSourceSignal],
) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::from([
        (FOLLOW_UP_SOURCE_COMMS_INGEST.to_string(), 0_i64),
        (FOLLOW_UP_SOURCE_PROMISE.to_string(), 0_i64),
    ]);
    for signal in signals {
        let family =
            follow_up_source_family(signal.label.as_deref(), signal.proposed_action.as_ref());
        *out.entry(family.to_string()).or_insert(0) += 1;
    }
    out
}

fn follow_up_source_family_histogram_from_pairs(
    pairs: Vec<(String, i64)>,
) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::from([
        (FOLLOW_UP_SOURCE_COMMS_INGEST.to_string(), 0_i64),
        (FOLLOW_UP_SOURCE_PROMISE.to_string(), 0_i64),
    ]);
    for (family, count) in pairs {
        *out.entry(family).or_insert(0) += count;
    }
    out
}

#[derive(Debug, Deserialize)]
pub struct ScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DistillBackfillTriggerQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
    /// Runtime pause control. Supplying it updates control state without also
    /// enqueueing a manual pass; `false` wakes the worker to resume.
    #[serde(default)]
    pub paused: Option<bool>,
}

async fn distill_queue_snapshot(
    api: &ChannelAssistApi,
    principal: &str,
    workspace: &str,
) -> anyhow::Result<magician_comms::channel_assist::store::DistillQueueCounts> {
    let policy = magician::magician_v2::observe_catchup::load_observe_catch_up_policy(
        &api.workspace_layout,
        principal,
        workspace,
    )
    .await;
    api.store
        .distill_queue_counts(
            principal,
            workspace,
            distill_backfill_cutoff_ms(policy.lookback_days, chrono::Utc::now().timestamp_millis()),
            magician_comms::channel_assist::assist::distill::MAX_DISTILL_ATTEMPTS,
        )
        .await
}

/// Inspect or enqueue one bounded historical brief-repair pass. The request is
/// scope-resolved like every other channel endpoint; execution remains on the
/// background worker so it observes normal-queue and dispatch-pressure gates.
pub async fn post_channel_assist_distill_backfill_handler(
    api: web::Data<ChannelAssistApi>,
    req: HttpRequest,
    query: web::Query<DistillBackfillTriggerQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let config = channel_distill_runtime_config();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let counts = match api
        .store
        .count_distill_backfill(
            &principal,
            &workspace,
            config.brief_contract_version,
            distill_backfill_cutoff_ms(config.backfill_lookback_days, now_ms),
            now_ms,
        )
        .await
    {
        Ok(counts) => counts,
        Err(error) => {
            return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error);
        },
    };

    if let Some(paused) = query.paused {
        if let Err(error) =
            set_distill_backfill_paused(&api.store, &principal, &workspace, paused).await
        {
            return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error);
        }
    } else if !query.dry_run {
        if !config.enabled {
            return err_json(
                actix_web::http::StatusCode::CONFLICT,
                "channel distillation worker is disabled",
            );
        }
        if let Err(error) = request_distill_backfill(&api.store, &principal, &workspace).await {
            return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error);
        }
    }
    let runtime = match api
        .store
        .distill_backfill_runtime(&principal, &workspace)
        .await
    {
        Ok(runtime) => runtime,
        Err(error) => {
            return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error);
        },
    };

    HttpResponse::Ok().json(serde_json::json!({
        "dry_run": query.dry_run,
        "queued": query.paused.is_none() && !query.dry_run,
        "control_updated": query.paused.is_some(),
        "scope": { "principal": principal, "workspace": workspace },
        "target_contract_version": config.brief_contract_version,
        "lookback_days": config.backfill_lookback_days,
        "batch_size": config.backfill_batch_size,
        "surfaced_first": config.backfill_surfaced_first,
        "backlog": counts,
        "runtime": distill_backfill_runtime_snapshot_json(&runtime),
    }))
}

fn err_json(status: actix_web::http::StatusCode, message: impl std::fmt::Display) -> HttpResponse {
    HttpResponse::build(status).json(serde_json::json!({ "error": message.to_string() }))
}

fn transition_result_or_response(
    annotation_id: &str,
    result: AnnotationTransitionResult,
) -> std::result::Result<MailThreadAnnotation, HttpResponse> {
    match result {
        AnnotationTransitionResult::Applied(updated) => Ok(updated),
        AnnotationTransitionResult::NotFound => Err(err_json(
            actix_web::http::StatusCode::NOT_FOUND,
            format!("annotation not found: {annotation_id}"),
        )),
        AnnotationTransitionResult::UnexpectedState { current, expected } => Err(err_json(
            actix_web::http::StatusCode::CONFLICT,
            format!(
                "annotation expected state {} but was {}",
                expected.as_db_str(),
                current.state.as_db_str()
            ),
        )),
        AnnotationTransitionResult::ActionInProgress { action, .. } => Err(err_json(
            actix_web::http::StatusCode::CONFLICT,
            format!("annotation action already in progress: {action}"),
        )),
    }
}

/// Dismiss is safe to retry. A repeated request must not append another audit
/// or feedback event, but returning the already-dismissed row lets a stale UI
/// remove its card without presenting a false action failure.
fn dismiss_transition_result_or_response(
    annotation_id: &str,
    result: AnnotationTransitionResult,
) -> std::result::Result<MailThreadAnnotation, HttpResponse> {
    match result {
        AnnotationTransitionResult::UnexpectedState { current, .. }
            if current.state == MailAnnotationState::Dismissed =>
        {
            Ok(current)
        },
        other => transition_result_or_response(annotation_id, other),
    }
}

/// One account's row in the sync status response.
#[derive(Debug, Serialize)]
struct AccountSyncStatus {
    provider: String,
    account_alias: String,
    /// Assistance lane (`user_assist` | `envoy`).
    lane: ChannelLane,
    /// Legacy sync-status connected flag: Gmail reports adapter-backed local
    /// profile readiness; other channels preserve enabled-in-config semantics.
    connected: bool,
    thread_count: u64,
    message_count: u64,
    last_synced_at: Option<i64>,
    last_error: Option<String>,
    last_internal_date: Option<i64>,
    provider_cursor: Option<String>,
}

/// `GET /channel-assist/sync/status`.
pub async fn get_channel_assist_sync_status_handler(
    api: web::Data<ChannelAssistApi>,
    req: HttpRequest,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };

    let channel_config = magician_comms::channel_assist::channel_observe::load_or_migrate(
        &api.workspace_layout,
        &principal,
        &workspace,
    )
    .await;
    let connection_ctx = adapter_connection_context(
        &api.workspace_layout,
        &principal,
        &workspace,
        &channel_config,
    );
    let counts = match api.store.count_summary(&principal, &workspace).await {
        Ok(counts) => counts,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let distill_queue = match distill_queue_snapshot(&api, &principal, &workspace).await {
        Ok(counts) => counts,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let pending_classify = match api
        .store
        .count_pending_classify(&principal, &workspace)
        .await
    {
        Ok(n) => n,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let history_lookback_days =
        channel_observe::normalize_history_lookback_days(channel_config.history_lookback_days);
    let distill_config = channel_distill_runtime_config();
    let backfill_now_ms = chrono::Utc::now().timestamp_millis();
    let distill_backfill = match api
        .store
        .count_distill_backfill(
            &principal,
            &workspace,
            distill_config.brief_contract_version,
            distill_backfill_cutoff_ms(distill_config.backfill_lookback_days, backfill_now_ms),
            backfill_now_ms,
        )
        .await
    {
        Ok(counts) => counts,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let distill_backfill_runtime = match api
        .store
        .distill_backfill_runtime(&principal, &workspace)
        .await
    {
        Ok(runtime) => runtime,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let classify_config =
        magician_comms::channel_assist::assist::classify::ChannelClassifyConfig::from_env();
    let reconcile_config =
        magician_comms::channel_assist::assist::reconcile::ChannelReconcileConfig::from_env();

    // The resolved channel accounts (registry + gmail observe fallback)
    // carry provider + lane; union with every (provider, alias) that
    // already has synced rows so a de-configured account stays visible
    // with its data and last cursor rather than silently vanishing.
    let resolved =
        magician_comms::channel_assist::channel_observe::message_accounts(&channel_config);
    let mut lane_by_key: HashMap<(String, String), ChannelLane> = HashMap::new();
    let mut keys: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for account in &resolved {
        let key = (account.provider.clone(), account.account_alias.clone());
        lane_by_key.insert(key.clone(), account.lane);
        if seen.insert(key.clone()) {
            keys.push(key);
        }
    }
    for c in &counts {
        let key = (c.provider.clone(), c.account_alias.clone());
        if seen.insert(key.clone()) {
            keys.push(key);
        }
    }
    keys.sort();

    let enabled_keys: HashSet<(String, String)> = resolved
        .iter()
        .map(|a| (a.provider.clone(), a.account_alias.clone()))
        .collect();

    let mut accounts = Vec::with_capacity(keys.len());
    for (provider, alias) in keys {
        let watermark = match api
            .store
            .get_watermark(&principal, &workspace, &provider, &alias)
            .await
        {
            Ok(watermark) => watermark,
            Err(err) => {
                return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err);
            },
        };
        let (thread_count, message_count) = counts
            .iter()
            .find(|c| c.provider == provider && c.account_alias == alias)
            .map(|c| (c.thread_count, c.message_count))
            .unwrap_or((0, 0));
        let key = (provider.clone(), alias.clone());
        let lane = lane_by_key.get(&key).copied().unwrap_or_default();
        let connected = adapter_registry::sync_status_connected_for(
            &connection_ctx,
            &provider,
            &alias,
            lane,
            enabled_keys.contains(&key),
        );
        accounts.push(AccountSyncStatus {
            lane,
            connected,
            thread_count,
            message_count,
            last_synced_at: watermark.as_ref().map(|w| w.last_synced_at),
            last_error: watermark.as_ref().and_then(|w| w.last_error.clone()),
            last_internal_date: watermark.as_ref().and_then(|w| w.last_internal_date),
            provider_cursor: watermark.as_ref().and_then(|w| w.provider_cursor.clone()),
            provider,
            account_alias: alias,
        });
    }

    HttpResponse::Ok().json(serde_json::json!({
        // "enabled" = any message-channel account is active in the unified config.
        "enabled": !resolved.is_empty(),
        "suppress_sensitive": channel_config.suppress_sensitive,
        "history_lookback_days": history_lookback_days,
        "pending_distill": distill_queue.pending,
        "distill_queue": distill_queue,
        "pending_classify": pending_classify,
        "workers": {
            "distill": {
                "enabled": distill_config.enabled,
                "batch": distill_config.batch,
                "concurrency": distill_config.concurrency,
                "coalesce_threads": distill_config.coalesce_threads,
                "brief_contract_version": distill_config.brief_contract_version,
                "summary_max_chars": distill_config.summary_max_chars,
                "interval_secs": distill_config.interval.as_secs(),
                "backfill": {
                    "enabled": distill_config.backfill_enabled,
                    "lookback_days": distill_config.backfill_lookback_days,
                    "batch_size": distill_config.backfill_batch_size,
                    "surfaced_first": distill_config.backfill_surfaced_first,
                    "backlog": distill_backfill,
                    "metrics": distill_backfill_runtime_snapshot_json(&distill_backfill_runtime),
                },
            },
            "classify": {
                "enabled": classify_config.enabled,
                "batch": classify_config.batch,
                "concurrency": classify_config.concurrency,
                "interval_secs": classify_config.interval.as_secs(),
            },
            "reconcile": {
                "enabled": reconcile_config.enabled,
                "batch": reconcile_config.batch,
                "newer_message_limit": reconcile_config.newer_message_limit,
                "interval_secs": reconcile_config.interval.as_secs(),
                "stale_after_secs": reconcile_config.stale_after.as_secs(),
                "metrics": magician_comms::channel_assist::assist::reconcile::runtime_snapshot_json(),
            },
        },
        "accounts": accounts,
    }))
}

fn histogram_map(pairs: Vec<(String, i64)>) -> serde_json::Map<String, serde_json::Value> {
    pairs
        .into_iter()
        .map(|(k, v)| (k, serde_json::json!(v)))
        .collect()
}

fn hist_get(pairs: &[(String, i64)], key: &str) -> i64 {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| *v)
        .unwrap_or(0)
}

fn channel_distill_runtime_config() -> ChannelDistillConfig {
    magician::config::load_default_magician_config()
        .map(|config| ChannelDistillConfig::from_settings(&config.channel_assist.distillation))
        .unwrap_or_else(|_| ChannelDistillConfig::from_env())
}

/// `GET /channel-assist/stats` — pipeline observability: message/lane totals, the
/// distill funnel, the classifier breakdown, and derived LLM call counts. Per-
/// account detail comes from `sync/status`; this is the aggregate view.
pub async fn get_channel_assist_stats_handler(
    api: web::Data<ChannelAssistApi>,
    req: HttpRequest,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };

    let counts = match api.store.count_summary(&principal, &workspace).await {
        Ok(counts) => counts,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let channel_config =
        channel_observe::load_or_migrate(&api.workspace_layout, &principal, &workspace).await;
    let resolved = channel_observe::message_accounts_all(&channel_config);
    let lane_by_key: HashMap<(String, String), String> = resolved
        .iter()
        .map(|a| {
            (
                (a.provider.clone(), a.account_alias.clone()),
                a.lane.as_db_str().to_string(),
            )
        })
        .collect();

    let mut total_threads: u64 = 0;
    let mut total_messages: u64 = 0;
    let mut by_provider: std::collections::BTreeMap<String, u64> =
        std::collections::BTreeMap::new();
    let mut by_lane: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for c in &counts {
        total_threads += c.thread_count;
        total_messages += c.message_count;
        *by_provider.entry(c.provider.clone()).or_default() += c.message_count;
        let lane = lane_by_key
            .get(&(c.provider.clone(), c.account_alias.clone()))
            .cloned()
            .unwrap_or_else(|| "user_assist".to_string());
        *by_lane.entry(lane).or_default() += c.message_count;
    }

    let distill = match api
        .store
        .distill_state_histogram(&principal, &workspace)
        .await
    {
        Ok(distill) => distill,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let ann_state = match api
        .store
        .annotation_state_histogram(&principal, &workspace)
        .await
    {
        Ok(ann_state) => ann_state,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let ann_label = match api
        .store
        .annotation_label_histogram(&principal, &workspace)
        .await
    {
        Ok(ann_label) => ann_label,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let needs_approval_source_family = match api
        .store
        .needs_approval_source_family_histogram(&principal, &workspace)
        .await
    {
        Ok(pairs) => follow_up_source_family_histogram_from_pairs(pairs),
        Err(err) => {
            tracing::warn!(
                principal = %principal,
                workspace = %workspace,
                error = %err,
                "mail assist source-family SQL histogram failed; falling back to row parse"
            );
            match api
                .store
                .needs_approval_source_signals(&principal, &workspace)
                .await
            {
                Ok(signals) => follow_up_source_family_histogram(&signals),
                Err(err) => {
                    return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err);
                },
            }
        },
    };
    let (classify_retrying, classify_failed) = match api
        .store
        .classify_retry_counts(&principal, &workspace)
        .await
    {
        Ok(counts) => counts,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let distill_backfill_runtime = match api
        .store
        .distill_backfill_runtime(&principal, &workspace)
        .await
    {
        Ok(runtime) => runtime,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };

    let distilled = hist_get(&distill, "done");
    let distill_queue = match distill_queue_snapshot(&api, &principal, &workspace).await {
        Ok(counts) => counts,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let classified: i64 = ann_state.iter().map(|(_, v)| *v).sum();
    let needs_approval = hist_get(&ann_state, "needs_approval");
    let history_lookback_days =
        channel_observe::normalize_history_lookback_days(channel_config.history_lookback_days);
    let distill_config = channel_distill_runtime_config();
    let backfill_now_ms = chrono::Utc::now().timestamp_millis();
    let distill_backfill = match api
        .store
        .count_distill_backfill(
            &principal,
            &workspace,
            distill_config.brief_contract_version,
            distill_backfill_cutoff_ms(distill_config.backfill_lookback_days, backfill_now_ms),
            backfill_now_ms,
        )
        .await
    {
        Ok(counts) => counts,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let brief_coverage = match api
        .store
        .brief_coverage(
            &principal,
            &workspace,
            distill_config.brief_contract_version,
        )
        .await
    {
        Ok(coverage) => coverage,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let classify_config =
        magician_comms::channel_assist::assist::classify::ChannelClassifyConfig::from_env();
    let reconcile_config =
        magician_comms::channel_assist::assist::reconcile::ChannelReconcileConfig::from_env();

    HttpResponse::Ok().json(serde_json::json!({
        "totals": {
            "threads": total_threads,
            "messages": total_messages,
            "by_provider": by_provider,
            "by_lane": by_lane,
        },
        "distill": {
            "pending": distill_queue.pending,
            "queue": distill_queue,
            "done": distilled,
            "suppressed": hist_get(&distill, "suppressed"),
            "skipped": hist_get(&distill, "skipped"),
            "by_state": histogram_map(distill),
            "briefs": brief_coverage,
        },
        "classify": {
            "by_state": histogram_map(ann_state),
            "by_label": histogram_map(ann_label),
            "needs_approval": needs_approval,
            "needs_approval_by_source_family": needs_approval_source_family,
            "retrying": classify_retrying,
            "failed": classify_failed,
        },
        // Funnel: what fraction of synced mail made it through each stage.
        "funnel": {
            "synced": total_messages,
            "distilled": distilled,
            "classified": classified,
            "needs_approval": needs_approval,
        },
        // LLM usage. Distill = one call per distilled message; classify = one
        // call per classified thread. Cost is 0 while both ops are bound to a
        // local ollama profile (gemma is free); real per-op cost comes from the
        // parquet telemetry (queried directly by the stats UI) once a REMOTE
        // profile is used.
        "llm": {
            "distill_calls": distilled,
            "classify_calls": classified,
            "cost_usd": 0.0,
            "note": "local ollama (gemma) — free; cost tracked when channel_classify is bound to a remote profile",
        },
        "runtime": {
            "history_lookback_days": history_lookback_days,
            "distill": {
                "enabled": distill_config.enabled,
                "batch": distill_config.batch,
                "concurrency": distill_config.concurrency,
                "coalesce_threads": distill_config.coalesce_threads,
                "brief_contract_version": distill_config.brief_contract_version,
                "summary_max_chars": distill_config.summary_max_chars,
                "interval_secs": distill_config.interval.as_secs(),
                "backfill": {
                    "enabled": distill_config.backfill_enabled,
                    "lookback_days": distill_config.backfill_lookback_days,
                    "batch_size": distill_config.backfill_batch_size,
                    "surfaced_first": distill_config.backfill_surfaced_first,
                    "backlog": distill_backfill,
                    "metrics": distill_backfill_runtime_snapshot_json(&distill_backfill_runtime),
                },
            },
            "classify": {
                "enabled": classify_config.enabled,
                "batch": classify_config.batch,
                "concurrency": classify_config.concurrency,
                "interval_secs": classify_config.interval.as_secs(),
            },
            "reconcile": {
                "enabled": reconcile_config.enabled,
                "batch": reconcile_config.batch,
                "newer_message_limit": reconcile_config.newer_message_limit,
                "interval_secs": reconcile_config.interval.as_secs(),
                "stale_after_secs": reconcile_config.stale_after.as_secs(),
                "metrics": magician_comms::channel_assist::assist::reconcile::runtime_snapshot_json(),
            },
        },
        // Which profile/provider/model each op is CONFIGURED to use, resolved
        // live from the operation router — never hardcoded. The UI shows this
        // (for example, `<configured model> via ollama`) instead of a baked-in label.
        "ops": {
            "distill": op_binding_json(
                api.operation_router.as_ref(),
                magician_comms::channel_assist::assist::distill::CHANNEL_INGEST_DISTILL_OPERATION,
            ),
            "classify": op_binding_json(
                api.operation_router.as_ref(),
                magician_comms::channel_assist::assist::classify::CHANNEL_CLASSIFY_OPERATION,
            ),
        },
    }))
}

#[derive(Debug, Deserialize)]
pub struct RecentDistillQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// Feed depth (clamped to the ring's capacity).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `GET /channel-assist/distill/recent` — the live distillation feed: the most
/// recently distilled messages, newest first, each pairing the input identity
/// (subject/sender — metadata only) with the local model's derived output
/// (summary/intent plus brief contract/detail status/revision) and per-message
/// latency. Reads the store's in-memory ring
/// (no DB round-trip); resets on restart by design (it's live activity, not
/// history — the durable safe result lives on the message rows).
pub async fn get_channel_assist_distill_recent_handler(
    api: web::Data<ChannelAssistApi>,
    req: HttpRequest,
    query: web::Query<RecentDistillQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let limit = query.limit.unwrap_or(12).clamp(1, 50);
    let items = api.store.recent_distill(&principal, &workspace, limit);
    HttpResponse::Ok().json(serde_json::json!({ "items": items }))
}

/// One channel account in the toggle UI: a discoverable account with
/// adapter-backed readiness merged with its current registry state.
#[derive(Debug, Serialize)]
struct ChannelRow {
    provider: String,
    provider_display: String,
    channel: String,
    channel_label: String,
    capabilities: ChannelCapabilities,
    account_alias: String,
    /// Human label (email for gmail, "self"/"presto" otherwise).
    display: String,
    lane: ChannelLane,
    /// Adapter-backed readiness for this account.
    connected: bool,
    /// Enabled in the unified channel_observe config (syncs next pass).
    enabled: bool,
    thread_count: u64,
    message_count: u64,
    /// Purposes the owner granted beyond observation (secure HITL P6:
    /// `verification_codes`).
    purposes: Vec<String>,
}

fn channel_provider_metadata(provider: &str) -> (String, String, String, ChannelCapabilities) {
    let capabilities = adapter_registry::capabilities_for(provider);
    match adapter_registry::descriptor_for(provider) {
        Some(descriptor) => (
            descriptor.display_label,
            descriptor.channel,
            descriptor.channel_label.to_string(),
            capabilities,
        ),
        None => (
            provider.to_string(),
            channel_observe::provider_to_channel(provider).to_string(),
            magician_comms::channel_assist::channel_providers::channel_label(provider).to_string(),
            capabilities,
        ),
    }
}

fn adapter_connection_context(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    channel_config: &magician::magician_v2::observe_connectors::ChannelObserveConfig,
) -> IngestContext {
    let auth_root = workspace_layout.capability_auth_root(principal, workspace);
    let repo_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let scope_paths = CapabilityWorkspaceManager::new(workspace_layout.clone(), repo_root)
        .scope_paths(principal, workspace);
    let history_lookback_days =
        channel_observe::normalize_history_lookback_days(channel_config.history_lookback_days);
    IngestContext {
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        workspace_layout: workspace_layout.clone(),
        auth_root,
        scope_paths,
        suppress_sensitive: channel_config.suppress_sensitive,
        backfill_days: history_lookback_days,
        min_internal_date: 0,
        max_threads: 0,
        ignore_provider_cursor: false,
    }
}

#[derive(Debug, Deserialize)]
pub struct ChannelUpdateRow {
    provider: String,
    account_alias: String,
    #[serde(default)]
    lane: ChannelLane,
    enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct ChannelUpdate {
    accounts: Vec<ChannelUpdateRow>,
    #[serde(default)]
    history_lookback_days: Option<u32>,
}

/// Discover candidate accounts across channels: gmail from operator-config
/// gws profiles (owner aliases user_assist + `presto` envoy), the user's
/// WhatsApp (`self`, wu.db ready), Presto's Kapso number (`presto`, creds
/// ready), and AgentMail. Returns `(provider, alias, display, default_lane,
/// connected)` tuples.
fn discover_channel_candidates(
    connection_ctx: &IngestContext,
) -> Vec<(String, String, String, ChannelLane, bool)> {
    let mut out: Vec<(String, String, String, ChannelLane, bool)> = Vec::new();

    // gmail: gws_accounts (owner) + gws-presto (envoy) if authenticated.
    let raw = std::fs::read_to_string(
        magician::magician_v2::artifact_v2::workspace::runtime_config_path(
            "operator-config.yaml",
            "skillshub/operator-config.yaml",
        ),
    )
    .unwrap_or_default();
    let cfg: serde_yaml::Value = serde_yaml::from_str(&raw).unwrap_or(serde_yaml::Value::Null);
    if let Some(seq) = cfg.get("gws_accounts").and_then(|v| v.as_sequence()) {
        for a in seq {
            let Some(name) = a.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            let email = a
                .get("expected_email")
                .and_then(|e| e.as_str())
                .unwrap_or("")
                .to_string();
            // Lane is data-driven (operator-config `agent_accounts:` + shipped
            // defaults) — no hardcoded `== "presto"`.
            let lane = channel_observe::account_lane("gmail", name);
            let display = if email.is_empty() {
                name.to_string()
            } else {
                email
            };
            let connected =
                adapter_registry::connection_status_for(connection_ctx, "gmail", name, lane)
                    .unwrap_or(false);
            out.push((
                "gmail".to_string(),
                name.to_string(),
                display,
                lane,
                connected,
            ));
        }
    }
    // gws-presto may exist without an operator-config entry.
    let presto_lane = channel_observe::account_lane("gmail", "presto");
    let presto_connected =
        adapter_registry::connection_status_for(connection_ctx, "gmail", "presto", presto_lane)
            .unwrap_or(false);
    if presto_connected && !out.iter().any(|(p, a, ..)| p == "gmail" && a == "presto") {
        out.push((
            "gmail".to_string(),
            "presto".to_string(),
            "presto".to_string(),
            presto_lane,
            true,
        ));
    }

    // whatsapp (user): adapter-backed wu.db presence.
    let whatsapp_lane = channel_observe::account_lane("whatsapp", "self");
    let wu_connected =
        adapter_registry::connection_status_for(connection_ctx, "whatsapp", "self", whatsapp_lane)
            .unwrap_or(false);
    out.push((
        "whatsapp".to_string(),
        "self".to_string(),
        "self".to_string(),
        whatsapp_lane,
        wu_connected,
    ));

    // whatsapp_kapso: Presto's number, creds present.
    let kapso_lane = channel_observe::account_lane("whatsapp_kapso", "presto");
    let kapso_connected = adapter_registry::connection_status_for(
        connection_ctx,
        "whatsapp_kapso",
        "presto",
        kapso_lane,
    )
    .unwrap_or(false);
    out.push((
        "whatsapp_kapso".to_string(),
        "presto".to_string(),
        "presto".to_string(),
        kapso_lane,
        kapso_connected,
    ));

    // telegram: normal public bot conversations already persisted in chat
    // sessions; the comms-assist adapter reads that local store.
    let telegram_lane = channel_observe::account_lane("telegram", "presto");
    out.push((
        "telegram".to_string(),
        "presto".to_string(),
        "presto".to_string(),
        telegram_lane,
        true,
    ));

    // agentmail: Magican's own email inbox (`MAGICIAN_AGENT_EMAIL`), inbox-scoped
    // key present.
    let am_alias = magician_comms::channel_assist::ingest_agentmail::AGENTMAIL_ACCOUNT_ALIAS;
    let agentmail_provider = magician_comms::channel_assist::ingest_agentmail::AGENTMAIL_PROVIDER;
    let agentmail_lane = channel_observe::account_lane(agentmail_provider, am_alias);
    let agentmail_connected = adapter_registry::connection_status_for(
        connection_ctx,
        agentmail_provider,
        am_alias,
        agentmail_lane,
    )
    .unwrap_or(false);
    out.push((
        agentmail_provider.to_string(),
        am_alias.to_string(),
        magician_comms::channel_assist::ingest_agentmail::agentmail_inbox_email()
            .unwrap_or_default(),
        agentmail_lane,
        agentmail_connected,
    ));

    out
}

/// `GET /channel-assist/channels` — the channel-toggle UI's account list:
/// discovered candidates merged with registry lane/enabled state + counts.
pub async fn get_channel_assist_channels_handler(
    api: web::Data<ChannelAssistApi>,
    req: HttpRequest,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    // Unified observe+assist U4: toggle state comes from the unified
    // channel_observe config (message channels), not the retired channel_assist
    // registry. `message_accounts_all` keeps disabled accounts so the UI can
    // show them; channel→provider maps email→gmail and filters out calendar.
    let channel_config = magician_comms::channel_assist::channel_observe::load_or_migrate(
        &api.workspace_layout,
        &principal,
        &workspace,
    )
    .await;
    let connection_ctx = adapter_connection_context(
        &api.workspace_layout,
        &principal,
        &workspace,
        &channel_config,
    );
    let configured =
        magician_comms::channel_assist::channel_observe::message_accounts_all(&channel_config);
    let reg_by_key: HashMap<(String, String), (ChannelLane, bool)> = configured
        .iter()
        .map(|a| {
            (
                (a.provider.clone(), a.account_alias.clone()),
                (a.lane, a.enabled),
            )
        })
        .collect();
    let purposes_by_key: HashMap<(String, String), Vec<String>> = channel_config
        .channels
        .iter()
        .filter_map(|entry| {
            let provider = channel_observe::channel_to_provider(&entry.channel)?;
            Some((
                (provider.to_string(), entry.account.clone()),
                entry.purposes.clone(),
            ))
        })
        .collect();
    let counts = match api.store.count_summary(&principal, &workspace).await {
        Ok(counts) => counts,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };

    let mut channels: Vec<ChannelRow> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for (provider, alias, display, default_lane, connected) in
        discover_channel_candidates(&connection_ctx)
    {
        let key = (provider.clone(), alias.clone());
        seen.insert(key.clone());
        let (lane, enabled) = reg_by_key
            .get(&key)
            .copied()
            .unwrap_or((default_lane, false));
        let (thread_count, message_count) = counts
            .iter()
            .find(|c| c.provider == provider && c.account_alias == alias)
            .map(|c| (c.thread_count, c.message_count))
            .unwrap_or((0, 0));
        let (provider_display, channel, channel_label, capabilities) =
            channel_provider_metadata(&provider);
        let purposes = purposes_by_key.get(&key).cloned().unwrap_or_default();
        channels.push(ChannelRow {
            provider,
            provider_display,
            channel,
            channel_label,
            capabilities,
            account_alias: alias,
            display,
            lane,
            connected,
            enabled,
            thread_count,
            message_count,
            purposes,
        });
    }
    // Configured accounts with no live candidate (e.g. de-authed) stay visible.
    for a in &configured {
        let key = (a.provider.clone(), a.account_alias.clone());
        if seen.insert(key) {
            let (thread_count, message_count) = counts
                .iter()
                .find(|c| c.provider == a.provider && c.account_alias == a.account_alias)
                .map(|c| (c.thread_count, c.message_count))
                .unwrap_or((0, 0));
            let (provider_display, channel, channel_label, capabilities) =
                channel_provider_metadata(&a.provider);
            channels.push(ChannelRow {
                purposes: purposes_by_key
                    .get(&(a.provider.clone(), a.account_alias.clone()))
                    .cloned()
                    .unwrap_or_default(),
                provider: a.provider.clone(),
                provider_display,
                channel,
                channel_label,
                capabilities,
                account_alias: a.account_alias.clone(),
                display: a.account_alias.clone(),
                lane: a.lane,
                connected: false,
                enabled: a.enabled,
                thread_count,
                message_count,
            });
        }
    }

    HttpResponse::Ok().json(serde_json::json!({
        "channels": channels,
        "history_lookback_days": channel_observe::normalize_history_lookback_days(
            channel_config.history_lookback_days,
        ),
        "history_lookback_options": channel_observe::HISTORY_LOOKBACK_DAY_OPTIONS,
    }))
}

/// `PUT /channel-assist/channels` — write the full channel registry (the UI
/// always sends the complete set, so the gmail fallback-replacement rule
/// can never silently drop an account).
pub async fn put_channel_assist_channels_handler(
    api: web::Data<ChannelAssistApi>,
    req: HttpRequest,
    query: web::Query<ScopeQuery>,
    body: web::Json<ChannelUpdate>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let mut accounts = Vec::with_capacity(body.accounts.len());
    for row in &body.accounts {
        if !magician_comms::channel_assist::channel_providers::is_known_provider(&row.provider) {
            return err_json(
                actix_web::http::StatusCode::BAD_REQUEST,
                format!("unknown provider: {}", row.provider),
            );
        }
        if row.account_alias.trim().is_empty() {
            return err_json(
                actix_web::http::StatusCode::BAD_REQUEST,
                "account_alias must not be empty",
            );
        }
        accounts.push(magician_comms::channel_assist::registry::ChannelAccount {
            provider: row.provider.clone(),
            account_alias: row.account_alias.clone(),
            lane: row.lane,
            enabled: row.enabled,
        });
    }
    // Unified observe+assist U4: the message-channel consent surface writes the
    // unified `channel_observe` config directly (the channel_assist registry is
    // retired — no longer written). The UI always sends the complete set, so
    // replacing all message channels can never silently drop an account;
    // calendar entries in the unified config are left untouched.
    if let Some(requested_lookback) = body.history_lookback_days {
        let catch_up_policy = magician::magician_v2::observe_catchup::load_observe_catch_up_policy(
            &api.workspace_layout,
            &principal,
            &workspace,
        )
        .await;
        if channel_observe::normalize_history_lookback_days(requested_lookback)
            != catch_up_policy.lookback_days
        {
            return err_json(
                actix_web::http::StatusCode::BAD_REQUEST,
                "History recovery is controlled by Observe > Startup catch-up; reload this page and change it there.",
            );
        }
    }
    let registry = magician_comms::channel_assist::registry::ChannelAccountRegistry { accounts };
    if let Err(err) = magician_comms::channel_assist::channel_observe::upsert_message_channels(
        &api.workspace_layout,
        &principal,
        &workspace,
        &registry,
        body.history_lookback_days,
    )
    .await
    {
        return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err);
    }
    // Echo the fresh view (with discovery + counts).
    get_channel_assist_channels_handler(api, req, query).await
}

/// `POST /channel-assist/sync/run` — one sync pass now (same fn as the loop).
pub async fn post_channel_assist_sync_run_handler(
    api: web::Data<ChannelAssistApi>,
    req: HttpRequest,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let outcome = sync::run_sync_pass(
        &api.workspace_layout,
        &api.store,
        &principal,
        &workspace,
        &ChannelSyncConfig::from_env(),
    )
    .await;
    HttpResponse::Ok().json(outcome)
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Parse the `thread_ids` query parameter: comma-separated, trimmed,
/// empties dropped, order-preserving dedup. Errors on an effectively-empty
/// list and on batches over [`MAX_ANNOTATION_THREAD_IDS`] (callers should
/// page rather than dump a whole mailbox into one URL).
fn parse_thread_ids(raw: &str) -> Result<Vec<String>, String> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for part in raw.split(',') {
        let id = part.trim();
        if id.is_empty() {
            continue;
        }
        if seen.insert(id.to_string()) {
            ids.push(id.to_string());
        }
    }
    if ids.is_empty() {
        return Err("thread_ids must contain at least one non-empty thread id".to_string());
    }
    if ids.len() > MAX_ANNOTATION_THREAD_IDS {
        return Err(format!(
            "thread_ids batch too large: {} ids (max {MAX_ANNOTATION_THREAD_IDS}) — page the request",
            ids.len()
        ));
    }
    Ok(ids)
}

#[derive(Debug, Deserialize)]
pub struct AnnotationsQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// Provider; only `gmail` exists in Phase 1 (defaulted).
    #[serde(default)]
    pub provider: Option<String>,
    /// Account alias (e.g. `business`) — required.
    #[serde(default)]
    pub account: Option<String>,
    /// Comma-separated provider thread ids — required, capped.
    #[serde(default)]
    pub thread_ids: Option<String>,
}

/// One requested thread id's answer in the batch GET: the thread summary
/// row if the sync has observed it, plus every annotation on the thread
/// (an empty array is a valid, meaningful answer — "observed, nothing
/// flagged").
#[derive(Debug, Serialize)]
struct ThreadAnnotationsEntry {
    thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread: Option<MailThreadRecord>,
    annotations: Vec<MailThreadAnnotation>,
}

/// `GET /channel-assist/annotations?provider=gmail&account=<alias>&thread_ids=a,b,c`.
pub async fn get_channel_assist_annotations_handler(
    api: web::Data<ChannelAssistApi>,
    req: HttpRequest,
    query: web::Query<AnnotationsQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let provider = query
        .provider
        .clone()
        .unwrap_or_else(|| GMAIL_PROVIDER.to_string());
    let account = match query.account.as_deref().map(str::trim) {
        Some(account) if !account.is_empty() => account.to_string(),
        _ => {
            return err_json(
                actix_web::http::StatusCode::BAD_REQUEST,
                "query parameter `account` is required",
            );
        },
    };
    let thread_ids = match query
        .thread_ids
        .as_deref()
        .ok_or_else(|| "query parameter `thread_ids` is required".to_string())
        .and_then(parse_thread_ids)
    {
        Ok(ids) => ids,
        Err(message) => return err_json(actix_web::http::StatusCode::BAD_REQUEST, message),
    };

    let threads = match api
        .store
        .get_threads_by_ids(&principal, &workspace, &provider, &account, &thread_ids)
        .await
    {
        Ok(threads) => threads,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let annotations = match api
        .store
        .list_annotations_by_thread_ids(&principal, &workspace, &provider, &account, &thread_ids)
        .await
    {
        Ok(annotations) => annotations,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };

    let mut threads_by_id: HashMap<String, MailThreadRecord> = threads
        .into_iter()
        .map(|t| (t.thread_id.clone(), t))
        .collect();
    let mut annotations_by_thread: HashMap<String, Vec<MailThreadAnnotation>> = HashMap::new();
    for annotation in annotations {
        annotations_by_thread
            .entry(annotation.thread_id.clone())
            .or_default()
            .push(annotation);
    }
    let entries: Vec<ThreadAnnotationsEntry> = thread_ids
        .into_iter()
        .map(|thread_id| ThreadAnnotationsEntry {
            thread: threads_by_id.remove(&thread_id),
            annotations: annotations_by_thread.remove(&thread_id).unwrap_or_default(),
            thread_id,
        })
        .collect();

    HttpResponse::Ok().json(serde_json::json!({
        "provider": provider,
        "account": account,
        "threads": entries,
    }))
}

/// `POST /channel-assist/annotations/{id}/dismiss` — transition to
/// `dismissed` and append the audit event (actor=user). Never deletes:
/// the annotation row survives with `state=dismissed` and the full event
/// trail intact.
/// Optional JSON body for `dismiss` — the reason it's being dismissed (e.g.
/// spam / already_handled / duplicate / delegated / not_relevant), recorded as
/// the negative-feedback comment so learning can distinguish WHY.
#[derive(Debug, Deserialize, Default)]
pub struct DismissBody {
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub attribution: Option<AttentionOutcomeAttribution>,
}

pub async fn post_channel_assist_annotation_dismiss_handler(
    api: web::Data<ChannelAssistApi>,
    learning: Option<web::Data<AttentionLearningService>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    body: web::Bytes,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let dismiss_body = serde_json::from_slice::<DismissBody>(&body).unwrap_or_default();
    let feedback_event_id = dismiss_body
        .event_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let feedback_attribution = dismiss_body.attribution.clone();
    let reason_raw = dismiss_body.reason;
    let annotation_id = path.into_inner();
    let reason = reason_raw.filter(|r| !r.trim().is_empty());
    let learning_cohort = if let Some(learning) = learning.as_ref() {
        load_follow_up_learning_cohort(
            &api.store,
            &principal,
            &workspace,
            learning.rescore_limit(),
            Some(&annotation_id),
        )
        .await
        .unwrap_or_default()
    } else {
        Vec::new()
    };
    let result = match api
        .store
        .transition_annotation_if_state(
            &principal,
            &workspace,
            &annotation_id,
            MailAnnotationState::NeedsApproval,
            MailAnnotationState::Dismissed,
            MailAssistActor::User,
            reason.clone().map(|r| serde_json::json!({ "reason": r })),
            Some(AnnotationTransitionFeedback {
                event_id: Some(feedback_event_id.clone()),
                verdict: MailFeedbackVerdict::NotHelpful,
                comment: reason.clone(),
            }),
            now_millis(),
        )
        .await
    {
        Ok(result) => result,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let updated = match dismiss_transition_result_or_response(&annotation_id, result) {
        Ok(updated) => updated,
        Err(resp) => return resp,
    };
    let outcome = match reason.as_deref().map(str::trim) {
        Some("wrong_classification" | "not_actionable") => AttentionOutcomeKind::NotActionable,
        Some("already_handled") => AttentionOutcomeKind::Obsolete,
        Some("delegated" | "wrong_owner") => AttentionOutcomeKind::NotOwner,
        Some("duplicate") => AttentionOutcomeKind::DuplicateOf,
        Some("spam" | "not_relevant") | None | Some(_) => AttentionOutcomeKind::Irrelevant,
    };
    let receipt = record_follow_up_attention_outcome(
        learning.as_ref().map(|learning| learning.get_ref()),
        &principal,
        &workspace,
        &annotation_id,
        learning_cohort,
        Some(feedback_event_id),
        feedback_attribution,
        outcome,
        reason,
        now_millis(),
    )
    .await;
    response_with_feedback_receipt(updated, receipt)
}

/// One channel Follow-up card — a `needs_approval` annotation joined with its
/// thread. Body-blind (metadata + the classifier's label/reason).
#[derive(Debug, Serialize)]
struct NeedsYouCard {
    candidate_id: String,
    source_revision: Option<String>,
    annotation_id: String,
    provider: String,
    account_alias: String,
    /// The mailbox's own address (which account the thread belongs to). Shown
    /// so it's clear the "Open" link targets this account, not the browser's
    /// default.
    account_email: Option<String>,
    thread_id: String,
    lane: String,
    state: String,
    /// A newer message invalidated a pending/inserted draft. Today surfaces
    /// this terminal state only so the owner can explicitly review/re-open it.
    review_required: bool,
    /// Debug/product provenance inside the unified Follow-ups surface:
    /// `promise` for promise/obligation-style follow-ups, `comms_ingest` for
    /// ordinary reply/action classification.
    source_family: String,
    label: Option<String>,
    confidence: Option<f64>,
    reason: Option<String>,
    proposed_action: Option<serde_json::Value>,
    subject: Option<String>,
    sender: Option<String>,
    summary: Option<String>,
    evidence_message_id: Option<String>,
    evidence_message_at: Option<i64>,
    /// When the annotation was created (classifier run time).
    created_at: i64,
    /// When the thread's latest message ARRIVED (epoch ms) — the real received
    /// time to show on the card.
    received_at: Option<i64>,
    /// Deep link to open the thread in its provider (gmail today; None else).
    open_url: Option<String>,
    /// Channel actions the provider's [`ChannelActionAdapter`] advertises for
    /// this card (e.g. iMessage `reply`). Empty for providers with no action
    /// adapter. Data-driven so the UI renders whatever the adapter exposes.
    available_actions: Vec<ChannelActionDescriptor>,
    baseline_rank: usize,
    learned_rank: usize,
    rank_delta: i64,
    learning_score: Option<f64>,
    actionability_probability: Option<f64>,
    actionability_explanation: Option<ActionabilityExplanation>,
    actionability_model_version: Option<String>,
    actionability_snapshot_id: Option<String>,
    semantic_feature_status: SemanticExtractionStatus,
    actionability_score_status: ActionabilityScoreStatus,
    actionability_mode: AttentionActionabilityMode,
    grouping: AttentionGroupingMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bandit_decision: Option<AttentionBanditDecisionMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decision_item: Option<AttentionDecisionItem>,
}

fn needs_you_card(
    row: NeedsApprovalRow,
    rank: Option<&AttentionRankMetadata>,
    grouping: AttentionGroupingMetadata,
    decision_item: Option<AttentionDecisionItem>,
) -> NeedsYouCard {
    let bandit_decision = decision_item
        .as_ref()
        .and_then(|item| item.bandit_decision.clone());
    let sender = match (&row.from_name, &row.from_address) {
        (Some(name), Some(address)) => Some(format!("{name} <{address}>")),
        (Some(name), None) => Some(name.clone()),
        (None, Some(address)) => Some(address.clone()),
        (None, None) => None,
    };
    let open_url = open_thread_url(
        &row.provider,
        &row.account_alias,
        &row.thread_id,
        row.account_email.as_deref(),
    );
    let source_family =
        follow_up_source_family(row.label.as_deref(), row.proposed_action.as_ref()).to_string();
    let available_actions = available_actions_for_provider(&row.provider);
    NeedsYouCard {
        candidate_id: row.annotation_id.clone(),
        source_revision: row
            .classification_input_revision
            .map(|revision| format!("distill:{revision}")),
        annotation_id: row.annotation_id,
        provider: row.provider,
        account_alias: row.account_alias,
        account_email: row.account_email,
        thread_id: row.thread_id,
        lane: row.lane,
        state: row.state.as_db_str().to_string(),
        review_required: row.state == MailAnnotationState::Stale,
        source_family,
        label: row.label,
        confidence: row.confidence,
        reason: row.reason,
        proposed_action: row.proposed_action,
        subject: row.subject,
        sender,
        summary: row.latest_summary,
        evidence_message_id: row.evidence_message_id,
        evidence_message_at: row.evidence_message_at,
        created_at: row.created_at,
        received_at: row.last_message_at,
        open_url,
        available_actions,
        baseline_rank: rank.map(|rank| rank.baseline_rank).unwrap_or(0),
        learned_rank: rank.map(|rank| rank.learned_rank).unwrap_or(0),
        rank_delta: rank.map(|rank| rank.rank_delta).unwrap_or(0),
        learning_score: rank.and_then(|rank| rank.learning_score),
        actionability_probability: rank.and_then(|rank| rank.actionability_probability),
        actionability_explanation: rank.and_then(|rank| rank.actionability_explanation.clone()),
        actionability_model_version: rank.and_then(|rank| rank.actionability_model_version.clone()),
        actionability_snapshot_id: rank.and_then(|rank| rank.actionability_snapshot_id.clone()),
        semantic_feature_status: rank
            .map(|rank| rank.semantic_feature_status)
            .unwrap_or(SemanticExtractionStatus::Missing),
        actionability_score_status: rank
            .map(|rank| rank.actionability_score_status)
            .unwrap_or(ActionabilityScoreStatus::Disabled),
        actionability_mode: rank
            .map(|rank| rank.actionability_mode)
            .unwrap_or(AttentionActionabilityMode::Disabled),
        grouping,
        bandit_decision,
        decision_item,
    }
}

/// Resolve the channel actions the provider's action adapter advertises. Empty
/// when the provider ships no action adapter.
fn available_actions_for_provider(provider: &str) -> Vec<ChannelActionDescriptor> {
    default_channel_adapters()
        .iter()
        .find(|adapter| adapter.provider() == provider)
        .and_then(|adapter| adapter.action_adapter())
        .map(|adapter| adapter.available_actions())
        .unwrap_or_default()
}

fn open_thread_url(
    provider: &str,
    account_alias: &str,
    thread_id: &str,
    account_email: Option<&str>,
) -> Option<String> {
    magician_comms::channel_assist::adapter_registry::thread_url_for(
        magician_comms::channel_assist::adapter_registry::ChannelThreadRef {
            provider,
            account_alias,
            account_email,
            thread_id,
        },
    )
}

#[derive(Debug, Deserialize)]
pub struct NeedsYouQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// Page size (clamped 1..=100; default 50).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Cursor from the previous page.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Include canonical learning diagnostics in this compatibility response.
    /// Today owns the canonical union through its dedicated projection request
    /// and sets this false for card lookup/pagination reads, preventing every
    /// page from rebuilding the same complete cross-lane universe.
    #[serde(default = "default_true")]
    pub include_projection: bool,
}

const fn default_true() -> bool {
    true
}

const fn should_compute_legacy_follow_up_projection(
    include_projection: bool,
    canonical_projection_available: bool,
    semantic_ranking_enabled: bool,
) -> bool {
    include_projection && !canonical_projection_available && semantic_ranking_enabled
}

const fn should_load_follow_up_source_histogram(legacy_projection_available: bool) -> bool {
    legacy_projection_available
}

const fn should_compute_follow_up_semantic_health(include_projection: bool) -> bool {
    include_projection
}

const LEARNED_FOLLOW_UP_CURSOR_PREFIX: &str = "learned-v1:";

fn encode_learned_follow_up_cursor(
    generation: u64,
    universe_digest: &str,
    candidate_id: &str,
) -> String {
    format!(
        "{LEARNED_FOLLOW_UP_CURSOR_PREFIX}{generation}:{universe_digest}:{}",
        urlencoding::encode(candidate_id)
    )
}

fn decode_learned_follow_up_cursor(cursor: &str) -> Result<(u64, String, String), String> {
    let raw = cursor
        .strip_prefix(LEARNED_FOLLOW_UP_CURSOR_PREFIX)
        .ok_or_else(|| {
            "learned ranking requires a learned-v1 cursor; refresh the list".to_string()
        })?;
    let mut parts = raw.splitn(3, ':');
    let generation = parts
        .next()
        .ok_or_else(|| "invalid learned Follow-up cursor".to_string())?;
    let generation = generation
        .parse::<u64>()
        .map_err(|_| "invalid learned Follow-up cursor generation".to_string())?;
    let universe_digest = parts
        .next()
        .filter(|digest| !digest.is_empty())
        .ok_or_else(|| "invalid learned Follow-up cursor universe".to_string())?
        .to_string();
    let candidate_id = parts
        .next()
        .ok_or_else(|| "invalid learned Follow-up cursor candidate".to_string())?;
    let candidate_id = urlencoding::decode(candidate_id)
        .map_err(|_| "invalid learned Follow-up cursor candidate".to_string())?
        .into_owned();
    if candidate_id.trim().is_empty() {
        return Err("invalid learned Follow-up cursor candidate".to_string());
    }
    Ok((generation, universe_digest, candidate_id))
}

fn follow_up_semantic_candidate(row: &NeedsApprovalRow) -> SemanticAttentionCandidate {
    let proposed_action = row
        .proposed_action
        .as_ref()
        .and_then(|value| serde_json::to_string(value).ok())
        .unwrap_or_default();
    SemanticAttentionCandidate {
        candidate_id: row.annotation_id.clone(),
        source_revision: row
            .classification_input_revision
            .map(|revision| format!("distill:{revision}")),
        semantic_text: [
            row.subject.as_deref().unwrap_or_default(),
            row.latest_summary.as_deref().unwrap_or_default(),
            row.label.as_deref().unwrap_or_default(),
            row.reason.as_deref().unwrap_or_default(),
            proposed_action.as_str(),
        ]
        .join("\n"),
        existing_embedding: None,
        actionability_features: Some(ActionabilityFeatureInput {
            semantic: deserialize_semantic_envelope(row.semantic_features.as_ref()),
            classifier_label: row.label.clone(),
            classifier_confidence: row.confidence,
            age_days: row.last_message_at.map(|received_at| {
                (chrono::Utc::now()
                    .timestamp_millis()
                    .saturating_sub(received_at))
                .max(0) as f64
                    / 86_400_000.0
            }),
            ..Default::default()
        }),
        grouping_features: Some(GroupingFeatureInput {
            semantic: deserialize_semantic_envelope(row.semantic_features.as_ref()),
            exact_source_identity: Some(format!(
                "{}:{}:{}",
                row.provider, row.account_alias, row.thread_id
            )),
            sender_identity: row
                .from_address
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_ascii_lowercase),
            account_identity: Some(format!("{}:{}", row.provider, row.account_alias)),
            event_at_ms: row.last_message_at,
            action_key: row.proposed_action.as_ref().and_then(|action| {
                ["kind", "action", "type"]
                    .into_iter()
                    .find_map(|key| action.get(key).and_then(serde_json::Value::as_str))
                    .map(str::to_ascii_lowercase)
            }),
            ..Default::default()
        }),
    }
}

async fn load_follow_up_learning_cohort(
    store: &MailAssistStore,
    principal: &str,
    workspace: &str,
    limit: usize,
    target_annotation_id: Option<&str>,
) -> anyhow::Result<Vec<NeedsApprovalRow>> {
    load_follow_up_learning_cohort_inner(
        store,
        principal,
        workspace,
        limit,
        target_annotation_id,
        true,
    )
    .await
}

/// `target_must_be_pending = false` still resolves a target that has already
/// left the approval queue. Reconciliation labels an annotation it just
/// retired, so insisting the row still be pending would silently yield no
/// candidate and drop the label.
async fn load_follow_up_learning_cohort_inner(
    store: &MailAssistStore,
    principal: &str,
    workspace: &str,
    limit: usize,
    target_annotation_id: Option<&str>,
    target_must_be_pending: bool,
) -> anyhow::Result<Vec<NeedsApprovalRow>> {
    let mut rows = store
        .list_needs_approval_attention_lane_page(
            principal,
            workspace,
            AttentionLane::FollowUp.as_str(),
            limit.max(1),
            0,
            None,
        )
        .await?
        .rows;
    if let Some(target_annotation_id) = target_annotation_id {
        if !rows
            .iter()
            .any(|row| row.annotation_id == target_annotation_id)
        {
            if let Some(target) = store
                .get_attention_row(
                    principal,
                    workspace,
                    target_annotation_id,
                    target_must_be_pending,
                )
                .await?
            {
                rows.push(target);
            }
        }
    }
    Ok(rows)
}

/// Turns a reconciliation-proved owner completion into an `ActionCompleted`
/// label.
///
/// `ActionCompleted` is the only positive the actionability model can learn
/// from, and the in-product approve/commit flow captures a small minority of
/// real completions: most follow-ups are discharged by the owner replying in
/// their own mail client, where there is no button to press. Reconciliation
/// already detects that to retire the card, so the evidence exists — it was
/// simply never written down as a label.
///
/// The label carries no attribution. The owner acted outside any impression,
/// so there is no decision or exposure to attribute it to, and inventing one
/// would corrupt the propensity record that off-policy evaluation depends on.
pub struct ReconcileAttentionLabeller {
    store: MailAssistStore,
    learning: AttentionLearningService,
    cohort_limit: usize,
}

impl ReconcileAttentionLabeller {
    pub fn new(
        store: MailAssistStore,
        learning: AttentionLearningService,
        cohort_limit: usize,
    ) -> Self {
        Self {
            store,
            learning,
            cohort_limit: cohort_limit.max(1),
        }
    }
}

#[async_trait::async_trait]
impl magician_comms::channel_assist::assist::completion_port::ReconcileCompletionSink
    for ReconcileAttentionLabeller
{
    async fn record_owner_completion(
        &self,
        principal: &str,
        workspace: &str,
        annotation_id: &str,
        completed_at: i64,
    ) {
        let cohort = match load_follow_up_learning_cohort_inner(
            &self.store,
            principal,
            workspace,
            self.cohort_limit,
            Some(annotation_id),
            false,
        )
        .await
        {
            Ok(cohort) => cohort,
            Err(error) => {
                tracing::warn!(
                    annotation_id,
                    error = %error,
                    "reconciliation completion could not load its learning cohort"
                );
                return;
            },
        };
        if record_follow_up_attention_outcome(
            Some(&self.learning),
            principal,
            workspace,
            annotation_id,
            cohort,
            None,
            None,
            AttentionOutcomeKind::ActionCompleted,
            Some("reconciled_owner_sent".to_string()),
            completed_at,
        )
        .await
        .is_none()
        {
            // Worth a line: this is the only source of positive actionability
            // labels that does not require the owner to click anything, so a
            // silent failure here keeps the model permanently untrainable.
            tracing::warn!(
                annotation_id,
                "reconciliation completion produced no attention outcome"
            );
        }
    }
}

async fn record_follow_up_attention_outcome(
    learning: Option<&AttentionLearningService>,
    principal: &str,
    workspace: &str,
    annotation_id: &str,
    cohort: Vec<NeedsApprovalRow>,
    event_id: Option<String>,
    attribution: Option<AttentionOutcomeAttribution>,
    outcome: AttentionOutcomeKind,
    reason: Option<String>,
    occurred_at: i64,
) -> Option<AttentionFeedbackReceipt> {
    let learning = learning?;
    let candidate = cohort
        .iter()
        .find(|row| row.annotation_id == annotation_id)
        .map(follow_up_semantic_candidate)?;
    let active = cohort.iter().map(follow_up_semantic_candidate).collect();
    match learning
        .record_and_propagate(
            principal,
            workspace,
            AttentionSurface::FollowUp,
            RecordAttentionOutcome {
                event_id: event_id.unwrap_or_else(|| Uuid::new_v4().to_string()),
                candidate,
                outcome,
                reason,
                label_quality: AttentionLabelQuality::Strong,
                occurred_at,
                attribution,
            },
            active,
        )
        .await
    {
        Ok(receipt) => Some(receipt),
        Err(error) => {
            tracing::warn!(annotation_id, error = %error, "failed to propagate Follow-up feedback");
            None
        },
    }
}

fn response_with_feedback_receipt<T: Serialize>(
    value: T,
    receipt: Option<AttentionFeedbackReceipt>,
) -> HttpResponse {
    let mut value = match serde_json::to_value(value) {
        Ok(value) => value,
        Err(error) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error),
    };
    if let (Some(object), Some(receipt)) = (value.as_object_mut(), receipt) {
        object.insert(
            "feedback_receipt".to_string(),
            serde_json::to_value(receipt).unwrap_or(serde_json::Value::Null),
        );
    }
    HttpResponse::Ok().json(value)
}

struct FollowUpLearningProjection {
    page: AttentionLanePage<NeedsApprovalRow>,
    universe_ids: Vec<String>,
    ranks: Vec<AttentionRankMetadata>,
    generation: u64,
    rank_scope: &'static str,
    grouping: AttentionGroupingResult,
    routing: AttentionRoutingEvaluation,
    decision: AttentionDecision,
    routing_health: AttentionRoutingHealth,
}

#[derive(Debug, Deserialize)]
pub struct AttentionPairCorrectionBody {
    pub event_id: String,
    pub surface: AttentionSurface,
    pub left: AttentionPairCandidateRef,
    pub right: AttentionPairCandidateRef,
    pub label: AttentionPairLabelKind,
    #[serde(default)]
    pub confidence: Option<f64>,
}

async fn load_pair_correction_universe(
    api: &ChannelAssistApi,
    resurfacing_store: &magician::magician_v2::attention::resurfacing::store::ResurfacingStore,
    principal: &str,
    workspace: &str,
    surface: AttentionSurface,
) -> anyhow::Result<Vec<SemanticAttentionCandidate>> {
    match surface {
        AttentionSurface::FollowUp => {
            let probe = api
                .store
                .list_needs_approval_attention_lane_page(
                    principal,
                    workspace,
                    AttentionLane::FollowUp.as_str(),
                    1,
                    0,
                    None,
                )
                .await?;
            Ok(api
                .store
                .list_needs_approval_attention_lane_page(
                    principal,
                    workspace,
                    AttentionLane::FollowUp.as_str(),
                    probe.total.max(1) as usize,
                    0,
                    None,
                )
                .await?
                .rows
                .iter()
                .map(follow_up_semantic_candidate)
                .collect())
        },
        AttentionSurface::WorthALook => {
            let probe = resurfacing_store
                .list_surfaced_page(principal, workspace, 1, 0, None)
                .await?;
            Ok(resurfacing_store
                .list_surfaced_page(principal, workspace, probe.total.max(1) as usize, 0, None)
                .await?
                .candidates
                .iter()
                .map(|candidate| {
                    crate::resurfacing_api::resurfacing_semantic_candidate(candidate, None)
                })
                .collect())
        },
    }
}

fn affected_pair_clusters(
    grouping: &AttentionGroupingResult,
    left_id: &str,
    right_id: &str,
) -> HashSet<String> {
    grouping
        .projection
        .clusters
        .iter()
        .filter(|cluster| {
            cluster.member_ids.iter().any(|member| member == left_id)
                || cluster.member_ids.iter().any(|member| member == right_id)
        })
        .map(|cluster| cluster.cluster_id.clone())
        .collect()
}

/// Persist one explicit owner pair correction and immediately recompute the
/// affected complete surface universe. Member lifecycle state is read only.
pub async fn post_attention_pair_correction_handler(
    api: web::Data<ChannelAssistApi>,
    resurfacing_store: web::Data<
        magician::magician_v2::attention::resurfacing::store::ResurfacingStore,
    >,
    learning: web::Data<AttentionLearningService>,
    req: HttpRequest,
    query: web::Query<ScopeQuery>,
    body: web::Json<AttentionPairCorrectionBody>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    if body.confidence.is_some_and(|confidence| confidence != 1.0) {
        return err_json(
            actix_web::http::StatusCode::BAD_REQUEST,
            "explicit owner pair corrections have confidence 1.0",
        );
    }
    let candidates = match load_pair_correction_universe(
        &api,
        &resurfacing_store,
        &principal,
        &workspace,
        body.surface,
    )
    .await
    {
        Ok(candidates) => candidates,
        Err(error) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error),
    };
    for candidate_ref in [&body.left, &body.right] {
        if !candidates.iter().any(|candidate| {
            candidate.candidate_id == candidate_ref.candidate_id
                && candidate.source_revision == candidate_ref.source_revision
        }) {
            return err_json(
                actix_web::http::StatusCode::CONFLICT,
                "pair correction candidate revision is no longer in the eligible universe",
            );
        }
    }
    let (_, ranks) = match learning
        .rank_eligible_universe(&principal, &workspace, body.surface, &candidates)
        .await
    {
        Ok(result) => result,
        Err(error) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error),
    };
    let before = match learning
        .group_eligible_universe(&principal, &workspace, body.surface, &candidates, &ranks)
        .await
    {
        Ok(grouping) => grouping,
        Err(error) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error),
    };
    let request = RecordAttentionPairLabel {
        event_id: body.event_id.clone(),
        surface: body.surface,
        left: body.left.clone(),
        right: body.right.clone(),
        label: body.label,
        source: AttentionPairLabelSource::Owner,
        label_quality: AttentionLabelQuality::Strong,
        confidence: 1.0,
        occurred_at: chrono::Utc::now().timestamp_millis(),
    };
    let (persisted, generation) = match learning
        .record_pair_label(&principal, &workspace, &request)
        .await
    {
        Ok(result) => result,
        Err(error) => return err_json(actix_web::http::StatusCode::CONFLICT, error),
    };
    let after = match learning
        .group_eligible_universe(&principal, &workspace, body.surface, &candidates, &ranks)
        .await
    {
        Ok(grouping) => grouping,
        Err(error) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error),
    };
    let mut affected = affected_pair_clusters(
        &before,
        &persisted.left.candidate_id,
        &persisted.right.candidate_id,
    );
    affected.extend(affected_pair_clusters(
        &after,
        &persisted.left.candidate_id,
        &persisted.right.candidate_id,
    ));
    let mut affected_cluster_ids: Vec<_> = affected.into_iter().collect();
    affected_cluster_ids.sort();
    HttpResponse::Ok().json(AttentionPairFeedbackReceipt {
        pair_label_id: persisted.pair_label_id,
        inserted: persisted.inserted,
        label: persisted.label,
        canonical_left_id: persisted.left.candidate_id,
        canonical_right_id: persisted.right.candidate_id,
        affected_cluster_ids,
        grouping_generation: generation,
        recomputed: true,
    })
}

/// Expand one Follow-up group without changing any member lifecycle.
pub async fn get_follow_up_group_members_handler(
    api: web::Data<ChannelAssistApi>,
    learning: web::Data<AttentionLearningService>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let probe = match api
        .store
        .list_needs_approval_attention_lane_page(
            &principal,
            &workspace,
            AttentionLane::FollowUp.as_str(),
            1,
            0,
            None,
        )
        .await
    {
        Ok(probe) => probe,
        Err(error) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error),
    };
    let rows = match api
        .store
        .list_needs_approval_attention_lane_page(
            &principal,
            &workspace,
            AttentionLane::FollowUp.as_str(),
            probe.total.max(1) as usize,
            0,
            None,
        )
        .await
    {
        Ok(page) => page.rows,
        Err(error) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error),
    };
    let candidates: Vec<_> = rows.iter().map(follow_up_semantic_candidate).collect();
    let (_, ranks) = match learning
        .rank_eligible_universe(
            &principal,
            &workspace,
            AttentionSurface::FollowUp,
            &candidates,
        )
        .await
    {
        Ok(result) => result,
        Err(error) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error),
    };
    let grouping = match learning
        .group_eligible_universe(
            &principal,
            &workspace,
            AttentionSurface::FollowUp,
            &candidates,
            &ranks,
        )
        .await
    {
        Ok(grouping) => grouping,
        Err(error) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error),
    };
    let cluster_id = path.into_inner();
    let Some(cluster) = grouping
        .projection
        .clusters
        .iter()
        .find(|cluster| cluster.cluster_id == cluster_id)
        .cloned()
    else {
        return err_json(
            actix_web::http::StatusCode::NOT_FOUND,
            "attention group not found",
        );
    };
    let rank_by_id: HashMap<_, _> = ranks
        .iter()
        .map(|rank| (rank.candidate_id.as_str(), rank))
        .collect();
    let row_by_id: HashMap<_, _> = rows
        .into_iter()
        .map(|row| (row.annotation_id.clone(), row))
        .collect();
    let items: Vec<_> = cluster
        .member_ids
        .iter()
        .filter_map(|member_id| {
            let row = row_by_id.get(member_id)?.clone();
            let metadata = grouping.projection.metadata.get(member_id)?.clone();
            Some(needs_you_card(
                row,
                rank_by_id.get(member_id.as_str()).copied(),
                metadata,
                None,
            ))
        })
        .collect();
    if items.len() != cluster.member_ids.len() {
        return err_json(
            actix_web::http::StatusCode::CONFLICT,
            "attention group membership changed; refresh the list",
        );
    }
    let total = items.len();
    HttpResponse::Ok().json(serde_json::json!({
        "cluster": cluster,
        "items": items,
        "total": total,
        "grouping_mode": grouping.mode,
        "grouping_snapshot_id": grouping.snapshot_id,
        "grouping_generation": grouping.generation,
    }))
}

async fn project_follow_up_learning(
    store: &MailAssistStore,
    learning: &AttentionLearningService,
    principal: &str,
    workspace: &str,
    limit: usize,
    cursor: Option<&str>,
    selected_candidate_ids: Option<&HashSet<String>>,
) -> Result<FollowUpLearningProjection, (actix_web::http::StatusCode, String)> {
    let decision_started = Instant::now();
    let probe = store
        .list_needs_approval_attention_lane_page(
            principal,
            workspace,
            AttentionLane::FollowUp.as_str(),
            1,
            0,
            None,
        )
        .await
        .map_err(|error| {
            (
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            )
        })?;
    let universe = store
        .list_needs_approval_attention_lane_page(
            principal,
            workspace,
            AttentionLane::FollowUp.as_str(),
            probe.total.max(1) as usize,
            0,
            None,
        )
        .await
        .map_err(|error| {
            (
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            )
        })?;
    let universe_candidates: Vec<SemanticAttentionCandidate> = universe
        .rows
        .iter()
        .map(follow_up_semantic_candidate)
        .collect();
    let source_family_by_id: HashMap<String, String> = universe
        .rows
        .iter()
        .map(|row| {
            (
                row.annotation_id.clone(),
                follow_up_source_family(row.label.as_deref(), row.proposed_action.as_ref())
                    .to_string(),
            )
        })
        .collect();
    let universe_ids: Vec<String> = universe_candidates
        .iter()
        .map(|candidate| candidate.candidate_id.clone())
        .collect();
    let rank_digest = rank_universe_digest(&universe_candidates);
    let (generation, ranks) = learning
        .rank_eligible_universe(
            principal,
            workspace,
            AttentionSurface::FollowUp,
            &universe_candidates,
        )
        .await
        .map_err(|error| {
            (
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            )
        })?;
    let grouping = learning
        .group_eligible_universe(
            principal,
            workspace,
            AttentionSurface::FollowUp,
            &universe_candidates,
            &ranks,
        )
        .await
        .map_err(|error| {
            (
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            )
        })?;
    let universe_digest =
        canonical_universe_digest(&rank_digest, &grouping.projection.representative_ids);
    let rank_by_id: HashMap<&str, usize> = ranks
        .iter()
        .map(|rank| (rank.candidate_id.as_str(), rank.learned_rank))
        .collect();
    let mut ordered = universe.rows;
    if grouping.mode == AttentionGroupingMode::Enforced
        || learning
            .serving_ranking_without_grouping_enabled_for(principal, workspace)
            .await
            .unwrap_or(false)
    {
        ordered.sort_by_key(|row| {
            rank_by_id
                .get(row.annotation_id.as_str())
                .copied()
                .unwrap_or(usize::MAX)
        });
    }
    if grouping.mode == AttentionGroupingMode::Enforced {
        let representatives: std::collections::HashSet<&str> = grouping
            .projection
            .representative_ids
            .iter()
            .map(String::as_str)
            .collect();
        ordered.retain(|row| representatives.contains(row.annotation_id.as_str()));
    }
    let served_candidate_ids: Vec<String> = ordered
        .iter()
        .map(|row| row.annotation_id.clone())
        .collect();

    let start = if let Some(cursor) = cursor {
        let (cursor_generation, cursor_universe, last_id) = decode_learned_follow_up_cursor(cursor)
            .map_err(|error| (actix_web::http::StatusCode::BAD_REQUEST, error))?;
        if cursor_generation != generation || cursor_universe != universe_digest {
            return Err((
                actix_web::http::StatusCode::CONFLICT,
                "attention ranking changed; refresh from the first page".to_string(),
            ));
        }
        ordered
            .iter()
            .position(|row| row.annotation_id == last_id)
            .map(|index| index + 1)
            .ok_or_else(|| {
                (
                    actix_web::http::StatusCode::CONFLICT,
                    "attention cursor item is no longer active; refresh the list".to_string(),
                )
            })?
    } else {
        0
    };
    let end = start.saturating_add(limit).min(ordered.len());
    let mut rows = ordered[start..end].to_vec();
    let page_candidate_ids: HashSet<String> =
        rows.iter().map(|row| row.annotation_id.clone()).collect();
    let selected_candidate_ids = selected_candidate_ids.unwrap_or(&page_candidate_ids);
    let has_more = end < ordered.len();
    let next_cursor = has_more.then(|| {
        encode_learned_follow_up_cursor(
            generation,
            &universe_digest,
            rows.last()
                .map(|row| row.annotation_id.as_str())
                .unwrap_or_default(),
        )
    });
    let routing_inputs: Vec<AttentionRoutingCandidate<'_>> = universe_candidates
        .iter()
        .filter_map(|candidate| {
            let rank = ranks
                .iter()
                .find(|rank| rank.candidate_id == candidate.candidate_id)?;
            let grouping_metadata = grouping.projection.metadata.get(&candidate.candidate_id)?;
            let source_family = source_family_by_id.get(&candidate.candidate_id)?;
            Some(AttentionRoutingCandidate {
                candidate,
                source_family,
                baseline_route: AttentionRoute::FollowUp,
                rank,
                grouping: grouping_metadata,
                hard_eligible: true,
                ineligibility_reason: None,
            })
        })
        .collect();
    if routing_inputs.len() != universe_candidates.len() {
        return Err((
            actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
            "attention routing inputs did not reconcile with the Follow-up universe".to_string(),
        ));
    }
    // This legacy endpoint currently owns only the baseline Follow-up source
    // universe. Learned cross-lane proposals are recorded, but application is
    // fail-closed until an authoritative union projector can materialize the
    // same move in Worth a look.
    let mut routing = learning
        .evaluate_routing_universe(
            principal,
            workspace,
            AttentionSurface::FollowUp,
            false,
            &routing_inputs,
        )
        .await
        .map_err(|error| {
            (
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            )
        })?;
    routing.finalize_served_projection(
        served_candidate_ids.iter().map(String::as_str),
        selected_candidate_ids.iter().map(String::as_str),
    );
    let decision_context = AttentionDecisionContext {
        queue_size: universe_candidates.len(),
        ..Default::default()
    };
    learning
        .apply_personal_bandit_ranking(
            principal,
            workspace,
            start == 0 && !has_more,
            &decision_context,
            &mut routing,
        )
        .await
        .map_err(|error| {
            (
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            )
        })?;
    if routing
        .bandit_health
        .as_ref()
        .is_some_and(|health| health.exploration_rate > 0.0)
        || routing.items.iter().any(|item| {
            item.bandit_decision
                .as_ref()
                .is_some_and(|bandit| bandit.applied)
        })
    {
        let mut selected: Vec<_> = routing.items.iter().filter(|item| item.selected).collect();
        selected.sort_by_key(|item| item.served_rank);
        rows = selected
            .into_iter()
            .filter_map(|item| {
                ordered
                    .iter()
                    .find(|row| row.annotation_id == item.candidate_id)
                    .cloned()
            })
            .collect();
    }
    let returned_item_count = routing.items.iter().filter(|item| item.selected).count();
    let decision = learning
        .record_routing_decision(
            principal,
            workspace,
            &universe_digest,
            decision_context,
            decision_started
                .elapsed()
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
            returned_item_count,
            &routing,
        )
        .await
        .map_err(|error| {
            (
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            )
        })?;
    let routing_health = learning
        .routing_health(principal, workspace, &routing)
        .await
        .map_err(|error| {
            (
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            )
        })?;
    Ok(FollowUpLearningProjection {
        page: AttentionLanePage {
            lane: AttentionLane::FollowUp,
            items: rows,
            total: ordered.len(),
            request_hitl_total: None,
            limit,
            cursor: cursor.map(str::to_string),
            next_cursor,
            has_more,
        },
        universe_ids,
        ranks,
        generation,
        rank_scope: "eligible_universe",
        grouping,
        routing,
        decision,
        routing_health,
    })
}

/// `GET /channel-assist/needs-you` — legacy-named compatibility route for
/// channel Follow-up cards: active `needs_approval` annotations (the
/// classifier's actionable, high-confidence output) joined with their thread.
/// Cursor-paginated and returns the `total` so callers can page — the volume
/// can grow large, and this list is surfaced natively in Today's Follow-ups
/// tab and the Attention inbox.
pub async fn get_channel_assist_needs_you_handler(
    api: web::Data<ChannelAssistApi>,
    learning: Option<web::Data<AttentionLearningService>>,
    semantic_worker: Option<web::Data<SemanticExtractionWorker>>,
    resurfacing_store: Option<
        web::Data<magician::magician_v2::attention::resurfacing::store::ResurfacingStore>,
    >,
    memory_api: Option<web::Data<crate::memory_api::MemoryApi>>,
    req: HttpRequest,
    query: web::Query<NeedsYouQuery>,
) -> HttpResponse {
    let projection_started = Instant::now();
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let store_started = Instant::now();
    let canonical_attention_projection = if query.include_projection {
        if let (Some(learning), Some(resurfacing_store)) =
            (learning.as_ref(), resurfacing_store.as_ref())
        {
            match crate::canonical_attention_api::project_canonical_attention_union(
                &api.store,
                resurfacing_store,
                learning,
                &principal,
                &workspace,
                memory_api.as_ref().map(|data| data.get_ref()),
            )
            .await
            {
                Ok(projection) => Some(projection),
                Err(error) => {
                    tracing::warn!(
                        principal,
                        workspace,
                        error = %error,
                        "canonical attention union unavailable; serving atomic legacy Follow-up baseline"
                    );
                    None
                },
            }
        } else {
            None
        }
    } else {
        None
    };
    let mut learning_projection = None;
    // Canonical delivery and the legacy learned-list path are mutually
    // exclusive, but that does not mean canonical delivery is observe-only.
    // Keep the serving-path switch separate from the health value returned to
    // clients so a successful Slice-1 canonical order is reported accurately.
    let configured_semantic_ranking_enabled =
        if query.include_projection && canonical_attention_projection.is_none() {
            if let Some(learning) = learning.as_ref() {
                learning
                    .serving_semantic_ranking_enabled_for(&principal, &workspace)
                    .await
                    .unwrap_or(false)
            } else {
                false
            }
        } else {
            false
        };
    let legacy_semantic_ranking_enabled = should_compute_legacy_follow_up_projection(
        query.include_projection,
        canonical_attention_projection.is_some(),
        configured_semantic_ranking_enabled,
    );
    let semantic_ranking_enabled = if !query.include_projection {
        false
    } else if let Some(projection) = canonical_attention_projection.as_ref() {
        crate::canonical_attention_api::canonical_slice1_order_is_active(
            projection.status,
            projection.integrity.load_complete,
            learning
                .as_ref()
                .is_some_and(|learning| learning.semantic_ranking_enabled()),
        )
    } else if let Some(learning) = learning.as_ref() {
        learning
            .serving_semantic_ranking_enabled_for(&principal, &workspace)
            .await
            .unwrap_or(false)
    } else {
        false
    };
    let page = if legacy_semantic_ranking_enabled {
        match project_follow_up_learning(
            &api.store,
            learning.as_ref().expect("checked above").get_ref(),
            &principal,
            &workspace,
            limit,
            query.cursor.as_deref(),
            None,
        )
        .await
        {
            Ok(projection) => {
                let page = projection.page.clone();
                learning_projection = Some(projection);
                page
            },
            Err((status, error)) => return err_json(status, error),
        }
    } else {
        match list_channel_attention_lane_rows(
            &api.store,
            AttentionLaneQuery {
                principal: &principal,
                workspace: &workspace,
                lane: AttentionLane::FollowUp,
                cursor: query.cursor.as_deref(),
                offset: 0,
                limit,
            },
        )
        .await
        {
            Ok(page) => page,
            Err(err) if err.to_string().contains("invalid attention lane cursor") => {
                return err_json(actix_web::http::StatusCode::BAD_REQUEST, err);
            },
            Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
        }
    };
    let returned_candidate_ids: HashSet<String> = page
        .items
        .iter()
        .map(|row| row.annotation_id.clone())
        .collect();
    let store_ms = store_started.elapsed().as_secs_f64() * 1000.0;
    if query.include_projection
        && learning_projection.is_none()
        && canonical_attention_projection.is_none()
    {
        if let Some(learning) = learning.as_ref() {
            match project_follow_up_learning(
                &api.store,
                learning.get_ref(),
                &principal,
                &workspace,
                limit,
                None,
                Some(&returned_candidate_ids),
            )
            .await
            {
                Ok(projection) => learning_projection = Some(projection),
                Err((_status, error)) => tracing::warn!(
                    principal,
                    workspace,
                    error,
                    "failed to compute observe-only Follow-up rank metadata"
                ),
            }
        }
    }
    let rank_by_id: HashMap<&str, &AttentionRankMetadata> = learning_projection
        .as_ref()
        .map(|projection| {
            projection
                .ranks
                .iter()
                .map(|rank| (rank.candidate_id.as_str(), rank))
                .collect()
        })
        .unwrap_or_default();
    // The canonical serving path leaves `learning_projection` unset, so the
    // per-item bindings resolve through canonical diagnostics instead. Every
    // aggregate response field already falls back this way; without the same
    // fallback here each row loses its learned rank and its decision binding,
    // and the client cannot record a verified impression against the decision
    // that served it — so no reward signal ever reaches the model.
    //
    // Canonical bindings are keyed by the surface-qualified id while these rows
    // carry the raw one, so the fallback normalizes before looking up.
    let canonical_bindings = canonical_attention_projection
        .as_ref()
        .and_then(|projection| projection.diagnostics.as_ref());
    let canonical_rank_by_id: HashMap<&str, &AttentionRankMetadata> = canonical_bindings
        .map(|diagnostics| {
            diagnostics
                .ranks
                .iter()
                .map(|rank| (rank.candidate_id.as_str(), rank))
                .collect()
        })
        .unwrap_or_default();
    let canonical_decision_item_by_id: HashMap<&str, &AttentionDecisionItem> = canonical_bindings
        .map(|diagnostics| {
            diagnostics
                .decision_items
                .iter()
                .map(|item| (item.candidate_id.as_str(), item))
                .collect()
        })
        .unwrap_or_default();
    let mapping_started = Instant::now();
    let cards: Vec<NeedsYouCard> = page
        .items
        .into_iter()
        .map(|r| {
            let sender = match (&r.from_name, &r.from_address) {
                (Some(name), Some(addr)) => Some(format!("{name} <{addr}>")),
                (Some(name), None) => Some(name.clone()),
                (None, Some(addr)) => Some(addr.clone()),
                (None, None) => None,
            };
            let open_url = open_thread_url(
                &r.provider,
                &r.account_alias,
                &r.thread_id,
                r.account_email.as_deref(),
            );
            let source_family =
                follow_up_source_family(r.label.as_deref(), r.proposed_action.as_ref()).to_string();
            let available_actions = available_actions_for_provider(&r.provider);
            let canonical_candidate_id =
                AttentionSurface::FollowUp.canonical_candidate_id(&r.annotation_id);
            let rank = rank_by_id
                .get(r.annotation_id.as_str())
                .copied()
                .or_else(|| {
                    canonical_rank_by_id
                        .get(canonical_candidate_id.as_str())
                        .copied()
                });
            let grouping = learning_projection
                .as_ref()
                .and_then(|projection| {
                    projection
                        .grouping
                        .projection
                        .metadata
                        .get(&r.annotation_id)
                })
                .cloned()
                .unwrap_or_else(|| {
                    singleton_grouping_metadata(AttentionPairCandidateRef {
                        candidate_id: r.annotation_id.clone(),
                        source_revision: r
                            .classification_input_revision
                            .map(|revision| format!("distill:{revision}")),
                    })
                });
            let decision_item = learning_projection
                .as_ref()
                .and_then(|projection| projection.routing.item(&r.annotation_id))
                .or_else(|| {
                    canonical_decision_item_by_id
                        .get(canonical_candidate_id.as_str())
                        .copied()
                })
                .cloned();
            let bandit_decision = decision_item
                .as_ref()
                .and_then(|item| item.bandit_decision.clone());
            NeedsYouCard {
                candidate_id: r.annotation_id.clone(),
                source_revision: r
                    .classification_input_revision
                    .map(|revision| format!("distill:{revision}")),
                annotation_id: r.annotation_id,
                provider: r.provider,
                account_alias: r.account_alias,
                account_email: r.account_email,
                thread_id: r.thread_id,
                lane: r.lane,
                state: r.state.as_db_str().to_string(),
                review_required: r.state == MailAnnotationState::Stale,
                source_family,
                label: r.label,
                confidence: r.confidence,
                reason: r.reason,
                proposed_action: r.proposed_action,
                subject: r.subject,
                sender,
                summary: r.latest_summary,
                evidence_message_id: r.evidence_message_id,
                evidence_message_at: r.evidence_message_at,
                created_at: r.created_at,
                received_at: r.last_message_at,
                open_url,
                available_actions,
                baseline_rank: rank.map(|rank| rank.baseline_rank).unwrap_or(0),
                learned_rank: rank.map(|rank| rank.learned_rank).unwrap_or(0),
                rank_delta: rank.map(|rank| rank.rank_delta).unwrap_or(0),
                learning_score: rank.and_then(|rank| rank.learning_score),
                actionability_probability: rank.and_then(|rank| rank.actionability_probability),
                actionability_explanation: rank
                    .and_then(|rank| rank.actionability_explanation.clone()),
                actionability_model_version: rank
                    .and_then(|rank| rank.actionability_model_version.clone()),
                actionability_snapshot_id: rank
                    .and_then(|rank| rank.actionability_snapshot_id.clone()),
                semantic_feature_status: rank
                    .map(|rank| rank.semantic_feature_status)
                    .unwrap_or(SemanticExtractionStatus::Missing),
                actionability_score_status: rank
                    .map(|rank| rank.actionability_score_status)
                    .unwrap_or(ActionabilityScoreStatus::Disabled),
                actionability_mode: rank
                    .map(|rank| rank.actionability_mode)
                    .unwrap_or(AttentionActionabilityMode::Disabled),
                grouping,
                bandit_decision,
                decision_item,
            }
        })
        .collect();
    let mapping_ms = mapping_started.elapsed().as_secs_f64() * 1000.0;
    let projection_ms = projection_started.elapsed().as_secs_f64() * 1000.0;
    let budget_ms = today_projection_latency_budget_ms();
    let within_budget = projection_ms <= budget_ms as f64;
    if !within_budget {
        // Soft budget: the projection is still served (the OK response below) and the
        // exact timings ride in the Server-Timing / X-Magician-Latency-Budget headers
        // regardless — so a breach is never an alert. It sits at DEBUG rather than
        // INFO because a chronically-over-budget projection fires this on EVERY
        // Today request, drowning the log; the per-request headers remain the
        // authoritative timing signal either way.
        tracing::debug!(
            target: "magician::channel_assist::latency",
            projection_ms,
            store_ms,
            mapping_ms,
            budget_ms,
            principal = principal.as_str(),
            workspace = workspace.as_str(),
            "Today channel projection exceeded its latency budget"
        );
    }
    // Only the legacy learned projection consumes this scope-wide histogram.
    // Canonical delivery already carries its health diagnostics, while callers
    // that set include_projection=false explicitly own those diagnostics through
    // the dedicated projection endpoint. Do not turn a paginated card read into
    // a second full-scope scan.
    let source_family_counts = if should_load_follow_up_source_histogram(
        learning_projection.is_some(),
    ) {
        match api
            .store
            .needs_approval_source_family_histogram(&principal, &workspace)
            .await
        {
            Ok(pairs) => follow_up_source_family_histogram_from_pairs(pairs)
                .into_iter()
                .map(|(family, count)| (family, count.max(0) as u64))
                .collect(),
            Err(error) => {
                tracing::warn!(principal, workspace, error = %error, "failed to load Follow-up source-family health");
                BTreeMap::new()
            },
        }
    } else {
        BTreeMap::new()
    };
    let canonical_diagnostics = canonical_attention_projection
        .as_ref()
        .and_then(|projection| projection.diagnostics.as_ref());
    let health = if let (Some(learning), Some(projection)) =
        (learning.as_ref(), learning_projection.as_ref())
    {
        learning
            .health(
                &principal,
                &workspace,
                AttentionSurface::FollowUp,
                page.total,
                &projection.universe_ids,
                source_family_counts,
                &projection.ranks,
            )
            .await
            .ok()
    } else if let Some(diagnostics) = canonical_diagnostics {
        Some(diagnostics.follow_up_health.clone())
    } else {
        None
    };
    // Semantic extraction health also walks the complete active universe. It is
    // diagnostic projection work, not part of card pagination or lookup.
    let semantic_extraction_health =
        if should_compute_follow_up_semantic_health(query.include_projection) {
            if let (Some(learning), Some(worker), Some(resurfacing_store)) = (
                learning.as_ref(),
                semantic_worker.as_ref(),
                resurfacing_store.as_ref(),
            ) {
                crate::attention_learning_api::build_semantic_extraction_health(
                    learning,
                    worker,
                    &api.store,
                    resurfacing_store,
                    &principal,
                    &workspace,
                )
                .await
                .ok()
            } else {
                None
            }
        } else {
            None
        };
    let actionability_training = if let Some(learning) = learning.as_ref() {
        learning
            .actionability_training_status(&principal, &workspace)
            .await
            .ok()
    } else {
        None
    };
    HttpResponse::Ok()
        .insert_header((
            "Server-Timing",
            format!(
                "channel_today_store;dur={store_ms:.2}, channel_today_map;dur={mapping_ms:.2}, channel_today_projection;dur={projection_ms:.2}"
            ),
        ))
        .insert_header((
            "X-Magician-Latency-Budget",
            format!(
                "channel_today_projection;dur={projection_ms:.2};budget={budget_ms};within={within_budget}"
            ),
        ))
        .json(serde_json::json!({
        "items": cards,
        "total": page.total,
        "limit": page.limit,
        "cursor": query.cursor,
        "next_cursor": page.next_cursor,
        "has_more": page.has_more,
        "health": health.as_ref(),
        "semantic_extraction_health": semantic_extraction_health,
        "actionability_training": actionability_training,
        "semantic_ranking_enabled": semantic_ranking_enabled,
        "rank_generation": learning_projection.as_ref().map(|projection| projection.generation)
            .or_else(|| canonical_diagnostics.map(|diagnostics| diagnostics.rank_generation)),
        "rank_scope": learning_projection.as_ref().map(|projection| projection.rank_scope)
            .or_else(|| canonical_diagnostics.map(|diagnostics| diagnostics.rank_scope.as_str())),
        "actionability_mode": health
            .as_ref()
            .map(|health| health.actionability_mode)
            .unwrap_or(AttentionActionabilityMode::Disabled),
        "actionability_snapshot_id": health
            .as_ref()
            .and_then(|health| health.actionability_snapshot_id.clone())
            .or_else(|| {
                learning
                    .as_ref()
                    .and_then(|learning| learning.actionability_snapshot_id().map(str::to_string))
            }),
        "semantic_extraction_coverage": health
            .as_ref()
            .map(|health| health.semantic_extraction_coverage)
            .unwrap_or(0.0),
        "actionability_scored_count": health
            .as_ref()
            .map(|health| health.actionability_scored_count)
            .unwrap_or(0),
        "actionability_fallback_count": health
            .as_ref()
            .map(|health| health.actionability_fallback_count)
            .unwrap_or(0),
        "grouping_mode": learning_projection
            .as_ref()
            .map(|projection| projection.grouping.mode)
            .or_else(|| canonical_diagnostics.map(|diagnostics| diagnostics.grouping_mode))
            .unwrap_or(AttentionGroupingMode::Disabled),
        "grouping_snapshot_id": learning_projection
            .as_ref()
            .and_then(|projection| projection.grouping.snapshot_id.as_deref())
            .or_else(|| canonical_diagnostics.and_then(|diagnostics| diagnostics.grouping_snapshot_id.as_deref())),
        "grouping_generation": learning_projection
            .as_ref()
            .map(|projection| projection.grouping.generation)
            .or_else(|| canonical_diagnostics.map(|diagnostics| diagnostics.grouping_generation)),
        "grouping_scope": canonical_diagnostics
            .map(|diagnostics| diagnostics.grouping_scope.as_str())
            .unwrap_or("eligible_universe"),
        "grouping_health": learning_projection
            .as_ref()
            .map(|projection| &projection.grouping.health)
            .or_else(|| canonical_diagnostics.map(|diagnostics| &diagnostics.grouping_health)),
        "decision": learning_projection
            .as_ref()
            .map(|projection| &projection.decision)
            .or_else(|| canonical_diagnostics.map(|diagnostics| &diagnostics.decision)),
        "impression_policy": learning.as_ref().map(|learning| serde_json::json!({
            "min_visible_ms": learning.routing_min_visible_ms(),
            "visibility_rule_version": learning.routing_visibility_rule_version(),
        })),
        "routing_health": learning_projection
            .as_ref()
            .map(|projection| &projection.routing_health)
            .or_else(|| canonical_diagnostics.map(|diagnostics| &diagnostics.routing_health)),
        "bandit_health": learning_projection
            .as_ref()
            .and_then(|projection| projection.routing.bandit_health.as_ref())
            .or_else(|| canonical_diagnostics.and_then(|diagnostics| diagnostics.bandit_health.as_ref())),
        "canonical_attention_projection_ref": canonical_attention_projection.as_ref().map(|projection| serde_json::json!({
            "projection_id": projection.projection_id.as_str(),
            "universe_digest": projection.universe_digest.as_str(),
            "status": projection.status,
        })),
        "latency": {
            "projection_ms": projection_ms,
            "budget_ms": budget_ms,
            "within_budget": within_budget,
        },
    }))
}

/// `GET /channel-assist/annotations/{id}/message` — the actual evidence message
/// the summary/classification was DERIVED FROM. v3 annotations store the exact
/// message id; v4 annotations also carry every message id in a coalesced
/// evidence batch. Legacy rows fall back to the newest distilled message. Body
/// is fetched LIVE (never persisted). `has_newer` flags that a newer message
/// arrived after the one we summarized.
pub async fn get_channel_assist_message_handler(
    api: web::Data<ChannelAssistApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let annotation_id = path.into_inner();
    let annotation = match api
        .store
        .get_annotation(&principal, &workspace, &annotation_id)
        .await
    {
        Ok(Some(a)) => a,
        Ok(None) => {
            return err_json(
                actix_web::http::StatusCode::NOT_FOUND,
                format!("annotation not found: {annotation_id}"),
            );
        },
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let scope = AttentionScope {
        principal: principal.clone(),
        workspace: workspace.clone(),
    };
    let resolver = ChannelEvidenceResolver::new(api.workspace_layout.clone(), api.store.clone());
    let evidence = match resolver.resolve_annotation(&scope, &annotation).await {
        Ok(Some(evidence)) => evidence,
        Ok(None) => {
            return HttpResponse::Ok().json(serde_json::json!({
                "annotation_id": annotation_id,
                "provider": annotation.provider,
                "thread_id": annotation.thread_id,
                "body": serde_json::Value::Null,
            }));
        },
        Err(error) => {
            return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error);
        },
    };
    let message = &evidence.primary;
    let original = resolver
        .fetch_original(
            &scope,
            &evidence,
            ORIGINAL_BODY_MAX_CHARS,
            ORIGINAL_RESPONSE_MAX_CHARS,
        )
        .await;
    let body = original
        .primary(&message.message_id)
        .and_then(|message| message.body.clone());
    let original_status = if evidence.sensitive_suppressed() {
        "suppressed"
    } else if !original.fetcher_available {
        "unsupported"
    } else if original.fetch_failed && !original.fetched_any {
        "unavailable"
    } else {
        "available"
    };
    if original.messages.is_empty() {
        return HttpResponse::Ok().json(serde_json::json!({
            "annotation_id": annotation_id,
            "provider": annotation.provider,
            "thread_id": annotation.thread_id,
            "body": serde_json::Value::Null,
        }));
    }
    HttpResponse::Ok().json(serde_json::json!({
        "annotation_id": annotation_id,
        "provider": annotation.provider,
        "thread_id": annotation.thread_id,
        "message_id": message.message_id.clone(),
        "subject": message.subject.clone(),
        // The stored summary this message was distilled into — what the
        // classifier acted on. Lets the UI show input (body) ↔ output (summary).
        "summary": message.summary.clone(),
        "received_at": message.internal_date,
        "has_newer": evidence.has_newer,
        "original_status": original_status,
        "body": body,
        "evidence_messages": original.messages,
    }))
}

/// Optional JSON body for `approve` — the owner's hint for the agent.
#[derive(Debug, Deserialize, Default)]
pub struct ApproveBody {
    #[serde(default)]
    pub hint: Option<String>,
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub attribution: Option<AttentionOutcomeAttribution>,
}

/// `POST /channel-assist/annotations/{id}/approve` — accept the follow-up:
/// transition the annotation to `approved`, log a `helpful` feedback, and
/// create a one-shot follow-up task linked to the thread. Accepts an optional
/// `{hint}` body — the owner's instruction for the agent. Only valid while the
/// annotation is `needs_approval`.
pub async fn post_channel_assist_annotation_approve_handler(
    api: web::Data<ChannelAssistApi>,
    learning: Option<web::Data<AttentionLearningService>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    body: web::Bytes,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    // Optional owner hint (tolerant of an empty body — the old callers sent none).
    let approve_body = serde_json::from_slice::<ApproveBody>(&body).unwrap_or_default();
    let event_id = approve_body
        .event_id
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let attribution = approve_body.attribution;
    let hint = approve_body
        .hint
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty());
    let annotation_id = path.into_inner();
    let learning_cohort = if let Some(learning) = learning.as_ref() {
        load_follow_up_learning_cohort(
            &api.store,
            &principal,
            &workspace,
            learning.rescore_limit(),
            Some(&annotation_id),
        )
        .await
        .unwrap_or_default()
    } else {
        Vec::new()
    };
    let (annotation, claim_id) = match api
        .store
        .begin_annotation_action_claim(
            &principal,
            &workspace,
            &annotation_id,
            "approve",
            MailAnnotationState::NeedsApproval,
            now_millis(),
        )
        .await
    {
        Ok(AnnotationActionClaimResult::Claimed {
            annotation,
            claim_id,
        }) => (annotation, claim_id),
        Ok(AnnotationActionClaimResult::Existing {
            annotation,
            task_id: Some(task_id),
        }) => {
            return HttpResponse::Ok().json(serde_json::json!({
                "annotation": annotation,
                "task_id": task_id,
            }));
        },
        Ok(AnnotationActionClaimResult::Existing { .. }) => {
            return err_json(
                actix_web::http::StatusCode::CONFLICT,
                "annotation approval is already in progress",
            );
        },
        Ok(AnnotationActionClaimResult::NotFound) => {
            return err_json(
                actix_web::http::StatusCode::NOT_FOUND,
                format!("annotation not found: {annotation_id}"),
            );
        },
        Ok(AnnotationActionClaimResult::UnexpectedState { current, expected }) => {
            return err_json(
                actix_web::http::StatusCode::CONFLICT,
                format!(
                    "annotation expected state {} but was {}",
                    expected.as_db_str(),
                    current.state.as_db_str()
                ),
            );
        },
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };

    // Resolve the exact evidence message first. The thread's rolling
    // `latest_summary` can advance after this annotation was created; the
    // task should describe the message the classifier actually acted on.
    let evidence_message = if let Some(message_id) = annotation.evidence_message_id.as_deref() {
        api.store
            .get_message(
                &principal,
                &workspace,
                &annotation.provider,
                &annotation.account_alias,
                message_id,
            )
            .await
            .ok()
            .flatten()
    } else {
        None
    };

    // Resolve the thread for a human-readable title and legacy summary
    // fallback when older annotations have no evidence message id.
    let thread = api
        .store
        .get_threads_by_ids(
            &principal,
            &workspace,
            &annotation.provider,
            &annotation.account_alias,
            std::slice::from_ref(&annotation.thread_id),
        )
        .await
        .ok()
        .and_then(|threads| threads.into_iter().next());
    let subject = thread
        .as_ref()
        .and_then(|t| t.subject.clone())
        .or_else(|| evidence_message.as_ref().and_then(|m| m.subject.clone()))
        .unwrap_or_else(|| annotation.thread_id.clone());
    let summary = evidence_message
        .as_ref()
        .and_then(|m| m.summary.clone())
        .or_else(|| thread.as_ref().and_then(|t| t.latest_summary.clone()))
        .filter(|s| !s.trim().is_empty());

    let label = annotation
        .label
        .clone()
        .unwrap_or_else(|| "follow_up".to_string());
    let reason = annotation.reason.clone().unwrap_or_default();
    let mut description = format!(
        "Follow up on the {} thread \"{}\" ({} / {}). Classifier: {}{}. Thread id: {}.",
        magician_comms::channel_assist::channel_providers::channel_label(&annotation.provider),
        subject,
        annotation.provider,
        annotation.account_alias,
        label,
        if reason.is_empty() {
            String::new()
        } else {
            format!(" — {reason}")
        },
        annotation.thread_id,
    );
    // Give the agent the locally-derived evidence summary (safe — never the
    // raw body) and the owner's explicit instruction, so it isn't acting blind.
    if let Some(s) = &summary {
        description.push_str(&format!("\n\nEvidence summary (locally derived): {s}"));
    }
    if let Some(action) = annotation
        .proposed_action
        .as_ref()
        .and_then(|v| v.as_object())
    {
        let mut parts = Vec::new();
        for key in ["follow_up_kind", "action_owner", "due_text", "urgency"] {
            if let Some(value) = action.get(key).and_then(|v| v.as_str()) {
                if !value.trim().is_empty() {
                    parts.push(format!("{key}: {}", value.trim()));
                }
            }
        }
        if !parts.is_empty() {
            description.push_str(&format!("\n\nFollow-up signal: {}.", parts.join(", ")));
        }
    }
    if let Some(h) = &hint {
        description.push_str(&format!("\n\nOwner's instruction: {h}"));
    }
    let input = CreateTaskInput {
        principal: principal.clone(),
        workspace: workspace.clone(),
        title: format!("Follow up: {subject}"),
        description,
        agent_id: "executive-assistant".to_string(),
        goal_id: None,
        ui_thread_id: "general".to_string(),
        priority: None,
        due_date: None,
        tags: vec![TaskTagRecord {
            id: "channel-assist".to_string(),
            name: "channel-assist".to_string(),
            color: None,
        }],
        created_by: "channel-assist".to_string(),
        depends_on: Vec::new(),
        approved: true,
        schedule: None,
        output_mode: TaskOutputMode::default(),
        chat_session_id: None,
        lifecycle: TaskLifecycle::Persistent,
        sync_mode: TaskSyncMode::Deferred,
    };
    let task_id = match api.artifact_v2_service.create_task(input).await {
        Ok(task) => task.manifest.task_id,
        Err(err) => {
            let err_message = format!("creating follow-up task: {err}");
            let _ = api
                .store
                .abort_annotation_action_claim(
                    &principal,
                    &workspace,
                    &annotation_id,
                    "approve",
                    &claim_id,
                )
                .await;
            return err_json(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                err_message,
            );
        },
    };

    let result = match api
        .store
        .complete_annotation_action_claim(
            &principal,
            &workspace,
            &annotation_id,
            "approve",
            &claim_id,
            &task_id,
            MailAnnotationState::NeedsApproval,
            MailAnnotationState::Approved,
            MailAssistActor::Worker,
            Some(serde_json::json!({
                "action": "approve",
                "task_id": task_id.clone()
            })),
            Some(AnnotationTransitionFeedback {
                event_id: Some(event_id.clone()),
                verdict: MailFeedbackVerdict::Helpful,
                comment: None,
            }),
            now_millis(),
        )
        .await
    {
        Ok(result) => result,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let updated = match transition_result_or_response(&annotation_id, result) {
        Ok(updated) => updated,
        Err(resp) => return resp,
    };

    let receipt = record_follow_up_attention_outcome(
        learning.as_ref().map(|learning| learning.get_ref()),
        &principal,
        &workspace,
        &annotation_id,
        learning_cohort,
        Some(event_id),
        attribution,
        AttentionOutcomeKind::ActionCompleted,
        None,
        now_millis(),
    )
    .await;
    response_with_feedback_receipt(
        serde_json::json!({
            "annotation": updated,
            "task_id": task_id,
        }),
        receipt,
    )
}

// ---------------------------------------------------------------------------
// Generic channel-action compose/commit (iMessage Assist Task 5)
// ---------------------------------------------------------------------------

/// The resolved context a channel action needs: the annotation, its thread, and
/// the latest inbound message text + summary. Built once from the store +
/// content fetcher so `compose`/`commit` can fill a [`ChannelActionRequest`].
struct ResolvedActionContext {
    annotation: MailThreadAnnotation,
    thread: Option<MailThreadRecord>,
    /// The live-fetched text of the message being acted on (the reply target).
    latest_message: Option<String>,
    /// The subject to show the drafter, if any.
    subject: Option<String>,
    /// The human sender label of the message being acted on.
    sender: Option<String>,
    /// The thread's rolling locally-derived summary.
    thread_summary: Option<String>,
    /// The actor id (participant handle) to reply to, when known.
    actor_external_id: Option<String>,
}

/// Fetch the annotation, its thread, and the latest inbound message text +
/// summary via the same [`ChannelEvidenceResolver`] the `.../message` endpoint
/// uses. Returns an error response on a missing annotation or store failure.
async fn resolve_action_context(
    api: &ChannelAssistApi,
    principal: &str,
    workspace: &str,
    annotation_id: &str,
) -> std::result::Result<ResolvedActionContext, HttpResponse> {
    let annotation = match api
        .store
        .get_annotation(principal, workspace, annotation_id)
        .await
    {
        Ok(Some(a)) => a,
        Ok(None) => {
            return Err(err_json(
                actix_web::http::StatusCode::NOT_FOUND,
                format!("annotation not found: {annotation_id}"),
            ));
        },
        Err(err) => {
            return Err(err_json(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                err,
            ));
        },
    };

    let scope = AttentionScope {
        principal: principal.to_string(),
        workspace: workspace.to_string(),
    };
    let resolver = ChannelEvidenceResolver::new(api.workspace_layout.clone(), api.store.clone());
    let evidence = match resolver.resolve_annotation(&scope, &annotation).await {
        Ok(evidence) => evidence,
        Err(err) => {
            return Err(err_json(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                err,
            ));
        },
    };

    let (thread, subject, sender, thread_summary, latest_message, actor_external_id) =
        if let Some(evidence) = evidence {
            let primary = &evidence.primary;
            let subject = primary
                .subject
                .clone()
                .or_else(|| evidence.thread.as_ref().and_then(|t| t.subject.clone()));
            let sender = match (&primary.from_name, &primary.from_address) {
                (Some(name), Some(addr)) => Some(format!("{name} <{addr}>")),
                (Some(name), None) => Some(name.clone()),
                (None, Some(addr)) => Some(addr.clone()),
                (None, None) => None,
            };
            let thread_summary = evidence
                .thread
                .as_ref()
                .and_then(|t| t.latest_summary.clone())
                .or_else(|| primary.summary.clone())
                .filter(|s| !s.trim().is_empty());
            // Live-fetch the reply-target body (never persisted).
            let live = resolver
                .fetch_original(
                    &scope,
                    &evidence,
                    ORIGINAL_BODY_MAX_CHARS,
                    ORIGINAL_RESPONSE_MAX_CHARS,
                )
                .await;
            let latest_message = live
                .primary(&primary.message_id)
                .and_then(|m| m.body.clone())
                .filter(|b| !b.trim().is_empty());
            let actor_external_id = primary.from_address.clone();
            (
                evidence.thread.clone(),
                subject,
                sender,
                thread_summary,
                latest_message,
                actor_external_id,
            )
        } else {
            // No resolvable evidence message — fall back to the thread record so
            // `commit` still has a recipient/subject and compose can surface a
            // clean "no message to reply to" error.
            let thread = api
                .store
                .get_threads_by_ids(
                    principal,
                    workspace,
                    &annotation.provider,
                    &annotation.account_alias,
                    std::slice::from_ref(&annotation.thread_id),
                )
                .await
                .ok()
                .and_then(|threads| threads.into_iter().next());
            let subject = thread.as_ref().and_then(|t| t.subject.clone());
            let sender =
                thread
                    .as_ref()
                    .and_then(|t| match (&t.latest_from_name, &t.latest_from_address) {
                        (Some(name), Some(addr)) => Some(format!("{name} <{addr}>")),
                        (Some(name), None) => Some(name.clone()),
                        (None, Some(addr)) => Some(addr.clone()),
                        (None, None) => None,
                    });
            let thread_summary = thread
                .as_ref()
                .and_then(|t| t.latest_summary.clone())
                .filter(|s| !s.trim().is_empty());
            let actor_external_id = thread.as_ref().and_then(|t| t.latest_from_address.clone());
            (
                thread,
                subject,
                sender,
                thread_summary,
                None,
                actor_external_id,
            )
        };

    Ok(ResolvedActionContext {
        annotation,
        thread,
        latest_message,
        subject,
        sender,
        thread_summary,
        actor_external_id,
    })
}

/// Build the [`ChannelActionContext`] the reply-draft op dispatches through:
/// the operation router + broadcaster from the app state plus the resolved
/// scope. Mirrors how the distill/classify workers construct their router LLMs.
fn build_channel_action_context(
    api: &ChannelAssistApi,
    principal: &str,
    workspace: &str,
) -> std::result::Result<ChannelActionContext, HttpResponse> {
    let Some(router) = api.operation_router.clone() else {
        return Err(err_json(
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "channel actions unavailable: operation router is not configured",
        ));
    };
    Ok(ChannelActionContext {
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        router,
        broadcaster: api.event_broadcaster.clone(),
    })
}

/// Build the [`ChannelActionRequest`] identity + compose-inputs from a resolved
/// action context. `body` and `hint` are supplied by the caller.
fn build_channel_action_request(
    resolved: &ResolvedActionContext,
    body: String,
    hint: Option<String>,
) -> ChannelActionRequest {
    let annotation = &resolved.annotation;
    let lane = resolved
        .thread
        .as_ref()
        .map(|t| t.lane)
        .unwrap_or(annotation.lane);
    ChannelActionRequest {
        provider: annotation.provider.clone(),
        account_alias: annotation.account_alias.clone(),
        lane,
        identity: ChannelIdentity {
            external_conversation_id: annotation.thread_id.clone(),
            external_message_id: annotation.evidence_message_id.clone(),
            actor_external_id: resolved.actor_external_id.clone(),
            actor_display_name: resolved
                .thread
                .as_ref()
                .and_then(|t| t.latest_from_name.clone()),
            direction: None,
        },
        reply_to_message_id: annotation.evidence_message_id.clone(),
        body,
        subject: resolved.subject.clone(),
        sender: resolved.sender.clone(),
        latest_message: resolved.latest_message.clone(),
        thread_summary: resolved.thread_summary.clone(),
        hint,
        provider_metadata: None,
    }
}

/// Resolve the provider's action adapter and confirm it advertises `action_id`.
/// 404 when the provider ships no adapter; 400 when the action is unknown.
fn action_adapter_or_response<'a>(
    adapters: &'a [Box<dyn adapter_registry::ChannelAdapter>],
    provider: &str,
    action_id: &str,
) -> std::result::Result<&'a dyn adapter_registry::ChannelActionAdapter, HttpResponse> {
    let adapter = adapters
        .iter()
        .find(|adapter| adapter.provider() == provider)
        .and_then(|adapter| adapter.action_adapter())
        .ok_or_else(|| {
            err_json(
                actix_web::http::StatusCode::NOT_FOUND,
                format!("provider '{provider}' exposes no channel actions"),
            )
        })?;
    if !adapter
        .available_actions()
        .iter()
        .any(|descriptor| descriptor.id == action_id)
    {
        return Err(err_json(
            actix_web::http::StatusCode::BAD_REQUEST,
            format!("unknown channel action '{action_id}' for provider '{provider}'"),
        ));
    }
    Ok(adapter)
}

/// Optional JSON body for `compose` — an owner redraft hint.
#[derive(Debug, Deserialize, Default)]
pub struct ChannelActionComposeBody {
    #[serde(default)]
    pub hint: Option<String>,
}

/// `POST /channel-assist/annotations/{id}/action/{action_id}/compose` — produce
/// an editable draft for a channel action. Resolves the annotation's thread +
/// latest message + summary, dispatches the provider adapter's local
/// reply-draft op, persists the draft, and returns `{compose_id, text}`.
pub async fn post_channel_assist_action_compose_handler(
    api: web::Data<ChannelAssistApi>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<ScopeQuery>,
    body: web::Bytes,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let (annotation_id, action_id) = path.into_inner();
    let hint = serde_json::from_slice::<ChannelActionComposeBody>(&body)
        .ok()
        .and_then(|b| b.hint)
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty());

    let resolved = match resolve_action_context(&api, &principal, &workspace, &annotation_id).await
    {
        Ok(resolved) => resolved,
        Err(resp) => return resp,
    };

    let adapters = default_channel_adapters();
    let adapter =
        match action_adapter_or_response(&adapters, &resolved.annotation.provider, &action_id) {
            Ok(adapter) => adapter,
            Err(resp) => return resp,
        };

    let ctx = match build_channel_action_context(&api, &principal, &workspace) {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    let action_req = build_channel_action_request(&resolved, String::new(), hint);

    let draft = match adapter.compose(&action_id, &ctx, &action_req).await {
        Ok(draft) => draft,
        Err(err) => {
            // Compose failures are the owner's problem to see (e.g. no local
            // model bound, or no message to reply to) — surface the message.
            return err_json(actix_web::http::StatusCode::BAD_REQUEST, err);
        },
    };

    let compose_id = Uuid::new_v4().to_string();
    if let Err(err) = api
        .store
        .put_channel_action_draft(
            &principal,
            &workspace,
            magician_comms::channel_assist::store::ChannelActionDraftRow {
                compose_id: compose_id.clone(),
                annotation_id: annotation_id.clone(),
                action_id: action_id.clone(),
                text: draft.text.clone(),
                created_at: now_millis(),
            },
        )
        .await
    {
        return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err);
    }

    HttpResponse::Ok().json(serde_json::json!({
        "compose_id": compose_id,
        "text": draft.text,
    }))
}

/// Body for `commit` — the (possibly edited) `body`, or the `compose_id` of a
/// previously composed draft to send verbatim.
#[derive(Debug, Deserialize, Default)]
pub struct ChannelActionCommitBody {
    #[serde(default)]
    pub compose_id: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub attribution: Option<AttentionOutcomeAttribution>,
}

/// `POST /channel-assist/annotations/{id}/action/{action_id}/commit` — execute
/// a channel action. Uses the request `body` when the owner edited the draft,
/// else loads the composed text by `compose_id`. On success, transitions the
/// annotation to `approved` (logging `helpful` feedback) and returns the
/// provider result.
pub async fn post_channel_assist_action_commit_handler(
    api: web::Data<ChannelAssistApi>,
    learning: Option<web::Data<AttentionLearningService>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<ScopeQuery>,
    body: web::Bytes,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let (annotation_id, action_id) = path.into_inner();
    let commit_body = serde_json::from_slice::<ChannelActionCommitBody>(&body).unwrap_or_default();
    let feedback_event_id = commit_body
        .event_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let feedback_attribution = commit_body.attribution.clone();
    let learning_cohort = if let Some(learning) = learning.as_ref() {
        load_follow_up_learning_cohort(
            &api.store,
            &principal,
            &workspace,
            learning.rescore_limit(),
            Some(&annotation_id),
        )
        .await
        .unwrap_or_default()
    } else {
        Vec::new()
    };

    let resolved = match resolve_action_context(&api, &principal, &workspace, &annotation_id).await
    {
        Ok(resolved) => resolved,
        Err(resp) => return resp,
    };

    let adapters = default_channel_adapters();
    let adapter =
        match action_adapter_or_response(&adapters, &resolved.annotation.provider, &action_id) {
            Ok(adapter) => adapter,
            Err(resp) => return resp,
        };
    let needs_compose = adapter
        .available_actions()
        .into_iter()
        .find(|descriptor| descriptor.id == action_id)
        .is_some_and(|descriptor| descriptor.needs_compose);

    // Resolve the text to send: an explicit (edited) body wins; else the stored
    // compose draft. Direct actions intentionally accept an empty body; their
    // provider adapter owns the action-specific request contract.
    let text = match commit_body
        .body
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        Some(edited) => edited.to_string(),
        None => {
            match commit_body
                .compose_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
            {
                None if !needs_compose => String::new(),
                None => {
                    return err_json(
                        actix_web::http::StatusCode::BAD_REQUEST,
                        "commit requires either a non-empty 'body' or a 'compose_id'",
                    )
                },
                Some(compose_id) => match api
                    .store
                    .get_channel_action_draft(&principal, &workspace, compose_id)
                    .await
                {
                    Ok(Some(draft)) => draft.text,
                    Ok(None) => {
                        return err_json(
                            actix_web::http::StatusCode::NOT_FOUND,
                            format!("compose draft not found: {compose_id}"),
                        );
                    },
                    Err(err) => {
                        return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err);
                    },
                },
            }
        },
    };

    let ctx = match build_channel_action_context(&api, &principal, &workspace) {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    let action_req = build_channel_action_request(&resolved, text, None);

    let result = match adapter.commit(&action_id, &ctx, &action_req).await {
        Ok(result) => result,
        Err(err) => {
            return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err);
        },
    };

    // Best-effort: transition the annotation to `approved` + log helpful
    // feedback (like `approve`). The send already succeeded, so a transition
    // conflict (e.g. not `needs_approval`) must not fail the whole action.
    if resolved.annotation.state == MailAnnotationState::NeedsApproval {
        let transition_recorded = matches!(
            api.store
                .transition_annotation_if_state(
                    &principal,
                    &workspace,
                    &annotation_id,
                    MailAnnotationState::NeedsApproval,
                    MailAnnotationState::Approved,
                    MailAssistActor::User,
                    Some(serde_json::json!({
                        "action": "channel_action_commit",
                        "action_id": action_id,
                    })),
                    Some(AnnotationTransitionFeedback {
                        event_id: Some(feedback_event_id.clone()),
                        verdict: MailFeedbackVerdict::Helpful,
                        comment: None,
                    }),
                    now_millis(),
                )
                .await,
            Ok(AnnotationTransitionResult::Applied(_))
        );
        if !transition_recorded {
            // The provider side effect already succeeded. Preserve the shared
            // event id in the append-only audit log so the background repair
            // tail can reconstruct the canonical learning outcome.
            let _ = api
                .store
                .append_feedback(
                    &principal,
                    &workspace,
                    MailAssistUserFeedback {
                        schema_version: resolved.annotation.schema_version,
                        id: feedback_event_id.clone(),
                        annotation_id: resolved.annotation.id.clone(),
                        provider: resolved.annotation.provider.clone(),
                        account_alias: resolved.annotation.account_alias.clone(),
                        thread_id: Some(resolved.annotation.thread_id.clone()),
                        verdict: MailFeedbackVerdict::Helpful,
                        comment: None,
                        actor: MailAssistActor::User,
                        created_at: now_millis(),
                    },
                )
                .await;
        }
    } else {
        // A provider action may be valid after another lifecycle transition.
        // Still leave a repairable owner-feedback event for this exact action.
        let _ = api
            .store
            .append_feedback(
                &principal,
                &workspace,
                MailAssistUserFeedback {
                    schema_version: resolved.annotation.schema_version,
                    id: feedback_event_id.clone(),
                    annotation_id: resolved.annotation.id.clone(),
                    provider: resolved.annotation.provider.clone(),
                    account_alias: resolved.annotation.account_alias.clone(),
                    thread_id: Some(resolved.annotation.thread_id.clone()),
                    verdict: MailFeedbackVerdict::Helpful,
                    comment: None,
                    actor: MailAssistActor::User,
                    created_at: now_millis(),
                },
            )
            .await;
    }

    let receipt = record_follow_up_attention_outcome(
        learning.as_ref().map(|learning| learning.get_ref()),
        &principal,
        &workspace,
        &annotation_id,
        learning_cohort,
        Some(feedback_event_id),
        feedback_attribution,
        AttentionOutcomeKind::ActionCompleted,
        None,
        now_millis(),
    )
    .await;
    response_with_feedback_receipt(
        serde_json::json!({
            "ok": true,
            "result": result,
        }),
        receipt,
    )
}

/// `POST /channel-assist/annotations/{id}/snooze` — drop the card without
/// deleting: transition `needs_approval` → `classified` (a quiet chip). The
/// annotation persists, so the thread is never re-classified;
/// resurface-on-material-change is Phase 7.
pub async fn post_channel_assist_annotation_snooze_handler(
    api: web::Data<ChannelAssistApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let annotation_id = path.into_inner();
    match api
        .store
        .get_annotation(&principal, &workspace, &annotation_id)
        .await
    {
        Ok(Some(a)) if a.state == MailAnnotationState::NeedsApproval => {},
        Ok(Some(a)) => {
            return err_json(
                actix_web::http::StatusCode::CONFLICT,
                format!(
                    "annotation is not awaiting approval (state: {})",
                    a.state.as_db_str()
                ),
            );
        },
        Ok(None) => {
            return err_json(
                actix_web::http::StatusCode::NOT_FOUND,
                format!("annotation not found: {annotation_id}"),
            );
        },
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    }
    let result = match api
        .store
        .transition_annotation_if_state(
            &principal,
            &workspace,
            &annotation_id,
            MailAnnotationState::NeedsApproval,
            MailAnnotationState::Classified,
            MailAssistActor::User,
            Some(serde_json::json!({ "action": "snooze" })),
            None,
            now_millis(),
        )
        .await
    {
        Ok(result) => result,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let updated = match transition_result_or_response(&annotation_id, result) {
        Ok(updated) => updated,
        Err(resp) => return resp,
    };
    HttpResponse::Ok().json(updated)
}

/// `POST /channel-assist/annotations/{id}/review` — explicitly re-open a
/// stale draft after the owner has reviewed the newer evidence. No draft is
/// sent or inserted; this only returns the recommendation to the approval
/// lane so the next action remains human-controlled.
pub async fn post_channel_assist_annotation_review_handler(
    api: web::Data<ChannelAssistApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let annotation_id = path.into_inner();
    let result = match api
        .store
        .transition_annotation_if_state(
            &principal,
            &workspace,
            &annotation_id,
            MailAnnotationState::Stale,
            MailAnnotationState::NeedsApproval,
            MailAssistActor::User,
            Some(serde_json::json!({
                "action": "review_stale_draft",
                "review_required": false,
            })),
            None,
            now_millis(),
        )
        .await
    {
        Ok(result) => result,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    match transition_result_or_response(&annotation_id, result) {
        Ok(updated) => HttpResponse::Ok().json(updated),
        Err(resp) => resp,
    }
}

/// `POST /channel-assist/annotations/{id}/acknowledge` — a NEUTRAL "seen, no
/// action needed" response. Transitions `needs_approval` → `acknowledged` and
/// creates NO task. Unlike [`post_channel_assist_annotation_useful_handler`], it
/// logs NO learning feedback — it means "I've seen this", not "this was worth
/// surfacing", so it must not skew the positive/negative signal. Only valid from
/// `needs_approval` (else 409).
pub async fn post_channel_assist_annotation_acknowledge_handler(
    api: web::Data<ChannelAssistApi>,
    learning: Option<web::Data<AttentionLearningService>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    body: web::Bytes,
) -> HttpResponse {
    let event = serde_json::from_slice::<FeedbackEventBody>(&body).unwrap_or_default();
    acknowledge_like_transition(
        &api,
        learning.as_ref().map(|learning| learning.get_ref()),
        &req,
        path.into_inner(),
        &query,
        "acknowledge",
        event.event_id,
        event.attribution,
        None,
    )
    .await
}

/// `POST /channel-assist/annotations/{id}/useful` — a POSITIVE "this was worth
/// surfacing" response with no immediate action. Transitions `needs_approval` →
/// `acknowledged` (no task) and logs a `helpful` feedback — the positive learning
/// signal (the mirror of dismiss; the same signal as "do it" minus the task). Only
/// valid from `needs_approval` (else 409).
pub async fn post_channel_assist_annotation_useful_handler(
    api: web::Data<ChannelAssistApi>,
    learning: Option<web::Data<AttentionLearningService>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    body: web::Bytes,
) -> HttpResponse {
    let event = serde_json::from_slice::<FeedbackEventBody>(&body).unwrap_or_default();
    acknowledge_like_transition(
        &api,
        learning.as_ref().map(|learning| learning.get_ref()),
        &req,
        path.into_inner(),
        &query,
        "useful",
        event.event_id,
        event.attribution,
        Some(AnnotationTransitionFeedback {
            event_id: None,
            verdict: MailFeedbackVerdict::Helpful,
            comment: None,
        }),
    )
    .await
}

/// Shared `needs_approval` → `acknowledged` transition for the neutral
/// `acknowledge` and positive `useful` actions. The only difference is the
/// `action` metadata label and whether a `helpful` learning `feedback` is logged.
async fn acknowledge_like_transition(
    api: &ChannelAssistApi,
    learning: Option<&AttentionLearningService>,
    req: &HttpRequest,
    annotation_id: String,
    query: &ScopeQuery,
    action: &str,
    event_id: Option<String>,
    attribution: Option<AttentionOutcomeAttribution>,
    mut feedback: Option<AnnotationTransitionFeedback>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let event_id = event_id.unwrap_or_else(|| Uuid::new_v4().to_string());
    if let Some(feedback) = feedback.as_mut() {
        feedback.event_id = Some(event_id.clone());
    }
    let annotation = match api
        .store
        .get_annotation(&principal, &workspace, &annotation_id)
        .await
    {
        Ok(Some(a)) => a,
        Ok(None) => {
            return err_json(
                actix_web::http::StatusCode::NOT_FOUND,
                format!("annotation not found: {annotation_id}"),
            );
        },
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    if annotation.state != MailAnnotationState::NeedsApproval {
        return err_json(
            actix_web::http::StatusCode::CONFLICT,
            format!(
                "annotation is not awaiting approval (state: {})",
                annotation.state.as_db_str()
            ),
        );
    }
    let learning_cohort = if let Some(learning) = learning {
        load_follow_up_learning_cohort(
            &api.store,
            &principal,
            &workspace,
            learning.rescore_limit(),
            Some(&annotation_id),
        )
        .await
        .unwrap_or_default()
    } else {
        Vec::new()
    };
    let result = match api
        .store
        .transition_annotation_if_state(
            &principal,
            &workspace,
            &annotation_id,
            MailAnnotationState::NeedsApproval,
            MailAnnotationState::Acknowledged,
            MailAssistActor::User,
            Some(serde_json::json!({ "action": action })),
            feedback,
            now_millis(),
        )
        .await
    {
        Ok(result) => result,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    match transition_result_or_response(&annotation_id, result) {
        Ok(updated) => {
            let outcome = if action == "useful" {
                AttentionOutcomeKind::Useful
            } else {
                AttentionOutcomeKind::NeutralSeen
            };
            let receipt = record_follow_up_attention_outcome(
                learning,
                &principal,
                &workspace,
                &annotation_id,
                learning_cohort,
                Some(event_id),
                attribution,
                outcome,
                None,
                now_millis(),
            )
            .await;
            response_with_feedback_receipt(updated, receipt)
        },
        Err(resp) => resp,
    }
}

#[derive(Debug, Deserialize, Default)]
struct FeedbackEventBody {
    #[serde(default)]
    event_id: Option<String>,
    #[serde(default)]
    attribution: Option<AttentionOutcomeAttribution>,
}

#[derive(Debug, Deserialize)]
pub struct FeedbackBody {
    pub verdict: MailFeedbackVerdict,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub attribution: Option<AttentionOutcomeAttribution>,
}

/// `POST /channel-assist/annotations/{id}/feedback` — append a typed
/// [`MailAssistUserFeedback`] as a `feedback` audit event (actor=user).
/// Append-only: repeated feedback appends more events, nothing is
/// overwritten.
pub async fn post_channel_assist_annotation_feedback_handler(
    api: web::Data<ChannelAssistApi>,
    learning: Option<web::Data<AttentionLearningService>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    body: web::Json<FeedbackBody>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let annotation_id = path.into_inner();
    let learning_cohort = if let Some(learning) = learning.as_ref() {
        load_follow_up_learning_cohort(
            &api.store,
            &principal,
            &workspace,
            learning.rescore_limit(),
            Some(&annotation_id),
        )
        .await
        .unwrap_or_default()
    } else {
        Vec::new()
    };
    let annotation = match api
        .store
        .get_annotation(&principal, &workspace, &annotation_id)
        .await
    {
        Ok(Some(annotation)) => annotation,
        Ok(None) => {
            return err_json(
                actix_web::http::StatusCode::NOT_FOUND,
                format!("annotation not found: {annotation_id}"),
            );
        },
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let feedback = MailAssistUserFeedback {
        schema_version: MAIL_ASSIST_SCHEMA_VERSION,
        id: body
            .event_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        annotation_id: annotation.id.clone(),
        provider: annotation.provider.clone(),
        account_alias: annotation.account_alias.clone(),
        thread_id: Some(annotation.thread_id.clone()),
        verdict: body.verdict,
        comment: body
            .comment
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_string),
        actor: MailAssistActor::User,
        created_at: now_millis(),
    };
    match api
        .store
        .append_feedback(&principal, &workspace, feedback.clone())
        .await
    {
        Ok(()) => {
            let reason = feedback.comment.clone();
            let outcome = match feedback.verdict {
                MailFeedbackVerdict::Helpful => Some(AttentionOutcomeKind::Useful),
                MailFeedbackVerdict::NotHelpful => Some(match reason.as_deref().map(str::trim) {
                    Some("wrong_classification" | "not_actionable") => {
                        AttentionOutcomeKind::NotActionable
                    },
                    Some("already_handled") => AttentionOutcomeKind::Obsolete,
                    Some("delegated" | "wrong_owner") => AttentionOutcomeKind::NotOwner,
                    Some("duplicate") => AttentionOutcomeKind::DuplicateOf,
                    Some("spam" | "not_relevant") | None | Some(_) => {
                        AttentionOutcomeKind::Irrelevant
                    },
                }),
                MailFeedbackVerdict::WrongLabel => Some(AttentionOutcomeKind::NotActionable),
                MailFeedbackVerdict::Other => None,
            };
            let receipt = if let Some(outcome) = outcome {
                record_follow_up_attention_outcome(
                    learning.as_ref().map(|learning| learning.get_ref()),
                    &principal,
                    &workspace,
                    &annotation_id,
                    learning_cohort,
                    Some(feedback.id.clone()),
                    body.attribution.clone(),
                    outcome,
                    reason,
                    feedback.created_at,
                )
                .await
            } else {
                None
            };
            let mut response = response_with_feedback_receipt(feedback, receipt);
            response.head_mut().status = actix_web::http::StatusCode::CREATED;
            response
        },
        Err(err) => err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    }
}

#[derive(Debug, Deserialize)]
pub struct WritingPreferenceFeedbackBody {
    pub scope: WritingPreferenceScopeKind,
    /// Exact owner-authored preference. May be combined with edit feedback.
    #[serde(default)]
    pub statement: Option<String>,
    /// Optional in-memory edit pair. Neither value is persisted or logged;
    /// only deterministic style statements derived from the pair are stored.
    #[serde(default)]
    pub original_draft: Option<String>,
    #[serde(default)]
    pub edited_draft: Option<String>,
    /// Explicit owner promotion. Candidates are otherwise shown for review.
    #[serde(default)]
    pub promote: bool,
}

async fn writing_preference_context(
    api: &ChannelAssistApi,
    principal: &str,
    workspace: &str,
    annotation_id: &str,
) -> Result<(MailThreadAnnotation, MailThreadRecord), HttpResponse> {
    let annotation = match api
        .store
        .get_annotation(principal, workspace, annotation_id)
        .await
    {
        Ok(Some(annotation)) => annotation,
        Ok(None) => {
            return Err(err_json(
                actix_web::http::StatusCode::NOT_FOUND,
                format!("annotation not found: {annotation_id}"),
            ));
        },
        Err(error) => {
            return Err(err_json(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                error,
            ));
        },
    };
    let threads = match api
        .store
        .get_threads_by_ids(
            principal,
            workspace,
            &annotation.provider,
            &annotation.account_alias,
            std::slice::from_ref(&annotation.thread_id),
        )
        .await
    {
        Ok(threads) => threads,
        Err(error) => {
            return Err(err_json(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                error,
            ));
        },
    };
    let Some(thread) = threads.into_iter().next() else {
        return Err(err_json(
            actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
            "annotation thread metadata is unavailable",
        ));
    };
    Ok((annotation, thread))
}

/// `GET /channel-assist/annotations/{id}/writing-preferences` — exact active
/// candidate/promoted statements for this sender and domain.
pub async fn get_channel_assist_writing_preferences_handler(
    api: web::Data<ChannelAssistApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let annotation_id = path.into_inner();
    let (annotation, thread) =
        match writing_preference_context(api.get_ref(), &principal, &workspace, &annotation_id)
            .await
        {
            Ok(context) => context,
            Err(response) => return response,
        };
    let domain = sender_domain(thread.latest_from_address.as_deref());
    match api
        .store
        .list_writing_preferences(
            &principal,
            &workspace,
            &annotation.provider,
            &annotation.account_alias,
            thread.latest_from_address.as_deref(),
            domain.as_deref(),
        )
        .await
    {
        Ok(items) => HttpResponse::Ok().json(serde_json::json!({ "items": items })),
        Err(error) => err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

/// `POST /channel-assist/annotations/{id}/writing-preferences` — learn exact
/// sender/domain statements from explicit feedback or a before/after draft
/// edit. Raw drafts stay request-local and are never persisted.
pub async fn post_channel_assist_writing_preferences_handler(
    api: web::Data<ChannelAssistApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    body: web::Json<WritingPreferenceFeedbackBody>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let annotation_id = path.into_inner();
    let (annotation, thread) =
        match writing_preference_context(api.get_ref(), &principal, &workspace, &annotation_id)
            .await
        {
            Ok(context) => context,
            Err(response) => return response,
        };
    let scope_value = match body.scope {
        WritingPreferenceScopeKind::Sender => thread
            .latest_from_address
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase),
        WritingPreferenceScopeKind::Domain => sender_domain(thread.latest_from_address.as_deref()),
    };
    let Some(scope_value) = scope_value else {
        return err_json(
            actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
            "sender metadata is unavailable for the requested preference scope",
        );
    };

    const MAX_EDIT_CHARS: usize = 100_000;
    if body
        .original_draft
        .as_ref()
        .is_some_and(|value| value.chars().count() > MAX_EDIT_CHARS)
        || body
            .edited_draft
            .as_ref()
            .is_some_and(|value| value.chars().count() > MAX_EDIT_CHARS)
    {
        return err_json(
            actix_web::http::StatusCode::PAYLOAD_TOO_LARGE,
            "draft edit feedback exceeds the 100000 character in-memory limit",
        );
    }

    let mut statements = Vec::new();
    if let Some(statement) = body.statement.as_deref() {
        match normalize_statement(statement) {
            Ok(statement) => statements.push(statement),
            Err(error) => return err_json(actix_web::http::StatusCode::BAD_REQUEST, error),
        }
    }
    match (&body.original_draft, &body.edited_draft) {
        (Some(original), Some(edited)) => {
            statements.extend(derive_edit_preferences(original, edited));
        },
        (None, None) => {},
        _ => {
            return err_json(
                actix_web::http::StatusCode::BAD_REQUEST,
                "original_draft and edited_draft must be provided together",
            );
        },
    }
    statements.sort();
    statements.dedup();
    if statements.is_empty() {
        return err_json(
            actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
            "no writing preference could be learned from this feedback",
        );
    }

    let mut stored = Vec::new();
    for statement in statements {
        let candidate = match api
            .store
            .upsert_writing_preference(
                &principal,
                &workspace,
                &annotation.provider,
                &annotation.account_alias,
                body.scope,
                &scope_value,
                &statement,
                WritingPreferenceStatus::Candidate,
                Some(&annotation_id),
                now_millis(),
            )
            .await
        {
            Ok(preference) => preference,
            Err(error) => {
                return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error);
            },
        };
        if body.promote && candidate.status != WritingPreferenceStatus::Promoted {
            if let Err(error) = promote_to_memory(&principal, &workspace, &candidate).await {
                return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error);
            }
            match api
                .store
                .set_writing_preference_status(
                    &principal,
                    &workspace,
                    &candidate.id,
                    WritingPreferenceStatus::Promoted,
                    now_millis(),
                )
                .await
            {
                Ok(Some(preference)) => stored.push(preference),
                Ok(None) => {
                    let _ = remove_from_memory(&principal, &workspace, &candidate).await;
                    return err_json(
                        actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                        "writing preference disappeared during promotion",
                    );
                },
                Err(error) => {
                    let _ = remove_from_memory(&principal, &workspace, &candidate).await;
                    return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error);
                },
            }
        } else {
            stored.push(candidate);
        }
    }
    HttpResponse::Created().json(serde_json::json!({
        "items": stored,
        "raw_drafts_persisted": false,
    }))
}

pub async fn post_channel_assist_writing_preference_promote_handler(
    api: web::Data<ChannelAssistApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let id = path.into_inner();
    let preference = match api
        .store
        .get_writing_preference(&principal, &workspace, &id)
        .await
    {
        Ok(Some(preference)) => preference,
        Ok(None) => {
            return err_json(
                actix_web::http::StatusCode::NOT_FOUND,
                format!("writing preference not found: {id}"),
            );
        },
        Err(error) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error),
    };
    if preference.status != WritingPreferenceStatus::Promoted {
        if let Err(error) = promote_to_memory(&principal, &workspace, &preference).await {
            return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error);
        }
    }
    match api
        .store
        .set_writing_preference_status(
            &principal,
            &workspace,
            &id,
            WritingPreferenceStatus::Promoted,
            now_millis(),
        )
        .await
    {
        Ok(Some(preference)) => HttpResponse::Ok().json(preference),
        Ok(None) => {
            if preference.status != WritingPreferenceStatus::Promoted {
                let _ = remove_from_memory(&principal, &workspace, &preference).await;
            }
            err_json(
                actix_web::http::StatusCode::NOT_FOUND,
                format!("writing preference not found: {id}"),
            )
        },
        Err(error) => {
            if preference.status != WritingPreferenceStatus::Promoted {
                let _ = remove_from_memory(&principal, &workspace, &preference).await;
            }
            err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error)
        },
    }
}

pub async fn post_channel_assist_writing_preference_dismiss_handler(
    api: web::Data<ChannelAssistApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let id = path.into_inner();
    let preference = match api
        .store
        .get_writing_preference(&principal, &workspace, &id)
        .await
    {
        Ok(Some(preference)) => preference,
        Ok(None) => {
            return err_json(
                actix_web::http::StatusCode::NOT_FOUND,
                format!("writing preference not found: {id}"),
            );
        },
        Err(error) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error),
    };
    if preference.status == WritingPreferenceStatus::Promoted {
        if let Err(error) = remove_from_memory(&principal, &workspace, &preference).await {
            return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error);
        }
    }
    match api
        .store
        .set_writing_preference_status(
            &principal,
            &workspace,
            &id,
            WritingPreferenceStatus::Dismissed,
            now_millis(),
        )
        .await
    {
        Ok(Some(preference)) => HttpResponse::Ok().json(preference),
        Ok(None) => {
            if preference.status == WritingPreferenceStatus::Promoted {
                let _ = promote_to_memory(&principal, &workspace, &preference).await;
            }
            err_json(
                actix_web::http::StatusCode::NOT_FOUND,
                format!("writing preference not found: {id}"),
            )
        },
        Err(error) => {
            if preference.status == WritingPreferenceStatus::Promoted {
                let _ = promote_to_memory(&principal, &workspace, &preference).await;
            }
            err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error)
        },
    }
}

#[derive(Debug, Deserialize)]
pub struct SeedAnnotationBody {
    /// Account alias — required.
    #[serde(default)]
    pub account: Option<String>,
    /// Provider thread id to annotate — required. A real synced id renders in
    /// place; an invented id gets a seed-origin thread row created for it.
    #[serde(default)]
    pub thread_id: Option<String>,
    /// Provider; only `gmail` exists in Phase 1 (defaulted).
    #[serde(default)]
    pub provider: Option<String>,
    /// Classifier-style label; defaults to `follow_up_candidate`.
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
    /// Subject for a thread row this seed materializes (ignored when the
    /// thread already exists); defaults to [`FIXTURE_SEED_SUBJECT`].
    #[serde(default)]
    pub subject: Option<String>,
    /// Mark the materialized thread row sensitive-suppressed: subject is
    /// stored as the redaction placeholder, ids retained (acceptance §4).
    #[serde(default)]
    pub sensitive_suppressed: bool,
}

/// Validated seed parameters (pure — unit-testable without a store).
#[derive(Debug, PartialEq)]
struct SeedRequest {
    provider: String,
    account: String,
    thread_id: String,
    label: String,
    reason: Option<String>,
    confidence: Option<f64>,
    subject: String,
    sensitive_suppressed: bool,
}

fn validate_seed_request(body: &SeedAnnotationBody) -> Result<SeedRequest, String> {
    let account = body
        .account
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .ok_or_else(|| "`account` is required".to_string())?;
    let thread_id = body
        .thread_id
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "`thread_id` is required".to_string())?;
    if let Some(confidence) = body.confidence {
        if !(0.0..=1.0).contains(&confidence) {
            return Err(format!(
                "`confidence` must be within 0.0..=1.0, got {confidence}"
            ));
        }
    }
    let subject = if body.sensitive_suppressed {
        magician_comms::channel_assist::types::REDACTED_SUBJECT_PLACEHOLDER.to_string()
    } else {
        body.subject
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(FIXTURE_SEED_SUBJECT)
            .to_string()
    };
    Ok(SeedRequest {
        provider: body
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .unwrap_or(GMAIL_PROVIDER)
            .to_string(),
        account: account.to_string(),
        thread_id: thread_id.to_string(),
        label: body
            .label
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .unwrap_or("follow_up_candidate")
            .to_string(),
        reason: body
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(str::to_string),
        confidence: body.confidence,
        subject,
        sensitive_suppressed: body.sensitive_suppressed,
    })
}

/// `POST /channel-assist/annotations/seed` — create a fixture annotation
/// (provenance [`FIXTURE_SEED_PROVENANCE`]) on a thread, materializing a
/// seed-origin thread row first when the sync hasn't observed the id.
/// This is the parent-plan First-Slice #5 path: it lets the extension/UI
/// render one known annotation before any classifier exists. The audit
/// actor is `worker` — the seed stands in for the Phase-2 classifier.
pub async fn post_channel_assist_annotation_seed_handler(
    api: web::Data<ChannelAssistApi>,
    req: HttpRequest,
    query: web::Query<ScopeQuery>,
    body: web::Json<SeedAnnotationBody>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let seed = match validate_seed_request(&body) {
        Ok(seed) => seed,
        Err(message) => return err_json(actix_web::http::StatusCode::BAD_REQUEST, message),
    };

    let existing = match api
        .store
        .get_threads_by_ids(
            &principal,
            &workspace,
            &seed.provider,
            &seed.account,
            std::slice::from_ref(&seed.thread_id),
        )
        .await
    {
        Ok(existing) => existing,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let now = now_millis();
    let thread_created = existing.is_empty();
    if thread_created {
        let record = MailThreadRecord {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: seed.provider.clone(),
            account_alias: seed.account.clone(),
            account_email: None,
            thread_id: seed.thread_id.clone(),
            lane: ChannelLane::default(),
            subject: Some(seed.subject.clone()),
            latest_summary: None,
            latest_from_name: None,
            latest_from_address: None,
            recipient_domains: Vec::new(),
            label_ids: Vec::new(),
            message_count: 1,
            last_message_at: Some(now),
            provider_cursor: None,
            sensitive_suppressed: seed.sensitive_suppressed,
            origin: MailRecordOrigin::Seed,
            first_observed_at: now,
            last_observed_at: now,
        };
        if let Err(err) = api
            .store
            .upsert_thread(&principal, &workspace, record)
            .await
        {
            return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err);
        }
    }

    let annotation = MailThreadAnnotation {
        schema_version: MAIL_ASSIST_SCHEMA_VERSION,
        id: Uuid::new_v4().to_string(),
        provider: seed.provider.clone(),
        account_alias: seed.account.clone(),
        thread_id: seed.thread_id.clone(),
        // Placeholder — the store inherits the REAL lane from the thread
        // row at create time (lane flows account → thread → annotation).
        lane: ChannelLane::default(),
        state: MailAnnotationState::Classified,
        label: Some(seed.label.clone()),
        confidence: seed.confidence,
        reason: seed.reason.clone(),
        evidence_refs: Vec::new(),
        evidence_message_id: None,
        evidence_message_at: None,
        classification_input_revision: None,
        semantic_features: None,
        proposed_action: None,
        provenance: Some(FIXTURE_SEED_PROVENANCE.to_string()),
        created_at: now,
        updated_at: now,
    };
    match api
        .store
        .create_annotation(&principal, &workspace, annotation, MailAssistActor::Worker)
        .await
    {
        // The store returns the annotation AS STORED (lane inherited from
        // the thread row) — echo that, not the request-side struct.
        Ok(created) => HttpResponse::Created().json(serde_json::json!({
            "annotation": created,
            "thread_created": thread_created,
        })),
        Err(err) => err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use magician_comms::channel_assist::types::REDACTED_SUBJECT_PLACEHOLDER;

    #[test]
    fn pair_correction_contract_parses_exact_revision_bound_owner_label() {
        let body: AttentionPairCorrectionBody = serde_json::from_value(serde_json::json!({
            "event_id": "event-1",
            "surface": "follow_up",
            "left": {"candidate_id": "a", "source_revision": "distill:1"},
            "right": {"candidate_id": "b", "source_revision": "distill:2"},
            "label": "not_duplicate",
            "confidence": 1.0
        }))
        .unwrap();
        assert_eq!(body.surface, AttentionSurface::FollowUp);
        assert_eq!(body.label, AttentionPairLabelKind::NotDuplicate);
        assert_eq!(body.left.source_revision.as_deref(), Some("distill:1"));
    }

    #[test]
    fn parse_thread_ids_splits_trims_and_dedups_preserving_order() {
        let ids = parse_thread_ids(" t-b , t-a ,, t-b ,t-c,").unwrap();
        assert_eq!(ids, vec!["t-b", "t-a", "t-c"]);
    }

    #[test]
    fn parse_thread_ids_rejects_effectively_empty_input() {
        assert!(parse_thread_ids("").is_err());
        assert!(parse_thread_ids(" , ,, ").is_err());
    }

    #[test]
    fn parse_thread_ids_enforces_the_batch_cap() {
        let at_cap = (0..MAX_ANNOTATION_THREAD_IDS)
            .map(|i| format!("t-{i}"))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            parse_thread_ids(&at_cap).unwrap().len(),
            MAX_ANNOTATION_THREAD_IDS
        );

        let over_cap = format!("{at_cap},t-overflow");
        let err = parse_thread_ids(&over_cap).unwrap_err();
        assert!(err.contains("batch too large"), "unexpected error: {err}");
        // Duplicates collapse BEFORE the cap check — a padded list of
        // repeats is not an over-cap request.
        let repeats = vec!["t-same"; MAX_ANNOTATION_THREAD_IDS + 20].join(",");
        assert_eq!(parse_thread_ids(&repeats).unwrap(), vec!["t-same"]);
    }

    #[test]
    fn source_family_histogram_prefers_route_metadata_and_keeps_legacy_fallback() {
        let signals = vec![
            NeedsApprovalSourceSignal {
                label: Some("needs_reply".to_string()),
                proposed_action: None,
            },
            NeedsApprovalSourceSignal {
                label: Some("needs_reply".to_string()),
                proposed_action: Some(serde_json::json!({
                    "attention_source_family": "comms_ingest",
                    "follow_up_kind": "owner_owes",
                })),
            },
            NeedsApprovalSourceSignal {
                label: Some("follow_up".to_string()),
                proposed_action: None,
            },
        ];

        let histogram = follow_up_source_family_histogram(&signals);

        assert_eq!(histogram.get(FOLLOW_UP_SOURCE_COMMS_INGEST), Some(&2));
        assert_eq!(histogram.get(FOLLOW_UP_SOURCE_PROMISE), Some(&1));
    }

    #[test]
    fn seed_validation_requires_account_and_thread_id() {
        let body = SeedAnnotationBody {
            account: None,
            thread_id: Some("t-1".to_string()),
            provider: None,
            label: None,
            reason: None,
            confidence: None,
            subject: None,
            sensitive_suppressed: false,
        };
        assert!(validate_seed_request(&body)
            .unwrap_err()
            .contains("account"));

        let body = SeedAnnotationBody {
            account: Some("business".to_string()),
            thread_id: Some("   ".to_string()),
            provider: None,
            label: None,
            reason: None,
            confidence: None,
            subject: None,
            sensitive_suppressed: false,
        };
        assert!(validate_seed_request(&body)
            .unwrap_err()
            .contains("thread_id"));
    }

    #[test]
    fn seed_validation_applies_defaults_and_bounds_confidence() {
        let body = SeedAnnotationBody {
            account: Some(" business ".to_string()),
            thread_id: Some(" t-1 ".to_string()),
            provider: None,
            label: None,
            reason: Some("  ".to_string()),
            confidence: Some(0.9),
            subject: None,
            sensitive_suppressed: false,
        };
        let seed = validate_seed_request(&body).unwrap();
        assert_eq!(seed.provider, GMAIL_PROVIDER);
        assert_eq!(seed.account, "business");
        assert_eq!(seed.thread_id, "t-1");
        assert_eq!(seed.label, "follow_up_candidate");
        assert_eq!(seed.reason, None);
        assert_eq!(seed.subject, FIXTURE_SEED_SUBJECT);
        assert!(!seed.sensitive_suppressed);

        let out_of_range = SeedAnnotationBody {
            confidence: Some(1.5),
            ..body
        };
        assert!(validate_seed_request(&out_of_range)
            .unwrap_err()
            .contains("confidence"));
    }

    #[test]
    fn seed_validation_redacts_subject_when_sensitive_suppressed() {
        let body = SeedAnnotationBody {
            account: Some("business".to_string()),
            thread_id: Some("t-1".to_string()),
            provider: None,
            label: None,
            reason: None,
            confidence: None,
            // Even an explicit subject is replaced — the redaction
            // placeholder is the ONLY subject a suppressed row may carry.
            subject: Some("real sensitive subject".to_string()),
            sensitive_suppressed: true,
        };
        let seed = validate_seed_request(&body).unwrap();
        assert_eq!(seed.subject, REDACTED_SUBJECT_PLACEHOLDER);
        assert!(seed.sensitive_suppressed);
    }

    // ---- generic channel-action compose/commit (Task 5) ----

    use magician_comms::channel_assist::ingest_imessage::IMESSAGE_PROVIDER;

    fn action_annotation(provider: &str) -> MailThreadAnnotation {
        MailThreadAnnotation {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            id: "anno-1".to_string(),
            provider: provider.to_string(),
            account_alias: "default".to_string(),
            thread_id: "chat-1".to_string(),
            lane: ChannelLane::default(),
            state: MailAnnotationState::NeedsApproval,
            label: Some("needs_reply".to_string()),
            confidence: Some(0.9),
            reason: None,
            evidence_refs: vec!["message:msg-1".to_string()],
            evidence_message_id: Some("msg-1".to_string()),
            evidence_message_at: Some(1_700),
            classification_input_revision: None,
            semantic_features: None,
            proposed_action: None,
            provenance: None,
            created_at: 100,
            updated_at: 100,
        }
    }

    #[test]
    fn repeated_dismiss_is_idempotent_but_other_terminal_states_still_conflict() {
        let mut dismissed = action_annotation(GMAIL_PROVIDER);
        dismissed.state = MailAnnotationState::Dismissed;
        let dismissed_id = dismissed.id.clone();
        let resolved = dismiss_transition_result_or_response(
            &dismissed_id,
            AnnotationTransitionResult::UnexpectedState {
                current: dismissed,
                expected: MailAnnotationState::NeedsApproval,
            },
        )
        .expect("an already-dismissed annotation is a successful retry");
        assert_eq!(resolved.state, MailAnnotationState::Dismissed);

        let mut acknowledged = action_annotation(GMAIL_PROVIDER);
        acknowledged.state = MailAnnotationState::Acknowledged;
        let acknowledged_id = acknowledged.id.clone();
        let response = dismiss_transition_result_or_response(
            &acknowledged_id,
            AnnotationTransitionResult::UnexpectedState {
                current: acknowledged,
                expected: MailAnnotationState::NeedsApproval,
            },
        )
        .expect_err("a different terminal decision must remain a conflict");
        assert_eq!(response.status(), actix_web::http::StatusCode::CONFLICT);
    }

    #[test]
    fn available_actions_reflect_the_provider_adapter() {
        // iMessage ships a reply action; providers without an action adapter
        // report none.
        let imessage = available_actions_for_provider(IMESSAGE_PROVIDER);
        assert_eq!(imessage.len(), 1);
        assert_eq!(imessage[0].id, "reply");
        assert!(imessage[0].needs_compose);

        assert!(available_actions_for_provider(GMAIL_PROVIDER).is_empty());
        assert!(available_actions_for_provider("no-such-provider").is_empty());
    }

    #[test]
    fn action_adapter_lookup_rejects_unknown_provider_and_action() {
        let adapters = default_channel_adapters();

        // The Ok arm is a trait object (not Debug), so assert on the status of
        // the Err arm directly rather than via `unwrap_err`.
        let err_status = |result: std::result::Result<
            &dyn adapter_registry::ChannelActionAdapter,
            HttpResponse,
        >| {
            match result {
                Ok(_) => panic!("expected an error response"),
                Err(resp) => resp.status(),
            }
        };

        // Unknown provider -> 404 (no action adapter).
        assert_eq!(
            err_status(action_adapter_or_response(
                &adapters,
                "no-such-provider",
                "reply"
            )),
            actix_web::http::StatusCode::NOT_FOUND
        );

        // Provider without an action adapter -> 404.
        assert_eq!(
            err_status(action_adapter_or_response(
                &adapters,
                GMAIL_PROVIDER,
                "reply"
            )),
            actix_web::http::StatusCode::NOT_FOUND
        );

        // Known provider, unknown action -> 400.
        assert_eq!(
            err_status(action_adapter_or_response(
                &adapters,
                IMESSAGE_PROVIDER,
                "react"
            )),
            actix_web::http::StatusCode::BAD_REQUEST
        );

        // Known provider + known action resolves.
        assert!(action_adapter_or_response(&adapters, IMESSAGE_PROVIDER, "reply").is_ok());
    }

    #[test]
    fn build_request_maps_thread_identity_and_compose_inputs() {
        let resolved = ResolvedActionContext {
            annotation: action_annotation(IMESSAGE_PROVIDER),
            thread: None,
            latest_message: Some("Are we still on for Friday?".to_string()),
            subject: Some("Plans".to_string()),
            sender: Some("Asha <+14155551234>".to_string()),
            thread_summary: Some("Coordinating Friday plans.".to_string()),
            actor_external_id: Some("+14155551234".to_string()),
        };
        let req = build_channel_action_request(
            &resolved,
            "See you then!".to_string(),
            Some("keep it short".to_string()),
        );
        assert_eq!(req.provider, IMESSAGE_PROVIDER);
        assert_eq!(req.account_alias, "default");
        assert_eq!(req.identity.external_conversation_id, "chat-1");
        assert_eq!(
            req.identity.actor_external_id.as_deref(),
            Some("+14155551234")
        );
        assert_eq!(req.identity.external_message_id.as_deref(), Some("msg-1"));
        assert_eq!(req.body, "See you then!");
        assert_eq!(req.subject.as_deref(), Some("Plans"));
        assert_eq!(req.sender.as_deref(), Some("Asha <+14155551234>"));
        assert_eq!(
            req.latest_message.as_deref(),
            Some("Are we still on for Friday?")
        );
        assert_eq!(
            req.thread_summary.as_deref(),
            Some("Coordinating Friday plans.")
        );
        assert_eq!(req.hint.as_deref(), Some("keep it short"));
    }

    #[test]
    fn follow_up_projection_remains_default_on_and_can_be_explicitly_skipped() {
        let legacy: NeedsYouQuery = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(legacy.include_projection);
        let projection_owner: NeedsYouQuery =
            serde_json::from_value(serde_json::json!({ "include_projection": false })).unwrap();
        assert!(!projection_owner.include_projection);
        assert!(!should_compute_legacy_follow_up_projection(
            projection_owner.include_projection,
            false,
            true,
        ));
        assert!(should_compute_legacy_follow_up_projection(
            legacy.include_projection,
            false,
            true,
        ));
        assert!(!should_compute_legacy_follow_up_projection(
            legacy.include_projection,
            true,
            true,
        ));
        assert!(!should_load_follow_up_source_histogram(false));
        assert!(should_load_follow_up_source_histogram(true));
        assert!(!should_compute_follow_up_semantic_health(
            projection_owner.include_projection,
        ));
        assert!(should_compute_follow_up_semantic_health(
            legacy.include_projection,
        ));
    }
}

/// Run a contextual action against a message follow-up.
///
/// Deliberately the same request/response contract as the Worth-a-look lane:
/// a client that can already create a reminder there needs no new logic here,
/// only a new place to invoke it. The action service decides what is reachable
/// — a follow-up has no interaction registry behind it, so kinds that read a
/// resurfacing source are rejected rather than silently degraded.
pub async fn post_channel_follow_up_contextual_action_handler(
    actions: Option<web::Data<ResurfacingActionService>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    body: web::Json<ResurfacingContextualActionRequest>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let Some(actions) = actions else {
        return err_json(
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "follow-up contextual actions are unavailable",
        );
    };
    let annotation_id = path.into_inner();
    match actions
        .execute_for(
            AttentionScope {
                principal,
                workspace,
            },
            ResurfacingActionTargetRef::ChannelFollowUp(annotation_id.clone()),
            body.into_inner(),
        )
        .await
    {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(error) => {
            crate::resurfacing_api::contextual_action_error_response(&annotation_id, error)
        },
    }
}

use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use actix_web::{web, HttpRequest, HttpResponse, Result};
use anyhow::Context as _;
use magician::magician_v2::today_projection_cache::{TodayItem, TodaySectionId};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use magician::magician_v2::{
    artifact_v2::{
        models::{TaskListItemV3, TaskOutputMode, TaskRecord},
        workspace::ArtifactV2Workspace,
        ArtifactV2Service, CreateTaskInput, ScopeRef, V3ReadApi,
    },
    attention_funnel::{
        route_attention_candidate, AttentionAction, AttentionActionKind, AttentionCandidate,
        AttentionFunnelStage, AttentionLane, AttentionRouteContext, AttentionRouteEvent,
        AttentionScope, AttentionSource, AttentionSourceFamily, AttentionSourceKind,
        AttentionTraceStatus, AttentionUrgency, RouteOutcome,
    },
    attention_funnel_store::AttentionFunnelStore,
    attention_lane_facade::{
        decode_attention_lane_cursor, list_attention_lane, list_feed_attention_lane,
        list_priority_attention_lane, AttentionLanePage, AttentionLaneQuery,
    },
    feed::action_adapter::MONITOR_UPDATE_SOURCE_KIND,
    feed::{
        today_action_adapter, FeedAction, FeedAttentionLane, FeedCounts, FeedItem, FeedItemPatch,
        FeedItemStatus, FeedItemType, FeedQuery, FeedStore, TodayActionPlan, TodayActionSubject,
        THINKING_MAP_ACTION_SOURCE_KIND,
    },
    learning::{
        CreateLearningCandidateRequest, CreateLearningEventRequest, LearningCandidate,
        LearningCandidateFilters, LearningCandidateState, LearningCandidateType,
        LearningCapabilityEvolutionApplicationFilters,
        LearningCapabilityEvolutionApplicationRecord, LearningCapabilityEvolutionApplicationStatus,
        LearningCapabilityEvolutionImplementationFilters,
        LearningCapabilityEvolutionImplementationRecord,
        LearningCapabilityEvolutionPostPromotionMonitorFilters,
        LearningCapabilityEvolutionPostPromotionMonitorRecord,
        LearningCapabilityEvolutionPostPromotionMonitorStatus,
        LearningCapabilityEvolutionPromotionFilters, LearningCapabilityEvolutionPromotionRecord,
        LearningCapabilityEvolutionProposal, LearningCapabilityEvolutionProposalFilters,
        LearningCapabilityEvolutionProposalStatus,
        LearningCapabilityEvolutionRollbackRecommendationFilters,
        LearningCapabilityEvolutionRollbackRecommendationRecord,
        LearningCapabilityEvolutionRollbackRecommendationStatus,
        LearningCapabilityEvolutionValidationFilters, LearningCapabilityEvolutionValidationReport,
        LearningCapabilityEvolutionValidationStatus, LearningEvaluationBacklogFilters,
        LearningEvaluationBacklogItem, LearningEvaluationRunFilters, LearningEvaluationRunReport,
        LearningEvent, LearningEvidenceRef, LearningGrowthEvaluationRunFilters,
        LearningGrowthEvaluationRunReport, LearningMemoryBridge, LearningRiskLevel, LearningScope,
        LearningStore,
    },
    monitors::{
        monitor_updates::{update_projects_to_changed, MonitorUpdateDetailV1},
        MonitorRunStatus,
    },
    realtime_events::{HitlLifecycleState, RuntimeTransportBroadcaster, RuntimeTransportEvent},
};
use magician_surfaces::thinking_map::{
    AssertionOrigin, EpistemicState, MapLifecycle, NodeKind, PromotionKind, ThinkingMap,
    ThinkingMapStore,
};

use crate::scope::resolve_required_scope;
use crate::today_projection_cache::{
    TodayCacheKey, TodayProjection, TodayProjectionCache, TodayProjectionSource,
};

use crate::thinking_maps_api::{
    promote_node_to_target, thinking_map_store_error, NodePromotionOutcome,
};
use magician::magician_v2::storage::{ListEntry, ListKind};

const LEARNING_CANDIDATE_FEED_PREFIX: &str = "learning_candidate:";
const LEARNING_INSIGHT_FEED_PREFIX: &str = "learning_insight:";
const SKILL_EVOLUTION_APPROVAL_ATTENTION_PREFIX: &str = "skill_evolution_approval:";
const ROLLBACK_RECOMMENDATION_ATTENTION_PREFIX: &str = "skill_evolution_rollback:";
const POST_PROMOTION_MONITOR_ATTENTION_PREFIX: &str = "skill_evolution_post_promotion:";
const FEED_LEARNING_ACTOR: &str = "learning_feed";
const FOLLOW_UP_TASK_MARKER_STALE_AFTER: Duration = Duration::from_secs(15 * 60);
const TODAY_SECTION_COLLECTION_LIMIT: usize = 1_050;
/// Today item-id prefix for thinking-map action candidates; the action
/// execution route uses it to decide which source projection to consult.
const TODAY_THINKING_MAP_ITEM_PREFIX: &str = "today:followups:thinking_map_action:";

#[derive(Debug, Clone)]
struct LearningMemoryFeedProjection {
    target_scope: String,
    target_tier: String,
    memory_key: String,
    memory_value: Value,
}

fn metadata_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .as_object()
        .and_then(|record| record.get(key))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn metadata_has_key(value: &serde_json::Value, key: &str) -> bool {
    value
        .as_object()
        .is_some_and(|record| record.contains_key(key))
}

fn feed_item_matches_attention_alias(item: &FeedItem, item_id: &str) -> bool {
    item.id == item_id
        || [
            "correlation_id",
            "pause_state_id",
            "approval_id",
            "request_id",
        ]
        .into_iter()
        .any(|key| metadata_string(&item.metadata, key).as_deref() == Some(item_id))
}

fn feed_item_is_canonical_hitl(item: &FeedItem) -> bool {
    matches!(
        metadata_string(&item.metadata, "attention_kind").as_deref(),
        Some(
            "hitl.requested"
                | "input.requested"
                | "waiting_for_confirmation"
                | "max_iterations_reached"
                | "user_request.pending"
        )
    ) || (metadata_string(&item.metadata, "input_type").is_some()
        && [
            "correlation_id",
            "pause_state_id",
            "approval_id",
            "request_id",
        ]
        .into_iter()
        .any(|key| metadata_string(&item.metadata, key).is_some()))
}

fn runtime_pending_hitl_feed_item(event: RuntimeTransportEvent) -> Option<FeedItem> {
    let RuntimeTransportEvent::HitlRequested {
        correlation_id,
        source,
        input_type,
        prompt,
        hint,
        input_schema,
        task_id,
        execution_id,
        agent_id,
        principal: Some(principal),
        workspace: Some(workspace),
        timestamp,
    } = event
    else {
        return None;
    };
    let item_type = if source == "approval" {
        FeedItemType::Approval
    } else {
        FeedItemType::Escalation
    };
    let uses_pause_state_id = matches!(
        source.as_str(),
        "agentic" | "escalation" | "primitive" | "inner_loop"
    );
    let mut metadata = json!({
        "attention_kind": "hitl.requested",
        "source": source,
        "input_type": input_type,
        "correlation_id": correlation_id,
        "request_id": correlation_id,
    });
    if let Some(record) = metadata.as_object_mut() {
        if item_type == FeedItemType::Approval {
            record.insert("approval_id".to_string(), json!(correlation_id));
        }
        if uses_pause_state_id {
            record.insert("pause_state_id".to_string(), json!(correlation_id));
        }
        if let Some(schema) = input_schema {
            record.insert("input_schema".to_string(), schema);
        }
        if let Some(execution_id) = execution_id {
            record.insert("execution_id".to_string(), json!(execution_id));
        }
        if let Some(hint) = hint.as_ref() {
            record.insert("hint".to_string(), json!(hint));
        }
    }
    Some(FeedItem {
        id: format!("runtime:hitl:{correlation_id}"),
        principal,
        workspace,
        item_type,
        task_id,
        ui_thread_id: None,
        agent_id,
        title: prompt,
        summary: hint,
        status: FeedItemStatus::NeedsAction,
        created_at: timestamp,
        updated_at: timestamp,
        actions: Vec::new(),
        metadata,
    })
}

fn write_json_atomic(path: &Path, value: &Value) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    // The shared durable writer: parent created, unique temp, fsync, rename,
    // parent-dir sync. The previous hand-roll discarded the fsync error with
    // `.ok()` and never synced the parent, so the rename could be durable while
    // the marker's contents were not.
    magician::magician_v2::artifact_v2::io::write_bytes_durably_sync(path, &bytes)
        .with_context(|| format!("write feed marker {}", path.display()))
}

fn remove_file_if_exists(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn follow_up_task_marker_is_stale(path: &Path) -> bool {
    // This check is the ONLY recovery path for a marker left behind by a
    // failed reservation, and callers pair `true` with delete-and-retry. So
    // the answer has to split three ways, not two:
    //
    // - `NotFound` is the one unambiguous answer: the claim is gone, and
    //   saying so is safe (the delete is a no-op, the retry re-creates).
    // - Any other metadata error is AMBIGUOUS — a transient stat failure on
    //   a marker another in-flight request holds live. An earlier fix failed
    //   open here, and that let a concurrent reserve delete the live marker
    //   and create the follow-up task twice: POSIX keeps the first holder's
    //   open fd writable after the unlink, so neither request sees an error.
    //   Fail closed and warn; the caller sees AlreadyExists and retries, and
    //   a marker that stays unreadable keeps warning until someone looks.
    // - A future mtime splits on magnitude. Within the stale window it is a
    //   small backwards clock step (NTP): hold the claim, ordinary staleness
    //   resumes when the clock catches up. Beyond the window it can never
    //   age out — a corrupt or restored timestamp — and holding it forever
    //   is the permanent silent wedge the `unwrap_or(false)` original had.
    let modified = match path.metadata().and_then(|metadata| metadata.modified()) {
        Ok(modified) => modified,
        Err(error) if error.kind() == ErrorKind::NotFound => return true,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "[FEED-API] follow-up marker metadata unreadable; holding the claim"
            );
            return false;
        },
    };
    match modified.elapsed() {
        Ok(elapsed) => elapsed >= FOLLOW_UP_TASK_MARKER_STALE_AFTER,
        Err(ahead) => ahead.duration() >= FOLLOW_UP_TASK_MARKER_STALE_AFTER,
    }
}

fn default_limit() -> usize {
    50
}

fn default_attention_section_limit() -> usize {
    5
}

fn default_today_section_limit() -> usize {
    8
}

fn default_today_digest_limit() -> usize {
    7
}

fn default_today_visibility_limit() -> usize {
    20
}

#[derive(Debug, Deserialize)]
pub struct FeedListQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub before: Option<i64>,
    #[serde(default)]
    pub after: Option<i64>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub ui_thread_id: Option<String>,
    #[serde(default)]
    pub item_type: Option<FeedItemType>,
    #[serde(default)]
    pub status: Option<FeedItemStatus>,
    #[serde(default)]
    pub agent_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FeedCountsQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub ui_thread_id: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct FeedLearningActionRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub revised_value: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct FeedInsightActionRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub memory_value: Option<String>,
    #[serde(default)]
    pub task_title: Option<String>,
    #[serde(default)]
    pub task_description: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub ui_thread_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FeedListResponse {
    pub items: Vec<FeedItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_before: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct FeedCountsResponse {
    pub counts: FeedCounts,
}

#[derive(Debug, Deserialize)]
pub struct FeedAttentionQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub ui_thread_id: Option<String>,
    #[serde(default = "default_attention_section_limit")]
    pub per_section: usize,
    /// Page size for the paginated inbox (the volume lanes). When present it
    /// overrides `per_section` for the returned lists; the `totals` field gives
    /// the true per-lane totals so the client knows whether to load more.
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub requests_cursor: Option<String>,
    #[serde(default)]
    pub approvals_cursor: Option<String>,
    #[serde(default)]
    pub escalations_cursor: Option<String>,
    #[serde(default)]
    pub failed_cursor: Option<String>,
    #[serde(default)]
    pub running_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FeedAttentionItemQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TodayQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default = "default_today_section_limit")]
    pub per_section: usize,
    #[serde(default)]
    pub section: Option<TodaySectionId>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub digest_refresh: bool,
    #[serde(default = "default_today_digest_limit")]
    pub digest_limit: usize,
    #[serde(default)]
    pub digest_offset: usize,
    /// The READER's own date, `YYYY-MM-DD`. Every Today date predicate is
    /// evaluated against it, because the server cannot know the reader's
    /// timezone and a date it invents is wrong in every positive-offset
    /// zone for part of every day.
    ///
    /// Absent falls back to the UTC date, which is exactly what Today did
    /// before this parameter existed — an older polling client is no worse
    /// off than it is now. This differs deliberately from the tasks list,
    /// whose date lanes reject a request without `today=`: Today is polled
    /// by clients whose deploy we do not control, and a hard failure on a
    /// polled endpoint is worse than the imprecision it replaces.
    #[serde(default)]
    pub today: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TodayVisibilityQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default = "default_today_visibility_limit")]
    pub limit: usize,
}

#[derive(Debug, Deserialize)]
pub struct TodayActionQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TodayVisibilityRequest {
    pub action: String,
    #[serde(default)]
    pub snooze_until: Option<i64>,
    #[serde(default)]
    pub snooze_minutes: Option<i64>,
    #[serde(default)]
    pub snapshot: Option<TodayVisibilitySnapshot>,
}

#[derive(Debug, Deserialize)]
pub struct AttentionDismissQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AttentionDismissRequest {
    pub item_id: String,
    /// When `false`, reverses the dismissal (undismiss). Defaults to `true`
    /// so the primary `/feed/attention/dismiss` route dismisses without a
    /// flag; the sibling `/feed/attention/undismiss` route forces `false`.
    #[serde(default = "default_true")]
    pub dismissed: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize, Default)]
pub struct FeedAttentionCounts {
    pub requests: u64,
    pub approvals: u64,
    pub escalations: u64,
    pub needs_action: u64,
    pub failed: u64,
    pub running: u64,
}

/// True per-lane totals independent of the returned page size. Kept distinct
/// from `counts` for clients that need per-lane pagination metadata without
/// inferring it from the badge/count contract.
#[derive(Debug, Serialize, Default)]
pub struct FeedAttentionTotals {
    pub requests: u64,
    pub approvals: u64,
    pub escalations: u64,
    pub failed: u64,
    pub running: u64,
}

#[derive(Debug, Serialize, Default)]
pub struct FeedAttentionLanePageMeta {
    pub total: usize,
    pub limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Serialize, Default)]
pub struct FeedAttentionPages {
    pub requests: FeedAttentionLanePageMeta,
    pub approvals: FeedAttentionLanePageMeta,
    pub escalations: FeedAttentionLanePageMeta,
    pub failed: FeedAttentionLanePageMeta,
    pub running: FeedAttentionLanePageMeta,
}

#[derive(Debug, Serialize)]
pub struct FeedAttentionResponse {
    pub counts: FeedAttentionCounts,
    #[serde(default)]
    pub totals: FeedAttentionTotals,
    #[serde(default)]
    pub pages: FeedAttentionPages,
    pub requests: Vec<FeedItem>,
    pub approvals: Vec<FeedItem>,
    pub escalations: Vec<FeedItem>,
    pub failed: Vec<FeedItem>,
    pub running: Vec<FeedItem>,
}

fn feed_attention_page_meta(page: &AttentionLanePage<FeedItem>) -> FeedAttentionLanePageMeta {
    FeedAttentionLanePageMeta {
        total: page.total,
        limit: page.limit,
        cursor: page.cursor.clone(),
        next_cursor: page.next_cursor.clone(),
        has_more: page.has_more,
    }
}

#[derive(Debug, Serialize, Default)]
pub struct TodaySections {
    pub needs_you: Vec<TodayItem>,
    pub delivered: Vec<TodayItem>,
    pub changed: Vec<TodayItem>,
    pub active_work: Vec<TodayItem>,
    pub spaces: Vec<TodayItem>,
    pub followups: Vec<TodayItem>,
}

#[derive(Debug, Serialize)]
pub struct TodaySectionPage {
    pub section: TodaySectionId,
    pub total: usize,
    pub limit: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Serialize, Default)]
pub struct TodayCounts {
    pub needs_you: u64,
    pub delivered: u64,
    pub changed: u64,
    pub active_work: u64,
    pub spaces: u64,
    pub followups: u64,
    pub total: u64,
}

#[derive(Debug, Serialize)]
pub struct TodayFreshness {
    pub source: String,
    pub generated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodayDigestBullet {
    pub id: String,
    pub text: String,
    pub source_kind: String,
    pub source_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub space_ids: Vec<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodayChangedDigest {
    pub generated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
    #[serde(default)]
    pub total: usize,
    #[serde(default)]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
    pub bullets: Vec<TodayDigestBullet>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TodayDigestCacheState {
    version: u32,
    source_fingerprint: String,
    digest: TodayChangedDigest,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TodayVisibilityRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<TodayVisibilitySnapshot>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodayVisibilitySnapshot {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub reason: String,
    pub section: String,
    pub source_kind: String,
    pub source_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub space_ids: Vec<String>,
    pub item_updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TodayVisibilityState {
    version: u32,
    #[serde(default)]
    items: HashMap<String, TodayVisibilityRecord>,
}

/// Per-item dismissal record for the attention surface. Keyed by the raw
/// [`FeedItem::id`] (e.g. `v3:attention:<attention_id>` for V3 attention
/// summaries, or the feed-store id for a `status: Failed` item) — a different
/// key form than Today's `today:<section>:<FeedItem.id>`, so this is a parallel
/// store, not a reuse of [`TodayVisibilityState`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AttentionDismissedRecord {
    pub dismissed_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct AttentionDismissedState {
    version: u32,
    #[serde(default)]
    items: HashMap<String, AttentionDismissedRecord>,
}

#[derive(Debug, Serialize)]
pub struct AttentionDismissedResponse {
    pub item_id: String,
    pub dismissed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<AttentionDismissedRecord>,
}

#[derive(Debug, Serialize)]
pub struct TodayVisibilityResponse {
    pub item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<TodayVisibilityRecord>,
}

#[derive(Debug, Serialize)]
pub struct TodayVisibilityListItem {
    pub item_id: String,
    pub hidden_kind: String,
    pub record: TodayVisibilityRecord,
}

#[derive(Debug, Serialize)]
pub struct TodayVisibilityListResponse {
    pub items: Vec<TodayVisibilityListItem>,
}

#[derive(Debug, Serialize)]
pub struct TodayResponse {
    pub principal: String,
    pub workspace: String,
    pub generated_at: i64,
    pub freshness: TodayFreshness,
    pub headline: String,
    pub digest: TodayChangedDigest,
    pub sections: TodaySections,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section_page: Option<TodaySectionPage>,
    pub counts: TodayCounts,
}

/// Minimum wall-clock gap between successive
/// `sync_learning_feed_best_effort` runs for the same scope. Pre-
/// v0.6.579 the sync fired on every `/feed`, `/feed/counts`, and
/// `/feed/attention` HTTP request — with the AttentionBar polling
/// every 15s, that meant ~4 syncs/min per connected frontend, each
/// upserting hundreds of learning-projection rows into DuckDB.
///
/// Why 15 minutes (not 60s or 5min): the learning data ingested by
/// this sync — reflection completions, evaluation runs, growth
/// evaluations, backlog items — is generated by background runners
/// that themselves cycle on minute-to-hour intervals. The most
/// frequent learning event today is `learning_reflection_completed`,
/// which fires once per agent cycle (~minutes) and overwhelmingly
/// produces "0 candidate(s)" no-ops. A 15-minute window keeps the
/// surface alive for actionable signals (eval failures, route
/// failures, memory-index rebuild failures) without churning the
/// DuckDB write path for telemetry the operator doesn't act on.
///
/// Other AttentionBar content (HITLs, failed runs, running tasks)
/// does NOT route through this sync and stays real-time.
const LEARNING_SYNC_THROTTLE: Duration = Duration::from_secs(15 * 60);

#[derive(Clone)]
pub struct FeedApi {
    feed_store: FeedStore,
    v3_service: Arc<ArtifactV2Service>,
    attention_store: Option<AttentionFunnelStore>,
    pending_hitl_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    /// Per-scope last successful `sync_learning_feed` time, gated by
    /// [`LEARNING_SYNC_THROTTLE`] via [`sync_learning_feed_if_due`].
    /// Shared across `FeedApi` clones (one `Arc<Mutex<…>>`) so all
    /// actix workers see the same throttle state.
    last_learning_sync_at: Arc<Mutex<HashMap<(String, String), Instant>>>,
    /// The Today projection cache, keyed by `(principal, workspace, the
    /// reader's date)`.
    ///
    /// **Borrowed from `ArtifactV2Service`, not built here.** Task lifecycle
    /// writes — create, complete, edit, delete — never pass through this API,
    /// and they change what `/today` should answer, so the cache has to live
    /// where both halves can reach it. One `Arc` shared across every `FeedApi`
    /// clone and with the service, for the same reason as the throttle above:
    /// the backend builds one of each and hands `Arc` clones to every actix
    /// worker. A test that builds its own service gets its own cache — which
    /// matters, because the key is the *scope*, and tests reuse scope strings
    /// over different temporary workspaces.
    today_projection_cache: Arc<TodayProjectionCache>,
}

impl FeedApi {
    pub fn new(feed_store: FeedStore, v3_service: Arc<ArtifactV2Service>) -> Self {
        let today_projection_cache = Arc::clone(v3_service.today_projection_cache());
        Self {
            feed_store,
            v3_service,
            attention_store: None,
            pending_hitl_broadcaster: None,
            last_learning_sync_at: Arc::new(Mutex::new(HashMap::new())),
            today_projection_cache,
        }
    }

    pub fn with_attention_store(mut self, attention_store: AttentionFunnelStore) -> Self {
        self.attention_store = Some(attention_store);
        self
    }

    pub fn with_pending_hitl_broadcaster(
        mut self,
        broadcaster: Arc<RuntimeTransportBroadcaster>,
    ) -> Self {
        self.pending_hitl_broadcaster = Some(broadcaster);
        self
    }

    pub async fn list_feed(
        &self,
        req: &HttpRequest,
        query: web::Query<FeedListQuery>,
    ) -> Result<HttpResponse> {
        let query = query.into_inner();
        let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        self.sync_learning_feed_best_effort(&principal, &workspace);
        let valid_task_ids = self.valid_task_ids(&principal, &workspace).await;
        // Pull a wider window than the requested limit so the orphan filter
        // below doesn't truncate the response by removing items that
        // reference deleted tasks. The limit is reapplied after filtering.
        let store_limit = if query.limit == usize::MAX {
            usize::MAX
        } else {
            query.limit.saturating_mul(4).max(query.limit)
        };
        let raw_items = match self
            .feed_store
            .list_items(FeedQuery {
                principal,
                workspace,
                before: query.before,
                after: query.after,
                limit: store_limit,
                task_id: None,
                ui_thread_id: query.ui_thread_id,
                item_type: query.item_type,
                status: query.status,
                agent_id: query.agent_id,
            })
            .await
        {
            Ok(items) => items,
            Err(error) => {
                return Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": format!("{error}")
                })));
            },
        };
        // Drop feed items that point at tasks no longer present in the
        // scope (orphans from deleted/archived tasks). Items without a
        // `task_id` are kept. If task listing failed, no filtering is
        // applied — better to show a stale entry than to silently hide
        // everything.
        let items = retain_existing_tasks(raw_items, &valid_task_ids, query.limit);
        let next_before = items.last().map(|item| item.updated_at);
        Ok(HttpResponse::Ok().json(FeedListResponse { items, next_before }))
    }

    pub async fn feed_counts(
        &self,
        req: &HttpRequest,
        query: web::Query<FeedCountsQuery>,
    ) -> Result<HttpResponse> {
        let query = query.into_inner();
        let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        self.sync_learning_feed_best_effort(&principal, &workspace);
        // Mirror list_feed: feed items whose task_id points at a deleted or
        // archived task are hidden from the items response, so they must not
        // be counted here either — otherwise the filter buttons show a
        // higher total than the rendered list.
        let valid_task_ids = self.valid_task_ids(&principal, &workspace).await;
        let raw_items = match self
            .feed_store
            .list_items(FeedQuery {
                principal: principal.clone(),
                workspace: workspace.clone(),
                before: None,
                after: None,
                limit: usize::MAX,
                task_id: None,
                ui_thread_id: query.ui_thread_id.clone(),
                item_type: None,
                status: None,
                agent_id: None,
            })
            .await
        {
            Ok(items) => items,
            Err(error) => {
                return Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": format!("{error}")
                })));
            },
        };
        let items = retain_existing_tasks(raw_items, &valid_task_ids, usize::MAX);
        let mut counts = FeedCounts::default();
        counts.total = items.len() as u64;
        for item in &items {
            match item.status {
                FeedItemStatus::Running => counts.running += 1,
                FeedItemStatus::NeedsAction => counts.needs_action += 1,
                FeedItemStatus::Failed => counts.failed += 1,
                FeedItemStatus::Done => counts.done += 1,
                FeedItemStatus::Info => counts.info += 1,
            }
        }
        Ok(HttpResponse::Ok().json(FeedCountsResponse { counts }))
    }

    pub async fn attention(
        &self,
        req: &HttpRequest,
        query: web::Query<FeedAttentionQuery>,
    ) -> Result<HttpResponse> {
        let query = query.into_inner();
        let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        self.sync_learning_feed_best_effort(&principal, &workspace);
        // Page size for the returned lane pages. When the client sends `limit`
        // it drives the cursor-paginated inbox; legacy callers fall back to
        // `per_section` (default 5). Downstream bucket code all keys off this.
        let per_section = query
            .limit
            .map(|l| l.clamp(1, 200))
            .unwrap_or_else(|| query.per_section.clamp(1, 20));
        let requests_cursor = query.requests_cursor;
        let approvals_cursor = query.approvals_cursor;
        let escalations_cursor = query.escalations_cursor;
        let failed_cursor = query.failed_cursor;
        let running_cursor = query.running_cursor;
        let ui_thread_id = query.ui_thread_id;
        for (name, cursor) in [
            ("requests", requests_cursor.as_deref()),
            ("approvals", approvals_cursor.as_deref()),
            ("escalations", escalations_cursor.as_deref()),
            ("failed", failed_cursor.as_deref()),
            ("running", running_cursor.as_deref()),
        ] {
            if let Some(cursor) = cursor {
                if let Err(error) = decode_attention_lane_cursor(cursor) {
                    return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                        "error": format!("invalid {name} cursor: {error}")
                    })));
                }
            }
        }

        // Internal tasks are chat-delegated machinery: their pending HITL
        // requests stay actionable, but their FAILURES belong to the internal
        // lane, not operator attention.
        let internal_scope =
            ScopeRef::system_internal_unauthenticated(&principal.clone(), &workspace.clone());
        let internal_task_ids: Vec<String> = self
            .all_internal_task_ids(&internal_scope)
            .await
            .into_iter()
            .collect();
        let requests_page = list_feed_attention_lane(
            &self.feed_store,
            AttentionLaneQuery {
                principal: &principal,
                workspace: &workspace,
                lane: AttentionLane::NeedsYou,
                cursor: requests_cursor.as_deref(),
                offset: 0,
                limit: per_section,
            },
            FeedAttentionLane::Requests,
            ui_thread_id.as_deref(),
            &[],
        )
        .await;
        let approvals_page = list_feed_attention_lane(
            &self.feed_store,
            AttentionLaneQuery {
                principal: &principal,
                workspace: &workspace,
                lane: AttentionLane::NeedsYou,
                cursor: approvals_cursor.as_deref(),
                offset: 0,
                limit: per_section,
            },
            FeedAttentionLane::Approvals,
            ui_thread_id.as_deref(),
            &[],
        )
        .await;
        let escalations_page = list_feed_attention_lane(
            &self.feed_store,
            AttentionLaneQuery {
                principal: &principal,
                workspace: &workspace,
                lane: AttentionLane::NeedsYou,
                cursor: escalations_cursor.as_deref(),
                offset: 0,
                limit: per_section,
            },
            FeedAttentionLane::Escalations,
            ui_thread_id.as_deref(),
            &[],
        )
        .await;
        let failed_page = list_feed_attention_lane(
            &self.feed_store,
            AttentionLaneQuery {
                principal: &principal,
                workspace: &workspace,
                lane: AttentionLane::Failed,
                cursor: failed_cursor.as_deref(),
                offset: 0,
                limit: per_section,
            },
            FeedAttentionLane::Failed,
            ui_thread_id.as_deref(),
            &internal_task_ids,
        )
        .await;
        let running_page = list_feed_attention_lane(
            &self.feed_store,
            AttentionLaneQuery {
                principal: &principal,
                workspace: &workspace,
                lane: AttentionLane::ActiveWork,
                cursor: running_cursor.as_deref(),
                offset: 0,
                limit: per_section,
            },
            FeedAttentionLane::Running,
            ui_thread_id.as_deref(),
            &[],
        )
        .await;
        let (requests_page, approvals_page, escalations_page, failed_page, running_page) = match (
            requests_page,
            approvals_page,
            escalations_page,
            failed_page,
            running_page,
        ) {
            (Ok(requests), Ok(approvals), Ok(escalations), Ok(failed), Ok(running)) => {
                (requests, approvals, escalations, failed, running)
            },
            (requests, approvals, escalations, failed, running) => {
                let error = requests
                    .err()
                    .or_else(|| approvals.err())
                    .or_else(|| escalations.err())
                    .or_else(|| failed.err())
                    .or_else(|| running.err())
                    .expect("one attention lane query failed");
                return Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": format!("{error}")
                })));
            },
        };
        self.record_feed_attention_route_events(
            &principal,
            &workspace,
            chrono::Utc::now().timestamp_millis(),
            &requests_page.items,
            &approvals_page.items,
            &escalations_page.items,
            &failed_page.items,
            &running_page.items,
        );

        let totals = FeedAttentionTotals {
            requests: requests_page.total as u64,
            approvals: approvals_page.total as u64,
            escalations: escalations_page.total as u64,
            failed: failed_page.total as u64,
            running: running_page.total as u64,
        };
        let counts = feed_attention_counts_from_totals(
            &totals,
            requests_page
                .request_hitl_total
                .unwrap_or(requests_page.total) as u64,
        );
        let pages = FeedAttentionPages {
            requests: feed_attention_page_meta(&requests_page),
            approvals: feed_attention_page_meta(&approvals_page),
            escalations: feed_attention_page_meta(&escalations_page),
            failed: feed_attention_page_meta(&failed_page),
            running: feed_attention_page_meta(&running_page),
        };
        let requests = requests_page.items;
        let approvals = approvals_page.items;
        let escalations = escalations_page.items;
        let failed = failed_page.items;
        let running = running_page.items;

        Ok(HttpResponse::Ok().json(FeedAttentionResponse {
            counts,
            totals,
            pages,
            requests,
            approvals,
            escalations,
            failed,
            running,
        }))
    }

    pub async fn attention_item(
        &self,
        req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<FeedAttentionItemQuery>,
    ) -> Result<HttpResponse> {
        let query = query.into_inner();
        let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let item_id = path.into_inner();
        self.sync_learning_feed_best_effort(&principal, &workspace);
        let stored_item = match self
            .feed_store
            .get_attention_item_by_alias(&principal, &workspace, &item_id)
            .await
        {
            Ok(item) => item,
            Err(error) => {
                return Ok(HttpResponse::InternalServerError().json(json!({
                    "error": format!("{error}")
                })));
            },
        };
        let valid_task_ids = self.valid_attention_task_ids(&principal, &workspace).await;
        let stored_item =
            retain_existing_tasks(stored_item.into_iter().collect(), &valid_task_ids, 1).pop();
        // Non-HITL rows are authoritative in FeedStore once their backing task
        // filter passes. Canonical HITL projections are different: they can be
        // stale after another surface resolves the request, so they must be
        // corroborated by current V3 attention state or the scoped durable
        // lifecycle registry before opening a prompt.
        if stored_item
            .as_ref()
            .is_some_and(|item| !feed_item_is_canonical_hitl(item))
        {
            return Ok(feed_attention_item_response(&item_id, stored_item));
        }

        // Taskless canonical requests (notably bot-auth) have no V3 task or
        // execution artifact by design. Consult their authoritative lifecycle
        // before V3 so an unrelated task-store outage cannot break a valid
        // notification deep link.
        let mut lifecycle_ids = vec![item_id.clone()];
        if let Some(stored_item) = stored_item.as_ref() {
            for key in [
                "correlation_id",
                "pause_state_id",
                "approval_id",
                "request_id",
            ] {
                if let Some(alias) = metadata_string(&stored_item.metadata, key) {
                    if !lifecycle_ids.contains(&alias) {
                        lifecycle_ids.push(alias);
                    }
                }
            }
        }
        let lifecycle_state =
            self.pending_hitl_broadcaster
                .as_ref()
                .map(|broadcaster| {
                    let mut saw_unavailable = false;
                    for id in &lifecycle_ids {
                        match broadcaster.hitl_lifecycle_state(&principal, &workspace, id) {
                            state @ (HitlLifecycleState::Pending(_)
                            | HitlLifecycleState::Resolved) => return state,
                            HitlLifecycleState::Unavailable => saw_unavailable = true,
                            HitlLifecycleState::Unknown => {},
                        }
                    }
                    if saw_unavailable {
                        HitlLifecycleState::Unavailable
                    } else {
                        HitlLifecycleState::Unknown
                    }
                })
                .unwrap_or(HitlLifecycleState::Unknown);
        if matches!(lifecycle_state, HitlLifecycleState::Resolved) {
            return Ok(feed_attention_item_response(&item_id, None));
        }
        if matches!(lifecycle_state, HitlLifecycleState::Unavailable) {
            return Ok(HttpResponse::ServiceUnavailable().json(json!({
                "error": "hitl_lifecycle_unavailable",
                "item_id": item_id,
            })));
        }
        let runtime_item = match lifecycle_state {
            HitlLifecycleState::Pending(event) => runtime_pending_hitl_feed_item(event),
            HitlLifecycleState::Resolved
            | HitlLifecycleState::Unknown
            | HitlLifecycleState::Unavailable => None,
        };
        if runtime_item
            .as_ref()
            .is_some_and(|item| item.task_id.is_none())
        {
            return Ok(feed_attention_item_response(&item_id, runtime_item));
        }

        // A canonical HITL notification can win the race against the async
        // FeedStore projection. Consult the authoritative V3 task attention
        // summaries both on a store miss and before accepting a stored HITL
        // row, preventing resolved/terminal projections from reopening.
        let authoritative_item = match self
            .list_v3_attention_items(&principal, &workspace, None)
            .await
        {
            Ok(items) => retain_existing_tasks(items, &valid_task_ids, usize::MAX)
                .into_iter()
                .find(|item| feed_item_matches_attention_alias(item, &item_id)),
            Err(error) => {
                return Ok(HttpResponse::InternalServerError().json(json!({
                    "error": format!("{error}")
                })));
            },
        };
        let item = if let Some(item) = authoritative_item {
            match self
                .feed_store
                .attention_item_is_dismissed(&principal, &workspace, &item.id)
                .await
            {
                Ok(true) => None,
                Ok(false) => Some(item),
                Err(error) => {
                    return Ok(HttpResponse::InternalServerError().json(json!({
                        "error": format!("{error}")
                    })));
                },
            }
        } else {
            None
        };
        Ok(feed_attention_item_response(&item_id, item))
    }

    pub async fn today(
        &self,
        req: &HttpRequest,
        query: web::Query<TodayQuery>,
    ) -> Result<HttpResponse> {
        let query = query.into_inner();
        let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        self.sync_learning_feed_best_effort(&principal, &workspace);
        // ONE clock read for the whole request. `generated_at` and the date
        // the collector evaluates its predicates against come from the same
        // instant, and the collector is handed the resolved date rather than
        // reading the clock a second time — two readings disagree only in
        // some timezones, which is the hardest kind of wrong to see.
        let now = chrono::Utc::now();
        let generated_at = now.timestamp_millis();
        let reader_today = match resolve_today_reader_date(query.today.as_deref(), now) {
            Ok(date) => date,
            Err(response) => return Ok(response),
        };
        let per_section = query.per_section.clamp(1, 20);
        let requested_section = query.section;
        let page_limit = query
            .limit
            .map(|limit| limit.clamp(1, 50))
            .unwrap_or(per_section);
        let page_cursor = query.cursor.as_deref();
        let digest_limit = query.digest_limit.clamp(1, 50);
        let digest_offset = query.digest_offset.min(1_000);
        // Preview responses still need backend totals for tab counters. Keep
        // the projected source window bounded, but do not cap it to the small
        // preview row count.
        let collection_limit = TODAY_SECTION_COLLECTION_LIMIT;
        let overfetch_limit = (collection_limit * 5).clamp(50, 5_250);
        let (projection, projection_source) = match self
            .today_projection_cached(
                &principal,
                &workspace,
                collection_limit,
                overfetch_limit,
                reader_today,
                generated_at,
            )
            .await
        {
            Ok(projection) => projection,
            Err(error) => {
                return Ok(HttpResponse::InternalServerError().json(json!({
                    "error": error
                })));
            },
        };
        // The projection carries the instant it was actually built, and that
        // is what the response reports: a cached projection handed out three
        // seconds later is three seconds old, and says so rather than
        // restamping itself as fresh.
        let TodayProjection {
            needs_you,
            delivered,
            changed,
            active_work,
            followups,
            generated_at,
        } = projection;

        let counts = TodayCounts {
            needs_you: needs_you.len() as u64,
            delivered: delivered.len() as u64,
            changed: changed.len() as u64,
            active_work: active_work.len() as u64,
            followups: followups.len() as u64,
            total: (needs_you.len()
                + delivered.len()
                + changed.len()
                + active_work.len()
                + followups.len()) as u64,
            ..Default::default()
        };
        let headline = today_headline(&counts);
        let digest = today_digest_page(
            self.today_changed_digest_cached(
                &principal,
                &workspace,
                generated_at,
                query.digest_refresh,
                &needs_you,
                &delivered,
                &changed,
                &active_work,
            )
            .await,
            digest_offset,
            digest_limit,
        );
        let (sections, section_page) = today_sections_for_response(
            needs_you,
            delivered,
            changed,
            active_work,
            followups,
            requested_section,
            page_cursor,
            page_limit,
            per_section,
        );
        Ok(HttpResponse::Ok().json(TodayResponse {
            principal,
            workspace,
            generated_at,
            freshness: TodayFreshness {
                // What actually happened, not a constant. A projection served
                // from the cache is up to one TTL old, and `generated_at`
                // alone made a reader subtract two numbers to find out.
                source: projection_source.as_wire_str().to_string(),
                generated_at,
            },
            headline,
            digest,
            sections,
            section_page,
            counts,
        }))
    }

    /// Everything `/today` derives from the corpus, before any of it is
    /// sectioned or paged.
    ///
    /// Split out of [`Self::today`] so one computation can serve preview mode
    /// *and* every section page inside one window — sectioning and paging
    /// depend on `section`, `cursor` and `limit`, which vary per request,
    /// and nothing here does. This is a pure extraction of what the handler
    /// used to do inline, with one deliberate consequence of the split:
    /// `record_today_route_events` is derived from the projection and moves
    /// with it, so route events are appended once per computed projection
    /// rather than once per poll. That stops a polling client appending the
    /// same routing decision several times a minute.
    ///
    /// `reader_today` and `generated_at` are resolved once by the caller and
    /// passed in; nothing here reads the clock.
    ///
    /// `Err` carries the message a failed store read should be reported with.
    /// The caller turns it into a 500 — and it is never cached, because the
    /// cache holds projections, not failures.
    async fn today_projection(
        &self,
        principal: &str,
        workspace: &str,
        collection_limit: usize,
        overfetch_limit: usize,
        reader_today: chrono::NaiveDate,
        generated_at: i64,
    ) -> std::result::Result<TodayProjection, String> {
        let scope = ScopeRef::system_internal_unauthenticated(
            &principal.to_string(),
            &workspace.to_string(),
        );
        // ═══ ONE walk of the task corpus, for everything below ═══
        //
        // This function used to take five of them. `valid_task_ids` walked the
        // corpus for a set of ids; `valid_attention_task_ids` walked it again
        // for the same ids plus the internal root; `list_scope_monitor_updates`
        // walked it a third time and then re-read every task's record
        // *individually* to ask a question the listed row already answered;
        // and the Follow-ups lane walked it a fourth. Four of the five kept
        // nothing but ids.
        //
        // The Follow-ups lane is the one pass that genuinely needs the built
        // rows, so it is the pass that survives, and everything else here is
        // derived from it or from the storage index. The id sets below are not
        // approximations of what the walk would have said — the first IS the
        // walk's own output, and the second differs only in the internal half,
        // which the index answers exactly.
        //
        // `list_v3_attention_items` below still takes two walks of its own
        // (`ArtifactV2Service::list_attention_items` lists both roots for
        // itself) and they are NOT covered by this hoist.
        // `today_takes_one_corpus_walk_of_its_own` asserts the total exactly,
        // so that remainder is visible rather than assumed away.
        let task_source = match self.v3_service.list_tasks(&scope).await {
            Ok(tasks) => tasks,
            Err(error) => {
                return Err(format!("{error}"));
            },
        };
        let valid_task_ids: Option<HashSet<String>> =
            Some(task_source.iter().map(|task| task.id.clone()).collect());
        // Same submission-eligibility filter as the attention `requests`
        // section: drop HITL prompts whose backing task is gone or already
        // terminal so the Today "Needs You" lane never strands them.
        //
        // `None` still means "could not establish the set, so do not filter":
        // the user half cannot fail here (it came from the walk above, which
        // already returned), but the internal half can, and a failure there
        // must not silently dismiss live prompts.
        let valid_attention_task_ids = match self.live_internal_task_ids(&scope).await {
            Some(internal_ids) => {
                let mut ids: HashSet<String> =
                    task_source.iter().map(|task| task.id.clone()).collect();
                ids.extend(internal_ids);
                Some(ids)
            },
            None => None,
        };
        // Internal task SUCCESSES are not user deliveries — Delivered shows
        // work the operator asked for, not chat-delegated machinery output.
        // `live_internal_task_ids` only covers still-actionable tasks, so the
        // all-ids index lookup supplies terminal (completed) internal ids too.
        let internal_task_ids: HashSet<String> = self.all_internal_task_ids(&scope).await;

        let mut needs_you_source = match self
            .list_v3_attention_items(principal, workspace, None)
            .await
        {
            Ok(items) => retain_existing_tasks(items, &valid_attention_task_ids, collection_limit),
            Err(error) => {
                return Err(format!("{error}"));
            },
        };

        let approvals = match self
            .feed_store
            .list_items(FeedQuery {
                principal: principal.to_string(),
                workspace: workspace.to_string(),
                limit: overfetch_limit,
                item_type: Some(FeedItemType::Approval),
                status: Some(FeedItemStatus::NeedsAction),
                ..Default::default()
            })
            .await
        {
            Ok(items) => retain_existing_tasks(items, &valid_task_ids, usize::MAX),
            Err(error) => {
                return Err(format!("{error}"));
            },
        };
        needs_you_source.extend(approvals);

        let escalations = match self
            .feed_store
            .list_items(FeedQuery {
                principal: principal.to_string(),
                workspace: workspace.to_string(),
                limit: overfetch_limit,
                item_type: Some(FeedItemType::Escalation),
                status: Some(FeedItemStatus::NeedsAction),
                ..Default::default()
            })
            .await
        {
            Ok(items) => retain_existing_tasks(items, &valid_task_ids, usize::MAX),
            Err(error) => {
                return Err(format!("{error}"));
            },
        };
        needs_you_source.extend(escalations);

        // Terminal failure reports (FeedItemStatus::Failed — execution.failed /
        // agent.cycle.failed / agent.goal.failed / agent.circuit.opened) are NOT
        // folded into "Needs You". They are non-actionable terminal reports: the
        // attention bar / `/attention` page keep them in a SEPARATE `failed` lane
        // and exclude them from the actionable count, so Today must not surface
        // them as (top-priority) actionable "Needs You" items. Any Failed items
        // that still slipped in via `list_v3_attention_items` above (C-suite
        // execution.failed cards) are filtered out in the dedupe pipeline below.
        needs_you_source.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        let needs_you = dedupe_feed_items_by_id(needs_you_source)
            .into_iter()
            // Drop terminal, non-actionable failure reports so they never appear
            // as "Needs You" (mirrors the attention surfaces, which segregate
            // Failed into their own lane). Genuinely-actionable HITL / approvals /
            // escalations are FeedItemStatus::NeedsAction and are untouched. Done
            // per-FeedItem so a task that has BOTH a failed row AND a still-pending
            // request keeps the pending one.
            .filter(|item| item.status != FeedItemStatus::Failed)
            .take(collection_limit)
            .map(|item| {
                let priority = today_needs_you_priority(&item);
                let reason = today_needs_you_reason(&item);
                today_item_from_feed_item(item, TodaySectionId::NeedsYou, priority, reason)
            })
            .collect::<Vec<_>>();

        let delivered_source = match self
            .feed_store
            .list_items(FeedQuery {
                principal: principal.to_string(),
                workspace: workspace.to_string(),
                limit: overfetch_limit,
                status: Some(FeedItemStatus::Done),
                ..Default::default()
            })
            .await
        {
            Ok(items) => retain_existing_tasks(items, &valid_task_ids, usize::MAX),
            Err(error) => {
                return Err(format!("{error}"));
            },
        };
        let delivered = delivered_source
            .into_iter()
            .filter(|item| {
                item.task_id
                    .as_deref()
                    .map(|task_id| !internal_task_ids.contains(task_id))
                    .unwrap_or(true)
            })
            .filter(today_item_is_delivered)
            .take(collection_limit)
            .map(|item| {
                today_item_from_feed_item(
                    item,
                    TodaySectionId::Delivered,
                    700,
                    "Completed work with user-visible output.".to_string(),
                )
            })
            .collect::<Vec<_>>();

        let active_source = match self
            .feed_store
            .list_items(FeedQuery {
                principal: principal.to_string(),
                workspace: workspace.to_string(),
                limit: overfetch_limit,
                status: Some(FeedItemStatus::Running),
                ..Default::default()
            })
            .await
        {
            Ok(items) => retain_existing_tasks(items, &valid_task_ids, usize::MAX),
            Err(error) => {
                return Err(format!("{error}"));
            },
        };
        let active_work = active_source
            .into_iter()
            .filter(today_item_is_active_work)
            .take(collection_limit)
            .map(|item| {
                today_item_from_feed_item(
                    item,
                    TodaySectionId::ActiveWork,
                    500,
                    "This work is currently running.".to_string(),
                )
            })
            .collect::<Vec<_>>();

        let learning_source = match self
            .feed_store
            .list_items(FeedQuery {
                principal: principal.to_string(),
                workspace: workspace.to_string(),
                limit: overfetch_limit,
                item_type: Some(FeedItemType::AgentLearning),
                ..Default::default()
            })
            .await
        {
            Ok(items) => items,
            Err(error) => {
                return Err(format!("{error}"));
            },
        };
        let mut changed = today_memory_learning_digest_items(
            learning_source,
            principal,
            workspace,
            collection_limit,
        );
        // Recurring Monitors Phase 3 (§9.2): material monitor updates join
        // the Changed lane. Quiet/degraded runs never appear here — the
        // update ledger already gated on emission + status.
        changed.extend(
            self.today_monitor_update_items(&scope, &task_source, collection_limit)
                .await,
        );
        changed.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        changed.truncate(collection_limit);
        let changed = changed;

        // `task_source` is the corpus walk hoisted to the top of this
        // function — the Follow-ups lane is the pass that needs the built
        // rows, so it is the pass every other consumer now reads off.
        let routine_source = match self
            .feed_store
            .list_items(FeedQuery {
                principal: principal.to_string(),
                workspace: workspace.to_string(),
                limit: overfetch_limit,
                item_type: Some(FeedItemType::RoutineResult),
                ..Default::default()
            })
            .await
        {
            Ok(items) => items,
            Err(error) => {
                return Err(format!("{error}"));
            },
        };
        let mut source_followups = self
            .today_meeting_followup_items_from_memory_off_reactor(
                principal,
                workspace,
                overfetch_limit,
                &task_source,
            )
            .await;
        source_followups.extend(
            self.today_thinking_map_action_items(
                principal,
                workspace,
                overfetch_limit,
                &task_source,
                false,
            )
            .await,
        );
        let followups = today_followup_items_from_sources(
            task_source,
            routine_source,
            source_followups,
            principal,
            workspace,
            collection_limit,
            reader_today,
            generated_at,
        );

        let visibility_state = self
            .today_visibility_state_off_reactor(principal, workspace)
            .await;
        let needs_you = today_apply_visibility_state(needs_you, &visibility_state, generated_at);
        let delivered = today_apply_visibility_state(delivered, &visibility_state, generated_at);
        let changed = today_apply_visibility_state(changed, &visibility_state, generated_at);
        let active_work =
            today_apply_visibility_state(active_work, &visibility_state, generated_at);
        let followups = today_apply_visibility_state(followups, &visibility_state, generated_at);
        self.record_today_route_events(
            principal,
            workspace,
            generated_at,
            &needs_you,
            &delivered,
            &changed,
            &active_work,
            &followups,
        );

        Ok(TodayProjection {
            needs_you,
            delivered,
            changed,
            active_work,
            followups,
            generated_at,
        })
    }

    /// [`Self::today_projection`], served from the per-scope cache whenever a
    /// live entry for this scope and date exists.
    ///
    /// **The computation happens outside the map.** `get` drops its read
    /// guard before returning, the `await` runs while nothing is held, and the
    /// insert happens afterwards — no `DashMap` guard is ever held across an
    /// `await`.
    ///
    /// A miss, an aged-out entry, or an entry a write has dropped all mean
    /// compute. A failed computation is handed back to the caller and never
    /// stored: the cache is best effort, and a cache that remembered errors
    /// would turn one bad read into ten seconds of them.
    ///
    /// Returns which of the two happened, so the response can report it
    /// rather than calling every projection live.
    async fn today_projection_cached(
        &self,
        principal: &str,
        workspace: &str,
        collection_limit: usize,
        overfetch_limit: usize,
        reader_today: chrono::NaiveDate,
        generated_at: i64,
    ) -> std::result::Result<(TodayProjection, TodayProjectionSource), String> {
        // The date in the key is the READER's, the same value the collector
        // evaluates its predicates against — never a second reading of the
        // clock, which would let two readers in different timezones share an
        // entry neither of them asked for.
        let key = TodayCacheKey::new(principal, workspace, reader_today.to_string());
        if let Some(cached) = self.today_projection_cache.get(&key) {
            return Ok((cached, TodayProjectionSource::Cache));
        }
        // Read the scope's write count BEFORE the walk, not after. A task
        // record that commits while the corpus is being read makes this
        // projection stale the moment it exists, and stamping it afterwards
        // would record a number saying the write was included. The insert
        // below declines in exactly that case, so the reader still gets the
        // newest projection available and no *later* reader is served it.
        let write_stamp = self
            .today_projection_cache
            .write_stamp(principal, workspace);
        let projection = self
            .today_projection(
                principal,
                workspace,
                collection_limit,
                overfetch_limit,
                reader_today,
                generated_at,
            )
            .await?;
        self.today_projection_cache
            .insert_if_unwritten_since(key, projection.clone(), write_stamp);
        Ok((projection, TodayProjectionSource::Computed))
    }

    /// Enumerate acknowledged action-node Today candidates across every ACTIVE
    /// thinking map in the scope. Read-side only: nothing is written. Store
    /// read failures degrade to an empty projection (Today must stay correct
    /// without the feature) rather than failing the whole Today response.
    async fn today_thinking_map_action_items(
        &self,
        principal: &str,
        workspace: &str,
        limit: usize,
        known_tasks: &[TaskListItemV3],
        include_promoted: bool,
    ) -> Vec<TodayItem> {
        let store = ThinkingMapStore::new(self.v3_service.workspace().clone());
        let summaries = match store.list_maps(principal, workspace).await {
            Ok(summaries) => summaries,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "[FEED-API] ignoring unreadable thinking maps for Today follow-ups"
                );
                return Vec::new();
            },
        };
        let mut items = Vec::new();
        for summary in summaries {
            if summary.lifecycle != MapLifecycle::Active {
                continue;
            }
            match store.load_map(principal, workspace, &summary.map_id).await {
                Ok(Some(map)) => items.extend(today_thinking_map_action_items_from_map(
                    &map,
                    principal,
                    workspace,
                    known_tasks,
                    include_promoted,
                )),
                Ok(None) => {},
                Err(error) => {
                    tracing::warn!(
                        map_id = %summary.map_id,
                        error = %error,
                        "[FEED-API] ignoring unreadable thinking map for Today follow-ups"
                    );
                },
            }
        }
        items.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        let mut items = today_dedupe_followup_items(items);
        items.truncate(limit);
        items
    }

    /// Recurring Monitors Phase 3 (§9.2): project EMITTED material monitor
    /// updates (and opted-in baselines) into Today `Changed`. Read-side only
    /// over the durable per-task update ledgers — the backend already gated
    /// policy, dedupe, and status at acceptance, so this never re-decides
    /// materiality. Store read failures degrade to an empty projection
    /// (Today must stay correct without the feature).
    ///
    /// Takes the listing `/today` already holds rather than the scope strings:
    /// this used to walk the corpus for a second time and then re-read every
    /// listed task's record a third, to answer "is this one a monitor" off
    /// `manifest.monitor_spec` — a fact the listed row carries as
    /// `monitor_revision`. In the author's own scope that was 137 record
    /// re-reads per `/today` to discover that none of the 137 is a monitor.
    async fn today_monitor_update_items(
        &self,
        scope: &ScopeRef,
        tasks: &[TaskListItemV3],
        limit: usize,
    ) -> Vec<TodayItem> {
        let principal = scope.principal().to_string();
        let workspace = scope.workspace();
        let updates = match self
            .v3_service
            .list_scope_monitor_updates_from(scope, tasks, limit.max(1))
            .await
        {
            Ok(updates) => updates,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "[FEED-API] ignoring unreadable monitor updates for Today Changed"
                );
                return Vec::new();
            },
        };
        updates
            .into_iter()
            .filter(update_projects_to_changed)
            .map(|update| today_item_from_monitor_update(update, &principal, &workspace))
            .collect()
    }

    pub async fn execute_today_item_action(
        &self,
        req: &HttpRequest,
        path: web::Path<(String, String)>,
        query: web::Query<TodayActionQuery>,
        broadcaster: Option<&Arc<RuntimeTransportBroadcaster>>,
    ) -> Result<HttpResponse> {
        let query = query.into_inner();
        let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let (item_id, action_id) = path.into_inner();
        // Re-project ONLY the source family the item id names. Thinking-map
        // candidates are enumerated with already-promoted nodes included so a
        // retry against a linked node resolves the existing task instead of
        // 404ing.
        let candidates = if item_id.starts_with(TODAY_THINKING_MAP_ITEM_PREFIX) {
            self.today_thinking_map_action_items(
                &principal,
                &workspace,
                TODAY_SECTION_COLLECTION_LIMIT,
                &[],
                true,
            )
            .await
        } else {
            today_meeting_followup_items_from_memory(
                self.v3_service.workspace(),
                &principal,
                &workspace,
                TODAY_SECTION_COLLECTION_LIMIT,
                &[],
            )
        };
        let Some(item) = candidates.into_iter().find(|item| item.id == item_id) else {
            return Ok(HttpResponse::NotFound().json(json!({
                "error": "today_item_not_found",
                "item_id": item_id,
            })));
        };
        let subject = today_action_subject(&item);
        let Some(adapter) = today_action_adapter(&subject.source_kind) else {
            return Ok(HttpResponse::BadRequest().json(json!({
                "error": "today_source_has_no_action_adapter",
                "source_kind": subject.source_kind,
            })));
        };
        let plan = match adapter.plan_action(&action_id, &subject) {
            Ok(plan) => plan,
            Err(error) => {
                return Ok(HttpResponse::BadRequest().json(json!({
                    "error": "today_action_not_supported",
                    "message": error.to_string(),
                })))
            },
        };
        let scope =
            ScopeRef::system_internal_unauthenticated(&principal.clone(), &workspace.clone());
        let existing_tasks = match self.v3_service.list_tasks(&scope).await {
            Ok(tasks) => tasks,
            Err(error) => {
                return Ok(HttpResponse::InternalServerError().json(json!({
                    "error": format!("failed to reconcile Today action tasks: {error}"),
                })))
            },
        };
        if let Some(existing) = existing_tasks
            .iter()
            .find(|task| adapter.task_is_linked(&subject, task))
        {
            let existing_task_id = existing.id.clone();
            return match self.v3_service.get_task(&scope, &existing_task_id).await {
                Ok(task) => Ok(HttpResponse::Ok().json(json!({
                    "action_id": action_id,
                    "item_id": item_id,
                    "task_id": existing_task_id,
                    "task": task,
                    "reused_task": true,
                    "navigate_to": {
                        "kind": "task",
                        "task_id": existing_task_id,
                    },
                }))),
                Err(error) => Ok(HttpResponse::InternalServerError().json(json!({
                    "error": format!("failed to open linked Today task: {error}"),
                }))),
            };
        }
        match plan {
            TodayActionPlan::EnsureTask(plan) => {
                match self
                    .v3_service
                    .ensure_task_with_id(plan.input, plan.task_id.clone())
                    .await
                {
                    Ok(task) => {
                        let task_id = task.manifest.task_id.clone();
                        // The task now exists, which suppresses the Today row
                        // that spawned it and adds a follow-up of its own.
                        // Invalidated after the create committed.
                        self.invalidate_today_projection(&principal, &workspace);
                        let body = json!({
                            "action_id": action_id,
                            "item_id": item_id,
                            "task_id": task_id,
                            "task": task,
                            "reused_task": false,
                            "navigate_to": {
                                "kind": "task",
                                "task_id": plan.task_id,
                            },
                        });
                        Ok(HttpResponse::Created().json(body))
                    },
                    Err(magician::magician_v2::artifact_v2::ArtifactV2Error::InvalidRequest(
                        message,
                    )) => Ok(HttpResponse::Conflict().json(json!({
                        "error": message,
                        "task_id": plan.task_id,
                    }))),
                    Err(error) => Ok(HttpResponse::InternalServerError().json(json!({
                        "error": error.to_string(),
                    }))),
                }
            },
            // Thinking-map candidates create tasks through the EXISTING
            // governed promote flow (`/thinking-maps/{map}/nodes/{node}/promote
            // {target:"task"}` semantics): idempotent via the node's
            // promoted_ref, provenance-preserving, and the map records the
            // link — which suppresses this Today candidate for every task
            // status. No confirm is passed: only owner-asserted nodes are
            // projected, so a state that now requires confirmation surfaces
            // as an explicit 409 pointing back at the map.
            TodayActionPlan::PromoteThinkingMapNode(plan) => {
                let outcome = promote_node_to_target(
                    self.v3_service.workspace(),
                    Some(self.v3_service.clone()),
                    broadcaster,
                    &principal,
                    &workspace,
                    &plan.map_id,
                    &plan.node_id,
                    PromotionKind::Task,
                    false,
                )
                .await;
                match outcome {
                    NodePromotionOutcome::Promoted {
                        object_id: task_id, ..
                    } => {
                        // Same as the EnsureTask arm: the promotion links the
                        // node, which suppresses this Today candidate, so the
                        // projection built before it is now wrong.
                        self.invalidate_today_projection(&principal, &workspace);
                        let task = self.v3_service.get_task(&scope, &task_id).await.ok();
                        Ok(HttpResponse::Created().json(json!({
                            "action_id": action_id,
                            "item_id": item_id,
                            "task_id": task_id,
                            "task": task,
                            "reused_task": false,
                            "navigate_to": {
                                "kind": "task",
                                "task_id": task_id,
                            },
                        })))
                    },
                    NodePromotionOutcome::AlreadyLinked { object_id: task_id } => {
                        let task = self.v3_service.get_task(&scope, &task_id).await.ok();
                        Ok(HttpResponse::Ok().json(json!({
                            "action_id": action_id,
                            "item_id": item_id,
                            "task_id": task_id,
                            "task": task,
                            "reused_task": true,
                            "navigate_to": {
                                "kind": "task",
                                "task_id": task_id,
                            },
                        })))
                    },
                    NodePromotionOutcome::NotPromotable {
                        epistemic_state,
                        tombstoned,
                    } => Ok(HttpResponse::Conflict().json(json!({
                        "error": "not_promotable",
                        "epistemic_state": epistemic_state,
                        "tombstoned": tombstoned,
                    }))),
                    NodePromotionOutcome::ConfirmationRequired {
                        assertion_origin,
                        epistemic_state,
                    } => Ok(HttpResponse::Conflict().json(json!({
                        "error": "confirmation_required",
                        "assertion_origin": assertion_origin,
                        "epistemic_state": epistemic_state,
                        "map_url": format!(
                            "/thinking-maps/{}?node={}",
                            urlencoding::encode(&plan.map_id),
                            urlencoding::encode(&plan.node_id)
                        ),
                    }))),
                    NodePromotionOutcome::MapNotFound => Ok(HttpResponse::NotFound().json(json!({
                        "error": "not_found",
                        "map_id": plan.map_id,
                    }))),
                    NodePromotionOutcome::NodeNotFound => {
                        Ok(HttpResponse::NotFound().json(json!({
                            "error": "node_not_found",
                            "node_id": plan.node_id,
                        })))
                    },
                    NodePromotionOutcome::TargetUnavailable => {
                        Ok(HttpResponse::ServiceUnavailable().json(json!({
                            "error": "promotion_unavailable",
                        })))
                    },
                    NodePromotionOutcome::CreateFailed { detail } => Ok(HttpResponse::BadGateway()
                        .json(json!({
                            "error": "task_create_failed",
                            "detail": detail,
                        }))),
                    NodePromotionOutcome::Store(error) => Ok(thinking_map_store_error(error)),
                }
            },
        }
    }

    fn record_today_route_events(
        &self,
        principal: &str,
        workspace: &str,
        generated_at: i64,
        needs_you: &[TodayItem],
        delivered: &[TodayItem],
        changed: &[TodayItem],
        active_work: &[TodayItem],
        followups: &[TodayItem],
    ) {
        let Some(attention_store) = self.attention_store.clone() else {
            return;
        };
        let events = today_route_events(
            principal,
            workspace,
            generated_at,
            needs_you,
            delivered,
            changed,
            active_work,
            followups,
        );
        if events.is_empty() {
            return;
        }
        tokio::spawn(async move {
            if let Err(error) = attention_store.append_events(events).await {
                tracing::warn!(
                    error = %error,
                    "[FEED-API] failed to record Today attention route events batch"
                );
            }
        });
    }

    fn record_feed_attention_route_events(
        &self,
        principal: &str,
        workspace: &str,
        generated_at: i64,
        requests: &[FeedItem],
        approvals: &[FeedItem],
        escalations: &[FeedItem],
        failed: &[FeedItem],
        running: &[FeedItem],
    ) {
        let Some(attention_store) = self.attention_store.clone() else {
            return;
        };
        let events = feed_attention_route_events(
            principal,
            workspace,
            generated_at,
            requests,
            approvals,
            escalations,
            failed,
            running,
        );
        if events.is_empty() {
            return;
        }
        tokio::spawn(async move {
            if let Err(error) = attention_store.append_events(events).await {
                tracing::warn!(
                    error = %error,
                    "[FEED-API] failed to record feed attention route events batch"
                );
            }
        });
    }

    /// Drop this scope's cached Today projection.
    ///
    /// **Load-bearing, not hygiene.** Both clients refetch the page they are
    /// on after removing a row. A stale cache hit on that refetch resurrects
    /// the row the reader just dismissed — worse than the gap the refetch was
    /// added to close.
    ///
    /// Every caller invalidates **after** its write commits. An invalidation
    /// that ran first could be re-populated by a concurrent read that still
    /// saw the pre-write state, and the reader would be handed the row they
    /// just removed for a further TTL.
    ///
    /// Dropping the scope rather than one date is deliberate: a reader may
    /// hold an entry either side of UTC midnight and a write makes both wrong.
    /// No other scope is touched.
    fn invalidate_today_projection(&self, principal: &str, workspace: &str) {
        self.today_projection_cache
            .invalidate_scope(principal, workspace);
    }

    fn today_visibility_state(&self, principal: &str, workspace: &str) -> TodayVisibilityState {
        let workspace_layout = self.v3_service.workspace();
        let path = today_visibility_state_path(workspace_layout, principal, workspace);
        load_today_visibility_state(workspace_layout, &path).unwrap_or_default()
    }

    /// [`Self::today_visibility_state`] off the reactor.
    ///
    /// A 20KB read and parse is small next to the corpus walk, and it is still
    /// a synchronous `open`/`read` on an actix worker in the middle of a
    /// request that already had four other reasons to block. The rule is
    /// about where blocking work runs, not about how much of it there is.
    async fn today_visibility_state_off_reactor(
        &self,
        principal: &str,
        workspace: &str,
    ) -> TodayVisibilityState {
        let workspace_layout = self.v3_service.workspace().clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let path = today_visibility_state_path(&workspace_layout, &principal, &workspace);
            load_today_visibility_state(&workspace_layout, &path).unwrap_or_default()
        })
        .await
        .unwrap_or_default()
    }

    /// [`today_meeting_followup_items_from_memory`] with the 1.1MB read and
    /// parse off the reactor and the projection itself left on it.
    ///
    /// Only the read moves. `today_meeting_followup_items_from_knowledge` is
    /// pure and borrows `known_tasks`, which is the whole corpus listing —
    /// cloning 137 `TaskListItemV3`s, `description` and all, into a blocking
    /// worker to avoid one borrow would cost more than the block it saved.
    async fn today_meeting_followup_items_from_memory_off_reactor(
        &self,
        principal: &str,
        workspace: &str,
        limit: usize,
        known_tasks: &[TaskListItemV3],
    ) -> Vec<TodayItem> {
        let workspace_layout = self.v3_service.workspace().clone();
        let owned_principal = principal.to_string();
        let owned_workspace = workspace.to_string();
        let knowledge = tokio::task::spawn_blocking(move || {
            let path =
                today_meeting_knowledge_path(&workspace_layout, &owned_principal, &owned_workspace);
            read_today_meeting_knowledge(&workspace_layout, &path)
        })
        .await;
        let knowledge = match knowledge {
            Ok(Some(knowledge)) => knowledge,
            // Absent or unreadable is the documented empty projection; a join
            // failure is the same outcome for the reader, and the warning is
            // the only thing that distinguishes them.
            Ok(None) => return Vec::new(),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "[FEED-API] meeting-memory read failed to join; Today follow-ups \
                     project empty this pass"
                );
                return Vec::new();
            },
        };
        today_meeting_followup_items_from_knowledge(
            &knowledge,
            principal,
            workspace,
            limit,
            known_tasks,
        )
    }

    pub async fn list_today_visibility(
        &self,
        req: &HttpRequest,
        query: web::Query<TodayVisibilityQuery>,
    ) -> Result<HttpResponse> {
        let query = query.into_inner();
        let limit = query.limit.clamp(1, 100);
        let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let state = self.today_visibility_state(&principal, &workspace);
        let now = chrono::Utc::now().timestamp_millis();
        let mut items = state
            .items
            .into_iter()
            .filter_map(|(item_id, record)| {
                let hidden_kind = if record.dismissed_at.is_some() {
                    Some("dismissed")
                } else if record
                    .snoozed_until
                    .is_some_and(|snoozed_until| snoozed_until > now)
                {
                    Some("snoozed")
                } else {
                    None
                }?;
                Some(TodayVisibilityListItem {
                    item_id,
                    hidden_kind: hidden_kind.to_string(),
                    record,
                })
            })
            .collect::<Vec<_>>();
        items.sort_by(|left, right| right.record.updated_at.cmp(&left.record.updated_at));
        items.truncate(limit);

        Ok(HttpResponse::Ok().json(TodayVisibilityListResponse { items }))
    }

    pub async fn update_today_visibility(
        &self,
        req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<TodayVisibilityQuery>,
        body: web::Json<TodayVisibilityRequest>,
    ) -> Result<HttpResponse> {
        let query = query.into_inner();
        let body = body.into_inner();
        let item_id = path.into_inner();
        let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let now = chrono::Utc::now().timestamp_millis();
        let workspace_layout = self.v3_service.workspace();
        let path = today_visibility_state_path(workspace_layout, &principal, &workspace);
        let mut state = load_today_visibility_state(workspace_layout, &path).unwrap_or_default();
        state.version = 1;

        let action = body.action.trim().to_ascii_lowercase();
        let snapshot = body.snapshot.clone();
        let record = match action.as_str() {
            "mark_seen" | "seen" => {
                let record = state.items.entry(item_id.clone()).or_default();
                record.seen_at = Some(record.seen_at.unwrap_or(now));
                if snapshot.is_some() {
                    record.snapshot = snapshot.clone();
                }
                record.updated_at = now;
                Some(record.clone())
            },
            "dismiss" => {
                let record = state.items.entry(item_id.clone()).or_default();
                record.seen_at = Some(record.seen_at.unwrap_or(now));
                record.dismissed_at = Some(now);
                record.snoozed_until = None;
                if snapshot.is_some() {
                    record.snapshot = snapshot.clone();
                }
                record.updated_at = now;
                Some(record.clone())
            },
            "snooze" => {
                let snooze_until = body
                    .snooze_until
                    .or_else(|| {
                        body.snooze_minutes.map(|minutes| {
                            now + minutes.clamp(1, 60 * 24 * 365).saturating_mul(60_000)
                        })
                    })
                    .unwrap_or(now + 24 * 60 * 60 * 1000);
                if snooze_until <= now {
                    return Ok(HttpResponse::BadRequest().json(json!({
                        "error": "snooze_until must be in the future"
                    })));
                }
                let record = state.items.entry(item_id.clone()).or_default();
                record.seen_at = Some(record.seen_at.unwrap_or(now));
                record.dismissed_at = None;
                record.snoozed_until = Some(snooze_until);
                if snapshot.is_some() {
                    record.snapshot = snapshot.clone();
                }
                record.updated_at = now;
                Some(record.clone())
            },
            "restore" => {
                state.items.remove(&item_id);
                None
            },
            _ => {
                return Ok(HttpResponse::BadRequest().json(json!({
                    "error": "action must be mark_seen, dismiss, snooze, or restore"
                })));
            },
        };

        if let Err(error) = workspace_layout.write_json_atomic_path_sync(&path, &state) {
            return Ok(HttpResponse::InternalServerError().json(json!({
                "error": format!("{error}")
            })));
        }
        // The write has committed, so the cached projection — which was
        // filtered through the state as it stood a moment ago — is now wrong
        // for this scope. The client's next request is its own refetch of the
        // page it is on; without this it would be served the row it just
        // dismissed.
        self.invalidate_today_projection(&principal, &workspace);

        Ok(HttpResponse::Ok().json(TodayVisibilityResponse { item_id, record }))
    }

    /// Upsert (or reverse) a server-side dismissal for an attention item,
    /// keyed by the raw [`FeedItem::id`]. `dismissed = true` records
    /// `{ dismissed_at: now }`; `dismissed = false` removes the record
    /// (undismiss). This is the durable, cross-surface replacement for the
    /// frontend's per-browser `localStorage['attention:dismissed-failed']`.
    /// FeedStore is authoritative for indexed attention queries; the legacy
    /// JSON file is retained only as a migration/compatibility mirror.
    pub async fn dismiss_attention_item(
        &self,
        req: &HttpRequest,
        query: web::Query<AttentionDismissQuery>,
        body: web::Json<AttentionDismissRequest>,
    ) -> Result<HttpResponse> {
        let query = query.into_inner();
        let body = body.into_inner();
        let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let item_id = body.item_id.trim().to_string();
        if item_id.is_empty() {
            return Ok(HttpResponse::BadRequest().json(json!({
                "error": "item_id must not be empty"
            })));
        }
        let now = chrono::Utc::now().timestamp_millis();
        let workspace_layout = self.v3_service.workspace();
        let path = attention_dismissed_state_path(workspace_layout, &principal, &workspace);
        let mut state = load_attention_dismissed_state(workspace_layout, &path).unwrap_or_default();
        state.version = 1;

        let record = if body.dismissed {
            let record = state
                .items
                .entry(item_id.clone())
                .or_insert(AttentionDismissedRecord { dismissed_at: now });
            record.dismissed_at = now;
            Some(record.clone())
        } else {
            state.items.remove(&item_id);
            None
        };

        if let Err(error) = self
            .feed_store
            .set_attention_dismissed(&principal, &workspace, &item_id, body.dismissed, now)
            .await
        {
            return Ok(HttpResponse::InternalServerError().json(json!({
                "error": format!("{error}")
            })));
        }
        if let Err(error) = save_attention_dismissed_state(workspace_layout, &path, &state) {
            tracing::warn!(
                principal = %principal,
                workspace = %workspace,
                item_id = %item_id,
                error = %error,
                "[FEED-API] failed to mirror indexed attention dismissal to legacy JSON"
            );
        }
        // Attention rows feed Today's "Needs You" lane, so a dismissal here
        // changes the projection too. Invalidated after the authoritative
        // feed-store write above committed, not before it.
        self.invalidate_today_projection(&principal, &workspace);

        Ok(HttpResponse::Ok().json(AttentionDismissedResponse {
            item_id,
            dismissed: body.dismissed,
            record,
        }))
    }

    /// The `Changed` digest, memoised on disk against a fingerprint of the
    /// lanes it was built from.
    ///
    /// # Neither half of the memo runs on the reactor any more
    ///
    /// The read was a synchronous load and parse. The write was worse: two
    /// `fsync`s — `write_bytes_atomic_sync` syncs the staging file and then
    /// the parent directory — taken inline whenever the fingerprint moved.
    /// And the fingerprint hashes item titles and summaries, so during a run
    /// it moves on *every* poll: this was two `fsync`s on an actix worker at
    /// the client's polling rate.
    ///
    /// **The write is now detached** rather than awaited. Nothing in the
    /// response depends on it — `digest` is already computed and returned —
    /// and its only reader is the next request's load, which on a miss simply
    /// recomputes a pure function. Two concurrent `/today`s racing to write it
    /// resolve as they always did, by atomic rename: last one wins, and either
    /// file is a valid memo of a real fingerprint.
    async fn today_changed_digest_cached(
        &self,
        principal: &str,
        workspace: &str,
        generated_at: i64,
        force_refresh: bool,
        needs_you: &[TodayItem],
        delivered: &[TodayItem],
        changed: &[TodayItem],
        active_work: &[TodayItem],
    ) -> TodayChangedDigest {
        let workspace_layout = self.v3_service.workspace();
        let cache_path = today_digest_cache_path(workspace_layout, principal, workspace);
        let source_fingerprint =
            today_digest_source_fingerprint(needs_you, delivered, changed, active_work);
        let cached_state = {
            let reader_layout = workspace_layout.clone();
            let reader_path = cache_path.clone();
            tokio::task::spawn_blocking(move || {
                load_today_digest_cache_state(&reader_layout, &reader_path)
            })
            .await
            .unwrap_or_default()
        };
        if !force_refresh {
            if let Some(state) = cached_state.as_ref() {
                if state.version == 2 && state.source_fingerprint == source_fingerprint {
                    return state.digest.clone();
                }
            }
        }

        let since = cached_state.as_ref().map(|state| state.digest.generated_at);
        let digest = today_changed_digest(
            generated_at,
            since,
            needs_you,
            delivered,
            changed,
            active_work,
        );
        let next_state = TodayDigestCacheState {
            version: 2,
            source_fingerprint,
            digest: digest.clone(),
        };
        let writer_layout = workspace_layout.clone();
        let owned_principal = principal.to_string();
        let owned_workspace = workspace.to_string();
        // Detached: the response is complete without it, and awaiting it would
        // put both of the write's `fsync`s in front of the reader.
        tokio::task::spawn_blocking(move || {
            if let Err(error) = writer_layout.write_json_atomic_path_sync(&cache_path, &next_state)
            {
                tracing::warn!(
                    principal = %owned_principal,
                    workspace = %owned_workspace,
                    path = %cache_path.display(),
                    error = %error,
                    "[FEED-API] failed to persist Today digest cache"
                );
            }
        });
        digest
    }

    /// Delete a single feed item by id. Persists the removal in the feed
    /// store and emits a `FeedItemRemoved` realtime event so connected
    /// frontends update without a refresh.
    pub async fn delete_item(
        &self,
        req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<FeedCountsQuery>,
    ) -> Result<HttpResponse> {
        let query = query.into_inner();
        let item_id = path.into_inner();
        let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        // Look up the item first so the FeedItemRemoved event can carry
        // task_id + ui_thread_id + execution_id (the consumer indexes by
        // those). Fall through to a bare `principal/workspace/id` event
        // when the item doesn't exist.
        let existing = self
            .feed_store
            .get_item(&principal, &workspace, &item_id)
            .await
            .ok()
            .flatten();
        let removed = match self
            .feed_store
            .remove_item(&principal, &workspace, &item_id)
            .await
        {
            Ok(removed) => removed,
            Err(error) => {
                return Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": format!("{error}")
                })));
            },
        };
        if removed {
            if let Some(broadcaster) = self.v3_service.runtime_event_broadcaster() {
                let task_id = existing.as_ref().and_then(|i| i.task_id.clone());
                let ui_thread_id = existing.as_ref().and_then(|i| i.ui_thread_id.clone());
                let execution_id = existing
                    .as_ref()
                    .and_then(|i| metadata_string(&i.metadata, "execution_id"));
                broadcaster.emit_transport_only(RuntimeTransportEvent::FeedItemRemoved {
                    principal: principal.clone(),
                    workspace: workspace.clone(),
                    id: item_id.clone(),
                    task_id,
                    ui_thread_id,
                    execution_id,
                    timestamp: chrono::Utc::now().timestamp_millis(),
                });
            }
            // Today is projected out of these same rows, so a row deleted
            // here is a row the cached projection would still hand back for
            // the rest of the window. Invalidated after the removal
            // committed, never before.
            self.invalidate_today_projection(&principal, &workspace);
        }
        Ok(HttpResponse::Ok().json(serde_json::json!({
            "removed": removed,
            "id": item_id,
        })))
    }

    /// Clear every feed item in the scope (or thread when `ui_thread_id`
    /// is set). Persists the deletion and emits one `FeedItemRemoved`
    /// event per removed item so connected frontends drop them live.
    pub async fn clear_items(
        &self,
        req: &HttpRequest,
        query: web::Query<FeedListQuery>,
    ) -> Result<HttpResponse> {
        let query = query.into_inner();
        let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let items = match self
            .feed_store
            .list_items(FeedQuery {
                principal: principal.clone(),
                workspace: workspace.clone(),
                before: None,
                after: None,
                limit: usize::MAX,
                task_id: None,
                ui_thread_id: query.ui_thread_id.clone(),
                item_type: None,
                status: None,
                agent_id: None,
            })
            .await
        {
            Ok(items) => items,
            Err(error) => {
                return Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": format!("{error}")
                })));
            },
        };
        let broadcaster = self.v3_service.runtime_event_broadcaster();
        let mut removed_ids = Vec::with_capacity(items.len());
        for item in items {
            match self
                .feed_store
                .remove_item(&item.principal, &item.workspace, &item.id)
                .await
            {
                Ok(true) => {
                    if let Some(broadcaster) = broadcaster {
                        let execution_id = metadata_string(&item.metadata, "execution_id");
                        broadcaster.emit_transport_only(RuntimeTransportEvent::FeedItemRemoved {
                            principal: item.principal.clone(),
                            workspace: item.workspace.clone(),
                            id: item.id.clone(),
                            task_id: item.task_id.clone(),
                            ui_thread_id: item.ui_thread_id.clone(),
                            execution_id,
                            timestamp: chrono::Utc::now().timestamp_millis(),
                        });
                    }
                    removed_ids.push(item.id);
                },
                Ok(false) => {},
                Err(error) => {
                    tracing::warn!(
                        item_id = %item.id,
                        %error,
                        "[FEED-API] failed to remove item during clear"
                    );
                },
            }
        }
        // One invalidation for the whole clear rather than one per row: the
        // scope holds no entries after the first, and the rest of the loop
        // would only re-walk a map it had already emptied.
        if !removed_ids.is_empty() {
            self.invalidate_today_projection(&principal, &workspace);
        }
        Ok(HttpResponse::Ok().json(serde_json::json!({
            "removed_count": removed_ids.len(),
            "removed_ids": removed_ids,
        })))
    }

    /// Garbage-collects feed items whose `task_id` references a task that
    /// no longer exists. Emits `FeedItemRemoved` for each row removed so
    /// connected clients drop them without a refresh. Idempotent.
    ///
    /// Refuses to run when task listing fails — without an authoritative
    /// task list we cannot tell orphans from live items, and a bare
    /// "DELETE everything with a task_id" would be catastrophic.
    pub async fn purge_orphans(
        &self,
        req: &HttpRequest,
        query: web::Query<FeedCountsQuery>,
    ) -> Result<HttpResponse> {
        let query = query.into_inner();
        let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let valid_task_ids = match self.valid_task_ids(&principal, &workspace).await {
            Some(set) => set,
            None => {
                return Ok(HttpResponse::ServiceUnavailable().json(serde_json::json!({
                    "error": "task_listing_unavailable",
                    "detail": "refusing to purge orphans without an authoritative task list",
                })));
            },
        };
        let removed = match self
            .feed_store
            .purge_orphan_items(&principal, &workspace, &valid_task_ids)
            .await
        {
            Ok(removed) => removed,
            Err(error) => {
                return Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": format!("{error}")
                })));
            },
        };
        if let Some(broadcaster) = self.v3_service.runtime_event_broadcaster() {
            for item in &removed {
                broadcaster.emit_transport_only(RuntimeTransportEvent::FeedItemRemoved {
                    principal: item.principal.clone(),
                    workspace: item.workspace.clone(),
                    id: item.id.clone(),
                    task_id: item.task_id.clone(),
                    ui_thread_id: item.ui_thread_id.clone(),
                    execution_id: metadata_string(&item.metadata, "execution_id"),
                    timestamp: chrono::Utc::now().timestamp_millis(),
                });
            }
        }
        // Orphan rows the projection filters out on the read side are not the
        // only thing this purges: the learning and routine lanes are collected
        // without the task-existence filter, so a purge can remove a row Today
        // was showing. One invalidation after the purge committed.
        if !removed.is_empty() {
            self.invalidate_today_projection(&principal, &workspace);
        }
        let removed_ids: Vec<String> = removed.iter().map(|item| item.id.clone()).collect();
        Ok(HttpResponse::Ok().json(serde_json::json!({
            "removed_count": removed_ids.len(),
            "removed_ids": removed_ids,
        })))
    }

    pub async fn confirm_learning_candidate(
        &self,
        req: &HttpRequest,
        path: web::Path<String>,
        body: web::Json<FeedLearningActionRequest>,
    ) -> Result<HttpResponse> {
        let candidate_id = path.into_inner();
        let request = body.into_inner();
        let (principal, workspace) = match resolve_required_scope(req.headers(), request.workspace)
        {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let store = self.learning_store();
        let scope = LearningScope::new(principal.clone(), workspace.clone());
        let candidate = match store.read_candidate(&scope, &candidate_id) {
            Ok(candidate) => candidate,
            Err(error) => {
                return Ok(HttpResponse::NotFound().json(json!({
                    "error": error.to_string()
                })));
            },
        };
        if learning_candidate_feed_item(&candidate).is_none() {
            return Ok(HttpResponse::BadRequest().json(json!({
                "error": "candidate is not a reviewable user-memory learning candidate"
            })));
        }
        let actor = request
            .actor
            .unwrap_or_else(|| FEED_LEARNING_ACTOR.to_string());
        let reason = request
            .reason
            .unwrap_or_else(|| "User confirmed the learning from the feed.".to_string());
        let bridge = LearningMemoryBridge::new(self.v3_service.workspace().clone());
        match bridge
            .promote_reviewed_candidate(&store, &scope, &candidate, &actor, &reason)
            .await
        {
            Ok(candidate) => {
                let removed = self
                    .remove_feed_item_with_event(
                        &principal,
                        &workspace,
                        &learning_candidate_feed_id(&candidate_id),
                    )
                    .await
                    .unwrap_or(false);
                Ok(HttpResponse::Ok().json(json!({
                    "candidate": candidate,
                    "removed_feed_item": removed
                })))
            },
            Err(error) => Ok(HttpResponse::BadRequest().json(json!({
                "error": error.to_string()
            }))),
        }
    }

    pub async fn edit_confirm_learning_candidate(
        &self,
        req: &HttpRequest,
        path: web::Path<String>,
        body: web::Json<FeedLearningActionRequest>,
    ) -> Result<HttpResponse> {
        let candidate_id = path.into_inner();
        let request = body.into_inner();
        let (principal, workspace) =
            match resolve_required_scope(req.headers(), request.workspace.clone()) {
                Ok(scope) => scope,
                Err(response) => return Ok(response),
            };
        let revised_value = match request
            .revised_value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(value) => value.to_string(),
            None => {
                return Ok(HttpResponse::BadRequest().json(json!({
                    "error": "revised_value is required"
                })));
            },
        };
        let actor = request
            .actor
            .clone()
            .unwrap_or_else(|| FEED_LEARNING_ACTOR.to_string());
        let reason = request
            .reason
            .clone()
            .unwrap_or_else(|| "User edited and confirmed the learning from the feed.".to_string());
        let store = self.learning_store();
        let scope = LearningScope::new(principal.clone(), workspace.clone());
        let candidate = match store.read_candidate(&scope, &candidate_id) {
            Ok(candidate) => candidate,
            Err(error) => {
                return Ok(HttpResponse::NotFound().json(json!({
                    "error": error.to_string()
                })));
            },
        };
        if learning_candidate_feed_item(&candidate).is_none() {
            return Ok(HttpResponse::BadRequest().json(json!({
                "error": "candidate is not a reviewable user-memory learning candidate"
            })));
        }
        let edit_result = match public_contact_identity_research_revised_value(
            &candidate,
            &revised_value,
            &actor,
        ) {
            Some((structured_value, structured_summary)) => store
                .revise_memory_candidate_json_value(
                    &scope,
                    &candidate_id,
                    structured_value,
                    structured_summary,
                    actor.clone(),
                    reason.clone(),
                ),
            None => store.revise_memory_candidate_value(
                &scope,
                &candidate_id,
                revised_value,
                actor.clone(),
                reason.clone(),
            ),
        };
        let edited = match edit_result {
            Ok(candidate) => candidate,
            Err(error) => {
                return Ok(HttpResponse::BadRequest().json(json!({
                    "error": error.to_string()
                })));
            },
        };
        let bridge = LearningMemoryBridge::new(self.v3_service.workspace().clone());
        match bridge
            .promote_reviewed_candidate(&store, &scope, &edited, &actor, &reason)
            .await
        {
            Ok(candidate) => {
                let removed = self
                    .remove_feed_item_with_event(
                        &principal,
                        &workspace,
                        &learning_candidate_feed_id(&candidate_id),
                    )
                    .await
                    .unwrap_or(false);
                Ok(HttpResponse::Ok().json(json!({
                    "candidate": candidate,
                    "removed_feed_item": removed
                })))
            },
            Err(error) => Ok(HttpResponse::BadRequest().json(json!({
                "error": error.to_string()
            }))),
        }
    }

    pub async fn archive_learning_candidate(
        &self,
        req: &HttpRequest,
        path: web::Path<String>,
        body: web::Json<FeedLearningActionRequest>,
    ) -> Result<HttpResponse> {
        let candidate_id = path.into_inner();
        let request = body.into_inner();
        let (principal, workspace) = match resolve_required_scope(req.headers(), request.workspace)
        {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let store = self.learning_store();
        let scope = LearningScope::new(principal.clone(), workspace.clone());
        let candidate = match store.read_candidate(&scope, &candidate_id) {
            Ok(candidate) => candidate,
            Err(error) => {
                return Ok(HttpResponse::NotFound().json(json!({
                    "error": error.to_string()
                })));
            },
        };
        if learning_candidate_feed_item(&candidate).is_none() {
            return Ok(HttpResponse::BadRequest().json(json!({
                "error": "candidate is not a reviewable user-memory learning candidate"
            })));
        }
        let actor = request
            .actor
            .unwrap_or_else(|| FEED_LEARNING_ACTOR.to_string());
        let reason = request
            .reason
            .unwrap_or_else(|| "User archived the learning from the feed.".to_string());
        match store.transition_candidate(
            &scope,
            &candidate_id,
            LearningCandidateState::Archived,
            actor,
            "feed_archived",
            reason,
            candidate.evidence_refs.clone(),
        ) {
            Ok(candidate) => {
                let removed = self
                    .remove_feed_item_with_event(
                        &principal,
                        &workspace,
                        &learning_candidate_feed_id(&candidate_id),
                    )
                    .await
                    .unwrap_or(false);
                Ok(HttpResponse::Ok().json(json!({
                    "candidate": candidate,
                    "removed_feed_item": removed
                })))
            },
            Err(error) => Ok(HttpResponse::BadRequest().json(json!({
                "error": error.to_string()
            }))),
        }
    }

    pub async fn archive_learning_insight(
        &self,
        req: &HttpRequest,
        path: web::Path<String>,
        body: web::Json<FeedInsightActionRequest>,
    ) -> Result<HttpResponse> {
        let insight_id = path.into_inner();
        let request = body.into_inner();
        let (principal, workspace) = match resolve_required_scope(req.headers(), request.workspace)
        {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
        let scope = LearningScope::new(principal.clone(), workspace.clone());
        let item = match self
            .resolve_learning_insight_item(&principal, &workspace, &insight_id)
            .await
        {
            Ok(item) => item,
            Err(response) => return Ok(response),
        };
        if let Err(error) = self.archive_learning_insight_id(
            &scope,
            &item.id,
            request.actor.as_deref().unwrap_or(FEED_LEARNING_ACTOR),
            request
                .reason
                .as_deref()
                .unwrap_or("Insight dismissed from the feed."),
        ) {
            return Ok(HttpResponse::InternalServerError().json(json!({
                "error": error.to_string()
            })));
        }
        let removed = self
            .remove_feed_item_with_event(&principal, &workspace, &item.id)
            .await
            .unwrap_or(false);
        self.append_learning_feed_event_best_effort(
            &scope,
            "learning_feed_insight_archived",
            &item,
            json!({
                "actor": request.actor.as_deref().unwrap_or(FEED_LEARNING_ACTOR),
                "reason": request.reason.as_deref().unwrap_or("Insight dismissed from the feed."),
                "removed_feed_item": removed
            }),
        );
        Ok(HttpResponse::Ok().json(json!({
            "insight_id": item.id,
            "removed_feed_item": removed
        })))
    }

    pub async fn save_learning_insight_to_memory(
        &self,
        req: &HttpRequest,
        path: web::Path<String>,
        body: web::Json<FeedInsightActionRequest>,
    ) -> Result<HttpResponse> {
        let insight_id = path.into_inner();
        let request = body.into_inner();
        let (principal, workspace) =
            match resolve_required_scope(req.headers(), request.workspace.clone()) {
                Ok(scope) => scope,
                Err(response) => return Ok(response),
            };
        let scope = LearningScope::new(principal.clone(), workspace.clone());
        let item = match self
            .resolve_learning_insight_item(&principal, &workspace, &insight_id)
            .await
        {
            Ok(item) => item,
            Err(response) => return Ok(response),
        };
        let memory_value = request
            .memory_value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| item.summary.clone())
            .unwrap_or_else(|| item.title.clone());
        let store = self.learning_store();
        let (candidate, reused_candidate) =
            match existing_saved_memory_candidate_for_insight(&store, &scope, &item.id) {
                Ok(Some(candidate)) => (candidate, true),
                Ok(None) => match store.create_candidate(
                    scope.clone(),
                    CreateLearningCandidateRequest {
                        principal: None,
                        workspace: None,
                        candidate_type: LearningCandidateType::MemoryFact,
                        state: LearningCandidateState::Proposed,
                        title: format!("Save insight: {}", item.title),
                        summary: memory_value.clone(),
                        rationale: "User asked to save a feed insight to memory.".to_string(),
                        proposed_change: json!({
                            "memory": {
                                "scope": "user",
                                "target_tier": "knowledge",
                                "key": insight_memory_key(&item),
                                "value": memory_value,
                                "source_type": "learning_insight",
                                "source_insight_id": item.id.clone(),
                            }
                        }),
                        proposed_target: Some("user.knowledge".to_string()),
                        confidence: metadata_number(&item.metadata, "confidence").or(Some(0.75)),
                        source_agent_id: item.agent_id.clone(),
                        source_task_id: item
                            .task_id
                            .clone()
                            .or_else(|| metadata_string(&item.metadata, "source_task_id")),
                        source_execution_id: metadata_string(&item.metadata, "source_execution_id"),
                        source_chat_session_id: metadata_string(
                            &item.metadata,
                            "source_chat_session_id",
                        ),
                        event_refs: Vec::new(),
                        evidence_refs: learning_evidence_refs_from_metadata(&item.metadata),
                        risk_level: LearningRiskLevel::Low,
                        review_required: true,
                        review_reason: Some(
                            "User requested saving an insight; review the exact wording before \
                             filing."
                                .to_string(),
                        ),
                        review_policy: json!({ "source": "feed_insight_save_to_memory" }),
                        promotion_target: Some("user.knowledge".to_string()),
                        promotion_policy: json!({ "bridge": "memory" }),
                    },
                ) {
                    Ok(candidate) => (candidate, false),
                    Err(error) => {
                        return Ok(HttpResponse::BadRequest().json(json!({
                            "error": error.to_string()
                        })));
                    },
                },
                Err(error) => {
                    return Ok(HttpResponse::InternalServerError().json(json!({
                        "error": error.to_string()
                    })));
                },
            };
        if let Err(error) = self.archive_learning_insight_id(
            &scope,
            &item.id,
            request.actor.as_deref().unwrap_or(FEED_LEARNING_ACTOR),
            "Insight converted into a reviewable memory candidate.",
        ) {
            return Ok(HttpResponse::InternalServerError().json(json!({
                "error": error.to_string(),
                "candidate_id": candidate.id,
                "reused_candidate": reused_candidate
            })));
        }
        let removed = self
            .remove_feed_item_with_event(&principal, &workspace, &item.id)
            .await
            .unwrap_or(false);
        self.sync_learning_candidate_feed(&principal, &workspace)
            .await
            .ok();
        self.append_learning_feed_event_best_effort(
            &scope,
            "learning_feed_insight_saved_to_memory",
            &item,
            json!({
                "candidate_id": candidate.id.clone(),
                "removed_feed_item": removed
            }),
        );
        Ok(HttpResponse::Ok().json(json!({
            "candidate": candidate,
            "removed_feed_item": removed,
            "reused_candidate": reused_candidate
        })))
    }

    pub async fn create_learning_insight_follow_up_task(
        &self,
        req: &HttpRequest,
        path: web::Path<String>,
        body: web::Json<FeedInsightActionRequest>,
    ) -> Result<HttpResponse> {
        let insight_id = path.into_inner();
        let request = body.into_inner();
        let (principal, workspace) =
            match resolve_required_scope(req.headers(), request.workspace.clone()) {
                Ok(scope) => scope,
                Err(response) => return Ok(response),
            };
        let scope = LearningScope::new(principal.clone(), workspace.clone());
        let item = match self
            .resolve_learning_insight_item(&principal, &workspace, &insight_id)
            .await
        {
            Ok(item) => item,
            Err(response) => return Ok(response),
        };
        if let Some(task) = match self
            .existing_follow_up_task_for_insight(&scope, &item.id)
            .await
        {
            Ok(task) => task,
            Err(error) => {
                return Ok(HttpResponse::InternalServerError().json(json!({
                    "error": error.to_string()
                })));
            },
        } {
            if let Err(error) = self.archive_learning_insight_id(
                &scope,
                &item.id,
                request.actor.as_deref().unwrap_or(FEED_LEARNING_ACTOR),
                "Insight converted into a follow-up task.",
            ) {
                return Ok(HttpResponse::InternalServerError().json(json!({
                    "error": error.to_string(),
                    "task_id": task.manifest.task_id,
                    "reused_task": true
                })));
            }
            let removed = self
                .remove_feed_item_with_event(&principal, &workspace, &item.id)
                .await
                .unwrap_or(false);
            return Ok(HttpResponse::Ok().json(json!({
                "task": task,
                "removed_feed_item": removed,
                "reused_task": true
            })));
        }
        let title = request
            .task_title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("Follow up: {}", item.title));
        let description = request
            .task_description
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| follow_up_task_description(&item));
        let actor = request.actor.as_deref().unwrap_or(FEED_LEARNING_ACTOR);
        if let Err(error) = self.reserve_follow_up_task_marker(&scope, &item, actor) {
            return Ok(HttpResponse::InternalServerError().json(json!({
                "error": error.to_string()
            })));
        }
        let task = match self
            .v3_service
            .create_task(CreateTaskInput {
                principal: principal.clone(),
                workspace: workspace.clone(),
                title,
                description,
                agent_id: request
                    .agent_id
                    .clone()
                    .or_else(|| item.agent_id.clone())
                    .unwrap_or_else(|| "personal-assistant".to_string()),
                goal_id: None,
                ui_thread_id: request
                    .ui_thread_id
                    .clone()
                    .or_else(|| item.ui_thread_id.clone())
                    .unwrap_or_else(|| "general".to_string()),
                priority: Some("normal".to_string()),
                due_date: None,
                tags: Vec::new(),
                created_by: "feed_insight".to_string(),
                depends_on: Vec::new(),
                approved: true,
                schedule: None,
                output_mode: TaskOutputMode::Accumulate,
                chat_session_id: None,
                lifecycle: magician::magician_v2::artifact_v2::models::TaskLifecycle::default(),
                sync_mode: magician::magician_v2::artifact_v2::models::TaskSyncMode::default(),
            })
            .await
        {
            Ok(task) => task,
            Err(error) => {
                if let Err(cleanup_error) = self.clear_follow_up_task_marker(&scope, &item.id) {
                    tracing::warn!(
                        insight_id = %item.id,
                        error = %cleanup_error,
                        "[FEED-API] failed to clear follow-up reservation after task creation failure"
                    );
                }
                return Ok(HttpResponse::BadRequest().json(json!({
                    "error": error.to_string()
                })));
            },
        };
        let created_event_result = self.try_append_learning_feed_event(
            &scope,
            "learning_feed_insight_follow_up_created",
            &item,
            json!({
                "task_id": task.manifest.task_id.clone(),
                "removed_feed_item": false
            }),
        );
        let marker_result =
            self.complete_follow_up_task_marker(&scope, &item, actor, &task.manifest.task_id);
        if let (Err(marker_error), Err(event_error)) = (&marker_result, &created_event_result) {
            return Ok(HttpResponse::InternalServerError().json(json!({
                "error": format!(
                    "created follow-up task but failed to persist retry marker ({marker_error}) and learning event ({event_error})"
                ),
                "task_id": task.manifest.task_id.clone(),
                "reused_task": false
            })));
        }
        if let Err(error) = marker_result {
            tracing::warn!(
                insight_id = %item.id,
                task_id = %task.manifest.task_id,
                error = %error,
                "[FEED-API] follow-up task marker write failed; learning event will preserve retry idempotency"
            );
        }
        if let Err(error) = created_event_result {
            tracing::warn!(
                insight_id = %item.id,
                task_id = %task.manifest.task_id,
                error = %error,
                "[FEED-API] follow-up learning event append failed; marker will preserve retry idempotency"
            );
        }
        if let Err(error) = self.archive_learning_insight_id(
            &scope,
            &item.id,
            request.actor.as_deref().unwrap_or(FEED_LEARNING_ACTOR),
            "Insight converted into a follow-up task.",
        ) {
            return Ok(HttpResponse::InternalServerError().json(json!({
                "error": error.to_string(),
                "task_id": task.manifest.task_id.clone(),
                "reused_task": false
            })));
        }
        let removed = self
            .remove_feed_item_with_event(&principal, &workspace, &item.id)
            .await
            .unwrap_or(false);
        self.append_learning_feed_event_best_effort(
            &scope,
            "learning_feed_insight_follow_up_archived",
            &item,
            json!({
                "task_id": task.manifest.task_id.clone(),
                "removed_feed_item": removed
            }),
        );
        Ok(HttpResponse::Ok().json(json!({
            "task": task,
            "removed_feed_item": removed,
            "reused_task": false
        })))
    }

    async fn resolve_learning_insight_item(
        &self,
        principal: &str,
        workspace: &str,
        insight_id: &str,
    ) -> std::result::Result<FeedItem, HttpResponse> {
        let item_id = learning_insight_feed_id(insight_id);
        if let Ok(Some(item)) = self
            .feed_store
            .get_item(principal, workspace, &item_id)
            .await
        {
            if item.item_type == FeedItemType::LearningInsight {
                return Ok(item);
            }
        }
        self.sync_learning_insight_feed(principal, workspace)
            .await
            .map_err(|error| {
                HttpResponse::InternalServerError().json(json!({ "error": error.to_string() }))
            })?;
        match self
            .feed_store
            .get_item(principal, workspace, &item_id)
            .await
        {
            Ok(Some(item)) if item.item_type == FeedItemType::LearningInsight => Ok(item),
            Ok(_) => Err(HttpResponse::NotFound().json(json!({
                "error": "learning_insight_not_found"
            }))),
            Err(error) => Err(HttpResponse::InternalServerError().json(json!({
                "error": error.to_string()
            }))),
        }
    }

    fn archive_learning_insight_id(
        &self,
        scope: &LearningScope,
        insight_id: &str,
        actor: &str,
        reason: &str,
    ) -> anyhow::Result<()> {
        let path = self.learning_insight_archive_path(scope, insight_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(
            path,
            serde_json::to_vec_pretty(&json!({
                "insight_id": insight_id,
                "actor": actor,
                "reason": reason,
                "archived_at": chrono::Utc::now().to_rfc3339(),
            }))?,
        )?;
        Ok(())
    }

    fn append_learning_feed_event_best_effort(
        &self,
        scope: &LearningScope,
        event_type: &str,
        item: &FeedItem,
        payload: Value,
    ) {
        if let Err(error) = self.try_append_learning_feed_event(scope, event_type, item, payload) {
            tracing::warn!(
                insight_id = %item.id,
                event_type,
                error = %error,
                "[FEED-API] failed to append learning feed action event"
            );
        }
    }

    fn try_append_learning_feed_event(
        &self,
        scope: &LearningScope,
        event_type: &str,
        item: &FeedItem,
        payload: Value,
    ) -> anyhow::Result<()> {
        let event = CreateLearningEventRequest {
            principal: None,
            workspace: None,
            event_type: event_type.to_string(),
            agent_id: item.agent_id.clone(),
            task_id: item
                .task_id
                .clone()
                .or_else(|| metadata_string(&item.metadata, "source_task_id")),
            execution_id: metadata_string(&item.metadata, "source_execution_id"),
            chat_session_id: item
                .ui_thread_id
                .clone()
                .or_else(|| metadata_string(&item.metadata, "source_chat_session_id")),
            summary: format!("{}: {}", event_type, item.title),
            evidence_refs: learning_evidence_refs_from_metadata(&item.metadata),
            payload: json!({
                "insight_id": item.id.clone(),
                "source_type": metadata_string(&item.metadata, "source_type"),
                "source_id": metadata_string(&item.metadata, "source_id"),
                "title": item.title.clone(),
                "summary": item.summary.clone(),
                "action_payload": payload
            }),
        };
        self.learning_store().append_event(scope.clone(), event)?;
        Ok(())
    }

    async fn existing_follow_up_task_for_insight(
        &self,
        scope: &LearningScope,
        insight_id: &str,
    ) -> anyhow::Result<Option<TaskRecord>> {
        if let Some(task) = self.follow_up_task_from_events(scope, insight_id).await? {
            return Ok(Some(task));
        }
        if let Some(task_id) = self.follow_up_task_marker_task_id(scope, insight_id)? {
            let Some((task_scope, task)) = self.v3_service.get_task_by_id(&task_id).await? else {
                return Err(anyhow::anyhow!(
                    "follow-up marker for insight `{insight_id}` references missing task \
                     `{task_id}`"
                ));
            };
            if task_scope.principal() == scope.principal
                && task_scope.workspace() == scope.workspace
            {
                return Ok(Some(task));
            }
            return Err(anyhow::anyhow!(
                "follow-up marker for insight `{insight_id}` references task `{task_id}` in a \
                 different scope"
            ));
        }
        Ok(None)
    }

    async fn follow_up_task_from_events(
        &self,
        scope: &LearningScope,
        insight_id: &str,
    ) -> anyhow::Result<Option<TaskRecord>> {
        let store = self.learning_store();
        for event in store.list_events(scope, usize::MAX)? {
            if event.event_type != "learning_feed_insight_follow_up_created" {
                continue;
            }
            if metadata_string(&event.payload, "insight_id").as_deref() != Some(insight_id) {
                continue;
            }
            let task_id = event
                .payload
                .get("action_payload")
                .and_then(|payload| metadata_string(payload, "task_id"))
                .or_else(|| metadata_string(&event.payload, "task_id"));
            let Some(task_id) = task_id else {
                continue;
            };
            let Some((task_scope, task)) = self.v3_service.get_task_by_id(&task_id).await? else {
                continue;
            };
            if task_scope.principal() == scope.principal
                && task_scope.workspace() == scope.workspace
            {
                return Ok(Some(task));
            }
        }
        Ok(None)
    }

    fn follow_up_task_marker_path(
        &self,
        scope: &LearningScope,
        insight_id: &str,
    ) -> std::path::PathBuf {
        self.v3_service
            .workspace()
            .learning_root(&scope.principal, &scope.workspace)
            .join("feed_insights")
            .join("actions")
            .join("follow_up_tasks")
            .join(format!("{}.json", safe_feed_id_segment(insight_id)))
    }

    fn follow_up_task_marker_task_id(
        &self,
        scope: &LearningScope,
        insight_id: &str,
    ) -> anyhow::Result<Option<String>> {
        let path = self.follow_up_task_marker_path(scope, insight_id);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) if follow_up_task_marker_is_stale(&path) => {
                remove_file_if_exists(&path)?;
                tracing::warn!(
                    insight_id,
                    error = %error,
                    "[FEED-API] cleared stale unreadable follow-up marker"
                );
                return Ok(None);
            },
            Err(error) => {
                return Err(anyhow::anyhow!("reading follow-up marker: {error}"));
            },
        };
        let marker: Value = match serde_json::from_slice(&bytes) {
            Ok(marker) => marker,
            Err(error) if follow_up_task_marker_is_stale(&path) => {
                remove_file_if_exists(&path)?;
                tracing::warn!(
                    insight_id,
                    error = %error,
                    "[FEED-API] cleared stale unparsable follow-up marker"
                );
                return Ok(None);
            },
            Err(error) => {
                return Err(anyhow::anyhow!("parsing follow-up marker: {error}"));
            },
        };
        if let Some(task_id) = metadata_string(&marker, "task_id") {
            return Ok(Some(task_id));
        }
        if follow_up_task_marker_is_stale(&path) {
            remove_file_if_exists(&path)?;
            tracing::warn!(
                insight_id,
                "[FEED-API] cleared stale reserved follow-up marker without task id"
            );
            return Ok(None);
        }
        Err(anyhow::anyhow!(
            "follow-up marker for insight `{insight_id}` exists but has no task_id; refusing to \
             create a duplicate task"
        ))
    }

    fn reserve_follow_up_task_marker(
        &self,
        scope: &LearningScope,
        item: &FeedItem,
        actor: &str,
    ) -> anyhow::Result<()> {
        let path = self.follow_up_task_marker_path(scope, &item.id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(&json!({
            "insight_id": item.id.clone(),
            "state": "reserved",
            "actor": actor,
            "title": item.title.clone(),
            "reserved_at": chrono::Utc::now().to_rfc3339(),
        }))?;
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error)
                if error.kind() == ErrorKind::AlreadyExists
                    && follow_up_task_marker_is_stale(&path) =>
            {
                remove_file_if_exists(&path)?;
                // Two racers can both watch the same stale marker vanish and
                // both reach this retry; `create_new` picks exactly one winner.
                // The loser's AlreadyExists deserves the same message as the
                // arm below, not a raw `File exists (os error 17)`.
                match OpenOptions::new().write(true).create_new(true).open(&path) {
                    Ok(file) => file,
                    Err(retry_error) if retry_error.kind() == ErrorKind::AlreadyExists => {
                        return Err(anyhow::anyhow!(
                            "follow-up marker already exists for insight `{}`; retry will \
                             reuse the recorded task when available",
                            item.id
                        ));
                    },
                    Err(retry_error) => return Err(retry_error.into()),
                }
            },
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                return Err(anyhow::anyhow!(
                    "follow-up marker already exists for insight `{}`; retry will reuse the \
                     recorded task when available",
                    item.id
                ));
            },
            Err(error) => return Err(error.into()),
        };
        // This marker is a claim: `create_new` is the atomic reservation, so it
        // cannot go through the temp+rename writer — the rename would clobber a
        // concurrent claim. What it must not do is discard the fsync result: a
        // reservation that evaporates on crash lets the follow-up task be created
        // twice. On failure the marker is left behind and the stale check above
        // reclaims it.
        file.write_all(&bytes)?;
        file.sync_all()?;
        magician::magician_v2::artifact_v2::io::sync_parent_dir_blocking(&path)?;
        Ok(())
    }

    fn complete_follow_up_task_marker(
        &self,
        scope: &LearningScope,
        item: &FeedItem,
        actor: &str,
        task_id: &str,
    ) -> anyhow::Result<()> {
        let path = self.follow_up_task_marker_path(scope, &item.id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        write_json_atomic(
            &path,
            &json!({
                "insight_id": item.id.clone(),
                "state": "created",
                "actor": actor,
                "task_id": task_id,
                "title": item.title.clone(),
                "created_at": chrono::Utc::now().to_rfc3339(),
            }),
        )?;
        Ok(())
    }

    fn clear_follow_up_task_marker(
        &self,
        scope: &LearningScope,
        insight_id: &str,
    ) -> anyhow::Result<()> {
        let path = self.follow_up_task_marker_path(scope, insight_id);
        remove_file_if_exists(&path)
    }

    /// Every row the storage index holds for one `kind` in one scope, or
    /// `None` when the index cannot answer.
    ///
    /// **The decline is the important part**, and it is
    /// `TaskApiV3::indexed_task_page`'s, not a second one invented here:
    /// no index was wired, or `is_ready()` is false, or the query failed.
    /// `is_ready()` is false from the moment a rebuild starts until it
    /// finishes, *including across a crash*, because the marker is on disk —
    /// a half-built index does not error and does not look empty, it looks
    /// like a complete index holding fewer tasks, which for an
    /// "is this task still there?" filter would silently hide live rows.
    /// Declining is always safe: the files are the source of truth and the
    /// index is a cache that may be deleted at any moment.
    fn indexed_scope_entries(&self, scope: &ScopeRef, kind: ListKind) -> Option<Vec<ListEntry>> {
        let index = self.v3_service.list_index()?;
        if !index.is_ready().unwrap_or(false) {
            return None;
        }
        match index.scope_entries(kind, &ArtifactV2Service::list_scope(scope)) {
            Ok(entries) => Some(entries),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "[LIST-INDEX] Scope membership query failed; falling back to the file walk"
                );
                None
            },
        }
    }

    /// Returns the set of task IDs for non-archived, existing tasks in the
    /// given scope. If listing fails, returns an empty set (we degrade
    /// gracefully by skipping the filter rather than failing the request).
    ///
    /// # This asks for ids, so it reads the index that holds ids
    ///
    /// The answer is a `HashSet<String>` and the walk that used to produce it
    /// read every task's three record files, built a `TaskListItemV3` per row
    /// — each cloning `manifest.description`, which averages 18KB in the
    /// author's own scope and reaches 66KB — summarised each task's primary
    /// output, scanned each one's plan, and resolved each one's dependency
    /// blocks. Then it kept the ids and dropped all of it. Two `/feed`
    /// requests land per 15s tick, each taking one of those.
    ///
    /// The index holds `(kind, principal, workspace) → id` and is reconciled
    /// by every task-record write and every delete, so its membership for a
    /// scope is the walk's membership. When it declines, the walk is still
    /// there and still correct.
    async fn valid_task_ids(&self, principal: &str, workspace: &str) -> Option<HashSet<String>> {
        let scope = ScopeRef::system_internal_unauthenticated(
            &principal.to_string(),
            &workspace.to_string(),
        );
        if let Some(entries) = self.indexed_scope_entries(&scope, ListKind::Task) {
            return Some(entries.into_iter().map(|entry| entry.id).collect());
        }
        match self.v3_service.list_tasks(&scope).await {
            Ok(tasks) => Some(tasks.into_iter().map(|t| t.id).collect()),
            Err(_) => None,
        }
    }

    /// Task IDs whose pending HITL prompts may legitimately surface in the
    /// attention `requests` / Today "Needs You" sections. A prompt is only
    /// actionable while its backing task can still receive the submission,
    /// so this includes every user-visible task PLUS internal
    /// (chat-delegated) tasks that are NOT in a terminal state. A terminal
    /// internal task can no longer accept input — submitting against it
    /// fails — so its prompt is dropped here rather than stranded as an
    /// un-resolvable card after the surfacing task is deleted. (Unlike the
    /// feed-store sections, these items live in the durable V3 attention
    /// projection table and are reconciled from task state in the background.)
    /// `None` means task listing failed; callers then skip filtering rather
    /// than wrongly dismiss live prompts on a transient error.
    async fn valid_attention_task_ids(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Option<HashSet<String>> {
        let scope = ScopeRef::system_internal_unauthenticated(
            &principal.to_string(),
            &workspace.to_string(),
        );
        let user_ids = self.valid_task_ids(principal, workspace).await?;
        let internal_ids = self.live_internal_task_ids(&scope).await?;
        let mut ids = user_ids;
        ids.extend(internal_ids);
        Some(ids)
    }

    /// Internal (chat-delegated) task ids that can still accept a submission.
    ///
    /// Split out of [`Self::valid_attention_task_ids`] so `/today`, which
    /// already holds the user-visible half from the one walk it takes, can ask
    /// for just this half instead of rebuilding both.
    ///
    /// `status` is the one column the terminal filter reads, and the index
    /// carries it, so the whole answer comes out of one SQLite statement. An
    /// index row whose `status` is stale by a write is the same risk the
    /// listing itself runs — the reconcile is on the write path — and it
    /// resolves the same way, on the next poll.
    async fn live_internal_task_ids(&self, scope: &ScopeRef) -> Option<HashSet<String>> {
        if let Some(entries) = self.indexed_scope_entries(scope, ListKind::Internal) {
            return Some(
                entries
                    .into_iter()
                    .filter(|entry| !task_status_str_is_terminal(&entry.status))
                    .map(|entry| entry.id)
                    .collect(),
            );
        }
        let internal_tasks = self.v3_service.list_internal_tasks(scope).await.ok()?;
        Some(
            internal_tasks
                .into_iter()
                .filter(|t| !task_status_str_is_terminal(&t.status))
                .map(|t| t.id)
                .collect(),
        )
    }

    /// ALL internal task ids, terminal included. The Failed-lane exclusion
    /// and the Delivered filter need exactly the terminal ones a failed or
    /// completed internal task has, so the liveness filter above would let
    /// them through. Index-first for the same one-SQLite-statement reason;
    /// the walk only runs when the index declines.
    async fn all_internal_task_ids(&self, scope: &ScopeRef) -> HashSet<String> {
        if let Some(entries) = self.indexed_scope_entries(scope, ListKind::Internal) {
            return entries.into_iter().map(|entry| entry.id).collect();
        }
        self.v3_service
            .list_internal_tasks(scope)
            .await
            .map(|tasks| tasks.into_iter().map(|task| task.id).collect())
            .unwrap_or_default()
    }

    async fn list_v3_attention_items(
        &self,
        principal: &str,
        workspace: &str,
        ui_thread_id: Option<&str>,
    ) -> anyhow::Result<Vec<FeedItem>> {
        self.v3_service
            .list_attention_items(
                &ScopeRef::system_internal_unauthenticated(
                    &principal.to_string(),
                    &workspace.to_string(),
                ),
                ui_thread_id,
            )
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))
    }

    fn learning_store(&self) -> LearningStore {
        LearningStore::new(self.v3_service.workspace().clone())
    }

    /// Throttled + fire-and-forget wrapper around [`sync_learning_feed`].
    ///
    /// Two layers of insulation between HTTP handlers and the actual
    /// sync work:
    ///
    /// 1. **Throttle** — skip when the per-scope window
    ///    ([`LEARNING_SYNC_THROTTLE`], 15min) hasn't elapsed since the last RUN
    ///    START. We mark the timestamp at the *beginning* of the run (not the
    ///    end) so concurrent requests during a slow sync don't all queue up
    ///    another one behind it.
    ///
    /// 2. **`tokio::spawn`** — the sync itself runs in a detached background
    ///    task; the HTTP handler returns immediately. The first request after
    ///    deploy used to block ~600ms while 125 historical entries projected;
    ///    now it returns instantly and the cards land in the feed
    ///    asynchronously (operator sees them on the next Today refresh, ~5-10s
    ///    later). On sync failure the timestamp is rolled back so the next call
    ///    retries instead of waiting another 15min with stale data.
    ///
    /// This is the only entry point HTTP handlers should call — the
    /// unthrottled `sync_learning_feed` exists for tests + explicit
    /// cache invalidation paths that need an immediate sync.
    fn sync_learning_feed_best_effort(&self, principal: &str, workspace: &str) {
        let key = (principal.to_string(), workspace.to_string());
        {
            let mut map = self
                .last_learning_sync_at
                .lock()
                .expect("learning-sync throttle mutex poisoned");
            if let Some(last) = map.get(&key) {
                if last.elapsed() < LEARNING_SYNC_THROTTLE {
                    // Within the throttle window — feed DB is at most
                    // 15min stale; skip the expensive walk.
                    return;
                }
            }
            // Opportunistic eviction so the map can't grow monotonically
            // as one-off scopes accumulate. Anything older than 2× the
            // throttle window can't be load-bearing (its next request
            // would re-insert anyway); dropping it bounds the map to
            // recently-active scopes.
            let stale_after = LEARNING_SYNC_THROTTLE * 2;
            map.retain(|_, instant| instant.elapsed() < stale_after);
            // Mark BEFORE spawn so other handlers don't queue a duplicate
            // sync while this one is in flight. The spawned task will
            // roll back on failure.
            map.insert(key.clone(), Instant::now());
        }
        let me = self.clone();
        tokio::spawn(async move {
            if let Err(error) = me.sync_learning_feed(&key.0, &key.1).await {
                tracing::warn!(
                    principal = %key.0,
                    workspace = %key.1,
                    error = %error,
                    "[FEED-API] background sync_learning_feed failed; rolling back throttle so next request retries"
                );
                let mut map = me
                    .last_learning_sync_at
                    .lock()
                    .expect("learning-sync throttle mutex poisoned");
                map.remove(&key);
            }
        });
    }

    async fn sync_learning_feed(&self, principal: &str, workspace: &str) -> anyhow::Result<()> {
        // Each sub-sync runs INDEPENDENTLY. Pre-v0.6.581 the legacy
        // sub-syncs used `?` to short-circuit, which meant the
        // user-facing agent-learnings projector (the most important
        // surface — it feeds Today) silently never ran whenever
        // `sync_learning_candidate_feed` or `sync_learning_insight_feed`
        // errored. Those legacy paths hit DuckDB corruption and IO
        // errors regularly (see v0.6.579), so the user-facing surface
        // was perpetually starved on misbehaving scopes.
        //
        // Now: log + continue per sub-sync, projector always runs.
        // Surface a combined error to the caller only when ALL sub-syncs
        // failed (rare; usually scope-level filesystem issue).
        let mut failures: Vec<String> = Vec::new();
        match reconcile_skill_evolution_attention_projection(
            &self.feed_store,
            self.learning_store(),
            LearningScope::new(principal.to_string(), workspace.to_string()),
        )
        .await
        {
            Ok(sync) => {
                for (previous, item) in sync.deltas {
                    self.emit_feed_delta(previous, item);
                }
                for item in sync.removed {
                    self.emit_feed_item_removed(&item);
                }
            },
            Err(error) => {
                tracing::warn!(
                    principal = %principal,
                    workspace = %workspace,
                    error = %error,
                    "[FEED-API] Skill Evolution attention projection failed; continuing with remaining sub-syncs"
                );
                failures.push(format!("skill_evolution_attention: {error}"));
            },
        }
        if let Err(error) = self
            .sync_learning_candidate_feed(principal, workspace)
            .await
        {
            tracing::warn!(
                principal = %principal,
                workspace = %workspace,
                error = %error,
                "[FEED-API] sync_learning_candidate_feed failed; continuing with remaining sub-syncs"
            );
            failures.push(format!("candidate_feed: {error}"));
        }
        if let Err(error) = self.sync_learning_insight_feed(principal, workspace).await {
            tracing::warn!(
                principal = %principal,
                workspace = %workspace,
                error = %error,
                "[FEED-API] sync_learning_insight_feed failed; continuing with remaining sub-syncs"
            );
            failures.push(format!("insight_feed: {error}"));
        }
        // Project the agent's user-facing accumulated knowledge
        // (research findings, contacts, routines, preferences,
        // workflows, skills) into `FeedItemType::AgentLearning`
        // cards. Today Activity filters to AgentLearning by default,
        // separating substantive learnings from the internal
        // telemetry that the `LearningInsight` / `LearningCandidate`
        // projections produce. See `agent_learnings_projection.rs`.
        // Wire the broadcaster when available so the projector can
        // emit `FeedItemCreated` / `FeedItemUpdated` / `FeedItemRemoved`
        // events alongside its DuckDB writes — connected Today
        // frontends then patch the visible card list without waiting
        // for the next HTTP poll. `None` when the orchestrator hasn't
        // installed a broadcaster (e.g., CLI commands).
        let broadcaster = self.v3_service.runtime_event_broadcaster();
        let mut projector =
            magician::magician_v2::feed::agent_learnings_projection::AgentLearningsProjector::new(
                self.v3_service.workspace(),
                &self.feed_store,
            );
        if let Some(broadcaster_ref) = broadcaster.as_ref() {
            projector = projector.with_broadcaster(broadcaster_ref);
        }
        match projector.sync(principal, workspace).await {
            Ok(summary) => {
                if summary.new_card_count > 0 || summary.removed_card_count > 0 {
                    tracing::debug!(
                        target: "feed::agent_learnings_projection",
                        principal,
                        workspace,
                        new_cards = summary.new_card_count,
                        removed_cards = summary.removed_card_count,
                        per_tier = ?summary.per_tier_projected,
                        "agent learnings projection emitted/removed cards"
                    );
                }
            },
            Err(error) => {
                tracing::warn!(
                    target: "feed::agent_learnings_projection",
                    principal,
                    workspace,
                    error = %error,
                    "agent learnings projection failed"
                );
                failures.push(format!("agent_learnings: {error}"));
            },
        }
        if failures.len() == 4 {
            anyhow::bail!("all learning sub-syncs failed: {}", failures.join("; "));
        }
        Ok(())
    }

    async fn sync_learning_candidate_feed(
        &self,
        principal: &str,
        workspace: &str,
    ) -> anyhow::Result<()> {
        let store = self.learning_store();
        let scope = LearningScope::new(principal.to_string(), workspace.to_string());
        let mut visible_ids = HashSet::new();
        for candidate_type in ["memory_fact", "memory_preference"] {
            let candidates = store.list_candidates(
                &scope,
                LearningCandidateFilters {
                    candidate_type: Some(candidate_type.to_string()),
                    limit: None,
                    ..Default::default()
                },
            )?;
            for candidate in candidates {
                let Some(item) = learning_candidate_feed_item(&candidate) else {
                    continue;
                };
                visible_ids.insert(item.id.clone());
                let previous = self.feed_store.upsert_item(item.clone()).await?;
                self.emit_feed_delta(previous, item);
            }
        }

        // Page through existing learning-candidate feed rows so cleanup is
        // exhaustive even when many rows share one millisecond.
        const SYNC_PAGE_SIZE: usize = 200;
        let mut cursor = None;
        loop {
            let page = self
                .feed_store
                .list_items_page(
                    FeedQuery {
                        principal: principal.to_string(),
                        workspace: workspace.to_string(),
                        item_type: Some(FeedItemType::LearningCandidate),
                        limit: SYNC_PAGE_SIZE,
                        ..Default::default()
                    },
                    cursor,
                )
                .await?;
            if page.items.is_empty() {
                break;
            }
            let has_more = page.has_more;
            cursor = page.next_cursor;
            for item in page.items {
                if !visible_ids.contains(&item.id) {
                    self.remove_feed_item_with_event(principal, workspace, &item.id)
                        .await?;
                }
            }
            if !has_more {
                break;
            }
        }
        Ok(())
    }

    async fn sync_learning_insight_feed(
        &self,
        principal: &str,
        workspace: &str,
    ) -> anyhow::Result<()> {
        let store = self.learning_store();
        let scope = LearningScope::new(principal.to_string(), workspace.to_string());
        let mut visible_candidate_ids = HashSet::new();
        let mut projected_count = 0usize;
        let mut created_count = 0usize;
        let mut updated_count = 0usize;
        let mut archived_skipped_count = 0usize;

        for candidate in store.list_candidates(
            &scope,
            LearningCandidateFilters {
                limit: None,
                ..Default::default()
            },
        )? {
            let Some(item) = learning_candidate_insight_feed_item(&candidate) else {
                continue;
            };
            if self.learning_insight_is_archived(&scope, &item.id) {
                archived_skipped_count += 1;
                continue;
            }
            visible_candidate_ids.insert(item.id.clone());
            let previous = self.feed_store.upsert_item(item.clone()).await?;
            update_projection_counts(
                &mut projected_count,
                &mut created_count,
                &mut updated_count,
                &previous,
                &item,
            );
            self.emit_feed_delta(previous, item);
        }

        for event in store.list_events(&scope, 500)? {
            let Some(item) = learning_event_insight_feed_item(&event) else {
                continue;
            };
            if self.learning_insight_is_archived(&scope, &item.id) {
                archived_skipped_count += 1;
                continue;
            }
            let previous = self.feed_store.upsert_item(item.clone()).await?;
            update_projection_counts(
                &mut projected_count,
                &mut created_count,
                &mut updated_count,
                &previous,
                &item,
            );
            self.emit_feed_delta(previous, item);
        }

        for report in store.list_evaluation_run_reports(
            &scope,
            LearningEvaluationRunFilters {
                limit: Some(100),
                ..Default::default()
            },
        )? {
            let item = evaluation_run_insight_feed_item(&report);
            if self.learning_insight_is_archived(&scope, &item.id) {
                archived_skipped_count += 1;
                continue;
            }
            let previous = self.feed_store.upsert_item(item.clone()).await?;
            update_projection_counts(
                &mut projected_count,
                &mut created_count,
                &mut updated_count,
                &previous,
                &item,
            );
            self.emit_feed_delta(previous, item);
        }

        for report in store.list_growth_evaluation_run_reports(
            &scope,
            LearningGrowthEvaluationRunFilters {
                limit: Some(50),
                ..Default::default()
            },
        )? {
            let item = growth_evaluation_insight_feed_item(&report);
            if self.learning_insight_is_archived(&scope, &item.id) {
                archived_skipped_count += 1;
                continue;
            }
            let previous = self.feed_store.upsert_item(item.clone()).await?;
            update_projection_counts(
                &mut projected_count,
                &mut created_count,
                &mut updated_count,
                &previous,
                &item,
            );
            self.emit_feed_delta(previous, item);
        }

        for backlog_item in store.list_evaluation_backlog_items(
            &scope,
            LearningEvaluationBacklogFilters {
                limit: Some(100),
                ..Default::default()
            },
        )? {
            let item = evaluation_backlog_insight_feed_item(&backlog_item);
            if self.learning_insight_is_archived(&scope, &item.id) {
                archived_skipped_count += 1;
                continue;
            }
            let previous = self.feed_store.upsert_item(item.clone()).await?;
            update_projection_counts(
                &mut projected_count,
                &mut created_count,
                &mut updated_count,
                &previous,
                &item,
            );
            self.emit_feed_delta(previous, item);
        }

        let removed_stale_count = self
            .cleanup_stale_candidate_insights(principal, workspace, &visible_candidate_ids)
            .await?;
        tracing::debug!(
            principal = %principal,
            workspace = %workspace,
            projected_count,
            created_count,
            updated_count,
            archived_skipped_count,
            removed_stale_count,
            "[FEED-API] synced learning insight feed projection"
        );
        Ok(())
    }

    async fn cleanup_stale_candidate_insights(
        &self,
        principal: &str,
        workspace: &str,
        visible_candidate_ids: &HashSet<String>,
    ) -> anyhow::Result<usize> {
        const SYNC_PAGE_SIZE: usize = 200;
        let mut cursor = None;
        let mut removed_count = 0usize;
        loop {
            let page = self
                .feed_store
                .list_items_page(
                    FeedQuery {
                        principal: principal.to_string(),
                        workspace: workspace.to_string(),
                        item_type: Some(FeedItemType::LearningInsight),
                        limit: SYNC_PAGE_SIZE,
                        ..Default::default()
                    },
                    cursor,
                )
                .await?;
            if page.items.is_empty() {
                break;
            }
            let has_more = page.has_more;
            cursor = page.next_cursor;
            for item in page.items {
                let source_type = metadata_string(&item.metadata, "source_type");
                if (self.learning_insight_is_archived(
                    &LearningScope::new(principal.to_string(), workspace.to_string()),
                    &item.id,
                ) || (source_type.as_deref() == Some("learning_candidate")
                    && !visible_candidate_ids.contains(&item.id)))
                    && self
                        .remove_feed_item_with_event(principal, workspace, &item.id)
                        .await?
                {
                    removed_count += 1;
                }
            }
            if !has_more {
                break;
            }
        }
        Ok(removed_count)
    }

    fn learning_insight_is_archived(&self, scope: &LearningScope, insight_id: &str) -> bool {
        self.learning_insight_archive_path(scope, insight_id)
            .exists()
    }

    fn learning_insight_archive_path(
        &self,
        scope: &LearningScope,
        insight_id: &str,
    ) -> std::path::PathBuf {
        self.v3_service
            .workspace()
            .learning_root(&scope.principal, &scope.workspace)
            .join("feed_insights")
            .join("archived")
            .join(format!("{}.json", safe_feed_id_segment(insight_id)))
    }

    /// Remove one feed row and tell the connected clients it is gone.
    ///
    /// This is the single point at which a learning action's row leaves the
    /// feed — `confirm`, `edit + confirm`, `archive candidate`, `archive
    /// insight`, `save to memory` and `create follow-up task` all end here,
    /// as do the two learning syncs that reconcile the projections. So this
    /// is where the Today projection is dropped, rather than at each of those
    /// call sites: a row removed here is a row a cached projection would
    /// otherwise hand back for the rest of the window, and the reader's own
    /// refetch is the request that would receive it.
    ///
    /// Only when a row actually went. A removal that found nothing changed
    /// nothing the projection reads, and every caller that also writes
    /// elsewhere — a task, a learning-store transition — is covered by the
    /// invalidation on *that* write.
    async fn remove_feed_item_with_event(
        &self,
        principal: &str,
        workspace: &str,
        item_id: &str,
    ) -> anyhow::Result<bool> {
        let existing = self
            .feed_store
            .get_item(principal, workspace, item_id)
            .await?;
        let removed = self
            .feed_store
            .remove_item(principal, workspace, item_id)
            .await?;
        if removed {
            if let Some(item) = existing.as_ref() {
                self.emit_feed_item_removed(item);
            }
            self.invalidate_today_projection(principal, workspace);
        }
        Ok(removed)
    }

    fn emit_feed_item_removed(&self, item: &FeedItem) {
        if let Some(broadcaster) = self.v3_service.runtime_event_broadcaster() {
            broadcaster.emit_transport_only(RuntimeTransportEvent::FeedItemRemoved {
                principal: item.principal.clone(),
                workspace: item.workspace.clone(),
                id: item.id.clone(),
                task_id: item.task_id.clone(),
                ui_thread_id: item.ui_thread_id.clone(),
                execution_id: metadata_string(&item.metadata, "execution_id"),
                timestamp: chrono::Utc::now().timestamp_millis(),
            });
        }
    }

    fn emit_feed_delta(&self, previous: Option<FeedItem>, item: FeedItem) {
        let Some(broadcaster) = self.v3_service.runtime_event_broadcaster() else {
            return;
        };
        match previous {
            None => {
                broadcaster.emit_transport_only(RuntimeTransportEvent::FeedItemCreated {
                    item,
                    timestamp: chrono::Utc::now().timestamp_millis(),
                });
            },
            Some(previous) => {
                let patch = FeedItemPatch::between(&previous, &item);
                if patch.is_empty() {
                    return;
                }
                broadcaster.emit_transport_only(RuntimeTransportEvent::FeedItemUpdated {
                    principal: item.principal.clone(),
                    workspace: item.workspace.clone(),
                    id: item.id.clone(),
                    task_id: item.task_id.clone(),
                    ui_thread_id: item.ui_thread_id.clone(),
                    execution_id: metadata_string(&item.metadata, "execution_id"),
                    patch,
                    timestamp: chrono::Utc::now().timestamp_millis(),
                });
            },
        }
    }
}

struct SkillEvolutionAttentionProjectionSync {
    deltas: Vec<(Option<FeedItem>, FeedItem)>,
    removed: Vec<FeedItem>,
}

async fn reconcile_skill_evolution_attention_projection(
    feed_store: &FeedStore,
    learning_store: LearningStore,
    scope: LearningScope,
) -> anyhow::Result<SkillEvolutionAttentionProjectionSync> {
    let load_store = learning_store.clone();
    let load_scope = scope.clone();
    let items = tokio::task::spawn_blocking(move || {
        load_skill_evolution_attention_feed_items(&load_store, &load_scope)
    })
    .await
    .map_err(|error| {
        anyhow::anyhow!("Skill Evolution attention projection task panicked: {error}")
    })??;
    let active_ids = items
        .iter()
        .map(|item| item.id.clone())
        .collect::<HashSet<_>>();
    let mut deltas = Vec::with_capacity(items.len());
    for item in items {
        let previous = feed_store.upsert_item(item.clone()).await?;
        deltas.push((previous, item));
    }

    let mut removed = Vec::new();
    for item in feed_store
        .list_items_by_id_prefixes(
            &scope.principal,
            &scope.workspace,
            &[
                SKILL_EVOLUTION_APPROVAL_ATTENTION_PREFIX,
                ROLLBACK_RECOMMENDATION_ATTENTION_PREFIX,
                POST_PROMOTION_MONITOR_ATTENTION_PREFIX,
            ],
        )
        .await?
    {
        if !active_ids.contains(&item.id)
            && feed_store
                .remove_item(&scope.principal, &scope.workspace, &item.id)
                .await?
        {
            removed.push(item);
        }
    }
    Ok(SkillEvolutionAttentionProjectionSync { deltas, removed })
}

fn load_skill_evolution_attention_feed_items(
    store: &LearningStore,
    scope: &LearningScope,
) -> anyhow::Result<Vec<FeedItem>> {
    let mut items = skill_evolution_gate_approval_feed_items(
        store.list_capability_evolution_proposals(
            scope,
            LearningCapabilityEvolutionProposalFilters {
                limit: None,
                ..Default::default()
            },
        )?,
        store.list_capability_evolution_validation_reports(
            scope,
            LearningCapabilityEvolutionValidationFilters {
                limit: None,
                ..Default::default()
            },
        )?,
        store.list_capability_evolution_implementation_records(
            scope,
            LearningCapabilityEvolutionImplementationFilters {
                limit: None,
                ..Default::default()
            },
        )?,
        store.list_capability_evolution_application_records(
            scope,
            LearningCapabilityEvolutionApplicationFilters {
                limit: None,
                ..Default::default()
            },
        )?,
        store.list_capability_evolution_promotion_records(
            scope,
            LearningCapabilityEvolutionPromotionFilters {
                limit: None,
                ..Default::default()
            },
        )?,
    );
    items.extend(rollback_recommendation_attention_feed_items(
        store.list_capability_evolution_rollback_recommendation_records(
            scope,
            LearningCapabilityEvolutionRollbackRecommendationFilters {
                status: Some(
                    LearningCapabilityEvolutionRollbackRecommendationStatus::Recommended
                        .as_str()
                        .to_string(),
                ),
                limit: None,
                ..Default::default()
            },
        )?,
    ));
    let monitors = store.list_capability_evolution_post_promotion_monitor_records(
        scope,
        LearningCapabilityEvolutionPostPromotionMonitorFilters {
            status: Some(
                LearningCapabilityEvolutionPostPromotionMonitorStatus::RegressionDetected
                    .as_str()
                    .to_string(),
            ),
            limit: None,
            ..Default::default()
        },
    )?;
    items.extend(post_promotion_monitor_attention_feed_items(
        store, scope, monitors,
    ));
    sort_attention_feed_items(&mut items);
    Ok(items)
}

fn learning_candidate_feed_item(candidate: &LearningCandidate) -> Option<FeedItem> {
    if candidate.state.is_terminal() || !candidate.candidate_type.is_memory_candidate() {
        return None;
    }
    if !matches!(
        candidate.state,
        LearningCandidateState::Observed
            | LearningCandidateState::Proposed
            | LearningCandidateState::Triaged
            | LearningCandidateState::Approved
            | LearningCandidateState::Evaluated
    ) {
        return None;
    }
    let projection = learning_memory_feed_projection(candidate)?;
    if projection.target_scope != "user" {
        return None;
    }
    if !magician::magician_v2::chat::service::is_curated_user_memory_tier(&projection.target_tier) {
        return None;
    }
    let feed_id = learning_candidate_feed_id(&candidate.id);
    let updated_at = candidate.updated_at.timestamp_millis();
    let created_at = candidate.created_at.timestamp_millis();
    let value_summary = summarize_learning_memory_value(&projection.memory_value);
    Some(FeedItem {
        id: feed_id.clone(),
        principal: candidate.scope.principal.to_string(),
        workspace: candidate.scope.workspace.to_string(),
        item_type: FeedItemType::LearningCandidate,
        task_id: None,
        ui_thread_id: None,
        agent_id: candidate.source_agent_id.clone(),
        title: if candidate.title.trim().is_empty() {
            "Review memory learning".to_string()
        } else {
            candidate.title.clone()
        },
        summary: Some(if value_summary.is_empty() {
            candidate.summary.clone()
        } else {
            value_summary.clone()
        }),
        status: FeedItemStatus::NeedsAction,
        created_at,
        updated_at,
        actions: vec![
            FeedAction {
                id: "confirm".to_string(),
                label: "Confirm".to_string(),
                action_type: Some("confirm_learning_candidate".to_string()),
                payload: json!({ "candidate_id": candidate.id.clone() }),
            },
            FeedAction {
                id: "edit_confirm".to_string(),
                label: "Edit + file".to_string(),
                action_type: Some("edit_confirm_learning_candidate".to_string()),
                payload: json!({ "candidate_id": candidate.id.clone() }),
            },
            FeedAction {
                id: "archive".to_string(),
                label: "Archive".to_string(),
                action_type: Some("archive_learning_candidate".to_string()),
                payload: json!({ "candidate_id": candidate.id.clone() }),
            },
        ],
        metadata: json!({
            "candidate_id": candidate.id.clone(),
            "dedupe_key": feed_id.clone(),
            "novelty": learning_candidate_novelty(candidate),
            "related_candidate_id": candidate.id.clone(),
            "candidate_type": candidate.candidate_type.as_str(),
            "candidate_state": candidate.state.as_str(),
            "proposed_target": candidate.proposed_target.clone(),
            "promotion_target": candidate.promotion_target.clone(),
            "confidence": candidate.confidence,
            "risk_level": candidate.risk_level.as_str(),
            "review_required": candidate.review_required,
            "review_reason": candidate.review_reason.clone(),
            "learning_kind": "memory",
            "target_scope": projection.target_scope,
            "target_tier": projection.target_tier,
            "memory_key": projection.memory_key,
            "memory_value_label": value_summary,
            "memory_value": projection.memory_value,
            "source_task_id": candidate.source_task_id.clone(),
            "source_execution_id": candidate.source_execution_id.clone(),
            "source_chat_session_id": candidate.source_chat_session_id.clone(),
            "source_artifact_ids": source_artifact_ids_from_evidence_refs(&candidate.evidence_refs),
            "evidence_refs": candidate.evidence_refs.clone(),
        }),
    })
}

fn public_contact_identity_research_revised_value(
    candidate: &LearningCandidate,
    revised_value: &str,
    actor: &str,
) -> Option<(Value, String)> {
    if candidate
        .proposed_change
        .get("public_contact_identity_research")
        .is_none()
    {
        return None;
    }
    let memory = candidate.proposed_change.get("memory")?.as_object()?;
    let value = memory.get("value")?.as_object()?;
    if value
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind != "public_contact_identity_research")
    {
        return None;
    }

    let reviewed_identity = revised_value.trim();
    if reviewed_identity.is_empty() {
        return None;
    }
    let mut revised = value.clone();
    revised.insert(
        "possible_identity".to_string(),
        Value::String(reviewed_identity.to_string()),
    );
    revised.insert(
        "owner_reviewed_identity".to_string(),
        Value::String(reviewed_identity.to_string()),
    );
    revised.insert("owner_reviewed".to_string(), Value::Bool(true));
    revised.insert(
        "owner_reviewed_by".to_string(),
        Value::String(actor.to_string()),
    );
    revised.insert(
        "owner_reviewed_at".to_string(),
        Value::String(chrono::Utc::now().to_rfc3339()),
    );
    revised.insert(
        "confidence".to_string(),
        Value::String("owner_reviewed".to_string()),
    );

    Some((
        Value::Object(revised),
        format!("Owner-reviewed public contact identity: {reviewed_identity}"),
    ))
}

fn learning_candidate_feed_id(candidate_id: &str) -> String {
    format!("{LEARNING_CANDIDATE_FEED_PREFIX}{candidate_id}")
}

fn skill_evolution_gate_approval_feed_items(
    proposals: Vec<LearningCapabilityEvolutionProposal>,
    validations: Vec<LearningCapabilityEvolutionValidationReport>,
    implementations: Vec<LearningCapabilityEvolutionImplementationRecord>,
    applications: Vec<LearningCapabilityEvolutionApplicationRecord>,
    promotions: Vec<LearningCapabilityEvolutionPromotionRecord>,
) -> Vec<FeedItem> {
    let mut passed_validations = HashMap::new();
    for validation in validations {
        if validation.status != LearningCapabilityEvolutionValidationStatus::Passed {
            continue;
        }
        let replace = passed_validations
            .get(&validation.candidate_id)
            .map(|existing: &LearningCapabilityEvolutionValidationReport| {
                validation.created_at > existing.created_at
            })
            .unwrap_or(true);
        if replace {
            passed_validations.insert(validation.candidate_id.clone(), validation);
        }
    }
    let mut latest_implementations = HashMap::new();
    for implementation in implementations {
        let replace = latest_implementations
            .get(&implementation.candidate_id)
            .map(
                |existing: &LearningCapabilityEvolutionImplementationRecord| {
                    implementation.created_at > existing.created_at
                },
            )
            .unwrap_or(true);
        if replace {
            latest_implementations.insert(implementation.candidate_id.clone(), implementation);
        }
    }
    let mut applied_applications = HashMap::new();
    let mut latest_applications = HashMap::new();
    for application in applications {
        let replace_latest = latest_applications
            .get(&application.candidate_id)
            .map(|existing: &LearningCapabilityEvolutionApplicationRecord| {
                application.created_at > existing.created_at
            })
            .unwrap_or(true);
        if replace_latest {
            latest_applications.insert(application.candidate_id.clone(), application.clone());
        }
        if application.status != LearningCapabilityEvolutionApplicationStatus::Applied {
            continue;
        }
        let replace = applied_applications
            .get(&application.candidate_id)
            .map(|existing: &LearningCapabilityEvolutionApplicationRecord| {
                application.created_at > existing.created_at
            })
            .unwrap_or(true);
        if replace {
            applied_applications.insert(application.candidate_id.clone(), application);
        }
    }
    let mut latest_promotions = HashMap::new();
    for promotion in promotions {
        let replace = latest_promotions
            .get(&promotion.candidate_id)
            .map(|existing: &LearningCapabilityEvolutionPromotionRecord| {
                promotion.created_at > existing.created_at
            })
            .unwrap_or(true);
        if replace {
            latest_promotions.insert(promotion.candidate_id.clone(), promotion);
        }
    }

    let mut items = Vec::new();
    for proposal in proposals {
        match proposal.status {
            LearningCapabilityEvolutionProposalStatus::ReadyForReview => {
                items.push(skill_evolution_gate_approval_feed_item(
                    &proposal,
                    SkillEvolutionApprovalGate::ProposalReview,
                    "Approve Skill Evolution proposal",
                    proposal.summary.clone(),
                    "Review proposal",
                    None,
                    None,
                    None,
                ));
            },
            LearningCapabilityEvolutionProposalStatus::Approved => {
                let validation = passed_validations.get(&proposal.candidate_id);
                let implementation = latest_implementations.get(&proposal.candidate_id);
                let applied_application = applied_applications.get(&proposal.candidate_id);
                let latest_application = latest_applications.get(&proposal.candidate_id);
                let promotion = latest_promotions.get(&proposal.candidate_id);
                if let (Some(validation), Some(implementation)) = (validation, implementation) {
                    let unapplied_patch_bundle =
                        applied_application.is_none() && !implementation.patches.is_empty();
                    if unapplied_patch_bundle {
                        let prepared_application = latest_application.filter(|application| {
                            application.status
                                == LearningCapabilityEvolutionApplicationStatus::Prepared
                        });
                        items.push(skill_evolution_gate_approval_feed_item(
                            &proposal,
                            SkillEvolutionApprovalGate::Apply,
                            "Approve Skill Evolution apply gate",
                            prepared_application
                                .map(|application| {
                                    format!(
                                        "Prepared application `{}` is ready for operator-approved \
                                         apply.",
                                        application.id
                                    )
                                })
                                .unwrap_or_else(|| {
                                    format!(
                                        "{} reviewed patch(es) ready for operator-approved \
                                         dry-run.",
                                        implementation.patches.len()
                                    )
                                }),
                            "Review apply",
                            Some(validation.id.clone()),
                            Some(implementation.id.clone()),
                            prepared_application.map(|application| application.id.clone()),
                        ));
                    }
                    if !unapplied_patch_bundle
                        && promotion.is_none()
                        && (!proposal_targets_scoped_skill(&proposal)
                            || applied_application.is_some())
                    {
                        items.push(skill_evolution_gate_approval_feed_item(
                            &proposal,
                            SkillEvolutionApprovalGate::Promotion,
                            "Approve Skill Evolution promotion",
                            "Validation and implementation evidence are ready for promotion \
                             review."
                                .to_string(),
                            "Review promotion",
                            Some(validation.id.clone()),
                            Some(implementation.id.clone()),
                            applied_application.map(|application| application.id.clone()),
                        ));
                    }
                }
            },
            _ => {},
        }
    }
    items.sort_by(|left, right| {
        left.metadata
            .get("skill_evolution_gate_priority")
            .and_then(Value::as_u64)
            .cmp(
                &right
                    .metadata
                    .get("skill_evolution_gate_priority")
                    .and_then(Value::as_u64),
            )
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| right.id.cmp(&left.id))
    });
    items
}

#[derive(Debug, Clone, Copy)]
enum SkillEvolutionApprovalGate {
    ProposalReview,
    Apply,
    Promotion,
}

impl SkillEvolutionApprovalGate {
    fn as_str(self) -> &'static str {
        match self {
            Self::ProposalReview => "proposal_review",
            Self::Apply => "apply_gate",
            Self::Promotion => "promotion_gate",
        }
    }

    fn attention_kind(self) -> &'static str {
        match self {
            Self::ProposalReview => "skill_evolution.proposal_review",
            Self::Apply => "skill_evolution.apply_gate",
            Self::Promotion => "skill_evolution.promotion_gate",
        }
    }

    fn priority(self) -> u64 {
        match self {
            Self::ProposalReview => 10,
            Self::Apply => 20,
            Self::Promotion => 30,
        }
    }
}

fn skill_evolution_gate_approval_feed_item(
    proposal: &LearningCapabilityEvolutionProposal,
    gate: SkillEvolutionApprovalGate,
    title: &str,
    summary: String,
    review_label: &str,
    validation_id: Option<String>,
    implementation_id: Option<String>,
    application_id: Option<String>,
) -> FeedItem {
    let feed_id = format!(
        "{SKILL_EVOLUTION_APPROVAL_ATTENTION_PREFIX}{}:{}",
        gate.as_str(),
        safe_feed_id_segment(&proposal.candidate_id)
    );
    let review_href = format!(
        "/skills/evolution?candidate_id={}&skill_evolution_gate={}",
        proposal.candidate_id,
        gate.as_str()
    );
    let target_surface = proposal_target_surface(proposal);
    let direct_action = match gate {
        SkillEvolutionApprovalGate::ProposalReview => "approve",
        SkillEvolutionApprovalGate::Apply if application_id.is_some() => "apply",
        SkillEvolutionApprovalGate::Apply => "dry_run",
        SkillEvolutionApprovalGate::Promotion => "promote",
    };
    let direct_action_enabled =
        matches!(gate, SkillEvolutionApprovalGate::ProposalReview) || target_surface.is_some();
    let updated_at = proposal.updated_at.timestamp_millis();
    let created_at = proposal.created_at.timestamp_millis();
    FeedItem {
        id: feed_id.clone(),
        principal: proposal.scope.principal.to_string(),
        workspace: proposal.scope.workspace.to_string(),
        item_type: FeedItemType::Approval,
        task_id: None,
        ui_thread_id: None,
        agent_id: None,
        title: title.to_string(),
        summary: Some(summary),
        status: FeedItemStatus::NeedsAction,
        created_at,
        updated_at,
        actions: vec![FeedAction {
            id: "review".to_string(),
            label: review_label.to_string(),
            action_type: Some("open_skill_evolution_gate".to_string()),
            payload: json!({
                "href": review_href,
                "candidate_id": proposal.candidate_id.clone(),
                "proposal_id": proposal.id.clone(),
                "gate": gate.as_str(),
                "direct_action": direct_action,
                "direct_action_enabled": direct_action_enabled,
                "target_surface": target_surface,
                "validation_id": validation_id.clone(),
                "implementation_id": implementation_id.clone(),
                "application_id": application_id.clone()
            }),
        }],
        metadata: json!({
            "attention_kind": gate.attention_kind(),
            "attention_target": "skill_evolution_gate",
            "source": "approval",
            "dedupe_key": feed_id.clone(),
            "route": review_href,
            "review_href": review_href,
            "review_label": review_label,
            "candidate_id": proposal.candidate_id.clone(),
            "proposal_id": proposal.id.clone(),
            "proposal_status": proposal.status.as_str(),
            "capability_id": proposal.capability_id.clone(),
            "proposed_fix_type": proposal.proposed_fix_type.clone(),
            "proposed_files": proposal.proposed_files.clone(),
            "validation_id": validation_id,
            "implementation_id": implementation_id,
            "application_id": application_id,
            "target_surface": target_surface,
            "skill_evolution_gate": gate.as_str(),
            "skill_evolution_gate_action": direct_action,
            "skill_evolution_gate_action_enabled": direct_action_enabled,
            "skill_evolution_gate_priority": gate.priority(),
            "source_type": "learning_capability_evolution_proposal",
            "source_id": proposal.id.clone(),
            "why_it_matters": "Skill Evolution requires an operator decision before this reviewed change can advance.",
        }),
    }
}

fn proposal_target_surface(proposal: &LearningCapabilityEvolutionProposal) -> Option<&'static str> {
    let mut has_scoped_skill = false;
    let mut has_source_skill = false;
    for path in proposal
        .proposed_files
        .iter()
        .chain(proposal.patches.iter().map(|patch| &patch.path))
    {
        if path.starts_with("skills/") {
            has_scoped_skill = true;
        } else if path.starts_with("skillshub/") {
            has_source_skill = true;
        }
    }
    match (has_scoped_skill, has_source_skill) {
        (true, true) => None,
        (false, true) => Some("source_skill"),
        _ => Some("scoped_skill"),
    }
}

fn proposal_targets_scoped_skill(proposal: &LearningCapabilityEvolutionProposal) -> bool {
    proposal
        .proposed_files
        .iter()
        .chain(proposal.patches.iter().map(|patch| &patch.path))
        .any(|path| path.starts_with("skills/"))
}

fn rollback_recommendation_attention_feed_item(
    recommendation: &LearningCapabilityEvolutionRollbackRecommendationRecord,
) -> FeedItem {
    let feed_id = format!(
        "{ROLLBACK_RECOMMENDATION_ATTENTION_PREFIX}{}",
        recommendation.id
    );
    let created_at = recommendation.created_at.timestamp_millis();
    let review_href = format!(
        "/skills/evolution?rollback_recommendation={}&candidate_id={}",
        recommendation.id, recommendation.candidate_id
    );
    FeedItem {
        id: feed_id.clone(),
        principal: recommendation.scope.principal.to_string(),
        workspace: recommendation.scope.workspace.to_string(),
        item_type: FeedItemType::Escalation,
        task_id: None,
        ui_thread_id: None,
        agent_id: None,
        title: "Review Skill Evolution rollback".to_string(),
        summary: Some(recommendation.summary.clone()),
        status: FeedItemStatus::NeedsAction,
        created_at,
        updated_at: created_at,
        actions: vec![FeedAction {
            id: "review".to_string(),
            label: "Review rollback".to_string(),
            action_type: Some("open_skill_evolution_rollback_recommendation".to_string()),
            payload: json!({
                "href": review_href,
                "candidate_id": recommendation.candidate_id.clone(),
                "recommendation_id": recommendation.id.clone(),
                "application_id": recommendation.application_id.clone()
            }),
        }],
        metadata: json!({
            "attention_kind": "skill_evolution.rollback_recommended",
            "attention_target": "skill_evolution_rollback_recommendation",
            "source": "escalation",
            "dedupe_key": feed_id.clone(),
            "review_href": review_href,
            "review_label": "Review rollback",
            "recommendation_id": recommendation.id.clone(),
            "candidate_id": recommendation.candidate_id.clone(),
            "proposal_id": recommendation.proposal_id.clone(),
            "validation_id": recommendation.validation_id.clone(),
            "implementation_id": recommendation.implementation_id.clone(),
            "application_id": recommendation.application_id.clone(),
            "promotion_id": recommendation.promotion_id.clone(),
            "capability_id": recommendation.capability_id.clone(),
            "rollback_status": recommendation.status.as_str(),
            "trigger_kind": recommendation.trigger_kind.clone(),
            "severity": recommendation.severity.clone(),
            "rollback_files": recommendation.rollback_files.clone(),
            "source_type": "learning_capability_rollback_recommendation",
            "source_id": recommendation.id.clone(),
            "why_it_matters": "An applied skill change has rollback evidence and a detected failure signal; review before further promotion.",
        }),
    }
}

fn rollback_recommendation_attention_feed_items(
    records: Vec<LearningCapabilityEvolutionRollbackRecommendationRecord>,
) -> Vec<FeedItem> {
    let mut items = records
        .into_iter()
        .map(|record| rollback_recommendation_attention_feed_item(&record))
        .collect::<Vec<_>>();
    sort_attention_feed_items(&mut items);
    items
}

fn post_promotion_monitor_attention_feed_item(
    monitor: &LearningCapabilityEvolutionPostPromotionMonitorRecord,
) -> FeedItem {
    let feed_id = format!(
        "{POST_PROMOTION_MONITOR_ATTENTION_PREFIX}{}",
        monitor.promotion_id
    );
    let created_at = monitor.created_at.timestamp_millis();
    let updated_at = monitor.updated_at.timestamp_millis();
    let review_href = format!(
        "/skills/evolution?post_promotion_monitor={}&candidate_id={}",
        monitor.promotion_id, monitor.candidate_id
    );
    FeedItem {
        id: feed_id.clone(),
        principal: monitor.scope.principal.to_string(),
        workspace: monitor.scope.workspace.to_string(),
        item_type: FeedItemType::Escalation,
        task_id: None,
        ui_thread_id: None,
        agent_id: None,
        title: "Review Skill Evolution post-promotion regression".to_string(),
        summary: Some(monitor.summary.clone()),
        status: FeedItemStatus::NeedsAction,
        created_at,
        updated_at,
        actions: vec![FeedAction {
            id: "review".to_string(),
            label: "Review monitor".to_string(),
            action_type: Some("open_skill_evolution_post_promotion_monitor".to_string()),
            payload: json!({
                "href": review_href,
                "candidate_id": monitor.candidate_id.clone(),
                "promotion_id": monitor.promotion_id.clone(),
                "monitor_id": monitor.id.clone(),
                "follow_up_candidate_id": monitor.follow_up_candidate_id.clone()
            }),
        }],
        metadata: json!({
            "attention_kind": "skill_evolution.post_promotion_regression",
            "attention_target": "skill_evolution_post_promotion_monitor",
            "source": "escalation",
            "dedupe_key": feed_id.clone(),
            "review_href": review_href,
            "review_label": "Review monitor",
            "monitor_id": monitor.id.clone(),
            "candidate_id": monitor.candidate_id.clone(),
            "proposal_id": monitor.proposal_id.clone(),
            "validation_id": monitor.validation_id.clone(),
            "implementation_id": monitor.implementation_id.clone(),
            "application_id": monitor.application_id.clone(),
            "promotion_id": monitor.promotion_id.clone(),
            "capability_id": monitor.capability_id.clone(),
            "monitor_status": monitor.status.as_str(),
            "skill_names": monitor.skill_names.clone(),
            "before_invocation_count": monitor.before_invocation_count,
            "after_invocation_count": monitor.after_invocation_count,
            "before_success_rate": monitor.before_success_rate,
            "after_success_rate": monitor.after_success_rate,
            "same_failure_recurrence_count": monitor.same_failure_recurrence_count,
            "new_failure_classes": monitor.new_failure_classes.clone(),
            "user_negative_feedback_count": monitor.user_negative_feedback_count,
            "follow_up_candidate_id": monitor.follow_up_candidate_id.clone(),
            "source_type": "learning_capability_post_promotion_monitor",
            "source_id": monitor.id.clone(),
            "why_it_matters": "A promoted skill change shows regression evidence, but no rollback recommendation is currently handling it.",
        }),
    }
}

fn post_promotion_monitor_attention_feed_items(
    store: &LearningStore,
    scope: &LearningScope,
    records: Vec<LearningCapabilityEvolutionPostPromotionMonitorRecord>,
) -> Vec<FeedItem> {
    let mut items = records
        .into_iter()
        .filter(|record| post_promotion_monitor_needs_attention(store, scope, record))
        .map(|record| post_promotion_monitor_attention_feed_item(&record))
        .collect::<Vec<_>>();
    sort_attention_feed_items(&mut items);
    items
}

fn post_promotion_monitor_needs_attention(
    store: &LearningStore,
    scope: &LearningScope,
    monitor: &LearningCapabilityEvolutionPostPromotionMonitorRecord,
) -> bool {
    if monitor.status != LearningCapabilityEvolutionPostPromotionMonitorStatus::RegressionDetected {
        return false;
    }
    if monitor.rollback_recommendation_id.is_some() {
        return false;
    }
    let Some(candidate_id) = monitor.follow_up_candidate_id.as_deref() else {
        return true;
    };
    match store.read_candidate(scope, candidate_id) {
        Ok(candidate) => !candidate.state.is_terminal(),
        Err(_) => true,
    }
}

fn update_projection_counts(
    projected_count: &mut usize,
    created_count: &mut usize,
    updated_count: &mut usize,
    previous: &Option<FeedItem>,
    item: &FeedItem,
) {
    *projected_count += 1;
    match previous {
        None => *created_count += 1,
        Some(previous) if !FeedItemPatch::between(previous, item).is_empty() => {
            *updated_count += 1;
        },
        Some(_) => {},
    }
}

fn learning_insight_feed_id(insight_id: &str) -> String {
    let insight_id = insight_id.trim();
    if insight_id.starts_with(LEARNING_INSIGHT_FEED_PREFIX) {
        insight_id.to_string()
    } else {
        format!(
            "{LEARNING_INSIGHT_FEED_PREFIX}{}",
            safe_feed_id_segment(insight_id)
        )
    }
}

fn learning_insight_source_feed_id(source_type: &str, source_id: &str) -> String {
    format!(
        "{LEARNING_INSIGHT_FEED_PREFIX}{}:{}",
        safe_feed_id_segment(source_type),
        safe_feed_id_segment(source_id)
    )
}

fn learning_candidate_insight_feed_item(candidate: &LearningCandidate) -> Option<FeedItem> {
    if candidate.state.is_terminal() || learning_candidate_feed_item(candidate).is_some() {
        return None;
    }
    let feed_id = learning_insight_source_feed_id("candidate", &candidate.id);
    let updated_at = candidate.updated_at.timestamp_millis();
    let created_at = candidate.created_at.timestamp_millis();
    let status = if candidate.review_required
        || matches!(candidate.risk_level.as_str(), "high" | "critical")
        || matches!(
            candidate.candidate_type,
            LearningCandidateType::BugReport
                | LearningCandidateType::ToolWrapperFix
                | LearningCandidateType::EvaluationCase
        ) {
        FeedItemStatus::NeedsAction
    } else {
        FeedItemStatus::Info
    };
    let kind = learning_candidate_insight_kind(candidate);
    Some(FeedItem {
        id: feed_id.clone(),
        principal: candidate.scope.principal.to_string(),
        workspace: candidate.scope.workspace.to_string(),
        item_type: FeedItemType::LearningInsight,
        task_id: candidate.source_task_id.clone(),
        ui_thread_id: candidate.source_chat_session_id.clone(),
        agent_id: candidate.source_agent_id.clone(),
        title: if candidate.title.trim().is_empty() {
            format!(
                "{} learning",
                humanize_token(candidate.candidate_type.as_str())
            )
        } else {
            candidate.title.clone()
        },
        summary: Some(candidate.summary.clone()),
        status,
        created_at,
        updated_at,
        actions: learning_insight_actions(&feed_id, true, true),
        metadata: json!({
            "insight_id": feed_id.clone(),
            "dedupe_key": feed_id.clone(),
            "novelty": learning_candidate_novelty(candidate),
            "related_candidate_id": candidate.id.clone(),
            "insight_kind": kind,
            "source_type": "learning_candidate",
            "source_id": candidate.id.clone(),
            "candidate_id": candidate.id.clone(),
            "candidate_type": candidate.candidate_type.as_str(),
            "candidate_state": candidate.state.as_str(),
            "why_it_matters": candidate.rationale.clone(),
            "confidence": candidate.confidence,
            "risk_level": candidate.risk_level.as_str(),
            "review_required": candidate.review_required,
            "review_reason": candidate.review_reason.clone(),
            "proposed_target": candidate.proposed_target.clone(),
            "promotion_target": candidate.promotion_target.clone(),
            "source_agent_id": candidate.source_agent_id.clone(),
            "source_task_id": candidate.source_task_id.clone(),
            "source_execution_id": candidate.source_execution_id.clone(),
            "source_chat_session_id": candidate.source_chat_session_id.clone(),
            "source_artifact_ids": source_artifact_ids_from_evidence_refs(&candidate.evidence_refs),
            "event_refs": candidate.event_refs.clone(),
            "evidence_refs": candidate.evidence_refs.clone(),
            "suggested_actions": ["open_evidence", "save_to_memory", "create_follow_up_task", "archive"],
        }),
    })
}

fn learning_event_insight_feed_item(event: &LearningEvent) -> Option<FeedItem> {
    if !is_high_signal_learning_event(event) {
        return None;
    }
    let feed_id = learning_insight_source_feed_id("event", &event.id);
    let created_at = event.created_at.timestamp_millis();
    let status = if event.event_type.contains("failed") || event.event_type.contains("error") {
        FeedItemStatus::Failed
    } else {
        FeedItemStatus::Info
    };
    Some(FeedItem {
        id: feed_id.clone(),
        principal: event.scope.principal.to_string(),
        workspace: event.scope.workspace.to_string(),
        item_type: FeedItemType::LearningInsight,
        task_id: event.task_id.clone(),
        ui_thread_id: event.chat_session_id.clone(),
        agent_id: event.agent_id.clone(),
        title: learning_event_title(event),
        summary: Some(event.summary.clone()),
        status,
        created_at,
        updated_at: created_at,
        actions: learning_insight_actions(&feed_id, true, true),
        metadata: json!({
            "insight_id": feed_id.clone(),
            "dedupe_key": feed_id.clone(),
            "novelty": metadata_string(&event.payload, "novelty").unwrap_or_else(|| "source_backed".to_string()),
            "related_candidate_id": metadata_string(&event.payload, "candidate_id"),
            "insight_kind": learning_event_kind(&event.event_type),
            "source_type": "learning_event",
            "source_id": event.id.clone(),
            "event_id": event.id.clone(),
            "event_type": event.event_type.clone(),
            "why_it_matters": learning_event_why_it_matters(event),
            "source_agent_id": event.agent_id.clone(),
            "source_task_id": event.task_id.clone(),
            "source_execution_id": event.execution_id.clone(),
            "source_chat_session_id": event.chat_session_id.clone(),
            "source_artifact_ids": source_artifact_ids_from_evidence_refs(&event.evidence_refs),
            "evidence_refs": event.evidence_refs.clone(),
            "payload": event.payload.clone(),
            "suggested_actions": ["open_evidence", "save_to_memory", "create_follow_up_task", "archive"],
        }),
    })
}

fn evaluation_run_insight_feed_item(report: &LearningEvaluationRunReport) -> FeedItem {
    let feed_id = learning_insight_source_feed_id("eval_run", &report.id);
    let created_at = report.created_at.timestamp_millis();
    let status = learning_eval_status_to_feed_status(report.status.as_str());
    FeedItem {
        id: feed_id.clone(),
        principal: report.scope.principal.to_string(),
        workspace: report.scope.workspace.to_string(),
        item_type: FeedItemType::LearningInsight,
        task_id: None,
        ui_thread_id: None,
        agent_id: None,
        title: format!(
            "Learning evaluation {}",
            humanize_token(report.status.as_str())
        ),
        summary: Some(report.summary.clone()),
        status,
        created_at,
        updated_at: created_at,
        actions: learning_insight_actions(&feed_id, true, true),
        metadata: json!({
            "insight_id": feed_id.clone(),
            "dedupe_key": feed_id.clone(),
            "novelty": metadata_string(&report.payload, "novelty").unwrap_or_else(|| "evaluation_signal".to_string()),
            "related_candidate_id": report.candidate_id.clone(),
            "insight_kind": "evaluation_result",
            "source_type": "learning_evaluation_run",
            "source_id": report.id.clone(),
            "run_id": report.id.clone(),
            "candidate_id": report.candidate_id.clone(),
            "backlog_id": report.backlog_id.clone(),
            "status": report.status.as_str(),
            "runner": report.runner.clone(),
            "commands": report.commands.clone(),
            "metrics": report.metrics.clone(),
            "payload": report.payload.clone(),
            "source_artifact_ids": source_artifact_ids_from_evidence_refs(&report.evidence_refs),
            "evidence_refs": report.evidence_refs.clone(),
            "why_it_matters": "Evaluation results show whether a proposed learning improved the system or introduced a regression.",
            "suggested_actions": ["open_evidence", "create_follow_up_task", "save_to_memory", "archive"],
        }),
    }
}

fn growth_evaluation_insight_feed_item(report: &LearningGrowthEvaluationRunReport) -> FeedItem {
    let feed_id = learning_insight_source_feed_id("growth_eval", &report.id);
    let created_at = report.created_at.timestamp_millis();
    let status = learning_eval_status_to_feed_status(report.status.as_str());
    let dimension_summary = report
        .dimensions
        .iter()
        .map(|dimension| {
            format!(
                "{}: {} ({:.0}%)",
                humanize_token(&dimension.dimension),
                dimension.status.as_str(),
                dimension.score * 100.0
            )
        })
        .collect::<Vec<_>>();
    FeedItem {
        id: feed_id.clone(),
        principal: report.scope.principal.to_string(),
        workspace: report.scope.workspace.to_string(),
        item_type: FeedItemType::LearningInsight,
        task_id: None,
        ui_thread_id: None,
        agent_id: None,
        title: format!(
            "Growth evaluation {}",
            humanize_token(report.status.as_str())
        ),
        summary: Some(report.summary.clone()),
        status,
        created_at,
        updated_at: created_at,
        actions: learning_insight_actions(&feed_id, true, true),
        metadata: json!({
            "insight_id": feed_id.clone(),
            "dedupe_key": feed_id.clone(),
            "novelty": metadata_string(&report.payload, "novelty").unwrap_or_else(|| "growth_signal".to_string()),
            "related_candidate_id": metadata_string(&report.payload, "candidate_id"),
            "insight_kind": "growth_evaluation",
            "source_type": "learning_growth_evaluation_run",
            "source_id": report.id.clone(),
            "run_id": report.id.clone(),
            "suite_id": report.suite_id.clone(),
            "status": report.status.as_str(),
            "dimensions": report.dimensions.clone(),
            "dimension_summary": dimension_summary,
            "scenarios": report.scenarios.clone(),
            "metrics": report.metrics.clone(),
            "payload": report.payload.clone(),
            "source_artifact_ids": source_artifact_ids_from_evidence_refs(&report.evidence_refs),
            "evidence_refs": report.evidence_refs.clone(),
            "why_it_matters": "Growth evaluations summarize whether the agent is improving across memory, skills, planning, evaluation, and autonomous execution.",
            "suggested_actions": ["open_evidence", "create_follow_up_task", "save_to_memory", "archive"],
        }),
    }
}

fn evaluation_backlog_insight_feed_item(item: &LearningEvaluationBacklogItem) -> FeedItem {
    let feed_id = learning_insight_source_feed_id("eval_backlog", &item.id);
    let updated_at = item.updated_at.timestamp_millis();
    let created_at = item.created_at.timestamp_millis();
    let status = match item.status.as_str() {
        "queued" | "in_review" => FeedItemStatus::NeedsAction,
        "archived" | "rejected" => FeedItemStatus::Done,
        _ => FeedItemStatus::Info,
    };
    FeedItem {
        id: feed_id.clone(),
        principal: item.scope.principal.to_string(),
        workspace: item.scope.workspace.to_string(),
        item_type: FeedItemType::LearningInsight,
        task_id: item.source_task_id.clone(),
        ui_thread_id: item.source_chat_session_id.clone(),
        agent_id: item
            .target_agent_id
            .clone()
            .or_else(|| item.source_agent_id.clone()),
        title: if item.title.trim().is_empty() {
            "Learning evaluation candidate".to_string()
        } else {
            item.title.clone()
        },
        summary: Some(item.summary.clone()),
        status,
        created_at,
        updated_at,
        actions: learning_insight_actions(&feed_id, false, true),
        metadata: json!({
            "insight_id": feed_id.clone(),
            "dedupe_key": feed_id.clone(),
            "novelty": "pending_evaluation",
            "related_candidate_id": item.candidate_id.clone(),
            "insight_kind": "evaluation_candidate",
            "source_type": "learning_evaluation_backlog",
            "source_id": item.id.clone(),
            "backlog_id": item.id.clone(),
            "candidate_id": item.candidate_id.clone(),
            "status": item.status.as_str(),
            "case_kind": item.case_kind.clone(),
            "priority": item.priority.clone(),
            "target_agent_id": item.target_agent_id.clone(),
            "focus_area": item.focus_area.clone(),
            "proposed_target": item.proposed_target.clone(),
            "source_agent_id": item.source_agent_id.clone(),
            "source_task_id": item.source_task_id.clone(),
            "source_execution_id": item.source_execution_id.clone(),
            "source_chat_session_id": item.source_chat_session_id.clone(),
            "case_spec": item.case_spec.clone(),
            "source_artifact_ids": source_artifact_ids_from_evidence_refs(&item.evidence_refs),
            "evidence_refs": item.evidence_refs.clone(),
            "why_it_matters": item.rationale.clone(),
            "suggested_actions": ["open_evidence", "create_follow_up_task", "archive"],
        }),
    }
}

fn learning_candidate_insight_kind(candidate: &LearningCandidate) -> &'static str {
    match candidate.candidate_type {
        LearningCandidateType::SkillUpdate
        | LearningCandidateType::WorkflowTemplate
        | LearningCandidateType::MemoryProcedure => "procedure_or_skill",
        LearningCandidateType::CapabilityUpdate
        | LearningCandidateType::ToolSchemaUpdate
        | LearningCandidateType::ToolWrapperFix => "capability_evolution",
        LearningCandidateType::AgentPersonaUpdate => "agent_behavior",
        LearningCandidateType::EvaluationCase => "evaluation_candidate",
        LearningCandidateType::ProgramStateUpdate => "program_state",
        // Boundary D. Grouped with agent behaviour rather than program state:
        // a supplemental-guidance revision proposes a change to how the
        // harness *operates*, which is what a reader of this feed needs to
        // recognise, while `program_state` is routine bookkeeping.
        LearningCandidateType::HarnessProfileRevision => "agent_behavior",
        LearningCandidateType::BugReport => "bug_pattern",
        LearningCandidateType::DocsUpdate => "documentation",
        LearningCandidateType::MemoryFact | LearningCandidateType::MemoryPreference => "memory",
        LearningCandidateType::Other => "learning",
    }
}

fn learning_event_kind(event_type: &str) -> &'static str {
    if event_type.contains("memory") {
        "memory"
    } else if event_type.contains("eval") {
        "evaluation"
    } else if event_type.contains("reflection") {
        "reflection"
    } else if event_type.contains("route") || event_type.contains("teaching") {
        "agent_behavior"
    } else if event_type.contains("index") || event_type.contains("retrieval") {
        "retrieval"
    } else {
        "learning_event"
    }
}

fn is_high_signal_learning_event(event: &LearningEvent) -> bool {
    let event_type = event.event_type.as_str();
    if matches!(
        event_type,
        "learning_candidate_created"
            | "learning_memory_candidate_promoted"
            | "memory_index_rebuild_completed"
            | "memory_index_reconcile_completed"
            | "memory_prompt_block_rendered"
    ) {
        return false;
    }
    let summary = event.summary.trim();
    let failure = event_type.contains("failed") || event_type.contains("error");
    !summary.is_empty()
        && (failure
            || event_type.contains("route")
            || event_type.contains("eval")
            || event_type.contains("reflection")
            || (event_type.contains("memory") && event_type.contains("completed"))
            || event_type.contains("teaching")
            || (event_type.contains("retrieval") && failure)
            || (event_type.contains("index") && failure))
}

fn learning_event_title(event: &LearningEvent) -> String {
    match event.event_type.as_str() {
        "learning_reflection_completed" => "Reflection produced learning signals".to_string(),
        "learning_route_failed" => "Learning route failed".to_string(),
        "memory_index_rebuild_failed" => "Memory index rebuild failed".to_string(),
        "memory_index_rebuild_completed" => "Memory index refreshed".to_string(),
        "memory_index_reconcile_failed" => "Memory index reconciliation failed".to_string(),
        "memory_index_reconcile_completed" => "Memory index refreshed".to_string(),
        other => format!("Learning {}", humanize_token(other)),
    }
}

fn learning_event_why_it_matters(event: &LearningEvent) -> String {
    if let Some(reason) = metadata_string(&event.payload, "why_it_matters") {
        return reason;
    }
    match learning_event_kind(&event.event_type) {
        "memory" => {
            "Memory activity changes what the agent will recall in future runs.".to_string()
        },
        "evaluation" => {
            "Evaluation activity shows where learning is validated or needs repair.".to_string()
        },
        "reflection" => {
            "Reflection activity turns execution evidence into reusable agent learning.".to_string()
        },
        "agent_behavior" => "Behavior-routing activity shows how the agent is adapting its next \
                             decisions."
            .to_string(),
        "retrieval" => {
            "Retrieval activity affects which prior knowledge reaches the model.".to_string()
        },
        _ => "This learning event may change future agent behavior or explain why it changed."
            .to_string(),
    }
}

fn learning_eval_status_to_feed_status(status: &str) -> FeedItemStatus {
    match status {
        "failed" => FeedItemStatus::Failed,
        "blocked" => FeedItemStatus::NeedsAction,
        "passed" => FeedItemStatus::Info,
        _ => FeedItemStatus::Info,
    }
}

fn learning_insight_actions(
    insight_id: &str,
    include_save_to_memory: bool,
    include_follow_up: bool,
) -> Vec<FeedAction> {
    let mut actions = vec![FeedAction {
        id: "open_evidence".to_string(),
        label: "Open evidence".to_string(),
        action_type: Some("open_learning_insight_evidence".to_string()),
        payload: json!({ "insight_id": insight_id }),
    }];
    if include_save_to_memory {
        actions.push(FeedAction {
            id: "save_to_memory".to_string(),
            label: "Save to memory".to_string(),
            action_type: Some("save_learning_insight_to_memory".to_string()),
            payload: json!({ "insight_id": insight_id }),
        });
    }
    if include_follow_up {
        actions.push(FeedAction {
            id: "create_follow_up_task".to_string(),
            label: "Create task".to_string(),
            action_type: Some("create_learning_insight_follow_up_task".to_string()),
            payload: json!({ "insight_id": insight_id }),
        });
    }
    actions.push(FeedAction {
        id: "archive".to_string(),
        label: "Archive".to_string(),
        action_type: Some("archive_learning_insight".to_string()),
        payload: json!({ "insight_id": insight_id }),
    });
    actions
}

fn insight_memory_key(item: &FeedItem) -> String {
    metadata_string(&item.metadata, "candidate_id")
        .or_else(|| metadata_string(&item.metadata, "event_id"))
        .or_else(|| metadata_string(&item.metadata, "run_id"))
        .or_else(|| metadata_string(&item.metadata, "backlog_id"))
        .map(|id| format!("{}:{}", normalize_memory_token(&item.title), id))
        .unwrap_or_else(|| normalize_memory_token(&item.title))
}

fn follow_up_task_description(item: &FeedItem) -> String {
    let mut lines = vec![
        format!("Follow up on this learning insight: {}", item.title),
        String::new(),
    ];
    if let Some(summary) = item
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        lines.push(format!("Summary: {summary}"));
    }
    if let Some(reason) = metadata_string(&item.metadata, "why_it_matters") {
        lines.push(format!("Why it matters: {reason}"));
    }
    if let Some(source_type) = metadata_string(&item.metadata, "source_type") {
        lines.push(format!("Source: {source_type} ({})", item.id));
    }
    if let Some(task_id) = item.task_id.as_deref() {
        lines.push(format!("Related task: {task_id}"));
    }
    if let Some(execution_id) = metadata_string(&item.metadata, "source_execution_id") {
        lines.push(format!("Related execution: {execution_id}"));
    }
    lines.join("\n")
}

fn metadata_number(value: &Value, key: &str) -> Option<f64> {
    value
        .as_object()
        .and_then(|record| record.get(key))
        .and_then(|value| match value {
            Value::Number(number) => number.as_f64(),
            Value::String(text) => text.trim().parse::<f64>().ok(),
            _ => None,
        })
}

fn learning_evidence_refs_from_metadata(metadata: &Value) -> Vec<LearningEvidenceRef> {
    metadata
        .as_object()
        .and_then(|record| record.get("evidence_refs"))
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

fn source_artifact_ids_from_evidence_refs(refs: &[LearningEvidenceRef]) -> Vec<String> {
    refs.iter()
        .filter(|reference| reference.kind == "artifact")
        .filter_map(|reference| {
            reference
                .id
                .clone()
                .or_else(|| reference.path.clone())
                .or_else(|| reference.uri.clone())
        })
        .collect()
}

fn learning_candidate_novelty(candidate: &LearningCandidate) -> String {
    metadata_string(&candidate.proposed_change, "novelty")
        .or_else(|| metadata_string(&candidate.review_policy, "novelty"))
        .unwrap_or_else(|| "source_backed".to_string())
}

fn safe_feed_id_segment(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            output.push(ch);
        } else {
            output.push('_');
        }
    }
    if output.is_empty() {
        "unknown".to_string()
    } else {
        output
    }
}

fn humanize_token(value: &str) -> String {
    let words = value
        .split(['_', '-', '.'])
        .map(str::trim)
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>();
    if words.is_empty() {
        "Learning".to_string()
    } else {
        words.join(" ")
    }
}

fn learning_memory_feed_projection(
    candidate: &LearningCandidate,
) -> Option<LearningMemoryFeedProjection> {
    let payload = memory_payload(candidate);
    let explicit_user_request = read_bool_any(
        payload,
        &[
            "explicit_user_request",
            "explicit_request",
            "user_requested",
        ],
    );
    let explicit_user_correction = read_bool_any(
        payload,
        &[
            "explicit_user_correction",
            "explicit_correction",
            "user_correction",
        ],
    );
    let target_scope = read_string_any(payload, &["scope", "memory_scope", "target_scope"])
        .or_else(|| target_scope_from_target(candidate.promotion_target.as_deref()))
        .or_else(|| target_scope_from_target(candidate.proposed_target.as_deref()))
        .map(|scope| normalize_memory_token(&scope))
        .unwrap_or_else(|| {
            if explicit_user_request
                || explicit_user_correction
                || candidate.candidate_type == LearningCandidateType::MemoryPreference
            {
                "user".to_string()
            } else {
                "agent".to_string()
            }
        });
    let target_tier = read_string_any(payload, &["target_tier", "tier_name", "tier"])
        .or_else(|| tier_from_target(candidate.promotion_target.as_deref()))
        .or_else(|| tier_from_target(candidate.proposed_target.as_deref()))
        .map(|tier| normalize_memory_token(&tier))
        .unwrap_or_else(|| {
            if candidate.candidate_type == LearningCandidateType::MemoryPreference {
                "preferences".to_string()
            } else {
                "knowledge".to_string()
            }
        });
    let memory_key = read_string_any(
        payload,
        &["key", "memory_key", "preference_key", "fact_key", "name"],
    )
    .unwrap_or_else(|| candidate.title.clone())
    .trim()
    .to_string();
    if memory_key.is_empty() {
        return None;
    }
    let memory_value = read_value_any(
        payload,
        &["value", "memory_value", "preference", "fact", "procedure"],
    )
    .unwrap_or_else(|| Value::String(candidate.summary.clone()));

    Some(LearningMemoryFeedProjection {
        target_scope,
        target_tier,
        memory_key,
        memory_value,
    })
}

fn existing_saved_memory_candidate_for_insight(
    store: &LearningStore,
    scope: &LearningScope,
    insight_id: &str,
) -> anyhow::Result<Option<LearningCandidate>> {
    for candidate in store.list_candidates(
        scope,
        LearningCandidateFilters {
            limit: None,
            ..Default::default()
        },
    )? {
        if metadata_string(memory_payload(&candidate), "source_insight_id").as_deref()
            == Some(insight_id)
        {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn memory_payload(candidate: &LearningCandidate) -> &Value {
    candidate
        .proposed_change
        .get("memory")
        .unwrap_or(&candidate.proposed_change)
}

fn read_string_any(payload: &Value, keys: &[&str]) -> Option<String> {
    let object = payload.as_object()?;
    for key in keys {
        let Some(value) = object.get(*key) else {
            continue;
        };
        match value {
            Value::String(text) => {
                let text = text.trim();
                if !text.is_empty() {
                    return Some(text.to_string());
                }
            },
            Value::Number(_) | Value::Bool(_) => return Some(value.to_string()),
            _ => {},
        }
    }
    None
}

fn read_value_any(payload: &Value, keys: &[&str]) -> Option<Value> {
    let object = payload.as_object()?;
    for key in keys {
        let Some(value) = object.get(*key) else {
            continue;
        };
        if !value.is_null() {
            return Some(value.clone());
        }
    }
    None
}

fn read_bool_any(payload: &Value, keys: &[&str]) -> bool {
    let Some(object) = payload.as_object() else {
        return false;
    };
    for key in keys {
        match object.get(*key) {
            Some(Value::Bool(value)) => return *value,
            Some(Value::String(value)) => {
                let value = value.trim().to_ascii_lowercase();
                if matches!(value.as_str(), "true" | "yes" | "1") {
                    return true;
                }
                if matches!(value.as_str(), "false" | "no" | "0") {
                    return false;
                }
            },
            _ => {},
        }
    }
    false
}

fn target_scope_from_target(target: Option<&str>) -> Option<String> {
    let target = target?.trim();
    let (scope, _) = target.split_once('.')?;
    let scope = scope.trim();
    if scope.is_empty() {
        None
    } else {
        Some(scope.to_string())
    }
}

fn tier_from_target(target: Option<&str>) -> Option<String> {
    let target = target?.trim();
    let (_, tier) = target.split_once('.')?;
    let tier = tier.trim();
    if tier.is_empty() {
        None
    } else {
        Some(tier.to_string())
    }
}

fn normalize_memory_token(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace([' ', '-'], "_")
}

fn today_compact_identifier_token(value: &str) -> String {
    let mut token = String::new();
    let mut last_was_separator = false;
    for ch in value.trim().chars().take(180) {
        if ch.is_ascii_alphanumeric() {
            token.push(ch.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator {
            token.push('_');
            last_was_separator = true;
        }
    }
    let token = token.trim_matches('_').to_string();
    if token.is_empty() {
        "unknown".to_string()
    } else {
        token
    }
}

fn summarize_json_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.trim().to_string(),
        Value::Null => String::new(),
        _ => value.to_string(),
    }
}

fn summarize_learning_memory_value(value: &Value) -> String {
    summarize_public_contact_identity_value(value).unwrap_or_else(|| summarize_json_value(value))
}

fn summarize_public_contact_identity_value(value: &Value) -> Option<String> {
    let object = value.as_object()?;
    if object
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind != "public_contact_identity_research")
    {
        return None;
    }

    let mut parts = Vec::new();
    if let Some(identity) = object
        .get("owner_reviewed_identity")
        .or_else(|| object.get("possible_identity"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        parts.push(format!("Identity: {identity}"));
    }
    if let Some(org) = object
        .get("org")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        parts.push(format!("Org: {org}"));
    }
    if let Some(role) = object
        .get("role")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        parts.push(format!("Role: {role}"));
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join(" · "))
}

/// Filters out task-bound feed items whose `task_id` references a task that no
/// longer exists, then truncates to `limit`. Durable deliveries and routines
/// can carry `task_id` as provenance even after the source task is
/// archived/deleted, so they are kept. If `valid_task_ids` is `None` (task
/// listing failed), no filtering is applied.
fn retain_existing_tasks(
    items: Vec<FeedItem>,
    valid_task_ids: &Option<HashSet<String>>,
    limit: usize,
) -> Vec<FeedItem> {
    let Some(valid) = valid_task_ids else {
        let mut items = items;
        items.truncate(limit);
        return items;
    };
    items
        .into_iter()
        .filter(|item| match &item.task_id {
            Some(task_id) if feed_item_requires_live_task(item) => valid.contains(task_id),
            None => true,
            _ => true,
        })
        .take(limit)
        .collect()
}

fn sort_attention_feed_items(items: &mut [FeedItem]) {
    items.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.id.cmp(&left.id))
    });
}

fn feed_item_requires_live_task(item: &FeedItem) -> bool {
    matches!(
        item.item_type,
        FeedItemType::Task | FeedItemType::Approval | FeedItemType::Escalation
    )
}

/// True when a `TaskListItemV3.status` string denotes a terminal state.
/// Mirrors `TaskStatus::is_terminal` for the already-stringified status the
/// attention-eligibility filter works with. Normalizes case/whitespace and
/// accepts the legacy single-l `canceled` spelling — matching
/// `today_task_status_is_terminal` and the internal-task list builder —
/// otherwise an oddly-spelled terminal task is mis-read as live and
/// over-keeps a stale prompt.
fn task_status_str_is_terminal(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "completed" | "failed" | "cancelled" | "canceled"
    )
}

fn task_status_str_is_active_work(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "planning" | "running" | "waiting_for_children" | "paused"
    )
}

fn feed_item_has_current_active_task_state(item: &FeedItem) -> bool {
    if item.item_type != FeedItemType::Task {
        return true;
    }
    let task_status = metadata_string(&item.metadata, "task_status");
    let has_task_status = task_status.is_some();
    let has_active_root_key = metadata_has_key(&item.metadata, "active_root_execution_id");
    if !has_task_status && !has_active_root_key {
        // Legacy feed row without current task-state metadata: preserve the
        // historical feed-status behavior instead of hiding potentially-live work.
        return true;
    }
    if task_status
        .as_deref()
        .is_some_and(|status| !task_status_str_is_active_work(status))
    {
        return false;
    }
    if has_active_root_key && metadata_string(&item.metadata, "active_root_execution_id").is_none()
    {
        return false;
    }
    true
}

fn dedupe_feed_items_by_id(items: Vec<FeedItem>) -> Vec<FeedItem> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for item in items {
        if seen.insert(item.id.clone()) {
            deduped.push(item);
        }
    }
    deduped
}

fn today_item_is_delivered(item: &FeedItem) -> bool {
    if item.status != FeedItemStatus::Done {
        return false;
    }
    match item.item_type {
        FeedItemType::DataDelivery | FeedItemType::RoutineResult => return true,
        FeedItemType::Task => {},
        _ => return false,
    }
    if item
        .summary
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
    {
        return true;
    }
    if metadata_string(&item.metadata, "completion_outcome").is_some() {
        return true;
    }
    let artifact_names = item
        .metadata
        .as_object()
        .and_then(|metadata| metadata.get("completion_artifact_names"));
    match artifact_names {
        Some(Value::Array(values)) => !values.is_empty(),
        Some(Value::String(value)) => !value.trim().is_empty(),
        _ => false,
    }
}

fn today_item_is_active_work(item: &FeedItem) -> bool {
    item.status == FeedItemStatus::Running
        && feed_item_has_current_active_task_state(item)
        && !matches!(
            item.item_type,
            FeedItemType::AgentLearning
                | FeedItemType::LearningCandidate
                | FeedItemType::LearningInsight
                | FeedItemType::AgentMessage
        )
}

/// Build the Follow-ups lane.
///
/// `today` is the READER's date, resolved once per request by
/// [`resolve_today_reader_date`] and passed in — never re-derived from the
/// clock here. `now_ms` is the same instant that date was resolved against,
/// so the day boundaries and the 24-hour staleness windows agree.
fn today_followup_items_from_sources(
    tasks: Vec<TaskListItemV3>,
    routine_results: Vec<FeedItem>,
    source_followups: Vec<TodayItem>,
    principal: &str,
    workspace: &str,
    limit: usize,
    today: chrono::NaiveDate,
    now_ms: i64,
) -> Vec<TodayItem> {
    let tomorrow = today + chrono::Duration::days(1);
    let stale_before_ms = now_ms - (24 * 60 * 60 * 1000);
    let stale_routine_before_ms = now_ms - (24 * 60 * 60 * 1000);
    let mut items = tasks
        .into_iter()
        .filter_map(|task| {
            today_followup_item_from_task(
                task,
                principal,
                workspace,
                today,
                tomorrow,
                stale_before_ms,
                stale_routine_before_ms,
            )
        })
        .collect::<Vec<_>>();
    items.extend(
        routine_results
            .into_iter()
            .filter_map(|item| today_followup_item_from_routine_result(item, principal, workspace)),
    );
    items.extend(source_followups);
    items.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.updated_at.cmp(&right.updated_at))
    });
    items = today_dedupe_followup_items(items);
    items.truncate(limit);
    items
}

fn today_meeting_knowledge_path(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> std::path::PathBuf {
    workspace_layout
        .memory_root(principal, workspace)
        .join("users")
        .join("knowledge.json")
}

/// Read the meeting-memory document, or `None` when there is nothing readable
/// there.
///
/// **Blocking, and by far the heaviest single read on the `/today` path**:
/// 1.1MB in the author's own scope, byte-scanned by
/// `workspace_json_document_is_admitted` and then parsed into an untyped
/// `Value`, whose node-per-scalar representation costs several times the
/// document on the heap. Split out of the projection so a caller can put it
/// where blocking work belongs — see
/// [`FeedApi::today_meeting_followup_items_from_memory_off_reactor`].
fn read_today_meeting_knowledge(
    workspace_layout: &ArtifactV2Workspace,
    path: &Path,
) -> Option<Value> {
    match workspace_layout.read_json_path_sync::<Value, _>(path) {
        Ok(value) => Some(value),
        Err(error) => {
            if let magician::magician_v2::artifact_v2::ArtifactV2Error::Io(io_error) = &error {
                if io_error.kind() == ErrorKind::NotFound {
                    return None;
                }
            }
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "[FEED-API] ignoring unreadable meeting memory for Today follow-ups"
            );
            None
        },
    }
}

fn today_meeting_followup_items_from_memory(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    limit: usize,
    known_tasks: &[TaskListItemV3],
) -> Vec<TodayItem> {
    let path = today_meeting_knowledge_path(workspace_layout, principal, workspace);
    let Some(knowledge) = read_today_meeting_knowledge(workspace_layout, &path) else {
        return Vec::new();
    };
    today_meeting_followup_items_from_knowledge(
        &knowledge,
        principal,
        workspace,
        limit,
        known_tasks,
    )
}

fn today_meeting_followup_items_from_knowledge(
    knowledge: &Value,
    principal: &str,
    workspace: &str,
    limit: usize,
    known_tasks: &[TaskListItemV3],
) -> Vec<TodayItem> {
    let mut items = Vec::new();
    for entry in today_research_finding_entries(knowledge) {
        if !today_memory_entry_is_meeting(entry) {
            continue;
        }
        let Some(action_items) = entry.get("action_items").and_then(Value::as_array) else {
            continue;
        };
        for (action_index, action_item) in action_items.iter().enumerate() {
            let Some(action_text) = today_meeting_action_item_text(action_item) else {
                continue;
            };
            if let Some(item) = today_meeting_action_followup_item(
                entry,
                action_item,
                action_index,
                &action_text,
                principal,
                workspace,
                known_tasks,
            ) {
                items.push(item);
            }
        }
    }
    items.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut items = today_dedupe_followup_items(items);
    items.truncate(limit);
    items
}

fn today_research_finding_entries(knowledge: &Value) -> Vec<&Value> {
    let mut entries = Vec::new();
    if let Some(values) = knowledge.get("research_findings").and_then(Value::as_array) {
        entries.extend(values.iter());
    }
    if let Some(values) = knowledge
        .get("fields")
        .and_then(|fields| fields.get("research_findings"))
        .and_then(Value::as_array)
    {
        entries.extend(values.iter());
    }
    if let Some(values) = knowledge
        .get("fields")
        .and_then(|fields| fields.get("findings"))
        .and_then(Value::as_array)
    {
        entries.extend(values.iter());
    }
    entries
}

fn today_memory_entry_is_meeting(entry: &Value) -> bool {
    metadata_string(entry, "source_type").is_some_and(|value| value == "meeting_capture")
        || metadata_string(entry, "key").is_some_and(|value| value.trim().starts_with("meeting:"))
        || metadata_string(entry, "thread_id")
            .is_some_and(|value| value.trim().starts_with("meeting-"))
}

fn today_meeting_action_item_text(action_item: &Value) -> Option<String> {
    match action_item {
        Value::String(value) => {
            let value = value.trim();
            if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            }
        },
        Value::Object(_) => ["text", "description", "action", "task", "title", "value"]
            .into_iter()
            .find_map(|key| metadata_string(action_item, key)),
        _ => None,
    }
}

fn today_first_source_url(entry: &Value) -> Option<String> {
    for key in ["source_url", "source_urls", "sources", "url"] {
        let Some(value) = entry.as_object().and_then(|record| record.get(key)) else {
            continue;
        };
        match value {
            Value::String(url) if today_url_is_openable(url) => {
                return Some(url.trim().to_string())
            },
            Value::Array(urls) => {
                if let Some(url) = urls
                    .iter()
                    .filter_map(Value::as_str)
                    .find(|url| today_url_is_openable(url))
                {
                    return Some(url.trim().to_string());
                }
            },
            Value::Object(source) => {
                if let Some(url) = ["url", "source_url", "summary"]
                    .into_iter()
                    .filter_map(|field| source.get(field).and_then(Value::as_str))
                    .find(|url| today_url_is_openable(url))
                {
                    return Some(url.trim().to_string());
                }
            },
            _ => {},
        }
    }
    None
}

fn today_url_is_openable(value: &str) -> bool {
    let value = value.trim();
    value.starts_with('/') || value.starts_with("http://") || value.starts_with("https://")
}

fn today_meeting_action_followup_item(
    entry: &Value,
    action_item: &Value,
    action_index: usize,
    action_text: &str,
    principal: &str,
    workspace: &str,
    known_tasks: &[TaskListItemV3],
) -> Option<TodayItem> {
    let action_text = action_text.trim();
    if action_text.is_empty() {
        return None;
    }
    let meeting_key = metadata_string(entry, "key")
        .or_else(|| {
            metadata_string(entry, "thread_id").map(|thread_id| format!("meeting:{thread_id}"))
        })
        .or_else(|| {
            metadata_string(entry, "title").map(|title| {
                format!(
                    "meeting:{}",
                    today_compact_identifier_token(&truncate_today_text(&title, 80))
                )
            })
        })?;
    let source_id = format!("{meeting_key}:action:{action_index}");
    let source_token = today_compact_identifier_token(&source_id);
    let thread_id = metadata_string(entry, "thread_id");
    let source_url = today_first_source_url(entry).or_else(|| {
        thread_id
            .as_ref()
            .map(|thread_id| format!("/meetings/{}", urlencoding::encode(thread_id)))
    });
    let meeting_title = metadata_string(entry, "title");
    let meeting_date = metadata_string(entry, "date");
    let meeting_summary = metadata_string(entry, "summary");
    let updated_at = today_json_timestamp_millis_for_keys(entry, &["updated_at", "date"])
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    let created_at =
        today_json_timestamp_millis_for_keys(entry, &["created_at", "date"]).unwrap_or(updated_at);
    let mut summary_parts = Vec::new();
    if let Some(title) = meeting_title.as_deref() {
        summary_parts.push(format!("From {title}"));
    }
    if let Some(date) = meeting_date.as_deref() {
        summary_parts.push(date.to_string());
    }
    if let Some(summary) = meeting_summary.as_deref() {
        summary_parts.push(truncate_today_text(summary, 160));
    }
    let detail_markdown = match meeting_summary.as_deref() {
        Some(summary) if !summary.trim().is_empty() => {
            format!(
                "## Action item\n\n{action_text}\n\n## Meeting summary\n\n{}",
                summary.trim()
            )
        },
        _ => action_text.to_string(),
    };
    let metadata = json!({
        "followup_kind": "meeting_action_item",
        "meeting_key": meeting_key,
        "meeting_title": meeting_title,
        "meeting_date": meeting_date,
        // Keep the card summary compact, but carry the complete Markdown into
        // native detail surfaces. Meeting threads can expire independently of
        // their durable action items, so clients must not need to re-fetch the
        // old thread to show the full context.
        "detail_markdown": detail_markdown,
        "meeting_summary": meeting_summary,
        "action_item": action_text,
        "action_index": action_index,
        "source_type": metadata_string(entry, "source_type"),
        "linked_task_id": today_meeting_action_item_linked_task_id(action_item),
    });
    let mut item = TodayItem {
        id: format!("today:followups:meeting_action:{source_token}"),
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        section: TodaySectionId::Followups,
        priority: 730,
        title: format!("Meeting action: {}", truncate_today_text(action_text, 120)),
        summary: if summary_parts.is_empty() {
            None
        } else {
            Some(summary_parts.join(" - "))
        },
        reason: "Captured as an action item from a meeting summary.".to_string(),
        source_kind: "meeting_action".to_string(),
        source_id,
        source_url,
        space_ids: today_space_ids(entry, workspace),
        thread_id,
        task_id: None,
        agent_id: None,
        status: FeedItemStatus::NeedsAction,
        actions: Vec::new(),
        evidence_refs: today_evidence_refs(entry),
        created_at,
        updated_at,
        expires_at: None,
        seen_at: None,
        dismissed_at: None,
        snoozed_until: None,
        metadata,
    };
    let subject = today_action_subject(&item);
    if let Some(adapter) = today_action_adapter(&item.source_kind) {
        if known_tasks
            .iter()
            .any(|task| adapter.task_is_linked(&subject, task))
        {
            return None;
        }
        item.actions = adapter.available_actions(&subject);
    }
    Some(item)
}

fn today_meeting_action_item_linked_task_id(action_item: &Value) -> Option<String> {
    ["task_id", "linked_task_id"]
        .into_iter()
        .find_map(|key| metadata_string(action_item, key))
        .filter(|task_id| task_id.starts_with("task_"))
}

/// Project acknowledged, unlinked action-kind nodes of one ACTIVE thinking map
/// into Today follow-up candidates (Live Thinking Map plan Phase 8).
///
/// A node qualifies only when it is owner-asserted (owner origin AND
/// asserted/confirmed epistemic state), action-kind, and not tombstoned. Once
/// the node carries a `task` promoted_ref the candidate is suppressed for
/// EVERY task status — the link recorded on the map is authoritative, so
/// linked, terminal, or even deleted tasks never resurrect the candidate.
/// `include_promoted` exists for the action-execution path only, so a retry
/// against an already-promoted node can resolve the linked task instead of
/// 404ing.
fn today_thinking_map_action_items_from_map(
    map: &ThinkingMap,
    principal: &str,
    workspace: &str,
    known_tasks: &[TaskListItemV3],
    include_promoted: bool,
) -> Vec<TodayItem> {
    if map.lifecycle != MapLifecycle::Active {
        return Vec::new();
    }
    let mut items = Vec::new();
    for (node_id, node) in &map.nodes {
        if node.kind != NodeKind::Action || node.tombstoned {
            continue;
        }
        if !matches!(
            node.assertion_origin,
            AssertionOrigin::OwnerSpoken | AssertionOrigin::OwnerEdited
        ) {
            continue;
        }
        if !matches!(
            node.epistemic_state,
            EpistemicState::Asserted | EpistemicState::Confirmed
        ) {
            continue;
        }
        let linked_task_id = node
            .promoted_refs
            .iter()
            .find(|promoted| promoted.destination_kind == PromotionKind::Task)
            .map(|promoted| promoted.object_id.clone());
        if linked_task_id.is_some() && !include_promoted {
            continue;
        }
        let source_id = format!("thinking_map:{}:node:{node_id}", map.map_id);
        let source_token = today_compact_identifier_token(&source_id);
        let updated_at = today_parse_task_timestamp_millis(&node.updated_at)
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
        let created_at = today_parse_task_timestamp_millis(&node.created_at).unwrap_or(updated_at);
        let metadata = json!({
            "followup_kind": "thinking_map_action",
            "map_id": map.map_id,
            "node_id": node_id,
            "map_title": map.title,
            "node_label": node.label,
            "node_epistemic_state": node.epistemic_state,
            "node_assertion_origin": node.assertion_origin,
            // Full context for native detail surfaces, mirroring the meeting
            // adapter's durable action card.
            "detail_markdown": node
                .detail_markdown
                .clone()
                .unwrap_or_else(|| node.label.clone()),
            "linked_task_id": linked_task_id,
        });
        let mut item = TodayItem {
            id: format!("{TODAY_THINKING_MAP_ITEM_PREFIX}{source_token}"),
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            section: TodaySectionId::Followups,
            priority: 725,
            title: format!("Map action: {}", truncate_today_text(&node.label, 120)),
            summary: Some(format!("From {}", truncate_today_text(&map.title, 80))),
            reason: "Acknowledged action on your thinking map without a linked task.".to_string(),
            source_kind: THINKING_MAP_ACTION_SOURCE_KIND.to_string(),
            source_id,
            // Canonical navigation back to the map (web detail page; native
            // clients map the same route).
            source_url: Some(format!(
                "/thinking-maps/{}?node={}",
                urlencoding::encode(&map.map_id),
                urlencoding::encode(node_id)
            )),
            space_ids: vec![workspace.to_string()],
            thread_id: Some(format!("thinking-map-{}", map.map_id)),
            task_id: None,
            agent_id: None,
            status: FeedItemStatus::NeedsAction,
            actions: Vec::new(),
            evidence_refs: Vec::new(),
            created_at,
            updated_at,
            expires_at: None,
            seen_at: None,
            dismissed_at: None,
            snoozed_until: None,
            metadata,
        };
        let subject = today_action_subject(&item);
        if let Some(adapter) = today_action_adapter(&item.source_kind) {
            // Meeting-action reconciliation rules: a task already durably
            // linked to this node (explicit id, provenance marker, or same
            // map-thread + title) suppresses the candidate.
            if known_tasks
                .iter()
                .any(|task| adapter.task_is_linked(&subject, task))
            {
                continue;
            }
            item.actions = adapter.available_actions(&subject);
        }
        items.push(item);
    }
    items
}

fn today_action_subject(item: &TodayItem) -> TodayActionSubject {
    TodayActionSubject {
        item_id: item.id.clone(),
        principal: item.principal.clone(),
        workspace: item.workspace.clone(),
        source_kind: item.source_kind.clone(),
        source_id: item.source_id.clone(),
        source_url: item.source_url.clone(),
        thread_id: item.thread_id.clone(),
        title: item.title.clone(),
        summary: item.summary.clone(),
        metadata: item.metadata.clone(),
    }
}

/// One Today `Changed` card for one emitted monitor update (Recurring
/// Monitors Phase 3, §9.2). The item id embeds the deterministic
/// `update_id` (itself a hash of the §7.4 dedupe key), so dismissing the
/// card suppresses exactly this change fingerprint — the NEXT different
/// fingerprint mints a new id and surfaces again (§5.4 step 4). Metadata
/// carries monitor task/update/fingerprint ids for exact deep-linking.
fn today_item_from_monitor_update(
    update: MonitorUpdateDetailV1,
    principal: &str,
    workspace: &str,
) -> TodayItem {
    let occurred_ms = today_parse_task_timestamp_millis(&update.occurred_at)
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    let reason = match update.status {
        MonitorRunStatus::Baseline => {
            "First baseline this monitor captured (you opted into baseline notifications)."
                .to_string()
        },
        _ => "A monitor you set up found a material change.".to_string(),
    };
    let mut item = TodayItem {
        id: format!("today:changed:monitor_update:{}", update.update_id),
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        section: TodaySectionId::Changed,
        priority: 640,
        title: update.headline.clone(),
        summary: Some(update.summary.clone()),
        reason,
        source_kind: MONITOR_UPDATE_SOURCE_KIND.to_string(),
        source_id: update.update_id.clone(),
        // Canonical monitor deep link (web `monitorsTaskRoute`): the monitor
        // surface with the EXACT update highlighted — never the plain
        // /tasks?task= view, which the web does not read for monitors.
        source_url: Some(
            magician::magician_v2::feed::action_adapter::monitor_deep_link(
                &update.monitor_task_id,
                Some(&update.update_id),
            ),
        ),
        space_ids: Vec::new(),
        thread_id: None,
        task_id: Some(update.monitor_task_id.clone()),
        agent_id: None,
        status: FeedItemStatus::Info,
        actions: Vec::new(),
        evidence_refs: Vec::new(),
        created_at: occurred_ms,
        updated_at: occurred_ms,
        expires_at: None,
        seen_at: None,
        dismissed_at: None,
        snoozed_until: None,
        metadata: json!({
            "update_kind": "monitor_update",
            "monitor_task_id": update.monitor_task_id,
            "update_id": update.update_id,
            "change_fingerprint": update.change_fingerprint,
            "execution_id": update.execution_id,
            "monitor_revision": update.monitor_revision,
            "status": update.status,
            "dedupe_key": update.notification.dedupe_key,
            "finding_count": update.findings.len(),
        }),
    };
    let subject = today_action_subject(&item);
    if let Some(adapter) = today_action_adapter(&item.source_kind) {
        item.actions = adapter.available_actions(&subject);
    }
    item
}

fn today_followup_item_from_task(
    task: TaskListItemV3,
    principal: &str,
    workspace: &str,
    today: chrono::NaiveDate,
    tomorrow: chrono::NaiveDate,
    stale_before_ms: i64,
    stale_routine_before_ms: i64,
) -> Option<TodayItem> {
    let due_date = task.due_date.as_deref().and_then(today_parse_task_due_date);
    let updated_at = today_parse_task_timestamp_millis(&task.updated_at).unwrap_or_default();
    let is_terminal = today_task_status_is_terminal(&task.status);
    let stale_routine_can_preempt = is_terminal
        || (!task.is_blocked
            && due_date.is_none_or(|due| due > tomorrow)
            && !today_task_is_stale_active(&task, updated_at, stale_before_ms));
    if stale_routine_can_preempt
        && today_task_is_stale_scheduled_routine(&task, updated_at, stale_routine_before_ms)
    {
        return today_stale_scheduled_routine_followup_item(
            task, principal, workspace, updated_at, due_date,
        );
    }
    if is_terminal {
        return None;
    }
    let (priority, reason, followup_kind, status) = if task.is_blocked {
        (
            780,
            "This task is blocked and needs a next step.".to_string(),
            "blocked",
            FeedItemStatus::NeedsAction,
        )
    } else if due_date.is_some_and(|due| due < today) {
        (
            770,
            "This task is overdue.".to_string(),
            "overdue",
            FeedItemStatus::NeedsAction,
        )
    } else if due_date == Some(today) {
        (
            750,
            "This task is due today.".to_string(),
            "due_today",
            FeedItemStatus::NeedsAction,
        )
    } else if due_date == Some(tomorrow) {
        (
            690,
            "This task is due tomorrow.".to_string(),
            "due_tomorrow",
            FeedItemStatus::Info,
        )
    } else if today_task_is_stale_active(&task, updated_at, stale_before_ms) {
        (
            640,
            "This active task has not moved in 24 hours.".to_string(),
            "stale_active",
            FeedItemStatus::Info,
        )
    } else {
        return None;
    };
    let created_at = today_parse_task_timestamp_millis(&task.created_at).unwrap_or(updated_at);
    let summary = today_task_followup_summary(&task);
    let space_ids = today_task_space_ids(&task);
    let task_id = task.id;
    let source_url = Some(format!(
        "/tasks?filter=all&selected={}",
        urlencoding::encode(&task_id)
    ));
    Some(TodayItem {
        id: format!("today:followups:task:{task_id}"),
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        section: TodaySectionId::Followups,
        priority,
        title: task.title,
        summary,
        reason,
        source_kind: "task".to_string(),
        source_id: task_id.clone(),
        source_url,
        space_ids,
        thread_id: Some(task.ui_thread_id),
        task_id: Some(task_id),
        agent_id: Some(task.agent_id),
        status,
        actions: Vec::new(),
        evidence_refs: Vec::new(),
        created_at,
        updated_at,
        expires_at: due_date
            .and_then(|due| due.and_hms_opt(23, 59, 59))
            .map(|due| due.and_utc().timestamp_millis()),
        seen_at: None,
        dismissed_at: None,
        snoozed_until: None,
        metadata: json!({
            "followup_kind": followup_kind,
            "task_status": task.status,
            "priority": task.priority,
            "due_date": task.due_date,
            "is_blocked": task.is_blocked,
            "current_step_title": task.current_step_title,
            "current_substep_title": task.current_substep_title,
        }),
    })
}

fn today_followup_item_from_routine_result(
    item: FeedItem,
    principal: &str,
    workspace: &str,
) -> Option<TodayItem> {
    if item.item_type != FeedItemType::RoutineResult || !today_routine_result_needs_review(&item) {
        return None;
    }
    let source_id = today_source_id(&item);
    let source_url = today_source_url(&item);
    let space_ids = today_space_ids(&item.metadata, workspace);
    let evidence_refs = today_evidence_refs(&item.metadata);
    let metadata = json!({
        "followup_kind": "routine_failed",
        "routine_status": item.status.clone(),
        "routine_metadata": item.metadata.clone(),
    });
    Some(TodayItem {
        id: format!("today:followups:routine_result:{}", item.id),
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        section: TodaySectionId::Followups,
        priority: 760,
        title: format!("Review routine: {}", item.title),
        summary: item.summary,
        reason: "A scheduled routine failed or was cancelled.".to_string(),
        source_kind: "routine_result".to_string(),
        source_id,
        source_url,
        space_ids,
        thread_id: item.ui_thread_id,
        task_id: item.task_id,
        agent_id: item.agent_id,
        status: FeedItemStatus::NeedsAction,
        actions: Vec::new(),
        evidence_refs,
        created_at: item.created_at,
        updated_at: item.updated_at,
        expires_at: metadata_i64_any(&metadata, &["completed_at", "expires_at", "deadline_at"]),
        seen_at: None,
        dismissed_at: None,
        snoozed_until: None,
        metadata,
    })
}

fn today_stale_scheduled_routine_followup_item(
    task: TaskListItemV3,
    principal: &str,
    workspace: &str,
    updated_at: i64,
    due_date: Option<chrono::NaiveDate>,
) -> Option<TodayItem> {
    let task_id = task.id.clone();
    let created_at = today_parse_task_timestamp_millis(&task.created_at).unwrap_or(updated_at);
    let summary = today_task_followup_summary(&task).or_else(|| {
        Some("Scheduled routine has not produced a recent visible result.".to_string())
    });
    let space_ids = today_task_space_ids(&task);
    let source_url = Some(format!(
        "/tasks?filter=all&selected={}",
        urlencoding::encode(&task_id)
    ));
    Some(TodayItem {
        id: format!("today:followups:scheduled_routine:{task_id}"),
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        section: TodaySectionId::Followups,
        priority: 610,
        title: format!("Check routine: {}", task.title),
        summary,
        reason: "This scheduled routine has not moved in 24 hours.".to_string(),
        source_kind: "scheduled_routine".to_string(),
        source_id: task_id.clone(),
        source_url,
        space_ids,
        thread_id: Some(task.ui_thread_id),
        task_id: Some(task_id),
        agent_id: Some(task.agent_id),
        status: FeedItemStatus::Info,
        actions: Vec::new(),
        evidence_refs: Vec::new(),
        created_at,
        updated_at,
        expires_at: due_date
            .and_then(|due| due.and_hms_opt(23, 59, 59))
            .map(|due| due.and_utc().timestamp_millis()),
        seen_at: None,
        dismissed_at: None,
        snoozed_until: None,
        metadata: json!({
            "followup_kind": "stale_scheduled_routine",
            "task_status": task.status,
            "schedule": task.schedule,
            "due_date": task.due_date,
            "current_step_title": task.current_step_title,
            "current_substep_title": task.current_substep_title,
        }),
    })
}

fn today_dedupe_followup_items(items: Vec<TodayItem>) -> Vec<TodayItem> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for item in items {
        let key = item
            .task_id
            .as_ref()
            .map(|task_id| format!("task:{task_id}"))
            .unwrap_or_else(|| format!("item:{}", item.id));
        if seen.insert(key) {
            deduped.push(item);
        }
    }
    deduped
}

fn today_task_status_is_terminal(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "completed" | "failed" | "cancelled" | "canceled"
    )
}

fn today_task_is_stale_active(
    task: &TaskListItemV3,
    updated_at: i64,
    stale_before_ms: i64,
) -> bool {
    matches!(
        task.status.trim().to_ascii_lowercase().as_str(),
        "planning" | "running" | "paused" | "deferred"
    ) && updated_at > 0
        && updated_at <= stale_before_ms
}

fn today_task_is_stale_scheduled_routine(
    task: &TaskListItemV3,
    updated_at: i64,
    stale_before_ms: i64,
) -> bool {
    task.schedule.is_some()
        && !today_task_schedule_is_paused(task.schedule.as_ref())
        && task.active_root_execution_id.is_none()
        && updated_at > 0
        && updated_at <= stale_before_ms
        && !matches!(
            task.status.trim().to_ascii_lowercase().as_str(),
            "running" | "planning" | "failed" | "cancelled" | "canceled"
        )
}

fn today_task_schedule_is_paused(schedule: Option<&Value>) -> bool {
    schedule
        .and_then(|schedule| schedule.as_object())
        .and_then(|schedule| schedule.get("paused"))
        .and_then(|paused| paused.as_bool())
        .unwrap_or(false)
}

fn today_routine_result_needs_review(item: &FeedItem) -> bool {
    if item.status == FeedItemStatus::Failed || item.status == FeedItemStatus::NeedsAction {
        return true;
    }
    [
        metadata_string(&item.metadata, "feed_status"),
        metadata_string(&item.metadata, "execution_status"),
        metadata_string(&item.metadata, "task_status"),
        metadata_string(&item.metadata, "outcome"),
    ]
    .into_iter()
    .flatten()
    .any(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "failed" | "failure" | "cancelled" | "canceled" | "needs_action"
        )
    })
}

/// The one date every Today predicate is evaluated against.
///
/// `today=YYYY-MM-DD` is the READER's local date. The server cannot know
/// their timezone, so once the reader has told us theirs we never invent
/// one: `/today` was wrong in every positive-offset zone for part of every
/// day precisely because the date came from `Utc::now()` at request time.
///
/// **Absent falls back to `now`'s UTC date** — exactly the pre-existing
/// behaviour, so an older polling client keeps working unchanged.
///
/// **Present but unparseable is rejected**, not silently fallen back to the
/// UTC date: a silent fallback reproduces the very defect this parameter
/// closes, and does it invisibly. Only a client that sends the parameter can
/// see this error, so no older client can meet it.
fn resolve_today_reader_date(
    requested: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> std::result::Result<chrono::NaiveDate, HttpResponse> {
    let Some(requested) = requested.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(now.date_naive());
    };
    chrono::NaiveDate::parse_from_str(requested, "%Y-%m-%d").map_err(|_| {
        HttpResponse::BadRequest().json(json!({
            "error": "today_must_be_yyyy_mm_dd",
            "detail": format!("today={requested:?} is not a YYYY-MM-DD date"),
        }))
    })
}

fn today_parse_task_due_date(value: &str) -> Option<chrono::NaiveDate> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .or_else(|| {
            chrono::DateTime::parse_from_rfc3339(value)
                .ok()
                .map(|timestamp| timestamp.date_naive())
        })
}

fn today_parse_task_timestamp_millis(value: &str) -> Option<i64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(parsed) = value.parse::<i64>() {
        return Some(parsed);
    }
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.timestamp_millis())
}

fn today_json_timestamp_millis_for_keys(value: &Value, keys: &[&str]) -> Option<i64> {
    for key in keys {
        let Some(candidate) = value.as_object().and_then(|object| object.get(*key)) else {
            continue;
        };
        if let Some(timestamp) = today_json_timestamp_millis(candidate) {
            return Some(timestamp);
        }
    }
    None
}

fn today_json_timestamp_millis(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_u64().and_then(|value| i64::try_from(value).ok())),
        Value::String(value) => {
            let value = value.trim();
            if value.is_empty() {
                return None;
            }
            today_parse_task_timestamp_millis(value).or_else(|| {
                chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
                    .ok()
                    .and_then(|date| date.and_hms_opt(0, 0, 0))
                    .map(|date| date.and_utc().timestamp_millis())
            })
        },
        _ => None,
    }
}

fn today_task_followup_summary(task: &TaskListItemV3) -> Option<String> {
    task.current_step_title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            task.current_substep_title
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .or_else(|| {
            let description = task.description.trim();
            if description.is_empty() {
                None
            } else {
                Some(description)
            }
        })
        .map(|value| truncate_today_text(value, 180))
}

fn today_task_space_ids(task: &TaskListItemV3) -> Vec<String> {
    let mut ids = task
        .tags
        .iter()
        .filter_map(|tag| {
            let name = tag.name.trim();
            name.strip_prefix("space:")
                .or_else(|| name.strip_prefix("space/"))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn today_memory_learning_digest_items(
    items: Vec<FeedItem>,
    principal: &str,
    workspace: &str,
    limit: usize,
) -> Vec<TodayItem> {
    // Connections have independent evidence and dismissal identities. Folding
    // them into a tier-wide digest would dismiss every future connection too.
    let mut connections = Vec::new();
    let mut by_tier: HashMap<String, Vec<FeedItem>> = HashMap::new();
    for item in items {
        if item.item_type != FeedItemType::AgentLearning {
            continue;
        }
        if metadata_string(&item.metadata, "tier").as_deref() == Some("memory_connections")
            && metadata_string(&item.metadata, "connection_id").is_some()
        {
            let feed_id = item.id.clone();
            let mut connection = today_item_from_feed_item(
                item,
                TodaySectionId::Changed,
                620,
                "Related memories that may be useful now.".to_string(),
            );
            // Detail belongs to this evidence card even if it carries an
            // originating task/thread or another source_id in its metadata.
            connection.source_id = feed_id;
            connection.source_url = Some(format!(
                "/feed?selected_item={}",
                urlencoding::encode(&connection.source_id)
            ));
            connections.push(connection);
            continue;
        }
        let tier = metadata_string(&item.metadata, "tier")
            .map(|value| normalize_memory_token(&value))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "knowledge".to_string());
        by_tier.entry(tier).or_default().push(item);
    }

    let mut digests = by_tier
        .into_iter()
        .filter_map(|(tier, mut items)| {
            if items.is_empty() {
                return None;
            }
            items.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
            let count = items.len();
            let latest_updated_at = items
                .iter()
                .map(|item| item.updated_at)
                .max()
                .unwrap_or_default();
            let earliest_created_at = items
                .iter()
                .map(|item| item.created_at)
                .min()
                .unwrap_or(latest_updated_at);
            let label = items
                .iter()
                .find_map(|item| metadata_string(&item.metadata, "label"))
                .unwrap_or_else(|| today_memory_tier_label(&tier).to_string());
            let snippets = items
                .iter()
                .take(5)
                .filter_map(today_memory_learning_snippet)
                .collect::<Vec<_>>();
            if snippets.is_empty() {
                return None;
            }
            let more_count = count.saturating_sub(snippets.len());
            let mut summary = snippets.join("; ");
            if more_count > 0 {
                summary.push_str(&format!(
                    "; plus {more_count} more {}",
                    today_memory_tier_noun(&tier, &label, more_count)
                ));
            }
            let digest_reason = format!("{}: {}", label, truncate_today_text(&summary, 220));
            let digest_summary = truncate_today_text(&summary, 420);
            let mut space_ids = items
                .iter()
                .flat_map(|item| today_space_ids(&item.metadata, workspace))
                .collect::<Vec<_>>();
            space_ids.sort();
            space_ids.dedup();
            let learned_items = items
                .iter()
                .take(10)
                .map(|item| {
                    json!({
                        "id": item.id.clone(),
                        "title": item.title.clone(),
                        "summary": item.summary.clone(),
                        "tier": tier.clone(),
                        "space_ids": today_space_ids(&item.metadata, workspace),
                        "updated_at": item.updated_at,
                    })
                })
                .collect::<Vec<_>>();
            let source_url = items
                .first()
                .map(|item| format!("/feed?selected_item={}", urlencoding::encode(&item.id)));
            Some(TodayItem {
                id: format!("today:changed:memory_learning:{tier}"),
                principal: principal.to_string(),
                workspace: workspace.to_string(),
                section: TodaySectionId::Changed,
                priority: 620,
                title: format!(
                    "Learned {count} {}",
                    today_memory_tier_noun(&tier, &label, count)
                ),
                summary: Some(digest_summary),
                reason: digest_reason,
                source_kind: "memory_learning_digest".to_string(),
                source_id: format!("memory_learning:{tier}"),
                source_url,
                space_ids,
                thread_id: None,
                task_id: None,
                agent_id: None,
                status: FeedItemStatus::Info,
                actions: Vec::new(),
                evidence_refs: Vec::new(),
                created_at: earliest_created_at,
                updated_at: latest_updated_at,
                expires_at: None,
                seen_at: None,
                dismissed_at: None,
                snoozed_until: None,
                metadata: json!({
                    "digest_kind": "memory_learning",
                    "tier": tier,
                    "label": label,
                    "item_count": count,
                    "learned_items": learned_items,
                }),
            })
        })
        .collect::<Vec<_>>();
    digests.extend(connections);
    digests.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    digests.truncate(limit);
    digests
}

fn today_memory_tier_label(tier: &str) -> &'static str {
    match tier {
        "preferences" => "Preferences",
        "skills" => "Skills",
        "contacts" => "Contacts",
        "workflows" => "Workflows",
        "identity" => "Identity",
        "organization" => "Organization",
        "accounts" => "Accounts",
        "channels" => "Channels",
        "knowledge" => "Knowledge",
        _ => "Knowledge",
    }
}

fn today_memory_tier_noun(tier: &str, label: &str, count: usize) -> String {
    let noun = match tier {
        "preferences" => {
            if count == 1 {
                "preference"
            } else {
                "preferences"
            }
        },
        "skills" => {
            if count == 1 {
                "skill"
            } else {
                "skills"
            }
        },
        "contacts" => {
            if count == 1 {
                "contact"
            } else {
                "contacts"
            }
        },
        "workflows" => {
            if count == 1 {
                "workflow"
            } else {
                "workflows"
            }
        },
        "accounts" => {
            if count == 1 {
                "account"
            } else {
                "accounts"
            }
        },
        "channels" => {
            if count == 1 {
                "channel"
            } else {
                "channels"
            }
        },
        "identity" => "identity item",
        "organization" => {
            if count == 1 {
                "organization item"
            } else {
                "organization items"
            }
        },
        "knowledge" => {
            if count == 1 {
                "knowledge item"
            } else {
                "knowledge items"
            }
        },
        _ => "",
    };
    if !noun.is_empty() {
        return noun.to_string();
    }
    let fallback = label.trim().to_lowercase();
    if fallback.is_empty() {
        if count == 1 {
            "memory item".to_string()
        } else {
            "memory items".to_string()
        }
    } else if count == 1 && fallback.ends_with('s') {
        fallback.trim_end_matches('s').to_string()
    } else {
        fallback
    }
}

fn today_memory_learning_snippet(item: &FeedItem) -> Option<String> {
    item.summary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            let title = strip_learning_label_prefix(&item.title);
            if title.is_empty() {
                None
            } else {
                Some(title)
            }
        })
        .map(|value| truncate_today_text(value, 120))
}

fn strip_learning_label_prefix(title: &str) -> &str {
    let trimmed = title.trim();
    trimmed
        .split_once(':')
        .map(|(_, rest)| rest.trim())
        .filter(|rest| !rest.is_empty())
        .unwrap_or(trimmed)
}

fn truncate_today_text(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut output = trimmed
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    output.push_str("...");
    output
}

fn today_digest_cache_path(
    workspace_layout: &magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> std::path::PathBuf {
    workspace_layout
        .ui_root(principal, workspace)
        .join("today_digest_state.json")
}

fn today_visibility_state_path(
    workspace_layout: &magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> std::path::PathBuf {
    workspace_layout
        .ui_root(principal, workspace)
        .join("today_visibility_state.json")
}

fn attention_dismissed_state_path(
    workspace_layout: &magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> std::path::PathBuf {
    workspace_layout
        .ui_root(principal, workspace)
        .join("attention_dismissed_state.json")
}

fn load_today_digest_cache_state(
    workspace_layout: &magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
    path: &Path,
) -> Option<TodayDigestCacheState> {
    match workspace_layout.read_json_path_sync::<TodayDigestCacheState, _>(path) {
        Ok(state) => Some(state),
        Err(error) => {
            if let magician::magician_v2::artifact_v2::ArtifactV2Error::Io(io_error) = &error {
                if io_error.kind() == ErrorKind::NotFound {
                    return None;
                }
            }
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "[FEED-API] ignoring unreadable Today digest cache"
            );
            None
        },
    }
}

fn load_today_visibility_state(
    workspace_layout: &magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
    path: &Path,
) -> Option<TodayVisibilityState> {
    match workspace_layout.read_json_path_sync::<TodayVisibilityState, _>(path) {
        Ok(state) => Some(state),
        Err(error) => {
            if let magician::magician_v2::artifact_v2::ArtifactV2Error::Io(io_error) = &error {
                if io_error.kind() == ErrorKind::NotFound {
                    return None;
                }
            }
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "[FEED-API] ignoring unreadable Today visibility state"
            );
            None
        },
    }
}

/// Load the per-scope attention dismissal set. Fail-open: a missing file, or
/// any read/parse error, yields the default (empty) state so a dismissed item
/// is never *hidden by mistake* on a corrupt store — everything shows.
fn load_attention_dismissed_state(
    workspace_layout: &magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
    path: &Path,
) -> Option<AttentionDismissedState> {
    match workspace_layout.read_json_path_sync::<AttentionDismissedState, _>(path) {
        Ok(state) => Some(state),
        Err(error) => {
            if let magician::magician_v2::artifact_v2::ArtifactV2Error::Io(io_error) = &error {
                if io_error.kind() == ErrorKind::NotFound {
                    return None;
                }
            }
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "[FEED-API] ignoring unreadable attention dismissed state"
            );
            None
        },
    }
}

fn save_attention_dismissed_state(
    workspace_layout: &magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
    path: &Path,
    state: &AttentionDismissedState,
) -> std::result::Result<(), magician::magician_v2::artifact_v2::ArtifactV2Error> {
    workspace_layout.write_json_atomic_path_sync(path, state)
}

fn today_apply_visibility_state(
    items: Vec<TodayItem>,
    state: &TodayVisibilityState,
    now_ms: i64,
) -> Vec<TodayItem> {
    items
        .into_iter()
        .filter_map(|mut item| {
            let Some(record) = state.items.get(&item.id) else {
                return Some(item);
            };
            if record.dismissed_at.is_some() {
                return None;
            }
            if record
                .snoozed_until
                .is_some_and(|snoozed_until| snoozed_until > now_ms)
            {
                return None;
            }
            item.seen_at = record.seen_at;
            item.dismissed_at = record.dismissed_at;
            item.snoozed_until = record.snoozed_until;
            Some(item)
        })
        .collect()
}

fn today_sections_for_response(
    needs_you: Vec<TodayItem>,
    delivered: Vec<TodayItem>,
    changed: Vec<TodayItem>,
    active_work: Vec<TodayItem>,
    followups: Vec<TodayItem>,
    requested_section: Option<TodaySectionId>,
    requested_cursor: Option<&str>,
    requested_limit: usize,
    preview_limit: usize,
) -> (TodaySections, Option<TodaySectionPage>) {
    let (needs_you, needs_you_page) = today_section_response_items(
        TodaySectionId::NeedsYou,
        needs_you,
        requested_section,
        requested_cursor,
        requested_limit,
        preview_limit,
    );
    let (delivered, delivered_page) = today_section_response_items(
        TodaySectionId::Delivered,
        delivered,
        requested_section,
        requested_cursor,
        requested_limit,
        preview_limit,
    );
    let (changed, changed_page) = today_section_response_items(
        TodaySectionId::Changed,
        changed,
        requested_section,
        requested_cursor,
        requested_limit,
        preview_limit,
    );
    let (active_work, active_work_page) = today_section_response_items(
        TodaySectionId::ActiveWork,
        active_work,
        requested_section,
        requested_cursor,
        requested_limit,
        preview_limit,
    );
    let (followups, followups_page) = today_section_response_items(
        TodaySectionId::Followups,
        followups,
        requested_section,
        requested_cursor,
        requested_limit,
        preview_limit,
    );
    (
        TodaySections {
            needs_you,
            delivered,
            changed,
            active_work,
            followups,
            ..Default::default()
        },
        needs_you_page
            .or(delivered_page)
            .or(changed_page)
            .or(active_work_page)
            .or(followups_page),
    )
}

fn today_section_response_items(
    section: TodaySectionId,
    items: Vec<TodayItem>,
    requested_section: Option<TodaySectionId>,
    requested_cursor: Option<&str>,
    requested_limit: usize,
    preview_limit: usize,
) -> (Vec<TodayItem>, Option<TodaySectionPage>) {
    match requested_section {
        Some(requested) if requested == section => {
            match today_lane_response_page(section, &items, requested_cursor, requested_limit) {
                Ok(page) => {
                    let meta = TodaySectionPage {
                        section,
                        total: page.total,
                        limit: page.limit,
                        cursor: page.cursor.clone(),
                        next_cursor: page.next_cursor.clone(),
                        has_more: page.has_more,
                    };
                    (page.items, Some(meta))
                },
                Err(error) => {
                    tracing::warn!(
                        section = today_section_slug(&section),
                        error = %error,
                        "[FEED-API] invalid Today lane cursor; returning empty page"
                    );
                    (
                        Vec::new(),
                        Some(TodaySectionPage {
                            section,
                            total: items.len(),
                            limit: requested_limit,
                            cursor: requested_cursor.map(ToOwned::to_owned),
                            next_cursor: None,
                            has_more: false,
                        }),
                    )
                },
            }
        },
        Some(_) => (Vec::new(), None),
        None => {
            let page = today_lane_response_page(section, &items, None, preview_limit)
                .unwrap_or_else(|_| today_lane_fallback_page(section, &items, preview_limit));
            (page.items, None)
        },
    }
}

fn today_lane_response_page(
    section: TodaySectionId,
    items: &[TodayItem],
    cursor: Option<&str>,
    limit: usize,
) -> anyhow::Result<magician::magician_v2::attention_lane_facade::AttentionLanePage<TodayItem>> {
    let Some(lane) = today_attention_lane(section) else {
        return Ok(today_lane_fallback_page(section, items, limit));
    };
    // Follow-ups is the one Today section ordered by a priority band before
    // anything else (`priority DESC, updated_at ASC`, nine deliberate bands),
    // so its cursor has to carry the band. The rest are ordered by
    // `updated_at` and their cursors already name their key.
    if section == TodaySectionId::Followups {
        return list_priority_attention_lane(lane, items, cursor, limit);
    }
    list_attention_lane(lane, items, cursor, limit)
}

fn today_lane_fallback_page(
    section: TodaySectionId,
    items: &[TodayItem],
    limit: usize,
) -> magician::magician_v2::attention_lane_facade::AttentionLanePage<TodayItem> {
    let limit = magician::magician_v2::attention_lane_facade::normalize_attention_lane_limit(limit);
    let total = items.len();
    let end = limit.min(total);
    magician::magician_v2::attention_lane_facade::AttentionLanePage {
        lane: today_attention_lane(section).unwrap_or(AttentionLane::Changed),
        items: items[..end].to_vec(),
        total,
        request_hitl_total: None,
        limit,
        cursor: None,
        next_cursor: None,
        has_more: end < total,
    }
}

fn feed_attention_route_events(
    principal: &str,
    workspace: &str,
    generated_at: i64,
    requests: &[FeedItem],
    approvals: &[FeedItem],
    escalations: &[FeedItem],
    failed: &[FeedItem],
    running: &[FeedItem],
) -> Vec<AttentionRouteEvent> {
    requests
        .iter()
        .flat_map(|item| {
            feed_attention_funnel_events(
                principal,
                workspace,
                generated_at,
                item,
                AttentionLane::NeedsYou,
            )
        })
        .chain(approvals.iter().flat_map(|item| {
            feed_attention_funnel_events(
                principal,
                workspace,
                generated_at,
                item,
                AttentionLane::NeedsYou,
            )
        }))
        .chain(escalations.iter().flat_map(|item| {
            feed_attention_funnel_events(
                principal,
                workspace,
                generated_at,
                item,
                AttentionLane::NeedsYou,
            )
        }))
        .chain(failed.iter().flat_map(|item| {
            feed_attention_funnel_events(
                principal,
                workspace,
                generated_at,
                item,
                AttentionLane::Failed,
            )
        }))
        .chain(running.iter().flat_map(|item| {
            feed_attention_funnel_events(
                principal,
                workspace,
                generated_at,
                item,
                AttentionLane::ActiveWork,
            )
        }))
        .collect()
}

fn feed_attention_counts_from_totals(
    totals: &FeedAttentionTotals,
    request_hitl_total: u64,
) -> FeedAttentionCounts {
    FeedAttentionCounts {
        requests: request_hitl_total,
        approvals: totals.approvals,
        escalations: totals.escalations,
        needs_action: totals
            .requests
            .saturating_add(totals.approvals)
            .saturating_add(totals.escalations),
        failed: totals.failed,
        running: totals.running,
    }
}

fn feed_attention_funnel_events(
    principal: &str,
    workspace: &str,
    generated_at: i64,
    item: &FeedItem,
    expected_lane: AttentionLane,
) -> Vec<AttentionRouteEvent> {
    let candidate = feed_attention_candidate(item, expected_lane);
    let context = feed_attention_route_context(item, expected_lane);
    let outcome = route_attention_candidate(&candidate, &context);
    let mut events = vec![
        feed_attention_trace_event(
            principal,
            workspace,
            generated_at,
            item,
            expected_lane,
            &candidate,
            AttentionFunnelStage::Ingested,
            AttentionTraceStatus::Succeeded,
            serde_json::json!({ "trace": "feed_item_loaded" }),
        ),
        feed_attention_trace_event(
            principal,
            workspace,
            generated_at,
            item,
            expected_lane,
            &candidate,
            AttentionFunnelStage::Extracted,
            AttentionTraceStatus::Succeeded,
            serde_json::json!({ "trace": "feed_item_normalized_for_router" }),
        ),
        feed_attention_trace_event(
            principal,
            workspace,
            generated_at,
            item,
            expected_lane,
            &candidate,
            AttentionFunnelStage::Filtered,
            filtered_trace_status(&outcome),
            serde_json::json!({
                "trace": "feed_route_filter_evaluated",
                "route_outcome": route_outcome_key(&outcome),
            }),
        ),
        feed_attention_terminal_event(
            principal,
            workspace,
            generated_at,
            item,
            expected_lane,
            &candidate,
            outcome.clone(),
        ),
    ];
    if matches!(outcome, RouteOutcome::Routed { .. }) {
        events.push(feed_attention_trace_event(
            principal,
            workspace,
            generated_at,
            item,
            expected_lane,
            &candidate,
            AttentionFunnelStage::Surfaced,
            AttentionTraceStatus::Succeeded,
            serde_json::json!({ "trace": "feed_attention_visible" }),
        ));
    }
    events
}

#[cfg(test)]
fn feed_attention_route_event(
    principal: &str,
    workspace: &str,
    generated_at: i64,
    item: &FeedItem,
    expected_lane: AttentionLane,
) -> AttentionRouteEvent {
    let candidate = feed_attention_candidate(item, expected_lane);
    let context = feed_attention_route_context(item, expected_lane);
    let outcome = route_attention_candidate(&candidate, &context);
    feed_attention_terminal_event(
        principal,
        workspace,
        generated_at,
        item,
        expected_lane,
        &candidate,
        outcome,
    )
}

fn feed_attention_terminal_event(
    principal: &str,
    workspace: &str,
    generated_at: i64,
    item: &FeedItem,
    expected_lane: AttentionLane,
    candidate: &AttentionCandidate,
    outcome: RouteOutcome,
) -> AttentionRouteEvent {
    let stage = match &outcome {
        RouteOutcome::Routed { .. } => AttentionFunnelStage::Routed,
        RouteOutcome::Dropped { .. } => AttentionFunnelStage::Dropped,
        RouteOutcome::Traced { .. } => AttentionFunnelStage::Filtered,
    };
    AttentionRouteEvent {
        event_id: feed_attention_route_event_id(principal, workspace, item, expected_lane),
        scope: AttentionScope {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
        },
        source: candidate.source.clone(),
        source_family: candidate.source_family,
        candidate_key: candidate.candidate_key.clone(),
        stage,
        outcome,
        occurred_at: item.updated_at,
        created_at: generated_at,
        confidence: None,
        metadata: json!({
            "producer": "feed_attention",
            "feed_item_id": item.id.as_str(),
            "expected_lane": expected_lane.as_str(),
            "item_type": item.item_type.as_db_str(),
            "status": item.status.as_db_str(),
            "task_id": item.task_id.as_deref(),
            "thread_id": item.ui_thread_id.as_deref(),
            "agent_id": item.agent_id.as_deref(),
        }),
    }
}

fn feed_attention_trace_event(
    principal: &str,
    workspace: &str,
    generated_at: i64,
    item: &FeedItem,
    expected_lane: AttentionLane,
    candidate: &AttentionCandidate,
    stage: AttentionFunnelStage,
    status: AttentionTraceStatus,
    detail: serde_json::Value,
) -> AttentionRouteEvent {
    AttentionRouteEvent {
        event_id: feed_attention_trace_event_id(
            principal,
            workspace,
            item,
            expected_lane,
            stage,
            status,
        ),
        scope: AttentionScope {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
        },
        source: candidate.source.clone(),
        source_family: candidate.source_family,
        candidate_key: candidate.candidate_key.clone(),
        stage,
        outcome: RouteOutcome::Traced { status },
        occurred_at: item.updated_at,
        created_at: generated_at,
        confidence: None,
        metadata: json!({
            "producer": "feed_attention",
            "feed_item_id": item.id.as_str(),
            "expected_lane": expected_lane.as_str(),
            "item_type": item.item_type.as_db_str(),
            "status": item.status.as_db_str(),
            "task_id": item.task_id.as_deref(),
            "thread_id": item.ui_thread_id.as_deref(),
            "agent_id": item.agent_id.as_deref(),
            "detail": detail,
        }),
    }
}

fn feed_attention_candidate(item: &FeedItem, expected_lane: AttentionLane) -> AttentionCandidate {
    let (source_kind, source_family) = feed_attention_source_identity(&item.item_type);
    let summary = item
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| item.title.trim())
        .to_string();
    AttentionCandidate {
        candidate_key: item.id.clone(),
        source: AttentionSource {
            kind: source_kind,
            source_ref: item
                .task_id
                .as_deref()
                .or(item.ui_thread_id.as_deref())
                .unwrap_or(item.id.as_str())
                .to_string(),
            provider: None,
            account_alias: None,
        },
        source_family,
        evidence_refs: feed_attention_evidence_refs(item),
        title: item.title.clone(),
        summary,
        action: Some(feed_attention_action(item, expected_lane)),
        urgency: feed_attention_urgency(item),
        confidence: None,
        metadata: json!({
            "feed_item_id": item.id.as_str(),
            "item_type": item.item_type.as_db_str(),
            "status": item.status.as_db_str(),
            "expected_lane": expected_lane.as_str(),
        }),
    }
}

fn feed_attention_source_identity(
    item_type: &FeedItemType,
) -> (AttentionSourceKind, AttentionSourceFamily) {
    match item_type {
        FeedItemType::LearningCandidate
        | FeedItemType::LearningInsight
        | FeedItemType::AgentLearning => {
            (AttentionSourceKind::Memory, AttentionSourceFamily::Memory)
        },
        FeedItemType::AgentMessage | FeedItemType::DataDelivery | FeedItemType::RoutineResult => {
            (AttentionSourceKind::Work, AttentionSourceFamily::Work)
        },
        FeedItemType::Task | FeedItemType::Approval | FeedItemType::Escalation => {
            (AttentionSourceKind::Task, AttentionSourceFamily::Task)
        },
    }
}

fn feed_attention_evidence_refs(item: &FeedItem) -> Vec<String> {
    let mut refs = Vec::new();
    if let Some(task_id) = item.task_id.as_deref() {
        refs.push(format!("task:{task_id}"));
    }
    if let Some(thread_id) = item.ui_thread_id.as_deref() {
        refs.push(format!("thread:{thread_id}"));
    }
    if refs.is_empty() {
        refs.push(format!("feed:{}", item.id));
    }
    refs
}

fn feed_attention_action(item: &FeedItem, expected_lane: AttentionLane) -> AttentionAction {
    let kind = match (&item.item_type, &item.status, expected_lane) {
        (FeedItemType::Approval, _, _) => AttentionActionKind::Approve,
        (_, FeedItemStatus::Running, AttentionLane::ActiveWork) => AttentionActionKind::Open,
        (_, FeedItemStatus::Failed, _) => AttentionActionKind::Review,
        (FeedItemType::Escalation, _, _) => AttentionActionKind::Review,
        _ => AttentionActionKind::Review,
    };
    AttentionAction {
        kind,
        label: kind.as_str().to_string(),
        payload: json!({
            "feed_item_id": item.id.as_str(),
            "task_id": item.task_id.as_deref(),
            "thread_id": item.ui_thread_id.as_deref(),
        }),
    }
}

fn feed_attention_urgency(item: &FeedItem) -> AttentionUrgency {
    match (&item.status, &item.item_type) {
        (FeedItemStatus::Failed, _) => AttentionUrgency::Critical,
        (_, FeedItemType::Escalation) => AttentionUrgency::High,
        (_, FeedItemType::Approval) => AttentionUrgency::High,
        _ => AttentionUrgency::Normal,
    }
}

fn feed_attention_route_context(
    item: &FeedItem,
    expected_lane: AttentionLane,
) -> AttentionRouteContext {
    let mut context = AttentionRouteContext::default();
    match expected_lane {
        AttentionLane::NeedsYou => {
            if matches!(&item.item_type, FeedItemType::Approval) {
                context.owner_approval_required = true;
            } else {
                context.owner_intervention_required = true;
            }
        },
        AttentionLane::ActiveWork => {
            context.active_work_state = true;
        },
        AttentionLane::Delivered => {
            context.delivered_work = true;
        },
        AttentionLane::Changed => {
            context.observed_change = true;
        },
        AttentionLane::Failed => {
            context.failed_work_report = true;
        },
        AttentionLane::FollowUp | AttentionLane::WorthALook => {},
    }
    context.missing_safe_summary = item
        .summary
        .as_deref()
        .map(str::trim)
        .unwrap_or_else(|| item.title.trim())
        .is_empty();
    context
}

fn today_route_events(
    principal: &str,
    workspace: &str,
    generated_at: i64,
    needs_you: &[TodayItem],
    delivered: &[TodayItem],
    changed: &[TodayItem],
    active_work: &[TodayItem],
    followups: &[TodayItem],
) -> Vec<AttentionRouteEvent> {
    let mut seen_sources = HashSet::new();
    let mut events = Vec::new();
    for item in needs_you
        .iter()
        .chain(followups.iter())
        .chain(active_work.iter())
        .chain(delivered.iter())
        .chain(changed.iter())
    {
        let duplicate_of_higher_priority_lane =
            !seen_sources.insert(today_duplicate_source_key(item));
        events.extend(today_funnel_events(
            principal,
            workspace,
            generated_at,
            item,
            duplicate_of_higher_priority_lane,
        ));
    }
    events
}

fn today_funnel_events(
    principal: &str,
    workspace: &str,
    generated_at: i64,
    item: &TodayItem,
    duplicate_of_higher_priority_lane: bool,
) -> Vec<AttentionRouteEvent> {
    let candidate = today_attention_candidate(item);
    let context = today_route_context(item, duplicate_of_higher_priority_lane);
    let outcome = route_attention_candidate(&candidate, &context);
    let mut events = vec![
        today_trace_event(
            principal,
            workspace,
            generated_at,
            item,
            &candidate,
            AttentionFunnelStage::Ingested,
            AttentionTraceStatus::Succeeded,
            serde_json::json!({ "trace": "today_item_loaded" }),
        ),
        today_trace_event(
            principal,
            workspace,
            generated_at,
            item,
            &candidate,
            AttentionFunnelStage::Extracted,
            AttentionTraceStatus::Succeeded,
            serde_json::json!({ "trace": "today_item_normalized_for_router" }),
        ),
        today_trace_event(
            principal,
            workspace,
            generated_at,
            item,
            &candidate,
            AttentionFunnelStage::Filtered,
            filtered_trace_status(&outcome),
            serde_json::json!({
                "trace": "today_route_filter_evaluated",
                "route_outcome": route_outcome_key(&outcome),
            }),
        ),
        today_terminal_event(
            principal,
            workspace,
            generated_at,
            item,
            &candidate,
            outcome.clone(),
        ),
    ];
    if matches!(outcome, RouteOutcome::Routed { .. }) {
        events.push(today_trace_event(
            principal,
            workspace,
            generated_at,
            item,
            &candidate,
            AttentionFunnelStage::Surfaced,
            AttentionTraceStatus::Succeeded,
            serde_json::json!({ "trace": "today_item_visible" }),
        ));
    }
    events
}

#[cfg(test)]
fn today_route_event(
    principal: &str,
    workspace: &str,
    generated_at: i64,
    item: &TodayItem,
) -> AttentionRouteEvent {
    let candidate = today_attention_candidate(item);
    let context = today_route_context(item, false);
    let outcome = route_attention_candidate(&candidate, &context);
    today_terminal_event(
        principal,
        workspace,
        generated_at,
        item,
        &candidate,
        outcome,
    )
}

fn today_terminal_event(
    principal: &str,
    workspace: &str,
    generated_at: i64,
    item: &TodayItem,
    candidate: &AttentionCandidate,
    outcome: RouteOutcome,
) -> AttentionRouteEvent {
    let stage = match &outcome {
        RouteOutcome::Routed { .. } => AttentionFunnelStage::Routed,
        RouteOutcome::Dropped { .. } => AttentionFunnelStage::Dropped,
        RouteOutcome::Traced { .. } => AttentionFunnelStage::Filtered,
    };
    AttentionRouteEvent {
        event_id: today_route_event_id(principal, workspace, item),
        scope: AttentionScope {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
        },
        source: candidate.source.clone(),
        source_family: candidate.source_family,
        candidate_key: candidate.candidate_key.clone(),
        stage,
        outcome,
        occurred_at: item.updated_at,
        created_at: generated_at,
        confidence: None,
        metadata: json!({
            "producer": "today_projection",
            "today_item_id": item.id.as_str(),
            "section": today_section_slug(&item.section),
            "expected_lane": today_attention_lane(item.section).map(|lane| lane.as_str()),
            "source_kind": item.source_kind.as_str(),
            "source_id": item.source_id.as_str(),
            "status": item.status.as_db_str(),
            "priority": item.priority,
            "task_id": item.task_id.as_deref(),
            "thread_id": item.thread_id.as_deref(),
            "agent_id": item.agent_id.as_deref(),
        }),
    }
}

fn today_trace_event(
    principal: &str,
    workspace: &str,
    generated_at: i64,
    item: &TodayItem,
    candidate: &AttentionCandidate,
    stage: AttentionFunnelStage,
    status: AttentionTraceStatus,
    detail: serde_json::Value,
) -> AttentionRouteEvent {
    AttentionRouteEvent {
        event_id: today_trace_event_id(principal, workspace, item, stage, status),
        scope: AttentionScope {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
        },
        source: candidate.source.clone(),
        source_family: candidate.source_family,
        candidate_key: candidate.candidate_key.clone(),
        stage,
        outcome: RouteOutcome::Traced { status },
        occurred_at: item.updated_at,
        created_at: generated_at,
        confidence: None,
        metadata: json!({
            "producer": "today_projection",
            "today_item_id": item.id.as_str(),
            "section": today_section_slug(&item.section),
            "expected_lane": today_attention_lane(item.section).map(|lane| lane.as_str()),
            "source_kind": item.source_kind.as_str(),
            "source_id": item.source_id.as_str(),
            "status": item.status.as_db_str(),
            "priority": item.priority,
            "task_id": item.task_id.as_deref(),
            "thread_id": item.thread_id.as_deref(),
            "agent_id": item.agent_id.as_deref(),
            "detail": detail,
        }),
    }
}

fn today_attention_candidate(item: &TodayItem) -> AttentionCandidate {
    let (source_kind, source_family) = today_attention_source_identity(&item.source_kind);
    AttentionCandidate {
        candidate_key: item.id.clone(),
        source: AttentionSource {
            kind: source_kind,
            source_ref: if item.source_id.trim().is_empty() {
                item.id.clone()
            } else {
                item.source_id.clone()
            },
            provider: None,
            account_alias: None,
        },
        source_family,
        evidence_refs: today_attention_evidence_refs(item),
        title: item.title.clone(),
        summary: item
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(item.reason.as_str())
            .to_string(),
        action: today_attention_action(item),
        urgency: today_attention_urgency(item),
        confidence: None,
        metadata: json!({
            "today_section": today_section_slug(&item.section),
            "source_kind": item.source_kind.as_str(),
            "status": item.status.as_db_str(),
            "priority": item.priority,
        }),
    }
}

fn today_route_context(
    item: &TodayItem,
    duplicate_of_higher_priority_lane: bool,
) -> AttentionRouteContext {
    let mut context = AttentionRouteContext::default();
    context.duplicate_of_higher_priority_lane = duplicate_of_higher_priority_lane;
    match item.section {
        TodaySectionId::NeedsYou => {
            if item.source_kind == "approval" {
                context.owner_approval_required = true;
            } else {
                context.owner_intervention_required = true;
            }
        },
        TodaySectionId::ActiveWork => {
            context.active_work_state = true;
        },
        TodaySectionId::Delivered => {
            context.delivered_work = true;
        },
        TodaySectionId::Changed => {
            context.observed_change = true;
        },
        TodaySectionId::Followups | TodaySectionId::Spaces => {},
    }
    context.missing_safe_summary = item
        .summary
        .as_deref()
        .map(str::trim)
        .unwrap_or_else(|| item.reason.trim())
        .is_empty();
    context
}

fn today_duplicate_source_key(item: &TodayItem) -> String {
    if let Some(task_id) = item
        .task_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        return format!("task:{task_id}");
    }
    if !item.source_id.trim().is_empty() {
        return format!("{}:{}", item.source_kind.as_str(), item.source_id.as_str());
    }
    if let Some(thread_id) = item
        .thread_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        return format!("thread:{thread_id}");
    }
    item.id.clone()
}

fn today_attention_action(item: &TodayItem) -> Option<AttentionAction> {
    match item.section {
        TodaySectionId::Followups => Some(AttentionAction {
            kind: AttentionActionKind::FollowUp,
            label: "follow up".to_string(),
            payload: json!({
                "source_kind": item.source_kind.as_str(),
                "source_id": item.source_id.as_str(),
                "status": item.status.as_db_str(),
            }),
        }),
        TodaySectionId::NeedsYou if item.source_kind == "approval" => Some(AttentionAction {
            kind: AttentionActionKind::Approve,
            label: "approve".to_string(),
            payload: json!({
                "source_kind": item.source_kind.as_str(),
                "source_id": item.source_id.as_str(),
            }),
        }),
        TodaySectionId::NeedsYou => Some(AttentionAction {
            kind: AttentionActionKind::Review,
            label: "review".to_string(),
            payload: json!({
                "source_kind": item.source_kind.as_str(),
                "source_id": item.source_id.as_str(),
            }),
        }),
        _ => None,
    }
}

fn today_attention_evidence_refs(item: &TodayItem) -> Vec<String> {
    if !item.evidence_refs.is_empty() {
        return item
            .evidence_refs
            .iter()
            .enumerate()
            .map(|(index, value)| {
                value
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| format!("{}:evidence:{index}", item.id))
            })
            .collect();
    }
    if let Some(task_id) = item.task_id.as_deref() {
        return vec![format!("task:{task_id}")];
    }
    if let Some(thread_id) = item.thread_id.as_deref() {
        return vec![format!("thread:{thread_id}")];
    }
    vec![format!("{}:{}", item.source_kind, item.source_id)]
}

fn today_attention_source_identity(
    source_kind: &str,
) -> (AttentionSourceKind, AttentionSourceFamily) {
    match source_kind {
        "memory_learning"
        | "memory_learning_digest"
        | "learning_candidate"
        | "learning_insight" => (AttentionSourceKind::Memory, AttentionSourceFamily::Memory),
        "task" | "scheduled_routine" | "monitor_update" => {
            (AttentionSourceKind::Task, AttentionSourceFamily::Task)
        },
        "meeting_action" => (AttentionSourceKind::Meeting, AttentionSourceFamily::Meeting),
        "calendar" | "calendar_event" => (
            AttentionSourceKind::Calendar,
            AttentionSourceFamily::Calendar,
        ),
        "screen_observation" => (
            AttentionSourceKind::ScreenObservation,
            AttentionSourceFamily::ScreenObservation,
        ),
        "tab_observation" => (
            AttentionSourceKind::TabObservation,
            AttentionSourceFamily::TabObservation,
        ),
        "published_surface" | "routine_result" | "approval" | "escalation" | "agent_message" => {
            (AttentionSourceKind::Work, AttentionSourceFamily::Work)
        },
        _ => (AttentionSourceKind::Other, AttentionSourceFamily::Other),
    }
}

fn today_attention_lane(section: TodaySectionId) -> Option<AttentionLane> {
    match section {
        TodaySectionId::NeedsYou => Some(AttentionLane::NeedsYou),
        TodaySectionId::Delivered => Some(AttentionLane::Delivered),
        TodaySectionId::Changed => Some(AttentionLane::Changed),
        TodaySectionId::ActiveWork => Some(AttentionLane::ActiveWork),
        TodaySectionId::Followups => Some(AttentionLane::FollowUp),
        TodaySectionId::Spaces => None,
    }
}

fn today_attention_urgency(item: &TodayItem) -> AttentionUrgency {
    if item.section == TodaySectionId::NeedsYou || item.priority >= 850 {
        AttentionUrgency::High
    } else if item.priority >= 650 {
        AttentionUrgency::Normal
    } else {
        AttentionUrgency::Low
    }
}

fn today_route_event_id(principal: &str, workspace: &str, item: &TodayItem) -> String {
    let raw = format!(
        "today_route\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}",
        principal,
        workspace,
        today_section_slug(&item.section),
        item.id.as_str(),
        item.status.as_db_str(),
        item.source_kind.as_str(),
        item.source_id.as_str(),
    );
    format!("today-route:{}", blake3::hash(raw.as_bytes()).to_hex())
}

fn today_trace_event_id(
    principal: &str,
    workspace: &str,
    item: &TodayItem,
    stage: AttentionFunnelStage,
    status: AttentionTraceStatus,
) -> String {
    let raw = format!(
        "today_trace\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}",
        principal,
        workspace,
        today_section_slug(&item.section),
        item.id.as_str(),
        item.status.as_db_str(),
        item.source_kind.as_str(),
        stage.as_str(),
        status.as_str(),
    );
    format!("today-trace:{}", blake3::hash(raw.as_bytes()).to_hex())
}

fn feed_attention_route_event_id(
    principal: &str,
    workspace: &str,
    item: &FeedItem,
    lane: AttentionLane,
) -> String {
    let raw = format!(
        "feed_attention_route\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}",
        principal,
        workspace,
        lane.as_str(),
        item.id.as_str(),
        item.item_type.as_db_str(),
        item.status.as_db_str(),
        item.updated_at,
    );
    format!(
        "feed-attention-route:{}",
        blake3::hash(raw.as_bytes()).to_hex()
    )
}

fn feed_attention_trace_event_id(
    principal: &str,
    workspace: &str,
    item: &FeedItem,
    lane: AttentionLane,
    stage: AttentionFunnelStage,
    status: AttentionTraceStatus,
) -> String {
    let raw = format!(
        "feed_attention_trace\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}",
        principal,
        workspace,
        lane.as_str(),
        item.id.as_str(),
        item.item_type.as_db_str(),
        item.status.as_db_str(),
        item.updated_at,
        stage.as_str(),
        status.as_str(),
    );
    format!(
        "feed-attention-trace:{}",
        blake3::hash(raw.as_bytes()).to_hex()
    )
}

fn filtered_trace_status(outcome: &RouteOutcome) -> AttentionTraceStatus {
    match outcome {
        RouteOutcome::Routed { .. } => AttentionTraceStatus::Succeeded,
        RouteOutcome::Dropped { .. } | RouteOutcome::Traced { .. } => AttentionTraceStatus::Skipped,
    }
}

fn route_outcome_key(outcome: &RouteOutcome) -> String {
    match outcome {
        RouteOutcome::Routed {
            lane,
            reason,
            priority,
        } => format!(
            "routed:{}:{}:{}",
            lane.as_str(),
            reason.as_str(),
            priority.as_str()
        ),
        RouteOutcome::Dropped { reason } => format!("dropped:{}", reason.as_str()),
        RouteOutcome::Traced { status } => format!("traced:{}", status.as_str()),
    }
}

fn today_digest_source_fingerprint(
    needs_you: &[TodayItem],
    delivered: &[TodayItem],
    changed: &[TodayItem],
    active_work: &[TodayItem],
) -> String {
    let mut sources = Vec::new();
    for item in needs_you
        .iter()
        .chain(delivered.iter())
        .chain(changed.iter())
        .chain(active_work.iter())
    {
        sources.push(json!({
            "section": today_section_slug(&item.section),
            "source_kind": item.source_kind.clone(),
            "source_id": item.source_id.clone(),
            "status": item.status.clone(),
            "title": item.title.clone(),
            "summary": item.summary.clone(),
            "source_url": item.source_url.clone(),
            "space_ids": item.space_ids.clone(),
            "updated_at": item.updated_at,
        }));
    }
    sources.sort_by(|left, right| left.to_string().cmp(&right.to_string()));
    let bytes = serde_json::to_vec(&sources).unwrap_or_default();
    blake3::hash(&bytes).to_hex().to_string()
}

fn today_changed_digest(
    generated_at: i64,
    since: Option<i64>,
    needs_you: &[TodayItem],
    delivered: &[TodayItem],
    changed: &[TodayItem],
    active_work: &[TodayItem],
) -> TodayChangedDigest {
    let mut candidates = Vec::new();
    candidates.extend(
        changed
            .iter()
            .filter(|item| today_item_is_after_since(item, since))
            .filter_map(|item| today_digest_bullet_from_item(item, "Memory", 900)),
    );
    candidates.extend(
        delivered
            .iter()
            .filter(|item| today_item_is_after_since(item, since))
            .filter_map(|item| today_digest_bullet_from_item(item, "Delivered", 800)),
    );
    candidates.extend(
        needs_you
            .iter()
            .filter(|item| today_item_is_after_since(item, since))
            .filter(|item| item.status == FeedItemStatus::Failed)
            .filter_map(|item| today_digest_bullet_from_item(item, "Attention", 750)),
    );
    candidates.extend(
        active_work
            .iter()
            .filter(|item| today_item_is_after_since(item, since))
            .filter_map(|item| today_digest_bullet_from_item(item, "Active", 650)),
    );

    let mut seen = HashSet::new();
    let mut bullets = candidates
        .into_iter()
        .filter(|(_, bullet)| seen.insert(format!("{}:{}", bullet.source_kind, bullet.source_id)))
        .collect::<Vec<_>>();
    bullets.sort_by(|(left_priority, left), (right_priority, right)| {
        right_priority
            .cmp(left_priority)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
    });
    let bullets = bullets
        .into_iter()
        .map(|(_, bullet)| bullet)
        .collect::<Vec<_>>();
    let total = bullets.len();
    TodayChangedDigest {
        generated_at,
        since,
        total,
        limit: total,
        offset: 0,
        bullets,
    }
}

fn today_digest_page(
    digest: TodayChangedDigest,
    requested_offset: usize,
    requested_limit: usize,
) -> TodayChangedDigest {
    let total = if digest.total == 0 && !digest.bullets.is_empty() {
        digest.bullets.len()
    } else {
        digest.total
    };
    let offset = requested_offset.min(total);
    let limit = requested_limit.max(1);
    let bullets = digest
        .bullets
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    TodayChangedDigest {
        generated_at: digest.generated_at,
        since: digest.since,
        total,
        limit,
        offset,
        bullets,
    }
}

fn today_item_is_after_since(item: &TodayItem, since: Option<i64>) -> bool {
    since.map(|since| item.updated_at > since).unwrap_or(true)
}

fn today_digest_bullet_from_item(
    item: &TodayItem,
    prefix: &str,
    priority: i64,
) -> Option<(i64, TodayDigestBullet)> {
    let text = today_digest_bullet_text(item, prefix)?;
    Some((
        priority,
        TodayDigestBullet {
            id: format!(
                "today:digest:{}:{}",
                today_section_slug(&item.section),
                item.source_id
            ),
            text,
            source_kind: item.source_kind.clone(),
            source_id: item.source_id.clone(),
            source_url: item.source_url.clone(),
            space_ids: item.space_ids.clone(),
            updated_at: item.updated_at,
        },
    ))
}

fn today_digest_bullet_text(item: &TodayItem, prefix: &str) -> Option<String> {
    let title = item.title.trim();
    let summary = item.summary.as_deref().map(str::trim).unwrap_or_default();
    let body = if !summary.is_empty() {
        format!("{title}: {summary}")
    } else {
        title.to_string()
    };
    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    Some(format!("{prefix}: {}", truncate_today_text(body, 180)))
}

fn today_needs_you_priority(item: &FeedItem) -> i64 {
    match item.status {
        FeedItemStatus::Failed => 980,
        FeedItemStatus::NeedsAction => match item.item_type {
            FeedItemType::Approval => 960,
            FeedItemType::Escalation => 950,
            FeedItemType::Task => 940,
            _ => 900,
        },
        _ => 850,
    }
}

fn today_needs_you_reason(item: &FeedItem) -> String {
    if item.status == FeedItemStatus::Failed {
        return "This failed and may need a decision.".to_string();
    }
    match item.item_type {
        FeedItemType::Approval => "This is waiting for approval.".to_string(),
        FeedItemType::Escalation => "This is blocked or escalated.".to_string(),
        FeedItemType::Task => "This needs your input.".to_string(),
        _ => "This needs your attention.".to_string(),
    }
}

fn today_item_from_feed_item(
    item: FeedItem,
    section: TodaySectionId,
    priority: i64,
    reason: String,
) -> TodayItem {
    let source_kind = today_source_kind(&item).to_string();
    let source_id = today_source_id(&item);
    let source_url = today_source_url(&item);
    let space_ids = today_space_ids(&item.metadata, &item.workspace);
    let evidence_refs = today_evidence_refs(&item.metadata);
    TodayItem {
        id: format!("today:{}:{}", today_section_slug(&section), item.id),
        principal: item.principal,
        workspace: item.workspace,
        section,
        priority,
        title: item.title,
        summary: item.summary,
        reason,
        source_kind,
        source_id,
        source_url,
        space_ids,
        thread_id: item.ui_thread_id,
        task_id: item.task_id,
        agent_id: item.agent_id,
        status: item.status,
        actions: item.actions,
        evidence_refs,
        created_at: item.created_at,
        updated_at: item.updated_at,
        expires_at: metadata_i64_any(&item.metadata, &["expires_at", "deadline_at", "stale_at"]),
        seen_at: None,
        dismissed_at: None,
        snoozed_until: None,
        metadata: item.metadata,
    }
}

fn today_section_slug(section: &TodaySectionId) -> &'static str {
    match section {
        TodaySectionId::NeedsYou => "needs_you",
        TodaySectionId::Delivered => "delivered",
        TodaySectionId::Changed => "changed",
        TodaySectionId::ActiveWork => "active_work",
        TodaySectionId::Spaces => "spaces",
        TodaySectionId::Followups => "followups",
    }
}

fn today_source_kind(item: &FeedItem) -> &'static str {
    match item.item_type {
        FeedItemType::Task => "task",
        FeedItemType::Approval => "approval",
        FeedItemType::AgentMessage => "agent_message",
        FeedItemType::DataDelivery => "published_surface",
        FeedItemType::RoutineResult => "routine_result",
        FeedItemType::Escalation => "escalation",
        FeedItemType::LearningCandidate => "learning_candidate",
        FeedItemType::LearningInsight => "learning_insight",
        FeedItemType::AgentLearning => "memory_learning",
    }
}

fn today_source_id(item: &FeedItem) -> String {
    metadata_string(&item.metadata, "surface_id")
        .or_else(|| metadata_string(&item.metadata, "source_id"))
        .or_else(|| item.task_id.clone())
        .or_else(|| item.ui_thread_id.clone())
        .unwrap_or_else(|| item.id.clone())
}

fn today_source_url(item: &FeedItem) -> Option<String> {
    match item.item_type {
        FeedItemType::DataDelivery | FeedItemType::RoutineResult => {
            if let Some(surface_id) = metadata_string(&item.metadata, "surface_id") {
                return Some(format!("/briefing/{}", urlencoding::encode(&surface_id)));
            }
            if let Some(route) = metadata_string(&item.metadata, "route") {
                if route.starts_with('/') {
                    return Some(route);
                }
            }
            if item.item_type == FeedItemType::RoutineResult {
                Some("/briefing".to_string())
            } else {
                None
            }
        },
        FeedItemType::Task | FeedItemType::Escalation => item.task_id.as_ref().map(|task_id| {
            format!(
                "/tasks?filter=all&selected={}",
                urlencoding::encode(task_id)
            )
        }),
        FeedItemType::Approval => Some(format!(
            "/attention?attention=1&attention_item={}",
            urlencoding::encode(&item.id)
        )),
        FeedItemType::AgentLearning
        | FeedItemType::LearningCandidate
        | FeedItemType::LearningInsight => Some("/memory".to_string()),
        FeedItemType::AgentMessage => item.ui_thread_id.as_ref().map(|thread_id| {
            format!(
                "/t/{}?selected_item={}",
                urlencoding::encode(thread_id),
                urlencoding::encode(&item.id)
            )
        }),
    }
}

fn today_space_ids(metadata: &Value, fallback_workspace: &str) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(space_id) = metadata_string(metadata, "space_id") {
        ids.push(space_id);
    }
    for key in ["space_ids", "spaces"] {
        let Some(values) = metadata.as_object().and_then(|object| object.get(key)) else {
            continue;
        };
        match values {
            Value::Array(values) => {
                ids.extend(
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(ToOwned::to_owned),
                );
            },
            Value::String(value) => {
                let value = value.trim();
                if !value.is_empty() {
                    ids.push(value.to_string());
                }
            },
            _ => {},
        }
    }
    if ids.is_empty() && !fallback_workspace.is_empty() {
        ids.push(fallback_workspace.to_string());
    }
    ids.sort();
    ids.dedup();
    ids
}

fn today_evidence_refs(metadata: &Value) -> Vec<Value> {
    metadata
        .as_object()
        .and_then(|object| object.get("evidence_refs"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn metadata_i64_any(metadata: &Value, keys: &[&str]) -> Option<i64> {
    for key in keys {
        let Some(value) = metadata.as_object().and_then(|object| object.get(*key)) else {
            continue;
        };
        match value {
            Value::Number(number) => {
                if let Some(value) = number.as_i64() {
                    return Some(value);
                }
            },
            Value::String(value) => {
                if let Ok(parsed) = value.trim().parse::<i64>() {
                    return Some(parsed);
                }
            },
            _ => {},
        }
    }
    None
}

fn today_headline(counts: &TodayCounts) -> String {
    let mut parts = Vec::new();
    if counts.needs_you == 0 {
        parts.push("Nothing needs you right now".to_string());
    } else {
        parts.push(format!(
            "{} {} need you",
            counts.needs_you,
            plural(counts.needs_you, "thing", "things")
        ));
    }
    if counts.delivered > 0 {
        parts.push(format!(
            "{} {} ready",
            counts.delivered,
            plural(counts.delivered, "delivery", "deliveries")
        ));
    }
    if counts.active_work > 0 {
        parts.push(format!(
            "{} active {}",
            counts.active_work,
            plural(counts.active_work, "work item", "work items")
        ));
    }
    format!("{}.", parts.join(". "))
}

fn plural(count: u64, singular: &'static str, plural: &'static str) -> &'static str {
    if count == 1 {
        singular
    } else {
        plural
    }
}

pub async fn list_feed_handler(
    api: web::Data<Arc<FeedApi>>,
    req: HttpRequest,
    query: web::Query<FeedListQuery>,
) -> Result<HttpResponse> {
    api.list_feed(&req, query).await
}

pub async fn feed_counts_handler(
    api: web::Data<Arc<FeedApi>>,
    req: HttpRequest,
    query: web::Query<FeedCountsQuery>,
) -> Result<HttpResponse> {
    api.feed_counts(&req, query).await
}

pub async fn feed_attention_handler(
    api: web::Data<Arc<FeedApi>>,
    req: HttpRequest,
    query: web::Query<FeedAttentionQuery>,
) -> Result<HttpResponse> {
    api.attention(&req, query).await
}

pub async fn feed_attention_item_handler(
    api: web::Data<Arc<FeedApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<FeedAttentionItemQuery>,
) -> Result<HttpResponse> {
    api.attention_item(&req, path, query).await
}

fn feed_attention_item_response(item_id: &str, item: Option<FeedItem>) -> HttpResponse {
    match item {
        Some(item) => HttpResponse::Ok().json(item),
        None => HttpResponse::NotFound().json(json!({
            "error": "attention_item_not_found",
            "item_id": item_id,
        })),
    }
}

pub async fn today_handler(
    api: web::Data<Arc<FeedApi>>,
    req: HttpRequest,
    query: web::Query<TodayQuery>,
) -> Result<HttpResponse> {
    api.today(&req, query).await
}

pub async fn today_item_action_handler(
    api: web::Data<Arc<FeedApi>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<TodayActionQuery>,
    broadcaster: Option<web::Data<Arc<RuntimeTransportBroadcaster>>>,
) -> Result<HttpResponse> {
    api.execute_today_item_action(
        &req,
        path,
        query,
        broadcaster.as_ref().map(|data| data.get_ref()),
    )
    .await
}

pub async fn today_visibility_list_handler(
    api: web::Data<Arc<FeedApi>>,
    req: HttpRequest,
    query: web::Query<TodayVisibilityQuery>,
) -> Result<HttpResponse> {
    api.list_today_visibility(&req, query).await
}

pub async fn today_visibility_handler(
    api: web::Data<Arc<FeedApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<TodayVisibilityQuery>,
    body: web::Json<TodayVisibilityRequest>,
) -> Result<HttpResponse> {
    api.update_today_visibility(&req, path, query, body).await
}

pub async fn feed_attention_dismiss_handler(
    api: web::Data<Arc<FeedApi>>,
    req: HttpRequest,
    query: web::Query<AttentionDismissQuery>,
    body: web::Json<AttentionDismissRequest>,
) -> Result<HttpResponse> {
    api.dismiss_attention_item(&req, query, body).await
}

pub async fn feed_attention_undismiss_handler(
    api: web::Data<Arc<FeedApi>>,
    req: HttpRequest,
    query: web::Query<AttentionDismissQuery>,
    body: web::Json<AttentionDismissRequest>,
) -> Result<HttpResponse> {
    // Force `dismissed = false` regardless of the body flag so this route is
    // an unambiguous "undismiss", the mirror of the `dismiss` route.
    let mut body = body.into_inner();
    body.dismissed = false;
    api.dismiss_attention_item(&req, query, web::Json(body))
        .await
}

pub async fn feed_delete_item_handler(
    api: web::Data<Arc<FeedApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<FeedCountsQuery>,
) -> Result<HttpResponse> {
    api.delete_item(&req, path, query).await
}

pub async fn feed_clear_handler(
    api: web::Data<Arc<FeedApi>>,
    req: HttpRequest,
    query: web::Query<FeedListQuery>,
) -> Result<HttpResponse> {
    api.clear_items(&req, query).await
}

pub async fn feed_purge_orphans_handler(
    api: web::Data<Arc<FeedApi>>,
    req: HttpRequest,
    query: web::Query<FeedCountsQuery>,
) -> Result<HttpResponse> {
    api.purge_orphans(&req, query).await
}

pub async fn feed_confirm_learning_candidate_handler(
    api: web::Data<Arc<FeedApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<FeedLearningActionRequest>,
) -> Result<HttpResponse> {
    api.confirm_learning_candidate(&req, path, body).await
}

pub async fn feed_edit_confirm_learning_candidate_handler(
    api: web::Data<Arc<FeedApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<FeedLearningActionRequest>,
) -> Result<HttpResponse> {
    api.edit_confirm_learning_candidate(&req, path, body).await
}

pub async fn feed_archive_learning_candidate_handler(
    api: web::Data<Arc<FeedApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<FeedLearningActionRequest>,
) -> Result<HttpResponse> {
    api.archive_learning_candidate(&req, path, body).await
}

pub async fn feed_archive_learning_insight_handler(
    api: web::Data<Arc<FeedApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<FeedInsightActionRequest>,
) -> Result<HttpResponse> {
    api.archive_learning_insight(&req, path, body).await
}

pub async fn feed_save_learning_insight_handler(
    api: web::Data<Arc<FeedApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<FeedInsightActionRequest>,
) -> Result<HttpResponse> {
    api.save_learning_insight_to_memory(&req, path, body).await
}

pub async fn feed_create_learning_insight_task_handler(
    api: web::Data<Arc<FeedApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<FeedInsightActionRequest>,
) -> Result<HttpResponse> {
    api.create_learning_insight_follow_up_task(&req, path, body)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    use magician::magician_v2::feed::FeedAttentionPageQuery;
    use magician::magician_v2::learning::{
        LearningCapabilityEvolutionApplicationMode, LearningCapabilityEvolutionProposalPatch,
    };
    use magician::magician_v2::test_support::build_test_artifact_v2_service;
    use serde_json::json;

    fn feed_item(id: &str, item_type: FeedItemType, task_id: Option<&str>) -> FeedItem {
        FeedItem {
            id: id.to_string(),
            principal: "alpha".to_string(),
            workspace: "prod".to_string(),
            item_type,
            task_id: task_id.map(str::to_string),
            ui_thread_id: Some("general".to_string()),
            agent_id: Some("atlas".to_string()),
            title: id.to_string(),
            summary: None,
            status: FeedItemStatus::Done,
            created_at: 1,
            updated_at: 1,
            actions: Vec::new(),
            metadata: json!({}),
        }
    }

    fn today_fixture_item(id: &str, section: TodaySectionId) -> TodayItem {
        today_item_from_feed_item(
            feed_item(id, FeedItemType::Task, Some(id)),
            section,
            100,
            "fixture".to_string(),
        )
    }

    #[test]
    fn approval_today_source_url_preserves_exact_attention_item() {
        let item = feed_item("approval/id with spaces", FeedItemType::Approval, None);
        assert_eq!(
            today_source_url(&item).as_deref(),
            Some("/attention?attention=1&attention_item=approval%2Fid%20with%20spaces")
        );
    }

    #[test]
    fn exact_attention_response_returns_item_or_not_found() {
        let item = feed_item("approval-1", FeedItemType::Approval, None);
        assert_eq!(
            feed_attention_item_response("approval-1", Some(item)).status(),
            actix_web::http::StatusCode::OK
        );
        assert_eq!(
            feed_attention_item_response("missing", None).status(),
            actix_web::http::StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn exact_attention_alias_matcher_accepts_canonical_metadata_ids() {
        let mut item = feed_item("raw-feed-id", FeedItemType::Approval, None);
        item.metadata = json!({
            "correlation_id": "correlation-1",
            "pause_state_id": "pause-1",
            "approval_id": "approval-1",
            "request_id": "request-1",
        });

        for item_id in [
            "raw-feed-id",
            "correlation-1",
            "pause-1",
            "approval-1",
            "request-1",
        ] {
            assert!(feed_item_matches_attention_alias(&item, item_id));
        }
        assert!(!feed_item_matches_attention_alias(
            &item,
            "other-scope-item"
        ));
    }

    #[actix_web::test]
    async fn exact_attention_route_decodes_percent_encoded_slashes_and_spaces() {
        let tmp = tempfile::tempdir().expect("temporary exact-attention workspace");
        let v3_service = build_test_artifact_v2_service(tmp.path());
        let feed_store = FeedStore::open(&tmp.path().join("feed")).expect("feed store");
        let mut item = feed_item("approval/id with spaces", FeedItemType::Escalation, None);
        item.status = FeedItemStatus::NeedsAction;
        feed_store
            .upsert_item(item)
            .await
            .expect("store encoded-id item");
        let api = Arc::new(FeedApi::new(feed_store, v3_service));
        let app = actix_web::test::init_service(
            actix_web::App::new().app_data(web::Data::new(api)).route(
                "/feed/attention/{item_id:.*}",
                web::get().to(feed_attention_item_handler),
            ),
        )
        .await;
        let request = actix_web::test::TestRequest::get()
            .uri("/feed/attention/approval%2Fid%20with%20spaces?workspace=prod")
            .insert_header(("X-Principal", "alpha"))
            .insert_header(("X-Workspace", "prod"))
            .to_request();
        let response = actix_web::test::call_service(&app, request).await;

        assert_eq!(response.status(), actix_web::http::StatusCode::OK);
        let body: FeedItem = actix_web::test::read_body_json(response).await;
        assert_eq!(body.id, "approval/id with spaces");
    }

    #[test]
    fn stored_canonical_hitl_rows_require_authoritative_pending_validation() {
        let ordinary = feed_item(
            "ordinary-escalation",
            FeedItemType::Escalation,
            Some("task-1"),
        );

        for attention_kind in [
            "hitl.requested",
            "input.requested",
            "waiting_for_confirmation",
            "max_iterations_reached",
            "user_request.pending",
        ] {
            let mut canonical = feed_item("stored-hitl", FeedItemType::Escalation, Some("task-1"));
            canonical.metadata = json!({
                "attention_kind": attention_kind,
                "correlation_id": "pause-1",
            });
            assert!(
                feed_item_is_canonical_hitl(&canonical),
                "{attention_kind} must not bypass live pending-state validation"
            );
        }
        let mut shaped = feed_item("stored-shaped-hitl", FeedItemType::Approval, Some("task-1"));
        shaped.metadata = json!({
            "input_type": "approval",
            "approval_id": "approval-1",
        });
        assert!(feed_item_is_canonical_hitl(&shaped));
        assert!(!feed_item_is_canonical_hitl(&ordinary));
    }

    #[actix_web::test]
    async fn exact_attention_handler_validates_stored_and_registry_hitl_authoritatively() {
        let tmp = tempfile::tempdir().expect("temporary feed workspace");
        let v3_service = build_test_artifact_v2_service(tmp.path());
        let feed_store = FeedStore::open(&tmp.path().join("feed")).expect("feed store");
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let api = FeedApi::new(feed_store.clone(), v3_service)
            .with_pending_hitl_broadcaster(Arc::clone(&broadcaster));
        let request = actix_web::test::TestRequest::default()
            .insert_header(("X-Principal", "alpha"))
            .insert_header(("X-Workspace", "prod"))
            .to_http_request();
        let query = || {
            web::Query(FeedAttentionItemQuery {
                workspace: Some("prod".to_string()),
            })
        };

        let mut stale = feed_item("stale-hitl", FeedItemType::Escalation, None);
        stale.status = FeedItemStatus::NeedsAction;
        stale.metadata = json!({
            "attention_kind": "waiting_for_confirmation",
            "correlation_id": "stale-pause",
        });
        feed_store
            .upsert_item(stale)
            .await
            .expect("store stale row");
        let stale_response = api
            .attention_item(
                &request,
                web::Path::from("stale-pause".to_string()),
                query(),
            )
            .await
            .expect("stale exact response");
        assert_eq!(
            stale_response.status(),
            actix_web::http::StatusCode::NOT_FOUND,
            "a stored HITL row without current V3/registry state must not reopen"
        );

        let mut ordinary = feed_item("ordinary-escalation", FeedItemType::Escalation, None);
        ordinary.status = FeedItemStatus::NeedsAction;
        feed_store
            .upsert_item(ordinary)
            .await
            .expect("store ordinary row");
        let ordinary_response = api
            .attention_item(
                &request,
                web::Path::from("ordinary-escalation".to_string()),
                query(),
            )
            .await
            .expect("ordinary exact response");
        assert_eq!(ordinary_response.status(), actix_web::http::StatusCode::OK);

        broadcaster.emit(RuntimeTransportEvent::HitlRequested {
            correlation_id: "bot_auth:alpha:prod:gmail".to_string(),
            source: "bot_auth".to_string(),
            input_type: "choice".to_string(),
            prompt: "Connect Gmail".to_string(),
            hint: None,
            input_schema: None,
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 100,
        });
        let taskless_response = api
            .attention_item(
                &request,
                web::Path::from("bot_auth:alpha:prod:gmail".to_string()),
                query(),
            )
            .await
            .expect("taskless exact response");
        assert_eq!(taskless_response.status(), actix_web::http::StatusCode::OK);

        broadcaster.emit(RuntimeTransportEvent::HitlRequested {
            correlation_id: "task-backed-stale".to_string(),
            source: "agentic".to_string(),
            input_type: "text".to_string(),
            prompt: "Need input".to_string(),
            hint: None,
            input_schema: None,
            task_id: Some("missing-task".to_string()),
            execution_id: Some("missing-execution".to_string()),
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 101,
        });
        let task_backed_response = api
            .attention_item(
                &request,
                web::Path::from("task-backed-stale".to_string()),
                query(),
            )
            .await
            .expect("task-backed exact response");
        assert_eq!(
            task_backed_response.status(),
            actix_web::http::StatusCode::NOT_FOUND,
            "registry-only task-backed rows must not override V3"
        );
    }

    #[actix_web::test]
    async fn exact_attention_handler_restores_taskless_lifecycle_across_restarts() {
        let temp_root = std::fs::canonicalize(std::env::temp_dir())
            .expect("canonical temporary root for hardened lifecycle authority");
        let tmp = tempfile::tempdir_in(temp_root).expect("temporary exact-attention workspace");
        let v3_service = build_test_artifact_v2_service(tmp.path());
        let feed_store = FeedStore::open(&tmp.path().join("feed")).expect("feed store");
        let lifecycle_workspace = ArtifactV2Workspace::new(tmp.path().join("lifecycle"));
        lifecycle_workspace
            .ensure_root_sync()
            .expect("lifecycle workspace root");
        let correlation_id = "bot_auth:alpha:prod:gmail";

        let first = RuntimeTransportBroadcaster::new(16)
            .with_hitl_lifecycle_persistence(lifecycle_workspace.clone());
        first.emit(RuntimeTransportEvent::HitlRequested {
            correlation_id: correlation_id.to_string(),
            source: "bot_auth".to_string(),
            input_type: "choice".to_string(),
            prompt: "Connect Gmail".to_string(),
            hint: None,
            input_schema: None,
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 100,
        });
        drop(first);

        let restarted = Arc::new(
            RuntimeTransportBroadcaster::new(16)
                .with_hitl_lifecycle_persistence(lifecycle_workspace.clone()),
        );
        let restarted_api = FeedApi::new(feed_store.clone(), Arc::clone(&v3_service))
            .with_pending_hitl_broadcaster(Arc::clone(&restarted));
        let request = actix_web::test::TestRequest::default()
            .insert_header(("X-Principal", "alpha"))
            .insert_header(("X-Workspace", "prod"))
            .to_http_request();
        let query = || {
            web::Query(FeedAttentionItemQuery {
                workspace: Some("prod".to_string()),
            })
        };
        let restored_response = restarted_api
            .attention_item(
                &request,
                web::Path::from(correlation_id.to_string()),
                query(),
            )
            .await
            .expect("restored taskless exact response");
        assert_eq!(restored_response.status(), actix_web::http::StatusCode::OK);

        restarted.emit(RuntimeTransportEvent::HitlResolved {
            correlation_id: correlation_id.to_string(),
            source: "bot_auth".to_string(),
            outcome: "responded".to_string(),
            decision: None,
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 101,
        });
        drop(restarted_api);
        drop(restarted);

        let after_resolution = Arc::new(
            RuntimeTransportBroadcaster::new(16)
                .with_hitl_lifecycle_persistence(lifecycle_workspace),
        );
        let resolved_api =
            FeedApi::new(feed_store, v3_service).with_pending_hitl_broadcaster(after_resolution);
        let resolved_response = resolved_api
            .attention_item(
                &request,
                web::Path::from(correlation_id.to_string()),
                query(),
            )
            .await
            .expect("resolved taskless exact response");
        assert_eq!(
            resolved_response.status(),
            actix_web::http::StatusCode::NOT_FOUND
        );
    }

    #[actix_web::test]
    async fn exact_attention_handler_prefers_a_known_alias_during_degraded_recovery() {
        let tmp = tempfile::tempdir().expect("temporary exact-attention workspace");
        let v3_service = build_test_artifact_v2_service(tmp.path());
        let feed_store = FeedStore::open(&tmp.path().join("feed")).expect("feed store");
        let lifecycle_workspace = ArtifactV2Workspace::new(tmp.path().join("lifecycle"));
        lifecycle_workspace
            .ensure_root_sync()
            .expect("lifecycle workspace root");
        let correlation_id = "bot_auth:alpha:prod:gmail";
        let request_event = RuntimeTransportEvent::HitlRequested {
            correlation_id: correlation_id.to_string(),
            source: "bot_auth".to_string(),
            input_type: "choice".to_string(),
            prompt: "Connect Gmail".to_string(),
            hint: None,
            input_schema: None,
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 100,
        };
        let valid = serde_json::to_string(&request_event).expect("serialize lifecycle request");
        std::fs::write(
            lifecycle_workspace
                .base_root()
                .join("pending_hitl_lifecycle.jsonl"),
            format!("{valid}\n{{not-json}}\n"),
        )
        .expect("write degraded lifecycle fixture");

        let broadcaster = Arc::new(
            RuntimeTransportBroadcaster::new(16)
                .with_hitl_lifecycle_persistence(lifecycle_workspace),
        );
        let mut stored = feed_item("projection-row-id", FeedItemType::Approval, None);
        stored.status = FeedItemStatus::NeedsAction;
        stored.metadata = json!({
            "attention_kind": "hitl.requested",
            "correlation_id": correlation_id,
        });
        feed_store
            .upsert_item(stored)
            .await
            .expect("store aliased HITL projection");
        let api = FeedApi::new(feed_store, v3_service)
            .with_pending_hitl_broadcaster(Arc::clone(&broadcaster));
        let request = actix_web::test::TestRequest::default()
            .insert_header(("X-Principal", "alpha"))
            .insert_header(("X-Workspace", "prod"))
            .to_http_request();
        let response = api
            .attention_item(
                &request,
                web::Path::from("projection-row-id".to_string()),
                web::Query(FeedAttentionItemQuery {
                    workspace: Some("prod".to_string()),
                }),
            )
            .await
            .expect("known-alias exact response");

        assert_eq!(response.status(), actix_web::http::StatusCode::OK);
    }

    #[test]
    fn taskless_runtime_hitl_maps_to_a_renderable_exact_attention_item() {
        let item = runtime_pending_hitl_feed_item(RuntimeTransportEvent::HitlRequested {
            correlation_id: "bot_auth:alpha:prod:gmail".to_string(),
            source: "bot_auth".to_string(),
            input_type: "choice".to_string(),
            prompt: "Connect Gmail".to_string(),
            hint: Some("Sign in to continue".to_string()),
            input_schema: Some(json!({
                "type": "choice",
                "options": [{"id": "open_auth_flow", "label": "Connect"}]
            })),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 123,
        })
        .expect("scoped taskless request should map");

        assert_eq!(item.id, "runtime:hitl:bot_auth:alpha:prod:gmail");
        assert_eq!(item.principal, "alpha");
        assert_eq!(item.workspace, "prod");
        assert_eq!(item.status, FeedItemStatus::NeedsAction);
        assert_eq!(item.task_id, None);
        assert_eq!(item.metadata["source"], "bot_auth");
        assert_eq!(item.metadata["input_type"], "choice");
        assert_eq!(item.metadata["correlation_id"], "bot_auth:alpha:prod:gmail");
        assert!(feed_item_matches_attention_alias(
            &item,
            "bot_auth:alpha:prod:gmail"
        ));
    }

    fn post_promotion_monitor_record(
        id: &str,
        promotion_id: &str,
    ) -> LearningCapabilityEvolutionPostPromotionMonitorRecord {
        LearningCapabilityEvolutionPostPromotionMonitorRecord {
            id: id.to_string(),
            scope: LearningScope::new("alpha".to_string(), "prod".to_string()),
            promotion_id: promotion_id.to_string(),
            candidate_id: "candidate".to_string(),
            proposal_id: "proposal".to_string(),
            validation_id: "validation".to_string(),
            implementation_id: None,
            application_id: None,
            capability_id: Some("skill:foo".to_string()),
            status: LearningCapabilityEvolutionPostPromotionMonitorStatus::RegressionDetected,
            summary: "promotion regressed".to_string(),
            skill_names: vec!["foo".to_string()],
            before_invocation_count: 2,
            after_invocation_count: 3,
            before_success_count: 2,
            after_success_count: 0,
            before_failure_count: 0,
            after_failure_count: 3,
            before_success_rate: Some(1.0),
            after_success_rate: Some(0.0),
            same_failure_recurrence_count: 1,
            new_failure_classes: vec!["tool_misuse".to_string()],
            user_negative_feedback_count: 0,
            rollback_recommendation_id: None,
            follow_up_candidate_id: None,
            evidence_refs: Vec::new(),
            payload: json!({}),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn rollback_recommendation_record(
        id: &str,
    ) -> LearningCapabilityEvolutionRollbackRecommendationRecord {
        LearningCapabilityEvolutionRollbackRecommendationRecord {
            id: id.to_string(),
            scope: LearningScope::new("alpha".to_string(), "prod".to_string()),
            candidate_id: format!("candidate_{id}"),
            proposal_id: format!("proposal_{id}"),
            validation_id: Some(format!("validation_{id}")),
            implementation_id: Some(format!("implementation_{id}")),
            application_id: format!("application_{id}"),
            promotion_id: None,
            capability_id: Some("skill:foo".to_string()),
            status: LearningCapabilityEvolutionRollbackRecommendationStatus::Recommended,
            trigger_kind: "catalog_refresh_failed".to_string(),
            severity: "high".to_string(),
            actor: "tester".to_string(),
            summary: "catalog failed".to_string(),
            rollback_files: vec!["skills/foo/SKILL.md".to_string()],
            evidence_refs: Vec::new(),
            payload: json!({}),
            created_at: chrono::Utc::now(),
        }
    }

    fn temp_learning_store() -> (tempfile::TempDir, LearningStore) {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(
            temp_dir.path(),
        );
        (temp_dir, LearningStore::new(workspace))
    }

    fn assert_second_attention_item_reachable(items: Vec<FeedItem>, lane: AttentionLane) {
        assert_eq!(items.len(), 2);
        let first = list_attention_lane(lane, &items, None, 1).expect("first attention page");
        assert!(first.has_more);
        let second = list_attention_lane(lane, &items, first.next_cursor.as_deref(), 1)
            .expect("second attention page");
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.items[0].id, items[1].id);
        assert!(!second.has_more);
    }

    fn candidate_request(state: LearningCandidateState) -> CreateLearningCandidateRequest {
        CreateLearningCandidateRequest {
            principal: None,
            workspace: None,
            candidate_type: LearningCandidateType::SkillUpdate,
            state,
            title: "follow-up".to_string(),
            summary: "follow-up candidate".to_string(),
            rationale: "regression follow-up".to_string(),
            proposed_change: json!({}),
            proposed_target: Some("skill:foo".to_string()),
            confidence: None,
            source_agent_id: None,
            source_task_id: None,
            source_execution_id: None,
            source_chat_session_id: None,
            event_refs: Vec::new(),
            evidence_refs: Vec::new(),
            risk_level: LearningRiskLevel::Low,
            review_required: false,
            review_reason: None,
            review_policy: json!({}),
            promotion_target: Some("skill:foo".to_string()),
            promotion_policy: json!({}),
        }
    }

    fn evolution_proposal(
        id: &str,
        candidate_id: &str,
        status: LearningCapabilityEvolutionProposalStatus,
        proposed_files: Vec<String>,
    ) -> LearningCapabilityEvolutionProposal {
        let now = chrono::Utc::now();
        let patches = proposed_files
            .iter()
            .map(|path| LearningCapabilityEvolutionProposalPatch {
                path: path.clone(),
                operation: "replace".to_string(),
                summary: format!("Update {path}"),
                diff: Some("diff --git a/file b/file".to_string()),
                metadata: json!({"reviewed_patch_content": true}),
            })
            .collect::<Vec<_>>();
        LearningCapabilityEvolutionProposal {
            id: id.to_string(),
            scope: LearningScope::new("alpha".to_string(), "prod".to_string()),
            candidate_id: candidate_id.to_string(),
            backlog_id: format!("backlog_{candidate_id}"),
            status,
            title: format!("Improve {candidate_id}"),
            summary: format!("Review Skill Evolution change for {candidate_id}."),
            capability_id: Some("skill:foo".to_string()),
            proposed_fix_type: Some("skill_update".to_string()),
            proposed_files,
            change_plan: json!({}),
            patches,
            eval_plan: Some(json!({"commands": ["cargo test -p magician --lib"]})),
            validation_plan: Some(json!({"commands": ["cargo check -p magician"]})),
            promotion_gate: Some(json!({"human_approval": "required"})),
            generated_by: "test".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    fn evolution_validation(
        candidate_id: &str,
        proposal_id: &str,
        status: LearningCapabilityEvolutionValidationStatus,
    ) -> LearningCapabilityEvolutionValidationReport {
        let passed = status == LearningCapabilityEvolutionValidationStatus::Passed;
        LearningCapabilityEvolutionValidationReport {
            id: format!("validation_{candidate_id}"),
            scope: LearningScope::new("alpha".to_string(), "prod".to_string()),
            candidate_id: candidate_id.to_string(),
            proposal_id: proposal_id.to_string(),
            status,
            capability_id: Some("skill:foo".to_string()),
            runner: "tester".to_string(),
            summary: "validation passed".to_string(),
            commands: vec!["cargo check -p magician".to_string()],
            evidence_refs: Vec::new(),
            metrics: json!({"passed": passed}),
            payload: json!({}),
            created_at: chrono::Utc::now(),
        }
    }

    fn evolution_implementation(
        candidate_id: &str,
        proposal_id: &str,
        validation_id: &str,
        patches: Vec<LearningCapabilityEvolutionProposalPatch>,
    ) -> LearningCapabilityEvolutionImplementationRecord {
        LearningCapabilityEvolutionImplementationRecord {
            id: format!("implementation_{candidate_id}"),
            scope: LearningScope::new("alpha".to_string(), "prod".to_string()),
            candidate_id: candidate_id.to_string(),
            proposal_id: proposal_id.to_string(),
            validation_id: validation_id.to_string(),
            capability_id: Some("skill:foo".to_string()),
            actor: "tester".to_string(),
            summary: "implementation ready".to_string(),
            applied_files: patches.iter().map(|patch| patch.path.clone()).collect(),
            patches,
            evidence_refs: Vec::new(),
            payload: json!({}),
            created_at: chrono::Utc::now(),
        }
    }

    fn evolution_application(
        candidate_id: &str,
        proposal_id: &str,
        validation_id: &str,
        implementation_id: &str,
    ) -> LearningCapabilityEvolutionApplicationRecord {
        LearningCapabilityEvolutionApplicationRecord {
            id: format!("application_{candidate_id}"),
            scope: LearningScope::new("alpha".to_string(), "prod".to_string()),
            candidate_id: candidate_id.to_string(),
            proposal_id: proposal_id.to_string(),
            validation_id: validation_id.to_string(),
            implementation_id: implementation_id.to_string(),
            capability_id: Some("skill:foo".to_string()),
            actor: "tester".to_string(),
            summary: "application applied".to_string(),
            mode: LearningCapabilityEvolutionApplicationMode::Apply,
            status: LearningCapabilityEvolutionApplicationStatus::Applied,
            changed_files: Vec::new(),
            evidence_refs: Vec::new(),
            payload: json!({}),
            created_at: chrono::Utc::now(),
        }
    }

    fn evolution_promotion(
        candidate_id: &str,
        proposal_id: &str,
        validation_id: &str,
        implementation_id: Option<String>,
        application_id: Option<String>,
    ) -> LearningCapabilityEvolutionPromotionRecord {
        LearningCapabilityEvolutionPromotionRecord {
            id: format!("promotion_{candidate_id}"),
            scope: LearningScope::new("alpha".to_string(), "prod".to_string()),
            candidate_id: candidate_id.to_string(),
            proposal_id: proposal_id.to_string(),
            validation_id: validation_id.to_string(),
            implementation_id,
            application_id,
            capability_id: Some("skill:foo".to_string()),
            actor: "tester".to_string(),
            summary: "promotion recorded".to_string(),
            applied_files: Vec::new(),
            evidence_refs: Vec::new(),
            payload: json!({}),
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn retain_existing_tasks_filters_task_rows_but_keeps_delivery_provenance() {
        let valid_task_ids = Some(HashSet::from(["live-task".to_string()]));
        let items = vec![
            feed_item("v3:task:live", FeedItemType::Task, Some("live-task")),
            feed_item("v3:task:stale", FeedItemType::Task, Some("stale-task")),
            feed_item(
                "data_delivery:surface-1",
                FeedItemType::DataDelivery,
                Some("stale-task"),
            ),
            feed_item(
                "routine:daily",
                FeedItemType::RoutineResult,
                Some("stale-task"),
            ),
        ];

        let retained = retain_existing_tasks(items, &valid_task_ids, usize::MAX);

        let retained_ids = retained
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            retained_ids,
            vec!["v3:task:live", "data_delivery:surface-1", "routine:daily"]
        );
    }

    #[test]
    fn today_sections_for_response_pages_requested_section_only() {
        let delivered = (0..5)
            .map(|index| {
                today_fixture_item(&format!("delivered-{index}"), TodaySectionId::Delivered)
            })
            .collect::<Vec<_>>();
        let changed = vec![today_fixture_item("changed-0", TodaySectionId::Changed)];
        let delivered_cursor =
            magician::magician_v2::attention_lane_facade::encode_attention_lane_cursor(
                delivered[1].updated_at,
                &delivered[1].id,
            );

        let (sections, page) = today_sections_for_response(
            Vec::new(),
            delivered,
            changed,
            Vec::new(),
            Vec::new(),
            Some(TodaySectionId::Delivered),
            Some(&delivered_cursor),
            2,
            8,
        );

        assert!(page.is_some());
        assert_eq!(
            sections
                .delivered
                .iter()
                .map(|item| item.source_id.as_str())
                .collect::<Vec<_>>(),
            vec!["delivered-2", "delivered-3"]
        );
        assert!(sections.changed.is_empty());
        assert!(sections.followups.is_empty());
    }

    #[test]
    fn today_sections_for_response_keeps_legacy_preview_without_requested_section() {
        let followups = (0..4)
            .map(|index| {
                today_fixture_item(&format!("followup-{index}"), TodaySectionId::Followups)
            })
            .collect::<Vec<_>>();

        let sections = today_sections_for_response(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            followups,
            None,
            None,
            99,
            2,
        );

        let (sections, page) = sections;
        assert!(page.is_none());
        assert_eq!(
            sections
                .followups
                .iter()
                .map(|item| item.source_id.as_str())
                .collect::<Vec<_>>(),
            vec!["followup-0", "followup-1"]
        );
    }

    #[test]
    fn feed_attention_route_event_maps_approval_to_needs_you() {
        let mut item = feed_item("approval-1", FeedItemType::Approval, Some("task-approval"));
        item.status = FeedItemStatus::NeedsAction;
        item.summary = Some("Approval is waiting.".to_string());

        let event =
            feed_attention_route_event("alpha", "prod", 123, &item, AttentionLane::NeedsYou);

        assert_eq!(event.source.kind, AttentionSourceKind::Task);
        assert_eq!(event.source_family, AttentionSourceFamily::Task);
        assert!(matches!(
            event.outcome,
            RouteOutcome::Routed {
                lane: AttentionLane::NeedsYou,
                ..
            }
        ));
    }

    #[test]
    fn feed_attention_route_event_maps_running_task_to_active_work() {
        let mut item = feed_item("running-1", FeedItemType::Task, Some("task-running"));
        item.status = FeedItemStatus::Running;
        item.summary = Some("Task is currently running.".to_string());

        let event =
            feed_attention_route_event("alpha", "prod", 123, &item, AttentionLane::ActiveWork);

        assert_eq!(event.source.kind, AttentionSourceKind::Task);
        assert_eq!(event.source_family, AttentionSourceFamily::Task);
        assert!(matches!(
            event.outcome,
            RouteOutcome::Routed {
                lane: AttentionLane::ActiveWork,
                ..
            }
        ));
    }

    #[test]
    fn feed_attention_route_event_maps_failed_task_to_failed_lane() {
        let mut item = feed_item("failed-1", FeedItemType::Task, Some("task-failed"));
        item.status = FeedItemStatus::Failed;
        item.summary = Some("Task execution failed.".to_string());

        let event = feed_attention_route_event("alpha", "prod", 123, &item, AttentionLane::Failed);

        assert_eq!(event.source.kind, AttentionSourceKind::Task);
        assert_eq!(event.source_family, AttentionSourceFamily::Task);
        assert!(matches!(
            event.outcome,
            RouteOutcome::Routed {
                lane: AttentionLane::Failed,
                ..
            }
        ));
    }

    #[test]
    fn feed_attention_route_event_id_is_stable_across_page_loads() {
        let mut item = feed_item(
            "approval-stable",
            FeedItemType::Approval,
            Some("task-approval"),
        );
        item.status = FeedItemStatus::NeedsAction;
        item.summary = Some("Approval is waiting.".to_string());

        let first =
            feed_attention_route_event("alpha", "prod", 123, &item, AttentionLane::NeedsYou);
        let second =
            feed_attention_route_event("alpha", "prod", 456, &item, AttentionLane::NeedsYou);

        assert_eq!(first.event_id, second.event_id);
        assert_ne!(first.created_at, second.created_at);
    }

    #[test]
    fn today_route_event_maps_active_work_to_active_work_lane() {
        let mut item = today_fixture_item("active-1", TodaySectionId::ActiveWork);
        item.status = FeedItemStatus::Running;
        item.priority = 500;
        item.source_kind = "task".to_string();

        let event = today_route_event("alpha", "prod", 123, &item);

        assert_eq!(event.source.kind, AttentionSourceKind::Task);
        assert_eq!(event.source_family, AttentionSourceFamily::Task);
        assert!(matches!(
            event.outcome,
            RouteOutcome::Routed {
                lane: AttentionLane::ActiveWork,
                ..
            }
        ));
    }

    #[test]
    fn today_route_event_maps_memory_learning_to_changed_lane() {
        let mut item = today_fixture_item("changed-1", TodaySectionId::Changed);
        item.status = FeedItemStatus::Info;
        item.source_kind = "memory_learning_digest".to_string();
        item.source_id = "memory_learning:preferences".to_string();

        let event = today_route_event("alpha", "prod", 123, &item);

        assert_eq!(event.source.kind, AttentionSourceKind::Memory);
        assert_eq!(event.source_family, AttentionSourceFamily::Memory);
        assert!(matches!(
            event.outcome,
            RouteOutcome::Routed {
                lane: AttentionLane::Changed,
                ..
            }
        ));
    }

    // ── Recurring Monitors Phase 3 — Today Changed projection ───────────

    fn monitor_update_fixture() -> MonitorUpdateDetailV1 {
        serde_json::from_str(include_str!(
            "../../magician/tests/fixtures/monitors/monitor_update_detail_v1.json"
        ))
        .expect("canonical update fixture decodes")
    }

    #[test]
    fn monitor_update_projects_into_changed_with_deep_link_metadata_and_open_action() {
        let update = monitor_update_fixture();
        assert!(update_projects_to_changed(&update));
        let item = today_item_from_monitor_update(update.clone(), "anonymous", "default");

        assert_eq!(item.section, TodaySectionId::Changed);
        assert_eq!(
            item.id,
            format!("today:changed:monitor_update:{}", update.update_id)
        );
        assert_eq!(item.title, update.headline);
        assert_eq!(item.source_kind, MONITOR_UPDATE_SOURCE_KIND);
        assert_eq!(item.task_id.as_deref(), Some("task_monitor_fixture_001"));
        // Deep-link payload: monitor task + exact update + fingerprint.
        assert_eq!(item.metadata["monitor_task_id"], "task_monitor_fixture_001");
        assert_eq!(item.metadata["update_id"], update.update_id.as_str());
        assert_eq!(item.metadata["change_fingerprint"], "chg_71d3f6a2c4e89b10");
        assert_eq!(item.metadata["execution_id"], "exec_fixture_0003");
        // The registered MonitorActionAdapter supplies the open-task action.
        // Both URLs target the CANONICAL monitor route the web actually
        // reads (`taskRoutes.ts monitorsTaskRoute`):
        // /tasks?type=monitors&selected=…&update=… — never /tasks?task=….
        let expected_url = format!(
            "/tasks?type=monitors&selected=task_monitor_fixture_001&update={}",
            update.update_id
        );
        assert_eq!(item.actions.len(), 1);
        assert_eq!(item.actions[0].id, "open_task");
        assert_eq!(item.actions[0].payload["url"], json!(expected_url));
        assert_eq!(item.source_url.as_deref(), Some(expected_url.as_str()));
    }

    #[test]
    fn quiet_and_suppressed_monitor_updates_never_become_changed_items() {
        // Unchanged every_run receipt → stays out of Changed (§3).
        let mut quiet = monitor_update_fixture();
        quiet.status = MonitorRunStatus::Unchanged;
        quiet.change_fingerprint = None;
        assert!(!update_projects_to_changed(&quiet));

        // Material change under `never` policy → recorded but not surfaced.
        let mut suppressed = monitor_update_fixture();
        suppressed.notification.emitted = false;
        assert!(!update_projects_to_changed(&suppressed));

        // Degraded runs go to Attention, never Changed.
        let mut degraded = monitor_update_fixture();
        degraded.status = MonitorRunStatus::Degraded;
        assert!(!update_projects_to_changed(&degraded));
    }

    #[test]
    fn dismissing_a_monitor_changed_card_suppresses_only_that_fingerprint() {
        let update = monitor_update_fixture();
        let dismissed_item = today_item_from_monitor_update(update.clone(), "anonymous", "default");

        // A LATER, different material change gets a different deterministic
        // update id (dedupe key embeds the change fingerprint) → new card id.
        let mut later = monitor_update_fixture();
        later.update_id = "mu_00000000000000ff".to_string();
        later.change_fingerprint = Some("chg_00000000000000ff".to_string());
        later.notification.dedupe_key =
            "anonymous/default:task_monitor_fixture_001:2:chg_00000000000000ff:today_changed"
                .to_string();
        let later_item = today_item_from_monitor_update(later, "anonymous", "default");
        assert_ne!(later_item.id, dismissed_item.id);

        // Visibility state: the dismissed fingerprint stays hidden, the new
        // fingerprint surfaces — the monitor is never suppressed forever.
        let mut state = TodayVisibilityState::default();
        state.items.insert(
            dismissed_item.id.clone(),
            TodayVisibilityRecord {
                dismissed_at: Some(1),
                updated_at: 1,
                ..Default::default()
            },
        );
        let visible = today_apply_visibility_state(
            vec![dismissed_item.clone(), later_item.clone()],
            &state,
            2,
        );
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, later_item.id);
    }

    #[test]
    fn today_route_event_maps_monitor_update_to_changed_lane() {
        let item = today_item_from_monitor_update(monitor_update_fixture(), "alpha", "prod");
        let event = today_route_event("alpha", "prod", 123, &item);
        assert_eq!(event.source.kind, AttentionSourceKind::Task);
        assert_eq!(event.source_family, AttentionSourceFamily::Task);
        assert!(matches!(
            event.outcome,
            RouteOutcome::Routed {
                lane: AttentionLane::Changed,
                ..
            }
        ));
    }

    #[test]
    fn today_route_event_id_is_stable_across_page_loads() {
        let item = today_fixture_item("delivered-1", TodaySectionId::Delivered);

        let first = today_route_event("alpha", "prod", 123, &item);
        let second = today_route_event("alpha", "prod", 456, &item);

        assert_eq!(first.event_id, second.event_id);
        assert_ne!(first.created_at, second.created_at);
    }

    #[test]
    fn today_route_events_mark_lower_priority_duplicate_sources() {
        let mut needs_you = today_fixture_item("shared-needs", TodaySectionId::NeedsYou);
        needs_you.source_kind = "task".to_string();
        needs_you.source_id = "task-shared".to_string();
        needs_you.task_id = Some("task-shared".to_string());

        let mut followup = today_fixture_item("shared-followup", TodaySectionId::Followups);
        followup.source_kind = "task".to_string();
        followup.source_id = "task-shared".to_string();
        followup.task_id = Some("task-shared".to_string());

        let events = today_route_events(
            "alpha",
            "prod",
            123,
            &[needs_you],
            &[],
            &[],
            &[],
            &[followup.clone()],
        );

        let duplicate_event = events
            .iter()
            .find(|event| {
                event.candidate_key == followup.id
                    && matches!(event.stage, AttentionFunnelStage::Dropped)
            })
            .expect("duplicate lower-priority item should emit a drop event");
        assert!(matches!(
            duplicate_event.outcome,
            RouteOutcome::Dropped {
                reason:
                    magician::magician_v2::attention_funnel::DropReason::DuplicateOfHigherPriorityLane
            }
        ));
    }

    #[test]
    fn task_status_str_is_terminal_classifies_every_task_status() {
        // Terminal: a finished/cancelled delegate's prompt must become
        // dismissible (dropped from the attention-eligible set).
        for terminal in ["completed", "failed", "cancelled"] {
            assert!(
                task_status_str_is_terminal(terminal),
                "`{terminal}` must be terminal"
            );
        }
        // Non-terminal: a LIVE prompt — especially `paused`, the canonical
        // HITL state — must stay eligible so the user can still answer it.
        // This is the regression we most care about (never drop a live prompt).
        for live in [
            "pending", "planning", "ready", "running", "paused", "deferred",
        ] {
            assert!(
                !task_status_str_is_terminal(live),
                "`{live}` must NOT be terminal — its prompt has to stay visible"
            );
        }
        // Normalization: legacy single-l `canceled`, casing, and surrounding
        // whitespace all resolve to terminal (mirrors today_task_status_is_terminal).
        for variant in ["canceled", "Cancelled", "FAILED", " completed "] {
            assert!(
                task_status_str_is_terminal(variant),
                "`{variant}` should normalize to terminal"
            );
        }
        // Unknown/empty → not terminal (fail-open: keep rather than drop).
        assert!(!task_status_str_is_terminal(""));
        assert!(!task_status_str_is_terminal("archived"));
    }

    #[test]
    fn active_work_rejects_stale_terminal_or_pending_task_metadata() {
        for task_status in ["failed", "completed", "cancelled", "pending"] {
            let mut item = feed_item(
                &format!("stale-{task_status}"),
                FeedItemType::Task,
                Some("task-1"),
            );
            item.status = FeedItemStatus::Running;
            item.metadata = json!({
                "task_status": task_status,
                "active_root_execution_id": null,
            });

            assert!(
                !today_item_is_active_work(&item),
                "task_status={task_status} must not surface in Active work"
            );
            assert!(
                !feed_item_has_current_active_task_state(&item),
                "task_status={task_status} must not surface in attention running"
            );
        }
    }

    #[test]
    fn active_work_keeps_current_running_task_metadata_and_legacy_rows() {
        for task_status in ["running", "planning", "waiting_for_children", "paused"] {
            let mut running = feed_item(
                &format!("{task_status}-current"),
                FeedItemType::Task,
                Some("task-1"),
            );
            running.status = FeedItemStatus::Running;
            running.metadata = json!({
                "task_status": task_status,
                "active_root_execution_id": "exec-1",
            });
            assert!(
                today_item_is_active_work(&running),
                "task_status={task_status} should surface in Active work"
            );
            assert!(
                feed_item_has_current_active_task_state(&running),
                "task_status={task_status} should surface in attention running"
            );
        }

        let mut legacy = feed_item("running-legacy", FeedItemType::Task, Some("task-2"));
        legacy.status = FeedItemStatus::Running;
        assert!(today_item_is_active_work(&legacy));
        assert!(feed_item_has_current_active_task_state(&legacy));
    }

    #[test]
    fn attention_eligibility_keeps_live_prompts_and_drops_terminal_or_deleted() {
        // Reproduce the set `valid_attention_task_ids` builds: ALL user tasks,
        // plus only NON-terminal internal tasks. (id, status)
        let user_tasks = [("user-running", "running"), ("user-failed", "failed")];
        let internal_tasks = [
            ("int-paused", "paused"),       // live HITL — keep
            ("int-completed", "completed"), // terminal — drop
            ("int-canceled", "canceled"),   // terminal (legacy spelling) — drop
        ];
        let mut valid: HashSet<String> = user_tasks.iter().map(|(id, _)| id.to_string()).collect();
        valid.extend(
            internal_tasks
                .iter()
                .filter(|(_, status)| !task_status_str_is_terminal(status))
                .map(|(id, _)| id.to_string()),
        );
        let valid = Some(valid);

        // V3-attention items are FeedItemType::Task carrying the task's own id
        // (see build_attention_feed_item); a chat-delegate HITL carries the
        // internal task's id.
        let items = vec![
            feed_item("att:user-running", FeedItemType::Task, Some("user-running")),
            feed_item("att:user-failed", FeedItemType::Task, Some("user-failed")),
            feed_item("att:int-paused", FeedItemType::Task, Some("int-paused")),
            feed_item(
                "att:int-completed",
                FeedItemType::Task,
                Some("int-completed"),
            ),
            feed_item("att:int-canceled", FeedItemType::Task, Some("int-canceled")),
            // Backing task deleted → absent from both lists → not in `valid`.
            feed_item("att:deleted", FeedItemType::Task, Some("ghost-task")),
        ];

        let kept = retain_existing_tasks(items, &valid, usize::MAX)
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();

        // User-facing prompts survive at any status; the live internal HITL
        // survives; terminal-internal and deleted-backing prompts are dropped.
        assert_eq!(
            kept,
            vec!["att:user-running", "att:user-failed", "att:int-paused"]
        );
    }

    #[test]
    fn memory_connections_keep_individual_today_dismissal_and_detail_targets() {
        let items = ["a", "b"]
            .into_iter()
            .map(|id| {
                let mut item = feed_item(
                    &format!("memory_connection:{id}"),
                    FeedItemType::AgentLearning,
                    Some("originating-task"),
                );
                item.principal = "p".into();
                item.workspace = "w".into();
                item.title = format!("Connection {id}");
                item.metadata = json!({"connection_id":id,"tier":"memory_connections","source_id":"original-source"});
                item
            })
            .collect();
        let items = today_memory_learning_digest_items(items, "p", "w", 10);
        assert_eq!(items.len(), 2);
        for (id, url) in [
            ("a", "/feed?selected_item=memory_connection%3Aa"),
            ("b", "/feed?selected_item=memory_connection%3Ab"),
        ] {
            let item = items
                .iter()
                .find(|item| item.id == format!("today:changed:memory_connection:{id}"))
                .unwrap();
            assert_eq!(item.source_id, format!("memory_connection:{id}"));
            assert_eq!(item.source_url.as_deref(), Some(url));
            assert_eq!(item.thread_id.as_deref(), Some("general"));
        }
    }

    #[test]
    fn today_digest_groups_agent_learning_by_memory_tier() {
        let mut first = feed_item(
            "agent_learning:preferences:a",
            FeedItemType::AgentLearning,
            None,
        );
        first.status = FeedItemStatus::Info;
        first.title = "Preferences: Use concise replies".to_string();
        first.summary = Some("Use concise replies in writing help.".to_string());
        first.created_at = 10;
        first.updated_at = 30;
        first.metadata = json!({
            "card_kind": "agent_learning",
            "tier": "preferences",
            "label": "Preferences",
        });

        let mut second = feed_item(
            "agent_learning:preferences:b",
            FeedItemType::AgentLearning,
            None,
        );
        second.status = FeedItemStatus::Info;
        second.title = "Preferences: Keep examples practical".to_string();
        second.summary = Some("Keep examples practical before adding style.".to_string());
        second.created_at = 20;
        second.updated_at = 40;
        second.metadata = json!({
            "card_kind": "agent_learning",
            "tier": "preferences",
            "label": "Preferences",
        });

        let mut raw_learning = feed_item(
            "learning_insight:eval:1",
            FeedItemType::LearningInsight,
            None,
        );
        raw_learning.status = FeedItemStatus::Info;

        let digests = today_memory_learning_digest_items(
            vec![first, raw_learning, second],
            "alpha",
            "prod",
            8,
        );

        assert_eq!(digests.len(), 1);
        let digest = &digests[0];
        assert_eq!(digest.section, TodaySectionId::Changed);
        assert_eq!(digest.source_kind, "memory_learning_digest");
        assert_eq!(
            digest.source_url.as_deref(),
            Some("/feed?selected_item=agent_learning%3Apreferences%3Ab")
        );
        assert_eq!(digest.status, FeedItemStatus::Info);
        assert_eq!(digest.title, "Learned 2 preferences");
        assert!(digest.reason.contains("Preferences:"));
        assert!(digest
            .reason
            .contains("Use concise replies in writing help."));
        assert!(!digest.reason.contains("Durable memory was distilled"));
        let summary = digest.summary.as_deref().unwrap_or_default();
        assert!(summary.contains("Use concise replies in writing help."));
        assert!(summary.contains("Keep examples practical before adding style."));
        assert_eq!(digest.metadata["item_count"], json!(2));
        assert_eq!(
            digest.metadata["learned_items"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            2
        );
    }

    #[test]
    fn today_source_url_accepts_canonical_and_legacy_research_shapes() {
        assert_eq!(
            today_first_source_url(&json!({
                "sources": ["https://example.test/canonical"]
            }))
            .as_deref(),
            Some("https://example.test/canonical")
        );
        assert_eq!(
            today_first_source_url(&json!({
                "sources": {"summary": "https://example.test/scalar-normalized"}
            }))
            .as_deref(),
            Some("https://example.test/scalar-normalized")
        );
        assert_eq!(
            today_first_source_url(&json!({
                "source_urls": ["https://example.test/legacy"]
            }))
            .as_deref(),
            Some("https://example.test/legacy")
        );
    }

    #[test]
    fn today_followups_project_meeting_memory_action_items() {
        let knowledge = json!({
            "research_findings": [
                {
                    "key": "meeting:meeting-weekly-2026-06-23",
                    "source_type": "meeting_capture",
                    "thread_id": "meeting-weekly-2026-06-23",
                    "title": "Weekly Review",
                    "date": "2026-06-23",
                    "summary": "Discussed the **launch checklist** and owners.\n\n- Mobile release must keep the complete meeting context.\n- The durable action card must remain useful after the meeting thread expires.\n- Markdown lists and emphasis must survive projection without truncation.",
                    "action_items": [
                        "Send the launch notes to the team",
                        {"description": "Book the review room"},
                        "   "
                    ]
                },
                {
                    "key": "finding:unrelated",
                    "source_type": "research",
                    "action_items": ["This should not appear"]
                }
            ],
            "fields": {
                "research_findings": [
                    {
                        "key": "meeting:legacy",
                        "action_items": ["Confirm legacy follow-up"],
                        "updated_at": "2026-06-23T10:00:00Z"
                    }
                ]
            }
        });

        let items =
            today_meeting_followup_items_from_knowledge(&knowledge, "alpha", "prod", 8, &[]);

        let titles = items
            .iter()
            .map(|item| item.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(items.len(), 3);
        assert!(titles.contains(&"Meeting action: Send the launch notes to the team"));
        assert!(titles.contains(&"Meeting action: Book the review room"));
        assert!(titles.contains(&"Meeting action: Confirm legacy follow-up"));
        let weekly = items
            .iter()
            .find(|item| item.source_id == "meeting:meeting-weekly-2026-06-23:action:0")
            .expect("weekly meeting item");
        assert_eq!(weekly.section, TodaySectionId::Followups);
        assert_eq!(weekly.source_kind, "meeting_action");
        assert_eq!(weekly.status, FeedItemStatus::NeedsAction);
        assert_eq!(weekly.actions.len(), 1);
        assert_eq!(weekly.actions[0].id, "create_task");
        assert_eq!(
            weekly.actions[0].action_type.as_deref(),
            Some("today_source_action")
        );
        assert_eq!(weekly.actions[0].payload["method"], json!("POST"));
        assert_eq!(
            weekly.source_url.as_deref(),
            Some("/meetings/meeting-weekly-2026-06-23")
        );
        assert_eq!(
            weekly.metadata["followup_kind"],
            json!("meeting_action_item")
        );
        assert!(weekly.metadata["detail_markdown"]
            .as_str()
            .is_some_and(|value| value.contains("**launch checklist**")
                && value.contains("durable action card")
                && value.contains("## Action item")));
        assert!(weekly.metadata["meeting_summary"]
            .as_str()
            .is_some_and(|value| value.chars().count() > 160));

        let linked_task: TaskListItemV3 = serde_json::from_value(json!({
            "id": "task_existing_elsewhere",
            "title": "Send the launch notes to the team",
            "description": "Created from the meeting action",
            "status": "pending",
            "agent_id": "personal-assistant",
            "ui_thread_id": "meeting-weekly-2026-06-23",
            "created_at": "2026-06-23T10:00:00Z",
            "updated_at": "2026-06-23T10:00:00Z"
        }))
        .expect("linked task fixture");
        let with_linked_task = today_meeting_followup_items_from_knowledge(
            &knowledge,
            "alpha",
            "prod",
            8,
            &[linked_task],
        );
        assert_eq!(with_linked_task.len(), 2);
        assert!(with_linked_task
            .iter()
            .all(|item| item.source_id != weekly.source_id));
    }

    use magician_surfaces::thinking_map::{PromotedRef, ThinkingMapSource, ThinkingNode};

    fn thinking_map_fixture() -> ThinkingMap {
        ThinkingMap::new(
            "map1".to_string(),
            "alpha",
            "prod",
            "Launch planning",
            ThinkingMapSource::Solo,
            "2026-07-22T10:00:00Z",
        )
    }

    fn thinking_node(
        node_id: &str,
        kind: NodeKind,
        state: EpistemicState,
        origin: AssertionOrigin,
    ) -> ThinkingNode {
        ThinkingNode {
            node_id: node_id.to_string(),
            kind,
            label: format!("Ship item {node_id}"),
            detail_markdown: None,
            epistemic_state: state,
            assertion_origin: origin,
            confidence: 0.9,
            speaker: None,
            source_refs: Vec::new(),
            parent_id: None,
            position: None,
            position_locked: false,
            promoted_refs: Vec::new(),
            tombstoned: false,
            created_at: "2026-07-22T10:00:00Z".to_string(),
            updated_at: "2026-07-22T11:00:00Z".to_string(),
        }
    }

    fn task_list_item(id: &str, status: &str, description: &str) -> TaskListItemV3 {
        serde_json::from_value(json!({
            "id": id,
            "title": "Some task",
            "description": description,
            "status": status,
            "agent_id": "personal-assistant",
            "ui_thread_id": "general",
            "created_at": "2026-07-22T10:00:00Z",
            "updated_at": "2026-07-22T10:00:00Z"
        }))
        .expect("task fixture")
    }

    #[test]
    fn today_followups_project_owner_asserted_unlinked_map_actions() {
        let mut map = thinking_map_fixture();
        for node in [
            // Qualifies: acknowledged (owner-asserted) action.
            thinking_node(
                "n1",
                NodeKind::Action,
                EpistemicState::Asserted,
                AssertionOrigin::OwnerSpoken,
            ),
            // Qualifies: confirmed owner edit.
            thinking_node(
                "n2",
                NodeKind::Action,
                EpistemicState::Confirmed,
                AssertionOrigin::OwnerEdited,
            ),
            // Not acknowledged: model-inferred provisional.
            thinking_node(
                "n3",
                NodeKind::Action,
                EpistemicState::Provisional,
                AssertionOrigin::ModelInferred,
            ),
            // Owner-asserted but not an action.
            thinking_node(
                "n4",
                NodeKind::Fact,
                EpistemicState::Asserted,
                AssertionOrigin::OwnerSpoken,
            ),
            // Participant content is data, never a Today action.
            thinking_node(
                "n5",
                NodeKind::Action,
                EpistemicState::Asserted,
                AssertionOrigin::ParticipantSpoken,
            ),
        ] {
            map.nodes.insert(node.node_id.clone(), node);
        }
        let mut tombstoned = thinking_node(
            "n6",
            NodeKind::Action,
            EpistemicState::Asserted,
            AssertionOrigin::OwnerSpoken,
        );
        tombstoned.tombstoned = true;
        map.nodes.insert("n6".to_string(), tombstoned);

        let items = today_thinking_map_action_items_from_map(&map, "alpha", "prod", &[], false);
        let ids = items
            .iter()
            .map(|item| item.source_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec!["thinking_map:map1:node:n1", "thinking_map:map1:node:n2"]
        );

        let item = &items[0];
        assert!(item.id.starts_with(TODAY_THINKING_MAP_ITEM_PREFIX));
        assert_eq!(item.section, TodaySectionId::Followups);
        assert_eq!(item.source_kind, THINKING_MAP_ACTION_SOURCE_KIND);
        assert_eq!(item.status, FeedItemStatus::NeedsAction);
        assert_eq!(item.title, "Map action: Ship item n1");
        assert_eq!(
            item.source_url.as_deref(),
            Some("/thinking-maps/map1?node=n1")
        );
        assert_eq!(item.thread_id.as_deref(), Some("thinking-map-map1"));
        assert_eq!(item.metadata["map_id"], json!("map1"));
        assert_eq!(item.metadata["node_id"], json!("n1"));
        assert_eq!(item.metadata["followup_kind"], json!("thinking_map_action"));
        assert_eq!(item.actions.len(), 1);
        assert_eq!(item.actions[0].id, "create_task");
        assert_eq!(
            item.actions[0].action_type.as_deref(),
            Some("today_source_action")
        );
        assert_eq!(
            item.actions[0].payload["endpoint"],
            json!(format!(
                "/api/magician/v2/today/items/{}/actions/create_task",
                item.id
            ))
        );
    }

    #[test]
    fn today_map_action_suppressed_after_task_promotion_for_every_task_status() {
        let mut map = thinking_map_fixture();
        let mut node = thinking_node(
            "n1",
            NodeKind::Action,
            EpistemicState::Asserted,
            AssertionOrigin::OwnerSpoken,
        );
        node.promoted_refs.push(PromotedRef {
            destination_kind: PromotionKind::Task,
            object_id: "task_linked".to_string(),
            linked_at: "2026-07-22T12:00:00Z".to_string(),
        });
        map.nodes.insert("n1".to_string(), node);

        // The promoted_ref on the node is authoritative: the candidate stays
        // suppressed whether the linked task is pending, terminal, or even
        // deleted from the task list entirely.
        for known_tasks in [
            Vec::new(),
            vec![task_list_item("task_linked", "pending", "")],
            vec![task_list_item("task_linked", "completed", "")],
            vec![task_list_item("task_linked", "cancelled", "")],
        ] {
            let items = today_thinking_map_action_items_from_map(
                &map,
                "alpha",
                "prod",
                &known_tasks,
                false,
            );
            assert!(items.is_empty(), "linked node must never resurface");
        }

        // The action-execution path re-projects with promoted nodes included
        // so a retry resolves the already-linked task.
        let items = today_thinking_map_action_items_from_map(&map, "alpha", "prod", &[], true);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].metadata["linked_task_id"], json!("task_linked"));

        // A memory-only promotion does NOT suppress the task candidate.
        let mut memory_map = thinking_map_fixture();
        let mut memory_node = thinking_node(
            "n2",
            NodeKind::Action,
            EpistemicState::Asserted,
            AssertionOrigin::OwnerSpoken,
        );
        memory_node.promoted_refs.push(PromotedRef {
            destination_kind: PromotionKind::Memory,
            object_id: "lc_memory".to_string(),
            linked_at: "2026-07-22T12:00:00Z".to_string(),
        });
        memory_map.nodes.insert("n2".to_string(), memory_node);
        let items =
            today_thinking_map_action_items_from_map(&memory_map, "alpha", "prod", &[], false);
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn today_map_action_candidates_only_from_active_maps() {
        let mut map = thinking_map_fixture();
        map.nodes.insert(
            "n1".to_string(),
            thinking_node(
                "n1",
                NodeKind::Action,
                EpistemicState::Asserted,
                AssertionOrigin::OwnerSpoken,
            ),
        );
        for lifecycle in [
            MapLifecycle::Paused,
            MapLifecycle::Archived,
            MapLifecycle::Deleted,
        ] {
            map.lifecycle = lifecycle;
            assert!(
                today_thinking_map_action_items_from_map(&map, "alpha", "prod", &[], false)
                    .is_empty()
            );
        }
        map.lifecycle = MapLifecycle::Active;
        assert_eq!(
            today_thinking_map_action_items_from_map(&map, "alpha", "prod", &[], false).len(),
            1
        );
    }

    #[test]
    fn today_map_action_reconciles_against_existing_tasks_like_meeting_actions() {
        let mut map = thinking_map_fixture();
        map.nodes.insert(
            "n1".to_string(),
            thinking_node(
                "n1",
                NodeKind::Action,
                EpistemicState::Asserted,
                AssertionOrigin::OwnerSpoken,
            ),
        );

        // A task carrying the durable provenance marker (e.g. created by the
        // promote endpoint moments before the link commit) suppresses the
        // candidate even without a promoted_ref on the node.
        let marker_task = task_list_item(
            "task_marker",
            "pending",
            "Some details\nThinking map node source: map1:n1",
        );
        assert!(today_thinking_map_action_items_from_map(
            &map,
            "alpha",
            "prod",
            &[marker_task],
            false
        )
        .is_empty());

        // An unrelated task leaves the candidate intact.
        let unrelated = task_list_item("task_other", "pending", "No provenance here");
        assert_eq!(
            today_thinking_map_action_items_from_map(&map, "alpha", "prod", &[unrelated], false)
                .len(),
            1
        );
    }

    #[test]
    fn today_standard_sections_reject_raw_learning_telemetry() {
        for item_type in [
            FeedItemType::AgentLearning,
            FeedItemType::LearningCandidate,
            FeedItemType::LearningInsight,
        ] {
            let mut item = feed_item("learning-noise", item_type, None);
            item.summary = Some("raw learning telemetry".to_string());
            item.metadata = json!({
                "completion_outcome": "raw learning telemetry",
                "completion_artifact_names": ["memory.json"],
            });

            item.status = FeedItemStatus::Done;
            assert!(!today_item_is_delivered(&item));

            item.status = FeedItemStatus::Running;
            assert!(!today_item_is_active_work(&item));
        }
    }

    #[test]
    fn today_changed_digest_paginates_and_keeps_source_refs() {
        let delivered = (0..9)
            .map(|index| {
                let mut item = feed_item(
                    &format!("data_delivery:surface-{index}"),
                    FeedItemType::DataDelivery,
                    None,
                );
                item.title = format!("Delivery {index}");
                item.summary = Some(format!("Useful output {index}"));
                item.updated_at = 100 + index;
                item.metadata = json!({
                    "surface_id": format!("surface-{index}"),
                    "space_id": "alpha-space",
                });
                today_item_from_feed_item(
                    item,
                    TodaySectionId::Delivered,
                    700,
                    "Completed work with user-visible output.".to_string(),
                )
            })
            .collect::<Vec<_>>();

        let digest = today_changed_digest(200, None, &[], &delivered, &[], &[]);
        let page = today_digest_page(digest, 0, 7);

        assert_eq!(page.generated_at, 200);
        assert_eq!(page.since, None);
        assert_eq!(page.total, 9);
        assert_eq!(page.limit, 7);
        assert_eq!(page.offset, 0);
        assert_eq!(page.bullets.len(), 7);
        assert!(page
            .bullets
            .iter()
            .all(|bullet| bullet.text.starts_with("Delivered: Delivery")));
        assert_eq!(
            page.bullets[0].source_url.as_deref(),
            Some("/briefing/surface-8")
        );
        assert_eq!(page.bullets[0].space_ids, vec!["alpha-space".to_string()]);

        let next_digest = today_changed_digest(300, Some(108), &[], &delivered, &[], &[]);

        assert_eq!(next_digest.generated_at, 300);
        assert_eq!(next_digest.since, Some(108));
        assert_eq!(next_digest.total, 0);
        assert!(next_digest.bullets.is_empty());
    }

    #[test]
    fn skill_evolution_gate_approval_item_links_to_skill_evolution() {
        let proposal = evolution_proposal(
            "proposal_review",
            "candidate_review",
            LearningCapabilityEvolutionProposalStatus::ReadyForReview,
            vec!["skills/foo/SKILL.md".to_string()],
        );

        let item = skill_evolution_gate_approval_feed_item(
            &proposal,
            SkillEvolutionApprovalGate::ProposalReview,
            "Approve Skill Evolution proposal",
            proposal.summary.clone(),
            "Review proposal",
            None,
            None,
            None,
        );

        assert_eq!(item.item_type, FeedItemType::Approval);
        assert_eq!(item.status, FeedItemStatus::NeedsAction);
        assert_eq!(
            metadata_string(&item.metadata, "attention_kind").as_deref(),
            Some("skill_evolution.proposal_review")
        );
        assert_eq!(
            metadata_string(&item.metadata, "review_href").as_deref(),
            Some(
                "/skills/evolution?candidate_id=candidate_review&\
                 skill_evolution_gate=proposal_review"
            )
        );
        assert_eq!(
            metadata_string(&item.metadata, "route").as_deref(),
            Some(
                "/skills/evolution?candidate_id=candidate_review&\
                 skill_evolution_gate=proposal_review"
            )
        );
        assert_eq!(
            metadata_string(&item.metadata, "review_label").as_deref(),
            Some("Review proposal")
        );
        assert_eq!(
            metadata_string(&item.metadata, "source_type").as_deref(),
            Some("learning_capability_evolution_proposal")
        );
        assert_eq!(item.actions.len(), 1);
        assert_eq!(item.actions[0].label, "Review proposal");
    }

    #[test]
    fn skill_evolution_gate_approval_items_project_review_apply_and_promotion() {
        let proposal_review = evolution_proposal(
            "proposal_review",
            "candidate_review",
            LearningCapabilityEvolutionProposalStatus::ReadyForReview,
            vec!["skills/review/SKILL.md".to_string()],
        );
        let proposal_apply = evolution_proposal(
            "proposal_apply",
            "candidate_apply",
            LearningCapabilityEvolutionProposalStatus::Approved,
            vec!["skills/apply/SKILL.md".to_string()],
        );
        let proposal_promote = evolution_proposal(
            "proposal_promote",
            "candidate_promote",
            LearningCapabilityEvolutionProposalStatus::Approved,
            vec!["skills/promote/SKILL.md".to_string()],
        );
        let validation_apply = evolution_validation(
            "candidate_apply",
            "proposal_apply",
            LearningCapabilityEvolutionValidationStatus::Passed,
        );
        let validation_promote = evolution_validation(
            "candidate_promote",
            "proposal_promote",
            LearningCapabilityEvolutionValidationStatus::Passed,
        );
        let implementation_apply = evolution_implementation(
            "candidate_apply",
            "proposal_apply",
            &validation_apply.id,
            proposal_apply.patches.clone(),
        );
        let implementation_promote = evolution_implementation(
            "candidate_promote",
            "proposal_promote",
            &validation_promote.id,
            proposal_promote.patches.clone(),
        );
        let application_promote = evolution_application(
            "candidate_promote",
            "proposal_promote",
            &validation_promote.id,
            &implementation_promote.id,
        );

        let items = skill_evolution_gate_approval_feed_items(
            vec![proposal_promote, proposal_apply, proposal_review],
            vec![validation_promote, validation_apply],
            vec![implementation_promote, implementation_apply],
            vec![application_promote],
            Vec::new(),
        );

        let attention_kinds = items
            .iter()
            .map(|item| metadata_string(&item.metadata, "attention_kind").expect("attention_kind"))
            .collect::<Vec<_>>();
        assert_eq!(
            attention_kinds,
            vec![
                "skill_evolution.proposal_review",
                "skill_evolution.apply_gate",
                "skill_evolution.promotion_gate",
            ]
        );
        assert_eq!(
            metadata_string(&items[1].metadata, "implementation_id").as_deref(),
            Some("implementation_candidate_apply")
        );
        assert_eq!(
            metadata_string(&items[1].metadata, "skill_evolution_gate_action").as_deref(),
            Some("dry_run")
        );
        assert_eq!(
            metadata_string(&items[1].metadata, "target_surface").as_deref(),
            Some("scoped_skill")
        );
        assert_eq!(
            metadata_string(&items[2].metadata, "application_id").as_deref(),
            Some("application_candidate_promote")
        );
    }

    #[test]
    fn skill_evolution_promotion_gate_waits_for_scoped_application_and_existing_promotion() {
        let proposal = evolution_proposal(
            "proposal_scoped",
            "candidate_scoped",
            LearningCapabilityEvolutionProposalStatus::Approved,
            vec!["skills/scoped/SKILL.md".to_string()],
        );
        let validation = evolution_validation(
            "candidate_scoped",
            "proposal_scoped",
            LearningCapabilityEvolutionValidationStatus::Passed,
        );
        let implementation = evolution_implementation(
            "candidate_scoped",
            "proposal_scoped",
            &validation.id,
            proposal.patches.clone(),
        );

        let items_before_apply = skill_evolution_gate_approval_feed_items(
            vec![proposal.clone()],
            vec![validation.clone()],
            vec![implementation.clone()],
            Vec::new(),
            Vec::new(),
        );
        let before_apply_kinds = items_before_apply
            .iter()
            .map(|item| metadata_string(&item.metadata, "attention_kind").expect("attention_kind"))
            .collect::<Vec<_>>();
        assert_eq!(before_apply_kinds, vec!["skill_evolution.apply_gate"]);

        let source_proposal = evolution_proposal(
            "proposal_source",
            "candidate_source",
            LearningCapabilityEvolutionProposalStatus::Approved,
            vec!["skillshub/source-tool/SKILL.md".to_string()],
        );
        let source_validation = evolution_validation(
            "candidate_source",
            "proposal_source",
            LearningCapabilityEvolutionValidationStatus::Passed,
        );
        let source_implementation = evolution_implementation(
            "candidate_source",
            "proposal_source",
            &source_validation.id,
            source_proposal.patches.clone(),
        );
        let source_items = skill_evolution_gate_approval_feed_items(
            vec![source_proposal],
            vec![source_validation],
            vec![source_implementation],
            Vec::new(),
            Vec::new(),
        );
        let source_kinds = source_items
            .iter()
            .map(|item| metadata_string(&item.metadata, "attention_kind").expect("attention_kind"))
            .collect::<Vec<_>>();
        assert_eq!(source_kinds, vec!["skill_evolution.apply_gate"]);
        assert_eq!(
            metadata_string(&source_items[0].metadata, "target_surface").as_deref(),
            Some("source_skill")
        );

        let mut prepared_application = evolution_application(
            "candidate_scoped",
            "proposal_scoped",
            &validation.id,
            &implementation.id,
        );
        prepared_application.id = "application_candidate_scoped_prepared".to_string();
        prepared_application.mode = LearningCapabilityEvolutionApplicationMode::DryRun;
        prepared_application.status = LearningCapabilityEvolutionApplicationStatus::Prepared;
        let items_after_dry_run = skill_evolution_gate_approval_feed_items(
            vec![proposal.clone()],
            vec![validation.clone()],
            vec![implementation.clone()],
            vec![prepared_application],
            Vec::new(),
        );
        assert_eq!(
            metadata_string(
                &items_after_dry_run[0].metadata,
                "skill_evolution_gate_action"
            )
            .as_deref(),
            Some("apply")
        );
        assert_eq!(
            metadata_string(&items_after_dry_run[0].metadata, "application_id").as_deref(),
            Some("application_candidate_scoped_prepared")
        );

        let application = evolution_application(
            "candidate_scoped",
            "proposal_scoped",
            &validation.id,
            &implementation.id,
        );
        let promotion = evolution_promotion(
            "candidate_scoped",
            "proposal_scoped",
            &validation.id,
            Some(implementation.id.clone()),
            Some(application.id.clone()),
        );

        let items_after_promotion = skill_evolution_gate_approval_feed_items(
            vec![proposal],
            vec![validation],
            vec![implementation],
            vec![application],
            vec![promotion],
        );
        assert!(items_after_promotion.is_empty());
    }

    #[test]
    fn rollback_recommendation_attention_item_links_to_skill_evolution() {
        let mut recommendation = rollback_recommendation_record("lcerollback_test");
        recommendation.candidate_id = "candidate".to_string();

        let item = rollback_recommendation_attention_feed_item(&recommendation);

        assert_eq!(item.item_type, FeedItemType::Escalation);
        assert_eq!(item.status, FeedItemStatus::NeedsAction);
        assert_eq!(
            metadata_string(&item.metadata, "attention_kind").as_deref(),
            Some("skill_evolution.rollback_recommended")
        );
        assert_eq!(
            metadata_string(&item.metadata, "review_href").as_deref(),
            Some(
                "/skills/evolution?rollback_recommendation=lcerollback_test&candidate_id=candidate"
            )
        );
        assert_eq!(item.actions.len(), 1);
    }

    #[test]
    fn special_attention_projections_reach_item_after_first_cursor_page() {
        let approvals = skill_evolution_gate_approval_feed_items(
            vec![
                evolution_proposal(
                    "proposal_one",
                    "candidate_one",
                    LearningCapabilityEvolutionProposalStatus::ReadyForReview,
                    vec!["skills/one/SKILL.md".to_string()],
                ),
                evolution_proposal(
                    "proposal_two",
                    "candidate_two",
                    LearningCapabilityEvolutionProposalStatus::ReadyForReview,
                    vec!["skills/two/SKILL.md".to_string()],
                ),
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        assert_second_attention_item_reachable(approvals, AttentionLane::NeedsYou);

        let rollbacks = rollback_recommendation_attention_feed_items(vec![
            rollback_recommendation_record("rollback_one"),
            rollback_recommendation_record("rollback_two"),
        ]);
        assert_second_attention_item_reachable(rollbacks, AttentionLane::NeedsYou);

        let scope = LearningScope::new("alpha".to_string(), "prod".to_string());
        let (_temp_dir, store) = temp_learning_store();
        let monitors = post_promotion_monitor_attention_feed_items(
            &store,
            &scope,
            vec![
                post_promotion_monitor_record("monitor_one", "promotion_one"),
                post_promotion_monitor_record("monitor_two", "promotion_two"),
            ],
        );
        assert_second_attention_item_reachable(monitors, AttentionLane::NeedsYou);
    }

    #[tokio::test]
    async fn durable_special_projection_pages_item_n_plus_one_and_removes_resolved_rows() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(
            temp_dir.path(),
        );
        let learning_store = LearningStore::new(workspace);
        let feed_store = FeedStore::open(temp_dir.path()).expect("feed store");
        let first_record = rollback_recommendation_record("rollback_one");
        let mut second_record = rollback_recommendation_record("rollback_two");
        learning_store
            .write_capability_evolution_rollback_recommendation_record(&first_record)
            .expect("first rollback record");
        learning_store
            .write_capability_evolution_rollback_recommendation_record(&second_record)
            .expect("second rollback record");

        reconcile_skill_evolution_attention_projection(
            &feed_store,
            learning_store.clone(),
            LearningScope::new("alpha".to_string(), "prod".to_string()),
        )
        .await
        .expect("initial special projection");
        let first = feed_store
            .list_attention_lane_page(FeedAttentionPageQuery {
                exclude_task_ids: Vec::new(),
                principal: "alpha".to_string(),
                workspace: "prod".to_string(),
                lane: FeedAttentionLane::Escalations,
                ui_thread_id: None,
                cursor: None,
                offset: 0,
                limit: 1,
            })
            .await
            .expect("first special page");
        assert_eq!(first.total, 2);
        assert!(first.has_more);
        let second = feed_store
            .list_attention_lane_page(FeedAttentionPageQuery {
                exclude_task_ids: Vec::new(),
                principal: "alpha".to_string(),
                workspace: "prod".to_string(),
                lane: FeedAttentionLane::Escalations,
                ui_thread_id: None,
                cursor: first.next_cursor,
                offset: 0,
                limit: 1,
            })
            .await
            .expect("second special page");
        assert_eq!(second.items.len(), 1);
        assert!(!second.has_more);

        second_record.status = LearningCapabilityEvolutionRollbackRecommendationStatus::Dismissed;
        learning_store
            .write_capability_evolution_rollback_recommendation_record(&second_record)
            .expect("dismissed rollback record");
        let sync = reconcile_skill_evolution_attention_projection(
            &feed_store,
            learning_store,
            LearningScope::new("alpha".to_string(), "prod".to_string()),
        )
        .await
        .expect("resolved special projection");
        assert_eq!(sync.removed.len(), 1);
        assert!(feed_store
            .get_item("alpha", "prod", "skill_evolution_rollback:rollback_two")
            .await
            .expect("read removed projection")
            .is_none());
    }

    #[test]
    fn post_promotion_monitor_attention_item_links_to_skill_evolution() {
        let mut monitor = post_promotion_monitor_record("lceppm_lcepromo_test", "lcepromo_test");
        monitor.follow_up_candidate_id = Some("candidate_follow_up".to_string());

        let item = post_promotion_monitor_attention_feed_item(&monitor);

        assert_eq!(item.item_type, FeedItemType::Escalation);
        assert_eq!(item.status, FeedItemStatus::NeedsAction);
        assert_eq!(
            metadata_string(&item.metadata, "attention_kind").as_deref(),
            Some("skill_evolution.post_promotion_regression")
        );
        assert_eq!(
            metadata_string(&item.metadata, "review_href").as_deref(),
            Some("/skills/evolution?post_promotion_monitor=lcepromo_test&candidate_id=candidate")
        );
        assert_eq!(item.actions.len(), 1);
    }

    #[test]
    fn post_promotion_monitor_attention_skips_resolved_regressions() {
        let scope = LearningScope::new("alpha".to_string(), "prod".to_string());
        let (_temp_dir, store) = temp_learning_store();

        let unresolved = post_promotion_monitor_record("lceppm_unresolved", "lcepromo_unresolved");
        assert!(post_promotion_monitor_needs_attention(
            &store,
            &scope,
            &unresolved
        ));

        let mut rollback_backed =
            post_promotion_monitor_record("lceppm_rollback", "lcepromo_rollback");
        rollback_backed.rollback_recommendation_id = Some("lcerollback_existing".to_string());
        assert!(!post_promotion_monitor_needs_attention(
            &store,
            &scope,
            &rollback_backed
        ));

        let terminal_candidate = store
            .create_candidate(
                scope.clone(),
                candidate_request(LearningCandidateState::Archived),
            )
            .expect("terminal candidate");
        let mut terminal_follow_up =
            post_promotion_monitor_record("lceppm_terminal", "lcepromo_terminal");
        terminal_follow_up.follow_up_candidate_id = Some(terminal_candidate.id);
        assert!(!post_promotion_monitor_needs_attention(
            &store,
            &scope,
            &terminal_follow_up
        ));

        let active_candidate = store
            .create_candidate(
                scope.clone(),
                candidate_request(LearningCandidateState::Triaged),
            )
            .expect("active candidate");
        let mut active_follow_up =
            post_promotion_monitor_record("lceppm_active", "lcepromo_active");
        active_follow_up.follow_up_candidate_id = Some(active_candidate.id);
        assert!(post_promotion_monitor_needs_attention(
            &store,
            &scope,
            &active_follow_up
        ));

        let mut missing_follow_up =
            post_promotion_monitor_record("lceppm_missing", "lcepromo_missing");
        missing_follow_up.follow_up_candidate_id = Some("missing_candidate".to_string());
        assert!(post_promotion_monitor_needs_attention(
            &store,
            &scope,
            &missing_follow_up
        ));

        let mut stable = post_promotion_monitor_record("lceppm_stable", "lcepromo_stable");
        stable.status = LearningCapabilityEvolutionPostPromotionMonitorStatus::Stable;
        assert!(!post_promotion_monitor_needs_attention(
            &store, &scope, &stable
        ));
    }

    #[test]
    fn post_promotion_monitor_attention_filters_resolved_rows() {
        let scope = LearningScope::new("alpha".to_string(), "prod".to_string());
        let (_temp_dir, store) = temp_learning_store();
        let mut rollback_backed =
            post_promotion_monitor_record("lceppm_rollback", "lcepromo_rollback");
        rollback_backed.rollback_recommendation_id = Some("lcerollback_existing".to_string());
        let actionable = post_promotion_monitor_record("lceppm_actionable", "lcepromo_actionable");

        let items = post_promotion_monitor_attention_feed_items(
            &store,
            &scope,
            vec![rollback_backed, actionable],
        );

        assert_eq!(items.len(), 1);
        assert_eq!(
            metadata_string(&items[0].metadata, "monitor_id").as_deref(),
            Some("lceppm_actionable")
        );
    }

    // ---------------------------------------------------------------
    // Today: the reader's date, and the projection cache
    // ---------------------------------------------------------------

    /// A `FeedApi` over a fresh temporary workspace, plus the feed store
    /// behind it so a test can change the corpus underneath a reader.
    fn today_test_api(tmp: &tempfile::TempDir) -> (FeedApi, FeedStore) {
        let v3_service = build_test_artifact_v2_service(tmp.path());
        let feed_store = FeedStore::open(&tmp.path().join("feed")).expect("today feed store");
        (FeedApi::new(feed_store.clone(), v3_service), feed_store)
    }

    fn today_task_fixture(due_date: Option<&str>) -> CreateTaskInput {
        CreateTaskInput {
            principal: "alpha".to_string(),
            workspace: "prod".to_string(),
            title: "Renew the certificate".to_string(),
            description: "the reader's date decides when this is due".to_string(),
            agent_id: "personal-assistant".to_string(),
            goal_id: None,
            ui_thread_id: "general".to_string(),
            priority: Some("normal".to_string()),
            due_date: due_date.map(str::to_string),
            tags: Vec::new(),
            created_by: "user".to_string(),
            depends_on: Vec::new(),
            approved: true,
            schedule: None,
            output_mode: TaskOutputMode::Accumulate,
            chat_session_id: None,
            lifecycle: magician::magician_v2::artifact_v2::models::TaskLifecycle::default(),
            sync_mode: magician::magician_v2::artifact_v2::models::TaskSyncMode::default(),
        }
    }

    /// Call `/today` exactly as the wire does — the query string is parsed
    /// by the same `Deserialize` the handler uses, so a field this crate
    /// spelled differently from the clients would not silently arrive as
    /// `None`. The compatibility query values are forwarded as the engraved
    /// scope headers that authentication middleware supplies on the wire.
    async fn today_response(api: &FeedApi, query: &str) -> (actix_web::http::StatusCode, Value) {
        let principal = query.split('&').find_map(|pair| {
            pair.strip_prefix("principal=")
                .filter(|value| !value.is_empty())
        });
        let workspace = query.split('&').find_map(|pair| {
            pair.strip_prefix("workspace=")
                .filter(|value| !value.is_empty())
        });
        let mut request = actix_web::test::TestRequest::default();
        if let Some(principal) = principal {
            request = request.insert_header(("X-Principal", principal));
        }
        if let Some(workspace) = workspace {
            request = request.insert_header(("X-Workspace", workspace));
        }
        let request = request.to_http_request();
        let query = web::Query::<TodayQuery>::from_query(query).expect("Today query deserializes");
        let response = api.today(&request, query).await.expect("Today response");
        let status = response.status();
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .expect("Today body");
        let body: Value = serde_json::from_slice(&body).expect("Today JSON");
        (status, body)
    }

    fn today_section_ids(body: &Value, section: &str) -> Vec<String> {
        body["sections"][section]
            .as_array()
            .unwrap_or_else(|| panic!("Today response should carry a {section} array"))
            .iter()
            .filter_map(|item| item["id"].as_str().map(str::to_string))
            .collect()
    }

    /// The reason Follow-ups gives `item_id` when the reader's date is
    /// `today`, or `None` when the lane does not carry that row at all.
    async fn today_followup_reason(api: &FeedApi, today: &str, item_id: &str) -> Option<String> {
        let (status, body) = today_response(
            api,
            &format!("principal=alpha&workspace=prod&today={today}"),
        )
        .await;
        assert_eq!(status, actix_web::http::StatusCode::OK);
        body["sections"]["followups"]
            .as_array()
            .expect("Today response should carry a followups array")
            .iter()
            .find(|item| item["id"].as_str() == Some(item_id))
            .map(|item| item["reason"].as_str().unwrap_or_default().to_string())
    }

    // ---------------------------------------------------------------
    // Today: how many times one request reads the corpus
    // ---------------------------------------------------------------

    /// **`/today` takes one corpus walk of its own, where it took five.**
    ///
    /// `today_projection` walked the corpus for `valid_task_ids`, again for
    /// `valid_attention_task_ids` (twice — the user root and the internal
    /// one), again for `list_scope_monitor_updates`, and again for the
    /// Follow-ups lane. Four of those kept nothing but ids, and the monitor
    /// pass then went back to disk once *per task* to re-read a field its own
    /// listing already carried. Against the author's 137-task scope that was
    /// ~690 record reads and 14.4MB parsed, every 30 seconds idle and roughly
    /// every 750ms during a run.
    ///
    /// # What the three remaining walks are
    ///
    /// Exactly one belongs to the projection: the Follow-ups lane, which is
    /// the only consumer that needs the built rows, and which every id-only
    /// consumer now reads off. The other two belong to
    /// `ArtifactV2Service::list_attention_items`, which lists the user root
    /// and the internal root for itself. That is a real remaining cost and
    /// **not** one this test excuses — the number is asserted exactly, so
    /// re-introducing an id-only pass moves it to four and fails here, and
    /// giving `list_attention_items` the listings the caller already holds
    /// would move it to one and fail here too, which is the failure a
    /// follow-up wants.
    ///
    /// Counted, not timed. A timing assertion measures the disk and the
    /// machine; the counter measures the thing that was wrong. It is
    /// incremented by the two functions that `read_dir` a task root, so it
    /// cannot be satisfied by a pass that walks and discards.
    #[actix_web::test]
    async fn today_takes_one_corpus_walk_of_its_own() {
        let tmp = tempfile::tempdir().expect("temporary Today workspace");
        let (api, _feed_store) = today_test_api(&tmp);
        for _ in 0..3 {
            api.v3_service
                .create_task(today_task_fixture(Some("2031-03-15")))
                .await
                .expect("a task to walk over");
        }
        // Ready, so the internal-task half of the attention filter is an
        // index read rather than a second walk of `internal_tasks/`.
        magician::magician_v2::test_support::wire_test_list_index(&api.v3_service);

        let before = api.v3_service.task_corpus_walks();
        api.today_projection(
            "alpha",
            "prod",
            TODAY_SECTION_COLLECTION_LIMIT,
            TODAY_SECTION_COLLECTION_LIMIT,
            chrono::NaiveDate::from_ymd_opt(2031, 3, 15).expect("a real date"),
            1_800_000_000_000,
        )
        .await
        .expect("the projection computes");

        assert_eq!(
            api.v3_service.task_corpus_walks() - before,
            3,
            "one for the Follow-ups lane, which every id-only consumer now \
             reads off, plus the two `list_attention_items` takes for itself. \
             Seven before this change (five of the user root, two of the \
             internal one)"
        );
    }

    /// **The indexed id set is the walk's id set** — including when the
    /// index's *columns* are stale, which is the state a reconcile that has
    /// not caught up leaves behind.
    ///
    /// Membership and freshness are different properties. `valid_task_ids`
    /// filters feed rows on "does this task still exist", and the index answers
    /// exactly that, because a create and a delete each reconcile it. A row
    /// whose `status` or `title` has drifted still names a task that exists,
    /// so it must not change the answer — the second half of this test
    /// corrupts precisely those columns and asserts the set does not move.
    #[actix_web::test]
    async fn the_indexed_task_id_set_is_the_walks_id_set_even_when_a_row_is_stale() {
        use magician::magician_v2::storage::ListPageQuery;

        let tmp = tempfile::tempdir().expect("temporary Today workspace");
        let (api, _feed_store) = today_test_api(&tmp);
        for _ in 0..3 {
            api.v3_service
                .create_task(today_task_fixture(None))
                .await
                .expect("a task");
        }
        let index = magician::magician_v2::test_support::wire_test_list_index(&api.v3_service);
        let scope =
            ScopeRef::system_internal_unauthenticated(&"alpha".to_string(), &"prod".to_string());

        let walked: HashSet<String> = api
            .v3_service
            .list_tasks(&scope)
            .await
            .expect("the walk lists")
            .into_iter()
            .map(|task| task.id)
            .collect();
        assert_eq!(walked.len(), 3, "the fixture has to have made three rows");
        assert_eq!(
            api.valid_task_ids("alpha", "prod").await,
            Some(walked.clone()),
            "the indexed answer and the walk's answer are the same set"
        );

        // Now make one row disagree with the disk on everything the index
        // stores *except* its id.
        let mut stale = index
            .page(&ListPageQuery::new(
                ListKind::Task,
                ArtifactV2Service::list_scope(&scope),
                50,
            ))
            .expect("the index pages")
            .items
            .into_iter()
            .next()
            .expect("a row to spoil");
        stale.status = "cancelled".to_string();
        stale.title = Some("a title the manifest never had".to_string());
        stale.updated_at = 1;
        index.upsert(&stale).expect("the stale row writes");

        assert_eq!(
            api.valid_task_ids("alpha", "prod").await,
            Some(walked),
            "a stale row still names a task that exists, so the membership \
             set it belongs to must not move"
        );
    }

    /// **An index that is not ready declines, and the walk answers.**
    ///
    /// A never-rebuilt index is unready and holds no rows, which is the one
    /// failure here a reader could not detect: consulted anyway it would not
    /// error and would not look empty-and-broken, it would look like a
    /// complete index over a scope with no tasks — and every feed row
    /// pointing at a real task would be filtered away as an orphan.
    #[actix_web::test]
    async fn valid_task_ids_falls_back_to_the_walk_when_the_index_is_not_ready() {
        let tmp = tempfile::tempdir().expect("temporary Today workspace");
        let (api, _feed_store) = today_test_api(&tmp);
        let task = api
            .v3_service
            .create_task(today_task_fixture(None))
            .await
            .expect("a task");
        magician::magician_v2::test_support::wire_unready_test_list_index(&api.v3_service);

        let ids = api
            .valid_task_ids("alpha", "prod")
            .await
            .expect("the walk still answers");
        assert!(
            ids.contains(&task.manifest.task_id),
            "an unready index must be declined, not believed — believing it \
             here hides every feed row whose task is alive"
        );
    }

    /// 2031-03-15T02:00 in IST is still 2031-03-14 in UTC. A reader whose
    /// clock says the 15th and a server whose clock says the 14th are
    /// looking at the same instant, and must not be handed the same Today:
    /// a task due on the 15th is due TODAY for that reader and TOMORROW for
    /// the server's date.
    ///
    /// The dates are years from any real clock deliberately. If the resolved
    /// date ever falls back to `Utc::now()` while `today=` was sent, the
    /// task is neither due, nor overdue, nor stale, and drops out of
    /// Follow-ups entirely — so this fails loudly instead of coincidentally
    /// agreeing with whatever day the server is having.
    #[actix_web::test]
    async fn a_reader_east_of_utc_gets_their_own_today_not_the_servers() {
        let tmp = tempfile::tempdir().expect("temporary Today workspace");
        let (api, _feed_store) = today_test_api(&tmp);
        let task = api
            .v3_service
            .create_task(today_task_fixture(Some("2031-03-15")))
            .await
            .expect("a due-dated task");
        let item_id = format!("today:followups:task:{}", task.manifest.task_id);

        assert_eq!(
            today_followup_reason(&api, "2031-03-15", &item_id)
                .await
                .as_deref(),
            Some("This task is due today."),
            "the reader's own date decides their Today"
        );
        assert_eq!(
            today_followup_reason(&api, "2031-03-14", &item_id)
                .await
                .as_deref(),
            Some("This task is due tomorrow."),
            "the same instant one day west is a different day, and must say so"
        );
        assert_eq!(
            today_followup_reason(&api, "2031-03-16", &item_id)
                .await
                .as_deref(),
            Some("This task is overdue."),
            "a day later the same row is overdue, with no write to it at all"
        );
        assert_eq!(
            today_followup_reason(&api, "2031-03-01", &item_id).await,
            None,
            "a fortnight before it is due, the task is not a follow-up at all"
        );
    }

    /// Older clients do not send `today=`, and Today is polled. Falling back
    /// to the UTC date is exactly the pre-existing behaviour, so they are no
    /// worse off; a 400 on a polled endpoint would be worse than the
    /// imprecision it replaces.
    ///
    /// Asserted through the projection rather than the status line: a
    /// fallback that resolved to the epoch, or to `None`, would still answer
    /// 200 while quietly filing every task as overdue.
    #[actix_web::test]
    async fn an_absent_date_still_answers_from_the_clock() {
        let tmp = tempfile::tempdir().expect("temporary Today workspace");
        let (api, _feed_store) = today_test_api(&tmp);
        let utc_today = chrono::Utc::now().date_naive();
        let task = api
            .v3_service
            .create_task(today_task_fixture(Some(&utc_today.to_string())))
            .await
            .expect("a task due on the server's own date");
        let item_id = format!("today:followups:task:{}", task.manifest.task_id);

        let (status, body) = today_response(&api, "principal=alpha&workspace=prod").await;

        assert_eq!(status, actix_web::http::StatusCode::OK);
        let followups = body["sections"]["followups"]
            .as_array()
            .expect("Today response should carry a followups array");
        let row = followups
            .iter()
            .find(|item| item["id"].as_str() == Some(item_id.as_str()))
            .expect("a task due on the UTC date is a follow-up when no date is sent");
        assert_eq!(
            row["reason"].as_str(),
            Some("This task is due today."),
            "an absent date answers from the UTC date, exactly as before"
        );
    }

    /// A malformed date is rejected rather than quietly falling back to the
    /// UTC date, which would reproduce the defect `today=` exists to close
    /// and do it invisibly. Only a client that sends the parameter can meet
    /// this error, so no older client is affected.
    #[actix_web::test]
    async fn a_date_that_is_not_a_date_is_refused_rather_than_guessed() {
        let tmp = tempfile::tempdir().expect("temporary Today workspace");
        let (api, _feed_store) = today_test_api(&tmp);

        let (status, body) =
            today_response(&api, "principal=alpha&workspace=prod&today=31-07-2026").await;

        assert_eq!(status, actix_web::http::StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], json!("today_must_be_yyyy_mm_dd"));
    }

    /// An empty `today=` is what a client sends when its own date helper
    /// returned nothing. That is an absent date, not a malformed one.
    #[actix_web::test]
    async fn an_empty_date_is_treated_as_absent_not_as_malformed() {
        let tmp = tempfile::tempdir().expect("temporary Today workspace");
        let (api, _feed_store) = today_test_api(&tmp);

        let (status, _body) = today_response(&api, "principal=alpha&workspace=prod&today=").await;

        assert_eq!(status, actix_web::http::StatusCode::OK);
    }

    fn today_delivered_item(id: &str) -> FeedItem {
        let mut item = feed_item(id, FeedItemType::DataDelivery, None);
        item.status = FeedItemStatus::Done;
        item
    }

    /// Two reads in one window do one corpus walk.
    ///
    /// Proved by changing the corpus underneath the second read rather than by
    /// counting reads: a row written to the feed store between the two
    /// requests cannot appear in a response served from the projection the
    /// first request built. A read counter would still pass against a handler
    /// that bypassed the cache and merely happened to be quick.
    #[actix_web::test]
    async fn two_reads_in_one_window_do_one_corpus_walk() {
        let tmp = tempfile::tempdir().expect("temporary Today workspace");
        let (api, feed_store) = today_test_api(&tmp);
        feed_store
            .upsert_item(today_delivered_item("first-delivery"))
            .await
            .expect("first delivery stored");

        let (_, before) =
            today_response(&api, "principal=alpha&workspace=prod&today=2031-03-15").await;
        assert_eq!(
            today_section_ids(&before, "delivered"),
            vec!["today:delivered:first-delivery"]
        );

        feed_store
            .upsert_item(today_delivered_item("second-delivery"))
            .await
            .expect("second delivery stored");
        let (_, after) =
            today_response(&api, "principal=alpha&workspace=prod&today=2031-03-15").await;

        assert_eq!(
            today_section_ids(&after, "delivered"),
            vec!["today:delivered:first-delivery"],
            "a second read inside the window is served the projection the first one built"
        );
        // The one field that must differ is the one that says which of the two
        // happened. Everything else — generated_at, counts, digest, sections —
        // is the projection the first read built, so it is compared whole
        // rather than field by field: a response that changed anywhere else
        // did not come out of the cache.
        assert_eq!(
            before["freshness"]["source"],
            json!("live_projection"),
            "the first read computed the projection, and says so"
        );
        assert_eq!(
            after["freshness"]["source"],
            json!("cached_projection"),
            "the second was handed a projection built seconds ago and must not call itself live"
        );
        let without_freshness = |body: &Value| {
            let mut body = body.clone();
            body["freshness"] = Value::Null;
            body
        };
        assert_eq!(
            without_freshness(&after),
            without_freshness(&before),
            "and the rest is the same response — generated_at, counts and digest included"
        );

        // The same scope on a different date is a different projection, so the
        // row written a moment ago is visible there. This is the date in the
        // cache key proved through the handler, not only in the cache's own
        // unit tests.
        let (_, other_date) =
            today_response(&api, "principal=alpha&workspace=prod&today=2031-03-16").await;
        let mut other_date_ids = today_section_ids(&other_date, "delivered");
        other_date_ids.sort();
        assert_eq!(
            other_date_ids,
            vec![
                "today:delivered:first-delivery",
                "today:delivered:second-delivery"
            ],
            "a projection computed for one date must never serve another"
        );
    }

    /// A section page and the preview that preceded it come out of one walk,
    /// and paging still happens per request — the whole reason the five lanes
    /// are cached and the response is not.
    #[actix_web::test]
    async fn a_section_page_is_paged_per_request_from_the_shared_projection() {
        let tmp = tempfile::tempdir().expect("temporary Today workspace");
        let (api, feed_store) = today_test_api(&tmp);
        for index in 0..3 {
            feed_store
                .upsert_item(today_delivered_item(&format!("delivery-{index}")))
                .await
                .expect("delivery stored");
        }

        let (_, preview) =
            today_response(&api, "principal=alpha&workspace=prod&today=2031-03-15").await;
        assert_eq!(today_section_ids(&preview, "delivered").len(), 3);
        assert!(
            preview["section_page"].is_null(),
            "preview mode has no page"
        );

        let (status, page) = today_response(
            &api,
            "principal=alpha&workspace=prod&today=2031-03-15&section=delivered&limit=2",
        )
        .await;

        assert_eq!(status, actix_web::http::StatusCode::OK);
        assert_eq!(
            today_section_ids(&page, "delivered").len(),
            2,
            "the cached lanes are paged per request, not served whole"
        );
        assert_eq!(page["section_page"]["total"], json!(3));
        assert_eq!(page["section_page"]["has_more"], json!(true));
        assert_eq!(
            page["counts"]["delivered"], preview["counts"]["delivered"],
            "both responses count the same projection"
        );
    }

    async fn today_dismiss(api: &FeedApi, scope: &str, item_id: &str) {
        let request = actix_web::test::TestRequest::default()
            .insert_header(("X-Principal", "alpha"))
            .insert_header(("X-Workspace", "prod"))
            .to_http_request();
        let response = api
            .update_today_visibility(
                &request,
                web::Path::from(item_id.to_string()),
                web::Query::<TodayVisibilityQuery>::from_query(scope)
                    .expect("visibility query deserializes"),
                web::Json(TodayVisibilityRequest {
                    action: "dismiss".to_string(),
                    snooze_until: None,
                    snooze_minutes: None,
                    snapshot: None,
                }),
            )
            .await
            .expect("dismissal response");
        assert_eq!(response.status(), actix_web::http::StatusCode::OK);
    }

    /// Both clients refetch the page they are on after removing a row. Without
    /// invalidation that refetch is served a projection built before the
    /// dismissal, and the row the reader just dismissed comes back — strictly
    /// worse than the drain the refetch was added to fix.
    ///
    /// This is the test the cache exists to survive. It is only meaningful
    /// while the TTL is non-zero under test and the cache is genuinely on the
    /// read path; both are true, so a handler that skipped the invalidation
    /// fails here rather than passing quietly.
    #[actix_web::test]
    async fn a_dismissal_is_not_undone_by_the_refetch_that_follows_it() {
        let tmp = tempfile::tempdir().expect("temporary Today workspace");
        let (api, feed_store) = today_test_api(&tmp);
        for id in ["keep-me", "dismiss-me"] {
            feed_store
                .upsert_item(today_delivered_item(id))
                .await
                .expect("delivery stored");
        }
        let query = "principal=alpha&workspace=prod&today=2031-03-15";

        let (_, before) = today_response(&api, query).await;
        assert!(
            today_section_ids(&before, "delivered")
                .contains(&"today:delivered:dismiss-me".to_string()),
            "the row has to be there before it can be dismissed"
        );

        today_dismiss(
            &api,
            "principal=alpha&workspace=prod",
            "today:delivered:dismiss-me",
        )
        .await;

        // The client's own refetch, well inside the cache window.
        let (_, after) = today_response(&api, query).await;

        let ids = today_section_ids(&after, "delivered");
        assert!(
            !ids.contains(&"today:delivered:dismiss-me".to_string()),
            "a dismissed row must not return"
        );
        assert!(
            ids.contains(&"today:delivered:keep-me".to_string()),
            "and the rest of the page survives the invalidation"
        );
        assert_eq!(
            after["freshness"]["source"],
            json!("live_projection"),
            "the write dropped the entry, so this response really was recomputed"
        );
    }

    /// A write clears the writer's projection and nobody else's.
    ///
    /// Easy to get wrong by reaching for a clear-all, and expensive when it
    /// is: every other reader in the process pays a full corpus walk for one
    /// owner's tap. Asserted by changing the other reader's corpus after their
    /// projection was built — a row they still cannot see is proof their entry
    /// survived, which no assertion about the dismisser's own view could show.
    #[actix_web::test]
    async fn one_readers_dismissal_does_not_cost_another_reader_their_projection() {
        let tmp = tempfile::tempdir().expect("temporary Today workspace");
        let (api, feed_store) = today_test_api(&tmp);
        feed_store
            .upsert_item(today_delivered_item("alpha-row"))
            .await
            .expect("alpha delivery stored");
        let mut beta_row = today_delivered_item("beta-row");
        beta_row.principal = "beta".to_string();
        feed_store
            .upsert_item(beta_row)
            .await
            .expect("beta delivery stored");

        let (_, alpha_before) =
            today_response(&api, "principal=alpha&workspace=prod&today=2031-03-15").await;
        assert_eq!(
            today_section_ids(&alpha_before, "delivered"),
            vec!["today:delivered:alpha-row"]
        );
        let (_, beta_before) =
            today_response(&api, "principal=beta&workspace=prod&today=2031-03-15").await;
        assert_eq!(
            today_section_ids(&beta_before, "delivered"),
            vec!["today:delivered:beta-row"],
            "one reader must never see another's Today"
        );

        today_dismiss(
            &api,
            "principal=alpha&workspace=prod",
            "today:delivered:alpha-row",
        )
        .await;

        let mut beta_second_row = today_delivered_item("beta-row-2");
        beta_second_row.principal = "beta".to_string();
        feed_store
            .upsert_item(beta_second_row)
            .await
            .expect("second beta delivery stored");
        let (_, beta_after) =
            today_response(&api, "principal=beta&workspace=prod&today=2031-03-15").await;

        assert_eq!(
            today_section_ids(&beta_after, "delivered"),
            vec!["today:delivered:beta-row"],
            "one reader's write must not cost another reader their projection"
        );
        let (_, alpha_after) =
            today_response(&api, "principal=alpha&workspace=prod&today=2031-03-15").await;
        assert!(
            today_section_ids(&alpha_after, "delivered").is_empty(),
            "the writer's own projection was dropped, so their dismissal is visible at once"
        );
    }

    /// Restoring is a write too. The row has to come back on the refetch that
    /// follows, for the same reason a dismissal has to stick: the reader's own
    /// actions are never stale to them.
    #[actix_web::test]
    async fn a_restore_brings_the_row_back_on_the_next_read() {
        let tmp = tempfile::tempdir().expect("temporary Today workspace");
        let (api, feed_store) = today_test_api(&tmp);
        feed_store
            .upsert_item(today_delivered_item("restore-me"))
            .await
            .expect("delivery stored");
        let query = "principal=alpha&workspace=prod&today=2031-03-15";
        let scope = "principal=alpha&workspace=prod";
        let item_id = "today:delivered:restore-me";

        today_response(&api, query).await;
        today_dismiss(&api, scope, item_id).await;
        let (_, dismissed) = today_response(&api, query).await;
        assert!(today_section_ids(&dismissed, "delivered").is_empty());

        let request = actix_web::test::TestRequest::default()
            .insert_header(("X-Principal", "alpha"))
            .insert_header(("X-Workspace", "prod"))
            .to_http_request();
        let response = api
            .update_today_visibility(
                &request,
                web::Path::from(item_id.to_string()),
                web::Query::<TodayVisibilityQuery>::from_query(scope)
                    .expect("visibility query deserializes"),
                web::Json(TodayVisibilityRequest {
                    action: "restore".to_string(),
                    snooze_until: None,
                    snooze_minutes: None,
                    snapshot: None,
                }),
            )
            .await
            .expect("restore response");
        assert_eq!(response.status(), actix_web::http::StatusCode::OK);

        let (_, restored) = today_response(&api, query).await;
        assert_eq!(
            today_section_ids(&restored, "delivered"),
            vec![item_id],
            "a restore is a write, and the projection built before it is wrong"
        );
    }

    /// Deleting a feed row is the same failure as a dismissal on a different
    /// route: Today is projected out of these rows, the client refetches the
    /// page it is on, and a projection built before the delete hands the row
    /// straight back.
    #[actix_web::test]
    async fn a_deleted_row_does_not_return_on_the_refetch_that_follows_it() {
        let tmp = tempfile::tempdir().expect("temporary Today workspace");
        let (api, feed_store) = today_test_api(&tmp);
        for id in ["keep-me", "delete-me"] {
            feed_store
                .upsert_item(today_delivered_item(id))
                .await
                .expect("delivery stored");
        }
        let query = "principal=alpha&workspace=prod&today=2031-03-15";

        let (_, before) = today_response(&api, query).await;
        assert!(
            today_section_ids(&before, "delivered")
                .contains(&"today:delivered:delete-me".to_string()),
            "the row has to be there before it can be deleted"
        );

        let request = actix_web::test::TestRequest::default()
            .insert_header(("X-Principal", "alpha"))
            .insert_header(("X-Workspace", "prod"))
            .to_http_request();
        let response = api
            .delete_item(
                &request,
                web::Path::from("delete-me".to_string()),
                web::Query::<FeedCountsQuery>::from_query("principal=alpha&workspace=prod")
                    .expect("counts query deserializes"),
            )
            .await
            .expect("delete response");
        assert_eq!(response.status(), actix_web::http::StatusCode::OK);

        // The client's own refetch, well inside the cache window.
        let (_, after) = today_response(&api, query).await;
        assert_eq!(
            today_section_ids(&after, "delivered"),
            vec!["today:delivered:keep-me"],
            "a deleted row must not return, and the rest of the page survives"
        );
    }

    /// Clearing the feed removes every row at once. One invalidation covers
    /// the batch, and the read that follows has to see an empty lane rather
    /// than the projection built a moment before the clear.
    #[actix_web::test]
    async fn clearing_the_feed_leaves_today_empty_on_the_next_read() {
        let tmp = tempfile::tempdir().expect("temporary Today workspace");
        let (api, feed_store) = today_test_api(&tmp);
        for id in ["first-delivery", "second-delivery"] {
            feed_store
                .upsert_item(today_delivered_item(id))
                .await
                .expect("delivery stored");
        }
        let query = "principal=alpha&workspace=prod&today=2031-03-15";

        let (_, before) = today_response(&api, query).await;
        assert_eq!(today_section_ids(&before, "delivered").len(), 2);

        let request = actix_web::test::TestRequest::default()
            .insert_header(("X-Principal", "alpha"))
            .insert_header(("X-Workspace", "prod"))
            .to_http_request();
        let response = api
            .clear_items(
                &request,
                web::Query::<FeedListQuery>::from_query("principal=alpha&workspace=prod")
                    .expect("list query deserializes"),
            )
            .await
            .expect("clear response");
        assert_eq!(response.status(), actix_web::http::StatusCode::OK);

        let (_, after) = today_response(&api, query).await;
        assert!(
            today_section_ids(&after, "delivered").is_empty(),
            "a cleared feed must not still project the rows it held"
        );
    }

    /// Every learning action — confirm, edit-and-confirm, archive a
    /// candidate, archive an insight, save one to memory, turn one into a
    /// follow-up task — finishes by removing its feed row through
    /// `remove_feed_item_with_event`, and that is the one place they drop the
    /// projection. Asserted on the shared helper through a real `/today`
    /// read rather than six times over six learning-store fixtures: what has
    /// to hold is that the row does not come back on the refetch.
    #[actix_web::test]
    async fn a_row_removed_by_a_learning_action_does_not_return_on_the_refetch() {
        let tmp = tempfile::tempdir().expect("temporary Today workspace");
        let (api, feed_store) = today_test_api(&tmp);
        for id in ["keep-me", "archive-me"] {
            feed_store
                .upsert_item(today_delivered_item(id))
                .await
                .expect("delivery stored");
        }
        let query = "principal=alpha&workspace=prod&today=2031-03-15";

        let (_, before) = today_response(&api, query).await;
        assert!(
            today_section_ids(&before, "delivered")
                .contains(&"today:delivered:archive-me".to_string()),
            "the row has to be there before a learning action can remove it"
        );

        let removed = api
            .remove_feed_item_with_event("alpha", "prod", "archive-me")
            .await
            .expect("the row is removed");
        assert!(removed, "the helper reports the row it actually removed");

        let (_, after) = today_response(&api, query).await;
        assert_eq!(
            today_section_ids(&after, "delivered"),
            vec!["today:delivered:keep-me"],
            "the row a learning action removed must not come back on the refetch"
        );
    }

    fn today_test_scope() -> ScopeRef {
        ScopeRef::system_internal_unauthenticated(&"alpha".to_string(), &"prod".to_string())
    }

    /// Completing a task on `/tasks` never touches `FeedApi`, and Today's
    /// Follow-ups lane is built from the task corpus. Until the service
    /// dropped the projection itself, the row the reader had just ticked off
    /// stayed in their Today for the rest of the window — and the surface
    /// claimed in writing that it could not.
    #[actix_web::test]
    async fn a_task_completed_on_the_tasks_surface_leaves_todays_followups_at_once() {
        let tmp = tempfile::tempdir().expect("temporary Today workspace");
        let (api, _feed_store) = today_test_api(&tmp);
        let task = api
            .v3_service
            .create_task(today_task_fixture(Some("2031-03-15")))
            .await
            .expect("a due-dated task");
        let item_id = format!("today:followups:task:{}", task.manifest.task_id);
        let query = "principal=alpha&workspace=prod&today=2031-03-15";

        let (_, before) = today_response(&api, query).await;
        assert!(
            today_section_ids(&before, "followups").contains(&item_id),
            "a task due today has to be a follow-up before completing it can remove one"
        );

        api.v3_service
            .update_task_status(&today_test_scope(), &task.manifest.task_id, "completed")
            .await
            .expect("the task completes");

        // The client's own refetch, well inside the cache window.
        let (_, after) = today_response(&api, query).await;
        assert!(
            !today_section_ids(&after, "followups").contains(&item_id),
            "a completed task must not still be a follow-up on the refetch that follows"
        );
    }

    /// The other direction, and the one a hit-only fix would miss: a task
    /// created after the projection was built has to appear in it. Nothing in
    /// the Today API saw this write — it arrived through the task service,
    /// which is the whole reason the cache lives there.
    #[actix_web::test]
    async fn a_task_created_after_the_projection_was_built_still_reaches_today() {
        let tmp = tempfile::tempdir().expect("temporary Today workspace");
        let (api, _feed_store) = today_test_api(&tmp);
        let first = api
            .v3_service
            .create_task(today_task_fixture(Some("2031-03-15")))
            .await
            .expect("the first due-dated task");
        let query = "principal=alpha&workspace=prod&today=2031-03-15";

        let (_, before) = today_response(&api, query).await;
        assert_eq!(
            today_section_ids(&before, "followups"),
            vec![format!("today:followups:task:{}", first.manifest.task_id)],
            "the projection is built, and holds exactly the one task that existed"
        );

        let second = api
            .v3_service
            .create_task(today_task_fixture(Some("2031-03-15")))
            .await
            .expect("a second due-dated task");
        let second_id = format!("today:followups:task:{}", second.manifest.task_id);

        let (_, after) = today_response(&api, query).await;
        assert!(
            today_section_ids(&after, "followups").contains(&second_id),
            "a task created inside the window is due today too, and Today has to say so"
        );
    }

    /// The staleness check has been patched three times: `unwrap_or(false)`
    /// wedged a corrupt marker forever, the fail-open fix let a transient
    /// stat error steal a live reservation, and the current three-way split
    /// is the reconciliation. Each branch is pinned here so the next edit
    /// has to argue with a failing test instead of a changelog.
    #[test]
    fn follow_up_marker_staleness_splits_three_ways() {
        let dir = tempfile::tempdir().expect("marker dir");
        let path = dir.path().join("insight.follow_up_task.json");

        // Gone is the one unambiguous answer: reclaimable.
        assert!(
            follow_up_task_marker_is_stale(&path),
            "missing marker is stale"
        );

        // A fresh, live marker holds its claim.
        std::fs::write(&path, b"{}").expect("marker");
        assert!(
            !follow_up_task_marker_is_stale(&path),
            "fresh marker is live"
        );

        let set_mtime = |offset: i64| {
            let now = std::time::SystemTime::now();
            let at = if offset >= 0 {
                now + std::time::Duration::from_secs(offset as u64)
            } else {
                now - std::time::Duration::from_secs((-offset) as u64)
            };
            let file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("reopen marker");
            file.set_modified(at).expect("set mtime");
        };

        // Past the window: ordinary staleness.
        set_mtime(-20 * 60);
        assert!(
            follow_up_task_marker_is_stale(&path),
            "20-minute-old marker is stale"
        );

        // A future mtime inside the window is a small backwards clock step —
        // hold the claim; the clock catches up and staleness resumes.
        set_mtime(5 * 60);
        assert!(
            !follow_up_task_marker_is_stale(&path),
            "a small clock step must not reclaim a live marker"
        );

        // A future mtime beyond the window can never age out: that is a
        // corrupt timestamp, and holding it forever is the original wedge.
        set_mtime(20 * 60);
        assert!(
            follow_up_task_marker_is_stale(&path),
            "a timestamp beyond the window is corruption, not skew"
        );
    }

    /// The ambiguous arm: metadata that cannot be read holds the claim.
    /// Failing open here is the round-two bug — a transient stat error on a
    /// marker another request held live let a concurrent reserve delete it
    /// and create the follow-up task twice.
    #[cfg(unix)]
    #[test]
    fn follow_up_marker_with_unreadable_metadata_holds_its_claim() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("marker dir");
        let parent = dir.path().join("markers");
        std::fs::create_dir(&parent).expect("parent");
        let path = parent.join("insight.follow_up_task.json");
        std::fs::write(&path, b"{}").expect("marker");

        // Stripping traversal from the parent makes metadata() fail with
        // EACCES — ambiguous, so the claim must hold.
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o000))
            .expect("lock parent");
        let verdict = follow_up_task_marker_is_stale(&path);
        // Restore before asserting so a failure still lets tempdir clean up.
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755))
            .expect("unlock parent");
        assert!(
            !verdict,
            "an unreadable marker must hold its claim, not be stolen"
        );
    }
}

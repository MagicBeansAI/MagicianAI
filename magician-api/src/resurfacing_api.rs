//! Proactive Resurfacing HTTP API (Phase 1, Task 8).
//!
//! Two owner-facing endpoints, registered alongside the channel-assist routes
//! under `web::scope("/api/magician/v2")`:
//!
//! - `GET  /api/magician/v2/channel-assist/resurfacing/today` — the cards the
//!   curator has already marked `surfaced`, each shaped into a small,
//!   signal-driven card (`line` + a generic `why_now` phrase).
//! - `POST /api/magician/v2/channel-assist/resurfacing/{candidate_id}/action` —
//!   record owner feedback (`open` | `acknowledge` | `dismiss`), applying the
//!   store's state/cooldown transition.
//! - `GET  /api/magician/v2/channel-assist/resurfacing/stats` — per-lane engagement
//!   (P4c): positive/negative tallies + the Laplace-smoothed engagement rate and
//!   the computed utility multiplier the scorer re-weights each lane by, so "is
//!   this helping?" is visible.
//!
//! Scope-aware in the house style (`resolve_required_scope`). The store is
//! pulled from app state as `web::Data<ResurfacingStore>` (Clone/Arc-backed);
//! the worker/config that registers it into `app_data` lands in Task 9.

use std::collections::{BTreeMap, HashSet};
use std::str::FromStr;
use std::time::Instant;

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;

use crate::canonical_attention_api::LegacyWorthFollowUpOwners;
use crate::scope::resolve_required_scope;
use magician::config::AttentionGroupingMode;
use magician::magician_v2::attention::learning::{
    canonical_universe_digest, deserialize_semantic_envelope, rank_universe_digest,
    singleton_grouping_metadata, ActionabilityFeatureInput, AttentionDecision,
    AttentionDecisionContext, AttentionDecisionItem, AttentionGroupingResult,
    AttentionLabelQuality, AttentionLearningService, AttentionOutcomeAttribution,
    AttentionOutcomeKind, AttentionPairCandidateRef, AttentionRankMetadata, AttentionRoute,
    AttentionRoutingCandidate, AttentionRoutingEvaluation, AttentionRoutingHealth,
    AttentionSurface, GroupingFeatureInput, RecordAttentionOutcome, SemanticAttentionCandidate,
    SemanticEmbedding,
};
use magician::magician_v2::attention::resurfacing::interaction::{
    ResurfacingActionKind, ResurfacingRecommendation,
};
use magician::magician_v2::attention::resurfacing::scoring::{
    utility_multiplier, utility_rate, ResurfacingScoringConfig,
};
use magician::magician_v2::attention::resurfacing::store::{
    ResurfacingRecommendationInteractionResult, ResurfacingStore, SurfacedCursor,
};
use magician::magician_v2::attention::resurfacing::types::{
    Candidate, DismissReason, FeedbackAction, SalienceSignals, SourceKind,
};
use magician::magician_v2::attention_funnel::{
    AttentionFunnelStage, AttentionLane, AttentionRouteEvent, AttentionScope, AttentionTraceStatus,
    RouteOutcome,
};
use magician::magician_v2::attention_funnel_store::AttentionFunnelStore;
use magician::magician_v2::attention_lane_facade::{AttentionLanePage, AttentionLaneQuery};
use magician_comms::channel_assist::attention_lane_bridge::{
    decode_resurfacing_attention_cursor, encode_resurfacing_attention_cursor,
    list_resurfacing_attention_lane_candidates,
};
use magician_comms::channel_assist::attention_learning::SemanticExtractionWorker;
use magician_comms::channel_assist::channel::ChannelAssistStore;
use magician_comms::channel_assist::resurfacing::actions::{
    ResurfacingActionError, ResurfacingActionService, ResurfacingContextualActionRequest,
};
use magician_comms::channel_assist::resurfacing::curator::{
    attention_candidate_from_resurfacing, repair_active_surfaced_candidates,
};
use magician_comms::channel_assist::resurfacing::interaction::{
    static_read_capabilities, ResolvedResurfacingDetail, ResurfacingActionCapability,
    ResurfacingInteractionRegistry, ResurfacingSourceStatus,
};

/// Default cards returned by the `today` read — a Today band shows a handful of
/// the most salient items per server-side page, not the whole backlog.
const TODAY_CARD_DEFAULT_LIMIT: usize = 20;

/// Hard cap for one `today` page. Keeps accidental large requests from turning
/// the owner surface into a full-table dump.
const TODAY_CARD_MAX_LIMIT: usize = 100;

/// Raw store page size for a Worth-a-look scan under the cross-lane identity
/// guard. The facade caps lane reads at 200, so keeping this at that bound
/// makes every continuation explicit. A paged read pulls one of these and stops
/// once its page is full; only the group-members read drains the lane.
const LEGACY_WORTH_RECONCILIATION_SCAN_LIMIT: usize = 200;

/// Max recent run records returned by the observability read (O2). A rolling
/// window big enough to inspect engine health across the last several passes of
/// each kind, without dumping the whole capped history.
const OBSERVABILITY_RUN_LIMIT: usize = 50;

/// Cooldown applied after a dismiss (~3 weeks) before a candidate can
/// resurface. Mirrors the store's dismiss transition contract.
const DISMISS_COOLDOWN_SECS: i64 =
    magician::magician_v2::attention::resurfacing::types::DEFAULT_DISMISS_COOLDOWN_SECS;

/// Cooldown applied after an open/acknowledge (~2 months) — a positive action
/// means the owner has engaged with the item, so hold it back longer than a
/// dismiss.
const ACK_COOLDOWN_SECS: i64 =
    magician::magician_v2::attention::resurfacing::types::DEFAULT_ACK_COOLDOWN_SECS;

/// Legacy query shape retained for wire compatibility. Scope is resolved from
/// the middleware-verified bearer identity by [`resolve_required_scope`].
#[derive(Debug, Deserialize)]
pub struct ScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ActiveRepairQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Query-string scope + pagination for `GET /channel-assist/resurfacing/today`.
#[derive(Debug, Deserialize)]
pub struct TodayQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub cursor_surfaced_at: Option<i64>,
    #[serde(default)]
    pub cursor_score: Option<f64>,
    #[serde(default)]
    pub cursor_candidate_id: Option<String>,
}

/// JSON body for the action endpoint: `{ "action": "open"|"acknowledge"|"dismiss" }`.
#[derive(Debug, Deserialize)]
pub struct ActionBody {
    pub action: String,
    /// Optional dismiss reason (channel-assist vocabulary: spam / already_handled
    /// / duplicate / delegated / not_relevant). Ignored for open/acknowledge. An
    /// unknown/absent value is a plain reasonless dismiss.
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub attribution: Option<AttentionOutcomeAttribution>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationInteractionEvent {
    Presented,
    Selected,
    Completed,
}

impl RecommendationInteractionEvent {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Presented => "presented",
            Self::Selected => "selected",
            Self::Completed => "completed",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecommendationInteractionBody {
    pub kind: ResurfacingActionKind,
    #[serde(default)]
    pub content_revision: Option<String>,
    pub event: RecommendationInteractionEvent,
}

fn err_json(status: actix_web::http::StatusCode, message: impl std::fmt::Display) -> HttpResponse {
    HttpResponse::build(status).json(serde_json::json!({ "error": message.to_string() }))
}

fn err_json_code(
    status: actix_web::http::StatusCode,
    code: &str,
    message: impl std::fmt::Display,
) -> HttpResponse {
    HttpResponse::build(status).json(serde_json::json!({
        "code": code,
        "error": message.to_string(),
    }))
}

/// Shared with the follow-up lane so both map the action error taxonomy to the
/// same status codes and stable error codes.
pub(super) fn contextual_action_error_response(
    candidate_id: &str,
    error: ResurfacingActionError,
) -> HttpResponse {
    let code = error.code();
    match error {
        ResurfacingActionError::Invalid(message) => {
            err_json_code(actix_web::http::StatusCode::BAD_REQUEST, code, message)
        },
        ResurfacingActionError::NotFound => err_json_code(
            actix_web::http::StatusCode::NOT_FOUND,
            code,
            "resurfacing candidate not found",
        ),
        ResurfacingActionError::StaleRevision(message)
        | ResurfacingActionError::NotActionable(message) => {
            err_json_code(actix_web::http::StatusCode::CONFLICT, code, message)
        },
        error @ (ResurfacingActionError::InProgress
        | ResurfacingActionError::IdempotencyConflict) => {
            err_json_code(actix_web::http::StatusCode::CONFLICT, code, error)
        },
        ResurfacingActionError::Unavailable(message) => err_json_code(
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            code,
            message,
        ),
        ResurfacingActionError::Internal(error) => {
            tracing::warn!(
                candidate_id,
                error = %error,
                "resurfacing contextual action failed"
            );
            err_json_code(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                code,
                "resurfacing contextual action failed",
            )
        },
    }
}

fn normalize_today_limit(limit: Option<usize>) -> usize {
    limit
        .unwrap_or(TODAY_CARD_DEFAULT_LIMIT)
        .clamp(1, TODAY_CARD_MAX_LIMIT)
}

fn parse_today_cursor(query: &TodayQuery) -> Result<Option<SurfacedCursor>, String> {
    let any = query.cursor_surfaced_at.is_some()
        || query.cursor_score.is_some()
        || query.cursor_candidate_id.is_some();
    if !any {
        return Ok(None);
    }

    let surfaced_at = query
        .cursor_surfaced_at
        .ok_or_else(|| "cursor_surfaced_at is required when paging by cursor".to_string())?;
    let salience_score = query
        .cursor_score
        .ok_or_else(|| "cursor_score is required when paging by cursor".to_string())?;
    if !salience_score.is_finite() {
        return Err("cursor_score must be finite".to_string());
    }
    let candidate_id = query
        .cursor_candidate_id
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_string();
    if candidate_id.is_empty() {
        return Err("cursor_candidate_id is required when paging by cursor".to_string());
    }

    Ok(Some(SurfacedCursor {
        surfaced_at,
        salience_score,
        candidate_id,
    }))
}

fn parse_today_cursor_token(query: &TodayQuery) -> Result<Option<String>, String> {
    let opaque_cursor = query
        .cursor
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let legacy_cursor = query.cursor_surfaced_at.is_some()
        || query.cursor_score.is_some()
        || query.cursor_candidate_id.is_some();
    if opaque_cursor.is_some() && legacy_cursor {
        return Err("use either cursor or cursor_* pagination fields, not both".to_string());
    }
    if let Some(cursor) = opaque_cursor {
        decode_resurfacing_attention_cursor(cursor).map_err(|err| err.to_string())?;
        return Ok(Some(cursor.to_string()));
    }
    parse_today_cursor(query)
        .map(|cursor| cursor.map(|cursor| encode_resurfacing_attention_cursor(&cursor)))
}

/// Fail one legacy Worth read closed, with the typed reason its error names.
///
/// Every cross-lane failure on this path answers the same way — an empty
/// payload under the key the endpoint returns, plus unavailable reconciliation
/// health — so the shape is written once rather than at each `match`.
fn legacy_worth_reconciliation_unavailable(
    principal: &str,
    workspace: &str,
    error: &anyhow::Error,
    items_key: &str,
    message: &'static str,
) -> HttpResponse {
    tracing::warn!(principal, workspace, error = %error, "{message}");
    let reason = crate::canonical_attention_api::legacy_worth_reconciliation_error_reason(error);
    let health = crate::canonical_attention_api::unavailable_legacy_worth_reconciliation(
        principal, workspace, reason,
    );
    let mut body = serde_json::Map::new();
    body.insert(
        "error".to_string(),
        serde_json::Value::String("cross_lane_reconciliation_unavailable".to_string()),
    );
    body.insert(items_key.to_string(), serde_json::Value::Array(Vec::new()));
    body.insert(
        "cross_lane_reconciliation".to_string(),
        serde_json::json!(health),
    );
    HttpResponse::ServiceUnavailable().json(serde_json::Value::Object(body))
}

/// How many cursor pages one Worth-a-look scan may pull for a lane holding
/// `total` rows, with one page of slack for rows written during the scan.
fn legacy_worth_scan_page_bound(total: usize) -> usize {
    total
        .saturating_add(LEGACY_WORTH_RECONCILIATION_SCAN_LIMIT - 1)
        .saturating_div(LEGACY_WORTH_RECONCILIATION_SCAN_LIMIT)
        .saturating_add(1)
        .max(1)
}

/// Drain the whole surfaced Worth-a-look lane in lane order.
///
/// Only the group-members read needs this: it ranks and groups the complete
/// visible universe to resolve one cluster, so no page can answer it. The paged
/// Today read no longer accumulates the lane — see
/// [`VisibleWorthPage`], which seeks and stops.
///
/// The per-page `page.total == total` assert this used to carry hard-errored
/// the entire read whenever anything wrote to the lane mid-scan. A total moving
/// between pages is ordinary in a live lane and says nothing about whether the
/// scan is sound; de-duplication by candidate id, the continuation-cursor
/// check, and the page bound do.
async fn drain_legacy_worth_universe(
    store: &ResurfacingStore,
    principal: &str,
    workspace: &str,
) -> anyhow::Result<Vec<Candidate>> {
    let mut candidates = Vec::new();
    let mut seen_candidate_ids = HashSet::new();
    let mut cursor: Option<String> = None;
    let mut pages_scanned = 0usize;
    let mut page_bound = 1usize;

    loop {
        let page = list_resurfacing_attention_lane_candidates(
            store,
            AttentionLaneQuery {
                principal,
                workspace,
                lane: AttentionLane::WorthALook,
                cursor: cursor.as_deref(),
                offset: 0,
                limit: LEGACY_WORTH_RECONCILIATION_SCAN_LIMIT,
            },
        )
        .await?;
        pages_scanned = pages_scanned.saturating_add(1);
        page_bound = page_bound.max(legacy_worth_scan_page_bound(page.total));
        anyhow::ensure!(
            pages_scanned <= page_bound,
            "Worth-a-look cursor scan exceeded its bounded source universe"
        );

        for candidate in page.items {
            anyhow::ensure!(
                seen_candidate_ids.insert(candidate.candidate_id.clone()),
                "Worth-a-look cursor scan repeated candidate {}",
                candidate.candidate_id
            );
            candidates.push(candidate);
        }

        if !page.has_more {
            return Ok(candidates);
        }

        let next_cursor = page
            .next_cursor
            .ok_or_else(|| anyhow::anyhow!("Worth-a-look cursor scan made no progress"))?;
        anyhow::ensure!(
            cursor.as_deref() != Some(next_cursor.as_str()),
            "Worth-a-look cursor scan repeated its continuation cursor"
        );
        cursor = Some(next_cursor);
    }
}

fn candidate_surfaced_cursor(candidate: &Candidate) -> SurfacedCursor {
    SurfacedCursor {
        surfaced_at: candidate.last_surfaced_at.unwrap_or(0),
        salience_score: f64::from(candidate.salience_score),
        candidate_id: candidate.candidate_id.clone(),
    }
}

/// One page of visible Worth-a-look rows, and the raw rows it examined to build
/// it.
struct VisibleWorthPage {
    page: AttentionLanePage<Candidate>,
    /// Every raw row the page examined, in lane order, with whether an active
    /// Follow-up owned it. The page's hidden count is these, counted.
    scanned: Vec<(Candidate, bool)>,
}

/// The bookkeeping a Worth-a-look page performs over raw lane rows arriving in
/// lane order.
///
/// Cross-lane de-duplication hides a row on a per-candidate decision, so a page
/// is decided by walking forward from its cursor until it is full — never by
/// accumulating the lane and slicing it. This is the part with arithmetic in
/// it, so it is a pure builder the caller feeds; the walk that feeds it is
/// [`collect_visible_legacy_worth_page`].
struct VisibleWorthPageBuilder {
    limit: usize,
    visible_to_skip: usize,
    items: Vec<Candidate>,
    scanned: Vec<(Candidate, bool)>,
    next_cursor: Option<String>,
    has_more: bool,
}

impl VisibleWorthPageBuilder {
    /// `offset` skips VISIBLE rows, matching what a caller means by it. A
    /// cursor already names its own position, so the two are never combined.
    fn new(cursor_is_set: bool, offset: usize, limit: usize) -> Self {
        Self {
            limit: normalize_today_limit(Some(limit)),
            visible_to_skip: if cursor_is_set { 0 } else { offset },
            items: Vec::new(),
            scanned: Vec::new(),
            next_cursor: None,
            has_more: false,
        }
    }

    /// Offer one raw lane row. Returns whether the scan should continue: once a
    /// visible row is seen past a full page, nothing further can change the
    /// answer.
    fn push(&mut self, candidate: &Candidate, owned_by_follow_up: bool) -> bool {
        if self.visible_to_skip > 0 {
            if !owned_by_follow_up {
                self.visible_to_skip -= 1;
            }
            return true;
        }
        if self.items.len() == self.limit {
            // Look-ahead only. Rows past the page are not rows the page
            // scanned, so they are deliberately not counted below.
            if !owned_by_follow_up {
                self.has_more = true;
                return false;
            }
            return true;
        }
        self.scanned.push((candidate.clone(), owned_by_follow_up));
        if !owned_by_follow_up {
            self.next_cursor = Some(encode_resurfacing_attention_cursor(
                &candidate_surfaced_cursor(candidate),
            ));
            self.items.push(candidate.clone());
        }
        true
    }

    fn finish(self, visible_total: usize, cursor: Option<&str>) -> VisibleWorthPage {
        let has_more = self.has_more;
        VisibleWorthPage {
            page: AttentionLanePage {
                lane: AttentionLane::WorthALook,
                items: self.items,
                total: visible_total,
                request_hitl_total: None,
                limit: self.limit,
                cursor: cursor.map(str::to_string),
                next_cursor: has_more.then_some(self.next_cursor).flatten(),
                has_more,
            },
            scanned: self.scanned,
        }
    }
}

/// Seek to the requested page and walk forward only as far as it needs.
///
/// The store's lane API already seeks by cursor and already knows the lane's
/// size. This asks it for pages until the requested page is full, so page one
/// costs one round trip rather than one per page in the lane.
#[allow(clippy::too_many_arguments)]
async fn collect_visible_legacy_worth_page(
    store: &ResurfacingStore,
    owners: &LegacyWorthFollowUpOwners,
    principal: &str,
    workspace: &str,
    cursor: Option<&str>,
    offset: usize,
    limit: usize,
    visible_total: usize,
) -> anyhow::Result<VisibleWorthPage> {
    let mut builder = VisibleWorthPageBuilder::new(cursor.is_some(), offset, limit);
    let mut scan_cursor: Option<String> = cursor.map(ToOwned::to_owned);
    let mut pages_scanned = 0usize;
    let mut page_bound = 1usize;

    loop {
        let page = list_resurfacing_attention_lane_candidates(
            store,
            AttentionLaneQuery {
                principal,
                workspace,
                lane: AttentionLane::WorthALook,
                cursor: scan_cursor.as_deref(),
                offset: 0,
                limit: LEGACY_WORTH_RECONCILIATION_SCAN_LIMIT,
            },
        )
        .await?;
        pages_scanned = pages_scanned.saturating_add(1);
        page_bound = page_bound.max(legacy_worth_scan_page_bound(page.total));
        anyhow::ensure!(
            pages_scanned <= page_bound,
            "Worth-a-look lane page scan exceeded its bounded source universe"
        );

        for candidate in &page.items {
            if !builder.push(candidate, owners.owns(candidate)?) {
                return Ok(builder.finish(visible_total, cursor));
            }
        }

        if !page.has_more {
            return Ok(builder.finish(visible_total, cursor));
        }
        let continuation = page
            .next_cursor
            .ok_or_else(|| anyhow::anyhow!("Worth-a-look lane page scan made no progress"))?;
        anyhow::ensure!(
            scan_cursor.as_deref() != Some(continuation.as_str()),
            "Worth-a-look lane page scan repeated its continuation cursor"
        );
        scan_cursor = Some(continuation);
    }
}

/// A generic, purely signal-driven "why now" phrase for a surfaced card. There
/// is NO category/topic logic here — the phrase describes the *signal shape*,
/// never the item's subject. Recency is deliberately the final fallback so stale
/// data or deterministic fallback rows do not explain themselves as merely
/// "recently active".
fn why_now(signals: &SalienceSignals) -> &'static str {
    // (phrase, value) in a fixed priority order; the max value wins.
    let ranked: [(&'static str, f32); 6] = [
        ("you keep coming back to this", signals.centrality),
        ("a relevant date is near", signals.temporal_anchor),
        ("you haven't returned to this in a while", signals.dormancy),
        ("you reference this often", signals.frequency),
        ("connected to other active threads", signals.cooccurrence),
        ("from a source you chose to follow", signals.source_affinity),
    ];
    let best = ranked.iter().fold(
        ranked[0],
        |best, &cur| if cur.1 > best.1 { cur } else { best },
    );
    if best.1 > 0.0 {
        best.0
    } else if signals.recency > 0.0 {
        "fresh context may connect to active work"
    } else {
        "may be worth revisiting"
    }
}

fn detail_label_for_source(source_kind: SourceKind) -> &'static str {
    match source_kind {
        SourceKind::Comm => "Message summary",
        SourceKind::Memory => "Memory summary",
        SourceKind::Task => "Task summary",
        SourceKind::Episode => "Episode summary",
        SourceKind::Calendar => "Calendar context",
        SourceKind::Note => "Note summary",
        SourceKind::Web => "Web source summary",
    }
}

async fn visible_recommendation(
    store: &ResurfacingStore,
    interactions: Option<&ResurfacingInteractionRegistry>,
    principal: &str,
    workspace: &str,
    candidate: &Candidate,
    capabilities: &[ResurfacingActionCapability],
) -> Option<ResurfacingRecommendation> {
    // Single-candidate read path (detail views). List paths must NOT use this
    // one per card — see `visible_recommendation_from`.
    interactions.filter(|registry| registry.recommendations_enabled())?;
    let recommendation = match store
        .get_recommendation(principal, workspace, &candidate.candidate_id)
        .await
    {
        Ok(recommendation) => recommendation,
        Err(error) => {
            tracing::warn!(
                candidate_id = %candidate.candidate_id,
                error = %error,
                "failed to read resurfacing recommendation"
            );
            None
        },
    };
    visible_recommendation_from(interactions, candidate, capabilities, recommendation)
}

/// The visibility half of [`visible_recommendation`], over a recommendation
/// the caller already has.
///
/// List endpoints fetch every card's recommendation in one batched query and
/// then run this per card. Doing the read inside the per-card loop instead
/// meant one `spawn_blocking` hop and one store-mutex acquisition per card,
/// serialised — a hundred round trips inside a single request for the
/// hundred-card lookup the Today page issues on mount.
fn visible_recommendation_from(
    interactions: Option<&ResurfacingInteractionRegistry>,
    candidate: &Candidate,
    capabilities: &[ResurfacingActionCapability],
    recommendation: Option<ResurfacingRecommendation>,
) -> Option<ResurfacingRecommendation> {
    let registry = interactions.filter(|registry| registry.recommendations_enabled())?;
    let recommendation = recommendation?;
    registry
        .recommendation_is_visible(candidate, capabilities, &recommendation)
        .then_some(recommendation)
}

const LEARNED_WORTH_CURSOR_PREFIX: &str = "learned-worth-v1:";

fn decode_learned_worth_cursor(cursor: &str) -> Result<(u64, String, String), String> {
    let raw = cursor
        .strip_prefix(LEARNED_WORTH_CURSOR_PREFIX)
        .ok_or_else(|| "learned ranking requires a learned-worth-v1 cursor; refresh".to_string())?;
    let mut parts = raw.splitn(3, ':');
    let generation = parts
        .next()
        .ok_or_else(|| "invalid learned Worth-a-look cursor".to_string())?;
    let generation = generation
        .parse::<u64>()
        .map_err(|_| "invalid learned Worth-a-look cursor generation".to_string())?;
    let universe_digest = parts
        .next()
        .filter(|digest| !digest.is_empty())
        .ok_or_else(|| "invalid learned Worth-a-look cursor universe".to_string())?
        .to_string();
    let candidate_id = parts
        .next()
        .ok_or_else(|| "invalid learned Worth-a-look cursor candidate".to_string())?;
    let candidate_id = urlencoding::decode(candidate_id)
        .map_err(|_| "invalid learned Worth-a-look cursor candidate".to_string())?
        .into_owned();
    if candidate_id.trim().is_empty() {
        return Err("invalid learned Worth-a-look cursor candidate".to_string());
    }
    Ok((generation, universe_digest, candidate_id))
}

pub(super) fn resurfacing_semantic_candidate(
    candidate: &Candidate,
    embedding: Option<SemanticEmbedding>,
) -> SemanticAttentionCandidate {
    let details = candidate
        .content_details
        .as_ref()
        .and_then(|details| serde_json::to_string(details).ok())
        .unwrap_or_default();
    let grouping_embedding = embedding.clone();
    SemanticAttentionCandidate {
        candidate_id: candidate.candidate_id.clone(),
        source_revision: candidate
            .content_revision
            .clone()
            .or_else(|| Some(candidate.content_digest.clone())),
        semantic_text: [
            candidate.title.as_str(),
            candidate.content_digest.as_str(),
            details.as_str(),
        ]
        .join("\n"),
        existing_embedding: embedding,
        actionability_features: Some(ActionabilityFeatureInput {
            semantic: deserialize_semantic_envelope(candidate.semantic_features.as_ref()),
            age_days: candidate.last_surfaced_at.map(|surfaced_at| {
                (chrono::Utc::now()
                    .timestamp_millis()
                    .saturating_sub(surfaced_at))
                .max(0) as f64
                    / 86_400_000.0
            }),
            slice1_actionability_probability: Some(candidate.salience_score.clamp(0.0, 1.0) as f64),
            ..Default::default()
        }),
        grouping_features: Some(GroupingFeatureInput {
            embedding: grouping_embedding,
            exact_source_identity: Some(format!(
                "{}:{}",
                candidate.source_kind.as_str(),
                candidate.source_ref
            )),
            event_at_ms: candidate.temporal_anchor_at,
            ..Default::default()
        }),
    }
}

async fn resurfacing_learning_cohort(
    store: &ResurfacingStore,
    principal: &str,
    workspace: &str,
    limit: usize,
) -> anyhow::Result<Vec<SemanticAttentionCandidate>> {
    let candidates = store
        .list_surfaced_page(principal, workspace, limit.max(1), 0, None)
        .await?
        .candidates;
    let ids: Vec<String> = candidates
        .iter()
        .map(|candidate| candidate.candidate_id.clone())
        .collect();
    let embeddings = store
        .list_candidate_embedding_snapshots_for_ids(principal, workspace, &ids)
        .await?;
    let embeddings: std::collections::HashMap<String, SemanticEmbedding> = embeddings
        .into_iter()
        .map(|embedding| {
            (
                embedding.candidate_id,
                SemanticEmbedding {
                    contract: embedding.embedding_contract,
                    vector: embedding.embedding,
                },
            )
        })
        .collect();
    Ok(candidates
        .iter()
        .map(|candidate| {
            resurfacing_semantic_candidate(
                candidate,
                embeddings.get(&candidate.candidate_id).cloned(),
            )
        })
        .collect())
}

struct WorthLearningProjection {
    universe_ids: Vec<String>,
    ranks: Vec<AttentionRankMetadata>,
    generation: u64,
    rank_scope: &'static str,
    grouping: AttentionGroupingResult,
    routing: AttentionRoutingEvaluation,
    decision: AttentionDecision,
    routing_health: AttentionRoutingHealth,
}

async fn project_worth_learning(
    store: &ResurfacingStore,
    learning: &AttentionLearningService,
    principal: &str,
    workspace: &str,
    limit: usize,
    offset: usize,
    cursor: Option<&str>,
    selected_candidate_ids: Option<&HashSet<String>>,
) -> Result<WorthLearningProjection, (actix_web::http::StatusCode, String)> {
    let decision_started = Instant::now();
    let probe = store
        .list_surfaced_page(principal, workspace, 1, 0, None)
        .await
        .map_err(|error| {
            (
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            )
        })?;
    let universe = store
        .list_surfaced_page(principal, workspace, probe.total.max(1) as usize, 0, None)
        .await
        .map_err(|error| {
            (
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            )
        })?;
    let universe_candidates: Vec<SemanticAttentionCandidate> = universe
        .candidates
        .iter()
        .map(|candidate| resurfacing_semantic_candidate(candidate, None))
        .collect();
    let source_family_by_id: std::collections::HashMap<String, String> = universe
        .candidates
        .iter()
        .map(|candidate| {
            (
                candidate.candidate_id.clone(),
                candidate.source_kind.as_str().to_string(),
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
            AttentionSurface::WorthALook,
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
            AttentionSurface::WorthALook,
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
    let rank_by_id: std::collections::HashMap<&str, usize> = ranks
        .iter()
        .map(|rank| (rank.candidate_id.as_str(), rank.learned_rank))
        .collect();
    let mut ordered = universe.candidates;
    if grouping.mode == AttentionGroupingMode::Enforced
        || learning
            .serving_ranking_without_grouping_enabled_for(principal, workspace)
            .await
            .unwrap_or(false)
    {
        ordered.sort_by_key(|candidate| {
            rank_by_id
                .get(candidate.candidate_id.as_str())
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
        ordered.retain(|candidate| representatives.contains(candidate.candidate_id.as_str()));
    }
    let served_candidate_ids: Vec<String> = ordered
        .iter()
        .map(|candidate| candidate.candidate_id.clone())
        .collect();
    let start = if let Some(cursor) = cursor {
        let (cursor_generation, cursor_universe, last_id) = decode_learned_worth_cursor(cursor)
            .map_err(|error| (actix_web::http::StatusCode::BAD_REQUEST, error))?;
        if cursor_generation != generation || cursor_universe != universe_digest {
            return Err((
                actix_web::http::StatusCode::CONFLICT,
                "attention ranking changed; refresh from the first page".to_string(),
            ));
        }
        ordered
            .iter()
            .position(|candidate| candidate.candidate_id == last_id)
            .map(|index| index + 1)
            .ok_or_else(|| {
                (
                    actix_web::http::StatusCode::CONFLICT,
                    "attention cursor item is no longer active; refresh".to_string(),
                )
            })?
    } else {
        offset.min(ordered.len())
    };
    let end = start.saturating_add(limit).min(ordered.len());
    let candidates = ordered[start..end].to_vec();
    let page_candidate_ids: HashSet<String> = candidates
        .iter()
        .map(|candidate| candidate.candidate_id.clone())
        .collect();
    let selected_candidate_ids = selected_candidate_ids.unwrap_or(&page_candidate_ids);
    let has_more = end < ordered.len();
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
                baseline_route: AttentionRoute::WorthALook,
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
            "attention routing inputs did not reconcile with the Worth-a-look universe".to_string(),
        ));
    }
    let mut routing = learning
        .evaluate_routing_universe(
            principal,
            workspace,
            AttentionSurface::WorthALook,
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
    Ok(WorthLearningProjection {
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

/// `GET /channel-assist/resurfacing/today`.
pub async fn get_resurfacing_today_handler(
    store: web::Data<ResurfacingStore>,
    interactions: Option<web::Data<ResurfacingInteractionRegistry>>,
    learning: Option<web::Data<AttentionLearningService>>,
    semantic_worker: Option<web::Data<SemanticExtractionWorker>>,
    mail_store: Option<web::Data<ChannelAssistStore>>,
    memory_api: Option<web::Data<crate::memory_api::MemoryApi>>,
    req: HttpRequest,
    query: web::Query<TodayQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };

    let limit = normalize_today_limit(query.limit);
    let offset = query.offset.unwrap_or(0);
    let canonical_attention_projection = if let (Some(learning), Some(mail_store)) =
        (learning.as_ref(), mail_store.as_ref())
    {
        match crate::canonical_attention_api::project_canonical_attention_union(
            mail_store,
            &store,
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
                    "canonical attention union unavailable; serving atomic legacy Worth-a-look baseline"
                );
                None
            },
        }
    } else {
        None
    };
    // The legacy endpoint remains an atomic baseline projection, while learned
    // order is served by the canonical union. Report the effective canonical
    // state rather than incorrectly equating a disabled legacy ranking path
    // with observe mode.
    let semantic_ranking_enabled =
        canonical_attention_projection
            .as_ref()
            .is_some_and(|projection| {
                crate::canonical_attention_api::canonical_slice1_order_is_active(
                    projection.status,
                    projection.integrity.load_complete,
                    learning
                        .as_ref()
                        .is_some_and(|learning| learning.semantic_ranking_enabled()),
                )
            });
    let cursor = match parse_today_cursor_token(&query) {
        Ok(cursor) => cursor,
        Err(err) => return err_json(actix_web::http::StatusCode::BAD_REQUEST, err),
    };

    let mut learning_projection = None;
    let Some(mail_store) = mail_store.as_ref() else {
        let health = crate::canonical_attention_api::unavailable_legacy_worth_reconciliation(
            &principal,
            &workspace,
            crate::canonical_attention_api::CrossLaneReconciliationReason::FollowUpStoreUnavailable,
        );
        return HttpResponse::ServiceUnavailable().json(serde_json::json!({
            "error": "cross_lane_reconciliation_unavailable",
            "cards": [],
            "cross_lane_reconciliation": health,
        }));
    };
    // Everything cross-lane de-duplication needs, and nothing more: the active
    // Follow-up owner set, and one count of how much of the lane it hides.
    // Neither is a page of the lane, and neither grows with the page requested.
    let owners = match crate::canonical_attention_api::load_active_follow_up_owners(
        mail_store, &principal, &workspace,
    )
    .await
    {
        Ok(owners) => owners,
        Err(error) => {
            return legacy_worth_reconciliation_unavailable(
                &principal,
                &workspace,
                &error,
                "cards",
                "legacy Worth-a-look Follow-up owner set unavailable",
            );
        },
    };
    let lane_identities = match store
        .list_surfaced_comm_source_refs(&principal, &workspace)
        .await
    {
        Ok(identities) => identities,
        Err(error) => {
            return legacy_worth_reconciliation_unavailable(
                &principal,
                &workspace,
                &error.context("reading Worth-a-look lane totals"),
                "cards",
                "legacy Worth-a-look source count unavailable",
            );
        },
    };
    let duplicate_hidden_total =
        match owners.count_owned_source_refs(&lane_identities.comm_source_refs) {
            Ok(total) => total,
            Err(error) => {
                return legacy_worth_reconciliation_unavailable(
                    &principal,
                    &workspace,
                    &error,
                    "cards",
                    "legacy Worth-a-look cross-lane reconciliation unavailable",
                );
            },
        };
    let raw_total = usize::try_from(lane_identities.total).unwrap_or(usize::MAX);
    // The two paths may only be held to the same duplicate count when they read
    // the same universe. The canonical union loads a BOUNDED Worth-a-look page,
    // so above that bound it is comparing a prefix against this path's whole
    // lane and would disagree by construction. `worth_a_look_source_total` is
    // the union's own loaded row count, so equality with the lane total is
    // exactly the statement "the union saw all of it".
    if canonical_attention_projection
        .as_ref()
        .is_some_and(|projection| {
            projection.cross_lane_reconciliation.status
                == crate::canonical_attention_api::CrossLaneReconciliationStatus::Succeeded
                && projection.integrity.worth_a_look_source_total == raw_total
                && projection.integrity.duplicate_hidden_total != duplicate_hidden_total
        })
    {
        let health = crate::canonical_attention_api::unavailable_legacy_worth_reconciliation(
            &principal,
            &workspace,
            crate::canonical_attention_api::CrossLaneReconciliationReason::WorthALookLoadUnavailable,
        );
        return HttpResponse::ServiceUnavailable().json(serde_json::json!({
            "error": "cross_lane_reconciliation_unavailable",
            "cards": [],
            "cross_lane_reconciliation": health,
        }));
    }
    let visible_total = raw_total.saturating_sub(duplicate_hidden_total);
    let visible_page = match collect_visible_legacy_worth_page(
        &store,
        &owners,
        &principal,
        &workspace,
        cursor.as_deref(),
        offset,
        limit,
        visible_total,
    )
    .await
    {
        Ok(page) => page,
        Err(error) => {
            return legacy_worth_reconciliation_unavailable(
                &principal,
                &workspace,
                &error,
                "cards",
                "legacy Worth-a-look page scan unavailable",
            );
        },
    };
    let scanned: Vec<crate::canonical_attention_api::ScannedWorthRow<'_>> = visible_page
        .scanned
        .iter()
        .map(|(candidate, owned)| (candidate, *owned))
        .collect();
    let cross_lane_health = match crate::canonical_attention_api::legacy_worth_page_health(
        &owners,
        &principal,
        &workspace,
        raw_total,
        duplicate_hidden_total,
        &scanned,
    ) {
        Ok(health) => health,
        Err(error) => {
            return legacy_worth_reconciliation_unavailable(
                &principal,
                &workspace,
                &error,
                "cards",
                "legacy Worth-a-look cross-lane reconciliation unavailable",
            );
        },
    };
    let page = visible_page.page;
    let returned_candidate_ids: HashSet<String> = page
        .items
        .iter()
        .map(|candidate| candidate.candidate_id.clone())
        .collect();
    if learning_projection.is_none()
        && canonical_attention_projection.is_none()
        && duplicate_hidden_total == 0
    {
        if let Some(learning) = learning.as_ref() {
            match project_worth_learning(
                &store,
                learning.get_ref(),
                &principal,
                &workspace,
                limit,
                0,
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
                    "failed to compute observe-only Worth-a-look rank metadata"
                ),
            }
        }
    }
    let rank_by_id: std::collections::HashMap<&str, &AttentionRankMetadata> = learning_projection
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
    // fallback here each card loses its learned rank and its decision binding,
    // and the client cannot record a verified impression against the decision
    // that served it — so no reward signal ever reaches the model.
    //
    // Canonical bindings are keyed by the surface-qualified id while these
    // cards carry the raw one, so the fallback normalizes before looking up.
    let canonical_bindings = canonical_attention_projection
        .as_ref()
        .and_then(|projection| projection.diagnostics.as_ref());
    let canonical_rank_by_id: std::collections::HashMap<&str, &AttentionRankMetadata> =
        canonical_bindings
            .map(|diagnostics| {
                diagnostics
                    .ranks
                    .iter()
                    .map(|rank| (rank.candidate_id.as_str(), rank))
                    .collect()
            })
            .unwrap_or_default();
    let canonical_decision_item_by_id: std::collections::HashMap<&str, &AttentionDecisionItem> =
        canonical_bindings
            .map(|diagnostics| {
                diagnostics
                    .decision_items
                    .iter()
                    .map(|item| (item.candidate_id.as_str(), item))
                    .collect()
            })
            .unwrap_or_default();

    // Both per-card store reads are hoisted into one batched query each. They
    // used to run inside the card loop — a `spawn_blocking` hop and a store
    // mutex acquisition per card per read, so a `limit=100` request paid 200
    // serialised SQLite round trips before it could answer. Neither lookup
    // depends on anything the loop computes, and both are pure reads keyed by
    // candidate id, so batching them changes only when the rows are read, not
    // which. A failed batch degrades exactly as a failed single read did: the
    // card falls back to its own title + the signal-derived phrase, and to no
    // recommendation.
    let shadow_memories = if let Some(memory_api) = memory_api.as_ref() {
        memory_api.shadow_memories(&principal, &workspace).await
    } else {
        Vec::new()
    };
    let page_candidate_ids: Vec<String> = page
        .items
        .iter()
        .map(|candidate| candidate.candidate_id.clone())
        .collect();
    let phrasing_by_id = store
        .get_phrasing_batch(&principal, &workspace, &page_candidate_ids)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(principal, workspace, error = %error, "failed to read Worth-a-look phrasing page");
            std::collections::HashMap::new()
        });
    let recommendation_by_id = if interactions
        .as_ref()
        .is_some_and(|registry| registry.recommendations_enabled())
    {
        store
            .get_recommendation_batch(&principal, &workspace, &page_candidate_ids)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(principal, workspace, error = %error, "failed to read Worth-a-look recommendation page");
                std::collections::HashMap::new()
            })
    } else {
        std::collections::HashMap::new()
    };

    // Prefer the LLM curator's stored phrasing (line + why) when present;
    // otherwise fall back to the candidate title + the generic, signal-derived
    // "why now" phrase (the deterministic path writes no phrasing row).
    let mut cards: Vec<serde_json::Value> = Vec::with_capacity(page.items.len());
    for c in page.items {
        let canonical_candidate_id =
            AttentionSurface::WorthALook.canonical_candidate_id(&c.candidate_id);
        let rank = rank_by_id
            .get(c.candidate_id.as_str())
            .copied()
            .or_else(|| {
                canonical_rank_by_id
                    .get(canonical_candidate_id.as_str())
                    .copied()
            });
        let source_revision = c
            .content_revision
            .clone()
            .or_else(|| Some(c.content_digest.clone()));
        let grouping = learning_projection
            .as_ref()
            .and_then(|projection| projection.grouping.projection.metadata.get(&c.candidate_id))
            .cloned()
            .unwrap_or_else(|| {
                singleton_grouping_metadata(AttentionPairCandidateRef {
                    candidate_id: c.candidate_id.clone(),
                    source_revision: source_revision.clone(),
                })
            });
        let decision_item: Option<AttentionDecisionItem> = learning_projection
            .as_ref()
            .and_then(|projection| projection.routing.item(&c.candidate_id))
            .or_else(|| {
                canonical_decision_item_by_id
                    .get(canonical_candidate_id.as_str())
                    .copied()
            })
            .cloned();
        let source_route = c.source_ref.clone();
        let open_url = source_route
            .starts_with("https://")
            .then(|| source_route.clone())
            .or_else(|| {
                source_route
                    .starts_with("http://")
                    .then(|| source_route.clone())
            });
        // Capability computation is metadata-only. In particular, this list
        // path must never call `resolve_detail` or a live content fetcher.
        let actions = interactions
            .as_ref()
            .map(|registry| registry.capabilities(&c, None))
            .unwrap_or_else(|| static_read_capabilities(&c, None));
        let recommendation = visible_recommendation_from(
            interactions.as_ref().map(|registry| registry.get_ref()),
            &c,
            &actions,
            recommendation_by_id.get(&c.candidate_id).cloned(),
        );
        let (line, why, curated) = match phrasing_by_id.get(&c.candidate_id) {
            Some((line, why)) => (line.clone(), why.clone(), true),
            None => (c.title.clone(), why_now(&c.signals).to_string(), false),
        };
        let (why, memory_key, memory_revision) =
            magician::magician_v2::attention::resurfacing::memory_context::worth_why_now(
                &c.title,
                &line,
                c.source_kind.as_str(),
                &why,
                curated,
                &shadow_memories,
            );
        let brief_status = if c.content_details.is_some() {
            "v2"
        } else {
            "legacy"
        };
        let brief = interactions
            .as_ref()
            .map(|registry| registry.rich_briefs_enabled())
            .unwrap_or(true)
            .then(|| c.content_details.clone())
            .flatten();
        cards.push(serde_json::json!({
            "candidate_id": c.candidate_id,
            "source_revision": source_revision,
            "line": line,
            "why_now": why,
            "memory_key": memory_key,
            "memory_revision": memory_revision,
            "summary": c.content_digest,
            "source_title": c.title,
            "source_kind": c.source_kind.as_str(),
            "source_ref": c.source_ref,
            "source_route": source_route,
            "open_url": open_url,
            "detail_label": detail_label_for_source(c.source_kind),
            "temporal_anchor_at": c.temporal_anchor_at,
            "brief": brief,
            "brief_status": brief_status,
            "content_revision": c.content_revision,
            "source_updated": false,
            "recommended_action": recommendation,
            "actions": actions,
            "baseline_rank": rank.map(|rank| rank.baseline_rank).unwrap_or(0),
            "learned_rank": rank.map(|rank| rank.learned_rank).unwrap_or(0),
            "rank_delta": rank.map(|rank| rank.rank_delta).unwrap_or(0),
            "learning_score": rank.and_then(|rank| rank.learning_score),
            "actionability_probability": rank.and_then(|rank| rank.actionability_probability),
            "actionability_explanation": rank.and_then(|rank| rank.actionability_explanation.clone()),
            "actionability_model_version": rank.and_then(|rank| rank.actionability_model_version.clone()),
            "actionability_snapshot_id": rank.and_then(|rank| rank.actionability_snapshot_id.clone()),
            "semantic_feature_status": rank.map(|rank| rank.semantic_feature_status.as_str()).unwrap_or("missing"),
            "actionability_score_status": rank.map(|rank| rank.actionability_score_status),
            "actionability_mode": rank.map(|rank| rank.actionability_mode),
            "grouping": grouping,
            "bandit_decision": decision_item
                .as_ref()
                .and_then(|item| item.bandit_decision.clone()),
            "decision_item": decision_item,
        }));
    }

    let next_cursor = page.next_cursor.as_deref().and_then(|cursor| {
        let cursor = decode_resurfacing_attention_cursor(cursor).ok()?;
        Some(serde_json::json!({
            "surfaced_at": cursor.surfaced_at,
            "score": cursor.salience_score,
            "candidate_id": cursor.candidate_id,
        }))
    });
    let next_cursor_token = page.next_cursor;

    let source_family_counts = match store.candidate_funnel(&principal, &workspace).await {
        Ok(rows) => rows
            .into_iter()
            .filter(|(state, _, _)| {
                *state == magician::magician_v2::attention::resurfacing::types::CandidateState::Surfaced
            })
            .fold(BTreeMap::new(), |mut counts, (_, source_kind, count)| {
                *counts.entry(source_kind.as_str().to_string()).or_insert(0) += count;
                counts
            }),
        Err(error) => {
            tracing::warn!(principal, workspace, error = %error, "failed to load Worth-a-look source-family health");
            BTreeMap::new()
        },
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
                AttentionSurface::WorthALook,
                page.total,
                &projection.universe_ids,
                source_family_counts,
                &projection.ranks,
            )
            .await
            .ok()
    } else if let Some(diagnostics) = canonical_diagnostics {
        Some(diagnostics.worth_a_look_health.clone())
    } else {
        None
    };
    let semantic_extraction_health =
        if let (Some(learning), Some(worker)) = (learning.as_ref(), semantic_worker.as_ref()) {
            crate::attention_learning_api::build_semantic_extraction_health(
                learning,
                worker,
                mail_store.as_ref(),
                &store,
                &principal,
                &workspace,
            )
            .await
            .ok()
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

    HttpResponse::Ok().json(serde_json::json!({
        "cards": cards,
        "total": page.total,
        "limit": page.limit,
        "offset": offset,
        "next_cursor": next_cursor,
        "next_cursor_token": next_cursor_token,
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
            .unwrap_or(magician::config::AttentionActionabilityMode::Disabled),
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
        "cross_lane_reconciliation": cross_lane_health,
        "canonical_attention_projection_ref": canonical_attention_projection.as_ref().map(|projection| serde_json::json!({
            "projection_id": projection.projection_id.as_str(),
            "universe_digest": projection.universe_digest.as_str(),
            "status": projection.status,
        })),
    }))
}

/// Expand one Worth-a-look group from the complete eligible universe without
/// transitioning, deleting, or otherwise mutating any member candidate.
pub async fn get_resurfacing_group_members_handler(
    store: web::Data<ResurfacingStore>,
    learning: web::Data<AttentionLearningService>,
    mail_store: Option<web::Data<ChannelAssistStore>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<TodayQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let Some(mail_store) = mail_store.as_ref() else {
        let health = crate::canonical_attention_api::unavailable_legacy_worth_reconciliation(
            &principal,
            &workspace,
            crate::canonical_attention_api::CrossLaneReconciliationReason::FollowUpStoreUnavailable,
        );
        return HttpResponse::ServiceUnavailable().json(serde_json::json!({
            "error": "cross_lane_reconciliation_unavailable",
            "items": [],
            "cross_lane_reconciliation": health,
        }));
    };
    // Resolving one cluster ranks and groups the whole visible universe, so
    // unlike the paged Today read this one genuinely needs every row.
    let owners = match crate::canonical_attention_api::load_active_follow_up_owners(
        mail_store, &principal, &workspace,
    )
    .await
    {
        Ok(owners) => owners,
        Err(error) => {
            return legacy_worth_reconciliation_unavailable(
                &principal,
                &workspace,
                &error,
                "items",
                "Worth group Follow-up owner set unavailable",
            );
        },
    };
    let mut candidates = match drain_legacy_worth_universe(&store, &principal, &workspace).await {
        Ok(universe) => universe,
        Err(error) => {
            return legacy_worth_reconciliation_unavailable(
                &principal,
                &workspace,
                &error,
                "items",
                "Worth group source scan unavailable",
            );
        },
    };
    let raw_total = candidates.len();
    let scanned = match candidates
        .iter()
        .map(|candidate| owners.owns(candidate).map(|owned| (candidate, owned)))
        .collect::<anyhow::Result<Vec<crate::canonical_attention_api::ScannedWorthRow<'_>>>>()
    {
        Ok(scanned) => scanned,
        Err(error) => {
            return legacy_worth_reconciliation_unavailable(
                &principal,
                &workspace,
                &error,
                "items",
                "Worth group cross-lane reconciliation unavailable",
            );
        },
    };
    let duplicate_hidden_total = scanned.iter().filter(|entry| entry.1).count();
    let hidden_candidate_ids: HashSet<String> = scanned
        .iter()
        .filter(|entry| entry.1)
        .map(|entry| entry.0.candidate_id.clone())
        .collect();
    let cross_lane_health = match crate::canonical_attention_api::legacy_worth_page_health(
        &owners,
        &principal,
        &workspace,
        raw_total,
        duplicate_hidden_total,
        &scanned,
    ) {
        Ok(health) => health,
        Err(error) => {
            return legacy_worth_reconciliation_unavailable(
                &principal,
                &workspace,
                &error,
                "items",
                "Worth group cross-lane reconciliation unavailable",
            );
        },
    };
    drop(scanned);
    candidates.retain(|candidate| !hidden_candidate_ids.contains(&candidate.candidate_id));
    let semantic_candidates: Vec<_> = candidates
        .iter()
        .map(|candidate| resurfacing_semantic_candidate(candidate, None))
        .collect();
    let (_, ranks) = match learning
        .rank_eligible_universe(
            &principal,
            &workspace,
            AttentionSurface::WorthALook,
            &semantic_candidates,
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
            AttentionSurface::WorthALook,
            &semantic_candidates,
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
    let rank_by_id: std::collections::HashMap<_, _> = ranks
        .iter()
        .map(|rank| (rank.candidate_id.as_str(), rank))
        .collect();
    let candidate_by_id: std::collections::HashMap<_, _> = candidates
        .iter()
        .map(|candidate| (candidate.candidate_id.as_str(), candidate))
        .collect();
    let items: Vec<_> = cluster
        .member_ids
        .iter()
        .filter_map(|member_id| {
            let candidate = candidate_by_id.get(member_id.as_str()).copied()?;
            let rank = rank_by_id.get(member_id.as_str()).copied();
            let member_grouping = grouping.projection.metadata.get(member_id)?.clone();
            let source_revision = candidate
                .content_revision
                .clone()
                .or_else(|| Some(candidate.content_digest.clone()));
            let source_route = candidate.source_ref.clone();
            let open_url = source_route
                .starts_with("https://")
                .then(|| source_route.clone())
                .or_else(|| source_route.starts_with("http://").then(|| source_route.clone()));
            Some(serde_json::json!({
                "candidate_id": candidate.candidate_id,
                "source_revision": source_revision,
                "line": candidate.title,
                "summary": candidate.content_digest,
                "source_title": candidate.title,
                "source_kind": candidate.source_kind.as_str(),
                "source_ref": candidate.source_ref,
                "source_route": source_route,
                "open_url": open_url,
                "state": candidate.state.as_str(),
                "temporal_anchor_at": candidate.temporal_anchor_at,
                "content_revision": candidate.content_revision,
                "actions": static_read_capabilities(candidate, None),
                "baseline_rank": rank.map(|rank| rank.baseline_rank).unwrap_or(0),
                "learned_rank": rank.map(|rank| rank.learned_rank).unwrap_or(0),
                "rank_delta": rank.map(|rank| rank.rank_delta).unwrap_or(0),
                "learning_score": rank.and_then(|rank| rank.learning_score),
                "actionability_probability": rank.and_then(|rank| rank.actionability_probability),
                "actionability_explanation": rank.and_then(|rank| rank.actionability_explanation.clone()),
                "actionability_model_version": rank.and_then(|rank| rank.actionability_model_version.clone()),
                "actionability_snapshot_id": rank.and_then(|rank| rank.actionability_snapshot_id.clone()),
                "semantic_feature_status": rank.map(|rank| rank.semantic_feature_status.as_str()).unwrap_or("missing"),
                "actionability_score_status": rank.map(|rank| rank.actionability_score_status),
                "actionability_mode": rank.map(|rank| rank.actionability_mode),
                "grouping": member_grouping,
            }))
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
        "cross_lane_reconciliation": cross_lane_health,
    }))
}

/// Inspect or execute one bounded active routing-repair batch. The deterministic
/// pass never invokes an LLM. Dry runs perform no Follow-up, candidate, receipt,
/// or telemetry writes.
pub async fn post_resurfacing_active_repair_handler(
    store: web::Data<ResurfacingStore>,
    channel_store: web::Data<ChannelAssistStore>,
    attention_store: Option<web::Data<AttentionFunnelStore>>,
    req: HttpRequest,
    query: web::Query<ActiveRepairQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let limit = query.limit.unwrap_or(10).clamp(1, 100);
    match repair_active_surfaced_candidates(
        &principal,
        &workspace,
        &store,
        &channel_store,
        attention_store.as_ref().map(|store| store.get_ref()),
        limit,
        chrono::Utc::now().timestamp(),
        query.dry_run,
    )
    .await
    {
        Ok(outcome) => HttpResponse::Ok().json(serde_json::json!({
            "scope": { "principal": principal, "workspace": workspace },
            "dry_run": query.dry_run,
            "limit": limit,
            "outcome": outcome,
        })),
        Err(error) => err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

/// `GET /channel-assist/resurfacing/{candidate_id}/detail`.
///
/// Resolves only scoped source metadata and combines it with the candidate's
/// persisted safe brief. It never invokes a live body fetcher.
pub async fn get_resurfacing_detail_handler(
    store: web::Data<ResurfacingStore>,
    interactions: Option<web::Data<ResurfacingInteractionRegistry>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    resolve_resurfacing_read(
        store,
        interactions,
        req,
        path.into_inner(),
        query.into_inner(),
        false,
    )
    .await
}

/// `GET /channel-assist/resurfacing/{candidate_id}/original`.
///
/// For communication sources this is the only resurfacing endpoint allowed to
/// fetch bounded live bodies. Task and memory adapters return their current
/// scoped canonical detail without a provider fetch.
pub async fn get_resurfacing_original_handler(
    store: web::Data<ResurfacingStore>,
    interactions: Option<web::Data<ResurfacingInteractionRegistry>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    resolve_resurfacing_read(
        store,
        interactions,
        req,
        path.into_inner(),
        query.into_inner(),
        true,
    )
    .await
}

async fn resolve_resurfacing_read(
    store: web::Data<ResurfacingStore>,
    interactions: Option<web::Data<ResurfacingInteractionRegistry>>,
    req: HttpRequest,
    candidate_id: String,
    query: ScopeQuery,
    include_original: bool,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let candidate_id = candidate_id.trim();
    if candidate_id.is_empty() {
        return err_json(
            actix_web::http::StatusCode::NOT_FOUND,
            "resurfacing candidate not found",
        );
    }
    let candidate = match store
        .get_candidate(&principal, &workspace, candidate_id)
        .await
    {
        Ok(Some(candidate)) => candidate,
        Ok(None) => {
            // A candidate in another scope is intentionally indistinguishable
            // from an unknown id.
            return err_json(
                actix_web::http::StatusCode::NOT_FOUND,
                "resurfacing candidate not found",
            );
        },
        Err(error) => {
            tracing::warn!(
                candidate_id,
                error = %error,
                "failed to load scoped resurfacing candidate"
            );
            return err_json(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                "failed to resolve resurfacing candidate",
            );
        },
    };
    let Some(interactions) = interactions else {
        return err_json(
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "resurfacing source interactions are unavailable",
        );
    };
    let scope = AttentionScope {
        principal,
        workspace,
    };
    let resolved = if include_original {
        interactions.resolve_original(&scope, &candidate).await
    } else {
        interactions.resolve_detail(&scope, &candidate).await
    };
    let resolved = match resolved {
        Ok(resolved) => resolved,
        Err(error) => {
            tracing::warn!(
                candidate_id,
                source_kind = candidate.source_kind.as_str(),
                original = include_original,
                error = %error,
                "failed to resolve resurfacing source"
            );
            return err_json(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                "failed to resolve resurfacing source",
            );
        },
    };
    let recommendation = visible_recommendation(
        &store,
        Some(interactions.get_ref()),
        &scope.principal,
        &scope.workspace,
        &candidate,
        &resolved.actions,
    )
    .await;
    HttpResponse::Ok().json(resurfacing_read_json(
        &candidate,
        resolved,
        include_original,
        interactions.rich_briefs_enabled(),
        recommendation,
    ))
}

fn resurfacing_read_json(
    candidate: &Candidate,
    resolved: ResolvedResurfacingDetail,
    include_original: bool,
    include_rich_brief: bool,
    recommendation: Option<ResurfacingRecommendation>,
) -> serde_json::Value {
    let restricted = resolved.status == ResurfacingSourceStatus::Suppressed;
    let title = if restricted {
        None
    } else {
        resolved.title.or_else(|| Some(candidate.title.clone()))
    };
    let summary = if restricted {
        None
    } else {
        resolved
            .summary
            .or_else(|| Some(candidate.content_digest.clone()))
    };
    serde_json::json!({
        "candidate_id": candidate.candidate_id,
        "source_kind": candidate.source_kind.as_str(),
        "status": resolved.status,
        "title": title,
        "summary": summary,
        "brief": if restricted || !include_rich_brief { None } else { candidate.content_details.clone() },
        "content_revision": if restricted { None } else { candidate.content_revision.clone() },
        "source_revision": if restricted { None } else { resolved.source_revision },
        "source_updated": resolved.source_updated,
        "has_newer": resolved.has_newer,
        "source_route": if restricted { None } else { resolved.source_route },
        "open_url": if restricted { None } else { resolved.open_url },
        "source": if restricted { None } else { resolved.source },
        "recommended_action": if restricted { None } else { recommendation },
        "actions": resolved.actions,
        "original": if include_original && !restricted { resolved.original } else { None },
        "temporal_anchor_at": candidate.temporal_anchor_at,
    })
}

/// Explicit telemetry for read-style primary recommendations. Contextual
/// actions record these stages inside their durable claim transaction instead.
pub async fn post_resurfacing_recommendation_event_handler(
    store: web::Data<ResurfacingStore>,
    interactions: Option<web::Data<ResurfacingInteractionRegistry>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    body: web::Json<RecommendationInteractionBody>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    if body
        .content_revision
        .as_ref()
        .is_some_and(|revision| revision.chars().count() > 160)
    {
        return err_json(
            actix_web::http::StatusCode::BAD_REQUEST,
            "content_revision exceeds 160 characters",
        );
    }
    let Some(interactions) = interactions.filter(|registry| registry.recommendations_enabled())
    else {
        return err_json(
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "resurfacing recommendations are disabled",
        );
    };
    let candidate_id = path.into_inner();
    let candidate_id = candidate_id.trim();
    if candidate_id.is_empty() || candidate_id.chars().count() > 160 {
        return err_json(
            actix_web::http::StatusCode::NOT_FOUND,
            "resurfacing candidate not found",
        );
    }
    let candidate = match store
        .get_candidate(&principal, &workspace, candidate_id)
        .await
    {
        Ok(Some(candidate)) => candidate,
        Ok(None) => {
            return err_json(
                actix_web::http::StatusCode::NOT_FOUND,
                "resurfacing candidate not found",
            );
        },
        Err(error) => {
            tracing::warn!(error = %error, "failed to load recommendation event candidate");
            return err_json(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                "failed to load resurfacing candidate",
            );
        },
    };
    let recommendation = match store
        .get_recommendation(&principal, &workspace, &candidate.candidate_id)
        .await
    {
        Ok(Some(recommendation)) => recommendation,
        Ok(None) => {
            return err_json(
                actix_web::http::StatusCode::CONFLICT,
                "the recommendation is no longer current",
            );
        },
        Err(error) => {
            tracing::warn!(error = %error, "failed to load current resurfacing recommendation");
            return err_json(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                "failed to load resurfacing recommendation",
            );
        },
    };
    let capabilities = interactions.capabilities(&candidate, None);
    if body.kind != recommendation.kind
        || body.content_revision.as_deref() != recommendation.content_revision.as_deref()
        || !interactions.recommendation_is_visible(&candidate, &capabilities, &recommendation)
    {
        return err_json(
            actix_web::http::StatusCode::CONFLICT,
            "the recommendation kind or content revision is stale",
        );
    }
    if matches!(body.event, RecommendationInteractionEvent::Presented) {
        return match store
            .record_recommendation_shown(
                &principal,
                &workspace,
                &candidate.candidate_id,
                &recommendation,
                chrono::Utc::now().timestamp(),
            )
            .await
        {
            Ok(recorded) => {
                HttpResponse::Ok().json(serde_json::json!({ "ok": true, "recorded": recorded }))
            },
            Err(error) => {
                tracing::warn!(error = %error, "failed to record recommendation presentation");
                err_json(
                    actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                    "failed to record resurfacing recommendation presentation",
                )
            },
        };
    }
    if !matches!(
        body.kind,
        ResurfacingActionKind::ViewDetails
            | ResurfacingActionKind::OpenSource
            | ResurfacingActionKind::ShowOriginal
    ) {
        return err_json(
            actix_web::http::StatusCode::BAD_REQUEST,
            "selected/completed client events are only valid for read-style recommendations",
        );
    }
    let result = store
        .record_recommendation_interaction(
            &principal,
            &workspace,
            &candidate.candidate_id,
            body.kind,
            body.content_revision.as_deref(),
            body.event.as_str(),
            chrono::Utc::now().timestamp(),
        )
        .await;
    match result {
        Ok(ResurfacingRecommendationInteractionResult::Recorded) => {
            HttpResponse::Ok().json(serde_json::json!({ "ok": true, "recorded": true }))
        },
        Ok(ResurfacingRecommendationInteractionResult::Duplicate) => {
            HttpResponse::Ok().json(serde_json::json!({ "ok": true, "recorded": false }))
        },
        Ok(ResurfacingRecommendationInteractionResult::NotShown) => err_json(
            actix_web::http::StatusCode::CONFLICT,
            "the recommendation was not presented or is no longer current",
        ),
        Ok(ResurfacingRecommendationInteractionResult::OutOfOrder) => err_json(
            actix_web::http::StatusCode::CONFLICT,
            "recommendation completion requires a prior selected event",
        ),
        Err(error) => {
            tracing::warn!(error = %error, "failed to record resurfacing recommendation event");
            err_json(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                "failed to record resurfacing recommendation event",
            )
        },
    }
}

/// `POST /channel-assist/resurfacing/{candidate_id}/actions` executes one
/// capability-validated contextual action with durable idempotency.
pub async fn post_resurfacing_contextual_action_handler(
    actions: Option<web::Data<ResurfacingActionService>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    body: web::Json<ResurfacingContextualActionRequest>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let Some(actions) = actions else {
        return err_json_code(
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "resurfacing contextual actions are unavailable",
        );
    };
    let candidate_id = path.into_inner();
    match actions
        .execute(
            AttentionScope {
                principal,
                workspace,
            },
            &candidate_id,
            body.into_inner(),
        )
        .await
    {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(error) => contextual_action_error_response(&candidate_id, error),
    }
}

/// `POST /channel-assist/resurfacing/{candidate_id}/action`.
pub async fn post_resurfacing_action_handler(
    store: web::Data<ResurfacingStore>,
    attention_store: Option<web::Data<AttentionFunnelStore>>,
    learning: Option<web::Data<AttentionLearningService>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
    body: web::Json<ActionBody>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };

    let action = match FeedbackAction::from_str(body.action.trim()) {
        Ok(action) => action,
        Err(err) => return err_json(actix_web::http::StatusCode::BAD_REQUEST, err),
    };
    // Only a dismiss carries a reason; anything unrecognized degrades to a plain
    // reasonless dismiss rather than erroring the request.
    let reason = body
        .reason
        .as_deref()
        .and_then(DismissReason::parse)
        .filter(|_| action == FeedbackAction::Dismiss);
    let candidate_id = path.into_inner();
    let feedback_event_id = match body.event_id.as_deref() {
        Some(event_id) if !event_id.trim().is_empty() && event_id.chars().count() <= 200 => {
            event_id.to_string()
        },
        Some(_) => {
            return err_json(
                actix_web::http::StatusCode::BAD_REQUEST,
                anyhow::anyhow!("event_id must contain 1..=200 characters"),
            );
        },
        None => uuid::Uuid::new_v4().to_string(),
    };
    let now = chrono::Utc::now().timestamp();
    let mut learning_cohort = if let Some(learning) = learning.as_ref() {
        resurfacing_learning_cohort(&store, &principal, &workspace, learning.rescore_limit())
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    if !learning_cohort
        .iter()
        .any(|candidate| candidate.candidate_id == candidate_id)
    {
        if let Ok(Some(candidate)) = store
            .get_candidate(&principal, &workspace, &candidate_id)
            .await
        {
            let embedding = store
                .list_candidate_embedding_snapshots_for_ids(
                    &principal,
                    &workspace,
                    std::slice::from_ref(&candidate_id),
                )
                .await
                .ok()
                .and_then(|snapshots| {
                    snapshots
                        .into_iter()
                        .find(|snapshot| snapshot.content_digest == candidate.content_digest)
                })
                .map(|snapshot| SemanticEmbedding {
                    contract: snapshot.embedding_contract,
                    vector: snapshot.embedding,
                });
            learning_cohort.push(resurfacing_semantic_candidate(&candidate, embedding));
        }
    }

    match store
        .record_action_with_reason_event(
            &principal,
            &workspace,
            &candidate_id,
            action,
            reason,
            Some(&feedback_event_id),
            now,
            DISMISS_COOLDOWN_SECS,
            ACK_COOLDOWN_SECS,
        )
        .await
    {
        Ok(()) => {
            if let Some(attention_store) = attention_store {
                record_resurfacing_action_trace(
                    attention_store.get_ref(),
                    store.get_ref(),
                    &principal,
                    &workspace,
                    &candidate_id,
                    action,
                    now,
                )
                .await;
            }
            let outcome = match action {
                FeedbackAction::Open => AttentionOutcomeKind::Useful,
                FeedbackAction::OwnerWork => AttentionOutcomeKind::ActionCompleted,
                FeedbackAction::Acknowledge => AttentionOutcomeKind::NeutralSeen,
                FeedbackAction::Dismiss => match reason {
                    Some(DismissReason::AlreadyHandled) => AttentionOutcomeKind::Obsolete,
                    Some(DismissReason::Duplicate) => AttentionOutcomeKind::DuplicateOf,
                    Some(DismissReason::Delegated) => AttentionOutcomeKind::NotOwner,
                    Some(DismissReason::Spam | DismissReason::NotRelevant) | None => {
                        AttentionOutcomeKind::Irrelevant
                    },
                },
            };
            let feedback_receipt = if let Some(learning) = learning.as_ref() {
                let candidate = learning_cohort
                    .iter()
                    .find(|candidate| candidate.candidate_id == candidate_id)
                    .cloned();
                if let Some(candidate) = candidate {
                    match learning
                        .record_and_propagate(
                            &principal,
                            &workspace,
                            AttentionSurface::WorthALook,
                            RecordAttentionOutcome {
                                event_id: feedback_event_id.clone(),
                                candidate,
                                outcome,
                                reason: reason.map(|reason| reason.as_str().to_string()),
                                label_quality: AttentionLabelQuality::Strong,
                                occurred_at: now.saturating_mul(1_000),
                                attribution: body.attribution.clone(),
                            },
                            learning_cohort,
                        )
                        .await
                    {
                        Ok(receipt) => Some(receipt),
                        Err(error) => {
                            tracing::warn!(candidate_id, error = %error, "failed to propagate Worth-a-look feedback");
                            None
                        },
                    }
                } else {
                    None
                }
            } else {
                None
            };
            HttpResponse::Ok().json(serde_json::json!({
                "ok": true,
                "feedback_receipt": feedback_receipt,
            }))
        },
        Err(err) => err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    }
}

async fn record_resurfacing_action_trace(
    attention_store: &AttentionFunnelStore,
    store: &ResurfacingStore,
    principal: &str,
    workspace: &str,
    candidate_id: &str,
    action: FeedbackAction,
    now: i64,
) {
    let candidate = match store
        .get_candidate(principal, workspace, candidate_id)
        .await
    {
        Ok(Some(candidate)) => candidate,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(
                candidate_id,
                error = %error,
                "failed to read resurfacing candidate for attention action trace"
            );
            return;
        },
    };
    let event = resurfacing_action_trace_event(principal, workspace, &candidate, action, now);
    let event_id = event.event_id.clone();
    if let Err(error) = attention_store.append_event(event).await {
        tracing::warn!(
            candidate_id,
            event_id = %event_id,
            error = %error,
            "failed to record resurfacing attention action trace"
        );
    }
}

fn resurfacing_action_trace_event(
    principal: &str,
    workspace: &str,
    candidate: &Candidate,
    action: FeedbackAction,
    now: i64,
) -> AttentionRouteEvent {
    let attention_candidate = attention_candidate_from_resurfacing(candidate);
    let now_ms = now.saturating_mul(1000);
    AttentionRouteEvent {
        event_id: resurfacing_action_trace_event_id(principal, workspace, candidate, action, now),
        scope: AttentionScope {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
        },
        source: attention_candidate.source.clone(),
        source_family: attention_candidate.source_family,
        candidate_key: attention_candidate.candidate_key.clone(),
        stage: AttentionFunnelStage::Acted,
        outcome: RouteOutcome::Traced {
            status: AttentionTraceStatus::Succeeded,
        },
        occurred_at: candidate
            .last_surfaced_at
            .unwrap_or(now)
            .saturating_mul(1000),
        created_at: now_ms,
        confidence: attention_candidate.confidence,
        metadata: serde_json::json!({
            "producer": "resurfacing_action",
            "action": action.as_str(),
            "source_kind": candidate.source_kind.as_str(),
            "candidate_state": candidate.state.as_str(),
            "salience_score": candidate.salience_score,
            "surface_count": candidate.surface_count,
            "dismiss_count": candidate.dismiss_count,
            "last_surfaced_at": candidate.last_surfaced_at,
        }),
    }
}

fn resurfacing_action_trace_event_id(
    principal: &str,
    workspace: &str,
    candidate: &Candidate,
    action: FeedbackAction,
    now: i64,
) -> String {
    let raw = format!(
        "resurfacing_action_trace\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}",
        principal,
        workspace,
        candidate.candidate_id,
        candidate.state.as_str(),
        action.as_str(),
        now,
    );
    format!(
        "resurfacing-action-trace:{}",
        blake3::hash(raw.as_bytes()).to_hex()
    )
}

/// `GET /channel-assist/resurfacing/stats`.
///
/// Per-lane utility legibility (P4c): for each `source_kind` the owner has ever
/// acted on, report the positive/negative tallies plus the Laplace-smoothed
/// engagement rate and the resulting utility multiplier for that lane — computed
/// via the SAME [`utility_rate`]/[`utility_multiplier`] helpers the scorer uses
/// (with [`ResurfacingScoringConfig::from_env`] so the reported numbers match
/// the running config). Lanes never acted on simply don't appear (the scorer
/// treats them as neutral `×1.0`).
///
/// `utility_multiplier` here is the LANE value, which is what this endpoint is
/// about. It is the exact factor for the penalty side, but an upper bound on the
/// boost side: the scorer scales a boost by each item's own centrality
/// (`utility_multiplier_for_item`), so an individual card in a popular lane may
/// receive anywhere from `×1.0` up to this number.
pub async fn get_resurfacing_stats_handler(
    store: web::Data<ResurfacingStore>,
    req: HttpRequest,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };

    let engagement = match store.kind_engagement(&principal, &workspace).await {
        Ok(engagement) => engagement,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };

    let cfg = ResurfacingScoringConfig::from_env();
    let lanes: Vec<serde_json::Value> = engagement
        .into_iter()
        .map(|(kind, positive, negative)| {
            serde_json::json!({
                "source_kind": kind.as_str(),
                "positive": positive,
                "negative": negative,
                "engagement_rate": utility_rate(positive, negative, cfg.utility_smoothing),
                "utility_multiplier": utility_multiplier(positive, negative, &cfg),
            })
        })
        .collect();

    HttpResponse::Ok().json(serde_json::json!({ "lanes": lanes }))
}

/// `GET /channel-assist/resurfacing/observability`.
///
/// A single rich snapshot of "what is the engine doing, how, what's done, what's
/// in the active queue, successes, failures" (O2). Additive over the existing reads — it
/// assembles, in one response:
///
/// * **`pipeline`** — per-kind (`scorer`/`curator`/`retention`) run aggregates
///   (totals, successes/failures, produced, avg duration, last start, last
///   error), keyed by kind.
/// * **`recent_runs`** — the newest [`OBSERVABILITY_RUN_LIMIT`] run records,
///   newest first.
/// * **`funnel`** — the candidate grid folded into per-state (`by_state`) and
///   per-lane (`by_lane`) totals, active-queue counts (`pending`/`eligible`),
///   raw candidate inventory (`candidate_pool`), `surfaced`, and the raw `detail`
///   grid. `pending` is not the raw pool; it is candidate-state rows whose
///   cooldown has elapsed and are eligible for curation now.
/// * **`queue`** — the same active-queue snapshot as a standalone block for
///   consumers that should not need to infer queue health from lifecycle states.
/// * **`watermarks`** — each corpus lane's scan cursor.
/// * **`sizes`** — per-table row counts.
/// * **`engagement`** — the SAME per-lane utility block the `/stats` endpoint
///   reports (positive/negative + smoothed rate + utility multiplier), computed
///   via the shared [`utility_rate`]/[`utility_multiplier`] helpers seeded from
///   [`ResurfacingScoringConfig::from_env`].
///
/// Scope-aware exactly like [`get_resurfacing_stats_handler`]; any store error
/// returns 500 via [`err_json`].
pub async fn get_resurfacing_observability_handler(
    store: web::Data<ResurfacingStore>,
    req: HttpRequest,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };

    // Pipeline health: per-kind run aggregates, keyed by kind.
    let aggregates = match store.run_aggregates(&principal, &workspace).await {
        Ok(aggregates) => aggregates,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let mut pipeline = serde_json::Map::new();
    for agg in &aggregates {
        pipeline.insert(
            agg.kind.clone(),
            serde_json::json!({
                "total": agg.total,
                "successes": agg.successes,
                "failures": agg.failures,
                "total_produced": agg.total_produced,
                "avg_duration_ms": agg.avg_duration_ms,
                "last_started_at": agg.last_started_at,
                "last_error": agg.last_error,
            }),
        );
    }

    // Recent run records (newest first).
    let recent_runs = match store
        .recent_runs(&principal, &workspace, OBSERVABILITY_RUN_LIMIT)
        .await
    {
        Ok(recent_runs) => recent_runs,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };

    let now = chrono::Utc::now().timestamp();

    // Funnel: fold the raw (state, lane, count) grid into per-state + per-lane
    // totals. The active queue counters below come from `queue_stats`, because
    // lifecycle state alone cannot tell eligible-now from cooled-down candidates.
    let funnel = match store.candidate_funnel(&principal, &workspace).await {
        Ok(funnel) => funnel,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let mut by_state: BTreeMap<String, u64> = BTreeMap::new();
    let mut by_lane: BTreeMap<String, u64> = BTreeMap::new();
    let mut detail: Vec<serde_json::Value> = Vec::with_capacity(funnel.len());
    for (state, kind, count) in &funnel {
        *by_state.entry(state.as_str().to_string()).or_insert(0) += *count;
        *by_lane.entry(kind.as_str().to_string()).or_insert(0) += *count;
        detail.push(serde_json::json!({
            "state": state.as_str(),
            "source_kind": kind.as_str(),
            "count": count,
        }));
    }

    let queue = match store.queue_stats(&principal, &workspace, now).await {
        Ok(queue) => queue,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };

    // Corpus scan cursors.
    let watermarks = match store.watermark_positions(&principal, &workspace).await {
        Ok(watermarks) => watermarks,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let watermarks: Vec<serde_json::Value> = watermarks
        .into_iter()
        .map(|(corpus_kind, cursor)| {
            serde_json::json!({ "corpus_kind": corpus_kind, "cursor": cursor })
        })
        .collect();

    // Per-table row counts.
    let sizes = match store.table_sizes(&principal, &workspace).await {
        Ok(sizes) => sizes,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };

    // Per-lane utility — identical shape/computation to `/stats`.
    let engagement = match store.kind_engagement(&principal, &workspace).await {
        Ok(engagement) => engagement,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let cfg = ResurfacingScoringConfig::from_env();
    let engagement: Vec<serde_json::Value> = engagement
        .into_iter()
        .map(|(kind, positive, negative)| {
            serde_json::json!({
                "source_kind": kind.as_str(),
                "positive": positive,
                "negative": negative,
                "engagement_rate": utility_rate(positive, negative, cfg.utility_smoothing),
                "utility_multiplier": utility_multiplier(positive, negative, &cfg),
            })
        })
        .collect();

    let recommendation_events = match store
        .recommendation_event_aggregates(&principal, &workspace)
        .await
    {
        Ok(events) => events,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let mut recommendation_by_kind: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut recommendation_shown = 0u64;
    let mut recommendation_selected = 0u64;
    let mut recommendation_completed = 0u64;
    for event in recommendation_events {
        match event.event_type.as_str() {
            "recommended" => recommendation_shown += event.count,
            "selected" => recommendation_selected += event.count,
            "completed" => recommendation_completed += event.count,
            _ => {},
        }
        *recommendation_by_kind
            .entry(event.recommendation_kind)
            .or_default()
            .entry(event.event_type)
            .or_default() += event.count;
    }
    let recommendation_acceptance_rate = if recommendation_shown == 0 {
        0.0
    } else {
        recommendation_selected as f64 / recommendation_shown as f64
    };
    let recommendation_completion_rate = if recommendation_selected == 0 {
        0.0
    } else {
        recommendation_completed as f64 / recommendation_selected as f64
    };
    let action_events = match store.action_event_aggregates(&principal, &workspace).await {
        Ok(events) => events,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let action_claim_states = match store
        .action_claim_state_histogram(&principal, &workspace)
        .await
    {
        Ok(states) => states.into_iter().collect::<BTreeMap<_, _>>(),
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let mut action_by_kind: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut action_errors: BTreeMap<String, u64> = BTreeMap::new();
    let mut action_started = 0u64;
    let mut action_completed = 0u64;
    let mut action_failed = 0u64;
    for event in action_events {
        match event.event_type.as_str() {
            "started" => action_started += event.count,
            "completed" => action_completed += event.count,
            "failed" => action_failed += event.count,
            _ => {},
        }
        *action_by_kind
            .entry(event.action_kind)
            .or_default()
            .entry(event.event_type)
            .or_default() += event.count;
        if let Some(error_class) = event.error_class {
            *action_errors.entry(error_class).or_default() += event.count;
        }
    }
    let brief_coverage = match store.brief_coverage(&principal, &workspace).await {
        Ok(coverage) => coverage,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let routing_repair_by_outcome = match store
        .routing_repair_outcome_histogram(&principal, &workspace)
        .await
    {
        Ok(outcomes) => outcomes.into_iter().collect::<BTreeMap<_, _>>(),
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };

    // Pre-serialize the map/struct payloads to `Value` so the JSON assembly
    // doesn't depend on `From` impls the `json!` macro lacks for `BTreeMap` /
    // custom structs.
    let empty_obj = || serde_json::json!({});
    HttpResponse::Ok().json(serde_json::json!({
        "pipeline": serde_json::Value::Object(pipeline),
        "recent_runs": serde_json::to_value(&recent_runs).unwrap_or_else(|_| serde_json::json!([])),
        "funnel": {
            "by_state": serde_json::to_value(&by_state).unwrap_or_else(|_| empty_obj()),
            "by_lane": serde_json::to_value(&by_lane).unwrap_or_else(|_| empty_obj()),
            "pending": queue.pending,
            "eligible": queue.eligible,
            "candidate_pool": queue.candidate_pool,
            "cooling": queue.cooling,
            "surfaced": queue.surfaced,
            "detail": detail,
        },
        "queue": serde_json::to_value(&queue).unwrap_or_else(|_| empty_obj()),
        "watermarks": watermarks,
        "sizes": serde_json::to_value(&sizes).unwrap_or_else(|_| empty_obj()),
        "engagement": engagement,
        "recommendations": {
            "shown": recommendation_shown,
            "selected": recommendation_selected,
            "completed": recommendation_completed,
            "acceptance_rate": recommendation_acceptance_rate,
            "completion_rate": recommendation_completion_rate,
            "by_kind": recommendation_by_kind,
        },
        "actions": {
            "attempts": action_started,
            "started": action_started,
            "completed": action_completed,
            "failed": action_failed,
            "completion_rate": if action_started == 0 { 0.0 } else { action_completed as f64 / action_started as f64 },
            "by_kind": action_by_kind,
            "errors": action_errors,
            "claim_states": action_claim_states,
        },
        "briefs": serde_json::to_value(&brief_coverage).unwrap_or_else(|_| empty_obj()),
        "routing_repair": {
            "total": routing_repair_by_outcome.values().copied().sum::<u64>(),
            "by_outcome": routing_repair_by_outcome,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use magician_comms::channel_assist::channel::ChannelAssistStore;

    // Alias the actix `test` module so it does not shadow the standard
    // `#[test]` attribute used by the pure (sync) `why_now` unit test below.
    use actix_web::{test as actix_test, App};

    use magician::magician_v2::agents::AgentMemoryResolver;
    use magician::magician_v2::attention::resurfacing::interaction::{
        ResurfacingActionKind, ResurfacingRecommendationSource,
    };
    use magician::magician_v2::attention::resurfacing::types::{
        candidate_id, Candidate, CandidateState, SalienceSignals, SourceKind,
    };
    use magician_comms::channel_assist::resurfacing::interaction::{
        MemoryInteractionAdapter, ResurfacingInteractionRegistry,
    };

    fn empty_channel_store() -> (tempfile::TempDir, ChannelAssistStore) {
        let dir = tempfile::TempDir::new().expect("creating channel-assist temp dir");
        let store =
            ChannelAssistStore::open(dir.path()).expect("opening channel-assist test store");
        (dir, store)
    }

    fn surfaced_candidate() -> Candidate {
        let source_ref = "user.knowledge#trip";
        Candidate {
            candidate_id: candidate_id(SourceKind::Memory, source_ref),
            source_kind: SourceKind::Memory,
            source_ref: source_ref.to_string(),
            title: "Plan the Tokyo trip".to_string(),
            content_digest: "digest".to_string(),
            content_details: None,
            content_revision: None,
            semantic_features: None,
            salience_score: 0.8,
            signals: SalienceSignals {
                // centrality dominates → deterministic why_now phrase.
                centrality: 0.9,
                ..SalienceSignals::default()
            },
            temporal_anchor_at: None,
            embedding_id: None,
            state: CandidateState::Surfaced,
            first_seen_at: 1_000,
            last_scored_at: 1_000,
            last_surfaced_at: Some(2_000),
            cooldown_until: 0,
            surface_count: 1,
            dismiss_count: 0,
        }
    }

    /// A minimal candidate in an arbitrary lane, so the stats test can seed
    /// engagement across two `source_kind`s.
    fn lane_candidate(kind: SourceKind, source_ref: &str) -> Candidate {
        Candidate {
            candidate_id: candidate_id(kind, source_ref),
            source_kind: kind,
            source_ref: source_ref.to_string(),
            title: "t".to_string(),
            content_digest: "d".to_string(),
            content_details: None,
            content_revision: None,
            semantic_features: None,
            salience_score: 0.5,
            signals: SalienceSignals::default(),
            temporal_anchor_at: None,
            embedding_id: None,
            state: CandidateState::Surfaced,
            first_seen_at: 1_000,
            last_scored_at: 1_000,
            last_surfaced_at: Some(2_000),
            cooldown_until: 0,
            surface_count: 1,
            dismiss_count: 0,
        }
    }

    #[test]
    fn why_now_maps_the_dominant_signal_to_a_generic_phrase() {
        let mut s = SalienceSignals::default();
        s.centrality = 0.9;
        assert_eq!(why_now(&s), "you keep coming back to this");

        let mut s = SalienceSignals::default();
        s.temporal_anchor = 0.7;
        assert_eq!(why_now(&s), "a relevant date is near");

        // All-zero bundle falls back to a neutral phrase, never recency.
        assert_eq!(
            why_now(&SalienceSignals::default()),
            "may be worth revisiting"
        );

        let mut s = SalienceSignals::default();
        s.recency = 0.8;
        assert_eq!(why_now(&s), "fresh context may connect to active work");
    }

    #[test]
    fn reconciled_worth_pagination_fills_visible_slots_and_advances_raw_cursor() {
        // The same lane, the same expected pages and the same health counts as
        // when this page was sliced out of an accumulated universe. What
        // changed is that the rows now arrive from a seek and the walk stops
        // once the page is decided; the arithmetic must not have moved.
        let candidate = |id: &str, surfaced_at: i64| {
            let mut candidate = lane_candidate(SourceKind::Memory, id);
            candidate.candidate_id = id.to_string();
            candidate.last_surfaced_at = Some(surfaced_at);
            candidate
        };
        let candidates = vec![
            candidate("visible-a", 500),
            candidate("hidden-a", 400),
            candidate("visible-b", 300),
            candidate("hidden-b", 200),
            candidate("visible-c", 100),
        ];
        let hidden = HashSet::from(["hidden-a".to_string(), "hidden-b".to_string()]);
        let visible_total = candidates
            .iter()
            .filter(|candidate| !hidden.contains(&candidate.candidate_id))
            .count();
        // Feed the builder the lane from a given starting position, exactly as
        // the store's seek would deliver it.
        let build = |cursor: Option<&str>, offset: usize, limit: usize| {
            let start = cursor
                .map(|cursor| {
                    let decoded =
                        decode_resurfacing_attention_cursor(cursor).expect("decodable cursor");
                    candidates
                        .iter()
                        .position(|candidate| candidate.candidate_id == decoded.candidate_id)
                        .expect("cursor names a lane row")
                        + 1
                })
                .unwrap_or(0);
            let mut builder = VisibleWorthPageBuilder::new(cursor.is_some(), offset, limit);
            for candidate in &candidates[start..] {
                if !builder.push(candidate, hidden.contains(&candidate.candidate_id)) {
                    break;
                }
            }
            builder.finish(visible_total, cursor)
        };
        let hidden_in_page =
            |page: &VisibleWorthPage| page.scanned.iter().filter(|entry| entry.1).count();

        let first = build(None, 0, 2);
        assert_eq!(first.page.total, 3);
        assert_eq!(
            first
                .page
                .items
                .iter()
                .map(|item| item.candidate_id.as_str())
                .collect::<Vec<_>>(),
            ["visible-a", "visible-b"]
        );
        assert_eq!(first.scanned.len(), 3);
        assert_eq!(hidden_in_page(&first), 1);
        assert!(first.page.has_more);

        let second = build(first.page.next_cursor.as_deref(), 0, 2);
        assert_eq!(
            second
                .page
                .items
                .iter()
                .map(|item| item.candidate_id.as_str())
                .collect::<Vec<_>>(),
            ["visible-c"]
        );
        assert_eq!(second.scanned.len(), 2);
        assert_eq!(hidden_in_page(&second), 1);
        assert!(!second.page.has_more);
        assert!(second.page.next_cursor.is_none());

        let offset = build(None, 1, 1);
        assert_eq!(offset.page.items[0].candidate_id, "visible-b");
        assert_eq!(offset.scanned.len(), 2);
        assert_eq!(hidden_in_page(&offset), 1);
    }

    #[test]
    fn a_worth_page_stops_scanning_once_its_page_is_decided() {
        // The defect this replaces: page one pulled every page in the lane
        // before returning eight rows. A page is decided by the rows it
        // serves plus the ONE visible row that proves another page exists.
        let candidates = (0..50)
            .map(|index| {
                let mut candidate = lane_candidate(SourceKind::Memory, &format!("c{index}"));
                candidate.candidate_id = format!("c{index}");
                candidate.last_surfaced_at = Some(1_000 - index);
                candidate
            })
            .collect::<Vec<_>>();

        let mut builder = VisibleWorthPageBuilder::new(false, 0, 2);
        let mut offered = 0usize;
        for candidate in &candidates {
            offered += 1;
            if !builder.push(candidate, false) {
                break;
            }
        }

        assert_eq!(
            offered, 3,
            "two visible rows plus one look-ahead row, not the lane"
        );
        let page = builder.finish(candidates.len(), None);
        assert_eq!(page.page.items.len(), 2);
        assert!(page.page.has_more);
        assert_eq!(
            page.scanned.len(),
            2,
            "the look-ahead row is not a row this page scanned"
        );
    }

    #[actix_web::test]
    async fn contextual_action_errors_include_stable_codes_and_statuses() {
        use actix_web::http::StatusCode;

        let cases = vec![
            (
                ResurfacingActionError::Invalid("bad input".to_string()),
                StatusCode::BAD_REQUEST,
                "invalid",
            ),
            (
                ResurfacingActionError::StaleRevision("refresh".to_string()),
                StatusCode::CONFLICT,
                "stale_revision",
            ),
            (
                ResurfacingActionError::InProgress,
                StatusCode::CONFLICT,
                "in_progress",
            ),
            (
                ResurfacingActionError::IdempotencyConflict,
                StatusCode::CONFLICT,
                "idempotency_conflict",
            ),
            (
                ResurfacingActionError::NotActionable("not actionable".to_string()),
                StatusCode::CONFLICT,
                "not_actionable",
            ),
            (
                ResurfacingActionError::Unavailable("offline".to_string()),
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
            ),
            (
                ResurfacingActionError::Internal(anyhow::anyhow!("storage")),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
        ];
        for (error, status, code) in cases {
            let response = contextual_action_error_response("candidate", error);
            assert_eq!(response.status(), status);
            let body = actix_web::body::to_bytes(response.into_body())
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["code"], code);
            assert!(body["error"]
                .as_str()
                .is_some_and(|message| !message.is_empty()));
        }
    }

    #[actix_web::test]
    async fn today_lists_surfaced_then_action_dismisses() {
        let store = ResurfacingStore::open_in_temp();
        let (_channel_store_dir, channel_store) = empty_channel_store();
        let candidate = surfaced_candidate();
        store
            .upsert_candidate("anonymous", "default", &candidate)
            .await
            .unwrap();

        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(store.clone()))
                .app_data(web::Data::new(channel_store))
                .route(
                    "/channel-assist/resurfacing/today",
                    web::get().to(get_resurfacing_today_handler),
                )
                .route(
                    "/channel-assist/resurfacing/{candidate_id}/action",
                    web::post().to(post_resurfacing_action_handler),
                ),
        )
        .await;

        // GET today → exactly one card whose line is the candidate title.
        let req = actix_test::TestRequest::get()
            .uri("/channel-assist/resurfacing/today?workspace=default")
            .insert_header(("X-Principal", "anonymous"))
            .insert_header(("X-Workspace", "default"))
            .to_request();
        let resp: serde_json::Value = actix_test::call_and_read_body_json(&app, req).await;
        let cards = resp["cards"].as_array().expect("cards array");
        assert_eq!(cards.len(), 1);
        assert_eq!(resp["total"].as_u64().unwrap(), 1);
        assert_eq!(
            resp["limit"].as_u64().unwrap(),
            TODAY_CARD_DEFAULT_LIMIT as u64
        );
        assert_eq!(resp["offset"].as_u64().unwrap(), 0);
        assert_eq!(resp["has_more"], false);
        assert!(resp["next_cursor"].is_null());
        assert_eq!(cards[0]["line"], "Plan the Tokyo trip");
        assert_eq!(cards[0]["candidate_id"], candidate.candidate_id);
        assert_eq!(cards[0]["why_now"], "you keep coming back to this");
        assert_eq!(cards[0]["summary"], "digest");
        assert_eq!(cards[0]["source_title"], "Plan the Tokyo trip");
        assert_eq!(cards[0]["detail_label"], "Memory summary");
        assert_eq!(cards[0]["source_kind"], "memory");
        assert!(cards[0]["actions"].as_array().is_some_and(|actions| {
            actions
                .iter()
                .any(|action| action["kind"] == "view_details")
        }));

        // POST action=dismiss → ok, and the store row is now Dismissed.
        let uri = format!(
            "/channel-assist/resurfacing/{}/action?workspace=default",
            candidate.candidate_id
        );
        let req = actix_test::TestRequest::post()
            .uri(&uri)
            .insert_header(("X-Principal", "anonymous"))
            .insert_header(("X-Workspace", "default"))
            .set_json(serde_json::json!({ "action": "dismiss" }))
            .to_request();
        let resp: serde_json::Value = actix_test::call_and_read_body_json(&app, req).await;
        assert_eq!(resp["ok"], true);

        let got = store
            .get_candidate("anonymous", "default", &candidate.candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.state, CandidateState::Dismissed);
    }

    #[actix_web::test]
    async fn action_rejects_malformed_event_ids_before_source_feedback_commits() {
        let store = ResurfacingStore::open_in_temp();
        let (_channel_store_dir, channel_store) = empty_channel_store();
        let candidate = surfaced_candidate();
        store
            .upsert_candidate("anonymous", "default", &candidate)
            .await
            .unwrap();
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(store.clone()))
                .app_data(web::Data::new(channel_store))
                .route(
                    "/channel-assist/resurfacing/{candidate_id}/action",
                    web::post().to(post_resurfacing_action_handler),
                ),
        )
        .await;
        let uri = format!(
            "/channel-assist/resurfacing/{}/action?workspace=default",
            candidate.candidate_id
        );
        for event_id in [String::new(), "   ".to_string(), "x".repeat(201)] {
            let request = actix_test::TestRequest::post()
                .uri(&uri)
                .insert_header(("X-Principal", "anonymous"))
                .insert_header(("X-Workspace", "default"))
                .set_json(serde_json::json!({
                    "action": "dismiss",
                    "event_id": event_id,
                }))
                .to_request();
            let response = actix_test::call_service(&app, request).await;
            assert_eq!(response.status(), actix_web::http::StatusCode::BAD_REQUEST);
        }
        let persisted = store
            .get_candidate("anonymous", "default", &candidate.candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.state, CandidateState::Surfaced);
        assert!(store
            .list_feedback_attention_repairs_after("anonymous", "default", 0, 0, 10)
            .await
            .unwrap()
            .is_empty());
    }

    #[actix_web::test]
    async fn today_fetch_does_not_count_shown_and_presented_event_is_deduped() {
        let store = ResurfacingStore::open_in_temp();
        let (_channel_store_dir, channel_store) = empty_channel_store();
        let candidate = surfaced_candidate();
        store
            .upsert_candidate("anonymous", "default", &candidate)
            .await
            .unwrap();
        store
            .upsert_recommendation(
                "anonymous",
                "default",
                &candidate.candidate_id,
                &ResurfacingRecommendation {
                    kind: ResurfacingActionKind::ViewDetails,
                    label: "Review details".to_string(),
                    rationale: "This is the safest next step.".to_string(),
                    confidence: 0.9,
                    content_revision: None,
                    source: ResurfacingRecommendationSource::Deterministic,
                },
                2_000,
            )
            .await
            .unwrap();
        let interactions = ResurfacingInteractionRegistry::from_adapters(Vec::new());
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(store.clone()))
                .app_data(web::Data::new(channel_store))
                .app_data(web::Data::new(interactions))
                .route(
                    "/channel-assist/resurfacing/today",
                    web::get().to(get_resurfacing_today_handler),
                )
                .route(
                    "/channel-assist/resurfacing/{candidate_id}/recommendation-event",
                    web::post().to(post_resurfacing_recommendation_event_handler),
                ),
        )
        .await;

        for _ in 0..2 {
            let request = actix_test::TestRequest::get()
                .uri("/channel-assist/resurfacing/today?workspace=default")
                .insert_header(("X-Principal", "anonymous"))
                .insert_header(("X-Workspace", "default"))
                .to_request();
            let response: serde_json::Value =
                actix_test::call_and_read_body_json(&app, request).await;
            assert_eq!(
                response["cards"][0]["recommended_action"]["kind"],
                "view_details"
            );
        }
        assert!(store
            .recommendation_event_aggregates("anonymous", "default")
            .await
            .unwrap()
            .is_empty());
        let event_uri = format!(
            "/channel-assist/resurfacing/{}/recommendation-event?workspace=default",
            candidate.candidate_id
        );
        let event = |event: &str| {
            actix_test::TestRequest::post()
                .uri(&event_uri)
                .insert_header(("X-Principal", "anonymous"))
                .insert_header(("X-Workspace", "default"))
                .set_json(serde_json::json!({
                    "kind": "view_details",
                    "content_revision": null,
                    "event": event,
                }))
                .to_request()
        };
        let presented: serde_json::Value =
            actix_test::call_and_read_body_json(&app, event("presented")).await;
        assert_eq!(presented["recorded"], true);
        let duplicate_presentation: serde_json::Value =
            actix_test::call_and_read_body_json(&app, event("presented")).await;
        assert_eq!(duplicate_presentation["recorded"], false);
        let out_of_order = actix_test::call_service(&app, event("completed")).await;
        assert_eq!(out_of_order.status(), actix_web::http::StatusCode::CONFLICT);
        let selected: serde_json::Value =
            actix_test::call_and_read_body_json(&app, event("selected")).await;
        assert_eq!(selected["recorded"], true);
        let duplicate: serde_json::Value =
            actix_test::call_and_read_body_json(&app, event("selected")).await;
        assert_eq!(duplicate["recorded"], false);
        let completed: serde_json::Value =
            actix_test::call_and_read_body_json(&app, event("completed")).await;
        assert_eq!(completed["recorded"], true);

        let events = store
            .recommendation_event_aggregates("anonymous", "default")
            .await
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == "recommended")
                .map(|event| event.count)
                .sum::<u64>(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == "selected")
                .map(|event| event.count)
                .sum::<u64>(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == "completed")
                .map(|event| event.count)
                .sum::<u64>(),
            1
        );
    }

    #[actix_web::test]
    async fn detail_and_original_are_distinct_and_cross_scope_ids_are_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ResurfacingStore::open(tmp.path()).unwrap();
        let candidate = surfaced_candidate();
        store
            .upsert_candidate("anonymous", "default", &candidate)
            .await
            .unwrap();
        let memory = AgentMemoryResolver::new(tmp.path());
        memory
            .resolve_for_scope("anonymous", "default")
            .unwrap()
            .save_user_knowledge(&serde_json::json!({
                "user.knowledge": [{
                    "key": "trip",
                    "value": "digest",
                    "updated_at": "2026-07-12T00:00:00Z"
                }]
            }))
            .await
            .unwrap();
        let interactions =
            ResurfacingInteractionRegistry::from_adapters(vec![std::sync::Arc::new(
                MemoryInteractionAdapter::new(memory),
            )]);
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(store))
                .app_data(web::Data::new(interactions))
                .route(
                    "/channel-assist/resurfacing/{candidate_id}/detail",
                    web::get().to(get_resurfacing_detail_handler),
                )
                .route(
                    "/channel-assist/resurfacing/{candidate_id}/original",
                    web::get().to(get_resurfacing_original_handler),
                ),
        )
        .await;

        let cross_scope_uri = format!(
            "/channel-assist/resurfacing/{}/detail?workspace=default",
            candidate.candidate_id
        );
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri(&cross_scope_uri)
                .insert_header(("X-Principal", "other"))
                .insert_header(("X-Workspace", "default"))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), actix_web::http::StatusCode::NOT_FOUND);

        let detail_uri = format!(
            "/channel-assist/resurfacing/{}/detail?workspace=default",
            candidate.candidate_id
        );
        let detail: serde_json::Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::get()
                .uri(&detail_uri)
                .insert_header(("X-Principal", "anonymous"))
                .insert_header(("X-Workspace", "default"))
                .to_request(),
        )
        .await;
        assert_eq!(detail["status"], "available");
        assert!(detail["original"].is_null());
        assert_eq!(detail["source"]["kind"], "memory");

        let original_uri = detail_uri.replace("/detail?", "/original?");
        let original: serde_json::Value = actix_test::call_and_read_body_json(
            &app,
            actix_test::TestRequest::get()
                .uri(&original_uri)
                .insert_header(("X-Principal", "anonymous"))
                .insert_header(("X-Workspace", "default"))
                .to_request(),
        )
        .await;
        assert_eq!(original["original"]["kind"], "memory");
        assert_eq!(original["original"]["summary"], "digest");
    }

    #[actix_web::test]
    async fn today_paginates_by_cursor_without_action_offset_drift() {
        let store = ResurfacingStore::open_in_temp();
        let (_channel_store_dir, channel_store) = empty_channel_store();
        let mut first = lane_candidate(SourceKind::Memory, "memory#first");
        first.last_surfaced_at = Some(3_000);
        first.salience_score = 0.9;
        let mut second = lane_candidate(SourceKind::Memory, "memory#second");
        second.last_surfaced_at = Some(2_000);
        second.salience_score = 0.8;
        let mut third = lane_candidate(SourceKind::Memory, "memory#third");
        third.last_surfaced_at = Some(1_000);
        third.salience_score = 0.7;
        for candidate in [&first, &second, &third] {
            store
                .upsert_candidate("anonymous", "default", candidate)
                .await
                .unwrap();
        }

        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(store.clone()))
                .app_data(web::Data::new(channel_store))
                .route(
                    "/channel-assist/resurfacing/today",
                    web::get().to(get_resurfacing_today_handler),
                )
                .route(
                    "/channel-assist/resurfacing/{candidate_id}/action",
                    web::post().to(post_resurfacing_action_handler),
                ),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/channel-assist/resurfacing/today?workspace=default&limit=2")
            .insert_header(("X-Principal", "anonymous"))
            .insert_header(("X-Workspace", "default"))
            .to_request();
        let resp: serde_json::Value = actix_test::call_and_read_body_json(&app, req).await;
        let cards = resp["cards"].as_array().expect("cards array");
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0]["candidate_id"], first.candidate_id);
        assert_eq!(cards[1]["candidate_id"], second.candidate_id);
        assert_eq!(resp["total"].as_u64().unwrap(), 3);
        assert_eq!(resp["limit"].as_u64().unwrap(), 2);
        assert_eq!(resp["has_more"], true);
        let cursor = resp["next_cursor"].as_object().expect("next cursor");
        assert_eq!(cursor["candidate_id"], second.candidate_id);

        // Mutating a previously loaded row changes the live surfaced set. A
        // plain offset=2 page would now skip `third`; the cursor page must not.
        let uri = format!(
            "/channel-assist/resurfacing/{}/action?workspace=default",
            first.candidate_id
        );
        let req = actix_test::TestRequest::post()
            .uri(&uri)
            .insert_header(("X-Principal", "anonymous"))
            .insert_header(("X-Workspace", "default"))
            .set_json(serde_json::json!({ "action": "dismiss" }))
            .to_request();
        let resp: serde_json::Value = actix_test::call_and_read_body_json(&app, req).await;
        assert_eq!(resp["ok"], true);

        let next_uri = format!(
            "/channel-assist/resurfacing/today?workspace=default&limit=2&cursor_surfaced_at={}&cursor_score={}&cursor_candidate_id={}",
            cursor["surfaced_at"].as_i64().unwrap(),
            cursor["score"].as_f64().unwrap(),
            cursor["candidate_id"].as_str().unwrap(),
        );
        let req = actix_test::TestRequest::get()
            .uri(&next_uri)
            .insert_header(("X-Principal", "anonymous"))
            .insert_header(("X-Workspace", "default"))
            .to_request();
        let resp: serde_json::Value = actix_test::call_and_read_body_json(&app, req).await;
        let cards = resp["cards"].as_array().expect("cards array");
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0]["candidate_id"], third.candidate_id);
        assert_eq!(resp["total"].as_u64().unwrap(), 2);
        assert_eq!(resp["has_more"], false);
        assert!(resp["next_cursor"].is_null());
    }

    #[actix_web::test]
    async fn today_prefers_stored_phrasing_over_the_generic_fallback() {
        let store = ResurfacingStore::open_in_temp();
        let (_channel_store_dir, channel_store) = empty_channel_store();
        let candidate = surfaced_candidate();
        store
            .upsert_candidate("anonymous", "default", &candidate)
            .await
            .unwrap();
        // The LLM curator wrote a bespoke line + why for this candidate.
        store
            .upsert_phrasing(
                "anonymous",
                "default",
                &candidate.candidate_id,
                "Want to pick the Tokyo trip back up?",
                "you had momentum here",
                candidate.content_revision.as_deref(),
                2_000,
            )
            .await
            .unwrap();

        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(store.clone()))
                .app_data(web::Data::new(channel_store))
                .route(
                    "/channel-assist/resurfacing/today",
                    web::get().to(get_resurfacing_today_handler),
                ),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/channel-assist/resurfacing/today?workspace=default")
            .insert_header(("X-Principal", "anonymous"))
            .insert_header(("X-Workspace", "default"))
            .to_request();
        let resp: serde_json::Value = actix_test::call_and_read_body_json(&app, req).await;
        let cards = resp["cards"].as_array().expect("cards array");
        assert_eq!(cards.len(), 1);
        // Stored phrasing wins over the title + signal-derived phrase.
        assert_eq!(cards[0]["line"], "Want to pick the Tokyo trip back up?");
        assert_eq!(cards[0]["why_now"], "you had momentum here");
    }

    #[actix_web::test]
    async fn stats_reports_per_lane_engagement() {
        let store = ResurfacingStore::open_in_temp();
        let mem = lane_candidate(SourceKind::Memory, "user.knowledge#m");
        let comm = lane_candidate(SourceKind::Comm, "comm#c");
        store
            .upsert_candidate("anonymous", "default", &mem)
            .await
            .unwrap();
        store
            .upsert_candidate("anonymous", "default", &comm)
            .await
            .unwrap();

        // One positive action (Open) plus a neutral Acknowledge on the memory
        // lane, one dismiss on the comm lane.
        store
            .record_action(
                "anonymous",
                "default",
                &mem.candidate_id,
                FeedbackAction::Open,
                100,
                3_600,
                86_400,
            )
            .await
            .unwrap();
        store
            .record_action(
                "anonymous",
                "default",
                &mem.candidate_id,
                FeedbackAction::Acknowledge,
                200,
                3_600,
                86_400,
            )
            .await
            .unwrap();
        store
            .record_action(
                "anonymous",
                "default",
                &comm.candidate_id,
                FeedbackAction::Dismiss,
                300,
                3_600,
                86_400,
            )
            .await
            .unwrap();

        let app =
            actix_test::init_service(App::new().app_data(web::Data::new(store.clone())).route(
                "/channel-assist/resurfacing/stats",
                web::get().to(get_resurfacing_stats_handler),
            ))
            .await;

        let req = actix_test::TestRequest::get()
            .uri("/channel-assist/resurfacing/stats?workspace=default")
            .insert_header(("X-Principal", "anonymous"))
            .insert_header(("X-Workspace", "default"))
            .to_request();
        let resp: serde_json::Value = actix_test::call_and_read_body_json(&app, req).await;
        let lanes = resp["lanes"].as_array().expect("lanes array");
        assert_eq!(lanes.len(), 2);

        let mem_lane = lanes
            .iter()
            .find(|l| l["source_kind"] == "memory")
            .expect("memory lane");
        assert_eq!(mem_lane["positive"].as_u64().unwrap(), 1);
        assert_eq!(mem_lane["negative"].as_u64().unwrap(), 0);
        // An all-positive lane is up-weighted (> 1.0).
        assert!(mem_lane["utility_multiplier"].as_f64().unwrap() > 1.0);

        let comm_lane = lanes
            .iter()
            .find(|l| l["source_kind"] == "comm")
            .expect("comm lane");
        assert_eq!(comm_lane["positive"].as_u64().unwrap(), 0);
        assert_eq!(comm_lane["negative"].as_u64().unwrap(), 1);
        // An all-negative lane is down-weighted (< 1.0).
        assert!(comm_lane["utility_multiplier"].as_f64().unwrap() < 1.0);
    }

    #[actix_web::test]
    async fn observability_reports_pipeline_funnel_and_sizes() {
        let store = ResurfacingStore::open_in_temp();

        // A surfaced memory candidate (acted on below), an eligible task
        // candidate, and a cooled-down comm candidate. That makes pending (active
        // queue) differ from the raw candidate pool.
        let surfaced = surfaced_candidate();
        store
            .upsert_candidate("anonymous", "default", &surfaced)
            .await
            .unwrap();
        let mut pending = lane_candidate(SourceKind::Task, "task#p");
        pending.state = CandidateState::Candidate;
        store
            .upsert_candidate("anonymous", "default", &pending)
            .await
            .unwrap();
        let mut cooling = lane_candidate(SourceKind::Comm, "comm#cooling");
        cooling.state = CandidateState::Candidate;
        cooling.cooldown_until = i64::MAX;
        store
            .upsert_candidate("anonymous", "default", &cooling)
            .await
            .unwrap();

        // A recorded background pass so the pipeline block carries the kind.
        store
            .record_run(
                "anonymous",
                "default",
                "scorer",
                1_000,
                12,
                4,
                true,
                None,
                1_050,
            )
            .await
            .unwrap();

        // An Open action -> engagement on the memory lane (and the surfaced
        // candidate transitions to `acted`, so `funnel.surfaced` drops to 0).
        store
            .record_action(
                "anonymous",
                "default",
                &surfaced.candidate_id,
                FeedbackAction::Open,
                100,
                3_600,
                86_400,
            )
            .await
            .unwrap();

        let app =
            actix_test::init_service(App::new().app_data(web::Data::new(store.clone())).route(
                "/channel-assist/resurfacing/observability",
                web::get().to(get_resurfacing_observability_handler),
            ))
            .await;

        let req = actix_test::TestRequest::get()
            .uri("/channel-assist/resurfacing/observability?workspace=default")
            .insert_header(("X-Principal", "anonymous"))
            .insert_header(("X-Workspace", "default"))
            .to_request();
        let resp: serde_json::Value = actix_test::call_and_read_body_json(&app, req).await;

        // Every top-level block is present.
        for key in [
            "pipeline",
            "recent_runs",
            "funnel",
            "queue",
            "watermarks",
            "sizes",
            "engagement",
        ] {
            assert!(resp.get(key).is_some(), "missing top-level key {key}");
        }

        // The recorded scorer run surfaces in the pipeline block, keyed by kind.
        assert_eq!(resp["pipeline"]["scorer"]["total"].as_u64().unwrap(), 1);
        assert_eq!(resp["pipeline"]["scorer"]["successes"].as_u64().unwrap(), 1);

        // Queue headline: one eligible pending item, two candidate-state rows in
        // the raw pool, and one cooled-down candidate. The surfaced memory
        // candidate was Open'd -> now `acted`, so surfaced == 0.
        assert_eq!(resp["funnel"]["pending"].as_u64().unwrap(), 1);
        assert_eq!(resp["funnel"]["eligible"].as_u64().unwrap(), 1);
        assert_eq!(resp["funnel"]["candidate_pool"].as_u64().unwrap(), 2);
        assert_eq!(resp["funnel"]["cooling"].as_u64().unwrap(), 1);
        assert_eq!(resp["funnel"]["surfaced"].as_u64().unwrap(), 0);
        assert_eq!(resp["queue"]["pending"].as_u64().unwrap(), 1);
        assert_eq!(resp["queue"]["candidate_pool"].as_u64().unwrap(), 2);
        assert_eq!(resp["queue"]["cooling"].as_u64().unwrap(), 1);

        // Sizes reflect the three candidates + one run.
        assert_eq!(resp["sizes"]["candidates"].as_u64().unwrap(), 3);
        assert_eq!(resp["sizes"]["runs"].as_u64().unwrap(), 1);

        // Engagement recorded the Open on the memory lane.
        let engagement = resp["engagement"].as_array().expect("engagement array");
        assert!(engagement.iter().any(|e| e["source_kind"] == "memory"));

        // The recent run is listed.
        let recent = resp["recent_runs"].as_array().expect("recent_runs array");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0]["kind"], "scorer");
    }
}

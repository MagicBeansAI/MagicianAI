//! Canonical, request-level Follow-up + Worth-a-look projection.
//!
//! This is the only projection allowed to apply a learned cross-lane route:
//! both complete source universes are loaded first, all internal identities are
//! origin-qualified, and every source is materialized exactly once.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

use actix_web::{web, HttpRequest, HttpResponse};
use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex as AsyncMutex, OnceCell as AsyncOnceCell};

use crate::channel_assist::adapter_registry::{self, ChannelThreadRef};
use crate::channel_assist::store::{MailAssistStore, NeedsApprovalPage, NeedsApprovalRow};
use magician::config::{AttentionBanditMode, AttentionGroupingMode, AttentionRoutingMode};
use magician::magician_v2::api_scope::resolve_required_scope;
use magician::magician_v2::attention::learning::AttentionWriterBusy;
use magician::magician_v2::attention::learning::{
    deserialize_semantic_envelope, ActionabilityFeatureInput, AttentionBanditHealth,
    AttentionDecision, AttentionDecisionContext, AttentionDecisionItem, AttentionDeliveryCandidate,
    AttentionDeliveryHealth, AttentionDeliveryPage, AttentionDeliveryReadError,
    AttentionDeliveryRefreshReason, AttentionDeliveryRootDecision, AttentionDeliveryStatus,
    AttentionGroupingHealth, AttentionGroupingMetadata, AttentionLearningHealth,
    AttentionLearningService, AttentionRankMetadata, AttentionRoute, AttentionRoutingCandidate,
    AttentionRoutingHealth, AttentionSurface, CanonicalAttentionRankCandidate,
    CreateAttentionDelivery, FrozenAttentionDelivery, GroupingFeatureInput,
    SemanticAttentionCandidate, ATTENTION_DELIVERY_SCHEMA_VERSION,
};
use magician::magician_v2::attention::resurfacing::source_refs::parse_comm_source_ref;
use magician::magician_v2::attention::resurfacing::store::ResurfacingStore;
use magician::magician_v2::attention::resurfacing::types::{Candidate, SourceKind};

pub const CANONICAL_ATTENTION_PROJECTION_SCHEMA_VERSION: u32 = 1;
pub const CANONICAL_DUPLICATE_ALIAS_LIMIT: usize = 100;

/// How many Worth-a-look rows one union load may ask its store for.
///
/// `ResurfacingStore::list_surfaced_page` clamps its SQL `LIMIT` to 1,000 while
/// returning an unbounded `COUNT(*)` as `total`. Asking for `total` above that
/// requested a page the store had already decided it would never return, so the
/// load could not be complete and the union fell to the unranked baseline for
/// every request until the population dropped back under the clamp. The union
/// asks for a page it can actually be served, and reports the page it got.
const CANONICAL_UNION_WORTH_PAGE_LIMIT: usize = 1_000;

/// Page size the union asks the Follow-up store for on its first (and, for any
/// realistic lane, only) read.
///
/// Unlike Worth-a-look this is not a clamp — the union still loads the whole
/// lane, and [`load_complete_follow_up_universe`] re-reads at the exact total
/// if a lane ever exceeds this. It is a hint chosen so the common case costs
/// one scan instead of the probe-then-load two: the lane holds one row per
/// active needs-you thread, so five figures is already far past any mailbox
/// that could be triaged by a person.
const CANONICAL_UNION_FOLLOW_UP_PAGE_HINT: usize = 20_000;

// A projection may hold thousands of fully materialized cards. Keep enough
// scope locality for normal multi-workspace use without letting a sequence of
// one-off scope reads retain hundreds of large trees.
const CANONICAL_PROJECTION_SCOPE_CACHE_LIMIT: usize = 16;

/// How long a cached canonical projection may be served after its source
/// generation changed. The generation token is a full-scan digest of both
/// lanes, and a live mail watcher changes it between requests, so an exact
/// match can never survive continuous churn — without this window every page
/// load pays the full recompute+persist. Same staleness trade the Today
/// projection cache makes; after the window the strict token check resumes.
const CANONICAL_PROJECTION_STALE_SERVE_WINDOW: std::time::Duration =
    std::time::Duration::from_secs(60);

/// Canonical projections and routing decisions retained per scope beyond the
/// retention tiers, pruned asynchronously in bounded transactions. Anything still
/// referenced by outcomes, impressions, deliveries, or recompute jobs is
/// exempt (see `prune_canonical_history`). At recompute rates driven by
/// mail-sync churn this bounds the store; the tiers alone would keep ~30 days
/// of transient projections (~300/day) with no upper bound on disk.
const CANONICAL_PROJECTION_HISTORY_KEEP: usize = 64;

#[derive(Default)]
struct CanonicalProjectionCacheRegistry {
    entries: HashMap<(String, String, String), Arc<CanonicalProjectionScopeCache>>,
    recency: VecDeque<(String, String, String)>,
}

static CANONICAL_PROJECTION_SCOPE_CACHES: Lazy<StdMutex<CanonicalProjectionCacheRegistry>> =
    Lazy::new(|| StdMutex::new(CanonicalProjectionCacheRegistry::default()));

fn prune_canonical_projection_scope_caches(
    registry: &mut CanonicalProjectionCacheRegistry,
    target_len: usize,
) {
    let mut protected_scanned = 0_usize;
    while registry.entries.len() > target_len && protected_scanned < registry.recency.len() {
        let Some(candidate) = registry.recency.pop_front() else {
            break;
        };
        let removable = registry
            .entries
            .get(&candidate)
            .is_some_and(|cache| Arc::strong_count(cache) == 1);
        if removable {
            registry.entries.remove(&candidate);
            protected_scanned = 0;
        } else {
            registry.recency.push_back(candidate);
            protected_scanned = protected_scanned.saturating_add(1);
        }
    }
}

#[derive(Clone)]
struct CachedCanonicalProjection {
    projection: Arc<CanonicalAttentionProjection>,
    policy_identity: String,
    cached_at: std::time::Instant,
}

/// One scope owns one active projection computation and one last verified
/// success. The active OnceCell is cancellation-safe: if the initializer is
/// dropped, a waiter can take over rather than leaving the scope permanently
/// wedged behind an `in_flight` bit.
struct CanonicalProjectionScopeCache {
    active: AsyncMutex<Option<Arc<AsyncOnceCell<Arc<CanonicalAttentionProjection>>>>>,
    cached: AsyncMutex<Option<CachedCanonicalProjection>>,
}

impl CanonicalProjectionScopeCache {
    fn new() -> Self {
        Self {
            active: AsyncMutex::new(None),
            cached: AsyncMutex::new(None),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("attention learning evidence changed during canonical projection")]
struct CanonicalLearningEvidenceChanged;

#[derive(Debug, thiserror::Error)]
#[error("authoritative source generation changed during canonical projection")]
struct CanonicalSourceGenerationChanged;

fn canonical_projection_scope_cache(
    store_identity: &str,
    principal: &str,
    workspace: &str,
) -> Arc<CanonicalProjectionScopeCache> {
    let key = (
        store_identity.to_string(),
        principal.to_string(),
        workspace.to_string(),
    );
    let mut registry = CANONICAL_PROJECTION_SCOPE_CACHES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(cache) = registry.entries.get(&key).cloned() {
        registry.recency.retain(|candidate| candidate != &key);
        registry.recency.push_back(key);
        return cache;
    }

    // The registry must own a strong reference or a completed flight's cache
    // disappears as soon as its request returns. Evict only registry-only
    // entries; an active request keeps its scope alive and coalescible even
    // when the bounded cache is under pressure.
    prune_canonical_projection_scope_caches(
        &mut registry,
        CANONICAL_PROJECTION_SCOPE_CACHE_LIMIT.saturating_sub(1),
    );
    let cache = Arc::new(CanonicalProjectionScopeCache::new());
    registry.entries.insert(key.clone(), cache.clone());
    registry.recency.push_back(key);
    cache
}

/// Drop the reusable result for a scope after a maintenance mutation changes
/// retained learning evidence. Removing the registry entry also isolates an
/// already-running request: it may finish for its original caller, but later
/// callers cannot attach to or reuse that pre-maintenance flight.
pub fn invalidate_canonical_attention_projection_cache(principal: &str, workspace: &str) {
    let mut registry = CANONICAL_PROJECTION_SCOPE_CACHES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    registry
        .entries
        .retain(|(_, candidate_principal, candidate_workspace), _| {
            candidate_principal != principal || candidate_workspace != workspace
        });
    registry
        .recency
        .retain(|(_, candidate_principal, candidate_workspace)| {
            candidate_principal != principal || candidate_workspace != workspace
        });
}

fn is_retryable_canonical_projection_input_change(error: &anyhow::Error) -> bool {
    error.chain().any(|source| {
        source
            .downcast_ref::<CanonicalLearningEvidenceChanged>()
            .is_some()
            || source
                .downcast_ref::<CanonicalSourceGenerationChanged>()
                .is_some()
    })
}

async fn retry_once_on_projection_input_change<T, F, Fut>(mut operation: F) -> (Result<T>, bool)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    match operation().await {
        Err(error) if is_retryable_canonical_projection_input_change(&error) => {
            (operation().await, true)
        },
        result => (result, false),
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalProjectionStatus {
    Succeeded,
    BaselineFallback,
}

/// Whether Slice-1 learned ordering is actually represented by a canonical
/// projection. The legacy list path being disabled is an implementation detail;
/// it must not be reported to clients as observe mode when the canonical path
/// has successfully applied the configured learned order.
pub const fn canonical_slice1_order_is_active(
    status: CanonicalProjectionStatus,
    load_complete: bool,
    semantic_ranking_enabled: bool,
) -> bool {
    matches!(status, CanonicalProjectionStatus::Succeeded)
        && load_complete
        && semantic_ranking_enabled
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalAttentionLane {
    FollowUp,
    WorthALook,
    NonSurfaced,
}

impl From<AttentionRoute> for CanonicalAttentionLane {
    fn from(route: AttentionRoute) -> Self {
        match route {
            AttentionRoute::FollowUp => Self::FollowUp,
            AttentionRoute::WorthALook => Self::WorthALook,
            AttentionRoute::NonSurfaced => Self::NonSurfaced,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CanonicalAttentionPolicy {
    pub mode: AttentionRoutingMode,
    pub snapshot_id: Option<String>,
    pub model_version: Option<String>,
    pub seed_identity: String,
    pub canary_fraction: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CanonicalAttentionIntegrity {
    pub load_complete: bool,
    pub exact_once: bool,
    pub source_total: usize,
    pub follow_up_source_total: usize,
    pub worth_a_look_source_total: usize,
    pub reconciled_total: usize,
    pub grouped_member_total: usize,
    pub materialized_total: usize,
    pub follow_up_lane_total: usize,
    pub worth_a_look_lane_total: usize,
    pub non_surfaced_total: usize,
    pub duplicate_hidden_total: usize,
    pub unmatched_total: usize,
    pub fallback_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CrossLaneReconciliationStatus {
    Succeeded,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CrossLaneReconciliationReason {
    FollowUpStoreUnavailable,
    FollowUpLoadUnavailable,
    FollowUpSourceChanged,
    WorthALookLoadUnavailable,
    MalformedWorthCommSourceRef,
    CanonicalProjectionUnavailable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalDuplicateAliasReason {
    ExactSourceIdentity,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CanonicalDuplicateAlias {
    pub owner_canonical_id: String,
    pub duplicate_canonical_id: String,
    pub reason: CanonicalDuplicateAliasReason,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CanonicalCrossLaneReconciliationHealth {
    pub schema_version: u32,
    pub status: CrossLaneReconciliationStatus,
    pub reason: Option<CrossLaneReconciliationReason>,
    pub authoritative_lane: CanonicalAttentionLane,
    pub principal: String,
    pub workspace: String,
    pub follow_up_source_total: Option<usize>,
    pub worth_a_look_source_total: Option<usize>,
    pub raw_source_total: Option<usize>,
    pub unique_source_total: Option<usize>,
    pub duplicate_hidden_total: usize,
    pub alias_record_total: usize,
    pub alias_records_returned: usize,
    pub aliases_truncated: bool,
    pub reconciliation_digest: Option<String>,
}

impl Default for CanonicalCrossLaneReconciliationHealth {
    fn default() -> Self {
        Self {
            schema_version: 1,
            status: CrossLaneReconciliationStatus::Unavailable,
            reason: Some(CrossLaneReconciliationReason::CanonicalProjectionUnavailable),
            authoritative_lane: CanonicalAttentionLane::FollowUp,
            principal: String::new(),
            workspace: String::new(),
            follow_up_source_total: None,
            worth_a_look_source_total: None,
            raw_source_total: None,
            unique_source_total: None,
            duplicate_hidden_total: 0,
            alias_record_total: 0,
            alias_records_returned: 0,
            aliases_truncated: false,
            reconciliation_digest: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LegacyWorthCrossLaneReconciliationHealth {
    pub schema_version: u32,
    pub status: CrossLaneReconciliationStatus,
    pub reason: Option<CrossLaneReconciliationReason>,
    pub authoritative_lane: CanonicalAttentionLane,
    pub principal: String,
    pub workspace: String,
    pub follow_up_source_total: Option<usize>,
    pub worth_a_look_source_total: Option<usize>,
    pub raw_source_total: Option<usize>,
    pub visible_source_total: Option<usize>,
    pub duplicate_hidden_total: Option<usize>,
    pub raw_scanned_total: Option<usize>,
    pub visible_page_total: Option<usize>,
    pub duplicate_hidden_page_total: Option<usize>,
    pub reconciliation_digest: Option<String>,
}

/// One raw Worth-a-look row a page examined, and whether an active Follow-up
/// owned its conversation.
pub type ScannedWorthRow<'a> = (&'a Candidate, bool);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CanonicalAttentionGroup {
    pub cluster_id: String,
    pub representative_id: String,
    pub member_ids: Vec<String>,
    pub member_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalOriginActionKind {
    OpenSource,
    Useful,
    Approve,
    Acknowledge,
    Dismiss,
    Snooze,
    OwnerWork,
    WrongLane,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalOriginActionMethod {
    Get,
    Post,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CanonicalOriginAction {
    pub id: String,
    pub kind: CanonicalOriginActionKind,
    pub label: String,
    pub method: CanonicalOriginActionMethod,
    pub href: String,
    pub requires_confirmation: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CanonicalAttentionOrigin {
    FollowUp {
        annotation_id: String,
        provider: String,
        account_alias: String,
        thread_id: String,
    },
    WorthALook {
        candidate_id: String,
        source_kind: String,
        source_ref: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CanonicalAttentionPayload {
    FollowUp {
        annotation_id: String,
        subject: Option<String>,
        sender: Option<String>,
        summary: Option<String>,
        label: Option<String>,
        reason: Option<String>,
        received_at: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        due_text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        due_at: Option<i64>,
        open_url: Option<String>,
    },
    WorthALook {
        candidate_id: String,
        line: String,
        why_now: String,
        summary: String,
        source_title: String,
        source_kind: String,
        source_ref: String,
        open_url: Option<String>,
        temporal_anchor_at: Option<i64>,
        brief: Option<serde_json::Value>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CanonicalAttentionItem {
    pub canonical_id: String,
    pub source_revision: Option<String>,
    pub origin_lane: CanonicalAttentionLane,
    pub served_lane: CanonicalAttentionLane,
    pub learned_lane: CanonicalAttentionLane,
    pub route_reason: String,
    pub route_applied: bool,
    pub origin: CanonicalAttentionOrigin,
    pub group: CanonicalAttentionGroup,
    pub actions: Vec<CanonicalOriginAction>,
    pub payload: CanonicalAttentionPayload,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CanonicalAttentionLanes {
    pub follow_up: Vec<CanonicalAttentionItem>,
    pub worth_a_look: Vec<CanonicalAttentionItem>,
    pub non_surfaced: Vec<CanonicalAttentionItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CanonicalAttentionProjection {
    pub schema_version: u32,
    pub status: CanonicalProjectionStatus,
    pub projection_id: String,
    pub universe_digest: String,
    /// Compact identity of both authoritative source lanes. Frozen cursor
    /// reads compare this without rebuilding the complete canonical union.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_generation_token: Option<String>,
    pub created_at: i64,
    pub policy: CanonicalAttentionPolicy,
    pub integrity: CanonicalAttentionIntegrity,
    #[serde(default)]
    pub cross_lane_reconciliation: CanonicalCrossLaneReconciliationHealth,
    #[serde(default)]
    pub duplicate_aliases: Vec<CanonicalDuplicateAlias>,
    /// Compact health captured from the same complete-universe evaluation as
    /// the served projection. Legacy lane endpoints consume this instead of
    /// running a second, surface-local learning projection that can disagree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<CanonicalAttentionDiagnostics>,
    pub lanes: CanonicalAttentionLanes,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CanonicalAttentionDiagnostics {
    pub rank_generation: u64,
    pub rank_scope: String,
    pub follow_up_health: AttentionLearningHealth,
    pub worth_a_look_health: AttentionLearningHealth,
    pub grouping_mode: AttentionGroupingMode,
    pub grouping_snapshot_id: Option<String>,
    pub grouping_generation: u64,
    pub grouping_scope: String,
    pub grouping_health: AttentionGroupingHealth,
    pub decision: AttentionDecision,
    pub routing_health: AttentionRoutingHealth,
    /// Visibility proof required to interpret the decision and routing health.
    /// Optional only so projections persisted by an older binary remain
    /// readable after an in-place upgrade.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impression_policy: Option<AttentionDeliveryImpressionPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandit_health: Option<AttentionBanditHealth>,
    /// Per-item rank and routing bindings from this same evaluation.
    ///
    /// Lane endpoints attach these to each served row. Without them a row
    /// carries no learned rank and no decision binding, and the client cannot
    /// record a verified impression against the decision that served it — so
    /// no reward signal ever reaches the model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ranks: Vec<AttentionRankMetadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decision_items: Vec<AttentionDecisionItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CanonicalAttentionProjectionResponse {
    pub canonical_attention_projection: CanonicalAttentionProjection,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CanonicalAttentionProjectionQuery {
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AttentionDeliveryQuery {
    pub workspace: Option<String>,
    pub page_size: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttentionDeliveryImpressionPolicy {
    pub min_visible_ms: u64,
    pub visibility_rule_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionDeliveryResponseItem {
    pub position: usize,
    pub candidate_id: String,
    pub source_revision: Option<String>,
    pub root_policy_propensity: f64,
    pub conditional_delivery_propensity: f64,
    pub exposure_token: String,
    pub item: CanonicalAttentionItem,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionDeliveryPageResponse {
    pub schema_version: u32,
    pub status: AttentionDeliveryStatus,
    pub fallback_reason: Option<String>,
    pub root_decision: AttentionDeliveryRootDecision,
    pub page: AttentionDeliveryPage,
    pub items: Vec<AttentionDeliveryResponseItem>,
    pub health: AttentionDeliveryHealth,
    pub impression_policy: AttentionDeliveryImpressionPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttentionDeliveryRefreshRequired {
    pub schema_version: u32,
    pub status: String,
    pub error: String,
    pub reason: AttentionDeliveryRefreshReason,
    pub lane: AttentionSurface,
    pub refresh_href: String,
}

#[derive(Debug, Clone)]
struct CanonicalSource {
    semantic: SemanticAttentionCandidate,
    evidence_surface: AttentionSurface,
    evidence_candidate_id: String,
    source_family: String,
    baseline_route: AttentionRoute,
    origin: CanonicalAttentionOrigin,
    actions: Vec<CanonicalOriginAction>,
    payload: CanonicalAttentionPayload,
    cross_surface_identity: Option<CrossSurfaceIdentity>,
}

/// Exact provider-owned conversation identity. It is compared component-wise
/// and never exposed or inferred from content text.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CrossSurfaceIdentity {
    provider: String,
    account_alias: String,
    thread_id: String,
}

impl CrossSurfaceIdentity {
    fn digest(&self) -> String {
        let mut digest = blake3::Hasher::new();
        for component in [&self.provider, &self.account_alias, &self.thread_id] {
            digest.update(&(component.len() as u64).to_le_bytes());
            digest.update(component.as_bytes());
        }
        digest.finalize().to_hex().to_string()
    }
}

fn canonical_follow_up_id(annotation_id: &str) -> String {
    AttentionSurface::FollowUp.canonical_candidate_id(annotation_id)
}

fn canonical_worth_id(candidate_id: &str) -> String {
    AttentionSurface::WorthALook.canonical_candidate_id(candidate_id)
}

fn canonical_learned_order_applies(
    slice1_enabled: bool,
    routing_mode: AttentionRoutingMode,
    routing_canary_assigned: bool,
    later_ranking_enabled: bool,
) -> bool {
    slice1_enabled
        || (routing_mode == AttentionRoutingMode::Canary
            && routing_canary_assigned
            && later_ranking_enabled)
}

fn worth_cross_surface_identity(candidate: &Candidate) -> Result<Option<CrossSurfaceIdentity>> {
    if candidate.source_kind != SourceKind::Comm {
        return Ok(None);
    }
    comm_cross_surface_identity(&candidate.source_ref).map(Some)
}

/// The exact typed conversation identity a communication source reference
/// names. A malformed reference is an error, never a distinct card.
fn comm_cross_surface_identity(source_ref: &str) -> Result<CrossSurfaceIdentity> {
    let parsed = parse_comm_source_ref(source_ref)
        .context("malformed Worth-a-look communication source reference")?;
    Ok(CrossSurfaceIdentity {
        provider: parsed.provider,
        account_alias: parsed.account_alias,
        thread_id: parsed.thread_id,
    })
}

fn proposed_action_due_text(action: Option<&serde_json::Value>) -> Option<String> {
    action
        .and_then(|value| value.get("due_text"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn proposed_action_due_at(action: Option<&serde_json::Value>) -> Option<i64> {
    let value = action.and_then(|value| value.get("due_at"))?;
    value
        .as_i64()
        .or_else(|| {
            value
                .as_f64()
                .and_then(|number| number.is_finite().then_some(number as i64))
        })
        .or_else(|| {
            value.as_str().and_then(|text| {
                chrono::DateTime::parse_from_rfc3339(text.trim())
                    .ok()
                    .map(|parsed| parsed.timestamp_millis())
            })
        })
        .filter(|at| *at > 0)
}

fn sender(row: &NeedsApprovalRow) -> Option<String> {
    match (&row.from_name, &row.from_address) {
        (Some(name), Some(address)) => Some(format!("{name} <{address}>")),
        (Some(name), None) => Some(name.clone()),
        (None, Some(address)) => Some(address.clone()),
        (None, None) => None,
    }
}

fn follow_up_source(row: &NeedsApprovalRow, as_of_ms: i64) -> CanonicalSource {
    let canonical_id = canonical_follow_up_id(&row.annotation_id);
    let cross_surface_identity = CrossSurfaceIdentity {
        provider: row.provider.clone(),
        account_alias: row.account_alias.clone(),
        thread_id: row.thread_id.clone(),
    };
    let source_revision = row
        .classification_input_revision
        .map(|revision| format!("distill:{revision}"));
    let proposed_action = row
        .proposed_action
        .as_ref()
        .and_then(|value| serde_json::to_string(value).ok())
        .unwrap_or_default();
    let open_url = adapter_registry::thread_url_for(ChannelThreadRef {
        provider: &row.provider,
        account_alias: &row.account_alias,
        account_email: row.account_email.as_deref(),
        thread_id: &row.thread_id,
    });
    let action_base = format!(
        "/api/magician/v2/channel-assist/annotations/{}",
        urlencoding::encode(&row.annotation_id)
    );
    let mut actions = vec![CanonicalOriginAction {
        id: "open_source".to_string(),
        kind: CanonicalOriginActionKind::OpenSource,
        label: "Open".to_string(),
        method: CanonicalOriginActionMethod::Get,
        href: open_url
            .clone()
            .unwrap_or_else(|| format!("{action_base}/message")),
        requires_confirmation: false,
    }];
    for (id, kind, label, confirmation) in [
        (
            "approve",
            CanonicalOriginActionKind::Approve,
            "Approve",
            true,
        ),
        (
            "acknowledge",
            CanonicalOriginActionKind::Acknowledge,
            "Acknowledge",
            false,
        ),
        ("useful", CanonicalOriginActionKind::Useful, "Useful", false),
        (
            "wrong_lane",
            CanonicalOriginActionKind::WrongLane,
            "Shouldn't have been flagged",
            false,
        ),
        (
            "dismiss",
            CanonicalOriginActionKind::Dismiss,
            "Dismiss",
            true,
        ),
        ("snooze", CanonicalOriginActionKind::Snooze, "Snooze", false),
    ] {
        actions.push(CanonicalOriginAction {
            id: id.to_string(),
            kind,
            label: label.to_string(),
            method: CanonicalOriginActionMethod::Post,
            href: format!("{action_base}/{id}"),
            requires_confirmation: confirmation,
        });
    }
    CanonicalSource {
        semantic: SemanticAttentionCandidate {
            candidate_id: canonical_id,
            source_revision: source_revision.clone(),
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
                    as_of_ms.saturating_sub(received_at).max(0) as f64 / 86_400_000.0
                }),
                ..Default::default()
            }),
            grouping_features: Some(GroupingFeatureInput {
                semantic: deserialize_semantic_envelope(row.semantic_features.as_ref()),
                exact_source_identity: Some(cross_surface_identity.digest()),
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
        },
        evidence_surface: AttentionSurface::FollowUp,
        evidence_candidate_id: row.annotation_id.clone(),
        source_family: "follow_up".to_string(),
        baseline_route: AttentionRoute::FollowUp,
        origin: CanonicalAttentionOrigin::FollowUp {
            annotation_id: row.annotation_id.clone(),
            provider: row.provider.clone(),
            account_alias: row.account_alias.clone(),
            thread_id: row.thread_id.clone(),
        },
        actions,
        payload: CanonicalAttentionPayload::FollowUp {
            annotation_id: row.annotation_id.clone(),
            subject: row.subject.clone(),
            sender: sender(row),
            summary: row.latest_summary.clone(),
            label: row.label.clone(),
            reason: row.reason.clone(),
            received_at: row.last_message_at,
            due_text: proposed_action_due_text(row.proposed_action.as_ref()),
            due_at: proposed_action_due_at(row.proposed_action.as_ref()),
            open_url,
        },
        cross_surface_identity: Some(cross_surface_identity),
    }
}

fn worth_source(candidate: &Candidate, as_of_ms: i64) -> Result<CanonicalSource> {
    let canonical_id = canonical_worth_id(&candidate.candidate_id);
    let cross_surface_identity = worth_cross_surface_identity(candidate)?;
    let source_revision = candidate
        .content_revision
        .clone()
        .or_else(|| Some(candidate.content_digest.clone()));
    let details = candidate
        .content_details
        .as_ref()
        .and_then(|details| serde_json::to_string(details).ok())
        .unwrap_or_default();
    let open_url = (candidate.source_ref.starts_with("https://")
        || candidate.source_ref.starts_with("http://"))
    .then(|| candidate.source_ref.clone());
    let action_href = format!(
        "/api/magician/v2/channel-assist/resurfacing/{}/action",
        urlencoding::encode(&candidate.candidate_id)
    );
    let mut actions = vec![CanonicalOriginAction {
        id: "open_source".to_string(),
        kind: CanonicalOriginActionKind::OpenSource,
        label: "Open".to_string(),
        method: CanonicalOriginActionMethod::Get,
        href: open_url.clone().unwrap_or_else(|| {
            format!(
                "/api/magician/v2/channel-assist/resurfacing/{}/detail",
                urlencoding::encode(&candidate.candidate_id)
            )
        }),
        requires_confirmation: false,
    }];
    actions.push(CanonicalOriginAction {
        id: "useful".to_string(),
        kind: CanonicalOriginActionKind::Useful,
        label: "Useful".to_string(),
        method: CanonicalOriginActionMethod::Post,
        href: action_href.clone(),
        requires_confirmation: false,
    });
    actions.push(CanonicalOriginAction {
        id: "owner_work".to_string(),
        kind: CanonicalOriginActionKind::OwnerWork,
        label: "This needs me".to_string(),
        method: CanonicalOriginActionMethod::Post,
        href: action_href.clone(),
        requires_confirmation: false,
    });
    actions.push(CanonicalOriginAction {
        id: "acknowledge".to_string(),
        kind: CanonicalOriginActionKind::Acknowledge,
        label: "Acknowledge".to_string(),
        method: CanonicalOriginActionMethod::Post,
        href: action_href.clone(),
        requires_confirmation: false,
    });
    actions.push(CanonicalOriginAction {
        id: "dismiss".to_string(),
        kind: CanonicalOriginActionKind::Dismiss,
        label: "Dismiss".to_string(),
        method: CanonicalOriginActionMethod::Post,
        href: action_href,
        requires_confirmation: true,
    });
    Ok(CanonicalSource {
        semantic: SemanticAttentionCandidate {
            candidate_id: canonical_id,
            source_revision: source_revision.clone(),
            semantic_text: [
                candidate.title.as_str(),
                candidate.content_digest.as_str(),
                details.as_str(),
            ]
            .join("\n"),
            existing_embedding: None,
            actionability_features: Some(ActionabilityFeatureInput {
                semantic: deserialize_semantic_envelope(candidate.semantic_features.as_ref()),
                age_days: candidate.last_surfaced_at.map(|surfaced_at| {
                    as_of_ms.saturating_sub(surfaced_at).max(0) as f64 / 86_400_000.0
                }),
                slice1_actionability_probability: Some(
                    candidate.salience_score.clamp(0.0, 1.0) as f64
                ),
                ..Default::default()
            }),
            grouping_features: Some(GroupingFeatureInput {
                exact_source_identity: cross_surface_identity
                    .as_ref()
                    .map(CrossSurfaceIdentity::digest)
                    .or_else(|| {
                        Some(format!(
                            "{}:{}",
                            candidate.source_kind.as_str(),
                            candidate.source_ref
                        ))
                    }),
                event_at_ms: candidate.temporal_anchor_at,
                ..Default::default()
            }),
        },
        evidence_surface: AttentionSurface::WorthALook,
        evidence_candidate_id: candidate.candidate_id.clone(),
        source_family: candidate.source_kind.as_str().to_string(),
        baseline_route: AttentionRoute::WorthALook,
        origin: CanonicalAttentionOrigin::WorthALook {
            candidate_id: candidate.candidate_id.clone(),
            source_kind: candidate.source_kind.as_str().to_string(),
            source_ref: candidate.source_ref.clone(),
        },
        actions,
        payload: CanonicalAttentionPayload::WorthALook {
            candidate_id: candidate.candidate_id.clone(),
            line: candidate.title.clone(),
            why_now: "Currently surfaced by the Worth-a-look curator".to_string(),
            summary: candidate.content_digest.clone(),
            source_title: candidate.title.clone(),
            source_kind: candidate.source_kind.as_str().to_string(),
            source_ref: candidate.source_ref.clone(),
            open_url,
            temporal_anchor_at: candidate.temporal_anchor_at,
            brief: candidate
                .content_details
                .as_ref()
                .and_then(|details| serde_json::to_value(details).ok()),
        },
        cross_surface_identity,
    })
}

pub fn unavailable_legacy_worth_reconciliation(
    principal: &str,
    workspace: &str,
    reason: CrossLaneReconciliationReason,
) -> LegacyWorthCrossLaneReconciliationHealth {
    LegacyWorthCrossLaneReconciliationHealth {
        schema_version: 1,
        status: CrossLaneReconciliationStatus::Unavailable,
        reason: Some(reason),
        authoritative_lane: CanonicalAttentionLane::FollowUp,
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        follow_up_source_total: None,
        worth_a_look_source_total: None,
        raw_source_total: None,
        visible_source_total: None,
        duplicate_hidden_total: None,
        raw_scanned_total: None,
        visible_page_total: None,
        duplicate_hidden_page_total: None,
        reconciliation_digest: None,
    }
}

pub fn legacy_worth_reconciliation_error_reason(
    error: &anyhow::Error,
) -> CrossLaneReconciliationReason {
    let chain = error
        .chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ");
    if chain.contains("malformed Worth-a-look communication source reference") {
        CrossLaneReconciliationReason::MalformedWorthCommSourceRef
    } else if chain.contains("Worth-a-look cursor scan")
        || chain.contains("Worth-a-look lane page scan")
        || chain.contains("Worth-a-look lane totals")
    {
        CrossLaneReconciliationReason::WorthALookLoadUnavailable
    } else if chain.contains("Follow-up source changed during legacy cross-lane reconciliation") {
        CrossLaneReconciliationReason::FollowUpSourceChanged
    } else {
        CrossLaneReconciliationReason::FollowUpLoadUnavailable
    }
}

/// The active Follow-up conversations, keyed by the exact provider-owned
/// identity a Worth-a-look candidate can collide with.
///
/// This is the whole input the cross-lane rule needs. The rule — *an email
/// thread with an open follow-up must not also appear under Worth a look,
/// Follow-up authoritative* — is decided per candidate, so a reader needs this
/// set and the rows it is actually serving, never the rest of the lane.
pub struct LegacyWorthFollowUpOwners {
    owners: HashMap<CrossSurfaceIdentity, String>,
    source_total: usize,
    material: Vec<(String, String, Option<i64>)>,
}

impl LegacyWorthFollowUpOwners {
    /// Whether an active Follow-up already owns this candidate's conversation.
    ///
    /// Only `comm` candidates carry a conversation identity at all; everything
    /// else is never a cross-lane duplicate. A malformed communication
    /// reference is an error rather than a distinct card, exactly as before.
    pub fn owns(&self, candidate: &Candidate) -> Result<bool> {
        let Some(identity) = worth_cross_surface_identity(candidate)? else {
            return Ok(false);
        };
        Ok(self.owners.contains_key(&identity))
    }

    /// How many of the lane's surfaced communication rows this owner set hides.
    ///
    /// The lane-wide hidden count is the only whole-lane quantity a page needs,
    /// and it is a count: it reads one column from the communication subset of
    /// the lane rather than paging the lane's full rows into memory.
    pub fn count_owned_source_refs(&self, comm_source_refs: &[String]) -> Result<usize> {
        let mut owned = 0usize;
        for source_ref in comm_source_refs {
            if self
                .owners
                .contains_key(&comm_cross_surface_identity(source_ref)?)
            {
                owned = owned.saturating_add(1);
            }
        }
        Ok(owned)
    }
}

/// Load the active Follow-up owner set once, proving it was read from one
/// consistent Follow-up lane.
///
/// The probe/load/verify triple is unchanged: it is what makes the owner set a
/// snapshot rather than three interleaved reads.
pub async fn load_active_follow_up_owners(
    mail_store: &MailAssistStore,
    principal: &str,
    workspace: &str,
) -> Result<LegacyWorthFollowUpOwners> {
    let probe = mail_store
        .list_needs_approval_attention_lane_page(principal, workspace, "follow_up", 1, 0, None)
        .await
        .context("probing active Follow-up identities for legacy cross-lane reconciliation")?;
    let loaded = mail_store
        .list_needs_approval_attention_lane_page(
            principal,
            workspace,
            "follow_up",
            probe.total.max(1) as usize,
            0,
            None,
        )
        .await
        .context("loading active Follow-up identities for legacy cross-lane reconciliation")?;
    let verify = mail_store
        .list_needs_approval_attention_lane_page(principal, workspace, "follow_up", 1, 0, None)
        .await
        .context("verifying active Follow-up identities for legacy cross-lane reconciliation")?;
    anyhow::ensure!(
        loaded.rows.len() as u64 == loaded.total
            && loaded.total == probe.total
            && verify.total == probe.total
            && match (probe.rows.first(), verify.rows.first()) {
                (Some(before), Some(after)) => {
                    loaded.rows.first().is_some_and(|during| {
                        before.annotation_id == during.annotation_id
                            && before.classification_input_revision
                                == during.classification_input_revision
                    }) && before.annotation_id == after.annotation_id
                        && before.classification_input_revision
                            == after.classification_input_revision
                },
                (None, None) => true,
                _ => false,
            },
        "Follow-up source changed during legacy cross-lane reconciliation"
    );

    let mut owners = HashMap::<CrossSurfaceIdentity, String>::new();
    let mut follow_up_material = Vec::with_capacity(loaded.rows.len());
    for row in &loaded.rows {
        let identity = CrossSurfaceIdentity {
            provider: row.provider.clone(),
            account_alias: row.account_alias.clone(),
            thread_id: row.thread_id.clone(),
        };
        let canonical_id = canonical_follow_up_id(&row.annotation_id);
        owners
            .entry(identity.clone())
            .and_modify(|owner| {
                if canonical_id.as_str() < owner.as_str() {
                    *owner = canonical_id.clone();
                }
            })
            .or_insert(canonical_id.clone());
        follow_up_material.push((
            identity.digest(),
            canonical_id,
            row.classification_input_revision,
        ));
    }
    follow_up_material.sort();

    Ok(LegacyWorthFollowUpOwners {
        owners,
        source_total: loaded.rows.len(),
        material: follow_up_material,
    })
}

/// Report one legacy Worth read: the lane's totals, and what this page scanned
/// to serve its rows.
///
/// The digest is health telemetry that identifies the reconciliation inputs
/// this read used, and it is now built from exactly those — the owner set plus
/// the page's own rows. It used to hash every candidate in the lane, which is
/// the only thing in cross-lane reconciliation that ever genuinely iterated the
/// universe, and it did so on every page of every read. It cannot simply be
/// dropped: a succeeded reconciliation with a null digest fails the client's
/// contract parse outright.
pub fn legacy_worth_page_health(
    owners: &LegacyWorthFollowUpOwners,
    principal: &str,
    workspace: &str,
    raw_source_total: usize,
    duplicate_hidden_total: usize,
    scanned: &[ScannedWorthRow<'_>],
) -> Result<LegacyWorthCrossLaneReconciliationHealth> {
    let mut alias_material = Vec::new();
    let mut worth_material = Vec::with_capacity(scanned.len());
    let mut duplicate_hidden_page_total = 0usize;
    for (candidate, owned) in scanned {
        let source_revision = candidate
            .content_revision
            .clone()
            .unwrap_or_else(|| candidate.content_digest.clone());
        worth_material.push((
            candidate.candidate_id.clone(),
            source_revision,
            candidate.source_kind.as_str().to_string(),
            candidate.source_ref.clone(),
        ));
        if !*owned {
            continue;
        }
        duplicate_hidden_page_total = duplicate_hidden_page_total.saturating_add(1);
        let owner = worth_cross_surface_identity(candidate)?
            .as_ref()
            .and_then(|identity| owners.owners.get(identity))
            .cloned()
            .unwrap_or_default();
        alias_material.push((owner, canonical_worth_id(&candidate.candidate_id)));
    }
    worth_material.sort();
    alias_material.sort();
    let mut digest = blake3::Hasher::new();
    digest.update(b"legacy-worth-cross-lane-v1\x1f");
    digest.update(principal.as_bytes());
    digest.update(b"\x1f");
    digest.update(workspace.as_bytes());
    digest.update(b"\x1f");
    digest.update(&serde_json::to_vec(&owners.material)?);
    digest.update(b"\x1f");
    digest.update(&serde_json::to_vec(&worth_material)?);
    digest.update(b"\x1f");
    digest.update(&serde_json::to_vec(&alias_material)?);
    let visible_source_total = raw_source_total.saturating_sub(duplicate_hidden_total);
    let raw_scanned_total = scanned.len();
    Ok(LegacyWorthCrossLaneReconciliationHealth {
        schema_version: 1,
        status: CrossLaneReconciliationStatus::Succeeded,
        reason: None,
        authoritative_lane: CanonicalAttentionLane::FollowUp,
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        follow_up_source_total: Some(owners.source_total),
        worth_a_look_source_total: Some(raw_source_total),
        raw_source_total: Some(raw_source_total),
        visible_source_total: Some(visible_source_total),
        duplicate_hidden_total: Some(duplicate_hidden_total),
        raw_scanned_total: Some(raw_scanned_total),
        visible_page_total: Some(raw_scanned_total.saturating_sub(duplicate_hidden_page_total)),
        duplicate_hidden_page_total: Some(duplicate_hidden_page_total),
        reconciliation_digest: Some(digest.finalize().to_hex().to_string()),
    })
}

async fn projection_policy_identity(
    learning: &AttentionLearningService,
    principal: &str,
    workspace: &str,
) -> Result<String> {
    let learning_identity = learning
        .canonical_projection_learning_identity(principal, workspace)
        .await?;
    let actionability = learning.resolve_actionability(principal, workspace).await?;
    let actionability_snapshot = actionability
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.snapshot_id.as_str())
        .or_else(|| learning.actionability_snapshot_id())
        .unwrap_or("none");
    let (routing_mode, routing_snapshot) = learning.resolve_routing(principal, workspace).await?;
    let routing_snapshot_id = routing_snapshot
        .as_ref()
        .map(|snapshot| snapshot.snapshot_id.as_str())
        .or_else(|| learning.routing_snapshot_id())
        .unwrap_or("baseline");
    Ok(format!(
        "routing={}:{}:{}:{:.12}:semantic_rank={}:actionability={}:{}:grouping={}:{}:diagnostics=canonical-health-v1:{learning_identity}",
        routing_mode.as_str(),
        routing_snapshot_id,
        learning.routing_seed_identity(),
        learning.routing_canary_fraction(),
        learning.semantic_ranking_enabled(),
        actionability.mode.as_str(),
        actionability_snapshot,
        learning.grouping_mode().as_str(),
        learning.grouping_snapshot_id().unwrap_or("none"),
    ))
}

fn reconcile_cross_lane_sources(
    raw_sources: Vec<CanonicalSource>,
) -> (Vec<CanonicalSource>, Vec<CanonicalDuplicateAlias>) {
    let follow_up_owners = raw_sources
        .iter()
        .filter(|source| source.baseline_route == AttentionRoute::FollowUp)
        .filter_map(|source| {
            source
                .cross_surface_identity
                .clone()
                .map(|identity| (identity, source.semantic.candidate_id.clone()))
        })
        .fold(
            HashMap::<CrossSurfaceIdentity, String>::new(),
            |mut owners, (identity, canonical_id)| {
                owners
                    .entry(identity)
                    .and_modify(|owner| {
                        if canonical_id.as_str() < owner.as_str() {
                            *owner = canonical_id.clone();
                        }
                    })
                    .or_insert(canonical_id);
                owners
            },
        );
    let mut aliases = Vec::new();
    let mut unique = Vec::with_capacity(raw_sources.len());
    for source in raw_sources {
        let owner: Option<&String> = (source.baseline_route == AttentionRoute::WorthALook)
            .then(|| source.cross_surface_identity.as_ref())
            .flatten()
            .and_then(|identity| follow_up_owners.get(identity));
        if let Some(owner_canonical_id) = owner {
            aliases.push(CanonicalDuplicateAlias {
                owner_canonical_id: owner_canonical_id.clone(),
                duplicate_canonical_id: source.semantic.candidate_id.clone(),
                reason: CanonicalDuplicateAliasReason::ExactSourceIdentity,
            });
        } else {
            unique.push(source);
        }
    }
    aliases.sort_by(|left, right| {
        (&left.owner_canonical_id, &left.duplicate_canonical_id)
            .cmp(&(&right.owner_canonical_id, &right.duplicate_canonical_id))
    });
    (unique, aliases)
}

fn universe_digest_source_state(raw_sources: &[CanonicalSource]) -> Result<blake3::Hasher> {
    let mut identities = raw_sources.iter().collect::<Vec<_>>();
    identities.sort_by(|left, right| left.semantic.candidate_id.cmp(&right.semantic.candidate_id));
    let mut digest = blake3::Hasher::new();
    digest.update(b"canonical-cross-lane-reconciliation-v1\x1f");
    for source in identities {
        // Serialize one identity at a time. Retaining every encoded payload in
        // a second vector amplified the complete canonical union before any
        // projection storage work had even begun.
        let material = serde_json::to_vec(&(
            &source.semantic.candidate_id,
            &source.semantic.source_revision,
            &source.origin,
            &source.payload,
            &source.actions,
        ))?;
        digest.update(source.semantic.candidate_id.as_bytes());
        digest.update(b"\x1e");
        digest.update(&material);
        digest.update(b"\x1f");
    }
    Ok(digest)
}

fn finish_universe_digest(
    mut digest: blake3::Hasher,
    aliases: &[CanonicalDuplicateAlias],
) -> String {
    for alias in aliases {
        digest.update(alias.owner_canonical_id.as_bytes());
        digest.update(b"\x1e");
        digest.update(alias.duplicate_canonical_id.as_bytes());
        digest.update(b"\x1eexact_source_identity\x1f");
    }
    digest.finalize().to_hex().to_string()
}

/// Bind projection/cache identity to every authoritative source field that can
/// affect ranking, grouping, routing, or the public payload. The older
/// reconciliation digest intentionally covered materialized origin/payload
/// identity only; using it alone could reuse a cached projection after a
/// classifier/semantic input changed at the same source revision.
fn source_bound_universe_digest(
    reconciliation_digest: &str,
    source_generation_token: &str,
) -> String {
    let mut digest = blake3::Hasher::new();
    digest.update(b"canonical-source-bound-universe-v2\0");
    for component in [reconciliation_digest, source_generation_token] {
        digest.update(&(component.len() as u64).to_le_bytes());
        digest.update(component.as_bytes());
    }
    digest.finalize().to_hex().to_string()
}

async fn canonical_source_generation_token(
    mail_store: &MailAssistStore,
    resurfacing_store: &ResurfacingStore,
    principal: &str,
    workspace: &str,
    as_of_ms: i64,
) -> Result<String> {
    let (follow_up, worth_a_look) = tokio::try_join!(
        mail_store.needs_approval_source_generation_token(
            principal,
            workspace,
            "follow_up",
            as_of_ms,
        ),
        resurfacing_store.surfaced_source_generation_token(
            principal,
            workspace,
            as_of_ms,
            CANONICAL_UNION_WORTH_PAGE_LIMIT,
        ),
    )?;
    let mut digest = blake3::Hasher::new();
    digest.update(b"canonical-attention-source-generation-v1\0");
    for token in [follow_up, worth_a_look] {
        digest.update(&(token.len() as u64).to_le_bytes());
        digest.update(token.as_bytes());
    }
    Ok(format!(
        "canonical-source-v1:{}",
        digest.finalize().to_hex()
    ))
}

#[cfg(any(test, feature = "test-fixtures"))]
fn universe_digest(
    raw_sources: &[CanonicalSource],
    aliases: &[CanonicalDuplicateAlias],
) -> Result<String> {
    Ok(finish_universe_digest(
        universe_digest_source_state(raw_sources)?,
        aliases,
    ))
}

fn validate_reconciled_projection_contract(
    projection: &CanonicalAttentionProjection,
    principal: &str,
    workspace: &str,
) -> Result<()> {
    let health = &projection.cross_lane_reconciliation;
    let integrity = &projection.integrity;
    let materialized = projection
        .lanes
        .follow_up
        .iter()
        .chain(projection.lanes.worth_a_look.iter())
        .chain(projection.lanes.non_surfaced.iter())
        .collect::<Vec<_>>();
    let materialized_ids = materialized
        .iter()
        .map(|item| item.canonical_id.as_str())
        .collect::<HashSet<_>>();
    let materialized_by_id = materialized
        .iter()
        .map(|item| (item.canonical_id.as_str(), *item))
        .collect::<HashMap<_, _>>();
    let materialized_follow_up_origin_total = materialized
        .iter()
        .filter(|item| item.origin_lane == CanonicalAttentionLane::FollowUp)
        .count();
    let materialized_worth_a_look_origin_total = materialized
        .iter()
        .filter(|item| item.origin_lane == CanonicalAttentionLane::WorthALook)
        .count();
    let materialized_origins_are_consistent =
        materialized
            .iter()
            .all(|item| match (&item.origin_lane, &item.origin) {
                (CanonicalAttentionLane::FollowUp, CanonicalAttentionOrigin::FollowUp { .. }) => {
                    item.canonical_id
                        .strip_prefix("follow_up:")
                        .is_some_and(|suffix| !suffix.is_empty())
                },
                (
                    CanonicalAttentionLane::WorthALook,
                    CanonicalAttentionOrigin::WorthALook { .. },
                ) => item
                    .canonical_id
                    .strip_prefix("worth_a_look:")
                    .is_some_and(|suffix| !suffix.is_empty()),
                _ => false,
            });
    let alias_duplicate_ids = projection
        .duplicate_aliases
        .iter()
        .map(|alias| alias.duplicate_canonical_id.as_str())
        .collect::<HashSet<_>>();
    let aliases_are_sorted = projection.duplicate_aliases.windows(2).all(|window| {
        (
            &window[0].owner_canonical_id,
            &window[0].duplicate_canonical_id,
        ) <= (
            &window[1].owner_canonical_id,
            &window[1].duplicate_canonical_id,
        )
    });
    let group_references_are_materialized = materialized.iter().all(|item| {
        item.group.member_count == item.group.member_ids.len()
            && !item.group.member_ids.is_empty()
            && item.group.member_ids.iter().collect::<HashSet<_>>().len()
                == item.group.member_ids.len()
            && item
                .group
                .member_ids
                .iter()
                .any(|member_id| member_id == &item.canonical_id)
            && materialized_ids.contains(item.group.representative_id.as_str())
            && item
                .group
                .member_ids
                .iter()
                .all(|member_id| materialized_ids.contains(member_id.as_str()))
    });
    let aliases_are_absent_from_groups = materialized.iter().all(|item| {
        !alias_duplicate_ids.contains(item.group.representative_id.as_str())
            && item
                .group
                .member_ids
                .iter()
                .all(|member_id| !alias_duplicate_ids.contains(member_id.as_str()))
    });

    anyhow::ensure!(
        projection.schema_version == CANONICAL_ATTENTION_PROJECTION_SCHEMA_VERSION
            && integrity.load_complete
            && integrity.exact_once
            && integrity.unmatched_total == 0
            && health.schema_version == 1
            && health.status == CrossLaneReconciliationStatus::Succeeded
            && health.reason.is_none()
            && health.authoritative_lane == CanonicalAttentionLane::FollowUp
            && health.principal == principal
            && health.workspace == workspace
            && health.raw_source_total == Some(integrity.source_total)
            && health.follow_up_source_total == Some(integrity.follow_up_source_total)
            && health.worth_a_look_source_total == Some(integrity.worth_a_look_source_total)
            && health.unique_source_total == Some(integrity.materialized_total)
            && integrity.source_total
                == integrity.follow_up_source_total + integrity.worth_a_look_source_total
            && integrity.reconciled_total == integrity.source_total
            && integrity.materialized_total == integrity.grouped_member_total
            && integrity.materialized_total + integrity.duplicate_hidden_total
                == integrity.source_total
            && materialized_follow_up_origin_total == integrity.follow_up_source_total
            && materialized_worth_a_look_origin_total + integrity.duplicate_hidden_total
                == integrity.worth_a_look_source_total
            && integrity.follow_up_lane_total
                + integrity.worth_a_look_lane_total
                + integrity.non_surfaced_total
                == integrity.materialized_total
            && integrity.follow_up_lane_total == projection.lanes.follow_up.len()
            && integrity.worth_a_look_lane_total == projection.lanes.worth_a_look.len()
            && integrity.non_surfaced_total == projection.lanes.non_surfaced.len()
            && health.duplicate_hidden_total == integrity.duplicate_hidden_total
            && health.alias_record_total == health.duplicate_hidden_total
            && health.alias_records_returned == projection.duplicate_aliases.len()
            && health.alias_records_returned <= CANONICAL_DUPLICATE_ALIAS_LIMIT
            && health.aliases_truncated
                == (health.alias_records_returned < health.alias_record_total)
            && materialized.len() == integrity.materialized_total
            && materialized_ids.len() == materialized.len()
            && materialized_origins_are_consistent
            && group_references_are_materialized
            && aliases_are_absent_from_groups
            && alias_duplicate_ids.len() == projection.duplicate_aliases.len()
            && aliases_are_sorted
            && projection.duplicate_aliases.iter().all(|alias| {
                alias.reason == CanonicalDuplicateAliasReason::ExactSourceIdentity
                    && alias.owner_canonical_id != alias.duplicate_canonical_id
                    && alias
                        .owner_canonical_id
                        .strip_prefix("follow_up:")
                        .is_some_and(|suffix| !suffix.is_empty())
                    && alias
                        .duplicate_canonical_id
                        .strip_prefix("worth_a_look:")
                        .is_some_and(|suffix| !suffix.is_empty())
                    && materialized_by_id
                        .get(alias.owner_canonical_id.as_str())
                        .is_some_and(|owner| {
                            owner.origin_lane == CanonicalAttentionLane::FollowUp
                                && matches!(
                                    &owner.origin,
                                    CanonicalAttentionOrigin::FollowUp { .. }
                                )
                        })
                    && !materialized_ids.contains(alias.duplicate_canonical_id.as_str())
            })
            && health.reconciliation_digest.as_deref() == Some(projection.universe_digest.as_str()),
        "canonical cross-lane reconciliation projection contract mismatch"
    );
    Ok(())
}

fn projection_id(
    principal: &str,
    workspace: &str,
    universe_digest: &str,
    policy_identity: &str,
) -> String {
    blake3::hash(
        format!(
            "canonical-attention-v{}\x1f{principal}\x1f{workspace}\x1f{universe_digest}\x1f{policy_identity}",
            CANONICAL_ATTENTION_PROJECTION_SCHEMA_VERSION
        )
        .as_bytes(),
    )
    .to_hex()
    .to_string()
}

/// How many Worth-a-look rows a load bounded by [`CANONICAL_UNION_WORTH_PAGE_LIMIT`]
/// is entitled to receive for a lane holding `total` of them.
fn expected_worth_page_rows(total: u64, page_limit: usize) -> u64 {
    total.min(page_limit as u64)
}

/// Prove each source page carries the rows its own total promised it.
///
/// The Follow-up store honours any limit, so its load is still the whole lane
/// and `rows == total` is still the right question to ask of it. The
/// Worth-a-look store does not: it clamps, so the union asks for a bounded page
/// and the only honest question is whether the page it asked for arrived
/// intact. Asserting the old `rows == total` there is what could never pass
/// above the clamp — a permanent arithmetic failure, reported as a concurrent
/// write that had not happened.
///
/// The former "totals changed during canonical load" arm compared each page's
/// total against a probe query's total. Both totals now come from the same
/// page as the rows they describe, so the comparison would be against itself;
/// a source that moves mid-load is caught by the generation-token fence in
/// [`project_canonical_attention_union_strict`], which covers the entire load
/// rather than the interval between a probe and its page.
fn validate_loaded_union_pages(
    follow_up_rows: usize,
    follow_up_total: u64,
    worth_rows: usize,
    worth_total: u64,
    worth_page_limit: usize,
) -> Result<()> {
    anyhow::ensure!(
        follow_up_rows as u64 == follow_up_total,
        "Follow-up source universe changed during canonical load"
    );
    let expected_worth_rows = expected_worth_page_rows(worth_total, worth_page_limit);
    anyhow::ensure!(
        worth_rows as u64 == expected_worth_rows,
        "Worth-a-look source page returned {worth_rows} of the {expected_worth_rows} rows it requested"
    );
    Ok(())
}

/// Read the whole Follow-up lane in one query where it fits, and only pay for
/// a second when it does not.
///
/// The lane has no clamp — the union is contractually the WHOLE lane — so the
/// page size has to come from somewhere. It used to come from a `LIMIT 1`
/// probe, which meant every request paid two full scans no matter how small
/// the lane was. Asking for [`CANONICAL_UNION_FOLLOW_UP_PAGE_HINT`] instead
/// costs the same single scan as any other limit and answers both questions at
/// once: the rows, and the `total` that says whether those rows are all of
/// them. The re-read is the old two-query cost, now paid only by a lane larger
/// than the hint, and it is sized exactly rather than guessed again.
async fn load_complete_follow_up_universe(
    mail_store: &MailAssistStore,
    principal: &str,
    workspace: &str,
) -> Result<NeedsApprovalPage> {
    let page = mail_store
        .list_needs_approval_attention_lane_page(
            principal,
            workspace,
            "follow_up",
            CANONICAL_UNION_FOLLOW_UP_PAGE_HINT,
            0,
            None,
        )
        .await
        .context("loading complete Follow-up universe")?;
    if page.rows.len() as u64 >= page.total {
        return Ok(page);
    }
    mail_store
        .list_needs_approval_attention_lane_page(
            principal,
            workspace,
            "follow_up",
            usize::try_from(page.total).unwrap_or(usize::MAX),
            0,
            None,
        )
        .await
        .context("loading complete Follow-up universe")
}

fn atomic_baseline_fallback_projection(
    learning: &AttentionLearningService,
    principal: &str,
    workspace: &str,
    reason: &str,
) -> CanonicalAttentionProjection {
    let universe_digest = blake3::hash(
        format!("incomplete-canonical-union\x1f{principal}\x1f{workspace}\x1f{reason}").as_bytes(),
    )
    .to_hex()
    .to_string();
    let projection_id = blake3::hash(
        format!("atomic-baseline-v1\x1f{principal}\x1f{workspace}\x1f{universe_digest}").as_bytes(),
    )
    .to_hex()
    .to_string();
    CanonicalAttentionProjection {
        schema_version: CANONICAL_ATTENTION_PROJECTION_SCHEMA_VERSION,
        status: CanonicalProjectionStatus::BaselineFallback,
        projection_id,
        universe_digest,
        source_generation_token: None,
        created_at: chrono::Utc::now().timestamp_millis(),
        policy: CanonicalAttentionPolicy {
            mode: AttentionRoutingMode::Baseline,
            snapshot_id: None,
            model_version: None,
            seed_identity: learning.routing_seed_identity().to_string(),
            canary_fraction: 0.0,
        },
        integrity: CanonicalAttentionIntegrity {
            load_complete: false,
            exact_once: false,
            source_total: 0,
            follow_up_source_total: 0,
            worth_a_look_source_total: 0,
            reconciled_total: 0,
            grouped_member_total: 0,
            materialized_total: 0,
            follow_up_lane_total: 0,
            worth_a_look_lane_total: 0,
            non_surfaced_total: 0,
            duplicate_hidden_total: 0,
            unmatched_total: 0,
            fallback_reason: Some(reason.to_string()),
        },
        cross_lane_reconciliation: CanonicalCrossLaneReconciliationHealth {
            schema_version: 1,
            status: CrossLaneReconciliationStatus::Unavailable,
            reason: Some(match reason {
                "follow_up_load_unavailable" => {
                    CrossLaneReconciliationReason::FollowUpLoadUnavailable
                },
                "follow_up_load_stale" => CrossLaneReconciliationReason::FollowUpSourceChanged,
                "worth_a_look_load_incomplete" | "worth_a_look_load_unavailable" => {
                    CrossLaneReconciliationReason::WorthALookLoadUnavailable
                },
                "malformed_worth_comm_source_ref" => {
                    CrossLaneReconciliationReason::MalformedWorthCommSourceRef
                },
                _ => CrossLaneReconciliationReason::CanonicalProjectionUnavailable,
            }),
            authoritative_lane: CanonicalAttentionLane::FollowUp,
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            follow_up_source_total: None,
            worth_a_look_source_total: None,
            raw_source_total: None,
            unique_source_total: None,
            duplicate_hidden_total: 0,
            alias_record_total: 0,
            alias_records_returned: 0,
            aliases_truncated: false,
            reconciliation_digest: None,
        },
        duplicate_aliases: Vec::new(),
        diagnostics: None,
        lanes: CanonicalAttentionLanes::default(),
    }
}

/// Public fallback diagnostics are bounded contract values. The full chained
/// error remains in the warning log above the response boundary.
fn canonical_fallback_reason(error: &anyhow::Error) -> &'static str {
    if error
        .chain()
        .any(|source| source.downcast_ref::<AttentionWriterBusy>().is_some())
    {
        // The store's writer was held past the serving-path bound. The page
        // is served from the atomic baseline rather than queued behind
        // whatever holds the writer; the reason names it so a chronic
        // holder is visible in the reconciliation diagnostics.
        return "attention_writer_busy";
    }
    if error.chain().any(|source| {
        source
            .downcast_ref::<CanonicalLearningEvidenceChanged>()
            .is_some()
    }) {
        return "canonical_learning_evidence_changed";
    }
    if error.chain().any(|source| {
        source
            .downcast_ref::<CanonicalSourceGenerationChanged>()
            .is_some()
    }) {
        return "canonical_source_generation_changed";
    }
    let chain = error
        .chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ");
    if chain.contains("Follow-up source universe changed")
        || chain.contains("Follow-up source revision changed")
        || chain.contains("Follow-up probe row disappeared")
    {
        "follow_up_load_stale"
    } else if chain.contains("malformed Worth-a-look communication source reference") {
        "malformed_worth_comm_source_ref"
    } else if chain.contains("Worth-a-look source page returned")
        || chain.contains("Worth-a-look source revision changed")
        || chain.contains("Worth-a-look probe row disappeared")
    {
        "worth_a_look_load_incomplete"
    } else if chain.contains("source totals changed") {
        "canonical_source_totals_changed"
    } else if chain.contains("probing complete Follow-up universe")
        || chain.contains("loading complete Follow-up universe")
    {
        "follow_up_load_unavailable"
    } else if chain.contains("probing complete Worth-a-look universe")
        || chain.contains("loading complete Worth-a-look universe")
    {
        "worth_a_look_load_unavailable"
    } else if chain.contains("ranking canonical attention union")
        || chain.contains("grouping canonical attention union")
        || chain.contains("routing canonical attention union")
    {
        "canonical_learning_unavailable"
    } else if chain.contains("recording canonical attention decision")
        || chain.contains("persist")
        || chain.contains("committed canonical attention projection")
    {
        "canonical_projection_store_unavailable"
    } else {
        "canonical_projector_unavailable"
    }
}

/// Load, evaluate, persist, and materialize one authoritative union. A cached
/// projection is reused only after both current source pages have been loaded
/// intact and produce the same deterministic digest. The Follow-up page is the
/// whole lane; the Worth-a-look page is bounded by
/// [`CANONICAL_UNION_WORTH_PAGE_LIMIT`], so above that bound the union ranks and
/// groups the top of the lane rather than declining to rank the lane at all.
/// Any stale, short, or unavailable dependency returns a typed, empty
/// projection; callers retain their untouched legacy baseline payload
/// atomically.
async fn reusable_cached_projection(
    scope_cache: &CanonicalProjectionScopeCache,
    mail_store: &MailAssistStore,
    resurfacing_store: &ResurfacingStore,
    learning: &AttentionLearningService,
    principal: &str,
    workspace: &str,
) -> Option<Arc<CanonicalAttentionProjection>> {
    let cached = scope_cache.cached.lock().await.clone()?;
    if cached.projection.status != CanonicalProjectionStatus::Succeeded {
        return None;
    }
    // Serve-stale window: within the window the cached projection serves
    // as-is and the generation-token digest scan (a full window-function pass
    // over both lanes) is skipped entirely. Beyond it, the strict token check
    // runs and any drift recomputes.
    if cached.cached_at.elapsed() > CANONICAL_PROJECTION_STALE_SERVE_WINDOW {
        let as_of_ms = chrono::Utc::now().timestamp_millis();
        let source_generation_token = canonical_source_generation_token(
            mail_store,
            resurfacing_store,
            principal,
            workspace,
            as_of_ms,
        )
        .await
        .ok()?;
        if cached.projection.source_generation_token.as_deref()
            != Some(source_generation_token.as_str())
        {
            return None;
        }
    }
    // Policy identity is checked in both arms: a policy change must re-rank
    // immediately regardless of the staleness window.
    let policy_identity = projection_policy_identity(learning, principal, workspace)
        .await
        .ok()?;
    (cached.policy_identity == policy_identity).then_some(cached.projection)
}

async fn compute_canonical_attention_projection(
    scope_cache: &CanonicalProjectionScopeCache,
    mail_store: &MailAssistStore,
    resurfacing_store: &ResurfacingStore,
    learning: &AttentionLearningService,
    principal: &str,
    workspace: &str,
) -> Arc<CanonicalAttentionProjection> {
    if let Some(projection) = reusable_cached_projection(
        scope_cache,
        mail_store,
        resurfacing_store,
        learning,
        principal,
        workspace,
    )
    .await
    {
        return projection;
    }

    let (result, retried) = retry_once_on_projection_input_change(|| {
        project_canonical_attention_union_strict(
            mail_store,
            resurfacing_store,
            learning,
            principal,
            workspace,
        )
    })
    .await;
    if retried {
        tracing::debug!(
            principal,
            workspace,
            "canonical attention projection retried after an authoritative input changed"
        );
    }
    match result {
        Ok(projection) => {
            let projection = Arc::new(projection);
            // Fail open only for cache population: the projection itself was
            // already strictly fenced and is safe to return. If the policy
            // identity cannot be reread, the next request recomputes instead
            // of trusting an incompletely bound cache entry.
            if projection.status == CanonicalProjectionStatus::Succeeded {
                if let Ok(policy_identity) =
                    projection_policy_identity(learning, principal, workspace).await
                {
                    *scope_cache.cached.lock().await = Some(CachedCanonicalProjection {
                        projection: projection.clone(),
                        policy_identity,
                        cached_at: std::time::Instant::now(),
                    });
                }
            }
            // Housekeeping must never delay page delivery or retain the scope
            // projection gate. The store coalesces and batches it independently.
            learning.store().schedule_canonical_history_prune(
                principal,
                workspace,
                CANONICAL_PROJECTION_HISTORY_KEEP,
            );
            // Automatic retention floor for the tier tables the latest-N
            // prune does not own (expired deliveries, consumed outcomes,
            // embeddings/scores, impressions, recompute jobs): the operator
            // path stays authoritative, but a persisting scope now also
            // sweeps at most once per day instead of never.
            learning.store().schedule_scoped_retention_if_due(
                principal,
                workspace,
                learning.delivery_retention_days(),
            );
            projection
        },
        Err(error) => {
            tracing::warn!(
                principal,
                workspace,
                retried,
                error = %error,
                "canonical attention union failed closed to atomic legacy baseline"
            );
            // Failure projections are shared only by callers already waiting
            // on this exact flight. They never become a later cache hit.
            Arc::new(atomic_baseline_fallback_projection(
                learning,
                principal,
                workspace,
                canonical_fallback_reason(&error),
            ))
        },
    }
}

pub async fn project_canonical_attention_union(
    mail_store: &MailAssistStore,
    resurfacing_store: &ResurfacingStore,
    learning: &AttentionLearningService,
    principal: &str,
    workspace: &str,
    memory_api: Option<&crate::channel_assist::memory_api::MemoryApi>,
) -> Result<CanonicalAttentionProjection> {
    let learning_store = learning.store();
    let store_identity = learning_store.database_path().to_string_lossy();
    let scope_cache = canonical_projection_scope_cache(&store_identity, principal, workspace);
    let flight = {
        let mut active = scope_cache.active.lock().await;
        active
            .get_or_insert_with(|| Arc::new(AsyncOnceCell::new()))
            .clone()
    };
    let projection = flight
        .get_or_init(|| {
            compute_canonical_attention_projection(
                &scope_cache,
                mail_store,
                resurfacing_store,
                learning,
                principal,
                workspace,
            )
        })
        .await
        .clone();
    let mut active = scope_cache.active.lock().await;
    if active
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, &flight))
    {
        *active = None;
    }
    drop(active);
    {
        let mut registry = CANONICAL_PROJECTION_SCOPE_CACHES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        prune_canonical_projection_scope_caches(
            &mut registry,
            CANONICAL_PROJECTION_SCOPE_CACHE_LIMIT,
        );
    }
    let mut projection = projection.as_ref().clone();
    overlay_worth_memory_why_now(
        &mut projection,
        resurfacing_store,
        memory_api,
        principal,
        workspace,
    )
    .await;
    Ok(projection)
}

async fn project_canonical_attention_union_strict(
    mail_store: &MailAssistStore,
    resurfacing_store: &ResurfacingStore,
    learning: &AttentionLearningService,
    principal: &str,
    workspace: &str,
) -> Result<CanonicalAttentionProjection> {
    let started = Instant::now();
    let source_generation_at = chrono::Utc::now().timestamp_millis();
    let source_generation_before = canonical_source_generation_token(
        mail_store,
        resurfacing_store,
        principal,
        workspace,
        source_generation_at,
    )
    .await
    .context("reading canonical source generation before load")?;
    // Both loads are sized from their OWN returned `total`, not from a
    // `LIMIT 1` probe run first. The probes existed only to learn how big a
    // page to ask for, and each one cost a full scan of its source — on the
    // mail side a `mail_annotations` scan with the same window function and
    // joins as the real load, against a 191MB database, on a 20s poll. The
    // page already reports `total`, so the probe was asking a question the
    // answer to which was about to arrive anyway. Detecting a source that
    // moved mid-load is not lost with them: that is what the
    // `source_generation_before` / `source_generation_token` fence below
    // does, across the whole load rather than across one row of it.
    let follow_up = load_complete_follow_up_universe(mail_store, principal, workspace).await?;
    let worth = resurfacing_store
        .list_surfaced_page(
            principal,
            workspace,
            CANONICAL_UNION_WORTH_PAGE_LIMIT,
            0,
            None,
        )
        .await
        .context("loading complete Worth-a-look universe")?;
    validate_loaded_union_pages(
        follow_up.rows.len(),
        follow_up.total,
        worth.candidates.len(),
        worth.total,
        CANONICAL_UNION_WORTH_PAGE_LIMIT,
    )?;
    let source_generation_token = canonical_source_generation_token(
        mail_store,
        resurfacing_store,
        principal,
        workspace,
        source_generation_at,
    )
    .await
    .context("reading canonical source generation after load")?;
    if source_generation_before != source_generation_token {
        return Err(CanonicalSourceGenerationChanged.into());
    }

    let follow_up_total = follow_up.rows.len();
    let worth_total = worth.candidates.len();
    let raw_source_total = follow_up_total + worth_total;
    let mut raw_sources = Vec::with_capacity(raw_source_total);
    raw_sources.extend(
        follow_up
            .rows
            .iter()
            .map(|row| follow_up_source(row, source_generation_at)),
    );
    raw_sources.extend(
        worth
            .candidates
            .iter()
            .map(|candidate| worth_source(candidate, source_generation_at))
            .collect::<Result<Vec<_>>>()?,
    );
    let canonical_ids: HashSet<&str> = raw_sources
        .iter()
        .map(|source| source.semantic.candidate_id.as_str())
        .collect();
    anyhow::ensure!(
        canonical_ids.len() == raw_sources.len(),
        "origin-qualified canonical attention identity collision"
    );
    let universe_digest_state = universe_digest_source_state(&raw_sources)?;
    let (sources, duplicate_aliases) = reconcile_cross_lane_sources(raw_sources);
    let duplicate_hidden_total = duplicate_aliases.len();
    anyhow::ensure!(
        sources.len() + duplicate_hidden_total == raw_source_total,
        "canonical cross-lane reconciliation did not conserve raw sources"
    );

    let reconciliation_digest = finish_universe_digest(universe_digest_state, &duplicate_aliases);
    let universe_digest =
        source_bound_universe_digest(&reconciliation_digest, &source_generation_token);
    let policy_identity = projection_policy_identity(learning, principal, workspace).await?;
    if let Some(projection_json) = learning
        .canonical_projection_json(principal, workspace, &universe_digest, &policy_identity)
        .await?
    {
        let projection: CanonicalAttentionProjection = serde_json::from_str(&projection_json)
            .context("decoding persisted canonical attention projection")?;
        validate_reconciled_projection_contract(&projection, principal, workspace)?;
        anyhow::ensure!(
            projection.schema_version == CANONICAL_ATTENTION_PROJECTION_SCHEMA_VERSION
                && projection.universe_digest == universe_digest
                && projection.cross_lane_reconciliation.status
                    == CrossLaneReconciliationStatus::Succeeded
                && projection.integrity.source_total == raw_source_total
                && projection.integrity.duplicate_hidden_total == duplicate_hidden_total
                && projection.cross_lane_reconciliation.alias_record_total
                    == duplicate_hidden_total
                && projection.source_generation_token.as_deref()
                    == Some(source_generation_token.as_str()),
            "persisted canonical attention projection identity mismatch"
        );
        return Ok(projection);
    }

    let canonical_rank_candidates: Vec<_> = sources
        .iter()
        .map(|source| CanonicalAttentionRankCandidate {
            candidate: &source.semantic,
            evidence_surface: source.evidence_surface,
            evidence_candidate_id: &source.evidence_candidate_id,
        })
        .collect();
    let (rank_generation, ranks) = learning
        .rank_canonical_eligible_universe(principal, workspace, &canonical_rank_candidates)
        .await
        .context("ranking canonical attention union")?;
    let grouping = learning
        .group_canonical_eligible_universe(principal, workspace, &canonical_rank_candidates, &ranks)
        .await
        .context("grouping canonical attention union")?;
    let rank_by_id: HashMap<&str, _> = ranks
        .iter()
        .map(|rank| (rank.candidate_id.as_str(), rank))
        .collect();
    let memory_filter =
        memory_routing_filter(resurfacing_store, principal, workspace, &sources).await;
    let routing_inputs: Vec<_> = sources
        .iter()
        .filter_map(|source| {
            let filter = memory_filter.get(&source.semantic.candidate_id);
            Some(AttentionRoutingCandidate {
                candidate: &source.semantic,
                source_family: source.source_family.as_str(),
                baseline_route: source.baseline_route,
                rank: rank_by_id
                    .get(source.semantic.candidate_id.as_str())
                    .copied()?,
                grouping: grouping
                    .projection
                    .metadata
                    .get(&source.semantic.candidate_id)?,
                hard_eligible: filter.map(|value| value.hard_eligible).unwrap_or(true),
                ineligibility_reason: filter.and_then(|value| value.reason.as_deref()),
            })
        })
        .collect();
    anyhow::ensure!(
        routing_inputs.len() == sources.len(),
        "canonical attention rank/group reconciliation failed"
    );
    let projection_id = projection_id(principal, workspace, &universe_digest, &policy_identity);
    let mut routing = learning
        .evaluate_canonical_routing_universe(
            projection_id.clone(),
            principal,
            workspace,
            &routing_inputs,
        )
        .await
        .context("routing canonical attention union")?;
    let union_contract_incomplete = learning.routing_mode() != AttentionRoutingMode::Baseline
        && routing
            .items
            .iter()
            .any(|item| matches!(item.route_reason.as_str(), "contract_mismatch"));
    if union_contract_incomplete {
        routing.mode = AttentionRoutingMode::Baseline;
        routing.canary_assigned = false;
        routing.degradation_reason = Some("canonical_union_invalid_or_stale".to_string());
        for item in &mut routing.items {
            item.served_route = item.baseline_route;
            item.routing_mode = AttentionRoutingMode::Baseline;
            item.route_applied = false;
            item.canary_assigned = false;
            item.route_reason = "canonical_union_invalid_or_stale".to_string();
        }
    }
    // Slice-1 ordering is an independent, within-lane policy. It cannot alter
    // `served_route`; learned cross-lane movement (including NonSurfaced)
    // remains exclusively owned by the validated routing-canary decision.
    // Slice-5 personal ranking may later reorder the first page of each lane
    // without changing `served_route`.
    let slice1_order_applies = learning.semantic_ranking_enabled();
    let later_order_enabled = learning
        .serving_semantic_ranking_enabled_for(principal, workspace)
        .await?;
    let learned_order_applies = canonical_learned_order_applies(
        slice1_order_applies,
        routing.mode,
        routing.canary_assigned,
        later_order_enabled,
    );
    let mut served_ids: Vec<_> = ranks
        .iter()
        .map(|rank| {
            (
                if learned_order_applies {
                    rank.learned_rank
                } else {
                    rank.baseline_rank
                },
                rank.candidate_id.as_str(),
            )
        })
        .collect();
    served_ids.sort_by_key(|(rank, _)| *rank);
    routing.finalize_served_projection(
        served_ids.iter().map(|(_, id)| *id),
        served_ids.iter().map(|(_, id)| *id),
    );
    let decision_context = AttentionDecisionContext {
        queue_size: sources.len(),
        ..Default::default()
    };
    learning
        .apply_personal_bandit_ranking(principal, workspace, true, &decision_context, &mut routing)
        .await
        .context("applying personal bandit ranking to canonical attention")?;
    apply_memory_filter_to_routed_items(&mut routing.items, &memory_filter);
    if projection_policy_identity(learning, principal, workspace).await? != policy_identity {
        return Err(CanonicalLearningEvidenceChanged.into());
    }
    if canonical_source_generation_token(
        mail_store,
        resurfacing_store,
        principal,
        workspace,
        source_generation_at,
    )
    .await?
        != source_generation_token
    {
        return Err(CanonicalSourceGenerationChanged.into());
    }

    let decision_result = learning
        .record_routing_decision(
            principal,
            workspace,
            &universe_digest,
            decision_context,
            started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            sources.len(),
            &routing,
        )
        .await;
    let decision = match decision_result {
        Ok(decision) => decision,
        Err(error) => {
            let replay = learning
                .get_routing_decision(principal, workspace, &projection_id)
                .await
                .context("reading an idempotent canonical attention decision")?;
            let replay_matches = replay.as_ref().is_some_and(|detail| {
                detail.decision.candidate_set_digest == universe_digest
                    && detail.decision.complete_cross_lane_universe
                    && detail.items.len() == sources.len()
            });
            if !replay_matches {
                return Err(error).context("recording canonical attention decision");
            }
            replay
                .expect("validated canonical attention decision replay")
                .decision
        },
    };
    let routing_health = learning
        .routing_health(principal, workspace, &routing)
        .await
        .context("reading canonical attention routing health")?;

    let cluster_by_id: HashMap<&str, _> = grouping
        .projection
        .clusters
        .iter()
        .map(|cluster| (cluster.cluster_id.as_str(), cluster))
        .collect();
    let source_by_id: HashMap<&str, _> = sources
        .iter()
        .map(|source| (source.semantic.candidate_id.as_str(), source))
        .collect();
    let mut lanes = CanonicalAttentionLanes::default();
    let mut routing_items = routing.items.clone();
    routing_items.sort_by_key(|item| item.served_rank);
    for decision_item in routing_items {
        let source = source_by_id
            .get(decision_item.candidate_id.as_str())
            .copied()
            .context("canonical routing item has no source")?;
        let grouping_metadata: &AttentionGroupingMetadata = grouping
            .projection
            .metadata
            .get(&decision_item.candidate_id)
            .context("canonical routing item has no group metadata")?;
        let cluster = cluster_by_id
            .get(grouping_metadata.cluster_id.as_str())
            .copied()
            .context("canonical group metadata has no cluster")?;
        let served_lane = CanonicalAttentionLane::from(decision_item.served_route);
        let item = CanonicalAttentionItem {
            canonical_id: decision_item.candidate_id,
            source_revision: decision_item.source_revision,
            origin_lane: CanonicalAttentionLane::from(source.baseline_route),
            served_lane,
            learned_lane: CanonicalAttentionLane::from(decision_item.learned_route),
            route_reason: decision_item.route_reason,
            route_applied: decision_item.route_applied,
            origin: source.origin.clone(),
            group: CanonicalAttentionGroup {
                cluster_id: cluster.cluster_id.clone(),
                representative_id: cluster.representative_id.clone(),
                member_ids: cluster.member_ids.clone(),
                member_count: cluster.member_ids.len(),
            },
            actions: source.actions.clone(),
            payload: source.payload.clone(),
        };
        match served_lane {
            CanonicalAttentionLane::FollowUp => lanes.follow_up.push(item),
            CanonicalAttentionLane::WorthALook => lanes.worth_a_look.push(item),
            CanonicalAttentionLane::NonSurfaced => lanes.non_surfaced.push(item),
        }
    }
    let materialized_total =
        lanes.follow_up.len() + lanes.worth_a_look.len() + lanes.non_surfaced.len();
    let grouped_member_total: usize = grouping
        .projection
        .clusters
        .iter()
        .map(|cluster| cluster.member_ids.len())
        .sum();
    anyhow::ensure!(
        materialized_total == sources.len() && grouped_member_total == sources.len(),
        "canonical attention materialization did not reconcile"
    );
    let materialized_ids = lanes
        .follow_up
        .iter()
        .chain(lanes.worth_a_look.iter())
        .chain(lanes.non_surfaced.iter())
        .map(|item| item.canonical_id.as_str())
        .collect::<HashSet<_>>();
    anyhow::ensure!(
        materialized_ids.len() == materialized_total
            && duplicate_aliases.iter().all(|alias| {
                materialized_ids.contains(alias.owner_canonical_id.as_str())
                    && !materialized_ids.contains(alias.duplicate_canonical_id.as_str())
            })
            && materialized_total + duplicate_hidden_total == raw_source_total
            && grouped_member_total + duplicate_hidden_total == raw_source_total,
        "canonical cross-lane owner/alias exact-once invariant failed"
    );
    let public_duplicate_aliases = duplicate_aliases
        .iter()
        .take(CANONICAL_DUPLICATE_ALIAS_LIMIT)
        .cloned()
        .collect::<Vec<_>>();
    let follow_up_ids = sources
        .iter()
        .filter(|source| source.baseline_route == AttentionRoute::FollowUp)
        .map(|source| source.semantic.candidate_id.clone())
        .collect::<Vec<_>>();
    let worth_a_look_ids = sources
        .iter()
        .filter(|source| source.baseline_route == AttentionRoute::WorthALook)
        .map(|source| source.semantic.candidate_id.clone())
        .collect::<Vec<_>>();
    let follow_up_id_set = follow_up_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let worth_a_look_id_set = worth_a_look_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let follow_up_ranks = ranks
        .iter()
        .filter(|rank| follow_up_id_set.contains(rank.candidate_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let worth_a_look_ranks = ranks
        .iter()
        .filter(|rank| worth_a_look_id_set.contains(rank.candidate_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let source_family_counts = |route| {
        sources
            .iter()
            .filter(|source| source.baseline_route == route)
            .fold(std::collections::BTreeMap::new(), |mut counts, source| {
                *counts.entry(source.source_family.clone()).or_insert(0) += 1;
                counts
            })
    };
    let follow_up_health = learning
        .health(
            principal,
            workspace,
            AttentionSurface::FollowUp,
            follow_up_ids.len(),
            &follow_up_ids,
            source_family_counts(AttentionRoute::FollowUp),
            &follow_up_ranks,
        )
        .await
        .context("reading canonical Follow-up learning health")?;
    let worth_a_look_health = learning
        .health(
            principal,
            workspace,
            AttentionSurface::WorthALook,
            worth_a_look_ids.len(),
            &worth_a_look_ids,
            source_family_counts(AttentionRoute::WorthALook),
            &worth_a_look_ranks,
        )
        .await
        .context("reading canonical Worth-a-look learning health")?;
    let diagnostics = CanonicalAttentionDiagnostics {
        rank_generation,
        rank_scope: "canonical_eligible_universe".to_string(),
        follow_up_health,
        worth_a_look_health,
        grouping_mode: grouping.mode,
        grouping_snapshot_id: grouping.snapshot_id.clone(),
        grouping_generation: grouping.generation,
        grouping_scope: "canonical_eligible_universe".to_string(),
        grouping_health: grouping.health.clone(),
        decision,
        routing_health,
        impression_policy: Some(AttentionDeliveryImpressionPolicy {
            min_visible_ms: learning.routing_min_visible_ms(),
            visibility_rule_version: learning.routing_visibility_rule_version().to_string(),
        }),
        bandit_health: routing.bandit_health.clone(),
        ranks: ranks.clone(),
        decision_items: routing.items.clone(),
    };
    let projection = CanonicalAttentionProjection {
        schema_version: CANONICAL_ATTENTION_PROJECTION_SCHEMA_VERSION,
        status: if routing.degradation_reason.is_some() {
            CanonicalProjectionStatus::BaselineFallback
        } else {
            CanonicalProjectionStatus::Succeeded
        },
        projection_id: projection_id.clone(),
        universe_digest: universe_digest.clone(),
        source_generation_token: Some(source_generation_token),
        created_at: routing.decided_at,
        policy: CanonicalAttentionPolicy {
            mode: routing.mode,
            snapshot_id: routing.snapshot_id,
            model_version: routing.model_version,
            seed_identity: routing.policy_seed_identity,
            canary_fraction: learning.routing_canary_fraction(),
        },
        integrity: CanonicalAttentionIntegrity {
            load_complete: true,
            exact_once: true,
            source_total: raw_source_total,
            follow_up_source_total: follow_up_total,
            worth_a_look_source_total: worth_total,
            reconciled_total: raw_source_total,
            grouped_member_total,
            materialized_total,
            follow_up_lane_total: lanes.follow_up.len(),
            worth_a_look_lane_total: lanes.worth_a_look.len(),
            non_surfaced_total: lanes.non_surfaced.len(),
            duplicate_hidden_total,
            unmatched_total: 0,
            fallback_reason: routing.degradation_reason,
        },
        cross_lane_reconciliation: CanonicalCrossLaneReconciliationHealth {
            schema_version: 1,
            status: CrossLaneReconciliationStatus::Succeeded,
            reason: None,
            authoritative_lane: CanonicalAttentionLane::FollowUp,
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            follow_up_source_total: Some(follow_up_total),
            worth_a_look_source_total: Some(worth_total),
            raw_source_total: Some(raw_source_total),
            unique_source_total: Some(sources.len()),
            duplicate_hidden_total,
            alias_record_total: duplicate_aliases.len(),
            alias_records_returned: public_duplicate_aliases.len(),
            aliases_truncated: public_duplicate_aliases.len() < duplicate_aliases.len(),
            reconciliation_digest: Some(universe_digest.clone()),
        },
        duplicate_aliases: public_duplicate_aliases,
        diagnostics: Some(diagnostics),
        lanes,
    };
    validate_reconciled_projection_contract(&projection, principal, workspace)?;
    let projection_created_at = projection.created_at;
    let projection_json = serde_json::to_string(&projection)?;
    // The normalized store consumes the encoded allocation. Drop the complete
    // in-memory tree first so persistence never retains the tree plus two full
    // JSON string copies at the same time.
    drop(projection);
    let persisted_json = learning
        .persist_canonical_projection_json_owned(
            principal,
            workspace,
            &universe_digest,
            &policy_identity,
            &projection_id,
            projection_created_at,
            projection_json,
        )
        .await?;
    serde_json::from_str(&persisted_json)
        .context("decoding committed canonical attention projection")
}

struct MemoryRoutingFilter {
    hard_eligible: bool,
    reason: Option<String>,
    explore: bool,
    propensity: f64,
}

const SERENDIPITY_SLOT_COUNT: usize = 1;

async fn memory_routing_filter(
    store: &ResurfacingStore,
    principal: &str,
    workspace: &str,
    sources: &[CanonicalSource],
) -> HashMap<String, MemoryRoutingFilter> {
    let judgements = store
        .list_latest_memory_judgements(principal, workspace)
        .await
        .unwrap_or_default();
    if judgements.is_empty() {
        return HashMap::new();
    }
    let dismissed = store
        .list_dismissed_candidate_ids(principal, workspace)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect::<HashSet<_>>();
    let mode =
        magician::magician_v2::attention::resurfacing::memory_effects::effective_memory_effect_mode(
        );
    let mut out = HashMap::new();
    for source in sources {
        let Some(judgement) = judgements.get(&source.evidence_candidate_id) else {
            continue;
        };
        let (hard_eligible, reason) = judgement.hard_eligible(mode);
        let mut reason = reason.or_else(|| judgement.proposed_action.clone());
        for conflict in &judgement.conflicts {
            match reason.as_mut() {
                Some(existing) if !existing.contains(&conflict.rationale) => {
                    existing.push_str("; ");
                    existing.push_str(&conflict.rationale);
                },
                None => reason = Some(conflict.rationale.clone()),
                Some(_) => {},
            }
        }
        let exploration =
            magician::magician_v2::attention::resurfacing::memory_effects::assign_exploration(
                judgement,
                &source.evidence_candidate_id,
                &dismissed,
                mode,
            );
        let filter = MemoryRoutingFilter {
            hard_eligible,
            reason,
            explore: exploration.explore,
            propensity: exploration.propensity,
        };
        out.insert(source.semantic.candidate_id.clone(), filter);
    }
    out
}

fn apply_memory_filter_to_routed_items(
    items: &mut [magician::magician_v2::attention::learning::routing::AttentionDecisionItem],
    filters: &HashMap<String, MemoryRoutingFilter>,
) {
    for item in items.iter_mut() {
        let Some(filter) = filters.get(&item.candidate_id) else {
            continue;
        };
        item.hard_eligible = filter.hard_eligible;
        if item.ineligibility_reason.is_none() {
            item.ineligibility_reason = filter.reason.clone();
        }
        item.exploration = filter.explore;
        if filter.explore {
            item.selection_probability = filter.propensity;
        }
    }
    admit_serendipity_on_decision_items(items);
}

fn admit_serendipity_on_decision_items(
    items: &mut [magician::magician_v2::attention::learning::routing::AttentionDecisionItem],
) {
    let recently: HashSet<&str> = items
        .iter()
        .filter(|item| item.served_route != AttentionRoute::NonSurfaced)
        .map(|item| item.candidate_id.as_str())
        .collect();
    let mut ranked: Vec<(usize, f32)> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item.served_route == AttentionRoute::NonSurfaced
                && admit_worth_serendipity_slot(
                    item,
                    !recently.contains(item.candidate_id.as_str()),
                )
        })
        .map(|(index, item)| {
            (
                index,
                serendipity_intrinsic_salience(item).unwrap_or_default(),
            )
        })
        .collect();
    ranked.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (index, _) in ranked.into_iter().take(SERENDIPITY_SLOT_COUNT) {
        let item = &mut items[index];
        item.served_route = AttentionRoute::WorthALook;
        if !item.route_reason.contains("serendipity_slot") {
            item.route_reason.push_str("; serendipity_slot");
        }
    }
}

fn move_serendipity_into_worth_lane(projection: &mut CanonicalAttentionProjection) {
    let Some(diagnostics) = projection.diagnostics.as_ref() else {
        return;
    };
    let admitted: HashSet<&str> = diagnostics
        .decision_items
        .iter()
        .filter(|item| {
            item.served_route == AttentionRoute::WorthALook
                && item.route_reason.contains("serendipity_slot")
        })
        .map(|item| item.candidate_id.as_str())
        .collect();
    if admitted.is_empty() {
        return;
    }
    let mut remaining = Vec::new();
    for mut item in projection.lanes.non_surfaced.drain(..) {
        if admitted.contains(item.canonical_id.as_str()) {
            item.served_lane = CanonicalAttentionLane::WorthALook;
            if !item.route_reason.contains("serendipity_slot") {
                item.route_reason.push_str("; serendipity_slot");
            }
            projection.lanes.worth_a_look.push(item);
        } else {
            remaining.push(item);
        }
    }
    projection.lanes.non_surfaced = remaining;
}

async fn overlay_worth_memory_why_now(
    projection: &mut CanonicalAttentionProjection,
    resurfacing_store: &ResurfacingStore,
    memory_api: Option<&crate::channel_assist::memory_api::MemoryApi>,
    principal: &str,
    workspace: &str,
) {
    let memories = if let Some(api) = memory_api {
        api.shadow_memories(principal, workspace).await
    } else {
        Vec::new()
    };
    let mut ids = Vec::new();
    for item in projection
        .lanes
        .follow_up
        .iter()
        .chain(projection.lanes.worth_a_look.iter())
        .chain(projection.lanes.non_surfaced.iter())
    {
        if let CanonicalAttentionPayload::WorthALook { candidate_id, .. } = &item.payload {
            ids.push(candidate_id.clone());
        }
    }
    if ids.is_empty() {
        return;
    }
    let phrasing = resurfacing_store
        .get_phrasing_batch(principal, workspace, &ids)
        .await
        .unwrap_or_default();
    apply_worth_why_now_to_items(&mut projection.lanes.follow_up, &phrasing, &memories);
    apply_worth_why_now_to_items(&mut projection.lanes.worth_a_look, &phrasing, &memories);
    apply_worth_why_now_to_items(&mut projection.lanes.non_surfaced, &phrasing, &memories);
    if let Some(diagnostics) = projection.diagnostics.as_mut() {
        let dismissed = resurfacing_store
            .list_dismissed_candidate_ids(principal, workspace)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();
        let persisted = resurfacing_store
            .list_latest_memory_judgements(principal, workspace)
            .await
            .unwrap_or_default();
        apply_memory_effects_to_decision_items(
            &mut diagnostics.decision_items,
            projection
                .lanes
                .follow_up
                .iter()
                .chain(projection.lanes.worth_a_look.iter())
                .chain(projection.lanes.non_surfaced.iter()),
            &memories,
            &dismissed,
            &persisted,
        );
    }
    move_serendipity_into_worth_lane(projection);
}

fn recent_positive_engagement_on_item(
    item: &magician::magician_v2::attention::learning::routing::AttentionDecisionItem,
) -> bool {
    if item.selected {
        return true;
    }
    if item.selection_probability.is_finite() && item.selection_probability > 0.0 {
        return true;
    }
    // Calibrated lane utilities are Platt-sigmoided onto 0..=1. A strictly
    // positive value is already on the item; it is not a new store read.
    positive_unit_interval(item.follow_up_utility)
        || positive_unit_interval(item.worth_a_look_utility)
}

fn positive_unit_interval(value: Option<f64>) -> bool {
    value.is_some_and(|value| value.is_finite() && value > 0.0 && value <= 1.0)
}

/// Worth-a-look has no dedicated salience on the decision item. The calibrated
/// Worth utility is already 0..=1 and is the only honest stand-in; skip rather
/// than guess any other scale.
fn serendipity_intrinsic_salience(
    item: &magician::magician_v2::attention::learning::routing::AttentionDecisionItem,
) -> Option<f32> {
    let utility = item.worth_a_look_utility?;
    if utility.is_finite() && (0.0..=1.0).contains(&utility) {
        Some(utility as f32)
    } else {
        None
    }
}

/// Additional novelty admit for Worth-a-look. Never lifts items below the
/// intrinsic salience floor, and never re-picks the candidate just shown.
fn admit_worth_serendipity_slot(
    item: &magician::magician_v2::attention::learning::routing::AttentionDecisionItem,
    unlike_recent: bool,
) -> bool {
    let Some(salience) = serendipity_intrinsic_salience(item) else {
        return false;
    };
    magician::magician_v2::attention::resurfacing::memory_effects::admit_serendipity_slot(
        salience,
        unlike_recent,
    )
}

fn surface_memory_judgement_on_item(
    item: &mut magician::magician_v2::attention::learning::routing::AttentionDecisionItem,
    judgement: &magician::magician_v2::attention::resurfacing::memory_effects::MemoryJudgement,
) {
    let (eligible, reason) = judgement.hard_eligible(
        magician::magician_v2::attention::resurfacing::memory_effects::effective_memory_effect_mode(
        ),
    );
    item.hard_eligible = eligible;
    if item.ineligibility_reason.is_none() {
        item.ineligibility_reason = reason.or_else(|| judgement.proposed_action.clone());
    }
    for conflict in &judgement.conflicts {
        match item.ineligibility_reason.as_mut() {
            Some(existing) if !existing.contains(&conflict.rationale) => {
                existing.push_str("; ");
                existing.push_str(&conflict.rationale);
            },
            None => item.ineligibility_reason = Some(conflict.rationale.clone()),
            Some(_) => {},
        }
    }
}

fn apply_memory_effects_to_decision_items<'a>(
    items: &mut [magician::magician_v2::attention::learning::routing::AttentionDecisionItem],
    projection_items: impl Iterator<Item = &'a CanonicalAttentionItem>,
    memories: &[magician::magician_v2::attention::resurfacing::memory_context::ScopedMemory],
    dismissed: &std::collections::HashSet<String>,
    persisted: &std::collections::HashMap<
        String,
        magician::magician_v2::attention::resurfacing::memory_effects::MemoryJudgement,
    >,
) {
    use magician::magician_v2::attention::resurfacing::memory_context::candidate_attributes_from_text;
    use magician::magician_v2::attention::resurfacing::memory_effects::{
        assign_exploration, effective_memory_effect_mode, evaluate_memory_effects,
    };

    let mut text_by_id = std::collections::HashMap::<String, (String, String)>::new();
    let mut persisted_by_id = persisted.clone();
    for item in projection_items {
        match &item.payload {
            CanonicalAttentionPayload::WorthALook {
                candidate_id,
                line,
                source_title,
                source_kind,
                ..
            } => {
                let text = (format!("{source_title} {line}"), source_kind.clone());
                text_by_id.insert(item.canonical_id.clone(), text.clone());
                text_by_id.insert(candidate_id.clone(), text);
                if let Some(judgement) = persisted.get(candidate_id) {
                    persisted_by_id.insert(item.canonical_id.clone(), judgement.clone());
                }
            },
            CanonicalAttentionPayload::FollowUp {
                subject, summary, ..
            } => {
                let text = format!(
                    "{} {}",
                    subject.as_deref().unwrap_or(""),
                    summary.as_deref().unwrap_or("")
                );
                text_by_id.insert(item.canonical_id.clone(), (text, "comm".to_string()));
            },
        }
    }
    let now = chrono::Utc::now().timestamp();
    for item in items.iter_mut() {
        let (text, source_kind) = text_by_id
            .get(&item.candidate_id)
            .cloned()
            .unwrap_or_else(|| (item.candidate_id.clone(), item.source_family.clone()));
        let judgement = persisted_by_id
            .get(&item.candidate_id)
            .cloned()
            .unwrap_or_else(|| {
                evaluate_memory_effects(
                    &candidate_attributes_from_text(&text, &source_kind),
                    memories,
                    now,
                    recent_positive_engagement_on_item(item),
                )
            });
        let exploration = assign_exploration(
            &judgement,
            &item.candidate_id,
            dismissed,
            effective_memory_effect_mode(),
        );
        surface_memory_judgement_on_item(item, &judgement);
        item.exploration = exploration.explore;
        if exploration.explore {
            item.selection_probability = exploration.propensity;
        }
    }
    admit_serendipity_on_decision_items(items);
}

pub fn apply_worth_why_now_to_items(
    items: &mut [CanonicalAttentionItem],
    phrasing: &std::collections::HashMap<String, (String, String)>,
    memories: &[magician::magician_v2::attention::resurfacing::memory_context::ScopedMemory],
) {
    for item in items {
        let CanonicalAttentionPayload::WorthALook {
            candidate_id,
            line,
            why_now,
            source_title,
            source_kind,
            ..
        } = &mut item.payload
        else {
            continue;
        };
        let (curated, fallback) = match phrasing.get(candidate_id.as_str()) {
            Some((_phrased_line, why)) => (true, why.clone()),
            None => (false, why_now.clone()),
        };
        let (text, _, _) =
            magician::magician_v2::attention::resurfacing::memory_context::worth_why_now(
                source_title,
                line,
                source_kind,
                &fallback,
                curated,
                memories,
            );
        *why_now = text;
    }
}

pub async fn get_canonical_attention_projection_handler(
    mail_store: web::Data<MailAssistStore>,
    resurfacing_store: web::Data<ResurfacingStore>,
    learning: web::Data<AttentionLearningService>,
    memory_api: Option<web::Data<crate::channel_assist::memory_api::MemoryApi>>,
    request: HttpRequest,
    query: web::Query<CanonicalAttentionProjectionQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(request.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match project_canonical_attention_union(
        &mail_store,
        &resurfacing_store,
        &learning,
        &principal,
        &workspace,
        memory_api.as_ref().map(|data| data.get_ref()),
    )
    .await
    {
        Ok(projection) => HttpResponse::Ok().json(CanonicalAttentionProjectionResponse {
            canonical_attention_projection: projection,
        }),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": error.to_string(),
            "canonical_attention_projection": serde_json::Value::Null,
        })),
    }
}

fn delivery_lane_items(
    projection: &CanonicalAttentionProjection,
    lane: AttentionSurface,
) -> &[CanonicalAttentionItem] {
    match lane {
        AttentionSurface::FollowUp => &projection.lanes.follow_up,
        AttentionSurface::WorthALook => &projection.lanes.worth_a_look,
    }
}

fn delivery_refresh_response(
    lane: AttentionSurface,
    reason: AttentionDeliveryRefreshReason,
) -> HttpResponse {
    HttpResponse::Conflict().json(AttentionDeliveryRefreshRequired {
        schema_version: ATTENTION_DELIVERY_SCHEMA_VERSION,
        status: "refresh_required".to_string(),
        error: "attention_delivery_refresh_required".to_string(),
        reason,
        lane,
        refresh_href: format!(
            "/api/magician/v2/channel-assist/attention-learning/canonical-deliveries/{}",
            lane.as_str()
        ),
    })
}

fn materialize_delivery_response(
    delivery: FrozenAttentionDelivery,
) -> Result<AttentionDeliveryPageResponse> {
    let items = delivery
        .items
        .into_iter()
        .map(|item| {
            let canonical: CanonicalAttentionItem = serde_json::from_str(&item.item_json)
                .context("decoding frozen canonical delivery item")?;
            anyhow::ensure!(
                canonical.canonical_id == item.candidate_id
                    && canonical.source_revision == item.source_revision,
                "frozen canonical delivery identity/revision mismatch"
            );
            Ok(AttentionDeliveryResponseItem {
                position: item.position,
                candidate_id: item.candidate_id,
                source_revision: item.source_revision,
                root_policy_propensity: item.root_policy_propensity,
                conditional_delivery_propensity: item.conditional_delivery_propensity,
                exposure_token: item.exposure_token,
                item: canonical,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(AttentionDeliveryPageResponse {
        schema_version: ATTENTION_DELIVERY_SCHEMA_VERSION,
        status: delivery.status,
        fallback_reason: delivery.fallback_reason,
        root_decision: delivery.root_decision,
        page: delivery.page,
        items,
        health: delivery.health,
        impression_policy: AttentionDeliveryImpressionPolicy {
            min_visible_ms: delivery.min_visible_ms,
            visibility_rule_version: delivery.visibility_rule_version,
        },
    })
}

/// Frozen lane delivery. Page zero samples at most once over the complete
/// canonical lane; every later request reads one stored opaque cursor page.
pub async fn get_canonical_attention_delivery_handler(
    mail_store: web::Data<MailAssistStore>,
    resurfacing_store: web::Data<ResurfacingStore>,
    learning: web::Data<AttentionLearningService>,
    memory_api: Option<web::Data<crate::channel_assist::memory_api::MemoryApi>>,
    request: HttpRequest,
    query: web::Query<AttentionDeliveryQuery>,
    path: web::Path<String>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(request.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let lane = match path.as_str() {
        "follow_up" => AttentionSurface::FollowUp,
        "worth_a_look" => AttentionSurface::WorthALook,
        _ => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": "invalid_attention_delivery_lane",
                "message": "lane must be follow_up or worth_a_look",
            }));
        },
    };
    let now = chrono::Utc::now().timestamp_millis();
    if let Some(cursor) = query.cursor.as_deref() {
        let stored_source_generation = match learning
            .attention_delivery_cursor_source_generation_token(
                &principal, &workspace, lane, cursor, now,
            )
            .await
        {
            Ok(token) => token,
            Err(AttentionDeliveryReadError::RefreshRequired(reason)) => {
                return delivery_refresh_response(lane, reason);
            },
            Err(AttentionDeliveryReadError::Storage(error)) => {
                return HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": "attention_delivery_read_failed",
                    "message": error.to_string(),
                }));
            },
        };
        let (current_source_generation, legacy_projection) =
            if let Some(stored_token) = stored_source_generation.as_deref() {
                let token = match canonical_source_generation_token(
                    &mail_store,
                    &resurfacing_store,
                    &principal,
                    &workspace,
                    now,
                )
                .await
                {
                    Ok(token) => token,
                    Err(error) => {
                        return HttpResponse::InternalServerError().json(serde_json::json!({
                            "error": "attention_delivery_source_generation_failed",
                            "message": error.to_string(),
                        }));
                    },
                };
                if token != stored_token {
                    return delivery_refresh_response(
                        lane,
                        AttentionDeliveryRefreshReason::ProjectionDrift,
                    );
                }
                (Some(token), None)
            } else {
                // Lazy compatibility only: deliveries created before compact
                // source-generation bindings preserve the old full projection
                // and universe-digest drift proof until they expire.
                let projection = match project_canonical_attention_union(
                    &mail_store,
                    &resurfacing_store,
                    &learning,
                    &principal,
                    &workspace,
                    memory_api.as_ref().map(|data| data.get_ref()),
                )
                .await
                {
                    Ok(projection) => projection,
                    Err(error) => {
                        return HttpResponse::InternalServerError().json(serde_json::json!({
                            "error": "attention_delivery_projection_failed",
                            "message": error.to_string(),
                        }));
                    },
                };
                (None, Some(projection))
            };
        let stored = match learning
            .read_attention_delivery_page(
                &principal,
                &workspace,
                lane,
                cursor,
                current_source_generation.as_deref(),
                legacy_projection
                    .as_ref()
                    .map(|projection| projection.projection_id.as_str()),
                legacy_projection
                    .as_ref()
                    .map(|projection| projection.universe_digest.as_str()),
                now,
            )
            .await
        {
            Ok(delivery) => delivery,
            Err(AttentionDeliveryReadError::RefreshRequired(reason)) => {
                return delivery_refresh_response(lane, reason);
            },
            Err(AttentionDeliveryReadError::Storage(error)) => {
                return HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": "attention_delivery_read_failed",
                    "message": error.to_string(),
                }));
            },
        };
        if query
            .page_size
            .is_some_and(|requested| requested != stored.page.page_size)
        {
            return delivery_refresh_response(
                lane,
                AttentionDeliveryRefreshReason::BindingMismatch,
            );
        }
        return match materialize_delivery_response(stored) {
            Ok(response) => HttpResponse::Ok().json(response),
            Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "attention_delivery_materialization_failed",
                "message": error.to_string(),
            })),
        };
    }
    let projection = match project_canonical_attention_union(
        &mail_store,
        &resurfacing_store,
        &learning,
        &principal,
        &workspace,
        memory_api.as_ref().map(|data| data.get_ref()),
    )
    .await
    {
        Ok(projection) => projection,
        Err(error) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "attention_delivery_projection_failed",
                "message": error.to_string(),
            }));
        },
    };
    let canonical_items = delivery_lane_items(&projection, lane);
    let delivery = {
        let page_size = query
            .page_size
            .unwrap_or_else(|| learning.delivery_default_page_size());
        if page_size == 0 || page_size > learning.delivery_max_page_size() {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": "invalid_attention_delivery_page_size",
                "message": format!(
                    "page_size must be within 1..={}",
                    learning.delivery_max_page_size()
                ),
            }));
        }
        let normalized_projection = match learning
            .canonical_projection_lane_size(&projection.projection_id, lane.as_str())
            .await
        {
            Ok(Some(total)) if total == canonical_items.len() => true,
            Ok(Some(_)) => {
                return HttpResponse::Conflict().json(serde_json::json!({
                    "error": "attention_delivery_projection_drift",
                }));
            },
            Ok(None) => false,
            Err(error) => {
                return HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": "attention_delivery_projection_page_failed",
                    "message": error.to_string(),
                }));
            },
        };
        let plan = if projection.status == CanonicalProjectionStatus::BaselineFallback {
            magician::magician_v2::attention::learning::AttentionDeliveryOrderPlan {
                status: AttentionDeliveryStatus::BaselineFallback,
                fallback_reason: Some("canonical_projection_baseline_fallback".to_string()),
                decision_id: uuid::Uuid::new_v4().to_string(),
                policy_snapshot_id: None,
                policy_model_version: None,
                posterior_version: 0,
                seed_identity: "baseline".to_string(),
                ordered_items: canonical_items
                    .iter()
                    .map(|item| {
                        magician::magician_v2::attention::learning::AttentionDeliveryOrderItem {
                            candidate_id: item.canonical_id.clone(),
                            source_revision: item.source_revision.clone(),
                            root_policy_propensity: 1.0,
                            attribution_item: None,
                        }
                    })
                    .collect(),
                health: AttentionDeliveryHealth {
                    bandit_mode: AttentionBanditMode::Disabled,
                    canary_assigned: false,
                    applied: false,
                    baseline_preserved: true,
                    complete_universe_recorded: true,
                    propensity_coverage: if canonical_items.is_empty() { 0.0 } else { 1.0 },
                    degradation_reason: Some("canonical_projection_baseline_fallback".to_string()),
                    root_sample_count: 0,
                    delivered_count: 0,
                    remaining_count: canonical_items.len(),
                    exact_revision_match: true,
                    replay: false,
                },
                policy_snapshot_json: None,
                context: AttentionDecisionContext {
                    queue_size: canonical_items.len(),
                    ..Default::default()
                },
            }
        } else {
            let detail = match learning
                .get_routing_decision(&principal, &workspace, &projection.projection_id)
                .await
            {
                Ok(Some(detail)) => detail,
                Ok(None) => {
                    return HttpResponse::InternalServerError().json(serde_json::json!({
                        "error": "attention_delivery_root_decision_missing",
                    }));
                },
                Err(error) => {
                    return HttpResponse::InternalServerError().json(serde_json::json!({
                        "error": "attention_delivery_root_decision_read_failed",
                        "message": error.to_string(),
                    }));
                },
            };
            match learning
                .plan_attention_delivery_order(&principal, &workspace, lane, &detail)
                .await
            {
                Ok(plan) => plan,
                Err(error) => {
                    return HttpResponse::InternalServerError().json(serde_json::json!({
                        "error": "attention_delivery_policy_failed",
                        "message": error.to_string(),
                    }));
                },
            }
        };
        let item_by_id = canonical_items
            .iter()
            .map(|item| (item.canonical_id.as_str(), item))
            .collect::<HashMap<_, _>>();
        let ordered_items = match plan
            .ordered_items
            .iter()
            .map(|ordered| {
                let item = item_by_id
                    .get(ordered.candidate_id.as_str())
                    .copied()
                    .context("frozen delivery order item missing from canonical lane")?;
                anyhow::ensure!(
                    item.source_revision == ordered.source_revision,
                    "frozen delivery order revision mismatch"
                );
                Ok(AttentionDeliveryCandidate {
                    candidate_id: ordered.candidate_id.clone(),
                    source_revision: ordered.source_revision.clone(),
                    root_policy_propensity: ordered.root_policy_propensity,
                    item_json: if normalized_projection {
                        String::new()
                    } else {
                        serde_json::to_string(item)?
                    },
                    attribution_item_json: ordered
                        .attribution_item
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?,
                })
            })
            .collect::<Result<Vec<_>>>()
        {
            Ok(items) if items.len() == canonical_items.len() => items,
            Ok(_) => {
                return HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": "attention_delivery_order_incomplete",
                }));
            },
            Err(error) => {
                return HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": "attention_delivery_order_invalid",
                    "message": error.to_string(),
                }));
            },
        };
        let expires_at = now.saturating_add(
            i64::try_from(learning.delivery_ttl_secs())
                .unwrap_or(i64::MAX / 1_000)
                .saturating_mul(1_000),
        );
        let create = CreateAttentionDelivery {
            status: plan.status,
            fallback_reason: plan.fallback_reason,
            root_decision: AttentionDeliveryRootDecision {
                decision_id: plan.decision_id,
                lane,
                projection_id: projection.projection_id.clone(),
                universe_digest: projection.universe_digest.clone(),
                source_generation_token: projection.source_generation_token.clone(),
                policy_snapshot_id: plan.policy_snapshot_id,
                policy_model_version: plan.policy_model_version,
                posterior_version: plan.posterior_version,
                seed_identity: plan.seed_identity,
                universe_size: ordered_items.len(),
                created_at: now,
                expires_at,
            },
            page_size,
            ordered_items,
            health: plan.health,
            policy_snapshot_json: plan.policy_snapshot_json,
            min_visible_ms: learning.routing_min_visible_ms(),
            visibility_rule_version: learning.routing_visibility_rule_version().to_string(),
            context: plan.context,
        };
        match learning
            .create_attention_delivery(&principal, &workspace, &create)
            .await
        {
            Ok(delivery) => delivery,
            Err(error)
                if error
                    .chain()
                    .any(|source| source.downcast_ref::<AttentionWriterBusy>().is_some()) =>
            {
                // The writer stayed held past the serving-path bound. This is
                // a retry, not a failure of the delivery: the client backs
                // off instead of the request queueing behind maintenance.
                return HttpResponse::ServiceUnavailable()
                    .insert_header(("Retry-After", "2"))
                    .json(serde_json::json!({
                        "error": "attention_writer_busy",
                        "message": error.to_string(),
                    }));
            },
            Err(error) => {
                return HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": "attention_delivery_create_failed",
                    "message": error.to_string(),
                }));
            },
        }
    };
    match materialize_delivery_response(delivery) {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "attention_delivery_materialization_failed",
            "message": error.to_string(),
        })),
    }
}

pub async fn get_attention_delivery_health_handler(
    learning: web::Data<AttentionLearningService>,
    request: HttpRequest,
    query: web::Query<CanonicalAttentionProjectionQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(request.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match learning
        .attention_delivery_health(
            &principal,
            &workspace,
            chrono::Utc::now().timestamp_millis(),
        )
        .await
    {
        Ok(health) => HttpResponse::Ok().json(serde_json::json!({
            "attention_delivery_health": health,
        })),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "attention_delivery_health_failed",
            "message": error.to_string(),
        })),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use magician::magician_v2::attention::resurfacing::types::{CandidateState, SalienceSignals};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn identity(provider: &str, account_alias: &str, thread_id: &str) -> CrossSurfaceIdentity {
        CrossSurfaceIdentity {
            provider: provider.to_string(),
            account_alias: account_alias.to_string(),
            thread_id: thread_id.to_string(),
        }
    }

    fn source(
        canonical_id: &str,
        route: AttentionRoute,
        cross_surface_identity: CrossSurfaceIdentity,
        semantic_text: &str,
    ) -> CanonicalSource {
        let evidence_surface = route
            .surface()
            .expect("test source must belong to a visible origin lane");
        let origin = match route {
            AttentionRoute::FollowUp => CanonicalAttentionOrigin::FollowUp {
                annotation_id: canonical_id.to_string(),
                provider: cross_surface_identity.provider.clone(),
                account_alias: cross_surface_identity.account_alias.clone(),
                thread_id: cross_surface_identity.thread_id.clone(),
            },
            AttentionRoute::WorthALook => CanonicalAttentionOrigin::WorthALook {
                candidate_id: canonical_id.to_string(),
                source_kind: "comm".to_string(),
                source_ref: "opaque-test-ref".to_string(),
            },
            AttentionRoute::NonSurfaced => unreachable!("origin source cannot be non-surfaced"),
        };
        let payload = match route {
            AttentionRoute::FollowUp => CanonicalAttentionPayload::FollowUp {
                annotation_id: canonical_id.to_string(),
                subject: Some(semantic_text.to_string()),
                sender: None,
                summary: None,
                label: None,
                reason: None,
                received_at: None,
                due_text: None,
                due_at: None,
                open_url: None,
            },
            AttentionRoute::WorthALook => CanonicalAttentionPayload::WorthALook {
                candidate_id: canonical_id.to_string(),
                line: semantic_text.to_string(),
                why_now: String::new(),
                summary: semantic_text.to_string(),
                source_title: semantic_text.to_string(),
                source_kind: "comm".to_string(),
                source_ref: "opaque-test-ref".to_string(),
                open_url: None,
                temporal_anchor_at: None,
                brief: None,
            },
            AttentionRoute::NonSurfaced => unreachable!("origin source cannot be non-surfaced"),
        };
        CanonicalSource {
            semantic: SemanticAttentionCandidate {
                candidate_id: canonical_id.to_string(),
                source_revision: Some("revision-1".to_string()),
                semantic_text: semantic_text.to_string(),
                existing_embedding: None,
                actionability_features: None,
                grouping_features: None,
            },
            evidence_surface,
            evidence_candidate_id: canonical_id.to_string(),
            source_family: "comm".to_string(),
            baseline_route: route,
            origin,
            actions: Vec::new(),
            payload,
            cross_surface_identity: Some(cross_surface_identity),
        }
    }

    fn worth_candidate(source_kind: SourceKind, source_ref: &str) -> Candidate {
        Candidate {
            candidate_id: "candidate-1".to_string(),
            source_kind,
            source_ref: source_ref.to_string(),
            title: "title".to_string(),
            content_digest: "content-revision".to_string(),
            content_details: None,
            content_revision: Some("revision-1".to_string()),
            semantic_features: None,
            salience_score: 0.5,
            signals: SalienceSignals::default(),
            temporal_anchor_at: None,
            embedding_id: None,
            state: CandidateState::Surfaced,
            first_seen_at: 1,
            last_scored_at: 1,
            last_surfaced_at: Some(1),
            cooldown_until: 0,
            surface_count: 1,
            dismiss_count: 0,
        }
    }

    fn projection_item(
        canonical_id: &str,
        origin_lane: CanonicalAttentionLane,
    ) -> CanonicalAttentionItem {
        let (origin, payload) = match origin_lane {
            CanonicalAttentionLane::FollowUp => (
                CanonicalAttentionOrigin::FollowUp {
                    annotation_id: canonical_id.to_string(),
                    provider: "gmail".to_string(),
                    account_alias: "business".to_string(),
                    thread_id: canonical_id.to_string(),
                },
                CanonicalAttentionPayload::FollowUp {
                    annotation_id: canonical_id.to_string(),
                    subject: Some(canonical_id.to_string()),
                    sender: None,
                    summary: None,
                    label: None,
                    reason: None,
                    received_at: None,
                    due_text: None,
                    due_at: None,
                    open_url: None,
                },
            ),
            CanonicalAttentionLane::WorthALook => (
                CanonicalAttentionOrigin::WorthALook {
                    candidate_id: canonical_id.to_string(),
                    source_kind: "memory".to_string(),
                    source_ref: canonical_id.to_string(),
                },
                CanonicalAttentionPayload::WorthALook {
                    candidate_id: canonical_id.to_string(),
                    line: canonical_id.to_string(),
                    why_now: String::new(),
                    summary: canonical_id.to_string(),
                    source_title: canonical_id.to_string(),
                    source_kind: "memory".to_string(),
                    source_ref: canonical_id.to_string(),
                    open_url: None,
                    temporal_anchor_at: None,
                    brief: None,
                },
            ),
            CanonicalAttentionLane::NonSurfaced => {
                panic!("a canonical source cannot originate in non-surfaced")
            },
        };
        CanonicalAttentionItem {
            canonical_id: canonical_id.to_string(),
            source_revision: Some("revision-1".to_string()),
            origin_lane,
            served_lane: origin_lane,
            learned_lane: origin_lane,
            route_reason: "baseline".to_string(),
            route_applied: false,
            origin,
            group: CanonicalAttentionGroup {
                cluster_id: format!("cluster:{canonical_id}"),
                representative_id: canonical_id.to_string(),
                member_ids: vec![canonical_id.to_string()],
                member_count: 1,
            },
            actions: Vec::new(),
            payload,
        }
    }

    fn overlay_decision_item(candidate_id: &str) -> AttentionDecisionItem {
        AttentionDecisionItem {
            decision_id: "d1".to_string(),
            candidate_id: candidate_id.to_string(),
            source_revision: None,
            feature_values: None,
            source_family: "comm".to_string(),
            hard_eligible: true,
            ineligibility_reason: None,
            baseline_route: magician::magician_v2::attention::learning::routing::AttentionRoute::WorthALook,
            learned_route: magician::magician_v2::attention::learning::routing::AttentionRoute::WorthALook,
            served_route: magician::magician_v2::attention::learning::routing::AttentionRoute::WorthALook,
            routing_mode: magician::config::AttentionRoutingMode::Baseline,
            routing_snapshot_id: None,
            routing_model_version: None,
            learned_route_confidence: None,
            utility_margin: None,
            route_reason: "baseline_mode".to_string(),
            route_applied: false,
            canary_assigned: false,
            owner_action_required_probability: None,
            information_value_probability: None,
            follow_up_utility: None,
            worth_a_look_utility: None,
            uncertainty: None,
            cluster_id: "c".to_string(),
            cluster_size: 1,
            representative: true,
            baseline_rank: 1,
            learned_rank: 1,
            served_rank: 1,
            selected: false,
            selection_probability: 0.0,
            exploration: false,
            feature_snapshot_digest: None,
            bandit_decision: None,
            extraction_status: magician::magician_v2::attention::learning::SemanticExtractionStatus::Succeeded,
            feature_contracts: magician::magician_v2::attention::learning::routing::AttentionDecisionFeatureContracts {
                routing_feature_contract: "test".to_string(),
                routing_snapshot_id: None,
                actionability_snapshot_id: None,
                actionability_model_version: None,
                grouping_snapshot_id: None,
                grouping_model_version: None,
                semantic_schema_version: 1,
                semantic_extractor_contract: "test".to_string(),
                semantic_prompt_version: None,
                semantic_model: None,
                semantic_profile: None,
            },
        }
    }

    fn overlay_vendor_memories(
        may_propose_action: bool,
    ) -> Vec<magician::magician_v2::attention::resurfacing::memory_context::ScopedMemory> {
        use magician::magician_v2::agents::memory_scope::MemoryScope;
        use magician::magician_v2::attention::resurfacing::memory_context::ScopedMemory;

        vec![ScopedMemory {
            key: "preferences: vendor".to_string(),
            tier: "preferences".to_string(),
            source_type: "owner_confirmed".to_string(),
            trust: magician::magician_v2::agents::MemoryTrust::Stated,
            kind: magician::magician_v2::agents::MemoryKind::Normative,
            text: "Avoid vendor calls".to_string(),
            updated_at: Some(chrono::Utc::now().timestamp().to_string()),
            scope: MemoryScope {
                topics: vec!["vendor".to_string()],
                entities: Vec::new(),
                applies_to: Vec::new(),
            },
            may_explain: true,
            may_suppress: true,
            may_condition: true,
            may_propose_action,
        }]
    }

    fn overlay_vendor_projection_item() -> CanonicalAttentionItem {
        let mut items = vec![projection_item(
            "vendor-invoice",
            CanonicalAttentionLane::WorthALook,
        )];
        if let CanonicalAttentionPayload::WorthALook {
            candidate_id,
            line,
            source_title,
            ..
        } = &mut items[0].payload
        {
            *candidate_id = "c1".to_string();
            *line = "Invoice from vendor".to_string();
            *source_title = "Invoice from vendor".to_string();
        }
        items.pop().expect("vendor projection item")
    }

    #[test]
    fn overlay_replaces_uncurated_stub_and_keeps_curator_phrasing() {
        use magician::magician_v2::agents::memory_scope::MemoryScope;
        use magician::magician_v2::attention::resurfacing::memory_context::ScopedMemory;

        let memories = vec![ScopedMemory {
            key: "preferences: vendor".to_string(),
            tier: "preferences".to_string(),
            source_type: "owner_confirmed".to_string(),
            trust: magician::magician_v2::agents::MemoryTrust::Stated,
            kind: magician::magician_v2::agents::MemoryKind::Normative,
            text: "Avoid vendor calls".to_string(),
            updated_at: Some("rev-1".to_string()),
            scope: MemoryScope {
                topics: vec!["vendor".to_string()],
                entities: Vec::new(),
                applies_to: Vec::new(),
            },
            may_explain: true,
            may_suppress: true,
            may_condition: true,
            may_propose_action: false,
        }];
        let mut items = vec![projection_item(
            "vendor-invoice",
            CanonicalAttentionLane::WorthALook,
        )];
        if let CanonicalAttentionPayload::WorthALook {
            candidate_id,
            line,
            why_now,
            source_title,
            ..
        } = &mut items[0].payload
        {
            *candidate_id = "c1".to_string();
            *line = "Invoice from vendor".to_string();
            *source_title = "Invoice from vendor".to_string();
            *why_now = "Currently surfaced by the Worth-a-look curator".to_string();
        }

        apply_worth_why_now_to_items(&mut items, &std::collections::HashMap::new(), &memories);
        match &items[0].payload {
            CanonicalAttentionPayload::WorthALook { why_now, .. } => {
                assert!(
                    why_now.contains("you said"),
                    "uncurated stub should yield a memory citation: {why_now}"
                );
            },
            _ => panic!("expected worth payload"),
        }

        let mut phrasing = std::collections::HashMap::new();
        phrasing.insert(
            "c1".to_string(),
            (
                "Invoice from vendor".to_string(),
                "curator wrote this".to_string(),
            ),
        );
        if let CanonicalAttentionPayload::WorthALook { why_now, .. } = &mut items[0].payload {
            *why_now = "Currently surfaced by the Worth-a-look curator".to_string();
        }
        apply_worth_why_now_to_items(&mut items, &phrasing, &memories);
        match &items[0].payload {
            CanonicalAttentionPayload::WorthALook { why_now, .. } => {
                assert_eq!(why_now, "curator wrote this");
            },
            _ => panic!("expected worth payload"),
        }
    }

    #[test]
    fn overlay_records_shadow_suppression_without_dropping_the_card() {
        let memories = overlay_vendor_memories(false);
        let items = vec![overlay_vendor_projection_item()];
        let mut recorded = vec![overlay_decision_item("c1")];
        recorded[0].selected = true;
        recorded[0].selection_probability = 1.0;
        apply_memory_effects_to_decision_items(
            &mut recorded,
            items.iter(),
            &memories,
            &std::collections::HashSet::new(),
            &std::collections::HashMap::new(),
        );
        assert!(recorded[0].hard_eligible, "shadow must still serve");
        assert!(
            recorded[0]
                .ineligibility_reason
                .as_deref()
                .unwrap_or_default()
                .contains("preferences: vendor"),
            "{:?}",
            recorded[0].ineligibility_reason
        );
        assert!(
            recorded[0].exploration,
            "shadow must log the would-hide card as exploration"
        );
        assert_eq!(recorded[0].selection_probability, 1.0);
    }

    #[test]
    fn overlay_with_engagement_records_stated_vs_behaviour_conflict() {
        let memories = overlay_vendor_memories(false);
        let items = vec![overlay_vendor_projection_item()];
        let mut recorded = vec![overlay_decision_item("c1")];
        recorded[0].selected = true;
        recorded[0].selection_probability = 1.0;
        apply_memory_effects_to_decision_items(
            &mut recorded,
            items.iter(),
            &memories,
            &std::collections::HashSet::new(),
            &std::collections::HashMap::new(),
        );
        assert!(recorded[0].hard_eligible, "shadow must still serve");
        let reason = recorded[0]
            .ineligibility_reason
            .as_deref()
            .unwrap_or_default();
        assert!(
            reason.contains("recent engagement"),
            "stated-vs-behaviour conflict should be on the overlay: {reason}"
        );
    }

    #[test]
    fn serendipity_helper_rejects_items_below_the_salience_floor() {
        let mut below = overlay_decision_item("c-low");
        below.worth_a_look_utility = Some(0.01);
        assert!(
            !admit_worth_serendipity_slot(&below, true),
            "novelty must not rescue junk below the salience floor"
        );

        let mut missing = overlay_decision_item("c-missing");
        missing.worth_a_look_utility = None;
        assert!(
            !admit_worth_serendipity_slot(&missing, true),
            "skip rather than guess a salience that is not on the item"
        );

        let mut off_scale = overlay_decision_item("c-off-scale");
        off_scale.worth_a_look_utility = Some(1.5);
        assert!(
            !admit_worth_serendipity_slot(&off_scale, true),
            "utility outside 0..=1 is not an honest salience stand-in"
        );

        let mut unlike_recent_but_same = overlay_decision_item("c-recent");
        unlike_recent_but_same.worth_a_look_utility = Some(0.20);
        assert!(
            !admit_worth_serendipity_slot(&unlike_recent_but_same, false),
            "novelty may not re-pick the candidate just shown"
        );

        let mut above = overlay_decision_item("c-ok");
        above.worth_a_look_utility = Some(0.20);
        assert!(
            admit_worth_serendipity_slot(&above, true),
            "items already above the floor and unlike recent may take a novelty slot"
        );
    }

    #[test]
    fn serendipity_admits_one_nonsurfaced_item_onto_worth() {
        let mut keep = overlay_decision_item("c-keep");
        keep.served_route = AttentionRoute::NonSurfaced;
        keep.worth_a_look_utility = Some(0.40);
        let mut junk = overlay_decision_item("c-junk");
        junk.served_route = AttentionRoute::NonSurfaced;
        junk.worth_a_look_utility = Some(0.01);
        let mut second = overlay_decision_item("c-second");
        second.served_route = AttentionRoute::NonSurfaced;
        second.worth_a_look_utility = Some(0.20);
        let mut items = vec![keep, junk, second];
        admit_serendipity_on_decision_items(&mut items);
        assert_eq!(items[0].served_route, AttentionRoute::WorthALook);
        assert!(items[0].route_reason.contains("serendipity_slot"));
        assert_eq!(items[1].served_route, AttentionRoute::NonSurfaced);
        assert_eq!(
            items[2].served_route,
            AttentionRoute::NonSurfaced,
            "only one novelty slot"
        );
    }

    fn valid_reconciled_projection() -> CanonicalAttentionProjection {
        let universe_digest = "test-universe-digest".to_string();
        CanonicalAttentionProjection {
            schema_version: CANONICAL_ATTENTION_PROJECTION_SCHEMA_VERSION,
            status: CanonicalProjectionStatus::Succeeded,
            projection_id: "test-projection".to_string(),
            universe_digest: universe_digest.clone(),
            source_generation_token: Some("source-generation".to_string()),
            created_at: 1,
            policy: CanonicalAttentionPolicy {
                mode: AttentionRoutingMode::Baseline,
                snapshot_id: None,
                model_version: None,
                seed_identity: "test-seed".to_string(),
                canary_fraction: 0.0,
            },
            integrity: CanonicalAttentionIntegrity {
                load_complete: true,
                exact_once: true,
                source_total: 3,
                follow_up_source_total: 1,
                worth_a_look_source_total: 2,
                reconciled_total: 3,
                grouped_member_total: 2,
                materialized_total: 2,
                follow_up_lane_total: 1,
                worth_a_look_lane_total: 1,
                non_surfaced_total: 0,
                duplicate_hidden_total: 1,
                unmatched_total: 0,
                fallback_reason: None,
            },
            cross_lane_reconciliation: CanonicalCrossLaneReconciliationHealth {
                schema_version: 1,
                status: CrossLaneReconciliationStatus::Succeeded,
                reason: None,
                authoritative_lane: CanonicalAttentionLane::FollowUp,
                principal: "principal".to_string(),
                workspace: "workspace".to_string(),
                follow_up_source_total: Some(1),
                worth_a_look_source_total: Some(2),
                raw_source_total: Some(3),
                unique_source_total: Some(2),
                duplicate_hidden_total: 1,
                alias_record_total: 1,
                alias_records_returned: 1,
                aliases_truncated: false,
                reconciliation_digest: Some(universe_digest),
            },
            duplicate_aliases: vec![CanonicalDuplicateAlias {
                owner_canonical_id: "follow_up:owner".to_string(),
                duplicate_canonical_id: "worth_a_look:hidden".to_string(),
                reason: CanonicalDuplicateAliasReason::ExactSourceIdentity,
            }],
            diagnostics: None,
            lanes: CanonicalAttentionLanes {
                follow_up: vec![projection_item(
                    "follow_up:owner",
                    CanonicalAttentionLane::FollowUp,
                )],
                worth_a_look: vec![projection_item(
                    "worth_a_look:visible",
                    CanonicalAttentionLane::WorthALook,
                )],
                non_surfaced: Vec::new(),
            },
        }
    }

    #[test]
    fn exact_typed_identity_hides_only_the_worth_duplicate_before_learning() {
        let owner_identity = identity("gmail", "business", "thread-7");
        let other_identity = identity("gmail", "business", "thread-8");
        let raw = vec![
            source(
                "follow_up:fu-7",
                AttentionRoute::FollowUp,
                owner_identity.clone(),
                "identical visible text",
            ),
            source(
                "worth_a_look:wa-7",
                AttentionRoute::WorthALook,
                owner_identity,
                "identical visible text",
            ),
            source(
                "worth_a_look:wa-8",
                AttentionRoute::WorthALook,
                other_identity,
                "identical visible text",
            ),
        ];

        let (visible, aliases) = reconcile_cross_lane_sources(raw);

        assert_eq!(visible.len(), 2);
        assert_eq!(aliases.len(), 1);
        assert!(visible
            .iter()
            .any(|item| item.semantic.candidate_id == "follow_up:fu-7"));
        assert!(visible
            .iter()
            .any(|item| item.semantic.candidate_id == "worth_a_look:wa-8"));
        assert!(!visible
            .iter()
            .any(|item| item.semantic.candidate_id == "worth_a_look:wa-7"));
        assert_eq!(aliases[0].owner_canonical_id, "follow_up:fu-7");
        assert_eq!(aliases[0].duplicate_canonical_id, "worth_a_look:wa-7");
        assert_eq!(
            aliases[0].reason,
            CanonicalDuplicateAliasReason::ExactSourceIdentity
        );
    }

    #[test]
    fn typed_identity_digest_is_unambiguous_and_revision_or_alias_drift_changes_universe() {
        let left = identity("a:b", "c", "d");
        let right = identity("a", "b:c", "d");
        assert_ne!(left, right);
        assert_ne!(left.digest(), right.digest());

        let owner = source(
            "follow_up:fu",
            AttentionRoute::FollowUp,
            left.clone(),
            "same text",
        );
        let duplicate = source(
            "worth_a_look:wa",
            AttentionRoute::WorthALook,
            left,
            "same text",
        );
        let raw = vec![owner, duplicate];
        let (_, aliases) = reconcile_cross_lane_sources(raw.clone());
        let base_digest = universe_digest(&raw, &aliases).expect("base digest");

        let mut revision_changed = raw.clone();
        revision_changed[1].semantic.source_revision = Some("revision-2".to_string());
        let revision_digest =
            universe_digest(&revision_changed, &aliases).expect("revision digest");
        let without_alias_digest = universe_digest(&raw, &[]).expect("digest without alias");

        assert_ne!(base_digest, revision_digest);
        assert_ne!(base_digest, without_alias_digest);
    }

    #[test]
    fn source_generation_participates_in_projection_cache_identity() {
        let reconciliation = "stable-materialized-universe";
        let first = source_bound_universe_digest(reconciliation, "source-generation-a");
        assert_eq!(
            first,
            source_bound_universe_digest(reconciliation, "source-generation-a")
        );
        assert_ne!(
            first,
            source_bound_universe_digest(reconciliation, "source-generation-b"),
            "semantic/classifier source drift must not reuse a cached projection"
        );
    }

    #[test]
    fn delivery_cursor_branch_finishes_before_first_page_projection_materialization() {
        let source = include_str!("canonical_attention.rs");
        let handler = source
            .split_once("pub async fn get_canonical_attention_delivery_handler(")
            .expect("delivery handler source")
            .1;
        let token_probe = handler
            .find("attention_delivery_cursor_source_generation_token(")
            .expect("cursor token probe");
        let cursor_return = handler
            .find("return match materialize_delivery_response(stored)")
            .expect("cursor early return");
        let first_page_projection = handler
            .find("\n    let projection = match project_canonical_attention_union(")
            .expect("first-page projection materialization");
        assert!(token_probe < cursor_return);
        assert!(cursor_return < first_page_projection);
    }

    #[test]
    fn communication_identity_parsing_is_strict_while_non_comm_sources_have_no_cross_lane_key() {
        let valid = worth_candidate(
            SourceKind::Comm,
            "gmail/business/thread%2Fseven/message%40one@1785200000000",
        );
        assert_eq!(
            worth_cross_surface_identity(&valid).expect("valid communication ref"),
            Some(identity("gmail", "business", "thread/seven"))
        );

        let malformed = worth_candidate(SourceKind::Comm, "gmail/business/thread-seven");
        let error = worth_cross_surface_identity(&malformed)
            .expect_err("malformed communication ref must fail closed");
        assert!(error
            .to_string()
            .contains("malformed Worth-a-look communication source reference"));

        let non_comm = worth_candidate(SourceKind::Memory, "any opaque source ref");
        assert_eq!(
            worth_cross_surface_identity(&non_comm).expect("non-comm source"),
            None
        );
    }

    #[test]
    fn cached_projection_rejects_aliases_with_wrong_origin_or_namespace() {
        let valid = valid_reconciled_projection();
        validate_reconciled_projection_contract(&valid, "principal", "workspace")
            .expect("control projection must satisfy the reconciliation contract");

        let mut worth_owner = valid.clone();
        worth_owner.duplicate_aliases[0].owner_canonical_id = "worth_a_look:visible".to_string();
        assert!(
            validate_reconciled_projection_contract(&worth_owner, "principal", "workspace")
                .is_err()
        );

        let mut invalid_owner_namespace = valid.clone();
        invalid_owner_namespace.lanes.follow_up[0].canonical_id = "owner".to_string();
        invalid_owner_namespace.lanes.follow_up[0]
            .group
            .representative_id = "owner".to_string();
        invalid_owner_namespace.lanes.follow_up[0].group.member_ids = vec!["owner".to_string()];
        invalid_owner_namespace.duplicate_aliases[0].owner_canonical_id = "owner".to_string();
        assert!(validate_reconciled_projection_contract(
            &invalid_owner_namespace,
            "principal",
            "workspace"
        )
        .is_err());

        let mut invalid_duplicate_namespace = valid;
        invalid_duplicate_namespace.duplicate_aliases[0].duplicate_canonical_id =
            "hidden".to_string();
        assert!(validate_reconciled_projection_contract(
            &invalid_duplicate_namespace,
            "principal",
            "workspace"
        )
        .is_err());
    }

    #[test]
    fn cached_projection_rejects_hidden_aliases_in_any_group_reference() {
        let valid = valid_reconciled_projection();

        let mut hidden_member = valid.clone();
        hidden_member.lanes.follow_up[0]
            .group
            .member_ids
            .push("worth_a_look:hidden".to_string());
        hidden_member.lanes.follow_up[0].group.member_count = 2;
        assert!(
            validate_reconciled_projection_contract(&hidden_member, "principal", "workspace")
                .is_err()
        );

        let mut hidden_representative = valid;
        hidden_representative.lanes.follow_up[0]
            .group
            .representative_id = "worth_a_look:hidden".to_string();
        assert!(validate_reconciled_projection_contract(
            &hidden_representative,
            "principal",
            "workspace"
        )
        .is_err());
    }

    #[test]
    fn cached_projection_rejects_raw_origin_accounting_drift() {
        let mut projection = valid_reconciled_projection();
        projection.integrity.follow_up_source_total = 0;
        projection.integrity.worth_a_look_source_total = 3;
        projection.cross_lane_reconciliation.follow_up_source_total = Some(0);
        projection
            .cross_lane_reconciliation
            .worth_a_look_source_total = Some(3);

        assert!(
            validate_reconciled_projection_contract(&projection, "principal", "workspace").is_err()
        );
    }

    #[test]
    fn slice1_order_is_independent_while_later_heads_remain_canary_gated() {
        for mode in [
            AttentionRoutingMode::Baseline,
            AttentionRoutingMode::Shadow,
            AttentionRoutingMode::Canary,
        ] {
            assert!(canonical_learned_order_applies(true, mode, false, false));
        }
        assert!(!canonical_learned_order_applies(
            false,
            AttentionRoutingMode::Baseline,
            false,
            true,
        ));
        assert!(!canonical_learned_order_applies(
            false,
            AttentionRoutingMode::Canary,
            false,
            true,
        ));
        assert!(canonical_learned_order_applies(
            false,
            AttentionRoutingMode::Canary,
            true,
            true,
        ));
    }

    #[test]
    fn canonical_slice1_health_reports_only_successfully_served_learned_order() {
        assert!(canonical_slice1_order_is_active(
            CanonicalProjectionStatus::Succeeded,
            true,
            true,
        ));
        assert!(!canonical_slice1_order_is_active(
            CanonicalProjectionStatus::Succeeded,
            true,
            false,
        ));
        assert!(!canonical_slice1_order_is_active(
            CanonicalProjectionStatus::Succeeded,
            false,
            true,
        ));
        assert!(!canonical_slice1_order_is_active(
            CanonicalProjectionStatus::BaselineFallback,
            false,
            true,
        ));
    }

    #[tokio::test]
    async fn projection_input_change_is_retried_exactly_once_before_success() {
        let attempts = AtomicUsize::new(0);
        let (result, retried) = retry_once_on_projection_input_change(|| {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                if attempt == 0 {
                    Err(CanonicalLearningEvidenceChanged.into())
                } else {
                    Ok("stable")
                }
            }
        })
        .await;

        assert!(retried);
        assert_eq!(result.unwrap(), "stable");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn projection_input_change_retry_is_bounded_and_other_errors_are_not_retried() {
        let transient_attempts = AtomicUsize::new(0);
        let (transient_result, retried) = retry_once_on_projection_input_change(|| {
            transient_attempts.fetch_add(1, Ordering::SeqCst);
            async { Err::<(), _>(CanonicalLearningEvidenceChanged.into()) }
        })
        .await;
        assert!(retried);
        assert!(is_retryable_canonical_projection_input_change(
            &transient_result.unwrap_err()
        ));
        assert_eq!(transient_attempts.load(Ordering::SeqCst), 2);

        let source_attempts = AtomicUsize::new(0);
        let (source_result, retried) = retry_once_on_projection_input_change(|| {
            source_attempts.fetch_add(1, Ordering::SeqCst);
            async { Err::<(), _>(CanonicalSourceGenerationChanged.into()) }
        })
        .await;
        assert!(retried);
        assert!(is_retryable_canonical_projection_input_change(
            &source_result.unwrap_err()
        ));
        assert_eq!(source_attempts.load(Ordering::SeqCst), 2);

        let permanent_attempts = AtomicUsize::new(0);
        let (permanent_result, retried) = retry_once_on_projection_input_change(|| {
            permanent_attempts.fetch_add(1, Ordering::SeqCst);
            async { Err::<(), _>(anyhow::anyhow!("source load unavailable")) }
        })
        .await;
        assert!(!retried);
        assert_eq!(
            permanent_result.unwrap_err().to_string(),
            "source load unavailable"
        );
        assert_eq!(permanent_attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn canonical_projection_cache_is_scoped_persistent_and_coalesces_callers() {
        let first =
            canonical_projection_scope_cache("singleflight-store", "singleflight-owner", "default");
        let second =
            canonical_projection_scope_cache("singleflight-store", "singleflight-owner", "default");
        let other =
            canonical_projection_scope_cache("singleflight-store", "other-owner", "default");
        let other_store =
            canonical_projection_scope_cache("different-store", "singleflight-owner", "default");
        assert!(Arc::ptr_eq(&first, &second));
        assert!(!Arc::ptr_eq(&first, &other));
        assert!(
            !Arc::ptr_eq(&first, &other_store),
            "identical scope names in separate stores must never share a projection"
        );

        let flight = Arc::new(AsyncOnceCell::new());
        *first.active.lock().await = Some(flight.clone());
        let calls = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let owner_calls = calls.clone();
        let owner_flight = flight.clone();
        let expected = Arc::new(valid_reconciled_projection());
        let expected_id = expected.projection_id.clone();
        let owner = tokio::spawn(async move {
            Arc::clone(
                owner_flight
                    .get_or_init(|| async move {
                        owner_calls.fetch_add(1, Ordering::SeqCst);
                        started_tx.send(()).unwrap();
                        release_rx.await.unwrap();
                        expected
                    })
                    .await,
            )
        });
        started_rx.await.unwrap();
        let waiter_calls = calls.clone();
        let waiter_flight = second.active.lock().await.clone().unwrap();
        let waiter = tokio::spawn(async move {
            Arc::clone(
                waiter_flight
                    .get_or_init(|| async move {
                        waiter_calls.fetch_add(1, Ordering::SeqCst);
                        panic!("a waiter must reuse the owner's projection")
                    })
                    .await,
            )
        });
        release_tx.send(()).unwrap();
        let owner_projection = owner.await.unwrap();
        let waiter_projection = waiter.await.unwrap();
        assert_eq!(owner_projection.projection_id, expected_id);
        assert_eq!(waiter_projection.projection_id, expected_id);
        assert!(
            Arc::ptr_eq(&owner_projection, &waiter_projection),
            "coalesced callers must share the immutable projection allocation"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let retained = Arc::downgrade(&first);
        drop(first);
        drop(second);
        assert!(
            retained.upgrade().is_some(),
            "the bounded registry owns completed caches"
        );
        let persisted =
            canonical_projection_scope_cache("singleflight-store", "singleflight-owner", "default");
        assert!(Arc::ptr_eq(
            &persisted,
            &canonical_projection_scope_cache(
                "singleflight-store",
                "singleflight-owner",
                "default",
            )
        ));

        invalidate_canonical_attention_projection_cache("singleflight-owner", "default");
        let after_invalidation =
            canonical_projection_scope_cache("singleflight-store", "singleflight-owner", "default");
        assert!(
            !Arc::ptr_eq(&persisted, &after_invalidation),
            "maintenance invalidation must isolate later callers from the old flight"
        );
        let other_store_after_invalidation =
            canonical_projection_scope_cache("different-store", "singleflight-owner", "default");
        assert!(
            !Arc::ptr_eq(&other_store, &other_store_after_invalidation),
            "scope invalidation must clear every store instance carrying that owner scope"
        );
    }

    #[test]
    fn a_clamped_worth_page_is_a_short_page_only_when_it_is_actually_short() {
        // 2,000 surfaced candidates against a store that will never return
        // more than 1,000 in one page — the retention sweep's per-scope cap is
        // twice the clamp, so this is reachable. The old assert compared the
        // 1,000 rows the store was willing to give against the 2,000-row
        // COUNT(*) and therefore could not pass for ANY request, which is what
        // silently reverted the band to the unranked atomic baseline.
        assert!(validate_loaded_union_pages(3, 3, 1_000, 2_000, 1_000).is_ok());

        // A page that came back short of what it asked for is still a failure,
        // and still routes to the same bounded public reason.
        let error = validate_loaded_union_pages(3, 3, 999, 2_000, 1_000)
            .expect_err("a short page must not be accepted");
        assert!(
            error
                .to_string()
                .contains("Worth-a-look source page returned"),
            "the message must describe the page, not a concurrent write: {error}"
        );
        assert_eq!(
            canonical_fallback_reason(&error),
            "worth_a_look_load_incomplete"
        );

        // Under the clamp the page IS the universe and nothing about the
        // question changes.
        assert!(validate_loaded_union_pages(3, 3, 12, 12, 1_000).is_ok());
        assert!(validate_loaded_union_pages(3, 3, 11, 12, 1_000).is_err());

        // A Follow-up page short of its own total is still the "universe
        // changed" failure — the check that survived the probe's removal.
        let error = validate_loaded_union_pages(2, 3, 12, 12, 1_000)
            .expect_err("a short Follow-up page must not be accepted");
        assert!(
            error
                .to_string()
                .contains("Follow-up source universe changed"),
            "unexpected message: {error}"
        );
    }

    /// The union asks the Follow-up store for a page sized by
    /// [`CANONICAL_UNION_FOLLOW_UP_PAGE_HINT`], so any lane at or under the
    /// hint is complete in one read and the second read never happens. The
    /// hint must therefore sit far above the Worth-a-look clamp, which is the
    /// only bound either lane carries in practice.
    #[test]
    fn the_follow_up_page_hint_covers_a_realistic_lane_in_one_read() {
        assert!(CANONICAL_UNION_FOLLOW_UP_PAGE_HINT > CANONICAL_UNION_WORTH_PAGE_LIMIT);
        // A lane at the hint is served whole by the first read.
        let total = CANONICAL_UNION_FOLLOW_UP_PAGE_HINT as u64;
        assert!(validate_loaded_union_pages(
            CANONICAL_UNION_FOLLOW_UP_PAGE_HINT,
            total,
            0,
            0,
            CANONICAL_UNION_WORTH_PAGE_LIMIT
        )
        .is_ok());
    }

    #[test]
    fn exhausted_projection_input_retry_has_a_bounded_public_fallback_reason() {
        let error = anyhow::Error::new(CanonicalLearningEvidenceChanged);
        assert_eq!(
            canonical_fallback_reason(&error),
            "canonical_learning_evidence_changed"
        );
        let source_error = anyhow::Error::new(CanonicalSourceGenerationChanged);
        assert_eq!(
            canonical_fallback_reason(&source_error),
            "canonical_source_generation_changed"
        );
    }
}

//! Slice-4 calibrated lane utilities, deterministic canary assignment, and
//! replayable decision/impression contracts.
//!
//! Routing is deliberately fail-closed. A candidate receives a learned route
//! only when its revision-bound Slice-2 actionability score and semantic
//! envelope match the immutable routing snapshot exactly. Missing or stale
//! inputs preserve the baseline route and remain visible in the decision
//! ledger.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{
    bandit::{AttentionBanditDecisionMetadata, AttentionBanditHealth},
    ActionabilityScoreStatus, AttentionGroupingMetadata, AttentionRankMetadata, AttentionSurface,
    SemanticAttentionCandidate, SemanticExtractionStatus, ACTIONABILITY_FEATURE_CONTRACT,
    ATTENTION_PAIR_FEATURE_CONTRACT, ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT,
    ATTENTION_SEMANTIC_SCHEMA_VERSION,
};
use crate::config::AttentionRoutingMode;

pub const ATTENTION_ROUTING_DECISION_SCHEMA_VERSION: u32 = 1;
pub const ATTENTION_IMPRESSION_SCHEMA_VERSION: u32 = 1;
pub const ATTENTION_ROUTING_FEATURE_CONTRACT: &str = "attention_routing_features_v1";
pub const ATTENTION_MIN_VISIBLE_MS_MAX: u64 = 60_000;
pub const ATTENTION_VISIBLE_MS_MAX: u64 = 86_400_000;
pub const ATTENTION_EVENT_ID_MAX_CHARS: usize = 200;
pub const ATTENTION_DECISION_ID_MAX_CHARS: usize = 200;
pub const ATTENTION_CANDIDATE_ID_MAX_CHARS: usize = 500;
pub const ATTENTION_SOURCE_REVISION_MAX_CHARS: usize = 500;
pub const ATTENTION_VISIBILITY_RULE_MAX_CHARS: usize = 100;
pub const ATTENTION_CLIENT_TYPE_MAX_CHARS: usize = 64;
pub const ATTENTION_CLIENT_VERSION_MAX_CHARS: usize = 128;
pub const ATTENTION_VIEWPORT_CLASS_MAX_CHARS: usize = 64;

const SUPPORTED_ROUTING_FEATURES: &[&str] = &[
    "model.owner_action_required_probability",
    "semantic.information_value_probability",
    "semantic.direct_request_probability",
    "semantic.broadcast_probability",
    "semantic.personal_obligation_probability",
    "group.duplicate_exposure_cost_log1p",
];

/// Ordered routing vector. Serving and training must share this function.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingFeatureVector {
    pub names: Vec<String>,
    pub values: Vec<f64>,
    pub contract: &'static str,
}

pub fn routing_feature_vector(
    actionability_probability: f64,
    information_value_probability: f64,
    direct_request_probability: f64,
    broadcast_probability: f64,
    personal_obligation_probability: f64,
    related_count: usize,
) -> RoutingFeatureVector {
    RoutingFeatureVector {
        names: SUPPORTED_ROUTING_FEATURES
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
        values: vec![
            actionability_probability,
            information_value_probability,
            direct_request_probability,
            broadcast_probability,
            personal_obligation_probability,
            (related_count as f64).ln_1p(),
        ],
        contract: ATTENTION_ROUTING_FEATURE_CONTRACT,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionRoute {
    FollowUp,
    WorthALook,
    NonSurfaced,
}

impl AttentionRoute {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FollowUp => "follow_up",
            Self::WorthALook => "worth_a_look",
            Self::NonSurfaced => "non_surfaced",
        }
    }

    pub const fn surface(self) -> Option<AttentionSurface> {
        match self {
            Self::FollowUp => Some(AttentionSurface::FollowUp),
            Self::WorthALook => Some(AttentionSurface::WorthALook),
            Self::NonSurfaced => None,
        }
    }
}

impl From<AttentionSurface> for AttentionRoute {
    fn from(value: AttentionSurface) -> Self {
        match value {
            AttentionSurface::FollowUp => Self::FollowUp,
            AttentionSurface::WorthALook => Self::WorthALook,
        }
    }
}

impl std::str::FromStr for AttentionRoute {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "follow_up" => Ok(Self::FollowUp),
            "worth_a_look" => Ok(Self::WorthALook),
            "non_surfaced" => Ok(Self::NonSurfaced),
            other => anyhow::bail!("unknown attention route: {other}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionUtilityHead {
    pub coefficients: Vec<f64>,
    pub intercept: f64,
    /// Calibration applied to the raw linear value. These parameters are part
    /// of the immutable snapshot; serving has no hidden utility constants.
    pub platt_a: f64,
    pub platt_b: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionRoutingTrainingManifest {
    pub dataset_digest: String,
    pub data_cutoff_at: i64,
    pub split_strategy: String,
    pub group_keys: Vec<String>,
    pub metrics: BTreeMap<String, f64>,
}

/// Immutable calibrated lane-policy snapshot. All utility weights,
/// uncertainty calibration, and serving gates live here so a config change
/// cannot silently alter the learned policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionRoutingPolicySnapshot {
    pub snapshot_id: String,
    pub model_version: String,
    pub feature_contract: String,
    pub actionability_feature_contract: String,
    pub actionability_snapshot_id: String,
    pub actionability_model_version: String,
    pub grouping_feature_contract: String,
    /// Exact optional Slice-3 identity represented by the training rows. Both
    /// fields are present together or absent together, and inference requires
    /// an exact match including the `None` case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grouping_snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grouping_model_version: Option<String>,
    pub semantic_schema_version: u32,
    pub semantic_extractor_contract: String,
    pub semantic_prompt_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_profile: Option<String>,
    pub feature_names: Vec<String>,
    pub follow_up: AttentionUtilityHead,
    pub worth_a_look: AttentionUtilityHead,
    /// If both calibrated utilities fall below this snapshot-owned threshold,
    /// the learned diagnostic route is `non_surfaced`.
    pub minimum_surface_utility: f64,
    /// Confidence is `sigmoid(a * abs(utility_margin) + b)` using these frozen
    /// calibrator parameters; uncertainty is its complement.
    pub confidence_platt_a: f64,
    pub confidence_platt_b: f64,
    pub minimum_route_confidence: f64,
    pub minimum_utility_margin: f64,
    pub trained_at: i64,
    pub training_manifest: AttentionRoutingTrainingManifest,
}

impl AttentionRoutingPolicySnapshot {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.snapshot_id.trim().is_empty(),
            "routing snapshot id is empty"
        );
        anyhow::ensure!(
            !self.model_version.trim().is_empty(),
            "routing model version is empty"
        );
        anyhow::ensure!(
            self.feature_contract == ATTENTION_ROUTING_FEATURE_CONTRACT,
            "routing feature contract is incompatible"
        );
        anyhow::ensure!(
            self.actionability_feature_contract == ACTIONABILITY_FEATURE_CONTRACT,
            "routing actionability feature contract is incompatible"
        );
        anyhow::ensure!(
            !self.actionability_snapshot_id.trim().is_empty()
                && !self.actionability_model_version.trim().is_empty(),
            "routing snapshot must bind an actionability snapshot and model"
        );
        anyhow::ensure!(
            self.grouping_feature_contract == ATTENTION_PAIR_FEATURE_CONTRACT,
            "routing grouping feature contract is incompatible"
        );
        anyhow::ensure!(
            self.grouping_snapshot_id.is_some() == self.grouping_model_version.is_some(),
            "routing grouping snapshot id and model version must both be present or absent"
        );
        anyhow::ensure!(
            self.grouping_snapshot_id
                .as_deref()
                .is_none_or(|value| !value.trim().is_empty())
                && self
                    .grouping_model_version
                    .as_deref()
                    .is_none_or(|value| !value.trim().is_empty()),
            "routing grouping snapshot identity is empty"
        );
        anyhow::ensure!(
            self.semantic_schema_version == ATTENTION_SEMANTIC_SCHEMA_VERSION
                && self.semantic_extractor_contract == ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT,
            "routing semantic contract is incompatible"
        );
        anyhow::ensure!(
            !self.semantic_prompt_version.trim().is_empty(),
            "routing semantic prompt version is empty"
        );
        anyhow::ensure!(
            self.semantic_model
                .as_deref()
                .is_none_or(|value| !value.trim().is_empty())
                && self
                    .semantic_profile
                    .as_deref()
                    .is_none_or(|value| !value.trim().is_empty()),
            "routing semantic producer identity is empty"
        );
        anyhow::ensure!(
            !self.feature_names.is_empty(),
            "routing snapshot has no features"
        );
        let mut unique = HashSet::new();
        for name in &self.feature_names {
            anyhow::ensure!(unique.insert(name), "duplicate routing feature: {name}");
            anyhow::ensure!(
                SUPPORTED_ROUTING_FEATURES.contains(&name.as_str()),
                "unsupported routing feature: {name}"
            );
        }
        for head in [&self.follow_up, &self.worth_a_look] {
            anyhow::ensure!(
                head.coefficients.len() == self.feature_names.len(),
                "routing utility coefficient dimension mismatch"
            );
            anyhow::ensure!(
                head.intercept.is_finite()
                    && head.platt_a.is_finite()
                    && head.platt_b.is_finite()
                    && head.coefficients.iter().all(|value| value.is_finite()),
                "routing utility parameters are invalid"
            );
        }
        anyhow::ensure!(
            self.minimum_surface_utility.is_finite()
                && (0.0..=1.0).contains(&self.minimum_surface_utility),
            "routing minimum surface utility must be within 0..=1"
        );
        anyhow::ensure!(
            self.confidence_platt_a.is_finite()
                && self.confidence_platt_a >= 0.0
                && self.confidence_platt_b.is_finite(),
            "routing confidence calibrator is invalid"
        );
        anyhow::ensure!(
            self.minimum_route_confidence.is_finite()
                && (0.0..=1.0).contains(&self.minimum_route_confidence),
            "routing minimum confidence must be within 0..=1"
        );
        anyhow::ensure!(
            self.minimum_utility_margin.is_finite()
                && (0.0..=1.0).contains(&self.minimum_utility_margin),
            "routing minimum utility margin must be within 0..=1"
        );
        anyhow::ensure!(
            !self.training_manifest.dataset_digest.trim().is_empty(),
            "routing training dataset digest is empty"
        );
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct AttentionRoutingCandidate<'a> {
    pub candidate: &'a SemanticAttentionCandidate,
    pub source_family: &'a str,
    pub baseline_route: AttentionRoute,
    pub rank: &'a AttentionRankMetadata,
    pub grouping: &'a AttentionGroupingMetadata,
    pub hard_eligible: bool,
    pub ineligibility_reason: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionDecisionItem {
    pub decision_id: String,
    pub candidate_id: String,
    pub source_revision: Option<String>,
    /// Point-in-time features, carried to the store and deduped there by their
    /// complete producer contract plus vector content. Skipped by serde on purpose: `item_json` is the
    /// per-decision routing record, and every decision repeats the same
    /// universe, so persisting the vector inline would multiply one candidate's
    /// features by the number of times it was served.
    #[serde(skip)]
    pub feature_values: Option<std::collections::BTreeMap<String, f64>>,
    pub source_family: String,
    pub hard_eligible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ineligibility_reason: Option<String>,
    pub baseline_route: AttentionRoute,
    pub learned_route: AttentionRoute,
    pub served_route: AttentionRoute,
    pub routing_mode: AttentionRoutingMode,
    #[serde(default)]
    pub routing_snapshot_id: Option<String>,
    #[serde(default)]
    pub routing_model_version: Option<String>,
    #[serde(default)]
    pub learned_route_confidence: Option<f64>,
    #[serde(default)]
    pub utility_margin: Option<f64>,
    pub route_reason: String,
    pub route_applied: bool,
    pub canary_assigned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_action_required_probability: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub information_value_probability: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_utility: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worth_a_look_utility: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty: Option<f64>,
    pub cluster_id: String,
    pub cluster_size: usize,
    pub representative: bool,
    pub baseline_rank: usize,
    pub learned_rank: usize,
    /// One-based position in the finalized served projection after ranking,
    /// grouping, and routing. Zero means the item was not materialized in that
    /// served lane (for example, a non-representative grouped member).
    #[serde(default)]
    pub served_rank: usize,
    pub selected: bool,
    /// Slice 4 selection is deterministic: returned items are 1.0 and every
    /// other evaluated item is 0.0. Randomized propensities begin in Slice 5.
    pub selection_probability: f64,
    pub exploration: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandit_decision: Option<AttentionBanditDecisionMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature_snapshot_digest: Option<String>,
    pub extraction_status: SemanticExtractionStatus,
    pub feature_contracts: AttentionDecisionFeatureContracts,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttentionDecisionFeatureContracts {
    pub routing_feature_contract: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actionability_snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actionability_model_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grouping_snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grouping_model_version: Option<String>,
    pub semantic_schema_version: u32,
    pub semantic_extractor_contract: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_prompt_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_profile: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionDecision {
    pub decision_id: String,
    pub decided_at: i64,
    pub surface: AttentionSurface,
    pub routing_mode: AttentionRoutingMode,
    #[serde(default)]
    pub routing_snapshot_id: Option<String>,
    #[serde(default)]
    pub routing_model_version: Option<String>,
    pub candidate_set_digest: String,
    pub eligible_item_count: usize,
    pub selected_item_count: usize,
    pub returned_item_count: usize,
    pub complete_universe_recorded: bool,
    pub complete_cross_lane_universe: bool,
    pub policy_seed_identity: String,
    pub canary_assigned: bool,
    pub context: AttentionDecisionContext,
    pub latency_ms: u64,
    #[serde(default)]
    pub degradation_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandit_health: Option<AttentionBanditHealth>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AttentionDecisionContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_time_bucket: Option<String>,
    pub queue_size: usize,
    pub recent_impression_count: u64,
    pub recent_action_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fatigue_state: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionRoutingEvaluation {
    pub decision_id: String,
    pub decided_at: i64,
    pub surface: AttentionSurface,
    pub mode: AttentionRoutingMode,
    pub snapshot_id: Option<String>,
    pub model_version: Option<String>,
    pub policy_seed_identity: String,
    pub canary_assigned: bool,
    /// True only when the caller evaluated the canonical union of baseline
    /// Follow-up and Worth-a-look candidates. Cross-lane application is
    /// forbidden otherwise so a moved item cannot disappear between lists.
    pub complete_cross_lane_universe: bool,
    pub degradation_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandit_health: Option<AttentionBanditHealth>,
    pub items: Vec<AttentionDecisionItem>,
}

impl AttentionRoutingEvaluation {
    pub fn item(&self, candidate_id: &str) -> Option<&AttentionDecisionItem> {
        self.items
            .iter()
            .find(|item| item.candidate_id == candidate_id)
    }

    pub fn finalize_served_projection<S, I>(&mut self, served_ids: S, selected_ids: I)
    where
        S: IntoIterator,
        S::Item: AsRef<str>,
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        let served_ranks: HashMap<String, usize> = served_ids
            .into_iter()
            .enumerate()
            .map(|(index, value)| (value.as_ref().to_string(), index + 1))
            .collect();
        let selected: HashSet<String> = selected_ids
            .into_iter()
            .map(|value| value.as_ref().to_string())
            .collect();
        for item in &mut self.items {
            item.served_rank = served_ranks
                .get(&item.candidate_id)
                .copied()
                .unwrap_or_default();
            item.selected = selected.contains(&item.candidate_id);
            item.selection_probability = if item.selected { 1.0 } else { 0.0 };
            item.exploration = false;
            item.bandit_decision = None;
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordAttentionImpression {
    pub event_id: String,
    pub decision_id: String,
    pub delivery_id: String,
    pub page_index: usize,
    pub position: usize,
    pub exposure_token: String,
    pub candidate_id: String,
    pub source_revision: Option<String>,
    pub surface: AttentionSurface,
    pub visible_ms: u64,
    pub visibility_rule_version: String,
    pub client_type: String,
    pub client_version: String,
    pub viewport_class: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionImpressionReceipt {
    pub impression_id: String,
    pub event_id: String,
    pub decision_id: String,
    pub delivery_id: String,
    pub page_index: usize,
    pub position: usize,
    pub exposure_token: String,
    pub candidate_id: String,
    pub source_revision: Option<String>,
    pub surface: AttentionSurface,
    pub accumulated_visible_ms: u64,
    pub min_visible_ms: u64,
    pub visibility_rule_version: String,
    pub root_policy_propensity: f64,
    pub conditional_delivery_propensity: f64,
    pub verified: bool,
    pub deduplicated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum AttentionImpressionError {
    #[error("invalid impression request: {0}")]
    InvalidRequest(String),
    #[error("decision item not found")]
    DecisionItemNotFound,
    #[error("decision item identity, revision, selected state, or served surface does not match")]
    DecisionItemMismatch,
    #[error("delivered card exposure not found")]
    DeliveryItemNotFound,
    #[error("delivery, page, position, revision, or exposure token does not match")]
    DeliveryBindingMismatch,
    #[error("impression event_id is already bound to a different identity")]
    EventIdentityConflict,
    #[error("impression visibility rule does not match the configured rule")]
    VisibilityRuleMismatch,
    #[error(transparent)]
    Storage(#[from] anyhow::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionRoutingHealth {
    pub routing_snapshot_valid: bool,
    pub evaluated_count: usize,
    pub learned_route_count: usize,
    pub applied_route_count: usize,
    pub baseline_retained_count: usize,
    pub impression_eligible_count: usize,
    pub decision_item_coverage: f64,
    /// Verified impressions for *this* decision only. A fresh decision is
    /// recorded on nearly every projection, so this is near-zero by
    /// construction and must not be read as "is the reward signal flowing".
    pub verified_impression_coverage: f64,
    /// Verified impressions across the whole scope, independent of any single
    /// decision. This is the signal-is-flowing indicator.
    #[serde(default)]
    pub verified_impression_total: u64,
    pub impression_dedupe_count: u64,
    pub all_candidates_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionDecisionDetail {
    pub decision: AttentionDecision,
    pub items: Vec<AttentionDecisionItem>,
}

pub fn deterministic_canary_assignment(
    fraction: f64,
    seed_identity: &str,
    principal: &str,
    workspace: &str,
    surface: AttentionSurface,
) -> bool {
    if fraction <= 0.0 {
        return false;
    }
    if fraction >= 1.0 {
        return true;
    }
    let digest = blake3::hash(
        format!(
            "{seed_identity}\0{principal}\0{workspace}\0{}",
            surface.as_str()
        )
        .as_bytes(),
    );
    let mut prefix = [0_u8; 8];
    prefix.copy_from_slice(&digest.as_bytes()[..8]);
    let bucket = u64::from_le_bytes(prefix) as f64 / u64::MAX as f64;
    bucket < fraction
}

pub fn evaluate_routing(
    decision_id: String,
    decided_at: i64,
    principal: &str,
    workspace: &str,
    requested_surface: AttentionSurface,
    configured_mode: AttentionRoutingMode,
    canary_fraction: f64,
    policy_seed_identity: &str,
    snapshot: Option<&AttentionRoutingPolicySnapshot>,
    complete_cross_lane_universe: bool,
    candidates: &[AttentionRoutingCandidate<'_>],
) -> Result<AttentionRoutingEvaluation> {
    let canary_assigned = configured_mode == AttentionRoutingMode::Canary
        && deterministic_canary_assignment(
            canary_fraction,
            policy_seed_identity,
            principal,
            workspace,
            requested_surface,
        );
    let (effective_mode, degradation_reason) = match (configured_mode, snapshot) {
        (AttentionRoutingMode::Baseline, _) => (AttentionRoutingMode::Baseline, None),
        (mode, Some(snapshot)) => match snapshot.validate() {
            Ok(()) => (mode, None),
            Err(_) => (
                AttentionRoutingMode::Baseline,
                Some("snapshot_invalid".to_string()),
            ),
        },
        (_, None) => (AttentionRoutingMode::Baseline, None),
    };
    let active_snapshot = (effective_mode != AttentionRoutingMode::Baseline)
        .then_some(snapshot)
        .flatten();
    let mut items = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        items.push(evaluate_candidate(
            &decision_id,
            effective_mode,
            canary_assigned,
            complete_cross_lane_universe,
            active_snapshot,
            candidate,
        )?);
    }
    if let Some(reason) = degradation_reason.as_deref() {
        for item in &mut items {
            item.route_reason = reason.to_string();
        }
    }
    assign_complete_lane_ranks(&mut items);
    Ok(AttentionRoutingEvaluation {
        decision_id,
        decided_at,
        surface: requested_surface,
        mode: effective_mode,
        snapshot_id: active_snapshot.map(|snapshot| snapshot.snapshot_id.clone()),
        model_version: active_snapshot.map(|snapshot| snapshot.model_version.clone()),
        policy_seed_identity: policy_seed_identity.to_string(),
        canary_assigned,
        complete_cross_lane_universe,
        degradation_reason,
        bandit_health: None,
        items,
    })
}

const KNN_LANE_MIN_WEIGHT: f64 = 0.25;
const KNN_DEMOTE_MAX: f64 = 0.35;
const KNN_PROMOTE_MIN: f64 = 0.65;

struct KnnLaneDecision {
    learned: AttentionRoute,
    confidence: f64,
    reason: &'static str,
    apply: bool,
}

fn knn_lane_decision(candidate: &AttentionRoutingCandidate<'_>) -> Option<KnnLaneDecision> {
    let probability = candidate.rank.slice1_actionability_probability?;
    let weight = candidate.rank.slice1_actionability_weight?;
    if !probability.is_finite()
        || !(0.0..=1.0).contains(&probability)
        || weight < KNN_LANE_MIN_WEIGHT
    {
        return None;
    }
    match candidate.baseline_route {
        AttentionRoute::FollowUp if probability <= KNN_DEMOTE_MAX => Some(KnnLaneDecision {
            learned: AttentionRoute::WorthALook,
            confidence: 1.0 - probability,
            reason: "knn_demotion_applied",
            apply: true,
        }),
        AttentionRoute::WorthALook if probability >= KNN_PROMOTE_MIN => Some(KnnLaneDecision {
            learned: AttentionRoute::FollowUp,
            confidence: probability,
            reason: "knn_promotion_applied",
            apply: true,
        }),
        AttentionRoute::FollowUp | AttentionRoute::WorthALook => Some(KnnLaneDecision {
            learned: candidate.baseline_route,
            confidence: (probability - 0.5).abs() * 2.0,
            reason: "knn_lane_shadow",
            apply: false,
        }),
        AttentionRoute::NonSurfaced => None,
    }
}

fn evaluate_candidate(
    decision_id: &str,
    mode: AttentionRoutingMode,
    canary_assigned: bool,
    complete_cross_lane_universe: bool,
    snapshot: Option<&AttentionRoutingPolicySnapshot>,
    candidate: &AttentionRoutingCandidate<'_>,
) -> Result<AttentionDecisionItem> {
    let baseline_route = candidate.baseline_route;
    let semantic_contract = candidate
        .candidate
        .actionability_features
        .as_ref()
        .and_then(|input| input.semantic.as_ref());
    let base_contracts = AttentionDecisionFeatureContracts {
        routing_feature_contract: ATTENTION_ROUTING_FEATURE_CONTRACT.to_string(),
        routing_snapshot_id: snapshot.map(|value| value.snapshot_id.clone()),
        actionability_snapshot_id: candidate.rank.actionability_snapshot_id.clone(),
        actionability_model_version: candidate.rank.actionability_model_version.clone(),
        grouping_snapshot_id: candidate.grouping.snapshot_id.clone(),
        grouping_model_version: candidate.grouping.model_version.clone(),
        semantic_schema_version: ATTENTION_SEMANTIC_SCHEMA_VERSION,
        semantic_extractor_contract: ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT.to_string(),
        semantic_prompt_version: semantic_contract.map(|value| value.prompt_version.clone()),
        semantic_model: semantic_contract.and_then(|value| value.model.clone()),
        semantic_profile: semantic_contract.and_then(|value| value.profile.clone()),
    };
    let captured_features = candidate
        .candidate
        .actionability_features
        .as_ref()
        .and_then(super::actionability::captured_feature_values);
    let fallback = |reason: &str| AttentionDecisionItem {
        decision_id: decision_id.to_string(),
        candidate_id: candidate.candidate.candidate_id.clone(),
        source_revision: candidate.candidate.source_revision.clone(),
        feature_values: captured_features.clone(),
        source_family: candidate.source_family.to_string(),
        hard_eligible: candidate.hard_eligible,
        ineligibility_reason: candidate.ineligibility_reason.map(str::to_string),
        baseline_route,
        learned_route: baseline_route,
        served_route: baseline_route,
        routing_mode: mode,
        routing_snapshot_id: snapshot.map(|value| value.snapshot_id.clone()),
        routing_model_version: snapshot.map(|value| value.model_version.clone()),
        learned_route_confidence: None,
        utility_margin: None,
        route_reason: reason.to_string(),
        route_applied: false,
        canary_assigned,
        owner_action_required_probability: candidate.rank.actionability_probability,
        information_value_probability: None,
        follow_up_utility: None,
        worth_a_look_utility: None,
        uncertainty: None,
        cluster_id: candidate.grouping.cluster_id.clone(),
        cluster_size: candidate.grouping.member_count,
        representative: candidate.grouping.is_representative,
        baseline_rank: candidate.rank.baseline_rank,
        learned_rank: candidate.rank.learned_rank,
        served_rank: 0,
        selected: false,
        selection_probability: 0.0,
        exploration: false,
        feature_snapshot_digest: None,
        bandit_decision: None,
        extraction_status: candidate.rank.semantic_feature_status,
        feature_contracts: base_contracts.clone(),
    };
    let Some(snapshot) = snapshot else {
        if let Some(knn) = knn_lane_decision(candidate) {
            let mut item = fallback(knn.reason);
            item.learned_route = knn.learned;
            item.learned_route_confidence = Some(knn.confidence);
            item.owner_action_required_probability = candidate
                .rank
                .slice1_actionability_probability
                .or(candidate.rank.actionability_probability);
            if knn.apply {
                item.served_route = knn.learned;
                item.route_applied = true;
            }
            return Ok(item);
        }
        return Ok(fallback("baseline_mode"));
    };
    if !candidate.hard_eligible {
        return Ok(fallback("hard_ineligible"));
    }
    if candidate.rank.actionability_score_status != ActionabilityScoreStatus::Scored
        || candidate.rank.actionability_snapshot_id.as_deref()
            != Some(snapshot.actionability_snapshot_id.as_str())
        || candidate.rank.actionability_model_version.as_deref()
            != Some(snapshot.actionability_model_version.as_str())
    {
        return Ok(fallback("contract_mismatch"));
    }
    if candidate.grouping.snapshot_id != snapshot.grouping_snapshot_id
        || candidate.grouping.model_version != snapshot.grouping_model_version
    {
        return Ok(fallback("contract_mismatch"));
    }
    let Some(actionability) = candidate
        .rank
        .actionability_probability
        .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
    else {
        return Ok(fallback("contract_mismatch"));
    };
    let Some(semantic) = candidate
        .candidate
        .actionability_features
        .as_ref()
        .and_then(|input| input.semantic.as_ref())
    else {
        return Ok(fallback("contract_mismatch"));
    };
    let exact_revision = semantic.is_compatible(candidate.candidate.source_revision.as_deref());
    if !exact_revision
        || semantic.status != SemanticExtractionStatus::Succeeded
        || semantic.schema_version != snapshot.semantic_schema_version
        || semantic.extractor_contract != snapshot.semantic_extractor_contract
        || semantic.prompt_version != snapshot.semantic_prompt_version
        || semantic.model != snapshot.semantic_model
        || semantic.profile != snapshot.semantic_profile
    {
        return Ok(fallback("contract_mismatch"));
    }
    let Some(semantic_features) = semantic.features.as_ref() else {
        return Ok(fallback("contract_mismatch"));
    };
    let extracted = routing_feature_vector(
        actionability,
        semantic_features.information_value_probability,
        semantic_features.direct_request_probability,
        semantic_features.broadcast_probability,
        semantic_features.personal_obligation_probability,
        candidate.grouping.related_count,
    );
    let feature_map: HashMap<&str, f64> = extracted
        .names
        .iter()
        .zip(&extracted.values)
        .map(|(name, value)| (name.as_str(), *value))
        .collect();
    let ordered: Vec<f64> = snapshot
        .feature_names
        .iter()
        .map(|name| feature_map.get(name.as_str()).copied().unwrap_or(0.0))
        .collect();
    let follow_up_utility = utility(&snapshot.follow_up, &ordered);
    let worth_a_look_utility = utility(&snapshot.worth_a_look, &ordered);
    let margin = (follow_up_utility - worth_a_look_utility).abs();
    let confidence = sigmoid(snapshot.confidence_platt_a * margin + snapshot.confidence_platt_b);
    let uncertainty = 1.0 - confidence;
    let learned_route =
        if follow_up_utility.max(worth_a_look_utility) < snapshot.minimum_surface_utility {
            AttentionRoute::NonSurfaced
        } else if follow_up_utility >= worth_a_look_utility {
            AttentionRoute::FollowUp
        } else {
            AttentionRoute::WorthALook
        };
    let gate_reason = if confidence < snapshot.minimum_route_confidence {
        Some("below_confidence_gate")
    } else if margin < snapshot.minimum_utility_margin {
        Some("below_margin_gate")
    } else {
        None
    };
    let route_applied = mode == AttentionRoutingMode::Canary
        && canary_assigned
        && complete_cross_lane_universe
        && gate_reason.is_none()
        && learned_route != baseline_route;
    let served_route = if route_applied {
        learned_route
    } else {
        baseline_route
    };
    let route_reason = if let Some(reason) = gate_reason {
        reason
    } else if mode == AttentionRoutingMode::Shadow {
        "shadow_only"
    } else if mode == AttentionRoutingMode::Canary && !canary_assigned {
        "not_in_canary"
    } else if mode == AttentionRoutingMode::Canary
        && learned_route != baseline_route
        && !complete_cross_lane_universe
    {
        "cross_lane_universe_incomplete"
    } else if route_applied {
        "learned_route_applied"
    } else {
        "baseline_route_retained"
    };
    let feature_snapshot_digest =
        routing_input_digest(snapshot, candidate.candidate, candidate.grouping, &ordered)?;
    Ok(AttentionDecisionItem {
        decision_id: decision_id.to_string(),
        candidate_id: candidate.candidate.candidate_id.clone(),
        source_revision: candidate.candidate.source_revision.clone(),
        feature_values: captured_features,
        source_family: candidate.source_family.to_string(),
        hard_eligible: candidate.hard_eligible,
        ineligibility_reason: candidate.ineligibility_reason.map(str::to_string),
        baseline_route,
        learned_route,
        served_route,
        routing_mode: mode,
        routing_snapshot_id: Some(snapshot.snapshot_id.clone()),
        routing_model_version: Some(snapshot.model_version.clone()),
        learned_route_confidence: Some(confidence),
        utility_margin: Some(margin),
        route_reason: route_reason.to_string(),
        route_applied,
        canary_assigned,
        owner_action_required_probability: Some(actionability),
        information_value_probability: Some(semantic_features.information_value_probability),
        follow_up_utility: Some(follow_up_utility),
        worth_a_look_utility: Some(worth_a_look_utility),
        uncertainty: Some(uncertainty),
        cluster_id: candidate.grouping.cluster_id.clone(),
        cluster_size: candidate.grouping.member_count,
        representative: candidate.grouping.is_representative,
        baseline_rank: candidate.rank.baseline_rank,
        learned_rank: candidate.rank.learned_rank,
        served_rank: 0,
        selected: false,
        selection_probability: 0.0,
        exploration: false,
        feature_snapshot_digest: Some(feature_snapshot_digest),
        bandit_decision: None,
        extraction_status: semantic.status,
        feature_contracts: base_contracts,
    })
}

fn utility(head: &AttentionUtilityHead, values: &[f64]) -> f64 {
    let raw = head
        .coefficients
        .iter()
        .zip(values)
        .fold(head.intercept, |sum, (coefficient, value)| {
            sum + coefficient * value
        });
    sigmoid(head.platt_a * raw + head.platt_b)
}

fn sigmoid(value: f64) -> f64 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exp = value.exp();
        exp / (1.0 + exp)
    }
}

fn routing_input_digest(
    snapshot: &AttentionRoutingPolicySnapshot,
    candidate: &SemanticAttentionCandidate,
    grouping: &AttentionGroupingMetadata,
    values: &[f64],
) -> Result<String> {
    #[derive(Serialize)]
    struct Input<'a> {
        candidate_id: &'a str,
        source_revision: Option<&'a str>,
        snapshot_id: &'a str,
        actionability_snapshot_id: &'a str,
        grouping_snapshot_id: Option<&'a str>,
        grouping_model_version: Option<&'a str>,
        cluster_id: &'a str,
        feature_names: &'a [String],
        values: &'a [f64],
    }
    let encoded = serde_json::to_vec(&Input {
        candidate_id: &candidate.candidate_id,
        source_revision: candidate.source_revision.as_deref(),
        snapshot_id: &snapshot.snapshot_id,
        actionability_snapshot_id: &snapshot.actionability_snapshot_id,
        grouping_snapshot_id: grouping.snapshot_id.as_deref(),
        grouping_model_version: grouping.model_version.as_deref(),
        cluster_id: &grouping.cluster_id,
        feature_names: &snapshot.feature_names,
        values,
    })
    .context("serializing routing feature snapshot")?;
    Ok(blake3::hash(&encoded).to_hex().to_string())
}

/// Learned ranks are assigned only after the complete evaluated universe has
/// been routed. Each lane is sorted by its calibrated utility, with baseline
/// rank and candidate id as deterministic ties. Pagination must happen later.
fn assign_complete_lane_ranks(items: &mut [AttentionDecisionItem]) {
    for route in [
        AttentionRoute::FollowUp,
        AttentionRoute::WorthALook,
        AttentionRoute::NonSurfaced,
    ] {
        let mut indexes: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.learned_route == route)
            .map(|(index, _)| index)
            .collect();
        indexes.sort_by(|left, right| {
            let utility_for = |item: &AttentionDecisionItem| match route {
                AttentionRoute::FollowUp => item.follow_up_utility,
                AttentionRoute::WorthALook => item.worth_a_look_utility,
                AttentionRoute::NonSurfaced => item
                    .follow_up_utility
                    .zip(item.worth_a_look_utility)
                    .map(|(left, right)| left.max(right)),
            };
            utility_for(&items[*right])
                .unwrap_or(f64::NEG_INFINITY)
                .total_cmp(&utility_for(&items[*left]).unwrap_or(f64::NEG_INFINITY))
                .then_with(|| items[*left].baseline_rank.cmp(&items[*right].baseline_rank))
                .then_with(|| items[*left].candidate_id.cmp(&items[*right].candidate_id))
        });
        for (rank, index) in indexes.into_iter().enumerate() {
            items[index].learned_rank = rank + 1;
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::attention::learning::{
        ActionabilityFeatureInput, AttentionGroupingMetadata, ChannelAttentionSemanticEnvelope,
        ChannelAttentionSemanticFeatures, CommunicationType, RequestedAction, SemanticActionOwner,
        SemanticDeadline, SemanticDeadlineKind,
    };

    fn snapshot() -> AttentionRoutingPolicySnapshot {
        AttentionRoutingPolicySnapshot {
            snapshot_id: "route-v1".to_string(),
            model_version: "utility-logistic-v1".to_string(),
            feature_contract: ATTENTION_ROUTING_FEATURE_CONTRACT.to_string(),
            actionability_feature_contract: ACTIONABILITY_FEATURE_CONTRACT.to_string(),
            actionability_snapshot_id: "action-v1".to_string(),
            actionability_model_version: "action-logistic-v1".to_string(),
            grouping_feature_contract: ATTENTION_PAIR_FEATURE_CONTRACT.to_string(),
            grouping_snapshot_id: Some("pair-v1".to_string()),
            grouping_model_version: Some("pair-logistic-v1".to_string()),
            semantic_schema_version: ATTENTION_SEMANTIC_SCHEMA_VERSION,
            semantic_extractor_contract: ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT.to_string(),
            semantic_prompt_version: "1.1.0".to_string(),
            semantic_model: None,
            semantic_profile: None,
            feature_names: vec![
                "model.owner_action_required_probability".to_string(),
                "semantic.information_value_probability".to_string(),
                "group.duplicate_exposure_cost_log1p".to_string(),
            ],
            follow_up: AttentionUtilityHead {
                coefficients: vec![4.0, -2.0, -0.5],
                intercept: -1.0,
                platt_a: 1.0,
                platt_b: 0.0,
            },
            worth_a_look: AttentionUtilityHead {
                coefficients: vec![-3.0, 4.0, -0.5],
                intercept: -1.0,
                platt_a: 1.0,
                platt_b: 0.0,
            },
            minimum_surface_utility: 0.2,
            confidence_platt_a: 10.0,
            confidence_platt_b: 0.0,
            minimum_route_confidence: 0.8,
            minimum_utility_margin: 0.2,
            trained_at: 1,
            training_manifest: AttentionRoutingTrainingManifest {
                dataset_digest: "fixture".to_string(),
                data_cutoff_at: 1,
                split_strategy: "grouped_temporal".to_string(),
                group_keys: vec!["sender".to_string()],
                metrics: BTreeMap::new(),
            },
        }
    }

    fn candidate() -> SemanticAttentionCandidate {
        SemanticAttentionCandidate {
            candidate_id: "candidate-1".to_string(),
            source_revision: Some("distill:7".to_string()),
            semantic_text: "safe brief".to_string(),
            existing_embedding: None,
            actionability_features: Some(ActionabilityFeatureInput {
                semantic: Some(ChannelAttentionSemanticEnvelope {
                    schema_version: ATTENTION_SEMANTIC_SCHEMA_VERSION,
                    extractor_contract: ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT.to_string(),
                    prompt_version: "1.1.0".to_string(),
                    model: None,
                    profile: None,
                    input_revision: 7,
                    source_revision: None,
                    status: SemanticExtractionStatus::Succeeded,
                    features: Some(ChannelAttentionSemanticFeatures {
                        communication_type: CommunicationType::Newsletter,
                        requested_action: RequestedAction::None,
                        action_owner: SemanticActionOwner::Sender,
                        direct_request_probability: 0.05,
                        broadcast_probability: 0.95,
                        personal_obligation_probability: 0.05,
                        information_value_probability: 0.95,
                        deadline: SemanticDeadline {
                            kind: SemanticDeadlineKind::None,
                            value: None,
                        },
                        campaign_or_event_identity: Some("campaign".to_string()),
                        evidence_refs: vec!["summary".to_string()],
                    }),
                    invalid_reason_code: None,
                }),
                ..Default::default()
            }),
            grouping_features: None,
        }
    }

    fn rank() -> AttentionRankMetadata {
        AttentionRankMetadata {
            candidate_id: "candidate-1".to_string(),
            baseline_rank: 1,
            learned_rank: 1,
            rank_delta: 0,
            learning_score: None,
            slice1_actionability_probability: None,
            slice1_actionability_weight: None,
            actionability_probability: Some(0.05),
            actionability_explanation: None,
            actionability_model_version: Some("action-logistic-v1".to_string()),
            actionability_snapshot_id: Some("action-v1".to_string()),
            semantic_feature_status: SemanticExtractionStatus::Succeeded,
            actionability_score_status: ActionabilityScoreStatus::Scored,
            actionability_mode: crate::config::AttentionActionabilityMode::Shadow,
        }
    }

    #[test]
    fn shadow_records_learned_route_but_never_applies_it() {
        let candidate = candidate();
        let rank = rank();
        let grouping = AttentionGroupingMetadata {
            cluster_id: "cluster-1".to_string(),
            representative_id: "candidate-1".to_string(),
            is_representative: true,
            member_count: 1,
            related_count: 0,
            model_version: Some("pair-logistic-v1".to_string()),
            snapshot_id: Some("pair-v1".to_string()),
            merge_probability: None,
        };
        let evaluation = evaluate_routing(
            "decision-1".to_string(),
            1,
            "owner",
            "workspace",
            AttentionSurface::FollowUp,
            AttentionRoutingMode::Shadow,
            0.0,
            "slice4",
            Some(&snapshot()),
            false,
            &[AttentionRoutingCandidate {
                candidate: &candidate,
                source_family: "comms_ingest",
                baseline_route: AttentionRoute::FollowUp,
                rank: &rank,
                grouping: &grouping,
                hard_eligible: true,
                ineligibility_reason: None,
            }],
        )
        .unwrap();
        assert_eq!(
            evaluation.items[0].learned_route,
            AttentionRoute::WorthALook
        );
        assert_eq!(evaluation.items[0].served_route, AttentionRoute::FollowUp);
        assert!(!evaluation.items[0].route_applied);
        assert_eq!(evaluation.items[0].route_reason, "shadow_only");
    }

    #[test]
    fn hard_ineligible_is_recorded_and_does_not_apply_learned_routing() {
        let candidate = candidate();
        let rank = rank();
        let grouping = AttentionGroupingMetadata {
            cluster_id: "cluster-1".to_string(),
            representative_id: "candidate-1".to_string(),
            is_representative: true,
            member_count: 1,
            related_count: 0,
            model_version: Some("pair-logistic-v1".to_string()),
            snapshot_id: Some("pair-v1".to_string()),
            merge_probability: None,
        };
        let evaluation = evaluate_routing(
            "decision-1".to_string(),
            1,
            "owner",
            "workspace",
            AttentionSurface::FollowUp,
            AttentionRoutingMode::Canary,
            0.0,
            "slice4",
            Some(&snapshot()),
            true,
            &[AttentionRoutingCandidate {
                candidate: &candidate,
                source_family: "comms_ingest",
                baseline_route: AttentionRoute::FollowUp,
                rank: &rank,
                grouping: &grouping,
                hard_eligible: false,
                ineligibility_reason: Some("stated preference preferences: avoid_vendor_calls"),
            }],
        )
        .unwrap();
        assert!(!evaluation.items[0].hard_eligible);
        assert_eq!(
            evaluation.items[0].ineligibility_reason.as_deref(),
            Some("stated preference preferences: avoid_vendor_calls")
        );
        assert_eq!(evaluation.items[0].route_reason, "hard_ineligible");
        assert_eq!(evaluation.items[0].served_route, AttentionRoute::FollowUp);
        assert!(!evaluation.items[0].route_applied);
    }

    #[test]
    fn exact_contract_mismatch_fails_closed_to_baseline() {
        let candidate = candidate();
        let mut rank = rank();
        rank.actionability_snapshot_id = Some("stale-action".to_string());
        let grouping = AttentionGroupingMetadata {
            cluster_id: "cluster-1".to_string(),
            representative_id: "candidate-1".to_string(),
            is_representative: true,
            member_count: 1,
            related_count: 0,
            model_version: Some("pair-logistic-v1".to_string()),
            snapshot_id: Some("pair-v1".to_string()),
            merge_probability: None,
        };
        let evaluation = evaluate_routing(
            "decision-1".to_string(),
            1,
            "owner",
            "workspace",
            AttentionSurface::FollowUp,
            AttentionRoutingMode::Canary,
            1.0,
            "slice4",
            Some(&snapshot()),
            false,
            &[AttentionRoutingCandidate {
                candidate: &candidate,
                source_family: "comms_ingest",
                baseline_route: AttentionRoute::FollowUp,
                rank: &rank,
                grouping: &grouping,
                hard_eligible: true,
                ineligibility_reason: None,
            }],
        )
        .unwrap();
        assert_eq!(evaluation.items[0].served_route, AttentionRoute::FollowUp);
        assert_eq!(evaluation.items[0].route_reason, "contract_mismatch");
    }

    #[test]
    fn routing_snapshot_rejects_partial_grouping_identity() {
        let mut snapshot = snapshot();
        snapshot.grouping_model_version = None;
        assert!(snapshot.validate().is_err());
    }

    #[test]
    fn grouping_snapshot_mismatch_blocks_duplicate_cost_and_route_proposal() {
        let candidate = candidate();
        let rank = rank();
        let grouping = AttentionGroupingMetadata {
            cluster_id: "cluster-1".to_string(),
            representative_id: "candidate-1".to_string(),
            is_representative: true,
            member_count: 4,
            related_count: 3,
            model_version: Some("pair-logistic-v1".to_string()),
            snapshot_id: Some("stale-pair".to_string()),
            merge_probability: Some(0.99),
        };
        let evaluation = evaluate_routing(
            "decision-1".to_string(),
            1,
            "owner",
            "workspace",
            AttentionSurface::FollowUp,
            AttentionRoutingMode::Shadow,
            0.0,
            "slice4",
            Some(&snapshot()),
            false,
            &[AttentionRoutingCandidate {
                candidate: &candidate,
                source_family: "comms_ingest",
                baseline_route: AttentionRoute::FollowUp,
                rank: &rank,
                grouping: &grouping,
                hard_eligible: true,
                ineligibility_reason: None,
            }],
        )
        .unwrap();
        let item = &evaluation.items[0];
        assert_eq!(item.route_reason, "contract_mismatch");
        assert_eq!(item.learned_route, AttentionRoute::FollowUp);
        assert!(item.follow_up_utility.is_none());
        assert!(item.feature_snapshot_digest.is_none());
    }

    #[test]
    fn absent_grouping_identity_must_match_absent_candidate_identity() {
        let candidate = candidate();
        let rank = rank();
        let mut no_grouping_snapshot = snapshot();
        no_grouping_snapshot.grouping_snapshot_id = None;
        no_grouping_snapshot.grouping_model_version = None;
        let grouping = AttentionGroupingMetadata {
            cluster_id: "singleton".to_string(),
            representative_id: "candidate-1".to_string(),
            is_representative: true,
            member_count: 1,
            related_count: 0,
            model_version: None,
            snapshot_id: None,
            merge_probability: None,
        };
        let evaluation = evaluate_routing(
            "decision-1".to_string(),
            1,
            "owner",
            "workspace",
            AttentionSurface::FollowUp,
            AttentionRoutingMode::Shadow,
            0.0,
            "slice4",
            Some(&no_grouping_snapshot),
            false,
            &[AttentionRoutingCandidate {
                candidate: &candidate,
                source_family: "comms_ingest",
                baseline_route: AttentionRoute::FollowUp,
                rank: &rank,
                grouping: &grouping,
                hard_eligible: true,
                ineligibility_reason: None,
            }],
        )
        .unwrap();
        assert_eq!(evaluation.items[0].route_reason, "shadow_only");
        assert!(evaluation.items[0].follow_up_utility.is_some());
    }

    #[test]
    fn deterministic_cohort_does_not_depend_on_random_decision_id() {
        let first = deterministic_canary_assignment(
            0.25,
            "slice4",
            "owner",
            "workspace",
            AttentionSurface::FollowUp,
        );
        let second = deterministic_canary_assignment(
            0.25,
            "slice4",
            "owner",
            "workspace",
            AttentionSurface::FollowUp,
        );
        assert_eq!(first, second);
    }

    #[test]
    fn finalized_served_projection_separates_display_rank_from_learned_rank() {
        let candidate = candidate();
        let rank = rank();
        let grouping = AttentionGroupingMetadata {
            cluster_id: "cluster-1".to_string(),
            representative_id: "candidate-1".to_string(),
            is_representative: true,
            member_count: 1,
            related_count: 0,
            model_version: Some("pair-logistic-v1".to_string()),
            snapshot_id: Some("pair-v1".to_string()),
            merge_probability: None,
        };
        let mut evaluation = evaluate_routing(
            "decision-1".to_string(),
            1,
            "owner",
            "workspace",
            AttentionSurface::FollowUp,
            AttentionRoutingMode::Shadow,
            0.0,
            "slice4",
            Some(&snapshot()),
            false,
            &[AttentionRoutingCandidate {
                candidate: &candidate,
                source_family: "comms_ingest",
                baseline_route: AttentionRoute::FollowUp,
                rank: &rank,
                grouping: &grouping,
                hard_eligible: true,
                ineligibility_reason: None,
            }],
        )
        .unwrap();

        evaluation.finalize_served_projection(["candidate-1"], ["candidate-1"]);

        assert_eq!(evaluation.items[0].served_rank, 1);
        assert!(evaluation.items[0].selected);
        assert_eq!(evaluation.items[0].selection_probability, 1.0);
        assert!(!evaluation.items[0].exploration);
    }
}

//! Frozen, decision-bound attention delivery contracts.
//!
//! A root decision owns one complete, ordered canonical lane. Pagination is a
//! deterministic materialization of that root and never invokes the policy.

use serde::{Deserialize, Serialize};

use super::{AttentionDecisionContext, AttentionDecisionItem, AttentionSurface};
use crate::config::AttentionBanditMode;

pub const ATTENTION_DELIVERY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttentionDeliveryStatus {
    Succeeded,
    BaselineFallback,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttentionDeliveryRefreshReason {
    Expired,
    ProjectionDrift,
    RevisionDrift,
    ScopeMismatch,
    BindingMismatch,
    CursorNotFound,
}

impl AttentionDeliveryRefreshReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::ProjectionDrift => "projection_drift",
            Self::RevisionDrift => "revision_drift",
            Self::ScopeMismatch => "scope_mismatch",
            Self::BindingMismatch => "binding_mismatch",
            Self::CursorNotFound => "cursor_not_found",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionDeliveryRootDecision {
    pub decision_id: String,
    pub lane: AttentionSurface,
    pub projection_id: String,
    pub universe_digest: String,
    /// Compact source-lane identity for bounded cursor drift checks. `None`
    /// retains compatibility with roots created before this binding existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_generation_token: Option<String>,
    pub policy_snapshot_id: Option<String>,
    pub policy_model_version: Option<String>,
    pub posterior_version: u64,
    pub seed_identity: String,
    pub universe_size: usize,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttentionDeliveryPage {
    pub delivery_id: String,
    pub page_index: usize,
    pub page_start: usize,
    pub page_size: usize,
    pub cursor: Option<String>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionDeliveryHealth {
    pub bandit_mode: AttentionBanditMode,
    pub canary_assigned: bool,
    pub applied: bool,
    pub baseline_preserved: bool,
    pub complete_universe_recorded: bool,
    pub propensity_coverage: f64,
    pub degradation_reason: Option<String>,
    pub root_sample_count: usize,
    pub delivered_count: usize,
    pub remaining_count: usize,
    pub exact_revision_match: bool,
    pub replay: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionDeliveryLedgerHealth {
    pub schema_version: u32,
    pub active_root_decisions: u64,
    pub expired_root_decisions: u64,
    pub active_pages: u64,
    pub active_cursors: u64,
    pub delivery_bound_impressions: u64,
    pub root_propensity_coverage: f64,
    pub next_expiry_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FrozenAttentionDeliveryItem {
    pub position: usize,
    pub candidate_id: String,
    pub source_revision: Option<String>,
    pub root_policy_propensity: f64,
    pub conditional_delivery_propensity: f64,
    pub exposure_token: String,
    /// Serialized API-owned canonical item. Keeping this opaque in the domain
    /// layer avoids a dependency from learning storage back into the API.
    pub item_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FrozenAttentionDelivery {
    pub status: AttentionDeliveryStatus,
    pub fallback_reason: Option<String>,
    pub root_decision: AttentionDeliveryRootDecision,
    pub page: AttentionDeliveryPage,
    pub items: Vec<FrozenAttentionDeliveryItem>,
    pub health: AttentionDeliveryHealth,
    pub min_visible_ms: u64,
    pub visibility_rule_version: String,
}

#[derive(Debug, Clone)]
pub struct AttentionDeliveryCandidate {
    pub candidate_id: String,
    pub source_revision: Option<String>,
    pub root_policy_propensity: f64,
    pub item_json: String,
    pub attribution_item_json: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CreateAttentionDelivery {
    pub status: AttentionDeliveryStatus,
    pub fallback_reason: Option<String>,
    pub root_decision: AttentionDeliveryRootDecision,
    pub page_size: usize,
    pub ordered_items: Vec<AttentionDeliveryCandidate>,
    pub health: AttentionDeliveryHealth,
    /// Exact immutable policy artifact used by the root sampler. Baseline
    /// roots store `None`; enabled roots never re-read a mutable alias.
    pub policy_snapshot_json: Option<String>,
    pub min_visible_ms: u64,
    pub visibility_rule_version: String,
    pub context: AttentionDecisionContext,
}

#[derive(Debug, Clone)]
pub struct AttentionDeliveryOrderItem {
    pub candidate_id: String,
    pub source_revision: Option<String>,
    pub root_policy_propensity: f64,
    pub attribution_item: Option<AttentionDecisionItem>,
}

#[derive(Debug, Clone)]
pub struct AttentionDeliveryOrderPlan {
    pub status: AttentionDeliveryStatus,
    pub fallback_reason: Option<String>,
    pub decision_id: String,
    pub policy_snapshot_id: Option<String>,
    pub policy_model_version: Option<String>,
    pub posterior_version: u64,
    pub seed_identity: String,
    pub ordered_items: Vec<AttentionDeliveryOrderItem>,
    pub health: AttentionDeliveryHealth,
    pub policy_snapshot_json: Option<String>,
    pub context: AttentionDecisionContext,
}

#[derive(Debug, thiserror::Error)]
pub enum AttentionDeliveryReadError {
    #[error("attention delivery refresh required: {0:?}")]
    RefreshRequired(AttentionDeliveryRefreshReason),
    #[error(transparent)]
    Storage(#[from] anyhow::Error),
}

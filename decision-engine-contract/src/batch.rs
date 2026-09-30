//! Classification batches. Item identities are opaque and never contain memory text.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::request::{DecisionResponse, DecisionState};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationMode {
    #[default]
    Shadow,
    Gate,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BatchRequest {
    #[serde(default)]
    pub mode: ClassificationMode,
    #[serde(default)]
    pub reference_version: String,
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub expected_policy_revision: String,
    #[serde(default)]
    pub projection_version: String,
    #[serde(default)]
    pub execution_budget_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<DecisionState>,
    /// Empty preserves the single-state request within contract v5.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<DecisionItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionItem {
    pub item_id: String,
    pub state: DecisionState,
    #[serde(default)]
    pub choice_candidates: BTreeMap<String, Vec<(String, String)>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Answered,
    Failed,
    NoFittingModel,
    Cancelled,
    NotStarted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionItemResult {
    /// Questions accepted by confidence and either reviewed qualification or
    /// explicit operator activation. Values identify evidence or operator provenance.
    /// Empty means observation only, even when the operation gate is enabled.
    #[serde(default)]
    pub eligible_answers: BTreeMap<String, String>,
    pub item_id: String,
    pub status: ItemStatus,
    pub response: Option<DecisionResponse>,
    pub thresholds: Option<BTreeMap<String, f64>>,
    pub error: Option<String>,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BatchResponse {
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub engine_instance: String,
    #[serde(default)]
    pub policy_revision: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<DecisionItemResult>,
    /// Service health is independent of whether a partial batch answered.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_health: BTreeMap<String, Option<String>>,
}

impl BatchRequest {
    pub fn validate_identity(&self) -> Result<(), &'static str> {
        if self.items.is_empty() {
            return Ok(());
        }
        if self.request_id.is_empty()
            || self.expected_policy_revision.is_empty()
            || self.projection_version.is_empty()
            || self.reference_version.is_empty()
            || self.execution_budget_ms == 0
        {
            return Err(
                "batch requires request, policy, projection, reference and execution budget",
            );
        }
        let mut ids = BTreeSet::new();
        for item in &self.items {
            if item.item_id.is_empty() || item.item_id.len() > 256 || !ids.insert(&item.item_id) {
                return Err("batch item identities must be nonempty, bounded and unique");
            }
        }
        if self.request_id.len() > 256
            || self.expected_policy_revision.len() > 256
            || self.projection_version.len() > 256
            || self.reference_version.len() > 256
        {
            return Err("batch identity exceeds limit");
        }
        Ok(())
    }
}

impl BatchResponse {
    /// Exact membership and ordering prevent a stale or reordered answer from
    /// being applied to the wrong candidate. Rejected batches still echo items.
    pub fn matches(&self, request: &BatchRequest) -> bool {
        if request.items.is_empty() {
            return self.items.is_empty();
        }
        request.validate_identity().is_ok()
            && self.request_id == request.request_id
            && !self.engine_instance.is_empty()
            && self.policy_revision == request.expected_policy_revision
            && self.items.len() == request.items.len()
            && self.items.iter().zip(&request.items).all(|(result, item)| {
                result.item_id == item.item_id
                    && (result.status == ItemStatus::Answered) == result.response.is_some()
            })
    }
}

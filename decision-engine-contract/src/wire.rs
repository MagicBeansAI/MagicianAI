//! Endpoint bodies.
//!
//! - `POST /v1/decide` — [`DecideRequest`] → [`DecideResponse`]
//! - `GET /v1/operations` — [`OperationsResponse`]
//! - `GET /health` — [`HealthResponse`]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::request::{DecisionResponse, DecisionState};

/// Major version of this contract. Version 5 carries memory authorization and
/// observation/batching policy in engine discovery.
/// The shared action client rejects a server whose major version differs.
pub const CONTRACT_VERSION: u32 = 5;

pub const DECIDE_PATH: &str = "/v1/decide";
pub const OPERATIONS_PATH: &str = "/v1/operations";
pub const HEALTH_PATH: &str = "/health";

/// Where the host is running. In local mode an operation that sees user
/// content only reaches models on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Locality {
    Local,
    #[default]
    Cloud,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecideRequest {
    #[serde(flatten, default)]
    pub batch: crate::batch::BatchRequest,
    pub contract_version: u32,
    pub operation: String,
    pub state: DecisionState,
    /// Per-request Choice options: question id → `(option id, label)`.
    #[serde(default)]
    pub choice_candidates: BTreeMap<String, Vec<(String, String)>>,
    #[serde(default)]
    pub locality: Locality,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecideStatus {
    Answered,
    /// Not configured, or a contract mismatch: keep the incumbent path.
    Unbound,
    /// Every model on the route was skipped or failed: keep the incumbent.
    NoFittingModel,
    /// The request or the answer was invalid.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecideResponse {
    #[serde(flatten, default)]
    pub batch: crate::batch::BatchResponse,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_calls: Vec<crate::telemetry::DecisionModelCall>,
    pub contract_version: u32,
    pub status: DecideStatus,
    pub response: Option<DecisionResponse>,
    /// Thresholds owned by the model that answered; `None` = never gate.
    pub thresholds: Option<BTreeMap<String, f64>>,
    pub error: Option<String>,
    pub latency_ms: u64,
}

/// One operation's rollout policy as the engine holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationPolicy {
    #[serde(default)]
    pub classification: crate::classification::ClassificationPolicy,
    pub name: String,
    pub shadow: bool,
    pub gate: bool,
    pub max_consecutive_steps: u32,
    pub sees_body: bool,
    /// Model entry names this operation routes across, per locality.
    pub route_local: Vec<String>,
    pub route_cloud: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationsResponse {
    #[serde(default)]
    pub engine_instance: String,
    #[serde(default)]
    pub policy_revision: String,
    pub contract_version: u32,
    /// Active shared action contract, absent on older or disabled engines.
    /// A configured operation alone does not prove that the endpoint is active.
    #[serde(default)]
    pub action_contract_version: Option<u32>,
    pub operations: Vec<OperationPolicy>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub contract_version: u32,
    pub status: String,
    pub engine_version: String,
}

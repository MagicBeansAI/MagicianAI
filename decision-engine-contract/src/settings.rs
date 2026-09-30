//! Owner-facing routing settings. Model credentials never cross this API.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SETTINGS_PATH: &str = "/v1/settings/routing";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRoute {
    pub primary: String,
    #[serde(default)]
    pub backup: Option<String>,
}
impl ModelRoute {
    pub fn names(&self) -> Vec<String> {
        std::iter::once(self.primary.clone())
            .chain(self.backup.clone())
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalityRoutes {
    pub local: ModelRoute,
    pub cloud: ModelRoute,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingModel {
    pub name: String,
    pub model: String,
    pub remote: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationRouting {
    pub name: String,
    /// None for a legacy tier with more than two entries: do not truncate it.
    pub routing: Option<LocalityRoutes>,
    pub configured_local: Vec<String>,
    pub configured_cloud: Vec<String>,
    pub active_local: Vec<String>,
    pub active_cloud: Vec<String>,
    pub allow_remote_when_local: bool,
    pub threshold_names: Vec<String>,
    pub thresholds_by_model: BTreeMap<String, BTreeMap<String, f64>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingSettings {
    pub contract_version: u32,
    /// Optimistic concurrency token for the complete settings file.
    pub revision: String,
    pub pending: bool,
    pub enabled: bool,
    pub models: Vec<RoutingModel>,
    pub operations: Vec<OperationRouting>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingUpdate {
    pub revision: String,
    pub operation: String,
    pub routing: LocalityRoutes,
    pub allow_remote_when_local: bool,
    pub thresholds_by_model: BTreeMap<String, BTreeMap<String, f64>>,
}

//! Observe-connector settings shared by the channel-assist data plane and
//! the observe HTTP surface. Extracted from `api::observe_connectors_api`
//! (api-crate extraction prerequisite); the api file re-exports it.

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::artifacts::durable_store::{
    open_local_durable_artifacts, DurableArtifactStore, DurableFrontmatter,
};
use crate::magician_v2::channel_types::ChannelLane;

use serde::{Deserialize, Serialize};

/// Per-producer consent + cadence config. One shape for every account-based
/// connector; the producer is the durable namespace (`email` | `calendar`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObserveConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Account aliases (from the discovery endpoint) the user chose to observe.
    #[serde(default)]
    pub accounts: Vec<String>,
    /// `daily` | `twice-daily` | `hourly`.
    #[serde(default = "default_frequency")]
    pub frequency: String,
    /// Local `HH:MM` the daily/twice-daily run fires at.
    #[serde(default = "default_time")]
    pub time: String,
    /// Suppress affirmatively-sensitive items from reviews (defaults on; email).
    #[serde(default = "default_true")]
    pub suppress_sensitive: bool,
    #[serde(default)]
    pub total_synced: u64,
    #[serde(default)]
    pub last_sync_at: Option<String>,
    /// The scheduled writer task created for this producer (None until enabled).
    #[serde(default)]
    pub schedule_task_id: Option<String>,
}

pub fn default_frequency() -> String {
    "daily".to_string()
}

pub fn default_true() -> bool {
    true
}

pub fn default_time() -> String {
    "07:00".to_string()
}

/// Read a producer's legacy `{producer}_observe` consent config for a scope
/// (`email` | `calendar`). Missing/unreadable is the default (disabled) —
/// never an error. This is the migration seam the unified `channel_observe`
/// config reads from (see `channel_assist::channel_observe::load_or_migrate`).
pub async fn load_producer_observe_config(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    producer: &str,
) -> ObserveConfig {
    let store = match open_local_durable_artifacts(workspace_layout, principal, workspace) {
        Ok(store) => store,
        Err(_) => return ObserveConfig::default(),
    };
    load_config(&store, &observe_namespace(producer)).await
}

/// Durable namespace for a producer's config (`email_observe` | `calendar_observe`).
pub fn observe_namespace(producer: &str) -> String {
    format!("{producer}_observe")
}

pub async fn load_config(store: &DurableArtifactStore, namespace: &str) -> ObserveConfig {
    match store.read(namespace, CONFIG_NAME).await {
        Ok((_, body)) => serde_json::from_str(&body).unwrap_or_default(),
        Err(_) => ObserveConfig::default(),
    }
}

pub const CONFIG_NAME: &str = "config.json";

impl Default for ObserveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            accounts: Vec::new(),
            frequency: default_frequency(),
            time: default_time(),
            suppress_sensitive: true,
            total_synced: 0,
            last_sync_at: None,
            schedule_task_id: None,
        }
    }
}

/// The persisted unified config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelObserveConfig {
    #[serde(default = "default_true")]
    pub suppress_sensitive: bool,
    #[serde(default = "default_history_lookback_days")]
    pub history_lookback_days: u32,
    #[serde(default)]
    pub cadence: ObserveCadence,
    #[serde(default)]
    pub channels: Vec<ChannelEntry>,
}

/// Durable namespace holding the unified config (sibling of the retired
/// `email_observe` / `channel_assist` namespaces).
pub const CHANNEL_OBSERVE_NAMESPACE: &str = "channel_observe";

/// The `email` channel resolves to the `gmail` provider; the `calendar`
/// channel stays on the digest and is never a message provider.
pub const CALENDAR_CHANNEL: &str = "calendar";

pub const DEFAULT_HISTORY_LOOKBACK_DAYS: u32 = 7;

pub fn default_history_lookback_days() -> u32 {
    DEFAULT_HISTORY_LOOKBACK_DAYS
}

/// Digest/back-compat cadence — drives the calendar (and, until U3, email)
/// scheduled writer task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObserveCadence {
    #[serde(default = "default_frequency")]
    pub frequency: String,
    #[serde(default = "default_time")]
    pub time: String,
}

impl Default for ObserveCadence {
    fn default() -> Self {
        Self {
            frequency: default_frequency(),
            time: default_time(),
        }
    }
}

/// One observed account. `channel` is the user-facing channel name;
/// `account` is the provider-local alias (gws profile / wu.db owner / Kapso
/// number / calendar alias); `lane` tags every row it produces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelEntry {
    pub channel: String,
    pub account: String,
    #[serde(default)]
    pub lane: ChannelLane,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Purposes the owner granted this account beyond observation (secure
    /// HITL P6): `verification_codes` lets the resolver read it for a live
    /// verification challenge. Observation consent alone grants none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub purposes: Vec<String>,
}

/// The purpose that permits automatic verification-code retrieval from an
/// account (secure HITL plan §6.2).
pub const VERIFICATION_CODES_PURPOSE: &str = "verification_codes";

impl ChannelEntry {
    pub fn has_purpose(&self, purpose: &str) -> bool {
        self.purposes.iter().any(|p| p == purpose)
    }
}

impl Default for ChannelObserveConfig {
    fn default() -> Self {
        Self {
            suppress_sensitive: true,
            history_lookback_days: DEFAULT_HISTORY_LOOKBACK_DAYS,
            cadence: ObserveCadence::default(),
            channels: Vec::new(),
        }
    }
}

/// Read the unified config for a scope, or `None` if unset/unreadable (never
/// an error — the caller decides whether to migrate).
pub async fn read_channel_observe(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> Option<ChannelObserveConfig> {
    let store = open_local_durable_artifacts(workspace_layout, principal, workspace).ok()?;
    match store.read(CHANNEL_OBSERVE_NAMESPACE, CONFIG_NAME).await {
        Ok((_, body)) => serde_json::from_str(&body).ok(),
        Err(_) => None,
    }
}

pub async fn write_channel_observe(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    config: &ChannelObserveConfig,
) -> anyhow::Result<()> {
    let store = open_local_durable_artifacts(workspace_layout, principal, workspace)?;
    let body = serde_json::to_string_pretty(config)?;
    let frontmatter = DurableFrontmatter {
        namespace: CHANNEL_OBSERVE_NAMESPACE.to_string(),
        name: CONFIG_NAME.to_string(),
        created_by: "channel_observe".to_string(),
        last_updated_by: "channel_observe".to_string(),
        last_updated: chrono::Utc::now(),
        content_type: Some("application/json".to_string()),
        source_execution_id: None,
        source_task_id: None,
        source_workflow_instance_id: None,
        source_run_id: None,
        source_cycle_id: None,
        source_agent_id: Some(principal.to_string()),
        producer_stage: Some("channel_observe_config".to_string()),
    };
    store
        .write(CHANNEL_OBSERVE_NAMESPACE, CONFIG_NAME, &body, frontmatter)
        .await?;
    Ok(())
}

//! Tier record serialization shape.
//!
//! Moved here from `magician::magician_v2::artifact_v2::memory` so that the
//! memory-candidate / memory-index code in this crate can deserialize tier
//! JSON without re-importing magician types. magician keeps a `pub use`
//! re-export so existing imports continue to work.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::memory_tiers::TierScope;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V3MemoryTierRecord {
    #[serde(default = "default_v3_memory_tier_schema_version")]
    pub schema_version: String,
    #[serde(default = "default_v3_memory_tier_record_type")]
    pub record_type: String,
    #[serde(default)]
    pub principal: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    pub tier_name: String,
    pub tier_scope: TierScope,
    #[serde(default)]
    pub goal_id: Option<String>,
    pub last_updated: DateTime<Utc>,
    #[serde(default)]
    pub fields: HashMap<String, Value>,
}

impl V3MemoryTierRecord {
    pub fn new(
        tier_name: impl Into<String>,
        tier_scope: TierScope,
        goal_id: Option<&str>,
        principal: Option<&str>,
        workspace: Option<&str>,
        agent_id: Option<&str>,
    ) -> Self {
        Self {
            schema_version: default_v3_memory_tier_schema_version(),
            record_type: default_v3_memory_tier_record_type(),
            principal: principal.map(str::to_string),
            workspace: workspace.map(str::to_string),
            agent_id: agent_id.map(str::to_string),
            tier_name: tier_name.into(),
            tier_scope,
            goal_id: goal_id.map(str::to_string),
            last_updated: Utc::now(),
            fields: HashMap::new(),
        }
    }
}

fn default_v3_memory_tier_schema_version() -> String {
    "v3_memory_tier/v1".to_string()
}

fn default_v3_memory_tier_record_type() -> String {
    "memory_tier".to_string()
}

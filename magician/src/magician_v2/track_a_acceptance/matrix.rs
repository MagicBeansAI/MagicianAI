//! Published capability-support matrix for Tier 2 and device-local owners.

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteOutcome {
    RegenerableLocalProjection,
    Unavailable,
    Provided,
}

#[derive(Debug, Deserialize)]
pub struct SupportMatrix {
    pub profile: String,
    pub accepted_at: String,
    pub owners: Vec<SupportEntry>,
}

#[derive(Debug, Deserialize)]
pub struct SupportEntry {
    pub id: String,
    pub tier: serde_yaml::Value,
    pub remote_outcome: RemoteOutcome,
    pub rebuild_sources: Vec<String>,
    pub watermark: String,
    pub degraded_behavior: String,
}

impl SupportEntry {
    pub fn tier_label(&self) -> String {
        match &self.tier {
            serde_yaml::Value::Number(n) => n.to_string(),
            serde_yaml::Value::String(s) => s.clone(),
            other => format!("{other:?}"),
        }
    }
}

pub fn matrix_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/components/magician-storage/capability-support-matrix.yaml")
}

pub fn load_support_matrix() -> anyhow::Result<SupportMatrix> {
    let text = std::fs::read_to_string(matrix_path())?;
    Ok(serde_yaml::from_str(&text)?)
}

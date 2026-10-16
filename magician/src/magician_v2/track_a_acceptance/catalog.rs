//! Load the YAML catalog (source of truth) for Track A inventory checks.

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct CatalogFile {
    pub measurements: CatalogMeasurements,
    pub owners: Vec<CatalogOwner>,
}

#[derive(Debug, Deserialize)]
pub struct CatalogMeasurements {
    pub owner_count: u64,
    pub tier1_count: u64,
    pub tier2_count: u64,
    pub device_local_count: u64,
}

#[derive(Debug, Deserialize)]
pub struct CatalogOwner {
    pub id: String,
    pub tier: serde_yaml::Value,
    pub class: String,
    pub readiness: CatalogReadiness,
    pub authority: CatalogAuthority,
    pub legacy_source: CatalogLegacy,
}

#[derive(Debug, Deserialize)]
pub struct CatalogReadiness {
    pub state: String,
    #[serde(default)]
    pub evidence: serde_yaml::Mapping,
}

#[derive(Debug, Deserialize)]
pub struct CatalogAuthority {
    pub state: String,
    pub canonical_profile: String,
}

#[derive(Debug, Deserialize)]
pub struct CatalogLegacy {
    pub state: String,
}

impl CatalogOwner {
    pub fn tier_label(&self) -> String {
        match &self.tier {
            serde_yaml::Value::Number(n) => n.to_string(),
            serde_yaml::Value::String(s) => s.clone(),
            other => format!("{other:?}"),
        }
    }

    pub fn is_tier1(&self) -> bool {
        self.tier_label() == "1"
    }

    pub fn is_tier2(&self) -> bool {
        self.tier_label() == "2"
    }

    pub fn is_device_local(&self) -> bool {
        self.tier_label() == "device_local"
    }
}

pub fn catalog_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/components/magician/storage-catalog.yaml")
}

pub fn load_catalog() -> anyhow::Result<CatalogFile> {
    let text = std::fs::read_to_string(catalog_path())?;
    Ok(serde_yaml::from_str(&text)?)
}

//! Neutral migration record and fenced phase graph (plan §15).

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::StorageError;
use crate::identifiers::{LogicalObjectId, StorageScope};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct StorageCatalogId(String);

impl StorageCatalogId {
    pub fn parse(raw: &str) -> Result<Self, StorageError> {
        LogicalObjectId::parse(raw).map(|id| Self(id.as_str().to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for StorageCatalogId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for StorageCatalogId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Explicit phases. Unknown values fail closed at parse/deserialize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationPhase {
    Planned,
    Exporting,
    Imported,
    Verified,
    CutoverPending,
    Cutover,
    RollbackPending,
    RolledBack,
    Settled,
}

pub const MIGRATION_PHASES: [MigrationPhase; 9] = [
    MigrationPhase::Planned,
    MigrationPhase::Exporting,
    MigrationPhase::Imported,
    MigrationPhase::Verified,
    MigrationPhase::CutoverPending,
    MigrationPhase::Cutover,
    MigrationPhase::RollbackPending,
    MigrationPhase::RolledBack,
    MigrationPhase::Settled,
];

impl MigrationPhase {
    pub fn parse(raw: &str) -> Result<Self, StorageError> {
        raw.parse()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Exporting => "exporting",
            Self::Imported => "imported",
            Self::Verified => "verified",
            Self::CutoverPending => "cutover_pending",
            Self::Cutover => "cutover",
            Self::RollbackPending => "rollback_pending",
            Self::RolledBack => "rolled_back",
            Self::Settled => "settled",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Settled)
    }
}

impl FromStr for MigrationPhase {
    type Err = StorageError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        for phase in MIGRATION_PHASES {
            if phase.as_str() == raw {
                return Ok(phase);
            }
        }
        Err(StorageError::invalid_key("unknown migration phase"))
    }
}

impl fmt::Display for MigrationPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationCounts {
    pub records: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationDigests {
    pub payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceWatermark {
    pub generation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageMigrationRecord {
    pub migration_id: Uuid,
    pub run_id: Uuid,
    pub store_id: StorageCatalogId,
    pub scope: StorageScope,
    pub source_profile: String,
    pub target_profile: String,
    pub source_generation: String,
    pub phase: MigrationPhase,
    pub counts: MigrationCounts,
    pub digests: MigrationDigests,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub cutover_fencing_generation: Option<u64>,
}

/// Legal fenced transitions. Same-phase retry is a no-op, not a transition.
#[allow(clippy::match_like_matches_macro)]
pub fn legal_transition(from: Option<MigrationPhase>, to: MigrationPhase) -> bool {
    use MigrationPhase::*;
    match (from, to) {
        (None, Planned) => true,
        (Some(Planned), Exporting) => true,
        (Some(Exporting), Imported) => true,
        (Some(Imported), Verified) => true,
        (Some(Verified), CutoverPending) => true,
        (Some(CutoverPending), Cutover) => true,
        (Some(Cutover), Settled) => true,
        (
            Some(Exporting) | Some(Imported) | Some(Verified) | Some(CutoverPending)
            | Some(Cutover),
            RollbackPending,
        ) => true,
        (Some(RollbackPending), RolledBack) => true,
        (Some(RolledBack), Settled) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_phase_fails_closed() {
        assert!(MigrationPhase::parse("copying").is_err());
        let err = serde_json::from_str::<MigrationPhase>("\"copying\"").unwrap_err();
        assert!(err.is_data());
    }

    #[test]
    fn catalog_id_rejects_path() {
        assert!(StorageCatalogId::parse("a/b").is_err());
        assert!(StorageCatalogId::parse("synthetic_owner").is_ok());
    }

    #[test]
    fn cannot_skip_from_planned_to_verified() {
        assert!(!legal_transition(
            Some(MigrationPhase::Planned),
            MigrationPhase::Verified
        ));
        assert!(legal_transition(None, MigrationPhase::Planned));
        assert!(legal_transition(
            Some(MigrationPhase::Verified),
            MigrationPhase::RollbackPending
        ));
        assert!(!legal_transition(
            Some(MigrationPhase::Planned),
            MigrationPhase::RollbackPending
        ));
        assert!(!legal_transition(
            Some(MigrationPhase::Settled),
            MigrationPhase::Cutover
        ));
    }
}

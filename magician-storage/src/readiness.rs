//! Catalog readiness, authority, and evidence schema (plan §5.12).

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::StorageError;

/// Monotonic Track A preparation states. Rollback never moves this backward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessState {
    Discovered,
    Characterized,
    WrappedLocal,
    CallersRouted,
    BypassGuarded,
    RemoteImplemented,
    RemoteConformant,
    MigrationQualified,
    RestoreQualified,
    RemoteReady,
}

/// Runtime authority. Independent of Track A readiness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityState {
    LocalActive,
    MigrationInProgress,
    RemoteActive,
    RollbackInProgress,
    LocalRolledBack,
}

/// Whether the pre-cutover source is still retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacySourceState {
    Retained,
    RetirementEligible,
    Retired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Test,
    Fixture,
    Adapter,
    Report,
    Commit,
    Guard,
}

/// Machine-checkable evidence pointer. Locators are paths, commits, or reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceLink {
    pub kind: EvidenceKind,
    pub locator: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub const READINESS_STATES: [ReadinessState; 10] = [
    ReadinessState::Discovered,
    ReadinessState::Characterized,
    ReadinessState::WrappedLocal,
    ReadinessState::CallersRouted,
    ReadinessState::BypassGuarded,
    ReadinessState::RemoteImplemented,
    ReadinessState::RemoteConformant,
    ReadinessState::MigrationQualified,
    ReadinessState::RestoreQualified,
    ReadinessState::RemoteReady,
];

const EVIDENCE_KEYS: [&str; 9] = [
    "characterized",
    "wrapped_local",
    "callers_routed",
    "bypass_guarded",
    "remote_implemented",
    "remote_conformant",
    "migration_qualified",
    "restore_qualified",
    "remote_ready",
];

impl ReadinessState {
    pub fn parse(raw: &str) -> Result<Self, StorageError> {
        raw.parse()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Discovered => "discovered",
            Self::Characterized => "characterized",
            Self::WrappedLocal => "wrapped_local",
            Self::CallersRouted => "callers_routed",
            Self::BypassGuarded => "bypass_guarded",
            Self::RemoteImplemented => "remote_implemented",
            Self::RemoteConformant => "remote_conformant",
            Self::MigrationQualified => "migration_qualified",
            Self::RestoreQualified => "restore_qualified",
            Self::RemoteReady => "remote_ready",
        }
    }

    pub fn rank(self) -> usize {
        READINESS_STATES
            .iter()
            .position(|state| *state == self)
            .expect("readiness enum is closed")
    }

    /// Keys required to occupy this state. `discovered` needs none; later
    /// states require every predecessor key plus their own.
    pub fn required_evidence_keys(self) -> &'static [&'static str] {
        &EVIDENCE_KEYS[..self.rank()]
    }

    pub fn successor(self) -> Option<Self> {
        READINESS_STATES.get(self.rank() + 1).copied()
    }
}

impl FromStr for ReadinessState {
    type Err = StorageError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        for state in READINESS_STATES {
            if state.as_str() == raw {
                return Ok(state);
            }
        }
        Err(StorageError::invalid_key("unknown readiness state"))
    }
}

impl fmt::Display for ReadinessState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AuthorityState {
    pub fn parse(raw: &str) -> Result<Self, StorageError> {
        match raw {
            "local_active" => Ok(Self::LocalActive),
            "migration_in_progress" => Ok(Self::MigrationInProgress),
            "remote_active" => Ok(Self::RemoteActive),
            "rollback_in_progress" => Ok(Self::RollbackInProgress),
            "local_rolled_back" => Ok(Self::LocalRolledBack),
            _ => Err(StorageError::invalid_key("unknown authority state")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalActive => "local_active",
            Self::MigrationInProgress => "migration_in_progress",
            Self::RemoteActive => "remote_active",
            Self::RollbackInProgress => "rollback_in_progress",
            Self::LocalRolledBack => "local_rolled_back",
        }
    }
}

impl LegacySourceState {
    pub fn parse(raw: &str) -> Result<Self, StorageError> {
        match raw {
            "retained" => Ok(Self::Retained),
            "retirement_eligible" => Ok(Self::RetirementEligible),
            "retired" => Ok(Self::Retired),
            _ => Err(StorageError::invalid_key("unknown legacy_source state")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Retained => "retained",
            Self::RetirementEligible => "retirement_eligible",
            Self::Retired => "retired",
        }
    }
}

impl EvidenceLink {
    pub fn new(kind: EvidenceKind, locator: impl Into<String>) -> Result<Self, StorageError> {
        let locator = locator.into();
        if locator.trim().is_empty() {
            return Err(StorageError::invalid_key("empty evidence locator"));
        }
        Ok(Self {
            kind,
            locator,
            note: None,
        })
    }
}

pub fn validate_readiness_evidence(
    state: ReadinessState,
    evidence: &BTreeMap<String, EvidenceLink>,
) -> Result<(), StorageError> {
    for key in state.required_evidence_keys() {
        match evidence.get(*key) {
            Some(link) if !link.locator.trim().is_empty() => {},
            _ => {
                return Err(StorageError::invalid_key(format!(
                    "readiness {state} missing evidence {key}"
                )));
            },
        }
    }
    Ok(())
}

/// Track A advances one state at a time. Skipping is a ledger error.
pub fn can_advance_readiness(from: ReadinessState, to: ReadinessState) -> bool {
    from.successor() == Some(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_readiness_fails_closed() {
        assert!(ReadinessState::parse("almost_ready").is_err());
        assert!(AuthorityState::parse("canonical").is_err());
        assert!(LegacySourceState::parse("deleted").is_err());
    }

    #[test]
    fn discovered_needs_no_evidence_keys() {
        assert!(ReadinessState::Discovered
            .required_evidence_keys()
            .is_empty());
        validate_readiness_evidence(ReadinessState::Discovered, &BTreeMap::new()).unwrap();
    }

    #[test]
    fn characterized_requires_its_key() {
        let err = validate_readiness_evidence(ReadinessState::Characterized, &BTreeMap::new())
            .unwrap_err();
        assert!(matches!(err, StorageError::InvalidKey { .. }));
        let mut evidence = BTreeMap::new();
        evidence.insert(
            "characterized".into(),
            EvidenceLink::new(EvidenceKind::Test, "tests/char.rs").unwrap(),
        );
        validate_readiness_evidence(ReadinessState::Characterized, &evidence).unwrap();
    }

    #[test]
    fn remote_ready_requires_every_predecessor_key() {
        let mut evidence = BTreeMap::new();
        for key in ReadinessState::RemoteReady.required_evidence_keys() {
            evidence.insert(
                (*key).to_string(),
                EvidenceLink::new(EvidenceKind::Test, format!("tests/{key}.rs")).unwrap(),
            );
        }
        validate_readiness_evidence(ReadinessState::RemoteReady, &evidence).unwrap();
        evidence.remove("restore_qualified");
        assert!(validate_readiness_evidence(ReadinessState::RemoteReady, &evidence).is_err());
    }

    #[test]
    fn readiness_cannot_skip() {
        assert!(can_advance_readiness(
            ReadinessState::Discovered,
            ReadinessState::Characterized
        ));
        assert!(!can_advance_readiness(
            ReadinessState::Discovered,
            ReadinessState::WrappedLocal
        ));
        assert!(!can_advance_readiness(
            ReadinessState::RemoteReady,
            ReadinessState::Characterized
        ));
    }
}

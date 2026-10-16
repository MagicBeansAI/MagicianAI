//! `intention.v1`: a read-only view of the owner's preferences, goals and
//! standing instructions, projected from the canonical stores that own them.
//!
//! This is a view, never a second store. Canonical memory (`knowledge.json`
//! tiers, the Notes-provider taste profile, app memory destinations) stays the
//! authority; adapters build these values on read and nothing here is written
//! back. Missing metadata stays unknown rather than being backfilled, and an
//! adapter may only *lower* the authority it reads (P0 contracts §6.2) — never
//! raise it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const INTENTION_SCHEMA_VERSION: &str = "intention.v1";

/// What an intention is, in general terms. It does not by itself decide how
/// hard Magician intervenes; [`AssertionStatus`] and the owner's policy do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentionKind {
    /// A liking, habit or style ("prefers vegetarian food").
    Taste,
    /// Something the owner is working toward ("eat healthier this month").
    Goal,
    /// An instruction about how Magician should act ("check with me before
    /// ordering meat for me").
    StandingInstruction,
}

/// How the intention came to be known. This decides the force it may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertionStatus {
    /// The owner said it, verified against an owner-role turn.
    DirectOwnerStatement,
    /// The owner confirmed an interpretation (the confirm endpoint, an
    /// approved taste proposal, a conflict answer).
    Confirmed,
    /// Inferred from conversation or behavior. Most knowledge is here.
    Inferred,
    /// Inferred from an untrusted capture (meeting, screen).
    InferredUntrusted,
    /// Provenance cannot be established (a profile line with no record of
    /// who wrote it).
    UnknownProvenance,
}

impl AssertionStatus {
    /// Whether this evidence alone may hold an act awaiting confirmation.
    /// Owner direction 2026-10-09: enforcement force comes from what the
    /// owner explicitly said; inferences inform but never hold.
    pub fn may_hold(self) -> bool {
        matches!(self, Self::DirectOwnerStatement | Self::Confirmed)
    }

    /// Whether this evidence may present a choice (a wait) under a
    /// choice-based owner policy, for a material conflict.
    pub fn may_offer_choice(self) -> bool {
        matches!(
            self,
            Self::DirectOwnerStatement | Self::Confirmed | Self::Inferred
        )
    }
}

/// Lifecycle states an intention can be in while it is eligible for
/// comparison. Retired records (superseded, retracted, expired, replaced) are
/// not intentions and never appear in the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentionLifecycle {
    Active,
    /// Saved but not yet reviewed. Eligible for alignment even though prompt
    /// ranking hides it: an instruction saved a minute ago governs the next
    /// act (P0 contracts §6.2).
    PendingReview,
    /// Disputed by a conflicting record; presented with that uncertainty.
    Unresolved,
}

/// The canonical owner of an intention. Writes go back through this owner,
/// never through the view.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "store", rename_all = "snake_case")]
pub enum IntentionSource {
    /// A tier of native user knowledge (`memory/users/knowledge.json`).
    NativeTier { tier: String },
    /// The owner-edited taste profile note. v1 never writes it (plan §5.3).
    TasteProfile,
    /// A governed app memory destination.
    AppMemory { destination: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRole {
    Owner,
    Assistant,
    Tool,
    External,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub source_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    pub role: EvidenceRole,
}

/// The owner's chosen intervention strength for an intention or globally.
/// Absent means the reviewed defaults (P0 contracts §8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterventionPolicy {
    /// Mention the tradeoff and continue.
    Advisory,
    /// Offer a choice and wait for a material conflict.
    ChoiceForMaterial,
    /// Wait for confirmation before acting against it.
    ConfirmBeforeException,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Intention {
    pub schema_version: String,
    /// Stable identity from the canonical owner (`memory_record_id`, a
    /// profile line id, an app record id). Host-supplied, never model-minted.
    pub id: String,
    /// Source-sensitive version (content hash or revision) of this record.
    pub revision: String,
    pub source: IntentionSource,
    pub kind: IntentionKind,
    pub assertion_status: AssertionStatus,
    /// The complete statement, negation and conditions preserved.
    pub statement: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<String>,
    /// Trusted structural scope (`MemoryScope`) where the record has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Value>,
    pub lifecycle: IntentionLifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<EvidenceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intervention_policy: Option<InterventionPolicy>,
}

/// Every source generation a captured assessment depends on. A change in any
/// of them invalidates the assessment (P0 contracts §6.3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StandingGeneration {
    /// Durable per-owner counter, bumped on every write to native user
    /// knowledge and on forgetting.
    pub intention_generation: u64,
    /// BLAKE3 version of the taste profile note, `None` when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_version: Option<String>,
    /// App memory `destination_generation`, when app memory contributes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_generation: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    /// Every eligible entry in the declared scope was read and assessed.
    /// Not a claim that every implicit preference was found.
    Complete,
    /// Entries, source passages or action facts were omitted or truncated.
    Partial,
    /// A required source could not be read or validated.
    Unavailable,
}

/// What the collector actually covered. An empty, successfully read
/// collection is `Complete` with no intentions; a failed read is never empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    pub status: CoverageStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unavailable_sources: Vec<String>,
}

impl Coverage {
    pub fn complete() -> Self {
        Self {
            status: CoverageStatus::Complete,
            omitted_ids: Vec::new(),
            unavailable_sources: Vec::new(),
        }
    }

    /// Fold another lane's coverage in: the weakest status wins and the
    /// omissions accumulate.
    pub fn merge(&mut self, other: Coverage) {
        self.status = match (self.status, other.status) {
            (CoverageStatus::Unavailable, _) | (_, CoverageStatus::Unavailable) => {
                CoverageStatus::Unavailable
            },
            (CoverageStatus::Partial, _) | (_, CoverageStatus::Partial) => CoverageStatus::Partial,
            _ => CoverageStatus::Complete,
        };
        self.omitted_ids.extend(other.omitted_ids);
        self.unavailable_sources.extend(other.unavailable_sources);
    }
}

/// The structurally eligible inventory for one owner/workspace, with what was
/// covered and the generations it was read at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntentionInventory {
    pub intentions: Vec<Intention>,
    pub coverage: Coverage,
    pub generation: StandingGeneration,
}

impl IntentionInventory {
    /// `nothing_to_compare` is available only for a complete, structurally
    /// empty inventory (plan §6.1). Low similarity, a top-k cutoff or a failed
    /// read never qualify.
    pub fn is_verified_empty(&self) -> bool {
        self.coverage.status == CoverageStatus::Complete && self.intentions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_evidence_may_hold() {
        assert!(AssertionStatus::DirectOwnerStatement.may_hold());
        assert!(AssertionStatus::Confirmed.may_hold());
        assert!(!AssertionStatus::Inferred.may_hold());
        assert!(!AssertionStatus::InferredUntrusted.may_hold());
        assert!(!AssertionStatus::UnknownProvenance.may_hold());
    }

    #[test]
    fn untrusted_and_unknown_evidence_never_offer_a_choice() {
        assert!(AssertionStatus::Inferred.may_offer_choice());
        assert!(!AssertionStatus::InferredUntrusted.may_offer_choice());
        assert!(!AssertionStatus::UnknownProvenance.may_offer_choice());
    }

    #[test]
    fn coverage_merge_keeps_the_weakest_status() {
        let mut coverage = Coverage::complete();
        coverage.merge(Coverage {
            status: CoverageStatus::Partial,
            omitted_ids: vec!["a".into()],
            unavailable_sources: Vec::new(),
        });
        assert_eq!(coverage.status, CoverageStatus::Partial);
        coverage.merge(Coverage {
            status: CoverageStatus::Unavailable,
            omitted_ids: Vec::new(),
            unavailable_sources: vec!["taste_profile".into()],
        });
        assert_eq!(coverage.status, CoverageStatus::Unavailable);
        assert_eq!(coverage.omitted_ids, vec!["a".to_string()]);
        assert_eq!(
            coverage.unavailable_sources,
            vec!["taste_profile".to_string()]
        );
    }

    #[test]
    fn only_a_complete_empty_inventory_is_verified_empty() {
        let empty_complete = IntentionInventory {
            intentions: Vec::new(),
            coverage: Coverage::complete(),
            generation: StandingGeneration::default(),
        };
        assert!(empty_complete.is_verified_empty());

        let empty_failed = IntentionInventory {
            coverage: Coverage {
                status: CoverageStatus::Unavailable,
                omitted_ids: Vec::new(),
                unavailable_sources: vec!["knowledge".into()],
            },
            ..empty_complete
        };
        assert!(!empty_failed.is_verified_empty());
    }
}

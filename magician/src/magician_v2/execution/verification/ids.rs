//! Typed identifiers for the verification controller.
//!
//! Every id that can reach the filesystem as a path component is parsed
//! through a constructor that rejects anything outside `[A-Za-z0-9_-]`, so a
//! traversal sequence cannot be smuggled through an operator-supplied or
//! journal-replayed identifier. This mirrors `CodeChangeProposalId::parse`.
//!
//! `Generation` is the fencing token. It is monotonic per gate and is the only
//! thing standing between a worker whose lease expired and a commit that
//! overwrites its replacement's work.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Longest accepted identifier. Matches the proposal-store limit so the two
/// records can key on each other without one silently truncating the other.
const MAX_ID_LEN: usize = 128;

fn validate_id(raw: &str, kind: &'static str) -> Result<()> {
    if raw.is_empty() {
        return Err(anyhow!("{kind} is empty"));
    }
    if raw.len() > MAX_ID_LEN {
        return Err(anyhow!("{kind} is {} chars; max {MAX_ID_LEN}", raw.len()));
    }
    for c in raw.chars() {
        if !matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_') {
            return Err(anyhow!(
                "{kind} contains disallowed char {c:?} (only [A-Za-z0-9_-] allowed)"
            ));
        }
    }
    Ok(())
}

macro_rules! typed_id {
    ($name:ident, $prefix:literal, $kind:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(String);

        impl $name {
            pub fn new() -> Self {
                Self(format!("{}-{}", $prefix, Uuid::new_v4()))
            }

            /// Parse an externally supplied id before it is used as a path
            /// component or as a compare-and-set operand.
            pub fn parse(raw: &str) -> Result<Self> {
                validate_id(raw, $kind)?;
                Ok(Self(raw.to_string()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

typed_id!(GateId, "vgate", "verification gate id");
typed_id!(AttestationId, "vatt", "verification attestation id");
typed_id!(AttemptId, "vatm", "verification attempt id");

/// Identifies one candidate snapshot offered to a gate.
///
/// A repair produces a *new* candidate, never a mutation of the previous one
/// (§4.2), so this doubles as the staleness check at finalisation: a green
/// attestation for candidate N must not release candidate N+1.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CandidateRevision {
    /// Stable reference to the candidate's origin record (proposal id, or the
    /// child execution id for a direct-apply Autopilot run).
    pub candidate_ref: String,
    /// Monotonic counter within the gate. Starts at 1.
    pub revision: u32,
}

impl CandidateRevision {
    pub fn new(candidate_ref: impl Into<String>, revision: u32) -> Result<Self> {
        let candidate_ref = candidate_ref.into();
        validate_id(&candidate_ref, "candidate ref")?;
        if revision == 0 {
            return Err(anyhow!("candidate revision starts at 1, got 0"));
        }
        Ok(Self {
            candidate_ref,
            revision,
        })
    }

    /// The next revision for the same gate after a repair.
    pub fn next(&self, candidate_ref: impl Into<String>) -> Result<Self> {
        Self::new(candidate_ref, self.revision.saturating_add(1))
    }
}

/// Monotonic fencing token.
///
/// Incremented every time a lease is claimed. A worker holding generation `g`
/// may only commit while the gate is still at `g`; once a replacement claims
/// the lease the gate moves to `g+1` and the old worker's commit is refused.
/// An expired worker finishing late is the normal case under load (§4.3), not
/// an exotic one, so this is checked on every mutating call rather than only
/// at finalisation.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct Generation(pub u64);

impl Generation {
    pub fn next(self) -> Self {
        Generation(self.0.saturating_add(1))
    }
}

impl std::fmt::Display for Generation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_carry_their_prefix() {
        assert!(GateId::new().as_str().starts_with("vgate-"));
        assert!(AttestationId::new().as_str().starts_with("vatt-"));
        assert!(AttemptId::new().as_str().starts_with("vatm-"));
    }

    #[test]
    fn parse_rejects_traversal_and_separators() {
        for bad in ["../escape", "a/b", "a\\b", "a.b", "", "with space"] {
            assert!(
                GateId::parse(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn parse_rejects_overlong_ids() {
        let long = "a".repeat(MAX_ID_LEN + 1);
        assert!(GateId::parse(&long).is_err());
        assert!(GateId::parse(&"a".repeat(MAX_ID_LEN)).is_ok());
    }

    #[test]
    fn candidate_revision_rejects_zero_and_bad_refs() {
        assert!(CandidateRevision::new("ccp-1", 0).is_err());
        assert!(CandidateRevision::new("../x", 1).is_err());
        assert!(CandidateRevision::new("ccp-1", 1).is_ok());
    }

    #[test]
    fn candidate_revision_next_increments() {
        let a = CandidateRevision::new("ccp-1", 1).unwrap();
        let b = a.next("ccp-2").unwrap();
        assert_eq!(b.revision, 2);
        assert_eq!(b.candidate_ref, "ccp-2");
    }

    #[test]
    fn generation_is_monotonic() {
        let g = Generation::default();
        assert_eq!(g.0, 0);
        assert_eq!(g.next().0, 1);
        assert_eq!(g.next().next().0, 2);
    }
}

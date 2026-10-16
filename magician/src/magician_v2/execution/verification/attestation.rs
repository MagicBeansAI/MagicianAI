//! `VerificationAttestation` — evidence that one candidate snapshot was
//! checked against one resolved policy by the shared runner.
//!
//! ## Why this is a separate record from the proposal
//!
//! `CodeChangeProposal::content_hash` (`file_edit/proposal.rs:519`) hashes
//! `test_evidence` along with the patch, scope and provenance, and the
//! diff-approval decide site recomputes that hash on the loaded on-disk record
//! to fail closed on any drift. Appending verification evidence to an approved
//! proposal would therefore not muddle its identity — it would break the
//! trusted-store integrity check outright. Evidence lives here and *references*
//! the proposal; the proposal is never mutated.
//!
//! ## What "immutable" means here, precisely
//!
//! * [`AttestationKey`] is immutable — set at creation, verified on every
//!   subsequent write.
//! * `attempts` is append-only — existing entries may never change and the
//!   vector may never shrink.
//! * `accepted_result` is written exactly once, under a fenced compare-and-set
//!   owned by the store.
//! * Once `accepted_result` is written the attestation is **sealed**: no
//!   further attempts may be appended. A manual re-verification creates a
//!   *sibling* attestation over the same key with a new id. Without sealing, a
//!   green attestation could accumulate red attempts underneath it and every
//!   reader would have to guess which attempt the acceptance referred to.
//!
//! ## Why scope and provenance are in the key
//!
//! Digests are content addresses, and two projects can legitimately produce
//! identical content — a shared template, a vendored file, an empty repo.
//! Without `scope`, `project_binding` and `candidate_ref` in the identity, one
//! project's green attestation could satisfy another project's gate, or one
//! principal's could satisfy another's. That is a cross-scope authority hole
//! wearing a cache's clothes.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::ids::{AttemptId, AttestationId, Generation};
use crate::magician_v2::execution::file_edit::transaction::TransactionScope;

/// Bumped when the on-disk shape changes incompatibly. Readers refuse a
/// version they do not understand rather than coercing it, because a
/// half-understood attestation is indistinguishable from a forged one.
pub const ATTESTATION_SCHEMA_VERSION: u32 = 1;

/// The immutable identity of an attestation.
///
/// Every field participates in [`AttestationKey::digest`]. Reuse is only ever
/// legitimate when *all* of them match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestationKey {
    /// Owning principal + workspace. Reused from the proposal store's type so
    /// the two records cannot disagree about what a scope is.
    pub scope: TransactionScope,
    /// Repository or VibeDev project this evidence belongs to.
    pub project_binding: String,
    /// The candidate snapshot's origin reference (proposal id, or child
    /// execution id for a direct-apply Autopilot run).
    pub candidate_ref: String,
    /// The `CodeChangeProposal` this candidate came from, when one exists.
    /// Autopilot self-apply runs legitimately have none.
    pub proposal_ref: Option<String>,
    /// Content address of the immutable *input* snapshot (§4.5). Build outputs
    /// written during checks are not part of this.
    pub snapshot_digest: String,
    /// Content address of the fully resolved required policy that was run.
    pub policy_digest: String,
    /// Content address of the runner environment — toolchain, image, runner
    /// implementation. In the key deliberately: reusing a green result after
    /// the compiler or runner changed would be unsafe.
    pub runner_env_digest: String,
}

impl AttestationKey {
    /// Stable content address of the whole key.
    ///
    /// Field names are included so that moving a value between two string
    /// fields changes the digest. A positional encoding would let
    /// `project_binding = "a", candidate_ref = "b"` collide with
    /// `project_binding = "ab", candidate_ref = ""`.
    pub fn digest(&self) -> blake3::Hash {
        let mut hasher = blake3::Hasher::new();
        let mut field = |name: &str, value: &str| {
            hasher.update(name.as_bytes());
            hasher.update(b"\x1f");
            hasher.update(value.as_bytes());
            hasher.update(b"\x1e");
        };
        field("principal", &self.scope.principal);
        field("workspace", &self.scope.workspace);
        field("project_binding", &self.project_binding);
        field("candidate_ref", &self.candidate_ref);
        field("proposal_ref", self.proposal_ref.as_deref().unwrap_or(""));
        field("snapshot_digest", &self.snapshot_digest);
        field("policy_digest", &self.policy_digest);
        field("runner_env_digest", &self.runner_env_digest);
        hasher.finalize()
    }

    /// Reject a key with empty required components. An attestation whose key
    /// is partly blank would collide with every other partly blank key.
    pub fn validate(&self) -> Result<()> {
        let required = [
            ("principal", self.scope.principal.as_str()),
            ("workspace", self.scope.workspace.as_str()),
            ("project_binding", self.project_binding.as_str()),
            ("candidate_ref", self.candidate_ref.as_str()),
            ("snapshot_digest", self.snapshot_digest.as_str()),
            ("policy_digest", self.policy_digest.as_str()),
            ("runner_env_digest", self.runner_env_digest.as_str()),
        ];
        for (name, value) in required {
            if value.trim().is_empty() {
                return Err(anyhow!("attestation key field `{name}` is empty"));
            }
        }
        if self
            .proposal_ref
            .as_ref()
            .is_some_and(|r| r.trim().is_empty())
        {
            return Err(anyhow!(
                "attestation key `proposal_ref` is present but empty; use None instead"
            ));
        }
        Ok(())
    }

    /// True when `other` may be served from this attestation's evidence.
    ///
    /// Deliberately a whole-key comparison rather than a digest-only one: a
    /// digest match with a field mismatch would mean a hash collision, and
    /// answering "yes" in that case is exactly the failure this guards.
    pub fn matches(&self, other: &AttestationKey) -> bool {
        self == other
    }
}

/// Result of one command inside one attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandResult {
    pub command: String,
    /// `None` when the command was killed by its per-command timeout.
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub timed_out: bool,
    /// Bounded capture. The runner truncates; full logs live in the run
    /// artifacts, not in the attestation.
    pub stdout_tail: String,
    pub stderr_tail: String,
    /// Advisory commands are recorded but never gate (§4.7).
    pub advisory: bool,
}

impl CommandResult {
    pub fn passed(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0)
    }
}

/// Outcome of a single attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    /// Every required command passed.
    Green,
    /// At least one required command failed.
    Red,
    /// The attempt could not be completed — crash, lost lease, runner
    /// unavailable. Never readable as a pass (§5.3); it is re-run under a new
    /// attempt.
    Indeterminate,
}

/// One execution of the resolved policy against the keyed snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationAttempt {
    pub attempt_id: AttemptId,
    /// The fencing generation the worker held. Recorded so a late commit from
    /// a superseded worker is identifiable after the fact, not just refused.
    pub generation: Generation,
    /// Opaque lease token the worker held while running this attempt.
    pub fenced_lease: String,
    pub started_at: DateTime<Utc>,
    pub settled_at: Option<DateTime<Utc>>,
    pub command_results: Vec<CommandResult>,
    pub outcome: AttemptOutcome,
}

impl VerificationAttempt {
    /// Derive the outcome from the required command results.
    ///
    /// Advisory results are excluded: they are recorded for humans and never
    /// gate. An attempt with no required commands is *not* green — "nothing
    /// ran" is `unverified` at the gate level, and inventing a green attempt
    /// for it would let the gate release on no evidence.
    pub fn outcome_from_required(results: &[CommandResult]) -> AttemptOutcome {
        let mut saw_required = false;
        for r in results.iter().filter(|r| !r.advisory) {
            saw_required = true;
            if !r.passed() {
                return AttemptOutcome::Red;
            }
        }
        if saw_required {
            AttemptOutcome::Green
        } else {
            AttemptOutcome::Indeterminate
        }
    }
}

/// The accepted, final result of an attestation. Written exactly once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedResult {
    pub attempt_id: AttemptId,
    pub outcome: AttemptOutcome,
    pub accepted_at: DateTime<Utc>,
    /// The generation that won the accept race. Recorded for audit.
    pub generation: Generation,
}

/// Evidence for one candidate snapshot under one policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationAttestation {
    pub schema_version: u32,
    pub attestation_id: AttestationId,
    pub key: AttestationKey,
    pub created_at: DateTime<Utc>,
    /// Append-only.
    pub attempts: Vec<VerificationAttempt>,
    /// Write-once; presence means the attestation is sealed.
    pub accepted_result: Option<AcceptedResult>,
}

impl VerificationAttestation {
    pub fn new(key: AttestationKey) -> Result<Self> {
        key.validate()?;
        Ok(Self {
            schema_version: ATTESTATION_SCHEMA_VERSION,
            attestation_id: AttestationId::new(),
            key,
            created_at: Utc::now(),
            attempts: Vec::new(),
            accepted_result: None,
        })
    }

    /// Sealed attestations reject further attempts.
    pub fn is_sealed(&self) -> bool {
        self.accepted_result.is_some()
    }

    pub fn attempt(&self, id: &AttemptId) -> Option<&VerificationAttempt> {
        self.attempts.iter().find(|a| &a.attempt_id == id)
    }

    /// Append an attempt, refusing if the attestation is already sealed.
    pub fn append_attempt(&mut self, attempt: VerificationAttempt) -> Result<()> {
        if self.is_sealed() {
            return Err(anyhow!(
                "attestation {} is sealed; create a sibling attestation instead of appending",
                self.attestation_id
            ));
        }
        if self.attempt(&attempt.attempt_id).is_some() {
            return Err(anyhow!(
                "attempt {} already present on attestation {}",
                attempt.attempt_id,
                self.attestation_id
            ));
        }
        self.attempts.push(attempt);
        Ok(())
    }

    /// Validate that `self` is a legal successor of `prior`.
    ///
    /// The store calls this before every write, so the invariants survive
    /// journal replay, concurrent writers and a hand-edited file on disk —
    /// not just the happy path through the typed API.
    pub fn validate_successor(&self, prior: &VerificationAttestation) -> Result<()> {
        if self.attestation_id != prior.attestation_id {
            return Err(anyhow!(
                "attestation id changed {} -> {}",
                prior.attestation_id,
                self.attestation_id
            ));
        }
        if self.key != prior.key {
            return Err(anyhow!(
                "attestation {} key changed after creation; keys are immutable",
                self.attestation_id
            ));
        }
        if self.created_at != prior.created_at {
            return Err(anyhow!(
                "attestation {} created_at changed after creation",
                self.attestation_id
            ));
        }
        if self.attempts.len() < prior.attempts.len() {
            return Err(anyhow!(
                "attestation {} attempts shrank {} -> {}; attempts are append-only",
                self.attestation_id,
                prior.attempts.len(),
                self.attempts.len()
            ));
        }
        for (i, (old, new)) in prior.attempts.iter().zip(self.attempts.iter()).enumerate() {
            if old != new {
                return Err(anyhow!(
                    "attestation {} attempt #{i} ({}) was modified; attempts are append-only",
                    self.attestation_id,
                    old.attempt_id
                ));
            }
        }
        match (&prior.accepted_result, &self.accepted_result) {
            // Write-once: an accepted result may never change or be cleared.
            (Some(old), Some(new)) if old != new => Err(anyhow!(
                "attestation {} accepted_result rewritten; it is write-once",
                self.attestation_id
            )),
            (Some(_), None) => Err(anyhow!(
                "attestation {} accepted_result cleared; it is write-once",
                self.attestation_id
            )),
            // Sealing: nothing may be appended in the same write that seals,
            // or after it.
            (Some(_), Some(_)) if self.attempts.len() != prior.attempts.len() => Err(anyhow!(
                "attestation {} appended an attempt while sealed",
                self.attestation_id
            )),
            _ => Ok(()),
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn scope() -> TransactionScope {
        TransactionScope {
            principal: "anonymous".into(),
            workspace: "default".into(),
        }
    }

    fn key() -> AttestationKey {
        AttestationKey {
            scope: scope(),
            project_binding: "proj-a".into(),
            candidate_ref: "ccp-1".into(),
            proposal_ref: Some("ccp-1".into()),
            snapshot_digest: "snap-1".into(),
            policy_digest: "pol-1".into(),
            runner_env_digest: "env-1".into(),
        }
    }

    fn attempt(outcome: AttemptOutcome) -> VerificationAttempt {
        VerificationAttempt {
            attempt_id: AttemptId::new(),
            generation: Generation(1),
            fenced_lease: "lease-1".into(),
            started_at: Utc::now(),
            settled_at: Some(Utc::now()),
            command_results: Vec::new(),
            outcome,
        }
    }

    #[test]
    fn key_digest_is_stable_and_field_sensitive() {
        let a = key();
        assert_eq!(a.digest(), key().digest());

        // Moving a character across a field boundary must change the digest.
        let mut b = key();
        b.project_binding = "proj".into();
        b.candidate_ref = "accp-1".into();
        assert_ne!(a.digest(), b.digest());
    }

    #[test]
    fn cross_scope_and_cross_project_keys_never_match() {
        let base = key();

        let mut other_principal = key();
        other_principal.scope.principal = "someone-else".into();
        assert!(!base.matches(&other_principal));

        let mut other_workspace = key();
        other_workspace.scope.workspace = "other".into();
        assert!(!base.matches(&other_workspace));

        let mut other_project = key();
        other_project.project_binding = "proj-b".into();
        assert!(!base.matches(&other_project));
    }

    #[test]
    fn identical_content_in_two_projects_yields_distinct_keys() {
        // The cache-collision hole: same snapshot, same policy, same runner —
        // different project. Must not be reusable.
        let mut a = key();
        let mut b = key();
        a.project_binding = "proj-a".into();
        b.project_binding = "proj-b".into();
        assert_eq!(a.snapshot_digest, b.snapshot_digest);
        assert_ne!(a.digest(), b.digest());
        assert!(!a.matches(&b));
    }

    #[test]
    fn runner_env_participates_in_identity() {
        let a = key();
        let mut b = key();
        b.runner_env_digest = "env-2".into();
        assert!(!a.matches(&b));
    }

    #[test]
    fn empty_key_fields_are_rejected() {
        let mut k = key();
        k.project_binding = "  ".into();
        assert!(k.validate().is_err());

        let mut k2 = key();
        k2.proposal_ref = Some(String::new());
        assert!(k2.validate().is_err());

        // proposal_ref is legitimately absent for Autopilot self-apply.
        let mut k3 = key();
        k3.proposal_ref = None;
        assert!(k3.validate().is_ok());
    }

    #[test]
    fn attempts_are_append_only() {
        let prior = {
            let mut a = VerificationAttestation::new(key()).unwrap();
            a.append_attempt(attempt(AttemptOutcome::Red)).unwrap();
            a
        };

        // Append is fine.
        let mut ok = prior.clone();
        ok.append_attempt(attempt(AttemptOutcome::Green)).unwrap();
        assert!(ok.validate_successor(&prior).is_ok());

        // Shrinking is not.
        let mut shrunk = prior.clone();
        shrunk.attempts.clear();
        assert!(shrunk.validate_successor(&prior).is_err());

        // Modifying an existing entry is not.
        let mut modified = prior.clone();
        modified.attempts[0].outcome = AttemptOutcome::Green;
        assert!(modified.validate_successor(&prior).is_err());
    }

    #[test]
    fn key_cannot_change_after_creation() {
        let prior = VerificationAttestation::new(key()).unwrap();
        let mut changed = prior.clone();
        changed.key.snapshot_digest = "snap-2".into();
        assert!(changed.validate_successor(&prior).is_err());
    }

    #[test]
    fn accepted_result_is_write_once() {
        let mut prior = VerificationAttestation::new(key()).unwrap();
        let a = attempt(AttemptOutcome::Green);
        let attempt_id = a.attempt_id.clone();
        prior.append_attempt(a).unwrap();

        let accepted = AcceptedResult {
            attempt_id,
            outcome: AttemptOutcome::Green,
            accepted_at: Utc::now(),
            generation: Generation(1),
        };

        let mut sealed = prior.clone();
        sealed.accepted_result = Some(accepted.clone());
        assert!(sealed.validate_successor(&prior).is_ok());

        // Rewriting it is refused.
        let mut rewritten = sealed.clone();
        rewritten.accepted_result = Some(AcceptedResult {
            outcome: AttemptOutcome::Red,
            ..accepted
        });
        assert!(rewritten.validate_successor(&sealed).is_err());

        // Clearing it is refused.
        let mut cleared = sealed.clone();
        cleared.accepted_result = None;
        assert!(cleared.validate_successor(&sealed).is_err());
    }

    #[test]
    fn sealed_attestation_rejects_further_attempts() {
        let mut a = VerificationAttestation::new(key()).unwrap();
        let first = attempt(AttemptOutcome::Green);
        let attempt_id = first.attempt_id.clone();
        a.append_attempt(first).unwrap();
        a.accepted_result = Some(AcceptedResult {
            attempt_id,
            outcome: AttemptOutcome::Green,
            accepted_at: Utc::now(),
            generation: Generation(1),
        });

        // Through the typed API.
        assert!(a.append_attempt(attempt(AttemptOutcome::Red)).is_err());

        // And through a raw successor write, which is how a replayed journal
        // or an edited file would try it.
        let mut smuggled = a.clone();
        smuggled.attempts.push(attempt(AttemptOutcome::Red));
        assert!(smuggled.validate_successor(&a).is_err());
    }

    #[test]
    fn duplicate_attempt_ids_are_rejected() {
        let mut a = VerificationAttestation::new(key()).unwrap();
        let one = attempt(AttemptOutcome::Red);
        a.append_attempt(one.clone()).unwrap();
        assert!(a.append_attempt(one).is_err());
    }

    #[test]
    fn outcome_ignores_advisory_and_refuses_empty() {
        let advisory_fail = CommandResult {
            command: "lint".into(),
            exit_code: Some(1),
            duration_ms: 1,
            timed_out: false,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            advisory: true,
        };
        let required_pass = CommandResult {
            command: "make check-all".into(),
            exit_code: Some(0),
            duration_ms: 1,
            timed_out: false,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            advisory: false,
        };

        // A failing advisory command does not turn the attempt red.
        assert_eq!(
            VerificationAttempt::outcome_from_required(&[
                advisory_fail.clone(),
                required_pass.clone()
            ]),
            AttemptOutcome::Green
        );

        // No required commands is NOT green — that is `unverified` at the gate.
        assert_eq!(
            VerificationAttempt::outcome_from_required(&[advisory_fail]),
            AttemptOutcome::Indeterminate
        );

        // A timed-out required command is red, not green-by-missing-exit-code.
        let timed_out = CommandResult {
            timed_out: true,
            exit_code: None,
            ..required_pass
        };
        assert_eq!(
            VerificationAttempt::outcome_from_required(&[timed_out]),
            AttemptOutcome::Red
        );
    }
}

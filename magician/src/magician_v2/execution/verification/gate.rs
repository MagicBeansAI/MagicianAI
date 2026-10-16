//! `VerificationGate` — the mutable spine that links a gated root execution to
//! the succession of immutable attestations produced for it.
//!
//! One record cannot hold both an immutable key and a repair counter: after a
//! repair the snapshot changes, so a single record would either mutate its own
//! key or verify repaired code under the old one. The gate is therefore
//! mutable and short-lived; [`super::attestation::VerificationAttestation`] is
//! immutable and permanent. A repair produces a new candidate and a new
//! attestation; the gate is what ties them together.
//!
//! Note there is an unrelated `resource_authority/gate.rs` in this tree. This
//! module gates *verification of code*, not resource reservations.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};

use super::ids::{AttestationId, CandidateRevision, GateId, Generation};
use crate::magician_v2::execution::file_edit::transaction::TransactionScope;

pub const GATE_SCHEMA_VERSION: u32 = 1;

/// Where the candidate came from, so repair can re-enter the *same* engineer
/// rather than calling a provider adapter directly (§4.6).
///
/// Populated from proposal/delegation provenance attached to the candidate
/// snapshot — never from the child's own terminal event, which is not
/// authoritative about who owned the work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateOrigin {
    /// The agent that owns repair. Repair creates or resumes an execution
    /// owned by this agent.
    pub engineer_agent_id: String,
    pub coding_profile: Option<String>,
    /// Engine identity (`pi`, `codex_app_server`, …) of the failed candidate.
    /// Provenance only. Whether repair may leave this engine is
    /// [`Self::constraint_auto`].
    pub coding_engine: Option<String>,
    /// True when the request's VibeDev constraint is Auto. Repair may then
    /// pick another eligible engine after this candidate is invalidated.
    #[serde(default)]
    pub constraint_auto: bool,
    /// Opaque, server-side reference used to resume the engine-native
    /// continuation. **Never surfaced to the model, the proposal, the gate API
    /// or the UI** — it is resolved internally through `run_coding_task`.
    pub coding_invocation_ref: Option<String>,
    pub child_execution_id: Option<String>,
}

/// Per-gate budgets (§6 phase 6).
///
/// Deliberately owned by this controller rather than inherited from the global
/// agentic ceiling. At the time of writing `DEFAULT_AGENTIC_MAX_DURATION_SECS`
/// is still 2400s (40 min), which the run-duration plan raises separately; a
/// repair loop bounded only by that ceiling would die inside it and look like
/// verification failing. Enabling bounded repair in production still wants
/// that plan landed — these budgets bound the *gate*, not the coding turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateBudgets {
    pub max_repair_rounds: u32,
    pub max_spend_usd: Option<f64>,
    pub max_elapsed_secs: Option<u64>,
}

impl Default for GateBudgets {
    fn default() -> Self {
        Self {
            max_repair_rounds: 3,
            max_spend_usd: None,
            max_elapsed_secs: Some(2 * 60 * 60),
        }
    }
}

/// Consumption against [`GateBudgets`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GateSpend {
    pub repair_rounds: u32,
    pub spend_usd: f64,
}

/// Lifecycle of a gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateStatus {
    /// A candidate success is held; verification is queued or running. This is
    /// the only status from which finalisation may occur.
    VerificationPending,
    /// Checks failed and the owning engineer is repairing. The prior candidate
    /// is already invalidated (§4.2).
    Repairing,
    Verified,
    /// No required checks were configured. Terminal, and explicitly *not* a
    /// silent pass.
    Unverified,
    /// Repair budget exhausted without reaching green.
    Exhausted,
    /// Runner or policy could not be reached. Fails closed — never reads as a
    /// pass, and never collapses into `Unverified`: one means "there was
    /// nothing to run", the other means "we could not run it".
    Unavailable,
    Cancelled,
    /// The proposal applied only partially; never eligible for terminal
    /// success.
    BlockedPartial,
}

impl GateStatus {
    /// Whether the gate has reached a state it will never leave.
    ///
    /// `Unavailable` is deliberately **not** terminal. "We could not run the
    /// checks" is a statement about this attempt, not about the candidate — an
    /// unreadable policy or a missing toolchain gets fixed, and a gate that
    /// absorbed on the first outage would either hang its task forever or
    /// release a candidate nobody checked. It stays retryable and is bounded
    /// by the gate's elapsed budget like everything else.
    pub fn is_terminal(self) -> bool {
        !matches!(
            self,
            GateStatus::VerificationPending | GateStatus::Repairing | GateStatus::Unavailable
        )
    }

    /// Legal transitions. Anything else is refused at the store boundary so a
    /// replayed or corrupted record cannot walk the gate backwards out of a
    /// terminal state.
    pub fn can_transition_to(self, next: GateStatus) -> bool {
        use GateStatus::*;
        match self {
            VerificationPending => matches!(
                next,
                VerificationPending
                    | Repairing
                    | Verified
                    | Unverified
                    | Exhausted
                    | Unavailable
                    | Cancelled
                    | BlockedPartial
            ),
            // A repair round ends by re-entering verification with a new
            // candidate, or by giving up.
            Repairing => matches!(
                next,
                Repairing | VerificationPending | Exhausted | Unavailable | Cancelled
            ),
            // Not terminal: a later attempt may find the runner or policy
            // back, and the elapsed budget is what eventually stops it.
            Unavailable => matches!(
                next,
                Unavailable
                    | VerificationPending
                    | Repairing
                    | Verified
                    | Unverified
                    | Exhausted
                    | Cancelled
            ),
            // Terminal states are absorbing.
            _ => next == self,
        }
    }
}

/// The durable projection consumers read (§5.1).
///
/// Kept separate from task status deliberately: adding terminal task statuses
/// would ripple through reducers, schedulers, chat, task cards, activity
/// streams and voice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationState {
    /// No gate exists for this task — a legacy run, or one that predates the
    /// controller. **This is the default on purpose.** Missing state must
    /// never read as `Verified`.
    #[default]
    Unknown,
    Verified,
    Repairing,
    Unverified,
    Exhausted,
    Unavailable,
    Cancelled,
    BlockedPartial,
    /// Gated and in flight.
    Verifying,
}

impl VerificationState {
    pub fn as_str(self) -> &'static str {
        match self {
            VerificationState::Unknown => "unknown",
            VerificationState::Verified => "verified",
            VerificationState::Repairing => "repairing",
            VerificationState::Unverified => "unverified",
            VerificationState::Exhausted => "exhausted",
            VerificationState::Unavailable => "unavailable",
            VerificationState::Cancelled => "cancelled",
            VerificationState::BlockedPartial => "blocked_partial",
            VerificationState::Verifying => "verifying",
        }
    }

    /// The single predicate automation should use for "this code was checked
    /// and passed". Anything that is not exactly `Verified` is not verified.
    pub fn is_verified(self) -> bool {
        matches!(self, VerificationState::Verified)
    }
}

impl From<GateStatus> for VerificationState {
    fn from(status: GateStatus) -> Self {
        match status {
            GateStatus::VerificationPending => VerificationState::Verifying,
            GateStatus::Repairing => VerificationState::Repairing,
            GateStatus::Verified => VerificationState::Verified,
            GateStatus::Unverified => VerificationState::Unverified,
            GateStatus::Exhausted => VerificationState::Exhausted,
            GateStatus::Unavailable => VerificationState::Unavailable,
            GateStatus::Cancelled => VerificationState::Cancelled,
            GateStatus::BlockedPartial => VerificationState::BlockedPartial,
        }
    }
}

/// A worker's claim on a gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateLease {
    pub holder: String,
    pub token: String,
    pub generation: Generation,
    pub acquired_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl GateLease {
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }
}

/// One gated root execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerificationGate {
    pub schema_version: u32,
    pub gate_id: GateId,
    pub scope: TransactionScope,
    pub project_binding: String,
    pub root_task_id: String,
    pub root_execution_id: String,
    pub current_candidate: CandidateRevision,
    pub origin: GateOrigin,
    pub budgets: GateBudgets,
    pub spend: GateSpend,
    pub active_attestation_ref: Option<AttestationId>,
    pub status: GateStatus,
    /// Monotonic fencing token. Bumped on every lease claim.
    pub generation: Generation,
    pub lease: Option<GateLease>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Structured reason accompanying a terminal non-green outcome.
    pub terminal_reason: Option<String>,
}

impl VerificationGate {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        scope: TransactionScope,
        project_binding: impl Into<String>,
        root_task_id: impl Into<String>,
        root_execution_id: impl Into<String>,
        current_candidate: CandidateRevision,
        origin: GateOrigin,
        budgets: GateBudgets,
    ) -> Result<Self> {
        let project_binding = project_binding.into();
        if project_binding.trim().is_empty() {
            return Err(anyhow!("gate project_binding is empty"));
        }
        if origin.engineer_agent_id.trim().is_empty() {
            return Err(anyhow!(
                "gate origin.engineer_agent_id is empty; repair would have no owner"
            ));
        }
        let now = Utc::now();
        Ok(Self {
            schema_version: GATE_SCHEMA_VERSION,
            gate_id: GateId::new(),
            scope,
            project_binding,
            root_task_id: root_task_id.into(),
            root_execution_id: root_execution_id.into(),
            current_candidate,
            origin,
            budgets,
            spend: GateSpend::default(),
            active_attestation_ref: None,
            status: GateStatus::VerificationPending,
            generation: Generation::default(),
            lease: None,
            created_at: now,
            updated_at: now,
            terminal_reason: None,
        })
    }

    pub fn verification_state(&self) -> VerificationState {
        VerificationState::from(self.status)
    }

    /// Whether another repair round is affordable.
    pub fn repair_budget_remains(&self, now: DateTime<Utc>) -> bool {
        if self.spend.repair_rounds >= self.budgets.max_repair_rounds {
            return false;
        }
        if let Some(max) = self.budgets.max_spend_usd {
            if self.spend.spend_usd >= max {
                return false;
            }
        }
        if let Some(max) = self.budgets.max_elapsed_secs {
            let elapsed = now.signed_duration_since(self.created_at);
            if elapsed >= ChronoDuration::seconds(max as i64) {
                return false;
            }
        }
        true
    }

    /// The compare-and-set precondition for releasing a candidate success
    /// (§4.3). All three conditions must hold together.
    ///
    /// Note this deliberately takes the attestation's *key* candidate ref plus
    /// the revision the worker believes it verified, rather than trusting the
    /// worker's word about either.
    pub fn may_finalize_green(
        &self,
        verified_candidate: &CandidateRevision,
        attestation: &AttestationId,
        outcome_is_green: bool,
        holder_generation: Generation,
    ) -> Result<()> {
        if self.status != GateStatus::VerificationPending {
            return Err(anyhow!(
                "gate {} is {:?}, not verification_pending",
                self.gate_id,
                self.status
            ));
        }
        if &self.current_candidate != verified_candidate {
            return Err(anyhow!(
                "gate {} holds candidate {:?} but attestation verified {:?}; a stale candidate can never be released",
                self.gate_id,
                self.current_candidate,
                verified_candidate
            ));
        }
        if !outcome_is_green {
            return Err(anyhow!(
                "gate {} refused finalisation: accepted attestation is not green",
                self.gate_id
            ));
        }
        if self.generation != holder_generation {
            return Err(anyhow!(
                "gate {} is at generation {} but worker holds {}; a superseded worker cannot commit",
                self.gate_id,
                self.generation,
                holder_generation
            ));
        }
        if self
            .active_attestation_ref
            .as_ref()
            .is_some_and(|active| active != attestation)
        {
            return Err(anyhow!(
                "gate {} active attestation is {:?}, not {}",
                self.gate_id,
                self.active_attestation_ref,
                attestation
            ));
        }
        Ok(())
    }

    /// Validate that `self` is a legal successor of `prior`.
    ///
    /// Enforced at the store boundary so the invariants survive replay and
    /// concurrent writers, not just the typed API.
    pub fn validate_successor(&self, prior: &VerificationGate) -> Result<()> {
        if self.gate_id != prior.gate_id {
            return Err(anyhow!(
                "gate id changed {} -> {}",
                prior.gate_id,
                self.gate_id
            ));
        }
        // Identity fields are fixed for the gate's lifetime.
        if self.scope != prior.scope
            || self.project_binding != prior.project_binding
            || self.root_task_id != prior.root_task_id
            || self.root_execution_id != prior.root_execution_id
            || self.created_at != prior.created_at
        {
            return Err(anyhow!(
                "gate {} identity fields are immutable",
                self.gate_id
            ));
        }
        if self.generation < prior.generation {
            return Err(anyhow!(
                "gate {} generation went backwards {} -> {}; fencing tokens are monotonic",
                self.gate_id,
                prior.generation,
                self.generation
            ));
        }
        if self.current_candidate.revision < prior.current_candidate.revision {
            return Err(anyhow!(
                "gate {} candidate revision went backwards {} -> {}",
                self.gate_id,
                prior.current_candidate.revision,
                self.current_candidate.revision
            ));
        }
        if self.spend.repair_rounds < prior.spend.repair_rounds {
            return Err(anyhow!(
                "gate {} repair_rounds decreased; budget consumption is monotonic",
                self.gate_id
            ));
        }
        if !prior.status.can_transition_to(self.status) {
            return Err(anyhow!(
                "gate {} illegal transition {:?} -> {:?}",
                self.gate_id,
                prior.status,
                self.status
            ));
        }
        Ok(())
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

    fn origin() -> GateOrigin {
        GateOrigin {
            engineer_agent_id: "engineer".into(),
            coding_profile: None,
            coding_engine: Some("pi".into()),
            constraint_auto: false,
            coding_invocation_ref: Some("opaque-server-side-ref".into()),
            child_execution_id: Some("exec-child".into()),
        }
    }

    fn gate() -> VerificationGate {
        VerificationGate::new(
            scope(),
            "proj-a",
            "task-1",
            "exec-1",
            CandidateRevision::new("ccp-1", 1).unwrap(),
            origin(),
            GateBudgets::default(),
        )
        .unwrap()
    }

    #[test]
    fn new_gate_starts_pending_at_generation_zero() {
        let g = gate();
        assert_eq!(g.status, GateStatus::VerificationPending);
        assert_eq!(g.generation, Generation(0));
        assert_eq!(g.verification_state(), VerificationState::Verifying);
    }

    #[test]
    fn gate_requires_an_engineer_to_own_repair() {
        let mut o = origin();
        o.engineer_agent_id = "  ".into();
        let err = VerificationGate::new(
            scope(),
            "proj-a",
            "task-1",
            "exec-1",
            CandidateRevision::new("ccp-1", 1).unwrap(),
            o,
            GateBudgets::default(),
        );
        assert!(err.is_err());
    }

    #[test]
    fn missing_verification_state_defaults_to_unknown_not_verified() {
        // The single most dangerous default in this subsystem.
        assert_eq!(VerificationState::default(), VerificationState::Unknown);
        assert!(!VerificationState::default().is_verified());
    }

    #[test]
    fn only_verified_counts_as_verified() {
        for s in [
            VerificationState::Unknown,
            VerificationState::Repairing,
            VerificationState::Unverified,
            VerificationState::Exhausted,
            VerificationState::Unavailable,
            VerificationState::Cancelled,
            VerificationState::BlockedPartial,
            VerificationState::Verifying,
        ] {
            assert!(!s.is_verified(), "{s:?} must not read as verified");
        }
        assert!(VerificationState::Verified.is_verified());
    }

    #[test]
    fn unavailable_and_unverified_are_distinct_states() {
        // Collapsing these turns an outage into a clean bill of health.
        assert_ne!(
            VerificationState::from(GateStatus::Unavailable),
            VerificationState::from(GateStatus::Unverified)
        );
    }

    #[test]
    fn terminal_states_are_absorbing() {
        // `Unavailable` is intentionally absent: it says the checks could not
        // be run, which is a fact about the attempt and not a verdict on the
        // candidate. See `unavailable_is_retryable_not_a_verdict`.
        for s in [
            GateStatus::Verified,
            GateStatus::Unverified,
            GateStatus::Exhausted,
            GateStatus::Cancelled,
            GateStatus::BlockedPartial,
        ] {
            assert!(s.is_terminal());
            assert!(!s.can_transition_to(GateStatus::VerificationPending));
            assert!(!s.can_transition_to(GateStatus::Repairing));
            assert!(s.can_transition_to(s));
        }
    }

    #[test]
    fn unavailable_is_retryable_not_a_verdict() {
        // Absorbing here would mean one unreadable policy or missing
        // toolchain permanently decides a candidate: the task either hangs
        // with no path forward, or gets released as success on code nothing
        // ever checked.
        assert!(!GateStatus::Unavailable.is_terminal());
        assert!(GateStatus::Unavailable.can_transition_to(GateStatus::VerificationPending));
        assert!(GateStatus::Unavailable.can_transition_to(GateStatus::Verified));
        // Still bounded — the elapsed budget settles a permanent outage.
        assert!(GateStatus::Unavailable.can_transition_to(GateStatus::Exhausted));
    }

    #[test]
    fn repair_cycles_back_into_pending_but_cannot_jump_to_verified() {
        assert!(GateStatus::VerificationPending.can_transition_to(GateStatus::Repairing));
        assert!(GateStatus::Repairing.can_transition_to(GateStatus::VerificationPending));
        // Green is only ever declared from the pending state, after an
        // attestation is accepted.
        assert!(!GateStatus::Repairing.can_transition_to(GateStatus::Verified));
    }

    #[test]
    fn finalize_requires_pending_matching_candidate_green_and_generation() {
        let mut g = gate();
        let att = AttestationId::new();
        g.active_attestation_ref = Some(att.clone());
        let cand = g.current_candidate.clone();

        assert!(g
            .may_finalize_green(&cand, &att, true, Generation(0))
            .is_ok());

        // Wrong candidate — the stale-release hole.
        let stale = CandidateRevision::new("ccp-1", 2).unwrap();
        assert!(g
            .may_finalize_green(&stale, &att, true, Generation(0))
            .is_err());

        // Not green.
        assert!(g
            .may_finalize_green(&cand, &att, false, Generation(0))
            .is_err());

        // Superseded worker.
        assert!(g
            .may_finalize_green(&cand, &att, true, Generation(1))
            .is_err());

        // Wrong status.
        let mut repairing = g.clone();
        repairing.status = GateStatus::Repairing;
        assert!(repairing
            .may_finalize_green(&cand, &att, true, Generation(0))
            .is_err());
    }

    #[test]
    fn generation_and_candidate_revision_never_go_backwards() {
        let prior = {
            let mut g = gate();
            g.generation = Generation(3);
            g.current_candidate = CandidateRevision::new("ccp-2", 2).unwrap();
            g.spend.repair_rounds = 1;
            g
        };

        let mut back_gen = prior.clone();
        back_gen.generation = Generation(2);
        assert!(back_gen.validate_successor(&prior).is_err());

        let mut back_cand = prior.clone();
        back_cand.current_candidate = CandidateRevision::new("ccp-1", 1).unwrap();
        assert!(back_cand.validate_successor(&prior).is_err());

        let mut back_rounds = prior.clone();
        back_rounds.spend.repair_rounds = 0;
        assert!(back_rounds.validate_successor(&prior).is_err());

        let mut forward = prior.clone();
        forward.generation = Generation(4);
        forward.current_candidate = CandidateRevision::new("ccp-3", 3).unwrap();
        forward.spend.repair_rounds = 2;
        assert!(forward.validate_successor(&prior).is_ok());
    }

    #[test]
    fn gate_identity_is_immutable() {
        let prior = gate();
        for mutate in [
            (|g: &mut VerificationGate| g.project_binding = "other".into())
                as fn(&mut VerificationGate),
            |g: &mut VerificationGate| g.root_task_id = "task-2".into(),
            |g: &mut VerificationGate| g.root_execution_id = "exec-2".into(),
            |g: &mut VerificationGate| g.scope.principal = "other".into(),
        ] {
            let mut next = prior.clone();
            mutate(&mut next);
            assert!(next.validate_successor(&prior).is_err());
        }
    }

    #[test]
    fn budget_exhaustion_blocks_further_repair() {
        let now = Utc::now();

        let mut rounds = gate();
        rounds.spend.repair_rounds = rounds.budgets.max_repair_rounds;
        assert!(!rounds.repair_budget_remains(now));

        let mut spend = gate();
        spend.budgets.max_spend_usd = Some(1.0);
        spend.spend.spend_usd = 1.0;
        assert!(!spend.repair_budget_remains(now));

        let mut elapsed = gate();
        elapsed.budgets.max_elapsed_secs = Some(60);
        assert!(!elapsed.repair_budget_remains(now + ChronoDuration::seconds(61)));

        assert!(gate().repair_budget_remains(now));
    }

    #[test]
    fn lease_expiry_is_time_based() {
        let now = Utc::now();
        let lease = GateLease {
            holder: "worker-1".into(),
            token: "t".into(),
            generation: Generation(1),
            acquired_at: now,
            expires_at: now + ChronoDuration::seconds(30),
        };
        assert!(!lease.is_expired_at(now));
        assert!(lease.is_expired_at(now + ChronoDuration::seconds(30)));
        assert!(lease.is_expired_at(now + ChronoDuration::seconds(31)));
    }
}

//! Verification controller — Magician owns the invariant that code must be
//! checked; the coding agent still decides what to change and how to repair it.
//!
//! Today a coding engine stopping is treated as success. This module holds a
//! candidate success *before* the terminal transaction, runs the project's
//! required checks against an immutable snapshot of exactly that candidate,
//! and only then releases completion — or hands diagnostics back to the same
//! engineer for a bounded repair.
//!
//! ```text
//! engine produces and applies code
//!         ↓
//! task stays running, visibly VERIFYING
//!         ↓
//! shared runner executes the resolved required policy
//!    ├─ pass  → release the stored candidate success → completed
//!    ├─ fail  → diagnostics back to the SAME engineer → bounded repair
//!    └─ none  → finish explicitly as UNVERIFIED
//! ```
//!
//! ## Module layout
//!
//! * [`ids`] — typed, path-safe identifiers and the [`ids::Generation`]
//!   fencing token.
//! * [`attestation`] — immutable, append-only evidence for one candidate
//!   snapshot under one policy. Kept separate from `CodeChangeProposal`
//!   because that record's `content_hash` covers `test_evidence`, so appending
//!   evidence to an approved proposal would break the trusted-store integrity
//!   check rather than merely muddling identity.
//! * [`gate`] — the mutable spine linking a gated root execution to its
//!   succession of attestations, plus the durable
//!   [`gate::VerificationState`] projection consumers read.
//! * [`journal`] — the write-ahead log that makes "atomically establish
//!   candidate + gate state + outbox entry" implementable instead of merely
//!   asserted.
//! * [`store`] — task-scoped persistence, advisory locking, fenced
//!   compare-and-set, and journal-backed recovery.
//!
//! ## Activation
//!
//! Everything here is inert until [`VerificationActivation`] says otherwise,
//! and the default is **off**. Two reasons, both load-bearing:
//!
//! 1. The plan's own Phase 1 regression bar requires that landing the
//!    persistence slice does not alter existing task completion behaviour. A
//!    default-off flag is what makes that checkable rather than hoped for.
//! 2. Neither coding engine may claim verification-gated completion until the
//!    controller activates for both together, so activation is a deployment
//!    decision, not a side effect of merging.

pub mod attestation;
pub mod budgets;
pub mod controller;
pub mod events;
pub mod gate;
pub mod gating;
pub mod ids;
pub mod journal;
pub mod policy;
pub mod repair;
pub mod runner;
pub mod snapshot;
pub mod store;

#[cfg(any(test, feature = "test-fixtures"))]
mod e2e_tests;
#[cfg(any(test, feature = "test-fixtures"))]
mod phase1_regression;

pub use attestation::{
    AcceptedResult, AttemptOutcome, AttestationKey, CommandResult, VerificationAttempt,
    VerificationAttestation,
};
pub use budgets::{BudgetConfig, BudgetDimension, BudgetVerdict};
pub use controller::{ControllerDeps, PassOutcome, VerificationController};
pub use events::{
    NullEventSink, RecordingEventSink, VerificationEvent, VerificationEventKind,
    VerificationEventSink,
};
pub use gate::{
    GateBudgets, GateLease, GateOrigin, GateSpend, GateStatus, VerificationGate, VerificationState,
};
pub use gating::{
    activation_from_env, activation_from_env_or_config, evaluate as evaluate_candidate,
    last_engine_from_coding_events, origin_from_provenance, CandidateApplication, CandidateFacts,
    GateDecision, HoldRequest,
};
pub use ids::{AttemptId, AttestationId, CandidateRevision, GateId, Generation};
pub use journal::{JournalPayload, JournalTransaction, OutboxEntry, VerificationJournal};
pub use policy::{
    resolve as resolve_policy, CheckSpec, NetworkPolicy, PolicySource, ResolvedPolicy,
    SandboxPolicy, VerificationPolicy,
};
pub use repair::{
    decide as decide_repair, format_diagnostics, may_switch_engine, repair_pinned_engine,
    FailureFingerprint, PriorRound, RepairDecision, RepairRequest,
};
pub use runner::{
    default_runner, read_repository_policy, RunReport, RunTermination, RunnerEnv,
    VerificationRunner,
};
pub use snapshot::{EphemeralWorkspace, MutationReport, SourceSnapshot};
pub use store::{CasOutcome, VerificationStore, DEFAULT_LEASE_SECS};

/// Host-resolved runtime settings for the controller.
///
/// Named here rather than in the config layer so this module owns its own
/// contract; `config` maps the `verification:` YAML block onto this and the
/// host hands it to the service at boot. An unwired service runs on the
/// defaults, which reproduce the pre-config behaviour exactly:
/// [`gate::GateBudgets::default`] budgets, no owner baseline, and a
/// 60-second reconciler cadence for the host that chooses to spawn one.
#[derive(Debug, Clone)]
pub struct VerificationRuntimeSettings {
    /// Budget configuration each new gate resolves against.
    pub budgets: BudgetConfig,
    /// Owner baseline policy passed to every pass. `None` keeps the
    /// repository-defines-its-own semantics.
    pub baseline: Option<VerificationPolicy>,
    /// Reconciler tick. `0` disables periodic driving, leaving startup
    /// recovery as the only scheduler — the pre-reconciler behaviour.
    pub reconcile_interval_secs: u64,
}

impl Default for VerificationRuntimeSettings {
    fn default() -> Self {
        Self {
            budgets: BudgetConfig::default(),
            baseline: None,
            reconcile_interval_secs: 60,
        }
    }
}

/// Engineer id recorded when a gate must exist but provenance is
/// unresolvable.
///
/// Only ever used for a `blocked_partial` gate, which is terminal — no repair
/// is dispatched from it — so this can never route work to a nonexistent
/// agent. It exists so a partial apply is *visible* as
/// `verification_state = blocked_partial` instead of being an unfinished task
/// with no record of why.
pub const UNATTRIBUTED_ENGINEER: &str = "unattributed";

/// Whether the controller gates completion at all.
///
/// `Disabled` is not a stub: the persistence layer still records gates and
/// attestations when asked, but no terminal path consults them, so completion
/// timing is byte-for-byte what it was before this module existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerificationActivation {
    /// Nothing is gated. Existing completion behaviour is untouched.
    #[default]
    Disabled,
    /// Gates are opened and evidence is recorded, but a candidate success is
    /// still released immediately. Lets a deployment observe blast radius —
    /// which tasks *would* have been held — before anything is held.
    Observe,
    /// Fully active: a candidate success is held until verification settles.
    Enforce,
}

impl VerificationActivation {
    /// True only in [`VerificationActivation::Enforce`]. Every call site that
    /// could delay or block completion must go through this, so enabling the
    /// controller is one decision in one place.
    pub fn gates_completion(self) -> bool {
        matches!(self, VerificationActivation::Enforce)
    }

    /// True when gates should be created at all.
    pub fn records_evidence(self) -> bool {
        matches!(
            self,
            VerificationActivation::Observe | VerificationActivation::Enforce
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            VerificationActivation::Disabled => "disabled",
            VerificationActivation::Observe => "observe",
            VerificationActivation::Enforce => "enforce",
        }
    }

    /// Parse from configuration. An unrecognised value is **not** an error and
    /// **not** `Enforce` — it degrades to `Disabled`, because a typo in a
    /// config key must never be the thing that starts gating production
    /// completions, and must never be the thing that stops them either.
    pub fn from_config(raw: Option<&str>) -> Self {
        match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("enforce") => VerificationActivation::Enforce,
            Some("observe") => VerificationActivation::Observe,
            _ => VerificationActivation::Disabled,
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn activation_defaults_to_disabled() {
        assert_eq!(
            VerificationActivation::default(),
            VerificationActivation::Disabled
        );
        assert!(!VerificationActivation::default().gates_completion());
        assert!(!VerificationActivation::default().records_evidence());
    }

    #[test]
    fn only_enforce_gates_completion() {
        assert!(!VerificationActivation::Disabled.gates_completion());
        assert!(!VerificationActivation::Observe.gates_completion());
        assert!(VerificationActivation::Enforce.gates_completion());
    }

    #[test]
    fn observe_records_without_gating() {
        assert!(VerificationActivation::Observe.records_evidence());
        assert!(!VerificationActivation::Observe.gates_completion());
    }

    #[test]
    fn unknown_config_values_degrade_to_disabled() {
        for raw in [
            None,
            Some(""),
            Some("  "),
            Some("yes"),
            Some("true"),
            Some("ENFORCED"),
        ] {
            assert_eq!(
                VerificationActivation::from_config(raw),
                VerificationActivation::Disabled,
                "unexpected activation for {raw:?}"
            );
        }
    }

    #[test]
    fn config_parsing_is_case_and_space_insensitive_for_known_values() {
        assert_eq!(
            VerificationActivation::from_config(Some(" Enforce ")),
            VerificationActivation::Enforce
        );
        assert_eq!(
            VerificationActivation::from_config(Some("OBSERVE")),
            VerificationActivation::Observe
        );
    }
}

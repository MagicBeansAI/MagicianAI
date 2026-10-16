//! Candidate-success gating — the decision of whether a terminal success is
//! held for verification.
//!
//! ## Where this sits
//!
//! `ArtifactV2Service::persist_execution_outcome` writes the terminal events,
//! flips the execution to terminal and starts synthesis **as one step**.
//! Verification started after that runs against a task the UI, task card,
//! synthesis and voice flow already believe succeeded. So the gate must
//! intercept *before* that transaction:
//!
//! ```text
//! persist_execution_outcome
//!   ├─ delegated child?  → handle_child_terminal      (never gated, §4.4)
//!   ├─ replay short-circuits A/B → return             (never re-gated)
//!   ├─ ►► GATE HERE ◄◄                                (this module)
//!   └─ Atomic Step 1: append_outcome_events + flip terminal + synthesis
//! ```
//!
//! ## Why the decision core is pure
//!
//! The service method is ~200 lines inside a 32,000-line file with a live
//! parallel workstream in the same crate. Putting the policy here — as
//! functions over plain inputs — means the rules are tested directly and the
//! call site stays a thin, reviewable block. It also means the "disabled"
//! path is provably a single early return rather than a behaviour woven
//! through the transaction.

use anyhow::Result;

use super::gate::{GateBudgets, GateOrigin, GateStatus, VerificationGate};
use super::ids::{CandidateRevision, GateId};
use super::VerificationActivation;
use crate::magician_v2::execution::file_edit::transaction::TransactionScope;

/// What the terminal path should do with a candidate success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    /// Run the normal terminal transaction unchanged. This is the answer
    /// whenever the controller is disabled, the execution is not a gatable
    /// VibeDev root, or the outcome is not a candidate success.
    Proceed,
    /// Hold the candidate. The caller must **not** run the terminal
    /// transaction; the task stays running and visibly verifying.
    Hold(Box<HoldRequest>),
    /// The candidate can never reach terminal success — a partially applied
    /// proposal. Distinct from `Hold` because there is nothing to verify.
    ///
    /// It still carries a [`HoldRequest`] when provenance is resolvable, so a
    /// `blocked_partial` gate can be opened. Without one the task would be
    /// left non-terminal with no record — a hang rather than a gate, and
    /// invisible to `verification_state`.
    RefuseTerminalSuccess {
        reason: String,
        request: Option<Box<HoldRequest>>,
    },
    /// A gate for this candidate is already open and has not settled.
    ///
    /// The terminal path can be re-entered for the same execution — a retry, a
    /// resume, or the controller itself releasing a verified gate — and
    /// re-deriving the decision from proposals and coding events would answer
    /// `Hold` every time. Opening a second gate fails, and treating that
    /// failure as "could not hold" would complete the very candidate the first
    /// gate is still verifying. Answer from the durable gate instead.
    AlreadyHeld { gate_id: GateId },
}

/// Everything needed to open a gate, gathered by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldRequest {
    pub scope: TransactionScope,
    pub project_binding: String,
    pub root_task_id: String,
    pub root_execution_id: String,
    pub candidate: CandidateRevision,
    pub origin: GateOrigin,
}

impl HoldRequest {
    pub fn into_gate(self, budgets: GateBudgets) -> Result<VerificationGate> {
        VerificationGate::new(
            self.scope,
            self.project_binding,
            self.root_task_id,
            self.root_execution_id,
            self.candidate,
            self.origin,
            budgets,
        )
    }
}

/// How the candidate reached the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateApplication {
    /// A `CodeChangeProposal` was approved and fully applied.
    ProposalApplied,
    /// A proposal applied only partially. Never terminal success (§5.3).
    ProposalPartiallyApplied,
    /// Autopilot self-applied on a work branch. Legitimate, and the mode that
    /// most needs harness-owned verification because nobody is watching.
    AutopilotSelfApplied,
}

/// The facts the terminal path already knows or can cheaply read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateFacts {
    /// `outcome.is_terminal`.
    pub is_terminal: bool,
    /// `outcome.execution_status == "completed"`.
    pub is_success: bool,
    /// False for delegated children. Children keep their existing terminal
    /// path; gating them would starve parent reconciliation and let repair
    /// children recurse into verification (§4.4).
    pub is_root: bool,
    /// Whether this run engaged a coding pipeline at all. A root that produced
    /// no code has nothing to verify.
    pub produced_code: bool,
    pub application: Option<CandidateApplication>,
}

/// Decide what to do with a terminal outcome.
///
/// Deliberately total and side-effect free: every early return is a reason
/// this run is not gated, and the reasons are enumerable in a test.
pub fn evaluate(
    activation: VerificationActivation,
    facts: &CandidateFacts,
    hold: impl FnOnce() -> Option<HoldRequest>,
) -> GateDecision {
    // The disabled path is exactly one branch. Nothing below it can run, so
    // completion timing is unchanged by construction.
    //
    // `observe` continues past here on purpose: it must reach the same
    // classification `enforce` would, so the recorded gate answers "which
    // candidates *would* have been held". It is the caller that releases the
    // candidate anyway — this function never decides completion timing.
    if !activation.records_evidence() {
        return GateDecision::Proceed;
    }
    if !facts.is_terminal || !facts.is_success {
        return GateDecision::Proceed;
    }
    if !facts.is_root {
        return GateDecision::Proceed;
    }

    // A partially applied proposal is refused whether or not it produced
    // code — the workspace is in a state nobody approved.
    if facts.application == Some(CandidateApplication::ProposalPartiallyApplied) {
        return GateDecision::RefuseTerminalSuccess {
            reason: "code-change proposal was only partially applied; \
                     terminal success is not available for a partial apply"
                .to_string(),
            request: hold().map(Box::new),
        };
    }

    if !facts.produced_code {
        return GateDecision::Proceed;
    }

    match hold() {
        Some(request) => GateDecision::Hold(Box::new(request)),
        // Provenance we cannot resolve means we cannot name an owner for
        // repair. Proceeding is correct rather than failing the task: the
        // controller's job is to gate what it can verify, not to break
        // completion for runs it cannot attribute.
        None => GateDecision::Proceed,
    }
}

/// The terminal gate status implied by a refusal.
pub fn refusal_status() -> GateStatus {
    GateStatus::BlockedPartial
}

/// Kill-switch, matching the existing `MAGICIAN_VIBEDEV_SUCCESS_GATE` idiom in
/// `artifact_v2::service`.
///
/// Reads `MAGICIAN_VERIFICATION_CONTROLLER`:
/// `enforce` | `observe` | anything else (including unset) → disabled.
pub fn activation_from_env() -> VerificationActivation {
    VerificationActivation::from_config(
        std::env::var("MAGICIAN_VERIFICATION_CONTROLLER")
            .ok()
            .as_deref(),
    )
}

/// Activation for a host that also has a configured mode.
///
/// The env var, when **set**, wins — it is the process-local emergency
/// switch, and setting it to a typo reads as `disabled`, which is the safe
/// direction for a kill switch to fail. When it is unset, the YAML
/// `verification.mode` owns the deployment decision.
pub fn activation_from_env_or_config(config_mode: Option<&str>) -> VerificationActivation {
    match std::env::var("MAGICIAN_VERIFICATION_CONTROLLER") {
        Ok(raw) => VerificationActivation::from_config(Some(&raw)),
        Err(_) => VerificationActivation::from_config(config_mode),
    }
}

/// Build the gate origin from proposal/delegation provenance.
///
/// `engineer_agent_id` comes from the provenance attached to the candidate
/// snapshot, **not** from the child's own terminal event — the child is not
/// authoritative about who owned the work.
pub fn origin_from_provenance(
    engineer_agent_id: Option<String>,
    coding_profile: Option<String>,
    coding_engine: Option<String>,
    coding_invocation_ref: Option<String>,
    child_execution_id: Option<String>,
    constraint_auto: bool,
) -> Option<GateOrigin> {
    let engineer_agent_id = engineer_agent_id?;
    if engineer_agent_id.trim().is_empty() {
        return None;
    }
    Some(GateOrigin {
        engineer_agent_id,
        coding_profile,
        coding_engine,
        constraint_auto,
        coding_invocation_ref,
        child_execution_id,
    })
}

/// Newest coding-event engine label, if any. Used as failed-candidate
/// provenance, not as a repair pin.
pub fn last_engine_from_coding_events(events: &[serde_json::Value]) -> Option<String> {
    events.iter().rev().find_map(|event| {
        event
            .get("payload")
            .and_then(|payload| payload.get("engine"))
            .or_else(|| event.get("engine"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

/// Log line for an observe-mode run: what *would* have been held.
pub fn observe_note(gate_id: Option<&GateId>, facts: &CandidateFacts) -> String {
    format!(
        "[VERIFICATION] observe-mode: candidate success (root={}, produced_code={}, application={:?}) \
         would have been held{}",
        facts.is_root,
        facts.produced_code,
        facts.application,
        gate_id
            .map(|id| format!(" as gate {id}"))
            .unwrap_or_default()
    )
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

    fn hold_request() -> HoldRequest {
        HoldRequest {
            scope: scope(),
            project_binding: "proj-a".into(),
            root_task_id: "task-1".into(),
            root_execution_id: "exec-1".into(),
            candidate: CandidateRevision::new("ccp-1", 1).unwrap(),
            origin: GateOrigin {
                engineer_agent_id: "engineer".into(),
                coding_profile: None,
                coding_engine: Some("pi".into()),
                constraint_auto: false,
                coding_invocation_ref: Some("opaque".into()),
                child_execution_id: Some("exec-child".into()),
            },
        }
    }

    fn gatable() -> CandidateFacts {
        CandidateFacts {
            is_terminal: true,
            is_success: true,
            is_root: true,
            produced_code: true,
            application: Some(CandidateApplication::ProposalApplied),
        }
    }

    #[test]
    fn disabled_activation_always_proceeds() {
        // The regression bar: with the controller off, no input shape can
        // change the terminal path.
        for facts in [
            gatable(),
            CandidateFacts {
                application: Some(CandidateApplication::ProposalPartiallyApplied),
                ..gatable()
            },
            CandidateFacts {
                is_root: false,
                ..gatable()
            },
        ] {
            assert_eq!(
                evaluate(VerificationActivation::Disabled, &facts, || Some(
                    hold_request()
                )),
                GateDecision::Proceed
            );
        }
    }

    #[test]
    fn observe_classifies_so_the_gate_can_record_the_would_be_hold() {
        // Observe answers "which candidates would enforce have held?". If it
        // short-circuits to `Proceed` here it records nothing and the mode is
        // indistinguishable from disabled — which is what it used to do.
        assert!(matches!(
            evaluate(VerificationActivation::Observe, &gatable(), || Some(
                hold_request()
            )),
            GateDecision::Hold(_)
        ));

        // Releasing the candidate is the caller's decision, taken from
        // `gates_completion`. This function never decides completion timing.
        assert!(!VerificationActivation::Observe.gates_completion());

        // Disabled still costs exactly one branch.
        assert_eq!(
            evaluate(VerificationActivation::Disabled, &gatable(), || Some(
                hold_request()
            )),
            GateDecision::Proceed
        );
    }

    #[test]
    fn enforce_holds_a_vibedev_root_candidate_success() {
        let decision = evaluate(VerificationActivation::Enforce, &gatable(), || {
            Some(hold_request())
        });
        match decision {
            GateDecision::Hold(req) => {
                assert_eq!(req.root_task_id, "task-1");
                assert_eq!(req.origin.engineer_agent_id, "engineer");
            },
            other => panic!("expected Hold, got {other:?}"),
        }
    }

    #[test]
    fn delegated_children_are_never_gated() {
        // Gating children would starve parent reconciliation and let repair
        // children recurse into verification.
        let facts = CandidateFacts {
            is_root: false,
            ..gatable()
        };
        assert_eq!(
            evaluate(VerificationActivation::Enforce, &facts, || Some(
                hold_request()
            )),
            GateDecision::Proceed
        );
    }

    #[test]
    fn non_success_and_non_terminal_outcomes_are_not_gated() {
        for facts in [
            CandidateFacts {
                is_success: false,
                ..gatable()
            },
            CandidateFacts {
                is_terminal: false,
                ..gatable()
            },
        ] {
            assert_eq!(
                evaluate(VerificationActivation::Enforce, &facts, || Some(
                    hold_request()
                )),
                GateDecision::Proceed
            );
        }
    }

    #[test]
    fn a_root_that_produced_no_code_has_nothing_to_verify() {
        let facts = CandidateFacts {
            produced_code: false,
            ..gatable()
        };
        assert_eq!(
            evaluate(VerificationActivation::Enforce, &facts, || Some(
                hold_request()
            )),
            GateDecision::Proceed
        );
    }

    #[test]
    fn a_partially_applied_proposal_never_reaches_terminal_success() {
        let facts = CandidateFacts {
            application: Some(CandidateApplication::ProposalPartiallyApplied),
            ..gatable()
        };
        match evaluate(VerificationActivation::Enforce, &facts, || {
            Some(hold_request())
        }) {
            GateDecision::RefuseTerminalSuccess { reason, request } => {
                assert!(reason.contains("partial"));
                // Provenance must ride along, or the task is left
                // non-terminal with no gate — a hang rather than a refusal.
                assert!(
                    request.is_some(),
                    "a refusal must still be able to open a blocked_partial gate"
                );
            },
            other => panic!("expected refusal, got {other:?}"),
        }
        assert_eq!(refusal_status(), GateStatus::BlockedPartial);
    }

    #[test]
    fn a_partial_apply_is_refused_even_when_no_code_was_detected() {
        // Ordering matters: the partial check must precede the
        // produced_code short-circuit, or a partial apply could slip through
        // as a plain success.
        let facts = CandidateFacts {
            produced_code: false,
            application: Some(CandidateApplication::ProposalPartiallyApplied),
            ..gatable()
        };
        assert!(matches!(
            evaluate(VerificationActivation::Enforce, &facts, || Some(
                hold_request()
            )),
            GateDecision::RefuseTerminalSuccess { .. }
        ));
    }

    #[test]
    fn autopilot_self_apply_is_gated_like_any_other_root() {
        // Autopilot is the mode that most needs harness-owned verification,
        // because nobody is watching.
        let facts = CandidateFacts {
            application: Some(CandidateApplication::AutopilotSelfApplied),
            ..gatable()
        };
        assert!(matches!(
            evaluate(VerificationActivation::Enforce, &facts, || Some(
                hold_request()
            )),
            GateDecision::Hold(_)
        ));
    }

    #[test]
    fn unresolvable_provenance_proceeds_rather_than_breaking_completion() {
        // We gate what we can verify. A run we cannot attribute to an
        // engineer has no repair owner, and failing it would punish the task
        // for our missing metadata.
        assert_eq!(
            evaluate(VerificationActivation::Enforce, &gatable(), || None),
            GateDecision::Proceed
        );
    }

    #[test]
    fn origin_requires_a_named_engineer() {
        assert!(origin_from_provenance(None, None, None, None, None, false).is_none());
        assert!(
            origin_from_provenance(Some("  ".into()), None, None, None, None, false).is_none(),
            "a blank engineer id would leave repair with no owner"
        );
        assert!(
            origin_from_provenance(Some("eng".into()), None, None, None, None, false).is_some()
        );
    }

    #[test]
    fn last_engine_reads_the_newest_coding_event() {
        let events = vec![
            serde_json::json!({ "payload": { "engine": "pi" } }),
            serde_json::json!({ "engine": "codex_app_server" }),
        ];
        assert_eq!(
            last_engine_from_coding_events(&events).as_deref(),
            Some("codex_app_server")
        );
        assert_eq!(last_engine_from_coding_events(&[]), None);
    }

    #[test]
    fn env_kill_switch_defaults_to_disabled() {
        std::env::remove_var("MAGICIAN_VERIFICATION_CONTROLLER");
        assert_eq!(activation_from_env(), VerificationActivation::Disabled);

        std::env::set_var("MAGICIAN_VERIFICATION_CONTROLLER", "nonsense");
        assert_eq!(activation_from_env(), VerificationActivation::Disabled);

        std::env::set_var("MAGICIAN_VERIFICATION_CONTROLLER", "enforce");
        assert_eq!(activation_from_env(), VerificationActivation::Enforce);

        std::env::remove_var("MAGICIAN_VERIFICATION_CONTROLLER");
    }
}

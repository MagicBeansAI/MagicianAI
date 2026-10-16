//! Boundary C: the decision a harness cycle leaves behind.
//!
//! [`cycle_outcome`](super::cycle_outcome) records what happened to a *dispatch*
//! — whether the agent ran at all. It says nothing about what the cycle decided
//! to do next, so a harness that stopped because it was waiting on an owner and
//! one that stopped because it had finished were indistinguishable after the
//! fact, and neither carried the budget that justified the choice.
//!
//! Every harness cycle now settles on exactly one [`HarnessCycleContinuation`]
//! with a rationale naming its budget. The decision is derived from what the
//! cycle stated when it stated one, and inferred from the episode outcome when
//! it did not — inference is what makes "exactly one decision per cycle" an
//! invariant rather than an aspiration.
//!
//! **What this deliberately does not do:** suppress future scheduled cycles.
//! `request_approval` and `wait_for_evidence` end *this* cycle and forbid the
//! harness from taking another iteration on its own authority; they do not
//! silence the owner's cron. Permanently gating a lane on a decision the agent
//! made about itself is how a harness wedges itself off, and the plan asks for
//! a terminal cycle, not a self-imposed shutdown.

use serde_json::{json, Value};

use crate::magician_v2::learning::{CreateLearningEventRequest, LearningScope, LearningStore};

use super::cycle_outcome::is_harness_goal;

/// Durable event type written to the per-scope learning event log.
pub const HARNESS_CYCLE_CONTINUATION_EVENT: &str = "harness_cycle_continuation_decision";

/// The program-runtime-state field this decision lands in.
pub const LAST_CYCLE_DECISION_FIELD: &str = "last_cycle_decision";

/// What a harness cycle decided about its own continuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessCycleContinuation {
    /// Work remains and the harness may take it up on its next iteration.
    Continue,
    /// Blocked on evidence that has to arrive from elsewhere. Terminal here:
    /// spending another iteration would re-read the same absence.
    WaitForEvidence,
    /// Blocked on an owner decision. Terminal here, and the strongest reason
    /// not to iterate — the next step is not the harness's to take.
    RequestApproval,
    /// Failed or ran out of budget in a way that a later attempt may survive.
    RetryLater,
    /// Finished, or stopped in a way no later attempt should reopen.
    Stop,
}

impl HarnessCycleContinuation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::WaitForEvidence => "wait_for_evidence",
            Self::RequestApproval => "request_approval",
            Self::RetryLater => "retry_later",
            Self::Stop => "stop",
        }
    }

    /// Parse a decision the cycle stated for itself. Unknown text is rejected
    /// rather than coerced: a decision nobody defined must not silently become
    /// permission to keep running.
    pub fn from_wire(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "continue" => Some(Self::Continue),
            "wait_for_evidence" => Some(Self::WaitForEvidence),
            "request_approval" => Some(Self::RequestApproval),
            "retry_later" => Some(Self::RetryLater),
            "stop" => Some(Self::Stop),
            _ => None,
        }
    }

    /// True when the cycle ends here and the harness must not start another
    /// iteration on its own authority.
    pub const fn is_terminal_for_cycle(self) -> bool {
        matches!(
            self,
            Self::WaitForEvidence | Self::RequestApproval | Self::Stop
        )
    }

    /// True when the decision leaves an autonomous continuation available.
    /// The exact complement of [`Self::is_terminal_for_cycle`], written
    /// separately because call sites read better asking the question they mean.
    pub const fn permits_autonomous_iteration(self) -> bool {
        matches!(self, Self::Continue | Self::RetryLater)
    }

    /// True when something outside the harness — an owner, or evidence — has to
    /// move before this lane is worth running again.
    pub const fn awaits_external_input(self) -> bool {
        matches!(self, Self::WaitForEvidence | Self::RequestApproval)
    }
}

/// What the cycle spent, as far as it can be honestly measured.
///
/// Token and cost are `Option` on purpose. They are not available at the
/// post-cycle boundary today, and a budget rationale that reports `0 tokens`
/// for an unmeasured run is worse than one that admits the number is missing —
/// it reads as a free cycle. When execution telemetry carries them, fill these
/// in; until then they are absent and the rationale says so.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CycleBudgetSnapshot {
    pub elapsed_ms: Option<u64>,
    /// Consecutive failures this lane has accumulated, from the episode.
    pub retries: Option<u32>,
    /// Open loops carried by program runtime state. Absent when the decision
    /// is settled somewhere that has not loaded that state — same rule as
    /// tokens: unmeasured is not zero.
    pub open_loops: Option<usize>,
    pub pending_actions: usize,
    pub tokens: Option<u64>,
    pub cost_microunits: Option<u64>,
}

impl CycleBudgetSnapshot {
    /// The budget as a sentence, for the decision's rationale. Missing
    /// measurements are named as unmeasured rather than omitted, so a reader
    /// can tell "cheap" from "not counted".
    pub fn rationale(&self) -> String {
        let elapsed = self
            .elapsed_ms
            .map(|ms| format!("{ms}ms elapsed"))
            .unwrap_or_else(|| "elapsed unmeasured".to_string());
        let retries = self
            .retries
            .map(|count| format!("{count} prior failure(s)"))
            .unwrap_or_else(|| "failure count unmeasured".to_string());
        let tokens = self
            .tokens
            .map(|count| format!("{count} tokens"))
            .unwrap_or_else(|| "tokens unmeasured".to_string());
        let cost = self
            .cost_microunits
            .map(|micro| format!("{micro} microunits"))
            .unwrap_or_else(|| "cost unmeasured".to_string());
        let open_loops = self
            .open_loops
            .map(|count| format!("{count} open loop(s)"))
            .unwrap_or_else(|| "open loops unmeasured".to_string());
        format!(
            "{elapsed}, {retries}, {open_loops}, {} pending action(s), {tokens}, {cost}",
            self.pending_actions
        )
    }

    pub fn to_json(&self) -> Value {
        json!({
            "elapsed_ms": self.elapsed_ms,
            "retries": self.retries,
            "open_loops": self.open_loops,
            "pending_actions": self.pending_actions,
            "tokens": self.tokens,
            "cost_microunits": self.cost_microunits,
        })
    }
}

/// One cycle's settled decision, ready to persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleContinuationDecision {
    pub continuation: HarnessCycleContinuation,
    /// Why this decision, in words, including the budget that justified it.
    pub rationale: String,
    /// True when the cycle named its own decision; false when it was inferred
    /// from the outcome. Recorded because an inferred decision is weaker
    /// evidence about intent than a stated one.
    pub stated_by_cycle: bool,
    pub budget: CycleBudgetSnapshot,
}

impl CycleContinuationDecision {
    pub fn to_json(&self) -> Value {
        json!({
            "decision": self.continuation.as_str(),
            "rationale": self.rationale,
            "stated_by_cycle": self.stated_by_cycle,
            "terminal_for_cycle": self.continuation.is_terminal_for_cycle(),
            "awaits_external_input": self.continuation.awaits_external_input(),
            "budget": self.budget.to_json(),
        })
    }

    /// The patch that lands this decision in program runtime state through the
    /// existing allowlisted bookkeeping channel.
    pub fn program_state_patch(&self) -> Value {
        json!({ LAST_CYCLE_DECISION_FIELD: self.to_json() })
    }
}

/// Settle exactly one decision for a cycle.
///
/// A decision the cycle stated for itself wins, because the cycle knows why it
/// stopped. Anything else is inferred from the episode outcome, so that "every
/// cycle persists one decision" holds even for a cycle that said nothing —
/// which is every cycle written before this existed.
pub fn decide_continuation(
    stated: Option<&str>,
    outcome_kind: &str,
    budget: CycleBudgetSnapshot,
) -> CycleContinuationDecision {
    if let Some(continuation) = stated.and_then(HarnessCycleContinuation::from_wire) {
        return CycleContinuationDecision {
            continuation,
            rationale: format!(
                "cycle stated `{}` after `{outcome_kind}` ({})",
                continuation.as_str(),
                budget.rationale()
            ),
            stated_by_cycle: true,
            budget,
        };
    }

    let (continuation, because) = match outcome_kind {
        "goal_achieved" => (HarnessCycleContinuation::Stop, "the goal was achieved"),
        "partial_progress" => (
            HarnessCycleContinuation::Continue,
            "progress was made and work remains",
        ),
        // Paused carries pending actions the owner has to answer.
        "paused" => (
            HarnessCycleContinuation::RequestApproval,
            "the cycle paused with actions awaiting a decision",
        ),
        "user_intervened" => (
            HarnessCycleContinuation::Stop,
            "a person took over this lane",
        ),
        // A fresh cycle gets a fresh budget, so exhaustion is worth retrying;
        // it is not a reason to abandon the lane.
        "budget_exhausted" => (
            HarnessCycleContinuation::RetryLater,
            "this cycle exhausted its budget",
        ),
        "failed" => (
            HarnessCycleContinuation::RetryLater,
            "the cycle failed in a way a later attempt may survive",
        ),
        // Repeated failure already tripped a breaker; hammering it again is
        // how a broken lane burns budget forever.
        "circuit_open" => (
            HarnessCycleContinuation::Stop,
            "the failure breaker is open",
        ),
        _ => (
            HarnessCycleContinuation::RetryLater,
            "the outcome was not recognised, so no autonomous continuation is assumed",
        ),
    };

    CycleContinuationDecision {
        rationale: format!(
            "inferred `{}` from `{outcome_kind}`: {because} ({})",
            continuation.as_str(),
            budget.rationale()
        ),
        continuation,
        stated_by_cycle: false,
        budget,
    }
}

/// Best-effort: record the cycle's continuation decision durably.
///
/// Self-filters to the harness lane, like its dispatch sibling, and never
/// blocks the pipeline on failure — governance observability must not be able
/// to break the loop it governs.
#[allow(clippy::too_many_arguments)]
pub fn record_cycle_continuation(
    store: &LearningStore,
    principal: &str,
    workspace: &str,
    agent_id: &str,
    goal_id: &str,
    cycle_id: Option<&str>,
    trigger_seq: Option<u64>,
    decision: &CycleContinuationDecision,
) {
    if !is_harness_goal(goal_id) {
        return;
    }
    let scope = LearningScope::new(principal, workspace);
    let request = CreateLearningEventRequest {
        principal: None,
        workspace: None,
        event_type: HARNESS_CYCLE_CONTINUATION_EVENT.to_string(),
        agent_id: Some(agent_id.to_string()),
        task_id: None,
        execution_id: None,
        chat_session_id: None,
        summary: format!(
            "Harness cycle decided `{}` ({}:{})",
            decision.continuation.as_str(),
            agent_id,
            goal_id
        ),
        evidence_refs: Vec::new(),
        payload: json!({
            "agent_id": agent_id,
            "goal_id": goal_id,
            "cycle_id": cycle_id,
            "trigger_seq": trigger_seq,
            "continuation": decision.to_json(),
        }),
    };
    if let Err(error) = store.append_event(scope, request) {
        tracing::warn!(
            agent_id = %agent_id,
            goal_id = %goal_id,
            decision = decision.continuation.as_str(),
            error = %error,
            "Failed to record harness cycle continuation decision"
        );
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn budget() -> CycleBudgetSnapshot {
        CycleBudgetSnapshot {
            elapsed_ms: Some(1_500),
            retries: Some(2),
            open_loops: Some(3),
            pending_actions: 1,
            tokens: None,
            cost_microunits: None,
        }
    }

    #[test]
    fn wire_names_are_stable_distinct_and_round_trip() {
        let all = [
            HarnessCycleContinuation::Continue,
            HarnessCycleContinuation::WaitForEvidence,
            HarnessCycleContinuation::RequestApproval,
            HarnessCycleContinuation::RetryLater,
            HarnessCycleContinuation::Stop,
        ];
        let mut seen = std::collections::HashSet::new();
        for decision in all {
            assert!(seen.insert(decision.as_str()), "duplicate {decision:?}");
            assert_eq!(
                HarnessCycleContinuation::from_wire(decision.as_str()),
                Some(decision)
            );
        }
        assert_eq!(
            HarnessCycleContinuation::from_wire("  REQUEST_APPROVAL "),
            Some(HarnessCycleContinuation::RequestApproval)
        );
    }

    /// An unrecognised decision must not become permission to keep running.
    #[test]
    fn an_unknown_stated_decision_is_refused_and_falls_back_to_inference() {
        assert_eq!(HarnessCycleContinuation::from_wire("carry_on"), None);
        let decision = decide_continuation(Some("carry_on"), "failed", budget());
        assert!(!decision.stated_by_cycle);
        assert_eq!(decision.continuation, HarnessCycleContinuation::RetryLater);
    }

    /// The plan's core rule: neither waiting decision may take another
    /// autonomous iteration.
    #[test]
    fn waiting_decisions_are_terminal_and_never_permit_another_iteration() {
        for decision in [
            HarnessCycleContinuation::WaitForEvidence,
            HarnessCycleContinuation::RequestApproval,
        ] {
            assert!(decision.is_terminal_for_cycle());
            assert!(!decision.permits_autonomous_iteration());
            assert!(decision.awaits_external_input());
        }
        assert!(HarnessCycleContinuation::Stop.is_terminal_for_cycle());
        assert!(!HarnessCycleContinuation::Stop.awaits_external_input());
        for decision in [
            HarnessCycleContinuation::Continue,
            HarnessCycleContinuation::RetryLater,
        ] {
            assert!(!decision.is_terminal_for_cycle());
            assert!(decision.permits_autonomous_iteration());
        }
    }

    /// Terminal and permitted are exact complements — a decision that is
    /// neither, or both, would let a cycle fall through the governance rule.
    #[test]
    fn every_decision_is_exactly_one_of_terminal_or_continuing() {
        for decision in [
            HarnessCycleContinuation::Continue,
            HarnessCycleContinuation::WaitForEvidence,
            HarnessCycleContinuation::RequestApproval,
            HarnessCycleContinuation::RetryLater,
            HarnessCycleContinuation::Stop,
        ] {
            assert_ne!(
                decision.is_terminal_for_cycle(),
                decision.permits_autonomous_iteration(),
                "{decision:?} is both or neither"
            );
        }
    }

    #[test]
    fn a_stated_decision_wins_over_the_outcome_it_disagrees_with() {
        let decision = decide_continuation(Some("wait_for_evidence"), "partial_progress", budget());
        assert!(decision.stated_by_cycle);
        assert_eq!(
            decision.continuation,
            HarnessCycleContinuation::WaitForEvidence
        );
        assert!(decision.rationale.contains("cycle stated"));
    }

    #[test]
    fn every_outcome_infers_a_decision_so_no_cycle_settles_undecided() {
        let cases = [
            ("goal_achieved", HarnessCycleContinuation::Stop),
            ("partial_progress", HarnessCycleContinuation::Continue),
            ("paused", HarnessCycleContinuation::RequestApproval),
            ("user_intervened", HarnessCycleContinuation::Stop),
            ("budget_exhausted", HarnessCycleContinuation::RetryLater),
            ("failed", HarnessCycleContinuation::RetryLater),
            ("circuit_open", HarnessCycleContinuation::Stop),
            ("something_new", HarnessCycleContinuation::RetryLater),
        ];
        for (outcome, expected) in cases {
            let decision = decide_continuation(None, outcome, budget());
            assert_eq!(decision.continuation, expected, "outcome `{outcome}`");
            assert!(!decision.stated_by_cycle);
            assert!(decision.rationale.contains(outcome));
        }
    }

    /// Deliverable 2: the decision carries its budget, and says plainly when a
    /// number was never measured rather than reporting a free cycle.
    #[test]
    fn the_rationale_names_the_budget_including_what_was_not_measured() {
        let decision = decide_continuation(None, "partial_progress", budget());
        assert!(decision.rationale.contains("1500ms elapsed"));
        assert!(decision.rationale.contains("2 prior failure(s)"));
        assert!(decision.rationale.contains("3 open loop(s)"));
        assert!(decision.rationale.contains("tokens unmeasured"));
        assert!(decision.rationale.contains("cost unmeasured"));

        let measured = decide_continuation(
            None,
            "partial_progress",
            CycleBudgetSnapshot {
                tokens: Some(4_096),
                cost_microunits: Some(120),
                ..budget()
            },
        );
        assert!(measured.rationale.contains("4096 tokens"));
        assert!(measured.rationale.contains("120 microunits"));
        assert!(!measured.rationale.contains("unmeasured"));
    }

    #[test]
    fn the_persisted_shape_carries_the_governance_facts() {
        let decision = decide_continuation(Some("request_approval"), "paused", budget());
        let json = decision.to_json();
        assert_eq!(json["decision"], "request_approval");
        assert_eq!(json["terminal_for_cycle"], true);
        assert_eq!(json["awaits_external_input"], true);
        assert_eq!(json["stated_by_cycle"], true);
        assert_eq!(json["budget"]["open_loops"], 3);
        assert!(json["budget"]["tokens"].is_null());

        let patch = decision.program_state_patch();
        assert!(patch.get(LAST_CYCLE_DECISION_FIELD).is_some());
    }
}

//! Structured, durable observability for harness cron-cycle dispatch.
//!
//! The scheduler -> dispatch -> execute chain historically dropped harness
//! cycles at several points with only a `debug!` line, so a harness agent that
//! "fired 42x but recorded 0 episodes" was unattributable. Every meaningful exit
//! now records a durable `harness_cycle_dispatch_outcome` learning event so the
//! self-improvement loop is observable (read back via the harness-cycles
//! endpoint). Recording is gated to the harness lane (`goal_id` prefixed
//! `harness:`) so ordinary chat/worker/autonomous cycles do not flood the log.

use serde_json::Value;

use crate::magician_v2::learning::{CreateLearningEventRequest, LearningScope, LearningStore};

/// Durable event type written to the per-scope learning event log.
pub const HARNESS_CYCLE_DISPATCH_OUTCOME_EVENT: &str = "harness_cycle_dispatch_outcome";

/// goal_id prefix that marks a harness self-management cycle
/// (format `harness:<agent>:<focus-slug>`, see `agents::autonomous_goal`).
const HARNESS_GOAL_PREFIX: &str = "harness:";

/// The outcome of one scheduled harness cron-cycle dispatch attempt.
///
/// `Dispatched` means the agentic run was actually launched; every `Dropped*`
/// variant is a silent early-return that previously left no trace. The
/// `Episode*` variants record whether the post-cycle pipeline persisted an
/// episode (the absence of which triggered this whole investigation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessCycleDispatchOutcome {
    Dispatched,
    DroppedDisabled,
    DroppedPaused,
    DroppedDefinitionStale,
    DroppedReservationChanged,
    DroppedProvisionFailed,
    /// Launched, but the spawned cycle task deduped against an episode that
    /// already exists for this exact `(agent, goal, trigger_seq)` tuple — a
    /// legitimate idempotency skip that previously looked like "0 episodes".
    DroppedDuplicate,
    /// Launched, but the spawned cycle task found no exact runtime execution id
    /// bound to the reservation, so it abandoned before running the agent.
    DroppedNoExecutionId,
    EpisodePersisted,
    EpisodePersistFailed,
}

impl HarnessCycleDispatchOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dispatched => "dispatched",
            Self::DroppedDisabled => "dropped_disabled",
            Self::DroppedPaused => "dropped_paused",
            Self::DroppedDefinitionStale => "dropped_definition_stale",
            Self::DroppedReservationChanged => "dropped_reservation_changed",
            Self::DroppedProvisionFailed => "dropped_provision_failed",
            Self::DroppedDuplicate => "dropped_duplicate",
            Self::DroppedNoExecutionId => "dropped_no_execution_id",
            Self::EpisodePersisted => "episode_persisted",
            Self::EpisodePersistFailed => "episode_persist_failed",
        }
    }

    /// True when the outcome means the cycle never executed the agent.
    pub const fn is_drop(self) -> bool {
        matches!(
            self,
            Self::DroppedDisabled
                | Self::DroppedPaused
                | Self::DroppedDefinitionStale
                | Self::DroppedReservationChanged
                | Self::DroppedProvisionFailed
                | Self::DroppedDuplicate
                | Self::DroppedNoExecutionId
        )
    }
}

/// True when this goal belongs to the harness self-management lane.
pub fn is_harness_goal(goal_id: &str) -> bool {
    goal_id.starts_with(HARNESS_GOAL_PREFIX)
}

/// Best-effort: record a durable harness cycle dispatch outcome.
///
/// Self-filters to the harness lane (no-op for non-`harness:` goals) so the
/// event log only carries self-improvement cycles. Never panics and never
/// blocks the dispatch path on failure (logs + moves on) — observability must
/// not be able to break the loop it observes.
#[allow(clippy::too_many_arguments)]
pub fn record_harness_cycle_outcome(
    store: &LearningStore,
    principal: &str,
    workspace: &str,
    agent_id: &str,
    goal_id: &str,
    cycle_id: Option<&str>,
    trigger_seq: Option<u64>,
    outcome: HarnessCycleDispatchOutcome,
    detail: Value,
) {
    if !is_harness_goal(goal_id) {
        return;
    }
    let scope = LearningScope::new(principal, workspace);
    let request = CreateLearningEventRequest {
        principal: None,
        workspace: None,
        event_type: HARNESS_CYCLE_DISPATCH_OUTCOME_EVENT.to_string(),
        agent_id: Some(agent_id.to_string()),
        task_id: None,
        execution_id: None,
        chat_session_id: None,
        summary: format!(
            "Harness cycle {} ({}:{})",
            outcome.as_str(),
            agent_id,
            goal_id
        ),
        evidence_refs: Vec::new(),
        payload: serde_json::json!({
            "outcome": outcome.as_str(),
            "is_drop": outcome.is_drop(),
            "agent_id": agent_id,
            "goal_id": goal_id,
            "cycle_id": cycle_id,
            "trigger_seq": trigger_seq,
            "detail": detail,
        }),
    };
    if let Err(error) = store.append_event(scope, request) {
        tracing::warn!(
            agent_id = %agent_id,
            goal_id = %goal_id,
            outcome = outcome.as_str(),
            error = %error,
            "Failed to record harness cycle dispatch outcome event"
        );
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn outcome_as_str_is_stable_and_distinct() {
        let all = [
            HarnessCycleDispatchOutcome::Dispatched,
            HarnessCycleDispatchOutcome::DroppedDisabled,
            HarnessCycleDispatchOutcome::DroppedPaused,
            HarnessCycleDispatchOutcome::DroppedDefinitionStale,
            HarnessCycleDispatchOutcome::DroppedReservationChanged,
            HarnessCycleDispatchOutcome::DroppedProvisionFailed,
            HarnessCycleDispatchOutcome::DroppedDuplicate,
            HarnessCycleDispatchOutcome::DroppedNoExecutionId,
            HarnessCycleDispatchOutcome::EpisodePersisted,
            HarnessCycleDispatchOutcome::EpisodePersistFailed,
        ];
        let mut seen = std::collections::HashSet::new();
        for outcome in all {
            assert!(
                seen.insert(outcome.as_str()),
                "duplicate as_str for {outcome:?}"
            );
        }
        assert!(!HarnessCycleDispatchOutcome::Dispatched.is_drop());
        assert!(!HarnessCycleDispatchOutcome::EpisodePersisted.is_drop());
        assert!(HarnessCycleDispatchOutcome::DroppedReservationChanged.is_drop());
        assert!(HarnessCycleDispatchOutcome::DroppedDuplicate.is_drop());
        assert!(HarnessCycleDispatchOutcome::DroppedNoExecutionId.is_drop());
    }

    #[test]
    fn only_harness_goals_are_observed() {
        assert!(is_harness_goal("harness:ceo:morning-briefing"));
        assert!(!is_harness_goal("autonomous:ceo:foo"));
        assert!(!is_harness_goal("task_1234"));
        assert!(!is_harness_goal(""));
    }
}

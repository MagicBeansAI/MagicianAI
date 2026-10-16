pub mod anomaly;
pub mod anomaly_store;
pub mod autofix;
pub mod backlog;
pub mod backlog_store;
pub mod config;
pub mod cycle_continuation;
pub mod cycle_outcome;
pub mod program;
pub mod program_doc;
pub mod registration;
pub mod scope;
pub mod supplemental_profile;
pub mod supplemental_profile_store;
pub mod traces;

/// One lock per harness record file, for the life of the process.
///
/// `BacklogStore` and `AnomalyStore` both mutate a record by reading
/// `{id}.json`, changing a field, and writing the whole thing back. Both are
/// constructed per request and hold nothing but a workspace, so there was no
/// lock anywhere and nothing ordered those three steps. Two handlers touching
/// the same item — a promote racing a review, an anomaly resolve racing a
/// re-report — each read the same record and each wrote its own version back.
/// The later write wins wholesale, so the earlier handler's field is gone while
/// its call returned success.
///
/// Keyed by **record path**, not by scope: these are per-item files, so two
/// different items never contend. That matters here because
/// `AnomalyStore::resolve_open_for_agent_goal` lists and then mutates N records
/// in a loop — a scope-wide lock would serialise the whole sweep against every
/// other harness write.
///
/// A `std::sync::Mutex` because every one of these paths is synchronous;
/// nothing awaits while it is held. One process owns a data root (decided
/// 2026-08-12), which is what makes an in-process lock the whole fix.
pub fn harness_record_lock(path: &std::path::Path) -> std::sync::Arc<std::sync::Mutex<()>> {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<
            std::collections::HashMap<std::path::PathBuf, std::sync::Arc<std::sync::Mutex<()>>>,
        >,
    > = std::sync::OnceLock::new();
    let mut registry = LOCKS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::sync::Arc::clone(registry.entry(path.to_path_buf()).or_default())
}

pub const LIST_EPISODES_TOOL_NAME: &str = "list_episodes";
pub const READ_TRACE_TOOL_NAME: &str = "read_trace";
pub const LIST_AGENTS_TOOL_NAME: &str = "list_agents";
pub const INSPECT_AGENT_TOOL_NAME: &str = "inspect_agent";
pub const SYSTEM_STATUS_TOOL_NAME: &str = "system_status";
pub const EVALUATE_HARNESS_TOOL_NAME: &str = "evaluate_harness";
pub const READ_PROGRAM_STATE_TOOL_NAME: &str = "read_program_state";
pub const LIST_PROPOSALS_TOOL_NAME: &str = "list_proposals";
pub const INSPECT_BACKLOG_DELIVERY_TOOL_NAME: &str = "inspect_backlog_delivery";
/// On-demand recall of prior work-outcomes: reads the per-agent evidence store
/// filtered to `producer == "work_outcome"` (the deterministic run-grained
/// ledger stamped by terminal agentic runs). A direct store read — NOT a
/// semantic/hybrid-ranked memory lane.
pub const WORK_LEDGER_TOOL_NAME: &str = "magician_work_ledger";
pub const CREATE_TASK_TOOL_NAME: &str = "create_task";
pub const REASSIGN_TASK_TOOL_NAME: &str = "reassign_task";
pub const CREATE_AGENT_TOOL_NAME: &str = "create_agent";
pub const UPDATE_AGENT_TOOL_NAME: &str = "update_agent";
pub const RETIRE_AGENT_TOOL_NAME: &str = "retire_agent";
pub const UPDATE_DELEGATION_TOOL_NAME: &str = "update_delegation";
pub const CREATE_PROPOSAL_TOOL_NAME: &str = "create_proposal";
pub const CREATE_DASHBOARD_TOOL_NAME: &str = "create_dashboard";
pub const UPDATE_PROGRAM_STATE_TOOL_NAME: &str = "update_program_state";
pub const PROPOSE_BACKLOG_ITEM_TOOL_NAME: &str = "propose_backlog_item";
pub const PROMOTE_BACKLOG_ITEM_TOOL_NAME: &str = "promote_backlog_item";
pub const REVIEW_BACKLOG_DELIVERY_TOOL_NAME: &str = "review_backlog_delivery";
// NOTE: notify_owner + the envoy owner-consent tools (ask_owner /
// propose_meeting / request_owner_action) are NOT harness tools — they only
// emit a UserRequest, with no HarnessScope / program-state / governance gate.
// They live on the compiled-handler path (execution/compiled_handlers/) so they
// dispatch in the reactive chat path. See the taxonomy in
// docs/plans/2026-06-09-kapso-envoy-loop-plan.md (Phase B).

pub const HARNESS_READ_TOOL_NAMES: &[&str] = &[
    LIST_EPISODES_TOOL_NAME,
    READ_TRACE_TOOL_NAME,
    LIST_AGENTS_TOOL_NAME,
    INSPECT_AGENT_TOOL_NAME,
    SYSTEM_STATUS_TOOL_NAME,
    EVALUATE_HARNESS_TOOL_NAME,
    READ_PROGRAM_STATE_TOOL_NAME,
    LIST_PROPOSALS_TOOL_NAME,
    WORK_LEDGER_TOOL_NAME,
    INSPECT_BACKLOG_DELIVERY_TOOL_NAME,
];

pub const HARNESS_ACTION_TOOL_NAMES: &[&str] = &[
    CREATE_TASK_TOOL_NAME,
    REASSIGN_TASK_TOOL_NAME,
    CREATE_AGENT_TOOL_NAME,
    UPDATE_AGENT_TOOL_NAME,
    RETIRE_AGENT_TOOL_NAME,
    UPDATE_DELEGATION_TOOL_NAME,
    CREATE_PROPOSAL_TOOL_NAME,
    CREATE_DASHBOARD_TOOL_NAME,
    UPDATE_PROGRAM_STATE_TOOL_NAME,
    PROPOSE_BACKLOG_ITEM_TOOL_NAME,
    PROMOTE_BACKLOG_ITEM_TOOL_NAME,
    REVIEW_BACKLOG_DELIVERY_TOOL_NAME,
];

/// Harness ACTION tools whose effects are consequential enough to require OWNER
/// APPROVAL before they apply, even inside an autonomous cycle (roster + program
/// self-mutation). These dispatch per-scope like the rest of the action set, but
/// a central `requires_approval` rule (see `harness_mutation_approval_rules`)
/// makes the executor's ApprovalGate pause them for the owner. Deliberately
/// EXCLUDES create_task/reassign_task (normal work assignment), create_proposal
/// (already the owner-review path), create_dashboard (benign), and the backlog
/// tools (propose = a suggestion; promote's coding still hits diff-approval).
/// Also EXCLUDES update_program_state: that is routine durable progress/state
/// maintenance for a harness program, not a structural mutation.
pub const HARNESS_APPROVAL_GATED_TOOL_NAMES: &[&str] = &[
    CREATE_AGENT_TOOL_NAME,
    UPDATE_AGENT_TOOL_NAME,
    RETIRE_AGENT_TOOL_NAME,
    UPDATE_DELEGATION_TOOL_NAME,
];

pub const HARNESS_TOOL_NAMES: &[&str] = &[
    LIST_EPISODES_TOOL_NAME,
    READ_TRACE_TOOL_NAME,
    LIST_AGENTS_TOOL_NAME,
    INSPECT_AGENT_TOOL_NAME,
    SYSTEM_STATUS_TOOL_NAME,
    EVALUATE_HARNESS_TOOL_NAME,
    READ_PROGRAM_STATE_TOOL_NAME,
    LIST_PROPOSALS_TOOL_NAME,
    WORK_LEDGER_TOOL_NAME,
    INSPECT_BACKLOG_DELIVERY_TOOL_NAME,
    CREATE_TASK_TOOL_NAME,
    REASSIGN_TASK_TOOL_NAME,
    CREATE_AGENT_TOOL_NAME,
    UPDATE_AGENT_TOOL_NAME,
    RETIRE_AGENT_TOOL_NAME,
    UPDATE_DELEGATION_TOOL_NAME,
    CREATE_PROPOSAL_TOOL_NAME,
    CREATE_DASHBOARD_TOOL_NAME,
    UPDATE_PROGRAM_STATE_TOOL_NAME,
    PROPOSE_BACKLOG_ITEM_TOOL_NAME,
    PROMOTE_BACKLOG_ITEM_TOOL_NAME,
    REVIEW_BACKLOG_DELIVERY_TOOL_NAME,
];

pub fn is_harness_tool_name(name: &str) -> bool {
    HARNESS_TOOL_NAMES.contains(&name)
}

pub use anomaly::{AnomalyKind, AnomalyStatus, HarnessAnomaly};
pub use anomaly_store::AnomalyStore;
pub use autofix::{autofix_env_override, select_autofix_targets};
pub use backlog::{
    BacklogDeliveryDisposition, BacklogDeliveryReview, BacklogItem, BacklogPriority, BacklogStatus,
};
pub use backlog_store::BacklogStore;
pub use config::{HarnessExecutionContext, HarnessServices, ScopedPausedAgentsIndex};
pub use cycle_continuation::{
    decide_continuation, record_cycle_continuation, CycleBudgetSnapshot, CycleContinuationDecision,
    HarnessCycleContinuation, HARNESS_CYCLE_CONTINUATION_EVENT, LAST_CYCLE_DECISION_FIELD,
};
pub use cycle_outcome::{
    record_harness_cycle_outcome, HarnessCycleDispatchOutcome, HARNESS_CYCLE_DISPATCH_OUTCOME_EVENT,
};
pub use program::{LoadedProgram, ProgramLoader, ProgramRuntimeState};
pub use registration::{
    harness_tool_grant_for_definition, reflects_program_runtime_state, HarnessToolGrant,
    COORDINATOR_ESCALATION_TOOL_NAME,
};
pub use scope::{HarnessScope, HarnessScopeTarget};
pub use supplemental_profile::{
    HarnessSupplementalProfile, ProfileRevisionError, ProfileRevisionProposal,
    ProfileRevisionRecord, ALLOWED_SECTIONS,
};
pub use supplemental_profile_store::{ApplyRevisionError, SupplementalProfileStore};
pub use traces::{HarnessTraceReader, LoadedTrace, TraceBlob};

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn work_ledger_tool_is_a_granted_harness_read_tool() {
        assert!(
            HARNESS_READ_TOOL_NAMES.contains(&WORK_LEDGER_TOOL_NAME),
            "work_ledger must be a harness READ tool (auto-registered read provider)"
        );
        assert!(
            HARNESS_TOOL_NAMES.contains(&WORK_LEDGER_TOOL_NAME),
            "work_ledger must be in HARNESS_TOOL_NAMES so it is auto-granted to harness agents"
        );
        assert!(
            is_harness_tool_name(WORK_LEDGER_TOOL_NAME),
            "work_ledger must be recognized as a harness tool so it is stripped from non-harness agents"
        );
        assert!(
            !HARNESS_ACTION_TOOL_NAMES.contains(&WORK_LEDGER_TOOL_NAME),
            "work_ledger is read-only and must not be an action tool"
        );
    }

    #[test]
    fn backlog_delivery_tools_are_granted_in_their_correct_lanes() {
        assert!(HARNESS_READ_TOOL_NAMES.contains(&INSPECT_BACKLOG_DELIVERY_TOOL_NAME));
        assert!(HARNESS_ACTION_TOOL_NAMES.contains(&REVIEW_BACKLOG_DELIVERY_TOOL_NAME));
        assert!(HARNESS_TOOL_NAMES.contains(&INSPECT_BACKLOG_DELIVERY_TOOL_NAME));
        assert!(HARNESS_TOOL_NAMES.contains(&REVIEW_BACKLOG_DELIVERY_TOOL_NAME));
        assert!(!HARNESS_ACTION_TOOL_NAMES.contains(&INSPECT_BACKLOG_DELIVERY_TOOL_NAME));
    }

    #[test]
    fn approval_gated_tools_are_a_dispatchable_action_subset_disjoint_from_backlog() {
        // Every owner-approval-gated mutation tool must also be a dispatchable
        // action tool (it can only be gated if it can dispatch), so a future
        // tool rename can't silently un-gate a mutation.
        for tool in HARNESS_APPROVAL_GATED_TOOL_NAMES {
            assert!(
                HARNESS_ACTION_TOOL_NAMES.contains(tool),
                "gated mutation tool {tool} must be in HARNESS_ACTION_TOOL_NAMES so it dispatches per-scope"
            );
        }
        // The backlog tools dispatch ungated (propose = a suggestion; promote's
        // coding still hits diff-approval) — they must never leak into the gate.
        assert!(
            !HARNESS_APPROVAL_GATED_TOOL_NAMES.contains(&PROPOSE_BACKLOG_ITEM_TOOL_NAME),
            "propose_backlog_item must not be owner-approval-gated"
        );
        assert!(
            !HARNESS_APPROVAL_GATED_TOOL_NAMES.contains(&PROMOTE_BACKLOG_ITEM_TOOL_NAME),
            "promote_backlog_item must not be owner-approval-gated"
        );
        assert!(
            !HARNESS_APPROVAL_GATED_TOOL_NAMES.contains(&REVIEW_BACKLOG_DELIVERY_TOOL_NAME),
            "review_backlog_delivery is routine backlog bookkeeping and must not be owner-approval-gated"
        );
        // create_task/reassign_task are normal work assignment — never gated.
        assert!(
            !HARNESS_APPROVAL_GATED_TOOL_NAMES.contains(&CREATE_TASK_TOOL_NAME),
            "create_task is normal work assignment and must not be gated"
        );
    }
}

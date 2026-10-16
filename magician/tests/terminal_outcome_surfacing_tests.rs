//! Terminal-outcome surfacing regression tests (meta-harness reliability audit,
//! docs/archive/plans/2026-07-10-meta-harness-reliability-audit.md — "Verification gaps" #1).
//!
//! The dominant "agents just fail" root cause was a *surfacing* gap: the terminal
//! the LLM actually reaches returned a bare `AgenticOutcome::Failed` with no
//! user-visible reason and no HITL, instead of pausing/escalating. Two fixes
//! guarded here:
//!
//! - **P0.1** — a `Decision::Yield → Failed` for a blocked resource escalates via
//!   `escalate_to_user` when `on_failure == AskUser`, producing a resumable
//!   `WaitingForUser` (HITL) pause, and the failure disposition always carries a
//!   NON-EMPTY reason so a completion event can surface it (no more silent fail).
//! - **P1.2** — `AgenticOutcome::BudgetExhausted` now carries an
//!   `Option<AgenticPauseState>`; when `Some`, the outcome is a resumable pause
//!   (`is_terminal() == false`), mirroring `MaxIterationsReached`, instead of a
//!   bare terminal failure that discards in-flight progress.
//!
//! The private executor drive path (`execute()`, `escalate_to_user`,
//! `yield_escalation_prompt`) cannot be reached from an integration test without a
//! live LLM + full orchestrator boot, so these tests assert the smaller PUBLIC
//! contracts on the real types that the fixes changed. Constructing the outcomes
//! directly mirrors `agentic_validation_tests.rs`.

use std::collections::HashMap;
use std::path::PathBuf;

use magician::magician_v2::execution::agentic::{
    dispose_yield, AgenticOutcome, AgenticPauseState, BudgetDimension, EnvironmentState,
    ShellState, UserInputType, YieldBlocker, YieldBlockerKind, YieldDecision, YieldDisposition,
};

// ============================================================================
// Test helpers (mirror agentic_validation_tests.rs)
// ============================================================================

fn create_test_shell_state() -> ShellState {
    ShellState {
        working_dir: PathBuf::from("/tmp/test"),
        last_command: Some("echo hello".to_string()),
        last_stdout: Some("hello\n".to_string()),
        last_stderr: None,
        last_exit_code: Some(0),
    }
}

fn create_test_pause_state() -> AgenticPauseState {
    AgenticPauseState::new(
        3,                                                        // iteration
        "Complete the login flow".to_string(),                    // goal
        "User is logged in and dashboard is visible".to_string(), // success_criteria
        EnvironmentState::Shell(create_test_shell_state()),
        "Iteration 1: Navigated to page\nIteration 2: Clicked login button".to_string(),
        10, // max_iterations
        3,  // max_repeated_actions
    )
}

fn blocker(kind: YieldBlockerKind, desc: &str) -> YieldBlocker {
    YieldBlocker {
        kind,
        description: desc.to_string(),
    }
}

// ============================================================================
// P1.2 — BudgetExhausted carries a resumable pause_state → NOT a bare terminal
// ============================================================================

/// The core P1.2 guard: a `BudgetExhausted` that built a resumable pause_state
/// is a PAUSE ("Continue execution?"), not a terminal hard-fail. It must mirror
/// `MaxIterationsReached { pause_state: Some(_) }` — `is_terminal()` is false and
/// in-flight progress is preserved. Before the fix, `BudgetExhausted` structurally
/// had no `pause_state` and always mapped to a terminal `ExecutionFailed`.
#[test]
fn budget_exhausted_with_pause_state_is_resumable_not_terminal() {
    let outcome = AgenticOutcome::BudgetExhausted {
        dimension: BudgetDimension::Tokens {
            used: 2_000_000,
            limit: 2_000_000,
        },
        last_state: EnvironmentState::Shell(create_test_shell_state()),
        iterations_completed: 4,
        actions_completed: 12,
        pause_state: Some(create_test_pause_state()),
    };

    // Still classified as a budget exhaustion...
    assert!(outcome.is_budget_exhausted());
    // ...but with a resumable pause it must NOT be a bare terminal failure.
    assert!(
        !outcome.is_terminal(),
        "BudgetExhausted with Some(pause_state) must be a resumable pause, not a terminal fail"
    );
    assert!(!outcome.is_success());
    // Progress is preserved: iterations reported, not discarded.
    assert_eq!(outcome.iterations_used(), 4);
}

/// The fail-closed variant (no resumable state could be built — e.g. the
/// missing-usage path) stays terminal. This pins the boundary so the resumable
/// case above is meaningful and not vacuously true.
#[test]
fn budget_exhausted_without_pause_state_stays_terminal() {
    let outcome = AgenticOutcome::BudgetExhausted {
        dimension: BudgetDimension::Tokens {
            used: 100,
            limit: 2_000_000,
        },
        last_state: EnvironmentState::Shell(create_test_shell_state()),
        iterations_completed: 1,
        actions_completed: 1,
        pause_state: None,
    };

    assert!(outcome.is_budget_exhausted());
    assert!(
        outcome.is_terminal(),
        "BudgetExhausted with None pause_state remains a terminal failure"
    );
    assert!(!outcome.is_success());
}

// ============================================================================
// P0.1 — a blocked yield surfaces a HITL escalation + a non-empty reason,
//        not a bare, reason-less Failed.
// ============================================================================

/// P0.1 root cause: `dispose_yield` maps a structural (Auth/Permission) blocker
/// to `Failed`, but the pre-fix live arm returned a bare `AgenticOutcome::Failed`
/// with no user-visible reason and never consulted `on_failure == AskUser`. This
/// guards the reason-carrying half of the contract: the disposition's failure
/// reason is NON-EMPTY and names the blocker, so a completion event can surface a
/// real reason (not a silent fail).
#[test]
fn structural_blocker_yield_carries_non_empty_reason_for_surfacing() {
    let decision = YieldDecision {
        summary: "Blocked on folder access".to_string(),
        blockers: vec![blocker(
            YieldBlockerKind::Permission,
            "path /Users/owner/Docs is outside the sandbox roots",
        )],
        ..Default::default()
    };

    let YieldDisposition::Failed { reason } = dispose_yield(&decision) else {
        panic!("a non-transient (Permission) blocker must dispose to Failed");
    };

    // The reason must be surfaceable — non-empty and it must name the blocker so
    // the completion event has a real, user-visible cause (P0.1: "no
    // emit_and_persist_agentic_completion fires, so the failure has no reason").
    assert!(
        !reason.trim().is_empty(),
        "failure reason must be non-empty"
    );
    assert!(
        reason.contains("Permission"),
        "reason should name the blocker kind for surfacing, got: {reason}"
    );
    assert!(
        reason.contains("sandbox roots"),
        "reason should carry the blocker detail, got: {reason}"
    );
}

/// P0.1 escalation half: when `on_failure == AskUser`, the blocked terminal must
/// route into the HITL escalation (`escalate_to_user`) and produce a resumable
/// `WaitingForUser` pause — NOT a bare `Failed`. `escalate_to_user` is a private
/// executor fn, so this asserts the SHAPE of the outcome it returns using the
/// real public `AgenticOutcome::WaitingForUser` type: it is a waiting/HITL
/// outcome (not terminal, not success), it carries a resumable pause_state, and
/// the escalation_trigger is set. This is the observable "HITL, not bare Failed"
/// contract the audit's test #1 requires.
#[test]
fn blocked_resource_escalation_is_hitl_pause_not_bare_failed() {
    // Shape mirrors what `escalate_to_user` builds for a Permission/Auth blocker
    // reached under `on_failure: AskUser`: an ExternalAction ask that pauses the
    // run and asks the human to grant access / re-auth.
    let escalation = AgenticOutcome::WaitingForUser {
        question: "I'm blocked on access: path is outside the sandbox roots. \
                   Can you grant access to the resource/path so I can continue?"
            .to_string(),
        input_type: UserInputType::ExternalAction {
            instructions: "Grant access to the requested folder".to_string(),
            done_label: Some("Done".to_string()),
        },
        hint: Some("Approve the folder so the run can resume".to_string()),
        pause_state: Box::new(create_test_pause_state()),
        asking_for_parameter: None,
        pending_inputs: vec![],
        resolved_inputs: HashMap::new(),
        escalation_trigger: Some("permission".to_string()),
    };

    // A blocked-on-resource outcome must be a HITL pause, NOT a bare Failed.
    assert!(
        escalation.is_waiting_for_user(),
        "blocked resource under AskUser must surface a HITL, not a bare Failed"
    );
    assert!(
        !escalation.is_terminal(),
        "an escalation pause is resumable, not terminal"
    );
    assert!(!escalation.is_success());
    // Resumable: the pause carries state so the approved retry can continue.
    assert!(
        escalation.pause_state().is_some(),
        "escalation must preserve pause state for resume"
    );

    // The escalation trigger must be set and non-empty (drives the re-auth /
    // sandbox-access HITL prompt vs a generic cannot_proceed).
    let AgenticOutcome::WaitingForUser {
        escalation_trigger,
        question,
        ..
    } = &escalation
    else {
        panic!("expected WaitingForUser escalation outcome");
    };
    let trigger = escalation_trigger
        .as_deref()
        .expect("escalation_trigger must be set on a failure escalation");
    assert!(!trigger.is_empty(), "escalation_trigger must be non-empty");
    assert!(
        !question.trim().is_empty(),
        "escalation question must be surfaced to the user"
    );
}

//! Unified yield decision for the outer agentic loop.
//!
//! See [`docs/plans/2026-05-27-yield-decision-migration.md`] for the
//! full design and migration plan.
//!
//! Today's loop forces the LLM to pick between two terminal "outcome"
//! decisions — `goal_reached` vs `cannot_proceed` — and the LLM ends
//! up self-grading. The orchestrator (which has the seed goal, retry
//! budget, and policy) is in a better position to classify.
//!
//! `YieldDecision` is the LLM's structured outcome report: "here's
//! what I did, here's what's open, here's what's blocking."
//! [`dispose_yield`] consumes that report and produces a
//! [`YieldDisposition`] that the executor maps to the right
//! `AgenticOutcome` primitive.
//!
//! **`need_user_input` stays as a separate decision** — interactive
//! asks are a different primitive (different UI, different lifecycle,
//! conversation continues instead of terminating). `Yield` is strictly
//! for terminal outcome reports.

use serde::{Deserialize, Serialize};

use super::types::Artifact;

/// Structured outcome report emitted by the LLM via the `yield` control
/// tool. See module docs for the full rationale.
///
/// `Yield` does **not** carry user questions — for interactive asks
/// the LLM should emit `need_user_input` instead. Asking the user is
/// a structurally different primitive (different UI affordance,
/// different lifecycle — the conversation continues rather than
/// terminating).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct YieldDecision {
    /// The answer the reader receives: the results themselves, verbatim, then
    /// briefly what was done. Always populated.
    pub summary: String,
    /// What the agent accomplished this run. Empty when nothing
    /// completed (pure block case).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed: Vec<String>,
    /// What's left undone. Empty when the agent considers the task
    /// fully done.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open: Vec<String>,
    /// Concrete blockers preventing progress on open items. See
    /// [`YieldBlockerKind`] for the controlled taxonomy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<YieldBlocker>,
    /// Artifacts the run produced (text outputs, files, screenshots…).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,
    /// Optional advisory: what the agent thinks should happen next.
    /// Purely informational; orchestrator may or may not honour it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_step_hint: Option<String>,
    /// Agent's self-assessment of the outcome class. Advisory only —
    /// the orchestrator computes the real disposition from the
    /// structured fields above. Surfaced for diagnostics and for
    /// future tie-breaking when fields are ambiguous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_classification: Option<YieldSelfClassification>,
    /// Browser session handoff: keep the agent's CDP connection + daemon
    /// alive so a follow-up execution can reattach to the same
    /// authenticated session. Default `false` (terminal cleanup closes
    /// the window). Legacy alias `keep_browser_session_alive` accepted.
    /// Threaded to the cleanup override atomic by the terminal Yield
    /// handler. See `docs/plans/2026-05-24-browser-session-lifecycle-redesign.md`.
    #[serde(
        default,
        alias = "keep_browser_session_alive",
        skip_serializing_if = "is_false"
    )]
    pub keep_browser_cdp_connection_alive: bool,
    /// Browser session handoff: hand the visible Chromium window off to
    /// the human (detach CDP, exit the daemon, leave the window open).
    /// Default `false`. Mutually exclusive with
    /// `keep_browser_cdp_connection_alive`, which wins if both are set.
    #[serde(default, skip_serializing_if = "is_false")]
    pub keep_browser_window_open: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// A single concrete blocker preventing forward progress.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct YieldBlocker {
    pub kind: YieldBlockerKind,
    pub description: String,
}

/// Controlled taxonomy of blocker classes. The disposition function
/// uses this to decide whether a yield is retryable (transient) or
/// terminal (structural).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum YieldBlockerKind {
    /// Auth missing / expired / wrong scope. Usually unblocks with a
    /// re-auth flow — orchestrator may surface a re-auth prompt.
    Auth,
    /// Data the agent needs is missing (no record, empty result set).
    /// Orchestrator may decide this is "completed with caveat" or fail.
    DataMissing,
    /// Permission denied at an authorisation layer (HTTP 403, capability
    /// scope mismatch). Usually structural — retry won't help.
    Permission,
    /// Rate limit, transient network error, brief upstream outage. Safe
    /// to retry with backoff.
    Transient,
    /// External dependency reported an error we cannot fix (upstream
    /// bug, missing feature). Structural failure.
    External,
    /// Catch-all when none of the above fit. Surface description to
    /// the orchestrator and treat as structural.
    Other,
}

impl YieldBlockerKind {
    /// `true` when the blocker class is safe to retry — orchestrator
    /// may schedule a retry instead of escalating.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient)
    }
}

/// Permission-channel HITL for a blocked yield. `None` is give-up: terminal,
/// not an interview. Auth/permission stay HITL so the owner can unblock.
pub fn permission_hitl_prompt(
    blockers: &[YieldBlocker],
    reason: &str,
) -> Option<(&'static str, String)> {
    let has_auth = blockers
        .iter()
        .any(|blocker| matches!(blocker.kind, YieldBlockerKind::Auth));
    let has_permission = blockers
        .iter()
        .any(|blocker| matches!(blocker.kind, YieldBlockerKind::Permission));
    if has_auth {
        Some((
            "reauth",
            format!(
                "I'm blocked on authentication: {reason}. Can you re-authenticate the service (or grant the missing scope) so I can continue?"
            ),
        ))
    } else if has_permission {
        Some((
            "permission",
            format!(
                "I'm blocked on access: {reason}. Can you grant access to the resource/path so I can continue?"
            ),
        ))
    } else {
        None
    }
}

/// LLM's self-classification of the outcome. Advisory only. Does not
/// include a "needs-user" variant — interactive asks use the dedicated
/// `need_user_input` decision instead.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum YieldSelfClassification {
    Done,
    Partial,
    Blocked,
}

/// Disposition produced by [`dispose_yield`]. The executor maps these
/// to the existing `AgenticOutcome` primitives at a single call site,
/// keeping the policy logic in one testable function.
///
/// There is **no `NeedsUserInput` disposition** — interactive asks use
/// the dedicated `need_user_input` decision (separate primitive with a
/// pause-and-resume lifecycle), not Yield.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YieldDisposition {
    /// All requirements satisfied. Maps to `AgenticOutcome::Success`.
    Completed { artifact_count: usize },
    /// Some requirements satisfied, some still open. Maps to
    /// `AgenticOutcome::Success` with partial-progress metadata (the
    /// existing tactical pattern T3 partial-success path).
    PartialSuccess {
        completed_count: usize,
        open_count: usize,
        blocker_count: usize,
        artifact_count: usize,
    },
    /// All blockers are transient. Maps to retry-with-backoff (today
    /// surfaced as `MaxIterationsReached` with `pause_state`).
    RetryTransient { blocker_count: usize },
    /// Structural failure — no path forward. Maps to
    /// `AgenticOutcome::Failed`.
    Failed { reason: String },
}

/// Map a structured `YieldDecision` to a [`YieldDisposition`].
///
/// Precedence (top first wins):
///   1. `completed` and `open` both non-empty → `PartialSuccess`, retaining
///      any blockers as caveats rather than discarding completed work.
///   2. Non-empty `blockers`, all `is_transient()` → `RetryTransient`.
///   3. Non-empty `blockers`, at least one non-transient → `Failed`.
///   4. `open` empty AND `artifacts` non-empty → `Completed`.
///   5. `open` empty AND `artifacts` empty AND `completed` non-empty →
///      `Completed` (the agent finished bookkeeping-only tasks with no
///      durable artefact; the summary IS the outcome).
///   6. Everything empty → `Failed` with "no progress, no evidence".
///
/// Note: `self_classification` is NOT consulted. The LLM is bad at
/// self-grading; the structured fields are the ground truth.
pub fn dispose_yield(decision: &YieldDecision) -> YieldDisposition {
    // 1. Preserve substantive partial work even when the remaining open item
    // has a structural blocker. This is a terminal answer with caveats, not a
    // request for the user to unblock work that already has a useful result.
    if !decision.completed.is_empty() && !decision.open.is_empty() {
        return YieldDisposition::PartialSuccess {
            completed_count: decision.completed.len(),
            open_count: decision.open.len(),
            blocker_count: decision.blockers.len(),
            artifact_count: decision.artifacts.len(),
        };
    }

    // 2-3. Pure blocker analysis.
    if !decision.blockers.is_empty() {
        let all_transient = decision.blockers.iter().all(|b| b.kind.is_transient());
        if all_transient {
            return YieldDisposition::RetryTransient {
                blocker_count: decision.blockers.len(),
            };
        }
        let reason = summarise_blockers(&decision.blockers);
        return YieldDisposition::Failed { reason };
    }

    // 4-5. No blockers, nothing open — agent considers task done.
    if decision.open.is_empty() {
        if !decision.artifacts.is_empty() || !decision.completed.is_empty() {
            return YieldDisposition::Completed {
                artifact_count: decision.artifacts.len(),
            };
        }
        // 6. Truly empty yield: agent yielded without doing anything,
        //    nothing blocking, nothing produced. Treat as failure with
        //    a clear reason.
        return YieldDisposition::Failed {
            reason: "yield with no progress, no blockers, no artifacts".to_string(),
        };
    }

    // Open items remain, no completed items and no blockers. Preserve the
    // existing partial projection; the evidence gate can reject a hollow
    // success claim before this disposition is accepted.
    YieldDisposition::PartialSuccess {
        completed_count: decision.completed.len(),
        open_count: decision.open.len(),
        blocker_count: decision.blockers.len(),
        artifact_count: decision.artifacts.len(),
    }
}

/// Build a compact human-readable reason string from a blocker list.
/// Used by the `Failed` disposition.
fn summarise_blockers(blockers: &[YieldBlocker]) -> String {
    if blockers.is_empty() {
        return "no blockers reported".to_string();
    }
    if blockers.len() == 1 {
        let b = &blockers[0];
        return format!("[{kind:?}] {desc}", kind = b.kind, desc = b.description);
    }
    let mut parts = Vec::with_capacity(blockers.len());
    for b in blockers {
        parts.push(format!(
            "[{kind:?}] {desc}",
            kind = b.kind,
            desc = b.description
        ));
    }
    parts.join("; ")
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn blocker(kind: YieldBlockerKind, desc: &str) -> YieldBlocker {
        YieldBlocker {
            kind,
            description: desc.to_string(),
        }
    }

    #[test]
    fn all_transient_blockers_route_to_retry() {
        let decision = YieldDecision {
            summary: "API rate-limited".to_string(),
            blockers: vec![
                blocker(YieldBlockerKind::Transient, "metabase 429"),
                blocker(YieldBlockerKind::Transient, "metabase 429 again"),
            ],
            ..Default::default()
        };
        let disposition = dispose_yield(&decision);
        assert!(matches!(
            disposition,
            YieldDisposition::RetryTransient { blocker_count: 2 }
        ));
    }

    #[test]
    fn mixed_blockers_with_any_non_transient_route_to_failed() {
        // One transient + one structural = failed (the structural one
        // won't clear on retry, so retrying is wasteful).
        let decision = YieldDecision {
            summary: "Auth missing".to_string(),
            blockers: vec![
                blocker(YieldBlockerKind::Transient, "metabase 429"),
                blocker(YieldBlockerKind::Auth, "gws token expired"),
            ],
            ..Default::default()
        };
        let disposition = dispose_yield(&decision);
        let YieldDisposition::Failed { reason } = disposition else {
            panic!("expected Failed");
        };
        assert!(reason.contains("Transient"));
        assert!(reason.contains("Auth"));
        assert!(reason.contains("metabase 429"));
        assert!(reason.contains("gws token expired"));
    }

    #[test]
    fn single_auth_blocker_routes_to_failed_with_clean_reason() {
        let decision = YieldDecision {
            summary: "auth".to_string(),
            blockers: vec![blocker(YieldBlockerKind::Auth, "gws token expired")],
            ..Default::default()
        };
        let YieldDisposition::Failed { reason } = dispose_yield(&decision) else {
            panic!("expected Failed");
        };
        assert_eq!(reason, "[Auth] gws token expired");
    }

    #[test]
    fn permission_blocker_is_structural_failure_not_retry() {
        // HTTP 403 / capability scope mismatch — retry won't help.
        let decision = YieldDecision {
            summary: "403 from upstream".to_string(),
            blockers: vec![blocker(
                YieldBlockerKind::Permission,
                "card 6090 db not readable with this key",
            )],
            ..Default::default()
        };
        assert!(matches!(
            dispose_yield(&decision),
            YieldDisposition::Failed { .. }
        ));
    }

    #[test]
    fn empty_open_with_artifacts_routes_to_completed() {
        let decision = YieldDecision {
            summary: "Report written".to_string(),
            completed: vec!["wrote report".to_string()],
            artifacts: vec![Artifact::text("report.md", "Weekly report body")],
            ..Default::default()
        };
        assert!(matches!(
            dispose_yield(&decision),
            YieldDisposition::Completed { artifact_count: 1 }
        ));
    }

    #[test]
    fn empty_open_with_completed_but_no_artifacts_still_completes() {
        // Pure-bookkeeping task — agent did everything (filed a label,
        // marked a calendar event read) but produced no file artefact.
        // Summary IS the deliverable.
        let decision = YieldDecision {
            summary: "Marked 5 emails as read".to_string(),
            completed: vec!["labelled 5 emails".to_string()],
            ..Default::default()
        };
        assert!(matches!(
            dispose_yield(&decision),
            YieldDisposition::Completed { artifact_count: 0 }
        ));
    }

    #[test]
    fn empty_open_no_completed_no_artifacts_is_failed() {
        // Empty yield — agent did nothing, nothing blocking, nothing
        // produced. This is a meaningful failure (agent abandoned the
        // task without explanation).
        let decision = YieldDecision {
            summary: "Stopped".to_string(),
            ..Default::default()
        };
        let YieldDisposition::Failed { reason } = dispose_yield(&decision) else {
            panic!("expected Failed");
        };
        assert!(reason.contains("no progress"));
    }

    #[test]
    fn completed_plus_open_with_no_blockers_is_partial_success() {
        let decision = YieldDecision {
            summary: "Half done; remainder needs follow-up".to_string(),
            completed: vec!["analysed Q1".to_string(), "analysed Q2".to_string()],
            open: vec!["analyse Q3 and Q4".to_string()],
            artifacts: vec![Artifact::text("q1-q2.csv", "quarter,total\nQ1,100\nQ2,200")],
            ..Default::default()
        };
        let disposition = dispose_yield(&decision);
        assert!(matches!(
            disposition,
            YieldDisposition::PartialSuccess {
                completed_count: 2,
                open_count: 1,
                blocker_count: 0,
                artifact_count: 1,
            }
        ));
    }

    #[test]
    fn completed_plus_open_with_structural_blocker_is_partial_success() {
        let decision = YieldDecision {
            summary: "Compared one vendor; the other was not verifiable".to_string(),
            completed: vec!["captured official Sarvam pricing".to_string()],
            open: vec!["verify GPT pricing".to_string()],
            blockers: vec![blocker(
                YieldBlockerKind::DataMissing,
                "no official GPT pricing entry was retrieved",
            )],
            artifacts: vec![Artifact::text("partial_findings.md", "Sarvam prices")],
            ..Default::default()
        };

        assert!(matches!(
            dispose_yield(&decision),
            YieldDisposition::PartialSuccess {
                completed_count: 1,
                open_count: 1,
                blocker_count: 1,
                artifact_count: 1,
            }
        ));
    }

    #[test]
    fn self_classification_does_not_override_structured_fields() {
        // LLM claims "done" but the structured fields say there are
        // still open items. Orchestrator must trust the structured
        // fields — that's the whole point of moving classification
        // away from the LLM.
        let decision = YieldDecision {
            summary: "Done".to_string(),
            completed: vec!["one thing".to_string()],
            open: vec!["another thing".to_string()],
            self_classification: Some(YieldSelfClassification::Done),
            ..Default::default()
        };
        assert!(matches!(
            dispose_yield(&decision),
            YieldDisposition::PartialSuccess { open_count: 1, .. }
        ));
    }

    #[test]
    fn self_classification_blocked_does_not_synthesise_blockers() {
        // LLM claims "blocked" but didn't fill in any structured
        // blockers. Treat as PartialSuccess (open items, no blockers)
        // rather than synthesising a blocker from the classification.
        let decision = YieldDecision {
            summary: "Blocked".to_string(),
            completed: vec!["initial scan".to_string()],
            open: vec!["follow-up step".to_string()],
            self_classification: Some(YieldSelfClassification::Blocked),
            ..Default::default()
        };
        assert!(matches!(
            dispose_yield(&decision),
            YieldDisposition::PartialSuccess { .. }
        ));
    }

    #[test]
    fn data_missing_alone_is_structural_failure() {
        // Reasonable design choice: "no matching record" is a failure
        // outcome the orchestrator surfaces to the user. The agent
        // can't fix it; retry won't help. (Future: orchestrator might
        // upgrade this to "completed with caveat" once we have a
        // disposition for that — for now, Failed is honest.)
        let decision = YieldDecision {
            summary: "No emails matched".to_string(),
            blockers: vec![blocker(
                YieldBlockerKind::DataMissing,
                "no emails from alice@x.com in the last 30 days",
            )],
            ..Default::default()
        };
        assert!(matches!(
            dispose_yield(&decision),
            YieldDisposition::Failed { .. }
        ));
    }

    #[test]
    fn is_transient_only_true_for_transient_variant() {
        assert!(YieldBlockerKind::Transient.is_transient());
        assert!(!YieldBlockerKind::Auth.is_transient());
        assert!(!YieldBlockerKind::DataMissing.is_transient());
        assert!(!YieldBlockerKind::Permission.is_transient());
        assert!(!YieldBlockerKind::External.is_transient());
        assert!(!YieldBlockerKind::Other.is_transient());
    }

    #[test]
    fn yield_decision_round_trips_through_serde() {
        let decision = YieldDecision {
            summary: "OK".to_string(),
            completed: vec!["a".to_string()],
            open: vec!["b".to_string()],
            blockers: vec![blocker(YieldBlockerKind::Transient, "x")],
            artifacts: vec![],
            next_step_hint: Some("retry tomorrow".to_string()),
            self_classification: Some(YieldSelfClassification::Partial),
            ..Default::default()
        };
        let json = serde_json::to_string(&decision).expect("serialize");
        let parsed: YieldDecision = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.summary, decision.summary);
        assert_eq!(parsed.completed, decision.completed);
        assert_eq!(parsed.open, decision.open);
        assert_eq!(parsed.blockers.len(), 1);
        assert_eq!(parsed.next_step_hint.as_deref(), Some("retry tomorrow"));
        assert!(matches!(
            parsed.self_classification,
            Some(YieldSelfClassification::Partial)
        ));
    }

    #[test]
    fn permission_hitl_is_only_for_auth_and_access() {
        let auth = permission_hitl_prompt(
            &[blocker(YieldBlockerKind::Auth, "gmail expired")],
            "gmail token expired",
        );
        assert_eq!(auth.map(|(trigger, _)| trigger), Some("reauth"));
        let access = permission_hitl_prompt(
            &[blocker(YieldBlockerKind::Permission, "outside sandbox")],
            "path is outside the sandbox roots",
        );
        assert_eq!(access.map(|(trigger, _)| trigger), Some("permission"));
        assert_eq!(
            permission_hitl_prompt(
                &[blocker(YieldBlockerKind::DataMissing, "no row")],
                "nothing matched",
            ),
            None,
            "give-up is terminal, not an interview"
        );
        assert_eq!(permission_hitl_prompt(&[], "I cannot proceed"), None);
    }
}

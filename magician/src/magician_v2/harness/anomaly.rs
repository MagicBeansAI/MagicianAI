use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnomalyKind {
    CycleFailed,
    CycleDropped,
    ToolUnavailable,
    StuckHitl,
    NoProgress,
    SandboxDenied,
    RosterDrift,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnomalyStatus {
    Open,
    FixDispatched,
    Resolved,
    Dismissed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessAnomaly {
    pub signature: String,
    pub principal: String,
    pub workspace: String,
    pub agent_id: String,
    pub goal_id: String,
    pub kind: AnomalyKind,
    pub summary: String,
    pub detail: String,
    pub status: AnomalyStatus,
    pub occurrences: u32,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub last_fix_dispatch_at: Option<DateTime<Utc>>,
    pub fix_task_id: Option<String>,
}

impl HarnessAnomaly {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        principal: &str,
        workspace: &str,
        agent_id: &str,
        goal_id: &str,
        kind: AnomalyKind,
        summary: &str,
        detail: &str,
    ) -> Self {
        let now = Utc::now();
        Self {
            signature: Self::signature(agent_id, goal_id, kind),
            principal: principal.into(),
            workspace: workspace.into(),
            agent_id: agent_id.into(),
            goal_id: goal_id.into(),
            kind,
            summary: summary.into(),
            detail: detail.into(),
            status: AnomalyStatus::Open,
            occurrences: 1,
            first_seen: now,
            last_seen: now,
            last_fix_dispatch_at: None,
            fix_task_id: None,
        }
    }

    pub fn signature(agent_id: &str, goal_id: &str, kind: AnomalyKind) -> String {
        let mut h = Sha256::new();
        h.update(agent_id.as_bytes());
        h.update(b"\0");
        h.update(goal_id.as_bytes());
        h.update(b"\0");
        h.update(format!("{kind:?}").as_bytes());
        format!("{:x}", h.finalize())[..16].to_string()
    }

    /// Snake_case wire form of `status`, matching the serde `rename_all`
    /// representation. A later API task filters anomalies by this string.
    pub fn status_wire(&self) -> &'static str {
        match self.status {
            AnomalyStatus::Open => "open",
            AnomalyStatus::FixDispatched => "fix_dispatched",
            AnomalyStatus::Resolved => "resolved",
            AnomalyStatus::Dismissed => "dismissed",
        }
    }
}

/// Classify a just-completed harness cycle into an optional anomaly.
/// `outcome` = the wire string recorded by `record_harness_cycle_outcome`
/// (e.g. "episode_persisted", "episode_persist_failed", "dropped_provision_failed").
/// `episode_failed` = the cycle produced a failed/absent durable episode, even
/// if the failed episode itself persisted successfully.
/// `detail` = the failure text assembled by the caller (last episode error /
/// episode outcome summary). Phase 1.5: on a failed cycle we inspect this text
/// so a path-permission denial, a stuck approval, a missing tool/pack, or a
/// no-progress stall lands as the dedicated `AnomalyKind` — making those
/// distinct failure modes visible to the reliability layer instead of collapsing
/// into a generic `CycleFailed`.
pub fn classify_cycle_anomaly(
    outcome: &str,
    episode_failed: bool,
    detail: &str,
) -> Option<(AnomalyKind, String)> {
    if episode_failed || outcome == "episode_persist_failed" {
        if let Some((kind, summary)) = classify_failure_detail(detail) {
            return Some((kind, summary));
        }
        return Some((
            AnomalyKind::CycleFailed,
            "autonomous cycle failed or failed to produce a durable episode".to_string(),
        ));
    }
    match outcome {
        "dropped_provision_failed" | "dropped_definition_stale" | "dropped_no_execution_id" => {
            Some((
                AnomalyKind::CycleDropped,
                format!("cycle dropped before running: {outcome}"),
            ))
        },
        // dropped_disabled / dropped_paused / dropped_duplicate / dropped_reservation_changed = intentional/benign
        _ => None,
    }
}

/// Inspect the failure `detail` text of a failed cycle for markers that pin it
/// to a specific `AnomalyKind`. Returns `None` when no marker matches (caller
/// falls back to the generic `CycleFailed`). Match order is significant:
/// sandbox/permission first (the highest-signal, owner-actionable case).
fn classify_failure_detail(detail: &str) -> Option<(AnomalyKind, String)> {
    let text = detail.to_lowercase();
    // Sandbox / permission denials — e.g. "Path '<p>' is outside file sandbox
    // allowed roots" / "Working directory '<p>' is outside sandbox allowed roots".
    const SANDBOX_MARKERS: &[&str] = &[
        "outside file sandbox",
        "outside sandbox allowed roots",
        "file sandbox allowed roots",
        "permission denied",
        "operation not permitted",
        "path access denied",
    ];
    if SANDBOX_MARKERS.iter().any(|m| text.contains(m)) {
        return Some((
            AnomalyKind::SandboxDenied,
            "cycle blocked on a path outside the file sandbox — owner approval to access it is required".to_string(),
        ));
    }
    // Stuck approvals / HITL waits that never resolved.
    const STUCK_HITL_MARKERS: &[&str] = &[
        "waiting for user",
        "waiting for approval",
        "awaiting approval",
        "approval never",
        "stuck approval",
        "pause never resolved",
        "hitl timed out",
    ];
    if STUCK_HITL_MARKERS.iter().any(|m| text.contains(m)) {
        return Some((
            AnomalyKind::StuckHitl,
            "cycle stalled on an unresolved approval/HITL request".to_string(),
        ));
    }
    // Missing tool / pack — the goal referenced a capability the agent lacks.
    const TOOL_MARKERS: &[&str] = &[
        "not a compiled pack",
        "unknown tool",
        "tool not found",
        "no such tool",
        "not granted",
        "capability not available",
        "missing tool",
        "pack not found",
    ];
    if TOOL_MARKERS.iter().any(|m| text.contains(m)) {
        return Some((
            AnomalyKind::ToolUnavailable,
            "cycle referenced a tool/pack the agent does not have".to_string(),
        ));
    }
    // No-progress stalls (iteration/budget exhaustion with nothing produced).
    const NO_PROGRESS_MARKERS: &[&str] = &[
        "no progress",
        "made no progress",
        "no meaningful progress",
        "empty artifacts",
        "produced no output",
    ];
    if NO_PROGRESS_MARKERS.iter().any(|m| text.contains(m)) {
        return Some((
            AnomalyKind::NoProgress,
            "cycle exhausted its budget without making progress".to_string(),
        ));
    }
    None
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn signature_is_stable_and_dedupes_by_agent_goal_kind() {
        let a = HarnessAnomaly::new(
            "anonymous",
            "default",
            "cto",
            "harness:cto:morning-engineering-standup",
            AnomalyKind::CycleFailed,
            "episode failed",
            "list_episodes not a compiled pack",
        );
        let b = HarnessAnomaly::new(
            "anonymous",
            "default",
            "cto",
            "harness:cto:morning-engineering-standup",
            AnomalyKind::CycleFailed,
            "episode failed AGAIN",
            "different detail",
        );
        assert_eq!(
            a.signature, b.signature,
            "same agent+goal+kind → same signature (dedupe key)"
        );
        let c = HarnessAnomaly::new(
            "anonymous",
            "default",
            "cto",
            "harness:cto:morning-engineering-standup",
            AnomalyKind::ToolUnavailable,
            "x",
            "y",
        );
        assert_ne!(
            a.signature, c.signature,
            "different kind → different signature"
        );
        assert_eq!(a.status, AnomalyStatus::Open);
    }

    #[test]
    fn classifies_failed_cycle_and_healthy_noop() {
        assert!(classify_cycle_anomaly("episode_persist_failed", true, "boom").is_some());
        assert_eq!(
            classify_cycle_anomaly("episode_persisted", true, "failed episode").map(|(k, _)| k),
            Some(AnomalyKind::CycleFailed)
        );
        assert_eq!(
            classify_cycle_anomaly("episode_persisted", false, "").map(|(k, _)| k),
            None
        ); // healthy → no anomaly
        assert_eq!(
            classify_cycle_anomaly("dropped_provision_failed", false, "").map(|(k, _)| k),
            Some(AnomalyKind::CycleDropped)
        );
        assert_eq!(
            classify_cycle_anomaly("dropped_disabled", false, "").map(|(k, _)| k),
            None
        ); // disabled is intentional, not an anomaly
    }

    #[test]
    fn failed_cycle_detail_routes_to_specific_kinds() {
        // Sandbox/permission markers -> SandboxDenied (highest signal).
        assert_eq!(
            classify_cycle_anomaly(
                "episode_persisted",
                true,
                "Path '/etc/hosts' is outside file sandbox allowed roots",
            )
            .map(|(k, _)| k),
            Some(AnomalyKind::SandboxDenied)
        );
        assert_eq!(
            classify_cycle_anomaly("episode_persist_failed", true, "permission denied")
                .map(|(k, _)| k),
            Some(AnomalyKind::SandboxDenied)
        );
        // Stuck approval -> StuckHitl.
        assert_eq!(
            classify_cycle_anomaly(
                "episode_persisted",
                true,
                "cycle ended still waiting for approval",
            )
            .map(|(k, _)| k),
            Some(AnomalyKind::StuckHitl)
        );
        // Missing tool/pack -> ToolUnavailable.
        assert_eq!(
            classify_cycle_anomaly(
                "episode_persisted",
                true,
                "list_episodes is not a compiled pack",
            )
            .map(|(k, _)| k),
            Some(AnomalyKind::ToolUnavailable)
        );
        // No-progress stall -> NoProgress.
        assert_eq!(
            classify_cycle_anomaly(
                "episode_persisted",
                true,
                "iteration budget exhausted; made no progress",
            )
            .map(|(k, _)| k),
            Some(AnomalyKind::NoProgress)
        );
        // Unmatched failure text still falls back to the generic CycleFailed.
        assert_eq!(
            classify_cycle_anomaly("episode_persist_failed", true, "boom").map(|(k, _)| k),
            Some(AnomalyKind::CycleFailed)
        );
    }

    #[test]
    fn anomaly_detail_maps_to_specific_kind_table_driven() {
        // Each row: (representative detail string, expected specific kind).
        // The strings embed the ACTUAL marker substrings matched by
        // `classify_failure_detail`, wrapped in realistic surrounding text so the
        // test guards the substring-contains classification, not an exact match.
        // A regression that drops the detail-string routing (finding #11) collapses
        // every one of these back into the generic `CycleFailed` and fails here.
        let cases: &[(&str, AnomalyKind)] = &[
            // SandboxDenied — path/permission fence markers (highest signal, matched first).
            (
                "Path '/etc/hosts' is outside file sandbox allowed roots",
                AnomalyKind::SandboxDenied,
            ),
            (
                "Working directory '/srv' is outside sandbox allowed roots",
                AnomalyKind::SandboxDenied,
            ),
            (
                "write failed: Permission denied (os error 13)",
                AnomalyKind::SandboxDenied,
            ),
            (
                "syscall blocked: Operation not permitted",
                AnomalyKind::SandboxDenied,
            ),
            // StuckHitl — unresolved approval / HITL wait markers.
            (
                "cycle ended still waiting for approval",
                AnomalyKind::StuckHitl,
            ),
            (
                "run stalled awaiting approval from the owner",
                AnomalyKind::StuckHitl,
            ),
            (
                "pause never resolved before teardown",
                AnomalyKind::StuckHitl,
            ),
            // ToolUnavailable — missing tool/pack markers.
            (
                "list_episodes is not a compiled pack",
                AnomalyKind::ToolUnavailable,
            ),
            (
                "dispatch aborted: unknown tool 'search_web'",
                AnomalyKind::ToolUnavailable,
            ),
            (
                "capability not available for this agent",
                AnomalyKind::ToolUnavailable,
            ),
            // NoProgress — budget/iteration exhaustion with nothing produced.
            (
                "iteration budget exhausted; made no progress",
                AnomalyKind::NoProgress,
            ),
            (
                "cycle produced no output after 4000 iterations",
                AnomalyKind::NoProgress,
            ),
            // Generic failure with no marker -> falls back to CycleFailed.
            ("unexpected internal error: boom", AnomalyKind::CycleFailed),
        ];

        for (detail, expected) in cases {
            let got = classify_cycle_anomaly("episode_persisted", true, detail).map(|(k, _)| k);
            assert_eq!(
                got,
                Some(*expected),
                "detail {detail:?} should classify as {expected:?}, got {got:?}"
            );
        }
    }

    #[test]
    fn status_wire_matches_serde_snake_case() {
        let mut a = HarnessAnomaly::new(
            "p",
            "w",
            "cto",
            "harness:cto:g",
            AnomalyKind::CycleFailed,
            "s",
            "d",
        );
        assert_eq!(a.status_wire(), "open");
        a.status = AnomalyStatus::FixDispatched;
        assert_eq!(a.status_wire(), "fix_dispatched");
    }
}

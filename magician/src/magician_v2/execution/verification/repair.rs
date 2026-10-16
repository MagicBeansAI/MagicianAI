//! Bounded repair — handing diagnostics back to the engineer that wrote the
//! code, and deciding when to stop.
//!
//! ## Repair re-enters the engineer, never a provider
//!
//! The controller must not invoke Pi, Codex or any adapter directly: that
//! bypasses agent authority, delegation reconciliation and cost accounting —
//! the properties the coordinator gate and the Build success-gate exist to
//! protect. Repair instead creates or resumes an execution owned by
//! `origin.engineer_agent_id` and goes through the normal `run_coding_task`
//! path.
//!
//! This turns out to need no new plumbing. `derive_coding_continuation_context`
//! resolves the engine-native continuation **server-side** from the task
//! record, keyed on the `__task_id` / `__execution_id` scope args — so the
//! native Pi session or Codex thread id never crosses the tool boundary in the
//! first place. Repair supplies task/execution identity and diagnostics; the
//! runtime supplies the continuation.
//!
//! ## Repair invalidates the candidate, it does not defer it
//!
//! ```text
//! candidate N → red → repair (through the owning engineer)
//!                   → normal parent reconciliation
//!                   → candidate N+1 → new attestation
//!                   → green → release candidate N+1
//! ```
//!
//! The parent's answer and artifacts describe the *pre-repair* state, so
//! candidate N is invalidated the moment repair starts — never eventually
//! released.
//!
//! ## Why no-progress is three conditions
//!
//! "Same failure twice" alone will stop legitimate repairs. Build output can
//! be byte-identical after a real partial fix (the remaining error is the one
//! that was always second), and it can differ only by timestamps, durations,
//! temp paths or memory addresses while nothing has actually changed. Halting
//! requires all three of: the same *normalised* failure, no relevant change to
//! the checked snapshot, and no new diagnostic information.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::attestation::CommandResult;
use super::gate::{GateOrigin, GateStatus, VerificationGate};
use super::ids::CandidateRevision;

/// What the owning engineer is asked to do.
///
/// Note what is *absent*: no native session id, no provider handle, no adapter
/// selection. Those are resolved server-side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairRequest {
    /// The agent that owns this repair. Repair runs as an execution owned by
    /// this agent, so authority and cost accounting are unchanged.
    pub engineer_agent_id: String,
    /// Scope args the runtime uses to resolve the engine-native continuation.
    pub root_task_id: String,
    pub root_execution_id: String,
    /// The candidate being replaced. Recorded so reconciliation can prove the
    /// successor supersedes exactly this one.
    pub superseded_candidate: CandidateRevision,
    /// Which round this is, 1-based.
    pub round: u32,
    /// Human- and model-readable diagnostics from the failed attempt.
    pub diagnostics: String,
    /// Engine the repair must stay on, when the VibeDev coding constraint
    /// pins one. `None` means Auto mode may pick another eligible engine now
    /// that the old candidate is invalidated.
    pub pinned_engine: Option<String>,
}

/// The controller's decision after a red attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairDecision {
    /// Hand diagnostics back and run another round.
    Repair(Box<RepairRequest>),
    /// Stop. Carries the structured reason that accompanies the terminal
    /// `exhausted` state.
    Exhausted { reason: String },
}

impl RepairDecision {
    pub fn terminal_status(&self) -> Option<GateStatus> {
        match self {
            RepairDecision::Repair(_) => None,
            RepairDecision::Exhausted { .. } => Some(GateStatus::Exhausted),
        }
    }
}

/// A normalised fingerprint of one failed attempt.
///
/// Two attempts with the same fingerprint failed "the same way" for the
/// purpose of no-progress detection. Everything volatile is stripped, because
/// a fingerprint that changes on every run makes the check useless, and one
/// that ignores real differences makes it dangerous.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureFingerprint {
    pub digest: String,
    /// The distinct diagnostic lines that contributed, kept for the
    /// new-information comparison.
    pub signals: BTreeSet<String>,
}

impl FailureFingerprint {
    pub fn from_results(results: &[CommandResult]) -> Self {
        let mut signals = BTreeSet::new();
        for result in results.iter().filter(|r| !r.advisory && !r.passed()) {
            signals.insert(format!("cmd:{}", normalize_line(&result.command)));
            if result.timed_out {
                signals.insert(format!("timeout:{}", normalize_line(&result.command)));
            }
            if let Some(code) = result.exit_code {
                signals.insert(format!("exit:{}:{code}", normalize_line(&result.command)));
            }
            for line in
                diagnostic_lines(&result.stderr_tail).chain(diagnostic_lines(&result.stdout_tail))
            {
                signals.insert(line);
            }
        }

        let mut hasher = blake3::Hasher::new();
        for signal in &signals {
            hasher.update(signal.as_bytes());
            hasher.update(b"\x1e");
        }
        Self {
            digest: hasher.finalize().to_hex().to_string(),
            signals,
        }
    }

    /// Signals present here that were not in `prior`. Any such signal is new
    /// diagnostic information, which means the loop is still learning.
    pub fn new_signals_since(&self, prior: &FailureFingerprint) -> BTreeSet<String> {
        self.signals.difference(&prior.signals).cloned().collect()
    }
}

/// Lines that look like a compiler/test diagnostic rather than progress noise.
fn diagnostic_lines(text: &str) -> impl Iterator<Item = String> + '_ {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.starts_with("error")
                || lower.starts_with("warning")
                || lower.contains("panicked at")
                || lower.contains("assertion")
                || lower.contains("failed:")
                || lower.contains("test result:")
        })
        .map(normalize_line)
}

/// Strip everything that varies between two runs of the same failure.
///
/// Timestamps, durations, temp paths, hex addresses, pids and line-of-output
/// counters all change run to run while describing the identical problem.
/// Leaving any of them in makes "same failure twice" effectively never true,
/// which silently disables the halt condition.
pub fn normalize_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let bytes: Vec<char> = line.chars().collect();
    let mut i = 0;

    while i < bytes.len() {
        let c = bytes[i];

        // Hex addresses / long hex ids → <hex>
        if c == '0' && i + 1 < bytes.len() && (bytes[i + 1] == 'x' || bytes[i + 1] == 'X') {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
                j += 1;
            }
            if j > i + 2 {
                out.push_str("<hex>");
                i = j;
                continue;
            }
        }

        // Any run of digits → <n>. Covers timestamps, durations, pids, counts
        // and line numbers. Line numbers are a real loss of precision, but a
        // repair that only moved a line number has not made progress, so
        // collapsing them is the safer error.
        if c.is_ascii_digit() {
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == '.') {
                i += 1;
            }
            out.push_str("<n>");
            continue;
        }

        out.push(c);
        i += 1;
    }

    let collapsed = collapse_temp_roots(&out);
    collapsed.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Collapse per-run temp roots to a single `/<tmp>/` token.
///
/// macOS puts *two* random segments after `/var/folders/` (the user's
/// darwin-user cache key and its subkey), so replacing only the prefix leaves
/// `/<tmp>/ab/xyz/…` and `/<tmp>/cd/uvw/…` looking like different files —
/// which is the whole failure mode this normalisation exists to prevent.
/// Those segments have to go too.
fn collapse_temp_roots(text: &str) -> String {
    const MACOS_ROOTS: [&str; 2] = ["/private/var/folders/", "/var/folders/"];

    let mut out = text.to_string();
    for root in MACOS_ROOTS {
        loop {
            let Some(at) = out.find(root) else { break };
            let rest = &out[at + root.len()..];
            // Skip the two opaque segments; stop early if the line was cut
            // short, so a truncated tail still normalises to something.
            let mut consumed = 0;
            for _ in 0..2 {
                match rest[consumed..].find('/') {
                    Some(slash) => consumed += slash + 1,
                    None => {
                        consumed = rest.len();
                        break;
                    },
                }
            }
            out.replace_range(at..at + root.len() + consumed, "/<tmp>/");
        }
    }
    out.replace("/tmp/", "/<tmp>/")
}

/// One prior round, for the no-progress comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorRound {
    pub fingerprint: FailureFingerprint,
    /// The snapshot digest that was checked.
    pub snapshot_digest: String,
}

/// The §4.8 rule. Halting requires **all three** conditions.
pub fn is_no_progress(
    current: &FailureFingerprint,
    current_snapshot_digest: &str,
    prior: &PriorRound,
) -> bool {
    let same_failure = current.digest == prior.fingerprint.digest;
    let unchanged_snapshot = current_snapshot_digest == prior.snapshot_digest;
    let no_new_information = current.new_signals_since(&prior.fingerprint).is_empty();

    same_failure && unchanged_snapshot && no_new_information
}

/// Decide whether to repair again.
pub fn decide(
    gate: &VerificationGate,
    now: chrono::DateTime<chrono::Utc>,
    current: &FailureFingerprint,
    current_snapshot_digest: &str,
    prior: Option<&PriorRound>,
    diagnostics: String,
) -> RepairDecision {
    if let Some(prior) = prior {
        if is_no_progress(current, current_snapshot_digest, prior) {
            return RepairDecision::Exhausted {
                reason: "repair made no progress: the same normalised failure, an unchanged \
                         snapshot, and no new diagnostic information"
                    .to_string(),
            };
        }
    }

    if !gate.repair_budget_remains(now) {
        return RepairDecision::Exhausted {
            reason: format!(
                "repair budget exhausted after {} round(s)",
                gate.spend.repair_rounds
            ),
        };
    }

    RepairDecision::Repair(Box::new(RepairRequest {
        engineer_agent_id: gate.origin.engineer_agent_id.clone(),
        root_task_id: gate.root_task_id.clone(),
        root_execution_id: gate.root_execution_id.clone(),
        superseded_candidate: gate.current_candidate.clone(),
        round: gate.spend.repair_rounds.saturating_add(1),
        diagnostics,
        pinned_engine: repair_pinned_engine(&gate.origin),
    }))
}

/// Render failed command output into diagnostics for the engineer.
///
/// Advisory results are excluded: they did not gate, so presenting them as
/// things to fix would send the engineer after work the harness did not
/// actually require.
pub fn format_diagnostics(results: &[CommandResult]) -> String {
    let mut out = String::new();
    out.push_str(
        "Verification failed. The following required checks did not pass. \
         Fix the underlying problem and the checks will be re-run automatically.\n",
    );
    for result in results.iter().filter(|r| !r.advisory && !r.passed()) {
        out.push_str("\n--- ");
        out.push_str(&result.command);
        if result.timed_out {
            out.push_str(" (timed out)");
        } else if let Some(code) = result.exit_code {
            out.push_str(&format!(" (exit {code})"));
        }
        out.push_str(" ---\n");
        if !result.stderr_tail.trim().is_empty() {
            out.push_str(result.stderr_tail.trim());
            out.push('\n');
        }
        if !result.stdout_tail.trim().is_empty() {
            out.push_str(result.stdout_tail.trim());
            out.push('\n');
        }
    }
    out
}

/// Whether repair may switch engines.
///
/// Free only when the request constraint is Auto, and only after the old
/// candidate is invalidated. A last-candidate engine on the origin is
/// provenance, not a pin.
pub fn may_switch_engine(origin: &GateOrigin) -> bool {
    origin.constraint_auto
}

/// Engine the repair must stay on. `None` in Auto mode so the coordinator
/// may propose another eligible profile; Magician still validates it.
pub fn repair_pinned_engine(origin: &GateOrigin) -> Option<String> {
    if may_switch_engine(origin) {
        None
    } else {
        origin.coding_engine.clone()
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::file_edit::transaction::TransactionScope;
    use crate::magician_v2::execution::verification::gate::GateBudgets;

    fn result(command: &str, code: i32, stderr: &str) -> CommandResult {
        CommandResult {
            command: command.into(),
            exit_code: Some(code),
            duration_ms: 10,
            timed_out: false,
            stdout_tail: String::new(),
            stderr_tail: stderr.into(),
            advisory: false,
        }
    }

    fn gate(rounds: u32) -> VerificationGate {
        let mut g = VerificationGate::new(
            TransactionScope {
                principal: "anonymous".into(),
                workspace: "default".into(),
            },
            "proj-a",
            "task-1",
            "exec-1",
            CandidateRevision::new("ccp-1", 1).unwrap(),
            GateOrigin {
                engineer_agent_id: "engineer".into(),
                coding_profile: None,
                coding_engine: Some("pi".into()),
                constraint_auto: false,
                coding_invocation_ref: Some("opaque".into()),
                child_execution_id: None,
            },
            GateBudgets::default(),
        )
        .unwrap();
        g.spend.repair_rounds = rounds;
        g
    }

    #[test]
    fn normalisation_strips_timestamps_durations_and_temp_paths() {
        let a = normalize_line("error[E0308]: mismatched types at 12:04:11 in 3.2s");
        let b = normalize_line("error[E0308]: mismatched types at 15:59:02 in 7.8s");
        assert_eq!(
            a, b,
            "volatile values must not make two runs look different"
        );

        let t1 = normalize_line("failed: /var/folders/ab/xyz/T/build-1/out");
        let t2 = normalize_line("failed: /private/var/folders/cd/uvw/T/build-2/out");
        assert_eq!(t1, t2);
    }

    #[test]
    fn normalisation_strips_hex_addresses() {
        assert_eq!(
            normalize_line("panicked at 0xdeadbeef"),
            normalize_line("panicked at 0xcafef00d")
        );
    }

    #[test]
    fn normalisation_keeps_genuinely_different_failures_distinct() {
        // The dangerous direction: over-normalising until every failure looks
        // the same would halt legitimate repairs immediately.
        assert_ne!(
            normalize_line("error[E0308]: mismatched types"),
            normalize_line("error[E0425]: cannot find value")
        );
        assert_ne!(
            normalize_line("error: unresolved import `foo`"),
            normalize_line("error: unresolved import `bar`")
        );
    }

    #[test]
    fn identical_failures_share_a_fingerprint() {
        let a = FailureFingerprint::from_results(&[result(
            "cargo test",
            101,
            "error[E0308]: mismatched types\nfinished in 3.1s",
        )]);
        let b = FailureFingerprint::from_results(&[result(
            "cargo test",
            101,
            "error[E0308]: mismatched types\nfinished in 9.7s",
        )]);
        assert_eq!(a.digest, b.digest);
    }

    #[test]
    fn a_partial_fix_that_reveals_a_new_error_is_new_information() {
        let before = FailureFingerprint::from_results(&[result(
            "cargo test",
            101,
            "error[E0308]: mismatched types",
        )]);
        let after = FailureFingerprint::from_results(&[result(
            "cargo test",
            101,
            "error[E0425]: cannot find value",
        )]);
        assert!(!after.new_signals_since(&before).is_empty());
        assert_ne!(before.digest, after.digest);
    }

    #[test]
    fn no_progress_requires_all_three_conditions() {
        let fp = FailureFingerprint::from_results(&[result("cargo test", 101, "error: boom")]);
        let prior = PriorRound {
            fingerprint: fp.clone(),
            snapshot_digest: "snap-1".into(),
        };

        // All three hold → halt.
        assert!(is_no_progress(&fp, "snap-1", &prior));

        // The snapshot changed — the engineer did something. Keep going even
        // though the failure text is identical, which is the case that a
        // naive "same failure twice" rule gets wrong.
        assert!(!is_no_progress(&fp, "snap-2", &prior));

        // New diagnostic information → keep going.
        let richer = FailureFingerprint::from_results(&[result(
            "cargo test",
            101,
            "error: boom\nerror: second problem",
        )]);
        assert!(!is_no_progress(&richer, "snap-1", &prior));
    }

    #[test]
    fn an_identical_failure_after_a_real_edit_still_gets_another_round() {
        // Output can legitimately be byte-identical after a partial fix.
        let fp = FailureFingerprint::from_results(&[result("cargo test", 101, "error: boom")]);
        let prior = PriorRound {
            fingerprint: fp.clone(),
            snapshot_digest: "snap-before".into(),
        };
        match decide(
            &gate(1),
            chrono::Utc::now(),
            &fp,
            "snap-after",
            Some(&prior),
            "diag".into(),
        ) {
            RepairDecision::Repair(req) => assert_eq!(req.round, 2),
            other => panic!("expected another round, got {other:?}"),
        }
    }

    #[test]
    fn a_stuck_loop_is_exhausted_not_repaired_forever() {
        let fp = FailureFingerprint::from_results(&[result("cargo test", 101, "error: boom")]);
        let prior = PriorRound {
            fingerprint: fp.clone(),
            snapshot_digest: "snap-1".into(),
        };
        let decision = decide(
            &gate(1),
            chrono::Utc::now(),
            &fp,
            "snap-1",
            Some(&prior),
            "diag".into(),
        );
        assert!(matches!(decision, RepairDecision::Exhausted { .. }));
        assert_eq!(decision.terminal_status(), Some(GateStatus::Exhausted));
    }

    #[test]
    fn budget_exhaustion_stops_repair() {
        let fp = FailureFingerprint::from_results(&[result("cargo test", 101, "error: boom")]);
        let g = gate(GateBudgets::default().max_repair_rounds);
        let decision = decide(&g, chrono::Utc::now(), &fp, "snap-1", None, "diag".into());
        match decision {
            RepairDecision::Exhausted { reason } => assert!(reason.contains("budget")),
            other => panic!("expected exhausted, got {other:?}"),
        }
    }

    #[test]
    fn the_first_red_round_always_gets_a_repair() {
        let fp = FailureFingerprint::from_results(&[result("cargo test", 101, "error: boom")]);
        let decision = decide(
            &gate(0),
            chrono::Utc::now(),
            &fp,
            "snap-1",
            None,
            "d".into(),
        );
        match decision {
            RepairDecision::Repair(req) => {
                assert_eq!(req.round, 1);
                assert_eq!(req.engineer_agent_id, "engineer");
                assert_eq!(req.superseded_candidate.revision, 1);
            },
            other => panic!("expected repair, got {other:?}"),
        }
    }

    #[test]
    fn a_repair_request_carries_no_native_session_handle() {
        // §4.6: neither the proposal, the gate API, the model nor the UI may
        // receive the native Pi session or Codex thread id. The type simply
        // has nowhere to put one.
        let decision = decide(
            &gate(0),
            chrono::Utc::now(),
            &FailureFingerprint::from_results(&[result("t", 1, "error: x")]),
            "snap",
            None,
            "d".into(),
        );
        let RepairDecision::Repair(req) = decision else {
            panic!("expected repair")
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("opaque"), "continuation ref must not leak");
        assert!(!json.to_lowercase().contains("session_id"));
        assert!(!json.to_lowercase().contains("thread_id"));
    }

    #[test]
    fn repair_is_pinned_to_the_engine_the_constraint_named() {
        let decision = decide(
            &gate(0),
            chrono::Utc::now(),
            &FailureFingerprint::from_results(&[result("t", 1, "error: x")]),
            "snap",
            None,
            "d".into(),
        );
        let RepairDecision::Repair(req) = decision else {
            panic!("expected repair")
        };
        assert_eq!(req.pinned_engine.as_deref(), Some("pi"));

        let pinned = GateOrigin {
            coding_engine: Some("pi".into()),
            constraint_auto: false,
            ..gate(0).origin
        };
        assert!(!may_switch_engine(&pinned));
        assert_eq!(repair_pinned_engine(&pinned).as_deref(), Some("pi"));

        let auto = GateOrigin {
            coding_engine: Some("pi".into()),
            constraint_auto: true,
            ..gate(0).origin
        };
        assert!(may_switch_engine(&auto));
        assert_eq!(repair_pinned_engine(&auto), None);

        let unpinned = GateOrigin {
            coding_engine: None,
            constraint_auto: false,
            ..gate(0).origin
        };
        assert!(!may_switch_engine(&unpinned));
    }

    #[test]
    fn auto_repair_is_not_pinned_to_the_failed_candidate_engine() {
        let mut g = gate(0);
        g.origin.coding_engine = Some("pi".into());
        g.origin.constraint_auto = true;
        let decision = decide(
            &g,
            chrono::Utc::now(),
            &FailureFingerprint::from_results(&[result("t", 1, "error: x")]),
            "snap",
            None,
            "d".into(),
        );
        let RepairDecision::Repair(req) = decision else {
            panic!("expected repair")
        };
        assert_eq!(req.pinned_engine, None);
    }

    #[test]
    fn diagnostics_exclude_advisory_failures() {
        let advisory = CommandResult {
            advisory: true,
            ..result("lint", 1, "error: style")
        };
        let required = result("cargo test", 101, "error[E0308]: mismatched types");
        let text = format_diagnostics(&[advisory, required]);

        assert!(text.contains("E0308"));
        assert!(
            !text.contains("error: style"),
            "advisory output must not be presented as something to fix"
        );
    }

    #[test]
    fn diagnostics_mention_a_timeout_as_such() {
        let timed_out = CommandResult {
            timed_out: true,
            exit_code: None,
            ..result("make check-all", 0, "")
        };
        let text = format_diagnostics(&[timed_out]);
        assert!(text.contains("timed out"));
    }

    #[test]
    fn a_timeout_and_an_exit_failure_are_different_fingerprints() {
        let a = FailureFingerprint::from_results(&[result("t", 1, "")]);
        let b = FailureFingerprint::from_results(&[CommandResult {
            timed_out: true,
            exit_code: None,
            ..result("t", 1, "")
        }]);
        assert_ne!(a.digest, b.digest);
    }
}

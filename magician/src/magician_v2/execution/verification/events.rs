//! Verification lifecycle events.
//!
//! ## Events are projections, never the source of truth
//!
//! Every event here is a replayable projection of state that is already
//! committed to the journal. A client that receives none of them must still
//! hydrate the correct state — a system whose state lives in its event stream
//! cannot survive a dropped connection, and this one is gating terminal
//! success. So each event carries enough identity (`gate_id`,
//! `candidate_revision`, `attestation_id`, `attempt_id`, `generation`,
//! `sequence`) to be correlated *after the fact* against the durable record,
//! and [`VerificationEvent::resulting_state`] is always derivable from the
//! gate rather than invented by the emitter.
//!
//! ## `checks_failed`, not `failed`
//!
//! A failed check normally leads to repair, so it is **not** task failure.
//! Naming it `failed` would make every consumer render a red terminal state
//! mid-loop, which is both wrong and alarming.
//!
//! ## Voice hears transitions, not commands
//!
//! Narrating `command_completed` would turn a build into a monologue.
//! [`VerificationEvent::is_voice_worthy`] restricts the voice surface to the
//! five transitions a listener actually needs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::gate::{VerificationGate, VerificationState};
use super::ids::{AttemptId, AttestationId, CandidateRevision, GateId, Generation};

/// The event kinds of §5.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationEventKind {
    /// A candidate was held and durable work enqueued.
    Queued,
    /// A worker claimed the gate and began running the policy.
    Started,
    /// One command finished. High volume — not for voice.
    CommandCompleted,
    /// Required checks failed. Normally followed by repair, so this is **not**
    /// task failure.
    ChecksFailed,
    /// The owning engineer has been handed diagnostics.
    RepairStarted,
    /// Green. The candidate success is released.
    Passed,
    /// No required checks were configured. Explicit, never a silent pass.
    Unverified,
    /// Repair budget spent without reaching green.
    Exhausted,
    /// Runner or policy could not be reached. Fails closed.
    Unavailable,
    /// An attempt could not be completed — crash, lost lease. Re-run under a
    /// new attempt; never readable as a pass.
    AttemptIndeterminate,
    Cancelled,
}

impl VerificationEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            VerificationEventKind::Queued => "verification.queued",
            VerificationEventKind::Started => "verification.started",
            VerificationEventKind::CommandCompleted => "verification.command_completed",
            VerificationEventKind::ChecksFailed => "verification.checks_failed",
            VerificationEventKind::RepairStarted => "verification.repair_started",
            VerificationEventKind::Passed => "verification.passed",
            VerificationEventKind::Unverified => "verification.unverified",
            VerificationEventKind::Exhausted => "verification.exhausted",
            VerificationEventKind::Unavailable => "verification.unavailable",
            VerificationEventKind::AttemptIndeterminate => "verification.attempt_indeterminate",
            VerificationEventKind::Cancelled => "verification.cancelled",
        }
    }

    /// Whether a live voice session should narrate this.
    ///
    /// Deliberately excludes `started`, `command_completed`,
    /// `attempt_indeterminate` and `repair_started`: the first three are
    /// mechanical, and repair is better narrated by the engineer's own
    /// activity than announced twice.
    pub fn is_voice_worthy(self) -> bool {
        matches!(
            self,
            VerificationEventKind::Queued
                | VerificationEventKind::Passed
                | VerificationEventKind::ChecksFailed
                | VerificationEventKind::Unverified
                | VerificationEventKind::Exhausted
        )
    }

    /// Whether this kind ends the gate's lifecycle.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            VerificationEventKind::Passed
                | VerificationEventKind::Unverified
                | VerificationEventKind::Exhausted
                | VerificationEventKind::Unavailable
                | VerificationEventKind::Cancelled
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationEvent {
    pub kind: VerificationEventKind,
    pub gate_id: GateId,
    pub root_task_id: String,
    pub root_execution_id: String,
    pub candidate_revision: CandidateRevision,
    pub attestation_id: Option<AttestationId>,
    pub attempt_id: Option<AttemptId>,
    pub generation: Generation,
    /// Monotonic per gate, so a consumer can detect a gap without needing the
    /// journal.
    pub sequence: u64,
    /// The state a consumer should render. Always the gate's own state — an
    /// event never asserts a state the record does not hold.
    pub resulting_state: VerificationState,
    pub occurred_at: DateTime<Utc>,
    /// Short human-readable detail. Never load-bearing.
    pub detail: Option<String>,
}

impl VerificationEvent {
    /// Build an event from the committed gate, so `resulting_state` cannot
    /// drift from the durable record.
    pub fn from_gate(
        kind: VerificationEventKind,
        gate: &VerificationGate,
        sequence: u64,
        attestation_id: Option<AttestationId>,
        attempt_id: Option<AttemptId>,
        detail: Option<String>,
    ) -> Self {
        Self {
            kind,
            gate_id: gate.gate_id.clone(),
            root_task_id: gate.root_task_id.clone(),
            root_execution_id: gate.root_execution_id.clone(),
            candidate_revision: gate.current_candidate.clone(),
            attestation_id,
            attempt_id,
            generation: gate.generation,
            sequence,
            resulting_state: gate.verification_state(),
            occurred_at: Utc::now(),
            detail,
        }
    }

    pub fn is_voice_worthy(&self) -> bool {
        self.kind.is_voice_worthy()
    }

    /// What a voice session should say.
    ///
    /// `unverified` deliberately never renders as plain "completed
    /// successfully" — the work was done and nothing claims it was checked,
    /// and a listener must be told the difference.
    pub fn voice_line(&self) -> Option<String> {
        if !self.is_voice_worthy() {
            return None;
        }
        Some(match self.kind {
            VerificationEventKind::Queued => "Running the project's checks now.".to_string(),
            VerificationEventKind::Passed => {
                "Completed, and the project's checks passed.".to_string()
            },
            VerificationEventKind::ChecksFailed => {
                "The checks failed; the engineer is working on a fix.".to_string()
            },
            VerificationEventKind::Unverified => {
                "Completed, but no checks were configured.".to_string()
            },
            VerificationEventKind::Exhausted => {
                "Stopped: the checks kept failing and the repair budget ran out.".to_string()
            },
            _ => return None,
        })
    }
}

/// Sink for lifecycle events.
///
/// A trait rather than a concrete broadcaster so the controller can be
/// exercised without a running event bus — and so a delivery failure is
/// visibly a *projection* failure, never something that can roll back
/// committed state.
pub trait VerificationEventSink: Send + Sync {
    fn emit(&self, event: &VerificationEvent);
}

/// Discards everything. The default until the runtime bus is wired.
pub struct NullEventSink;

impl VerificationEventSink for NullEventSink {
    fn emit(&self, _event: &VerificationEvent) {}
}

/// Collects events in memory. For tests and for observe-mode blast-radius
/// reporting.
#[derive(Default)]
pub struct RecordingEventSink {
    events: std::sync::Mutex<Vec<VerificationEvent>>,
}

impl RecordingEventSink {
    pub fn events(&self) -> Vec<VerificationEvent> {
        self.events.lock().map(|e| e.clone()).unwrap_or_default()
    }

    pub fn kinds(&self) -> Vec<VerificationEventKind> {
        self.events().iter().map(|e| e.kind).collect()
    }
}

impl VerificationEventSink for RecordingEventSink {
    fn emit(&self, event: &VerificationEvent) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event.clone());
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::file_edit::transaction::TransactionScope;
    use crate::magician_v2::execution::verification::gate::{
        GateBudgets, GateOrigin, GateStatus, VerificationGate,
    };

    fn gate() -> VerificationGate {
        VerificationGate::new(
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
                coding_engine: None,
                constraint_auto: false,
                coding_invocation_ref: None,
                child_execution_id: None,
            },
            GateBudgets::default(),
        )
        .unwrap()
    }

    #[test]
    fn checks_failed_is_not_named_failed_and_is_not_terminal() {
        // Naming it `failed` would make consumers render a red terminal state
        // mid-loop, while repair is still expected to run.
        assert_eq!(
            VerificationEventKind::ChecksFailed.as_str(),
            "verification.checks_failed"
        );
        assert!(!VerificationEventKind::ChecksFailed.is_terminal());
    }

    #[test]
    fn voice_hears_transitions_not_commands() {
        assert!(!VerificationEventKind::CommandCompleted.is_voice_worthy());
        assert!(!VerificationEventKind::Started.is_voice_worthy());
        assert!(!VerificationEventKind::AttemptIndeterminate.is_voice_worthy());
        for kind in [
            VerificationEventKind::Queued,
            VerificationEventKind::Passed,
            VerificationEventKind::ChecksFailed,
            VerificationEventKind::Unverified,
            VerificationEventKind::Exhausted,
        ] {
            assert!(kind.is_voice_worthy(), "{kind:?} should reach voice");
        }
    }

    #[test]
    fn unverified_is_never_narrated_as_plain_success() {
        let mut g = gate();
        g.status = GateStatus::Unverified;
        let event = VerificationEvent::from_gate(
            VerificationEventKind::Unverified,
            &g,
            1,
            None,
            None,
            None,
        );
        let line = event.voice_line().unwrap();
        assert!(line.contains("no checks were configured"));
        assert!(
            !line.contains("successfully"),
            "must not imply the work was checked"
        );
    }

    #[test]
    fn resulting_state_is_taken_from_the_gate_not_invented() {
        let mut g = gate();
        g.status = GateStatus::Verified;
        let event =
            VerificationEvent::from_gate(VerificationEventKind::Passed, &g, 3, None, None, None);
        assert_eq!(event.resulting_state, VerificationState::Verified);
        assert_eq!(event.generation, g.generation);
        assert_eq!(event.candidate_revision, g.current_candidate);
    }

    #[test]
    fn every_event_carries_correlation_identity() {
        let g = gate();
        let att = AttestationId::new();
        let attempt = AttemptId::new();
        let event = VerificationEvent::from_gate(
            VerificationEventKind::Started,
            &g,
            7,
            Some(att.clone()),
            Some(attempt.clone()),
            Some("detail".into()),
        );
        assert_eq!(event.gate_id, g.gate_id);
        assert_eq!(event.attestation_id, Some(att));
        assert_eq!(event.attempt_id, Some(attempt));
        assert_eq!(event.sequence, 7);
        assert_eq!(event.root_task_id, "task-1");
    }

    #[test]
    fn unavailable_is_terminal_and_distinct_from_unverified() {
        assert!(VerificationEventKind::Unavailable.is_terminal());
        assert_ne!(
            VerificationEventKind::Unavailable.as_str(),
            VerificationEventKind::Unverified.as_str()
        );
        // Unavailable is deliberately NOT voice-worthy as a success-shaped
        // line; it has no reassuring narration.
        assert!(!VerificationEventKind::Unavailable.is_voice_worthy());
    }

    #[test]
    fn recording_sink_captures_order() {
        let sink = RecordingEventSink::default();
        let g = gate();
        for (i, kind) in [
            VerificationEventKind::Queued,
            VerificationEventKind::Started,
            VerificationEventKind::Passed,
        ]
        .into_iter()
        .enumerate()
        {
            sink.emit(&VerificationEvent::from_gate(
                kind, &g, i as u64, None, None, None,
            ));
        }
        assert_eq!(
            sink.kinds(),
            vec![
                VerificationEventKind::Queued,
                VerificationEventKind::Started,
                VerificationEventKind::Passed
            ]
        );
    }
}

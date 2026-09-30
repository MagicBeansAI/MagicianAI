//! The App Copilot rail's product logic, split out of `TutorRun`'s home
//! module (plan workstream 1.2c /
//! docs/plans/2026-08-26-platform-layering-and-app-extraction-plan.md;
//! see also 4.1's prerequisite refactor).
//!
//! What lives here is the code that exists because a tutor run can ride
//! the App Copilot product rail rather than the Personal Tutor one: the
//! rail identity itself (`TutorProductRail`), the single-use manual-action
//! check receipt and its storyboard matcher, the store's receipt minting
//! and user-action preemption entry points, and the demo-and-cleanup
//! guards. Everything moved is a pure move — `tutor.rs` re-exports the
//! public types so every existing `crate::magician_v2::tutor::…` import
//! path keeps resolving unedited — and the run store, step state machine,
//! and validators stay in `tutor.rs` where the shared run model lives.

use serde::{Deserialize, Serialize};

use super::{
    current_unix_time_ms, TutorActionEnvelope, TutorRun, TutorRunMode, TutorRunStore,
    TutorSafetyLevel, TutorStep, TutorStepKind, TutorUserActionEvent, TutorUserActionRecordResult,
};

/// A no-user-action check authorizes only the immediately following App
/// Copilot delegation. This is deliberately short lived and storyboard-bound.
pub(crate) const COPILOT_ACTION_CHECK_MAX_AGE_MS: i64 = 30_000;

/// Product identity retained by the run after the initiating chat turn. The
/// action-capable App Copilot rail must not be inferred from `TutorRunMode`:
/// mode describes safety/behavior, while the rail determines routing and
/// authorization on later background turns.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TutorProductRail {
    #[default]
    PersonalTutor,
    AppCopilot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopilotActionCheckReceipt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storyboard_step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storyboard_step_label: Option<String>,
    pub checked_at_ms: i64,
}

/// Whether normalized turn text asks for a demo flow whose created objects
/// must be cleaned up before the run can complete. Both halves are
/// required: "demo" alone is a Personal Tutor concept-demonstration
/// request, and cleanup wording alone has nothing to clean up.
pub(crate) fn tutor_prompt_requests_demo_cleanup(normalized: &str) -> bool {
    let has_demo = normalized.contains("demo") || normalized.contains("demonstrate");
    let has_cleanup = normalized.contains("cleanup")
        || normalized.contains("clean up")
        || normalized.contains("clean it up")
        || normalized.contains("delete it")
        || normalized.contains("remove it");
    has_demo && has_cleanup
}

impl TutorRun {
    pub fn is_app_copilot(&self) -> bool {
        self.product_rail == TutorProductRail::AppCopilot
    }

    pub(crate) fn has_matching_copilot_action_check(&self, envelope: &TutorActionEnvelope) -> bool {
        let Some(receipt) = self.copilot_action_check.as_ref() else {
            return false;
        };
        let age_ms = current_unix_time_ms().saturating_sub(receipt.checked_at_ms);
        age_ms <= COPILOT_ACTION_CHECK_MAX_AGE_MS
            && storyboard_identity_matches_receipt(receipt, envelope)
    }

    /// The App Copilot demo-and-cleanup completion guard, extracted from
    /// `TutorRun::complete` unchanged: a demo-and-cleanup run cannot
    /// complete before its run-created objects are recorded, and not while
    /// any of them remains uncleared. `None` frees the caller to continue
    /// the shared completion checks.
    pub(crate) fn app_copilot_demo_cleanup_completion_error(&self) -> Option<String> {
        if !(self.is_app_copilot() && self.mode == TutorRunMode::DemoAndCleanup) {
            return None;
        }
        if self.created_objects.is_empty() {
            return Some(
                "cannot complete App Copilot demo-and-cleanup before recording the object created \
                 by this run"
                    .to_string(),
            );
        }
        let uncleared = self
            .created_objects
            .iter()
            .filter(|object| {
                !self
                    .removed_created_object_labels
                    .iter()
                    .any(|label| label == &object.label)
            })
            .map(|object| object.label.as_str())
            .collect::<Vec<_>>();
        if uncleared.is_empty() {
            None
        } else {
            Some(format!(
                "cannot complete App Copilot demo-and-cleanup while run-owned object(s) remain: {}",
                uncleared.join(", ")
            ))
        }
    }
}

impl TutorRunStore {
    pub fn record_copilot_action_check(
        &self,
        run_id: &str,
        storyboard_step_id: Option<String>,
        storyboard_step_label: Option<String>,
    ) -> Result<TutorRun, String> {
        let mut inner = self.lock_inner()?;
        let Some(run) = inner.runs.get_mut(run_id) else {
            return Err(format!("tutor run `{run_id}` not found"));
        };
        if !run.is_app_copilot() {
            return Err("manual-action checks apply only to App Copilot runs".to_string());
        }
        if run.pending_action.is_some() {
            return Err(
                "cannot check for a new user action while another action is pending".to_string(),
            );
        }
        if !run.has_successful_draw_storyboard_binding(
            storyboard_step_id.as_deref(),
            storyboard_step_label.as_deref(),
        ) {
            return Err(
                "App Copilot action check must reference the immediately preceding successful \
                 screen-draw storyboard step"
                    .to_string(),
            );
        }
        run.copilot_action_check = Some(CopilotActionCheckReceipt {
            storyboard_step_id,
            storyboard_step_label,
            checked_at_ms: current_unix_time_ms(),
        });
        Ok(run.clone())
    }

    pub fn record_user_action_event_and_preempt_pending(
        &self,
        event: TutorUserActionEvent,
    ) -> Result<TutorUserActionRecordResult, String> {
        let mut inner = self.lock_inner()?;
        let (run_snapshot, pending_execution_id) = {
            let Some(run) = inner.runs.get_mut(&event.run_id) else {
                return Err(format!("tutor run `{}` not found", event.run_id));
            };
            if run.status.is_terminal() {
                return Err(format!(
                    "cannot record user action for terminal tutor run `{}`",
                    event.run_id
                ));
            }

            (run.clone(), run.pending_action_execution_id.clone())
        };

        let events = inner
            .user_action_events_by_run
            .entry(event.run_id.clone())
            .or_default();
        events.push(event.clone());
        if events.len() > 32 {
            let drop_count = events.len().saturating_sub(32);
            events.drain(0..drop_count);
        }
        Ok(TutorUserActionRecordResult {
            event,
            applied_to_pending_action: pending_execution_id.is_some(),
            preempted_execution_id: pending_execution_id,
            run: Some(run_snapshot),
        })
    }

    /// Finalize manual preemption only after the delegated execution has been
    /// cancelled or is already terminal. Until this method succeeds the
    /// original pending action remains authoritative.
    pub fn finalize_user_action_preemption(
        &self,
        event: &TutorUserActionEvent,
    ) -> Result<TutorRun, String> {
        let mut inner = self.lock_inner()?;
        let Some(run) = inner.runs.get_mut(&event.run_id) else {
            return Err(format!("tutor run `{}` not found", event.run_id));
        };
        let Some(pending) = run.pending_action.clone() else {
            return Ok(run.clone());
        };
        let created_object = run.pending_created_object.take();
        let removed_object_label = (pending.safety == TutorSafetyLevel::SessionOwnedDestructive)
            .then(|| pending.target.clone())
            .flatten();
        let target = pending.target.clone().or_else(|| event.target.clone());
        run.propose_step(TutorStep {
            kind: TutorStepKind::Observe,
            label: "observe after user-performed Copilot action".to_string(),
            target: target.clone(),
            expected_state: Some(event.evidence.clone()),
            safety: TutorSafetyLevel::VisualOnly,
            source_entity_ids: Vec::new(),
            visual_entity_map: None,
        })?;
        run.propose_step(TutorStep {
            kind: TutorStepKind::Verify,
            label: format!("verify user performed {}", pending.label),
            target,
            expected_state: Some(event.evidence.clone()),
            safety: TutorSafetyLevel::VisualOnly,
            source_entity_ids: Vec::new(),
            visual_entity_map: None,
        })?;
        if let Some(object) = created_object {
            run.record_created_object(object);
        }
        if let Some(label) = removed_object_label {
            if run.owns_object_label(&label) && !run.removed_created_object_labels.contains(&label)
            {
                run.removed_created_object_labels.push(label);
            }
        }
        let updated = run.clone();
        if let Some(events) = inner.user_action_events_by_run.get_mut(&event.run_id) {
            events.retain(|candidate| candidate != event);
        }
        Ok(updated)
    }
}

fn storyboard_identity_matches_receipt(
    receipt: &CopilotActionCheckReceipt,
    envelope: &TutorActionEnvelope,
) -> bool {
    let id_matches = receipt
        .storyboard_step_id
        .as_deref()
        .is_some_and(|receipt_id| {
            envelope
                .storyboard_step_id
                .as_deref()
                .is_some_and(|envelope_id| {
                    !receipt_id.trim().is_empty() && receipt_id.trim() == envelope_id.trim()
                })
        });
    let label_matches = receipt
        .storyboard_step_label
        .as_deref()
        .is_some_and(|receipt_label| {
            envelope
                .storyboard_step_label
                .as_deref()
                .is_some_and(|envelope_label| {
                    !receipt_label.trim().is_empty()
                        && receipt_label.trim() == envelope_label.trim()
                })
        });
    let id_conflicts = receipt
        .storyboard_step_id
        .as_deref()
        .is_some_and(|receipt_id| {
            envelope
                .storyboard_step_id
                .as_deref()
                .is_some_and(|envelope_id| receipt_id.trim() != envelope_id.trim())
        });
    let label_conflicts = receipt
        .storyboard_step_label
        .as_deref()
        .is_some_and(|receipt_label| {
            envelope
                .storyboard_step_label
                .as_deref()
                .is_some_and(|envelope_label| receipt_label.trim() != envelope_label.trim())
        });
    (id_matches || label_matches) && !id_conflicts && !label_conflicts
}

#[cfg(test)]
mod tests {
    use crate::magician_v2::tutor::TutorCreatedObject;

    use super::*;

    fn receipt(id: Option<&str>, label: Option<&str>) -> CopilotActionCheckReceipt {
        CopilotActionCheckReceipt {
            storyboard_step_id: id.map(str::to_string),
            storyboard_step_label: label.map(str::to_string),
            checked_at_ms: 0,
        }
    }

    fn envelope(id: Option<&str>, label: Option<&str>) -> TutorActionEnvelope {
        TutorActionEnvelope {
            run_id: "run-1".to_string(),
            step_kind: TutorStepKind::Click,
            storyboard_step_id: id.map(str::to_string),
            storyboard_step_label: label.map(str::to_string),
            target: "Open button".to_string(),
            expected_state: "Item is open".to_string(),
            safety: TutorSafetyLevel::ReversibleAction,
            observation_evidence: "Open button is visible".to_string(),
            action_instruction: None,
            created_object: None,
        }
    }

    #[test]
    fn storyboard_identity_matches_on_id_or_label_but_rejects_conflicts() {
        // Either identity alone is enough…
        assert!(storyboard_identity_matches_receipt(
            &receipt(Some("step-open"), None),
            &envelope(Some("step-open"), None),
        ));
        assert!(storyboard_identity_matches_receipt(
            &receipt(None, Some("Open item")),
            &envelope(None, Some("Open item")),
        ));
        // …and whitespace around either spelling is not a distinction.
        assert!(storyboard_identity_matches_receipt(
            &receipt(Some("step-open"), None),
            &envelope(Some(" step-open "), None),
        ));
        // But a conflicting id or label vetoes even when the other half
        // matches, so a receipt for one step cannot authorize another.
        assert!(!storyboard_identity_matches_receipt(
            &receipt(Some("step-open"), Some("Open item")),
            &envelope(Some("step-open"), Some("Delete item")),
        ));
        assert!(!storyboard_identity_matches_receipt(
            &receipt(Some("step-open"), None),
            &envelope(Some("step-close"), None),
        ));
        // Absent identities on both sides match nothing.
        assert!(!storyboard_identity_matches_receipt(
            &receipt(None, None),
            &envelope(None, None),
        ));
    }

    #[test]
    fn demo_cleanup_requires_both_demo_and_cleanup_wording() {
        assert!(tutor_prompt_requests_demo_cleanup(
            "demo the flow then clean it up"
        ));
        assert!(tutor_prompt_requests_demo_cleanup(
            "demonstrate the rule and remove it afterwards"
        ));
        assert!(!tutor_prompt_requests_demo_cleanup(
            "demo the flow with narration"
        ));
        assert!(!tutor_prompt_requests_demo_cleanup(
            "clean up the leftovers from earlier"
        ));
    }

    #[test]
    fn demo_cleanup_completion_guard_binds_to_the_app_copilot_rail() {
        // A Personal Tutor run in the same mode is not the rail's concern.
        let tutor_run = TutorRun::new_incremental(
            "run-tutor",
            TutorRunMode::DemoAndCleanup,
            "@tutor demo the flow then clean it up",
        )
        .expect("tutor run");
        assert!(!tutor_run.is_app_copilot());
        assert_eq!(tutor_run.mode, TutorRunMode::DemoAndCleanup);
        assert!(tutor_run
            .app_copilot_demo_cleanup_completion_error()
            .is_none());

        let copilot_run = TutorRun::new_incremental(
            "run-copilot",
            TutorRunMode::DemoAndCleanup,
            "@copilot demo the flow then clean it up",
        )
        .expect("copilot run");
        assert!(copilot_run.is_app_copilot());
        let missing_record = copilot_run
            .app_copilot_demo_cleanup_completion_error()
            .expect("must record the run-created object first");
        assert!(missing_record.contains("before recording the object"));

        let mut recorded = copilot_run.clone();
        recorded.record_created_object(TutorCreatedObject {
            label: "scratch note".to_string(),
            object_type: None,
            evidence: None,
        });
        let uncleared = recorded
            .app_copilot_demo_cleanup_completion_error()
            .expect("uncleared run-owned object blocks completion");
        assert!(uncleared.contains("scratch note"));

        recorded
            .removed_created_object_labels
            .push("scratch note".to_string());
        assert!(recorded
            .app_copilot_demo_cleanup_completion_error()
            .is_none());
    }
}

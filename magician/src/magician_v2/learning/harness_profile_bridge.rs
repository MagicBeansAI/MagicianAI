//! Boundary D's routing: a reflection's proposed guidance revision reaches
//! live harness guidance only through an evaluation and an owner.
//!
//! `HarnessSupplementalProfile::apply` has always demanded a validated
//! proposal, an evaluation id and an explicit approval. Nothing produced any
//! of them: the candidate type existed, the apply path existed, and no route
//! joined them, so the boundary was safe by being inert. This is the route,
//! and it is deliberately three separate calls rather than one:
//!
//! 1. [`route_candidate`] stages a reflection's proposal for review. It never
//!    applies, whatever the candidate's state or risk level says — the other
//!    bridges auto-apply low-risk candidates, and that is exactly the thing
//!    this boundary must not do.
//! 2. [`record_evaluation`] is the controlled run. It writes what was tried,
//!    what the acceptance condition was, and what actually happened.
//! 3. [`apply_approved_candidate`] is the only writer, and it needs both an
//!    owner and a *passing* evaluation record it looks up itself.
//!
//! Step 3 stamps the evaluation id from the record it found rather than from
//! the proposal. The proposal must carry one to validate, but a proposal is
//! model-authored text: left to itself it can name an evaluation that never
//! ran, and the field would attest to nothing.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::warn;

use crate::magician_v2::{
    artifact_v2::workspace::ArtifactV2Workspace,
    harness::{ProfileRevisionProposal, SupplementalProfileStore},
};

use super::{
    CreateLearningEventRequest, LearningCandidate, LearningCandidateState, LearningScope,
    LearningStore,
};

const BRIDGE_ACTOR: &str = "learning_harness_profile_bridge";

pub const HARNESS_PROFILE_CANDIDATE_STAGED_EVENT: &str = "harness_profile_candidate_staged";
pub const HARNESS_PROFILE_CANDIDATE_INVALID_EVENT: &str = "harness_profile_candidate_invalid";
pub const HARNESS_PROFILE_EVALUATION_RECORDED_EVENT: &str = "harness_profile_evaluation_recorded";
pub const HARNESS_PROFILE_REVISION_APPLIED_EVENT: &str = "harness_profile_revision_applied";

/// The controlled run that stands between a proposal and live guidance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HarnessProfileEvaluationRecord {
    pub evaluation_id: String,
    pub candidate_id: String,
    pub program_relative_path: String,
    /// Who or what ran it. A harness cycle, a named eval lane, or an operator.
    pub runner: String,
    /// Copied from the proposal so the record reads on its own later, when the
    /// candidate may have moved on.
    pub evaluation_case: String,
    pub acceptance_condition: String,
    /// What actually happened, in the runner's words.
    pub observed: String,
    pub passed: bool,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LearningHarnessProfileRouteOutcome {
    pub candidate_id: String,
    pub staged: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LearningHarnessProfileApplyOutcome {
    pub candidate_id: String,
    pub applied: bool,
    pub revision: Option<u32>,
    pub evaluation_id: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct LearningHarnessProfileBridge {
    workspace_layout: ArtifactV2Workspace,
}

impl LearningHarnessProfileBridge {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    /// Stage a reflection's proposal for owner review. Never applies.
    pub fn route_candidate(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        candidate: &LearningCandidate,
    ) -> Result<LearningHarnessProfileRouteOutcome> {
        if !candidate.candidate_type.is_harness_profile_candidate() {
            return Ok(LearningHarnessProfileRouteOutcome {
                candidate_id: candidate.id.clone(),
                staged: false,
                reason: "not_a_harness_profile_candidate".to_string(),
            });
        }

        let stored = match store.read_candidate(scope, &candidate.id) {
            Ok(value) => Some(value),
            Err(error) if store.error_is_not_found(&error) => None,
            Err(error) => return Err(error),
        };
        let candidate = stored.as_ref().unwrap_or(candidate);
        if candidate.state.is_terminal() {
            return Ok(LearningHarnessProfileRouteOutcome {
                candidate_id: candidate.id.clone(),
                staged: false,
                reason: format!("candidate_state_is_terminal: {}", candidate.state.as_str()),
            });
        }

        match parse_proposal(candidate) {
            Ok((proposal, program_relative_path)) => {
                store.append_event(
                    scope.clone(),
                    CreateLearningEventRequest {
                        principal: None,
                        workspace: None,
                        event_type: HARNESS_PROFILE_CANDIDATE_STAGED_EVENT.to_string(),
                        agent_id: candidate.source_agent_id.clone(),
                        task_id: candidate.source_task_id.clone(),
                        execution_id: candidate.source_execution_id.clone(),
                        chat_session_id: candidate.source_chat_session_id.clone(),
                        summary: format!(
                            "Harness profile revision `{}` staged for evaluation and owner approval.",
                            candidate.id
                        ),
                        evidence_refs: candidate.evidence_refs.clone(),
                        payload: json!({
                            "candidate_id": candidate.id,
                            "program_relative_path": program_relative_path,
                            "sections": proposal.after.keys().collect::<Vec<_>>(),
                            "evaluation_case": proposal.evaluation_case,
                            "acceptance_condition": proposal.acceptance_condition,
                            "risk": proposal.risk,
                            "rollback_plan": proposal.rollback_plan,
                        }),
                    },
                )?;
                if matches!(
                    candidate.state,
                    LearningCandidateState::Observed | LearningCandidateState::Proposed
                ) {
                    let _ = store.transition_candidate(
                        scope,
                        &candidate.id,
                        LearningCandidateState::Triaged,
                        BRIDGE_ACTOR,
                        "staged_for_owner_approval",
                        "a harness profile revision is never applied from a reflection alone"
                            .to_string(),
                        candidate.evidence_refs.clone(),
                    );
                }
                Ok(LearningHarnessProfileRouteOutcome {
                    candidate_id: candidate.id.clone(),
                    staged: true,
                    reason: "staged_for_owner_approval".to_string(),
                })
            },
            Err(error) => {
                store.append_event(
                    scope.clone(),
                    CreateLearningEventRequest {
                        principal: None,
                        workspace: None,
                        event_type: HARNESS_PROFILE_CANDIDATE_INVALID_EVENT.to_string(),
                        agent_id: candidate.source_agent_id.clone(),
                        task_id: candidate.source_task_id.clone(),
                        execution_id: candidate.source_execution_id.clone(),
                        chat_session_id: candidate.source_chat_session_id.clone(),
                        summary: format!(
                            "Harness profile revision `{}` is not reviewable: {error}",
                            candidate.id
                        ),
                        evidence_refs: candidate.evidence_refs.clone(),
                        payload: json!({
                            "candidate_id": candidate.id,
                            "error": error.to_string(),
                        }),
                    },
                )?;
                Ok(LearningHarnessProfileRouteOutcome {
                    candidate_id: candidate.id.clone(),
                    staged: false,
                    reason: format!("invalid_proposal: {error}"),
                })
            },
        }
    }

    /// Record a controlled run of the proposal's own evaluation case.
    ///
    /// A failing run is recorded exactly like a passing one. The register this
    /// boundary came from was closed with its exit conditions unmet, and the
    /// lesson written into the profile's own history rules is that failures
    /// stay auditable.
    pub fn record_evaluation(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        candidate: &LearningCandidate,
        runner: &str,
        observed: &str,
        passed: bool,
    ) -> Result<HarnessProfileEvaluationRecord> {
        let (proposal, program_relative_path) = parse_proposal(candidate)?;
        if runner.trim().is_empty() {
            return Err(anyhow!("an evaluation needs a runner"));
        }
        if observed.trim().is_empty() {
            return Err(anyhow!("an evaluation needs an observed result"));
        }

        let record = HarnessProfileEvaluationRecord {
            evaluation_id: format!("hpe-{}", uuid::Uuid::new_v4()),
            candidate_id: candidate.id.clone(),
            program_relative_path,
            runner: runner.to_string(),
            evaluation_case: proposal.evaluation_case.clone(),
            acceptance_condition: proposal.acceptance_condition.clone(),
            observed: observed.to_string(),
            passed,
            recorded_at: Utc::now(),
        };
        self.write_evaluation(scope, &record)?;

        store.append_event(
            scope.clone(),
            CreateLearningEventRequest {
                principal: None,
                workspace: None,
                event_type: HARNESS_PROFILE_EVALUATION_RECORDED_EVENT.to_string(),
                agent_id: candidate.source_agent_id.clone(),
                task_id: candidate.source_task_id.clone(),
                execution_id: candidate.source_execution_id.clone(),
                chat_session_id: candidate.source_chat_session_id.clone(),
                summary: format!(
                    "Harness profile revision `{}` evaluated: {}",
                    candidate.id,
                    if passed { "passed" } else { "failed" }
                ),
                evidence_refs: candidate.evidence_refs.clone(),
                payload: serde_json::to_value(&record).unwrap_or(Value::Null),
            },
        )?;
        Ok(record)
    }

    /// The only writer. Needs an owner and a passing evaluation it looks up
    /// itself.
    pub fn apply_approved_candidate(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
        candidate: &LearningCandidate,
        approved_by: &str,
    ) -> Result<LearningHarnessProfileApplyOutcome> {
        if !candidate.candidate_type.is_harness_profile_candidate() {
            return Ok(refused(candidate, "not_a_harness_profile_candidate"));
        }
        if approved_by.trim().is_empty() {
            return Ok(refused(candidate, "an owner approval is required"));
        }

        let stored = match store.read_candidate(scope, &candidate.id) {
            Ok(value) => Some(value),
            Err(error) if store.error_is_not_found(&error) => None,
            Err(error) => return Err(error),
        };
        let candidate = stored.as_ref().unwrap_or(candidate);
        if candidate.state != LearningCandidateState::Approved {
            return Ok(refused(
                candidate,
                &format!(
                    "candidate_state_is_not_approved: {}",
                    candidate.state.as_str()
                ),
            ));
        }

        let Some(evaluation) = self.read_evaluation(scope, &candidate.id)? else {
            return Ok(refused(candidate, "no evaluation has been recorded"));
        };
        if !evaluation.passed {
            return Ok(refused(
                candidate,
                "the recorded evaluation did not meet its acceptance condition",
            ));
        }

        let (mut proposal, program_relative_path) = parse_proposal(candidate)?;
        // The record, not the proposal's own claim. A proposal has to carry an
        // evaluation id to validate at all, and nothing stops model-authored
        // text from naming one that never ran.
        proposal.evaluation_id = evaluation.evaluation_id.clone();

        let profile_store = SupplementalProfileStore::new(self.workspace_layout.clone());
        let profile = profile_store
            .apply_revision(
                &scope.principal,
                &scope.workspace,
                &program_relative_path,
                &proposal,
                approved_by,
            )
            .map_err(|error| anyhow!("{error}"))?;

        store.append_event(
            scope.clone(),
            CreateLearningEventRequest {
                principal: None,
                workspace: None,
                event_type: HARNESS_PROFILE_REVISION_APPLIED_EVENT.to_string(),
                agent_id: candidate.source_agent_id.clone(),
                task_id: candidate.source_task_id.clone(),
                execution_id: candidate.source_execution_id.clone(),
                chat_session_id: candidate.source_chat_session_id.clone(),
                summary: format!(
                    "Harness profile revision `{}` applied as revision {} by {approved_by}.",
                    candidate.id, profile.revision
                ),
                evidence_refs: candidate.evidence_refs.clone(),
                payload: json!({
                    "candidate_id": candidate.id,
                    "program_relative_path": program_relative_path,
                    "revision": profile.revision,
                    "evaluation_id": evaluation.evaluation_id,
                    "approved_by": approved_by,
                    "sections": proposal.after.keys().collect::<Vec<_>>(),
                }),
            },
        )?;
        // Promoted, not Implemented: the evaluation happened before the apply
        // (that is the boundary's rule), and applying is what makes the
        // guidance live — there is no later step this could be waiting on.
        let _ = store.transition_candidate(
            scope,
            &candidate.id,
            LearningCandidateState::Promoted,
            BRIDGE_ACTOR,
            "harness_profile_revision_applied",
            format!(
                "applied as revision {} against evaluation {}",
                profile.revision, evaluation.evaluation_id
            ),
            candidate.evidence_refs.clone(),
        );

        Ok(LearningHarnessProfileApplyOutcome {
            candidate_id: candidate.id.clone(),
            applied: true,
            revision: Some(profile.revision),
            evaluation_id: Some(evaluation.evaluation_id),
            reason: "applied".to_string(),
        })
    }

    fn evaluation_path(&self, scope: &LearningScope, candidate_id: &str) -> std::path::PathBuf {
        self.workspace_layout
            .programs_supplemental_profiles_dir(&scope.principal, &scope.workspace)
            .join("evaluations")
            .join(format!("{}.json", sanitize(candidate_id)))
    }

    fn write_evaluation(
        &self,
        scope: &LearningScope,
        record: &HarnessProfileEvaluationRecord,
    ) -> Result<()> {
        let path = self.evaluation_path(scope, &record.candidate_id);
        self.workspace_layout
            .write_json_atomic_path_sync(&path, record)
            .map_err(|error| anyhow!("failed to write the evaluation record: {error}"))
    }

    /// The latest evaluation for a candidate, or `None` when none has run.
    pub fn read_evaluation(
        &self,
        scope: &LearningScope,
        candidate_id: &str,
    ) -> Result<Option<HarnessProfileEvaluationRecord>> {
        let path = self.evaluation_path(scope, candidate_id);
        match self
            .workspace_layout
            .read_json_path_sync::<HarnessProfileEvaluationRecord, _>(&path)
        {
            Ok(record) => Ok(Some(record)),
            Err(_) => Ok(None),
        }
    }
}

fn refused(candidate: &LearningCandidate, reason: &str) -> LearningHarnessProfileApplyOutcome {
    LearningHarnessProfileApplyOutcome {
        candidate_id: candidate.id.clone(),
        applied: false,
        revision: None,
        evaluation_id: None,
        reason: reason.to_string(),
    }
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// A candidate's `proposed_change` as a validated proposal, plus the program
/// it supplements. `proposed_target` names the program; without it there is no
/// way to know which program's guidance this is, and guidance for the wrong
/// program is worse than none.
fn parse_proposal(candidate: &LearningCandidate) -> Result<(ProfileRevisionProposal, String)> {
    let program_relative_path = candidate
        .proposed_target
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow!("a harness profile revision must name its program in `proposed_target`")
        })?
        .to_string();

    let mut proposal: ProfileRevisionProposal =
        serde_json::from_value(candidate.proposed_change.clone()).map_err(|error| {
            anyhow!("`proposed_change` is not a harness profile revision proposal: {error}")
        })?;
    // The candidate's own id is the authority on which candidate this is.
    proposal.candidate_id = candidate.id.clone();
    proposal.validate().map_err(|error| anyhow!("{}", error))?;
    Ok((proposal, program_relative_path))
}

pub fn log_harness_profile_route_error(candidate_id: &str, error: &anyhow::Error) {
    warn!(
        candidate_id = %candidate_id,
        error = %error,
        "[LEARNING] harness profile candidate could not be routed"
    );
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    use crate::magician_v2::learning::{
        CreateLearningCandidateRequest, LearningCandidateType, LearningRiskLevel, LearningStore,
    };

    const PROGRAM: &str = "program.md";

    fn proposal_json(section_body: &str) -> Value {
        serde_json::to_value(ProfileRevisionProposal {
            candidate_id: "will-be-overwritten".to_string(),
            episode_evidence: vec!["episode-1".to_string()],
            // A plausible-looking id for an evaluation that never ran. The
            // apply path must not take this at its word.
            evaluation_id: "hpe-fabricated".to_string(),
            before: BTreeMap::new(),
            after: BTreeMap::from([(
                "operating_notes".to_string(),
                Some(section_body.to_string()),
            )]),
            expected_benefit: "fewer repeated no-progress cycles".to_string(),
            confidence: "medium".to_string(),
            risk: "low — guidance only".to_string(),
            rollback_plan: "revert the revision".to_string(),
            evaluation_case: "harness cycle on the stalled lane".to_string(),
            acceptance_condition: "two consecutive cycles settle with a decision".to_string(),
        })
        .expect("serialize")
    }

    fn seed(
        state: LearningCandidateState,
        proposed_change: Value,
        proposed_target: Option<&str>,
    ) -> (
        tempfile::TempDir,
        LearningStore,
        LearningScope,
        LearningCandidate,
    ) {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LearningStore::new(ArtifactV2Workspace::new(temp.path()));
        let scope = LearningScope::new("anonymous", "default");
        let candidate = store
            .ensure_candidate_with_id(
                scope.clone(),
                CreateLearningCandidateRequest {
                    principal: None,
                    workspace: None,
                    candidate_type: LearningCandidateType::HarnessProfileRevision,
                    state,
                    title: "Tighten the stalled-lane note".to_owned(),
                    summary: "Guidance revision proposed by reflection.".to_owned(),
                    rationale: String::new(),
                    proposed_change,
                    proposed_target: proposed_target.map(str::to_string),
                    confidence: None,
                    source_agent_id: None,
                    source_task_id: None,
                    source_execution_id: None,
                    source_chat_session_id: None,
                    event_refs: Vec::new(),
                    evidence_refs: Vec::new(),
                    risk_level: LearningRiskLevel::Low,
                    review_required: true,
                    review_reason: None,
                    review_policy: Value::Null,
                    promotion_target: None,
                    promotion_policy: Value::Null,
                },
                "lc_harness_profile_candidate",
            )
            .expect("created candidate");
        (temp, store, scope, candidate)
    }

    fn bridge(store: &LearningStore) -> LearningHarnessProfileBridge {
        LearningHarnessProfileBridge::new(store.workspace_layout().clone())
    }

    fn live_profile(
        store: &LearningStore,
        scope: &LearningScope,
    ) -> crate::magician_v2::harness::HarnessSupplementalProfile {
        SupplementalProfileStore::new(store.workspace_layout().clone()).load(
            &scope.principal,
            &scope.workspace,
            PROGRAM,
        )
    }

    /// The acceptance criterion the boundary exists for: a reflection alone
    /// changes nothing. Even a low-risk candidate the other bridges would
    /// auto-apply is only staged here.
    #[test]
    fn a_reflection_stages_a_revision_and_changes_no_live_guidance() {
        let (_t, store, scope, candidate) = seed(
            LearningCandidateState::Proposed,
            proposal_json("note"),
            Some(PROGRAM),
        );

        let outcome = bridge(&store)
            .route_candidate(&store, &scope, &candidate)
            .expect("route");

        assert!(outcome.staged);
        assert_eq!(outcome.reason, "staged_for_owner_approval");
        assert_eq!(live_profile(&store, &scope).revision, 0);
        assert_eq!(
            store
                .read_candidate(&scope, &candidate.id)
                .expect("read")
                .state,
            LearningCandidateState::Triaged
        );
    }

    /// A proposal that cannot answer an owner's questions is not reviewable.
    /// The section allowlist is the load-bearing half: `tools` is authority.
    #[test]
    fn a_revision_that_reaches_outside_the_allowlist_is_not_staged() {
        let mut bad = proposal_json("note");
        bad["after"] = json!({ "tools": "browser, shell" });
        let (_t, store, scope, candidate) =
            seed(LearningCandidateState::Proposed, bad, Some(PROGRAM));

        let outcome = bridge(&store)
            .route_candidate(&store, &scope, &candidate)
            .expect("route");

        assert!(!outcome.staged);
        assert!(
            outcome.reason.contains("not revisable"),
            "reason was: {}",
            outcome.reason
        );
        assert_eq!(live_profile(&store, &scope).revision, 0);
    }

    /// Guidance has to know which program it belongs to.
    #[test]
    fn a_revision_without_a_program_is_not_staged() {
        let (_t, store, scope, candidate) = seed(
            LearningCandidateState::Proposed,
            proposal_json("note"),
            None,
        );

        let outcome = bridge(&store)
            .route_candidate(&store, &scope, &candidate)
            .expect("route");

        assert!(!outcome.staged);
        assert!(outcome.reason.contains("proposed_target"));
    }

    /// Approval is necessary and not sufficient: without a controlled run
    /// there is nothing to say the change does what it claims.
    #[test]
    fn an_approved_revision_without_an_evaluation_is_refused() {
        let (_t, store, scope, candidate) = seed(
            LearningCandidateState::Approved,
            proposal_json("note"),
            Some(PROGRAM),
        );

        let outcome = bridge(&store)
            .apply_approved_candidate(&store, &scope, &candidate, "owner")
            .expect("apply");

        assert!(!outcome.applied);
        assert_eq!(outcome.reason, "no evaluation has been recorded");
        assert_eq!(live_profile(&store, &scope).revision, 0);
    }

    /// A failed run is recorded, and it blocks the apply rather than being
    /// quietly discarded.
    #[test]
    fn a_failed_evaluation_blocks_the_apply_and_stays_on_the_record() {
        let (_t, store, scope, candidate) = seed(
            LearningCandidateState::Approved,
            proposal_json("note"),
            Some(PROGRAM),
        );
        let b = bridge(&store);
        let record = b
            .record_evaluation(
                &store,
                &scope,
                &candidate,
                "harness-cycle",
                "one cycle settled, the second stalled again",
                false,
            )
            .expect("record");
        assert!(!record.passed);

        let outcome = b
            .apply_approved_candidate(&store, &scope, &candidate, "owner")
            .expect("apply");

        assert!(!outcome.applied);
        assert_eq!(
            outcome.reason,
            "the recorded evaluation did not meet its acceptance condition"
        );
        assert_eq!(live_profile(&store, &scope).revision, 0);
        assert!(
            b.read_evaluation(&scope, &candidate.id)
                .expect("read")
                .is_some(),
            "a failed evaluation stays auditable"
        );
    }

    /// An owner approving an unapproved candidate does not make it approved:
    /// the learning lane's state is the approval of record.
    #[test]
    fn an_owner_cannot_apply_a_candidate_the_lane_has_not_approved() {
        let (_t, store, scope, candidate) = seed(
            LearningCandidateState::Triaged,
            proposal_json("note"),
            Some(PROGRAM),
        );
        let b = bridge(&store);
        b.record_evaluation(&store, &scope, &candidate, "harness-cycle", "held", true)
            .expect("record");

        let outcome = b
            .apply_approved_candidate(&store, &scope, &candidate, "owner")
            .expect("apply");

        assert!(!outcome.applied);
        assert!(outcome.reason.contains("candidate_state_is_not_approved"));
        assert_eq!(live_profile(&store, &scope).revision, 0);
    }

    /// And with both — a passing run and an owner — the guidance actually
    /// changes, which is the half that was missing entirely.
    #[test]
    fn an_approved_and_evaluated_revision_reaches_live_guidance() {
        let (_t, store, scope, candidate) = seed(
            LearningCandidateState::Approved,
            proposal_json("Check the backlog before opening a new loop."),
            Some(PROGRAM),
        );
        let b = bridge(&store);
        let record = b
            .record_evaluation(
                &store,
                &scope,
                &candidate,
                "harness-cycle",
                "two consecutive cycles settled with a decision",
                true,
            )
            .expect("record");

        let outcome = b
            .apply_approved_candidate(&store, &scope, &candidate, "owner")
            .expect("apply");

        assert!(outcome.applied, "reason: {}", outcome.reason);
        assert_eq!(outcome.revision, Some(1));

        let profile = live_profile(&store, &scope);
        assert_eq!(profile.revision, 1);
        assert_eq!(
            profile.sections.get("operating_notes").map(String::as_str),
            Some("Check the backlog before opening a new loop.")
        );
        assert!(
            profile.render_block().is_some(),
            "the reader has something to read"
        );

        // The applied history carries the evaluation that actually ran, not
        // the id the model wrote into its own proposal.
        assert_eq!(profile.history[0].evaluation_id, record.evaluation_id);
        assert_ne!(profile.history[0].evaluation_id, "hpe-fabricated");
        assert_eq!(profile.history[0].approved_by, "owner");
        assert_eq!(
            store
                .read_candidate(&scope, &candidate.id)
                .expect("read")
                .state,
            LearningCandidateState::Promoted
        );
    }

    /// Applying twice would mean a second revision from one approval; the
    /// stale-`before` guard in the profile refuses it.
    #[test]
    fn the_same_revision_cannot_be_applied_twice() {
        let (_t, store, scope, candidate) = seed(
            LearningCandidateState::Approved,
            proposal_json("note"),
            Some(PROGRAM),
        );
        let b = bridge(&store);
        b.record_evaluation(&store, &scope, &candidate, "harness-cycle", "held", true)
            .expect("record");
        b.apply_approved_candidate(&store, &scope, &candidate, "owner")
            .expect("first apply");

        // The lane moved the candidate to Promoted, which is the first
        // refusal; the profile's stale-`before` guard is the second.
        let second = b.apply_approved_candidate(&store, &scope, &candidate, "owner");
        match second {
            Ok(outcome) => assert!(!outcome.applied, "a second apply must not succeed"),
            Err(error) => assert!(
                error.to_string().contains("stale"),
                "unexpected error: {error}"
            ),
        }
        assert_eq!(live_profile(&store, &scope).revision, 1);
    }
}

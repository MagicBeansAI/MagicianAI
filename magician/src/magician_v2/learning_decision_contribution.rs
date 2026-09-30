//! Destination owner for app-proposed learning-candidate decisions.
//!
//! Apps never own the learning substrate; they propose decisions, the owner
//! applies them. This module is the plan-2.5 apply-path seam beside
//! `attention_lane_contribution.rs`: it validates a sealed
//! [`AppLearningDecisionProposalV1`] against the port's V1 admission shape
//! (one exact replaceable source head, closed decision vocabulary, bounded
//! reason, header-carried idempotency) and, on a verified
//! [`AppLearningOwnerDecisionEnvelopeV1`] signed by the trusted desktop
//! identity, reaches the real owner-gated substrate transition
//! (`LearningStore::transition_candidate`) — the same transition path the
//! first-party `/learning/candidates/{id}/transition` API serves. Approve
//! maps to `approved`, reject to `rejected`, and snooze records its deferral
//! in the decision log without changing state; `promoted` is deliberately
//! unreachable from this port because the promotion bridges stay
//! first-party. Replays of the same signed decision are idempotent through a
//! marker in the transition's evidence refs, mirroring the memory port's
//! dedupe-key discipline. Durable proposal staging, owner-review listing and
//! the dispatch outbox arrive with the destination consumer; no existing
//! learning surface changes.

use magician_app_contract::contribution::{
    AppContributionRetractionPolicy, AppContributionUpdatePolicy, AppLearningDecisionKindV1,
    AppLearningDecisionProposalV1, AppLearningOwnerDecisionEnvelopeV1, AppLearningOwnerDecisionV1,
    APP_LEARNING_DECISION_CONTRACT_ID,
};
use serde::Serialize;

use crate::magician_v2::learning::{
    LearningCandidate, LearningCandidateState, LearningEvidenceRef, LearningScope, LearningStore,
};

/// Evidence kind stamped onto every transition this port applies. The paired
/// id is the decision's idempotency marker, so a replayed signed decision is
/// recognizably already-applied from the durable decision log alone.
const APP_LEARNING_DECISION_EVIDENCE_KIND: &str = "app_learning_decision";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppLearningContributionError {
    /// The sealed contract rejected the bytes (caps, digests, vocabulary, or
    /// an unknown field).
    InvalidContract(String),
    /// The owner envelope's signature or sealed digests failed verification.
    InvalidSignature(String),
    /// Structurally valid, but not admissible through this port's V1 shape.
    NotAdmissible(&'static str),
    /// Applying a valid accepted decision against the learning substrate
    /// failed (missing candidate, terminal candidate, lifetime, or a store
    /// error).
    Apply(String),
}

impl std::fmt::Display for AppLearningContributionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidContract(error) => {
                write!(formatter, "invalid app-learning contract: {error}")
            },
            Self::InvalidSignature(error) => {
                write!(
                    formatter,
                    "app-learning owner signature is invalid: {error}"
                )
            },
            Self::NotAdmissible(rule) => {
                write!(formatter, "inadmissible app-learning decision: {rule}")
            },
            Self::Apply(error) => {
                write!(formatter, "app-learning apply failed: {error}")
            },
        }
    }
}

impl std::error::Error for AppLearningContributionError {}

/// Fail-closed V1 admission shape for a staged learning-decision proposal.
///
/// Mirrors the memory and attention ports' `StageProposal` guard: contract
/// validation runs first, then the port-specific shape — exactly one source
/// head, declared replaceable and tombstoned on any drift. Everything else
/// (state mapping, transition admission) stays with the owner.
pub fn validate_app_learning_decision(
    proposal: &AppLearningDecisionProposalV1,
) -> Result<(), AppLearningContributionError> {
    proposal
        .validate()
        .map_err(|error| AppLearningContributionError::InvalidContract(error.to_string()))?;
    if proposal.header.destination_contract_id != APP_LEARNING_DECISION_CONTRACT_ID {
        return Err(AppLearningContributionError::NotAdmissible(
            "V1 app-learning ingress requires the learning-decision destination contract",
        ));
    }
    if proposal.header.update_policy != AppContributionUpdatePolicy::ReplaceExactSourceHead
        || proposal.header.retraction_policy
            != AppContributionRetractionPolicy::TombstoneOnAnySourceDrift
        || proposal.header.sources.len() != 1
    {
        return Err(AppLearningContributionError::NotAdmissible(
            "V1 app-learning ingress requires one exact replaceable source head",
        ));
    }
    Ok(())
}

/// Domain-separated idempotency key for one proposed decision.
///
/// Mirrors the memory and attention ports' source high-water key so the
/// destination consumer can deduplicate response-loss retries without a new
/// identity scheme: installation + scope + exact source + header
/// `dedupe_key`.
pub fn app_learning_decision_idempotency_key(
    proposal: &AppLearningDecisionProposalV1,
) -> Result<String, AppLearningContributionError> {
    let source =
        proposal
            .header
            .sources
            .first()
            .ok_or(AppLearningContributionError::NotAdmissible(
                "V1 app-learning proposal lost its source",
            ))?;
    digest_serialized(
        "magician.app-learning-decision-high-water.v1",
        &(
            &proposal.header.installation_id,
            &proposal.header.scope_binding_ref,
            &source.canonical_source_ref,
            &proposal.header.dedupe_key,
        ),
    )
}

/// Stable replay marker for one accepted decision.
///
/// Derived from the sealed proposal identity (not the mutable header
/// revision), so re-applying the same signed decision is detectable from the
/// durable decision log while a genuinely different proposal for the same
/// candidate remains a distinct transition.
pub fn app_learning_decision_marker(
    proposal: &AppLearningDecisionProposalV1,
) -> Result<String, AppLearningContributionError> {
    digest_serialized(
        "magician.app-learning-decision-marker.v1",
        &(
            &proposal.header.installation_id,
            &proposal.candidate_id,
            &proposal.proposal_digest,
        ),
    )
}

/// Owner-owned state mapping for the closed decision vocabulary.
///
/// `None` means "record the decision without changing state" — the snooze
/// deferral. `promoted` is unreachable by construction: the promotion
/// bridges (memory, procedure, program-state) stay first-party, exactly as
/// the plan's scope-limited apply path requires.
pub fn app_learning_target_state(
    proposal: &AppLearningDecisionProposalV1,
) -> Option<LearningCandidateState> {
    match proposal.decision {
        AppLearningDecisionKindV1::Approve => Some(LearningCandidateState::Approved),
        AppLearningDecisionKindV1::Reject => Some(LearningCandidateState::Rejected),
        AppLearningDecisionKindV1::Snooze => None,
    }
}

/// Actor recorded on transitions this port applies. The installation id
/// keeps first-party consoles and the decision log separable without a new
/// identity scheme.
pub fn app_learning_decision_actor(proposal: &AppLearningDecisionProposalV1) -> String {
    format!("app-learning-decision:{}", proposal.header.installation_id)
}

/// Outcome of applying one signed owner decision.
#[derive(Debug, Clone, PartialEq)]
pub enum AppLearningDecisionApplication {
    /// The decision was applied through the real substrate transition.
    Applied { candidate: LearningCandidate },
    /// The same signed decision was already applied; the substrate is
    /// unchanged and the current candidate is returned for the receipt.
    AlreadyApplied { candidate: LearningCandidate },
    /// The owner declined the proposal (`reject`/`revoke`): the destination
    /// is untouched and no transition was attempted.
    OwnerDeclined,
}

/// Apply a signed owner decision against the real learning substrate.
///
/// Fail-closed order: verify the envelope's ed25519 signature against the
/// trusted desktop identity, validate the sealed proposal and the port's V1
/// admission shape, require the application moment inside the proposal
/// lifetime, then check the durable decision log for this decision's replay
/// marker before calling `LearningStore::transition_candidate` — the same
/// owner-gated transition the first-party learning API serves. Only an
/// owner `accept` touches the store; `reject`/`revoke` decline without a
/// transition. Terminal candidates fail closed rather than resurrecting a
/// review the substrate has already closed.
pub fn apply_signed_app_learning_owner_decision(
    envelope: &AppLearningOwnerDecisionEnvelopeV1,
    desktop_identity_public_key_hex: &str,
    store: &LearningStore,
    scope: &LearningScope,
    now_ms: i64,
) -> Result<AppLearningDecisionApplication, AppLearningContributionError> {
    envelope
        .verify_signature(desktop_identity_public_key_hex)
        .map_err(|error| AppLearningContributionError::InvalidSignature(error.to_string()))?;
    let proposal = &envelope.review.proposal;
    validate_app_learning_decision(proposal)?;
    if envelope.decision != AppLearningOwnerDecisionV1::Accept {
        return Ok(AppLearningDecisionApplication::OwnerDeclined);
    }
    if now_ms < proposal.header.issued_at_ms || now_ms >= proposal.header.expires_at_ms {
        return Err(AppLearningContributionError::Apply(
            "app-learning decision application is outside the proposal lifetime".to_owned(),
        ));
    }
    let candidate = store
        .read_candidate(scope, &proposal.candidate_id)
        .map_err(|error| {
            AppLearningContributionError::Apply(format!(
                "learning candidate `{}` could not be read: {error:#}",
                proposal.candidate_id
            ))
        })?;
    let marker = app_learning_decision_marker(proposal)?;
    let already_applied = store
        .read_decisions(scope, &proposal.candidate_id)
        .map_err(|error| {
            AppLearningContributionError::Apply(format!(
                "learning candidate `{}` decisions could not be read: {error:#}",
                proposal.candidate_id
            ))
        })?
        .iter()
        .any(|entry| {
            entry.evidence_refs.iter().any(|evidence| {
                evidence.kind == APP_LEARNING_DECISION_EVIDENCE_KIND
                    && evidence.id.as_deref() == Some(marker.as_str())
            })
        });
    if already_applied {
        return Ok(AppLearningDecisionApplication::AlreadyApplied { candidate });
    }
    let target_state =
        app_learning_target_state(proposal).unwrap_or_else(|| candidate.state.clone());
    // Every decision — a state transition or a snooze's same-state deferral —
    // fails closed on a terminal candidate; the review the substrate closed
    // cannot be reopened or annotated through this port.
    if candidate.state.is_terminal() {
        return Err(AppLearningContributionError::Apply(format!(
            "learning candidate `{}` is terminal in state `{}` and accepts no further decisions \
             (declined `{}`)",
            proposal.candidate_id,
            candidate.state.as_str(),
            target_state.as_str()
        )));
    }
    let evidence = LearningEvidenceRef {
        kind: APP_LEARNING_DECISION_EVIDENCE_KIND.to_owned(),
        id: Some(marker),
        path: None,
        uri: None,
        summary: Some(format!(
            "magician.learning-decision proposal {}",
            proposal.header.proposal_id
        )),
    };
    let actor = app_learning_decision_actor(proposal);
    let decision = format!("app_{}", proposal.decision.as_str());
    store
        .transition_candidate(
            scope,
            &proposal.candidate_id,
            target_state,
            actor,
            decision,
            proposal.reason.clone(),
            vec![evidence],
        )
        .map(|candidate| AppLearningDecisionApplication::Applied { candidate })
        .map_err(|error| {
            AppLearningContributionError::Apply(format!(
                "learning candidate `{}` could not transition: {error:#}",
                proposal.candidate_id
            ))
        })
}

fn digest_serialized<T: Serialize>(
    domain: &str,
    value: &T,
) -> Result<String, AppLearningContributionError> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        AppLearningContributionError::InvalidContract(format!(
            "app-learning digest input is not serializable: {error}"
        ))
    })?;
    let mut framed = Vec::with_capacity(domain.len() + bytes.len() + 16);
    framed.extend_from_slice(&(domain.len() as u64).to_be_bytes());
    framed.extend_from_slice(domain.as_bytes());
    framed.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    framed.extend_from_slice(&bytes);
    Ok(magician_app_contract::contribution::content_digest(&framed))
}

#[cfg(test)]
mod tests {
    use ring::signature::KeyPair;

    use magician_app_contract::contribution::{
        content_digest, AppContributionClassification, AppContributionEvidenceClass,
        AppContributionHandlingLabelsV1, AppContributionModelProcessing,
        AppContributionSettlementRefV1, AppContributionSourceHeaderV1, AppContributionSourceRefV1,
        AppLearningOwnerReviewV1,
    };

    use super::*;
    use crate::magician_v2::{
        artifact_v2::workspace::ArtifactV2Workspace,
        learning::{CreateLearningCandidateRequest, LearningCandidateType, LearningRiskLevel},
    };

    fn digest(label: &str) -> String {
        content_digest(label.as_bytes())
    }

    fn source_header() -> AppContributionSourceHeaderV1 {
        AppContributionSourceHeaderV1 {
            contract_version: 1,
            destination_contract_id: APP_LEARNING_DECISION_CONTRACT_ID.to_owned(),
            destination_contract_version: 1,
            destination_schema_digest: digest("learning-destination-schema"),
            proposal_id: "learning-proposal:1".to_owned(),
            proposal_revision: 1,
            scope_binding_ref: "scope_binding:1".to_owned(),
            installation_id: "installation:1".to_owned(),
            installation_generation: 2,
            package_revision_ref: "package:1".to_owned(),
            package_content_digest: digest("package"),
            grant_revision: 3,
            grant_authority_digest: digest("grant"),
            schema_revision: 4,
            schema_digest: digest("schema"),
            workflow_id: "workflow:approve_candidate".to_owned(),
            workflow_digest: digest("workflow"),
            action_id: "action:approve_candidate".to_owned(),
            action_digest: digest("action"),
            contribution_port_id: "learning_decision".to_owned(),
            contribution_port_digest: digest("learning-port"),
            settlement: AppContributionSettlementRefV1::Mutation {
                mutation_receipt_id: "mutation:1".to_owned(),
                first_change_sequence: 7,
                last_change_sequence: 7,
            },
            sources: vec![AppContributionSourceRefV1 {
                installation_id: "installation:1".to_owned(),
                entity_name: "review_decision".to_owned(),
                record_id: "record:1".to_owned(),
                record_revision: 1,
                selected_fields: vec!["decision".to_owned()],
                canonical_source_ref: "source:record:1".to_owned(),
                canonical_source_digest: digest("source"),
                handling_labels: AppContributionHandlingLabelsV1 {
                    classification: AppContributionClassification::Personal,
                    model_processing: AppContributionModelProcessing::LocalOnly,
                    policy_digest: digest("source-policy"),
                    provenance_digest: digest("source-provenance"),
                },
            }],
            handling_labels: AppContributionHandlingLabelsV1 {
                classification: AppContributionClassification::Personal,
                model_processing: AppContributionModelProcessing::LocalOnly,
                policy_digest: digest("policy"),
                provenance_digest: digest("provenance"),
            },
            purpose: "apply-owner-reviewed-learning-decision".to_owned(),
            audiences: vec!["learning-reviewer".to_owned()],
            evidence_class: AppContributionEvidenceClass::Hypothesis,
            issued_at_ms: 1_000,
            expires_at_ms: 2_000,
            dedupe_key: "dedupe:1".to_owned(),
            update_policy: AppContributionUpdatePolicy::ReplaceExactSourceHead,
            retraction_policy: AppContributionRetractionPolicy::TombstoneOnAnySourceDrift,
        }
    }

    fn sealed_proposal(decision: AppLearningDecisionKindV1) -> AppLearningDecisionProposalV1 {
        let reason = "Approve after reviewing the evidence.".to_owned();
        AppLearningDecisionProposalV1 {
            header: source_header(),
            candidate_id: "lc_test_candidate".to_owned(),
            decision,
            reason_digest: content_digest(reason.as_bytes()),
            reason,
            snooze_until_ms: matches!(decision, AppLearningDecisionKindV1::Snooze).then_some(1_800),
            proposal_digest: String::new(),
        }
        .seal()
        .expect("sealed learning proposal")
    }

    fn snooze_proposal() -> AppLearningDecisionProposalV1 {
        sealed_proposal(AppLearningDecisionKindV1::Snooze)
    }

    struct SigningIdentity {
        key_pair: ring::signature::Ed25519KeyPair,
        digest: String,
    }

    impl SigningIdentity {
        fn mint() -> Self {
            use ring::rand::SystemRandom;
            let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .expect("generated desktop identity");
            Self {
                key_pair: ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
                    .expect("parsed desktop identity"),
                digest: digest("desktop-identity"),
            }
        }

        fn public_key_hex(&self) -> String {
            hex_lower(&self.key_pair.public_key().as_ref())
        }

        fn sign_envelope(
            &self,
            proposal: AppLearningDecisionProposalV1,
            decision: AppLearningOwnerDecisionV1,
        ) -> AppLearningOwnerDecisionEnvelopeV1 {
            let review = AppLearningOwnerReviewV1::mint(
                0,
                None,
                "desktop-key:1".to_owned(),
                self.digest.clone(),
                proposal,
            )
            .expect("minted review");
            let retention = match decision {
                AppLearningOwnerDecisionV1::Accept => Some(2_000),
                AppLearningOwnerDecisionV1::Reject | AppLearningOwnerDecisionV1::Revoke => None,
            };
            let unsigned = AppLearningOwnerDecisionEnvelopeV1::prepare(review, decision, retention)
                .expect("prepared envelope");
            let signature = self.key_pair.sign(&unsigned.signing_bytes().unwrap());
            unsigned
                .with_signature_hex(hex_lower(signature.as_ref()))
                .expect("signed envelope")
        }
    }

    fn hex_lower(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn store_with_proposed_candidate() -> (tempfile::TempDir, LearningStore, LearningScope) {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LearningStore::new(ArtifactV2Workspace::new(temp.path()));
        let scope = LearningScope::new("anonymous", "default");
        store
            .ensure_candidate_with_id(
                scope.clone(),
                CreateLearningCandidateRequest {
                    principal: None,
                    workspace: None,
                    candidate_type: LearningCandidateType::MemoryFact,
                    state: LearningCandidateState::Proposed,
                    title: "Test candidate".to_owned(),
                    summary: "A candidate proposed for review.".to_owned(),
                    rationale: String::new(),
                    proposed_change: serde_json::json!({"value": "test"}),
                    proposed_target: None,
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
                    review_policy: serde_json::Value::Null,
                    promotion_target: None,
                    promotion_policy: serde_json::Value::Null,
                },
                "lc_test_candidate",
            )
            .expect("created candidate");
        (temp, store, scope)
    }

    #[test]
    fn admission_requires_one_exact_replaceable_source_head() {
        let proposal = sealed_proposal(AppLearningDecisionKindV1::Approve);
        validate_app_learning_decision(&proposal).expect("admissible proposal");

        let mut two_sources = proposal.clone();
        let mut second = two_sources.header.sources[0].clone();
        second.record_id = "record:2".to_owned();
        second.canonical_source_ref = "source:record:2".to_owned();
        two_sources.header.sources.push(second);
        two_sources.proposal_digest.clear();
        let two_sources = two_sources
            .seal()
            .expect("contract-valid two-source proposal");
        assert_eq!(
            validate_app_learning_decision(&two_sources),
            Err(AppLearningContributionError::NotAdmissible(
                "V1 app-learning ingress requires one exact replaceable source head",
            ))
        );

        let mut revision_updates = proposal;
        revision_updates.header.update_policy = AppContributionUpdatePolicy::NewProposalRevision;
        revision_updates.proposal_digest.clear();
        let revision_updates = revision_updates
            .seal()
            .expect("contract-valid revision-policy proposal");
        assert_eq!(
            validate_app_learning_decision(&revision_updates),
            Err(AppLearningContributionError::NotAdmissible(
                "V1 app-learning ingress requires one exact replaceable source head",
            ))
        );
    }

    #[test]
    fn idempotency_key_and_marker_bind_decision_identity() {
        let proposal = sealed_proposal(AppLearningDecisionKindV1::Approve);
        let repeated = sealed_proposal(AppLearningDecisionKindV1::Approve);
        assert_eq!(
            app_learning_decision_idempotency_key(&proposal).unwrap(),
            app_learning_decision_idempotency_key(&repeated).unwrap()
        );
        assert_eq!(
            app_learning_decision_marker(&proposal).unwrap(),
            app_learning_decision_marker(&repeated).unwrap()
        );

        let mut different_dedupe = proposal.clone();
        different_dedupe.header.dedupe_key = "dedupe:2".to_owned();
        different_dedupe.proposal_digest.clear();
        let different_dedupe = different_dedupe.seal().expect("resealed proposal");
        assert_ne!(
            app_learning_decision_idempotency_key(&proposal).unwrap(),
            app_learning_decision_idempotency_key(&different_dedupe).unwrap()
        );
        assert_ne!(
            app_learning_decision_marker(&proposal).unwrap(),
            app_learning_decision_marker(&different_dedupe).unwrap()
        );
    }

    #[test]
    fn state_mapping_never_reaches_promotion() {
        let approve = sealed_proposal(AppLearningDecisionKindV1::Approve);
        let reject = sealed_proposal(AppLearningDecisionKindV1::Reject);
        let snooze = snooze_proposal();
        assert_eq!(
            app_learning_target_state(&approve),
            Some(LearningCandidateState::Approved)
        );
        assert_eq!(
            app_learning_target_state(&reject),
            Some(LearningCandidateState::Rejected)
        );
        assert_eq!(app_learning_target_state(&snooze), None);
        for proposal in [&approve, &reject, &snooze] {
            assert_ne!(
                app_learning_target_state(proposal),
                Some(LearningCandidateState::Promoted)
            );
        }
    }

    #[test]
    fn signed_accept_applies_the_real_substrate_transition() {
        let (_temp, store, scope) = store_with_proposed_candidate();
        let identity = SigningIdentity::mint();
        let proposal = sealed_proposal(AppLearningDecisionKindV1::Approve);
        let envelope = identity.sign_envelope(proposal.clone(), AppLearningOwnerDecisionV1::Accept);

        let applied = apply_signed_app_learning_owner_decision(
            &envelope,
            &identity.public_key_hex(),
            &store,
            &scope,
            1_500,
        )
        .expect("applied signed decision");
        let AppLearningDecisionApplication::Applied { candidate } = applied else {
            panic!("first application must apply the transition");
        };
        assert_eq!(candidate.state, LearningCandidateState::Approved);

        let stored = store
            .read_candidate(&scope, "lc_test_candidate")
            .expect("stored candidate");
        assert_eq!(stored.state, LearningCandidateState::Approved);
        let decisions = store
            .read_decisions(&scope, "lc_test_candidate")
            .expect("decision log");
        let entry = decisions
            .iter()
            .find(|entry| {
                entry.evidence_refs.iter().any(|evidence| {
                    evidence.kind == "app_learning_decision"
                        && evidence.id.as_deref()
                            == Some(app_learning_decision_marker(&proposal).unwrap().as_str())
                })
            })
            .expect("the applied transition carries the port's marker");
        assert_eq!(
            entry.actor,
            "app-learning-decision:installation:1".to_owned()
        );
        assert_eq!(entry.decision, "app_approve".to_owned());
        assert_eq!(
            entry.from_state.as_ref(),
            Some(&LearningCandidateState::Proposed)
        );

        // Replaying the exact same signed decision is idempotent.
        let replay = apply_signed_app_learning_owner_decision(
            &envelope,
            &identity.public_key_hex(),
            &store,
            &scope,
            1_600,
        )
        .expect("replayed signed decision");
        assert!(matches!(
            replay,
            AppLearningDecisionApplication::AlreadyApplied { .. }
        ));
        let decisions_after_replay = store
            .read_decisions(&scope, "lc_test_candidate")
            .expect("decision log after replay");
        let marker_entries = decisions_after_replay
            .iter()
            .filter(|entry| {
                entry.evidence_refs.iter().any(|evidence| {
                    evidence.kind == "app_learning_decision"
                        && evidence.id.as_deref()
                            == Some(app_learning_decision_marker(&proposal).unwrap().as_str())
                })
            })
            .count();
        assert_eq!(marker_entries, 1, "replay must not append a second marker");
    }

    #[test]
    fn reject_and_snooze_decisions_reach_the_store_without_promotion() {
        let (_temp, store, scope) = store_with_proposed_candidate();
        let identity = SigningIdentity::mint();

        let snooze = identity.sign_envelope(snooze_proposal(), AppLearningOwnerDecisionV1::Accept);
        let applied = apply_signed_app_learning_owner_decision(
            &snooze,
            &identity.public_key_hex(),
            &store,
            &scope,
            1_500,
        )
        .expect("applied snooze");
        let AppLearningDecisionApplication::Applied { candidate } = applied else {
            panic!("snooze must still be applied as a logged decision");
        };
        assert_eq!(
            candidate.state,
            LearningCandidateState::Proposed,
            "snooze records its deferral without changing state"
        );

        let reject = identity.sign_envelope(
            sealed_proposal(AppLearningDecisionKindV1::Reject),
            AppLearningOwnerDecisionV1::Accept,
        );
        let applied = apply_signed_app_learning_owner_decision(
            &reject,
            &identity.public_key_hex(),
            &store,
            &scope,
            1_600,
        )
        .expect("applied reject");
        let AppLearningDecisionApplication::Applied { candidate } = applied else {
            panic!("reject must apply the transition");
        };
        assert_eq!(candidate.state, LearningCandidateState::Rejected);
    }

    #[test]
    fn owner_reject_declines_without_touching_the_store() {
        let (_temp, store, scope) = store_with_proposed_candidate();
        let identity = SigningIdentity::mint();
        let envelope = identity.sign_envelope(
            sealed_proposal(AppLearningDecisionKindV1::Approve),
            AppLearningOwnerDecisionV1::Reject,
        );
        let outcome = apply_signed_app_learning_owner_decision(
            &envelope,
            &identity.public_key_hex(),
            &store,
            &scope,
            1_500,
        )
        .expect("declined");
        assert_eq!(outcome, AppLearningDecisionApplication::OwnerDeclined);
        let candidate = store
            .read_candidate(&scope, "lc_test_candidate")
            .expect("candidate");
        assert_eq!(candidate.state, LearningCandidateState::Proposed);
    }

    #[test]
    fn apply_fails_closed_on_bad_signature_lifetime_and_terminal_state() {
        let (_temp, store, scope) = store_with_proposed_candidate();
        let identity = SigningIdentity::mint();
        let other_identity = SigningIdentity::mint();
        let envelope = identity.sign_envelope(
            sealed_proposal(AppLearningDecisionKindV1::Approve),
            AppLearningOwnerDecisionV1::Accept,
        );

        assert!(matches!(
            apply_signed_app_learning_owner_decision(
                &envelope,
                &other_identity.public_key_hex(),
                &store,
                &scope,
                1_500,
            ),
            Err(AppLearningContributionError::InvalidSignature(_))
        ));

        for stale_ms in [999_i64, 2_000] {
            assert!(matches!(
                apply_signed_app_learning_owner_decision(
                    &envelope,
                    &identity.public_key_hex(),
                    &store,
                    &scope,
                    stale_ms,
                ),
                Err(AppLearningContributionError::Apply(_))
            ));
        }

        store
            .transition_candidate(
                &scope,
                "lc_test_candidate",
                LearningCandidateState::Rejected,
                "test",
                "close",
                "closing the candidate before the port applies",
                Vec::new(),
            )
            .expect("terminal transition");
        assert!(matches!(
            apply_signed_app_learning_owner_decision(
                &envelope,
                &identity.public_key_hex(),
                &store,
                &scope,
                1_500,
            ),
            Err(AppLearningContributionError::Apply(_))
        ));

        // Snooze on the same terminal candidate fails closed too: its
        // same-state deferral is still a decision on a closed review, not a
        // way to keep annotating it after the substrate closed it.
        let snooze_envelope =
            identity.sign_envelope(snooze_proposal(), AppLearningOwnerDecisionV1::Accept);
        assert!(matches!(
            apply_signed_app_learning_owner_decision(
                &snooze_envelope,
                &identity.public_key_hex(),
                &store,
                &scope,
                1_500,
            ),
            Err(AppLearningContributionError::Apply(_))
        ));
    }

    #[test]
    fn apply_fails_closed_for_a_missing_candidate() {
        let (_temp, store, scope) = store_with_proposed_candidate();
        let identity = SigningIdentity::mint();
        let mut proposal = sealed_proposal(AppLearningDecisionKindV1::Approve);
        proposal.candidate_id = "lc_missing_candidate".to_owned();
        proposal.proposal_digest.clear();
        let proposal = proposal.seal().expect("resealed for a missing candidate");
        let envelope = identity.sign_envelope(proposal, AppLearningOwnerDecisionV1::Accept);
        assert!(matches!(
            apply_signed_app_learning_owner_decision(
                &envelope,
                &identity.public_key_hex(),
                &store,
                &scope,
                1_500,
            ),
            Err(AppLearningContributionError::Apply(_))
        ));
    }
}

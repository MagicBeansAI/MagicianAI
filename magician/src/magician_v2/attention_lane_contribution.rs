//! Destination owner for app-proposed attention-lane candidates.
//!
//! Apps never own attention state; they propose, the owner decides. This
//! module is the plan-1.1 destination-side seam beside
//! `agents/app_memory_ingress.rs`: it validates a sealed
//! [`AppAttentionCandidateProposalV1`] against the port's V1 admission shape
//! (one exact replaceable source head, closed lane vocabulary, bounded card
//! text, header-carried idempotency) and renders an owner-accepted candidate
//! as one [`AttentionLaneItem`] — the record type the lane facade already
//! pages for every other attention surface. Durable staging, owner-review
//! listing, and the dispatch outbox deliberately arrive with the 3.1
//! consumer; this module lands only validation, idempotency identity, and
//! the record adapter, so no existing lane changes.

use magician_app_contract::contribution::{
    AppAttentionCandidateProposalV1, AppAttentionLaneV1, AppContributionRetractionPolicy,
    AppContributionUpdatePolicy, APP_ATTENTION_CANDIDATE_CONTRACT_ID,
};
use serde::Serialize;
use serde_json::json;

use crate::magician_v2::attention_funnel::{
    AttentionLane, AttentionSourceFamily, AttentionSourceKind,
};
use crate::magician_v2::attention_lane_facade::{self, AttentionLaneItem, AttentionLanePage};

/// Bounded provenance metadata attached to a rendered record. The card body
/// itself stays inside the sealed proposal digest; this ceiling only guards
/// the destination-minted identity metadata below.
const MAX_APP_ATTENTION_RECORD_METADATA_BYTES: usize = 2 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppAttentionContributionError {
    /// The sealed contract rejected the bytes (caps, digests, vocabulary, or
    /// an unknown field).
    InvalidContract(String),
    /// Structurally valid, but not admissible through this port's V1 shape.
    NotAdmissible(&'static str),
    /// Rendering a valid accepted candidate failed (time bounds or the
    /// metadata ceiling).
    Render(String),
}

impl std::fmt::Display for AppAttentionContributionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidContract(error) => {
                write!(formatter, "invalid app-attention contract: {error}")
            },
            Self::NotAdmissible(rule) => {
                write!(formatter, "inadmissible app-attention candidate: {rule}")
            },
            Self::Render(error) => write!(formatter, "app-attention render failed: {error}"),
        }
    }
}

impl std::error::Error for AppAttentionContributionError {}

/// Fail-closed V1 admission shape for a staged attention candidate.
///
/// Mirrors the memory port's `StageProposal` guard: contract validation runs
/// first, then the port-specific shape — exactly one source head, declared
/// replaceable and tombstoned on any drift. Everything else (lane placement,
/// ordering, final admission) stays with the owner.
pub fn validate_app_attention_candidate(
    proposal: &AppAttentionCandidateProposalV1,
) -> Result<(), AppAttentionContributionError> {
    proposal
        .validate()
        .map_err(|error| AppAttentionContributionError::InvalidContract(error.to_string()))?;
    if proposal.header.destination_contract_id != APP_ATTENTION_CANDIDATE_CONTRACT_ID {
        return Err(AppAttentionContributionError::NotAdmissible(
            "V1 app-attention ingress requires the attention destination contract",
        ));
    }
    if proposal.header.update_policy != AppContributionUpdatePolicy::ReplaceExactSourceHead
        || proposal.header.retraction_policy
            != AppContributionRetractionPolicy::TombstoneOnAnySourceDrift
        || proposal.header.sources.len() != 1
    {
        return Err(AppAttentionContributionError::NotAdmissible(
            "V1 app-attention ingress requires one exact replaceable source head",
        ));
    }
    Ok(())
}

/// Domain-separated idempotency key for one exact source head.
///
/// Mirrors the memory port's source high-water key so the 3.1 dispatch
/// consumer can deduplicate response-loss retries without a new identity
/// scheme: installation + scope + exact source + header `dedupe_key`.
pub fn app_attention_candidate_idempotency_key(
    proposal: &AppAttentionCandidateProposalV1,
) -> Result<String, AppAttentionContributionError> {
    let source =
        proposal
            .header
            .sources
            .first()
            .ok_or(AppAttentionContributionError::NotAdmissible(
                "V1 app-attention proposal lost its source",
            ))?;
    digest_serialized(
        "magician.app-attention-source-high-water.v1",
        &(
            &proposal.header.installation_id,
            &proposal.header.scope_binding_ref,
            &source.canonical_source_ref,
            &proposal.header.dedupe_key,
        ),
    )
}

/// Map the contract's closed lane vocabulary onto the core lane enum.
///
/// Both vocabularies carry the same seven wire strings by contract; this
/// total variant-to-variant mapping keeps any drift a compile error instead
/// of a runtime translation table.
pub fn app_attention_lane(lane: AppAttentionLaneV1) -> AttentionLane {
    match lane {
        AppAttentionLaneV1::NeedsYou => AttentionLane::NeedsYou,
        AppAttentionLaneV1::FollowUp => AttentionLane::FollowUp,
        AppAttentionLaneV1::WorthALook => AttentionLane::WorthALook,
        AppAttentionLaneV1::ActiveWork => AttentionLane::ActiveWork,
        AppAttentionLaneV1::Delivered => AttentionLane::Delivered,
        AppAttentionLaneV1::Changed => AttentionLane::Changed,
        AppAttentionLaneV1::Failed => AttentionLane::Failed,
    }
}

/// Stable record id for one accepted candidate.
///
/// Derived from the sealed proposal identity so re-rendering the same
/// acceptance is idempotent while different revisions of the same proposal
/// remain distinct rows in a lane.
pub fn app_attention_record_id(
    proposal: &AppAttentionCandidateProposalV1,
) -> Result<String, AppAttentionContributionError> {
    let digest = digest_serialized(
        "magician.app-attention-record-id.v1",
        &(&proposal.header.installation_id, &proposal.proposal_digest),
    )?;
    Ok(format!(
        "app-attention-record:{}",
        digest.trim_start_matches("blake3:")
    ))
}

/// Render an owner-accepted candidate as one facade lane record.
///
/// The destination, not the app, owns every field that orders or places the
/// card: `updated_at` is the owner's acceptance time, the source kind and
/// family stay on the neutral `other` substrate (apps are not a native
/// attention source kind), and the claimed urgency travels as bounded
/// metadata the owner interprets. The proposal must still be contract-valid
/// and admissible, and acceptance must fall inside the proposal's lifetime —
/// an acceptance at or after expiry is stale and fails closed, exactly as a
/// memory `StageProposal` receipt does.
pub fn render_accepted_app_attention_candidate(
    proposal: &AppAttentionCandidateProposalV1,
    accepted_at_ms: i64,
) -> Result<AttentionLaneItem, AppAttentionContributionError> {
    validate_app_attention_candidate(proposal)?;
    if accepted_at_ms < proposal.header.issued_at_ms
        || accepted_at_ms >= proposal.header.expires_at_ms
    {
        return Err(AppAttentionContributionError::Render(
            "app-attention acceptance is outside the proposal lifetime".to_owned(),
        ));
    }
    let metadata = json!({
        "app_installation_id": proposal.header.installation_id,
        "app_proposal_id": proposal.header.proposal_id,
        "app_proposal_digest": proposal.proposal_digest,
        "app_contribution_port_id": proposal.header.contribution_port_id,
        "app_claimed_priority": proposal.priority.as_str(),
    });
    if serde_json::to_vec(&metadata)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
        > MAX_APP_ATTENTION_RECORD_METADATA_BYTES
    {
        return Err(AppAttentionContributionError::Render(
            "app-attention record metadata exceeds its ceiling".to_owned(),
        ));
    }
    Ok(AttentionLaneItem {
        id: app_attention_record_id(proposal)?,
        lane: app_attention_lane(proposal.lane),
        title: proposal.title.clone(),
        summary: proposal.summary.clone(),
        source_kind: AttentionSourceKind::Other,
        source_family: AttentionSourceFamily::Other,
        source_ref: proposal.primary_source_ref.clone(),
        created_at: proposal.header.issued_at_ms,
        updated_at: accepted_at_ms,
        metadata,
    })
}

/// Page rendered records through the shared lane facade.
///
/// Callers supply records in their destination-owned order (newest first for
/// the keyset cursor); the facade resolves cursors exactly as it does for
/// Today, Follow-ups, and Worth-a-look.
pub fn list_app_attention_candidates(
    lane: AttentionLane,
    ordered_records: &[AttentionLaneItem],
    cursor: Option<&str>,
    limit: usize,
) -> Result<AttentionLanePage<AttentionLaneItem>, AppAttentionContributionError> {
    attention_lane_facade::list_attention_lane(lane, ordered_records, cursor, limit).map_err(
        |error| {
            AppAttentionContributionError::Render(format!(
                "app-attention lane page failed: {error}"
            ))
        },
    )
}

fn digest_serialized<T: Serialize>(
    domain: &str,
    value: &T,
) -> Result<String, AppAttentionContributionError> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        AppAttentionContributionError::Render(format!(
            "app-attention digest input is not serializable: {error}"
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
    use super::*;
    use magician_app_contract::contribution::{
        content_digest, AppAttentionPriorityV1, AppContributionClassification,
        AppContributionEvidenceClass, AppContributionHandlingLabelsV1,
        AppContributionModelProcessing, AppContributionSettlementRefV1,
        AppContributionSourceHeaderV1, AppContributionSourceRefV1,
    };

    // The claimed urgency vocabulary is destination-interpreted content; the
    // test pins it to the closed contract set so a future variant cannot
    // silently reorder a lane through this module.
    const CLAIMED_PRIORITY_VOCABULARY: [&str; 4] = [
        AppAttentionPriorityV1::Background.as_str(),
        AppAttentionPriorityV1::Normal.as_str(),
        AppAttentionPriorityV1::Elevated.as_str(),
        AppAttentionPriorityV1::Urgent.as_str(),
    ];

    fn digest(label: &str) -> String {
        content_digest(label.as_bytes())
    }

    fn source_header() -> AppContributionSourceHeaderV1 {
        AppContributionSourceHeaderV1 {
            contract_version: 1,
            destination_contract_id: APP_ATTENTION_CANDIDATE_CONTRACT_ID.to_owned(),
            destination_contract_version: 1,
            destination_schema_digest: digest("destination-schema"),
            proposal_id: "attention-proposal:1".to_owned(),
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
            workflow_id: "workflow:surface_card".to_owned(),
            workflow_digest: digest("workflow"),
            action_id: "action:surface_card".to_owned(),
            action_digest: digest("action"),
            contribution_port_id: "attention_candidate".to_owned(),
            contribution_port_digest: digest("port"),
            settlement: AppContributionSettlementRefV1::Mutation {
                mutation_receipt_id: "mutation:1".to_owned(),
                first_change_sequence: 7,
                last_change_sequence: 7,
            },
            sources: vec![AppContributionSourceRefV1 {
                installation_id: "installation:1".to_owned(),
                entity_name: "card".to_owned(),
                record_id: "record:1".to_owned(),
                record_revision: 1,
                selected_fields: vec!["title".to_owned()],
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
            purpose: "surface-owner-reviewed-lane-card".to_owned(),
            audiences: vec!["personal-agent".to_owned()],
            evidence_class: AppContributionEvidenceClass::Hypothesis,
            issued_at_ms: 1_000,
            expires_at_ms: 2_000,
            dedupe_key: "dedupe:1".to_owned(),
            update_policy: AppContributionUpdatePolicy::ReplaceExactSourceHead,
            retraction_policy: AppContributionRetractionPolicy::TombstoneOnAnySourceDrift,
        }
    }

    fn sealed_proposal() -> AppAttentionCandidateProposalV1 {
        let title = "A bounded lane card".to_owned();
        AppAttentionCandidateProposalV1 {
            header: source_header(),
            lane: AppAttentionLaneV1::WorthALook,
            priority: AppAttentionPriorityV1::Normal,
            title_digest: content_digest(title.as_bytes()),
            title,
            summary: Some("One reviewed summary sentence.".to_owned()),
            primary_source_ref: "source:record:1".to_owned(),
            proposal_digest: String::new(),
        }
        .seal()
        .expect("sealed attention proposal")
    }

    #[test]
    fn admission_requires_one_exact_replaceable_source_head() {
        let proposal = sealed_proposal();
        validate_app_attention_candidate(&proposal).expect("admissible proposal");

        let mut two_sources = proposal.clone();
        let mut second = two_sources.header.sources[0].clone();
        second.record_id = "record:2".to_owned();
        second.canonical_source_ref = "source:record:2".to_owned();
        two_sources.header.sources.push(second);
        two_sources.primary_source_ref = "source:record:2".to_owned();
        two_sources.proposal_digest.clear();
        let two_sources = two_sources
            .seal()
            .expect("contract-valid two-source proposal");
        assert_eq!(
            validate_app_attention_candidate(&two_sources),
            Err(AppAttentionContributionError::NotAdmissible(
                "V1 app-attention ingress requires one exact replaceable source head",
            ))
        );

        let mut revision_updates = proposal;
        revision_updates.header.update_policy = AppContributionUpdatePolicy::NewProposalRevision;
        revision_updates.proposal_digest.clear();
        let revision_updates = revision_updates
            .seal()
            .expect("contract-valid revision-policy proposal");
        assert_eq!(
            validate_app_attention_candidate(&revision_updates),
            Err(AppAttentionContributionError::NotAdmissible(
                "V1 app-attention ingress requires one exact replaceable source head",
            ))
        );
    }

    #[test]
    fn admission_rejects_substituted_contract_bytes() {
        let mut substituted = sealed_proposal();
        substituted.title.push('!');
        assert!(matches!(
            validate_app_attention_candidate(&substituted),
            Err(AppAttentionContributionError::InvalidContract(_))
        ));
    }

    #[test]
    fn idempotency_key_binds_source_and_dedupe_identity() {
        let proposal = sealed_proposal();
        let repeated = sealed_proposal();
        assert_eq!(
            app_attention_candidate_idempotency_key(&proposal).unwrap(),
            app_attention_candidate_idempotency_key(&repeated).unwrap()
        );

        let mut different_dedupe = proposal.clone();
        different_dedupe.header.dedupe_key = "dedupe:2".to_owned();
        different_dedupe.proposal_digest.clear();
        let different_dedupe = different_dedupe.seal().expect("resealed proposal");
        assert_ne!(
            app_attention_candidate_idempotency_key(&proposal).unwrap(),
            app_attention_candidate_idempotency_key(&different_dedupe).unwrap()
        );
    }

    #[test]
    fn lane_vocabulary_matches_the_core_enum() {
        let pairs = [
            (AppAttentionLaneV1::NeedsYou, AttentionLane::NeedsYou),
            (AppAttentionLaneV1::FollowUp, AttentionLane::FollowUp),
            (AppAttentionLaneV1::WorthALook, AttentionLane::WorthALook),
            (AppAttentionLaneV1::ActiveWork, AttentionLane::ActiveWork),
            (AppAttentionLaneV1::Delivered, AttentionLane::Delivered),
            (AppAttentionLaneV1::Changed, AttentionLane::Changed),
            (AppAttentionLaneV1::Failed, AttentionLane::Failed),
        ];
        for (contract_lane, core_lane) in pairs {
            assert_eq!(contract_lane.as_str(), core_lane.as_str());
            assert_eq!(app_attention_lane(contract_lane), core_lane);
        }
    }

    #[test]
    fn record_rendering_is_destination_owned() {
        let proposal = sealed_proposal();
        let record =
            render_accepted_app_attention_candidate(&proposal, 1_500).expect("rendered record");
        assert_eq!(record.lane, AttentionLane::WorthALook);
        assert_eq!(record.title, "A bounded lane card");
        assert_eq!(
            record.summary.as_deref(),
            Some("One reviewed summary sentence.")
        );
        assert_eq!(record.source_ref, "source:record:1");
        assert_eq!(record.source_kind, AttentionSourceKind::Other);
        assert_eq!(record.source_family, AttentionSourceFamily::Other);
        assert_eq!(record.created_at, 1_000);
        assert_eq!(record.updated_at, 1_500);
        assert_eq!(record.id, app_attention_record_id(&proposal).unwrap());
        assert_eq!(
            record.metadata["app_claimed_priority"],
            serde_json::json!("normal")
        );
        assert_eq!(
            record.metadata["app_proposal_digest"],
            serde_json::json!(proposal.proposal_digest)
        );
        assert!(
            serde_json::to_vec(&record.metadata).unwrap().len()
                <= MAX_APP_ATTENTION_RECORD_METADATA_BYTES
        );
        assert!(record.id.starts_with("app-attention-record:"));

        // Rendering is idempotent for the same acceptance.
        let repeated =
            render_accepted_app_attention_candidate(&proposal, 1_500).expect("rendered again");
        assert_eq!(record, repeated);

        // The claimed urgency vocabulary stays the closed contract set.
        assert_eq!(
            CLAIMED_PRIORITY_VOCABULARY,
            ["background", "normal", "elevated", "urgent"]
        );
    }

    #[test]
    fn rendering_fails_closed_outside_the_proposal_lifetime() {
        let proposal = sealed_proposal();
        assert!(matches!(
            render_accepted_app_attention_candidate(&proposal, 999),
            Err(AppAttentionContributionError::Render(_))
        ));
        assert!(matches!(
            render_accepted_app_attention_candidate(&proposal, 2_000),
            Err(AppAttentionContributionError::Render(_))
        ));
        let mut substituted = proposal;
        substituted.summary = Some("replaced after sealing".to_owned());
        assert!(matches!(
            render_accepted_app_attention_candidate(&substituted, 1_500),
            Err(AppAttentionContributionError::InvalidContract(_))
        ));
    }

    #[test]
    fn rendered_records_page_through_the_lane_facade() {
        let newest = sealed_proposal();
        let mut middle = sealed_proposal();
        middle.header.proposal_id = "attention-proposal:2".to_owned();
        middle.title = "Second card".to_owned();
        middle.title_digest = content_digest(middle.title.as_bytes());
        middle.proposal_digest.clear();
        let middle = middle.seal().expect("sealed middle proposal");
        let mut oldest = sealed_proposal();
        oldest.header.proposal_id = "attention-proposal:3".to_owned();
        oldest.title = "Third card".to_owned();
        oldest.title_digest = content_digest(oldest.title.as_bytes());
        oldest.proposal_digest.clear();
        let oldest = oldest.seal().expect("sealed oldest proposal");

        // Destination-owned order: newest acceptance first.
        let records = [
            render_accepted_app_attention_candidate(&newest, 1_700).unwrap(),
            render_accepted_app_attention_candidate(&middle, 1_500).unwrap(),
            render_accepted_app_attention_candidate(&oldest, 1_300).unwrap(),
        ];
        let first_page =
            list_app_attention_candidates(AttentionLane::WorthALook, &records, None, 2)
                .expect("first page");
        assert_eq!(first_page.total, 3);
        assert_eq!(first_page.items.len(), 2);
        assert!(first_page.has_more);
        assert_eq!(first_page.items[0].title, "A bounded lane card");

        let second_page = list_app_attention_candidates(
            AttentionLane::WorthALook,
            &records,
            first_page.next_cursor.as_deref(),
            2,
        )
        .expect("second page");
        assert_eq!(second_page.items.len(), 1);
        assert!(!second_page.has_more);
        assert_eq!(second_page.items[0].title, "Third card");
        assert!(second_page.next_cursor.is_none());
    }
}

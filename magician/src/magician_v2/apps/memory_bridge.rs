//! Phase 5B governed memory bridge.
//!
//! App records stay authoritative. This adopter puts the Phase-0 source-
//! eligibility envelope on the canonical memory/retrieval path, settles
//! accepted candidates when their sources change, and refuses raw-record
//! auto-promotion. An unavailable eligibility authority fails closed.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::authority::AuthenticatedAppScope;
use super::memory::{
    propose_app_memory_candidate, resolve_app_memory_eligibility, AppMemoryCandidate,
    AppMemoryCandidateLifecycleError, AppMemoryEligibilityError, AppMemoryEligibilityFence,
    AppMemorySemanticDestination, AppMemorySourceRef, AppMemoryTierScope, ResolvedAppMemorySource,
};
use super::models::{
    AppContractLimits, AppDigest, AppInstallationId, AppModelProcessing, AppName, AppRecordId,
    AppReference, AppRevision,
};
use super::policy::AppJoinedContent;
use super::records::AppRecordRevision;

/// Canonical metadata key carried on a memory-candidate document or tier item.
pub const APP_SOURCE_ELIGIBILITY_METADATA_KEY: &str = "app_source_eligibility";
pub const APP_SOURCE_MODEL_PROCESSING_METADATA_KEY: &str = "app_source_model_processing";
const ENVELOPE_SCHEMA_VERSION: u8 = 1;

/// Identity envelope stored on the canonical memory record. This is not a
/// grant: retrieval must still re-resolve current source evidence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemorySourceEligibilityEnvelope {
    pub schema_version: u8,
    pub candidate_id: AppReference,
    pub candidate_revision: AppRevision,
    pub candidate_fingerprint: AppDigest,
    pub sources: Vec<AppMemoryEnvelopeSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryEnvelopeSource {
    pub installation_id: AppInstallationId,
    pub entity_name: AppName,
    pub record_id: AppRecordId,
    pub record_revision: AppRevision,
}

#[derive(Debug)]
pub enum AppMemoryRetrievalDecision {
    Eligible(AppMemoryEligibilityFence),
    Settled(AppMemoryCandidate),
    FailClosed(AppMemoryEligibilityError),
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppMemoryBridgeError {
    #[error(transparent)]
    Eligibility(#[from] AppMemoryEligibilityError),
    #[error(transparent)]
    Lifecycle(#[from] AppMemoryCandidateLifecycleError),
    #[error("raw app records cannot auto-promote into memory")]
    RawRecordAutoPromotionDenied,
    #[error("app-memory eligibility authority is unavailable")]
    EligibilityAuthorityUnavailable,
}

/// Creating or editing an app record is never itself a memory write.
pub fn refuse_raw_record_auto_promotion(
    _record: &AppRecordRevision,
) -> Result<(), AppMemoryBridgeError> {
    Err(AppMemoryBridgeError::RawRecordAutoPromotionDenied)
}

pub fn canonical_source_eligibility_envelope(
    candidate: &AppMemoryCandidate,
) -> AppMemorySourceEligibilityEnvelope {
    AppMemorySourceEligibilityEnvelope {
        schema_version: ENVELOPE_SCHEMA_VERSION,
        candidate_id: candidate.candidate_id.clone(),
        candidate_revision: candidate.candidate_revision,
        candidate_fingerprint: candidate.candidate_fingerprint.clone(),
        sources: candidate.source_refs.iter().map(envelope_source).collect(),
    }
}

pub fn attach_source_eligibility_envelope(metadata: &mut Value, candidate: &AppMemoryCandidate) {
    let envelope = canonical_source_eligibility_envelope(candidate);
    if let Value::Object(map) = metadata {
        map.insert(
            APP_SOURCE_ELIGIBILITY_METADATA_KEY.to_owned(),
            serde_json::to_value(envelope).expect("eligibility envelope is serializable"),
        );
        map.insert(
            APP_SOURCE_MODEL_PROCESSING_METADATA_KEY.to_owned(),
            serde_json::to_value(candidate.handling_labels.model_processing)
                .expect("model-processing label is serializable"),
        );
        map.remove("temperature");
        map.remove("force_prompt_inclusion");
    }
}

/// Recover the source-owned processing label paired with an eligibility
/// envelope. A source-linked document with a missing or malformed label is
/// legacy/ambiguous evidence and therefore cannot enter a model prompt.
pub fn parse_source_model_processing(
    metadata: &Value,
) -> Option<Result<AppModelProcessing, AppMemoryBridgeError>> {
    let value = metadata.get(APP_SOURCE_MODEL_PROCESSING_METADATA_KEY)?;
    Some(
        serde_json::from_value(value.clone())
            .map_err(|error| AppMemoryEligibilityError::InvalidCandidate(error.to_string()).into()),
    )
}

pub fn parse_source_eligibility_envelope(
    metadata: &Value,
) -> Option<Result<AppMemorySourceEligibilityEnvelope, AppMemoryBridgeError>> {
    let value = metadata.get(APP_SOURCE_ELIGIBILITY_METADATA_KEY)?;
    Some(
        serde_json::from_value(value.clone())
            .map_err(|error| AppMemoryEligibilityError::InvalidCandidate(error.to_string()).into()),
    )
}

/// Prompt inclusion for a canonical memory document.
///
/// Documents without an envelope stay ordinary memories. Documents that carry
/// one require live source evidence: a missing authority fails closed rather
/// than trusting a stored fence after disable, retain or purge.
pub fn canonical_app_memory_may_enter_prompt(
    metadata: &Value,
    candidate: Option<&AppMemoryCandidate>,
    live_sources: Option<&[&ResolvedAppMemorySource]>,
    resolved_at: DateTime<Utc>,
) -> bool {
    match parse_source_eligibility_envelope(metadata) {
        None => true,
        Some(Err(_)) => false,
        Some(Ok(_)) => {
            let Some(candidate) = candidate else {
                return false;
            };
            let Some(live_sources) = live_sources else {
                return false;
            };
            matches!(
                evaluate_app_memory_retrieval(candidate, live_sources, resolved_at),
                AppMemoryRetrievalDecision::Eligible(_)
            )
        },
    }
}

pub fn propose_source_linked_candidate(
    authenticated_scope: &AuthenticatedAppScope,
    candidate_id: AppReference,
    intended_tier_scope: AppMemoryTierScope,
    semantic_destination: AppMemorySemanticDestination,
    joined_content: &AppJoinedContent,
    resolved_sources: &[&ResolvedAppMemorySource],
    proposed_at: DateTime<Utc>,
) -> Result<AppMemoryCandidate, AppMemoryBridgeError> {
    Ok(propose_app_memory_candidate(
        authenticated_scope,
        candidate_id,
        intended_tier_scope,
        semantic_destination,
        joined_content,
        resolved_sources,
        proposed_at,
        &AppContractLimits::default(),
    )?)
}

pub fn evaluate_app_memory_retrieval(
    candidate: &AppMemoryCandidate,
    live_sources: &[&ResolvedAppMemorySource],
    resolved_at: DateTime<Utc>,
) -> AppMemoryRetrievalDecision {
    match resolve_app_memory_eligibility(
        candidate,
        live_sources,
        resolved_at,
        &AppContractLimits::default(),
    ) {
        Ok(fence) => AppMemoryRetrievalDecision::Eligible(fence),
        Err(error) => AppMemoryRetrievalDecision::FailClosed(error),
    }
}

/// Apply the required candidate command when current sources make an accepted
/// memory ineligible. Unavailable evidence fails closed without mutating state.
pub fn settle_app_memory_candidate(
    candidate: &AppMemoryCandidate,
    live_sources: &[&ResolvedAppMemorySource],
    settled_at: DateTime<Utc>,
) -> Result<AppMemoryRetrievalDecision, AppMemoryBridgeError> {
    match evaluate_app_memory_retrieval(candidate, live_sources, settled_at) {
        AppMemoryRetrievalDecision::Eligible(fence) => {
            Ok(AppMemoryRetrievalDecision::Eligible(fence))
        },
        AppMemoryRetrievalDecision::FailClosed(error) => {
            if let Some(command) = error.required_candidate_command() {
                Ok(AppMemoryRetrievalDecision::Settled(
                    candidate.apply(command, settled_at)?,
                ))
            } else {
                Ok(AppMemoryRetrievalDecision::FailClosed(error))
            }
        },
        AppMemoryRetrievalDecision::Settled(next) => Ok(AppMemoryRetrievalDecision::Settled(next)),
    }
}

pub fn independently_corroborated_after_source_removal(
    candidate: &AppMemoryCandidate,
    remaining_sources: &[&ResolvedAppMemorySource],
) -> bool {
    remaining_sources.len() >= 1
        && remaining_sources.len() < candidate.source_refs.len()
        && remaining_sources.iter().all(|source| {
            candidate.source_refs.iter().any(|known| {
                memory_source_identity(known) == memory_source_identity(source.source())
            })
        })
}

fn envelope_source(source: &AppMemorySourceRef) -> AppMemoryEnvelopeSource {
    AppMemoryEnvelopeSource {
        installation_id: source.installation_id.clone(),
        entity_name: source.entity_name.clone(),
        record_id: source.record_id.clone(),
        record_revision: source.record_revision,
    }
}

fn memory_source_identity(
    source: &AppMemorySourceRef,
) -> (&AppInstallationId, &AppName, &AppRecordId) {
    (
        &source.installation_id,
        &source.entity_name,
        &source.record_id,
    )
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;
    use std::collections::BTreeSet;

    use crate::magician_v2::apps::authority::{AppScopeAuthentication, ResolvedAppAuthority};
    use crate::magician_v2::apps::lifecycle::{AppInstallationLifecycle, AppInstallationStatus};
    use crate::magician_v2::apps::memory::{
        AppMemoryCandidateCommand, AppMemoryCandidateStatus, AppMemoryResolvedSourceState,
    };
    use crate::magician_v2::apps::models::{
        AppDataClassification, AppDataEnvelope, AppDataSource, AppFieldPath, AppHandlingLabels,
        AppModelProcessing, AppProtocolVersion, AppScopeBindingRef, AppSourceRef, AppSourceRefKind,
    };
    use crate::magician_v2::apps::policy::{
        join_app_content, AppHandlingConstraint, ResolvedAppHandlingLabels, RevalidatedAppEnvelope,
    };
    use crate::magician_v2::apps::records::{
        AppBackgroundExecution, AppDataHandlingPolicy, AppExternalEgress, AppInstallation,
        AppMemoryPromotion, AppNetworkPolicy, AppPersonalAgentAccess, AppRecordActorKind,
        AppRecordProvenance, AppResourceCeiling, AppScope,
    };

    fn time(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 18, 0, 0, second)
            .single()
            .unwrap()
    }

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).unwrap()
    }

    fn digest(value: &str) -> AppDigest {
        AppDigest::blake3(value.as_bytes())
    }

    fn policy(memory_promotion: AppMemoryPromotion) -> AppDataHandlingPolicy {
        AppDataHandlingPolicy {
            classification_floor: AppDataClassification::Personal,
            model_processing: AppModelProcessing::LocalOnly,
            personal_agent_access: AppPersonalAgentAccess::ApprovedProjection,
            memory_promotion,
            external_egress: AppExternalEgress::Denied,
            approved_destinations: Vec::new(),
        }
    }

    fn resources() -> AppResourceCeiling {
        AppResourceCeiling {
            max_input_tokens: 1,
            max_output_tokens: 1,
            max_cost_microusd: 1,
            max_paid_tool_invocations: 1,
            max_active_seconds: 1,
            max_lifetime_seconds: 1,
            max_browser_network_actions: 1,
            max_concurrent_foreground_runs: 1,
            max_concurrent_background_runs: 0,
            max_records: 10,
            max_payload_bytes: 1_024,
            max_attachment_bytes: 1_024,
            max_monthly_tokens: 2,
            max_monthly_cost_microusd: 1,
        }
    }

    fn scope() -> AppScope {
        AppScope {
            principal: reference("anonymous"),
            workspace: reference("default"),
        }
    }

    fn authenticated_scope() -> AuthenticatedAppScope {
        AuthenticatedAppScope::from_verified_session(
            scope(),
            AppScopeBindingRef::parse("scope_1").unwrap(),
            reference("actor:owner"),
            reference("session:1"),
            AppRevision::new(1).unwrap(),
            time(0),
            time(59),
        )
        .unwrap()
    }

    fn authority(
        memory_promotion: AppMemoryPromotion,
        resolved_at: DateTime<Utc>,
    ) -> ResolvedAppAuthority {
        ResolvedAppAuthority {
            scope_binding_ref: AppScopeBindingRef::parse("scope_1").unwrap(),
            actor_ref: reference("actor:owner"),
            session_ref: reference("session:1"),
            authentication: AppScopeAuthentication::AuthenticatedSession,
            authentication_revision: AppRevision::new(1).unwrap(),
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            installation_generation: 2,
            package_revision_ref: reference("package:1"),
            grant_revision: AppRevision::new(1).unwrap(),
            grant_authority_digest: digest("grant-authority"),
            schema_revision: AppRevision::new(1).unwrap(),
            surface_revision: None,
            authority_digest: digest("authority"),
            effective_tools: BTreeSet::new(),
            effective_context_reads: BTreeSet::new(),
            effective_data_handling_policy: policy(memory_promotion),
            effective_background_execution: AppBackgroundExecution::Denied,
            effective_network_policy: AppNetworkPolicy::Denied,
            effective_resources: resources(),
            effective_any_public_host: false,
            resolved_at,
        }
    }

    fn record(revision: u64) -> AppRecordRevision {
        AppRecordRevision {
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            entity_name: AppName::parse("person").unwrap(),
            record_id: AppRecordId::parse("record_1").unwrap(),
            record_revision: AppRevision::new(revision).unwrap(),
            dataset_generation: revision,
            schema_revision: AppRevision::new(1).unwrap(),
            payload: json!({"name": "Asha", "role": "mentor"}),
            handling_override: None,
            created_at: time(1),
            updated_at: time(revision as u32 + 1),
            deleted_at: None,
            provenance: AppRecordProvenance {
                actor_kind: AppRecordActorKind::User,
                actor_id: reference("actor:owner"),
                execution_id: None,
                output_revision: None,
                mutation_receipt_id: None,
                source_artifact_refs: Vec::new(),
                citation_refs: Vec::new(),
            },
        }
    }

    fn labels() -> AppHandlingLabels {
        AppHandlingLabels {
            classification: AppDataClassification::Personal,
            model_processing: AppModelProcessing::LocalOnly,
            policy_digest: digest("field-policy"),
            provenance_digest: digest("field-provenance"),
        }
    }

    fn source_evidence(
        auth: &AuthenticatedAppScope,
        authority: &ResolvedAppAuthority,
        record: &AppRecordRevision,
        eligibility_revision: u64,
        resolved_at: DateTime<Utc>,
    ) -> ResolvedAppMemorySource {
        ResolvedAppMemorySource::from_enabled_record(
            auth,
            authority,
            record,
            reference("entity:person/record:1"),
            vec![AppFieldPath::parse("name").unwrap()],
            ResolvedAppHandlingLabels::from_trusted_policy(labels()),
            AppRevision::new(eligibility_revision).unwrap(),
            resolved_at,
        )
        .unwrap()
    }

    fn joined_content(auth: &AuthenticatedAppScope) -> AppJoinedContent {
        let source_ref = AppSourceRef {
            kind: AppSourceRefKind::EntityField,
            reference: reference("entity:person/record:1"),
            revision: Some(AppRevision::new(1).unwrap()),
            fields: vec![AppFieldPath::parse("name").unwrap()],
        };
        let value = json!({"name": "Asha"});
        let envelope = AppDataEnvelope {
            protocol_version: AppProtocolVersion::V1,
            source: AppDataSource::AppStore,
            scope_binding_ref: auth.scope_binding_ref().clone(),
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            package_revision_ref: reference("package:1"),
            schema_revision: AppRevision::new(1).unwrap(),
            grant_revision: AppRevision::new(1).unwrap(),
            value_schema_ref: reference("schema:person"),
            value: value.clone(),
            source_refs: vec![source_ref],
            handling_labels: labels(),
            content_digest: AppDigest::blake3_canonical_json(&value).unwrap(),
            produced_at: time(5),
            expires_at: None,
        };
        let revalidated = RevalidatedAppEnvelope::from_trusted_resolution(
            &envelope,
            ResolvedAppHandlingLabels::from_trusted_policy(labels()),
            &AppContractLimits::default(),
        )
        .unwrap();
        join_app_content(
            &[revalidated],
            Value::String("Asha is a mentor".to_owned()),
            &AppHandlingConstraint {
                classification_floor: AppDataClassification::Personal,
                model_processing: AppModelProcessing::LocalOnly,
                policy_digest: digest("memory-constraint"),
                purpose: reference("memory:promotion"),
                audience_ref: reference("user:owner"),
            },
            &AppContractLimits::default(),
        )
        .unwrap()
    }

    fn accepted_candidate() -> (
        AuthenticatedAppScope,
        ResolvedAppAuthority,
        AppRecordRevision,
        ResolvedAppMemorySource,
        AppMemoryCandidate,
    ) {
        let auth = authenticated_scope();
        let proposal_authority = authority(AppMemoryPromotion::CandidateAllowed, time(11));
        let record = record(1);
        let proposal_source = source_evidence(&auth, &proposal_authority, &record, 1, time(11));
        let candidate = propose_source_linked_candidate(
            &auth,
            reference("memory:candidate:1"),
            AppMemoryTierScope::User,
            AppMemorySemanticDestination::Entities,
            &joined_content(&auth),
            &[&proposal_source],
            time(11),
        )
        .unwrap()
        .apply(AppMemoryCandidateCommand::Accept, time(12))
        .unwrap();
        let current_authority = authority(AppMemoryPromotion::CandidateAllowed, time(13));
        let source = source_evidence(&auth, &current_authority, &record, 1, time(13));
        (auth, current_authority, record, source, candidate)
    }

    fn unavailable_installation(
        authority: &ResolvedAppAuthority,
        status: AppInstallationStatus,
    ) -> AppInstallation {
        AppInstallation {
            scope: scope(),
            installation_id: authority.installation_id.clone(),
            package_revision_ref: authority.package_revision_ref.clone(),
            lifecycle: AppInstallationLifecycle {
                status,
                generation: 3,
                update_return_status: None,
            },
            grant_revision: Some(authority.grant_revision),
            active_schema_revision: Some(authority.schema_revision),
            active_surface_revision: Some(AppRevision::new(1).unwrap()),
            created_at: time(1),
            updated_at: time(9),
            disabled_at: (status == AppInstallationStatus::Disabled).then_some(time(9)),
            quarantined_at: None,
            uninstalled_at: matches!(
                status,
                AppInstallationStatus::UninstalledRetained | AppInstallationStatus::Purged
            )
            .then_some(time(9)),
            purged_at: (status == AppInstallationStatus::Purged).then_some(time(9)),
        }
    }

    #[test]
    fn raw_records_cannot_auto_promote() {
        assert_eq!(
            refuse_raw_record_auto_promotion(&record(1)),
            Err(AppMemoryBridgeError::RawRecordAutoPromotionDenied)
        );
    }

    #[test]
    fn accepted_current_source_may_enter_prompt_with_live_evidence() {
        let (_, _, _, source, candidate) = accepted_candidate();
        let mut metadata = json!({"candidate_kind": "collection_item"});
        attach_source_eligibility_envelope(&mut metadata, &candidate);
        assert!(metadata.get("temperature").is_none());
        assert!(canonical_app_memory_may_enter_prompt(
            &metadata,
            Some(&candidate),
            Some(&[&source]),
            time(13),
        ));
    }

    #[test]
    fn missing_eligibility_authority_fails_closed() {
        let (_, _, _, _, candidate) = accepted_candidate();
        let mut metadata = json!({});
        attach_source_eligibility_envelope(&mut metadata, &candidate);
        assert!(!canonical_app_memory_may_enter_prompt(
            &metadata,
            Some(&candidate),
            None,
            time(13),
        ));
        assert!(!canonical_app_memory_may_enter_prompt(
            &metadata,
            None,
            Some(&[]),
            time(13),
        ));
    }

    #[test]
    fn ordinary_memory_without_envelope_still_retrieves() {
        assert!(canonical_app_memory_may_enter_prompt(
            &json!({"candidate_kind": "tier"}),
            None,
            None,
            time(13),
        ));
    }

    #[test]
    fn record_update_settles_accepted_candidate_stale() {
        let (auth, authority, _, _, candidate) = accepted_candidate();
        let changed = record(2);
        let source = source_evidence(&auth, &authority, &changed, 2, time(13));
        let AppMemoryRetrievalDecision::Settled(next) =
            settle_app_memory_candidate(&candidate, &[&source], time(13)).unwrap()
        else {
            panic!("updated record must settle the candidate");
        };
        assert_eq!(next.status, AppMemoryCandidateStatus::Stale);
        assert!(!canonical_app_memory_may_enter_prompt(
            &{
                let mut metadata = json!({});
                attach_source_eligibility_envelope(&mut metadata, &next);
                metadata
            },
            Some(&next),
            Some(&[&source]),
            time(14),
        ));
    }

    #[test]
    fn disable_and_retain_settle_memory_dormant() {
        let (auth, authority, _, source, candidate) = accepted_candidate();
        for status in [
            AppInstallationStatus::Disabled,
            AppInstallationStatus::UninstalledRetained,
        ] {
            let dormant = ResolvedAppMemorySource::from_unavailable_installation(
                &auth,
                &unavailable_installation(&authority, status),
                source.source().clone(),
                AppRevision::new(2).unwrap(),
                time(13),
            )
            .unwrap();
            assert_eq!(dormant.state(), AppMemoryResolvedSourceState::Dormant);
            let AppMemoryRetrievalDecision::FailClosed(error) =
                evaluate_app_memory_retrieval(&candidate, &[&dormant], time(13))
            else {
                panic!("dormant source must fail retrieval");
            };
            assert_eq!(error, AppMemoryEligibilityError::SourceDormant { status });
        }
    }

    #[test]
    fn purge_tombstones_sole_source_and_keeps_independent_corroboration() {
        let (auth, authority, _, source, candidate) = accepted_candidate();
        let purged = ResolvedAppMemorySource::from_unavailable_installation(
            &auth,
            &unavailable_installation(&authority, AppInstallationStatus::Purged),
            source.source().clone(),
            AppRevision::new(3).unwrap(),
            time(13),
        )
        .unwrap();
        let AppMemoryRetrievalDecision::Settled(next) =
            settle_app_memory_candidate(&candidate, &[&purged], time(13)).unwrap()
        else {
            panic!("purged sole source must tombstone");
        };
        assert_eq!(next.status, AppMemoryCandidateStatus::Tombstoned);

        let mut corroborated = candidate.clone();
        let mut second = source.source().clone();
        second.record_id = AppRecordId::parse("record_2").unwrap();
        second.canonical_source_ref.reference = reference("entity:person/record:2");
        corroborated.source_refs.push(second.clone());
        assert!(independently_corroborated_after_source_removal(
            &corroborated,
            &[&source]
        ));
        assert!(!independently_corroborated_after_source_removal(
            &candidate,
            &[&purged]
        ));
    }

    #[test]
    fn envelope_cannot_assign_temperature_or_force_prompt() {
        let (_, _, _, _, candidate) = accepted_candidate();
        let mut metadata = json!({
            "temperature": "hot",
            "force_prompt_inclusion": true
        });
        attach_source_eligibility_envelope(&mut metadata, &candidate);
        assert!(metadata.get("temperature").is_none());
        assert!(metadata.get("force_prompt_inclusion").is_none());
        let envelope = parse_source_eligibility_envelope(&metadata)
            .unwrap()
            .unwrap();
        assert_eq!(envelope.schema_version, 1);
        assert_eq!(envelope.candidate_id, candidate.candidate_id);
    }
}

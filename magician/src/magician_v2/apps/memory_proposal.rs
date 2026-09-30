//! Production owner for source-linked app-memory proposals.
//!
//! The model supplies only a short-lived projection handle, a selected row and
//! a derived claim. The owner reopens the exact row under fresh direct-owner
//! authority and publishes only a `Proposed` candidate. Acceptance remains a
//! separate reviewed lifecycle command.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Map, Value};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::{
    authority::AuthenticatedAppScope,
    boundary::{AppPersonalAgentPublicationFence, AppPersonalAgentReadAuthority},
    entity_adapter::{AppEntityAdapterError, AppEntityAdapterService},
    memory::{AppMemoryCandidate, AppMemorySemanticDestination, AppMemoryTierScope},
    memory_bridge::{propose_source_linked_candidate, AppMemoryBridgeError},
    memory_store::{AppMemoryCandidatePublication, AppMemoryStoreError},
    models::{
        AppContractError, AppContractLimits, AppDataEnvelope, AppDataSource, AppDigest,
        AppHandlingLabels, AppProtocolVersion, AppQueryPage, AppQueryRequest, AppRecordId,
        AppRecordProjection, AppReference, AppSourceRef, AppSourceRefKind, ValidateAppContract,
    },
    policy::{
        join_app_content, AppHandlingConstraint, AppPolicyError, ResolvedAppHandlingLabels,
        RevalidatedAppEnvelope,
    },
    registry::AppRegistryService,
};
use crate::magician_v2::execution::agent_resources::AgentResources;

pub struct AppMemoryProposalRequest {
    pub source_query: AppQueryRequest,
    pub source_page: AppQueryPage,
    pub source_record_id: Option<AppRecordId>,
    pub candidate_id: AppReference,
    pub intended_tier_scope: AppMemoryTierScope,
    pub semantic_destination: AppMemorySemanticDestination,
    pub derived_claim_or_summary: String,
}

impl std::fmt::Debug for AppMemoryProposalRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppMemoryProposalRequest")
            .field(
                "source_installation_id",
                &self.source_page.envelope.installation_id,
            )
            .field("source_row_count", &self.source_page.envelope.value.len())
            .field("has_source_record_id", &self.source_record_id.is_some())
            .field("candidate_id", &self.candidate_id)
            .field("intended_tier_scope", &self.intended_tier_scope)
            .field("semantic_destination", &self.semantic_destination)
            .field("claim_bytes", &self.derived_claim_or_summary.len())
            .finish_non_exhaustive()
    }
}

/// Server-only result carrier. The candidate is safe only under the complete
/// source policy carried beside it through the compiled result guard.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub(crate) enum AppMemoryProposalOutcome {
    Proposed(AppMemoryProposalSuccess),
    Cancelled(AppMemoryProposalCancellation),
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AppMemoryProposalPublicationOutcome {
    Published,
    Recovered,
}

/// Model-facing success keeps the established candidate projection flat while
/// making publication recovery and effect settlement explicit. The complete
/// value remains protected by the result guard beside this JSON.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AppMemoryProposalSuccess {
    #[serde(flatten)]
    candidate: AppMemoryCandidate,
    publication_outcome: AppMemoryProposalPublicationOutcome,
    effect_committed: bool,
}

impl AppMemoryProposalSuccess {
    fn new(
        candidate: AppMemoryCandidate,
        publication_outcome: AppMemoryProposalPublicationOutcome,
    ) -> Self {
        Self {
            candidate,
            publication_outcome,
            effect_committed: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppMemoryProposalCancellation {
    protocol_version: AppProtocolVersion,
    candidate_id: AppReference,
    status: &'static str,
    error_code: &'static str,
    retryable: bool,
    effect_committed: bool,
}

impl AppMemoryProposalCancellation {
    fn new(candidate_id: AppReference) -> Self {
        Self {
            protocol_version: AppProtocolVersion::V1,
            candidate_id,
            status: "cancelled",
            error_code: "cancelled_before_publish",
            retryable: false,
            effect_committed: false,
        }
    }
}

pub(crate) struct AppGovernedMemoryProposal {
    outcome: AppMemoryProposalOutcome,
    effective_policy: super::records::AppDataHandlingPolicy,
}

impl AppGovernedMemoryProposal {
    pub(crate) fn into_parts(
        self,
    ) -> (
        AppMemoryProposalOutcome,
        super::records::AppDataHandlingPolicy,
    ) {
        (self.outcome, self.effective_policy)
    }
}

#[derive(Debug, Clone)]
pub struct AppMemoryProposalService {
    registry: AppRegistryService,
    entity_adapter: AppEntityAdapterService,
}

impl AppMemoryProposalService {
    pub fn new(registry: AppRegistryService) -> Self {
        Self {
            entity_adapter: AppEntityAdapterService::new(registry.clone()),
            registry,
        }
    }

    pub(crate) async fn propose(
        &self,
        authenticated: &AuthenticatedAppScope,
        authority: AppPersonalAgentReadAuthority,
        request: AppMemoryProposalRequest,
        resources: &AgentResources,
        publication_fence: &AppPersonalAgentPublicationFence,
        calling_profile_name: &str,
        cancellation: Option<&CancellationToken>,
        now: DateTime<Utc>,
    ) -> Result<AppGovernedMemoryProposal, AppMemoryProposalError> {
        request
            .source_page
            .validate_app_contract(&AppContractLimits::default())?;
        request
            .source_query
            .validate_app_contract(&AppContractLimits::default())?;
        if !request.source_query.relation_expansions.is_empty() {
            return Err(AppMemoryProposalError::UnsupportedSourceRelations);
        }
        if request.derived_claim_or_summary.is_empty()
            || request.derived_claim_or_summary.len()
                > AppContractLimits::default().max_value_bytes()
        {
            return Err(AppMemoryProposalError::InvalidClaim);
        }
        let projection =
            select_projection(&request.source_page, request.source_record_id.as_ref())?.clone();
        select_source_ref(&request.source_page, &projection)?;
        let revalidated = self
            .entity_adapter
            .revalidate_personal_agent_projection(
                authenticated,
                authority,
                request.source_query,
                &request.source_page.envelope,
                &projection,
                now,
            )
            .await?;
        let resolved_source = revalidated.memory_source(authenticated, now)?;
        let source_envelope = source_envelope(authenticated, &revalidated, now)?;
        let resolved_labels =
            ResolvedAppHandlingLabels::from_trusted_policy(source_envelope.handling_labels.clone());
        let source = RevalidatedAppEnvelope::from_trusted_resolution(
            &source_envelope,
            resolved_labels,
            &AppContractLimits::default(),
        )?;
        let policy_digest = AppDigest::blake3_canonical_json(&serde_json::to_value(
            revalidated.handling_policy(),
        )?)?;
        let constraint = AppHandlingConstraint {
            classification_floor: revalidated.handling_policy().classification_floor,
            model_processing: revalidated.handling_policy().model_processing,
            policy_digest,
            purpose: AppReference::parse(format!(
                "memory-proposal:{}",
                request.candidate_id.as_str()
            ))?,
            audience_ref: AppReference::parse("memory:candidate-review")?,
        };
        let joined = join_app_content(
            &[source],
            Value::String(request.derived_claim_or_summary),
            &constraint,
            &AppContractLimits::default(),
        )?;
        let candidate = propose_source_linked_candidate(
            authenticated,
            request.candidate_id,
            request.intended_tier_scope,
            request.semantic_destination,
            &joined,
            &[&resolved_source],
            now,
        )?;
        let effective_policy = revalidated.handling_policy().clone();
        let config = resources.magician_config_snapshot();
        super::processing_boundary::reattest_personal_agent_publication(
            &config,
            authenticated,
            calling_profile_name,
            publication_fence,
            Utc::now(),
        )?;
        let publication = self
            .registry
            .publish_app_memory_candidate(authenticated, candidate, cancellation.cloned(), now)
            .await
            .map_err(|source| {
                if memory_store_error_may_have_committed(&source) {
                    AppMemoryProposalError::PublicationStateUnknown { source }
                } else {
                    AppMemoryProposalError::PublicationFailed { source }
                }
            })?;
        // Publication may cross the effect boundary. Re-open the provider
        // grant once more before the remaining pure outcome projection. A
        // denial cannot undo or hide the durable recovery identity: the
        // caller supplied `candidate_id`, and the store's exact replay path
        // recovers that same candidate on a later authorized retry.
        let config = resources.magician_config_snapshot();
        super::processing_boundary::reattest_personal_agent_publication(
            &config,
            authenticated,
            calling_profile_name,
            publication_fence,
            Utc::now(),
        )
        .map_err(|source| AppMemoryProposalError::PublicationAfterCommitDenied { source })?;
        let outcome = match publication {
            AppMemoryCandidatePublication::Published(candidate) => {
                AppMemoryProposalOutcome::Proposed(AppMemoryProposalSuccess::new(
                    candidate,
                    AppMemoryProposalPublicationOutcome::Published,
                ))
            },
            AppMemoryCandidatePublication::Recovered(candidate) => {
                AppMemoryProposalOutcome::Proposed(AppMemoryProposalSuccess::new(
                    candidate,
                    AppMemoryProposalPublicationOutcome::Recovered,
                ))
            },
            AppMemoryCandidatePublication::Cancelled { candidate_id } => {
                AppMemoryProposalOutcome::Cancelled(AppMemoryProposalCancellation::new(
                    candidate_id,
                ))
            },
        };
        Ok(AppGovernedMemoryProposal {
            outcome,
            effective_policy,
        })
    }
}

fn select_projection<'a>(
    page: &'a AppQueryPage,
    record_id: Option<&AppRecordId>,
) -> Result<&'a AppRecordProjection, AppMemoryProposalError> {
    match record_id {
        Some(record_id) => {
            let mut matching = page
                .envelope
                .value
                .iter()
                .filter(|projection| &projection.record_id == record_id);
            let projection = matching
                .next()
                .ok_or(AppMemoryProposalError::MissingSourceRecord)?;
            if matching.next().is_some() {
                return Err(AppMemoryProposalError::AmbiguousSourceRecord);
            }
            Ok(projection)
        },
        None => match page.envelope.value.as_slice() {
            [projection] => Ok(projection),
            [] => Err(AppMemoryProposalError::MissingSourceRecord),
            _ => Err(AppMemoryProposalError::SourceRecordRequired),
        },
    }
}

fn select_source_ref<'a>(
    page: &'a AppQueryPage,
    projection: &AppRecordProjection,
) -> Result<&'a AppSourceRef, AppMemoryProposalError> {
    let identity = serde_json::json!({
        "entity": projection.entity,
        "record_id": projection.record_id,
    });
    let digest = AppDigest::blake3_canonical_json(&identity)?;
    let reference = AppReference::parse(format!("record:{}", digest.as_str()))?;
    let mut matching = page.envelope.source_refs.iter().filter(|source| {
        source.kind == AppSourceRefKind::EntityField
            && source.reference == reference
            && source.revision == Some(projection.record_revision)
    });
    let source = matching
        .next()
        .ok_or(AppMemoryProposalError::MissingSourceProvenance)?;
    if matching.next().is_some()
        || source.fields.is_empty()
        || !projection
            .fields
            .keys()
            .all(|field| source.fields.contains(field))
    {
        return Err(AppMemoryProposalError::AmbiguousSourceProvenance);
    }
    Ok(source)
}

fn source_envelope(
    authenticated: &AuthenticatedAppScope,
    source: &super::entity_store::AppRevalidatedRecordProjection,
    now: DateTime<Utc>,
) -> Result<AppDataEnvelope<Value>, AppMemoryProposalError> {
    let value = Value::Object(
        source
            .projection()
            .fields
            .iter()
            .map(|(field, value)| (field.to_string(), value.clone()))
            .collect::<Map<_, _>>(),
    );
    let source_refs = vec![source.source_ref().clone()];
    let handling_labels = AppHandlingLabels {
        classification: source.handling_policy().classification_floor,
        model_processing: source.handling_policy().model_processing,
        policy_digest: AppDigest::blake3_canonical_json(&serde_json::to_value(
            source.handling_policy(),
        )?)?,
        provenance_digest: AppDigest::blake3_canonical_json(&serde_json::to_value(&source_refs)?)?,
    };
    let value_schema_ref = AppReference::parse(format!(
        "value-schema:{}",
        AppDigest::blake3_canonical_json(&value)?.as_str()
    ))?;
    let envelope = AppDataEnvelope {
        protocol_version: AppProtocolVersion::V1,
        source: AppDataSource::AppStore,
        scope_binding_ref: authenticated.scope_binding_ref().clone(),
        installation_id: source.active().installation_id().clone(),
        package_revision_ref: source.active().package_revision_ref().clone(),
        schema_revision: source.active().schema_revision(),
        grant_revision: source.active().grant_revision(),
        value_schema_ref,
        content_digest: AppDigest::blake3_canonical_json(&value)?,
        value,
        source_refs,
        handling_labels,
        produced_at: now,
        expires_at: None,
    };
    envelope.validate_app_contract(&AppContractLimits::default())?;
    Ok(envelope)
}

#[derive(Debug, Error)]
pub enum AppMemoryProposalError {
    #[error(transparent)]
    Boundary(#[from] super::boundary::AppBoundaryError),
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error(transparent)]
    Entity(#[from] AppEntityAdapterError),
    #[error(transparent)]
    Eligibility(#[from] super::memory::AppMemoryEligibilityError),
    #[error(transparent)]
    Bridge(#[from] AppMemoryBridgeError),
    #[error("app-memory candidate publication outcome is unknown")]
    PublicationStateUnknown {
        #[source]
        source: AppMemoryStoreError,
    },
    #[error("app-memory candidate publication failed before commit")]
    PublicationFailed {
        #[source]
        source: AppMemoryStoreError,
    },
    #[error("app-memory candidate committed but final publication authority is stale")]
    PublicationAfterCommitDenied {
        #[source]
        source: super::boundary::AppBoundaryError,
    },
    #[error(transparent)]
    Policy(#[from] AppPolicyError),
    #[error(transparent)]
    Encoding(#[from] serde_json::Error),
    #[error("memory candidate claim is empty or exceeds the value ceiling")]
    InvalidClaim,
    #[error("the selected source record is missing")]
    MissingSourceRecord,
    #[error("the source page contains the selected record more than once")]
    AmbiguousSourceRecord,
    #[error("a source record id is required when the page has multiple rows")]
    SourceRecordRequired,
    #[error("the selected source record has no exact provenance")]
    MissingSourceProvenance,
    #[error("the selected source record has ambiguous provenance")]
    AmbiguousSourceProvenance,
    #[error("V1 app-memory proposals do not admit relation-expanded source projections")]
    UnsupportedSourceRelations,
}

impl AppMemoryProposalError {
    pub(crate) fn effect_may_be_committed(&self) -> bool {
        matches!(
            self,
            Self::PublicationStateUnknown { .. } | Self::PublicationAfterCommitDenied { .. }
        )
    }
}

fn memory_store_error_may_have_committed(error: &AppMemoryStoreError) -> bool {
    matches!(
        error,
        AppMemoryStoreError::Sqlite(_)
            | AppMemoryStoreError::Registry(super::registry::AppRegistryError::WorkerTerminated(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_publication_errors_preserve_effect_stage() {
        assert!(AppMemoryProposalError::PublicationStateUnknown {
            source: AppMemoryStoreError::CompareAndSwapLost,
        }
        .effect_may_be_committed());
        assert!(AppMemoryProposalError::PublicationAfterCommitDenied {
            source: super::super::boundary::AppBoundaryError::StalePersonalAgentProviderGrant,
        }
        .effect_may_be_committed());
        assert!(!AppMemoryProposalError::PublicationFailed {
            source: AppMemoryStoreError::CompareAndSwapLost,
        }
        .effect_may_be_committed());
        assert!(!AppMemoryProposalError::InvalidClaim.effect_may_be_committed());
    }

    #[test]
    fn cancelled_proposal_is_a_typed_non_effect_outcome() {
        let outcome = AppMemoryProposalOutcome::Cancelled(AppMemoryProposalCancellation::new(
            AppReference::parse("memory-candidate:cancelled").expect("candidate ref"),
        ));
        let value = serde_json::to_value(outcome).expect("serialize cancellation");

        assert_eq!(value["status"], "cancelled");
        assert_eq!(value["error_code"], "cancelled_before_publish");
        assert_eq!(value["retryable"], false);
        assert_eq!(value["effect_committed"], false);
        assert!(value.get("derived_claim_or_summary").is_none());
        assert!(value.get("source_refs").is_none());
    }

    #[test]
    fn publication_outcome_names_are_stable() {
        assert_eq!(
            serde_json::to_value(AppMemoryProposalPublicationOutcome::Published).unwrap(),
            Value::String("published".to_owned()),
        );
        assert_eq!(
            serde_json::to_value(AppMemoryProposalPublicationOutcome::Recovered).unwrap(),
            Value::String("recovered".to_owned()),
        );
    }
}

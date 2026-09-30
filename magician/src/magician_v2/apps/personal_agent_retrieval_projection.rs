//! High-level worker adapter for the private personal-agent retrieval owner.
//!
//! API/runtime callers see this service, never destination receipts, grant
//! fences, provider identities, leases, or acknowledgement permits. The
//! service can be constructed only inside the core crate with a server-owned
//! provider configuration and a live reviewed-authority resolver. Production
//! uses the registry/store-backed factory below; no transport bytes can supply
//! either owner.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, OnceLock},
    time::Duration as StdDuration,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use magician_app_contract::contribution::{
    content_digest, AppMemoryInvalidationV1, AppPersonalAgentRetrievalProjectionProposalV1,
};

use super::{
    authority::AuthenticatedAppScope,
    contribution::{load_consumed_contribution_task_bindings, AppContributionError},
    entity_store::AppEntityStoreService,
    models::{AppDigest, AppInstallationId, AppName, AppRecordId, AppRevision},
    records::{AppContributionDestinationBinding, AppContributionSource},
    registry::AppRegistryService,
    retrieval_contribution_outbox::{
        AppRetrievalDeliveryDispatchEvidence, AppRetrievalDeliveryKind, AppRetrievalDeliveryLease,
        AppRetrievalSourceAckPermit,
    },
};
use crate::magician_v2::{
    agents::{
        storage::AgentStorage, PersonalAgentRetrievalApplyOutcomeV1, PersonalAgentRetrievalError,
        PersonalAgentRetrievalGrantFenceV1, PersonalAgentRetrievalInspectionSnapshotV1,
        PersonalAgentRetrievalOwner, PersonalAgentRetrievalProviderIdentityV1,
    },
    artifact_v2::workspace::ArtifactV2Workspace,
};

const RETRIEVAL_DELIVERY_LEASE: StdDuration = StdDuration::from_secs(60);
const MAX_RETRY_DELAY_SECONDS: u64 = 60;
const RETRIEVAL_PROVIDER_ID: &str = "magician-personal-agent-retrieval-local";
/// Names which provider revision owns every scope's durable retrieval set.
/// Bump it when projection semantics change incompatibly. A scope whose head
/// is still at generation 0 re-seeds itself under the new owner; a sealed set
/// (generation > 0) fails closed until a migration for that bump exists.
const RETRIEVAL_PROVIDER_REVISION: u64 = 1;
const RETRIEVAL_PROVIDER_AUTHORITY_SEED: &[u8] =
    b"magician.personal-agent.retrieval-provider.authority.v1";
const RETRIEVAL_PROJECTION_SCHEMA_SEED: &[u8] =
    b"magician.personal-agent.retrieval-projection.schema.v1";
static PRODUCTION_PROVIDER: OnceLock<
    Result<AppPersonalAgentRetrievalProviderConfigurationV1, String>,
> = OnceLock::new();

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppPersonalAgentRetrievalProjectionReport {
    pub proposals_applied: usize,
    pub invalidations_applied: usize,
    pub expirations_applied: usize,
}

#[derive(Debug)]
pub enum AppPersonalAgentRetrievalProjectionError {
    Source(AppContributionError),
    Destination(PersonalAgentRetrievalError),
    CurrentAuthority(String),
    MissingExpectedHead,
}

impl fmt::Display for AppPersonalAgentRetrievalProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(error) => write!(formatter, "retrieval source journal failed: {error}"),
            Self::Destination(error) => write!(formatter, "retrieval destination failed: {error}"),
            Self::CurrentAuthority(error) => {
                write!(
                    formatter,
                    "retrieval current authority refused delivery: {error}"
                )
            },
            Self::MissingExpectedHead => {
                write!(
                    formatter,
                    "retrieval expiration has no acknowledged predecessor"
                )
            },
        }
    }
}

impl std::error::Error for AppPersonalAgentRetrievalProjectionError {}

impl From<AppContributionError> for AppPersonalAgentRetrievalProjectionError {
    fn from(error: AppContributionError) -> Self {
        Self::Source(error)
    }
}

impl From<PersonalAgentRetrievalError> for AppPersonalAgentRetrievalProjectionError {
    fn from(error: PersonalAgentRetrievalError) -> Self {
        Self::Destination(error)
    }
}

/// Server configuration for one durable retrieval implementation. It has no
/// deserializer and is converted to the destination's private identity only
/// inside this adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppPersonalAgentRetrievalProviderConfigurationV1 {
    provider_id: String,
    provider_revision: u64,
    provider_authority_digest: String,
    projection_schema_digest: String,
}

impl AppPersonalAgentRetrievalProviderConfigurationV1 {
    pub(crate) fn from_trusted_configuration(
        provider_id: impl Into<String>,
        provider_revision: u64,
        provider_authority_digest: impl Into<String>,
        projection_schema_digest: impl Into<String>,
    ) -> Result<Self, PersonalAgentRetrievalError> {
        let value = Self {
            provider_id: provider_id.into(),
            provider_revision,
            provider_authority_digest: provider_authority_digest.into(),
            projection_schema_digest: projection_schema_digest.into(),
        };
        value.to_private_identity()?;
        Ok(value)
    }

    fn to_private_identity(
        &self,
    ) -> Result<PersonalAgentRetrievalProviderIdentityV1, PersonalAgentRetrievalError> {
        PersonalAgentRetrievalProviderIdentityV1::from_trusted_provider(
            self.provider_id.clone(),
            self.provider_revision,
            self.provider_authority_digest.clone(),
            self.projection_schema_digest.clone(),
        )
    }
}

/// Non-deserializable result of resolving current server authority. Every axis
/// is compared with the sealed proposal before the adapter mints a private
/// destination grant fence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppPersonalAgentRetrievalReviewedAuthorityV1 {
    installation_id: String,
    installation_generation: u64,
    package_content_digest: String,
    grant_revision: u64,
    grant_authority_digest: String,
    contribution_port_digest: String,
    target_agent_id: String,
    target_goal_id: Option<String>,
}

impl AppPersonalAgentRetrievalReviewedAuthorityV1 {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_current_reviewed_authority(
        installation_id: impl Into<String>,
        installation_generation: u64,
        package_content_digest: impl Into<String>,
        grant_revision: u64,
        grant_authority_digest: impl Into<String>,
        contribution_port_digest: impl Into<String>,
        target_agent_id: impl Into<String>,
        target_goal_id: Option<String>,
    ) -> Self {
        Self {
            installation_id: installation_id.into(),
            installation_generation,
            package_content_digest: package_content_digest.into(),
            grant_revision,
            grant_authority_digest: grant_authority_digest.into(),
            contribution_port_digest: contribution_port_digest.into(),
            target_agent_id: target_agent_id.into(),
            target_goal_id,
        }
    }

    fn matches(&self, proposal: &AppPersonalAgentRetrievalProjectionProposalV1) -> bool {
        self.installation_id == proposal.header.installation_id
            && self.installation_generation == proposal.header.installation_generation
            && self.package_content_digest == proposal.header.package_content_digest
            && self.grant_revision == proposal.header.grant_revision
            && self.grant_authority_digest == proposal.header.grant_authority_digest
            && self.contribution_port_digest == proposal.header.contribution_port_digest
            && self.target_agent_id == proposal.target_agent_id
            && self.target_goal_id == proposal.target_goal_id
    }

    fn into_private_fence(
        self,
        provider: PersonalAgentRetrievalProviderIdentityV1,
    ) -> Result<PersonalAgentRetrievalGrantFenceV1, PersonalAgentRetrievalError> {
        PersonalAgentRetrievalGrantFenceV1::from_reviewed_grant(
            self.installation_id,
            self.installation_generation,
            self.package_content_digest,
            self.grant_revision,
            self.grant_authority_digest,
            self.contribution_port_digest,
            self.target_agent_id,
            self.target_goal_id,
            provider,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppPersonalAgentRetrievalAuthorityPurpose {
    Admission,
    /// Read-only prompt/search projection. The retained proposal is merely a
    /// lookup key: current installation, lock, grant and exact source
    /// authority are reopened before and after the destination snapshot.
    PromptDisclosure,
    /// Byte-identical replay of a row whose destination call may already have
    /// committed. This cannot widen authority; it only recovers the exact
    /// retained operation so its ordered invalidation can subsequently run.
    RecoveryAdmission,
    SafetyInvalidation,
}

#[async_trait]
pub(crate) trait AppPersonalAgentRetrievalCurrentAuthorityResolver: Send + Sync {
    async fn resolve_current_authority(
        &self,
        authenticated: &AuthenticatedAppScope,
        proposal: &AppPersonalAgentRetrievalProjectionProposalV1,
        invalidation: Option<&AppMemoryInvalidationV1>,
        purpose: AppPersonalAgentRetrievalAuthorityPurpose,
        now: DateTime<Utc>,
    ) -> Result<AppPersonalAgentRetrievalReviewedAuthorityV1, String>;
}

#[derive(Clone)]
struct RegistryPersonalAgentRetrievalAuthorityResolver {
    registry: AppRegistryService,
    entity_store: AppEntityStoreService,
}

#[async_trait]
impl AppPersonalAgentRetrievalCurrentAuthorityResolver
    for RegistryPersonalAgentRetrievalAuthorityResolver
{
    async fn resolve_current_authority(
        &self,
        authenticated: &AuthenticatedAppScope,
        proposal: &AppPersonalAgentRetrievalProjectionProposalV1,
        invalidation: Option<&AppMemoryInvalidationV1>,
        purpose: AppPersonalAgentRetrievalAuthorityPurpose,
        now: DateTime<Utc>,
    ) -> Result<AppPersonalAgentRetrievalReviewedAuthorityV1, String> {
        proposal.validate().map_err(|error| error.to_string())?;
        if matches!(
            purpose,
            AppPersonalAgentRetrievalAuthorityPurpose::SafetyInvalidation
                | AppPersonalAgentRetrievalAuthorityPurpose::RecoveryAdmission
        ) {
            invalidation
                .map(|value| value.validate().map_err(|error| error.to_string()))
                .transpose()?;
            if purpose == AppPersonalAgentRetrievalAuthorityPurpose::SafetyInvalidation
                && invalidation.is_none()
            {
                return Err("retrieval invalidation lost its sealed command".to_owned());
            }
            if purpose == AppPersonalAgentRetrievalAuthorityPurpose::RecoveryAdmission
                && invalidation.is_some()
            {
                return Err("retrieval admission recovery carried an invalidation".to_owned());
            }
            // The outbox lease already rederived the exact retained source
            // row. Current app authority may have been revoked precisely
            // because this safety operation must retract its old projection.
            return Ok(reviewed_authority_from_retained_proposal(proposal));
        }
        if invalidation.is_some() {
            return Err("retrieval admission carried an invalidation command".to_owned());
        }

        let installation_id = AppInstallationId::parse(proposal.header.installation_id.clone())
            .map_err(|error| error.to_string())?;
        let source = proposal
            .header
            .sources
            .first()
            .filter(|_| proposal.header.sources.len() == 1)
            .ok_or_else(|| "retrieval V1 requires one exact source".to_owned())?;
        let entity =
            AppName::parse(source.entity_name.clone()).map_err(|error| error.to_string())?;
        let record_id =
            AppRecordId::parse(source.record_id.clone()).map_err(|error| error.to_string())?;
        let record_revision =
            AppRevision::new(source.record_revision).map_err(|error| error.to_string())?;
        let snapshot = self
            .entity_store
            .runtime_contribution_source_snapshot(
                authenticated,
                &installation_id,
                &entity,
                &record_id,
                record_revision,
                now.clone(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let (installation, active) = snapshot.into_parts();
        let package = self
            .registry
            .package_revision(
                authenticated,
                &installation.package_revision_ref,
                now.clone(),
            )
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "retrieval package revision is unavailable".to_owned())?;
        let package_lock = self
            .registry
            .package_dependency_lock(
                authenticated,
                &installation.package_revision_ref,
                now.clone(),
            )
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "retrieval package lock is unavailable".to_owned())?;
        let workflow_id = AppName::parse(proposal.header.workflow_id.clone())
            .map_err(|error| error.to_string())?;
        let action_id =
            AppName::parse(proposal.header.action_id.clone()).map_err(|error| error.to_string())?;
        let port_id = AppName::parse(proposal.header.contribution_port_id.clone())
            .map_err(|error| error.to_string())?;
        let task_identity = format!(
            "task:retrieval-authority:{}",
            content_digest(proposal.header.proposal_id.as_bytes()).trim_start_matches("blake3:")
        );
        let bindings = load_consumed_contribution_task_bindings(
            &self.registry,
            authenticated,
            &task_identity,
            &installation,
            &workflow_id,
            &action_id,
            &package.content_digest,
            &package_lock,
            active.grant(),
            now,
        )
        .await
        .map_err(|error| error.to_string())?;
        let binding = bindings
            .iter()
            .find(|binding| binding.port_id() == &port_id)
            .ok_or_else(|| "retrieval contribution port is not currently reviewed".to_owned())?;
        let reviewed = binding.reviewed_grant();
        let AppContributionSource::MutationBackedEntityProjection {
            entity: reviewed_entity,
            selected_fields,
        } = &reviewed.source;
        let reviewed_fields = selected_fields
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let reviewed_audiences = reviewed
            .audiences
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let retention_ms = i64::try_from(reviewed.maximum_retention_seconds)
            .ok()
            .and_then(|seconds| seconds.checked_mul(1_000))
            .ok_or_else(|| "retrieval retention ceiling overflowed".to_owned())?;
        let source_ref_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
            "schema": "magician.app-contribution-source-ref.v1",
            "installation_id": &installation_id,
            "entity": &entity,
            "record_id": &record_id,
            "selected_fields": &source.selected_fields,
        }))
        .map_err(|error| error.to_string())?;
        let source_identity_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
            "schema": "magician.app-contribution-source-identity.v1",
            "source_ref_digest": &source_ref_digest,
            "record_revision": record_revision,
            "handling_labels": &source.handling_labels,
        }))
        .map_err(|error| error.to_string())?;
        let canonical_source_ref = format!(
            "entity:{}",
            source_ref_digest.as_str().trim_start_matches("blake3:")
        );
        if authenticated.scope_binding_ref().as_str() != proposal.header.scope_binding_ref
            || proposal.header.destination_schema_digest
                != content_digest(RETRIEVAL_PROJECTION_SCHEMA_SEED)
            || source.canonical_source_ref != canonical_source_ref
            || source.canonical_source_digest != source_identity_digest.to_string()
            || installation.lifecycle.generation != proposal.header.installation_generation
            || installation.package_revision_ref.to_string() != proposal.header.package_revision_ref
            || package.content_digest.to_string() != proposal.header.package_content_digest
            || package.entity_schema_digest.to_string() != proposal.header.schema_digest
            || active.grant_revision().get() != proposal.header.grant_revision
            || active.grant().authority_digest.to_string() != proposal.header.grant_authority_digest
            || active.schema_revision().get() != proposal.header.schema_revision
            || binding.workflow_declaration_digest().to_string() != proposal.header.workflow_digest
            || binding.action_declaration_digest().to_string() != proposal.header.action_digest
            || binding.locked_port_digest().to_string() != proposal.header.contribution_port_digest
            || binding.destination_binding()
                != AppContributionDestinationBinding::PersonalAssistantRetrievalNoGoal
            || reviewed_entity != &entity
            || reviewed_fields != source.selected_fields
            || reviewed.purposes.len() != 1
            || reviewed.purposes[0].as_str() != proposal.header.purpose
            || reviewed_audiences != proposal.header.audiences
            || reviewed.evidence_classes.len() != 1
            || reviewed.evidence_classes[0] != proposal.header.evidence_class
            || proposal.header.expires_at_ms - proposal.header.issued_at_ms > retention_ms
            || proposal.target_agent_id != "personal-assistant"
            || proposal.target_goal_id.is_some()
        {
            return Err(
                "retrieval proposal no longer matches current reviewed authority".to_owned(),
            );
        }
        Ok(
            AppPersonalAgentRetrievalReviewedAuthorityV1::from_current_reviewed_authority(
                proposal.header.installation_id.clone(),
                proposal.header.installation_generation,
                proposal.header.package_content_digest.clone(),
                proposal.header.grant_revision,
                proposal.header.grant_authority_digest.clone(),
                proposal.header.contribution_port_digest.clone(),
                proposal.target_agent_id.clone(),
                proposal.target_goal_id.clone(),
            ),
        )
    }
}

fn reviewed_authority_from_retained_proposal(
    proposal: &AppPersonalAgentRetrievalProjectionProposalV1,
) -> AppPersonalAgentRetrievalReviewedAuthorityV1 {
    AppPersonalAgentRetrievalReviewedAuthorityV1::from_current_reviewed_authority(
        proposal.header.installation_id.clone(),
        proposal.header.installation_generation,
        proposal.header.package_content_digest.clone(),
        proposal.header.grant_revision,
        proposal.header.grant_authority_digest.clone(),
        proposal.header.contribution_port_digest.clone(),
        proposal.target_agent_id.clone(),
        proposal.target_goal_id.clone(),
    )
}

#[derive(Clone)]
pub struct AppPersonalAgentRetrievalProjectionService {
    registry: AppRegistryService,
    workspace: ArtifactV2Workspace,
    provider: AppPersonalAgentRetrievalProviderConfigurationV1,
    authority_resolver: Arc<dyn AppPersonalAgentRetrievalCurrentAuthorityResolver>,
}

impl fmt::Debug for AppPersonalAgentRetrievalProjectionService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppPersonalAgentRetrievalProjectionService")
            .field("workspace", &self.workspace)
            .field("provider", &self.provider)
            .field("authority_resolver", &"server-owned")
            .finish_non_exhaustive()
    }
}

impl AppPersonalAgentRetrievalProjectionService {
    /// Construct the only production retrieval projection service. The
    /// provider identity is content-bound to both the private destination
    /// owner and this authority adapter; the resolver independently reopens
    /// current registry/store authority for every proposal admission.
    pub fn from_current_registry(
        registry: AppRegistryService,
        workspace: ArtifactV2Workspace,
    ) -> Result<Self, PersonalAgentRetrievalError> {
        let provider = production_provider_configuration()?;
        let resolver = Arc::new(RegistryPersonalAgentRetrievalAuthorityResolver {
            entity_store: AppEntityStoreService::new(registry.clone()),
            registry: registry.clone(),
        });
        Ok(Self::from_current_authority_resolver(
            registry, workspace, provider, resolver,
        ))
    }

    pub(crate) fn from_current_authority_resolver(
        registry: AppRegistryService,
        workspace: ArtifactV2Workspace,
        provider: AppPersonalAgentRetrievalProviderConfigurationV1,
        authority_resolver: Arc<dyn AppPersonalAgentRetrievalCurrentAuthorityResolver>,
    ) -> Self {
        Self {
            registry,
            workspace,
            provider,
            authority_resolver,
        }
    }

    pub async fn repair_scope(
        &self,
        authenticated: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<(), AppPersonalAgentRetrievalProjectionError> {
        let destination = self.destination_for(authenticated)?;
        let recovered_head = destination.recover(authenticated).await?;
        self.registry
            .audit_retrieval_destination_head(authenticated, recovered_head, now)
            .await?;
        self.registry
            .compact_retrieval_delivery_journal(authenticated, Utc::now())
            .await?;
        Ok(())
    }

    pub async fn contribution_state(
        &self,
        authenticated: &AuthenticatedAppScope,
        limit: usize,
    ) -> Result<PersonalAgentRetrievalInspectionSnapshotV1, AppPersonalAgentRetrievalProjectionError>
    {
        self.destination_for(authenticated)?
            .inspect_source_state(authenticated, limit)
            .await
            .map_err(Into::into)
    }

    /// Drain at most one event. The unified source sequence prevents an
    /// invalidation or expiration from overtaking an earlier response-lost
    /// proposal, and keeps the expected destination predecessor exact.
    pub async fn drain_scope(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease_owner: &str,
        now: DateTime<Utc>,
    ) -> Result<AppPersonalAgentRetrievalProjectionReport, AppPersonalAgentRetrievalProjectionError>
    {
        let Some(lease) = self
            .registry
            .claim_retrieval_delivery_outbox(
                authenticated,
                lease_owner.to_owned(),
                RETRIEVAL_DELIVERY_LEASE,
                now,
            )
            .await?
        else {
            return Ok(AppPersonalAgentRetrievalProjectionReport::default());
        };
        let authority = match self
            .resolve_authority(authenticated, &lease, Utc::now())
            .await
        {
            Ok(authority) => authority,
            Err(error) => {
                self.registry
                    .release_retrieval_delivery_lease(
                        authenticated,
                        lease,
                        retry_delay(1),
                        Utc::now(),
                    )
                    .await?;
                return Err(error);
            },
        };
        let permit = self
            .registry
            .begin_retrieval_delivery_dispatch(authenticated, lease, Utc::now())
            .await?;
        let evidence = permit.into_evidence(Utc::now().timestamp_millis())?;
        let result = self
            .deliver(
                authenticated,
                authority,
                &evidence,
                Utc::now().timestamp_millis(),
            )
            .await;
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                let delay = retry_delay(evidence.attempt_count);
                self.registry
                    .release_retrieval_delivery_dispatch(
                        authenticated,
                        evidence.dispatch_ack,
                        delay,
                        Utc::now(),
                    )
                    .await?;
                return Err(error);
            },
        };
        let receipt = match outcome {
            PersonalAgentRetrievalApplyOutcomeV1::Applied(receipt)
            | PersonalAgentRetrievalApplyOutcomeV1::ExactReplay(receipt) => receipt,
        };
        let mut report = AppPersonalAgentRetrievalProjectionReport::default();
        match evidence.kind {
            AppRetrievalDeliveryKind::Proposal => report.proposals_applied = 1,
            AppRetrievalDeliveryKind::Invalidation => report.invalidations_applied = 1,
            AppRetrievalDeliveryKind::Expiration => report.expirations_applied = 1,
        }
        self.registry
            .acknowledge_retrieval_delivery_outbox(
                authenticated,
                AppRetrievalSourceAckPermit::from_destination(receipt, evidence.dispatch_ack),
                Utc::now(),
            )
            .await?;
        Ok(report)
    }

    /// Produce a bounded prompt/search projection only after two independent
    /// current-authority fences around the private destination snapshot. The
    /// first snapshot is never disclosed; it supplies exact proposal
    /// identities for review. The second snapshot must contain the same sealed
    /// bytes and every returned row is revalidated once more at the final
    /// disclosure boundary.
    pub(crate) async fn prompt_projection(
        &self,
        authenticated: &AuthenticatedAppScope,
        target_agent_id: &str,
        target_goal_id: Option<&str>,
        maximum_results: usize,
        now: DateTime<Utc>,
    ) -> Result<
        Vec<AppPersonalAgentRetrievalProjectionProposalV1>,
        AppPersonalAgentRetrievalProjectionError,
    > {
        authenticated.ensure_live_at(&now).map_err(|error| {
            AppPersonalAgentRetrievalProjectionError::CurrentAuthority(error.to_string())
        })?;
        let destination = self.destination_for(authenticated)?;
        let first = destination
            .projected_candidates_for_target(
                authenticated,
                target_agent_id,
                target_goal_id,
                maximum_results,
                now.timestamp_millis(),
            )
            .await?;
        let mut reviewed = BTreeMap::new();
        for proposal in first {
            let Ok(authority) = self
                .authority_resolver
                .resolve_current_authority(
                    authenticated,
                    &proposal,
                    None,
                    AppPersonalAgentRetrievalAuthorityPurpose::PromptDisclosure,
                    Utc::now(),
                )
                .await
            else {
                continue;
            };
            if authority.matches(&proposal) {
                reviewed.insert(
                    proposal.header.proposal_id.clone(),
                    proposal.proposal_digest.clone(),
                );
            }
        }
        if reviewed.is_empty() {
            return Ok(Vec::new());
        }

        let final_now = Utc::now();
        authenticated.ensure_live_at(&final_now).map_err(|error| {
            AppPersonalAgentRetrievalProjectionError::CurrentAuthority(error.to_string())
        })?;
        let second = destination
            .projected_candidates_for_target(
                authenticated,
                target_agent_id,
                target_goal_id,
                maximum_results,
                final_now.timestamp_millis(),
            )
            .await?;
        let mut disclosed = Vec::with_capacity(second.len());
        for proposal in second {
            if reviewed.get(&proposal.header.proposal_id) != Some(&proposal.proposal_digest) {
                continue;
            }
            let Ok(authority) = self
                .authority_resolver
                .resolve_current_authority(
                    authenticated,
                    &proposal,
                    None,
                    AppPersonalAgentRetrievalAuthorityPurpose::PromptDisclosure,
                    Utc::now(),
                )
                .await
            else {
                continue;
            };
            if !authority.matches(&proposal) {
                continue;
            }
            disclosed.push(proposal);
        }
        Ok(disclosed)
    }

    async fn resolve_authority(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease: &AppRetrievalDeliveryLease,
        now: DateTime<Utc>,
    ) -> Result<
        Option<AppPersonalAgentRetrievalReviewedAuthorityV1>,
        AppPersonalAgentRetrievalProjectionError,
    > {
        let purpose = match lease.kind() {
            AppRetrievalDeliveryKind::Proposal if lease.was_dispatched() => {
                AppPersonalAgentRetrievalAuthorityPurpose::RecoveryAdmission
            },
            AppRetrievalDeliveryKind::Proposal => {
                AppPersonalAgentRetrievalAuthorityPurpose::Admission
            },
            AppRetrievalDeliveryKind::Invalidation => {
                AppPersonalAgentRetrievalAuthorityPurpose::SafetyInvalidation
            },
            AppRetrievalDeliveryKind::Expiration => return Ok(None),
        };
        let authority = self
            .authority_resolver
            .resolve_current_authority(
                authenticated,
                lease.proposal(),
                lease.invalidation(),
                purpose,
                now,
            )
            .await
            .map_err(AppPersonalAgentRetrievalProjectionError::CurrentAuthority)?;
        if !authority.matches(lease.proposal()) {
            return Err(AppPersonalAgentRetrievalProjectionError::CurrentAuthority(
                "resolved authority does not match the exact sealed proposal".to_owned(),
            ));
        }
        Ok(Some(authority))
    }

    async fn deliver(
        &self,
        authenticated: &AuthenticatedAppScope,
        authority: Option<AppPersonalAgentRetrievalReviewedAuthorityV1>,
        evidence: &AppRetrievalDeliveryDispatchEvidence,
        recorded_at_ms: i64,
    ) -> Result<PersonalAgentRetrievalApplyOutcomeV1, AppPersonalAgentRetrievalProjectionError>
    {
        let provider = self.provider.to_private_identity()?;
        let destination = self.destination_for_with_provider(authenticated, provider.clone())?;
        match evidence.kind {
            AppRetrievalDeliveryKind::Proposal => {
                let fence = authority
                    .ok_or_else(|| {
                        AppPersonalAgentRetrievalProjectionError::CurrentAuthority(
                            "proposal admission lost reviewed authority".to_owned(),
                        )
                    })?
                    .into_private_fence(provider)?;
                Ok(destination
                    .stage_projection(
                        authenticated,
                        fence,
                        evidence.proposal.clone(),
                        evidence.expected_destination_head.clone(),
                        recorded_at_ms,
                    )
                    .await?)
            },
            AppRetrievalDeliveryKind::Invalidation => {
                let fence = authority
                    .ok_or_else(|| {
                        AppPersonalAgentRetrievalProjectionError::CurrentAuthority(
                            "safety invalidation lost reviewed source authority".to_owned(),
                        )
                    })?
                    .into_private_fence(provider)?;
                let invalidation = evidence.invalidation.clone().ok_or_else(|| {
                    AppPersonalAgentRetrievalProjectionError::CurrentAuthority(
                        "invalidation journal lost exact bytes".to_owned(),
                    )
                })?;
                Ok(destination
                    .invalidate_projection(
                        authenticated,
                        fence,
                        invalidation,
                        evidence.expected_destination_head.clone(),
                        recorded_at_ms,
                    )
                    .await?)
            },
            AppRetrievalDeliveryKind::Expiration => {
                let expected = evidence
                    .expected_destination_head
                    .clone()
                    .ok_or(AppPersonalAgentRetrievalProjectionError::MissingExpectedHead)?;
                Ok(destination
                    .expire_projection(
                        authenticated,
                        &evidence.proposal.header.proposal_id,
                        &evidence.proposal.proposal_digest,
                        expected,
                        recorded_at_ms,
                    )
                    .await?)
            },
        }
    }

    fn destination_for(
        &self,
        authenticated: &AuthenticatedAppScope,
    ) -> Result<PersonalAgentRetrievalOwner, PersonalAgentRetrievalError> {
        self.destination_for_with_provider(authenticated, self.provider.to_private_identity()?)
    }

    fn destination_for_with_provider(
        &self,
        authenticated: &AuthenticatedAppScope,
        provider: PersonalAgentRetrievalProviderIdentityV1,
    ) -> Result<PersonalAgentRetrievalOwner, PersonalAgentRetrievalError> {
        let principal = authenticated.scope().principal.as_str();
        let workspace = authenticated.scope().workspace.as_str();
        let root = self
            .workspace
            .scoped_agent_runtime_root(principal, workspace);
        let storage = AgentStorage::new_in_workspace(root, self.workspace.clone());
        PersonalAgentRetrievalOwner::new(
            storage,
            principal,
            workspace,
            authenticated.scope_binding_ref().as_str(),
            provider,
        )
    }
}

fn production_provider_configuration(
) -> Result<AppPersonalAgentRetrievalProviderConfigurationV1, PersonalAgentRetrievalError> {
    match PRODUCTION_PROVIDER.get_or_init(|| {
        // The authority digest is derived from the explicit provider id,
        // revision and seed — never from source bytes. It used to hash the raw
        // bytes of seventeen source files, so every edit to any of them (a
        // formatting pass included) changed the owner identity, orphaned each
        // scope's persisted head and left boot repair retrying forever.
        let provider_material = [
            RETRIEVAL_PROVIDER_ID.as_bytes(),
            &b"\n"[..],
            RETRIEVAL_PROVIDER_REVISION.to_string().as_bytes(),
            &b"\n"[..],
            RETRIEVAL_PROVIDER_AUTHORITY_SEED,
        ]
        .concat();
        AppPersonalAgentRetrievalProviderConfigurationV1::from_trusted_configuration(
            RETRIEVAL_PROVIDER_ID,
            RETRIEVAL_PROVIDER_REVISION,
            content_digest(&provider_material),
            content_digest(RETRIEVAL_PROJECTION_SCHEMA_SEED),
        )
        .map_err(|error| error.to_string())
    }) {
        Ok(provider) => Ok(provider.clone()),
        Err(error) => Err(PersonalAgentRetrievalError::Validation(error.clone())),
    }
}

fn retry_delay(attempt_count: u32) -> StdDuration {
    let shift = attempt_count.saturating_sub(1).min(6);
    StdDuration::from_secs(
        1_u64
            .checked_shl(shift)
            .unwrap_or(MAX_RETRY_DELAY_SECONDS)
            .min(MAX_RETRY_DELAY_SECONDS),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_is_bounded() {
        assert_eq!(retry_delay(1), StdDuration::from_secs(1));
        assert_eq!(
            retry_delay(u32::MAX),
            StdDuration::from_secs(MAX_RETRY_DELAY_SECONDS)
        );
    }
}

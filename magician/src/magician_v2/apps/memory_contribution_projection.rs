//! Production owner for delivering app memory contribution journals.
//!
//! The HTTP/runtime crate sees only this high-level service. Source leases,
//! dispatch permits, and destination acknowledgements remain move-only inside
//! the core crate so no transport caller can manufacture delivery authority.

use std::{fmt, time::Duration as StdDuration};

use chrono::{DateTime, Utc};
use magician_app_contract::contribution::{
    AppMemoryDestinationReceiptV1, AppMemoryOwnerDecisionEnvelopeV1, AppMemoryOwnerReviewV1,
};

use super::{
    authority::AuthenticatedAppScope, contribution::AppContributionError,
    registry::AppRegistryService,
};
use crate::magician_v2::{
    agents::{
        AgentMemoryError, AgentMemoryResolver, AppMemoryContributionStateSnapshotV1,
        AppMemoryDestinationApplyResult,
    },
    artifact_v2::workspace::ArtifactV2Workspace,
};

const APP_MEMORY_PROJECTION_CLAIM_BATCH: usize = 16;
const APP_MEMORY_PROJECTION_LEASE: StdDuration = StdDuration::from_secs(60);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppMemoryContributionProjectionReport {
    pub proposals_applied: usize,
    pub invalidations_applied: usize,
}

#[derive(Debug)]
pub enum AppMemoryContributionProjectionError {
    Source(AppContributionError),
    Destination(AgentMemoryError),
    InvalidDestinationResult(&'static str),
}

impl fmt::Display for AppMemoryContributionProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(error) => write!(formatter, "app-memory source journal failed: {error}"),
            Self::Destination(error) => {
                write!(formatter, "app-memory destination owner failed: {error}")
            },
            Self::InvalidDestinationResult(kind) => write!(
                formatter,
                "app-memory destination returned the wrong result kind for {kind}",
            ),
        }
    }
}

impl std::error::Error for AppMemoryContributionProjectionError {}

impl From<AppContributionError> for AppMemoryContributionProjectionError {
    fn from(error: AppContributionError) -> Self {
        Self::Source(error)
    }
}

impl From<AgentMemoryError> for AppMemoryContributionProjectionError {
    fn from(error: AgentMemoryError) -> Self {
        Self::Destination(error)
    }
}

#[derive(Debug, Clone)]
pub struct AppMemoryContributionProjectionService {
    registry: AppRegistryService,
    destination_resolver: AgentMemoryResolver,
}

impl AppMemoryContributionProjectionService {
    pub fn new(registry: AppRegistryService, workspace: ArtifactV2Workspace) -> Self {
        Self {
            registry,
            destination_resolver: AgentMemoryResolver::with_workspace_layout(workspace),
        }
    }

    /// Repair the destination receipt/head/projection chain before the worker
    /// starts claiming source rows. Source journal compaction is safe only
    /// after exact destination acknowledgements have been retained.
    pub async fn repair_scope(
        &self,
        authenticated: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<(), AppMemoryContributionProjectionError> {
        let destination = self.destination_for(authenticated)?;
        destination.recover_app_memory_destination().await?;
        self.registry
            .compact_memory_contribution_journals(authenticated, now)
            .await?;
        Ok(())
    }

    pub async fn pending_owner_reviews(
        &self,
        authenticated: &AuthenticatedAppScope,
        desktop_identity_key_id: &str,
        desktop_identity_digest: &str,
        limit: usize,
        now: DateTime<Utc>,
    ) -> Result<Vec<AppMemoryOwnerReviewV1>, AppMemoryContributionProjectionError> {
        self.destination_for(authenticated)?
            .pending_app_memory_owner_reviews(
                authenticated,
                desktop_identity_key_id,
                desktop_identity_digest,
                limit,
                now.timestamp_millis(),
            )
            .await
            .map_err(Into::into)
    }

    pub async fn contribution_state(
        &self,
        authenticated: &AuthenticatedAppScope,
        desktop_identity: Option<(&str, &str)>,
        limit: usize,
    ) -> Result<AppMemoryContributionStateSnapshotV1, AppMemoryContributionProjectionError> {
        self.destination_for(authenticated)?
            .app_memory_contribution_state(authenticated, desktop_identity, limit)
            .await
            .map_err(Into::into)
    }

    pub async fn apply_owner_decision(
        &self,
        authenticated: &AuthenticatedAppScope,
        desktop_identity_public_key_hex: &str,
        expected_desktop_identity_key_id: &str,
        expected_desktop_identity_digest: &str,
        envelope: AppMemoryOwnerDecisionEnvelopeV1,
        now: DateTime<Utc>,
    ) -> Result<AppMemoryDestinationReceiptV1, AppMemoryContributionProjectionError> {
        let result = self
            .destination_for(authenticated)?
            .apply_app_memory_owner_decision(
                authenticated,
                desktop_identity_public_key_hex,
                expected_desktop_identity_key_id,
                expected_desktop_identity_digest,
                envelope,
                now.timestamp_millis(),
            )
            .await?;
        let AppMemoryDestinationApplyResult::Decision {
            destination_receipt,
        } = result
        else {
            return Err(
                AppMemoryContributionProjectionError::InvalidDestinationResult("owner decision"),
            );
        };
        Ok(destination_receipt)
    }

    /// Drain one bounded batch. Invalidations run first so source revocation,
    /// expiry, and lifecycle safety cannot be starved by new proposal volume.
    /// A response-lost destination write remains `dispatching` and is reclaimed
    /// by the registry only after its exact lease expires.
    pub async fn drain_scope(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease_owner: &str,
        now: DateTime<Utc>,
    ) -> Result<AppMemoryContributionProjectionReport, AppMemoryContributionProjectionError> {
        let destination = self.destination_for(authenticated)?;
        let mut report = AppMemoryContributionProjectionReport::default();

        let invalidation_owner = format!("{lease_owner}:invalidation");
        let invalidations = self
            .registry
            .claim_memory_invalidation_outbox(
                authenticated,
                invalidation_owner,
                APP_MEMORY_PROJECTION_CLAIM_BATCH,
                APP_MEMORY_PROJECTION_LEASE,
                now,
            )
            .await?;
        for lease in invalidations {
            let permit = self
                .registry
                .begin_memory_invalidation_dispatch(authenticated, lease, Utc::now())
                .await?;
            let result = destination
                .invalidate_app_memory_candidate(
                    authenticated,
                    permit,
                    Utc::now().timestamp_millis(),
                )
                .await?;
            let AppMemoryDestinationApplyResult::Invalidation { source_ack, .. } = result else {
                return Err(
                    AppMemoryContributionProjectionError::InvalidDestinationResult("invalidation"),
                );
            };
            self.registry
                .acknowledge_memory_invalidation_outbox(authenticated, source_ack, Utc::now())
                .await?;
            report.invalidations_applied = report.invalidations_applied.saturating_add(1);
        }

        let proposal_owner = format!("{lease_owner}:proposal");
        let proposals = self
            .registry
            .claim_memory_contribution_outbox(
                authenticated,
                proposal_owner,
                APP_MEMORY_PROJECTION_CLAIM_BATCH,
                APP_MEMORY_PROJECTION_LEASE,
                Utc::now(),
            )
            .await?;
        for lease in proposals {
            let permit = self
                .registry
                .begin_memory_contribution_dispatch(authenticated, lease, Utc::now())
                .await?;
            let result = destination
                .stage_app_memory_candidate(authenticated, permit, Utc::now().timestamp_millis())
                .await?;
            let AppMemoryDestinationApplyResult::Proposal { source_ack, .. } = result else {
                return Err(
                    AppMemoryContributionProjectionError::InvalidDestinationResult("proposal"),
                );
            };
            self.registry
                .acknowledge_memory_contribution_outbox(authenticated, source_ack, Utc::now())
                .await?;
            report.proposals_applied = report.proposals_applied.saturating_add(1);
        }

        self.registry
            .compact_memory_contribution_journals(authenticated, Utc::now())
            .await?;
        Ok(report)
    }

    fn destination_for(
        &self,
        authenticated: &AuthenticatedAppScope,
    ) -> Result<crate::magician_v2::agents::AgentMemoryService, AgentMemoryError> {
        self.destination_resolver.resolve_for_scope(
            authenticated.scope().principal.as_str(),
            authenticated.scope().workspace.as_str(),
        )
    }
}

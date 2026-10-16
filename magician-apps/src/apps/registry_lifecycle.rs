//! Durable Phase-1 lifecycle, approval and projection-outbox boundary.
//!
//! A serialized approval is stored evidence, never authority. Reviewed commits
//! rejoin it to the live authenticated session, exact package/attempt rows and
//! grant/schema/surface revisions inside one SQLite `IMMEDIATE` transaction.
//! The same transaction advances the installation and appends the metadata-only
//! outbox event, so a crash can leave neither a half-enabled installation nor a
//! missing projection event.

use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use magician_app_contract::contribution::AppMemoryInvalidationReasonV1;
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(test)]
use super::records::AppSurfaceBinding;
use super::{
    approval_boundary::{
        apply_reviewed_installation_transition, authorize_installation_approval,
        AppInstallationApprovalExpectation,
    },
    authority::AuthenticatedAppScope,
    lifecycle::{
        reduce_attempt, AppInstallationCommand, AppInstallationStatus, AppLifecycleAttemptCommand,
        AppLifecycleAttemptState,
    },
    memory_store::{registry_memory_store_error, settle_candidates_for_installation},
    models::{
        decode_app_contract, AppContractLimits, AppDigest, AppInstallationId, AppReference,
        AppRevision, ValidateAppContract,
    },
    records::{
        AppGrantRevision, AppInstallation, AppInstallationApproval, AppLifecycleAttempt,
        AppLifecycleAttemptKind, AppPackageRevision, AppSchemaRevision, AppScope, AppSurfaceStatus,
    },
    registry::{
        encode_bounded_json, enum_json_label, format_timestamp, AppRegistryError,
        AppRegistryService,
    },
    surface_compiler::CompiledAppSurfaceSet,
};

const MAX_OUTBOX_CLAIM_BATCH: usize = 100;
const MAX_OUTBOX_RECOVERY_BATCH: usize = 1_000;
const MAX_OUTBOX_LEASE: StdDuration = StdDuration::from_secs(300);

/// Store-ready approval evidence created only after a live server-owned scope
/// has been joined to the reviewed decision. It is intentionally move-only and
/// cannot be deserialized from an API body.
#[derive(Debug)]
pub struct AppApprovalPublication {
    approval: AppInstallationApproval,
    attempt_kind: AppLifecycleAttemptKind,
    current_global_policy_revision: AppRevision,
}

impl AppApprovalPublication {
    pub fn from_authenticated_decision(
        authenticated: &AuthenticatedAppScope,
        approval: AppInstallationApproval,
        attempt_kind: AppLifecycleAttemptKind,
        current_global_policy_revision: AppRevision,
        now: &DateTime<Utc>,
    ) -> Result<Self, AppRegistryError> {
        let expectation =
            approval_expectation(&approval, attempt_kind, current_global_policy_revision);
        let _fence = authorize_installation_approval(authenticated, &approval, expectation, now)?;
        Ok(Self {
            approval,
            attempt_kind,
            current_global_policy_revision,
        })
    }

    pub fn approval_id(&self) -> &AppReference {
        &self.approval.approval_id
    }

    pub fn revision(&self) -> AppRevision {
        self.approval.revision
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppApprovalPublicationOutcome {
    Created,
    AlreadyPresent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppApprovalPublicationReceipt {
    pub approval_id: AppReference,
    pub revision: AppRevision,
    pub attempt_id: AppReference,
    pub installation_id: AppInstallationId,
    pub outcome: AppApprovalPublicationOutcome,
}

/// Exact reviewed revision set. Construction performs the pure decision check;
/// the registry repeats it against the durable approval and current rows inside
/// the compare-and-consume transaction.
#[derive(Debug)]
pub struct AppReviewedInstallationCommit {
    approval: AppInstallationApproval,
    attempt_kind: AppLifecycleAttemptKind,
    grant: AppGrantRevision,
    schema: AppSchemaRevision,
    surface: CompiledAppSurfaceSet,
    event_id: AppReference,
    idempotency_key: AppDigest,
    current_global_policy_revision: AppRevision,
    update_migration_run_id: Option<AppReference>,
    update_plan_digest: Option<AppDigest>,
    update_source_fence_digest: Option<AppDigest>,
}

impl AppReviewedInstallationCommit {
    #[allow(clippy::too_many_arguments)]
    pub fn from_verified_review(
        authenticated: &AuthenticatedAppScope,
        approval: AppInstallationApproval,
        attempt_kind: AppLifecycleAttemptKind,
        grant: AppGrantRevision,
        schema: AppSchemaRevision,
        surface: CompiledAppSurfaceSet,
        event_id: AppReference,
        idempotency_key: AppDigest,
        current_global_policy_revision: AppRevision,
        now: &DateTime<Utc>,
    ) -> Result<Self, AppRegistryError> {
        let limits = AppContractLimits::default();
        grant.validate_app_contract(&limits)?;
        schema.validate_app_contract(&limits)?;
        if surface.surfaces().is_empty() {
            return Err(AppRegistryError::InvalidControlPlane(
                "a reviewed surface generation cannot be empty".to_owned(),
            ));
        }
        if surface.schema_revision() != schema.revision {
            return Err(AppRegistryError::InvalidControlPlane(
                "reviewed surface generation does not name the reviewed schema revision".to_owned(),
            ));
        }
        for member in surface.surfaces().values() {
            member.binding().validate_app_contract(&limits)?;
            if member.binding().status != AppSurfaceStatus::Active {
                return Err(AppRegistryError::InvalidControlPlane(
                    "every reviewed surface generation member must be active".to_owned(),
                ));
            }
        }
        let _fence = authorize_installation_approval(
            authenticated,
            &approval,
            approval_expectation(&approval, attempt_kind, current_global_policy_revision),
            now,
        )?;
        if grant.installation_id != schema.installation_id
            || &grant.installation_id != surface.installation_id()
            || grant.package_revision_ref != schema.package_revision_ref
            || &grant.package_revision_ref != surface.package_revision_ref()
        {
            return Err(AppRegistryError::InvalidControlPlane(
                "reviewed grant, schema and surface do not name one installation/package"
                    .to_owned(),
            ));
        }
        if grant.revoked_at.is_some() {
            return Err(AppRegistryError::InvalidControlPlane(
                "a revoked grant cannot be activated".to_owned(),
            ));
        }
        if grant.authority_digest != approval.granted_authority_digest {
            return Err(AppRegistryError::InvalidControlPlane(
                "reviewed grant does not match the approval authority digest".to_owned(),
            ));
        }
        if grant.granted_interactive_capabilities != approval.interactive_capability_grants {
            return Err(AppRegistryError::InvalidControlPlane(
                "reviewed grant does not match the approval interactive capability matrix"
                    .to_owned(),
            ));
        }
        if grant.approved_by != approval.actor_ref || grant.approved_at != approval.issued_at {
            return Err(AppRegistryError::InvalidControlPlane(
                "reviewed grant is not bound to the approval actor/timestamp".to_owned(),
            ));
        }
        if schema.canonical_data_handling_policy != grant.granted_data_handling_policy {
            return Err(AppRegistryError::InvalidControlPlane(
                "reviewed schema policy does not match the granted data policy".to_owned(),
            ));
        }
        let granted_policy_digest = canonical_contract_digest(&grant.granted_data_handling_policy)?;
        let data_policy_diff_digest = canonical_contract_digest(&(
            &grant.requested_data_handling_policy,
            &grant.granted_data_handling_policy,
        ))?;
        let resource_diff_digest = canonical_contract_digest(&(
            &grant.requested_resource_ceiling,
            &grant.granted_resource_ceiling,
        ))?;
        let schema_diff_digest = canonical_contract_digest(&schema)?;
        let migration_diff_digest = canonical_contract_digest(&(
            schema.compatibility_with_previous,
            &schema.migration_plan_ref,
        ))?;
        if grant.granted_data_handling_policy_digest != granted_policy_digest
            || approval.data_policy_diff_digest != data_policy_diff_digest
            || approval.resource_diff_digest != resource_diff_digest
            || approval.schema_diff_digest != schema_diff_digest
            || approval.migration_diff_digest != migration_diff_digest
        {
            return Err(AppRegistryError::InvalidControlPlane(
                "reviewed decision digests do not match the exact grant/schema revisions"
                    .to_owned(),
            ));
        }
        Ok(Self {
            approval,
            attempt_kind,
            grant,
            schema,
            surface,
            event_id,
            idempotency_key,
            current_global_policy_revision,
            update_migration_run_id: None,
            update_plan_digest: None,
            update_source_fence_digest: None,
        })
    }

    /// Attach server-produced coordinator authority after the ordinary owner
    /// review has been validated. These values are private and cannot be
    /// supplied by a deserialized commit.
    pub fn with_update_authorization(
        mut self,
        run: Option<&super::update::AppDurableUpdateRun>,
    ) -> Result<Self, AppRegistryError> {
        match (self.attempt_kind, run) {
            (AppLifecycleAttemptKind::InitialInstall, None) => Ok(self),
            (AppLifecycleAttemptKind::Update | AppLifecycleAttemptKind::Reinstall, Some(run))
                if run.attempt_kind == self.attempt_kind
                    && run.installation_id == self.grant.installation_id
                    && run.destination_package_revision_ref == self.grant.package_revision_ref
                    && run.destination_schema_revision == self.schema.revision =>
            {
                self.update_migration_run_id = Some(run.migration_run_id.clone());
                self.update_plan_digest = Some(run.update_plan_digest.clone());
                self.update_source_fence_digest = Some(run.source_fence.fence_digest.clone());
                Ok(self)
            },
            _ => Err(AppRegistryError::InvalidControlPlane(
                "reviewed commit is missing or mismatches update coordinator authority".to_owned(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppLifecycleEventKind {
    InstallationEnabled,
    InstallationUpdated,
    InstallationReinstalled,
    InstallationRolledBack,
    InstallationDisabled,
    InstallationReenabled,
    InstallationQuarantined,
    InstallationRetained,
    UpdateBegan,
    UpdateFailed,
    GrantRevoked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppLifecycleOutboxEvent {
    pub event_id: AppReference,
    pub installation_id: AppInstallationId,
    pub package_revision_ref: AppReference,
    pub installation_generation: u64,
    pub event_kind: AppLifecycleEventKind,
    pub lifecycle_status: AppInstallationStatus,
    pub grant_revision: Option<AppRevision>,
    pub schema_revision: Option<AppRevision>,
    pub surface_revision: Option<AppRevision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reenable_review_identity: Option<AppReenableReviewIdentity>,
    pub occurred_at: DateTime<Utc>,
}

impl ValidateAppContract for AppLifecycleOutboxEvent {
    fn validate_app_contract(
        &self,
        _limits: &AppContractLimits,
    ) -> Result<(), super::models::AppContractError> {
        if self.installation_generation == 0 {
            return Err(super::models::AppContractError::invalid(
                "installation_generation",
                "must be greater than zero",
            ));
        }
        let any_revisions = self.grant_revision.is_some()
            || self.schema_revision.is_some()
            || self.surface_revision.is_some();
        let all_revisions = self.grant_revision.is_some()
            && self.schema_revision.is_some()
            && self.surface_revision.is_some();
        match self.lifecycle_status {
            AppInstallationStatus::ReadyForReview if any_revisions => {
                return Err(super::models::AppContractError::invalid(
                    "active_revisions",
                    "ready-for-review lifecycle events cannot expose active revisions",
                ));
            },
            AppInstallationStatus::ReadyForReview => {},
            _ if !all_revisions => {
                return Err(super::models::AppContractError::invalid(
                    "active_revisions",
                    "active post-review lifecycle events require all active revisions",
                ));
            },
            _ => {},
        }
        if self.reenable_review_identity.is_some()
            != (self.event_kind == AppLifecycleEventKind::InstallationReenabled)
        {
            return Err(super::models::AppContractError::invalid(
                "reenable_review_identity",
                "is present only for a reviewed re-enable event",
            ));
        }
        if let Some(identity) = self.reenable_review_identity.as_ref() {
            identity.validate()?;
            if identity.installation_id != self.installation_id
                || identity.installation_generation.saturating_add(1)
                    != self.installation_generation
            {
                return Err(super::models::AppContractError::invalid(
                    "reenable_review_identity",
                    "does not name the immediately preceding installation generation",
                ));
            }
        }
        Ok(())
    }
}

/// Bounded owner-visible identity for one current disabled installation review.
/// It contains digests and non-sensitive package labels, never package bytes,
/// grant payloads, schema fields, routes, task IDs, or retained app data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppReenableReviewIdentity {
    pub installation_id: AppInstallationId,
    pub installation_generation: u64,
    pub package_id: AppReference,
    pub package_version: String,
    pub package_content_digest: AppDigest,
    pub package_lock_digest: AppDigest,
    pub grant_revision: AppRevision,
    pub grant_identity_digest: AppDigest,
    pub schema_revision: AppRevision,
    pub schema_identity_digest: AppDigest,
    pub surface_revision: AppRevision,
    pub surface_identity_digest: AppDigest,
    pub global_policy_revision: AppRevision,
    pub implementation_identity_digest: AppDigest,
    pub review_digest: AppDigest,
}

#[derive(Serialize)]
struct AppReenableReviewMaterial<'a> {
    domain: &'static str,
    installation_id: &'a AppInstallationId,
    installation_generation: u64,
    package_id: &'a AppReference,
    package_version: &'a str,
    package_content_digest: &'a AppDigest,
    package_lock_digest: &'a AppDigest,
    grant_revision: AppRevision,
    grant_identity_digest: &'a AppDigest,
    schema_revision: AppRevision,
    schema_identity_digest: &'a AppDigest,
    surface_revision: AppRevision,
    surface_identity_digest: &'a AppDigest,
    global_policy_revision: AppRevision,
    implementation_identity_digest: &'a AppDigest,
}

impl AppReenableReviewIdentity {
    fn recompute_review_digest(&self) -> Result<AppDigest, super::models::AppContractError> {
        let material = AppReenableReviewMaterial {
            domain: "magician.app.reenable-review.v1",
            installation_id: &self.installation_id,
            installation_generation: self.installation_generation,
            package_id: &self.package_id,
            package_version: &self.package_version,
            package_content_digest: &self.package_content_digest,
            package_lock_digest: &self.package_lock_digest,
            grant_revision: self.grant_revision,
            grant_identity_digest: &self.grant_identity_digest,
            schema_revision: self.schema_revision,
            schema_identity_digest: &self.schema_identity_digest,
            surface_revision: self.surface_revision,
            surface_identity_digest: &self.surface_identity_digest,
            global_policy_revision: self.global_policy_revision,
            implementation_identity_digest: &self.implementation_identity_digest,
        };
        let value = serde_json::to_value(material).map_err(|error| {
            super::models::AppContractError::invalid("review_digest", error.to_string())
        })?;
        AppDigest::blake3_canonical_json(&value).map_err(|error| {
            super::models::AppContractError::invalid("review_digest", error.to_string())
        })
    }

    fn validate(&self) -> Result<(), super::models::AppContractError> {
        if self.installation_generation == 0
            || self.package_version.is_empty()
            || self.package_version.len() > 128
        {
            return Err(super::models::AppContractError::invalid(
                "reenable_review_identity",
                "contains an invalid generation or package version",
            ));
        }
        if self.recompute_review_digest()? != self.review_digest {
            return Err(super::models::AppContractError::invalid(
                "review_digest",
                "does not match the exact re-enable review material",
            ));
        }
        Ok(())
    }
}

/// Move-only proof produced after current source/lock review. Only the review
/// service can construct it; the registry repeats every durable component in
/// the same IMMEDIATE transaction as the lifecycle CAS.
#[derive(Debug)]
pub struct AppReviewedReenableCommit {
    identity: AppReenableReviewIdentity,
    event_id: AppReference,
    idempotency_key: AppDigest,
}

impl AppReviewedReenableCommit {
    pub(crate) fn from_current_review(
        identity: AppReenableReviewIdentity,
        echoed_review_digest: &AppDigest,
        event_id: AppReference,
        idempotency_key: AppDigest,
    ) -> Result<Self, AppRegistryError> {
        if &identity.review_digest != echoed_review_digest {
            return Err(AppRegistryError::StateConflict(
                "re-enable review changed; refresh the exact current review".to_owned(),
            ));
        }
        Ok(Self {
            identity,
            event_id,
            idempotency_key,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppLifecycleMutationOutcome {
    Applied,
    AlreadyApplied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppLifecycleMutationReceipt {
    pub installation_id: AppInstallationId,
    pub generation: u64,
    pub status: AppInstallationStatus,
    pub event_id: AppReference,
    pub reenable_review_identity: Option<AppReenableReviewIdentity>,
    pub outcome: AppLifecycleMutationOutcome,
}

/// Move-only delivery lease. The token is intentionally private and the type
/// has no transport deserializer, preventing a client from fabricating an ack.
#[derive(Debug)]
pub struct AppOutboxLease {
    sequence: i64,
    event: AppLifecycleOutboxEvent,
    lease_owner: AppReference,
    lease_token: AppReference,
    lease_expires_at: DateTime<Utc>,
    attempt_count: u32,
}

impl AppOutboxLease {
    pub fn event(&self) -> &AppLifecycleOutboxEvent {
        &self.event
    }

    pub fn lease_owner(&self) -> &AppReference {
        &self.lease_owner
    }

    pub fn lease_expires_at(&self) -> &DateTime<Utc> {
        &self.lease_expires_at
    }

    pub fn attempt_count(&self) -> u32 {
        self.attempt_count
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppOutboxAcknowledgeOutcome {
    Delivered,
    AlreadyDelivered,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppOutboxAcknowledgeReceipt {
    pub event_id: AppReference,
    pub delivery_receipt_id: AppReference,
    pub outcome: AppOutboxAcknowledgeOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppOutboxRecoveryReceipt {
    pub recovered_leases: usize,
    pub more_expired_leases_may_remain: bool,
}

#[allow(async_fn_in_trait)]
pub trait AppRegistryLifecycleExt {
    async fn publish_installation_approval(
        &self,
        authenticated: &AuthenticatedAppScope,
        publication: AppApprovalPublication,
        now: DateTime<Utc>,
    ) -> Result<AppApprovalPublicationReceipt, AppRegistryError>;
    async fn commit_reviewed_installation(
        &self,
        authenticated: &AuthenticatedAppScope,
        commit: AppReviewedInstallationCommit,
        now: DateTime<Utc>,
    ) -> Result<AppLifecycleMutationReceipt, AppRegistryError>;
    #[allow(clippy::too_many_arguments)]
    async fn transition_installation(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: AppInstallationId,
        expected_generation: u64,
        command: AppInstallationCommand,
        event_id: AppReference,
        idempotency_key: AppDigest,
        now: DateTime<Utc>,
    ) -> Result<AppLifecycleMutationReceipt, AppRegistryError>;
    async fn reenable_review_identity(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        implementation_identity_digest: AppDigest,
        current_global_policy_revision: AppRevision,
        now: DateTime<Utc>,
    ) -> Result<AppReenableReviewIdentity, AppRegistryError>;
    async fn commit_reviewed_reenable(
        &self,
        authenticated: &AuthenticatedAppScope,
        commit: AppReviewedReenableCommit,
        now: DateTime<Utc>,
    ) -> Result<AppLifecycleMutationReceipt, AppRegistryError>;
    #[allow(clippy::too_many_arguments)]
    async fn reviewed_reenable_replay(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        expected_generation: u64,
        review_digest: &AppDigest,
        event_id: &AppReference,
        idempotency_key: &AppDigest,
        now: DateTime<Utc>,
    ) -> Result<Option<AppLifecycleMutationReceipt>, AppRegistryError>;
    #[allow(clippy::too_many_arguments)]
    async fn revoke_active_grant(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: AppInstallationId,
        expected_generation: u64,
        expected_grant_revision: AppRevision,
        event_id: AppReference,
        idempotency_key: AppDigest,
        now: DateTime<Utc>,
    ) -> Result<AppLifecycleMutationReceipt, AppRegistryError>;
    async fn claim_lifecycle_outbox(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease_owner: AppReference,
        limit: usize,
        lease_duration: StdDuration,
        now: DateTime<Utc>,
    ) -> Result<Vec<AppOutboxLease>, AppRegistryError>;
    async fn acknowledge_lifecycle_outbox(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease: AppOutboxLease,
        delivery_receipt_id: AppReference,
        now: DateTime<Utc>,
    ) -> Result<AppOutboxAcknowledgeReceipt, AppRegistryError>;
    async fn release_lifecycle_outbox(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease: AppOutboxLease,
        retry_delay: StdDuration,
        now: DateTime<Utc>,
    ) -> Result<(), AppRegistryError>;
    async fn recover_expired_lifecycle_outbox_leases(
        &self,
        authenticated: &AuthenticatedAppScope,
        limit: usize,
        now: DateTime<Utc>,
    ) -> Result<AppOutboxRecoveryReceipt, AppRegistryError>;
}

impl AppRegistryLifecycleExt for AppRegistryService {
    async fn publish_installation_approval(
        &self,
        authenticated: &AuthenticatedAppScope,
        publication: AppApprovalPublication,
        now: DateTime<Utc>,
    ) -> Result<AppApprovalPublicationReceipt, AppRegistryError> {
        let operation_authentication = authenticated.clone();
        self.execute_scoped_write(authenticated, &now, move |connection, scope| {
            publish_approval_blocking(
                connection,
                scope,
                &operation_authentication,
                publication,
                &now,
            )
        })
        .await
    }

    async fn commit_reviewed_installation(
        &self,
        authenticated: &AuthenticatedAppScope,
        commit: AppReviewedInstallationCommit,
        now: DateTime<Utc>,
    ) -> Result<AppLifecycleMutationReceipt, AppRegistryError> {
        let operation_authentication = authenticated.clone();
        let receipt = self
            .execute_scoped_write(authenticated, &now, move |connection, scope| {
                commit_reviewed_blocking(connection, scope, &operation_authentication, commit, &now)
            })
            .await?;
        self.hide_computed_capability_scope(authenticated);
        self.tombstone_app_memory_index_projection_for_scope(
            authenticated,
            &format!(
                "installation-lifecycle:{}:{}:{}",
                receipt.installation_id.as_str(),
                receipt.generation,
                receipt.event_id.as_str(),
            ),
        )
        .await
        .map_err(registry_memory_store_error)?;
        Ok(receipt)
    }

    #[allow(clippy::too_many_arguments)]
    async fn transition_installation(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: AppInstallationId,
        expected_generation: u64,
        command: AppInstallationCommand,
        event_id: AppReference,
        idempotency_key: AppDigest,
        now: DateTime<Utc>,
    ) -> Result<AppLifecycleMutationReceipt, AppRegistryError> {
        if matches!(
            command,
            AppInstallationCommand::EnableReviewed
                | AppInstallationCommand::Enable
                | AppInstallationCommand::ReenableReviewed
                | AppInstallationCommand::CommitUpdate
                | AppInstallationCommand::CommitReviewedReinstall
                | AppInstallationCommand::Purge
        ) {
            return Err(AppRegistryError::InvalidControlPlane(
                "reviewed and purge transitions require their dedicated evidence boundary"
                    .to_owned(),
            ));
        }
        let receipt = self
            .execute_scoped_write(authenticated, &now, move |connection, scope| {
                transition_installation_blocking(
                    connection,
                    scope,
                    installation_id,
                    expected_generation,
                    command,
                    event_id,
                    idempotency_key,
                    &now,
                )
            })
            .await?;
        self.hide_computed_capability_scope(authenticated);
        self.tombstone_app_memory_index_projection_for_scope(
            authenticated,
            &format!(
                "installation-lifecycle:{}:{}:{}",
                receipt.installation_id.as_str(),
                receipt.generation,
                receipt.event_id.as_str(),
            ),
        )
        .await
        .map_err(registry_memory_store_error)?;
        Ok(receipt)
    }

    async fn reenable_review_identity(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        implementation_identity_digest: AppDigest,
        current_global_policy_revision: AppRevision,
        now: DateTime<Utc>,
    ) -> Result<AppReenableReviewIdentity, AppRegistryError> {
        let installation_id = installation_id.clone();
        self.execute_scoped_read(authenticated, &now, move |connection, scope| {
            build_reenable_review_identity(
                connection,
                scope,
                &installation_id,
                &implementation_identity_digest,
                current_global_policy_revision,
            )
        })
        .await?
        .ok_or_else(|| AppRegistryError::MissingRecord {
            entity: "app registry scope",
            identity: authenticated.scope_binding_ref().to_string(),
        })
    }

    async fn commit_reviewed_reenable(
        &self,
        authenticated: &AuthenticatedAppScope,
        commit: AppReviewedReenableCommit,
        now: DateTime<Utc>,
    ) -> Result<AppLifecycleMutationReceipt, AppRegistryError> {
        let receipt = self
            .execute_scoped_write(authenticated, &now, move |connection, scope| {
                commit_reviewed_reenable_blocking(connection, scope, commit, &now)
            })
            .await?;
        self.hide_computed_capability_scope(authenticated);
        self.tombstone_app_memory_index_projection_for_scope(
            authenticated,
            &format!(
                "installation-lifecycle:{}:{}:{}",
                receipt.installation_id.as_str(),
                receipt.generation,
                receipt.event_id.as_str(),
            ),
        )
        .await
        .map_err(registry_memory_store_error)?;
        Ok(receipt)
    }

    #[allow(clippy::too_many_arguments)]
    async fn reviewed_reenable_replay(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        expected_generation: u64,
        review_digest: &AppDigest,
        event_id: &AppReference,
        idempotency_key: &AppDigest,
        now: DateTime<Utc>,
    ) -> Result<Option<AppLifecycleMutationReceipt>, AppRegistryError> {
        let installation_id = installation_id.clone();
        let review_digest = review_digest.clone();
        let event_id = event_id.clone();
        let idempotency_key = idempotency_key.clone();
        Ok(self
            .execute_scoped_read(authenticated, &now, move |connection, scope| {
                let installation = load_installation(connection, &installation_id)?;
                ensure_installation_scope(&installation, scope)?;
                let stored = connection
                    .query_row(
                        "SELECT idempotency_key, payload_json FROM app_lifecycle_outbox WHERE \
                         event_id = ?1",
                        params![event_id.as_str()],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
                    )
                    .optional()?;
                let Some((stored_key, payload)) = stored else {
                    return Ok(None);
                };
                if stored_key != idempotency_key.as_str() {
                    return Err(AppRegistryError::StateConflict(
                        "re-enable replay idempotency identity changed".to_owned(),
                    ));
                }
                let event: AppLifecycleOutboxEvent =
                    decode_app_contract(&payload, &AppContractLimits::default())?;
                let identity = event.reenable_review_identity.clone().ok_or_else(|| {
                    AppRegistryError::StateConflict(
                        "re-enable replay is missing its reviewed identity".to_owned(),
                    )
                })?;
                if installation.lifecycle.status != AppInstallationStatus::Enabled
                    || installation.lifecycle.generation != expected_generation.saturating_add(1)
                    || event.installation_id != installation_id
                    || event.installation_generation != installation.lifecycle.generation
                    || event.event_kind != AppLifecycleEventKind::InstallationReenabled
                    || identity.installation_generation != expected_generation
                    || identity.review_digest != review_digest
                {
                    return Err(AppRegistryError::StateConflict(
                        "re-enable replay does not match the exact current installation transition"
                            .to_owned(),
                    ));
                }
                Ok(Some(AppLifecycleMutationReceipt {
                    installation_id,
                    generation: installation.lifecycle.generation,
                    status: installation.lifecycle.status,
                    event_id,
                    reenable_review_identity: Some(identity),
                    outcome: AppLifecycleMutationOutcome::AlreadyApplied,
                }))
            })
            .await?
            .flatten())
    }

    #[allow(clippy::too_many_arguments)]
    async fn revoke_active_grant(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: AppInstallationId,
        expected_generation: u64,
        expected_grant_revision: AppRevision,
        event_id: AppReference,
        idempotency_key: AppDigest,
        now: DateTime<Utc>,
    ) -> Result<AppLifecycleMutationReceipt, AppRegistryError> {
        let receipt = self
            .execute_scoped_write(authenticated, &now, move |connection, scope| {
                revoke_active_grant_blocking(
                    connection,
                    scope,
                    installation_id,
                    expected_generation,
                    expected_grant_revision,
                    event_id,
                    idempotency_key,
                    &now,
                )
            })
            .await?;
        self.hide_computed_capability_scope(authenticated);
        self.tombstone_app_memory_index_projection_for_scope(
            authenticated,
            &format!(
                "installation-lifecycle:{}:{}:{}",
                receipt.installation_id.as_str(),
                receipt.generation,
                receipt.event_id.as_str(),
            ),
        )
        .await
        .map_err(registry_memory_store_error)?;
        Ok(receipt)
    }

    async fn claim_lifecycle_outbox(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease_owner: AppReference,
        limit: usize,
        lease_duration: StdDuration,
        now: DateTime<Utc>,
    ) -> Result<Vec<AppOutboxLease>, AppRegistryError> {
        if limit == 0 || limit > MAX_OUTBOX_CLAIM_BATCH {
            return Err(AppRegistryError::InvalidControlPlane(format!(
                "outbox claim limit must be between 1 and {MAX_OUTBOX_CLAIM_BATCH}"
            )));
        }
        if lease_duration.is_zero() || lease_duration > MAX_OUTBOX_LEASE {
            return Err(AppRegistryError::InvalidControlPlane(
                "outbox lease must be between one second and five minutes".to_owned(),
            ));
        }
        self.execute_scoped_background_write(authenticated, &now, move |connection, _scope| {
            claim_outbox_blocking(connection, lease_owner, limit, lease_duration, &now)
        })
        .await
    }

    async fn acknowledge_lifecycle_outbox(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease: AppOutboxLease,
        delivery_receipt_id: AppReference,
        now: DateTime<Utc>,
    ) -> Result<AppOutboxAcknowledgeReceipt, AppRegistryError> {
        self.execute_scoped_background_write(authenticated, &now, move |connection, _scope| {
            acknowledge_outbox_blocking(connection, lease, delivery_receipt_id, &now)
        })
        .await
    }

    async fn release_lifecycle_outbox(
        &self,
        authenticated: &AuthenticatedAppScope,
        lease: AppOutboxLease,
        retry_delay: StdDuration,
        now: DateTime<Utc>,
    ) -> Result<(), AppRegistryError> {
        if retry_delay > StdDuration::from_secs(3_600) {
            return Err(AppRegistryError::InvalidControlPlane(
                "outbox retry delay cannot exceed one hour".to_owned(),
            ));
        }
        self.execute_scoped_background_write(authenticated, &now, move |connection, _scope| {
            release_outbox_blocking(connection, lease, retry_delay, &now)
        })
        .await
    }

    /// Explicit bounded recovery; this is never called by service construction
    /// and therefore cannot turn startup into a scope-wide scan.
    async fn recover_expired_lifecycle_outbox_leases(
        &self,
        authenticated: &AuthenticatedAppScope,
        limit: usize,
        now: DateTime<Utc>,
    ) -> Result<AppOutboxRecoveryReceipt, AppRegistryError> {
        if limit == 0 || limit > MAX_OUTBOX_RECOVERY_BATCH {
            return Err(AppRegistryError::InvalidControlPlane(format!(
                "outbox recovery limit must be between 1 and {MAX_OUTBOX_RECOVERY_BATCH}"
            )));
        }
        self.execute_scoped_background_write(authenticated, &now, move |connection, _scope| {
            recover_outbox_blocking(connection, limit, &now)
        })
        .await
    }
}

fn publish_approval_blocking(
    connection: &mut rusqlite::Connection,
    scope: &AppScope,
    authenticated: &AuthenticatedAppScope,
    publication: AppApprovalPublication,
    now: &DateTime<Utc>,
) -> Result<AppApprovalPublicationReceipt, AppRegistryError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (attempt, installation_id) = load_attempt_and_installation_id(
        &transaction,
        &publication.approval.install_or_update_attempt_id,
    )?;
    if attempt.kind != publication.attempt_kind
        || attempt.state != AppLifecycleAttemptState::ReadyForReview
    {
        return Err(AppRegistryError::StateConflict(
            "approval does not target a ready-for-review attempt".to_owned(),
        ));
    }
    let package = load_package(&transaction, &attempt.candidate_package_revision_ref)?;
    let installation = load_installation(&transaction, &installation_id)?;
    ensure_installation_scope(&installation, scope)?;
    validate_review_target(&attempt, &installation, publication.attempt_kind)?;
    if installation.package_revision_ref != attempt.candidate_package_revision_ref
        && publication.attempt_kind == AppLifecycleAttemptKind::InitialInstall
    {
        return Err(AppRegistryError::StateConflict(
            "initial attempt and proposed installation package differ".to_owned(),
        ));
    }
    if publication.approval.package_content_digest != package.content_digest
        || publication.approval.requested_authority_digest != package.requested_authority_digest
    {
        return Err(AppRegistryError::StateConflict(
            "approval does not match the durable package decision inputs".to_owned(),
        ));
    }
    let _fence = authorize_installation_approval(
        authenticated,
        &publication.approval,
        approval_expectation(
            &publication.approval,
            publication.attempt_kind,
            publication.current_global_policy_revision,
        ),
        now,
    )?;
    let approval_json = encode_bounded_json(&publication.approval, &AppContractLimits::default())?;
    let inserted = transaction.execute(
        "INSERT INTO app_installation_approvals (
             approval_id, revision, attempt_id, package_content_digest,
             requested_authority_digest, granted_authority_digest, session_ref,
             authentication_revision, expires_at, consumed_at,
             consumed_installation_revision, record_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, NULL, ?10, ?11)
         ON CONFLICT(approval_id, revision) DO NOTHING",
        params![
            publication.approval.approval_id.as_str(),
            revision_i64(publication.approval.revision)?,
            publication.approval.install_or_update_attempt_id.as_str(),
            publication.approval.package_content_digest.as_str(),
            publication.approval.requested_authority_digest.as_str(),
            publication.approval.granted_authority_digest.as_str(),
            publication.approval.session_ref.as_str(),
            revision_i64(publication.approval.authentication_revision)?,
            format_timestamp(&publication.approval.expires_at),
            approval_json,
            format_timestamp(&publication.approval.issued_at),
        ],
    )?;
    let stored: Option<Vec<u8>> = transaction
        .query_row(
            "SELECT record_json FROM app_installation_approvals
             WHERE approval_id = ?1 AND revision = ?2",
            params![
                publication.approval.approval_id.as_str(),
                revision_i64(publication.approval.revision)?
            ],
            |row| row.get(0),
        )
        .optional()?;
    if stored.as_deref() != Some(approval_json.as_slice()) {
        return Err(AppRegistryError::IdentityConflict {
            entity: "installation approval",
            identity: publication.approval.approval_id.to_string(),
        });
    }
    transaction.commit()?;
    Ok(AppApprovalPublicationReceipt {
        approval_id: publication.approval.approval_id,
        revision: publication.approval.revision,
        attempt_id: publication.approval.install_or_update_attempt_id,
        installation_id,
        outcome: if inserted == 0 {
            AppApprovalPublicationOutcome::AlreadyPresent
        } else {
            AppApprovalPublicationOutcome::Created
        },
    })
}

fn commit_reviewed_blocking(
    connection: &mut rusqlite::Connection,
    scope: &AppScope,
    authenticated: &AuthenticatedAppScope,
    commit: AppReviewedInstallationCommit,
    now: &DateTime<Utc>,
) -> Result<AppLifecycleMutationReceipt, AppRegistryError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let durable_approval = load_approval(
        &transaction,
        &commit.approval.approval_id,
        commit.approval.revision,
    )?;
    if !same_approval_decision(&durable_approval, &commit.approval) {
        return Err(AppRegistryError::StateConflict(
            "reviewed commit does not match the durable approval revision".to_owned(),
        ));
    }
    let (attempt, installation_id) = load_attempt_and_installation_id(
        &transaction,
        &durable_approval.install_or_update_attempt_id,
    )?;
    let package = load_package(&transaction, &attempt.candidate_package_revision_ref)?;
    let installation = load_installation(&transaction, &installation_id)?;
    ensure_installation_scope(&installation, scope)?;

    if durable_approval.consumed_at.is_some() {
        return verify_reviewed_replay(
            &transaction,
            &commit,
            &durable_approval,
            &attempt,
            &installation,
        );
    }
    validate_review_target(&attempt, &installation, commit.attempt_kind)?;
    // Candidate publication snapshots data_policy + entities + workflows into
    // `requested_data_policy_digest`. That is not the digest of the grant's
    // `requested_data_handling_policy` object. Schema compilation already
    // binds the granted policy to the reviewed schema.
    if commit.grant.installation_id != installation_id
        || commit.grant.package_revision_ref != attempt.candidate_package_revision_ref
        || package.content_digest != durable_approval.package_content_digest
        || package.requested_authority_digest != durable_approval.requested_authority_digest
        || commit.grant.authority_digest != durable_approval.granted_authority_digest
    {
        return Err(AppRegistryError::StateConflict(
            "reviewed commit no longer matches the current attempt/package/installation".to_owned(),
        ));
    }
    validate_revision_successors(&installation, &commit)?;
    let expectation = AppInstallationApprovalExpectation {
        approval_id: &durable_approval.approval_id,
        approval_revision: durable_approval.revision,
        attempt_id: &attempt.attempt_id,
        attempt_kind: attempt.kind,
        package_content_digest: &package.content_digest,
        requested_authority_digest: &package.requested_authority_digest,
        granted_authority_digest: &commit.grant.authority_digest,
        data_policy_diff_digest: &durable_approval.data_policy_diff_digest,
        resource_diff_digest: &durable_approval.resource_diff_digest,
        schema_diff_digest: &durable_approval.schema_diff_digest,
        migration_diff_digest: &durable_approval.migration_diff_digest,
        global_policy_revision: commit.current_global_policy_revision,
    };
    let fence =
        authorize_installation_approval(authenticated, &durable_approval, expectation, now)?;
    let command = reviewed_command(attempt.kind);
    let lifecycle =
        apply_reviewed_installation_transition(&installation.lifecycle, command, fence)?;
    let mut next_installation = installation.clone();
    next_installation.package_revision_ref = attempt.candidate_package_revision_ref.clone();
    next_installation.lifecycle = lifecycle;
    next_installation.grant_revision = Some(commit.grant.revision);
    next_installation.active_schema_revision = Some(commit.schema.revision);
    next_installation.active_surface_revision = Some(commit.surface.surface_revision());
    next_installation.updated_at = *now;
    apply_reviewed_timestamps(attempt.kind, &mut next_installation);
    next_installation.validate_app_contract(&AppContractLimits::default())?;

    let mut next_attempt = attempt.clone();
    next_attempt.state = reduce_attempt(attempt.state, AppLifecycleAttemptCommand::Commit)?;
    next_attempt.approval_ref = Some(durable_approval.approval_id.clone());
    next_attempt.updated_at = *now;
    next_attempt.validate_app_contract(&AppContractLimits::default())?;

    let mut consumed_approval = durable_approval.clone();
    consumed_approval.consumed_at = Some(*now);
    consumed_approval.consumed_installation_revision =
        Some(AppRevision::new(next_installation.lifecycle.generation)?);
    consumed_approval.validate_app_contract(&AppContractLimits::default())?;

    insert_revision_set(&transaction, &commit, now)?;
    super::update::promote_staged_update_in_transaction(
        &transaction,
        scope,
        &installation,
        &attempt,
        &commit.grant,
        &commit.schema,
        commit.surface.surface_revision(),
        commit.update_migration_run_id.as_ref(),
        commit.update_plan_digest.as_ref(),
        commit.update_source_fence_digest.as_ref(),
        *now,
    )
    .map_err(|error| {
        AppRegistryError::StateConflict(format!("update coordinator switch denied: {error}"))
    })?;
    update_installation_cas(&transaction, &installation, &next_installation)?;
    update_attempt_cas(&transaction, &attempt, &next_attempt)?;
    consume_approval_cas(&transaction, &durable_approval, &consumed_approval)?;
    let event_kind = reviewed_event_kind(attempt.kind);
    let event = lifecycle_event(
        commit.event_id.clone(),
        &next_installation,
        event_kind,
        *now,
    );
    insert_outbox_event(&transaction, &event, &commit.idempotency_key, now)?;
    if matches!(
        attempt.kind,
        AppLifecycleAttemptKind::Update | AppLifecycleAttemptKind::Reinstall
    ) {
        append_installation_contribution_invalidations(
            &transaction,
            scope,
            installation_id.as_str(),
            AppMemoryInvalidationReasonV1::PolicyChanged,
            now,
        )?;
    }
    transaction.commit()?;
    Ok(AppLifecycleMutationReceipt {
        installation_id,
        generation: next_installation.lifecycle.generation,
        status: next_installation.lifecycle.status,
        event_id: commit.event_id,
        reenable_review_identity: None,
        outcome: AppLifecycleMutationOutcome::Applied,
    })
}

#[allow(clippy::too_many_arguments)]
fn transition_installation_blocking(
    connection: &mut rusqlite::Connection,
    scope: &AppScope,
    installation_id: AppInstallationId,
    expected_generation: u64,
    command: AppInstallationCommand,
    event_id: AppReference,
    idempotency_key: AppDigest,
    now: &DateTime<Utc>,
) -> Result<AppLifecycleMutationReceipt, AppRegistryError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let installation = load_installation(&transaction, &installation_id)?;
    ensure_installation_scope(&installation, scope)?;
    if installation.lifecycle.generation != expected_generation {
        if installation.lifecycle.generation == expected_generation.saturating_add(1) {
            let event_kind = ordinary_event_kind(command)?;
            let event = lifecycle_event(
                event_id.clone(),
                &installation,
                event_kind,
                installation.updated_at,
            );
            if verify_outbox_identity(&transaction, &event, &idempotency_key)? {
                return Ok(AppLifecycleMutationReceipt {
                    installation_id,
                    generation: installation.lifecycle.generation,
                    status: installation.lifecycle.status,
                    event_id,
                    reenable_review_identity: None,
                    outcome: AppLifecycleMutationOutcome::AlreadyApplied,
                });
            }
        }
        return Err(AppRegistryError::GenerationConflict {
            expected: expected_generation,
            actual: installation.lifecycle.generation,
        });
    }
    let mut next = installation.clone();
    next.lifecycle = installation.lifecycle.apply(command)?;
    next.updated_at = *now;
    apply_lifecycle_timestamps(&mut next, command, *now);
    next.validate_app_contract(&AppContractLimits::default())?;
    let event_kind = ordinary_event_kind(command)?;
    let event = lifecycle_event(event_id.clone(), &next, event_kind, *now);
    update_installation_cas(&transaction, &installation, &next)?;
    insert_outbox_event(&transaction, &event, &idempotency_key, now)?;
    let contribution_invalidation_reason = match command {
        AppInstallationCommand::Disable => {
            Some(AppMemoryInvalidationReasonV1::InstallationDisabled)
        },
        AppInstallationCommand::Quarantine => {
            Some(AppMemoryInvalidationReasonV1::InstallationQuarantined)
        },
        AppInstallationCommand::UninstallRetain => {
            Some(AppMemoryInvalidationReasonV1::InstallationUninstalledRetained)
        },
        _ => None,
    };
    if let Some(reason) = contribution_invalidation_reason {
        append_installation_contribution_invalidations(
            &transaction,
            scope,
            installation_id.as_str(),
            reason,
            now,
        )?;
    }
    if matches!(
        command,
        AppInstallationCommand::Disable
            | AppInstallationCommand::UninstallRetain
            | AppInstallationCommand::Quarantine
    ) {
        settle_candidates_for_installation(&transaction, scope, &installation_id, *now)
            .map_err(registry_memory_store_error)?;
    }
    transaction.commit()?;
    Ok(AppLifecycleMutationReceipt {
        installation_id,
        generation: next.lifecycle.generation,
        status: next.lifecycle.status,
        event_id,
        reenable_review_identity: None,
        outcome: AppLifecycleMutationOutcome::Applied,
    })
}

fn build_reenable_review_identity(
    connection: &rusqlite::Connection,
    scope: &AppScope,
    installation_id: &AppInstallationId,
    implementation_identity_digest: &AppDigest,
    current_global_policy_revision: AppRevision,
) -> Result<AppReenableReviewIdentity, AppRegistryError> {
    let installation: AppInstallation = load_record(
        connection,
        "SELECT record_json FROM app_installations WHERE installation_id = ?1",
        installation_id.as_str(),
        "installation",
    )?;
    ensure_installation_scope(&installation, scope)?;
    if installation.lifecycle.status != AppInstallationStatus::Disabled {
        return Err(AppRegistryError::StateConflict(
            "only a disabled installation can receive a re-enable review".to_owned(),
        ));
    }
    let package: AppPackageRevision = load_record(
        connection,
        "SELECT record_json FROM app_package_revisions WHERE package_revision_ref = ?1",
        installation.package_revision_ref.as_str(),
        "package revision",
    )?;
    let lock_bytes: Vec<u8> = connection.query_row(
        "SELECT dependency_lock_json FROM app_package_revisions WHERE package_revision_ref = ?1",
        params![installation.package_revision_ref.as_str()],
        |row| row.get(0),
    )?;
    let package_lock = super::package_lock::decode_persisted_package_lock(&lock_bytes)?;
    if package_lock.lock_digest() != &package.dependency_lock_digest {
        return Err(AppRegistryError::StateConflict(
            "current package lock no longer matches the installed package revision".to_owned(),
        ));
    }
    let grant_revision = installation.grant_revision.ok_or_else(|| {
        AppRegistryError::StateConflict("disabled installation has no active grant".to_owned())
    })?;
    let grant: AppGrantRevision = load_revision_record(
        connection,
        "app_grant_revisions",
        installation_id,
        grant_revision,
        "grant revision",
    )?;
    if grant.package_revision_ref != installation.package_revision_ref || grant.revoked_at.is_some()
    {
        return Err(AppRegistryError::StateConflict(
            "disabled installation grant is revoked or names another package".to_owned(),
        ));
    }
    let schema_revision = installation.active_schema_revision.ok_or_else(|| {
        AppRegistryError::StateConflict("disabled installation has no active schema".to_owned())
    })?;
    let schema: AppSchemaRevision = load_revision_record(
        connection,
        "app_schema_revisions",
        installation_id,
        schema_revision,
        "schema revision",
    )?;
    if schema.package_revision_ref != installation.package_revision_ref {
        return Err(AppRegistryError::StateConflict(
            "disabled installation schema names another package".to_owned(),
        ));
    }
    let surface_revision = installation.active_surface_revision.ok_or_else(|| {
        AppRegistryError::StateConflict("disabled installation has no active surface".to_owned())
    })?;
    let (surface_package, surface_schema, surface_digest, member_count): (
        String,
        i64,
        String,
        i64,
    ) = connection.query_row(
        "SELECT package_revision_ref, schema_revision, compiled_set_digest, member_count
           FROM app_surface_generations WHERE installation_id = ?1 AND revision = ?2",
        params![installation_id.as_str(), revision_i64(surface_revision)?],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let actual_members: i64 = connection.query_row(
        "SELECT COUNT(*) FROM app_surface_generation_members
          WHERE installation_id = ?1 AND revision = ?2",
        params![installation_id.as_str(), revision_i64(surface_revision)?],
        |row| row.get(0),
    )?;
    if surface_package != installation.package_revision_ref.as_str()
        || surface_schema != revision_i64(schema_revision)?
        || member_count <= 0
        || member_count != actual_members
    {
        return Err(AppRegistryError::StateConflict(
            "disabled installation surface generation is incomplete or stale".to_owned(),
        ));
    }
    let grant_identity_digest = canonical_contract_digest(&grant)?;
    let schema_identity_digest = canonical_contract_digest(&schema)?;
    let surface_identity_digest = AppDigest::parse(surface_digest)?;
    let material = AppReenableReviewMaterial {
        domain: "magician.app.reenable-review.v1",
        installation_id,
        installation_generation: installation.lifecycle.generation,
        package_id: &package.package_id,
        package_version: &package.semantic_version,
        package_content_digest: &package.content_digest,
        package_lock_digest: package_lock.lock_digest(),
        grant_revision,
        grant_identity_digest: &grant_identity_digest,
        schema_revision,
        schema_identity_digest: &schema_identity_digest,
        surface_revision,
        surface_identity_digest: &surface_identity_digest,
        global_policy_revision: current_global_policy_revision,
        implementation_identity_digest,
    };
    let review_digest = canonical_contract_digest(&material)?;
    Ok(AppReenableReviewIdentity {
        installation_id: installation_id.clone(),
        installation_generation: installation.lifecycle.generation,
        package_id: package.package_id,
        package_version: package.semantic_version,
        package_content_digest: package.content_digest,
        package_lock_digest: package_lock.lock_digest().clone(),
        grant_revision,
        grant_identity_digest,
        schema_revision,
        schema_identity_digest,
        surface_revision,
        surface_identity_digest,
        global_policy_revision: current_global_policy_revision,
        implementation_identity_digest: implementation_identity_digest.clone(),
        review_digest,
    })
}

fn load_revision_record<T>(
    connection: &rusqlite::Connection,
    table: &'static str,
    installation_id: &AppInstallationId,
    revision: AppRevision,
    entity: &'static str,
) -> Result<T, AppRegistryError>
where
    T: ValidateAppContract + serde::de::DeserializeOwned,
{
    let sql = match table {
        "app_grant_revisions" => {
            "SELECT record_json FROM app_grant_revisions WHERE installation_id = ?1 AND revision = \
             ?2"
        },
        "app_schema_revisions" => {
            "SELECT record_json FROM app_schema_revisions WHERE installation_id = ?1 AND revision \
             = ?2"
        },
        _ => {
            return Err(AppRegistryError::InvalidControlPlane(
                "unsupported reviewed revision table".to_owned(),
            ))
        },
    };
    let bytes: Option<Vec<u8>> = connection
        .query_row(
            sql,
            params![installation_id.as_str(), revision_i64(revision)?],
            |row| row.get(0),
        )
        .optional()?;
    decode_required(bytes, entity, installation_id.as_str())
}

fn commit_reviewed_reenable_blocking(
    connection: &mut rusqlite::Connection,
    scope: &AppScope,
    commit: AppReviewedReenableCommit,
    now: &DateTime<Utc>,
) -> Result<AppLifecycleMutationReceipt, AppRegistryError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let installation = load_installation(&transaction, &commit.identity.installation_id)?;
    ensure_installation_scope(&installation, scope)?;
    if installation.lifecycle.generation != commit.identity.installation_generation {
        if installation.lifecycle.generation
            == commit.identity.installation_generation.saturating_add(1)
            && installation.lifecycle.status == AppInstallationStatus::Enabled
        {
            let stored = load_outbox_event_for_replay(
                &transaction,
                &commit.event_id,
                &commit.idempotency_key,
            )?;
            if stored.installation_id == installation.installation_id
                && stored.installation_generation == installation.lifecycle.generation
                && stored.event_kind == AppLifecycleEventKind::InstallationReenabled
                && stored.lifecycle_status == AppInstallationStatus::Enabled
                && stored.reenable_review_identity.as_ref() == Some(&commit.identity)
            {
                return Ok(AppLifecycleMutationReceipt {
                    installation_id: installation.installation_id,
                    generation: installation.lifecycle.generation,
                    status: installation.lifecycle.status,
                    event_id: commit.event_id,
                    reenable_review_identity: Some(commit.identity),
                    outcome: AppLifecycleMutationOutcome::AlreadyApplied,
                });
            }
        }
        return Err(AppRegistryError::GenerationConflict {
            expected: commit.identity.installation_generation,
            actual: installation.lifecycle.generation,
        });
    }
    let current = build_reenable_review_identity(
        &transaction,
        scope,
        &installation.installation_id,
        &commit.identity.implementation_identity_digest,
        commit.identity.global_policy_revision,
    )?;
    if current != commit.identity {
        return Err(AppRegistryError::StateConflict(
            "re-enable review changed immediately before commit".to_owned(),
        ));
    }
    let mut next = installation.clone();
    next.lifecycle = installation
        .lifecycle
        .apply(AppInstallationCommand::ReenableReviewed)?;
    next.updated_at = *now;
    next.disabled_at = None;
    next.validate_app_contract(&AppContractLimits::default())?;
    let mut event = lifecycle_event(
        commit.event_id.clone(),
        &next,
        AppLifecycleEventKind::InstallationReenabled,
        *now,
    );
    event.reenable_review_identity = Some(commit.identity.clone());
    update_installation_cas(&transaction, &installation, &next)?;
    insert_outbox_event(&transaction, &event, &commit.idempotency_key, now)?;
    transaction.commit()?;
    Ok(AppLifecycleMutationReceipt {
        installation_id: next.installation_id,
        generation: next.lifecycle.generation,
        status: next.lifecycle.status,
        event_id: commit.event_id,
        reenable_review_identity: Some(commit.identity),
        outcome: AppLifecycleMutationOutcome::Applied,
    })
}

fn load_outbox_event_for_replay(
    transaction: &Transaction<'_>,
    event_id: &AppReference,
    idempotency_key: &AppDigest,
) -> Result<AppLifecycleOutboxEvent, AppRegistryError> {
    let stored: Option<(String, Vec<u8>)> = transaction
        .query_row(
            "SELECT idempotency_key, payload_json FROM app_lifecycle_outbox WHERE event_id = ?1",
            params![event_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((stored_key, payload)) = stored else {
        return Err(AppRegistryError::StateConflict(
            "re-enable replay is missing its exact lifecycle event".to_owned(),
        ));
    };
    if stored_key != idempotency_key.as_str() {
        return Err(AppRegistryError::StateConflict(
            "re-enable replay idempotency identity changed".to_owned(),
        ));
    }
    decode_app_contract(&payload, &AppContractLimits::default()).map_err(Into::into)
}

#[allow(clippy::too_many_arguments)]
fn revoke_active_grant_blocking(
    connection: &mut rusqlite::Connection,
    scope: &AppScope,
    installation_id: AppInstallationId,
    expected_generation: u64,
    expected_grant_revision: AppRevision,
    event_id: AppReference,
    idempotency_key: AppDigest,
    now: &DateTime<Utc>,
) -> Result<AppLifecycleMutationReceipt, AppRegistryError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let installation = load_installation(&transaction, &installation_id)?;
    ensure_installation_scope(&installation, scope)?;
    if installation.grant_revision != Some(expected_grant_revision) {
        return Err(AppRegistryError::StateConflict(
            "active grant revision changed before revocation".to_owned(),
        ));
    }
    if installation.lifecycle.generation != expected_generation {
        if installation.lifecycle.generation == expected_generation.saturating_add(1)
            && installation.lifecycle.status == AppInstallationStatus::Quarantined
        {
            let event = lifecycle_event(
                event_id.clone(),
                &installation,
                AppLifecycleEventKind::GrantRevoked,
                installation.updated_at,
            );
            if verify_outbox_identity(&transaction, &event, &idempotency_key)? {
                return Ok(AppLifecycleMutationReceipt {
                    installation_id,
                    generation: installation.lifecycle.generation,
                    status: installation.lifecycle.status,
                    event_id,
                    reenable_review_identity: None,
                    outcome: AppLifecycleMutationOutcome::AlreadyApplied,
                });
            }
        }
        return Err(AppRegistryError::GenerationConflict {
            expected: expected_generation,
            actual: installation.lifecycle.generation,
        });
    }
    let grant_json: Vec<u8> = transaction.query_row(
        "SELECT record_json FROM app_grant_revisions
         WHERE installation_id = ?1 AND revision = ?2",
        params![
            installation_id.as_str(),
            revision_i64(expected_grant_revision)?
        ],
        |row| row.get(0),
    )?;
    let grant: AppGrantRevision = decode_app_contract(&grant_json, &AppContractLimits::default())?;
    if grant.revoked_at.is_some() {
        return Err(AppRegistryError::StateConflict(
            "active grant is already revoked without the expected lifecycle event".to_owned(),
        ));
    }
    let mut revoked = grant.clone();
    revoked.revoked_at = Some(*now);
    revoked.validate_app_contract(&AppContractLimits::default())?;
    let revoked_json = encode_bounded_json(&revoked, &AppContractLimits::default())?;
    let updated_grant = transaction.execute(
        "UPDATE app_grant_revisions SET revoked_at = ?1, record_json = ?2
         WHERE installation_id = ?3 AND revision = ?4
           AND revoked_at IS NULL AND record_json = ?5",
        params![
            format_timestamp(now),
            revoked_json,
            installation_id.as_str(),
            revision_i64(expected_grant_revision)?,
            grant_json,
        ],
    )?;
    if updated_grant != 1 {
        return Err(AppRegistryError::CompareAndSwapLost("grant revocation"));
    }
    let mut next = installation.clone();
    next.lifecycle = installation
        .lifecycle
        .apply(AppInstallationCommand::Quarantine)?;
    next.updated_at = *now;
    next.quarantined_at = Some(*now);
    next.validate_app_contract(&AppContractLimits::default())?;
    let event = lifecycle_event(
        event_id.clone(),
        &next,
        AppLifecycleEventKind::GrantRevoked,
        *now,
    );
    update_installation_cas(&transaction, &installation, &next)?;
    insert_outbox_event(&transaction, &event, &idempotency_key, now)?;
    append_installation_contribution_invalidations(
        &transaction,
        scope,
        installation_id.as_str(),
        AppMemoryInvalidationReasonV1::GrantRevoked,
        now,
    )?;
    settle_candidates_for_installation(&transaction, scope, &installation_id, *now)
        .map_err(registry_memory_store_error)?;
    transaction.commit()?;
    Ok(AppLifecycleMutationReceipt {
        installation_id,
        generation: next.lifecycle.generation,
        status: next.lifecycle.status,
        event_id,
        reenable_review_identity: None,
        outcome: AppLifecycleMutationOutcome::Applied,
    })
}

fn append_installation_contribution_invalidations(
    transaction: &Transaction<'_>,
    scope: &AppScope,
    installation_id: &str,
    reason: AppMemoryInvalidationReasonV1,
    now: &DateTime<Utc>,
) -> Result<(), AppRegistryError> {
    super::memory_contribution_outbox::append_memory_installation_invalidations_in_transaction(
        transaction,
        scope,
        installation_id,
        reason,
        now,
    )
    .map_err(|error| {
        AppRegistryError::InvalidControlPlane(format!(
            "installation memory invalidations could not be published: {error}"
        ))
    })?;
    super::retrieval_contribution_outbox::append_retrieval_installation_invalidations_in_transaction(
        transaction,
        scope,
        installation_id,
        reason,
        now,
    )
    .map_err(|error| {
        AppRegistryError::InvalidControlPlane(format!(
            "installation retrieval invalidations could not be published: {error}"
        ))
    })?;
    Ok(())
}

fn claim_outbox_blocking(
    connection: &mut rusqlite::Connection,
    lease_owner: AppReference,
    limit: usize,
    lease_duration: StdDuration,
    now: &DateTime<Utc>,
) -> Result<Vec<AppOutboxLease>, AppRegistryError> {
    let lease_duration = Duration::from_std(lease_duration).map_err(|_| {
        AppRegistryError::InvalidControlPlane("outbox lease duration overflow".to_owned())
    })?;
    let lease_expires_at = now.checked_add_signed(lease_duration).ok_or_else(|| {
        AppRegistryError::InvalidControlPlane("outbox lease expiry overflow".to_owned())
    })?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let max_payload_bytes = i64::try_from(AppContractLimits::default().max_document_bytes())
        .map_err(|_| {
            AppRegistryError::InvalidControlPlane("outbox payload limit overflow".to_owned())
        })?;
    let corrupt_ready_rows: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_lifecycle_outbox
          WHERE delivered_at IS NULL AND available_at <= ?1
            AND (delivery_state = 'pending'
                 OR (delivery_state = 'leased' AND lease_expires_at <= ?1))
            AND length(payload_json) NOT BETWEEN 1 AND ?2",
        params![format_timestamp(now), max_payload_bytes],
        |row| row.get(0),
    )?;
    if corrupt_ready_rows != 0 {
        return Err(AppRegistryError::InvalidControlPlane(
            "lifecycle outbox contains an unbounded ready row".to_owned(),
        ));
    }
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT sequence, payload_json, attempt_count
             FROM app_lifecycle_outbox
             WHERE delivered_at IS NULL AND available_at <= ?1
               AND (delivery_state = 'pending'
                    OR (delivery_state = 'leased' AND lease_expires_at <= ?1))
               AND length(payload_json) BETWEEN 1 AND ?2
             ORDER BY sequence ASC LIMIT ?3",
        )?;
        let mapped = statement.query_map(
            params![format_timestamp(now), max_payload_bytes, usize_i64(limit)?],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?;
        mapped.collect::<Result<Vec<_>, _>>()?
    };
    let mut leases = Vec::with_capacity(rows.len());
    for (sequence, payload, prior_attempt_count) in rows {
        let event: AppLifecycleOutboxEvent =
            decode_app_contract(&payload, &AppContractLimits::default())?;
        let token = AppReference::parse(format!("outbox-lease:{}", Uuid::new_v4().simple()))?;
        let updated = transaction.execute(
            "UPDATE app_lifecycle_outbox
             SET delivery_state = 'leased', lease_owner = ?1, lease_token = ?2,
                 lease_expires_at = ?3, attempt_count = attempt_count + 1
             WHERE sequence = ?4 AND delivered_at IS NULL AND available_at <= ?5
               AND (delivery_state = 'pending'
                    OR (delivery_state = 'leased' AND lease_expires_at <= ?5))",
            params![
                lease_owner.as_str(),
                token.as_str(),
                format_timestamp(&lease_expires_at),
                sequence,
                format_timestamp(now),
            ],
        )?;
        if updated != 1 {
            return Err(AppRegistryError::CompareAndSwapLost("outbox claim"));
        }
        let attempt_count = prior_attempt_count
            .checked_add(1)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| {
                AppRegistryError::InvalidControlPlane(
                    "outbox attempt count exceeds supported range".to_owned(),
                )
            })?;
        leases.push(AppOutboxLease {
            sequence,
            event,
            lease_owner: lease_owner.clone(),
            lease_token: token,
            lease_expires_at,
            attempt_count,
        });
    }
    transaction.commit()?;
    Ok(leases)
}

fn acknowledge_outbox_blocking(
    connection: &mut rusqlite::Connection,
    lease: AppOutboxLease,
    delivery_receipt_id: AppReference,
    now: &DateTime<Utc>,
) -> Result<AppOutboxAcknowledgeReceipt, AppRegistryError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stored: Option<(String, Option<String>, Option<String>, Option<String>)> = transaction
        .query_row(
            "SELECT delivery_state, lease_token, delivery_receipt_id, delivered_at
             FROM app_lifecycle_outbox WHERE sequence = ?1 AND event_id = ?2",
            params![lease.sequence, lease.event.event_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((state, stored_token, stored_receipt, delivered_at)) = stored else {
        return Err(AppRegistryError::MissingRecord {
            entity: "outbox event",
            identity: lease.event.event_id.to_string(),
        });
    };
    if state == "delivered" {
        if stored_receipt.as_deref() == Some(delivery_receipt_id.as_str()) && delivered_at.is_some()
        {
            return Ok(AppOutboxAcknowledgeReceipt {
                event_id: lease.event.event_id,
                delivery_receipt_id,
                outcome: AppOutboxAcknowledgeOutcome::AlreadyDelivered,
            });
        }
        return Err(AppRegistryError::OutboxLeaseStale);
    }
    if now >= &lease.lease_expires_at || stored_token.as_deref() != Some(lease.lease_token.as_str())
    {
        return Err(AppRegistryError::OutboxLeaseStale);
    }
    let updated = transaction.execute(
        "UPDATE app_lifecycle_outbox
         SET delivery_state = 'delivered', delivery_receipt_id = ?1,
             delivered_at = ?2, lease_owner = NULL, lease_token = NULL,
             lease_expires_at = NULL
         WHERE sequence = ?3 AND event_id = ?4 AND delivery_state = 'leased'
           AND lease_owner = ?5 AND lease_token = ?6 AND lease_expires_at = ?7
           AND delivered_at IS NULL",
        params![
            delivery_receipt_id.as_str(),
            format_timestamp(now),
            lease.sequence,
            lease.event.event_id.as_str(),
            lease.lease_owner.as_str(),
            lease.lease_token.as_str(),
            format_timestamp(&lease.lease_expires_at),
        ],
    )?;
    if updated != 1 {
        return Err(AppRegistryError::OutboxLeaseStale);
    }
    transaction.commit()?;
    Ok(AppOutboxAcknowledgeReceipt {
        event_id: lease.event.event_id,
        delivery_receipt_id,
        outcome: AppOutboxAcknowledgeOutcome::Delivered,
    })
}

fn release_outbox_blocking(
    connection: &mut rusqlite::Connection,
    lease: AppOutboxLease,
    retry_delay: StdDuration,
    now: &DateTime<Utc>,
) -> Result<(), AppRegistryError> {
    if now >= &lease.lease_expires_at {
        return Err(AppRegistryError::OutboxLeaseStale);
    }
    let delay = Duration::from_std(retry_delay).map_err(|_| {
        AppRegistryError::InvalidControlPlane("outbox retry delay overflow".to_owned())
    })?;
    let available_at = now.checked_add_signed(delay).ok_or_else(|| {
        AppRegistryError::InvalidControlPlane("outbox retry timestamp overflow".to_owned())
    })?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let updated = transaction.execute(
        "UPDATE app_lifecycle_outbox
         SET delivery_state = 'pending', available_at = ?1,
             lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
         WHERE sequence = ?2 AND event_id = ?3 AND delivery_state = 'leased'
           AND lease_owner = ?4 AND lease_token = ?5 AND lease_expires_at = ?6
           AND delivered_at IS NULL",
        params![
            format_timestamp(&available_at),
            lease.sequence,
            lease.event.event_id.as_str(),
            lease.lease_owner.as_str(),
            lease.lease_token.as_str(),
            format_timestamp(&lease.lease_expires_at),
        ],
    )?;
    if updated != 1 {
        return Err(AppRegistryError::OutboxLeaseStale);
    }
    transaction.commit()?;
    Ok(())
}

fn recover_outbox_blocking(
    connection: &mut rusqlite::Connection,
    limit: usize,
    now: &DateTime<Utc>,
) -> Result<AppOutboxRecoveryReceipt, AppRegistryError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let sequences = {
        let mut statement = transaction.prepare(
            "SELECT sequence FROM app_lifecycle_outbox
             WHERE delivery_state = 'leased' AND delivered_at IS NULL
               AND lease_expires_at <= ?1
             ORDER BY sequence ASC LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![format_timestamp(now), usize_i64(limit.saturating_add(1))?],
            |row| row.get::<_, i64>(0),
        )?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let more = sequences.len() > limit;
    let recover = &sequences[..sequences.len().min(limit)];
    for sequence in recover {
        transaction.execute(
            "UPDATE app_lifecycle_outbox
             SET delivery_state = 'pending', lease_owner = NULL,
                 lease_token = NULL, lease_expires_at = NULL
             WHERE sequence = ?1 AND delivery_state = 'leased'
               AND delivered_at IS NULL AND lease_expires_at <= ?2",
            params![sequence, format_timestamp(now)],
        )?;
    }
    transaction.commit()?;
    Ok(AppOutboxRecoveryReceipt {
        recovered_leases: recover.len(),
        more_expired_leases_may_remain: more,
    })
}

fn load_attempt_and_installation_id(
    transaction: &Transaction<'_>,
    attempt_id: &AppReference,
) -> Result<(AppLifecycleAttempt, AppInstallationId), AppRegistryError> {
    let stored: Option<(Vec<u8>, Option<String>)> = transaction
        .query_row(
            "SELECT record_json, installation_id FROM app_lifecycle_attempts
             WHERE attempt_id = ?1",
            params![attempt_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((bytes, installation_id)) = stored else {
        return Err(AppRegistryError::MissingRecord {
            entity: "lifecycle attempt",
            identity: attempt_id.to_string(),
        });
    };
    let attempt = decode_app_contract(&bytes, &AppContractLimits::default())?;
    let installation_id = installation_id.ok_or_else(|| {
        AppRegistryError::StateConflict(
            "lifecycle attempt has no registry-owned proposed installation link".to_owned(),
        )
    })?;
    Ok((attempt, AppInstallationId::parse(installation_id)?))
}

fn load_package(
    transaction: &Transaction<'_>,
    package_ref: &AppReference,
) -> Result<AppPackageRevision, AppRegistryError> {
    load_record(
        transaction,
        "SELECT record_json FROM app_package_revisions WHERE package_revision_ref = ?1",
        package_ref.as_str(),
        "package revision",
    )
}

pub(super) fn load_installation(
    transaction: &rusqlite::Connection,
    installation_id: &AppInstallationId,
) -> Result<AppInstallation, AppRegistryError> {
    load_record(
        transaction,
        "SELECT record_json FROM app_installations WHERE installation_id = ?1",
        installation_id.as_str(),
        "installation",
    )
}

fn load_approval(
    transaction: &Transaction<'_>,
    approval_id: &AppReference,
    revision: AppRevision,
) -> Result<AppInstallationApproval, AppRegistryError> {
    let bytes: Option<Vec<u8>> = transaction
        .query_row(
            "SELECT record_json FROM app_installation_approvals
             WHERE approval_id = ?1 AND revision = ?2",
            params![approval_id.as_str(), revision_i64(revision)?],
            |row| row.get(0),
        )
        .optional()?;
    decode_required(bytes, "installation approval", approval_id.as_str())
}

fn load_record<T>(
    transaction: &rusqlite::Connection,
    sql: &str,
    identity: &str,
    entity: &'static str,
) -> Result<T, AppRegistryError>
where
    T: ValidateAppContract + serde::de::DeserializeOwned,
{
    let bytes = transaction
        .query_row(sql, params![identity], |row| row.get::<_, Vec<u8>>(0))
        .optional()?;
    decode_required(bytes, entity, identity)
}

fn decode_required<T>(
    bytes: Option<Vec<u8>>,
    entity: &'static str,
    identity: &str,
) -> Result<T, AppRegistryError>
where
    T: ValidateAppContract + serde::de::DeserializeOwned,
{
    let bytes = bytes.ok_or_else(|| AppRegistryError::MissingRecord {
        entity,
        identity: identity.to_owned(),
    })?;
    decode_app_contract(&bytes, &AppContractLimits::default()).map_err(Into::into)
}

fn approval_expectation(
    approval: &AppInstallationApproval,
    attempt_kind: AppLifecycleAttemptKind,
    current_global_policy_revision: AppRevision,
) -> AppInstallationApprovalExpectation<'_> {
    AppInstallationApprovalExpectation {
        approval_id: &approval.approval_id,
        approval_revision: approval.revision,
        attempt_id: &approval.install_or_update_attempt_id,
        attempt_kind,
        package_content_digest: &approval.package_content_digest,
        requested_authority_digest: &approval.requested_authority_digest,
        granted_authority_digest: &approval.granted_authority_digest,
        data_policy_diff_digest: &approval.data_policy_diff_digest,
        resource_diff_digest: &approval.resource_diff_digest,
        schema_diff_digest: &approval.schema_diff_digest,
        migration_diff_digest: &approval.migration_diff_digest,
        global_policy_revision: current_global_policy_revision,
    }
}

fn validate_review_target(
    attempt: &AppLifecycleAttempt,
    installation: &AppInstallation,
    expected_kind: AppLifecycleAttemptKind,
) -> Result<(), AppRegistryError> {
    if attempt.kind != expected_kind || attempt.state != AppLifecycleAttemptState::ReadyForReview {
        return Err(AppRegistryError::StateConflict(
            "reviewed decision does not target the current ready-for-review attempt".to_owned(),
        ));
    }
    let valid = match expected_kind {
        AppLifecycleAttemptKind::InitialInstall => {
            attempt.installation_id.is_none()
                && attempt.source_installation_generation.is_none()
                && installation.package_revision_ref == attempt.candidate_package_revision_ref
                && installation.lifecycle.status == AppInstallationStatus::ReadyForReview
                && installation.lifecycle.generation == 1
        },
        AppLifecycleAttemptKind::Update => {
            attempt.installation_id.as_ref() == Some(&installation.installation_id)
                && attempt
                    .source_installation_generation
                    .and_then(|generation| generation.checked_add(1))
                    == Some(installation.lifecycle.generation)
                && installation.lifecycle.status == AppInstallationStatus::UpdatePending
        },
        AppLifecycleAttemptKind::Reinstall => {
            attempt.installation_id.as_ref() == Some(&installation.installation_id)
                && attempt.source_installation_generation == Some(installation.lifecycle.generation)
                && installation.lifecycle.status == AppInstallationStatus::UninstalledRetained
        },
    };
    if !valid {
        return Err(AppRegistryError::StateConflict(
            "reviewed attempt no longer matches its exact installation generation/state".to_owned(),
        ));
    }
    Ok(())
}

fn validate_revision_successors(
    installation: &AppInstallation,
    commit: &AppReviewedInstallationCommit,
) -> Result<(), AppRegistryError> {
    let lifecycle_is_reviewable = match commit.attempt_kind {
        AppLifecycleAttemptKind::InitialInstall => {
            installation.lifecycle.status == AppInstallationStatus::ReadyForReview
                && installation.lifecycle.generation == 1
                && installation.grant_revision.is_none()
                && installation.active_schema_revision.is_none()
                && installation.active_surface_revision.is_none()
        },
        AppLifecycleAttemptKind::Update => {
            installation.lifecycle.status == AppInstallationStatus::UpdatePending
        },
        AppLifecycleAttemptKind::Reinstall => {
            installation.lifecycle.status == AppInstallationStatus::UninstalledRetained
        },
    };
    if !lifecycle_is_reviewable {
        return Err(AppRegistryError::StateConflict(
            "installation is not in the lifecycle state required by the reviewed attempt"
                .to_owned(),
        ));
    }
    let expected = match commit.attempt_kind {
        AppLifecycleAttemptKind::InitialInstall => 1,
        AppLifecycleAttemptKind::Update | AppLifecycleAttemptKind::Reinstall => installation
            .grant_revision
            .map(AppRevision::get)
            .ok_or_else(|| {
                AppRegistryError::StateConflict(
                    "update/reinstall has no active grant revision".to_owned(),
                )
            })?
            .checked_add(1)
            .ok_or_else(|| {
                AppRegistryError::InvalidControlPlane("grant revision exhausted".to_owned())
            })?,
    };
    let schema_expected = match commit.attempt_kind {
        AppLifecycleAttemptKind::InitialInstall => 1,
        _ => installation
            .active_schema_revision
            .map(AppRevision::get)
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| {
                AppRegistryError::InvalidControlPlane("schema revision exhausted".to_owned())
            })?,
    };
    let surface_expected = match commit.attempt_kind {
        AppLifecycleAttemptKind::InitialInstall => 1,
        _ => installation
            .active_surface_revision
            .map(AppRevision::get)
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| {
                AppRegistryError::InvalidControlPlane("surface revision exhausted".to_owned())
            })?,
    };
    if commit.grant.revision.get() != expected
        || commit.schema.revision.get() != schema_expected
        || commit.surface.surface_revision().get() != surface_expected
    {
        return Err(AppRegistryError::StateConflict(
            "reviewed revisions are not exact successors of the active set".to_owned(),
        ));
    }
    Ok(())
}

fn insert_revision_set(
    transaction: &Transaction<'_>,
    commit: &AppReviewedInstallationCommit,
    now: &DateTime<Utc>,
) -> Result<(), AppRegistryError> {
    let limits = AppContractLimits::default();
    let grant_json = encode_bounded_json(&commit.grant, &limits)?;
    let schema_json = encode_bounded_json(&commit.schema, &limits)?;
    transaction.execute(
        "INSERT INTO app_grant_revisions (
             installation_id, revision, package_revision_ref, authority_digest,
             granted_data_policy_digest, revoked_at, record_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7)",
        params![
            commit.grant.installation_id.as_str(),
            revision_i64(commit.grant.revision)?,
            commit.grant.package_revision_ref.as_str(),
            commit.grant.authority_digest.as_str(),
            commit.grant.granted_data_handling_policy_digest.as_str(),
            grant_json,
            format_timestamp(&commit.grant.approved_at),
        ],
    )?;
    transaction.execute(
        "INSERT INTO app_schema_revisions (
             installation_id, revision, package_revision_ref, record_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            commit.schema.installation_id.as_str(),
            revision_i64(commit.schema.revision)?,
            commit.schema.package_revision_ref.as_str(),
            schema_json,
            format_timestamp(&commit.schema.created_at),
        ],
    )?;
    let set = &commit.surface;
    transaction.execute(
        "INSERT INTO app_surface_generations (
             installation_id, revision, package_revision_ref, schema_revision,
             compiled_set_digest, member_count, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            set.installation_id().as_str(),
            revision_i64(set.surface_revision())?,
            set.package_revision_ref().as_str(),
            revision_i64(set.schema_revision())?,
            set.compiled_set_digest().as_str(),
            usize_i64(set.surfaces().len())?,
            format_timestamp(now),
        ],
    )?;
    for surface in set.surfaces().values() {
        let binding = surface.binding();
        transaction.execute(
            "INSERT INTO app_surface_generation_members (
                 installation_id, revision, view_id, app_local_route,
                 canonical_host_route, compiled_view_digest, binding_json,
                 envelope_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                binding.installation_id.as_str(),
                revision_i64(binding.surface_revision)?,
                binding.view_id.as_str(),
                binding.app_local_route.as_str(),
                binding.canonical_host_route.as_str(),
                binding.compiled_view_digest.as_str(),
                encode_bounded_json(binding, &limits)?,
                encode_bounded_json(surface.envelope(), &limits)?,
                format_timestamp(now),
            ],
        )?;
    }
    Ok(())
}

pub(super) fn update_installation_cas(
    transaction: &Transaction<'_>,
    current: &AppInstallation,
    next: &AppInstallation,
) -> Result<(), AppRegistryError> {
    let current_json = encode_bounded_json(current, &AppContractLimits::default())?;
    let next_json = encode_bounded_json(next, &AppContractLimits::default())?;
    let updated = transaction.execute(
        "UPDATE app_installations
         SET package_revision_ref = ?1, lifecycle_status = ?2,
             lifecycle_generation = ?3, record_json = ?4, updated_at = ?5
         WHERE installation_id = ?6 AND lifecycle_generation = ?7
           AND record_json = ?8",
        params![
            next.package_revision_ref.as_str(),
            enum_json_label(&next.lifecycle.status)?,
            u64_i64(next.lifecycle.generation, "installation generation")?,
            next_json,
            format_timestamp(&next.updated_at),
            current.installation_id.as_str(),
            u64_i64(current.lifecycle.generation, "installation generation")?,
            current_json,
        ],
    )?;
    if updated != 1 {
        return Err(AppRegistryError::CompareAndSwapLost(
            "installation lifecycle",
        ));
    }
    Ok(())
}

fn update_attempt_cas(
    transaction: &Transaction<'_>,
    current: &AppLifecycleAttempt,
    next: &AppLifecycleAttempt,
) -> Result<(), AppRegistryError> {
    let current_json = encode_bounded_json(current, &AppContractLimits::default())?;
    let next_json = encode_bounded_json(next, &AppContractLimits::default())?;
    let updated = transaction.execute(
        "UPDATE app_lifecycle_attempts
         SET state = ?1, record_json = ?2, updated_at = ?3
         WHERE attempt_id = ?4 AND state = ?5 AND record_json = ?6",
        params![
            enum_json_label(&next.state)?,
            next_json,
            format_timestamp(&next.updated_at),
            current.attempt_id.as_str(),
            enum_json_label(&current.state)?,
            current_json,
        ],
    )?;
    if updated != 1 {
        return Err(AppRegistryError::CompareAndSwapLost("lifecycle attempt"));
    }
    Ok(())
}

fn consume_approval_cas(
    transaction: &Transaction<'_>,
    current: &AppInstallationApproval,
    next: &AppInstallationApproval,
) -> Result<(), AppRegistryError> {
    let current_json = encode_bounded_json(current, &AppContractLimits::default())?;
    let next_json = encode_bounded_json(next, &AppContractLimits::default())?;
    let updated = transaction.execute(
        "UPDATE app_installation_approvals
         SET consumed_at = ?1, consumed_installation_revision = ?2,
             record_json = ?3
         WHERE approval_id = ?4 AND revision = ?5 AND consumed_at IS NULL
           AND consumed_installation_revision IS NULL AND record_json = ?6",
        params![
            format_timestamp(
                next.consumed_at
                    .as_ref()
                    .expect("validated consumed approval")
            ),
            revision_i64(
                next.consumed_installation_revision
                    .expect("validated consumed approval")
            )?,
            next_json,
            current.approval_id.as_str(),
            revision_i64(current.revision)?,
            current_json,
        ],
    )?;
    if updated != 1 {
        return Err(AppRegistryError::CompareAndSwapLost("approval consumption"));
    }
    Ok(())
}

pub(super) fn insert_outbox_event(
    transaction: &Transaction<'_>,
    event: &AppLifecycleOutboxEvent,
    idempotency_key: &AppDigest,
    now: &DateTime<Utc>,
) -> Result<(), AppRegistryError> {
    event.validate_app_contract(&AppContractLimits::default())?;
    let payload = encode_bounded_json(event, &AppContractLimits::default())?;
    transaction.execute(
        "INSERT INTO app_lifecycle_outbox (
             event_id, idempotency_key, installation_id,
             installation_generation, event_kind, payload_json,
             delivery_state, attempt_count, available_at, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', 0, ?7, ?8)",
        params![
            event.event_id.as_str(),
            idempotency_key.as_str(),
            event.installation_id.as_str(),
            u64_i64(event.installation_generation, "outbox generation")?,
            enum_json_label(&event.event_kind)?,
            payload,
            format_timestamp(now),
            format_timestamp(now),
        ],
    )?;
    Ok(())
}

fn verify_reviewed_replay(
    transaction: &Transaction<'_>,
    commit: &AppReviewedInstallationCommit,
    durable_approval: &AppInstallationApproval,
    attempt: &AppLifecycleAttempt,
    installation: &AppInstallation,
) -> Result<AppLifecycleMutationReceipt, AppRegistryError> {
    if attempt.state != AppLifecycleAttemptState::Committed
        || attempt.approval_ref.as_ref() != Some(&commit.approval.approval_id)
        || installation.grant_revision != Some(commit.grant.revision)
        || installation.active_schema_revision != Some(commit.schema.revision)
        || installation.active_surface_revision != Some(commit.surface.surface_revision())
        || installation.package_revision_ref != commit.grant.package_revision_ref
        || durable_approval.consumed_installation_revision
            != Some(AppRevision::new(installation.lifecycle.generation)?)
    {
        return Err(AppRegistryError::StateConflict(
            "consumed approval does not describe this committed installation".to_owned(),
        ));
    }
    verify_exact_revision(
        transaction,
        "app_grant_revisions",
        &commit.grant.installation_id,
        commit.grant.revision,
        &commit.grant,
    )?;
    verify_exact_revision(
        transaction,
        "app_schema_revisions",
        &commit.schema.installation_id,
        commit.schema.revision,
        &commit.schema,
    )?;
    verify_exact_surface_generation(transaction, &commit.surface)?;
    let event = lifecycle_event(
        commit.event_id.clone(),
        installation,
        reviewed_event_kind(commit.attempt_kind),
        installation.updated_at,
    );
    if !verify_outbox_identity(transaction, &event, &commit.idempotency_key)? {
        return Err(AppRegistryError::StateConflict(
            "committed installation is missing its exact lifecycle outbox event".to_owned(),
        ));
    }
    Ok(AppLifecycleMutationReceipt {
        installation_id: installation.installation_id.clone(),
        generation: installation.lifecycle.generation,
        status: installation.lifecycle.status,
        event_id: commit.event_id.clone(),
        reenable_review_identity: None,
        outcome: AppLifecycleMutationOutcome::AlreadyApplied,
    })
}

fn verify_exact_revision<T: Serialize>(
    transaction: &Transaction<'_>,
    table: &'static str,
    installation_id: &AppInstallationId,
    revision: AppRevision,
    expected: &T,
) -> Result<(), AppRegistryError> {
    let sql = match table {
        "app_grant_revisions" => {
            "SELECT record_json FROM app_grant_revisions
             WHERE installation_id = ?1 AND revision = ?2"
        },
        "app_schema_revisions" => {
            "SELECT record_json FROM app_schema_revisions
             WHERE installation_id = ?1 AND revision = ?2"
        },
        _ => unreachable!("only fixed registry-owned revision tables are accepted"),
    };
    let stored: Option<Vec<u8>> = transaction
        .query_row(
            sql,
            params![installation_id.as_str(), revision_i64(revision)?],
            |row| row.get(0),
        )
        .optional()?;
    let expected = encode_bounded_json(expected, &AppContractLimits::default())?;
    if stored.as_deref() != Some(expected.as_slice()) {
        return Err(AppRegistryError::StateConflict(format!(
            "{table} replay bytes differ"
        )));
    }
    Ok(())
}

fn verify_exact_surface_generation(
    transaction: &Transaction<'_>,
    set: &CompiledAppSurfaceSet,
) -> Result<(), AppRegistryError> {
    let generation: Option<(String, i64, String, i64)> = transaction
        .query_row(
            "SELECT package_revision_ref, schema_revision, compiled_set_digest, member_count
                       FROM app_surface_generations
                      WHERE installation_id = ?1 AND revision = ?2",
            params![
                set.installation_id().as_str(),
                revision_i64(set.surface_revision())?
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let expected_count = usize_i64(set.surfaces().len())?;
    if generation
        != Some((
            set.package_revision_ref().as_str().to_owned(),
            revision_i64(set.schema_revision())?,
            set.compiled_set_digest().as_str().to_owned(),
            expected_count,
        ))
    {
        return Err(AppRegistryError::StateConflict(
            "surface generation replay metadata differs".to_owned(),
        ));
    }
    let stored_count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_surface_generation_members
                  WHERE installation_id = ?1 AND revision = ?2",
        params![
            set.installation_id().as_str(),
            revision_i64(set.surface_revision())?
        ],
        |row| row.get(0),
    )?;
    if stored_count != expected_count {
        return Err(AppRegistryError::StateConflict(
            "surface generation replay member count differs".to_owned(),
        ));
    }
    for member in set.surfaces().values() {
        let binding = member.binding();
        let stored: Option<(String, String, String, Vec<u8>, Vec<u8>)> = transaction
            .query_row(
                "SELECT app_local_route, canonical_host_route, compiled_view_digest,
                                binding_json, envelope_json
                           FROM app_surface_generation_members
                          WHERE installation_id = ?1 AND revision = ?2 AND view_id = ?3",
                params![
                    binding.installation_id.as_str(),
                    revision_i64(binding.surface_revision)?,
                    binding.view_id.as_str(),
                ],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let expected = (
            binding.app_local_route.clone(),
            binding.canonical_host_route.clone(),
            binding.compiled_view_digest.as_str().to_owned(),
            encode_bounded_json(binding, &AppContractLimits::default())?,
            encode_bounded_json(member.envelope(), &AppContractLimits::default())?,
        );
        if stored != Some(expected) {
            return Err(AppRegistryError::StateConflict(
                "surface generation replay member differs".to_owned(),
            ));
        }
    }
    Ok(())
}

fn verify_outbox_identity(
    transaction: &Transaction<'_>,
    event: &AppLifecycleOutboxEvent,
    idempotency_key: &AppDigest,
) -> Result<bool, AppRegistryError> {
    let stored: Option<(String, String, i64, Vec<u8>)> = transaction
        .query_row(
            "SELECT idempotency_key, event_kind, installation_generation, payload_json
             FROM app_lifecycle_outbox WHERE event_id = ?1",
            params![event.event_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((stored_key, stored_kind, stored_generation, stored_payload)) = stored else {
        return Ok(false);
    };
    let expected_payload = encode_bounded_json(event, &AppContractLimits::default())?;
    Ok(stored_key == idempotency_key.as_str()
        && stored_kind == enum_json_label(&event.event_kind)?
        && stored_generation == u64_i64(event.installation_generation, "outbox generation")?
        && stored_payload == expected_payload)
}

pub(super) fn lifecycle_event(
    event_id: AppReference,
    installation: &AppInstallation,
    event_kind: AppLifecycleEventKind,
    occurred_at: DateTime<Utc>,
) -> AppLifecycleOutboxEvent {
    AppLifecycleOutboxEvent {
        event_id,
        installation_id: installation.installation_id.clone(),
        package_revision_ref: installation.package_revision_ref.clone(),
        installation_generation: installation.lifecycle.generation,
        event_kind,
        lifecycle_status: installation.lifecycle.status,
        grant_revision: installation.grant_revision,
        schema_revision: installation.active_schema_revision,
        surface_revision: installation.active_surface_revision,
        reenable_review_identity: None,
        occurred_at,
    }
}

fn reviewed_command(kind: AppLifecycleAttemptKind) -> AppInstallationCommand {
    match kind {
        AppLifecycleAttemptKind::InitialInstall => AppInstallationCommand::EnableReviewed,
        AppLifecycleAttemptKind::Update => AppInstallationCommand::CommitUpdate,
        AppLifecycleAttemptKind::Reinstall => AppInstallationCommand::CommitReviewedReinstall,
    }
}

fn reviewed_event_kind(kind: AppLifecycleAttemptKind) -> AppLifecycleEventKind {
    match kind {
        AppLifecycleAttemptKind::InitialInstall => AppLifecycleEventKind::InstallationEnabled,
        AppLifecycleAttemptKind::Update => AppLifecycleEventKind::InstallationUpdated,
        AppLifecycleAttemptKind::Reinstall => AppLifecycleEventKind::InstallationReinstalled,
    }
}

fn ordinary_event_kind(
    command: AppInstallationCommand,
) -> Result<AppLifecycleEventKind, AppRegistryError> {
    match command {
        AppInstallationCommand::Disable => Ok(AppLifecycleEventKind::InstallationDisabled),
        AppInstallationCommand::BeginUpdate => Ok(AppLifecycleEventKind::UpdateBegan),
        AppInstallationCommand::FailUpdate => Ok(AppLifecycleEventKind::UpdateFailed),
        AppInstallationCommand::Quarantine => Ok(AppLifecycleEventKind::InstallationQuarantined),
        AppInstallationCommand::UninstallRetain => Ok(AppLifecycleEventKind::InstallationRetained),
        _ => Err(AppRegistryError::InvalidControlPlane(
            "command is not an ordinary lifecycle transition".to_owned(),
        )),
    }
}

fn apply_lifecycle_timestamps(
    installation: &mut AppInstallation,
    command: AppInstallationCommand,
    now: DateTime<Utc>,
) {
    match command {
        AppInstallationCommand::Disable => installation.disabled_at = Some(now),
        AppInstallationCommand::Quarantine => installation.quarantined_at = Some(now),
        AppInstallationCommand::UninstallRetain => installation.uninstalled_at = Some(now),
        _ => {},
    }
}

fn apply_reviewed_timestamps(
    attempt_kind: AppLifecycleAttemptKind,
    installation: &mut AppInstallation,
) {
    match attempt_kind {
        AppLifecycleAttemptKind::InitialInstall | AppLifecycleAttemptKind::Reinstall => {
            installation.disabled_at = None;
            installation.quarantined_at = None;
            installation.uninstalled_at = None;
            installation.purged_at = None;
        },
        AppLifecycleAttemptKind::Update => {
            if installation.lifecycle.status == AppInstallationStatus::Enabled {
                installation.disabled_at = None;
            }
        },
    }
}

fn ensure_installation_scope(
    installation: &AppInstallation,
    scope: &AppScope,
) -> Result<(), AppRegistryError> {
    if &installation.scope != scope {
        return Err(AppRegistryError::ScopeCollision);
    }
    Ok(())
}

fn revision_i64(revision: AppRevision) -> Result<i64, AppRegistryError> {
    u64_i64(revision.get(), "revision")
}

fn usize_i64(value: usize) -> Result<i64, AppRegistryError> {
    i64::try_from(value).map_err(|_| {
        AppRegistryError::InvalidControlPlane("bounded count exceeds SQLite range".to_owned())
    })
}

fn u64_i64(value: u64, field: &'static str) -> Result<i64, AppRegistryError> {
    i64::try_from(value)
        .map_err(|_| AppRegistryError::InvalidControlPlane(format!("{field} exceeds SQLite range")))
}

fn canonical_contract_digest<T: Serialize>(value: &T) -> Result<AppDigest, AppRegistryError> {
    let value = serde_json::to_value(value)?;
    AppDigest::blake3_canonical_json(&value).map_err(AppRegistryError::from)
}

fn same_approval_decision(
    durable: &AppInstallationApproval,
    proposed: &AppInstallationApproval,
) -> bool {
    let mut durable = durable.clone();
    durable.consumed_at = None;
    durable.consumed_installation_revision = None;
    durable == *proposed
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::apps::update::{
        compute_permission_diff, AppUpdateCoordinatorService, AppUpdatePlanRequest,
    };
    use magician::magician_v2::{
        apps::{
            manifest::{
                canonical_view_schema_digest, parse_app_manifest_frontmatter,
                tests::valid_skill_document, AppPackageLimits,
            },
            models::{AppDataClassification, AppModelProcessing, AppName},
            package_staging::AppPackageStager,
            records::{
                AppApprovalAuthentication, AppBackgroundExecution, AppDataHandlingPolicy,
                AppExternalEgress, AppMemoryPromotion, AppNetworkPolicy, AppPersonalAgentAccess,
                AppResourceCeiling, AppSchemaCompatibility,
            },
            registry::{
                tests::{
                    authenticated_scope, canonical_tempdir, publication,
                    publication_with_requested_policy_digest, reference,
                    reviewable_update_publication, time,
                },
                AppRegistryPublicationOutcome,
            },
            schema_compiler::{canonical_entity_schema_digest, compile_app_schema},
            surface_compiler::{compile_app_surface_set, VerifiedAppSurfaceSource},
        },
        artifact_v2::workspace::ArtifactV2Workspace,
    };

    #[derive(Clone)]
    struct ReviewedFixture {
        approval: AppInstallationApproval,
        grant: AppGrantRevision,
        schema: AppSchemaRevision,
        surface: AppSurfaceBinding,
    }

    fn denied_policy() -> AppDataHandlingPolicy {
        AppDataHandlingPolicy {
            classification_floor: AppDataClassification::Personal,
            model_processing: AppModelProcessing::None,
            personal_agent_access: AppPersonalAgentAccess::Denied,
            memory_promotion: AppMemoryPromotion::Denied,
            external_egress: AppExternalEgress::Denied,
            approved_destinations: Vec::new(),
        }
    }

    fn zero_ceiling() -> AppResourceCeiling {
        AppResourceCeiling {
            max_input_tokens: 0,
            max_output_tokens: 0,
            max_cost_microusd: 0,
            max_paid_tool_invocations: 0,
            max_active_seconds: 0,
            max_lifetime_seconds: 0,
            max_browser_network_actions: 0,
            max_concurrent_foreground_runs: 0,
            max_concurrent_background_runs: 0,
            max_records: 0,
            max_payload_bytes: 0,
            max_attachment_bytes: 0,
            max_monthly_tokens: 0,
            max_monthly_cost_microusd: 0,
        }
    }

    fn reviewed_fixture(
        package: &AppPackageRevision,
        package_ref: AppReference,
    ) -> ReviewedFixture {
        let installation_id = AppInstallationId::parse("install_1").unwrap();
        let policy = denied_policy();
        let mut grant = AppGrantRevision {
            installation_id: installation_id.clone(),
            revision: AppRevision::new(1).unwrap(),
            package_revision_ref: package_ref.clone(),
            requested_tools: Vec::new(),
            granted_tools: Vec::new(),
            requested_agents: Vec::new(),
            granted_agents: Vec::new(),
            requested_personalities: Vec::new(),
            granted_personalities: Vec::new(),
            requested_context_reads: Vec::new(),
            granted_context_reads: Vec::new(),
            requested_interactive_capabilities: Vec::new(),
            granted_interactive_capabilities: Vec::new(),
            granted_custom_surface_entry_points: Vec::new(),
            requested_behavior_grants: Vec::new(),
            granted_behavior_grants: Vec::new(),
            requested_event_behavior_grants: Vec::new(),
            granted_event_behavior_grants: Vec::new(),
            requested_notification_grants: Vec::new(),
            granted_notification_grants: Vec::new(),
            requested_memory_read: None,
            granted_memory_read: None,
            requested_secret_uses: None,
            granted_secret_uses: None,
            granted_any_public_host: false,
            requested_personal_agent_data_access: Vec::new(),
            granted_personal_agent_data_access: Vec::new(),
            requested_data_handling_policy: policy.clone(),
            granted_data_handling_policy: policy.clone(),
            granted_data_handling_policy_digest: AppDigest::blake3(b"placeholder"),
            requested_background_execution: AppBackgroundExecution::Denied,
            granted_background_execution: AppBackgroundExecution::Denied,
            requested_network_policy: AppNetworkPolicy::Denied,
            granted_network_policy: AppNetworkPolicy::Denied,
            requested_resource_ceiling: zero_ceiling(),
            granted_resource_ceiling: zero_ceiling(),
            approved_by: reference("actor:owner"),
            approved_at: time(3),
            authority_digest: AppDigest::blake3(b"grant-authority"),
            revoked_at: None,
        };
        grant.granted_data_handling_policy_digest =
            canonical_contract_digest(&grant.granted_data_handling_policy).unwrap();
        let manifest = parse_app_manifest_frontmatter(
            valid_skill_document().as_bytes(),
            &AppPackageLimits::default(),
        )
        .unwrap();
        let schema = compile_app_schema(
            manifest.manifest(),
            installation_id.clone(),
            package_ref.clone(),
            AppRevision::new(1).unwrap(),
            policy,
            AppSchemaCompatibility::Initial,
            None,
            &package.entity_schema_digest,
            time(3),
        )
        .unwrap()
        .into_revision();
        let surface = AppSurfaceBinding {
            installation_id,
            surface_revision: AppRevision::new(1).unwrap(),
            package_revision_ref: package_ref,
            app_local_route: "/".to_owned(),
            canonical_host_route: "/apps/install_1".to_owned(),
            view_id: AppName::parse("home").unwrap(),
            compiled_view_digest: AppDigest::blake3(b"compiled-view"),
            published_surface_ref: None,
            status: AppSurfaceStatus::Active,
        };
        let approval = AppInstallationApproval {
            approval_id: reference("approval:1"),
            revision: AppRevision::new(1).unwrap(),
            authenticated_scope_ref: super::super::models::AppScopeBindingRef::parse("scope_1")
                .unwrap(),
            actor_ref: reference("actor:owner"),
            session_ref: reference("session:1"),
            authentication: AppApprovalAuthentication::AuthenticatedSession,
            authentication_revision: AppRevision::new(1).unwrap(),
            install_or_update_attempt_id: reference("attempt:1"),
            package_content_digest: package.content_digest.clone(),
            requested_authority_digest: package.requested_authority_digest.clone(),
            granted_authority_digest: grant.authority_digest.clone(),
            data_policy_diff_digest: canonical_contract_digest(&(
                &grant.requested_data_handling_policy,
                &grant.granted_data_handling_policy,
            ))
            .unwrap(),
            resource_diff_digest: canonical_contract_digest(&(
                &grant.requested_resource_ceiling,
                &grant.granted_resource_ceiling,
            ))
            .unwrap(),
            schema_diff_digest: canonical_contract_digest(&schema).unwrap(),
            migration_diff_digest: canonical_contract_digest(&(
                schema.compatibility_with_previous,
                &schema.migration_plan_ref,
            ))
            .unwrap(),
            global_policy_revision: AppRevision::new(7).unwrap(),
            workflow_material_bindings: Vec::new(),
            contribution_grants: Vec::new(),
            interactive_capability_grants: Vec::new(),
            issued_at: time(3),
            expires_at: time(20),
            consumed_at: None,
            consumed_installation_revision: None,
        };
        ReviewedFixture {
            approval,
            grant,
            schema,
            surface,
        }
    }

    fn reviewed_commit(
        authenticated: &AuthenticatedAppScope,
        fixture: &ReviewedFixture,
        event_id: &str,
        now: DateTime<Utc>,
    ) -> AppReviewedInstallationCommit {
        AppReviewedInstallationCommit::from_verified_review(
            authenticated,
            fixture.approval.clone(),
            AppLifecycleAttemptKind::InitialInstall,
            fixture.grant.clone(),
            fixture.schema.clone(),
            compiled_surface_set(fixture),
            reference(event_id),
            AppDigest::blake3(event_id.as_bytes()),
            AppRevision::new(7).unwrap(),
            &now,
        )
        .unwrap()
    }

    fn compiled_surface_set(fixture: &ReviewedFixture) -> CompiledAppSurfaceSet {
        compiled_surface_set_for_schema(fixture, fixture.schema.revision)
    }

    fn compiled_surface_set_for_schema(
        fixture: &ReviewedFixture,
        schema_revision: AppRevision,
    ) -> CompiledAppSurfaceSet {
        let manifest = parse_app_manifest_frontmatter(
            valid_skill_document().as_bytes(),
            &AppPackageLimits::default(),
        )
        .unwrap();
        let entity_digest = canonical_entity_schema_digest(manifest.manifest()).unwrap();
        let view_digest = canonical_view_schema_digest(manifest.manifest()).unwrap();
        let source = VerifiedAppSurfaceSource::for_test(
            manifest,
            fixture.schema.installation_id.clone(),
            fixture.schema.package_revision_ref.clone(),
            schema_revision,
            fixture.schema.created_at,
            entity_digest,
            view_digest,
        );
        compile_app_surface_set(&source, fixture.surface.surface_revision).unwrap()
    }

    #[test]
    fn reviewed_commit_rejects_a_generation_for_another_schema_revision() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let publication = publication(&workspace, "anonymous", "default", "attempt:1", "install_1");
        let fixture = reviewed_fixture(
            publication.package_revision(),
            publication.package_revision_ref().clone(),
        );
        let wrong_generation =
            compiled_surface_set_for_schema(&fixture, AppRevision::new(2).unwrap());
        let authenticated = authenticated_scope("anonymous", "default");

        assert!(matches!(
            AppReviewedInstallationCommit::from_verified_review(
                &authenticated,
                fixture.approval,
                AppLifecycleAttemptKind::InitialInstall,
                fixture.grant,
                fixture.schema,
                wrong_generation,
                reference("event:wrong-schema"),
                AppDigest::blake3(b"event:wrong-schema"),
                AppRevision::new(7).unwrap(),
                &time(4),
            ),
            Err(AppRegistryError::InvalidControlPlane(message))
                if message.contains("does not name the reviewed schema revision")
        ));
    }

    async fn enabled_service() -> (
        TempDir,
        AppRegistryService,
        AuthenticatedAppScope,
        ReviewedFixture,
    ) {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let service = AppRegistryService::new(workspace.clone());
        let authenticated = authenticated_scope("anonymous", "default");
        let policy_digest = canonical_contract_digest(&denied_policy()).unwrap();
        let publication = publication_with_requested_policy_digest(
            &workspace,
            "anonymous",
            "default",
            "attempt:1",
            "install_1",
            policy_digest,
        );
        let package = publication.package_revision().clone();
        let package_ref = publication.package_revision_ref().clone();
        service
            .publish_ready_for_review(&authenticated, publication, time(2))
            .await
            .unwrap();
        let initial_review = service
            .ready_for_review_attempt_for_installation(
                &authenticated,
                &AppInstallationId::parse("install_1").unwrap(),
                time(2),
            )
            .await
            .unwrap()
            .expect("the initial installation must remain reviewable");
        assert_eq!(initial_review.kind, AppLifecycleAttemptKind::InitialInstall);
        assert!(initial_review.source_installation_generation.is_none());
        let fixture = reviewed_fixture(&package, package_ref);
        service
            .publish_installation_approval(
                &authenticated,
                AppApprovalPublication::from_authenticated_decision(
                    &authenticated,
                    fixture.approval.clone(),
                    AppLifecycleAttemptKind::InitialInstall,
                    AppRevision::new(7).unwrap(),
                    &time(4),
                )
                .unwrap(),
                time(4),
            )
            .await
            .unwrap();
        service
            .commit_reviewed_installation(
                &authenticated,
                reviewed_commit(&authenticated, &fixture, "event:enabled", time(5)),
                time(5),
            )
            .await
            .unwrap();
        (temporary, service, authenticated, fixture)
    }

    #[tokio::test]
    async fn reviewed_commit_consumes_once_and_replay_verifies_every_durable_output() {
        let (_temporary, service, authenticated, fixture) = enabled_service().await;
        let installation = service
            .installation(
                &authenticated,
                &AppInstallationId::parse("install_1").unwrap(),
                time(6),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            installation.lifecycle.status,
            AppInstallationStatus::Enabled
        );
        assert_eq!(installation.lifecycle.generation, 2);
        assert_eq!(
            installation.grant_revision,
            Some(AppRevision::new(1).unwrap())
        );
        let (declared_members, stored_members): (i64, i64) = service
            .execute_scoped_read(&authenticated, &time(6), |connection, _| {
                let declared = connection.query_row(
                    "SELECT member_count FROM app_surface_generations
                      WHERE installation_id = 'install_1' AND revision = 1",
                    [],
                    |row| row.get(0),
                )?;
                let stored = connection.query_row(
                    "SELECT COUNT(*) FROM app_surface_generation_members
                      WHERE installation_id = 'install_1' AND revision = 1",
                    [],
                    |row| row.get(0),
                )?;
                Ok((declared, stored))
            })
            .await
            .unwrap()
            .unwrap();
        assert!(declared_members > 0);
        assert_eq!(declared_members, stored_members);

        let replay = service
            .commit_reviewed_installation(
                &authenticated,
                reviewed_commit(&authenticated, &fixture, "event:enabled", time(6)),
                time(6),
            )
            .await
            .unwrap();
        assert_eq!(replay.outcome, AppLifecycleMutationOutcome::AlreadyApplied);

        let leases = service
            .claim_lifecycle_outbox(
                &authenticated,
                reference("projector:directory"),
                10,
                StdDuration::from_secs(30),
                time(6),
            )
            .await
            .unwrap();
        assert_eq!(leases.len(), 1);
        assert_eq!(
            leases[0].event().event_kind,
            AppLifecycleEventKind::InstallationEnabled
        );
        service
            .acknowledge_lifecycle_outbox(
                &authenticated,
                leases.into_iter().next().unwrap(),
                reference("delivery:1"),
                time(7),
            )
            .await
            .unwrap();
        assert!(service
            .claim_lifecycle_outbox(
                &authenticated,
                reference("projector:directory"),
                10,
                StdDuration::from_secs(30),
                time(8),
            )
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn reviewed_reenable_replays_exact_outbox_after_service_restart() {
        let (temporary, service, authenticated, _fixture) = enabled_service().await;
        let installation_id = AppInstallationId::parse("install_1").unwrap();
        service
            .transition_installation(
                &authenticated,
                installation_id.clone(),
                2,
                AppInstallationCommand::Disable,
                reference("event:disable-before-reenable"),
                AppDigest::blake3(b"disable-before-reenable"),
                time(6),
            )
            .await
            .unwrap();
        let implementation_digest = AppDigest::blake3(b"reviewed-current-implementation");
        let identity = service
            .reenable_review_identity(
                &authenticated,
                &installation_id,
                implementation_digest,
                AppRevision::new(7).unwrap(),
                time(7),
            )
            .await
            .unwrap();
        let event_id = reference("event:reviewed-reenable");
        let idempotency_key = AppDigest::blake3(b"reviewed-reenable-request");
        let first = service
            .commit_reviewed_reenable(
                &authenticated,
                AppReviewedReenableCommit::from_current_review(
                    identity.clone(),
                    &identity.review_digest,
                    event_id.clone(),
                    idempotency_key.clone(),
                )
                .unwrap(),
                time(8),
            )
            .await
            .unwrap();
        assert_eq!(first.outcome, AppLifecycleMutationOutcome::Applied);
        assert_eq!(first.status, AppInstallationStatus::Enabled);
        assert_eq!(first.reenable_review_identity.as_ref(), Some(&identity));

        drop(service);
        let reopened = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let replay = reopened
            .commit_reviewed_reenable(
                &authenticated,
                AppReviewedReenableCommit::from_current_review(
                    identity.clone(),
                    &identity.review_digest,
                    event_id,
                    idempotency_key,
                )
                .unwrap(),
                time(9),
            )
            .await
            .unwrap();
        assert_eq!(replay.outcome, AppLifecycleMutationOutcome::AlreadyApplied);
        assert_eq!(replay.generation, first.generation);
        assert_eq!(replay.status, first.status);
        assert_eq!(replay.reenable_review_identity, Some(identity));
    }

    #[tokio::test]
    async fn reviewed_reenable_rejects_stale_review_after_grant_revocation() {
        let (_temporary, service, authenticated, _fixture) = enabled_service().await;
        let installation_id = AppInstallationId::parse("install_1").unwrap();
        service
            .transition_installation(
                &authenticated,
                installation_id.clone(),
                2,
                AppInstallationCommand::Disable,
                reference("event:disable-before-stale-review"),
                AppDigest::blake3(b"disable-before-stale-review"),
                time(6),
            )
            .await
            .unwrap();
        let identity = service
            .reenable_review_identity(
                &authenticated,
                &installation_id,
                AppDigest::blake3(b"current-implementation"),
                AppRevision::new(7).unwrap(),
                time(7),
            )
            .await
            .unwrap();
        service
            .revoke_active_grant(
                &authenticated,
                installation_id,
                3,
                AppRevision::new(1).unwrap(),
                reference("event:revoke-after-review"),
                AppDigest::blake3(b"revoke-after-review"),
                time(8),
            )
            .await
            .unwrap();
        let error = service
            .commit_reviewed_reenable(
                &authenticated,
                AppReviewedReenableCommit::from_current_review(
                    identity.clone(),
                    &identity.review_digest,
                    reference("event:stale-reenable"),
                    AppDigest::blake3(b"stale-reenable"),
                )
                .unwrap(),
                time(9),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AppRegistryError::GenerationConflict {
                expected: 3,
                actual: 4
            }
        ));
    }

    #[tokio::test]
    async fn stale_outbox_lease_cannot_ack_after_bounded_recovery_and_reclaim() {
        let (_temporary, service, authenticated, _fixture) = enabled_service().await;
        let lease = service
            .claim_lifecycle_outbox(
                &authenticated,
                reference("projector:old"),
                1,
                StdDuration::from_secs(2),
                time(6),
            )
            .await
            .unwrap()
            .pop()
            .unwrap();
        let recovered = service
            .recover_expired_lifecycle_outbox_leases(&authenticated, 1, time(9))
            .await
            .unwrap();
        assert_eq!(recovered.recovered_leases, 1);
        assert!(matches!(
            service
                .acknowledge_lifecycle_outbox(
                    &authenticated,
                    lease,
                    reference("delivery:stale"),
                    time(9),
                )
                .await,
            Err(AppRegistryError::OutboxLeaseStale)
        ));
        let reclaimed = service
            .claim_lifecycle_outbox(
                &authenticated,
                reference("projector:new"),
                1,
                StdDuration::from_secs(30),
                time(9),
            )
            .await
            .unwrap();
        assert_eq!(reclaimed.len(), 1);
        assert_eq!(reclaimed[0].attempt_count(), 2);
    }

    #[tokio::test]
    async fn lifecycle_outbox_rejects_an_oversized_ready_payload_before_blob_loading() {
        let (_temporary, service, authenticated, _fixture) = enabled_service().await;
        let oversized =
            i64::try_from(AppContractLimits::default().max_document_bytes()).unwrap() + 1;
        service
            .execute_scoped_write(&authenticated, &time(6), move |connection, _| {
                connection.execute(
                    "UPDATE app_lifecycle_outbox SET payload_json = zeroblob(?1)",
                    params![oversized],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(service
            .claim_lifecycle_outbox(
                &authenticated,
                reference("projector:bounded"),
                10,
                StdDuration::from_secs(30),
                time(7),
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn grant_revocation_and_quarantine_share_one_replayable_generation_event() {
        let (_temporary, service, authenticated, _fixture) = enabled_service().await;
        let first = service
            .revoke_active_grant(
                &authenticated,
                AppInstallationId::parse("install_1").unwrap(),
                2,
                AppRevision::new(1).unwrap(),
                reference("event:revoked"),
                AppDigest::blake3(b"event:revoked"),
                time(7),
            )
            .await
            .unwrap();
        assert_eq!(first.status, AppInstallationStatus::Quarantined);
        assert_eq!(first.generation, 3);
        let replay = service
            .revoke_active_grant(
                &authenticated,
                AppInstallationId::parse("install_1").unwrap(),
                2,
                AppRevision::new(1).unwrap(),
                reference("event:revoked"),
                AppDigest::blake3(b"event:revoked"),
                time(8),
            )
            .await
            .unwrap();
        assert_eq!(replay.outcome, AppLifecycleMutationOutcome::AlreadyApplied);
        assert_eq!(replay.generation, 3);
    }

    #[tokio::test]
    async fn disable_hides_computed_capabilities_before_outbox_claim() {
        let (_temporary, service, authenticated, _fixture) = enabled_service().await;
        let hidden = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&hidden);
        service.set_computed_capability_hide(std::sync::Arc::new(move |principal, workspace| {
            assert_eq!(principal, "anonymous");
            assert_eq!(workspace, "default");
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }));
        service
            .transition_installation(
                &authenticated,
                AppInstallationId::parse("install_1").unwrap(),
                2,
                AppInstallationCommand::Disable,
                reference("event:hidden"),
                AppDigest::blake3(b"event:hidden"),
                time(7),
            )
            .await
            .unwrap();
        assert!(
            hidden.load(std::sync::atomic::Ordering::SeqCst),
            "overlay hide must not wait for the lifecycle outbox claim"
        );
    }

    #[tokio::test]
    async fn outbox_identity_conflict_rolls_back_the_lifecycle_cas() {
        let (_temporary, service, authenticated, _fixture) = enabled_service().await;
        let error = service
            .transition_installation(
                &authenticated,
                AppInstallationId::parse("install_1").unwrap(),
                2,
                AppInstallationCommand::Disable,
                reference("event:enabled"),
                AppDigest::blake3(b"different-operation"),
                time(7),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AppRegistryError::Sqlite(_)));
        let installation = service
            .installation(
                &authenticated,
                &AppInstallationId::parse("install_1").unwrap(),
                time(8),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            installation.lifecycle.status,
            AppInstallationStatus::Enabled
        );
        assert_eq!(installation.lifecycle.generation, 2);
    }

    #[tokio::test]
    async fn update_attempt_publication_is_generation_bound_and_exactly_replayable() {
        let (temporary, service, authenticated, mut fixture) = enabled_service().await;
        service
            .transition_installation(
                &authenticated,
                AppInstallationId::parse("install_1").unwrap(),
                2,
                AppInstallationCommand::BeginUpdate,
                reference("event:update-began"),
                AppDigest::blake3(b"event:update-began"),
                time(7),
            )
            .await
            .unwrap();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let policy_digest = canonical_contract_digest(&denied_policy()).unwrap();
        let first = service
            .publish_reviewable_revision(
                &authenticated,
                reviewable_update_publication(
                    &workspace,
                    policy_digest.clone(),
                    "attempt:update-1",
                    2,
                ),
                time(9),
            )
            .await
            .unwrap();
        assert_eq!(first.outcome, AppRegistryPublicationOutcome::Created);
        let replay = service
            .publish_reviewable_revision(
                &authenticated,
                reviewable_update_publication(&workspace, policy_digest, "attempt:update-1", 2),
                time(10),
            )
            .await
            .unwrap();
        assert_eq!(
            replay.outcome,
            AppRegistryPublicationOutcome::AlreadyPresent
        );

        let current_grant = fixture.grant.clone();
        fixture.grant.revision = AppRevision::new(2).unwrap();
        fixture.grant.approved_at = time(11);
        let permission_diff = compute_permission_diff(&current_grant, &fixture.grant);
        let parked_generation = service
            .installation(
                &authenticated,
                &AppInstallationId::parse("install_1").unwrap(),
                time(10),
            )
            .await
            .unwrap()
            .unwrap()
            .lifecycle
            .generation;
        let coordinator = AppUpdateCoordinatorService::from_parts(
            service.clone(),
            AppPackageStager::new(workspace.clone()),
        );
        let plan = coordinator
            .prepare_plan(
                &authenticated,
                &AppInstallationId::parse("install_1").unwrap(),
                AppUpdatePlanRequest {
                    attempt_id: reference("attempt:update-1"),
                    expected_parked_generation: parked_generation,
                    migration_operations: Vec::new(),
                },
                permission_diff.clone(),
                fixture.grant.granted_data_handling_policy.clone(),
                time(10),
            )
            .await
            .unwrap();
        // The old grant is None. A reviewed remote destination must compile
        // independently of that source policy, and seal a different dry-run
        // identity even when the caller supplies the same coarse diff.
        let mut remote_policy = fixture.grant.granted_data_handling_policy.clone();
        remote_policy.model_processing = AppModelProcessing::RemoteAllowed;
        let remote_plan = coordinator
            .prepare_plan(
                &authenticated,
                &AppInstallationId::parse("install_1").unwrap(),
                AppUpdatePlanRequest {
                    attempt_id: reference("attempt:update-1"),
                    expected_parked_generation: parked_generation,
                    migration_operations: Vec::new(),
                },
                permission_diff.clone(),
                remote_policy.clone(),
                time(10),
            )
            .await
            .unwrap();
        assert_ne!(plan.migration_run_id, remote_plan.migration_run_id);
        assert_ne!(plan.schema_diff_digest, remote_plan.schema_diff_digest);
        let remote_run = coordinator
            .authorize_reviewed_switch(
                &authenticated,
                &AppInstallationId::parse("install_1").unwrap(),
                &reference("attempt:update-1"),
                &remote_plan.migration_run_id,
                &remote_plan.update_plan_digest,
                &permission_diff,
                false,
                time(10),
            )
            .await
            .unwrap();
        let manifest = parse_app_manifest_frontmatter(
            valid_skill_document().as_bytes(),
            &AppPackageLimits::default(),
        )
        .unwrap();
        let remote_schema = compile_app_schema(
            manifest.manifest(),
            AppInstallationId::parse("install_1").unwrap(),
            remote_run.destination_package_revision_ref.clone(),
            remote_run.destination_schema_revision,
            remote_policy,
            AppSchemaCompatibility::Compatible,
            None,
            &canonical_entity_schema_digest(manifest.manifest()).unwrap(),
            time(10),
        )
        .unwrap()
        .into_revision();
        assert_eq!(
            remote_run
                .destination_schema_preview
                .canonical_entity_schema,
            remote_schema.canonical_entity_schema,
        );
        let update_run = coordinator
            .authorize_reviewed_switch(
                &authenticated,
                &AppInstallationId::parse("install_1").unwrap(),
                &reference("attempt:update-1"),
                &plan.migration_run_id,
                &plan.update_plan_digest,
                &permission_diff,
                false,
                time(11),
            )
            .await
            .unwrap();
        fixture.schema = update_run.destination_schema_preview.clone();
        fixture.surface.surface_revision = AppRevision::new(2).unwrap();
        fixture.approval.approval_id = reference("approval:update-1");
        fixture.approval.install_or_update_attempt_id = reference("attempt:update-1");
        fixture.approval.issued_at = time(11);
        fixture.approval.expires_at = time(20);
        fixture.approval.data_policy_diff_digest = canonical_contract_digest(&(
            &fixture.grant.requested_data_handling_policy,
            &fixture.grant.granted_data_handling_policy,
        ))
        .unwrap();
        fixture.approval.resource_diff_digest = canonical_contract_digest(&(
            &fixture.grant.requested_resource_ceiling,
            &fixture.grant.granted_resource_ceiling,
        ))
        .unwrap();
        fixture.approval.schema_diff_digest = canonical_contract_digest(&fixture.schema).unwrap();
        fixture.approval.migration_diff_digest = canonical_contract_digest(&(
            fixture.schema.compatibility_with_previous,
            &fixture.schema.migration_plan_ref,
        ))
        .unwrap();
        service
            .publish_installation_approval(
                &authenticated,
                AppApprovalPublication::from_authenticated_decision(
                    &authenticated,
                    fixture.approval.clone(),
                    AppLifecycleAttemptKind::Update,
                    AppRevision::new(7).unwrap(),
                    &time(12),
                )
                .unwrap(),
                time(12),
            )
            .await
            .unwrap();
        let surfaces = compiled_surface_set(&fixture);
        let commit = AppReviewedInstallationCommit::from_verified_review(
            &authenticated,
            fixture.approval,
            AppLifecycleAttemptKind::Update,
            fixture.grant,
            fixture.schema,
            surfaces,
            reference("event:updated"),
            AppDigest::blake3(b"event:updated"),
            AppRevision::new(7).unwrap(),
            &time(13),
        )
        .unwrap()
        .with_update_authorization(Some(&update_run))
        .unwrap();
        let committed = service
            .commit_reviewed_installation(&authenticated, commit, time(13))
            .await
            .unwrap();
        assert_eq!(committed.status, AppInstallationStatus::Enabled);
        assert_eq!(committed.generation, 4);

        let installation_id = AppInstallationId::parse("install_1").unwrap();
        service
            .transition_installation(
                &authenticated,
                installation_id.clone(),
                4,
                AppInstallationCommand::BeginUpdate,
                reference("event:second-update"),
                AppDigest::blake3(b"second-update"),
                time(14),
            )
            .await
            .unwrap();
        service
            .publish_reviewable_revision(
                &authenticated,
                reviewable_update_publication(
                    &workspace,
                    canonical_contract_digest(&denied_policy()).unwrap(),
                    "attempt:update-2",
                    4,
                ),
                time(15),
            )
            .await
            .unwrap();
        assert!(service
            .ready_for_review_attempt_for_installation(&authenticated, &installation_id, time(15),)
            .await
            .unwrap()
            .is_some());
        service
            .transition_installation(
                &authenticated,
                installation_id.clone(),
                5,
                AppInstallationCommand::FailUpdate,
                reference("event:cancel-second-update"),
                AppDigest::blake3(b"cancel-second-update"),
                time(16),
            )
            .await
            .unwrap();
        service
            .transition_installation(
                &authenticated,
                installation_id.clone(),
                6,
                AppInstallationCommand::BeginUpdate,
                reference("event:third-update"),
                AppDigest::blake3(b"third-update"),
                time(17),
            )
            .await
            .unwrap();
        assert!(
            service
                .ready_for_review_attempt_for_installation(
                    &authenticated,
                    &installation_id,
                    time(18),
                )
                .await
                .unwrap()
                .is_none(),
            "the cancelled attempt cannot serve the new generation"
        );
        assert!(
            service
                .lifecycle_attempt(&authenticated, &reference("attempt:update-2"), time(18),)
                .await
                .unwrap()
                .is_some(),
            "cancelled review evidence must remain readable"
        );
        service
            .publish_reviewable_revision(
                &authenticated,
                reviewable_update_publication(
                    &workspace,
                    canonical_contract_digest(&denied_policy()).unwrap(),
                    "attempt:update-3",
                    6,
                ),
                time(19),
            )
            .await
            .unwrap();
        assert_eq!(
            service
                .ready_for_review_attempt_for_installation(
                    &authenticated,
                    &installation_id,
                    time(19),
                )
                .await
                .unwrap()
                .unwrap()
                .attempt_id,
            reference("attempt:update-3")
        );
    }

    #[test]
    fn approval_commit_and_outbox_lease_are_not_transport_constructible_or_cloneable() {
        static_assertions::assert_not_impl_any!(
            AppApprovalPublication: Clone, serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppReviewedInstallationCommit: Clone, serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppOutboxLease: Clone, serde::de::DeserializeOwned
        );
    }
}

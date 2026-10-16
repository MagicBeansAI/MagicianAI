//! One-shot Phase-0 approval boundary for reviewed install/update transitions.
//!
//! Durable compare-and-consume is a Phase-1 store responsibility. This dormant
//! contract ensures that its input is exact, current and session-bound and that
//! ordinary lifecycle callers cannot mistake a serialized approval for proof.

use chrono::{DateTime, Utc};
use thiserror::Error;

use super::{
    authority::{AppAuthorityError, AppScopeAuthentication, AuthenticatedAppScope},
    lifecycle::{AppInstallationCommand, AppInstallationLifecycle, AppLifecycleError},
    models::{
        AppContractError, AppContractLimits, AppDigest, AppReference, AppRevision,
        ValidateAppContract,
    },
    records::{
        AppApprovalAuthentication, AppInstallationApproval, AppLifecycleAttemptKind,
        AppReviewedWorkflowMaterialBinding,
    },
};

/// Reproduce the immutable approval identity from material shown during owner
/// review. Keeping this derivation in the core approval kernel lets workflow
/// launch/recovery revalidate a consumed approval without depending on the
/// route-facing `magician-apps` review service.
pub fn expected_app_installation_approval_ref(
    attempt_id: &AppReference,
    session_ref: &AppReference,
    granted_authority_digest: &AppDigest,
    workflow_material_bindings: &[AppReviewedWorkflowMaterialBinding],
) -> Result<AppReference, AppContractError> {
    let value = serde_json::to_value(workflow_material_bindings).map_err(|error| {
        AppContractError::invalid(
            "workflow_material_bindings",
            format!("cannot encode approval identity: {error}"),
        )
    })?;
    let workflow_material_digest = AppDigest::blake3_canonical_json(&value).map_err(|error| {
        AppContractError::invalid(
            "workflow_material_bindings",
            format!("cannot digest approval identity: {error}"),
        )
    })?;
    let prefix = "approval:app-install";
    let mut hasher = blake3::Hasher::new();
    hasher.update(prefix.as_bytes());
    for part in [
        attempt_id.as_str(),
        session_ref.as_str(),
        granted_authority_digest.as_str(),
        workflow_material_digest.as_str(),
    ] {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    AppReference::parse(format!("{prefix}:{}", hasher.finalize().to_hex()))
}

pub struct AppInstallationApprovalExpectation<'a> {
    pub approval_id: &'a AppReference,
    pub approval_revision: AppRevision,
    pub attempt_id: &'a AppReference,
    pub attempt_kind: AppLifecycleAttemptKind,
    pub package_content_digest: &'a AppDigest,
    pub requested_authority_digest: &'a AppDigest,
    pub granted_authority_digest: &'a AppDigest,
    pub data_policy_diff_digest: &'a AppDigest,
    pub resource_diff_digest: &'a AppDigest,
    pub schema_diff_digest: &'a AppDigest,
    pub migration_diff_digest: &'a AppDigest,
    pub global_policy_revision: AppRevision,
}

/// Move-only approval proof consumed by the reviewed lifecycle boundary.
/// It has no transport deserializer and cannot be cloned for two transitions.
#[derive(Debug, PartialEq, Eq)]
pub struct AppInstallationApprovalFence {
    approval_id: AppReference,
    approval_revision: AppRevision,
    attempt_id: AppReference,
    attempt_kind: AppLifecycleAttemptKind,
}

impl AppInstallationApprovalFence {
    pub fn approval_id(&self) -> &AppReference {
        &self.approval_id
    }

    pub fn approval_revision(&self) -> AppRevision {
        self.approval_revision
    }

    pub fn attempt_id(&self) -> &AppReference {
        &self.attempt_id
    }
}

pub fn authorize_installation_approval(
    authenticated: &AuthenticatedAppScope,
    approval: &AppInstallationApproval,
    expected: AppInstallationApprovalExpectation<'_>,
    now: &DateTime<Utc>,
) -> Result<AppInstallationApprovalFence, AppInstallationApprovalError> {
    authenticated.ensure_live_at(now)?;
    approval.validate_app_contract(&AppContractLimits::default())?;
    if approval.consumed_at.is_some() || approval.consumed_installation_revision.is_some() {
        return Err(AppInstallationApprovalError::AlreadyConsumed);
    }
    if now < &approval.issued_at || now >= &approval.expires_at {
        return Err(AppInstallationApprovalError::Expired);
    }
    if approval.authenticated_scope_ref != *authenticated.scope_binding_ref()
        || approval.actor_ref != *authenticated.actor_ref()
        || approval.session_ref != *authenticated.session_ref()
        || approval_authentication(authenticated.authentication()) != Some(approval.authentication)
        || approval.authentication_revision != authenticated.authentication_revision()
    {
        return Err(AppInstallationApprovalError::AuthenticationMismatch);
    }
    if approval.approval_id != *expected.approval_id
        || approval.revision != expected.approval_revision
        || approval.install_or_update_attempt_id != *expected.attempt_id
        || approval.package_content_digest != *expected.package_content_digest
        || approval.requested_authority_digest != *expected.requested_authority_digest
        || approval.granted_authority_digest != *expected.granted_authority_digest
        || approval.data_policy_diff_digest != *expected.data_policy_diff_digest
        || approval.resource_diff_digest != *expected.resource_diff_digest
        || approval.schema_diff_digest != *expected.schema_diff_digest
        || approval.migration_diff_digest != *expected.migration_diff_digest
        || approval.global_policy_revision != expected.global_policy_revision
    {
        return Err(AppInstallationApprovalError::DecisionMismatch);
    }
    Ok(AppInstallationApprovalFence {
        approval_id: approval.approval_id.clone(),
        approval_revision: approval.revision,
        attempt_id: approval.install_or_update_attempt_id.clone(),
        attempt_kind: expected.attempt_kind,
    })
}

pub fn apply_reviewed_installation_transition(
    current: &AppInstallationLifecycle,
    command: AppInstallationCommand,
    approval: AppInstallationApprovalFence,
) -> Result<AppInstallationLifecycle, AppInstallationApprovalError> {
    let expected_command = match approval.attempt_kind {
        AppLifecycleAttemptKind::InitialInstall => AppInstallationCommand::EnableReviewed,
        AppLifecycleAttemptKind::Update => AppInstallationCommand::CommitUpdate,
        AppLifecycleAttemptKind::Reinstall => AppInstallationCommand::CommitReviewedReinstall,
    };
    if command != expected_command {
        return Err(AppInstallationApprovalError::TransitionNotReviewBound);
    }
    current.apply(command).map_err(Into::into)
}

fn approval_authentication(value: AppScopeAuthentication) -> Option<AppApprovalAuthentication> {
    match value {
        AppScopeAuthentication::AuthenticatedSession => {
            Some(AppApprovalAuthentication::AuthenticatedSession)
        },
        AppScopeAuthentication::TrustedLoopbackSingleUser => {
            Some(AppApprovalAuthentication::TrustedLoopbackSingleUser)
        },
        AppScopeAuthentication::TrustedSystemPackageHost => {
            Some(AppApprovalAuthentication::TrustedSystemPackageHost)
        },
        // Background repair authority is deliberately incapable of consuming
        // interactive installation approval evidence.
        AppScopeAuthentication::SystemWorker
        | AppScopeAuthentication::TaskExecution
        | AppScopeAuthentication::ReviewedBackgroundLaunch => None,
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppInstallationApprovalError {
    #[error(transparent)]
    Authority(#[from] AppAuthorityError),
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error(transparent)]
    Lifecycle(#[from] AppLifecycleError),
    #[error("installation approval has already been consumed")]
    AlreadyConsumed,
    #[error("installation approval is expired or not yet live")]
    Expired,
    #[error("installation approval does not match the live authenticated session")]
    AuthenticationMismatch,
    #[error("installation approval does not match the current reviewed decision")]
    DecisionMismatch,
    #[error("installation approval fence was offered to a non-review lifecycle transition")]
    TransitionNotReviewBound,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::magician_v2::apps::{models::AppScopeBindingRef, records::AppScope};

    fn time(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 15, 0, 0, second)
            .single()
            .unwrap()
    }

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).unwrap()
    }

    fn revision(value: u64) -> AppRevision {
        AppRevision::new(value).unwrap()
    }

    fn digest(value: &str) -> AppDigest {
        AppDigest::blake3(value.as_bytes())
    }

    fn authenticated() -> AuthenticatedAppScope {
        AuthenticatedAppScope::from_verified_session(
            AppScope {
                principal: reference("anonymous"),
                workspace: reference("default"),
            },
            AppScopeBindingRef::parse("scope_anonymous_default").unwrap(),
            reference("actor:owner"),
            reference("session:1"),
            revision(4),
            time(0),
            time(20),
        )
        .unwrap()
    }

    fn approval() -> AppInstallationApproval {
        AppInstallationApproval {
            approval_id: reference("approval:1"),
            revision: revision(2),
            authenticated_scope_ref: AppScopeBindingRef::parse("scope_anonymous_default").unwrap(),
            actor_ref: reference("actor:owner"),
            session_ref: reference("session:1"),
            authentication: AppApprovalAuthentication::AuthenticatedSession,
            authentication_revision: revision(4),
            install_or_update_attempt_id: reference("attempt:1"),
            package_content_digest: digest("package"),
            requested_authority_digest: digest("requested"),
            granted_authority_digest: digest("granted"),
            data_policy_diff_digest: digest("data"),
            resource_diff_digest: digest("resource"),
            schema_diff_digest: digest("schema"),
            migration_diff_digest: digest("migration"),
            global_policy_revision: revision(9),
            workflow_material_bindings: Vec::new(),
            contribution_grants: Vec::new(),
            interactive_capability_grants: Vec::new(),
            issued_at: time(1),
            expires_at: time(10),
            consumed_at: None,
            consumed_installation_revision: None,
        }
    }

    fn expectation<'a>(
        approval: &'a AppInstallationApproval,
    ) -> AppInstallationApprovalExpectation<'a> {
        AppInstallationApprovalExpectation {
            approval_id: &approval.approval_id,
            approval_revision: approval.revision,
            attempt_id: &approval.install_or_update_attempt_id,
            attempt_kind: AppLifecycleAttemptKind::InitialInstall,
            package_content_digest: &approval.package_content_digest,
            requested_authority_digest: &approval.requested_authority_digest,
            granted_authority_digest: &approval.granted_authority_digest,
            data_policy_diff_digest: &approval.data_policy_diff_digest,
            resource_diff_digest: &approval.resource_diff_digest,
            schema_diff_digest: &approval.schema_diff_digest,
            migration_diff_digest: &approval.migration_diff_digest,
            global_policy_revision: approval.global_policy_revision,
        }
    }

    #[test]
    fn exact_current_approval_enables_one_reviewed_transition() {
        let authenticated = authenticated();
        let approval = approval();
        let fence = authorize_installation_approval(
            &authenticated,
            &approval,
            expectation(&approval),
            &time(2),
        )
        .unwrap();
        let enabled = apply_reviewed_installation_transition(
            &AppInstallationLifecycle::ready_for_review(),
            AppInstallationCommand::EnableReviewed,
            fence,
        )
        .unwrap();
        assert_eq!(enabled.generation, 2);
    }

    #[test]
    fn stale_replayed_and_cross_session_approvals_fail_closed() {
        let authenticated = authenticated();
        // Named `stale` rather than `approval`: a local binding of that name
        // shadows the `approval()` fixture for the rest of the function, and
        // the three cases below all call it again.
        let mut stale = approval();
        stale.global_policy_revision = revision(8);
        assert!(matches!(
            authorize_installation_approval(
                &authenticated,
                &stale,
                AppInstallationApprovalExpectation {
                    approval_revision: revision(3),
                    global_policy_revision: revision(9),
                    ..expectation(&stale)
                },
                &time(2),
            ),
            Err(AppInstallationApprovalError::DecisionMismatch)
        ));

        let mut replayed = approval();
        replayed.consumed_at = Some(time(3));
        replayed.consumed_installation_revision = Some(revision(7));
        assert!(matches!(
            authorize_installation_approval(
                &authenticated,
                &replayed,
                expectation(&replayed),
                &time(4),
            ),
            Err(AppInstallationApprovalError::AlreadyConsumed)
        ));

        let mut other_session = approval();
        other_session.session_ref = reference("session:other");
        assert!(matches!(
            authorize_installation_approval(
                &authenticated,
                &other_session,
                expectation(&other_session),
                &time(2),
            ),
            Err(AppInstallationApprovalError::AuthenticationMismatch)
        ));
        let mut other_scope = approval();
        other_scope.authenticated_scope_ref = AppScopeBindingRef::parse("scope_other").unwrap();
        assert!(matches!(
            authorize_installation_approval(
                &authenticated,
                &other_scope,
                expectation(&other_scope),
                &time(2),
            ),
            Err(AppInstallationApprovalError::AuthenticationMismatch)
        ));
        static_assertions::assert_not_impl_any!(
            AppInstallationApprovalFence: Clone, serde::de::DeserializeOwned
        );
    }

    #[test]
    fn approval_attempt_kind_cannot_authorize_another_reviewed_transition() {
        let authenticated = authenticated();
        let approval = approval();
        let fence = authorize_installation_approval(
            &authenticated,
            &approval,
            expectation(&approval),
            &time(2),
        )
        .unwrap();
        let pending = AppInstallationLifecycle {
            status: super::super::lifecycle::AppInstallationStatus::UpdatePending,
            generation: 2,
            update_return_status: Some(
                super::super::lifecycle::AppStableOperationalStatus::Enabled,
            ),
        };
        assert!(matches!(
            apply_reviewed_installation_transition(
                &pending,
                AppInstallationCommand::CommitUpdate,
                fence,
            ),
            Err(AppInstallationApprovalError::TransitionNotReviewBound)
        ));
    }
}

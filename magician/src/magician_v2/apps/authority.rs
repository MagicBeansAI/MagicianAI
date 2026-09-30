//! Pure app-authority resolution.
//!
//! This module has no request extraction, persistence, cache or dispatch side
//! effects. Auth adapters may mint [`AuthenticatedAppScope`], then a future
//! service can resolve an exact installation snapshot here before projecting
//! or dispatching anything. The same resolved value is intended for model
//! projection and the load-bearing dispatch/store checks.

use std::{collections::BTreeSet, net::IpAddr};

use chrono::{DateTime, Utc};
use serde::Serialize;
use thiserror::Error;

use super::{
    lifecycle::AppInstallationStatus,
    models::{
        AppContractLimits, AppDigest, AppInstallationId, AppReference, AppRevision,
        AppScopeBindingRef, ValidateAppContract,
    },
    records::{
        validate_authority_ceiling_parts, AppBackgroundExecution, AppDataHandlingPolicy,
        AppExternalEgress, AppGrantRevision, AppInstallation, AppNetworkPolicy, AppResourceCeiling,
        AppSchemaRevision, AppScope, AppSurfaceBinding, AppSurfaceStatus,
    },
    system_boot_admission::TrustedSystemPackageHostGrant,
};

/// Server-owned evidence class. This type is serializable for audit but is
/// intentionally not deserializable from any transport payload.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppScopeAuthentication {
    AuthenticatedSession,
    TrustedLoopbackSingleUser,
    /// A normal V3 task execution whose exact app binding was persisted before
    /// dispatch. This is minted only while revalidating that binding; it is not
    /// a general background-worker credential.
    TaskExecution,
    /// An exact, short-lived background launch whose server-owned token was
    /// validated against the reviewed action and its source input.
    ReviewedBackgroundLaunch,
    /// Server-owned background maintenance. This class is never selectable by
    /// a transport adapter and cannot authorize an interactive approval.
    SystemWorker,
    /// Host approval for one exact package proven to belong to the current
    /// digest-pinned system inventory. Only `system_boot_admission` can mint
    /// the move-only witness used to construct this class.
    TrustedSystemPackageHost,
}

/// An authenticated actor/session-to-scope binding.
///
/// Fields are private and the type has no `Deserialize` implementation, so an
/// app body, bridge message or model result cannot become this value merely by
/// naming a principal/workspace. Future request adapters must construct it only
/// after their own session verification.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthenticatedAppScope {
    scope: AppScope,
    scope_binding_ref: AppScopeBindingRef,
    actor_ref: AppReference,
    session_ref: AppReference,
    authentication: AppScopeAuthentication,
    authentication_revision: AppRevision,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trusted_system_package_installation: Option<AppInstallationId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    background_launch_installation: Option<AppInstallationId>,
}

impl AuthenticatedAppScope {
    /// Mint from an already authenticated server session. This is an adapter
    /// seam, not authentication by itself.
    pub fn from_verified_session(
        scope: AppScope,
        scope_binding_ref: AppScopeBindingRef,
        actor_ref: AppReference,
        session_ref: AppReference,
        authentication_revision: AppRevision,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, AppAuthorityError> {
        Self::new(
            scope,
            scope_binding_ref,
            actor_ref,
            session_ref,
            AppScopeAuthentication::AuthenticatedSession,
            authentication_revision,
            issued_at,
            expires_at,
        )
    }

    /// Development fallback allowed only for an actual loopback peer in an
    /// explicitly single-user deployment. Forwarded headers never enter this
    /// decision.
    pub fn from_trusted_loopback(
        peer_ip: IpAddr,
        single_user_deployment: bool,
        scope: AppScope,
        scope_binding_ref: AppScopeBindingRef,
        actor_ref: AppReference,
        session_ref: AppReference,
        authentication_revision: AppRevision,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, AppAuthorityError> {
        if !peer_ip.is_loopback() || !single_user_deployment {
            return Err(AppAuthorityError::UntrustedLoopbackFallback);
        }
        Self::new(
            scope,
            scope_binding_ref,
            actor_ref,
            session_ref,
            AppScopeAuthentication::TrustedLoopbackSingleUser,
            authentication_revision,
            issued_at,
            expires_at,
        )
    }

    /// Mint a short-lived, exact-scope capability for a server-owned worker.
    ///
    /// The type and all of its fields remain non-deserializable. Callers must
    /// derive `scope` from the canonical workspace layout rather than request
    /// input, and a fresh process/run reference prevents a stale worker from
    /// being mistaken for a current user session.
    pub fn from_system_worker(
        scope: AppScope,
        scope_binding_ref: AppScopeBindingRef,
        worker_ref: AppReference,
        run_ref: AppReference,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, AppAuthorityError> {
        let lifetime = expires_at.signed_duration_since(issued_at);
        if lifetime <= chrono::Duration::zero() || lifetime > chrono::Duration::minutes(10) {
            return Err(AppAuthorityError::InvalidSystemWorkerAuthenticationWindow);
        }
        let authentication_revision =
            AppRevision::new(1).map_err(|_| AppAuthorityError::InvalidAuthenticationRevision)?;
        Self::new(
            scope,
            scope_binding_ref,
            worker_ref,
            run_ref,
            AppScopeAuthentication::SystemWorker,
            authentication_revision,
            issued_at,
            expires_at,
        )
    }

    pub fn from_task_execution(
        scope: AppScope,
        scope_binding_ref: AppScopeBindingRef,
        execution_ref: AppReference,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, AppAuthorityError> {
        let lifetime = expires_at.signed_duration_since(issued_at);
        if lifetime <= chrono::Duration::zero() || lifetime > chrono::Duration::minutes(10) {
            return Err(AppAuthorityError::InvalidSystemWorkerAuthenticationWindow);
        }
        Self::new(
            scope,
            scope_binding_ref,
            execution_ref.clone(),
            execution_ref,
            AppScopeAuthentication::TaskExecution,
            AppRevision::new(1).map_err(|_| AppAuthorityError::InvalidAuthenticationRevision)?,
            issued_at,
            expires_at,
        )
    }

    pub(crate) fn from_reviewed_background_launch(
        worker: &Self,
        proof: super::workflows::ValidatedAppBackgroundLaunch,
        now: DateTime<Utc>,
    ) -> Result<Self, AppAuthorityError> {
        worker.ensure_live_at(&now)?;
        let (scope_binding_ref, installation_id, expires_at) = proof.into_scope_binding();
        if worker.authentication != AppScopeAuthentication::SystemWorker
            || scope_binding_ref != worker.scope_binding_ref
            || expires_at <= now
        {
            return Err(AppAuthorityError::SystemWorkerCannotExecuteApp);
        }
        let mut scope = Self::new(
            worker.scope.clone(),
            scope_binding_ref,
            worker.actor_ref.clone(),
            worker.session_ref.clone(),
            AppScopeAuthentication::ReviewedBackgroundLaunch,
            worker.authentication_revision,
            now,
            expires_at.min(worker.expires_at),
        )?;
        scope.background_launch_installation = Some(installation_id);
        Ok(scope)
    }

    /// Elevate one live system-admission worker to approve exactly the package
    /// named by an unforgeable boot-admission witness.
    ///
    /// This constructor is crate-private and consumes a witness whose fields
    /// are private to `system_boot_admission`; transport adapters, ordinary
    /// workers and package content cannot manufacture this authentication
    /// class. The exact installation binding prevents a valid witness for one
    /// seed package from being reused to approve another.
    pub(crate) fn from_trusted_system_package_host(
        system_worker: &Self,
        grant: TrustedSystemPackageHostGrant,
        now: DateTime<Utc>,
    ) -> Result<Self, AppAuthorityError> {
        system_worker.ensure_live_at(&now)?;
        if system_worker.authentication != AppScopeAuthentication::SystemWorker {
            return Err(AppAuthorityError::InvalidTrustedSystemPackageHostGrantor);
        }
        let mut authenticated = Self::new(
            system_worker.scope.clone(),
            system_worker.scope_binding_ref.clone(),
            system_worker.actor_ref.clone(),
            system_worker.session_ref.clone(),
            AppScopeAuthentication::TrustedSystemPackageHost,
            system_worker.authentication_revision,
            system_worker.issued_at,
            system_worker.expires_at,
        )?;
        authenticated.trusted_system_package_installation = Some(grant.into_installation_id());
        Ok(authenticated)
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        scope: AppScope,
        scope_binding_ref: AppScopeBindingRef,
        actor_ref: AppReference,
        session_ref: AppReference,
        authentication: AppScopeAuthentication,
        authentication_revision: AppRevision,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, AppAuthorityError> {
        if expires_at <= issued_at {
            return Err(AppAuthorityError::InvalidAuthenticationWindow);
        }
        Ok(Self {
            scope,
            scope_binding_ref,
            actor_ref,
            session_ref,
            authentication,
            authentication_revision,
            issued_at,
            expires_at,
            trusted_system_package_installation: None,
            background_launch_installation: None,
        })
    }

    pub fn scope(&self) -> &AppScope {
        &self.scope
    }

    pub fn scope_binding_ref(&self) -> &AppScopeBindingRef {
        &self.scope_binding_ref
    }

    pub fn actor_ref(&self) -> &AppReference {
        &self.actor_ref
    }

    pub fn session_ref(&self) -> &AppReference {
        &self.session_ref
    }

    pub fn authentication(&self) -> AppScopeAuthentication {
        self.authentication
    }

    pub fn authentication_revision(&self) -> AppRevision {
        self.authentication_revision
    }

    pub fn issued_at(&self) -> &DateTime<Utc> {
        &self.issued_at
    }

    pub fn expires_at(&self) -> &DateTime<Utc> {
        &self.expires_at
    }

    pub fn trusted_system_package_installation(&self) -> Option<&AppInstallationId> {
        self.trusted_system_package_installation.as_ref()
    }

    pub fn ensure_live_at(&self, now: &DateTime<Utc>) -> Result<(), AppAuthorityError> {
        if now < &self.issued_at || now >= &self.expires_at {
            return Err(AppAuthorityError::AuthenticationExpired);
        }
        Ok(())
    }
}

/// One hard authority ceiling. Empty sets and zero resource values mean deny;
/// they never mean unspecified or unlimited.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAuthorityCeiling {
    pub tools: BTreeSet<AppReference>,
    pub context_reads: BTreeSet<AppReference>,
    pub data_handling_policy: AppDataHandlingPolicy,
    pub background_execution: AppBackgroundExecution,
    pub network_policy: AppNetworkPolicy,
    pub resources: AppResourceCeiling,
}

impl AppAuthorityCeiling {
    fn validate(&self, limits: &AppContractLimits) -> Result<(), String> {
        validate_authority_ceiling_parts(
            self.tools.len(),
            self.context_reads.len(),
            &self.data_handling_policy,
            &self.background_execution,
            &self.network_policy,
            &self.resources,
            limits,
        )
        .map_err(|error| error.to_string())
    }
}

/// `parent` is `None` only when no parent execution exists. A present ceiling
/// whose sets/resources are empty/zero is an explicit parent denial.
pub struct AppAuthorityResolutionInput<'a> {
    pub authenticated_scope: &'a AuthenticatedAppScope,
    pub installation: &'a AppInstallation,
    pub grant: &'a AppGrantRevision,
    pub schema: &'a AppSchemaRevision,
    pub surface: Option<&'a AppSurfaceBinding>,
    pub agent: &'a AppAuthorityCeiling,
    pub trust: &'a AppAuthorityCeiling,
    pub parent: Option<&'a AppAuthorityCeiling>,
    pub now: DateTime<Utc>,
}

/// Immutable result used by both projection and consequential boundaries.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResolvedAppAuthority {
    pub scope_binding_ref: AppScopeBindingRef,
    pub actor_ref: AppReference,
    pub session_ref: AppReference,
    pub authentication: AppScopeAuthentication,
    pub authentication_revision: AppRevision,
    pub installation_id: AppInstallationId,
    pub installation_generation: u64,
    pub package_revision_ref: AppReference,
    pub grant_revision: AppRevision,
    /// Digest of the exact durable grant revision used for this resolution.
    /// This is distinct from `authority_digest`, which also commits to the
    /// authenticated actor/session and every effective authority ceiling.
    pub grant_authority_digest: AppDigest,
    pub schema_revision: AppRevision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface_revision: Option<AppRevision>,
    pub authority_digest: AppDigest,
    pub effective_tools: BTreeSet<AppReference>,
    pub effective_context_reads: BTreeSet<AppReference>,
    pub effective_data_handling_policy: AppDataHandlingPolicy,
    pub effective_background_execution: AppBackgroundExecution,
    pub effective_network_policy: AppNetworkPolicy,
    pub effective_resources: AppResourceCeiling,
    /// The owner's explicit "any public host" grant (`app_in_place_skill_v1`),
    /// carried by the grant alone and bound through `grant_authority_digest`
    /// (the grant's authority digest folds it in when set), so it is not a
    /// separate term of `authority_digest`: every resolution keeps its
    /// digest, and a resolution rebuilt from an accepted ceiling (which
    /// carries no network grant of its own) sees `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub effective_any_public_host: bool,
    pub resolved_at: DateTime<Utc>,
}

impl ResolvedAppAuthority {
    pub fn permits_tool(&self, tool: &AppReference) -> bool {
        self.effective_tools.contains(tool)
    }

    pub fn permits_context_read(&self, context: &AppReference) -> bool {
        self.effective_context_reads.contains(context)
    }

    /// Recompute the effective-authority identity from the complete admitted
    /// snapshot. Final dispatch boundaries use this before comparing durable
    /// grant/install/schema identity so an in-memory field mutation cannot be
    /// mistaken for a freshly resolved authority.
    pub fn canonical_authority_digest(&self) -> Result<AppDigest, AppAuthorityError> {
        canonical_effective_authority_digest(&ResolvedAppAuthorityDigestMaterial {
            scope_binding_ref: &self.scope_binding_ref,
            actor_ref: &self.actor_ref,
            session_ref: &self.session_ref,
            authentication: self.authentication,
            authentication_revision: self.authentication_revision,
            installation_id: &self.installation_id,
            installation_generation: self.installation_generation,
            package_revision_ref: &self.package_revision_ref,
            grant_revision: self.grant_revision,
            schema_revision: self.schema_revision,
            surface_revision: self.surface_revision,
            grant_authority_digest: &self.grant_authority_digest,
            effective_tools: &self.effective_tools,
            effective_context_reads: &self.effective_context_reads,
            effective_data_handling_policy: &self.effective_data_handling_policy,
            effective_background_execution: &self.effective_background_execution,
            effective_network_policy: &self.effective_network_policy,
            effective_resources: &self.effective_resources,
        })
    }
}

#[derive(Serialize)]
struct ResolvedAppAuthorityDigestMaterial<'a> {
    scope_binding_ref: &'a AppScopeBindingRef,
    actor_ref: &'a AppReference,
    session_ref: &'a AppReference,
    authentication: AppScopeAuthentication,
    authentication_revision: AppRevision,
    installation_id: &'a AppInstallationId,
    installation_generation: u64,
    package_revision_ref: &'a AppReference,
    grant_revision: AppRevision,
    schema_revision: AppRevision,
    surface_revision: Option<AppRevision>,
    grant_authority_digest: &'a AppDigest,
    effective_tools: &'a BTreeSet<AppReference>,
    effective_context_reads: &'a BTreeSet<AppReference>,
    effective_data_handling_policy: &'a AppDataHandlingPolicy,
    effective_background_execution: &'a AppBackgroundExecution,
    effective_network_policy: &'a AppNetworkPolicy,
    effective_resources: &'a AppResourceCeiling,
}

fn canonical_effective_authority_digest(
    material: &ResolvedAppAuthorityDigestMaterial<'_>,
) -> Result<AppDigest, AppAuthorityError> {
    let digest_bytes = serde_json::to_vec(material)
        .map_err(|error| AppAuthorityError::DigestEncoding(error.to_string()))?;
    Ok(AppDigest::blake3(&digest_bytes))
}

pub fn resolve_app_authority(
    input: AppAuthorityResolutionInput<'_>,
) -> Result<ResolvedAppAuthority, AppAuthorityError> {
    input.authenticated_scope.ensure_live_at(&input.now)?;
    if matches!(
        input.authenticated_scope.authentication(),
        AppScopeAuthentication::SystemWorker | AppScopeAuthentication::TrustedSystemPackageHost
    ) {
        return Err(AppAuthorityError::SystemWorkerCannotExecuteApp);
    }
    if input.authenticated_scope.authentication()
        == AppScopeAuthentication::ReviewedBackgroundLaunch
        && input
            .authenticated_scope
            .background_launch_installation
            .as_ref()
            != Some(&input.installation.installation_id)
    {
        return Err(AppAuthorityError::SystemWorkerCannotExecuteApp);
    }
    let limits = AppContractLimits::default();
    input
        .grant
        .validate_app_contract(&limits)
        .map_err(|error| AppAuthorityError::InvalidGrant(error.to_string()))?;
    input
        .schema
        .validate_app_contract(&limits)
        .map_err(|error| AppAuthorityError::InvalidSchema(error.to_string()))?;
    input
        .installation
        .validate_app_contract(&limits)
        .map_err(|error| AppAuthorityError::InvalidInstallation(error.to_string()))?;
    input
        .agent
        .validate(&limits)
        .map_err(|message| AppAuthorityError::InvalidCeiling {
            layer: "agent",
            message,
        })?;
    input
        .trust
        .validate(&limits)
        .map_err(|message| AppAuthorityError::InvalidCeiling {
            layer: "trust",
            message,
        })?;
    if let Some(parent) = input.parent {
        parent
            .validate(&limits)
            .map_err(|message| AppAuthorityError::InvalidCeiling {
                layer: "parent",
                message,
            })?;
    }

    if &input.installation.scope != input.authenticated_scope.scope() {
        return Err(AppAuthorityError::ScopeMismatch);
    }
    if input.installation.lifecycle.status != AppInstallationStatus::Enabled {
        return Err(AppAuthorityError::InstallationUnavailable {
            status: input.installation.lifecycle.status,
        });
    }
    if input.grant.revoked_at.is_some() {
        return Err(AppAuthorityError::GrantRevoked);
    }
    if &input.grant.approved_at > &input.now {
        return Err(AppAuthorityError::GrantNotYetApproved);
    }

    let expected_grant_revision = input
        .installation
        .grant_revision
        .ok_or(AppAuthorityError::StaleRevision("grant"))?;
    let expected_schema_revision = input
        .installation
        .active_schema_revision
        .ok_or(AppAuthorityError::StaleRevision("schema"))?;
    if input.grant.installation_id != input.installation.installation_id
        || input.grant.package_revision_ref != input.installation.package_revision_ref
        || input.grant.revision != expected_grant_revision
    {
        return Err(AppAuthorityError::StaleRevision("grant"));
    }
    if input.schema.installation_id != input.installation.installation_id
        || input.schema.package_revision_ref != input.installation.package_revision_ref
        || input.schema.revision != expected_schema_revision
    {
        return Err(AppAuthorityError::StaleRevision("schema"));
    }

    let surface_revision = match input.surface {
        Some(surface) => {
            surface
                .validate_app_contract(&limits)
                .map_err(|error| AppAuthorityError::InvalidSurface(error.to_string()))?;
            if surface.installation_id != input.installation.installation_id
                || surface.package_revision_ref != input.installation.package_revision_ref
                || Some(surface.surface_revision) != input.installation.active_surface_revision
                || surface.status != AppSurfaceStatus::Active
            {
                return Err(AppAuthorityError::StaleRevision("surface"));
            }
            Some(surface.surface_revision)
        },
        None => None,
    };

    let granted_tools = input.grant.granted_tools.iter().cloned().collect();
    let effective_tools = intersect_authority_sets(
        granted_tools,
        &input.agent.tools,
        &input.trust.tools,
        input.parent.map(|parent| &parent.tools),
    );
    let granted_context_reads = input.grant.granted_context_reads.iter().cloned().collect();
    let effective_context_reads = intersect_authority_sets(
        granted_context_reads,
        &input.agent.context_reads,
        &input.trust.context_reads,
        input.parent.map(|parent| &parent.context_reads),
    );
    let effective_data_handling_policy = intersect_data_policies(
        &input.grant.granted_data_handling_policy,
        &input.agent.data_handling_policy,
        &input.trust.data_handling_policy,
        input.parent.map(|parent| &parent.data_handling_policy),
    );
    let mut effective_background_execution = intersect_background_execution(
        &input.grant.granted_background_execution,
        &input.agent.background_execution,
        &input.trust.background_execution,
        input.parent.map(|parent| &parent.background_execution),
    );
    let effective_network_policy = intersect_network_policies(
        &input.grant.granted_network_policy,
        &input.agent.network_policy,
        &input.trust.network_policy,
        input.parent.map(|parent| &parent.network_policy),
    );
    let effective_resources = intersect_resource_ceilings(
        &input.grant.granted_resource_ceiling,
        &input.agent.resources,
        &input.trust.resources,
        input.parent.map(|parent| &parent.resources),
    );
    reconcile_background_with_resources(&mut effective_background_execution, &effective_resources);

    let digest_material = ResolvedAppAuthorityDigestMaterial {
        scope_binding_ref: input.authenticated_scope.scope_binding_ref(),
        actor_ref: input.authenticated_scope.actor_ref(),
        session_ref: input.authenticated_scope.session_ref(),
        authentication: input.authenticated_scope.authentication(),
        authentication_revision: input.authenticated_scope.authentication_revision(),
        installation_id: &input.installation.installation_id,
        installation_generation: input.installation.lifecycle.generation,
        package_revision_ref: &input.installation.package_revision_ref,
        grant_revision: input.grant.revision,
        schema_revision: input.schema.revision,
        surface_revision,
        grant_authority_digest: &input.grant.authority_digest,
        effective_tools: &effective_tools,
        effective_context_reads: &effective_context_reads,
        effective_data_handling_policy: &effective_data_handling_policy,
        effective_background_execution: &effective_background_execution,
        effective_network_policy: &effective_network_policy,
        effective_resources: &effective_resources,
    };
    let authority_digest = canonical_effective_authority_digest(&digest_material)?;
    let effective_any_public_host_allowed =
        effective_data_handling_policy.external_egress == AppExternalEgress::AnyPublicHost;

    Ok(ResolvedAppAuthority {
        scope_binding_ref: input.authenticated_scope.scope_binding_ref().clone(),
        actor_ref: input.authenticated_scope.actor_ref().clone(),
        session_ref: input.authenticated_scope.session_ref().clone(),
        authentication: input.authenticated_scope.authentication(),
        authentication_revision: input.authenticated_scope.authentication_revision(),
        installation_id: input.installation.installation_id.clone(),
        installation_generation: input.installation.lifecycle.generation,
        package_revision_ref: input.installation.package_revision_ref.clone(),
        grant_revision: input.grant.revision,
        grant_authority_digest: input.grant.authority_digest.clone(),
        schema_revision: input.schema.revision,
        surface_revision,
        authority_digest,
        effective_tools,
        effective_context_reads,
        effective_data_handling_policy,
        effective_background_execution,
        effective_network_policy,
        effective_resources,
        // Every ceiling must allow it: the owner's grant, and the intersected
        // data-handling policy of the grant (the app's reviewed request),
        // agent, trust and parent, which keeps `AnyPublicHost` only when all
        // of them carry it.
        effective_any_public_host: input.grant.granted_any_public_host
            && effective_any_public_host_allowed,
        resolved_at: input.now,
    })
}

fn intersect_authority_sets(
    mut effective: BTreeSet<AppReference>,
    agent: &BTreeSet<AppReference>,
    trust: &BTreeSet<AppReference>,
    parent: Option<&BTreeSet<AppReference>>,
) -> BTreeSet<AppReference> {
    effective.retain(|item| agent.contains(item) && trust.contains(item));
    if let Some(parent) = parent {
        effective.retain(|item| parent.contains(item));
    }
    effective
}

fn intersect_data_policies(
    grant: &AppDataHandlingPolicy,
    agent: &AppDataHandlingPolicy,
    trust: &AppDataHandlingPolicy,
    parent: Option<&AppDataHandlingPolicy>,
) -> AppDataHandlingPolicy {
    let policies = [Some(grant), Some(agent), Some(trust), parent];
    let classification_floor = policies
        .iter()
        .flatten()
        .map(|policy| policy.classification_floor)
        .max()
        .expect("grant, agent and trust policies are always present");
    let model_processing = policies
        .iter()
        .flatten()
        .map(|policy| policy.model_processing)
        .min()
        .expect("grant, agent and trust policies are always present");
    let personal_agent_access = policies
        .iter()
        .flatten()
        .map(|policy| policy.personal_agent_access)
        .min()
        .expect("grant, agent and trust policies are always present");
    let memory_promotion = policies
        .iter()
        .flatten()
        .map(|policy| policy.memory_promotion)
        .min()
        .expect("grant, agent and trust policies are always present");
    let mut external_egress = policies
        .iter()
        .flatten()
        .map(|policy| policy.external_egress)
        .min()
        .expect("grant, agent and trust policies are always present");
    let mut approved_destinations = grant
        .approved_destinations
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    for policy in [agent, trust].into_iter().chain(parent) {
        let ceiling = policy
            .approved_destinations
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        approved_destinations.retain(|destination| ceiling.contains(destination));
    }
    // Destination egress with no destination left is denial; "any public
    // host" stands on its own (it still needs the owner's explicit grant).
    if external_egress == AppExternalEgress::Denied
        || (external_egress == AppExternalEgress::ApprovedDestinations
            && approved_destinations.is_empty())
    {
        external_egress = AppExternalEgress::Denied;
        approved_destinations.clear();
    }
    AppDataHandlingPolicy {
        classification_floor,
        model_processing,
        personal_agent_access,
        memory_promotion,
        external_egress,
        approved_destinations: approved_destinations.into_iter().collect(),
    }
}

fn intersect_background_execution(
    grant: &AppBackgroundExecution,
    agent: &AppBackgroundExecution,
    trust: &AppBackgroundExecution,
    parent: Option<&AppBackgroundExecution>,
) -> AppBackgroundExecution {
    let mut min_interval_seconds = 0u64;
    let mut max_concurrent_runs = u16::MAX;
    for execution in [Some(grant), Some(agent), Some(trust), parent]
        .into_iter()
        .flatten()
    {
        let AppBackgroundExecution::Granted {
            min_interval_seconds: interval,
            max_concurrent_runs: concurrency,
        } = execution
        else {
            return AppBackgroundExecution::Denied;
        };
        min_interval_seconds = min_interval_seconds.max(*interval);
        max_concurrent_runs = max_concurrent_runs.min(*concurrency);
    }
    if min_interval_seconds == 0 || max_concurrent_runs == 0 {
        AppBackgroundExecution::Denied
    } else {
        AppBackgroundExecution::Granted {
            min_interval_seconds,
            max_concurrent_runs,
        }
    }
}

fn intersect_network_policies(
    grant: &AppNetworkPolicy,
    agent: &AppNetworkPolicy,
    trust: &AppNetworkPolicy,
    parent: Option<&AppNetworkPolicy>,
) -> AppNetworkPolicy {
    let AppNetworkPolicy::ApprovedDestinations { destinations } = grant else {
        return AppNetworkPolicy::Denied;
    };
    let mut effective = destinations.iter().cloned().collect::<BTreeSet<_>>();
    for policy in [Some(agent), Some(trust), parent].into_iter().flatten() {
        let AppNetworkPolicy::ApprovedDestinations { destinations } = policy else {
            return AppNetworkPolicy::Denied;
        };
        let ceiling = destinations.iter().cloned().collect::<BTreeSet<_>>();
        effective.retain(|destination| ceiling.contains(destination));
    }
    if effective.is_empty() {
        AppNetworkPolicy::Denied
    } else {
        AppNetworkPolicy::ApprovedDestinations {
            destinations: effective.into_iter().collect(),
        }
    }
}

fn intersect_resource_ceilings(
    grant: &AppResourceCeiling,
    agent: &AppResourceCeiling,
    trust: &AppResourceCeiling,
    parent: Option<&AppResourceCeiling>,
) -> AppResourceCeiling {
    let ceilings = [Some(grant), Some(agent), Some(trust), parent];
    let min = |select: fn(&AppResourceCeiling) -> u64| {
        ceilings
            .iter()
            .flatten()
            .map(|ceiling| select(ceiling))
            .min()
            .expect("grant, agent and trust resource ceilings are always present")
    };
    let min_u16 = |select: fn(&AppResourceCeiling) -> u16| {
        ceilings
            .iter()
            .flatten()
            .map(|ceiling| select(ceiling))
            .min()
            .expect("grant, agent and trust resource ceilings are always present")
    };
    AppResourceCeiling {
        max_input_tokens: min(|ceiling| ceiling.max_input_tokens),
        max_output_tokens: min(|ceiling| ceiling.max_output_tokens),
        max_cost_microusd: min(|ceiling| ceiling.max_cost_microusd),
        max_paid_tool_invocations: min(|ceiling| ceiling.max_paid_tool_invocations),
        max_active_seconds: min(|ceiling| ceiling.max_active_seconds),
        max_lifetime_seconds: min(|ceiling| ceiling.max_lifetime_seconds),
        max_browser_network_actions: min(|ceiling| ceiling.max_browser_network_actions),
        max_concurrent_foreground_runs: min_u16(|ceiling| ceiling.max_concurrent_foreground_runs),
        max_concurrent_background_runs: min_u16(|ceiling| ceiling.max_concurrent_background_runs),
        max_records: min(|ceiling| ceiling.max_records),
        max_payload_bytes: min(|ceiling| ceiling.max_payload_bytes),
        max_attachment_bytes: min(|ceiling| ceiling.max_attachment_bytes),
        max_monthly_tokens: min(|ceiling| ceiling.max_monthly_tokens),
        max_monthly_cost_microusd: min(|ceiling| ceiling.max_monthly_cost_microusd),
    }
}

fn reconcile_background_with_resources(
    background: &mut AppBackgroundExecution,
    resources: &AppResourceCeiling,
) {
    let AppBackgroundExecution::Granted {
        min_interval_seconds,
        max_concurrent_runs,
    } = background
    else {
        return;
    };
    let interval = *min_interval_seconds;
    let effective_concurrency =
        (*max_concurrent_runs).min(resources.max_concurrent_background_runs);
    *background = if effective_concurrency == 0 {
        AppBackgroundExecution::Denied
    } else {
        AppBackgroundExecution::Granted {
            min_interval_seconds: interval,
            max_concurrent_runs: effective_concurrency,
        }
    };
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppAuthorityError {
    #[error("trusted-loopback app fallback requires a real loopback peer and single-user mode")]
    UntrustedLoopbackFallback,
    #[error("app authentication expiry must be later than issue time")]
    InvalidAuthenticationWindow,
    #[error("system-worker authentication revision is invalid")]
    InvalidAuthenticationRevision,
    #[error("system-worker authentication lifetime must be positive and at most ten minutes")]
    InvalidSystemWorkerAuthenticationWindow,
    #[error("trusted system-package host authority requires a live system-admission worker")]
    InvalidTrustedSystemPackageHostGrantor,
    #[error("app authentication is not live at the resolution time")]
    AuthenticationExpired,
    #[error("system-worker maintenance authority cannot execute an app")]
    SystemWorkerCannotExecuteApp,
    #[error("authenticated app scope does not match the installation scope")]
    ScopeMismatch,
    #[error("app installation is unavailable in state {status:?}")]
    InstallationUnavailable { status: AppInstallationStatus },
    #[error("app grant has been revoked")]
    GrantRevoked,
    #[error("app grant approval is in the future")]
    GrantNotYetApproved,
    #[error("app authority carries a stale {0} revision")]
    StaleRevision(&'static str),
    #[error("invalid app grant: {0}")]
    InvalidGrant(String),
    #[error("invalid app schema: {0}")]
    InvalidSchema(String),
    #[error("invalid app installation: {0}")]
    InvalidInstallation(String),
    #[error("invalid app surface: {0}")]
    InvalidSurface(String),
    #[error("invalid {layer} app authority ceiling: {message}")]
    InvalidCeiling {
        layer: &'static str,
        message: String,
    },
    #[error("failed to encode app authority digest: {0}")]
    DigestEncoding(String),
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;
    use crate::magician_v2::apps::{
        lifecycle::AppInstallationLifecycle,
        models::{AppContractLimits, AppDataClassification, AppModelProcessing},
        records::{
            AppBackgroundExecution, AppDataHandlingPolicy, AppEntityProjectionGrant,
            AppExternalEgress, AppMemoryPromotion, AppNetworkPolicy, AppPersonalAgentAccess,
            AppResourceCeiling, AppSchemaCompatibility,
        },
    };

    fn time(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 14, 0, 0, second)
            .single()
            .unwrap()
    }

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).unwrap()
    }

    fn revision(value: u64) -> AppRevision {
        AppRevision::new(value).unwrap()
    }

    fn installation_id() -> AppInstallationId {
        AppInstallationId::parse("install_1").unwrap()
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
            revision(4),
            time(0),
            time(10),
        )
        .unwrap()
    }

    fn policy() -> AppDataHandlingPolicy {
        AppDataHandlingPolicy {
            classification_floor: AppDataClassification::Ordinary,
            model_processing: AppModelProcessing::None,
            personal_agent_access: AppPersonalAgentAccess::Denied,
            memory_promotion: AppMemoryPromotion::Denied,
            external_egress: AppExternalEgress::Denied,
            approved_destinations: Vec::new(),
        }
    }

    fn resources(value: u64) -> AppResourceCeiling {
        AppResourceCeiling {
            max_input_tokens: value,
            max_output_tokens: value,
            max_cost_microusd: value,
            max_paid_tool_invocations: value,
            max_active_seconds: value,
            max_lifetime_seconds: value,
            max_browser_network_actions: value,
            max_concurrent_foreground_runs: u16::try_from(value).unwrap(),
            max_concurrent_background_runs: u16::try_from(value).unwrap(),
            max_records: value,
            max_payload_bytes: value,
            max_attachment_bytes: value,
            max_monthly_tokens: value.saturating_mul(2),
            max_monthly_cost_microusd: value,
        }
    }

    fn grant() -> AppGrantRevision {
        AppGrantRevision {
            installation_id: installation_id(),
            revision: revision(2),
            package_revision_ref: reference("package:1"),
            requested_tools: vec![reference("tool:a"), reference("tool:b")],
            granted_tools: vec![reference("tool:a"), reference("tool:b")],
            requested_agents: Vec::new(),
            granted_agents: Vec::new(),
            requested_personalities: Vec::new(),
            granted_personalities: Vec::new(),
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
            requested_context_reads: vec![reference("context:own")],
            granted_context_reads: vec![reference("context:own")],
            requested_personal_agent_data_access: Vec::<AppEntityProjectionGrant>::new(),
            granted_personal_agent_data_access: Vec::new(),
            requested_data_handling_policy: policy(),
            granted_data_handling_policy: policy(),
            granted_data_handling_policy_digest: AppDigest::blake3(b"policy"),
            requested_background_execution: AppBackgroundExecution::Denied,
            granted_background_execution: AppBackgroundExecution::Denied,
            requested_network_policy: AppNetworkPolicy::Denied,
            granted_network_policy: AppNetworkPolicy::Denied,
            requested_resource_ceiling: resources(10),
            granted_resource_ceiling: resources(10),
            approved_by: reference("actor:owner"),
            approved_at: time(1),
            authority_digest: AppDigest::blake3(b"grant"),
            revoked_at: None,
        }
    }

    fn schema() -> AppSchemaRevision {
        AppSchemaRevision {
            installation_id: installation_id(),
            revision: revision(3),
            package_revision_ref: reference("package:1"),
            canonical_entity_schema: json!({"clip": {"title": "text"}}),
            canonical_data_handling_policy: policy(),
            compiled_validation_schema: json!({"type": "object"}),
            compiled_index_plan: json!({}),
            compatibility_with_previous: AppSchemaCompatibility::Initial,
            migration_plan_ref: None,
            created_at: time(1),
        }
    }

    fn installation() -> AppInstallation {
        AppInstallation {
            scope: scope(),
            installation_id: installation_id(),
            package_revision_ref: reference("package:1"),
            lifecycle: AppInstallationLifecycle {
                status: AppInstallationStatus::Enabled,
                generation: 7,
                update_return_status: None,
            },
            grant_revision: Some(revision(2)),
            active_schema_revision: Some(revision(3)),
            active_surface_revision: Some(revision(5)),
            created_at: time(0),
            updated_at: time(1),
            disabled_at: None,
            quarantined_at: None,
            uninstalled_at: None,
            purged_at: None,
        }
    }

    fn set(values: &[&str]) -> BTreeSet<AppReference> {
        values.iter().map(|value| reference(value)).collect()
    }

    fn authority_ceiling_with_resources(
        tools: &[&str],
        context_reads: &[&str],
        resource_limit: u64,
    ) -> AppAuthorityCeiling {
        AppAuthorityCeiling {
            tools: set(tools),
            context_reads: set(context_reads),
            data_handling_policy: policy(),
            background_execution: AppBackgroundExecution::Denied,
            network_policy: AppNetworkPolicy::Denied,
            resources: resources(resource_limit),
        }
    }

    fn authority_ceiling(tools: &[&str], context_reads: &[&str]) -> AppAuthorityCeiling {
        authority_ceiling_with_resources(tools, context_reads, 10)
    }

    /// "Any public host" is effective only when the owner granted it AND
    /// the app asked for it AND every ceiling (agent, trust, parent) keeps
    /// the `AnyPublicHost` egress level.
    #[test]
    fn any_public_host_needs_the_grant_the_request_and_every_ceiling() {
        let auth = authenticated_scope();
        let installation = installation();
        let schema = schema();
        let any_policy = || {
            let mut policy = policy();
            policy.external_egress = AppExternalEgress::AnyPublicHost;
            policy
        };
        let with_egress = |mut policy: AppDataHandlingPolicy, egress: AppExternalEgress| {
            policy.external_egress = egress;
            policy.approved_destinations = if egress == AppExternalEgress::Denied {
                Vec::new()
            } else {
                vec![reference("destination:a.example.com")]
            };
            policy
        };
        let ceiling = |egress: AppExternalEgress| {
            let mut ceiling = authority_ceiling(&["tool:a", "tool:b"], &["context:own"]);
            ceiling.data_handling_policy = with_egress(ceiling.data_handling_policy, egress);
            ceiling
        };
        let resolve = |granted: bool,
                       requested: AppExternalEgress,
                       agent: AppExternalEgress,
                       trust: AppExternalEgress,
                       parent: Option<AppExternalEgress>| {
            let mut grant = grant();
            grant.granted_any_public_host = granted;
            grant.requested_data_handling_policy = with_egress(any_policy(), requested);
            grant.granted_data_handling_policy = grant.requested_data_handling_policy.clone();
            let parent = parent.map(ceiling);
            resolve_app_authority(AppAuthorityResolutionInput {
                authenticated_scope: &auth,
                installation: &installation,
                grant: &grant,
                schema: &schema,
                surface: None,
                agent: &ceiling(agent),
                trust: &ceiling(trust),
                parent: parent.as_ref(),
                now: time(2),
            })
            .unwrap()
        };
        use AppExternalEgress::{AnyPublicHost as Any, ApprovedDestinations as Named, Denied};
        let all = resolve(true, Any, Any, Any, Some(Any));
        assert!(all.effective_any_public_host);
        assert_eq!(all.effective_data_handling_policy.external_egress, Any);
        // Any one missing layer switches it off.
        assert!(
            !resolve(false, Any, Any, Any, None).effective_any_public_host,
            "no grant"
        );
        assert!(
            !resolve(true, Named, Any, Any, None).effective_any_public_host,
            "not requested"
        );
        assert!(
            !resolve(true, Any, Named, Any, None).effective_any_public_host,
            "agent ceiling"
        );
        assert!(
            !resolve(true, Any, Any, Denied, None).effective_any_public_host,
            "trust ceiling"
        );
        assert!(
            !resolve(true, Any, Any, Any, Some(Named)).effective_any_public_host,
            "parent ceiling"
        );
        // An absent axis keeps every existing authority digest.
        let plain = resolve(false, Denied, Denied, Denied, None);
        let json = serde_json::to_value(&plain).unwrap();
        assert!(json.get("effective_any_public_host").is_none());
    }

    #[test]
    fn every_authority_layer_only_narrows_and_empty_parent_denies_all() {
        let auth = authenticated_scope();
        let installation = installation();
        let grant = grant();
        let schema = schema();
        let agent = authority_ceiling(&["tool:a", "tool:b"], &["context:own"]);
        let trust = authority_ceiling(&["tool:a"], &["context:own"]);
        let parent = authority_ceiling_with_resources(&[], &["context:own"], 0);
        let resolved = resolve_app_authority(AppAuthorityResolutionInput {
            authenticated_scope: &auth,
            installation: &installation,
            grant: &grant,
            schema: &schema,
            surface: None,
            agent: &agent,
            trust: &trust,
            parent: Some(&parent),
            now: time(2),
        })
        .unwrap();
        assert!(resolved.effective_tools.is_empty());
        assert!(resolved.permits_context_read(&reference("context:own")));
        assert_eq!(resolved.effective_resources.max_input_tokens, 0);
        assert_eq!(
            resolved.effective_background_execution,
            AppBackgroundExecution::Denied
        );
    }

    #[test]
    fn copied_scope_names_stale_revisions_and_disabled_state_fail_closed() {
        let auth = authenticated_scope();
        let mut installation = installation();
        installation.scope.workspace = reference("other");
        let grant = grant();
        let schema = schema();
        let ceiling = authority_ceiling(&["tool:a", "tool:b"], &["context:own"]);
        let resolve = |installation: &AppInstallation| {
            resolve_app_authority(AppAuthorityResolutionInput {
                authenticated_scope: &auth,
                installation,
                grant: &grant,
                schema: &schema,
                surface: None,
                agent: &ceiling,
                trust: &ceiling,
                parent: None,
                now: time(2),
            })
        };
        assert_eq!(
            resolve(&installation),
            Err(AppAuthorityError::ScopeMismatch)
        );

        installation.scope = scope();
        installation.grant_revision = Some(revision(99));
        assert_eq!(
            resolve(&installation),
            Err(AppAuthorityError::StaleRevision("grant"))
        );

        installation.grant_revision = Some(revision(2));
        installation.lifecycle.status = AppInstallationStatus::Disabled;
        installation.disabled_at = Some(time(2));
        assert!(matches!(
            resolve(&installation),
            Err(AppAuthorityError::InstallationUnavailable { .. })
        ));
    }

    #[test]
    fn loopback_fallback_cannot_be_enabled_by_forwarded_or_nonlocal_identity() {
        let args = || {
            (
                scope(),
                AppScopeBindingRef::parse("scope_1").unwrap(),
                reference("actor:owner"),
                reference("session:loopback"),
                revision(1),
                time(0),
                time(2),
            )
        };
        let (scope, binding, actor, session, revision, issued, expires) = args();
        assert_eq!(
            AuthenticatedAppScope::from_trusted_loopback(
                "192.0.2.1".parse().unwrap(),
                true,
                scope,
                binding,
                actor,
                session,
                revision,
                issued,
                expires,
            ),
            Err(AppAuthorityError::UntrustedLoopbackFallback)
        );
        let (scope, binding, actor, session, revision, issued, expires) = args();
        assert_eq!(
            AuthenticatedAppScope::from_trusted_loopback(
                "127.0.0.1".parse().unwrap(),
                false,
                scope,
                binding,
                actor,
                session,
                revision,
                issued,
                expires,
            ),
            Err(AppAuthorityError::UntrustedLoopbackFallback)
        );
    }

    #[test]
    fn system_worker_authority_is_exact_scoped_short_lived_and_not_transport_mintable() {
        let worker = AuthenticatedAppScope::from_system_worker(
            scope(),
            AppScopeBindingRef::parse("scope_worker").unwrap(),
            reference("worker:app-projection"),
            reference("run:boot-1"),
            time(1),
            time(3),
        )
        .unwrap();
        assert_eq!(worker.scope(), &scope());
        assert_eq!(
            worker.authentication(),
            AppScopeAuthentication::SystemWorker
        );
        assert!(worker.ensure_live_at(&time(1)).is_ok());
        assert_eq!(
            worker.ensure_live_at(&time(3)),
            Err(AppAuthorityError::AuthenticationExpired)
        );
        assert_eq!(
            AuthenticatedAppScope::from_system_worker(
                scope(),
                AppScopeBindingRef::parse("scope_worker").unwrap(),
                reference("worker:app-projection"),
                reference("run:boot-2"),
                time(1),
                time(1) + chrono::Duration::minutes(11),
            ),
            Err(AppAuthorityError::InvalidSystemWorkerAuthenticationWindow)
        );

        // The public representation can be audited, but the authority type has
        // no Deserialize implementation. Its serialized class therefore cannot
        // be fed through either request adapter to select SystemWorker.
        let encoded = serde_json::to_value(&worker).unwrap();
        assert_eq!(encoded["authentication"], "system_worker");
        static_assertions::assert_not_impl_any!(
            AuthenticatedAppScope: serde::de::DeserializeOwned
        );

        let ceiling = authority_ceiling(&["tool:a", "tool:b"], &["context:own"]);
        assert!(matches!(
            resolve_app_authority(AppAuthorityResolutionInput {
                authenticated_scope: &worker,
                installation: &installation(),
                grant: &grant(),
                schema: &schema(),
                surface: None,
                agent: &ceiling,
                trust: &ceiling,
                parent: None,
                now: time(2),
            }),
            Err(AppAuthorityError::SystemWorkerCannotExecuteApp)
        ));
    }

    #[test]
    fn reviewed_background_scope_cannot_cross_installations_or_ignore_revocation() {
        let mut authenticated = authenticated_scope();
        authenticated.authentication = AppScopeAuthentication::ReviewedBackgroundLaunch;
        authenticated.background_launch_installation = Some(installation().installation_id);
        let ceiling = authority_ceiling(&["tool:a", "tool:b"], &["context:own"]);
        let resolve = |installation: &AppInstallation| {
            resolve_app_authority(AppAuthorityResolutionInput {
                authenticated_scope: &authenticated,
                installation,
                grant: &grant(),
                schema: &schema(),
                surface: None,
                agent: &ceiling,
                trust: &ceiling,
                parent: None,
                now: time(2),
            })
        };
        let mut target = installation();
        assert!(resolve(&target).is_ok());
        target.installation_id = AppInstallationId::parse("install-other").unwrap();
        assert!(matches!(
            resolve(&target),
            Err(AppAuthorityError::SystemWorkerCannotExecuteApp)
        ));
        target = installation();
        target.lifecycle.status = AppInstallationStatus::Disabled;
        target.disabled_at = Some(time(2));
        assert!(matches!(
            resolve(&target),
            Err(AppAuthorityError::InstallationUnavailable { .. })
        ));
    }

    #[test]
    fn grant_and_schema_contracts_are_revalidated_at_resolution() {
        let mut grant = grant();
        grant.granted_tools.push(reference("tool:forged"));
        assert!(grant
            .validate_app_contract(&AppContractLimits::default())
            .is_err());

        let auth = authenticated_scope();
        let installation = installation();
        let schema = schema();
        let ceiling = authority_ceiling(&["tool:a", "tool:b", "tool:forged"], &["context:own"]);
        assert!(matches!(
            resolve_app_authority(AppAuthorityResolutionInput {
                authenticated_scope: &auth,
                installation: &installation,
                grant: &grant,
                schema: &schema,
                surface: None,
                agent: &ceiling,
                trust: &ceiling,
                parent: None,
                now: time(2),
            }),
            Err(AppAuthorityError::InvalidGrant(_))
        ));
    }

    #[test]
    fn every_independent_authority_ceiling_is_revalidated_before_intersection() {
        let auth = authenticated_scope();
        let installation = installation();
        let grant = grant();
        let schema = schema();
        let valid = authority_ceiling(&["tool:a", "tool:b"], &["context:own"]);
        let mut invalid_agent = valid.clone();
        invalid_agent.background_execution = AppBackgroundExecution::Granted {
            min_interval_seconds: 60,
            max_concurrent_runs: 2,
        };
        invalid_agent.resources.max_concurrent_background_runs = 1;

        assert!(matches!(
            resolve_app_authority(AppAuthorityResolutionInput {
                authenticated_scope: &auth,
                installation: &installation,
                grant: &grant,
                schema: &schema,
                surface: None,
                agent: &invalid_agent,
                trust: &valid,
                parent: None,
                now: time(2),
            }),
            Err(AppAuthorityError::InvalidCeiling { layer: "agent", .. })
        ));
        assert!(matches!(
            resolve_app_authority(AppAuthorityResolutionInput {
                authenticated_scope: &auth,
                installation: &installation,
                grant: &grant,
                schema: &schema,
                surface: None,
                agent: &valid,
                trust: &invalid_agent,
                parent: None,
                now: time(2),
            }),
            Err(AppAuthorityError::InvalidCeiling { layer: "trust", .. })
        ));
        assert!(matches!(
            resolve_app_authority(AppAuthorityResolutionInput {
                authenticated_scope: &auth,
                installation: &installation,
                grant: &grant,
                schema: &schema,
                surface: None,
                agent: &valid,
                trust: &valid,
                parent: Some(&invalid_agent),
                now: time(2),
            }),
            Err(AppAuthorityError::InvalidCeiling {
                layer: "parent",
                ..
            })
        ));
    }

    #[test]
    fn final_background_authority_cannot_exceed_the_effective_resource_axis() {
        let mut background = AppBackgroundExecution::Granted {
            min_interval_seconds: 60,
            max_concurrent_runs: 4,
        };
        let mut zero_resources = resources(1);
        zero_resources.max_concurrent_background_runs = 0;
        reconcile_background_with_resources(&mut background, &zero_resources);
        assert_eq!(background, AppBackgroundExecution::Denied);
    }

    #[test]
    fn exact_surface_revision_is_fenced_when_a_surface_is_present() {
        let auth = authenticated_scope();
        let installation = installation();
        let grant = grant();
        let schema = schema();
        let surface = AppSurfaceBinding {
            installation_id: installation_id(),
            surface_revision: revision(4),
            package_revision_ref: reference("package:1"),
            app_local_route: "/".to_owned(),
            canonical_host_route: "/apps/install_1".to_owned(),
            view_id: crate::magician_v2::apps::models::AppName::parse("home").unwrap(),
            compiled_view_digest: AppDigest::blake3(b"view"),
            published_surface_ref: None,
            status: AppSurfaceStatus::Active,
        };
        let ceiling = authority_ceiling(&["tool:a", "tool:b"], &["context:own"]);
        assert_eq!(
            resolve_app_authority(AppAuthorityResolutionInput {
                authenticated_scope: &auth,
                installation: &installation,
                grant: &grant,
                schema: &schema,
                surface: Some(&surface),
                agent: &ceiling,
                trust: &ceiling,
                parent: None,
                now: time(2),
            }),
            Err(AppAuthorityError::StaleRevision("surface"))
        );
    }

    #[test]
    fn authority_digest_changes_with_any_effective_ceiling() {
        let auth = authenticated_scope();
        let installation = installation();
        let grant = grant();
        let schema = schema();
        let all_tools = authority_ceiling(&["tool:a", "tool:b"], &["context:own"]);
        let one_tool = authority_ceiling(&["tool:a"], &["context:own"]);
        let resolve = |trust: &AppAuthorityCeiling| {
            resolve_app_authority(AppAuthorityResolutionInput {
                authenticated_scope: &auth,
                installation: &installation,
                grant: &grant,
                schema: &schema,
                surface: None,
                agent: &all_tools,
                trust,
                parent: None,
                now: time(2),
            })
            .unwrap()
        };
        let broad = resolve(&all_tools);
        let narrow = resolve(&one_tool);
        assert_eq!(broad.grant_authority_digest, grant.authority_digest);
        assert_eq!(
            broad.canonical_authority_digest().unwrap(),
            broad.authority_digest
        );
        assert_ne!(broad.grant_authority_digest, broad.authority_digest);
        assert_ne!(broad.authority_digest, narrow.authority_digest);

        let mut tampered = broad.clone();
        tampered.effective_resources.max_input_tokens -= 1;
        assert_ne!(
            tampered.canonical_authority_digest().unwrap(),
            tampered.authority_digest
        );
    }

    #[test]
    fn non_tool_authority_axes_intersect_without_special_unlimited_values() {
        let mut granted_policy = policy();
        granted_policy.classification_floor = AppDataClassification::Ordinary;
        granted_policy.model_processing = AppModelProcessing::RemoteAllowed;
        granted_policy.external_egress = AppExternalEgress::ApprovedDestinations;
        granted_policy.approved_destinations = vec![reference("dest:a"), reference("dest:b")];

        let mut agent_policy = granted_policy.clone();
        agent_policy.classification_floor = AppDataClassification::Sensitive;
        agent_policy.model_processing = AppModelProcessing::LocalOnly;
        agent_policy.approved_destinations = vec![reference("dest:b"), reference("dest:c")];
        let mut trust_policy = granted_policy.clone();
        trust_policy.approved_destinations = vec![reference("dest:b")];
        let effective =
            intersect_data_policies(&granted_policy, &agent_policy, &trust_policy, None);
        assert_eq!(
            effective.classification_floor,
            AppDataClassification::Sensitive
        );
        assert_eq!(effective.model_processing, AppModelProcessing::LocalOnly);
        assert_eq!(effective.approved_destinations, vec![reference("dest:b")]);

        assert_eq!(
            intersect_background_execution(
                &AppBackgroundExecution::Granted {
                    min_interval_seconds: 60,
                    max_concurrent_runs: 4,
                },
                &AppBackgroundExecution::Granted {
                    min_interval_seconds: 120,
                    max_concurrent_runs: 2,
                },
                &AppBackgroundExecution::Granted {
                    min_interval_seconds: 90,
                    max_concurrent_runs: 1,
                },
                None,
            ),
            AppBackgroundExecution::Granted {
                min_interval_seconds: 120,
                max_concurrent_runs: 1,
            }
        );

        assert_eq!(
            intersect_network_policies(
                &AppNetworkPolicy::ApprovedDestinations {
                    destinations: vec![reference("dest:a"), reference("dest:b")],
                },
                &AppNetworkPolicy::ApprovedDestinations {
                    destinations: vec![reference("dest:b")],
                },
                &AppNetworkPolicy::ApprovedDestinations {
                    destinations: vec![reference("dest:b"), reference("dest:c")],
                },
                None,
            ),
            AppNetworkPolicy::ApprovedDestinations {
                destinations: vec![reference("dest:b")],
            }
        );

        assert_eq!(
            intersect_resource_ceilings(&resources(10), &resources(7), &resources(4), None)
                .max_input_tokens,
            4
        );
    }

    #[test]
    fn expired_scope_is_rejected_before_installation_resolution() {
        let auth = authenticated_scope();
        let installation = installation();
        let grant = grant();
        let schema = schema();
        let ceiling = authority_ceiling(&["tool:a", "tool:b"], &["context:own"]);
        assert_eq!(
            resolve_app_authority(AppAuthorityResolutionInput {
                authenticated_scope: &auth,
                installation: &installation,
                grant: &grant,
                schema: &schema,
                surface: None,
                agent: &ceiling,
                trust: &ceiling,
                parent: None,
                now: time(10),
            }),
            Err(AppAuthorityError::AuthenticationExpired)
        );
    }
}

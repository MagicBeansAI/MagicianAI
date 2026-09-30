//! Owner review and enablement for a `ready_for_review` installation.
//!
//! Candidate publication is inert. This module is the missing owner surface:
//! it hydrates the requested grant from the staged package, lets the owner
//! subset tools/agents/personalities, then publishes approval evidence and
//! consumes it through the existing [`AppReviewedInstallationCommit`] kernel.
//! There is no second lifecycle.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    agent_capability::{
        agent_definition_permits_app_task, seal_reviewed_workflow_material, AppAgentCapabilityError,
    },
    approval_boundary::AppInstallationApprovalError,
    authoring_catalog::{
        resolve_authoring_primitive_catalog, AuthoringDiscoveryRoots, DEFAULT_APP_WORKFLOW_AGENT,
    },
    authority::{AppAuthorityError, AppScopeAuthentication, AuthenticatedAppScope},
    custom_surface_review::{
        reviewed_custom_surface_request, AppCustomSurfaceEntryGrantRequest,
        AppReviewedCustomSurfaceEntryPoint, AppReviewedCustomSurfaceRequest,
    },
    interactive::{AppInteractiveCapabilityRequest, AppReviewedInteractiveCapabilityGrant},
    lifecycle::AppInstallationStatus,
    manifest::{
        event_behavior_projection_schema, AppManifestBehaviorInputSelector, AppManifestBody,
        AppManifestError, AppManifestInputSchema, AppManifestPolicy, AppManifestResources,
        AppManifestTrigger, AppPackageManifest,
    },
    models::{AppContractError, AppDigest, AppInstallationId, AppName, AppReference, AppRevision},
    package_lock::{
        authorize_locked_primitive, AppLockedPrimitiveBinding, AppPackageLock, AppPackageLockError,
    },
    package_staging::{AppPackageStager, AppPackageStagingError, StagedAppPackage},
    primitive_catalog::{
        AppPrimitiveContainment, AppPrimitiveDispatchStatus, AppPrimitiveEffect,
        AppPrimitiveEligibilityStatus, AppPrimitiveExecutionClass, AppPrimitiveInvocationMode,
        AppPrimitiveKind, AppPrimitiveSchemaState, AppPrimitiveSourceKind,
    },
    records::{
        app_behavior_request_digest, app_event_behavior_request_digest,
        app_event_subscription_digest, app_granted_authority_digest,
        app_notification_request_digest, AppApprovalAuthentication, AppBackgroundExecution,
        AppBehaviorGrant, AppBehaviorResourceCeiling, AppContributionDestinationBinding,
        AppContributionFrequency, AppContributionSource, AppDataHandlingPolicy,
        AppEventBehaviorGrant, AppExternalEgress, AppGrantRevision,
        AppGrantedCustomSurfaceEntryPoint, AppInstallation, AppInstallationApproval,
        AppLifecycleAttempt, AppLifecycleAttemptKind, AppNetworkPolicy, AppNotificationGrant,
        AppNotificationSeverityV1, AppPackageRevision, AppResourceCeiling,
        AppReviewedContributionPortGrant, AppReviewedWorkflowMaterialBinding,
        AppSchemaCompatibility,
    },
    registry::{AppRegistryError, AppRegistryService},
    registry_lifecycle::{
        AppApprovalPublication, AppLifecycleMutationOutcome, AppLifecycleMutationReceipt,
        AppReenableReviewIdentity, AppRegistryLifecycleExt, AppReviewedInstallationCommit,
        AppReviewedReenableCommit,
    },
    schema_compiler::{compile_app_schema, AppSchemaCompilerError},
    surface_compiler::{
        compile_app_surface_set, AppSurfaceCompilerError, VerifiedAppSurfaceSource,
    },
    tool_catalog::canonical_app_tool_ref,
    update::{
        compute_permission_diff, AppDurableUpdateRun, AppPermissionDiff, AppUpdateCoordinatorError,
        AppUpdateCoordinatorService,
    },
};
use magician::magician_v2::agents::{AgentDefinitionStore, DefinitionStoreError};

/// Current host policy generation committed into each owner approval.
/// Bump only when the host-wide policy that approvals bind to changes.
pub const CURRENT_APP_GLOBAL_POLICY_REVISION: u64 = 1;
const APPROVAL_LIFETIME: Duration = Duration::minutes(5);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppInstallationReview {
    pub installation_id: AppInstallationId,
    pub attempt_id: AppReference,
    pub attempt_kind: AppLifecycleAttemptKind,
    pub package_revision_ref: AppReference,
    pub package_content_digest: AppDigest,
    pub name: String,
    pub version: String,
    pub description: String,
    pub requested_tools: Vec<AppReference>,
    pub requested_agents: Vec<AppReference>,
    pub requested_personalities: Vec<AppReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_interactive_capabilities: Vec<AppReviewedInteractiveCapabilityGrant>,
    pub tool_dispatch: Vec<AppReviewedToolDispatch>,
    pub workflows: Vec<AppReviewedWorkflowGrant>,
    pub workflow_material_bindings: Vec<AppReviewedWorkflowMaterialBinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_contribution_ports: Vec<AppReviewedContributionPortGrant>,
    /// Scripted custom-surface request (plan 1.6, `custom_surfaces_v1`):
    /// declared entry points, the full executable-member inventory with
    /// digests, and the static asset-scan findings. `None` means the
    /// package requests no custom surfaces and reviews exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_custom_surface: Option<AppReviewedCustomSurfaceRequest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_behaviors: Vec<AppBehaviorGrant>,
    /// Full selector shown to the owner; the durable grant carries its digest
    /// and runtime must compare it with the reopened manifest selector.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub requested_behavior_input_selectors: BTreeMap<AppName, AppManifestBehaviorInputSelector>,
    /// Exact structured-output schemas shown with the behavior review. The
    /// corresponding grant stores their canonical digest, not a second mutable
    /// schema copy.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub requested_behavior_output_schemas: BTreeMap<AppName, AppManifestInputSchema>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_event_behaviors: Vec<AppEventBehaviorGrant>,
    /// Host-owned, exact input projections shown beside each event behavior.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub requested_event_projection_schemas: BTreeMap<AppName, AppManifestInputSchema>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub requested_event_output_schemas: BTreeMap<AppName, AppManifestInputSchema>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_notifications: Vec<AppNotificationGrant>,
    pub workflow_material_digest: AppDigest,
    pub inert_workflows: Vec<AppInertWorkflow>,
    pub requested_data_handling_policy: AppDataHandlingPolicy,
    pub requested_background_execution: AppBackgroundExecution,
    pub requested_network_policy: AppNetworkPolicy,
    pub requested_resource_ceiling: AppResourceCeiling,
    /// How this attempt's request compares to the grant already in force, on
    /// every authority axis.
    ///
    /// `None` for a first install — there is nothing to compare against. On an
    /// update or reinstall the owner would otherwise see only "the app wants
    /// X", with no indication of which parts of X are new. An update that
    /// quietly adds an egress destination or shortens its background interval
    /// looks identical to one that changed nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_diff: Option<AppPermissionDiff>,
    /// `app_memory_read_v1` request shown to the owner: which user tiers and
    /// agents' memory the app wants, why, and which requested tiers are
    /// sensitive (never granted unless the owner ticks them). `None` means
    /// the app requests no owner memory and reviews exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_memory_read: Option<AppReviewedMemoryRead>,
    /// `app_secret_use_v1`: secrets the locked tools ask to use, each with
    /// the one host it can reach. Nothing is granted unless the owner ticks
    /// it. `None` means no locked tool uses a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_secret_uses: Option<Vec<super::secret_access::AppSecretUseRequest>>,
    /// `app_in_place_skill_v1`: how each network-capable or in-place OS-jail
    /// tool runs and which hosts it declares, shown per tool.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_runtime: Vec<AppReviewedToolRuntime>,
    /// Whether the owner may grant "any public host" (some locked tool can
    /// use the network). Off unless the owner ticks it at approval.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub offers_any_public_host: bool,
}

/// How one app OS-jail tool runs, for the install review.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppReviewedToolRuntime {
    pub tool: AppReference,
    /// The skill the tool runs in place from; `None` for a copied
    /// single-file tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_place_from: Option<String>,
    /// Hosts the tool declares: the destinations the review suggests.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declared_hosts: Vec<String>,
    /// The tool declares no host and reaches only the hosts this app is
    /// granted (any website under "any public host").
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reaches_granted_hosts: bool,
    /// The hosts this tool can actually reach if the app is approved as
    /// requested: its declared hosts the app asks for, or every host the app
    /// asks for when it declares none. Its keys can reach only these (unless
    /// "any public host" is also granted to an undeclared tool).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reachable_hosts: Vec<String>,
}

/// Review material for an app's owner-memory request.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppReviewedMemoryRead {
    pub request: super::memory_access::AppMemoryReadRequest,
    pub request_digest: AppDigest,
    /// Requested tiers that need an explicit tick.
    pub sensitive_tiers: Vec<String>,
    /// What is granted if the owner approves without choosing: requested
    /// non-sensitive tiers and requested agents while the owner uses the app,
    /// nothing in the background.
    pub default_grant: super::memory_access::AppMemoryReadGrant,
}

/// Owner's memory choice at approval, echoing the reviewed request digest.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMemoryReadGrantRequest {
    pub reviewed_request_digest: AppDigest,
    #[serde(default)]
    pub interactive: super::memory_access::AppMemoryReadSelection,
    #[serde(default)]
    pub background: super::memory_access::AppMemoryReadSelection,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppReviewedToolDispatch {
    pub tool: AppReference,
    pub dispatchable: bool,
    pub attested_operations: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppReviewedWorkflowGrant {
    pub workflow_id: AppName,
    pub uses: Vec<AppReference>,
    pub agent: AppReference,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub personality: Option<AppReference>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppInertWorkflow {
    pub workflow_id: AppName,
    pub reasons: Vec<String>,
}

/// Owner-selected narrowing of one exact reviewed contribution port. The
/// source entity and destination cannot be substituted: they are recovered
/// from `reviewed_grant_digest` and the immutable package lock.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppContributionPortGrantRequest {
    pub workflow_id: AppName,
    pub port_id: AppName,
    pub reviewed_grant_digest: AppDigest,
    pub selected_fields: Vec<super::models::AppFieldPath>,
    pub purposes: Vec<AppName>,
    pub audiences: Vec<AppReference>,
    pub evidence_classes: Vec<magician_app_contract::contribution::AppContributionEvidenceClass>,
    pub frequency: AppContributionFrequency,
    pub maximum_retention_seconds: u64,
}

/// Owner-selected narrowing of one exact interactive request shown by review.
/// Raw transport/device/host identifiers and control tokens are deliberately
/// absent; targets remain logical selectors consumed by the physical owner.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppInteractiveCapabilityGrantRequest {
    pub dependency_ref: AppReference,
    pub reviewed_request_digest: AppDigest,
    pub granted: AppInteractiveCapabilityRequest,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppInstallationApproveRequest {
    /// Digest returned by the exact review the owner saw. Missing or stale
    /// evidence requires a fresh review; approval never recomputes mutable
    /// agent/personality bytes silently.
    #[serde(default)]
    pub review_material_digest: Option<AppDigest>,
    /// Omit to grant every requested tool. An empty list grants none.
    #[serde(default)]
    pub granted_tools: Option<Vec<AppReference>>,
    #[serde(default)]
    pub granted_agents: Option<Vec<AppReference>>,
    #[serde(default)]
    pub granted_personalities: Option<Vec<AppReference>>,
    /// Omit to grant every reviewed contribution ceiling. An explicit empty
    /// list denies all ports; every listed entry must narrow an exact digest.
    #[serde(default)]
    pub granted_contribution_ports: Option<Vec<AppContributionPortGrantRequest>>,
    /// Omit to grant every reviewed interactive request whose tool is also
    /// granted. An explicit empty list denies all interactive authority.
    #[serde(default)]
    pub granted_interactive_capabilities: Option<Vec<AppInteractiveCapabilityGrantRequest>>,
    /// Owner-selected subset of the reviewed custom-surface entry points
    /// (plan 1.6). Omitted or empty grants NO surfaces — the grant is
    /// never implicit-all; every listed entry must narrow the exact
    /// reviewed request digest.
    #[serde(default)]
    pub granted_custom_surface_entry_points: Option<Vec<AppCustomSurfaceEntryGrantRequest>>,
    /// Omit to grant every reviewed behavior exactly as requested. An empty
    /// list denies every behavior; selected entries must echo the reviewed
    /// digest and may only slow cadence or lower numeric ceilings.
    #[serde(default)]
    pub granted_behaviors: Option<Vec<AppBehaviorGrantRequest>>,
    /// New event/notification authority is explicit-deny by default. Omitted
    /// and empty both grant none; each listed entry may only narrow an exact
    /// reviewed digest.
    #[serde(default)]
    pub granted_event_behaviors: Option<Vec<AppEventBehaviorGrantRequest>>,
    #[serde(default)]
    pub granted_notifications: Option<Vec<AppNotificationGrantRequest>>,
    /// Exact durable migration run reviewed for update/reinstall. Initial
    /// installation remains a distinct path and rejects these fields.
    #[serde(default)]
    pub migration_run_id: Option<AppReference>,
    #[serde(default)]
    pub update_plan_digest: Option<AppDigest>,
    /// Required for every reviewed operation set containing a destructive
    /// rename/map/retire, even when the current dataset is empty.
    #[serde(default)]
    pub destructive_migration_confirmed: bool,
    /// Owner-chosen memory access (`app_memory_read_v1`). Omit to take the
    /// reviewed default grant; every entry must be within the request.
    #[serde(default)]
    pub granted_memory_read: Option<AppMemoryReadGrantRequest>,
    /// Owner-ticked secret uses (`app_secret_use_v1`). Omitted or empty
    /// grants none: a secret is never granted implicitly.
    #[serde(default)]
    pub granted_secret_uses: Option<Vec<super::secret_access::AppSecretUseGrant>>,
    /// The explicit "any public host" grant (`app_in_place_skill_v1`). Off
    /// unless set; only valid when the review offers it.
    #[serde(default)]
    pub granted_any_public_host: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppBehaviorGrantRequest {
    pub behavior_id: AppName,
    pub reviewed_request_digest: AppDigest,
    pub min_interval_seconds: u64,
    pub resources: AppBehaviorResourceCeiling,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppEventBehaviorGrantRequest {
    pub event_behavior_id: AppName,
    pub reviewed_request_digest: AppDigest,
    pub min_interval_seconds: u64,
    pub resources: AppBehaviorResourceCeiling,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppNotificationGrantRequest {
    pub workflow_id: AppName,
    pub port_id: AppName,
    pub reviewed_request_digest: AppDigest,
    pub severity_ceiling: AppNotificationSeverityV1,
    pub max_notifications_per_period: u32,
    pub period_seconds: u64,
    pub max_pending: u16,
    pub ttl_seconds: u64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppInstallationApproveOutcome {
    Enabled,
    AlreadyEnabled,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppInstallationApproveReceipt {
    pub installation_id: AppInstallationId,
    pub generation: u64,
    pub status: AppInstallationStatus,
    pub grant_revision: AppRevision,
    pub schema_revision: AppRevision,
    pub surface_revision: AppRevision,
    pub approval_id: AppReference,
    pub attempt_id: AppReference,
    pub outcome: AppInstallationApproveOutcome,
    pub inert_workflows: Vec<AppInertWorkflow>,
    /// Entry-point routes granted out of the reviewed custom-surface
    /// request, in reviewed order. Empty means the capability was granted
    /// no surfaces (the fail-closed default).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub granted_custom_surface_entry_points: Vec<String>,
}

#[derive(Debug, Error)]
pub enum AppInstallationReviewError {
    #[error(transparent)]
    Authentication(#[from] AppAuthorityError),
    #[error(transparent)]
    Registry(#[from] AppRegistryError),
    #[error(transparent)]
    Staging(#[from] AppPackageStagingError),
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error(transparent)]
    Manifest(#[from] AppManifestError),
    #[error(transparent)]
    Schema(#[from] AppSchemaCompilerError),
    #[error(transparent)]
    Surface(#[from] AppSurfaceCompilerError),
    #[error(transparent)]
    Approval(#[from] AppInstallationApprovalError),
    #[error(transparent)]
    PackageLock(#[from] AppPackageLockError),
    #[error(transparent)]
    Definition(#[from] DefinitionStoreError),
    #[error(transparent)]
    AgentCapability(#[from] AppAgentCapabilityError),
    #[error(transparent)]
    CustomSurfaceReview(#[from] super::custom_surface_review::AppCustomSurfaceReviewError),
    /// Reading the grant already in force, to show the owner what an update
    /// would change. Surfaced rather than swallowed: "we could not tell you
    /// what changed" must never reach the owner looking like "nothing changed".
    #[error(transparent)]
    EntityStore(#[from] super::entity_store::AppEntityStoreError),
    #[error(transparent)]
    Update(#[from] AppUpdateCoordinatorError),
    #[error("the app installation is not available in this authenticated scope")]
    NotFound,
    #[error("the installation is not waiting for owner review: {0}")]
    NotReady(String),
    #[error("the owner grant is invalid: {0}")]
    InvalidGrant(String),
}

#[derive(Clone)]
pub struct AppInstallationReviewService {
    registry: AppRegistryService,
    stager: AppPackageStager,
    agent_definition_store: AgentDefinitionStore,
}

impl AppInstallationReviewService {
    pub fn from_parts(registry: AppRegistryService, stager: AppPackageStager) -> Self {
        let agent_definition_store =
            AgentDefinitionStore::with_workspace_layout(stager.workspace().clone());
        Self {
            registry,
            stager,
            agent_definition_store,
        }
    }

    pub async fn review(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<AppInstallationReview, AppInstallationReviewError> {
        if authenticated.authentication() == AppScopeAuthentication::TrustedSystemPackageHost
            && authenticated.trusted_system_package_installation() != Some(installation_id)
        {
            return Err(AppInstallationReviewError::InvalidGrant(
                "system-package host authority is bound to a different installation".to_owned(),
            ));
        }
        let prepared = self
            .prepare_initial_review(authenticated, installation_id, now)
            .await?;
        if authenticated.authentication() == AppScopeAuthentication::TrustedSystemPackageHost
            && prepared.attempt.kind != AppLifecycleAttemptKind::InitialInstall
        {
            return Err(AppInstallationReviewError::InvalidGrant(
                "system-package host authority can review only an initial boot installation"
                    .to_owned(),
            ));
        }
        Ok(prepared.review)
    }

    pub async fn approve(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        request: AppInstallationApproveRequest,
        now: DateTime<Utc>,
    ) -> Result<AppInstallationApproveReceipt, AppInstallationReviewError> {
        authenticated.ensure_live_at(&now)?;
        if authenticated.authentication() == AppScopeAuthentication::TrustedSystemPackageHost
            && authenticated.trusted_system_package_installation() != Some(installation_id)
        {
            return Err(AppInstallationReviewError::InvalidGrant(
                "system-package host authority is bound to a different installation".to_owned(),
            ));
        }
        if let Some(already_enabled) = self
            .already_enabled_receipt(authenticated, installation_id, now)
            .await?
        {
            return Ok(already_enabled);
        }
        let prepared = self
            .prepare_initial_review(authenticated, installation_id, now)
            .await?;
        if authenticated.authentication() == AppScopeAuthentication::TrustedSystemPackageHost
            && prepared.attempt.kind != AppLifecycleAttemptKind::InitialInstall
        {
            return Err(AppInstallationReviewError::InvalidGrant(
                "system-package host authority can approve only an initial boot installation"
                    .to_owned(),
            ));
        }
        if request.review_material_digest.as_ref()
            != Some(&prepared.review.workflow_material_digest)
        {
            return Err(AppInstallationReviewError::InvalidGrant(
                "review material changed or was not presented; refresh installation review"
                    .to_owned(),
            ));
        }
        let reviewed_update = match prepared.attempt.kind {
            AppLifecycleAttemptKind::InitialInstall => {
                if request.migration_run_id.is_some()
                    || request.update_plan_digest.is_some()
                    || request.destructive_migration_confirmed
                {
                    return Err(AppInstallationReviewError::InvalidGrant(
                        "initial installation cannot consume update coordinator evidence"
                            .to_owned(),
                    ));
                }
                None
            },
            AppLifecycleAttemptKind::Update | AppLifecycleAttemptKind::Reinstall => {
                let migration_run_id = request.migration_run_id.as_ref().ok_or_else(|| {
                    AppInstallationReviewError::NotReady(
                        "update/reinstall requires an exact reviewed migration run".to_owned(),
                    )
                })?;
                let update_plan_digest = request.update_plan_digest.as_ref().ok_or_else(|| {
                    AppInstallationReviewError::NotReady(
                        "update/reinstall requires the exact reviewed update-plan digest"
                            .to_owned(),
                    )
                })?;
                let permission_diff =
                    prepared.review.permission_diff.as_ref().ok_or_else(|| {
                        AppInstallationReviewError::NotReady(
                            "update/reinstall is missing its bounded permission diff".to_owned(),
                        )
                    })?;
                Some(
                    AppUpdateCoordinatorService::from_parts(
                        self.registry.clone(),
                        self.stager.clone(),
                    )
                    .authorize_reviewed_switch(
                        authenticated,
                        installation_id,
                        &prepared.attempt.attempt_id,
                        migration_run_id,
                        update_plan_digest,
                        permission_diff,
                        request.destructive_migration_confirmed,
                        now,
                    )
                    .await?,
                )
            },
        };
        let granted_tools = select_granted_subset(
            "tools",
            &prepared.review.requested_tools,
            request.granted_tools.as_deref(),
            canonicalize_tool_ref,
        )?;
        let granted_agents = select_granted_subset(
            "agents",
            &prepared.review.requested_agents,
            request.granted_agents.as_deref(),
            canonicalize_prefixed_ref("agent"),
        )?;
        let granted_personalities = select_granted_subset(
            "personalities",
            &prepared.review.requested_personalities,
            request.granted_personalities.as_deref(),
            canonicalize_prefixed_ref("personality"),
        )?;
        let granted_contribution_ports = select_contribution_port_grants(
            &prepared.review.requested_contribution_ports,
            request.granted_contribution_ports.as_deref(),
        )?;
        let granted_interactive_capabilities = select_interactive_capability_grants(
            &prepared.review.requested_interactive_capabilities,
            request.granted_interactive_capabilities.as_deref(),
            &granted_tools,
            &prepared.package_lock,
        )?;
        // Custom-surface narrowing (plan 1.6): the grant is never
        // implicit-all. An omitted or empty selection grants no surfaces;
        // a non-empty selection must narrow the exact reviewed request
        // digest; and granting anything without a reviewed request is a
        // substitution, not a narrowing.
        let granted_custom_surface = match prepared.review.requested_custom_surface.as_ref() {
            Some(reviewed) => {
                reviewed.narrow(request.granted_custom_surface_entry_points.as_deref())?
            },
            None => {
                if request.granted_custom_surface_entry_points.is_some() {
                    return Err(AppInstallationReviewError::InvalidGrant(
                        "custom-surface entry points were granted without a reviewed                          custom-surface request"
                            .to_owned(),
                    ));
                }
                Vec::new()
            },
        };
        let granted_behaviors = select_behavior_grants(
            &prepared.review.requested_behaviors,
            request.granted_behaviors.as_deref(),
        )?;
        let granted_event_behaviors = select_event_behavior_grants(
            &prepared.review.requested_event_behaviors,
            request.granted_event_behaviors.as_deref(),
        )?;
        let granted_notifications = select_notification_grants(
            &prepared.review.requested_notifications,
            request.granted_notifications.as_deref(),
        )?;
        let granted_background_execution = selected_background_execution(
            &prepared.review,
            &granted_behaviors,
            &granted_event_behaviors,
        );
        let mut granted_resource_ceiling = prepared.review.requested_resource_ceiling.clone();
        if matches!(
            &granted_background_execution,
            AppBackgroundExecution::Denied
        ) {
            granted_resource_ceiling.max_concurrent_background_runs = 0;
        }

        let grant_revision =
            next_reviewed_revision(prepared.attempt.kind, prepared.installation.grant_revision)?;
        let schema_revision = next_reviewed_revision(
            prepared.attempt.kind,
            prepared.installation.active_schema_revision,
        )?;
        let surface_revision_id = next_reviewed_revision(
            prepared.attempt.kind,
            prepared.installation.active_surface_revision,
        )?;
        let (compatibility, migration_plan_ref) = reviewed_schema_migration(&reviewed_update);
        let inert_workflows = inert_workflows_for_grant(
            &prepared.review.workflows,
            &granted_tools,
            &granted_agents,
            &granted_personalities,
            &prepared.review.tool_dispatch,
        );
        let mut grant = AppGrantRevision {
            installation_id: prepared.installation.installation_id.clone(),
            revision: grant_revision,
            package_revision_ref: prepared.package_revision_ref().clone(),
            requested_tools: prepared.review.requested_tools.clone(),
            granted_tools,
            requested_agents: prepared.review.requested_agents.clone(),
            granted_agents,
            requested_personalities: prepared.review.requested_personalities.clone(),
            granted_personalities,
            requested_interactive_capabilities: prepared
                .review
                .requested_interactive_capabilities
                .clone(),
            granted_interactive_capabilities,
            // Persist the narrowed custom-surface grant (plan 1.6): the
            // durable grant, not just the receipt, carries the exact
            // (route, document, digest) set the owner attested, and the
            // runtime scripted host refuses anything outside it.
            granted_custom_surface_entry_points: granted_custom_surface_entries(
                &granted_custom_surface,
            ),
            requested_behavior_grants: prepared.review.requested_behaviors.clone(),
            granted_behavior_grants: granted_behaviors,
            requested_event_behavior_grants: prepared.review.requested_event_behaviors.clone(),
            granted_event_behavior_grants: granted_event_behaviors,
            requested_notification_grants: prepared.review.requested_notifications.clone(),
            granted_notification_grants: granted_notifications,
            requested_memory_read: prepared
                .review
                .requested_memory_read
                .as_ref()
                .map(|memory| memory.request.clone()),
            granted_memory_read: select_memory_read_grant(
                prepared.review.requested_memory_read.as_ref(),
                request.granted_memory_read.as_ref(),
            )?,
            requested_secret_uses: prepared.review.requested_secret_uses.clone(),
            granted_secret_uses: select_secret_use_grant(
                prepared.review.requested_secret_uses.as_deref(),
                request.granted_secret_uses.as_deref(),
                &super::secret_access::network_policy_hosts(
                    &prepared.review.requested_network_policy,
                ),
                prepared
                    .review
                    .requested_data_handling_policy
                    .external_egress
                    == AppExternalEgress::AnyPublicHost,
            )?,
            granted_any_public_host: if request.granted_any_public_host {
                if !prepared.review.offers_any_public_host {
                    return Err(AppInstallationReviewError::InvalidGrant(
                        "none of this app's tools can use the network, so \"any public host\" \
                         cannot be granted"
                            .to_owned(),
                    ));
                }
                true
            } else {
                false
            },
            requested_context_reads: Vec::new(),
            granted_context_reads: Vec::new(),
            requested_personal_agent_data_access: Vec::new(),
            granted_personal_agent_data_access: Vec::new(),
            requested_data_handling_policy: prepared.review.requested_data_handling_policy.clone(),
            granted_data_handling_policy: prepared.review.requested_data_handling_policy.clone(),
            granted_data_handling_policy_digest: AppDigest::blake3(b"pending"),
            requested_background_execution: prepared.review.requested_background_execution.clone(),
            granted_background_execution,
            requested_network_policy: prepared.review.requested_network_policy.clone(),
            granted_network_policy: prepared.review.requested_network_policy.clone(),
            requested_resource_ceiling: prepared.review.requested_resource_ceiling.clone(),
            granted_resource_ceiling,
            approved_by: authenticated.actor_ref().clone(),
            approved_at: now,
            authority_digest: AppDigest::blake3(b"pending"),
            revoked_at: None,
        };
        grant.granted_data_handling_policy_digest =
            contract_digest(&grant.granted_data_handling_policy)?;
        grant.authority_digest = granted_authority_digest(&grant, &granted_contribution_ports)?;

        // Publication can succeed before the final switch fails. An exact
        // retry must reproduce the immutable approval, including schema and
        // grant timestamps, instead of colliding with its own durable receipt.
        // Rebuild every decision field below; publication still compares all
        // bytes and the consume boundary still checks expiry and live policy.
        let approval_id = expected_app_installation_approval_ref(
            &prepared.attempt.attempt_id,
            authenticated.session_ref(),
            &grant.authority_digest,
            &prepared.review.workflow_material_bindings,
        )?;
        let prior_approval = self
            .registry
            .installation_approval(authenticated, &approval_id, now)
            .await?;
        let renew_expired = prior_approval
            .as_ref()
            .is_some_and(|approval| approval.consumed_at.is_none() && approval.expires_at <= now);
        let approval_time = prior_approval
            .as_ref()
            .filter(|_| !renew_expired)
            .map_or(now, |approval| approval.issued_at);
        grant.approved_at = approval_time;

        let compiled_schema = compile_app_schema(
            prepared.manifest(),
            prepared.installation.installation_id.clone(),
            prepared.package_revision_ref().clone(),
            schema_revision,
            grant.granted_data_handling_policy.clone(),
            compatibility,
            migration_plan_ref,
            &prepared.package.entity_schema_digest,
            approval_time,
        )?;
        let schema = compiled_schema.into_revision();
        let surface_source = VerifiedAppSurfaceSource::from_registry_snapshot(
            &prepared.staged,
            &prepared.package,
            &schema,
        )?;
        let surface = compile_app_surface_set(&surface_source, surface_revision_id)?;

        let mut approval = build_owner_approval(
            authenticated,
            &prepared.attempt,
            &prepared.package,
            &grant,
            &schema,
            &prepared.review.workflow_material_bindings,
            &granted_contribution_ports,
            approval_time,
        )?;
        if let Some(prior) = prior_approval {
            // A fresh authenticated approval request may renew an expired,
            // unconsumed receipt as a new revision. Never rewrite its history
            // or extend the lifetime of the original decision.
            approval.revision = if renew_expired {
                AppRevision::new(prior.revision.get().checked_add(1).ok_or_else(|| {
                    AppInstallationReviewError::InvalidGrant("approval revision exhausted".into())
                })?)?
            } else {
                prior.revision
            };
        }
        let current_policy = AppRevision::new(CURRENT_APP_GLOBAL_POLICY_REVISION)?;
        let publication = AppApprovalPublication::from_authenticated_decision(
            authenticated,
            approval.clone(),
            prepared.attempt.kind,
            current_policy,
            &now,
        )?;
        let event_id = derived_reference(
            "event:app-enabled",
            &[
                approval.approval_id.as_str(),
                prepared.installation.installation_id.as_str(),
            ],
        )?;
        let surface_revision = surface.surface_revision();
        // Construct the consume-side commit before any durable write. Publishing
        // first could leave an unconsumed approval if grant/schema/surface
        // evidence later fails the kernel.
        let commit = AppReviewedInstallationCommit::from_verified_review(
            authenticated,
            approval.clone(),
            prepared.attempt.kind,
            grant.clone(),
            schema.clone(),
            surface,
            event_id.clone(),
            AppDigest::blake3(event_id.as_str().as_bytes()),
            current_policy,
            &now,
        )?
        .with_update_authorization(reviewed_update.as_ref())?;
        self.registry
            .publish_installation_approval(authenticated, publication, now)
            .await?;
        let receipt = self
            .registry
            .commit_reviewed_installation(authenticated, commit, now)
            .await?;

        Ok(AppInstallationApproveReceipt {
            installation_id: receipt.installation_id,
            generation: receipt.generation,
            status: receipt.status,
            grant_revision: grant.revision,
            schema_revision: schema.revision,
            surface_revision,
            approval_id: approval.approval_id,
            attempt_id: prepared.attempt.attempt_id,
            outcome: match receipt.outcome {
                AppLifecycleMutationOutcome::Applied => AppInstallationApproveOutcome::Enabled,
                AppLifecycleMutationOutcome::AlreadyApplied => {
                    AppInstallationApproveOutcome::AlreadyEnabled
                },
            },
            inert_workflows,
            granted_custom_surface_entry_points: granted_custom_surface
                .iter()
                .map(|entry| entry.route.clone())
                .collect(),
        })
    }

    /// Recompute the exact current disabled-installation identity that the
    /// owner must see before re-enable. This does not create approval evidence
    /// and cannot mutate lifecycle state.
    pub async fn review_reenable(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<AppReenableReviewIdentity, AppInstallationReviewError> {
        if authenticated.authentication() == AppScopeAuthentication::TrustedSystemPackageHost {
            return Err(AppInstallationReviewError::InvalidGrant(
                "system-package host authority cannot reverse an owner disable".to_owned(),
            ));
        }
        let implementation_identity_digest = self
            .current_reenable_implementation_digest(authenticated, installation_id, now)
            .await?;
        self.registry
            .reenable_review_identity(
                authenticated,
                installation_id,
                implementation_identity_digest,
                AppRevision::new(CURRENT_APP_GLOBAL_POLICY_REVISION)?,
                now,
            )
            .await
            .map_err(Into::into)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn approve_reenable(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        expected_generation: u64,
        echoed_review_digest: AppDigest,
        event_id: AppReference,
        idempotency_key: AppDigest,
        now: DateTime<Utc>,
    ) -> Result<AppLifecycleMutationReceipt, AppInstallationReviewError> {
        if authenticated.authentication() == AppScopeAuthentication::TrustedSystemPackageHost {
            return Err(AppInstallationReviewError::InvalidGrant(
                "system-package host authority cannot reverse an owner disable".to_owned(),
            ));
        }
        if let Some(receipt) = self
            .registry
            .reviewed_reenable_replay(
                authenticated,
                installation_id,
                expected_generation,
                &echoed_review_digest,
                &event_id,
                &idempotency_key,
                now,
            )
            .await?
        {
            return Ok(receipt);
        }
        let identity = self
            .review_reenable(authenticated, installation_id, now)
            .await?;
        if identity.installation_generation != expected_generation {
            return Err(AppRegistryError::GenerationConflict {
                expected: expected_generation,
                actual: identity.installation_generation,
            }
            .into());
        }
        let commit = AppReviewedReenableCommit::from_current_review(
            identity,
            &echoed_review_digest,
            event_id,
            idempotency_key,
        )?;
        self.registry
            .commit_reviewed_reenable(authenticated, commit, now)
            .await
            .map_err(Into::into)
    }

    async fn current_reenable_implementation_digest(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<AppDigest, AppInstallationReviewError> {
        authenticated.ensure_live_at(&now)?;
        let installation = self
            .registry
            .installation(authenticated, installation_id, now)
            .await?
            .ok_or(AppInstallationReviewError::NotFound)?;
        if installation.lifecycle.status != AppInstallationStatus::Disabled {
            return Err(AppInstallationReviewError::NotReady(format!(
                "installation status is {:?}; only disabled can be re-enabled",
                installation.lifecycle.status
            )));
        }
        let package = self
            .registry
            .package_revision(authenticated, &installation.package_revision_ref, now)
            .await?
            .ok_or(AppInstallationReviewError::NotFound)?;
        let package_lock = self
            .registry
            .package_dependency_lock(authenticated, &installation.package_revision_ref, now)
            .await?
            .ok_or_else(|| {
                AppInstallationReviewError::NotReady(
                    "installed package is missing its immutable dependency lock".to_owned(),
                )
            })?;
        if package_lock.lock_digest() != &package.dependency_lock_digest {
            return Err(AppInstallationReviewError::NotReady(
                "installed package dependency lock changed".to_owned(),
            ));
        }
        let staged = self
            .stager
            .load_staged_package(authenticated, package.content_digest.clone(), now)
            .await?;
        let manifest = staged.candidate().manifest().manifest();
        let primitive_snapshot =
            resolve_authoring_primitive_catalog(&AuthoringDiscoveryRoots::for_workspace_scope(
                self.stager.workspace(),
                authenticated.scope().principal.as_str(),
                authenticated.scope().workspace.as_str(),
            ));
        let artifact_store = super::os_jail::AppOsJailArtifactStore::open_or_create(
            &self.stager.workspace().apps_root(
                authenticated.scope().principal.as_str(),
                authenticated.scope().workspace.as_str(),
            ),
        )
        .ok();
        let primitive_dispatch = revalidate_manifest_primitive_bindings(
            manifest,
            &package_lock,
            &primitive_snapshot,
            artifact_store.as_ref(),
        )
        .await?;
        let requested_tools = requested_tool_refs(manifest)?;
        let tool_dispatch: Vec<AppReviewedToolDispatch> = requested_tools
            .iter()
            .map(|tool| {
                let name = tool
                    .as_str()
                    .strip_prefix("capability:")
                    .unwrap_or(tool.as_str());
                primitive_dispatch
                    .get(&super::manifest::normalized_collision_key(name))
                    .map(|reviewed| AppReviewedToolDispatch {
                        tool: tool.clone(),
                        dispatchable: reviewed.dispatchable,
                        attested_operations: reviewed.attested_operations.clone(),
                        reason: reviewed.reason.clone(),
                    })
                    .unwrap_or_else(|| AppReviewedToolDispatch {
                        tool: tool.clone(),
                        dispatchable: false,
                        attested_operations: Vec::new(),
                        reason: "immutable primitive binding is unavailable".to_owned(),
                    })
            })
            .collect();
        if tool_dispatch.iter().any(|item| !item.dispatchable) {
            return Err(AppInstallationReviewError::InvalidGrant(
                "one or more installed primitive implementations are no longer current".to_owned(),
            ));
        }
        let workflow_material_bindings = self
            .resolve_reviewed_workflow_material(authenticated, manifest)
            .await?;
        let contribution_ports = reviewed_contribution_ports(manifest, &package_lock)?;
        contract_digest(&(
            package_lock.lock_digest(),
            tool_dispatch,
            workflow_material_bindings,
            contribution_ports,
        ))
    }

    async fn already_enabled_receipt(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<Option<AppInstallationApproveReceipt>, AppInstallationReviewError> {
        let Some(installation) = self
            .registry
            .installation(authenticated, installation_id, now)
            .await?
        else {
            return Ok(None);
        };
        if installation.lifecycle.status != AppInstallationStatus::Enabled {
            return Ok(None);
        }
        let attempt = self
            .registry
            .committed_attempt_for_installation(authenticated, installation_id, now)
            .await?
            .ok_or_else(|| {
                AppInstallationReviewError::NotReady(
                    "enabled installation has no committed review attempt".to_owned(),
                )
            })?;
        if attempt.candidate_package_revision_ref != installation.package_revision_ref {
            return Err(AppInstallationReviewError::NotReady(
                "enabled installation package no longer matches its committed review".to_owned(),
            ));
        }
        let grant_revision = installation.grant_revision.ok_or_else(|| {
            AppInstallationReviewError::NotReady(
                "enabled installation is missing its grant revision".to_owned(),
            )
        })?;
        let schema_revision = installation.active_schema_revision.ok_or_else(|| {
            AppInstallationReviewError::NotReady(
                "enabled installation is missing its schema revision".to_owned(),
            )
        })?;
        let surface_revision = installation.active_surface_revision.ok_or_else(|| {
            AppInstallationReviewError::NotReady(
                "enabled installation is missing its surface revision".to_owned(),
            )
        })?;
        let approval_id = attempt.approval_ref.clone().ok_or_else(|| {
            AppInstallationReviewError::NotReady(
                "committed review attempt is missing its approval".to_owned(),
            )
        })?;
        Ok(Some(AppInstallationApproveReceipt {
            installation_id: installation.installation_id,
            generation: installation.lifecycle.generation,
            status: installation.lifecycle.status,
            grant_revision,
            schema_revision,
            surface_revision,
            approval_id,
            attempt_id: attempt.attempt_id,
            outcome: AppInstallationApproveOutcome::AlreadyEnabled,
            inert_workflows: Vec::new(),
            granted_custom_surface_entry_points: Vec::new(),
        }))
    }

    async fn prepare_initial_review(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<PreparedReview, AppInstallationReviewError> {
        authenticated.ensure_live_at(&now)?;
        let installation = self
            .registry
            .installation(authenticated, installation_id, now)
            .await?
            .ok_or(AppInstallationReviewError::NotFound)?;
        let expected_kind = match installation.lifecycle.status {
            AppInstallationStatus::ReadyForReview => AppLifecycleAttemptKind::InitialInstall,
            AppInstallationStatus::UpdatePending => AppLifecycleAttemptKind::Update,
            AppInstallationStatus::UninstalledRetained => AppLifecycleAttemptKind::Reinstall,
            other => {
                return Err(AppInstallationReviewError::NotReady(format!(
                    "installation status is {other:?}"
                )));
            },
        };
        let attempt = self
            .registry
            .ready_for_review_attempt_for_installation(authenticated, installation_id, now)
            .await?
            .ok_or_else(|| {
                AppInstallationReviewError::NotReady(
                    "no ready-for-review attempt is linked to this installation".to_owned(),
                )
            })?;
        if attempt.kind != expected_kind {
            return Err(AppInstallationReviewError::NotReady(format!(
                "review attempt kind {:?} does not match installation status {:?}",
                attempt.kind, installation.lifecycle.status
            )));
        }
        if attempt.kind == AppLifecycleAttemptKind::InitialInstall
            && attempt.candidate_package_revision_ref != installation.package_revision_ref
        {
            return Err(AppInstallationReviewError::NotReady(
                "the review attempt no longer names this installation package".to_owned(),
            ));
        }
        let package_revision_ref = attempt.candidate_package_revision_ref.clone();
        let package = self
            .registry
            .package_revision(authenticated, &package_revision_ref, now)
            .await?
            .ok_or(AppInstallationReviewError::NotFound)?;
        let staged = self
            .stager
            .load_staged_package(authenticated, package.content_digest.clone(), now)
            .await?;
        let manifest = staged.candidate().manifest().manifest();
        let package_lock = self
            .registry
            .package_dependency_lock(authenticated, &package_revision_ref, now)
            .await?
            .ok_or_else(|| {
                AppInstallationReviewError::NotReady(
                    "candidate package is missing its immutable dependency lock".to_owned(),
                )
            })?;
        let primitive_snapshot =
            resolve_authoring_primitive_catalog(&AuthoringDiscoveryRoots::for_workspace_scope(
                self.stager.workspace(),
                authenticated.scope().principal.as_str(),
                authenticated.scope().workspace.as_str(),
            ));
        let artifact_store = super::os_jail::AppOsJailArtifactStore::open_or_create(
            &self.stager.workspace().apps_root(
                authenticated.scope().principal.as_str(),
                authenticated.scope().workspace.as_str(),
            ),
        )
        .ok();
        let primitive_dispatch = revalidate_manifest_primitive_bindings(
            manifest,
            &package_lock,
            &primitive_snapshot,
            artifact_store.as_ref(),
        )
        .await?;
        let requested_tools = requested_tool_refs(manifest)?;
        let requested_agents = requested_agent_refs(manifest)?;
        let requested_personalities = requested_named_refs(
            "personality",
            manifest
                .app
                .workflows
                .values()
                .filter_map(|workflow| workflow.personality.as_ref()),
        )?;
        let workflows = workflow_grant_needs(manifest)?;
        let workflow_material_bindings = self
            .resolve_reviewed_workflow_material(authenticated, manifest)
            .await?;
        let requested_contribution_ports = reviewed_contribution_ports(manifest, &package_lock)?;
        let requested_interactive_capabilities = reviewed_interactive_capabilities(&package_lock)?;
        let requested_custom_surface =
            reviewed_custom_surface_request(manifest, staged.candidate().members())?;
        let requested_behaviors = reviewed_behavior_grants(manifest)?;
        let requested_behavior_input_selectors = manifest
            .app
            .behaviors
            .iter()
            .map(|behavior| (behavior.id.clone(), behavior.input.clone()))
            .collect::<BTreeMap<_, _>>();
        let requested_behavior_output_schemas = manifest
            .app
            .behaviors
            .iter()
            .filter_map(|behavior| {
                behavior
                    .output_schema
                    .as_ref()
                    .map(|schema| (behavior.id.clone(), schema.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        let requested_event_behaviors = reviewed_event_behavior_grants(manifest)?;
        let requested_event_projection_schemas = manifest
            .app
            .event_behaviors
            .iter()
            .map(|behavior| {
                (
                    behavior.id.clone(),
                    event_behavior_projection_schema(&behavior.subscription),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let requested_event_output_schemas = manifest
            .app
            .event_behaviors
            .iter()
            .filter_map(|behavior| {
                behavior
                    .output_schema
                    .as_ref()
                    .map(|schema| (behavior.id.clone(), schema.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        let requested_notifications = reviewed_notification_grants(manifest)?;
        let workflow_material_digest = if requested_contribution_ports.is_empty()
            && requested_interactive_capabilities.is_empty()
            && requested_custom_surface.is_none()
            && requested_behaviors.is_empty()
            && requested_event_behaviors.is_empty()
            && requested_notifications.is_empty()
        {
            contract_digest(&workflow_material_bindings)?
        } else {
            contract_digest(&(
                &workflow_material_bindings,
                &requested_contribution_ports,
                &requested_interactive_capabilities,
                &requested_custom_surface,
                &requested_behaviors,
                &requested_behavior_input_selectors,
                &requested_behavior_output_schemas,
                &requested_event_behaviors,
                &requested_event_projection_schemas,
                &requested_event_output_schemas,
                &requested_notifications,
            ))?
        };
        // Only a package that requests owner memory folds it into the review
        // material, so every other package's digest is unchanged.
        let requested_memory_read = reviewed_memory_read(manifest)?;
        let workflow_material_digest = match &requested_memory_read {
            None => workflow_material_digest,
            Some(memory) => contract_digest(&(&workflow_material_digest, memory))?,
        };
        // Likewise only a package whose locked tools use secrets folds them
        // into the review material.
        let requested_secret_uses = reviewed_secret_uses(manifest, &primitive_snapshot)?;
        let tool_runtime = reviewed_tool_runtime(manifest, &primitive_snapshot)?;
        // Offered only when the app itself asks for it and a locked in-place
        // tool that declares no host could use it (a tool that declares hosts
        // stays at declared ∩ granted).
        let offers_any_public_host = manifest.app.data_policy.defaults.external_egress
            == AppExternalEgress::AnyPublicHost
            && tool_runtime
                .iter()
                .any(|runtime| runtime.reaches_granted_hosts || runtime.declared_hosts == ["*"]);
        let workflow_material_digest = match &requested_secret_uses {
            None => workflow_material_digest,
            Some(secrets) => contract_digest(&(&workflow_material_digest, secrets))?,
        };
        let tool_dispatch: Vec<AppReviewedToolDispatch> = requested_tools
            .iter()
            .map(|tool| {
                let name = tool
                    .as_str()
                    .strip_prefix("capability:")
                    .unwrap_or(tool.as_str());
                if let Some(reviewed) =
                    primitive_dispatch.get(&super::manifest::normalized_collision_key(name))
                {
                    return AppReviewedToolDispatch {
                        tool: tool.clone(),
                        dispatchable: reviewed.dispatchable,
                        attested_operations: reviewed.attested_operations.clone(),
                        reason: reviewed.reason.clone(),
                    };
                }
                AppReviewedToolDispatch {
                    tool: tool.clone(),
                    dispatchable: false,
                    attested_operations: Vec::new(),
                    reason: "the immutable primitive binding was not revalidated from exact \
                             reviewed bytes"
                        .to_owned(),
                }
            })
            .collect();
        let inert_workflows = inert_workflows_for_grant(
            &workflows,
            &requested_tools,
            &requested_agents,
            &requested_personalities,
            &tool_dispatch,
        );
        let mut review = AppInstallationReview {
            installation_id: installation.installation_id.clone(),
            attempt_id: attempt.attempt_id.clone(),
            attempt_kind: attempt.kind,
            package_revision_ref,
            package_content_digest: package.content_digest.clone(),
            name: manifest.name.to_string(),
            version: manifest.version.clone(),
            description: manifest.description.clone(),
            requested_tools,
            requested_agents,
            requested_personalities,
            requested_interactive_capabilities,
            tool_dispatch,
            workflows,
            workflow_material_bindings,
            requested_contribution_ports,
            requested_custom_surface,
            requested_behaviors,
            requested_behavior_input_selectors,
            requested_behavior_output_schemas,
            requested_event_behaviors,
            requested_event_projection_schemas,
            requested_event_output_schemas,
            requested_notifications,
            workflow_material_digest,
            inert_workflows,
            requested_data_handling_policy: data_handling_policy(
                &manifest.app.data_policy.defaults,
            ),
            requested_background_execution: requested_background(&manifest.app),
            requested_network_policy: requested_network(&manifest.app.data_policy.defaults),
            requested_resource_ceiling: requested_resource_ceiling(
                &manifest.app.resources,
                requested_background(&manifest.app),
                &requested_network(&manifest.app.data_policy.defaults),
            ),
            permission_diff: None,
            requested_memory_read,
            requested_secret_uses,
            tool_runtime,
            offers_any_public_host,
        };
        // An update or reinstall is judged against what the owner already
        // granted, not in isolation. Compared as-requested: it answers "if I
        // approve this as it stands, what widens?" — which is the question in
        // front of the owner at this moment.
        //
        // `data_owner_schema`, NOT `active_schema`: a review happens precisely
        // when the installation is NOT enabled — parked in `UpdatePending` for
        // an update, `UninstalledRetained` for a reinstall — so the
        // enabled-only resolver refuses exactly the two cases this diff exists
        // for, and the diff would silently always be `None`. Reading a grant to
        // show the owner is not executing under it.
        //
        // A genuinely absent prior grant leaves the diff absent; a failure to
        // READ one is surfaced, because "we could not tell you what changed"
        // must never render the same as "nothing changed".
        if !matches!(attempt.kind, AppLifecycleAttemptKind::InitialInstall) {
            let entity_store =
                super::entity_store::AppEntityStoreService::new(self.registry.clone());
            let active = entity_store
                .data_owner_schema(authenticated, installation_id, now)
                .await?;
            if let Some(active) = active {
                let proposed = as_requested_grant_for_diff(
                    &review,
                    &installation,
                    authenticated.actor_ref(),
                    now,
                );
                review.permission_diff = Some(compute_permission_diff(active.grant(), &proposed));
            }
        }
        Ok(PreparedReview {
            installation,
            attempt,
            package,
            staged,
            package_lock,
            review,
        })
    }

    async fn resolve_reviewed_workflow_material(
        &self,
        authenticated: &AuthenticatedAppScope,
        manifest: &AppPackageManifest,
    ) -> Result<Vec<AppReviewedWorkflowMaterialBinding>, AppInstallationReviewError> {
        let scoped_definitions = self.agent_definition_store.for_scope(
            authenticated.scope().principal.as_str(),
            authenticated.scope().workspace.as_str(),
        );
        let personality_root = self.stager.workspace().scope_skills_root(
            authenticated.scope().principal.as_str(),
            authenticated.scope().workspace.as_str(),
        );
        let extras = magician::magician_v2::config_extras::extra_skills_dirs();
        let mut personality_search = Vec::with_capacity(extras.len().saturating_add(1));
        personality_search.push(personality_root.as_path());
        personality_search.extend(extras.iter().map(std::path::PathBuf::as_path));
        let mut bindings = Vec::with_capacity(manifest.app.workflows.len());
        let mut definitions: BTreeMap<String, magician::magician_v2::agents::AgentDefinition> =
            BTreeMap::new();
        let mut personalities: BTreeMap<String, magician::magician_v2::skills::PersonalitySpec> =
            BTreeMap::new();
        for (workflow_id, workflow) in &manifest.app.workflows {
            let runner = workflow
                .agent
                .as_ref()
                .map(AppName::as_str)
                .unwrap_or(DEFAULT_APP_WORKFLOW_AGENT);
            let definition = match definitions.get(runner) {
                Some(definition) => definition.clone(),
                None => {
                    let definition = scoped_definitions
                        .get_definition(runner)
                        .await?
                        .ok_or_else(|| {
                            AppInstallationReviewError::InvalidGrant(format!(
                                "workflow `{workflow_id}` runner `{runner}` is unavailable for \
                                 exact review"
                            ))
                        })?
                        .definition;
                    definitions.insert(runner.to_owned(), definition.clone());
                    definition
                },
            };
            if !agent_definition_permits_app_task(&definition) {
                return Err(AppInstallationReviewError::InvalidGrant(format!(
                    "workflow `{workflow_id}` runner `{runner}` is disabled or does not permit \
                     task invocation"
                )));
            }
            let agent_ref = AppReference::parse(format!("agent:{runner}"))?;
            let personality_ref = workflow
                .personality
                .as_ref()
                .map(|name| AppReference::parse(format!("personality:{name}")))
                .transpose()?;
            let personality = match workflow.personality.as_ref() {
                Some(name) => match personalities.get(name.as_str()) {
                    Some(personality) => Some(personality.clone()),
                    None => {
                        let personality = magician::magician_v2::skills::lookup_personality_mode(
                            &personality_search,
                            name.as_str(),
                        )
                        .ok_or_else(|| {
                            AppInstallationReviewError::InvalidGrant(format!(
                                "workflow `{workflow_id}` personality `{name}` is unavailable for \
                                 exact review"
                            ))
                        })?;
                        personalities.insert(name.as_str().to_owned(), personality.clone());
                        Some(personality)
                    },
                },
                None => None,
            };
            bindings.push(seal_reviewed_workflow_material(
                workflow_id,
                &agent_ref,
                &definition,
                personality_ref.as_ref().zip(personality.as_ref()),
            )?);
        }
        Ok(bindings)
    }
}

struct PreparedReview {
    installation: AppInstallation,
    attempt: AppLifecycleAttempt,
    package: AppPackageRevision,
    staged: StagedAppPackage,
    package_lock: AppPackageLock,
    review: AppInstallationReview,
}

impl PreparedReview {
    fn manifest(&self) -> &AppPackageManifest {
        self.staged.candidate().manifest().manifest()
    }

    fn package_revision_ref(&self) -> &AppReference {
        &self.review.package_revision_ref
    }
}

fn requested_tool_refs(
    manifest: &AppPackageManifest,
) -> Result<Vec<AppReference>, AppInstallationReviewError> {
    let mut refs = BTreeSet::new();
    for tool in manifest.app.dependencies.declared_tools()? {
        refs.insert(canonicalize_tool_ref(&AppReference::parse(
            tool.name.as_str(),
        )?)?);
    }
    Ok(refs.into_iter().collect())
}

#[derive(Debug)]
struct RevalidatedPrimitiveDispatch {
    dispatchable: bool,
    attested_operations: Vec<String>,
    reason: String,
}

/// Existing Zepto/Swiggy locks that were sealed as OS-jail/blocked must be
/// republished. The current catalog is governed MCP; a lock that still names
/// a jail artifact, or that never received an MCP implementation plan, is
/// not silently upgraded.
fn governed_mcp_lock_refusal(binding: &AppLockedPrimitiveBinding) -> Option<&'static str> {
    if binding.actions().iter().any(|action| {
        action.physical_artifact_revision_ref().is_some()
            || action.physical_artifact_digest().is_some()
    }) {
        return Some(
            "containment changed to governed MCP; this lock still names an OS-jail artifact. \
             Owner-approved republish and installation re-review are required",
        );
    }
    if binding
        .actions()
        .iter()
        .all(|action| action.dispatchable() && action.implementation_plan_digest().is_some())
    {
        return None;
    }
    Some(
        "this lock predates the governed MCP contain profile; owner-approved republish \
         and installation re-review are required",
    )
}

async fn revalidate_manifest_primitive_bindings(
    manifest: &AppPackageManifest,
    package_lock: &super::package_lock::AppPackageLock,
    snapshot: &super::primitive_catalog::AppPrimitiveCatalogSnapshot,
    artifact_store: Option<&super::os_jail::AppOsJailArtifactStore>,
) -> Result<BTreeMap<String, RevalidatedPrimitiveDispatch>, AppInstallationReviewError> {
    let mut dispatch = BTreeMap::new();
    for tool in manifest.app.dependencies.declared_tools()? {
        let selector = tool
            .primitive_ref
            .as_ref()
            .map(AppReference::as_str)
            .unwrap_or_else(|| tool.name.as_str());
        let dependency_ref = AppReference::parse(format!("capability:{}", tool.name))?;
        let locked = package_lock.capability(&dependency_ref).ok_or_else(|| {
            AppPackageLockError::LockedPrimitiveMissing(dependency_ref.to_string())
        })?;
        // Legacy locks remain readable, but they cannot prove descriptor,
        // action, implementation-plan or physical-owner identity. Surface a
        // uniformly inert review item; owner-approved republish/re-review is
        // the only upgrade path and never infers code from a friendly name.
        if locked.primitive_binding().is_none()
            || locked.primitive_binding().is_some_and(|binding| {
                binding.actions().iter().any(|action| {
                    action.dispatchable() && action.implementation_plan_digest().is_none()
                })
            })
        {
            dispatch.insert(
                super::manifest::normalized_collision_key(tool.name.as_str()),
                RevalidatedPrimitiveDispatch {
                    dispatchable: false,
                    attested_operations: Vec::new(),
                    reason: "legacy package lock requires owner-approved republish and \
                             installation re-review"
                        .to_owned(),
                },
            );
            continue;
        }
        let descriptor = snapshot.resolve(selector).map_err(|_| {
            AppPackageLockError::PrimitiveBindingMismatch(dependency_ref.to_string())
        })?;
        if !matches!(
            descriptor.kind(),
            AppPrimitiveKind::CompiledTool
                | AppPrimitiveKind::ToolSkill
                | AppPrimitiveKind::Agent
                | AppPrimitiveKind::Interactive
        ) || tool
            .primitive_ref
            .as_ref()
            .is_some_and(|selected| selected != descriptor.identity())
        {
            return Err(
                AppPackageLockError::PrimitiveBindingMismatch(dependency_ref.to_string()).into(),
            );
        }
        // A locked binding that no longer matches the live descriptor is the
        // same situation as the legacy lock above: the package cannot prove
        // action identity against the platform as it stands today, and only an
        // owner-approved republish can restore that proof. Treat it the same
        // way — an inert, non-dispatchable review item — rather than refusing
        // the whole review. Failing hard here made every affected installation
        // unreviewable and unapprovable, with the owner told only that the
        // package evidence was rejected, even though the manifest was correct
        // and the drift was on the platform side.
        let binding = match authorize_locked_primitive(package_lock, &dependency_ref, &descriptor) {
            Ok(binding) => binding,
            Err(AppPackageLockError::PrimitiveBindingMismatch(_)) => {
                dispatch.insert(
                    super::manifest::normalized_collision_key(tool.name.as_str()),
                    RevalidatedPrimitiveDispatch {
                        dispatchable: false,
                        attested_operations: Vec::new(),
                        reason: "locked primitive binding no longer matches the live descriptor; \
                                 requires owner-approved republish and installation re-review"
                            .to_owned(),
                    },
                );
                continue;
            },
            Err(error) => return Err(error.into()),
        };
        let exact_public_leaf_selected = match descriptor.kind() {
            AppPrimitiveKind::Agent => {
                manifest_selects_exact_locked_leaf(&tool.actions, binding, "agent_as_tool")
            },
            AppPrimitiveKind::Interactive => binding.actions().first().is_some_and(|action| {
                manifest_selects_exact_locked_leaf(&tool.actions, binding, action.name())
            }),
            _ => true,
        };
        if !exact_public_leaf_selected {
            return Err(
                AppPackageLockError::PrimitiveBindingMismatch(dependency_ref.to_string()).into(),
            );
        }
        let sealed_interactive_owner = descriptor.kind() != AppPrimitiveKind::Interactive
            || (sealed_interactive_owner_is_reviewed(&dependency_ref, &descriptor, binding)
                && package_lock
                    .interactive_capability(&dependency_ref)
                    .is_some_and(|interactive| {
                        interactive
                            .review_grant(binding, interactive.request().clone())
                            .is_ok()
                    }));
        let sealed_agent_owner = descriptor.kind() != AppPrimitiveKind::Agent
            || snapshot
                .source_material(descriptor.identity())
                .is_some_and(|source| {
                    sealed_agent_tool_owner_is_reviewed(
                        &dependency_ref,
                        &descriptor,
                        binding,
                        source,
                    )
                });
        let mcp_lock_refusal = if descriptor.kind() == AppPrimitiveKind::ToolSkill
            && descriptor.containment() == AppPrimitiveContainment::GovernedMcp
        {
            governed_mcp_lock_refusal(binding)
        } else {
            None
        };
        let mut physical_refusal = None;
        let physical_artifacts_valid = if descriptor.kind() == AppPrimitiveKind::ToolSkill {
            if descriptor.containment() == AppPrimitiveContainment::GovernedMcp {
                mcp_lock_refusal.is_none()
            } else {
                match (
                    artifact_store,
                    snapshot.source_material(descriptor.identity()),
                ) {
                    (Some(artifact_store), Some(source)) => {
                        let mut valid = true;
                        for action in binding.actions() {
                            if let Err(error) = super::os_jail::revalidate_locked_os_jail_artifact(
                                source.to_vec(),
                                binding.clone(),
                                action.clone(),
                                artifact_store.clone(),
                                snapshot
                                    .source_directory(descriptor.identity())
                                    .map(std::path::Path::to_path_buf),
                            )
                            .await
                            {
                                // An in-place skill says exactly why it cannot
                                // run, or that it changed since approval.
                                if matches!(
                                    error,
                                    super::os_jail::AppOsJailError::InPlaceUnavailable(_)
                                        | super::os_jail::AppOsJailError::SkillChangedSinceApproval
                                ) {
                                    physical_refusal = Some(error.to_string());
                                }
                                valid = false;
                                break;
                            }
                        }
                        valid
                    },
                    _ => false,
                }
            }
        } else {
            sealed_interactive_owner && sealed_agent_owner
        };
        let attested_operations = binding
            .actions()
            .iter()
            // `authorize_locked_primitive` above already compared the exact
            // descriptor/action digests reconstructed from reviewed bytes.
            // Reclassifying from a friendly name here loses the ToolSkill's
            // declared shape and can disagree with that sealed decision.
            .filter(|action| action.dispatchable() && physical_artifacts_valid)
            .map(|action| action.name().to_owned())
            .collect::<Vec<_>>();
        let dispatchable = descriptor.dispatch().status() == AppPrimitiveDispatchStatus::Ready
            && !attested_operations.is_empty();
        let reason = if descriptor.kind() == AppPrimitiveKind::Interactive
            && !sealed_interactive_owner
        {
            "the interactive descriptor is not an exact sealed Browser/macOS/Android owner action"
                .to_owned()
        } else if descriptor.kind() == AppPrimitiveKind::Agent && !sealed_agent_owner {
            "the agent descriptor is not the exact sealed agent_as_tool Artifact V3 leaf".to_owned()
        } else if !physical_artifacts_valid {
            if let Some(reason) = mcp_lock_refusal {
                reason.to_owned()
            } else if let Some(reason) = physical_refusal {
                reason
            } else {
                "the private executable artifact is absent, changed or not content-addressed by this \
                 package lock"
                    .to_owned()
            }
        } else if dispatchable {
            if tool.actions.is_empty() {
                "descriptor, every advertised action schema and implementation bytes match the \
                 immutable package lock (legacy empty selector approves all listed actions)"
                    .to_owned()
            } else {
                "descriptor and the manifest-selected action schemas/implementation bytes match \
                 the immutable package lock"
                    .to_owned()
            }
        } else {
            descriptor.dispatch().reason().to_owned()
        };
        dispatch.insert(
            super::manifest::normalized_collision_key(tool.name.as_str()),
            RevalidatedPrimitiveDispatch {
                dispatchable,
                attested_operations,
                reason,
            },
        );
    }
    Ok(dispatch)
}

fn manifest_selects_exact_locked_leaf(
    selectors: &[String],
    binding: &super::package_lock::AppLockedPrimitiveBinding,
    expected_action: &str,
) -> bool {
    if selectors.len() != 1 || binding.actions().len() != 1 {
        return false;
    }
    let action = &binding.actions()[0];
    action.name() == expected_action
        && (super::app_tool_bind::normalize_app_action_name(&selectors[0])
            == super::app_tool_bind::normalize_app_action_name(expected_action)
            || selectors[0].trim() == action.action_ref().as_str())
}

/// Agent is not generic app delegation authority. Review admits only one
/// complete, current, Task-callable definition whose sole app action is the
/// typed `agent_as_tool` leaf. The immutable lock has already compared the
/// descriptor/action digests; this independent predicate re-parses the exact
/// source and recomputes the schema, byte ceiling and complete physical-owner
/// digest. That digest covers the no-spawn V3 child runtime, fixed named JSON
/// result artifact, terminal carrier, cancellation and replay owners.
fn sealed_agent_tool_owner_is_reviewed(
    dependency_ref: &AppReference,
    descriptor: &super::primitive_catalog::AppPrimitiveDescriptor,
    binding: &super::package_lock::AppLockedPrimitiveBinding,
    source: &[u8],
) -> bool {
    if descriptor.kind() != AppPrimitiveKind::Agent
        || descriptor.source().kind() != AppPrimitiveSourceKind::ScopedAgent
        || descriptor.execution_class() != AppPrimitiveExecutionClass::AgentTask
        || descriptor.containment() != AppPrimitiveContainment::TaskOwner
        || descriptor.default_io_kind() != super::app_tool_bind::AppToolIoKind::Unbound
        || descriptor.dispatch().status() != AppPrimitiveDispatchStatus::Ready
        || descriptor.eligibility().status() != AppPrimitiveEligibilityStatus::Lockable
        || !descriptor.exposure().apps()
        || descriptor.allowed_modes() != &BTreeSet::from([AppPrimitiveInvocationMode::AgentAsTool])
        || descriptor.actions().len() != 1
        || binding.actions().len() != 1
        || binding.source_content_digest() != &AppDigest::blake3(source)
    {
        return false;
    }
    let Ok(source_text) = std::str::from_utf8(source) else {
        return false;
    };
    let Ok(definition) = magician::magician_v2::agents::AgentDefinition::from_yaml_str(source_text)
    else {
        return false;
    };
    let Some(declaration) = definition.app_tool.as_ref() else {
        return false;
    };
    if !agent_definition_permits_app_task(&definition)
        || definition.agent_id != descriptor.name()
        || dependency_ref.as_str() != format!("capability:{}", definition.agent_id)
        || declaration.max_input_bytes == 0
        || declaration.max_result_bytes == 0
        || declaration.max_input_bytes > 256 * 1024
        || declaration.max_result_bytes > 256 * 1024
        || declaration.input.fields.len() > 256
        || declaration.result.fields.len() > 256
    {
        return false;
    }
    let descriptor_action = &descriptor.actions()[0];
    let locked_action = &binding.actions()[0];
    let expected_input =
        AppDigest::blake3_canonical_json(&declaration.input.to_primitive_json_schema()).ok();
    let expected_result =
        AppDigest::blake3_canonical_json(&declaration.result.to_primitive_json_schema()).ok();
    let expected_implementation = super::agent_capability::agent_tool_implementation_plan_digest(
        binding.source_content_digest(),
        declaration,
    )
    .ok();
    descriptor_action.name() == "agent_as_tool"
        && descriptor_action.dispatch().status() == AppPrimitiveDispatchStatus::Ready
        && descriptor_action.input_schema().state() == AppPrimitiveSchemaState::Inline
        && descriptor_action.result_schema().state() == AppPrimitiveSchemaState::Inline
        && descriptor_action.effects() == &BTreeSet::from([AppPrimitiveEffect::Undeclared])
        && descriptor_action.physical_artifact_revision_ref().is_none()
        && descriptor_action.physical_artifact_digest().is_none()
        && descriptor_action.input_schema().digest() == expected_input.as_ref()
        && descriptor_action.result_schema().digest() == expected_result.as_ref()
        && descriptor_action.implementation_plan_digest() == expected_implementation.as_ref()
        && descriptor_action.transport_result_byte_ceiling() == Some(declaration.max_result_bytes)
        && locked_action.name() == "agent_as_tool"
        && locked_action.dispatchable()
        && locked_action.action_digest() == descriptor_action.action_digest()
        && locked_action.input_schema_digest() == expected_input.as_ref()
        && locked_action.result_schema_digest() == expected_result.as_ref()
        && locked_action.implementation_plan_digest() == expected_implementation.as_ref()
        && locked_action.transport_result_byte_ceiling() == Some(declaration.max_result_bytes)
        && locked_action.physical_artifact_revision_ref().is_none()
        && locked_action.physical_artifact_digest().is_none()
}

/// Interactive is not a generic containment class. Installation review may
/// approve only the exact owner adapters whose workflow/executor paths
/// consume the common interactive/effect permit today. Descriptor and selected
/// action bytes have already been compared with the immutable lock by
/// `authorize_locked_primitive`; this final predicate independently fixes the
/// profile, containment, public capability, schema, result bound and physical
/// implementation identity. A future owner must add an equally narrow arm.
fn sealed_interactive_owner_is_reviewed(
    dependency_ref: &AppReference,
    descriptor: &super::primitive_catalog::AppPrimitiveDescriptor,
    binding: &super::package_lock::AppLockedPrimitiveBinding,
) -> bool {
    if descriptor.kind() != AppPrimitiveKind::Interactive
        || descriptor.dispatch().status() != AppPrimitiveDispatchStatus::Ready
        || descriptor.eligibility().status() != AppPrimitiveEligibilityStatus::Lockable
        || !descriptor.exposure().apps()
        || descriptor.allowed_modes()
            != &BTreeSet::from([
                AppPrimitiveInvocationMode::CallTool,
                AppPrimitiveInvocationMode::InteractiveAction,
            ])
        || binding.actions().len() != 1
    {
        return false;
    }
    let action = &binding.actions()[0];
    if !action.dispatchable()
        || action.implementation_plan_digest().is_none()
        || action.input_schema_digest().is_none()
        || action.result_schema_digest().is_none()
    {
        return false;
    }
    let schema_digest = |schema: serde_json::Value| AppDigest::blake3_canonical_json(&schema).ok();
    let browser_implementation =
        super::browser_capability::app_browser_runtime_implementation_digest();
    match (
        descriptor.name(),
        descriptor.execution_class(),
        descriptor.containment(),
    ) {
        (
            "browser",
            AppPrimitiveExecutionClass::BrowserOwner,
            AppPrimitiveContainment::BrowserSession,
        ) if interactive_dependency_alias_matches(
            dependency_ref,
            "capability:browser",
            action.name(),
        ) =>
        {
            let input = super::browser_capability::browser_action_input_schema(action.name());
            let result = (action.name() == "snapshot")
                .then(super::browser_capability::browser_observe_result_schema)
                .unwrap_or_else(super::browser_capability::browser_action_result_schema);
            let ceiling = if action.name() == "snapshot" {
                super::browser_capability::APP_BROWSER_OBSERVE_RESULT_CEILING
            } else {
                super::browser_capability::APP_BROWSER_ACTION_RESULT_CEILING
            };
            input.is_some()
                && action.input_schema_digest()
                    == input.and_then(|schema| schema_digest(schema)).as_ref()
                && action.result_schema_digest() == schema_digest(result).as_ref()
                && action.implementation_plan_digest() == Some(&browser_implementation)
                && action.transport_result_byte_ceiling() == Some(ceiling)
        },
        (
            "macos-ui-automation",
            AppPrimitiveExecutionClass::MacosHostOwner,
            AppPrimitiveContainment::MacosHost,
        ) if interactive_dependency_alias_matches(
            dependency_ref,
            "capability:macos-ui-automation",
            action.name(),
        ) =>
        {
            let operation = super::macos_host::macos_operation(action.name());
            let input = super::macos_host::macos_action_input_schema(action.name());
            let result = (action.name() == "snapshot")
                .then(super::macos_host::macos_observe_result_schema)
                .unwrap_or_else(super::macos_host::macos_action_result_schema);
            let expected_implementation = operation.and_then(|operation| {
                super::macos_host::macos_operation_implementation_plan_digest(
                    binding.source_content_digest(),
                    operation,
                )
                .ok()
            });
            let ceiling = if action.name() == "snapshot" {
                super::macos_host::APP_MACOS_OBSERVE_RESULT_CEILING
            } else {
                super::macos_host::APP_MACOS_ACTION_RESULT_CEILING
            };
            operation.is_some()
                && input.is_some()
                && action.input_schema_digest()
                    == input.and_then(|schema| schema_digest(schema)).as_ref()
                && action.result_schema_digest() == schema_digest(result).as_ref()
                && action.implementation_plan_digest() == expected_implementation.as_ref()
                && action.transport_result_byte_ceiling() == Some(ceiling)
        },
        (
            descriptor_name @ ("android_snapshot" | "android_screenshot" | "android_act"
            | "android_app"),
            AppPrimitiveExecutionClass::AndroidDeviceOwner,
            AppPrimitiveContainment::AndroidDevice,
        ) if interactive_dependency_alias_matches(
            dependency_ref,
            &format!("capability:{descriptor_name}"),
            action.name(),
        ) =>
        {
            let input = super::android_device::android_action_input_schema(action.name());
            let result = super::android_device::android_action_result_schema(action.name());
            let expected_implementation =
                super::android_device::app_android_action_implementation_plan_digest(
                    binding.source_content_digest(),
                    action.name(),
                )
                .ok();
            let ceiling = match action.name() {
                "snapshot" => super::android_device::APP_ANDROID_SNAPSHOT_RESULT_CEILING,
                "screenshot" => super::android_device::APP_ANDROID_SCREENSHOT_RESULT_CEILING,
                _ => super::android_device::APP_ANDROID_ACTION_RESULT_CEILING,
            };
            input.is_some()
                && result.is_some()
                && action.input_schema_digest()
                    == input.and_then(|schema| schema_digest(schema)).as_ref()
                && action.result_schema_digest()
                    == result.and_then(|schema| schema_digest(schema)).as_ref()
                && action.implementation_plan_digest() == expected_implementation.as_ref()
                && action.transport_result_byte_ceiling() == Some(ceiling)
        },
        // Plan-1.3 experience classes. These arms independently recompute the
        // admission identity exactly like the browser arm above, but they are
        // reachable only for a dispatch-Ready descriptor — the shared gate at
        // the top of this predicate keeps both classes fail-closed today
        // because the admitted descriptors are deliberately Conditional until
        // a reviewed physical-owner consumer lands.
        (
            "overlay-draw",
            AppPrimitiveExecutionClass::OverlayDrawOwner,
            AppPrimitiveContainment::OverlaySurface,
        ) if interactive_dependency_alias_matches(
            dependency_ref,
            "capability:overlay-draw",
            action.name(),
        ) =>
        {
            let input = super::experience_capability::experience_action_input_schema(action.name());
            let result =
                super::experience_capability::experience_action_result_schema(action.name());
            let expected_implementation =
                super::experience_capability::experience_action_implementation_plan_digest(
                    action.name(),
                    binding.source_content_digest(),
                )
                .ok()
                .flatten();
            let ceiling =
                super::experience_capability::experience_action_result_ceiling(action.name());
            input.is_some()
                && result.is_some()
                && action.input_schema_digest()
                    == input.and_then(|schema| schema_digest(schema)).as_ref()
                && action.result_schema_digest()
                    == result.and_then(|schema| schema_digest(schema)).as_ref()
                && action.implementation_plan_digest() == expected_implementation.as_ref()
                && action.transport_result_byte_ceiling() == ceiling
        },
        (
            "narration",
            AppPrimitiveExecutionClass::NarrationOwner,
            AppPrimitiveContainment::MediaRail,
        ) if interactive_dependency_alias_matches(
            dependency_ref,
            "capability:narration",
            action.name(),
        ) =>
        {
            let input = super::experience_capability::experience_action_input_schema(action.name());
            let result =
                super::experience_capability::experience_action_result_schema(action.name());
            let expected_implementation =
                super::experience_capability::experience_action_implementation_plan_digest(
                    action.name(),
                    binding.source_content_digest(),
                )
                .ok()
                .flatten();
            let ceiling =
                super::experience_capability::experience_action_result_ceiling(action.name());
            input.is_some()
                && result.is_some()
                && action.input_schema_digest()
                    == input.and_then(|schema| schema_digest(schema)).as_ref()
                && action.result_schema_digest()
                    == result.and_then(|schema| schema_digest(schema)).as_ref()
                && action.implementation_plan_digest() == expected_implementation.as_ref()
                && action.transport_result_byte_ceiling() == ceiling
        },
        _ => false,
    }
}

fn interactive_dependency_alias_matches(
    dependency_ref: &AppReference,
    canonical: &str,
    action_name: &str,
) -> bool {
    dependency_ref.as_str() == canonical
        || dependency_ref.as_str() == format!("{canonical}__{action_name}")
}

fn requested_agent_refs(
    manifest: &AppPackageManifest,
) -> Result<Vec<AppReference>, AppInstallationReviewError> {
    let mut names: Vec<&AppName> = manifest
        .app
        .workflows
        .values()
        .filter_map(|workflow| workflow.agent.as_ref())
        .collect();
    let default_runner = AppName::parse(DEFAULT_APP_WORKFLOW_AGENT)?;
    if manifest
        .app
        .workflows
        .values()
        .any(|workflow| workflow.agent.is_none())
    {
        names.push(&default_runner);
    }
    requested_named_refs("agent", names.into_iter())
}

fn reviewed_contribution_ports(
    manifest: &AppPackageManifest,
    package_lock: &AppPackageLock,
) -> Result<Vec<AppReviewedContributionPortGrant>, AppInstallationReviewError> {
    let requested_count = manifest
        .app
        .workflows
        .values()
        .map(|workflow| workflow.contribution_ports.len())
        .sum::<usize>();
    if requested_count != package_lock.contribution_bindings().len() {
        return Err(AppInstallationReviewError::InvalidGrant(
            "immutable contribution-port lock does not cover the reviewed manifest".to_owned(),
        ));
    }
    let mut reviewed = Vec::with_capacity(requested_count);
    for binding in package_lock.contribution_bindings() {
        let workflow = manifest
            .app
            .workflows
            .get(binding.workflow_id())
            .ok_or_else(|| {
                AppInstallationReviewError::InvalidGrant(format!(
                    "locked contribution workflow `{}` is not declared",
                    binding.workflow_id()
                ))
            })?;
        let declaration = workflow
            .contribution_ports
            .get(binding.port_id())
            .ok_or_else(|| {
                AppInstallationReviewError::InvalidGrant(format!(
                    "locked contribution port `{}/{}` is not declared",
                    binding.workflow_id(),
                    binding.port_id()
                ))
            })?;
        let result_digest = AppDigest::blake3_canonical_json(
            &serde_json::to_value(&workflow.result).map_err(|error| {
                AppInstallationReviewError::InvalidGrant(format!(
                    "cannot encode contribution result contract: {error}"
                ))
            })?,
        )
        .map_err(|error| {
            AppInstallationReviewError::InvalidGrant(format!(
                "cannot digest contribution result contract: {error}"
            ))
        })?;
        let workflow_digest =
            AppDigest::blake3_canonical_json(&serde_json::to_value(workflow).map_err(|error| {
                AppInstallationReviewError::InvalidGrant(format!(
                    "cannot encode contribution workflow declaration: {error}"
                ))
            })?)
            .map_err(|error| {
                AppInstallationReviewError::InvalidGrant(format!(
                    "cannot digest contribution workflow declaration: {error}"
                ))
            })?;
        let action_digests = manifest
            .app
            .actions
            .iter()
            .filter(|(_, action)| action.workflow == *binding.workflow_id())
            .map(|(action_id, action)| {
                AppDigest::blake3_canonical_json(&serde_json::to_value(action).map_err(
                    |error| {
                        AppInstallationReviewError::InvalidGrant(format!(
                            "cannot encode contribution action declaration: {error}"
                        ))
                    },
                )?)
                .map(|digest| (action_id.clone(), digest))
                .map_err(|error| {
                    AppInstallationReviewError::InvalidGrant(format!(
                        "cannot digest contribution action declaration: {error}"
                    ))
                })
            })
            .collect::<Result<BTreeMap<_, _>, AppInstallationReviewError>>()?;
        if binding.declaration() != declaration
            || binding.destination_binding()
                != AppContributionDestinationBinding::for_destination(declaration.destination)
            || binding.workflow_declaration_digest() != &workflow_digest
            || binding.workflow_result_digest() != &result_digest
            || binding.action_declaration_digests() != &action_digests
        {
            return Err(AppInstallationReviewError::InvalidGrant(format!(
                "locked contribution port `{}/{}` substitutes reviewed bytes",
                binding.workflow_id(),
                binding.port_id()
            )));
        }
        reviewed.push(
            AppReviewedContributionPortGrant {
                schema: "magician.app-reviewed-contribution-port-grant.v1".to_owned(),
                workflow_id: binding.workflow_id().clone(),
                port_id: binding.port_id().clone(),
                locked_port_digest: binding.binding_digest().clone(),
                source: declaration.source.clone(),
                destination: declaration.destination,
                destination_binding: AppContributionDestinationBinding::for_destination(
                    declaration.destination,
                ),
                purposes: declaration.purposes.clone(),
                audiences: declaration.audiences.clone(),
                evidence_classes: declaration.evidence_classes.clone(),
                frequency: declaration.frequency,
                maximum_retention_seconds: declaration.maximum_retention_seconds,
                grant_digest: AppDigest::blake3(b"pending-reviewed-contribution-port"),
            }
            .seal()?,
        );
    }
    Ok(reviewed)
}

fn reviewed_interactive_capabilities(
    package_lock: &AppPackageLock,
) -> Result<Vec<AppReviewedInteractiveCapabilityGrant>, AppInstallationReviewError> {
    let mut reviewed = Vec::new();
    for dependency in package_lock.dependencies() {
        let Some(binding) = dependency.interactive_binding() else {
            continue;
        };
        let primitive = dependency.primitive_binding().ok_or_else(|| {
            AppInstallationReviewError::InvalidGrant(format!(
                "interactive dependency `{}` lost its primitive binding",
                dependency.dependency_ref()
            ))
        })?;
        reviewed.push(binding.review_grant(primitive, binding.request().clone())?);
    }
    reviewed.sort_by(|left, right| left.dependency_ref().cmp(right.dependency_ref()));
    Ok(reviewed)
}

fn select_interactive_capability_grants(
    requested: &[AppReviewedInteractiveCapabilityGrant],
    selected: Option<&[AppInteractiveCapabilityGrantRequest]>,
    granted_tools: &[AppReference],
    package_lock: &AppPackageLock,
) -> Result<Vec<AppReviewedInteractiveCapabilityGrant>, AppInstallationReviewError> {
    let granted_tools = granted_tools.iter().collect::<BTreeSet<_>>();
    let Some(selected) = selected else {
        return Ok(requested
            .iter()
            .filter(|grant| granted_tools.contains(grant.dependency_ref()))
            .cloned()
            .collect());
    };
    if selected.len() > requested.len() {
        return Err(AppInstallationReviewError::InvalidGrant(
            "granted interactive capability set exceeds the reviewed request".to_owned(),
        ));
    }
    let requested = requested
        .iter()
        .map(|grant| (grant.dependency_ref(), grant))
        .collect::<BTreeMap<_, _>>();
    let mut grants = BTreeMap::new();
    for selection in selected {
        if !granted_tools.contains(&selection.dependency_ref) {
            return Err(AppInstallationReviewError::InvalidGrant(format!(
                "interactive dependency `{}` cannot be granted when its tool is denied",
                selection.dependency_ref
            )));
        }
        let reviewed = requested.get(&selection.dependency_ref).ok_or_else(|| {
            AppInstallationReviewError::InvalidGrant(format!(
                "interactive dependency `{}` was not shown for review",
                selection.dependency_ref
            ))
        })?;
        if reviewed.requested_request_digest() != &selection.reviewed_request_digest {
            return Err(AppInstallationReviewError::InvalidGrant(format!(
                "interactive dependency `{}` review digest is stale or substituted",
                selection.dependency_ref
            )));
        }
        let dependency = package_lock
            .capability(&selection.dependency_ref)
            .ok_or_else(|| {
                AppPackageLockError::LockedPrimitiveMissing(selection.dependency_ref.to_string())
            })?;
        let primitive = dependency.primitive_binding().ok_or_else(|| {
            AppPackageLockError::LockedPrimitiveBindingUnavailable(
                selection.dependency_ref.to_string(),
            )
        })?;
        let binding = dependency.interactive_binding().ok_or_else(|| {
            AppInstallationReviewError::InvalidGrant(format!(
                "interactive dependency `{}` requires package republish and re-review",
                selection.dependency_ref
            ))
        })?;
        let grant = binding.review_grant(primitive, selection.granted.clone())?;
        if grants
            .insert(selection.dependency_ref.clone(), grant)
            .is_some()
        {
            return Err(AppInstallationReviewError::InvalidGrant(format!(
                "interactive dependency `{}` is selected more than once",
                selection.dependency_ref
            )));
        }
    }
    Ok(grants.into_values().collect())
}

fn select_contribution_port_grants(
    requested: &[AppReviewedContributionPortGrant],
    selected: Option<&[AppContributionPortGrantRequest]>,
) -> Result<Vec<AppReviewedContributionPortGrant>, AppInstallationReviewError> {
    let Some(selected) = selected else {
        return Ok(requested.to_vec());
    };
    if selected.len() > requested.len() {
        return Err(AppInstallationReviewError::InvalidGrant(
            "granted contribution-port set exceeds the reviewed request".to_owned(),
        ));
    }
    let mut seen = BTreeSet::new();
    let mut grants = Vec::with_capacity(selected.len());
    for selected in selected {
        let key = (selected.workflow_id.clone(), selected.port_id.clone());
        if !seen.insert(key.clone()) {
            return Err(AppInstallationReviewError::InvalidGrant(format!(
                "contribution port `{}/{}` is granted more than once",
                selected.workflow_id, selected.port_id
            )));
        }
        let reviewed = requested
            .iter()
            .find(|candidate| {
                candidate.workflow_id == selected.workflow_id
                    && candidate.port_id == selected.port_id
            })
            .ok_or_else(|| {
                AppInstallationReviewError::InvalidGrant(format!(
                    "contribution port `{}/{}` was not requested",
                    selected.workflow_id, selected.port_id
                ))
            })?;
        if selected.reviewed_grant_digest != reviewed.grant_digest
            || !ordered_subset(&selected.purposes, &reviewed.purposes)
            || !ordered_subset(&selected.audiences, &reviewed.audiences)
            || !ordered_subset(&selected.evidence_classes, &reviewed.evidence_classes)
            || !selected.frequency.narrows(reviewed.frequency)
            || selected.maximum_retention_seconds == 0
            || selected.maximum_retention_seconds > reviewed.maximum_retention_seconds
        {
            return Err(AppInstallationReviewError::InvalidGrant(format!(
                "contribution port `{}/{}` widens or substitutes reviewed authority",
                selected.workflow_id, selected.port_id
            )));
        }
        let source = match &reviewed.source {
            AppContributionSource::MutationBackedEntityProjection {
                entity,
                selected_fields: requested_fields,
            } => {
                if !ordered_subset(&selected.selected_fields, requested_fields) {
                    return Err(AppInstallationReviewError::InvalidGrant(format!(
                        "contribution port `{}/{}` selects unreviewed result fields",
                        selected.workflow_id, selected.port_id
                    )));
                }
                AppContributionSource::MutationBackedEntityProjection {
                    entity: entity.clone(),
                    selected_fields: canonical_unique(selected.selected_fields.clone())?,
                }
            },
        };
        grants.push(
            AppReviewedContributionPortGrant {
                schema: reviewed.schema.clone(),
                workflow_id: reviewed.workflow_id.clone(),
                port_id: reviewed.port_id.clone(),
                locked_port_digest: reviewed.locked_port_digest.clone(),
                source,
                destination: reviewed.destination,
                destination_binding: reviewed.destination_binding,
                purposes: canonical_unique(selected.purposes.clone())?,
                audiences: canonical_unique(selected.audiences.clone())?,
                evidence_classes: canonical_unique(selected.evidence_classes.clone())?,
                frequency: selected.frequency,
                maximum_retention_seconds: selected.maximum_retention_seconds,
                grant_digest: AppDigest::blake3(b"pending-reviewed-contribution-port"),
            }
            .seal()?,
        );
    }
    grants.sort_by(|left, right| {
        (&left.workflow_id, &left.port_id).cmp(&(&right.workflow_id, &right.port_id))
    });
    Ok(grants)
}

fn ordered_subset<T: Ord>(selected: &[T], requested: &[T]) -> bool {
    if selected.is_empty() {
        return false;
    }
    let selected = selected.iter().collect::<BTreeSet<_>>();
    let requested = requested.iter().collect::<BTreeSet<_>>();
    selected.len() <= requested.len() && selected.is_subset(&requested)
}

fn canonical_unique<T: Ord>(values: Vec<T>) -> Result<Vec<T>, AppInstallationReviewError> {
    let original_len = values.len();
    let values = values.into_iter().collect::<BTreeSet<_>>();
    if values.len() != original_len || values.is_empty() {
        return Err(AppInstallationReviewError::InvalidGrant(
            "contribution-port narrowing is empty or contains duplicates".to_owned(),
        ));
    }
    Ok(values.into_iter().collect())
}

fn workflow_grant_needs(
    manifest: &AppPackageManifest,
) -> Result<Vec<AppReviewedWorkflowGrant>, AppInstallationReviewError> {
    let default_agent =
        canonicalize_prefixed_ref("agent")(&AppReference::parse(DEFAULT_APP_WORKFLOW_AGENT)?)?;
    let personality = canonicalize_prefixed_ref("personality");
    let mut workflows = Vec::new();
    for (workflow_id, workflow) in &manifest.app.workflows {
        let agent = match workflow.agent.as_ref() {
            Some(name) => canonicalize_prefixed_ref("agent")(&AppReference::parse(name.as_str())?)?,
            None => default_agent.clone(),
        };
        let personality = workflow
            .personality
            .as_ref()
            .map(|name| personality(&AppReference::parse(name.as_str())?))
            .transpose()?;
        let mut uses = BTreeSet::new();
        for tool in &workflow.uses {
            uses.insert(canonicalize_tool_ref(&AppReference::parse(tool.as_str())?)?);
        }
        workflows.push(AppReviewedWorkflowGrant {
            workflow_id: workflow_id.clone(),
            uses: uses.into_iter().collect(),
            agent,
            personality,
        });
    }
    workflows.sort_by(|left, right| left.workflow_id.cmp(&right.workflow_id));
    Ok(workflows)
}

fn inert_workflows_for_grant(
    workflows: &[AppReviewedWorkflowGrant],
    granted_tools: &[AppReference],
    granted_agents: &[AppReference],
    granted_personalities: &[AppReference],
    tool_dispatch: &[AppReviewedToolDispatch],
) -> Vec<AppInertWorkflow> {
    let tools: BTreeSet<_> = granted_tools.iter().collect();
    let agents: BTreeSet<_> = granted_agents.iter().collect();
    let personalities: BTreeSet<_> = granted_personalities.iter().collect();
    let mut inert = Vec::new();
    for workflow in workflows {
        let mut reasons = Vec::new();
        for tool in &workflow.uses {
            if !tools.contains(tool) {
                reasons.push(format!("missing tool `{tool}`"));
            } else if tool_dispatch
                .iter()
                .find(|reviewed| reviewed.tool == *tool)
                .is_some_and(|reviewed| !reviewed.dispatchable)
            {
                reasons.push(format!("tool `{tool}` has no admitted physical dispatch"));
            }
        }
        if !agents.contains(&workflow.agent) {
            reasons.push(format!("missing agent `{}`", workflow.agent));
        }
        if let Some(personality) = &workflow.personality {
            if !personalities.contains(personality) {
                reasons.push(format!("missing personality `{personality}`"));
            }
        }
        if !reasons.is_empty() {
            inert.push(AppInertWorkflow {
                workflow_id: workflow.workflow_id.clone(),
                reasons,
            });
        }
    }
    inert
}

/// The grant this attempt would produce if the owner approved it exactly as
/// requested.
///
/// Built only to be the right-hand side of a permission diff, never persisted
/// and never granted: the owner may still subset tools/agents/personalities at
/// approve time, which can only narrow it. Diffing as-requested is the
/// conservative direction — it shows the widest thing approving could do.
///
/// The digests are placeholders because a diff compares authority axes, not
/// identity; `compute_permission_diff` reads neither.
fn as_requested_grant_for_diff(
    review: &AppInstallationReview,
    installation: &AppInstallation,
    actor: &AppReference,
    now: DateTime<Utc>,
) -> AppGrantRevision {
    AppGrantRevision {
        installation_id: installation.installation_id.clone(),
        revision: installation
            .grant_revision
            .unwrap_or_else(|| AppRevision::new(1).expect("revision 1 is valid")),
        package_revision_ref: review.package_revision_ref.clone(),
        requested_tools: review.requested_tools.clone(),
        granted_tools: review.requested_tools.clone(),
        requested_agents: review.requested_agents.clone(),
        granted_agents: review.requested_agents.clone(),
        requested_personalities: review.requested_personalities.clone(),
        granted_personalities: review.requested_personalities.clone(),
        requested_interactive_capabilities: review.requested_interactive_capabilities.clone(),
        granted_interactive_capabilities: review.requested_interactive_capabilities.clone(),
        // As-requested on this axis too: the widest thing approving could
        // do is grant every reviewed entry point, so an update that adds
        // or re-binds a surface shows as an expansion in the diff. The
        // owner may still narrow at approve time, which can only shrink
        // the granted set.
        granted_custom_surface_entry_points: review
            .requested_custom_surface
            .as_ref()
            .map(|requested| granted_custom_surface_entries(&requested.entry_points))
            .unwrap_or_default(),
        requested_behavior_grants: review.requested_behaviors.clone(),
        granted_behavior_grants: review.requested_behaviors.clone(),
        requested_event_behavior_grants: review.requested_event_behaviors.clone(),
        granted_event_behavior_grants: review.requested_event_behaviors.clone(),
        requested_notification_grants: review.requested_notifications.clone(),
        granted_notification_grants: review.requested_notifications.clone(),
        requested_memory_read: None,
        granted_memory_read: None,
        // As requested: every secret the tools ask for, so a new key use on
        // update shows as widening.
        requested_secret_uses: review.requested_secret_uses.clone(),
        granted_secret_uses: review.requested_secret_uses.as_ref().map(|requests| {
            requests
                .iter()
                .map(super::secret_access::AppSecretUseRequest::grant)
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect()
        }),
        // Approving as it stands never grants "any public host": it needs
        // the owner's explicit tick, which the grant diff then reports.
        granted_any_public_host: false,
        requested_context_reads: Vec::new(),
        granted_context_reads: Vec::new(),
        requested_personal_agent_data_access: Vec::new(),
        granted_personal_agent_data_access: Vec::new(),
        requested_data_handling_policy: review.requested_data_handling_policy.clone(),
        granted_data_handling_policy: review.requested_data_handling_policy.clone(),
        granted_data_handling_policy_digest: AppDigest::blake3(b"permission-diff-only"),
        requested_background_execution: review.requested_background_execution.clone(),
        granted_background_execution: review.requested_background_execution.clone(),
        requested_network_policy: review.requested_network_policy.clone(),
        granted_network_policy: review.requested_network_policy.clone(),
        requested_resource_ceiling: review.requested_resource_ceiling.clone(),
        granted_resource_ceiling: review.requested_resource_ceiling.clone(),
        approved_by: actor.clone(),
        approved_at: now,
        authority_digest: AppDigest::blake3(b"permission-diff-only"),
        revoked_at: None,
    }
}

fn next_reviewed_revision(
    kind: AppLifecycleAttemptKind,
    current: Option<AppRevision>,
) -> Result<AppRevision, AppInstallationReviewError> {
    match kind {
        AppLifecycleAttemptKind::InitialInstall => Ok(AppRevision::new(1)?),
        AppLifecycleAttemptKind::Update | AppLifecycleAttemptKind::Reinstall => {
            let current = current.ok_or_else(|| {
                AppInstallationReviewError::NotReady(
                    "update or reinstall is missing the current revision".to_owned(),
                )
            })?;
            AppRevision::new(current.get().saturating_add(1)).map_err(Into::into)
        },
    }
}

fn reviewed_schema_migration(
    run: &Option<AppDurableUpdateRun>,
) -> (AppSchemaCompatibility, Option<AppReference>) {
    match run.as_ref().and_then(|run| run.migration_plan.as_ref()) {
        Some(plan) => (plan.compatibility(), Some(plan.plan_ref().clone())),
        None if run.is_some() => (AppSchemaCompatibility::Compatible, None),
        None => (AppSchemaCompatibility::Initial, None),
    }
}

fn requested_named_refs<'a>(
    prefix: &'static str,
    names: impl Iterator<Item = &'a super::models::AppName>,
) -> Result<Vec<AppReference>, AppInstallationReviewError> {
    let canonicalize = canonicalize_prefixed_ref(prefix);
    let mut refs = BTreeSet::new();
    for name in names {
        refs.insert(canonicalize(&AppReference::parse(name.as_str())?)?);
    }
    Ok(refs.into_iter().collect())
}

fn data_handling_policy(policy: &AppManifestPolicy) -> AppDataHandlingPolicy {
    AppDataHandlingPolicy {
        classification_floor: policy.classification_floor,
        model_processing: policy.model_processing,
        personal_agent_access: policy.personal_agent_access,
        memory_promotion: policy.memory_promotion,
        external_egress: policy.external_egress,
        approved_destinations: policy.approved_destinations.clone(),
    }
}

fn requested_network(policy: &AppManifestPolicy) -> AppNetworkPolicy {
    match policy.external_egress {
        AppExternalEgress::ApprovedDestinations => AppNetworkPolicy::ApprovedDestinations {
            destinations: policy.approved_destinations.clone(),
        },
        // "Any public host" is its own grant; the listed destinations (if
        // any) are still the named network policy.
        AppExternalEgress::AnyPublicHost if !policy.approved_destinations.is_empty() => {
            AppNetworkPolicy::ApprovedDestinations {
                destinations: policy.approved_destinations.clone(),
            }
        },
        AppExternalEgress::AnyPublicHost | AppExternalEgress::Denied => AppNetworkPolicy::Denied,
    }
}

fn reviewed_behavior_grants(
    manifest: &AppPackageManifest,
) -> Result<Vec<AppBehaviorGrant>, AppInstallationReviewError> {
    let mut reviewed = Vec::with_capacity(manifest.app.behaviors.len());
    for behavior in &manifest.app.behaviors {
        let declared = manifest
            .app
            .resources
            .behaviors
            .get(&behavior.id)
            .ok_or_else(|| {
                AppInstallationReviewError::InvalidGrant(format!(
                    "behavior `{}` is missing its reviewed resource ceiling",
                    behavior.id
                ))
            })?;
        let resources = AppBehaviorResourceCeiling {
            max_tokens_per_run: declared.per_run.max_tokens,
            max_cost_microusd_per_run: declared.per_run.max_cost_usd.microusd(),
            max_active_seconds_per_run: declared.per_run.max_active_seconds,
            max_tokens_per_month: declared.monthly.max_tokens,
            max_cost_microusd_per_month: declared.monthly.max_cost_usd.microusd(),
            max_starts_per_period: declared.max_starts_per_period,
            period_seconds: declared.period_seconds,
            max_causation_depth: declared.max_causation_depth,
            max_spend_depth: declared.max_spend_depth,
            max_contribution_proposals_per_run: declared.max_contribution_proposals_per_run,
        };
        let output_schema_digest = behavior
            .output_schema
            .as_ref()
            .map(contract_digest)
            .transpose()?;
        let input_selector_digest = contract_digest(&behavior.input)?;
        let steps_digest = super::manifest::app_behavior_steps_digest(&behavior.steps)
            .map_err(|error| AppInstallationReviewError::InvalidGrant(error.to_string()))?;
        let min_interval_seconds = behavior.cadence.min_interval_seconds();
        let reviewed_request_digest = app_behavior_request_digest(
            &behavior.id,
            &behavior.purpose,
            &behavior.action,
            &input_selector_digest,
            &behavior.operations,
            steps_digest.as_ref(),
            output_schema_digest.as_ref(),
            min_interval_seconds,
            &resources,
        )?;
        reviewed.push(AppBehaviorGrant {
            behavior_id: behavior.id.clone(),
            purpose: behavior.purpose.clone(),
            action: behavior.action.clone(),
            input_selector_digest,
            operations: behavior.operations.clone(),
            steps_digest,
            output_schema_digest,
            min_interval_seconds,
            resources,
            reviewed_request_digest,
        });
    }
    reviewed.sort_by(|left, right| left.behavior_id.cmp(&right.behavior_id));
    Ok(reviewed)
}

fn reviewed_event_behavior_grants(
    manifest: &AppPackageManifest,
) -> Result<Vec<AppEventBehaviorGrant>, AppInstallationReviewError> {
    let mut reviewed = Vec::with_capacity(manifest.app.event_behaviors.len());
    for behavior in &manifest.app.event_behaviors {
        let declared = manifest
            .app
            .resources
            .event_behaviors
            .get(&behavior.id)
            .ok_or_else(|| {
                AppInstallationReviewError::InvalidGrant(format!(
                    "event behavior `{}` is missing its reviewed resource ceiling",
                    behavior.id
                ))
            })?;
        let resources = AppBehaviorResourceCeiling {
            max_tokens_per_run: declared.per_run.max_tokens,
            max_cost_microusd_per_run: declared.per_run.max_cost_usd.microusd(),
            max_active_seconds_per_run: declared.per_run.max_active_seconds,
            max_tokens_per_month: declared.monthly.max_tokens,
            max_cost_microusd_per_month: declared.monthly.max_cost_usd.microusd(),
            max_starts_per_period: declared.max_starts_per_period,
            period_seconds: declared.period_seconds,
            max_causation_depth: declared.max_causation_depth,
            max_spend_depth: declared.max_spend_depth,
            max_contribution_proposals_per_run: declared.max_contribution_proposals_per_run,
        };
        let subscription_digest = app_event_subscription_digest(&behavior.subscription)?;
        let projection_schema = event_behavior_projection_schema(&behavior.subscription);
        let projection_schema_digest = contract_digest(&projection_schema)?;
        let output_schema_digest = behavior
            .output_schema
            .as_ref()
            .map(contract_digest)
            .transpose()?;
        let steps_digest = super::manifest::app_behavior_steps_digest(&behavior.steps)
            .map_err(|error| AppInstallationReviewError::InvalidGrant(error.to_string()))?;
        let reviewed_request_digest = app_event_behavior_request_digest(
            &behavior.id,
            &behavior.purpose,
            &behavior.action,
            &subscription_digest,
            &projection_schema_digest,
            &behavior.operations,
            steps_digest.as_ref(),
            output_schema_digest.as_ref(),
            behavior.min_interval_seconds,
            &resources,
        )?;
        reviewed.push(AppEventBehaviorGrant {
            event_behavior_id: behavior.id.clone(),
            purpose: behavior.purpose.clone(),
            action: behavior.action.clone(),
            subscription: behavior.subscription.clone(),
            subscription_digest,
            projection_schema_digest,
            steps_digest,
            operations: behavior.operations.clone(),
            output_schema_digest,
            min_interval_seconds: behavior.min_interval_seconds,
            resources,
            reviewed_request_digest,
        });
    }
    reviewed.sort_by(|left, right| left.event_behavior_id.cmp(&right.event_behavior_id));
    Ok(reviewed)
}

fn reviewed_notification_grants(
    manifest: &AppPackageManifest,
) -> Result<Vec<AppNotificationGrant>, AppInstallationReviewError> {
    let mut reviewed = Vec::new();
    for (workflow_id, workflow) in &manifest.app.workflows {
        for (port_id, port) in &workflow.notification_ports {
            let reviewed_request_digest = app_notification_request_digest(
                workflow_id,
                port_id,
                &port.purpose,
                port.kind,
                port.severity_ceiling,
                port.max_notifications_per_period,
                port.period_seconds,
                port.max_pending,
                port.ttl_seconds,
            )?;
            reviewed.push(AppNotificationGrant {
                workflow_id: workflow_id.clone(),
                port_id: port_id.clone(),
                purpose: port.purpose.clone(),
                kind: port.kind,
                severity_ceiling: port.severity_ceiling,
                max_notifications_per_period: port.max_notifications_per_period,
                period_seconds: port.period_seconds,
                max_pending: port.max_pending,
                ttl_seconds: port.ttl_seconds,
                reviewed_request_digest,
            });
        }
    }
    Ok(reviewed)
}

fn select_behavior_grants(
    requested: &[AppBehaviorGrant],
    selected: Option<&[AppBehaviorGrantRequest]>,
) -> Result<Vec<AppBehaviorGrant>, AppInstallationReviewError> {
    let Some(selected) = selected else {
        return Ok(requested.to_vec());
    };
    let requested_by_id = requested
        .iter()
        .map(|request| (&request.behavior_id, request))
        .collect::<BTreeMap<_, _>>();
    let mut selected_ids = BTreeSet::new();
    let mut granted = Vec::with_capacity(selected.len());
    for selection in selected {
        if !selected_ids.insert(selection.behavior_id.clone()) {
            return Err(AppInstallationReviewError::InvalidGrant(
                "granted_behaviors contains duplicate behavior ids".to_owned(),
            ));
        }
        let request = requested_by_id.get(&selection.behavior_id).ok_or_else(|| {
            AppInstallationReviewError::InvalidGrant(format!(
                "behavior `{}` was not in the reviewed request",
                selection.behavior_id
            ))
        })?;
        if selection.reviewed_request_digest != request.reviewed_request_digest
            || selection.min_interval_seconds < request.min_interval_seconds
            || selection.min_interval_seconds > super::manifest::APP_BEHAVIOR_MAX_INTERVAL_SECONDS
            || !selected_behavior_resources_narrow(&selection.resources, &request.resources)
        {
            return Err(AppInstallationReviewError::InvalidGrant(format!(
                "behavior `{}` widens or substitutes the reviewed cadence/resource request",
                selection.behavior_id
            )));
        }
        let mut narrowed = (*request).clone();
        narrowed.min_interval_seconds = selection.min_interval_seconds;
        narrowed.resources = selection.resources.clone();
        granted.push(narrowed);
    }
    granted.sort_by(|left, right| left.behavior_id.cmp(&right.behavior_id));
    Ok(granted)
}

fn select_event_behavior_grants(
    requested: &[AppEventBehaviorGrant],
    selected: Option<&[AppEventBehaviorGrantRequest]>,
) -> Result<Vec<AppEventBehaviorGrant>, AppInstallationReviewError> {
    let Some(selected) = selected else {
        return Ok(Vec::new());
    };
    let requested_by_id = requested
        .iter()
        .map(|request| (&request.event_behavior_id, request))
        .collect::<BTreeMap<_, _>>();
    let mut selected_ids = BTreeSet::new();
    let mut granted = Vec::with_capacity(selected.len());
    for selection in selected {
        if !selected_ids.insert(selection.event_behavior_id.clone()) {
            return Err(AppInstallationReviewError::InvalidGrant(
                "granted_event_behaviors contains duplicate event behavior ids".to_owned(),
            ));
        }
        let request = requested_by_id
            .get(&selection.event_behavior_id)
            .ok_or_else(|| {
                AppInstallationReviewError::InvalidGrant(format!(
                    "event behavior `{}` was not in the reviewed request",
                    selection.event_behavior_id
                ))
            })?;
        if selection.reviewed_request_digest != request.reviewed_request_digest
            || selection.min_interval_seconds < request.min_interval_seconds
            || selection.min_interval_seconds > super::manifest::APP_BEHAVIOR_MAX_INTERVAL_SECONDS
            || !selected_behavior_resources_narrow(&selection.resources, &request.resources)
        {
            return Err(AppInstallationReviewError::InvalidGrant(format!(
                "event behavior `{}` widens or substitutes the reviewed interval/resource request",
                selection.event_behavior_id
            )));
        }
        let mut narrowed = (*request).clone();
        narrowed.min_interval_seconds = selection.min_interval_seconds;
        narrowed.resources = selection.resources.clone();
        granted.push(narrowed);
    }
    granted.sort_by(|left, right| left.event_behavior_id.cmp(&right.event_behavior_id));
    Ok(granted)
}

fn select_notification_grants(
    requested: &[AppNotificationGrant],
    selected: Option<&[AppNotificationGrantRequest]>,
) -> Result<Vec<AppNotificationGrant>, AppInstallationReviewError> {
    let Some(selected) = selected else {
        return Ok(Vec::new());
    };
    let requested_by_key = requested
        .iter()
        .map(|request| {
            (
                (request.workflow_id.clone(), request.port_id.clone()),
                request,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut selected_keys = BTreeSet::new();
    let mut granted = Vec::with_capacity(selected.len());
    for selection in selected {
        let key = (selection.workflow_id.clone(), selection.port_id.clone());
        if !selected_keys.insert(key.clone()) {
            return Err(AppInstallationReviewError::InvalidGrant(
                "granted_notifications contains duplicate workflow/port identities".to_owned(),
            ));
        }
        let request = requested_by_key.get(&key).ok_or_else(|| {
            AppInstallationReviewError::InvalidGrant(format!(
                "notification port `{}.{}` was not in the reviewed request",
                selection.workflow_id, selection.port_id
            ))
        })?;
        if selection.reviewed_request_digest != request.reviewed_request_digest
            || selection.period_seconds != request.period_seconds
            || notification_severity_rank(selection.severity_ceiling)
                > notification_severity_rank(request.severity_ceiling)
            || selection.max_notifications_per_period == 0
            || selection.max_notifications_per_period > request.max_notifications_per_period
            || selection.max_pending == 0
            || selection.max_pending > request.max_pending
            || selection.ttl_seconds < super::manifest::APP_NOTIFICATION_MIN_TTL_SECONDS
            || selection.ttl_seconds > request.ttl_seconds
        {
            return Err(AppInstallationReviewError::InvalidGrant(format!(
                "notification port `{}.{}` widens or substitutes the reviewed request",
                selection.workflow_id, selection.port_id
            )));
        }
        let mut narrowed = (*request).clone();
        narrowed.severity_ceiling = selection.severity_ceiling;
        narrowed.max_notifications_per_period = selection.max_notifications_per_period;
        narrowed.max_pending = selection.max_pending;
        narrowed.ttl_seconds = selection.ttl_seconds;
        granted.push(narrowed);
    }
    granted.sort_by(|left, right| {
        (&left.workflow_id, &left.port_id).cmp(&(&right.workflow_id, &right.port_id))
    });
    Ok(granted)
}

fn notification_severity_rank(severity: AppNotificationSeverityV1) -> u8 {
    match severity {
        AppNotificationSeverityV1::Info => 0,
        AppNotificationSeverityV1::Warning => 1,
        AppNotificationSeverityV1::Critical => 2,
    }
}

fn selected_behavior_resources_narrow(
    grant: &AppBehaviorResourceCeiling,
    request: &AppBehaviorResourceCeiling,
) -> bool {
    grant.max_tokens_per_run > 0
        && grant.max_cost_microusd_per_run > 0
        && grant.max_active_seconds_per_run > 0
        && grant.max_tokens_per_month > 0
        && grant.max_cost_microusd_per_month > 0
        && grant.max_starts_per_period > 0
        && grant.max_causation_depth > 0
        && grant.max_spend_depth > 0
        && grant.period_seconds == request.period_seconds
        && grant.max_tokens_per_run <= request.max_tokens_per_run
        && grant.max_cost_microusd_per_run <= request.max_cost_microusd_per_run
        && grant.max_active_seconds_per_run <= request.max_active_seconds_per_run
        && grant.max_tokens_per_month <= request.max_tokens_per_month
        && grant.max_cost_microusd_per_month <= request.max_cost_microusd_per_month
        && grant.max_starts_per_period <= request.max_starts_per_period
        && grant.max_causation_depth <= request.max_causation_depth
        && grant.max_spend_depth <= request.max_spend_depth
        && grant.max_contribution_proposals_per_run <= request.max_contribution_proposals_per_run
}

fn requested_background(body: &AppManifestBody) -> AppBackgroundExecution {
    if let Some(min_interval_seconds) = body
        .behaviors
        .iter()
        .map(|behavior| behavior.cadence.min_interval_seconds())
        .chain(
            body.event_behaviors
                .iter()
                .map(|behavior| behavior.min_interval_seconds),
        )
        .min()
    {
        return AppBackgroundExecution::Granted {
            min_interval_seconds,
            max_concurrent_runs: 1,
        };
    }
    if body.workflows.values().any(|workflow| {
        matches!(
            workflow.trigger,
            AppManifestTrigger::Schedule | AppManifestTrigger::Event
        )
    }) {
        AppBackgroundExecution::Granted {
            min_interval_seconds: 60,
            max_concurrent_runs: 1,
        }
    } else {
        AppBackgroundExecution::Denied
    }
}

fn selected_background_execution(
    review: &AppInstallationReview,
    scheduled: &[AppBehaviorGrant],
    events: &[AppEventBehaviorGrant],
) -> AppBackgroundExecution {
    // Preserve the pre-declaration compatibility path for legacy Event or
    // Schedule workflows. Once either exact behavior vocabulary is requested,
    // unattended authority exists only for an explicitly granted entry.
    if review.requested_behaviors.is_empty() && review.requested_event_behaviors.is_empty() {
        return review.requested_background_execution.clone();
    }
    scheduled
        .iter()
        .map(|grant| grant.min_interval_seconds)
        .chain(events.iter().map(|grant| grant.min_interval_seconds))
        .min()
        .map_or(AppBackgroundExecution::Denied, |min_interval_seconds| {
            AppBackgroundExecution::Granted {
                min_interval_seconds,
                max_concurrent_runs: 1,
            }
        })
}

fn requested_resource_ceiling(
    resources: &AppManifestResources,
    background: AppBackgroundExecution,
    network: &AppNetworkPolicy,
) -> AppResourceCeiling {
    let background_runs = match background {
        AppBackgroundExecution::Granted {
            max_concurrent_runs,
            ..
        } => max_concurrent_runs,
        AppBackgroundExecution::Denied => 0,
    };
    let monthly = resources.monthly.max_tokens;
    let per_run = resources.per_run.max_tokens.min(monthly);
    // Prefer output so a 1-token ceiling can still emit. Even splits keep
    // input as the remainder.
    let max_output_tokens = per_run.div_ceil(2);
    let max_input_tokens = per_run.saturating_sub(max_output_tokens);
    let max_browser_network_actions = match network {
        AppNetworkPolicy::Denied => 0,
        AppNetworkPolicy::ApprovedDestinations { destinations } => {
            // Keep the ceiling tight: a few fetches per approved destination.
            // Zero here would grant network policy and then refuse every
            // attested external dispatch.
            u64::try_from(destinations.len().saturating_mul(4).max(1)).unwrap_or(1)
        },
    };
    AppResourceCeiling {
        max_input_tokens,
        max_output_tokens,
        max_cost_microusd: resources.per_run.max_cost_usd.microusd(),
        max_paid_tool_invocations: 0,
        max_active_seconds: resources.per_run.max_active_seconds,
        max_lifetime_seconds: resources.per_run.max_active_seconds,
        max_browser_network_actions,
        max_concurrent_foreground_runs: 1,
        max_concurrent_background_runs: background_runs,
        max_records: resources.storage.max_records,
        max_payload_bytes: resources.storage.max_bytes,
        max_attachment_bytes: 0,
        max_monthly_tokens: monthly,
        max_monthly_cost_microusd: resources.monthly.max_cost_usd.microusd(),
    }
}

fn canonicalize_tool_ref(value: &AppReference) -> Result<AppReference, AppInstallationReviewError> {
    canonical_app_tool_ref(value.as_str()).map_err(Into::into)
}

fn canonicalize_prefixed_ref(
    prefix: &'static str,
) -> impl Fn(&AppReference) -> Result<AppReference, AppInstallationReviewError> {
    move |value| {
        let raw = value.as_str();
        let expected = format!("{prefix}:");
        if raw.starts_with(&expected) {
            Ok(value.clone())
        } else if raw.contains(':') {
            Err(AppInstallationReviewError::InvalidGrant(format!(
                "granted {prefix} `{raw}` is not a {prefix} identity"
            )))
        } else {
            Ok(AppReference::parse(format!("{prefix}:{raw}"))?)
        }
    }
}

fn select_granted_subset(
    field: &str,
    requested: &[AppReference],
    granted: Option<&[AppReference]>,
    canonicalize: impl Fn(&AppReference) -> Result<AppReference, AppInstallationReviewError>,
) -> Result<Vec<AppReference>, AppInstallationReviewError> {
    let Some(granted) = granted else {
        return Ok(requested.to_vec());
    };
    let requested_set: BTreeSet<_> = requested.iter().cloned().collect();
    let mut unique = BTreeSet::new();
    for item in granted {
        let item = canonicalize(item)?;
        if !requested_set.contains(&item) {
            return Err(AppInstallationReviewError::InvalidGrant(format!(
                "granted {field} `{item}` was not requested by the package"
            )));
        }
        unique.insert(item);
    }
    Ok(unique.into_iter().collect())
}

fn granted_authority_digest(
    grant: &AppGrantRevision,
    contribution_grants: &[AppReviewedContributionPortGrant],
) -> Result<AppDigest, AppInstallationReviewError> {
    app_granted_authority_digest(grant, contribution_grants).map_err(Into::into)
}

/// Freeze the owner-narrowed entry points into the durable grant shape.
/// The route/document/digest triple is copied verbatim from the reviewed
/// request: an empty set stays empty (the grant is never implicit-all) and
/// the digest is the fence the runtime host re-verifies against the live
/// package bytes.
fn granted_custom_surface_entries(
    narrowed: &[AppReviewedCustomSurfaceEntryPoint],
) -> Vec<AppGrantedCustomSurfaceEntryPoint> {
    narrowed
        .iter()
        .map(|entry| AppGrantedCustomSurfaceEntryPoint {
            route: entry.route.clone(),
            document: entry.document.clone(),
            document_digest: entry.document_digest.clone(),
        })
        .collect()
}

fn build_owner_approval(
    authenticated: &AuthenticatedAppScope,
    attempt: &AppLifecycleAttempt,
    package: &AppPackageRevision,
    grant: &AppGrantRevision,
    schema: &super::records::AppSchemaRevision,
    workflow_material_bindings: &[AppReviewedWorkflowMaterialBinding],
    contribution_grants: &[AppReviewedContributionPortGrant],
    now: DateTime<Utc>,
) -> Result<AppInstallationApproval, AppInstallationReviewError> {
    let authentication = match authenticated.authentication() {
        AppScopeAuthentication::AuthenticatedSession => {
            AppApprovalAuthentication::AuthenticatedSession
        },
        AppScopeAuthentication::TrustedLoopbackSingleUser => {
            AppApprovalAuthentication::TrustedLoopbackSingleUser
        },
        AppScopeAuthentication::TrustedSystemPackageHost => {
            AppApprovalAuthentication::TrustedSystemPackageHost
        },
        AppScopeAuthentication::SystemWorker
        | AppScopeAuthentication::TaskExecution
        | AppScopeAuthentication::ReviewedBackgroundLaunch => {
            return Err(AppInstallationReviewError::InvalidGrant(
                "background workers cannot approve an installation".to_owned(),
            ));
        },
    };
    let approval_id = expected_app_installation_approval_ref(
        &attempt.attempt_id,
        authenticated.session_ref(),
        &grant.authority_digest,
        workflow_material_bindings,
    )?;
    Ok(AppInstallationApproval {
        approval_id,
        revision: AppRevision::new(1)?,
        authenticated_scope_ref: authenticated.scope_binding_ref().clone(),
        actor_ref: authenticated.actor_ref().clone(),
        session_ref: authenticated.session_ref().clone(),
        authentication,
        authentication_revision: authenticated.authentication_revision(),
        install_or_update_attempt_id: attempt.attempt_id.clone(),
        package_content_digest: package.content_digest.clone(),
        requested_authority_digest: package.requested_authority_digest.clone(),
        granted_authority_digest: grant.authority_digest.clone(),
        data_policy_diff_digest: contract_digest(&(
            &grant.requested_data_handling_policy,
            &grant.granted_data_handling_policy,
        ))?,
        resource_diff_digest: contract_digest(&(
            &grant.requested_resource_ceiling,
            &grant.granted_resource_ceiling,
        ))?,
        schema_diff_digest: contract_digest(schema)?,
        migration_diff_digest: contract_digest(&(
            schema.compatibility_with_previous,
            &schema.migration_plan_ref,
        ))?,
        global_policy_revision: AppRevision::new(CURRENT_APP_GLOBAL_POLICY_REVISION)?,
        workflow_material_bindings: workflow_material_bindings.to_vec(),
        contribution_grants: contribution_grants.to_vec(),
        interactive_capability_grants: grant.granted_interactive_capabilities.clone(),
        issued_at: now,
        expires_at: now + APPROVAL_LIFETIME,
        consumed_at: None,
        consumed_installation_revision: None,
    })
}

fn contract_digest<T: Serialize>(value: &T) -> Result<AppDigest, AppInstallationReviewError> {
    let value = serde_json::to_value(value).map_err(|error| {
        AppInstallationReviewError::InvalidGrant(format!("cannot encode grant digest: {error}"))
    })?;
    AppDigest::blake3_canonical_json(&value).map_err(|error| {
        AppInstallationReviewError::InvalidGrant(format!("cannot digest grant: {error}"))
    })
}

/// Reproduce the immutable approval identity from the material actually shown
/// during review. Runtime consumers use this seam to reopen a consumed
/// approval without maintaining a second approval-id derivation.
pub(crate) fn expected_app_installation_approval_ref(
    attempt_id: &AppReference,
    session_ref: &AppReference,
    granted_authority_digest: &AppDigest,
    workflow_material_bindings: &[AppReviewedWorkflowMaterialBinding],
) -> Result<AppReference, AppInstallationReviewError> {
    let workflow_material_digest = contract_digest(&workflow_material_bindings)?;
    derived_reference(
        "approval:app-install",
        &[
            attempt_id.as_str(),
            session_ref.as_str(),
            granted_authority_digest.as_str(),
            workflow_material_digest.as_str(),
        ],
    )
}

fn derived_reference(
    prefix: &str,
    parts: &[&str],
) -> Result<AppReference, AppInstallationReviewError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(prefix.as_bytes());
    for part in parts {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    AppReference::parse(format!("{prefix}:{}", hasher.finalize().to_hex())).map_err(Into::into)
}

fn reviewed_memory_read(
    manifest: &super::manifest::AppPackageManifest,
) -> Result<Option<AppReviewedMemoryRead>, AppInstallationReviewError> {
    let Some(request) = manifest
        .app
        .memory
        .as_ref()
        .and_then(|memory| memory.read.clone())
    else {
        return Ok(None);
    };
    let invalid = |reason: &str| AppInstallationReviewError::InvalidGrant(reason.to_owned());
    let request_digest = super::memory_access::memory_read_request_digest(&request)
        .ok_or_else(|| invalid("memory read request could not be digested"))?;
    let default_grant = super::memory_access::default_memory_read_grant(&request)
        .ok_or_else(|| invalid("memory read request could not be digested"))?;
    let sensitive_tiers = request
        .user_tiers
        .iter()
        .filter(|tier| {
            super::memory_access::app_memory_tier_readability(tier)
                != Some(super::memory_access::AppMemoryTierReadability::Ordinary)
        })
        .cloned()
        .collect();
    Ok(Some(AppReviewedMemoryRead {
        request,
        request_digest,
        sensitive_tiers,
        default_grant,
    }))
}

/// The memory grant an approval creates: the owner's explicit choice (which
/// must pin the reviewed request and stay within it), or the reviewed default.
/// A choice for an app that requested no memory is refused.
fn select_memory_read_grant(
    reviewed: Option<&AppReviewedMemoryRead>,
    chosen: Option<&AppMemoryReadGrantRequest>,
) -> Result<Option<super::memory_access::AppMemoryReadGrant>, AppInstallationReviewError> {
    match (reviewed, chosen) {
        (None, None) => Ok(None),
        (None, Some(_)) => Err(AppInstallationReviewError::InvalidGrant(
            "this app requested no memory access".to_owned(),
        )),
        (Some(reviewed), None) => Ok(Some(reviewed.default_grant.clone())),
        (Some(reviewed), Some(chosen)) => super::memory_access::owner_memory_read_grant(
            &reviewed.request,
            &chosen.reviewed_request_digest,
            chosen.interactive.clone(),
            chosen.background.clone(),
        )
        .map(Some)
        .map_err(AppInstallationReviewError::InvalidGrant),
    }
}

/// The secret uses of every locked OS-jail tool, from its reviewed source.
/// A tool whose secrets contract is outside the supported shape surfaces no
/// request; the jail refuses to run it, so nothing can be granted for it.
fn reviewed_secret_uses(
    manifest: &AppPackageManifest,
    snapshot: &super::primitive_catalog::AppPrimitiveCatalogSnapshot,
) -> Result<Option<Vec<super::secret_access::AppSecretUseRequest>>, AppInstallationReviewError> {
    let mut requests = std::collections::BTreeSet::new();
    for tool in manifest.app.dependencies.declared_tools()? {
        let selector = tool
            .primitive_ref
            .as_ref()
            .map(AppReference::as_str)
            .unwrap_or_else(|| tool.name.as_str());
        let Ok(descriptor) = snapshot.resolve(selector) else {
            continue;
        };
        if descriptor.kind() != AppPrimitiveKind::ToolSkill
            || descriptor.containment() == AppPrimitiveContainment::GovernedMcp
        {
            continue;
        }
        let Some(source) = snapshot
            .source_material(descriptor.identity())
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
        else {
            continue;
        };
        let tool_ref = AppReference::parse(format!("capability:{}", tool.name))?;
        if let Ok(found) = super::secret_access::app_secret_use_requests(&tool_ref, source) {
            requests.extend(found);
        }
    }
    if requests.len() > super::secret_access::MAX_APP_SECRET_USE_REQUESTS {
        return Err(AppInstallationReviewError::InvalidGrant(
            "the app's tools request too many secrets".to_owned(),
        ));
    }
    let mut requests = requests.into_iter().collect::<Vec<_>>();
    super::secret_access::mark_ungrantable_secret_uses(
        &mut requests,
        &super::secret_access::network_policy_hosts(&requested_network(
            &manifest.app.data_policy.defaults,
        )),
        manifest.app.data_policy.defaults.external_egress == AppExternalEgress::AnyPublicHost,
    );
    Ok((!requests.is_empty()).then_some(requests))
}

/// How each locked OS-jail tool runs: in place from its skill (and which
/// skill), and which hosts it declares. Only tools that run in place or
/// declare hosts are listed.
fn reviewed_tool_runtime(
    manifest: &AppPackageManifest,
    snapshot: &super::primitive_catalog::AppPrimitiveCatalogSnapshot,
) -> Result<Vec<AppReviewedToolRuntime>, AppInstallationReviewError> {
    let mut runtime = Vec::new();
    for tool in manifest.app.dependencies.declared_tools()? {
        let selector = tool
            .primitive_ref
            .as_ref()
            .map(AppReference::as_str)
            .unwrap_or_else(|| tool.name.as_str());
        let Ok(descriptor) = snapshot.resolve(selector) else {
            continue;
        };
        if descriptor.kind() != AppPrimitiveKind::ToolSkill
            || descriptor.containment() == AppPrimitiveContainment::GovernedMcp
        {
            continue;
        }
        let Some(source) = snapshot
            .source_material(descriptor.identity())
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
        else {
            continue;
        };
        let (in_place_from, declared_hosts, reaches_granted_hosts) =
            super::os_jail::reviewed_os_jail_runtime(
                source,
                snapshot.source_directory(descriptor.identity()),
            );
        if in_place_from.is_none() && declared_hosts.is_empty() {
            continue;
        }
        let requested_hosts = match requested_network(&manifest.app.data_policy.defaults) {
            AppNetworkPolicy::ApprovedDestinations { destinations } => destinations
                .iter()
                .filter_map(|destination| destination.as_str().strip_prefix("destination:"))
                .map(str::to_owned)
                .collect::<Vec<_>>(),
            AppNetworkPolicy::Denied => Vec::new(),
        };
        let reachable_hosts = if declared_hosts == ["*"] {
            Vec::new()
        } else if declared_hosts.is_empty() {
            if reaches_granted_hosts {
                requested_hosts
            } else {
                Vec::new()
            }
        } else {
            declared_hosts
                .iter()
                .filter(|host| requested_hosts.contains(host))
                .cloned()
                .collect()
        };
        runtime.push(AppReviewedToolRuntime {
            tool: AppReference::parse(format!("capability:{}", tool.name))?,
            in_place_from,
            declared_hosts,
            reaches_granted_hosts,
            reachable_hosts,
        });
    }
    runtime.sort_by(|left, right| left.tool.cmp(&right.tool));
    Ok(runtime)
}

/// The owner's secret-use grant. Absent or empty grants nothing.
fn select_secret_use_grant(
    reviewed: Option<&[super::secret_access::AppSecretUseRequest]>,
    chosen: Option<&[super::secret_access::AppSecretUseGrant]>,
    app_hosts: &[String],
    app_requests_any_host: bool,
) -> Result<Option<Vec<super::secret_access::AppSecretUseGrant>>, AppInstallationReviewError> {
    match (reviewed, chosen) {
        (None, None) => Ok(None),
        (None, Some(chosen)) if chosen.is_empty() => Ok(None),
        (None, Some(_)) => Err(AppInstallationReviewError::InvalidGrant(
            "this app's tools request no secrets".to_owned(),
        )),
        (Some(_), None) => Ok(Some(Vec::new())),
        // An unscoped key of a tool that needs a scope (a grant made before
        // per-key scopes, re-submitted as it stands) stays ungranted.
        (Some(reviewed), Some(chosen)) => super::secret_access::validate_secret_use_grant(
            reviewed,
            &super::secret_access::without_unscoped_legacy_grants(reviewed, chosen),
            app_hosts,
            app_requests_any_host,
        )
        .map(Some)
        .map_err(|error| AppInstallationReviewError::InvalidGrant(error.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_memory_choice_must_narrow_the_reviewed_request() {
        use super::super::memory_access::{
            default_memory_read_grant, memory_read_request_digest, AppMemoryReadRequest,
            AppMemoryReadSelection,
        };
        let request = AppMemoryReadRequest {
            user_tiers: vec!["preferences".into(), "identity".into()],
            agents: vec!["scribe".into()],
            purpose: "Personalise".into(),
        };
        let reviewed = AppReviewedMemoryRead {
            request: request.clone(),
            request_digest: memory_read_request_digest(&request).unwrap(),
            sensitive_tiers: vec!["identity".into()],
            default_grant: default_memory_read_grant(&request).unwrap(),
        };
        assert_eq!(select_memory_read_grant(None, None).unwrap(), None);
        assert_eq!(
            select_memory_read_grant(Some(&reviewed), None).unwrap(),
            Some(reviewed.default_grant.clone()),
            "omitting the choice takes the reviewed default"
        );
        let chosen = |tiers: &[&str], digest: AppDigest| AppMemoryReadGrantRequest {
            reviewed_request_digest: digest,
            interactive: AppMemoryReadSelection {
                user_tiers: tiers.iter().map(|tier| (*tier).to_owned()).collect(),
                agents: Vec::new(),
            },
            background: AppMemoryReadSelection::default(),
        };
        let ticked = select_memory_read_grant(
            Some(&reviewed),
            Some(&chosen(&["identity"], reviewed.request_digest.clone())),
        )
        .unwrap()
        .unwrap();
        assert!(
            ticked.interactive.grants_tier("identity"),
            "an explicit tick grants a sensitive tier"
        );
        assert!(select_memory_read_grant(
            Some(&reviewed),
            Some(&chosen(&["contacts"], reviewed.request_digest.clone()))
        )
        .is_err());
        assert!(select_memory_read_grant(
            Some(&reviewed),
            Some(&chosen(
                &["preferences"],
                AppDigest::blake3(b"other request")
            ))
        )
        .is_err());
        assert!(
            select_memory_read_grant(None, Some(&chosen(&[], reviewed.request_digest.clone())))
                .is_err(),
            "a memory choice for an app that requested none is refused"
        );
    }
    use magician::magician_v2::{
        apps::{
            authoring::{
                run_app_authoring_command, AppAuthoringCommand, AppCatalogDiscoverArgs,
                AppCheckArgs, AppInitArgs, AppPackArgs,
            },
            candidate_publication::AppCandidatePublicationService,
            entity_store::AppEntityStoreService,
            manifest::{
                parse_app_manifest_frontmatter, tests::valid_skill_document, AppPackageLimits,
            },
            package_lock::AppLockedPrimitiveBinding,
            package_staging::AppPackageStager,
            package_transfer::admit_package_archive,
            primitive_catalog::AppPrimitiveCatalogBuilder,
            records::{AppEventSubscriptionV1, AppEventTerminalOutcomeV1, AppNotificationKindV1},
            registry::{
                tests::{authenticated_scope, canonical_tempdir, time},
                AppRegistryService,
            },
            tool_eligibility::typed_app_tool_document,
        },
        artifact_v2::workspace::ArtifactV2Workspace,
        execution::compiled_providers::{
            embedded_compiled_pack_defs_ref, embedded_compiled_pack_yaml,
        },
    };

    fn item4_behavior_resources() -> AppBehaviorResourceCeiling {
        AppBehaviorResourceCeiling {
            max_tokens_per_run: 100,
            max_cost_microusd_per_run: 100,
            max_active_seconds_per_run: 30,
            max_tokens_per_month: 1_000,
            max_cost_microusd_per_month: 1_000,
            max_starts_per_period: 4,
            period_seconds: 3_600,
            max_causation_depth: 2,
            max_spend_depth: 2,
            max_contribution_proposals_per_run: 1,
        }
    }

    fn item4_event_grant() -> AppEventBehaviorGrant {
        AppEventBehaviorGrant {
            event_behavior_id: AppName::parse("on_completion").unwrap(),
            purpose: "Summarize one canonical completion.".to_owned(),
            action: AppName::parse("summarize_completion").unwrap(),
            subscription: AppEventSubscriptionV1::InstallationExecutionTerminal {
                outcomes: vec![AppEventTerminalOutcomeV1::Succeeded],
            },
            subscription_digest: AppDigest::blake3(b"subscription"),
            projection_schema_digest: AppDigest::blake3(b"projection"),
            steps_digest: None,
            operations: Vec::new(),
            output_schema_digest: None,
            min_interval_seconds: 60,
            resources: item4_behavior_resources(),
            reviewed_request_digest: AppDigest::blake3(b"event-review"),
        }
    }

    fn item4_notification_grant() -> AppNotificationGrant {
        AppNotificationGrant {
            workflow_id: AppName::parse("daily_digest").unwrap(),
            port_id: AppName::parse("owner_briefing").unwrap(),
            purpose: "Keep the owner informed.".to_owned(),
            kind: AppNotificationKindV1::Briefing,
            severity_ceiling: AppNotificationSeverityV1::Warning,
            max_notifications_per_period: 4,
            period_seconds: 3_600,
            max_pending: 4,
            ttl_seconds: 7_200,
            reviewed_request_digest: AppDigest::blake3(b"notification-review"),
        }
    }

    #[test]
    fn item4_review_is_explicit_deny_and_only_accepts_exact_narrowing() {
        let event = item4_event_grant();
        let notification = item4_notification_grant();
        assert!(
            select_event_behavior_grants(std::slice::from_ref(&event), None)
                .unwrap()
                .is_empty()
        );
        assert!(
            select_notification_grants(std::slice::from_ref(&notification), None)
                .unwrap()
                .is_empty()
        );

        let mut event_resources = event.resources.clone();
        event_resources.max_tokens_per_run = 50;
        event_resources.max_tokens_per_month = 500;
        let selected_event = AppEventBehaviorGrantRequest {
            event_behavior_id: event.event_behavior_id.clone(),
            reviewed_request_digest: event.reviewed_request_digest.clone(),
            min_interval_seconds: 120,
            resources: event_resources.clone(),
        };
        let granted_event = select_event_behavior_grants(
            std::slice::from_ref(&event),
            Some(std::slice::from_ref(&selected_event)),
        )
        .unwrap();
        assert_eq!(granted_event[0].min_interval_seconds, 120);
        assert_eq!(granted_event[0].resources, event_resources);
        assert_eq!(granted_event[0].purpose, event.purpose);
        assert_eq!(granted_event[0].subscription, event.subscription);

        let mut substituted_event = selected_event.clone();
        substituted_event.reviewed_request_digest = AppDigest::blake3(b"substituted-review");
        assert!(select_event_behavior_grants(
            std::slice::from_ref(&event),
            Some(std::slice::from_ref(&substituted_event)),
        )
        .is_err());

        let selected_notification = AppNotificationGrantRequest {
            workflow_id: notification.workflow_id.clone(),
            port_id: notification.port_id.clone(),
            reviewed_request_digest: notification.reviewed_request_digest.clone(),
            severity_ceiling: AppNotificationSeverityV1::Info,
            max_notifications_per_period: 2,
            period_seconds: notification.period_seconds,
            max_pending: 2,
            ttl_seconds: 3_600,
        };
        let granted_notification = select_notification_grants(
            std::slice::from_ref(&notification),
            Some(std::slice::from_ref(&selected_notification)),
        )
        .unwrap();
        assert_eq!(
            granted_notification[0].severity_ceiling,
            AppNotificationSeverityV1::Info
        );
        assert_eq!(granted_notification[0].max_pending, 2);
        assert_eq!(granted_notification[0].ttl_seconds, 3_600);
        assert_eq!(granted_notification[0].purpose, notification.purpose);

        let mut widened_notification = selected_notification;
        widened_notification.ttl_seconds = notification.ttl_seconds + 1;
        assert!(select_notification_grants(
            std::slice::from_ref(&notification),
            Some(std::slice::from_ref(&widened_notification)),
        )
        .is_err());
    }

    fn sdk_archive_bytes(root: &std::path::Path, name: &str) -> Vec<u8> {
        let project = root.join(name);
        run_app_authoring_command(&AppAuthoringCommand::Init(AppInitArgs {
            name: name.to_owned(),
            path: Some(project.clone()),
        }))
        .expect("scaffold succeeds");
        let archive = root.join(format!("{name}.app.zip"));
        run_app_authoring_command(&AppAuthoringCommand::Pack(AppPackArgs {
            path: project,
            publisher: "publisher:owner".to_owned(),
            resolutions: None,
            output: Some(archive.clone()),
            discover: AppCatalogDiscoverArgs::default(),
        }))
        .expect("pack succeeds");
        std::fs::read(archive).expect("archive is readable")
    }

    async fn published_candidate(
        root: &std::path::Path,
        archive: Vec<u8>,
    ) -> (
        AppInstallationReviewService,
        AuthenticatedAppScope,
        AppInstallationId,
    ) {
        let workspace = ArtifactV2Workspace::new(root);
        let candidates = AppCandidatePublicationService::new(workspace.clone());
        let authenticated = authenticated_scope("anonymous", "default");
        let created = candidates
            .publish_archive_candidate(
                &authenticated,
                admit_package_archive(&archive).expect("archive admits"),
                time(1),
            )
            .await
            .expect("candidate publishes");
        assert_eq!(created.state, AppInstallationStatus::ReadyForReview);
        let service = AppInstallationReviewService::from_parts(
            AppRegistryService::new(workspace.clone()),
            AppPackageStager::new(workspace),
        );
        (service, authenticated, created.installation_id)
    }

    /// Build a publishable package archive whose manifest declares the
    /// `custom_surface` permission with two entry points, mirroring the
    /// package-transfer test fixture (portable lock claim + deterministic
    /// archive codec). The suffix keys the package identity so the same
    /// bundle can back two distinct installations (an exact replay of one
    /// identity is an idempotent no-op, not a second installation).
    fn custom_surface_archive_bytes(suffix: &str) -> Vec<u8> {
        let entry_points = "      - route: /canvas\n        document: surfaces/canvas.html\n      \
                            - route: /board\n        document: surfaces/board.html\n";
        let manifest = valid_skill_document()
            .replacen(
                "name: learning-plan\n",
                &format!("name: custom-surface-{suffix}\n"),
                1,
            )
            .replacen(
                "  workflows:\n    build:\n      prompt: workflows/build.md\n      runner: auto\n      uses: [content_search]\n      procedures: [skill:summarize]\n      input:\n        type: object\n        fields:\n          topic: { type: text, required: true }\n      result:\n        kind: entity_projection\n        entities: [plan]\n      may_mutate: [plan]\n      trigger: user\n  actions:\n    create:\n      workflow: build\n      input_from: build.input\n      result_from: build.result\n",
                "  workflows: {}\n  actions: {}\n",
                1,
            )
            .replacen(
                "    procedure_skills:\n      - skill: skill:summarize\n        version_requirement: \"^2\"\n        vendored_path: vendor/skills/summarize/SKILL.md\n    capabilities:\n      - capability: content_search\n        version_requirement: \"^1\"\n",
                "    procedure_skills: []\n    capabilities: []\n",
                1,
            )
            .replacen(
                "    app_sdk_version: \"1\"\n",
                "    app_sdk_version: \"1\"\n    required_features: [custom_surfaces_v1]\n",
                1,
            )
            .replacen(
                "app:\n",
                &format!(
                    "app:\n  permissions: [custom_surface]\n  custom_surface:\n    \
                     entry_points:\n{entry_points}"
                ),
                1,
            );
        let temporary = canonical_tempdir();
        let package_root = temporary.path().join(format!("custom-surface-{suffix}"));
        std::fs::create_dir(&package_root).expect("package root");
        for (relative, bytes) in [
            ("SKILL.md", manifest.into_bytes()),
            ("assets/icon.svg", b"<svg/>".to_vec()),
            (
                "surfaces/canvas.html",
                b"<html><body>canvas</body></html>".to_vec(),
            ),
            ("surfaces/canvas.js", b"console.log('canvas');".to_vec()),
            (
                "surfaces/board.html",
                b"<html><body>board</body></html>".to_vec(),
            ),
            (
                "fixtures/app-fixtures.json",
                br#"{
  "schema_version": 1,
  "records": [
    {
      "name": "new_plan",
      "entity": "plan",
      "value": { "topic": "Rust", "status": "new" }
    }
  ],
  "workflows": [],
  "views": [
    { "name": "plans_with_one_record", "view": "plans", "minimum_records": 1 }
  ]
}
"#
                .to_vec(),
            ),
        ] {
            let path = package_root.join(relative);
            std::fs::create_dir_all(path.parent().expect("member parent"))
                .expect("member parent directory");
            std::fs::write(path, bytes).expect("package member");
        }
        run_app_authoring_command(&AppAuthoringCommand::Check(AppCheckArgs {
            path: package_root.clone(),
            write_generated: true,
        }))
        .expect("generated package artifacts");
        let archive = temporary
            .path()
            .join(format!("custom-surface-{suffix}.app.zip"));
        run_app_authoring_command(&AppAuthoringCommand::Pack(AppPackArgs {
            path: package_root,
            publisher: "publisher:fixture".to_owned(),
            resolutions: None,
            output: Some(archive.clone()),
            discover: AppCatalogDiscoverArgs::default(),
        }))
        .expect("package packs through the authoring pipeline");
        std::fs::read(archive).expect("archive is readable")
    }

    #[test]
    fn granted_subset_must_be_requested() {
        let requested = vec![
            canonicalize_tool_ref(&AppReference::parse("content_read").unwrap()).unwrap(),
            canonicalize_tool_ref(&AppReference::parse("time_math").unwrap()).unwrap(),
        ];
        assert_eq!(requested[0].as_str(), "capability:content_read");
        assert_eq!(
            select_granted_subset("tools", &requested, None, canonicalize_tool_ref).unwrap(),
            requested
        );
        assert_eq!(
            select_granted_subset(
                "tools",
                &requested,
                Some(&[AppReference::parse("content_read").unwrap()]),
                canonicalize_tool_ref,
            )
            .unwrap()
            .len(),
            1
        );
        assert!(matches!(
            select_granted_subset(
                "tools",
                &requested,
                Some(&[AppReference::parse("invented-tool").unwrap()]),
                canonicalize_tool_ref,
            ),
            Err(AppInstallationReviewError::InvalidGrant(_))
        ));
        let agent = AppReference::parse("agent:research-agent").unwrap();
        assert_eq!(
            select_granted_subset(
                "agents",
                &[agent.clone()],
                Some(&[AppReference::parse("research-agent").unwrap()]),
                canonicalize_prefixed_ref("agent"),
            )
            .unwrap(),
            vec![agent]
        );
    }

    #[test]
    fn exact_primitive_dependency_parses_for_authoritative_binding_revalidation() {
        let source = valid_skill_document().replace(
            "    capabilities:\n      - capability: content_search\n        version_requirement: \
             \"^1\"",
            "    tools:\n      - name: content_search\n        primitive_ref: \
             primitive:tool-skill:\
             0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n        \
             version_requirement: \"^1\"",
        );
        let manifest =
            parse_app_manifest_frontmatter(source.as_bytes(), &AppPackageLimits::default())
                .expect("exact-selector manifest");
        let tools = manifest
            .manifest()
            .declared_tools()
            .expect("declared exact tool");
        assert!(tools[0].primitive_ref.is_some());
    }

    #[test]
    fn install_review_admits_only_exact_singleton_sealed_interactive_actions() {
        let browser_source = String::from_utf8(typed_app_tool_document("browser", "1.0.0", ""))
            .unwrap()
            .replace(
                "---\nReturn one ranked next step.",
                "    runtime_catalog:\n      categories: [browser]\n      composition_category: \
                 web_operations\n---\nReturn one ranked next step.",
            );
        let macos_source = String::from_utf8(typed_app_tool_document(
            "macos-ui-automation",
            "1.0.0",
            "",
        ))
        .unwrap()
        .replace(
            "---\nReturn one ranked next step.",
            "    runtime_catalog:\n      categories: [macos_automation, ui_automation]\n      \
             composition_category: macos_operations\n---\nReturn one ranked next step.",
        );
        for (source, selector, dependency) in [
            (browser_source, "browser", "capability:browser"),
            (
                macos_source,
                "macos-ui-automation",
                "capability:macos-ui-automation",
            ),
        ] {
            let mut builder = AppPrimitiveCatalogBuilder::new();
            builder.add_tool_skill(source.as_bytes());
            let snapshot = builder.finish();
            let descriptor = snapshot.resolve(selector).unwrap();
            let binding = AppLockedPrimitiveBinding::from_descriptor_with_action_selectors(
                &descriptor,
                &["snapshot".to_owned()],
            )
            .unwrap();
            let dependency = AppReference::parse(dependency).unwrap();
            assert_eq!(
                sealed_interactive_owner_is_reviewed(&dependency, &descriptor, &binding),
                true,
            );
            if selector == "browser" {
                assert!(!manifest_selects_exact_locked_leaf(
                    &[],
                    &binding,
                    "snapshot",
                ));
                assert!(manifest_selects_exact_locked_leaf(
                    &["snapshot".to_owned()],
                    &binding,
                    "snapshot",
                ));
                assert!(manifest_selects_exact_locked_leaf(
                    &[binding.actions()[0].action_ref().to_string()],
                    &binding,
                    "snapshot",
                ));
                assert!(!manifest_selects_exact_locked_leaf(
                    &["navigate".to_owned()],
                    &binding,
                    "snapshot",
                ));
            }

            let mut raw_action = serde_json::to_value(&binding).unwrap();
            raw_action["actions"][0]["name"] = serde_json::json!("raw_call");
            let raw_action: AppLockedPrimitiveBinding = serde_json::from_value(raw_action).unwrap();
            assert!(!sealed_interactive_owner_is_reviewed(
                &dependency,
                &descriptor,
                &raw_action,
            ));
            assert!(!sealed_interactive_owner_is_reviewed(
                &AppReference::parse("capability:unrelated-owner").unwrap(),
                &descriptor,
                &binding,
            ));
        }

        let android_pack = embedded_compiled_pack_defs_ref()
            .iter()
            .find(|pack| pack.name == "android_snapshot")
            .expect("embedded Android snapshot pack");
        let android_source = embedded_compiled_pack_yaml("android_snapshot")
            .expect("embedded Android snapshot source");
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_compiled_pack(android_pack, android_source.as_bytes());
        let snapshot = builder.finish();
        let descriptor = snapshot
            .resolve("android_snapshot")
            .expect("sealed Android snapshot descriptor");
        let binding = AppLockedPrimitiveBinding::from_descriptor_with_action_selectors(
            &descriptor,
            &["snapshot".to_owned()],
        )
        .unwrap();
        let dependency = AppReference::parse("capability:android_snapshot").unwrap();
        assert!(sealed_interactive_owner_is_reviewed(
            &dependency,
            &descriptor,
            &binding,
        ));

        let mut substituted = serde_json::to_value(&binding).unwrap();
        substituted["actions"][0]["name"] = serde_json::json!("android_act");
        let substituted: AppLockedPrimitiveBinding = serde_json::from_value(substituted).unwrap();
        assert!(!sealed_interactive_owner_is_reviewed(
            &dependency,
            &descriptor,
            &substituted,
        ));

        let mut substituted_implementation = serde_json::to_value(&binding).unwrap();
        substituted_implementation["actions"][0]["implementation_plan_digest"] =
            serde_json::Value::Null;
        let substituted_implementation: AppLockedPrimitiveBinding =
            serde_json::from_value(substituted_implementation).unwrap();
        assert!(!sealed_interactive_owner_is_reviewed(
            &dependency,
            &descriptor,
            &substituted_implementation,
        ));

        let action_pack = embedded_compiled_pack_defs_ref()
            .iter()
            .find(|pack| pack.name == "android_act")
            .expect("embedded Android action pack");
        let action_source =
            embedded_compiled_pack_yaml("android_act").expect("embedded Android mutation source");
        let mut action_builder = AppPrimitiveCatalogBuilder::new();
        action_builder.add_compiled_pack(action_pack, action_source.as_bytes());
        let action_snapshot = action_builder.finish();
        let action_descriptor = action_snapshot.resolve("android_act").unwrap();
        let action_binding = AppLockedPrimitiveBinding::from_descriptor_with_action_selectors(
            &action_descriptor,
            &["tap".to_owned()],
        )
        .unwrap();
        assert!(sealed_interactive_owner_is_reviewed(
            &AppReference::parse("capability:android_act").unwrap(),
            &action_descriptor,
            &action_binding,
        ));
        assert!(!sealed_interactive_owner_is_reviewed(
            &AppReference::parse("capability:android_act__scroll").unwrap(),
            &action_descriptor,
            &action_binding,
        ));
    }

    #[test]
    fn experience_primitives_fail_closed_at_interactive_review_today() {
        // Plan 1.3 admits the overlay-draw/narration vocabulary only. The
        // revalidation arms above exist so a future reviewed consumer is
        // identity-checked exactly like the browser owner, but the shared
        // dispatch-Ready gate keeps both classes unapprovable today: the
        // admitted descriptors are deliberately Conditional and their locked
        // actions are non-dispatchable.
        let mut builder = AppPrimitiveCatalogBuilder::new();
        builder.add_experience_primitives();
        let snapshot = builder.finish();
        for (name, leaf) in [("overlay-draw", "draw"), ("narration", "speak")] {
            let descriptor = snapshot.resolve(name).expect("experience descriptor");
            assert_eq!(
                descriptor.dispatch().status(),
                AppPrimitiveDispatchStatus::Conditional,
                "{name} must not be dispatch-Ready"
            );
            let binding = AppLockedPrimitiveBinding::from_descriptor_with_action_selectors(
                &descriptor,
                &[leaf.to_owned()],
            )
            .expect("single admitted leaf");
            assert!(binding
                .actions()
                .iter()
                .all(|action| !action.dispatchable()));
            for dependency in [
                AppReference::parse(format!("capability:{name}")).unwrap(),
                AppReference::parse(format!("capability:{name}__{leaf}")).unwrap(),
            ] {
                assert!(
                    !sealed_interactive_owner_is_reviewed(&dependency, &descriptor, &binding),
                    "{name} must fail closed at review until the reviewed consumer lands"
                );
            }
        }
    }

    #[test]
    fn install_review_admits_only_exact_sealed_agent_tool_leaf() {
        let source = br#"agent_id: reviewed-child
version: 1
name: Reviewed child
description: One exact callable child.
persona: Return only the reviewed typed result.
tools: [content_read]
app_tool:
  input:
    type: object
    fields:
      request:
        type: text
        required: true
  result:
    type: object
    fields:
      answer:
        type: markdown
        required: true
  max_input_bytes: 16384
  max_result_bytes: 32768
"#;
        let mut builder = AppPrimitiveCatalogBuilder::new();
        assert!(builder.add_agent_definition(source, false));
        let snapshot = builder.finish();
        let descriptor = snapshot.resolve("reviewed-child").unwrap();
        let binding = AppLockedPrimitiveBinding::from_descriptor(&descriptor).unwrap();
        let dependency = AppReference::parse("capability:reviewed-child").unwrap();
        assert!(sealed_agent_tool_owner_is_reviewed(
            &dependency,
            &descriptor,
            &binding,
            source,
        ));
        assert!(!manifest_selects_exact_locked_leaf(
            &[],
            &binding,
            "agent_as_tool",
        ));
        assert!(manifest_selects_exact_locked_leaf(
            &["agent_as_tool".to_owned()],
            &binding,
            "agent_as_tool",
        ));
        assert!(!manifest_selects_exact_locked_leaf(
            &["delegate_to_agent".to_owned()],
            &binding,
            "agent_as_tool",
        ));

        let mut substituted_action = serde_json::to_value(&binding).unwrap();
        substituted_action["actions"][0]["name"] = serde_json::json!("delegate_to_agent");
        let substituted_action: AppLockedPrimitiveBinding =
            serde_json::from_value(substituted_action).unwrap();
        assert!(!sealed_agent_tool_owner_is_reviewed(
            &dependency,
            &descriptor,
            &substituted_action,
            source,
        ));

        let mut missing_implementation = serde_json::to_value(&binding).unwrap();
        missing_implementation["actions"][0]["implementation_plan_digest"] =
            serde_json::Value::Null;
        let missing_implementation: AppLockedPrimitiveBinding =
            serde_json::from_value(missing_implementation).unwrap();
        assert!(!sealed_agent_tool_owner_is_reviewed(
            &dependency,
            &descriptor,
            &missing_implementation,
            source,
        ));
        let mut missing_result_schema = serde_json::to_value(&binding).unwrap();
        missing_result_schema["actions"][0]["result_schema_digest"] = serde_json::Value::Null;
        let missing_result_schema: AppLockedPrimitiveBinding =
            serde_json::from_value(missing_result_schema).unwrap();
        assert!(!sealed_agent_tool_owner_is_reviewed(
            &dependency,
            &descriptor,
            &missing_result_schema,
            source,
        ));
        let mut widened_result = serde_json::to_value(&binding).unwrap();
        widened_result["actions"][0]["transport_result_byte_ceiling"] = serde_json::json!(32_769);
        let widened_result: AppLockedPrimitiveBinding =
            serde_json::from_value(widened_result).unwrap();
        assert!(!sealed_agent_tool_owner_is_reviewed(
            &dependency,
            &descriptor,
            &widened_result,
            source,
        ));
        assert!(!sealed_agent_tool_owner_is_reviewed(
            &AppReference::parse("capability:another-agent").unwrap(),
            &descriptor,
            &binding,
            source,
        ));
        let changed_source = std::str::from_utf8(source).unwrap().replace(
            "Return only the reviewed typed result.",
            "A mutable replacement.",
        );
        assert!(!sealed_agent_tool_owner_is_reviewed(
            &dependency,
            &descriptor,
            &binding,
            changed_source.as_bytes(),
        ));

        let mut default_builder = AppPrimitiveCatalogBuilder::new();
        assert!(default_builder.add_agent_definition(source, true));
        let default_snapshot = default_builder.finish();
        let default_descriptor = default_snapshot.resolve("reviewed-child").unwrap();
        let default_binding =
            AppLockedPrimitiveBinding::from_descriptor(&default_descriptor).unwrap();
        assert!(!sealed_agent_tool_owner_is_reviewed(
            &dependency,
            &default_descriptor,
            &default_binding,
            source,
        ));
    }

    #[test]
    fn inert_workflows_name_missing_grants_without_blocking_enablement() {
        let workflow = AppReviewedWorkflowGrant {
            workflow_id: AppName::parse("build").unwrap(),
            uses: vec![
                canonicalize_tool_ref(&AppReference::parse("content_read").unwrap()).unwrap(),
            ],
            agent: AppReference::parse("agent:personal-assistant").unwrap(),
            personality: None,
        };
        let inert = inert_workflows_for_grant(
            &[workflow.clone()],
            &[],
            &[AppReference::parse("agent:personal-assistant").unwrap()],
            &[],
            &[],
        );
        assert_eq!(inert.len(), 1);
        assert!(inert[0]
            .reasons
            .iter()
            .any(|reason| reason.contains("content_read")));
        assert!(inert_workflows_for_grant(
            &[workflow],
            &[AppReference::parse("capability:content_read").unwrap()],
            &[AppReference::parse("agent:personal-assistant").unwrap()],
            &[],
            &[],
        )
        .is_empty());
    }

    #[test]
    fn granted_tool_without_physical_dispatch_keeps_workflow_inert() {
        let tool = AppReference::parse("capability:content_read").unwrap();
        let workflow = AppReviewedWorkflowGrant {
            workflow_id: AppName::parse("build").unwrap(),
            uses: vec![tool.clone()],
            agent: AppReference::parse("agent:personal-assistant").unwrap(),
            personality: None,
        };
        let inert = inert_workflows_for_grant(
            &[workflow],
            &[tool.clone()],
            &[AppReference::parse("agent:personal-assistant").unwrap()],
            &[],
            &[AppReviewedToolDispatch {
                tool,
                dispatchable: false,
                attested_operations: Vec::new(),
                reason: "OS-jail owner unavailable".to_owned(),
            }],
        );
        assert_eq!(inert.len(), 1);
        assert!(inert[0]
            .reasons
            .iter()
            .any(|reason| reason.contains("no admitted physical dispatch")));
    }

    #[test]
    fn resource_ceiling_prefers_output_and_follows_network_policy() {
        let resources: AppManifestResources = serde_yaml::from_str(
            "per_run:\n  max_tokens: 1\n  max_cost_usd: 0.01\n  max_active_seconds: \
             30\nmonthly:\n  max_tokens: 10\n  max_cost_usd: 1\nstorage:\n  max_records: 10\n  \
             max_bytes: 1024\n",
        )
        .expect("fixture resources");
        let denied = requested_resource_ceiling(
            &resources,
            AppBackgroundExecution::Denied,
            &AppNetworkPolicy::Denied,
        );
        assert_eq!(denied.max_output_tokens, 1);
        assert_eq!(denied.max_input_tokens, 0);
        assert_eq!(denied.max_browser_network_actions, 0);
        let allowed = requested_resource_ceiling(
            &resources,
            AppBackgroundExecution::Denied,
            &AppNetworkPolicy::ApprovedDestinations {
                destinations: vec![AppReference::parse("destination:example.com").unwrap()],
            },
        );
        assert_eq!(allowed.max_browser_network_actions, 4);
    }

    #[tokio::test]
    async fn owner_review_lists_requested_authority_and_approve_enables() {
        let temporary = canonical_tempdir();
        let archive = sdk_archive_bytes(temporary.path(), "review-notes");
        let (service, authenticated, installation_id) =
            published_candidate(temporary.path(), archive).await;

        let review = service
            .review(&authenticated, &installation_id, time(2))
            .await
            .expect("review loads");
        assert_eq!(review.name, "review-notes");
        assert!(review.requested_tools.is_empty());
        assert!(review.requested_agents.is_empty());
        assert!(review.workflows.is_empty());
        assert!(review.inert_workflows.is_empty());
        assert_eq!(review.attempt_kind, AppLifecycleAttemptKind::InitialInstall);

        let enabled = service
            .approve(
                &authenticated,
                &installation_id,
                AppInstallationApproveRequest {
                    review_material_digest: Some(review.workflow_material_digest.clone()),
                    ..AppInstallationApproveRequest::default()
                },
                time(3),
            )
            .await
            .expect("approve enables");
        assert_eq!(enabled.status, AppInstallationStatus::Enabled);
        assert_eq!(enabled.outcome, AppInstallationApproveOutcome::Enabled);
        assert_eq!(enabled.generation, 2);
        assert_eq!(enabled.grant_revision.get(), 1);

        let replay = service
            .approve(
                &authenticated,
                &installation_id,
                AppInstallationApproveRequest::default(),
                time(4),
            )
            .await
            .expect("enabled retry is idempotent");
        assert_eq!(replay.status, AppInstallationStatus::Enabled);
        assert_eq!(
            replay.outcome,
            AppInstallationApproveOutcome::AlreadyEnabled
        );
        assert_eq!(replay.generation, enabled.generation);
        assert_eq!(replay.grant_revision, enabled.grant_revision);
        assert_eq!(replay.approval_id, enabled.approval_id);
    }

    /// The 1.6 completion: approve PERSISTS the narrowed custom-surface
    /// grant on the durable grant revision — not only the receipt — and an
    /// omitted grant persists an empty set. The live grant is what the
    /// scripted host enforces at request time.
    #[tokio::test]
    async fn approve_persists_the_granted_custom_surface_entry_points_on_the_grant_revision() {
        let temporary = canonical_tempdir();
        let archive = custom_surface_archive_bytes("subset");
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let candidates = AppCandidatePublicationService::new(workspace.clone());
        let authenticated = authenticated_scope("anonymous", "default");
        let created = candidates
            .publish_archive_candidate(
                &authenticated,
                admit_package_archive(&archive).expect("archive admits"),
                time(1),
            )
            .await
            .expect("candidate publishes");
        let installation_id = created.installation_id;
        let registry = AppRegistryService::new(workspace.clone());
        let service = AppInstallationReviewService::from_parts(
            registry.clone(),
            AppPackageStager::new(workspace),
        );

        let review = service
            .review(&authenticated, &installation_id, time(2))
            .await
            .expect("review loads");
        let requested = review.requested_custom_surface.as_ref().expect("reviewed");
        assert_eq!(requested.entry_points.len(), 2);
        let canvas = requested
            .entry_points
            .iter()
            .find(|entry| entry.route == "/canvas")
            .expect("canvas reviewed");

        // Grant ONLY /canvas out of the two reviewed entry points.
        let approved = service
            .approve(
                &authenticated,
                &installation_id,
                AppInstallationApproveRequest {
                    review_material_digest: Some(review.workflow_material_digest.clone()),
                    granted_custom_surface_entry_points: Some(vec![
                        AppCustomSurfaceEntryGrantRequest {
                            route: canvas.route.clone(),
                            document: canvas.document.clone(),
                            reviewed_request_digest: requested.request_digest.clone(),
                        },
                    ]),
                    ..AppInstallationApproveRequest::default()
                },
                time(3),
            )
            .await
            .expect("approve enables");
        assert_eq!(
            approved.granted_custom_surface_entry_points,
            vec!["/canvas".to_owned()]
        );

        // The LIVE grant revision — the record the runtime host resolves —
        // carries the exact narrowed (route, document, digest) set.
        let active = AppEntityStoreService::new(registry)
            .active_schema(&authenticated, &installation_id, time(4))
            .await
            .expect("live grant resolves")
            .expect("installation is enabled");
        let granted = &active.grant().granted_custom_surface_entry_points;
        assert_eq!(granted.len(), 1);
        assert_eq!(granted[0].route, "/canvas");
        assert_eq!(granted[0].document, "surfaces/canvas.html");
        assert_eq!(granted[0].document_digest, canvas.document_digest);

        // Omitted grant = empty set on the receipt AND on the persisted
        // grant: zero surfaces, never implicit-all. A second, distinct
        // installation of the same bundle shape proves it without touching
        // the first.
        let second = candidates
            .publish_archive_candidate(
                &authenticated,
                admit_package_archive(&custom_surface_archive_bytes("zero"))
                    .expect("archive admits"),
                time(5),
            )
            .await
            .expect("second candidate publishes");
        let review = service
            .review(&authenticated, &second.installation_id, time(6))
            .await
            .expect("second review loads");
        let zero = service
            .approve(
                &authenticated,
                &second.installation_id,
                AppInstallationApproveRequest {
                    review_material_digest: Some(review.workflow_material_digest.clone()),
                    ..AppInstallationApproveRequest::default()
                },
                time(7),
            )
            .await
            .expect("zero-surface approve enables");
        assert!(zero.granted_custom_surface_entry_points.is_empty());
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let active = AppEntityStoreService::new(registry)
            .active_schema(&authenticated, &second.installation_id, time(8))
            .await
            .expect("live grant resolves")
            .expect("second installation is enabled");
        assert!(active
            .grant()
            .granted_custom_surface_entry_points
            .is_empty());
    }

    /// The Apps directory advertises manifest-declared custom-surface
    /// hostability (`custom_surface_entry_count`) so native clients can
    /// gate affordances like the iOS viewless "Open app" button on
    /// declared entry points instead of dead-ending in the WebView. The
    /// count is captured from the admitted manifest at publication time;
    /// a package without the declaration reports 0 on an otherwise
    /// unchanged wire shape.
    #[tokio::test]
    async fn directory_reports_the_manifest_custom_surface_entry_count() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let candidates = AppCandidatePublicationService::new(workspace.clone());
        let authenticated = authenticated_scope("anonymous", "default");
        for archive in [
            sdk_archive_bytes(temporary.path(), "directory-plain"),
            custom_surface_archive_bytes("directory"),
        ] {
            candidates
                .publish_archive_candidate(
                    &authenticated,
                    admit_package_archive(&archive).expect("archive admits"),
                    time(1),
                )
                .await
                .expect("candidate publishes");
        }
        let page = crate::apps::app_directory::AppDirectoryService::new(AppRegistryService::new(
            workspace,
        ))
        .list(
            &authenticated,
            crate::apps::app_directory::AppDirectoryQuery {
                section: crate::apps::app_directory::AppDirectorySection::NeedsAttention,
                pinned_target_kind: None,
                search: None,
                limit: crate::apps::app_directory::DEFAULT_APP_DIRECTORY_LIMIT,
                cursor: None,
            },
            time(2),
        )
        .await
        .expect("directory lists the fresh candidates");
        let wire = serde_json::to_value(&page).expect("page serializes");
        let counts: std::collections::BTreeMap<String, u64> = wire["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .map(|entry| {
                (
                    entry["name"].as_str().expect("entry name").to_owned(),
                    entry["custom_surface_entry_count"]
                        .as_u64()
                        .expect("wire carries the hostability signal"),
                )
            })
            .collect();
        assert_eq!(counts.len(), 2);
        assert_eq!(counts.get("custom-surface-directory"), Some(&2));
        assert_eq!(counts.get("directory-plain"), Some(&0));
    }

    #[tokio::test]
    async fn owner_cannot_invent_a_tool_that_the_package_did_not_request() {
        let temporary = canonical_tempdir();
        let archive = sdk_archive_bytes(temporary.path(), "review-strict");
        let (service, authenticated, installation_id) =
            published_candidate(temporary.path(), archive).await;
        let review = service
            .review(&authenticated, &installation_id, time(2))
            .await
            .expect("review loads");
        let invented = service
            .approve(
                &authenticated,
                &installation_id,
                AppInstallationApproveRequest {
                    review_material_digest: Some(review.workflow_material_digest),
                    granted_tools: Some(vec![AppReference::parse("invented-tool").unwrap()]),
                    ..AppInstallationApproveRequest::default()
                },
                time(3),
            )
            .await;
        assert!(matches!(
            invented,
            Err(AppInstallationReviewError::InvalidGrant(message))
                if message.contains("invented-tool")
        ));
    }

    #[tokio::test]
    async fn approval_requires_the_exact_reviewed_workflow_material_digest() {
        let temporary = canonical_tempdir();
        let archive = sdk_archive_bytes(temporary.path(), "review-material-fence");
        let (service, authenticated, installation_id) =
            published_candidate(temporary.path(), archive).await;

        let missing = service
            .approve(
                &authenticated,
                &installation_id,
                AppInstallationApproveRequest::default(),
                time(3),
            )
            .await;
        assert!(matches!(
            missing,
            Err(AppInstallationReviewError::InvalidGrant(message))
                if message.contains("refresh installation review")
        ));

        let stale = service
            .approve(
                &authenticated,
                &installation_id,
                AppInstallationApproveRequest {
                    review_material_digest: Some(AppDigest::blake3(b"stale-review")),
                    ..AppInstallationApproveRequest::default()
                },
                time(3),
            )
            .await;
        assert!(matches!(
            stale,
            Err(AppInstallationReviewError::InvalidGrant(message))
                if message.contains("refresh installation review")
        ));
    }

    #[test]
    fn contribution_grants_narrow_every_dimension_and_refuse_substitution() {
        let requested = AppReviewedContributionPortGrant {
            schema: "magician.app-reviewed-contribution-port-grant.v1".to_owned(),
            workflow_id: AppName::parse("build").unwrap(),
            port_id: AppName::parse("summary_memory").unwrap(),
            locked_port_digest: AppDigest::blake3(b"locked-port"),
            source: AppContributionSource::MutationBackedEntityProjection {
                entity: AppName::parse("plan").unwrap(),
                selected_fields: vec![
                    super::super::models::AppFieldPath::parse("status").unwrap(),
                    super::super::models::AppFieldPath::parse("topic").unwrap(),
                ],
            },
            destination: super::super::records::AppContributionDestination::Memory,
            destination_binding: AppContributionDestinationBinding::MemoryUserKnowledge,
            purposes: vec![AppName::parse("learning_continuity").unwrap()],
            audiences: vec![AppReference::parse("user:owner").unwrap()],
            evidence_classes: vec![
                magician_app_contract::contribution::AppContributionEvidenceClass::Hypothesis,
            ],
            frequency: AppContributionFrequency {
                max_proposals: 4,
                window_seconds: 3_600,
            },
            maximum_retention_seconds: 604_800,
            grant_digest: AppDigest::blake3(b"pending"),
        }
        .seal()
        .unwrap();
        let selected = AppContributionPortGrantRequest {
            workflow_id: requested.workflow_id.clone(),
            port_id: requested.port_id.clone(),
            reviewed_grant_digest: requested.grant_digest.clone(),
            selected_fields: vec![super::super::models::AppFieldPath::parse("topic").unwrap()],
            purposes: vec![AppName::parse("learning_continuity").unwrap()],
            audiences: vec![AppReference::parse("user:owner").unwrap()],
            evidence_classes: vec![
                magician_app_contract::contribution::AppContributionEvidenceClass::Hypothesis,
            ],
            frequency: AppContributionFrequency {
                max_proposals: 1,
                window_seconds: 7_200,
            },
            maximum_retention_seconds: 86_400,
        };
        let narrowed = select_contribution_port_grants(&[requested.clone()], Some(&[selected]))
            .expect("every selected contribution axis narrows");
        assert_eq!(narrowed.len(), 1);
        assert_eq!(narrowed[0].maximum_retention_seconds, 86_400);

        let substituted = AppContributionPortGrantRequest {
            workflow_id: requested.workflow_id.clone(),
            port_id: requested.port_id.clone(),
            reviewed_grant_digest: AppDigest::blake3(b"substituted-review"),
            selected_fields: vec![super::super::models::AppFieldPath::parse("topic").unwrap()],
            purposes: vec![AppName::parse("learning_continuity").unwrap()],
            audiences: vec![AppReference::parse("user:owner").unwrap()],
            evidence_classes: vec![
                magician_app_contract::contribution::AppContributionEvidenceClass::Hypothesis,
            ],
            frequency: requested.frequency,
            maximum_retention_seconds: requested.maximum_retention_seconds,
        };
        assert!(matches!(
            select_contribution_port_grants(&[requested], Some(&[substituted])),
            Err(AppInstallationReviewError::InvalidGrant(message))
                if message.contains("widens or substitutes")
        ));
    }

    /// Every serialized field of `AppInstallationReview` must be accepted by the
    /// owner UI's review parser.
    ///
    /// `parseReview` validates with an EXACT key set and returns `null` on any
    /// key it does not know, which the page reports only as "The app review
    /// payload is invalid." The optional fields here are `skip_serializing_if`,
    /// so a new one stays invisible until some package happens to use the
    /// feature — and then that package becomes unreviewable with no indication
    /// of which field did it.
    ///
    /// That happened twice on 2026-09-07: first the whole behaviour/custom
    /// surface family (every shipped system package declares behaviours, so all
    /// of them were unreviewable), then `requested_notifications` and
    /// `requested_event_output_schemas` on the repair pass, because the struct
    /// was read from a truncated view. Adding a field here without adding it
    /// there now fails the suite instead of a user's review click.
    #[test]
    fn every_review_field_is_accepted_by_the_owner_ui_parser() {
        let source = include_str!("installation_review.rs");
        let struct_body = source
            .split("pub struct AppInstallationReview {")
            .nth(1)
            .and_then(|tail| tail.split("\n}").next())
            .expect("review struct must remain present");
        let fields = struct_body
            .lines()
            .filter_map(|line| line.trim().strip_prefix("pub "))
            .filter_map(|line| line.split(':').next())
            .filter(|name| {
                !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
            })
            .collect::<Vec<_>>();
        assert!(
            fields.len() > 20,
            "field extraction produced only {} names; the parser below would pass vacuously",
            fields.len()
        );
        let parser = include_str!("../../../ui/unified-ui/src/lib/apps/installationReview.ts");
        let allow_list = parser
            .split("function parseReview(")
            .nth(1)
            .and_then(|tail| tail.split("])) return").next())
            .expect("parseReview key allow-list must remain present");
        let missing = fields
            .iter()
            .filter(|field| !allow_list.contains(&format!("'{field}'")))
            .collect::<Vec<_>>();
        assert!(
            missing.is_empty(),
            "these review fields are not accepted by parseReview in \
             ui/unified-ui/src/lib/apps/installationReview.ts, so any package using \
             them cannot be reviewed in the UI: {missing:?}"
        );
    }
}

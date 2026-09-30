//! Authenticated app-package control plane and Phase-2E owner data plane.
//!
//! These handlers consume [`VerifiedRequestIdentity`] from the outer
//! credential middleware. The middleware engraves compatibility headers from
//! that verified identity, so caller-supplied scope values cannot select app
//! authority. Package import stages immutable bytes but deliberately creates no
//! installation, grant, approval, schedule, memory or executable route.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock as StdRwLock};
use std::time::Duration as StdDuration;

use crate::device_pairing_api::MobileEnrollmentConfig;
use actix_web::{body::to_bytes, http::StatusCode, web, HttpMessage, HttpRequest, HttpResponse};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures_util::{FutureExt, StreamExt};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Semaphore;

use magician_app_contract::contribution::{
    app_claims_decision_destination_schema_digest, app_meeting_control_destination_schema_digest,
    AppClaimsDecisionOwnerDecisionEnvelopeV1, AppMeetingControlOwnerDecisionEnvelopeV1,
    AppMemoryOwnerDecisionEnvelopeV1, AppMemoryOwnerReviewV1, APP_MEMORY_OWNER_REVIEW_MAX_ITEMS,
};
use magician_app_contract::{
    has_canonical_app_action_run_namespace, supported_public_operation, AppContractCapabilities,
    AppContractCapabilityLimits, AppHttpMethod, AppPublicOperationId,
};

use magician::config::AppBackgroundBehaviorSettings;
use magician::config::AppSystemPackageSettings;
use magician::magician_v2::agents::{
    AppMemoryContributionStateSnapshotV1, ConsequenceClass,
    PersonalAgentRetrievalInspectionSnapshotV1, APP_MEMORY_CONTRIBUTION_STATE_MAX_ITEMS,
};
use magician::magician_v2::apps::authoring_catalog::{
    list_authoring_agents, list_authoring_personalities, list_authoring_procedures,
    list_authoring_tools, show_authoring_tool, AuthoringCatalogError, AuthoringDiscoveryRoots,
    AuthoringToolKind, AuthoringToolListFilter,
};
use magician::magician_v2::apps::authority::{AppScopeAuthentication, AuthenticatedAppScope};
use magician::magician_v2::apps::background_behaviors::{
    AppArtifactTaskAcceptanceProbe, AppBehaviorDispatch, AppBehaviorHealthCursor,
    AppBehaviorRuntimeLimits, AppBehaviorScheduler, AppBehaviorSchedulerError,
    AppBehaviorSettlement, APP_BEHAVIOR_MAX_HEALTH_ITEMS,
};
use magician::magician_v2::apps::boundary::{AppBoundaryError, VerifiedAppTransportSession};
use magician::magician_v2::apps::candidate_publication::{
    AppCandidatePublicationError, AppCandidatePublicationOutcome, AppCandidatePublicationReceipt,
    AppCandidatePublicationService,
};
use magician::magician_v2::apps::capability_catalog::AppComputedCapabilityOverlayCache;
use magician::magician_v2::apps::capability_publication::{
    AppCapabilityPublicationError, AppCapabilityPublicationOutcome,
    AppCapabilityPublicationService, APP_CAPABILITY_SKILL_MAX_BYTES,
    APP_CAPABILITY_SKILL_MEDIA_TYPE,
};
use magician::magician_v2::apps::composition_service::{
    AppActionCompositionRequest, AppActionResultComposition, AppComposeInvokeError,
    AppCompositionService,
};
use magician::magician_v2::apps::entity_adapter::{AppEntityAdapterError, AppEntityAdapterService};
use magician::magician_v2::apps::entity_mutation::AppEntityMutationError;
use magician::magician_v2::apps::entity_store::{AppEntityStoreError, AppEntityStoreService};
use magician::magician_v2::apps::event_behaviors::{
    AppCanonicalExecutionTerminalV1, AppEventBehaviorDispatch, AppEventBehaviorError,
    AppEventBehaviorRuntimeLimits, AppEventBehaviorService, AppEventBehaviorSettlement,
    AppEventIngressAdmission,
};
use magician::magician_v2::apps::lifecycle::{AppInstallationCommand, AppInstallationStatus};
use magician::magician_v2::apps::macos_pairing_service::{
    AppMacosHostPairingResetAck, AppMacosPairingControlError, AppMacosPairingOwnerService,
    AppMacosPairingRevokeRequest, AppMacosPairingSetupRequest,
};
use magician::magician_v2::apps::manifest::AppManifestInputSchema;
use magician::magician_v2::apps::memory::{
    AppMemoryCandidate, AppMemoryCandidateCommand, AppMemoryCandidateStatus,
};
use magician::magician_v2::apps::memory_contribution_projection::{
    AppMemoryContributionProjectionError, AppMemoryContributionProjectionService,
};
use magician::magician_v2::apps::memory_store::AppMemoryStoreError;
use magician::magician_v2::apps::models::{
    app_run_status_from_task, decode_app_contract, AppActionInvocation, AppActionLaunchResponse,
    AppActionResult, AppContractError, AppContractLimits, AppDigest, AppDirectActionRequest,
    AppErrorCode, AppErrorDisposition, AppErrorEnvelope, AppExpectedRecordRevision,
    AppInstallationId, AppMutationAtomicity, AppMutationCommand, AppMutationOperation, AppName,
    AppOrderDirection, AppProtocolVersion, AppQueryRequest, AppRecordId, AppReference, AppRevision,
    AppRunHandle, AppRunSnapshot, AppRunStatus, AppScopeBindingRef, ValidateAppContract,
};
use magician::magician_v2::apps::observability::{
    AppTraceEvent, AppTraceOperation, AppTraceOutcome, AppTraceRetryClass, AppTraceStage,
};
use magician::magician_v2::apps::owner_notifications::AppOwnerNotificationService;
use magician::magician_v2::apps::package_staging::{
    AppPackageStageOutcome, AppPackageStager, AppPackageStagingError,
};
use magician::magician_v2::apps::package_transfer::{
    admit_package_archive, encode_package_archive, AdmittedAppPackageArchive,
    AppPackageTransferError, APP_PACKAGE_ARCHIVE_MAX_BYTES, APP_PACKAGE_ARCHIVE_MEDIA_TYPE,
};
use magician::magician_v2::apps::personal_agent_retrieval_projection::AppPersonalAgentRetrievalProjectionService;
use magician::magician_v2::apps::portability::{
    authorize_archive_write, build_package_archive_manifest, AppApprovedDataImport,
    AppArchiveProtectionRequest, AppDataArchiveManifest, AppDataImportPreview,
    AppExportSourceState, AppLogicalArchive, AppPackageImportRequirements,
    AppPlaintextExportApproval,
};
use magician::magician_v2::apps::portable_archive_transfer::{
    decode_app_portable_archive, encode_app_portable_archive, AppArchivePassphrase,
    AppArchiveWriteReceipt, AppPortableArchiveTransferError,
    APP_PORTABLE_ARCHIVE_ENCRYPTED_MEDIA_TYPE, APP_PORTABLE_ARCHIVE_MAX_BYTES,
    APP_PORTABLE_ARCHIVE_PLAINTEXT_MEDIA_TYPE,
};
use magician::magician_v2::apps::procedure_publication::{
    AppProcedurePublicationError, AppProcedurePublicationOutcome, AppProcedurePublicationService,
    APP_PROCEDURE_SKILL_MAX_BYTES, APP_PROCEDURE_SKILL_MEDIA_TYPE,
};
use magician::magician_v2::apps::query_semantics::AppQuerySemanticsError;
use magician::magician_v2::apps::records::AppEventTerminalOutcomeV1;
use magician::magician_v2::apps::records::{AppGrantRevision, AppLifecycleAttemptKind, AppScope};
use magician::magician_v2::apps::registry::{AppRegistryError, AppRegistryService};
use magician::magician_v2::apps::slot_assignments::{
    AppSlotAssignmentError, AppSlotAssignmentStore, AppSlotAssignmentWriteRequest, AppSlotId,
    AppSlotInventoryError, AppSlotInventoryResolver, AppSlotInventorySnapshot,
    AppSlotResolutionBatchRequest, AppSlotSettingsQuery, AppSlotSystemDefaultsMaintainer,
    AppSlotSystemDefaultsMaintenance, AppSlotSystemDefaultsOutcome, BootAdmittedSlotDefaults,
    TrustedSystemSlotDefaultSet, APP_SLOT_RESOLUTION_BATCH_MAX_REQUEST_BYTES,
    APP_SLOT_RESOLUTION_BATCH_MAX_RESPONSE_BYTES, DEFAULT_APP_SLOT_ASSIGNMENT_LIMIT,
    DEFAULT_APP_SLOT_PICKER_LIMIT,
};
use magician::magician_v2::apps::system_boot_admission::{
    admit_system_packages, SystemBootAdmissionReport,
};
use magician::magician_v2::apps::tool_catalog::canonical_app_tool_ref;
use magician::magician_v2::apps::widget_runtime::{
    AppIndicatorListOutcome, AppWidgetRenderBatchOutcome, AppWidgetRenderBatchRequest,
    AppWidgetRuntime, AppWidgetRuntimeError, APP_INDICATOR_MAX_PAGE_ITEMS,
    APP_WIDGET_RENDER_MAX_REQUEST_BYTES,
};
use magician::magician_v2::apps::workflows::{
    AppActionCancellationRequest, AppBackgroundLaunchAuthority, AppInteractiveStopRequest,
    AppWorkflowError, AppWorkflowLaunch, AppWorkflowService,
};
use magician::magician_v2::artifact_v2::models::CanonicalEvent;
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::artifact_v2::workspace::{
    scope_hosts_user_subsystems, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};
use magician::magician_v2::artifact_v2::{
    ArtifactV2EventType, RuntimeCanonicalEventObserver, ScopeRef, V3ReadApi,
    CANONICAL_UI_THREAD_ID_FIELD, SOURCE_EVENT_REF_FIELD,
};
use magician::magician_v2::claims_decision_contribution::{
    apply_signed_app_claims_decision, apply_staged_app_ingest, validate_app_claims_decision,
    AppClaimsDecisionCurrentAuthority, AppStagedIngestHostResolution,
};
use magician::magician_v2::cloudflare_access::{
    VerifiedRequestAuthentication, VerifiedRequestIdentity,
};
use magician::magician_v2::commitments::Commitments;
use magician::magician_v2::evidence::transcript_ingestion::{
    StagedIngestApplication, StagedIngestRoster, MAX_STAGED_INGEST_SPEAKERS,
    STAGED_INGEST_REQUEST_ENTITY,
};
use magician::magician_v2::evidence::{
    EvidenceDecisionScope, ProjectedReviewReceipt, ReviewReceiptProjector, ReviewReceiptPublisher,
    TranscriptIngestion, REVIEW_RECEIPT_ENTITY,
};
use magician::magician_v2::execution::agent_resources::AgentResources;
use magician::magician_v2::execution::scoped_capability_resolver::ScopedCapabilityResolver;
use magician::magician_v2::execution::verification::ids::GateId;
use magician::magician_v2::execution::EVIDENCE_DATA_TOOL_NAME;
use magician::magician_v2::meeting_control_contribution::{
    apply_signed_app_meeting_control, scope_control_receipt, validate_app_meeting_control,
    AppMeetingControlCurrentAuthority, AppMeetingControlReceipt, CONTROL_RECEIPT_ENTITY,
};
use magician::magician_v2::realtime_events::RuntimeTransportBroadcaster;
use magician::magician_v2::user_requests::UserRequestService;
use magician_apps::apps::app_directory::{
    AppDirectoryActivityRequest, AppDirectoryError, AppDirectoryQuery, AppDirectorySection,
    AppDirectoryService, AppDirectoryTargetKind, DEFAULT_APP_DIRECTORY_LIMIT,
};
use magician_apps::apps::app_publications::AppPublicationReconciler;
use magician_apps::apps::entity_changes::{
    emit_app_entity_change, AppEntityChangeError, AppEntityChangeService,
    DEFAULT_APP_ENTITY_CHANGE_LIMIT, MAX_APP_ENTITY_CHANGE_LIMIT,
};
use magician_apps::apps::entity_outbox::AppRegistryEntityOutboxExt;
use magician_apps::apps::entity_portability::{
    AppDataPortabilityService, AppEntityPortabilityError,
};
use magician_apps::apps::installation_purge::{
    AppInstallationPurgeError, AppInstallationPurgeService, AppWholeInstallationPurgeCommitRequest,
};
use magician_apps::apps::installation_review::{
    AppInstallationApproveRequest, AppInstallationReviewError, AppInstallationReviewService,
};
use magician_apps::apps::registry_lifecycle::AppRegistryLifecycleExt;
use magician_apps::apps::sandbox::{
    AppBridgeMessage, AppBridgeMethod, AppCustomSurfaceTeardown, AppSandboxError,
};
use magician_apps::apps::surface_assets::AppSurfaceAssetError;
use magician_apps::apps::surface_hydration::{
    AppSurfaceHydrationError, AppSurfaceHydrationService, AppSurfaceMutationError,
    AppSurfaceMutationRequest,
};
use magician_apps::apps::surface_runtime::{
    surface_admission_for_enabled_installation, AppCustomSurfaceCancellationReceipt,
    AppCustomSurfaceRetryDisposition, AppCustomSurfaceRunCancelRequest,
    AppCustomSurfaceRunReadRequest, AppCustomSurfaceRunReply, AppCustomSurfaceRunWaitRequest,
    AppCustomSurfaceRuntime, AppCustomSurfaceRuntimeError,
};
use magician_apps::apps::surface_scripted_host::{
    parse_scripted_surface_session_asset_address, scripted_surfaces_enabled_in_config,
    v1_contract_capabilities_reply, AppScriptedSurfaceHostError, AppScriptedSurfaceRequestScope,
    AppScriptedSurfaceRuntime, AppSurfaceV1BridgeMessage, AppSurfaceV1Method,
};
use magician_apps::apps::surface_worker::{package_has_javascript, package_has_wasm};
use magician_apps::apps::update::{
    AppCodeOnlyRollbackRequest, AppDataRewindCommitRequest, AppUpdateCoordinatorError,
    AppUpdateCoordinatorService, AppUpdatePlanRequest,
};

const APP_ROUTE_AUTHORITY_LIFETIME_SECONDS: i64 = 5 * 60;
const APP_PACKAGE_UPLOAD_TIMEOUT: StdDuration = StdDuration::from_secs(2 * 60);
const APP_DATA_BODY_TIMEOUT: StdDuration = StdDuration::from_secs(30);
const APP_HTTP_ACTION_CALLER_REF: &str = "surface:app-http-data-plane";
const APP_CUSTOM_SURFACE_ACTION_CALLER_REF: &str = "surface:app-custom-surface-data-plane";
const DEFAULT_APP_MEMORY_REVIEW_LIMIT: usize = 8;
const MAX_APP_MEMORY_REVIEW_LIMIT: usize = 16;
const APP_MEMORY_REVIEW_MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_APP_MEMORY_REVIEW_CURSOR_BYTES: usize = 1024;
const APP_PURGE_PREVIEW_LIFETIME_SECONDS: i64 = 5 * 60;
const APP_INTERACTIVE_STOP_BODY_BYTES: usize = 8 * 1024;
const APP_ACTION_CANCELLATION_BODY_BYTES: usize = 8 * 1024;
const APP_LIFECYCLE_CONTROL_BODY_BYTES: usize = 8 * 1024;
const APP_BACKGROUND_BEHAVIOR_CONTROL_BODY_BYTES: usize = 8 * 1024;
const APP_PORTABILITY_APPROVAL_LIFETIME_SECONDS: i64 = 5 * 60;
const APP_PORTABILITY_MAX_PENDING_IMPORTS: usize = 16;
const DEFAULT_SLOT_ASSIGNMENT_BLOCKING_OPERATIONS: usize = 4;
// One transfer can transiently hold the bounded compressed archive plus the
// admitted 64 MiB bundle. Serializing this lane avoids multiplying that peak
// memory under concurrent imports/exports; callers receive explicit overload.
const DEFAULT_TRANSFER_BLOCKING_OPERATIONS: usize = 1;

#[derive(Debug, Clone)]
struct AppDataImportHttpSession {
    scope_binding_ref: AppScopeBindingRef,
    destination_installation_id: AppInstallationId,
    source: AppDataArchiveManifest,
    preview: AppDataImportPreview,
    approval_ref: Option<AppReference>,
    approval: Option<AppApprovedDataImport>,
    expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AppPortableExportKind {
    Data,
    Combined,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppPortableExportRequest {
    request_id: AppReference,
    kind: AppPortableExportKind,
    #[serde(default = "default_archive_protection")]
    protection: AppArchiveProtectionRequest,
    #[serde(default)]
    warned_plaintext_confirmed: bool,
}

fn default_archive_protection() -> AppArchiveProtectionRequest {
    AppArchiveProtectionRequest::Default
}

impl ValidateAppContract for AppPortableExportRequest {
    fn validate_app_contract(&self, _limits: &AppContractLimits) -> Result<(), AppContractError> {
        AppReference::parse(self.request_id.as_str().to_owned())?;
        if matches!(
            self.protection,
            AppArchiveProtectionRequest::ExplicitPlaintext
        ) != self.warned_plaintext_confirmed
        {
            return Err(AppContractError::InvalidField {
                field: "warned_plaintext_confirmed",
                message: "must be true only for the separate explicit-plaintext action".to_string(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppDataImportApprovalRequest {
    request_id: AppReference,
    preview_digest: AppDigest,
}

impl ValidateAppContract for AppDataImportApprovalRequest {
    fn validate_app_contract(&self, _limits: &AppContractLimits) -> Result<(), AppContractError> {
        AppReference::parse(self.request_id.as_str().to_owned())?;
        AppDigest::parse(self.preview_digest.as_str().to_owned())?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppDataImportCommitRequest {
    request_id: AppReference,
    preview_digest: AppDigest,
    approval_ref: AppReference,
}

impl ValidateAppContract for AppDataImportCommitRequest {
    fn validate_app_contract(&self, _limits: &AppContractLimits) -> Result<(), AppContractError> {
        AppReference::parse(self.request_id.as_str().to_owned())?;
        AppDigest::parse(self.preview_digest.as_str().to_owned())?;
        AppReference::parse(self.approval_ref.as_str().to_owned())?;
        Ok(())
    }
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct AppDataImportPreviewHttpReceipt {
    request_id: AppReference,
    archive_receipt: AppArchiveWriteReceipt,
    package_payload_present: bool,
    foreign_authority_transferred: bool,
    preview: AppDataImportPreview,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct AppDataImportApprovalHttpReceipt {
    request_id: AppReference,
    approval_ref: AppReference,
    preview_digest: AppDigest,
    expires_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct AppDataImportCommitHttpReceipt<T: Serialize> {
    request_id: AppReference,
    foreign_authority_transferred: bool,
    receipt: T,
}

/// What one scope's system packages did at boot.
#[derive(Debug)]
pub struct SystemPackageBootSummary {
    pub scope_principal: String,
    pub scope_workspace: String,
    pub report: SystemBootAdmissionReport,
    /// `(package_dir, error)` for packages that were admitted but could not be
    /// enabled. Kept separate from admission failures because the two mean
    /// different things: an admission failure is bad bytes, an enablement
    /// failure is good bytes the host would not grant.
    pub enablement_failures: Vec<(String, String)>,
}

/// Every system package this boot resolved out of the seed root but could not
/// carry to a live installation, named so a refusal can say which.
///
/// Sorted and deduplicated. A package that failed admission never reached
/// enablement, so in practice the two lists are disjoint, but this is read as
/// a proof of completeness rather than as a tally and must not depend on that.
fn unrealized_system_packages<'a>(
    admission_failures: impl Iterator<Item = &'a str>,
    enablement_failures: &'a [(String, String)],
) -> Vec<&'a str> {
    let mut unrealized: Vec<&str> = admission_failures
        .chain(
            enablement_failures
                .iter()
                .map(|(package_dir, _)| package_dir.as_str()),
        )
        .collect();
    unrealized.sort_unstable();
    unrealized.dedup();
    unrealized
}

/// How a mint should read a scope that can see none of the bundles this boot
/// admitted. See `mint_admitted_slot_defaults` for why the two callers differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlindScopePolicy {
    /// Boot: an inert deployment legitimately pins nothing.
    Tolerate,
    /// Owner-approval refresh: refuse rather than write an empty set over
    /// whatever the scope already has pinned.
    Refuse,
}

/// What one scope's boot admission proved, retained for the life of the
/// process.
///
/// The digest pins the seed bytes this deployment resolved, the bundle count is
/// what a scope's visibility is judged against, and the installation ids name
/// every package the admission carried. None of the three can be produced by a
/// request, which is what lets an ordinary owner approval — arriving on an HTTP
/// route with no admission report of its own — re-derive the pinned default set
/// the boot itself could not yet prove.
#[derive(Debug, Clone)]
struct BootAdmittedSystemScope {
    inventory_digest: AppDigest,
    admitted_bundles: usize,
    installations: BTreeSet<AppInstallationId>,
}

impl BootAdmittedSystemScope {
    /// Only constructible from a report, so an installation named here is
    /// always one this deployment's own seed-root admission published.
    fn from_report(report: &SystemBootAdmissionReport) -> Self {
        Self {
            inventory_digest: report.inventory_digest().clone(),
            admitted_bundles: report.trusted_inventory().admitted_bundles(),
            installations: report
                .outcomes
                .iter()
                .filter_map(|outcome| outcome.result.as_ref().ok())
                .map(|receipt| receipt.installation_id.clone())
                .collect(),
        }
    }

    /// Whether this installation is one the boot admitted, and so whether a
    /// lifecycle change to it can move the pinned system default set at all.
    fn covers(&self, installation_id: &AppInstallationId) -> bool {
        self.installations.contains(installation_id)
    }
}

mod cleanup;

#[derive(Clone)]
pub struct AppPlatformApi {
    registry: AppRegistryService,
    stager: AppPackageStager,
    candidate_publications: AppCandidatePublicationService,
    procedure_publications: AppProcedurePublicationService,
    capability_publications: AppCapabilityPublicationService,
    entity_adapter: AppEntityAdapterService,
    surface_hydration: AppSurfaceHydrationService,
    entity_changes: AppEntityChangeService,
    app_publications: AppPublicationReconciler,
    app_directory: AppDirectoryService,
    slot_assignments: AppSlotAssignmentStore,
    slot_inventory: Arc<StdRwLock<Option<Arc<dyn AppSlotInventoryResolver>>>>,
    /// This process's own boot admission, per scope. Shared across clones so
    /// the routes read what the boot pass proved rather than re-proving it.
    system_admissions: Arc<StdRwLock<BTreeMap<(String, String), BootAdmittedSystemScope>>>,
    slot_assignment_slots: Arc<Semaphore>,
    installation_purge: AppInstallationPurgeService,
    data_portability: AppDataPortabilityService,
    data_import_sessions: Arc<StdRwLock<BTreeMap<String, AppDataImportHttpSession>>>,
    workflows: Arc<std::sync::RwLock<AppWorkflowService>>,
    custom_surfaces: Arc<AppCustomSurfaceRuntime>,
    scripted_surfaces: Arc<AppScriptedSurfaceRuntime>,
    computed_capabilities: Arc<AppComputedCapabilityOverlayCache>,
    scoped_capabilities: Arc<StdRwLock<Option<Arc<ScopedCapabilityResolver>>>>,
    event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    transfer_slots: Arc<Semaphore>,
    macos_pairing: Option<AppMacosPairingOwnerService>,
    memory_contributions: AppMemoryContributionProjectionService,
    widget_runtime: AppWidgetRuntime,
    // Optional only so a deterministic owner-initialization failure remains a
    // typed unavailable lane; proposal bytes can never construct this service.
    personal_agent_retrieval: Option<AppPersonalAgentRetrievalProjectionService>,
    workspace: ArtifactV2Workspace,
    projection_worker_started: Arc<AtomicBool>,
    background_behaviors: Arc<StdRwLock<Option<AppBehaviorScheduler>>>,
    event_behaviors: Arc<StdRwLock<AppEventBehaviorService>>,
    background_behavior_worker_started: Arc<AtomicBool>,
    background_behavior_event_admission: AppEventIngressAdmission,
}

impl AppPlatformApi {
    /// The governed data plane, shared rather than rebuilt.
    ///
    /// `/social/*` serves the Town Square package's corpus through this same
    /// adapter (queue item 6, slice 4). Constructing a second one would mean a
    /// second registry and a second set of caches over one database, which is
    /// how two readers of the same rows start disagreeing.
    pub fn entity_adapter(&self) -> AppEntityAdapterService {
        self.entity_adapter.clone()
    }

    pub fn registry(&self) -> AppRegistryService {
        self.registry.clone()
    }

    /// Registered after boot, so an early caller legitimately sees `None`.
    pub fn background_behaviors(&self) -> Arc<StdRwLock<Option<AppBehaviorScheduler>>> {
        Arc::clone(&self.background_behaviors)
    }

    /// Admit the deployment's `distribution: system` packages for every scope.
    ///
    /// This is the owner the platform has been missing. Until it existed, the
    /// packages under the seed root parsed correctly in tests and were
    /// unreachable in a running binary, because both staging and publication
    /// refuse a system manifest without host-controlled digest-pinned boot
    /// admission. Every one of the six app-platform increments named this as
    /// its blocking gate.
    ///
    /// Admission first publishes **inert `ready_for_review` installations**,
    /// exactly like every other producer. When the deployment explicitly sets
    /// `enable_at_boot`, the unforgeable admission report then mints a host
    /// grantor bound to one successful installation at a time. Seed provenance
    /// alone still earns class rather than power.
    ///
    /// Failures are logged per package and never propagate: one malformed seed
    /// package must not cost the deployment the other built-ins, and a startup
    /// path that returns on the first error would do precisely that.
    pub async fn admit_system_packages_at_boot(
        &self,
        settings: AppSystemPackageSettings,
    ) -> Vec<SystemPackageBootSummary> {
        if !settings.admit_at_boot {
            tracing::info!("system boot admission disabled by configuration");
            return Vec::new();
        }
        if !settings.enable_at_boot {
            tracing::info!(
                "system package boot activation is disabled; admitted packages remain inert until owner approval"
            );
        }
        let workspace = self.workspace.clone();
        let scopes = match tokio::task::spawn_blocking(move || workspace.list_scopes()).await {
            Ok(scopes) => scopes,
            Err(error) => {
                tracing::warn!(error = %error, "system boot admission could not enumerate scopes");
                return Vec::new();
            },
        };
        // The default scope always exists conceptually even before anything has
        // written to it, and it is the scope a single-user deployment actually
        // uses. Discovering zero scopes on a first boot must not mean the
        // deployment ships with no apps.
        let default_scope = (
            DEFAULT_SCOPE_PRINCIPAL.to_owned(),
            DEFAULT_SCOPE_WORKSPACE.to_owned(),
        );
        let mut scopes = scopes;
        if !scopes.contains(&default_scope) {
            scopes.push(default_scope);
        }
        // A reserved sink has no owner to approve an app and no surface to run
        // one on. Admitting into it copied the whole system package set and an
        // app store into a bucket that exists only to catch scopeless records —
        // 7.5 MB of byte-identical bundles per sink, plus a SQLite file nothing
        // would ever read. Enumerating every scope is fine; materializing a
        // subsystem the scope cannot use is what was wrong.
        let before = scopes.len();
        scopes.retain(|(principal, workspace_name)| {
            scope_hosts_user_subsystems(principal, workspace_name)
        });
        if scopes.len() != before {
            tracing::debug!(
                skipped = before - scopes.len(),
                "system boot admission skipped reserved sinks"
            );
        }

        let run_ref = match AppReference::parse(format!(
            "run:app-system-boot-admission:{}",
            uuid::Uuid::new_v4()
        )) {
            Ok(run_ref) => run_ref,
            Err(error) => {
                tracing::warn!(error = %error, "invalid system boot admission run reference");
                return Vec::new();
            },
        };

        let mut admitted = Vec::new();
        // Collected across the whole pass rather than applied per scope:
        // maintaining a scope's pinned defaults is synchronous and enumerates
        // scopes itself, so the awaiting work happens here and the writing
        // happens once, after the loop.
        let mut slot_defaults = BootAdmittedSlotDefaults::new();
        for (principal, workspace_name) in scopes {
            let now = Utc::now();
            let authenticated = match system_worker_scope_for_actor(
                &principal,
                &workspace_name,
                "worker:app-system-boot-admission",
                &run_ref,
                now,
            ) {
                Ok(authenticated) => authenticated,
                Err(error) => {
                    tracing::warn!(
                        principal,
                        workspace = workspace_name,
                        error = %error,
                        "invalid system boot admission scope"
                    );
                    continue;
                },
            };
            let report = match admit_system_packages(
                &self.candidate_publications,
                &self.workspace,
                &authenticated,
                now,
            )
            .await
            {
                Ok(report) => report,
                Err(error) => {
                    tracing::warn!(
                        principal,
                        workspace = workspace_name,
                        error = %error,
                        "system boot admission could not resolve the seed inventory"
                    );
                    continue;
                },
            };
            // Surfacing authority follows the same proof installation does.
            // Without this the widget runtime compiles every system manifest as
            // installable and refuses it, so the deployment's own widgets never
            // reach a client. The pin can only come out of the resolution
            // above — this API cannot mint one.
            self.widget_runtime
                .adopt_trusted_system_inventory(report.trusted_inventory());
            for (package, error) in report.failures() {
                tracing::warn!(
                    principal,
                    workspace = workspace_name,
                    package,
                    error,
                    "system package was refused at boot admission"
                );
            }
            tracing::info!(
                principal,
                workspace = workspace_name,
                admitted = report.admitted(),
                total = report.outcomes.len(),
                inventory_digest = %report.inventory_digest(),
                trusted_bundles = report.trusted_inventory().admitted_bundles(),
                "system boot admission complete"
            );
            let mut enablement_failures = Vec::new();
            if settings.enable_at_boot {
                for outcome in &report.outcomes {
                    let Ok(receipt) = &outcome.result else {
                        continue;
                    };
                    if let Err(error) = self
                        .enable_boot_admitted_package(
                            &report,
                            &receipt.installation_id,
                            &principal,
                            &workspace_name,
                            &outcome.package_dir,
                        )
                        .await
                    {
                        enablement_failures.push((outcome.package_dir.clone(), error));
                    }
                }
            }
            // Pinned slot defaults are read from the resolved inventory, so
            // they follow enablement rather than admission: only an enabled
            // package reaches the picker, and pinning a slot to a package
            // nobody can render would leave a default no user can clear.
            //
            // A scope that does not resolve is left out of the source entirely.
            // Maintenance then refuses it and keeps whatever it already had,
            // which is the fail-closed half — an empty set is a real
            // instruction to retire every default, and a boot that merely
            // could not look must never issue it. The same holds one package
            // at a time, which is why the resolve below is handed this boot's
            // failures rather than only its digest.
            // Register this scope's compiled installations before minting.
            // The inventory snapshot trusts only packages the widget runtime
            // has registered, and the projection worker's first refresh runs
            // later; minting first read an empty inventory and refused every
            // scope on a healthy boot ("sees none of the N system bundles").
            if let Err(error) = self
                .widget_runtime
                .refresh_scope_registrations(&authenticated, Utc::now())
                .await
            {
                tracing::warn!(
                    principal,
                    workspace = workspace_name,
                    error = %error,
                    "widget registration refresh before the slot-default mint failed; the mint may refuse this scope"
                );
            }
            match self
                .resolve_admitted_slot_defaults(&authenticated, &report, &enablement_failures)
                .await
            {
                Ok(scope_defaults) => {
                    // A contested slot is pinned to nobody, and an empty slot
                    // reads exactly like a widget that was never declared. Only
                    // the author of the conflicting seed manifests can settle
                    // it, and this warning is the only thing that tells them a
                    // conflict happened at all — the drop is otherwise silent.
                    for (slot_id, claimants) in scope_defaults.contested_slots() {
                        let packages = claimants
                            .iter()
                            .map(AppReference::as_str)
                            .collect::<Vec<_>>()
                            .join(", ");
                        tracing::warn!(
                            principal,
                            workspace = workspace_name,
                            slot = slot_id.as_str(),
                            packages = packages.as_str(),
                            "system packages contest a slot default; the slot is pinned to nobody"
                        );
                    }
                    slot_defaults.record(authenticated.scope(), scope_defaults);
                },
                Err(error) => tracing::warn!(
                    principal,
                    workspace = workspace_name,
                    error,
                    "scope has no admitted system slot defaults this boot"
                ),
            }
            admitted.push(SystemPackageBootSummary {
                scope_principal: principal.clone(),
                scope_workspace: workspace_name.clone(),
                report,
                enablement_failures,
            });
        }
        self.maintain_system_slot_defaults(slot_defaults).await;
        admitted
    }

    /// One boot scope's pinned system default set.
    ///
    /// Refuses outright when this boot did not carry every package it resolved
    /// all the way to a live installation. The set is applied as a whole set,
    /// so treating a publication or enablement failure as mere absence would
    /// retire that package's pins for every user. Only a package that actually
    /// left the seed root may retire its slots, and such a package leaves no
    /// failure behind. The digest cannot make that distinction: it is computed
    /// over the resolved inventory before any publication, so a half-realized
    /// boot carries the same digest a healthy one does.
    ///
    /// The admission this proves is retained, because the boot is not the last
    /// caller that needs it. When `enable_at_boot` is disabled, nothing is
    /// enabled while this runs, so the mint below refuses on every first boot;
    /// the owner approval that later makes the set provable arrives on a route
    /// with no report of its own.
    async fn resolve_admitted_slot_defaults(
        &self,
        authenticated: &AuthenticatedAppScope,
        report: &SystemBootAdmissionReport,
        enablement_failures: &[(String, String)],
    ) -> Result<TrustedSystemSlotDefaultSet, String> {
        let unrealized = unrealized_system_packages(
            report.failures().map(|(package_dir, _)| package_dir),
            enablement_failures,
        );
        if !unrealized.is_empty() {
            return Err(format!(
                "boot did not realize every resolved system package: {}",
                unrealized.join(", ")
            ));
        }
        let admitted = BootAdmittedSystemScope::from_report(report);
        // Retained before the mint and whether or not it succeeds. Retention is
        // deliberately downstream of the completeness check above: a boot that
        // could not realize one of its own packages must not hand a later
        // approval an admission that silently omits it.
        self.retain_boot_admitted_system_scope(authenticated.scope(), admitted.clone());
        self.mint_admitted_slot_defaults(authenticated, &admitted, BlindScopePolicy::Tolerate)
            .await
    }

    /// Derive one scope's pinned default set from the inventory the host's own
    /// widget runtime resolves for it.
    ///
    /// The admission's inventory digest comes from a seed-root resolution and
    /// pins which bundles the deployment ships; the snapshot supplies the
    /// per-scope installation generations no deployment-wide digest knows. Both
    /// halves are host-minted — this API cannot forge either — which is what
    /// lets the result carry trusted-system authority.
    ///
    /// A set applied as the whole truth has to account for every admitted
    /// installation, because not every absence is recorded as a failure.
    /// Widget registration failures are swallowed inside the resolver —
    /// `reconcile_scope_registrations` logs an omitted installation at debug
    /// level and moves on — so a package can be admitted, enabled, and still
    /// missing from the picker with nothing left behind to refuse on.
    ///
    /// The proof is per installation rather than a floor on visible package
    /// count. Every installation this boot admitted must either appear in the
    /// snapshot as a trusted-system package or be one the registry can explain
    /// as inactive. This distinction matters when every built-in is awaiting
    /// review or has been owner-disabled: an empty set is then real and must be
    /// allowed to retire stale pins. The explanation is
    /// [`Self::unaccounted_system_installations`].
    ///
    /// This over-refuses when the explanation is unavailable, deliberately. A
    /// scope that keeps a stale pin keeps an inert one — slot resolution hides
    /// a widget whose package cannot render — which is a far smaller cost than
    /// blanking a healthy deployment's defaults for as long as it stays up.
    /// Whether a scope that can see *none* of the bundles this boot admitted is
    /// allowed to mint a set.
    ///
    /// At boot it is: an inert deployment legitimately pins nothing, and the
    /// admitted-but-not-yet-enabled state is the normal one when
    /// `enable_at_boot` is off. On an owner-approval refresh it is not: that
    /// set is applied as the whole truth over whatever the scope already has,
    /// and a scope seeing none of its own admitted bundles cannot be the
    /// authority for "this deployment pins nothing". The type's own rule —
    /// "I could not read the seed root" and "this deployment pins nothing" are
    /// different facts — is the same distinction, one caller further out.
    async fn mint_admitted_slot_defaults(
        &self,
        authenticated: &AuthenticatedAppScope,
        admitted: &BootAdmittedSystemScope,
        blind_scope: BlindScopePolicy,
    ) -> Result<TrustedSystemSlotDefaultSet, String> {
        let resolver = self
            .slot_inventory
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or_else(|| "the widget inventory owner is not installed".to_owned())?;
        let inventory = resolver
            .snapshot(authenticated, Utc::now())
            .await
            .map_err(|error| format!("inventory: {error}"))?;
        let admitted_bundles = admitted.admitted_bundles;
        // Membership, not a count: a package the snapshot carries as
        // *installable* contributes no system default either — the mint reads
        // only trusted-system packages — so being present without provenance
        // is another way of being missing, and must read as one here.
        let visible_system_packages = inventory
            .packages
            .iter()
            .filter(|package| package.has_trusted_system_provenance())
            .map(|package| package.binding.installation_id.as_str())
            .collect::<BTreeSet<_>>();
        if blind_scope == BlindScopePolicy::Refuse
            && visible_system_packages.is_empty()
            && admitted_bundles > 0
        {
            return Err(format!(
                "the scope sees none of the {admitted_bundles} system bundles this boot admitted"
            ));
        }
        let unaccounted = self
            .unaccounted_system_installations(authenticated, admitted, &visible_system_packages)
            .await?;
        if !unaccounted.is_empty() {
            return Err(format!(
                "the scope sees {} of the {admitted_bundles} system bundles this boot admitted \
                 and cannot account for {}",
                visible_system_packages.len(),
                unaccounted.join(", ")
            ));
        }
        TrustedSystemSlotDefaultSet::from_boot_inventory(
            admitted.inventory_digest.clone(),
            &inventory,
        )
        .map_err(|error| format!("admitted defaults: {error}"))
    }

    /// The boot-admitted installations this scope owes the mint a package for
    /// and did not supply.
    ///
    /// Absence alone is not a fault. An installation the owner has not
    /// approved, or has disabled, quarantined or uninstalled, is provably not
    /// renderable — slot resolution would hide its widget anyway — so its
    /// pinned slots are free to retire, and refusing on it would freeze a
    /// scope's whole default set on an ordinary owner decision. An
    /// installation the registry still reports as `Enabled` is the opposite
    /// fact: the picker owes it a package state, and the ways it can be
    /// missing are the ones that record nothing — a widget registration the
    /// resolver swallowed at debug level, or a package the runtime is holding
    /// inert. Those must refuse, because the set is applied as the whole
    /// truth and the omission would delete that package's pins.
    ///
    /// A read that fails or finds no installation counts as unaccounted for:
    /// an absence this cannot explain is exactly what it exists to refuse on.
    ///
    /// Called for every mint, but a healthy scope whose packages are all
    /// visible pays for no registry read at all.
    async fn unaccounted_system_installations(
        &self,
        authenticated: &AuthenticatedAppScope,
        admitted: &BootAdmittedSystemScope,
        visible_system_packages: &BTreeSet<&str>,
    ) -> Result<Vec<String>, String> {
        let mut unaccounted = Vec::new();
        for installation_id in &admitted.installations {
            if visible_system_packages.contains(installation_id.as_str()) {
                continue;
            }
            // A fresh clock per read rather than the snapshot's: a scope whose
            // session expired while the snapshot was being built must fail the
            // read, not be carried past its expiry by an older timestamp.
            let installation = self
                .registry
                .installation(authenticated, installation_id, Utc::now())
                .await
                .map_err(|error| format!("installation {installation_id}: {error}"))?;
            match installation {
                Some(installation)
                    if installation.lifecycle.status == AppInstallationStatus::Enabled =>
                {
                    unaccounted.push(installation_id.as_str().to_owned());
                },
                Some(_) => {},
                None => unaccounted.push(format!("{installation_id} (missing)")),
            }
        }
        Ok(unaccounted)
    }

    fn retain_boot_admitted_system_scope(
        &self,
        scope: &AppScope,
        admitted: BootAdmittedSystemScope,
    ) {
        self.system_admissions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                (
                    scope.principal.as_str().to_owned(),
                    scope.workspace.as_str().to_owned(),
                ),
                admitted,
            );
    }

    /// `None` for a scope this process never admitted system packages for, so
    /// a deployment with boot admission switched off mints nothing rather than
    /// falling back to something weaker.
    fn boot_admitted_system_scope(&self, scope: &AppScope) -> Option<BootAdmittedSystemScope> {
        self.system_admissions
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(
                scope.principal.as_str().to_owned(),
                scope.workspace.as_str().to_owned(),
            ))
            .cloned()
    }

    /// Re-pin one scope's system slot defaults after an owner approval.
    ///
    /// Deployments that leave `enable_at_boot` disabled publish inert
    /// `ready_for_review` installations. Approval is what grants an app its
    /// authority, and the platform refuses to record an ordinary background
    /// worker as the grantor. Without this post-approval caller, the pinned
    /// defaults would first appear only after a later boot, even though the
    /// approval itself is durable.
    ///
    /// The request contributes nothing but the scope it already authenticated
    /// and the installation it already approved. The digest, the bundle count
    /// and the admitted ids all come from this process's own boot admission,
    /// and the inventory from the host's own resolver.
    ///
    /// Returns what maintenance did so a test can assert the pin moved; the
    /// route only logs it. A refusal is not the approval's failure — the
    /// installation is approved either way — so nothing here propagates.
    async fn refresh_system_slot_defaults(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
    ) -> Option<AppSlotSystemDefaultsMaintenance> {
        let admitted = self.boot_admitted_system_scope(authenticated.scope())?;
        // Only a boot-admitted package can move the set. A widget that is not
        // trusted-system cannot carry a system default at all — picker
        // validation refuses `system_default` on one — so every other approval
        // would buy a snapshot and a file lock for a guaranteed no-op.
        if !admitted.covers(installation_id) {
            return None;
        }
        let defaults = match self
            .mint_admitted_slot_defaults(authenticated, &admitted, BlindScopePolicy::Refuse)
            .await
        {
            Ok(defaults) => defaults,
            Err(error) => {
                // Ordinary while a deployment is approved one package at a
                // time: nothing is written and the scope keeps its pins.
                tracing::debug!(
                    principal = authenticated.scope().principal.as_str(),
                    workspace = authenticated.scope().workspace.as_str(),
                    error,
                    "approval did not yield an admitted system slot default set"
                );
                return None;
            },
        };
        // Same conflict, same authoring bug, same only-warning as at boot: the
        // approval order decides nothing, so a slot two seed manifests both
        // claim is dropped here too and the drop is otherwise silent.
        for (slot_id, claimants) in defaults.contested_slots() {
            let packages = claimants
                .iter()
                .map(AppReference::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            tracing::warn!(
                principal = authenticated.scope().principal.as_str(),
                workspace = authenticated.scope().workspace.as_str(),
                slot = slot_id.as_str(),
                packages = packages.as_str(),
                "system packages contest a slot default; the slot is pinned to nobody"
            );
        }
        let mut resolved = BootAdmittedSlotDefaults::new();
        resolved.record(authenticated.scope(), defaults);
        // The maintainer enumerates every scope on disk while this call proves
        // exactly one, so the others refuse and keep what they have — correct,
        // and not this approval's news to report.
        let outcomes = self.run_system_slot_defaults_maintenance(resolved).await;
        let outcome = outcomes.into_iter().find(|outcome| {
            outcome.principal == authenticated.scope().principal.as_str()
                && outcome.workspace == authenticated.scope().workspace.as_str()
        })?;
        match outcome.result {
            Ok(maintenance) => {
                tracing::info!(
                    principal = outcome.principal,
                    workspace = outcome.workspace,
                    pinned = maintenance.pinned_slots,
                    retired = maintenance.retired_slots,
                    changed = maintenance.changed,
                    "system slot defaults maintained after approval"
                );
                Some(maintenance)
            },
            Err(error) => {
                tracing::warn!(
                    principal = outcome.principal,
                    workspace = outcome.workspace,
                    error = %error,
                    "scope kept the system slot defaults it already had"
                );
                None
            },
        }
    }

    /// Run the host-owned maintainer off the async runtime.
    ///
    /// Scope enumeration and the per-scope file lock are blocking work. Every
    /// caller goes through the maintainer rather than writing one scope's
    /// document directly: it is the single writer of `workspace_defaults`, and
    /// a second one would be free to disagree with it about which packages a
    /// set omits.
    async fn run_system_slot_defaults_maintenance(
        &self,
        resolved: BootAdmittedSlotDefaults,
    ) -> Vec<AppSlotSystemDefaultsOutcome> {
        let store = self.slot_assignments.clone();
        match tokio::task::spawn_blocking(move || {
            AppSlotSystemDefaultsMaintainer::new(store).maintain_every_scope(&resolved, Utc::now())
        })
        .await
        {
            Ok(outcomes) => outcomes,
            Err(error) => {
                tracing::warn!(error = %error, "system slot default maintenance did not run");
                Vec::new()
            },
        }
    }

    /// Republish every stored scope's pinned system default set.
    ///
    /// Without this the maintainer has no caller, `workspace_defaults` stays
    /// permanently empty, and no system package's widget ever becomes a
    /// default for anyone — the widgets compile, publish, enable, and still
    /// surface nowhere.
    ///
    /// Runs once for the deployment rather than inside the scope loop because
    /// the maintainer enumerates scopes itself, including the default scope a
    /// single-user deployment uses before anything has written to it. It is
    /// blocking work — a directory scan plus a per-scope file lock — so it
    /// runs on a blocking worker.
    async fn maintain_system_slot_defaults(&self, resolved: BootAdmittedSlotDefaults) {
        if resolved.is_empty() {
            // Not "this deployment pins nothing" but "this boot could not tell",
            // so every scope keeps what it has. Running maintenance anyway
            // would refuse every scope and say so once per workspace.
            tracing::warn!(
                "no scope resolved an admitted system slot default set; pinned defaults unchanged"
            );
            return;
        }
        let outcomes = self.run_system_slot_defaults_maintenance(resolved).await;
        for outcome in &outcomes {
            match &outcome.result {
                Ok(maintenance) => tracing::info!(
                    principal = outcome.principal,
                    workspace = outcome.workspace,
                    pinned = maintenance.pinned_slots,
                    retired = maintenance.retired_slots,
                    changed = maintenance.changed,
                    "system slot defaults maintained"
                ),
                Err(error) => tracing::warn!(
                    principal = outcome.principal,
                    workspace = outcome.workspace,
                    error = %error,
                    "scope kept the system slot defaults it already had"
                ),
            }
        }
    }

    /// Approve one boot-admitted system package so its surfaces are reachable.
    ///
    /// This is a host review, not a forged owner decision. It runs the same
    /// two steps every reviewer runs — `review()` to obtain the material, then
    /// `approve()` presenting the digest it was actually shown — so the
    /// "you approved what you were shown" property holds literally rather than
    /// by assertion. The acting identity is the boot-admission worker, so an
    /// audit reads "the host enabled its own package", never "the owner
    /// approved this".
    ///
    /// The ordinary request defaults are used, so implicit-all grant fields and
    /// explicit-deny grant fields retain their normal review semantics. The
    /// owner can still revoke through the ordinary grant-revocation, disable
    /// and quarantine routes.
    ///
    /// An early status read short-circuits an already-enabled installation, so
    /// this remains idempotent across boots without trying to review committed
    /// material again.
    async fn enable_boot_admitted_package(
        &self,
        report: &SystemBootAdmissionReport,
        installation_id: &AppInstallationId,
        principal: &str,
        workspace_name: &str,
        package_dir: &str,
    ) -> Result<(), String> {
        let now = Utc::now();
        let authenticated = report
            .host_grantor_scope(installation_id, now)
            .map_err(|error| {
                tracing::warn!(
                    principal,
                    workspace = workspace_name,
                    package = package_dir,
                    error = %error,
                    "could not mint authority for a boot-admitted system package"
                );
                format!("host grantor: {error}")
            })?;
        let installation = self
            .registry
            .installation(&authenticated, installation_id, now)
            .await
            .map_err(|error| format!("read installation: {error}"))?
            .ok_or_else(|| {
                "admitted system installation disappeared before enablement".to_owned()
            })?;
        match installation.lifecycle.status {
            AppInstallationStatus::Enabled => {
                tracing::debug!(
                    principal,
                    workspace = workspace_name,
                    package = package_dir,
                    "system package was already enabled at boot"
                );
                return Ok(());
            },
            AppInstallationStatus::Disabled
            | AppInstallationStatus::Quarantined
            | AppInstallationStatus::UpdatePending
            | AppInstallationStatus::UninstalledRetained
            | AppInstallationStatus::Purged => {
                // The config opts into first activation, not into undoing a
                // later owner safety decision or approving an owner-started
                // update. Counting this as success lets slot maintenance
                // retire the currently hidden package's defaults instead of
                // freezing the entire scope.
                tracing::debug!(
                    principal,
                    workspace = workspace_name,
                    package = package_dir,
                    status = ?installation.lifecycle.status,
                    "system package remains inactive by owner decision"
                );
                return Ok(());
            },
            AppInstallationStatus::ReadyForReview => {},
        }
        let review = self
            .installation_review()
            .review(&authenticated, installation_id, now)
            .await
            .map_err(|error| {
                tracing::warn!(
                    principal,
                    workspace = workspace_name,
                    package = package_dir,
                    error = %error,
                    "could not review a boot-admitted system package"
                );
                format!("review: {error}")
            })?;
        let request = AppInstallationApproveRequest {
            review_material_digest: Some(review.workflow_material_digest.clone()),
            ..Default::default()
        };
        match self
            .installation_review()
            .approve(&authenticated, installation_id, request, Utc::now())
            .await
        {
            Ok(receipt) => {
                tracing::info!(
                    principal,
                    workspace = workspace_name,
                    package = package_dir,
                    status = ?receipt.status,
                    outcome = ?receipt.outcome,
                    "system package enabled at boot"
                );
                Ok(())
            },
            Err(error) => {
                tracing::warn!(
                    principal,
                    workspace = workspace_name,
                    package = package_dir,
                    error = %error,
                    "could not enable a boot-admitted system package"
                );
                Err(format!("approve: {error}"))
            },
        }
    }

    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        let registry = AppRegistryService::new(workspace.clone());
        let stager = AppPackageStager::new(workspace.clone());
        let computed_capabilities = Arc::new(AppComputedCapabilityOverlayCache::default());
        let widget_runtime =
            AppWidgetRuntime::with_package_stager(registry.clone(), stager.clone());
        let scoped_capabilities = Arc::new(StdRwLock::new(None::<Arc<ScopedCapabilityResolver>>));
        // The runtime resolver is safe before compiler registration: its
        // empty inventory grants no assignment authority. Once the admitted
        // compiler registers an exact installation generation, this same
        // owner revalidates registry/package truth for every snapshot.
        let slot_inventory: Arc<StdRwLock<Option<Arc<dyn AppSlotInventoryResolver>>>> =
            Arc::new(StdRwLock::new(Some(Arc::new(widget_runtime.clone()))));
        let hide_overlay = Arc::clone(&computed_capabilities);
        let hide_widgets = widget_runtime.clone();
        let hide_scoped = Arc::clone(&scoped_capabilities);
        registry.set_computed_capability_hide(Arc::new(move |principal, workspace| {
            hide_overlay.evict(principal, workspace);
            hide_widgets.evict_scope_materializations(principal, workspace);
            if let Some(resolver) = hide_scoped
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
            {
                resolver.invalidate_scope(principal, workspace);
            }
        }));
        let personal_agent_retrieval =
            match AppPersonalAgentRetrievalProjectionService::from_current_registry(
                registry.clone(),
                workspace.clone(),
            ) {
                Ok(service) => Some(service),
                Err(error) => {
                    tracing::error!(
                        error = %error,
                        "personal-agent retrieval projection owner is unavailable"
                    );
                    None
                },
            };
        let memory_contributions =
            AppMemoryContributionProjectionService::new(registry.clone(), workspace.clone());
        let event_behaviors = AppEventBehaviorService::new(
            registry.clone(),
            stager.clone(),
            AppEventBehaviorRuntimeLimits::default(),
        )
        .expect("default app event-behavior runtime limits are valid");
        Self {
            entity_adapter: AppEntityAdapterService::new(registry.clone()),
            surface_hydration: AppSurfaceHydrationService::new(registry.clone()),
            entity_changes: AppEntityChangeService::new(registry.clone()),
            app_publications: AppPublicationReconciler::new(registry.clone(), workspace.clone()),
            app_directory: AppDirectoryService::new(registry.clone()),
            slot_assignments: AppSlotAssignmentStore::new(workspace.clone()),
            slot_inventory,
            system_admissions: Arc::new(StdRwLock::new(BTreeMap::new())),
            slot_assignment_slots: Arc::new(Semaphore::new(
                DEFAULT_SLOT_ASSIGNMENT_BLOCKING_OPERATIONS,
            )),
            installation_purge: AppInstallationPurgeService::new(registry.clone()),
            data_portability: AppDataPortabilityService::new(registry.clone()),
            data_import_sessions: Arc::new(StdRwLock::new(BTreeMap::new())),
            workflows: Arc::new(std::sync::RwLock::new(AppWorkflowService::new(
                workspace.clone(),
            ))),
            candidate_publications: AppCandidatePublicationService::from_parts(
                workspace.clone(),
                registry.clone(),
                stager.clone(),
            ),
            procedure_publications: AppProcedurePublicationService::new(registry.clone()),
            capability_publications: AppCapabilityPublicationService::new(registry.clone()),
            registry,
            stager,
            custom_surfaces: Arc::new(AppCustomSurfaceRuntime::with_killable_worker()),
            scripted_surfaces: Arc::new(AppScriptedSurfaceRuntime::default()),
            computed_capabilities,
            scoped_capabilities,
            event_broadcaster: None,
            transfer_slots: Arc::new(Semaphore::new(DEFAULT_TRANSFER_BLOCKING_OPERATIONS)),
            macos_pairing: AppMacosPairingOwnerService::new(workspace.clone()).ok(),
            memory_contributions,
            widget_runtime,
            personal_agent_retrieval,
            workspace,
            projection_worker_started: Arc::new(AtomicBool::new(false)),
            background_behaviors: Arc::new(StdRwLock::new(None)),
            event_behaviors: Arc::new(StdRwLock::new(event_behaviors)),
            background_behavior_worker_started: Arc::new(AtomicBool::new(false)),
            background_behavior_event_admission: AppEventIngressAdmission::default(),
        }
    }

    pub fn computed_capability_overlay(&self) -> Arc<AppComputedCapabilityOverlayCache> {
        Arc::clone(&self.computed_capabilities)
    }

    /// Host-owned registration/read seam for admitted widget declaration
    /// compilers and the bounded slot-picker inventory owner.
    pub fn widget_runtime(&self) -> AppWidgetRuntime {
        self.widget_runtime.clone()
    }

    pub fn set_scoped_capability_resolver(&self, resolver: Arc<ScopedCapabilityResolver>) {
        *self
            .scoped_capabilities
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(resolver);
    }

    /// Install the host-owned widget inventory. Implementations must derive
    /// trusted-system provenance from digest-pinned boot admission, never from
    /// manifest distribution or ordinary registry directory metadata.
    pub fn set_slot_inventory_resolver(&self, resolver: Arc<dyn AppSlotInventoryResolver>) {
        *self
            .slot_inventory
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(resolver);
    }

    pub fn set_workflow_service(&self, workflows: AppWorkflowService) {
        *self
            .workflows
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = workflows;
    }

    /// Rotate the global app-data root generation through the registry owner
    /// and immediately rekey the authenticated operator scope. The caller is
    /// responsible for enforcing the local setup-token boundary.
    pub(crate) async fn rotate_app_data_root_key_generation(
        &self,
        authenticated: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<String, AppRegistryError> {
        self.registry
            .rotate_at_rest_key_generation(authenticated, now)
            .await
    }

    fn installation_review(&self) -> AppInstallationReviewService {
        AppInstallationReviewService::from_parts(self.registry.clone(), self.stager.clone())
    }

    /// The auth gate must consult the same live surface sessions as the host
    /// and asset handlers; a separate runtime would reject every frame.
    pub fn scripted_surface_authenticator(
        &self,
    ) -> magician::magician_v2::auth::middleware::ScriptedSurfaceAssetAuthenticator {
        let surfaces = Arc::clone(&self.scripted_surfaces);
        magician::magician_v2::auth::middleware::ScriptedSurfaceAssetAuthenticator::new(
            move |path, now| {
                let credential = surfaces.asset_credential(path, now)?;
                let scope = AppScope {
                    principal: AppReference::parse(credential.scope().principal.clone()).ok()?,
                    workspace: AppReference::parse(credential.scope().workspace.clone()).ok()?,
                };
                VerifiedAppTransportSession::from_verified_session(
                    scope.clone(),
                    scope_binding_ref(&scope).ok()?,
                    credential.host_session_ref().clone(),
                    credential.host_session_ref().clone(),
                    AppRevision::new(1).ok()?,
                    now,
                    credential.expires_at(),
                )
                .ok()?
                .bind_request(Some(&scope), &now)
                .ok()
            },
        )
    }

    fn update_coordinator(&self) -> AppUpdateCoordinatorService {
        AppUpdateCoordinatorService::from_parts(self.registry.clone(), self.stager.clone())
    }

    /// Server-owned VibeDev adoption seam. There is intentionally no HTTP
    /// request-body equivalent: the candidate service loads and verifies the
    /// canonical gate and green attestation before it will publish review
    /// state, and this facade preserves the process-shared registry/stager.
    pub async fn publish_verified_vibedev_candidate(
        &self,
        authenticated: &AuthenticatedAppScope,
        gate_id: GateId,
        root_task_id: String,
        root_execution_id: String,
        package_relative_path: PathBuf,
        now: DateTime<Utc>,
    ) -> Result<
        magician::magician_v2::apps::candidate_publication::AppCandidatePublicationReceipt,
        AppCandidatePublicationError,
    > {
        self.candidate_publications
            .publish_verified_vibedev_candidate(
                authenticated,
                gate_id,
                root_task_id,
                root_execution_id,
                package_relative_path,
                now,
            )
            .await
    }

    fn workflow_service(&self) -> AppWorkflowService {
        self.workflows
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Server-owned schedule ingress. HTTP invocation never reaches this
    /// adapter; the move-only authority binds the exact scheduled payload and
    /// launch identity before common workflow admission derives background
    /// scheduler/resource policy.
    pub async fn invoke_scheduled_app_action(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        invocation: AppActionInvocation<serde_json::Value>,
        resources: &AgentResources,
        launch_ref: AppReference,
        now: DateTime<Utc>,
    ) -> Result<AppWorkflowLaunch, AppWorkflowError> {
        let authority = AppBackgroundLaunchAuthority::from_server_schedule(
            authenticated,
            installation_id.clone(),
            launch_ref,
            &invocation.input,
            now,
        )?;
        self.workflow_service()
            .invoke_background(
                authenticated,
                installation_id,
                invocation,
                resources,
                authority,
                now,
            )
            .await
    }

    /// Behavior-specific schedule ingress. In addition to the ordinary
    /// background launch proof, this seals the exact reviewed behavior grant
    /// into the durable workflow binding so execution/resume and model I/O can
    /// reopen the same operation/resource authority.
    pub async fn invoke_scheduled_app_behavior(
        &self,
        authenticated: &AuthenticatedAppScope,
        dispatch: &AppBehaviorDispatch,
        resources: &AgentResources,
        authority: AppBackgroundLaunchAuthority,
        now: DateTime<Utc>,
    ) -> Result<AppWorkflowLaunch, AppWorkflowError> {
        self.workflow_service()
            .invoke_background(
                authenticated,
                dispatch.installation_id(),
                dispatch.invocation().clone(),
                resources,
                authority,
                now,
            )
            .await
    }

    /// Event-router-specific ingress. The durable dispatch owns the exact
    /// reviewed grant and projection; this adapter deliberately accepts no
    /// caller-supplied installation or launch identity.
    pub async fn invoke_event_app_behavior(
        &self,
        authenticated: &AuthenticatedAppScope,
        dispatch: &AppEventBehaviorDispatch,
        resources: &AgentResources,
        authority: AppBackgroundLaunchAuthority,
        now: DateTime<Utc>,
    ) -> Result<AppWorkflowLaunch, AppWorkflowError> {
        self.workflow_service()
            .invoke_background(
                authenticated,
                dispatch.installation_id(),
                dispatch.invocation().clone(),
                resources,
                authority,
                now,
            )
            .await
    }

    pub fn with_event_broadcaster(
        mut self,
        event_broadcaster: Arc<RuntimeTransportBroadcaster>,
    ) -> Self {
        self.app_publications = self
            .app_publications
            .with_event_broadcaster(Arc::clone(&event_broadcaster));
        self.event_broadcaster = Some(event_broadcaster);
        self
    }

    /// Build the single process-owned canonical event router installed on the
    /// Artifact sink at boot. It is intentionally not exposed as an HTTP
    /// ingress and uses the same clone-shared workflow/registry owners as the
    /// background dispatcher.
    pub fn canonical_event_behavior_observer(&self) -> Arc<dyn RuntimeCanonicalEventObserver> {
        Arc::new(AppCanonicalEventBehaviorObserver {
            api: self.clone(),
            run_ref: AppReference::parse(format!("run:app-event-router:{}", uuid::Uuid::new_v4()))
                .expect("server-generated app event-router run reference is valid"),
        })
    }

    /// Start the single durable app projection owner. The worker is explicitly
    /// wired by the binary after runtime construction; request handlers never
    /// spawn competing consumers or perform scope-wide repair.
    pub fn spawn_projection_worker(&self) -> Option<tokio::task::JoinHandle<()>> {
        if self
            .projection_worker_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        let worker = AppProjectionWorker {
            workspace: self.workspace.clone(),
            registry: self.registry.clone(),
            memory_contributions: self.memory_contributions.clone(),
            personal_agent_retrieval: self.personal_agent_retrieval.clone(),
            entity_changes: self.entity_changes.clone(),
            app_publications: self.app_publications.clone(),
            custom_surfaces: Arc::clone(&self.custom_surfaces),
            scripted_surfaces: Arc::clone(&self.scripted_surfaces),
            computed_capabilities: Arc::clone(&self.computed_capabilities),
            widget_runtime: self.widget_runtime.clone(),
            scoped_capabilities: Arc::clone(&self.scoped_capabilities),
            event_broadcaster: self.event_broadcaster.clone(),
            run_ref: AppReference::parse(format!("run:app-projection:{}", uuid::Uuid::new_v4()))
                .expect("server-generated app projection run reference is valid"),
        };
        Some(tokio::spawn(worker.run()))
    }

    /// Start the one process-owned app-debt supervisor. Its independent lanes
    /// keep owner-notification delivery and terminal retention owned even when
    /// the boot master disables schedule/event admission, and bounded restart
    /// backoff recovers an unexpected worker return or panic.
    pub fn spawn_background_behavior_worker(
        &self,
        resources: Arc<AgentResources>,
        user_requests: Arc<UserRequestService>,
        settings: AppBackgroundBehaviorSettings,
    ) -> Option<tokio::task::JoinHandle<()>> {
        if self
            .background_behavior_worker_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        // A replacement supervisor must not inherit the prior boot master's
        // scheduler handle during construction. The observer keys admission
        // from this exact slot plus the ownership latch, so clear both runtime
        // surfaces before any fallible setup and publish the new value only
        // after every enabled component is ready.
        self.background_behavior_event_admission.close_and_drain();
        self.workflow_service()
            .set_background_behavior_runtime_admission(false);
        *self
            .background_behaviors
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let scheduler = if settings.enabled {
            match AppBehaviorScheduler::new(
                self.registry.clone(),
                self.stager.clone(),
                AppBehaviorRuntimeLimits {
                    max_installations_per_scope: settings.max_installations_per_scope,
                    max_claims_per_scope_tick: settings.max_claims_per_scope_tick,
                    lease_seconds: settings.lease_seconds,
                    retry_seconds: settings.retry_seconds,
                },
            ) {
                Ok(scheduler) => Some(scheduler),
                Err(error) => {
                    self.background_behavior_worker_started
                        .store(false, Ordering::Release);
                    tracing::error!(error = %error, "app background-behavior scheduler is unavailable");
                    return None;
                },
            }
        } else {
            None
        };
        let event_behaviors = if settings.enabled {
            match AppEventBehaviorService::new(
                self.registry.clone(),
                self.stager.clone(),
                AppEventBehaviorRuntimeLimits {
                    max_claims_per_scope_tick: settings.max_claims_per_scope_tick.min(32),
                    lease_seconds: settings.lease_seconds,
                    retry_seconds: settings.retry_seconds,
                },
            ) {
                Ok(service) => service,
                Err(error) => {
                    self.background_behavior_worker_started
                        .store(false, Ordering::Release);
                    tracing::error!(error = %error, "app event-behavior router is unavailable");
                    return None;
                },
            }
        } else {
            self.event_behaviors
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        };
        *self
            .event_behaviors
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = event_behaviors.clone();
        *self
            .background_behaviors
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = scheduler.clone();
        self.workflow_service()
            .set_background_behavior_runtime_admission(scheduler.is_some());
        if scheduler.is_some() {
            self.background_behavior_event_admission.open();
        }
        let worker = AppBackgroundBehaviorWorker {
            api: self.clone(),
            scheduler,
            event_behaviors,
            owner_notifications: AppOwnerNotificationService::new(self.registry.clone()),
            user_requests,
            workspace: self.workspace.clone(),
            resources,
            notification_claim_limit: u16::try_from(
                settings.max_claims_per_scope_tick.clamp(1, 32),
            )
            .expect("notification claim ceiling fits u16"),
            notification_lease_seconds: settings.lease_seconds.clamp(5, 300),
            tick_interval: StdDuration::from_secs(settings.tick_interval_seconds.max(1)),
            // A timed-out scope leaves only durable leases, which the next
            // pass reclaims. Keeping this below half the configured lease
            // prevents one unhealthy scope from consuming the whole sweep.
            scope_timeout: StdDuration::from_secs((settings.lease_seconds / 2).clamp(1, 30)),
            run_ref: AppReference::parse(format!(
                "run:app-background-behavior:{}",
                uuid::Uuid::new_v4()
            ))
            .expect("server-generated app background-behavior run reference is valid"),
        };
        let api = self.clone();
        // Construct ownership outside the future. If the returned task is
        // aborted before its first poll (or spawn unwinds), dropping the
        // unpolled future must still close admission and release the latch.
        let owned = AppBackgroundBehaviorSupervisorGuard {
            started: Arc::clone(&api.background_behavior_worker_started),
            event_admission: api.background_behavior_event_admission.clone(),
            scheduler: Arc::clone(&api.background_behaviors),
            workflow_service: api.workflow_service(),
        };
        Some(tokio::spawn(async move {
            // The outer guard is process ownership/admission, distinct from
            // the per-attempt running bit exposed in health. Aborting this
            // supervisor releases ownership; a caught worker panic never does.
            let _owned = owned;
            let mut consecutive_failures = 0u32;
            loop {
                // Startup cancellation ends the owner; it is not a failed
                // worker attempt that should be marked unhealthy or restarted.
                if !magician::magician_v2::runtime::startup::wait_for_http().await {
                    return;
                }
                let attempt_started = tokio::time::Instant::now();
                let outcome = {
                    // Marked on the scheduler the worker actually drives, so
                    // every reader of its `health()` sees the same bit. With
                    // schedule admission off there is no scheduler and nothing
                    // to report through, which is the honest answer.
                    let _running = worker
                        .scheduler
                        .as_ref()
                        .map(AppBehaviorScheduler::mark_worker_running);
                    AssertUnwindSafe(worker.clone().run()).catch_unwind().await
                };
                if !magician::magician_v2::runtime::startup::wait_for_http().await {
                    return;
                }
                if attempt_started.elapsed() >= APP_BACKGROUND_BEHAVIOR_RESTART_RESET_AFTER {
                    consecutive_failures = 0;
                }
                let restart_delay = app_background_behavior_restart_delay(consecutive_failures);
                consecutive_failures = consecutive_failures.saturating_add(1);
                match outcome {
                    Ok(()) => tracing::error!(
                        restart_delay_ms = restart_delay.as_millis(),
                        "app background-behavior worker exited unexpectedly; supervisor will restart it"
                    ),
                    Err(_) => tracing::error!(
                        restart_delay_ms = restart_delay.as_millis(),
                        "app background-behavior worker panicked; supervisor will restart it"
                    ),
                }
                tokio::time::sleep(restart_delay).await;
            }
        }))
    }

    fn background_behavior_scheduler(&self) -> Option<AppBehaviorScheduler> {
        self.background_behaviors
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

const APP_MACOS_PAIRING_CONTROL_BODY_BYTES: usize = 8 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppBackgroundBehaviorHealthQuery {
    limit: Option<usize>,
    before_updated_at: Option<String>,
    before_installation_id: Option<String>,
    before_behavior_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppBackgroundBehaviorPolicyRequest {
    expected_revision: u64,
    paused: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppBackgroundBehaviorRetryRequest {
    expected_installation_generation: u64,
    expected_revision: u64,
}

async fn retry_app_background_behavior_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<(AppInstallationId, AppName)>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let request = match read_bounded_raw_body(
        &req,
        &mut payload,
        "application/json",
        APP_BACKGROUND_BEHAVIOR_CONTROL_BODY_BYTES,
        "app_background_behavior_retry",
    )
    .await
    .and_then(|bytes| {
        serde_json::from_slice::<AppBackgroundBehaviorRetryRequest>(&bytes).map_err(|_| {
            api_error(
                StatusCode::BAD_REQUEST,
                "app_background_behavior_retry_invalid",
                "The background-behavior retry request is invalid.",
            )
        })
    }) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let Some(scheduler) = api.background_behavior_scheduler() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "app_background_behaviors_disabled",
            "App background behaviors are disabled for this process.",
        );
    };
    let (installation_id, behavior_id) = path.into_inner();
    match scheduler
        .retry_blocked_launch(
            &authenticated,
            installation_id,
            behavior_id,
            request.expected_installation_generation,
            request.expected_revision,
            now,
        )
        .await
    {
        Ok(revision) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(serde_json::json!({"status":"retry_scheduled", "revision":revision})),
        Err(error) => app_background_behavior_error_response(error),
    }
}

/// `GET /api/magician/v2/apps/background-behaviors`
async fn get_app_background_behaviors_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    query: web::Query<AppBackgroundBehaviorHealthQuery>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let Some(scheduler) = api.background_behavior_scheduler() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "app_background_behaviors_disabled",
            "App background behaviors are disabled for this process.",
        );
    };
    let limit = query.limit.unwrap_or(APP_BEHAVIOR_MAX_HEALTH_ITEMS);
    let cursor = match (
        query.before_updated_at.clone(),
        query.before_installation_id.clone(),
        query.before_behavior_id.clone(),
    ) {
        (None, None, None) => None,
        (Some(updated_at), Some(installation_id), Some(behavior_id)) => {
            Some(AppBehaviorHealthCursor {
                updated_at,
                installation_id,
                behavior_id,
            })
        },
        _ => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "app_background_behavior_cursor_invalid",
                "The complete background-behavior health cursor is required.",
            );
        },
    };
    match scheduler.health(&authenticated, limit, cursor, now).await {
        Ok(snapshot) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(snapshot),
        Err(error) => app_background_behavior_error_response(error),
    }
}

/// `PUT /api/magician/v2/apps/background-behaviors/policy`
async fn put_app_background_behavior_policy_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let request = match read_bounded_raw_body(
        &req,
        &mut payload,
        "application/json",
        APP_BACKGROUND_BEHAVIOR_CONTROL_BODY_BYTES,
        "app_background_behavior_policy",
    )
    .await
    .and_then(|bytes| {
        serde_json::from_slice::<AppBackgroundBehaviorPolicyRequest>(&bytes).map_err(|_| {
            api_error(
                StatusCode::BAD_REQUEST,
                "app_background_behavior_policy_invalid",
                "The app background-behavior policy request is invalid.",
            )
        })
    }) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let Some(scheduler) = api.background_behavior_scheduler() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "app_background_behaviors_disabled",
            "App background behaviors are disabled for this process.",
        );
    };
    match scheduler
        .set_scope_paused(
            &authenticated,
            request.expected_revision,
            request.paused,
            now,
        )
        .await
    {
        Ok(policy) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(policy),
        Err(error) => app_background_behavior_error_response(error),
    }
}

fn app_background_behavior_error_response(error: AppBehaviorSchedulerError) -> HttpResponse {
    let (status, code, message) = match error {
        AppBehaviorSchedulerError::InvalidRuntimePolicy => (
            StatusCode::BAD_REQUEST,
            "app_background_behavior_policy_invalid",
            "The app background-behavior request is outside the supported bounds.",
        ),
        AppBehaviorSchedulerError::LeaseLost => (
            StatusCode::CONFLICT,
            "app_background_behavior_revision_conflict",
            "The app background-behavior policy or lease changed concurrently.",
        ),
        AppBehaviorSchedulerError::InventoryCapacityExceeded => (
            StatusCode::TOO_MANY_REQUESTS,
            "app_background_behavior_inventory_limited",
            "The scoped app background-behavior inventory exceeds its process ceiling.",
        ),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "app_background_behavior_unavailable",
            "The app background-behavior scheduler is temporarily unavailable.",
        ),
    };
    api_error(status, code, message)
}

/// `GET /api/magician/v2/apps/macos-pairing`
async fn get_app_macos_pairing_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let Some(owner) = api.macos_pairing.as_ref() else {
        return macos_pairing_error_response(AppMacosPairingControlError::DesktopUnavailable);
    };
    match owner.status(&authenticated).await {
        Ok(status) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(status),
        Err(error) => macos_pairing_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/macos-pairing/setup`
async fn begin_app_macos_pairing_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let request = match read_bounded_raw_body(
        &req,
        &mut payload,
        "application/json",
        APP_MACOS_PAIRING_CONTROL_BODY_BYTES,
        "app_macos_pairing",
    )
    .await
    .and_then(|bytes| {
        serde_json::from_slice::<AppMacosPairingSetupRequest>(&bytes).map_err(|_| {
            api_error(
                StatusCode::BAD_REQUEST,
                "app_macos_pairing_request_invalid",
                "The macOS pairing setup request is invalid.",
            )
        })
    }) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let Some(owner) = api.macos_pairing.as_ref() else {
        return macos_pairing_error_response(AppMacosPairingControlError::DesktopUnavailable);
    };
    match owner.begin_setup(&authenticated, request, now).await {
        Ok(status) => HttpResponse::Accepted()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(status),
        Err(error) => macos_pairing_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/macos-pairing/advance`
async fn advance_app_macos_pairing_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let Some(owner) = api.macos_pairing.as_ref() else {
        return macos_pairing_error_response(AppMacosPairingControlError::DesktopUnavailable);
    };
    match owner.advance(&authenticated, now).await {
        Ok(status) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(status),
        Err(error) => macos_pairing_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/macos-pairing/revoke`
async fn revoke_app_macos_pairing_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let request = match read_bounded_raw_body(
        &req,
        &mut payload,
        "application/json",
        APP_MACOS_PAIRING_CONTROL_BODY_BYTES,
        "app_macos_pairing",
    )
    .await
    .and_then(|bytes| {
        serde_json::from_slice::<AppMacosPairingRevokeRequest>(&bytes).map_err(|_| {
            api_error(
                StatusCode::BAD_REQUEST,
                "app_macos_pairing_request_invalid",
                "The macOS pairing revoke request is invalid.",
            )
        })
    }) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let Some(owner) = api.macos_pairing.as_ref() else {
        return macos_pairing_error_response(AppMacosPairingControlError::DesktopUnavailable);
    };
    match owner.revoke(&authenticated, request, now).await {
        Ok(status) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(status),
        Err(error) => macos_pairing_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/macos-pairing/reset/challenge`
async fn begin_app_macos_pairing_reset_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let Some(owner) = api.macos_pairing.as_ref() else {
        return macos_pairing_error_response(AppMacosPairingControlError::DesktopUnavailable);
    };
    match owner.reset_challenge(&authenticated, now).await {
        Ok(challenge) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(challenge),
        Err(error) => macos_pairing_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/macos-pairing/reset`
async fn complete_app_macos_pairing_reset_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let acknowledgment = match read_bounded_raw_body(
        &req,
        &mut payload,
        "application/json",
        APP_MACOS_PAIRING_CONTROL_BODY_BYTES,
        "app_macos_pairing_reset",
    )
    .await
    .and_then(|bytes| {
        serde_json::from_slice::<AppMacosHostPairingResetAck>(&bytes).map_err(|_| {
            api_error(
                StatusCode::BAD_REQUEST,
                "app_macos_pairing_reset_invalid",
                "The signed macOS pairing reset acknowledgment is invalid.",
            )
        })
    }) {
        Ok(acknowledgment) => acknowledgment,
        Err(response) => return response,
    };
    let Some(owner) = api.macos_pairing.as_ref() else {
        return macos_pairing_error_response(AppMacosPairingControlError::DesktopUnavailable);
    };
    match owner.reset(&authenticated, acknowledgment, now).await {
        Ok(status) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(status),
        Err(error) => macos_pairing_error_response(error),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppMemoryOwnerReviewQuery {
    #[serde(default = "default_app_memory_owner_review_limit")]
    limit: usize,
}

fn default_app_memory_owner_review_limit() -> usize {
    8
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct AppMemoryOwnerReviewListResponse {
    reviews: Vec<AppMemoryOwnerReviewV1>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppMemoryContributionStateQuery {
    #[serde(default = "default_app_memory_contribution_state_limit")]
    limit: usize,
}

fn default_app_memory_contribution_state_limit() -> usize {
    16
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct AppMemoryContributionStateResponse {
    memory: AppMemoryContributionStateSnapshotV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retrieval: Option<PersonalAgentRetrievalInspectionSnapshotV1>,
    retrieval_available: bool,
}

fn interactive_owner_scope(authenticated: &AuthenticatedAppScope) -> bool {
    matches!(
        authenticated.authentication(),
        AppScopeAuthentication::AuthenticatedSession
            | AppScopeAuthentication::TrustedLoopbackSingleUser
    )
}

/// `GET /api/magician/v2/apps/memory-contributions/owner-reviews`
async fn list_app_memory_owner_reviews_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    query: web::Query<AppMemoryOwnerReviewQuery>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if !interactive_owner_scope(&authenticated) {
        return api_error(
            StatusCode::FORBIDDEN,
            "app_memory_owner_review_requires_interactive_session",
            "App-memory owner review requires an interactive user session.",
        );
    }
    if query.limit == 0 || query.limit > APP_MEMORY_OWNER_REVIEW_MAX_ITEMS {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_app_memory_owner_review_limit",
            "The app-memory owner review limit must be between 1 and 16.",
        );
    }
    let Some(pairing) = api.macos_pairing.as_ref() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "app_memory_desktop_owner_unavailable",
            "The code-verified desktop owner is unavailable.",
        );
    };
    let identity = match pairing.active_desktop_identity(&authenticated).await {
        Ok(Some(identity)) => identity,
        Ok(None) => {
            return api_error(
                StatusCode::CONFLICT,
                "app_memory_desktop_owner_not_paired",
                "Pair and finalize the code-verified desktop owner before reviewing app memory.",
            );
        },
        Err(error) => return macos_pairing_error_response(error),
    };
    match api
        .memory_contributions
        .pending_owner_reviews(
            &authenticated,
            &identity.desktop_identity_key_id,
            &identity.desktop_identity_digest,
            query.limit,
            now,
        )
        .await
    {
        Ok(reviews) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(AppMemoryOwnerReviewListResponse { reviews }),
        Err(error) => app_memory_contribution_projection_error_response(error),
    }
}

/// `GET /api/magician/v2/apps/memory-contributions/state`
async fn list_app_memory_contribution_state_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    query: web::Query<AppMemoryContributionStateQuery>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if !interactive_owner_scope(&authenticated) {
        return api_error(
            StatusCode::FORBIDDEN,
            "app_memory_state_requires_interactive_session",
            "App-memory state inspection requires an interactive user session.",
        );
    }
    if query.limit == 0 || query.limit > APP_MEMORY_CONTRIBUTION_STATE_MAX_ITEMS {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_app_memory_state_limit",
            "The app-memory state limit must be between 1 and 32.",
        );
    }
    let desktop_identity = match authenticated.authentication() {
        AppScopeAuthentication::TrustedLoopbackSingleUser => match api.macos_pairing.as_ref() {
            Some(pairing) => match pairing.active_desktop_identity(&authenticated).await {
                Ok(Some(identity)) => Some(identity),
                Ok(None) => None,
                Err(error) => return macos_pairing_error_response(error),
            },
            None => None,
        },
        _ => None,
    };
    let memory = match api
        .memory_contributions
        .contribution_state(
            &authenticated,
            desktop_identity.as_ref().map(|identity| {
                (
                    identity.desktop_identity_key_id.as_str(),
                    identity.desktop_identity_digest.as_str(),
                )
            }),
            query.limit,
        )
        .await
    {
        Ok(snapshot) => snapshot,
        Err(error) => return app_memory_contribution_projection_error_response(error),
    };
    let retrieval = match api.personal_agent_retrieval.as_ref() {
        Some(service) => match service
            .contribution_state(&authenticated, query.limit)
            .await
        {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                tracing::warn!(error = %error, "personal-agent retrieval state inspection failed");
                return api_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "app_retrieval_state_unavailable",
                    "The personal-agent retrieval state owner is unavailable.",
                );
            },
        },
        None => None,
    };
    HttpResponse::Ok()
        .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
        .json(AppMemoryContributionStateResponse {
            memory,
            retrieval_available: retrieval.is_some(),
            retrieval,
        })
}

/// `POST /api/magician/v2/apps/memory-contributions/owner-decisions`
async fn apply_app_memory_owner_decision_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    body: web::Json<AppMemoryOwnerDecisionEnvelopeV1>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if !interactive_owner_scope(&authenticated) {
        return api_error(
            StatusCode::FORBIDDEN,
            "app_memory_owner_decision_requires_interactive_session",
            "App-memory owner decisions require an interactive user session.",
        );
    }
    let Some(pairing) = api.macos_pairing.as_ref() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "app_memory_desktop_owner_unavailable",
            "The code-verified desktop owner is unavailable.",
        );
    };
    let identity = match pairing.active_desktop_identity(&authenticated).await {
        Ok(Some(identity)) => identity,
        Ok(None) => {
            return api_error(
                StatusCode::CONFLICT,
                "app_memory_desktop_owner_not_paired",
                "Pair and finalize the code-verified desktop owner before deciding app memory.",
            );
        },
        Err(error) => return macos_pairing_error_response(error),
    };
    match api
        .memory_contributions
        .apply_owner_decision(
            &authenticated,
            &identity.desktop_identity_public_key_hex,
            &identity.desktop_identity_key_id,
            &identity.desktop_identity_digest,
            body.into_inner(),
            now,
        )
        .await
    {
        Ok(receipt) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(receipt),
        Err(error) => app_memory_contribution_projection_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/claims-decisions/owner-decisions`
///
/// The desktop supplies a closed signed envelope; every mutable authority
/// input is resolved again from the authenticated route immediately before the
/// destination CAS transition.
async fn apply_app_claims_owner_decision_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AppClaimsDecisionOwnerDecisionEnvelopeV1>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if !interactive_owner_scope(&authenticated) {
        return api_error(
            StatusCode::FORBIDDEN,
            "app_claims_owner_decision_requires_interactive_session",
            "Claims decisions require an interactive user session.",
        );
    }
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let installation = match api
        .registry
        .installation(&authenticated, &installation_id, now)
        .await
    {
        Ok(Some(installation)) => installation,
        Ok(None) => {
            return api_error(
                StatusCode::NOT_FOUND,
                "app_installation_not_found",
                "The claims-decision installation does not exist in this authenticated scope.",
            )
        },
        Err(error) => return registry_error_response(error),
    };
    if installation.lifecycle.status != AppInstallationStatus::Enabled {
        return api_error(
            StatusCode::CONFLICT,
            "app_claims_installation_not_enabled",
            "The claims-decision installation is not enabled.",
        );
    }
    let Some(pairing) = api.macos_pairing.as_ref() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "app_claims_desktop_owner_unavailable",
            "The code-verified desktop owner is unavailable.",
        );
    };
    let envelope = body.into_inner();
    if let Err(error) = envelope.validate() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "app_claims_owner_decision_invalid",
            &error.to_string(),
        );
    }
    if let Err(error) = validate_app_claims_decision(&envelope.review.proposal) {
        return api_error(
            StatusCode::BAD_REQUEST,
            "app_claims_command_invalid",
            &error.to_string(),
        );
    }
    let source_header = &envelope.review.proposal.header;
    if source_header.installation_id != installation_id.to_string()
        || source_header.scope_binding_ref != authenticated.scope_binding_ref().as_str()
        || source_header.installation_generation != installation.lifecycle.generation
        || source_header.package_revision_ref != installation.package_revision_ref.to_string()
        || envelope.review.proposal.by != authenticated.actor_ref().as_str()
    {
        return api_error(
            StatusCode::CONFLICT,
            "app_claims_route_authority_mismatch",
            "The signed claims command does not match the route's authenticated scope, actor, installation, or generation.",
        );
    }
    let source_entity = match AppName::parse(source_header.source_entity_name.clone()) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let source_record_id = match AppRecordId::parse(source_header.source_record_id.clone()) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let source_record_revision = match AppRevision::new(source_header.source_record_revision) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let workflow_id = match AppName::parse(source_header.workflow_id.clone()) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let action_id = match AppName::parse(source_header.action_id.clone()) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };

    // Re-admit the immutable package named by the current installation and
    // resolve the exact workflow/action declarations. Friendly ids in the
    // signed command are not enough to survive an update or reinstall.
    let package_checked_at = Utc::now();
    let package = match api
        .registry
        .package_revision(
            &authenticated,
            &installation.package_revision_ref,
            package_checked_at,
        )
        .await
    {
        Ok(Some(package)) => package,
        Ok(None) => {
            return api_error(
                StatusCode::CONFLICT,
                "app_claims_package_authority_unavailable",
                "The claims-decision installation's immutable package revision is unavailable.",
            )
        },
        Err(error) => return registry_error_response(error),
    };
    let staged = match api
        .stager
        .load_staged_package(&authenticated, package.content_digest.clone(), Utc::now())
        .await
    {
        Ok(staged) => staged,
        Err(error) => return staging_error_response(error),
    };
    let manifest = staged.candidate().manifest().manifest();
    let action = match manifest.app.actions.get(&action_id) {
        Some(action) if action.workflow == workflow_id => action,
        _ => {
            return api_error(
                StatusCode::CONFLICT,
                "app_claims_action_authority_unavailable",
                "The signed claims command no longer names its exact current workflow action.",
            )
        },
    };
    let workflow = match manifest.app.workflows.get(&workflow_id) {
        Some(workflow) => workflow,
        None => {
            return api_error(
                StatusCode::CONFLICT,
                "app_claims_workflow_authority_unavailable",
                "The signed claims command no longer names a current workflow.",
            )
        },
    };
    let workflow_value = match serde_json::to_value(workflow) {
        Ok(value) => value,
        Err(error) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_claims_workflow_authority_invalid",
                &error.to_string(),
            )
        },
    };
    let workflow_digest = match AppDigest::blake3_canonical_json(&workflow_value) {
        Ok(value) => value,
        Err(error) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_claims_workflow_authority_invalid",
                &error.to_string(),
            )
        },
    };
    let action_value = match serde_json::to_value(action) {
        Ok(value) => value,
        Err(error) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_claims_action_authority_invalid",
                &error.to_string(),
            )
        },
    };
    let action_digest = match AppDigest::blake3_canonical_json(&action_value) {
        Ok(value) => value,
        Err(error) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_claims_action_authority_invalid",
                &error.to_string(),
            )
        },
    };

    // Pairing is independently mutable and lives outside the registry
    // snapshot. Reopen it after package work, then make the source/authority
    // snapshot below the final asynchronous registry operation.
    let identity =
        match pairing.active_desktop_identity(&authenticated).await {
            Ok(Some(identity)) => identity,
            Ok(None) => return api_error(
                StatusCode::CONFLICT,
                "app_claims_desktop_owner_not_paired",
                "The code-verified desktop owner pairing changed before claims-decision admission.",
            ),
            Err(error) => return macos_pairing_error_response(error),
        };

    // Make the source-record read the final asynchronous registry operation.
    // It reopens installation/grant/schema authority and the exact live,
    // undeleted review_decision head in one SQLite snapshot.
    let authority_checked_at = Utc::now();
    let authority_snapshot = match AppEntityStoreService::new(api.registry.clone())
        .runtime_contribution_source_snapshot(
            &authenticated,
            &installation_id,
            &source_entity,
            &source_record_id,
            source_record_revision,
            authority_checked_at,
        )
        .await
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return entity_adapter_error_response(AppEntityAdapterError::from(error));
        },
    };
    let (current_installation, active, source_record_digest, source_record_payload) =
        authority_snapshot.into_source_parts();
    let source_record_digest = match source_record_digest {
        Some(digest) => digest.to_string(),
        None => {
            return api_error(
                StatusCode::CONFLICT,
                "app_claims_source_authority_unavailable",
                "The signed claims command has no current source-record digest.",
            )
        },
    };
    let source_record_payload = match source_record_payload {
        Some(payload) => payload,
        None => {
            return api_error(
                StatusCode::CONFLICT,
                "app_claims_source_authority_unavailable",
                "The signed claims command has no current source-record payload.",
            )
        },
    };
    if current_installation.package_revision_ref != installation.package_revision_ref
        || active.package_revision_ref() != &installation.package_revision_ref
    {
        return api_error(
            StatusCode::CONFLICT,
            "app_claims_package_authority_changed",
            "The claims-decision package authority changed while the command was being admitted.",
        );
    }
    let current_authority = AppClaimsDecisionCurrentAuthority {
        destination_schema_digest: app_claims_decision_destination_schema_digest(),
        installation_generation: active.installation_generation(),
        package_revision_ref: active.package_revision_ref().to_string(),
        package_content_digest: package.content_digest.to_string(),
        grant_revision: active.grant_revision().get(),
        grant_authority_digest: active.grant().authority_digest.to_string(),
        schema_revision: active.schema_revision().get(),
        schema_digest: package.entity_schema_digest.to_string(),
        source_entity_name: source_entity.to_string(),
        source_record_id: source_record_id.to_string(),
        source_record_revision: source_record_revision.get(),
        source_record_digest,
        source_record_payload,
        workflow_id: workflow_id.to_string(),
        workflow_digest: workflow_digest.to_string(),
        action_id: action_id.to_string(),
        action_digest: action_digest.to_string(),
    };
    let ingestion = TranscriptIngestion::new(api.workspace.clone());
    let commitments = Commitments::new(api.workspace.clone());
    let installation_id_text = installation_id.to_string();
    let pairing_generation = identity.pairing_generation;
    let public_key = identity.desktop_identity_public_key_hex;
    let key_id = identity.desktop_identity_key_id;
    let identity_digest = identity.desktop_identity_digest;
    let schema_revision = active.schema_revision();
    // The destination applies on a blocking worker and takes the authenticated
    // scope with it. The publish below needs the same value: it names the ledger
    // to read and carries the owner's authority into the store.
    let publishing_scope = authenticated.clone();
    // Proven from the grant in the same snapshot that authorises the
    // transition below, and before the worker runs: the publish that follows
    // hands over the whole scope's decision ledger, so it may not ride a grant
    // resolved at handler entry and no longer current at the store fence.
    let publication_grant =
        AppClaimsReviewReceiptPublicationGrant::prove(&installation_id, active.grant());
    let applied = web::block(move || {
        // Registry/pairing I/O and blocking-worker admission may outlive the
        // timestamp sampled at handler entry. Re-sample at the mutation
        // boundary so an expired auth scope or signed review cannot cross the
        // destination CAS transition on that earlier timestamp.
        let decision_time = Utc::now();
        apply_signed_app_claims_decision(
            &envelope,
            &public_key,
            pairing_generation,
            &key_id,
            &identity_digest,
            &installation_id_text,
            &current_authority,
            &authenticated,
            &ingestion,
            &commitments,
            decision_time,
        )
    })
    .await;
    // The destination's receipts land in a host-owned ledger the app cannot
    // reach. This is the only path from that ledger into the package's declared
    // `review_receipt` entity, and without it the console's receipts view — the
    // owner's record that a signed decision really settled a claim — is empty
    // for the life of the installation.
    //
    // It runs on the refusal path too: a refusal journals nothing, so it
    // publishes nothing of its own, but a scope carrying rows an earlier call
    // could not publish heals on the cheapest call that reaches it. Only a
    // worker that never ran is skipped, because then no drain ran either.
    if applied.is_ok() {
        match &publication_grant {
            Some(grant) => {
                publish_app_claims_review_receipts(
                    api.get_ref(),
                    &publishing_scope,
                    grant,
                    schema_revision,
                    Utc::now(),
                )
                .await;
            },
            // The decision stands; only the ledger is withheld. Say so on every
            // decision rather than once, because the receipts view staying
            // empty is otherwise indistinguishable from a store that failed.
            None => tracing::warn!(
                installation_id = %installation_id,
                "the destination does not hold the owner-granted scope-wide evidence read the \
                 review receipt ledger belongs to; nothing is published and no cursor moves"
            ),
        }
    }
    match applied {
        Ok(Ok(outcome)) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(outcome),
        Ok(Err(error)) => api_error(
            StatusCode::CONFLICT,
            "app_claims_owner_decision_refused",
            &error.to_string(),
        ),
        Err(error) => api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "app_claims_owner_decision_worker_unavailable",
            &format!("The claims-decision store worker failed: {error}"),
        ),
    }
}

/// Rows one call will try to publish before handing the request back.
///
/// The steady state is one. A larger page matters when an earlier publish
/// failed partway, and when an installation is catching up on rows minted
/// before it existed — a reinstall starts at the head of nothing. Neither may
/// hold an owner's decision open until it is drained; the publication cursor
/// makes resuming free, so whatever is left rides the next decision into this
/// installation.
const APP_CLAIMS_REVIEW_RECEIPT_PUBLISH_PAGE: usize = 16;

/// The canonical grant a destination must hold before the scope's projected
/// review-receipt ledger may be published into it.
///
/// `evidence_data` is the owner-reviewed, read-only binder over the very
/// registers this ledger describes, and the only capability that admits a
/// package to them scope-wide. An installation that does not hold it was given
/// no view of the register at all, and a projected receipt is a view of the
/// register. Not a subset one: the binder deliberately withholds decision
/// provenance, so a receipt says more about a decision than the granted read
/// does — which is the reason the fence has to be the grant the owner reviewed
/// rather than the shape of an entity the package named itself.
fn app_review_receipt_publication_capability() -> Option<AppReference> {
    canonical_app_tool_ref(EVIDENCE_DATA_TOOL_NAME).ok()
}

/// Whether an owner's grant carries the scope-wide evidence read the
/// review-receipt ledger belongs to.
///
/// The required reference is canonicalised and the granted ones are compared
/// as stored — the same direction `declared_workflow_is_grant_eligible` uses,
/// because approval canonicalises every reference it records. Reading it the
/// other way would let an uncanonical entry in a grant match this fence.
fn grant_carries_scope_wide_evidence_read(granted_tools: &[AppReference]) -> bool {
    app_review_receipt_publication_capability()
        .is_some_and(|required| granted_tools.contains(&required))
}

/// Proof that one installation may be handed the SCOPE's projected
/// review-receipt ledger.
///
/// The ledger is scope-wide by construction — the registers are per-scope and
/// the completion journal carries no originating installation — so a publish
/// offers the destination every receipted claims and commitment decision in the
/// scope, including ones taken through other installations. The store fence
/// the publish already crosses cannot bound that:
/// `authorize_app_owner_store_mutation` checks the contract, the expected
/// schema revision and the idempotency key, never which entity the owner may
/// write, so without this type the effective admission is "the package
/// declares a `review_receipt` entity" — a name any package may choose for
/// itself.
///
/// So the fence is the grant the owner actually reviewed for this package. It
/// is minted from the grant resolved in the same snapshot that authorises the
/// destination transition, and it carries the installation it was proven for,
/// so neither the check nor the destination can drift from the other at the
/// call site.
#[derive(Debug)]
struct AppClaimsReviewReceiptPublicationGrant {
    installation_id: AppInstallationId,
}

impl AppClaimsReviewReceiptPublicationGrant {
    /// Fails closed. A grant bound to a different installation, or one the
    /// owner narrowed to exclude the register read, publishes nothing rather
    /// than falling through to the entity declaration the store happens to
    /// accept.
    fn prove(installation_id: &AppInstallationId, grant: &AppGrantRevision) -> Option<Self> {
        if grant.installation_id != *installation_id
            || !grant_carries_scope_wide_evidence_read(&grant.granted_tools)
        {
            return None;
        }
        Some(Self {
            installation_id: installation_id.clone(),
        })
    }

    fn installation_id(&self) -> &AppInstallationId {
        &self.installation_id
    }
}

/// Publish the projected `review_receipt` rows this installation has not been
/// given yet into the claims-review package's declared entity.
///
/// It reads the projector's LEDGER, never the destination's return value. A row
/// exists only for a decision a register receipted and then journalled, so a
/// refused or still-retryable decision publishes nothing and an app can never
/// see a row claiming a settlement the destination did not record.
///
/// The rows are written on the OWNER's authenticated authority. `review_receipt`
/// appears in no workflow's `may_mutate`, so the package still cannot write its
/// own receipts; this route can, because the owner is the one who signed the
/// decision each row describes.
///
/// Best-effort, exactly as the projection it publishes is: the decision already
/// applied, and failing the owner's request because a view could not be
/// refreshed would invert the risk this record manages. The cursor is what
/// makes that safe — it advances only over rows the store actually accepted, so
/// a failure here republishes rather than loses.
///
/// The cursor is the ROUTE'S INSTALLATION'S, not the scope's. The ledger is
/// scope-wide because the registers are, but a publication lands in exactly one
/// entity store, and an installation is a store with its own lifetime. One
/// scope-wide cursor made a publish into this installation claim delivery into
/// every other installation in the scope — rows they never received and,
/// because the cursor never rewinds, never would.
///
/// The rows a destination catches up on are the SCOPE's, not the ones this
/// installation happened to sign: the completion journal records no originating
/// installation, and a receipts view that stopped at the installation boundary
/// would go blank on reinstall while the settlements it describes stayed in the
/// register. That reach is why the destination arrives as a proven
/// [`AppClaimsReviewReceiptPublicationGrant`] rather than an installation id —
/// the grant bounds *who* may be handed the ledger, which is the only half of
/// it a caller here can bound at all.
async fn publish_app_claims_review_receipts(
    api: &AppPlatformApi,
    authenticated: &AuthenticatedAppScope,
    grant: &AppClaimsReviewReceiptPublicationGrant,
    schema_revision: AppRevision,
    now: DateTime<Utc>,
) {
    // Never the route's path parameter: the id this publishes into is the one
    // the grant above was proven for, so a proof minted for one destination
    // cannot be spent on another.
    let installation_id = grant.installation_id();
    // The scope comes from the authenticated value, never from the envelope: a
    // signed command names an installation, and an installation is not a claim
    // about whose ledger to read.
    let scope = authenticated.scope();
    let projection_scope =
        EvidenceDecisionScope::new(scope.principal.as_str(), scope.workspace.as_str());
    // The destination store, on the other hand, IS the proven installation —
    // it is the store `owner_mutate` below writes into, so it is the only thing
    // a recorded cursor may be understood to speak for.
    let publisher = match ReviewReceiptPublisher::installation(&installation_id.to_string()) {
        Ok(publisher) => publisher,
        Err(error) => {
            tracing::warn!(
                error = %error,
                installation_id = %installation_id,
                "the review receipt publication destination could not be named; nothing is \
                 published and no cursor moves"
            );
            return;
        },
    };
    let projector = ReviewReceiptProjector::new(api.workspace.clone());
    // The ledger is files and a per-destination advisory lock, so both touches
    // go to a blocking worker rather than parking an executor thread behind
    // whichever decision into this installation holds the lock.
    let read = {
        let projector = projector.clone();
        let projection_scope = projection_scope.clone();
        let publisher = publisher.clone();
        web::block(move || {
            projector.pending_publications(
                &projection_scope,
                &publisher,
                APP_CLAIMS_REVIEW_RECEIPT_PUBLISH_PAGE,
            )
        })
        .await
    };
    let pending = match read {
        Ok(Ok(page)) => page,
        Ok(Err(error)) => {
            tracing::warn!(
                error = %error,
                installation_id = %installation_id,
                "the review receipt ledger could not be read; the decision is applied and the \
                 next decision republishes it"
            );
            return;
        },
        Err(error) => {
            tracing::warn!(
                error = %error,
                installation_id = %installation_id,
                "the review receipt ledger worker was unavailable"
            );
            return;
        },
    };

    let mut published_through = None;
    for entry in &pending.pending {
        let Some(command) = app_claims_review_receipt_mutation(&entry.row, schema_revision) else {
            // A row this build cannot express as a mutation is a defect, not a
            // transient fault. Stopping leaves the cursor behind it, so a fixed
            // build publishes it; advancing would drop it for good.
            tracing::warn!(
                decision_id = %entry.row.decision_id,
                installation_id = %installation_id,
                "a projected review receipt could not be expressed as a store mutation"
            );
            break;
        };
        match api
            .entity_adapter
            .owner_mutate(authenticated, installation_id, command, now)
            .await
        {
            // The row is already there. One decision, one row: a healed journal
            // entry replays its completion at a fresh seq, so a collision here
            // is the ordinary case rather than a failure.
            Ok(_)
            | Err(AppEntityAdapterError::Mutation(AppEntityMutationError::RecordAlreadyExists)) => {
                published_through = Some(entry.cursor_after)
            },
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    decision_id = %entry.row.decision_id,
                    installation_id = %installation_id,
                    "a review receipt could not be published into the package receipts view"
                );
                break;
            },
        }
    }

    // Only over rows the store took. A cursor that ran past a failure would
    // call those rows published and never offer them again.
    let Some(cursor) = published_through else {
        return;
    };
    let recorded =
        web::block(move || projector.record_published(&projection_scope, &publisher, cursor)).await;
    match recorded {
        Ok(Ok(())) => {},
        Ok(Err(error)) => tracing::warn!(
            error = %error,
            installation_id = %installation_id,
            "the review receipt publication cursor could not be advanced; the rows are published \
             and the next decision republishes them idempotently"
        ),
        Err(error) => tracing::warn!(
            error = %error,
            installation_id = %installation_id,
            "the review receipt publication cursor worker was unavailable"
        ),
    }
}

/// The owner mutation that publishes one projected receipt row.
///
/// The record id is named rather than host-minted so the row's identity IS the
/// decision's identity in its register: a second publish of one decision
/// collides and is refused, instead of adding a second row describing the same
/// settlement.
fn app_claims_review_receipt_mutation(
    receipt: &ProjectedReviewReceipt,
    schema_revision: AppRevision,
) -> Option<AppMutationCommand> {
    let record_id = receipt.package_record_id();
    Some(AppMutationCommand {
        protocol_version: AppProtocolVersion::V1,
        idempotency_key: AppReference::parse(format!("claims-review-receipt:{record_id}")).ok()?,
        atomicity: AppMutationAtomicity::AllOrNothing,
        expected_schema_revision: schema_revision,
        operations: vec![AppMutationOperation::Create {
            entity: AppName::parse(REVIEW_RECEIPT_ENTITY).ok()?,
            temporary_id: AppName::parse(REVIEW_RECEIPT_ENTITY).ok()?,
            record_id: Some(AppRecordId::parse(record_id.as_str()).ok()?),
            // The destination owns this shape: the row is the projected
            // receipt, field for field, so a publisher here cannot quietly
            // reword what the register decided.
            payload: serde_json::to_value(receipt).ok()?,
        }],
        expected_record_revisions: Vec::new(),
    })
}

/// One staged ingest, as the owner's client applies it.
///
/// The staged row itself is deliberately absent: it is read from the store
/// under a live authority snapshot, never accepted from the body. What is here
/// is exactly what the package may not say — which named people are ours,
/// which identity we spoke as, what the room cost, and when it happened.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppStagedIngestApplyRequest {
    /// The `ingest_request` row to apply, and the revision the caller read it
    /// at. A row that moved since is refused rather than applied from a head
    /// the owner never saw.
    record_id: String,
    record_revision: u64,
    /// Our side's identity in the room. The roster must place it on our side,
    /// which the register checks.
    effective_speaker: String,
    /// Host identity resolution, split by side. Every named person in the
    /// staged mapping must appear in one of these or the apply refuses: the
    /// document says who spoke, and only the owner says which side they were
    /// on.
    #[serde(default)]
    ours: Vec<String>,
    #[serde(default)]
    counterparty: Vec<String>,
    /// When the words were said, which is neither when they were staged nor
    /// when they are being applied.
    occurred_at: DateTime<Utc>,
    /// Defaults to bounded communication — words said to people already in the
    /// room. A caller that knows the room was more raises it.
    #[serde(default)]
    consequence_class: Option<String>,
}

/// The consequence class an owner named, mapped closed.
///
/// No permissive default and no `private_local`: an unrecognised word read as
/// the mildest class would put an outward act below the review threshold it
/// belonged above, and the words in a staged room already reached somebody.
fn staged_ingest_consequence_class(named: Option<&str>) -> Result<ConsequenceClass, &'static str> {
    let Some(named) = named else {
        return Ok(ConsequenceClass::BoundedCommunication);
    };
    match named.trim().to_ascii_lowercase().as_str() {
        "bounded_communication" => Ok(ConsequenceClass::BoundedCommunication),
        "confidential_disclosure" => Ok(ConsequenceClass::ConfidentialDisclosure),
        "submission_or_publication" => Ok(ConsequenceClass::SubmissionOrPublication),
        "commitment_or_transaction" => Ok(ConsequenceClass::CommitmentOrTransaction),
        _ => Err(
            "the consequence class must be one of bounded_communication, \
             confidential_disclosure, submission_or_publication or commitment_or_transaction",
        ),
    }
}

/// Build the host roster from the two owner-supplied lists.
///
/// A name on both lists is refused by the roster itself rather than resolved
/// by order, and an empty `ours` is refused here: our side's words are the only
/// ones that can become a claim, so a roster with nobody on our side is an
/// apply that can only produce an act with an empty review queue while looking
/// like it worked.
fn staged_ingest_roster(
    request: &AppStagedIngestApplyRequest,
) -> Result<StagedIngestRoster, String> {
    if request.ours.is_empty() {
        return Err(
            "a staged ingest names at least one identity of ours: only our side's words become \
             a claim somebody is asked about"
                .to_string(),
        );
    }
    let named = request
        .ours
        .len()
        .saturating_add(request.counterparty.len());
    if named > MAX_STAGED_INGEST_SPEAKERS {
        return Err(format!(
            "a host roster places {named} people; the reviewed ceiling is \
             {MAX_STAGED_INGEST_SPEAKERS}"
        ));
    }
    let mut roster = StagedIngestRoster::new();
    for identity in &request.ours {
        roster = roster
            .resolved_ours(identity.clone())
            .map_err(|error| format!("{error:#}"))?;
    }
    for identity in &request.counterparty {
        roster = roster
            .resolved_counterparty(identity.clone())
            .map_err(|error| format!("{error:#}"))?;
    }
    Ok(roster)
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/claims-ingests/apply`
///
/// The host half of the claims-review package's `stage_ingest` workflow. The
/// package writes an `ingest_request` row and can do nothing else with it: the
/// canonical ingest entry is host-owned, and the sides of the room — the one
/// input deciding whose words become an outward assertion in our name — are
/// resolved here from the owner's session rather than read off the row.
///
/// It carries no desktop signature, unlike its claims-decision sibling, and
/// that is deliberate: recording that a room happened is what
/// `POST /transcripts/ingest` already does on an authenticated session with
/// caller-supplied attribution, while settling a claim is what needs the
/// paired desktop. Requiring more here than the canonical entry requires would
/// close no door.
///
/// # Two stores, and why a failed stamp is not a failed apply
///
/// The act lands in the host register and the `applied` state lands in the
/// package's row; they cannot be one write. The apply is idempotent under one
/// `ingest_id` — same key, same act, same claim ids — so a stamp that does not
/// land leaves the row at `recorded` and the same request retried resumes the
/// same act rather than queueing the room twice. Answering
/// `staged_row_stamped: false` is therefore the honest outcome; failing the
/// request would report an act that exists as one that does not.
async fn apply_app_staged_ingest_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AppStagedIngestApplyRequest>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if !interactive_owner_scope(&authenticated) {
        return api_error(
            StatusCode::FORBIDDEN,
            "app_staged_ingest_requires_interactive_session",
            "Applying a staged ingest requires an interactive user session.",
        );
    }
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let request = body.into_inner();
    let installation = match api
        .registry
        .installation(&authenticated, &installation_id, now)
        .await
    {
        Ok(Some(installation)) => installation,
        Ok(None) => {
            return api_error(
                StatusCode::NOT_FOUND,
                "app_installation_not_found",
                "The staged-ingest installation does not exist in this authenticated scope.",
            )
        },
        Err(error) => return registry_error_response(error),
    };
    if installation.lifecycle.status != AppInstallationStatus::Enabled {
        return api_error(
            StatusCode::CONFLICT,
            "app_staged_ingest_installation_not_enabled",
            "The staged-ingest installation is not enabled.",
        );
    }
    let source_entity = match AppName::parse(STAGED_INGEST_REQUEST_ENTITY) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let source_record_id = match AppRecordId::parse(request.record_id.clone()) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let source_record_revision = match AppRevision::new(request.record_revision) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let roster = match staged_ingest_roster(&request) {
        Ok(roster) => roster,
        Err(message) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "app_staged_ingest_roster_invalid",
                &message,
            )
        },
    };
    let consequence_class =
        match staged_ingest_consequence_class(request.consequence_class.as_deref()) {
            Ok(class) => class,
            Err(message) => {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "app_staged_ingest_consequence_class_invalid",
                    message,
                )
            },
        };

    // The live head, read in one snapshot with the installation/grant/schema
    // authority that admits it. The row is the app's own writing, so nothing
    // downstream trusts it: this read only guarantees the bytes applied are the
    // bytes currently stored under the revision the owner reviewed.
    let authority_checked_at = Utc::now();
    let snapshot = match AppEntityStoreService::new(api.registry.clone())
        .runtime_contribution_source_snapshot(
            &authenticated,
            &installation_id,
            &source_entity,
            &source_record_id,
            source_record_revision,
            authority_checked_at,
        )
        .await
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return entity_adapter_error_response(AppEntityAdapterError::from(error));
        },
    };
    let (current_installation, active, _source_record_digest, source_record_payload) =
        snapshot.into_source_parts();
    if current_installation.package_revision_ref != installation.package_revision_ref {
        return api_error(
            StatusCode::CONFLICT,
            "app_staged_ingest_package_authority_changed",
            "The staged-ingest package authority changed while the request was being admitted.",
        );
    }
    let Some(staged_row) = source_record_payload else {
        return api_error(
            StatusCode::CONFLICT,
            "app_staged_ingest_source_unavailable",
            "The staged ingest request has no current source-record payload.",
        );
    };
    let schema_revision = active.schema_revision();

    let resolution = AppStagedIngestHostResolution {
        roster,
        effective_speaker: request.effective_speaker.clone(),
        consequence_class,
        occurred_at: request.occurred_at,
    };
    let ingestion = TranscriptIngestion::new(api.workspace.clone());
    // The destination takes the authenticated scope onto the blocking worker;
    // the stamp below needs the same value to write on the owner's authority.
    let stamping_scope = authenticated.clone();
    let applied = web::block(move || {
        // Registry I/O and blocking-worker admission may outlive the timestamp
        // sampled at handler entry, so the scope's liveness is re-checked
        // against the clock at the write rather than that earlier one.
        apply_staged_app_ingest(
            &staged_row,
            resolution,
            &authenticated,
            &ingestion,
            Utc::now(),
        )
    })
    .await;
    let application = match applied {
        Ok(Ok(application)) => application,
        Ok(Err(error)) => {
            return api_error(
                StatusCode::CONFLICT,
                "app_staged_ingest_refused",
                &error.to_string(),
            )
        },
        Err(error) => {
            return api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "app_staged_ingest_worker_unavailable",
                &format!("The staged-ingest store worker failed: {error}"),
            )
        },
    };

    let stamped = stamp_applied_staged_ingest(
        api.get_ref(),
        &stamping_scope,
        &installation_id,
        &source_entity,
        &source_record_id,
        source_record_revision,
        schema_revision,
        &application,
    )
    .await;

    HttpResponse::Ok()
        .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
        .json(json!({
            "ingest_id": &application.ingest_id,
            "transcript_key": &application.transcript_key,
            "outward_act_ref": application.outward_act_ref(),
            // Verbatim from the act row, so a replay of one ingest answers with
            // the time the room was first recorded rather than this clock.
            "act_recorded_at": application.act_recorded_at(),
            "utterances_seen": application.ingested.utterances_seen,
            // Counts and reasons, not claim rows: the console reads the queue
            // through its own governed sync, and a second copy here would be a
            // second projection of the register to keep in step.
            "claims": application.ingested.claims.len(),
            "skipped": application
                .ingested
                .skipped
                .iter()
                .map(|skipped| json!({
                    "segment_key": skipped.segment_key,
                    "reason": skipped.reason.as_str(),
                }))
                .collect::<Vec<_>>(),
            "staged_row_stamped": stamped,
        }))
}

/// Stamp the applied row with the three fields only the host may write.
///
/// The act is durable before this runs, so a failure is warned about and
/// reported rather than returned as a failed apply: the row stays at
/// `recorded`, and the same request retried resumes the same act and stamps
/// again. Reporting success would be the one wrong answer — a console showing
/// a queued room as settled.
///
/// The revision expectation is the point of doing it as an update rather than
/// a blind write: a row the package changed between the snapshot and here is
/// refused, so a stamp never lands on words nobody applied.
#[allow(clippy::too_many_arguments)]
async fn stamp_applied_staged_ingest(
    api: &AppPlatformApi,
    authenticated: &AuthenticatedAppScope,
    installation_id: &AppInstallationId,
    source_entity: &AppName,
    source_record_id: &AppRecordId,
    source_record_revision: AppRevision,
    schema_revision: AppRevision,
    application: &StagedIngestApplication,
) -> bool {
    let Ok(idempotency_key) =
        AppReference::parse(format!("claims-review-ingest:{source_record_id}"))
    else {
        tracing::warn!(
            target: "apps",
            installation_id = %installation_id,
            ingest_id = %application.ingest_id,
            "the staged ingest applied but its stamp could not be named; the row stays \
             `recorded` and a retry resumes the same act"
        );
        return false;
    };
    let command = AppMutationCommand {
        protocol_version: AppProtocolVersion::V1,
        idempotency_key,
        atomicity: AppMutationAtomicity::AllOrNothing,
        expected_schema_revision: schema_revision,
        operations: vec![AppMutationOperation::Update {
            entity: source_entity.clone(),
            record_id: source_record_id.clone(),
            // The three host fields and nothing else. The words, the mapping,
            // the audience and the owner's stated reason are the package's own
            // row and are not this route's to reword.
            patch: json!({
                "apply_state": "applied",
                "actor_ref": authenticated.actor_ref().as_str(),
                "act_ref": application.outward_act_ref(),
            }),
        }],
        expected_record_revisions: vec![AppExpectedRecordRevision {
            entity: source_entity.clone(),
            record_id: source_record_id.clone(),
            revision: source_record_revision,
        }],
    };
    match api
        .entity_adapter
        .owner_mutate(authenticated, installation_id, command, Utc::now())
        .await
    {
        Ok(_) => true,
        Err(error) => {
            tracing::warn!(
                target: "apps",
                error = %error,
                installation_id = %installation_id,
                ingest_id = %application.ingest_id,
                "the staged ingest applied but its row could not be stamped; the row stays \
                 `recorded` and a retry resumes the same act"
            );
            false
        },
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/meeting-controls/owner-decisions`
///
/// Sibling of the claims-decision route with one added obligation: capture
/// controls can begin an hours-long recording, so the destination re-checks the
/// surface gesture's freshness against its own clock at the mutation boundary,
/// refuses a start while any capture is live, and audits every accepted and
/// refused act. Every mutable authority input is resolved again from the
/// authenticated route immediately before that boundary.
async fn apply_app_meeting_control_owner_decision_handler(
    api: web::Data<AppPlatformApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AppMeetingControlOwnerDecisionEnvelopeV1>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if !interactive_owner_scope(&authenticated) {
        return api_error(
            StatusCode::FORBIDDEN,
            "app_meeting_control_requires_interactive_session",
            "Meeting controls require an interactive user session.",
        );
    }
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let installation = match api
        .registry
        .installation(&authenticated, &installation_id, now)
        .await
    {
        Ok(Some(installation)) => installation,
        Ok(None) => {
            return api_error(
                StatusCode::NOT_FOUND,
                "app_installation_not_found",
                "The meeting-control installation does not exist in this authenticated scope.",
            )
        },
        Err(error) => return registry_error_response(error),
    };
    if installation.lifecycle.status != AppInstallationStatus::Enabled {
        return api_error(
            StatusCode::CONFLICT,
            "app_meeting_control_installation_not_enabled",
            "The meeting-control installation is not enabled.",
        );
    }
    let Some(pairing) = api.macos_pairing.as_ref() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "app_meeting_control_desktop_owner_unavailable",
            "The code-verified desktop owner is unavailable.",
        );
    };
    let envelope = body.into_inner();
    if let Err(error) = envelope.validate() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "app_meeting_control_owner_decision_invalid",
            &error.to_string(),
        );
    }
    if let Err(error) = validate_app_meeting_control(&envelope.review.proposal) {
        return api_error(
            StatusCode::BAD_REQUEST,
            "app_meeting_control_command_invalid",
            &error.to_string(),
        );
    }
    let source_header = &envelope.review.proposal.header;
    if source_header.installation_id != installation_id.to_string()
        || source_header.scope_binding_ref != authenticated.scope_binding_ref().as_str()
        || source_header.installation_generation != installation.lifecycle.generation
        || source_header.package_revision_ref != installation.package_revision_ref.to_string()
        || envelope.review.proposal.by != authenticated.actor_ref().as_str()
    {
        return api_error(
            StatusCode::CONFLICT,
            "app_meeting_control_route_authority_mismatch",
            "The signed meeting command does not match the route's authenticated scope, actor, installation, or generation.",
        );
    }
    let source_entity = match AppName::parse(source_header.source_entity_name.clone()) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let source_record_id = match AppRecordId::parse(source_header.source_record_id.clone()) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let source_record_revision = match AppRevision::new(source_header.source_record_revision) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let workflow_id = match AppName::parse(source_header.workflow_id.clone()) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let action_id = match AppName::parse(source_header.action_id.clone()) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };

    let package_checked_at = Utc::now();
    let package = match api
        .registry
        .package_revision(
            &authenticated,
            &installation.package_revision_ref,
            package_checked_at,
        )
        .await
    {
        Ok(Some(package)) => package,
        Ok(None) => {
            return api_error(
                StatusCode::CONFLICT,
                "app_meeting_control_package_authority_unavailable",
                "The meeting-control installation's immutable package revision is unavailable.",
            )
        },
        Err(error) => return registry_error_response(error),
    };
    let staged = match api
        .stager
        .load_staged_package(&authenticated, package.content_digest.clone(), Utc::now())
        .await
    {
        Ok(staged) => staged,
        Err(error) => return staging_error_response(error),
    };
    let manifest = staged.candidate().manifest().manifest();
    let action = match manifest.app.actions.get(&action_id) {
        Some(action) if action.workflow == workflow_id => action,
        _ => {
            return api_error(
                StatusCode::CONFLICT,
                "app_meeting_control_action_authority_unavailable",
                "The signed meeting command no longer names its exact current workflow action.",
            )
        },
    };
    let workflow = match manifest.app.workflows.get(&workflow_id) {
        Some(workflow) => workflow,
        None => {
            return api_error(
                StatusCode::CONFLICT,
                "app_meeting_control_workflow_authority_unavailable",
                "The signed meeting command no longer names a current workflow.",
            )
        },
    };
    let workflow_value = match serde_json::to_value(workflow) {
        Ok(value) => value,
        Err(error) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_meeting_control_workflow_authority_invalid",
                &error.to_string(),
            )
        },
    };
    let workflow_digest = match AppDigest::blake3_canonical_json(&workflow_value) {
        Ok(value) => value,
        Err(error) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_meeting_control_workflow_authority_invalid",
                &error.to_string(),
            )
        },
    };
    let action_value = match serde_json::to_value(action) {
        Ok(value) => value,
        Err(error) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_meeting_control_action_authority_invalid",
                &error.to_string(),
            )
        },
    };
    let action_digest = match AppDigest::blake3_canonical_json(&action_value) {
        Ok(value) => value,
        Err(error) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_meeting_control_action_authority_invalid",
                &error.to_string(),
            )
        },
    };

    let identity =
        match pairing.active_desktop_identity(&authenticated).await {
            Ok(Some(identity)) => identity,
            Ok(None) => return api_error(
                StatusCode::CONFLICT,
                "app_meeting_control_desktop_owner_not_paired",
                "The code-verified desktop owner pairing changed before meeting-control admission.",
            ),
            Err(error) => return macos_pairing_error_response(error),
        };

    // Make the source-record read the final asynchronous registry operation, as
    // the claims sibling does: it reopens installation/grant/schema authority
    // and the exact live, undeleted `control_request` head in one snapshot.
    let authority_checked_at = Utc::now();
    let authority_snapshot = match AppEntityStoreService::new(api.registry.clone())
        .runtime_contribution_source_snapshot(
            &authenticated,
            &installation_id,
            &source_entity,
            &source_record_id,
            source_record_revision,
            authority_checked_at,
        )
        .await
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return entity_adapter_error_response(AppEntityAdapterError::from(error));
        },
    };
    let (current_installation, active, source_record_digest, source_record_payload) =
        authority_snapshot.into_source_parts();
    let source_record_digest = match source_record_digest {
        Some(digest) => digest.to_string(),
        None => {
            return api_error(
                StatusCode::CONFLICT,
                "app_meeting_control_source_authority_unavailable",
                "The signed meeting command has no current source-record digest.",
            )
        },
    };
    let source_record_payload = match source_record_payload {
        Some(payload) => payload,
        None => {
            return api_error(
                StatusCode::CONFLICT,
                "app_meeting_control_source_authority_unavailable",
                "The signed meeting command has no current source-record payload.",
            )
        },
    };
    if current_installation.package_revision_ref != installation.package_revision_ref
        || active.package_revision_ref() != &installation.package_revision_ref
    {
        return api_error(
            StatusCode::CONFLICT,
            "app_meeting_control_package_authority_changed",
            "The meeting-control package authority changed while the command was being admitted.",
        );
    }
    let current_authority = AppMeetingControlCurrentAuthority {
        destination_schema_digest: app_meeting_control_destination_schema_digest(),
        installation_generation: active.installation_generation(),
        package_revision_ref: active.package_revision_ref().to_string(),
        package_content_digest: package.content_digest.to_string(),
        grant_revision: active.grant_revision().get(),
        grant_authority_digest: active.grant().authority_digest.to_string(),
        schema_revision: active.schema_revision().get(),
        schema_digest: package.entity_schema_digest.to_string(),
        source_entity_name: source_entity.to_string(),
        source_record_id: source_record_id.to_string(),
        source_record_revision: source_record_revision.get(),
        source_record_digest,
        source_record_payload,
        workflow_id: workflow_id.to_string(),
        workflow_digest: workflow_digest.to_string(),
        action_id: action_id.to_string(),
        action_digest: action_digest.to_string(),
    };

    // Re-sample at the mutation boundary. Registry and pairing I/O may outlive
    // the timestamp taken at handler entry, and for a capture start that
    // difference is exactly what the gesture's expiry is measuring.
    let decision_time = Utc::now();
    let applied = apply_signed_app_meeting_control(
        &envelope,
        &identity.desktop_identity_public_key_hex,
        identity.pairing_generation,
        &identity.desktop_identity_key_id,
        &identity.desktop_identity_digest,
        &installation_id.to_string(),
        &current_authority,
        &authenticated,
        Arc::clone(resources.get_ref()),
        decision_time,
    )
    .await;
    // The destination's receipt lands in a host-owned ledger the app cannot
    // reach. This is the only path from that ledger into the package's declared
    // `control_receipt` entity, and without it the console's receipts view —
    // the operator's record that a signed command really touched capture — is
    // empty for the life of the installation.
    //
    // It runs on the refusal path too, and unlike the claims sibling it MUST:
    // a refusal that SPENT the envelope mints a `refused` row, and a retry of
    // a spent envelope mints nothing ever again, so gating this on
    // `applied.is_ok()` the way the claims sibling does would strand that row.
    //
    // Running it unconditionally is safe because the ledger read cannot land
    // outside this installation. It is keyed by the scope and `decision_id`,
    // and `envelope.validate()` above already refused any envelope whose
    // `decision_id` is not the contract's own digest of that envelope — a
    // digest over `proposal_digest`, which covers the header, which names the
    // installation this route checked against its own path. So a row read here
    // was minted by a command on THIS installation, and the worst an unproven
    // resubmission can do is republish a row this installation already earned,
    // which is the healing the publisher's own doc describes.
    publish_app_meeting_control_receipt(
        api.get_ref(),
        &authenticated,
        &installation_id,
        active.schema_revision(),
        &resources.get_ref().artifact_workspace,
        &envelope.decision_id,
        Utc::now(),
    )
    .await;
    match applied {
        Ok(outcome) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(outcome),
        Err(error) => api_error(
            StatusCode::CONFLICT,
            "app_meeting_control_owner_decision_refused",
            &error.to_string(),
        ),
    }
}

/// Publish one signed meeting control's durable receipt into the package's
/// `control_receipt` entity.
///
/// It reads the LEDGER, never the destination's return value. The ledger mints
/// a row only for a fate that is final and only for a command whose owner
/// signature was proven, so a still-retryable refusal publishes nothing and an
/// app can never see a row claiming a capture the destination did not record.
///
/// The ledger is scope-wide while this write lands in the path-named
/// installation's store, and the only thing that keeps those the same
/// installation is the decision id: the contract refuses an envelope whose
/// `decision_id` is not a digest of its own sealed proposal, and that proposal
/// carries the header the caller's route already checked. Pass a decision id
/// from anywhere other than an envelope the route validated and that property
/// is gone — this reads a sibling installation's row and publishes it here.
///
/// The row is written on the OWNER's authenticated authority. `control_receipt`
/// appears in no workflow's `may_mutate`, so the package still cannot write its
/// own receipt; this route can, because the owner is the one who signed the
/// command the row describes.
///
/// Best-effort, exactly as the ledger and the capture-control audit are: the
/// control already happened, and failing the owner's request because a
/// projection could not be written would invert the risk this record manages.
/// A row already published stays as it is — one signed decision, one row — and
/// because the ledger outlives the publish, a later submission of a spent
/// envelope republishes a row a transient store failure lost.
async fn publish_app_meeting_control_receipt(
    api: &AppPlatformApi,
    authenticated: &AuthenticatedAppScope,
    installation_id: &AppInstallationId,
    schema_revision: AppRevision,
    artifact_workspace: &ArtifactV2Workspace,
    decision_id: &str,
    now: DateTime<Utc>,
) {
    let Some(receipt) = scope_control_receipt(artifact_workspace, authenticated, decision_id)
    else {
        return;
    };
    let Some(command) = app_meeting_control_receipt_mutation(&receipt, schema_revision) else {
        return;
    };
    match api
        .entity_adapter
        .owner_mutate(authenticated, installation_id, command, now)
        .await
    {
        Ok(_) => {},
        // The row is already there. One signed decision, one row: for a
        // resubmitted envelope this is the expected outcome, not a failure.
        Err(AppEntityAdapterError::Mutation(AppEntityMutationError::RecordAlreadyExists)) => {},
        Err(error) => tracing::warn!(
            error = %error,
            installation_id = %installation_id,
            "a meeting control receipt could not be published into the package receipts view"
        ),
    }
}

/// The owner mutation that publishes one receipt row.
///
/// The record id is named rather than host-minted so the row's identity IS the
/// signed decision's identity: a second publish of one decision collides and is
/// refused, instead of adding a second row describing the same capture.
fn app_meeting_control_receipt_mutation(
    receipt: &AppMeetingControlReceipt,
    schema_revision: AppRevision,
) -> Option<AppMutationCommand> {
    let record_id = receipt.package_record_id()?;
    Some(AppMutationCommand {
        protocol_version: AppProtocolVersion::V1,
        idempotency_key: AppReference::parse(format!("meeting-control-receipt:{record_id}"))
            .ok()?,
        atomicity: AppMutationAtomicity::AllOrNothing,
        expected_schema_revision: schema_revision,
        operations: vec![AppMutationOperation::Create {
            entity: AppName::parse(CONTROL_RECEIPT_ENTITY).ok()?,
            temporary_id: AppName::parse(CONTROL_RECEIPT_ENTITY).ok()?,
            record_id: Some(AppRecordId::parse(record_id.as_str()).ok()?),
            // The destination owns this shape: the row is the receipt, field
            // for field, so a projector here cannot quietly reword it.
            payload: serde_json::to_value(receipt).ok()?,
        }],
        expected_record_revisions: Vec::new(),
    })
}

const APP_BACKGROUND_BEHAVIOR_MAX_SCOPES_PER_TICK: usize = 256;
const APP_BACKGROUND_BEHAVIOR_DIRECTORY_SCAN_CEILING: usize = 65_536;
const APP_BACKGROUND_BEHAVIOR_DIRECTORY_PAGE_SIZE: usize = 256;
const APP_BACKGROUND_BEHAVIOR_DIRECTORY_PAGES_PER_REFILL: usize = 8;
// Independent scopes can progress while another waits on its own records.
// The registry's fair background admission bounds SQL work and preserves
// foreground headroom. This bounds in-flight sweeps, not installed App count.
const APP_BACKGROUND_BEHAVIOR_SCOPE_CONCURRENCY: usize = 4;
const APP_BACKGROUND_BEHAVIOR_RESTART_MAX: StdDuration = StdDuration::from_secs(30);
const APP_BACKGROUND_BEHAVIOR_RESTART_RESET_AFTER: StdDuration = StdDuration::from_secs(60);

struct AppBackgroundBehaviorSupervisorGuard {
    started: Arc<AtomicBool>,
    event_admission: AppEventIngressAdmission,
    scheduler: Arc<StdRwLock<Option<AppBehaviorScheduler>>>,
    workflow_service: AppWorkflowService,
}

impl Drop for AppBackgroundBehaviorSupervisorGuard {
    fn drop(&mut self) {
        // Close workflow admission before releasing the spawn latch. A new
        // supervisor can then acquire the latch and reopen admission without
        // an old guard racing its newly admitted generation back to closed.
        self.event_admission.close_and_drain();
        self.workflow_service
            .set_background_behavior_runtime_admission(false);
        *self
            .scheduler
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.started.store(false, Ordering::Release);
    }
}

#[derive(Default)]
struct AppBackgroundScopeDiscoveryCursor {
    principal_after: Option<String>,
    principal: Option<String>,
    workspace_after: Option<String>,
    pending_principals: VecDeque<String>,
    pass_complete_after_pending: bool,
}

#[derive(Clone)]
struct AppCanonicalEventBehaviorObserver {
    api: AppPlatformApi,
    run_ref: AppReference,
}

#[async_trait::async_trait]
impl RuntimeCanonicalEventObserver for AppCanonicalEventBehaviorObserver {
    async fn observe_persisted(&self, event: &CanonicalEvent) -> Result<(), String> {
        if event.event_type != ArtifactV2EventType::AgenticExecutionCompleted.as_str() {
            return Ok(());
        }
        let Some(event_admission_epoch) =
            self.api.background_behavior_event_admission.current_epoch()
        else {
            return Ok(());
        };
        if self.api.background_behavior_scheduler().is_none() {
            // The boot master is checked before projection admission as well
            // as at task runtime, so a disabled process does not accumulate
            // event-execution debt it has explicitly refused to own.
            return Ok(());
        }
        let outcome = match event
            .payload
            .get("outcome")
            .and_then(serde_json::Value::as_str)
        {
            Some("success") => AppEventTerminalOutcomeV1::Succeeded,
            Some("failed") => AppEventTerminalOutcomeV1::Failed,
            // Waiting, budget, loop, and cannot-proceed outcomes are not V1
            // terminal subscription facts. They remain canonical for their
            // existing consumers but are not reinterpreted here.
            _ => return Ok(()),
        };
        let ui_thread_id = event
            .payload
            .get(CANONICAL_UI_THREAD_ID_FIELD)
            .and_then(serde_json::Value::as_str);
        let scope = ScopeRef::system_internal_unauthenticated(&event.principal, &event.workspace);
        let Some(source_authority) = self
            .api
            .workflow_service()
            .canonical_event_behavior_source(&scope, &event.task_id, &event.execution_id)
            .await
            .map_err(|error| error.to_string())?
        else {
            // Only sealed workflow authority may establish that a canonical
            // terminal belongs to an app. A non-app UI-thread value is a safe
            // negative filter only after that authority lookup returned none;
            // applying it first would hide a substituted host routing field on
            // a real app execution.
            return Ok(());
        };
        if source_authority.is_background_origin() {
            // V1 suppresses every background-origin terminal. This bounded
            // one-hop rule prevents schedule/event recursion until a sealed
            // causation and spend lineage can be propagated end to end.
            return Ok(());
        }
        let source_event_ref = event
            .payload
            .get(SOURCE_EVENT_REF_FIELD)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "canonical app terminal omitted its host source reference".to_owned())?;
        let ui_thread_id = ui_thread_id.ok_or_else(|| {
            "canonical app terminal omitted its host UI-thread identity".to_owned()
        })?;
        if ui_thread_id != format!("app:{}", source_authority.installation_id()) {
            return Err("canonical app terminal disagrees with its sealed installation".to_owned());
        }
        let recorded_at = DateTime::parse_from_rfc3339(&event.timestamp)
            .map(|value| value.with_timezone(&Utc))
            .map_err(|_| "canonical app terminal timestamp is invalid".to_owned())?;
        let now = Utc::now();
        let authenticated = system_worker_scope_for_actor(
            &event.principal,
            &event.workspace,
            "worker:app-event-router",
            &self.run_ref,
            now,
        )?;
        let event_behaviors = self
            .api
            .event_behaviors
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        event_behaviors
            .accept_canonical_execution_terminal(
                &authenticated,
                AppCanonicalExecutionTerminalV1::from_host_projection(
                    source_authority,
                    source_event_ref.to_owned(),
                    event.execution_id.clone(),
                    outcome,
                    recorded_at,
                )
                .map_err(|error| error.to_string())?,
                &self.api.background_behavior_event_admission,
                event_admission_epoch,
                now,
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone)]
struct AppBackgroundBehaviorWorker {
    api: AppPlatformApi,
    scheduler: Option<AppBehaviorScheduler>,
    event_behaviors: AppEventBehaviorService,
    owner_notifications: AppOwnerNotificationService,
    user_requests: Arc<UserRequestService>,
    workspace: ArtifactV2Workspace,
    resources: Arc<AgentResources>,
    notification_claim_limit: u16,
    notification_lease_seconds: u64,
    tick_interval: StdDuration,
    scope_timeout: StdDuration,
    run_ref: AppReference,
}

impl AppBackgroundBehaviorWorker {
    async fn refill_scope_batch(
        &self,
        cursor: &mut AppBackgroundScopeDiscoveryCursor,
        pending: &mut VecDeque<(String, String)>,
    ) -> Result<bool, magician::magician_v2::artifact_v2::ArtifactV2Error> {
        let scopes_root = self.workspace.scopes_root();
        let mut directory_pages = 0usize;
        while pending.len() < APP_BACKGROUND_BEHAVIOR_MAX_SCOPES_PER_TICK {
            if directory_pages >= APP_BACKGROUND_BEHAVIOR_DIRECTORY_PAGES_PER_REFILL {
                return Ok(false);
            }
            if cursor.principal.is_none() {
                if let Some(principal) = cursor.pending_principals.pop_front() {
                    cursor.principal = Some(principal);
                    cursor.workspace_after = None;
                    continue;
                }
                if cursor.pass_complete_after_pending {
                    cursor.pass_complete_after_pending = false;
                    return Ok(true);
                }
            }
            if let Some(principal) = cursor.principal.clone() {
                let remaining = APP_BACKGROUND_BEHAVIOR_MAX_SCOPES_PER_TICK - pending.len();
                directory_pages = directory_pages.saturating_add(1);
                let page = match self
                    .workspace
                    .read_dir_page_path_or_empty(
                        scopes_root.join(&principal),
                        cursor.workspace_after.as_deref(),
                        remaining.max(1),
                        APP_BACKGROUND_BEHAVIOR_DIRECTORY_SCAN_CEILING,
                    )
                    .await
                {
                    Ok(page) => page,
                    Err(error) => {
                        tracing::warn!(
                            principal,
                            error = %error,
                            "app background-behavior principal workspace scan was isolated"
                        );
                        cursor.principal = None;
                        cursor.workspace_after = None;
                        continue;
                    },
                };
                if page.overflow && cursor.workspace_after.is_none() {
                    tracing::warn!(
                        principal,
                        scan_ceiling = APP_BACKGROUND_BEHAVIOR_DIRECTORY_SCAN_CEILING,
                        "app background-behavior workspace directory exceeds its physical scan ceiling; processing the admitted prefix"
                    );
                }
                for entry in page.entries.iter().filter(|entry| entry.is_dir) {
                    let registry_path = self
                        .workspace
                        .app_store_db_path(&principal, &entry.file_name);
                    match self.workspace.symlink_metadata_path(registry_path).await {
                        Ok(Some(metadata)) if metadata.file_type().is_file() => {
                            pending.push_back((principal.clone(), entry.file_name.clone()));
                        },
                        Ok(_) => {},
                        Err(error) => tracing::warn!(
                            principal,
                            workspace = %entry.file_name,
                            error = %error,
                            "app background-behavior workspace registry metadata was isolated"
                        ),
                    }
                }
                if page.complete || (page.overflow && page.next_after.is_none()) {
                    cursor.principal = None;
                    cursor.workspace_after = None;
                } else {
                    let Some(next_after) = page.next_after else {
                        return Err(
                            magician::magician_v2::artifact_v2::ArtifactV2Error::InvalidRequest(
                                "incomplete app background-behavior workspace page omitted its cursor"
                                    .to_owned(),
                            ),
                        );
                    };
                    if cursor.workspace_after.as_ref() == Some(&next_after) {
                        return Err(
                            magician::magician_v2::artifact_v2::ArtifactV2Error::InvalidRequest(
                                "app background-behavior workspace cursor did not advance"
                                    .to_owned(),
                            ),
                        );
                    }
                    cursor.workspace_after = Some(next_after);
                }
                continue;
            }

            directory_pages = directory_pages.saturating_add(1);
            let page = self
                .workspace
                .read_dir_page_path_or_empty(
                    &scopes_root,
                    cursor.principal_after.as_deref(),
                    APP_BACKGROUND_BEHAVIOR_DIRECTORY_PAGE_SIZE,
                    APP_BACKGROUND_BEHAVIOR_DIRECTORY_SCAN_CEILING,
                )
                .await?;
            if page.overflow && cursor.principal_after.is_none() {
                tracing::warn!(
                    scan_ceiling = APP_BACKGROUND_BEHAVIOR_DIRECTORY_SCAN_CEILING,
                    "app background-behavior principal directory exceeds its physical scan ceiling; processing the admitted prefix"
                );
            }
            let terminal_page = page.complete || (page.overflow && page.next_after.is_none());
            if terminal_page {
                cursor.principal_after = None;
                cursor.pass_complete_after_pending = true;
            } else {
                let Some(next_after) = page.next_after.clone() else {
                    return Err(
                        magician::magician_v2::artifact_v2::ArtifactV2Error::InvalidRequest(
                            "incomplete app background-behavior principal page omitted its cursor"
                                .to_owned(),
                        ),
                    );
                };
                if cursor.principal_after.as_ref() == Some(&next_after) {
                    return Err(
                        magician::magician_v2::artifact_v2::ArtifactV2Error::InvalidRequest(
                            "app background-behavior principal cursor did not advance".to_owned(),
                        ),
                    );
                }
                cursor.principal_after = Some(next_after);
            }
            cursor.pending_principals.extend(
                page.entries
                    .into_iter()
                    .filter(|entry| entry.is_dir)
                    .map(|entry| entry.file_name),
            );
        }
        Ok(false)
    }

    async fn compact_event_history(&self, principal: &str, workspace: &str) {
        let now = Utc::now();
        let authenticated =
            match background_behavior_worker_scope(principal, workspace, &self.run_ref, now) {
                Ok(authenticated) => authenticated,
                Err(error) => {
                    tracing::warn!(
                        principal,
                        workspace,
                        error = %error,
                        "invalid app event-retention worker scope"
                    );
                    return;
                },
            };
        if let Err(error) = self
            .event_behaviors
            .compact_terminal(&authenticated, now)
            .await
        {
            tracing::warn!(
                principal,
                workspace,
                error = %error,
                "app event-behavior terminal compaction will retry"
            );
        }
    }

    async fn run_event_behaviors(
        &self,
        authenticated: &AuthenticatedAppScope,
        principal: &str,
        workspace: &str,
    ) {
        let dispatches = match self
            .event_behaviors
            .claim_due(authenticated, &self.run_ref, Utc::now())
            .await
        {
            Ok(dispatches) => dispatches,
            Err(error) => {
                tracing::warn!(
                    principal,
                    workspace,
                    error = %error,
                    "app event-behavior claim will retry"
                );
                return;
            },
        };
        for dispatch in dispatches {
            let Some(authorize_budget) =
                background_behavior_dispatch_budget(dispatch.lease_expires_at())
            else {
                continue;
            };
            let authority = match tokio::time::timeout(
                authorize_budget,
                self.event_behaviors
                    .authorize_dispatch(authenticated, &dispatch, Utc::now()),
            )
            .await
            {
                Err(_) => {
                    tracing::warn!(
                        principal,
                        workspace,
                        installation_id = %dispatch.installation_id(),
                        event_behavior_id = %dispatch.event_behavior_id(),
                        "app event-behavior authorization exhausted its lease budget"
                    );
                    continue;
                },
                Ok(Ok(Some(authority))) => authority,
                Ok(Ok(None)) => continue,
                Ok(Err(error)) => {
                    tracing::warn!(
                        principal,
                        workspace,
                        installation_id = %dispatch.installation_id(),
                        event_behavior_id = %dispatch.event_behavior_id(),
                        error = %error,
                        "app event-behavior lease was fenced before dispatch"
                    );
                    if !matches!(error, AppEventBehaviorError::LeaseLost) {
                        let error_text = error.to_string();
                        if let Err(settle_error) = self
                            .event_behaviors
                            .settle(
                                authenticated,
                                &dispatch,
                                AppEventBehaviorSettlement::Retry,
                                Some(&error_text),
                                Utc::now(),
                            )
                            .await
                        {
                            tracing::warn!(
                                principal,
                                workspace,
                                error = %settle_error,
                                "app event-behavior pre-dispatch failure could not be settled"
                            );
                        }
                    }
                    continue;
                },
            };
            let Some(launch_budget) =
                background_behavior_dispatch_budget(dispatch.lease_expires_at())
            else {
                continue;
            };
            let outcome = tokio::time::timeout(
                launch_budget,
                self.api.invoke_event_app_behavior(
                    authenticated,
                    &dispatch,
                    self.resources.as_ref(),
                    authority,
                    Utc::now(),
                ),
            )
            .await;
            let (settlement, error) = match outcome {
                Ok(Ok(launch)) => {
                    tracing::info!(
                        principal,
                        workspace,
                        installation_id = %dispatch.installation_id(),
                        event_behavior_id = %dispatch.event_behavior_id(),
                        task_id = %launch.task_id,
                        "app event behavior accepted"
                    );
                    (AppEventBehaviorSettlement::Accepted, None)
                },
                Ok(Err(error)) => {
                    let settlement = if error.is_background_launch_retryable() {
                        AppEventBehaviorSettlement::Retry
                    } else {
                        AppEventBehaviorSettlement::Blocked
                    };
                    tracing::warn!(
                        principal,
                        workspace,
                        installation_id = %dispatch.installation_id(),
                        event_behavior_id = %dispatch.event_behavior_id(),
                        error = %error,
                        retryable = matches!(settlement, AppEventBehaviorSettlement::Retry),
                        "app event behavior launch was refused"
                    );
                    (settlement, Some(error.to_string()))
                },
                Err(_) => (
                    AppEventBehaviorSettlement::Retry,
                    Some("workflow admission exceeded the durable lease budget".to_owned()),
                ),
            };
            if let Err(error) = self
                .event_behaviors
                .settle(
                    authenticated,
                    &dispatch,
                    settlement,
                    error.as_deref(),
                    Utc::now(),
                )
                .await
            {
                tracing::warn!(
                    principal,
                    workspace,
                    installation_id = %dispatch.installation_id(),
                    event_behavior_id = %dispatch.event_behavior_id(),
                    error = %error,
                    "app event-behavior settlement will recover from its durable lease"
                );
            }
        }
    }

    async fn run_owner_notifications(&self, principal: &str, workspace: &str) {
        let claim_now = Utc::now();
        let authenticated = match background_behavior_worker_scope(
            principal,
            workspace,
            &self.run_ref,
            claim_now,
        ) {
            Ok(authenticated) => authenticated,
            Err(error) => {
                tracing::warn!(
                    principal,
                    workspace,
                    error = %error,
                    "invalid app owner-notification worker scope"
                );
                return;
            },
        };
        if let Err(error) = self
            .owner_notifications
            .compact_terminal(&authenticated, Utc::now())
            .await
        {
            tracing::warn!(
                principal,
                workspace,
                error = %error,
                "app owner-notification terminal compaction will retry"
            );
        }
        let claims = match self
            .owner_notifications
            .claim_due(
                &authenticated,
                self.run_ref.as_str(),
                self.notification_claim_limit,
                self.notification_lease_seconds,
                claim_now,
            )
            .await
        {
            Ok(claims) => claims,
            Err(error) => {
                tracing::warn!(
                    principal,
                    workspace,
                    error = %error,
                    "app owner-notification claim will retry"
                );
                return;
            },
        };
        for claim in claims {
            let delivery_now = Utc::now();
            let delivery_scope = match background_behavior_worker_scope(
                principal,
                workspace,
                &self.run_ref,
                delivery_now,
            ) {
                Ok(authenticated) => authenticated,
                Err(error) => {
                    tracing::warn!(
                        principal,
                        workspace,
                        error = %error,
                        "app owner-notification delivery scope could not be refreshed"
                    );
                    continue;
                },
            };
            let attempt = match self
                .owner_notifications
                .deliver_claim(
                    &delivery_scope,
                    self.user_requests.as_ref(),
                    claim,
                    delivery_now,
                )
                .await
            {
                Ok(attempt) => attempt,
                Err(error) => {
                    // Do not settle a revalidation or transient delivery
                    // failure. The exact fenced lease expires and the same
                    // deterministic effect is recovered on a later pass.
                    tracing::warn!(
                        principal,
                        workspace,
                        error = %error,
                        "app owner-notification delivery will recover from its durable lease"
                    );
                    continue;
                },
            };
            let settle_now = Utc::now();
            let settle_scope = match background_behavior_worker_scope(
                principal,
                workspace,
                &self.run_ref,
                settle_now,
            ) {
                Ok(authenticated) => authenticated,
                Err(error) => {
                    tracing::warn!(
                        principal,
                        workspace,
                        error = %error,
                        "app owner-notification settlement scope could not be refreshed"
                    );
                    continue;
                },
            };
            if let Err(error) = self
                .owner_notifications
                .settle_delivery(&settle_scope, attempt, settle_now)
                .await
            {
                tracing::warn!(
                    principal,
                    workspace,
                    error = %error,
                    "app owner-notification settlement will recover from its durable lease"
                );
            }
        }
    }

    async fn run_scope(&self, principal: String, workspace: String) {
        // Owner attention is independent of the unattended-execution boot
        // master. Keep it in an independent lane so neither a stalled delivery
        // nor busy unattended execution can consume the other's scope window.
        let owner_notification_lane = self.run_owner_notifications(&principal, &workspace);
        let event_retention_lane = self.compact_event_history(&principal, &workspace);
        let Some(scheduler) = self.scheduler.as_ref() else {
            tokio::join!(owner_notification_lane, event_retention_lane);
            return;
        };
        let now = Utc::now();
        let authenticated =
            match background_behavior_worker_scope(&principal, &workspace, &self.run_ref, now) {
                Ok(authenticated) => authenticated,
                Err(error) => {
                    tracing::warn!(
                        principal,
                        workspace,
                        error = %error,
                        "invalid app background-behavior worker scope"
                    );
                    return;
                },
            };
        let scheduled_lane = async {
            let artifact_scope = ScopeRef::system_internal_unauthenticated(&principal, &workspace);
            let task_acceptance = AppArtifactTaskAcceptanceProbe::new(
                self.resources.artifact_v2_service.clone(),
                artifact_scope,
            );
            let dispatches = match scheduler
                .claim_due(&authenticated, &self.run_ref, &task_acceptance, now)
                .await
            {
                Ok(dispatches) => dispatches,
                Err(error) => {
                    tracing::warn!(
                        principal,
                        workspace,
                        error = %error,
                        "app background-behavior claim will retry"
                    );
                    Vec::new()
                },
            };
            for dispatch in dispatches {
                let Some(authorize_budget) =
                    background_behavior_dispatch_budget(dispatch.lease_expires_at())
                else {
                    continue;
                };
                let authority = match tokio::time::timeout(
                    authorize_budget,
                    scheduler.authorize_dispatch(&authenticated, &dispatch, Utc::now()),
                )
                .await
                {
                    Err(_) => {
                        tracing::warn!(
                            principal,
                            workspace,
                            installation_id = %dispatch.installation_id(),
                            behavior_id = %dispatch.behavior_id(),
                            "app background-behavior authorization exhausted its lease budget"
                        );
                        continue;
                    },
                    Ok(result) => match result {
                        Ok(authority) => authority,
                        Err(error) => {
                            tracing::warn!(
                                principal,
                                workspace,
                                installation_id = %dispatch.installation_id(),
                                behavior_id = %dispatch.behavior_id(),
                                error = %error,
                                "app background-behavior lease was fenced before dispatch"
                            );
                            if !matches!(error, AppBehaviorSchedulerError::LeaseLost) {
                                let error_text = error.to_string();
                                if let Err(settle_error) = scheduler
                                    .settle(
                                        &authenticated,
                                        &dispatch,
                                        AppBehaviorSettlement::Retry,
                                        Some(&error_text),
                                        Utc::now(),
                                    )
                                    .await
                                {
                                    tracing::warn!(
                                        principal,
                                        workspace,
                                        error = %settle_error,
                                        "app background-behavior pre-dispatch failure could not be settled"
                                    );
                                }
                            }
                            continue;
                        },
                    },
                };
                let Some(launch_budget) =
                    background_behavior_dispatch_budget(dispatch.lease_expires_at())
                else {
                    continue;
                };
                let outcome = tokio::time::timeout(
                    launch_budget,
                    self.api.invoke_scheduled_app_behavior(
                        &authenticated,
                        &dispatch,
                        self.resources.as_ref(),
                        authority,
                        Utc::now(),
                    ),
                )
                .await;
                let (settlement, error) = match outcome {
                    Ok(Ok(launch)) => {
                        tracing::info!(
                            principal,
                            workspace,
                            installation_id = %dispatch.installation_id(),
                            behavior_id = %dispatch.behavior_id(),
                            task_id = %launch.task_id,
                            "app background behavior accepted"
                        );
                        (AppBehaviorSettlement::Accepted, None)
                    },
                    Ok(Err(error)) => {
                        let settlement = if error.is_background_launch_retryable() {
                            AppBehaviorSettlement::Retry
                        } else {
                            AppBehaviorSettlement::Blocked
                        };
                        tracing::warn!(
                            principal,
                            workspace,
                            installation_id = %dispatch.installation_id(),
                            behavior_id = %dispatch.behavior_id(),
                            error = %error,
                            retryable = matches!(settlement, AppBehaviorSettlement::Retry),
                            "app background behavior launch was refused"
                        );
                        (settlement, Some(error.to_string()))
                    },
                    Err(_) => {
                        tracing::warn!(
                            principal,
                            workspace,
                            installation_id = %dispatch.installation_id(),
                            behavior_id = %dispatch.behavior_id(),
                            "app background behavior launch exhausted its lease budget"
                        );
                        (
                            AppBehaviorSettlement::Retry,
                            Some("workflow admission exceeded the durable lease budget".to_owned()),
                        )
                    },
                };
                if let Err(error) = scheduler
                    .settle(
                        &authenticated,
                        &dispatch,
                        settlement,
                        error.as_deref(),
                        Utc::now(),
                    )
                    .await
                {
                    tracing::warn!(
                        principal,
                        workspace,
                        installation_id = %dispatch.installation_id(),
                        behavior_id = %dispatch.behavior_id(),
                        error = %error,
                        "app background-behavior settlement will recover from its durable lease"
                    );
                }
                if settlement == AppBehaviorSettlement::Accepted {
                    if let Err(error) = scheduler
                        .observe_dispatch(&authenticated, &dispatch, &task_acceptance, Utc::now())
                        .await
                    {
                        tracing::warn!(%error, "app recurring execution observation will retry");
                    }
                }
            }
        };
        // Every debt class receives the same independent sweep window; one
        // cannot consume the outer scope budget before another starts.
        tokio::join!(
            owner_notification_lane,
            event_retention_lane,
            scheduled_lane,
            self.run_event_behaviors(&authenticated, &principal, &workspace)
        );
    }

    async fn run(self) {
        // Keep the discovery cursor across ticks. Each refill admits at most
        // eight bounded directory pages and each execution batch at most 256
        // scopes, so a sparse or malformed tree cannot monopolize this task.
        let mut pending_scopes = VecDeque::new();
        let mut discovery_cursor = AppBackgroundScopeDiscoveryCursor::default();
        loop {
            if pending_scopes.is_empty() {
                match self
                    .refill_scope_batch(&mut discovery_cursor, &mut pending_scopes)
                    .await
                {
                    Ok(_pass_complete) => {},
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            "app background-behavior worker could not enumerate scopes"
                        );
                        tokio::time::sleep(self.tick_interval).await;
                        continue;
                    },
                }
            }
            let scope_count = pending_scopes
                .len()
                .min(APP_BACKGROUND_BEHAVIOR_MAX_SCOPES_PER_TICK);
            let scopes = (0..scope_count)
                .filter_map(|_| pending_scopes.pop_front())
                .collect::<Vec<_>>();
            run_background_scope_batch(
                scopes,
                    |(principal, workspace)| {
                        let worker = self.clone();
                        async move {
                            if tokio::time::timeout(
                                worker.scope_timeout,
                                worker.run_scope(principal.clone(), workspace.clone()),
                            )
                            .await
                            .is_err()
                            {
                                tracing::warn!(
                                    principal,
                                    workspace,
                                    timeout_ms = worker.scope_timeout.as_millis(),
                                    "app background-behavior scope exhausted its sweep budget; durable leases will recover"
                                );
                            }
                        }
                    },
                )
                .await;
            tokio::time::sleep(self.tick_interval).await;
        }
    }
}

async fn run_background_scope_batch<F, Fut>(scopes: Vec<(String, String)>, run_scope: F)
where
    F: FnMut((String, String)) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    futures_util::stream::iter(scopes)
        .for_each_concurrent(Some(APP_BACKGROUND_BEHAVIOR_SCOPE_CONCURRENCY), run_scope)
        .await;
}

fn background_behavior_dispatch_budget(expires_at: DateTime<Utc>) -> Option<StdDuration> {
    let remaining_ms = expires_at
        .signed_duration_since(Utc::now())
        .num_milliseconds()
        .checked_sub(250)?;
    (remaining_ms > 0).then(|| StdDuration::from_millis(remaining_ms as u64))
}

fn app_background_behavior_restart_delay(consecutive_failures: u32) -> StdDuration {
    let shift = consecutive_failures.min(5);
    let seconds = 1u64
        .checked_shl(shift)
        .unwrap_or(APP_BACKGROUND_BEHAVIOR_RESTART_MAX.as_secs())
        .min(APP_BACKGROUND_BEHAVIOR_RESTART_MAX.as_secs());
    StdDuration::from_secs(seconds)
}

const APP_PROJECTION_CLAIM_BATCH: usize = 64;
const APP_PROJECTION_LEASE: StdDuration = StdDuration::from_secs(60);
const APP_PROJECTION_IDLE: StdDuration = StdDuration::from_secs(1);
const APP_PROJECTION_SCOPE_DISCOVERY: StdDuration = StdDuration::from_secs(10);
const APP_WIDGET_REGISTRATION_REFRESH: StdDuration = StdDuration::from_secs(30);

/// Boot repair is retried on every worker pass. Report the first three
/// failures and then one in every `BOOT_REPAIR_RETRY_LOG_EVERY`, so a scope
/// that never repairs stays visible without a warning per pass.
const BOOT_REPAIR_RETRY_LOG_EVERY: u32 = 60;

fn note_boot_repair_retry(
    failures: &mut HashMap<((String, String), &'static str), u32>,
    scope: &(String, String),
    subsystem: &'static str,
    error: &dyn std::fmt::Display,
) {
    let count = failures.entry((scope.clone(), subsystem)).or_insert(0);
    *count = count.saturating_add(1);
    if *count <= 3 || *count % BOOT_REPAIR_RETRY_LOG_EVERY == 0 {
        tracing::warn!(
            principal = scope.0.as_str(),
            workspace = scope.1.as_str(),
            subsystem,
            consecutive_failures = *count,
            error = %error,
            "app boot repair will retry"
        );
    }
}

#[derive(Clone)]
struct AppProjectionWorker {
    workspace: ArtifactV2Workspace,
    registry: AppRegistryService,
    memory_contributions: AppMemoryContributionProjectionService,
    personal_agent_retrieval: Option<AppPersonalAgentRetrievalProjectionService>,
    entity_changes: AppEntityChangeService,
    app_publications: AppPublicationReconciler,
    custom_surfaces: Arc<AppCustomSurfaceRuntime>,
    scripted_surfaces: Arc<AppScriptedSurfaceRuntime>,
    computed_capabilities: Arc<AppComputedCapabilityOverlayCache>,
    widget_runtime: AppWidgetRuntime,
    scoped_capabilities: Arc<StdRwLock<Option<Arc<ScopedCapabilityResolver>>>>,
    event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    run_ref: AppReference,
}

impl AppProjectionWorker {
    async fn run(self) {
        if !magician::magician_v2::runtime::startup::wait_for_http().await {
            return;
        }
        let mut repaired_scopes = HashSet::new();
        let mut repaired_memory_scopes = HashSet::new();
        let mut repaired_retrieval_scopes = HashSet::new();
        let mut repair_failures: HashMap<((String, String), &'static str), u32> = HashMap::new();
        let mut next_widget_registration_refresh = HashMap::new();
        let mut scopes = Vec::new();
        let mut next_scope_discovery = tokio::time::Instant::now();
        loop {
            if scopes.is_empty() || tokio::time::Instant::now() >= next_scope_discovery {
                let workspace = self.workspace.clone();
                // Tenants only. The projection worker opens each scope's app
                // store and holds the connection for the life of the process,
                // so enumerating a reserved sink kept an `app_store.sqlite3`
                // open for a scope that boot admission already refuses to admit
                // anything into.
                match tokio::task::spawn_blocking(move || workspace.list_tenant_scopes()).await {
                    Ok(discovered) => {
                        // Scope directories can be removed or repaired while
                        // this process stays alive. Retaining cadence/repair
                        // keys forever would turn the worker's bookkeeping
                        // into disk-history rather than current bounded work.
                        let current = discovered.iter().cloned().collect::<HashSet<_>>();
                        repaired_scopes.retain(|scope| current.contains(scope));
                        repaired_memory_scopes.retain(|scope| current.contains(scope));
                        repaired_retrieval_scopes.retain(|scope| current.contains(scope));
                        repair_failures.retain(|(scope, _), _| current.contains(scope));
                        next_widget_registration_refresh.retain(|scope, _| current.contains(scope));
                        scopes = discovered;
                    },
                    Err(error) => {
                        tracing::warn!(error = %error, "app projection worker could not enumerate scopes");
                    },
                }
                next_scope_discovery = tokio::time::Instant::now() + APP_PROJECTION_SCOPE_DISCOVERY;
            }
            for (principal, workspace) in scopes.iter().cloned() {
                let key = (principal.clone(), workspace.clone());
                let now = Utc::now();
                let authenticated = match system_worker_scope(
                    &principal,
                    &workspace,
                    &self.run_ref,
                    now,
                ) {
                    Ok(authenticated) => authenticated,
                    Err(error) => {
                        tracing::warn!(principal, workspace, error = %error, "invalid app projection worker scope");
                        continue;
                    },
                };
                let registry_path = self.workspace.app_store_db_path(&principal, &workspace);
                let has_registry = std::fs::symlink_metadata(&registry_path)
                    .is_ok_and(|metadata| metadata.file_type().is_file());
                let publication_path = self
                    .workspace
                    .published_surfaces_dir(&principal, &workspace);
                let has_publication_records = std::fs::symlink_metadata(&publication_path)
                    .is_ok_and(|metadata| metadata.file_type().is_dir());
                if !has_registry && !has_publication_records {
                    continue;
                }
                if !repaired_scopes.contains(&key) {
                    match self
                        .app_publications
                        .repair_scope(&authenticated, Utc::now())
                        .await
                    {
                        Ok(repaired) => {
                            repaired_scopes.insert(key.clone());
                            tracing::info!(
                                principal,
                                workspace,
                                repaired,
                                "app publication boot repair completed"
                            );
                        },
                        Err(error) => tracing::warn!(
                            principal,
                            workspace,
                            error = %error,
                            "app publication boot repair will retry"
                        ),
                    }
                }
                if !has_registry {
                    continue;
                }
                let scope_key = (principal.clone(), workspace.clone());
                // Each boot repair gates only its own drain. A failed repair
                // used to `continue` past every later drain and the
                // capability-overlay/widget refreshes, so one stale retrieval
                // head silenced the whole scope for as long as it stayed stale.
                let memory_ready = if repaired_memory_scopes.contains(&scope_key) {
                    true
                } else {
                    match self
                        .memory_contributions
                        .repair_scope(&authenticated, Utc::now())
                        .await
                    {
                        Ok(()) => {
                            repaired_memory_scopes.insert(scope_key.clone());
                            repair_failures.remove(&(scope_key.clone(), "memory-contributions"));
                            true
                        },
                        Err(error) => {
                            note_boot_repair_retry(
                                &mut repair_failures,
                                &scope_key,
                                "memory-contributions",
                                &error,
                            );
                            false
                        },
                    }
                };
                let retrieval_ready = match self.personal_agent_retrieval.as_ref() {
                    None => false,
                    Some(_) if repaired_retrieval_scopes.contains(&scope_key) => true,
                    Some(retrieval) => {
                        match retrieval.repair_scope(&authenticated, Utc::now()).await {
                            Ok(()) => {
                                repaired_retrieval_scopes.insert(scope_key.clone());
                                repair_failures
                                    .remove(&(scope_key.clone(), "personal-agent-retrieval"));
                                true
                            },
                            Err(error) => {
                                note_boot_repair_retry(
                                    &mut repair_failures,
                                    &scope_key,
                                    "personal-agent-retrieval",
                                    &error,
                                );
                                false
                            },
                        }
                    },
                };
                self.drain_entity_outbox(&authenticated).await;
                self.drain_lifecycle_outbox(&authenticated).await;
                if memory_ready {
                    self.drain_memory_contribution_outbox(&authenticated).await;
                }
                if retrieval_ready {
                    self.drain_personal_agent_retrieval_outbox(&authenticated)
                        .await;
                }
                self.refresh_computed_capability_overlay(&authenticated, &principal, &workspace)
                    .await;
                let refresh_due = next_widget_registration_refresh
                    .get(&key)
                    .is_none_or(|deadline| tokio::time::Instant::now() >= *deadline);
                if refresh_due {
                    if let Err(error) = self
                        .widget_runtime
                        .refresh_scope_registrations(&authenticated, Utc::now())
                        .await
                    {
                        tracing::warn!(
                            principal,
                            workspace,
                            error = %error,
                            "app widget registration refresh will retry"
                        );
                    }
                    next_widget_registration_refresh.insert(
                        key.clone(),
                        tokio::time::Instant::now() + APP_WIDGET_REGISTRATION_REFRESH,
                    );
                }
                if let Err(error) = self
                    .widget_runtime
                    .evaluate_due_indicators(
                        &authenticated,
                        magician::magician_v2::apps::widget_runtime::APP_INDICATOR_MAX_EVALUATIONS_PER_TICK,
                        Utc::now(),
                    )
                    .await
                {
                    tracing::warn!(
                        principal,
                        workspace,
                        error = %error,
                        "app indicator due-work evaluation will retry"
                    );
                }
            }
            tokio::time::sleep(APP_PROJECTION_IDLE).await;
        }
    }

    async fn drain_entity_outbox(&self, authenticated: &AuthenticatedAppScope) {
        let Some(broadcaster) = self.event_broadcaster.as_ref() else {
            // Delivery is not complete without the runtime transport. Leaving
            // rows pending makes later broadcaster wiring/restart recover them.
            return;
        };
        let lease_owner = delivery_reference("app-entity-worker", &self.run_ref);
        let leases = match self
            .registry
            .claim_entity_outbox(
                authenticated,
                lease_owner,
                APP_PROJECTION_CLAIM_BATCH,
                APP_PROJECTION_LEASE,
                Utc::now(),
            )
            .await
        {
            Ok(leases) => leases,
            Err(error) => {
                tracing::warn!(error = %error, "app entity outbox claim failed");
                return;
            },
        };
        let mut leases = leases.into_iter();
        while let Some(lease) = leases.next() {
            let event_id = lease.event().event_id().clone();
            let delivery_receipt = delivery_reference("app-entity-delivery", &event_id);
            match self
                .entity_changes
                .signal_for_outbox_event(authenticated, lease.event(), Utc::now())
                .await
            {
                Ok(signal) => {
                    if let Some(signal) = signal {
                        emit_app_entity_change(broadcaster, authenticated, signal);
                    }
                    if let Err(error) = self
                        .registry
                        .acknowledge_entity_outbox(
                            authenticated,
                            lease,
                            delivery_receipt,
                            Utc::now(),
                        )
                        .await
                    {
                        tracing::warn!(event_id = %event_id, error = %error, "app entity outbox acknowledgement failed");
                    }
                },
                Err(error) => {
                    let delay = retry_delay(lease.attempt_count());
                    if let Err(release_error) = self
                        .registry
                        .release_entity_outbox(authenticated, lease, delay, Utc::now())
                        .await
                    {
                        tracing::warn!(event_id = %event_id, error = %release_error, "app entity outbox release failed");
                    }
                    tracing::warn!(event_id = %event_id, error = %error, "app entity outbox delivery will retry");
                    // Preserve sequence delivery: later claimed rows are made
                    // immediately available instead of overtaking this event.
                    for pending in leases {
                        if let Err(release_error) = self
                            .registry
                            .release_entity_outbox(
                                authenticated,
                                pending,
                                StdDuration::ZERO,
                                Utc::now(),
                            )
                            .await
                        {
                            tracing::warn!(error = %release_error, "app entity outbox ordered release failed");
                        }
                    }
                    break;
                },
            }
        }
    }

    async fn drain_lifecycle_outbox(&self, authenticated: &AuthenticatedAppScope) {
        let lease_owner = delivery_reference("app-publication-worker", &self.run_ref);
        let leases = match self
            .registry
            .claim_lifecycle_outbox(
                authenticated,
                lease_owner,
                APP_PROJECTION_CLAIM_BATCH,
                APP_PROJECTION_LEASE,
                Utc::now(),
            )
            .await
        {
            Ok(leases) => leases,
            Err(error) => {
                tracing::warn!(error = %error, "app lifecycle outbox claim failed");
                return;
            },
        };
        let mut leases = leases.into_iter();
        while let Some(lease) = leases.next() {
            let event_id = lease.event().event_id.clone();
            let delivery_receipt = delivery_reference("app-publication-delivery", &event_id);
            match self
                .app_publications
                .reconcile_lifecycle_event(authenticated, lease.event(), Utc::now())
                .await
            {
                Ok(_) => {
                    let _ = self.custom_surfaces.teardown_for_lifecycle_event(
                        &lease.event().installation_id,
                        lease.event().event_kind,
                    );
                    let _ = self.scripted_surfaces.teardown_for_lifecycle_event(
                        &lease.event().installation_id,
                        lease.event().event_kind,
                    );
                    let principal = authenticated.scope().principal.as_str();
                    let workspace = authenticated.scope().workspace.as_str();
                    self.computed_capabilities.evict(principal, workspace);
                    self.invalidate_scoped_capabilities(principal, workspace);
                    if let Err(error) = self
                        .registry
                        .acknowledge_lifecycle_outbox(
                            authenticated,
                            lease,
                            delivery_receipt,
                            Utc::now(),
                        )
                        .await
                    {
                        tracing::warn!(event_id = %event_id, error = %error, "app lifecycle outbox acknowledgement failed");
                    }
                },
                Err(error) => {
                    let delay = retry_delay(lease.attempt_count());
                    if let Err(release_error) = self
                        .registry
                        .release_lifecycle_outbox(authenticated, lease, delay, Utc::now())
                        .await
                    {
                        tracing::warn!(event_id = %event_id, error = %release_error, "app lifecycle outbox release failed");
                    }
                    tracing::warn!(event_id = %event_id, error = %error, "app lifecycle publication delivery will retry");
                    for pending in leases {
                        if let Err(release_error) = self
                            .registry
                            .release_lifecycle_outbox(
                                authenticated,
                                pending,
                                StdDuration::ZERO,
                                Utc::now(),
                            )
                            .await
                        {
                            tracing::warn!(error = %release_error, "app lifecycle outbox ordered release failed");
                        }
                    }
                    break;
                },
            }
        }
    }

    async fn refresh_computed_capability_overlay(
        &self,
        authenticated: &AuthenticatedAppScope,
        principal: &str,
        workspace: &str,
    ) {
        match self
            .registry
            .computed_capability_scope_overlay(authenticated, Utc::now())
            .await
        {
            Ok(overlay) => {
                if self
                    .computed_capabilities
                    .replace(principal, workspace, overlay)
                {
                    self.invalidate_scoped_capabilities(principal, workspace);
                }
            },
            Err(error) => tracing::warn!(
                principal,
                workspace,
                error = %error,
                "app computed-capability overlay refresh failed"
            ),
        }
    }

    async fn drain_memory_contribution_outbox(&self, authenticated: &AuthenticatedAppScope) {
        let lease_owner = delivery_reference("app-memory-worker", &self.run_ref).to_string();
        match self
            .memory_contributions
            .drain_scope(authenticated, &lease_owner, Utc::now())
            .await
        {
            Ok(report) => {
                if report.proposals_applied != 0 || report.invalidations_applied != 0 {
                    tracing::info!(
                        proposals = report.proposals_applied,
                        invalidations = report.invalidations_applied,
                        "app memory contribution projection advanced"
                    );
                }
            },
            Err(error) => tracing::warn!(
                error = %error,
                "app memory contribution projection will retry"
            ),
        }
    }

    async fn drain_personal_agent_retrieval_outbox(&self, authenticated: &AuthenticatedAppScope) {
        let Some(retrieval) = self.personal_agent_retrieval.as_ref() else {
            return;
        };
        let lease_owner =
            delivery_reference("app-personal-agent-retrieval-worker", &self.run_ref).to_string();
        match retrieval
            .drain_scope(authenticated, &lease_owner, Utc::now())
            .await
        {
            Ok(report) => {
                if report.proposals_applied != 0
                    || report.invalidations_applied != 0
                    || report.expirations_applied != 0
                {
                    tracing::info!(
                        proposals = report.proposals_applied,
                        invalidations = report.invalidations_applied,
                        expirations = report.expirations_applied,
                        "personal-agent retrieval projection advanced"
                    );
                }
            },
            Err(error) => tracing::warn!(
                error = %error,
                "personal-agent retrieval projection will retry"
            ),
        }
    }

    fn invalidate_scoped_capabilities(&self, principal: &str, workspace: &str) {
        if let Some(resolver) = self
            .scoped_capabilities
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            let _ = resolver.invalidate_scope(principal, workspace);
        }
    }
}

fn system_worker_scope(
    principal: &str,
    workspace: &str,
    run_ref: &AppReference,
    now: chrono::DateTime<Utc>,
) -> Result<AuthenticatedAppScope, String> {
    system_worker_scope_for_actor(principal, workspace, "worker:app-projection", run_ref, now)
}

fn background_behavior_worker_scope(
    principal: &str,
    workspace: &str,
    run_ref: &AppReference,
    now: chrono::DateTime<Utc>,
) -> Result<AuthenticatedAppScope, String> {
    system_worker_scope_for_actor(
        principal,
        workspace,
        "worker:app-background-behavior",
        run_ref,
        now,
    )
}

pub(crate) fn system_worker_scope_for_actor(
    principal: &str,
    workspace: &str,
    actor_ref: &str,
    run_ref: &AppReference,
    now: chrono::DateTime<Utc>,
) -> Result<AuthenticatedAppScope, String> {
    let scope = AppScope {
        principal: AppReference::parse(principal.to_owned()).map_err(|error| error.to_string())?,
        workspace: AppReference::parse(workspace.to_owned()).map_err(|error| error.to_string())?,
    };
    let digest = AppDigest::blake3(format!("{principal}\0{workspace}").as_bytes());
    let scope_binding_ref = AppScopeBindingRef::parse(format!(
        "scope_{}",
        digest.as_str().trim_start_matches("blake3:")
    ))
    .map_err(|error| error.to_string())?;
    AuthenticatedAppScope::from_system_worker(
        scope,
        scope_binding_ref,
        AppReference::parse(actor_ref).map_err(|error| error.to_string())?,
        run_ref.clone(),
        now,
        now + ChronoDuration::minutes(10),
    )
    .map_err(|error| error.to_string())
}

fn delivery_reference(prefix: &str, source: &AppReference) -> AppReference {
    let digest = AppDigest::blake3(format!("{prefix}\0{source}").as_bytes());
    AppReference::parse(format!(
        "{prefix}:{}",
        digest.as_str().trim_start_matches("blake3:")
    ))
    .expect("digest-derived app projection reference is valid")
}

fn retry_delay(attempt_count: u32) -> StdDuration {
    let shift = attempt_count.saturating_sub(1).min(6);
    StdDuration::from_secs(1_u64.checked_shl(shift).unwrap_or(60).min(60))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppSurfaceHydrationQuery {
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    sort_field: Option<String>,
    #[serde(default)]
    sort_direction: Option<AppOrderDirection>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppEntityChangesQuery {
    #[serde(default)]
    after_change_sequence: u64,
    surface_revision: u64,
    #[serde(default = "default_app_entity_change_limit")]
    limit: usize,
}

fn default_app_entity_change_limit() -> usize {
    DEFAULT_APP_ENTITY_CHANGE_LIMIT
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppDirectoryHttpQuery {
    #[serde(default = "default_app_directory_section")]
    section: AppDirectorySection,
    #[serde(default)]
    pinned_target_kind: Option<AppDirectoryTargetKind>,
    #[serde(default)]
    search: Option<String>,
    #[serde(default = "default_app_directory_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
}

fn default_app_directory_section() -> AppDirectorySection {
    AppDirectorySection::Installed
}

fn default_app_directory_limit() -> usize {
    DEFAULT_APP_DIRECTORY_LIMIT
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct AppDirectActionContractResponse {
    action_id: AppName,
    input: AppManifestInputSchema,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppMemoryCandidateListQuery {
    #[serde(default)]
    status: Option<AppMemoryCandidateStatus>,
    #[serde(default = "default_app_memory_review_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
}

fn default_app_memory_review_limit() -> usize {
    DEFAULT_APP_MEMORY_REVIEW_LIMIT
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct AppMemoryCandidateListResponse {
    candidates: Vec<AppMemoryCandidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
    /// Compatibility signal for clients that have not adopted `next_cursor`.
    /// A true value always has a cursor when at least one row was returned.
    truncated: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppMemoryCandidateListCursor {
    version: u8,
    scope_binding_ref: AppScopeBindingRef,
    authentication_revision: AppRevision,
    status: Option<AppMemoryCandidateStatus>,
    updated_at: DateTime<Utc>,
    candidate_id: AppReference,
}

fn encode_app_memory_candidate_cursor(
    cursor: &AppMemoryCandidateListCursor,
) -> Result<String, serde_json::Error> {
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(cursor)?))
}

fn decode_app_memory_candidate_cursor(encoded: &str) -> Result<AppMemoryCandidateListCursor, ()> {
    if encoded.is_empty() || encoded.len() > MAX_APP_MEMORY_REVIEW_CURSOR_BYTES {
        return Err(());
    }
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| ())?;
    if bytes.len() > MAX_APP_MEMORY_REVIEW_CURSOR_BYTES {
        return Err(());
    }
    let cursor: AppMemoryCandidateListCursor = serde_json::from_slice(&bytes).map_err(|_| ())?;
    if cursor.version != 1 {
        return Err(());
    }
    Ok(cursor)
}

fn bounded_app_memory_candidate_list(
    candidates: Vec<AppMemoryCandidate>,
    requested: usize,
    scope_binding_ref: &AppScopeBindingRef,
    authentication_revision: AppRevision,
    status: Option<AppMemoryCandidateStatus>,
) -> Result<AppMemoryCandidateListResponse, serde_json::Error> {
    // Leave room for the response object and JSON separators. Each durable
    // candidate has already passed the app-contract document bound, but a page
    // must not multiply that bound without limit.
    // Reserve a full cursor/object budget as well as separators so the
    // candidate sum cannot push the final JSON envelope over 4 MiB.
    let mut serialized_bytes = 2_048_usize;
    let mut selected = Vec::with_capacity(requested.min(candidates.len()));
    let mut truncated = false;
    for candidate in candidates {
        if selected.len() == requested {
            truncated = true;
            break;
        }
        let candidate_bytes = serde_json::to_vec(&candidate)?.len();
        let Some(next_bytes) = serialized_bytes
            .checked_add(candidate_bytes)
            .and_then(|bytes| bytes.checked_add(1))
        else {
            truncated = true;
            break;
        };
        if next_bytes > APP_MEMORY_REVIEW_MAX_BYTES {
            truncated = true;
            break;
        }
        serialized_bytes = next_bytes;
        selected.push(candidate);
    }
    let next_cursor = if truncated {
        selected
            .last()
            .map(|candidate| {
                encode_app_memory_candidate_cursor(&AppMemoryCandidateListCursor {
                    version: 1,
                    scope_binding_ref: scope_binding_ref.clone(),
                    authentication_revision,
                    status,
                    updated_at: candidate.updated_at,
                    candidate_id: candidate.candidate_id.clone(),
                })
            })
            .transpose()?
    } else {
        None
    };
    Ok(AppMemoryCandidateListResponse {
        candidates: selected,
        next_cursor,
        truncated,
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppSlotSettingsHttpQuery {
    #[serde(default = "default_slot_assignment_limit")]
    assignment_limit: usize,
    #[serde(default)]
    assignment_cursor: Option<String>,
    #[serde(default = "default_slot_picker_limit")]
    picker_limit: usize,
    #[serde(default)]
    picker_cursor: Option<String>,
}

fn default_slot_assignment_limit() -> usize {
    DEFAULT_APP_SLOT_ASSIGNMENT_LIMIT
}

fn default_slot_picker_limit() -> usize {
    DEFAULT_APP_SLOT_PICKER_LIMIT
}

async fn app_slot_inventory_snapshot(
    api: &AppPlatformApi,
    authenticated: &AuthenticatedAppScope,
    now: DateTime<Utc>,
) -> Result<AppSlotInventorySnapshot, HttpResponse> {
    let resolver = api
        .slot_inventory
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .ok_or_else(|| {
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "app_slot_inventory_unavailable",
                "The current installed-widget inventory is not available.",
            )
        })?;
    let inventory = resolver
        .snapshot(authenticated, now)
        .await
        .map_err(app_slot_inventory_error_response)?;
    inventory
        .validate()
        .map_err(app_slot_assignment_error_response)?;
    Ok(inventory)
}

/// Read saved assignments once, then reopen only their installed packages.
/// No slot disk worker is held while waiting on package or registry workers.
async fn resolve_requested_app_slots(
    api: &AppPlatformApi,
    authenticated: &AuthenticatedAppScope,
    slot_ids: Vec<AppSlotId>,
    now: DateTime<Utc>,
) -> Result<
    magician::magician_v2::apps::slot_assignments::AppSlotResolutionBatchResponse,
    HttpResponse,
> {
    let permit = Arc::clone(&api.slot_assignment_slots)
        .acquire_owned()
        .await
        .map_err(|_| app_slot_overloaded_response())?;
    let store = api.slot_assignments.clone();
    let scope = authenticated.scope().clone();
    let snapshot = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        store.prepare_resolution(&scope, scope.principal.as_str(), &slot_ids)
    })
    .await
    .map_err(app_slot_worker_error)?
    .map_err(app_slot_assignment_error_response)?;
    let installation_ids = snapshot.installation_ids();
    if installation_ids.is_empty() {
        return snapshot
            .resolve(&AppSlotInventorySnapshot::empty())
            .map_err(app_slot_assignment_error_response);
    }
    let resolver = api
        .slot_inventory
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .ok_or_else(|| app_slot_inventory_error_response(AppSlotInventoryError::Unavailable))?;
    let inventory = resolver
        .snapshot_for_installations(authenticated, &installation_ids, now)
        .await
        .map_err(app_slot_inventory_error_response)?;
    snapshot
        .resolve(&inventory)
        .map_err(app_slot_assignment_error_response)
}

fn app_slot_inventory_error_response(error: AppSlotInventoryError) -> HttpResponse {
    match error {
        AppSlotInventoryError::Unavailable => api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "app_slot_inventory_unavailable",
            "The current installed-widget inventory is not available.",
        ),
        AppSlotInventoryError::Invalid(detail) => {
            tracing::error!(error = %detail, "app slot inventory resolver returned invalid state");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_slot_inventory_invalid",
                "The installed-widget inventory could not be validated.",
            )
        },
    }
}

fn app_slot_assignment_error_response(error: AppSlotAssignmentError) -> HttpResponse {
    let detail = error.to_string();
    match &error {
        AppSlotAssignmentError::Invalid(_) => api_error(
            StatusCode::BAD_REQUEST,
            "invalid_app_slot_assignment",
            &detail,
        ),
        AppSlotAssignmentError::RevisionConflict { .. }
        | AppSlotAssignmentError::FenceLost { .. } => api_error(
            StatusCode::CONFLICT,
            "app_slot_assignment_conflict",
            "The slot settings changed; reload them before retrying this mutation.",
        ),
        AppSlotAssignmentError::WidgetUnavailable => api_error(
            StatusCode::CONFLICT,
            "app_slot_widget_unavailable",
            "The widget is not an enabled current picker candidate.",
        ),
        AppSlotAssignmentError::RevisionExhausted
        | AppSlotAssignmentError::FenceExhausted
        | AppSlotAssignmentError::Corrupt(_)
        | AppSlotAssignmentError::Storage(_) => {
            tracing::error!(error = %error, "app slot assignment store failed");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_slot_assignment_failed",
                "The slot settings could not be completed safely.",
            )
        },
    }
}

fn app_slot_worker_error(error: tokio::task::JoinError) -> HttpResponse {
    tracing::error!(error = %error, "app slot assignment worker terminated");
    api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "app_slot_assignment_worker_unavailable",
        "The slot settings worker is temporarily unavailable.",
    )
}

fn app_slot_overloaded_response() -> HttpResponse {
    api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "app_slot_assignment_worker_unavailable",
        "The slot settings worker is shutting down.",
    )
}

/// `GET /api/magician/v2/apps/slots/{slot_id}`
async fn resolve_app_slot_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let slot_id = match AppSlotId::parse(path.into_inner()) {
        Ok(slot_id) => slot_id,
        Err(error) => return app_slot_assignment_error_response(error),
    };
    match resolve_requested_app_slots(&api, &authenticated, vec![slot_id], now).await {
        Ok(mut response) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(response.assignments.remove(0)),
        Err(response) => response,
    }
}

/// `POST /api/magician/v2/apps/slots/resolve-batch`
///
/// Resolves up to twelve exact page regions with one current-inventory
/// snapshot and one slot-state load. Response order always matches request
/// order; repeated slots are rejected rather than multiplying work.
async fn resolve_app_slots_batch_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let bytes = match read_bounded_raw_body(
        &req,
        &mut payload,
        "application/json",
        APP_SLOT_RESOLUTION_BATCH_MAX_REQUEST_BYTES,
        "app_slot_resolution_batch",
    )
    .await
    {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };
    let request: AppSlotResolutionBatchRequest = match serde_json::from_slice(&bytes) {
        Ok(request) => request,
        Err(_) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_app_slot_resolution_batch",
                "The slot resolution batch is invalid.",
            )
        },
    };
    if let Err(error) = request.validate_app_contract(&AppContractLimits::default()) {
        return contract_error_response(error);
    }
    match resolve_requested_app_slots(&api, &authenticated, request.slot_ids, now).await {
        Ok(response) => {
            let bytes = match serde_json::to_vec(&response) {
                Ok(bytes) if bytes.len() <= APP_SLOT_RESOLUTION_BATCH_MAX_RESPONSE_BYTES => bytes,
                Ok(_) => {
                    return api_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "app_slot_resolution_response_too_large",
                        "The bounded slot response could not be produced safely.",
                    )
                },
                Err(error) => {
                    tracing::error!(error = %error, "encoding app slot resolution batch failed");
                    return api_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "app_slot_resolution_encoding_failed",
                        "The bounded slot response could not be encoded.",
                    );
                },
            };
            HttpResponse::Ok()
                .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
                .insert_header((actix_web::http::header::CONTENT_TYPE, "application/json"))
                .body(bytes)
        },
        Err(response) => response,
    }
}

/// `GET /api/magician/v2/apps/slot-assignments`
///
/// This is both the bounded settings/picker read and write-fence acquisition.
/// Opening a newer editor supersedes older editors without changing the layout
/// revision; the subsequent POST must present both returned numbers.
async fn list_app_slot_assignments_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    query: Result<web::Query<AppSlotSettingsHttpQuery>, actix_web::Error>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let query = match query {
        Ok(query) => query.into_inner(),
        Err(_) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_app_slot_settings_query",
                "The slot settings query is invalid.",
            )
        },
    };
    let query = AppSlotSettingsQuery {
        assignment_limit: query.assignment_limit,
        assignment_cursor: query.assignment_cursor,
        picker_limit: query.picker_limit,
        picker_cursor: query.picker_cursor,
    };
    if let Err(error) = query.validate() {
        return app_slot_assignment_error_response(error);
    }
    let permit = match Arc::clone(&api.slot_assignment_slots).acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => return app_slot_overloaded_response(),
    };
    let inventory = match app_slot_inventory_snapshot(&api, &authenticated, now).await {
        Ok(inventory) => inventory,
        Err(response) => return response,
    };
    let store = api.slot_assignments.clone();
    let scope = authenticated.scope().clone();
    let user_ref = scope.principal.to_string();
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        store.acquire_settings_page(&scope, &user_ref, &query, &inventory)
    })
    .await
    {
        Ok(Ok(page)) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(page),
        Ok(Err(error)) => app_slot_assignment_error_response(error),
        Err(error) => app_slot_worker_error(error),
    }
}

/// `POST /api/magician/v2/apps/slot-assignments`
async fn mutate_app_slot_assignment_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let request = match read_bounded_app_contract::<AppSlotAssignmentWriteRequest>(
        &req,
        &mut payload,
    )
    .await
    {
        Ok(request) => request,
        Err(response) => return response,
    };
    let permit = match Arc::clone(&api.slot_assignment_slots).acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => return app_slot_overloaded_response(),
    };
    let inventory = match app_slot_inventory_snapshot(&api, &authenticated, now.clone()).await {
        Ok(inventory) => inventory,
        Err(response) => return response,
    };
    let store = api.slot_assignments.clone();
    let scope = authenticated.scope().clone();
    let user_ref = scope.principal.to_string();
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        store.apply_user_assignment(&scope, &user_ref, &request, &inventory, now)
    })
    .await
    {
        Ok(Ok(receipt)) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(receipt),
        Ok(Err(error)) => app_slot_assignment_error_response(error),
        Err(error) => app_slot_worker_error(error),
    }
}

/// `GET /api/magician/v2/apps/directory`
async fn app_directory_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    query: web::Query<AppDirectoryHttpQuery>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let query = query.into_inner();
    match api
        .app_directory
        .list(
            &authenticated,
            AppDirectoryQuery {
                section: query.section,
                pinned_target_kind: query.pinned_target_kind,
                search: query.search,
                limit: query.limit,
                cursor: query.cursor,
            },
            now,
        )
        .await
    {
        Ok(page) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(page),
        Err(error) => app_directory_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/directory-activity`
pub async fn app_directory_activity_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(installation_id) => installation_id,
        Err(error) => return contract_error_response(error),
    };
    let activity =
        match read_bounded_app_contract::<AppDirectoryActivityRequest>(&req, &mut payload).await {
            Ok(activity) => activity,
            Err(response) => return response,
        };
    match api
        .app_directory
        .record_activity(&authenticated, &installation_id, activity, now)
        .await
    {
        Ok(receipt) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(receipt),
        Err(error) => app_directory_error_response(error),
    }
}

/// `GET /api/magician/v2/apps/installations/{installation_id}/entity-changes`
async fn app_entity_changes_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: Result<web::Query<AppEntityChangesQuery>, actix_web::Error>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(installation_id) => installation_id,
        Err(error) => return contract_error_response(error),
    };
    let query = match query {
        Ok(query) => query,
        Err(_) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_app_contract",
                "The entity-change query does not match the supported public contract.",
            );
        },
    };
    let surface_revision = match AppRevision::new(query.surface_revision) {
        Ok(revision) => revision,
        Err(error) => return contract_error_response(error),
    };
    match api
        .entity_changes
        .read_after(
            &authenticated,
            &installation_id,
            surface_revision,
            query.after_change_sequence,
            query.limit,
            now,
        )
        .await
    {
        Ok(batch) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(batch),
        Err(error) => entity_change_error_response(error),
    }
}

/// `GET /api/magician/v2/apps/installations/{installation_id}/surfaces[/<route>]`
async fn hydrate_app_surface_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    query: web::Query<AppSurfaceHydrationQuery>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    // Percent-decoding before validation could turn an encoded slash or dot
    // segment into a different route. App routes intentionally require their
    // canonical portable-ASCII representation on the wire.
    if req.uri().path().contains('%') {
        return api_error(
            StatusCode::NOT_FOUND,
            "app_surface_not_found",
            "The app surface does not exist in this authenticated scope.",
        );
    }
    let Some(raw_installation_id) = req.match_info().get("installation_id") else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_app_contract",
            "The app installation id is missing.",
        );
    };
    let installation_id = match AppInstallationId::parse(raw_installation_id.to_owned()) {
        Ok(installation_id) => installation_id,
        Err(error) => return contract_error_response(error),
    };
    let requested_route = req
        .match_info()
        .get("surface_tail")
        .filter(|tail| !tail.is_empty())
        .map_or_else(|| "/".to_owned(), |tail| format!("/{tail}"));
    let cursor = match query
        .cursor
        .as_ref()
        .map(|value| AppReference::parse(value.to_owned()))
        .transpose()
    {
        Ok(cursor) => cursor,
        Err(error) => return contract_error_response(error),
    };
    let sort = match (&query.sort_field, query.sort_direction) {
        (None, None) => None,
        (Some(field), Some(direction)) => match AppName::parse(field.clone()) {
            Ok(field) => Some((field, direction)),
            Err(error) => return contract_error_response(error),
        },
        _ => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_app_surface_read_intent",
                "Sort field and direction must be supplied together.",
            );
        },
    };
    match api
        .surface_hydration
        .hydrate(
            &authenticated,
            &installation_id,
            &requested_route,
            cursor,
            sort,
            now.clone(),
        )
        .await
    {
        Ok(hydration) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(hydration),
        Err(error) => surface_hydration_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/data/query`
pub async fn query_app_data_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(installation_id) => installation_id,
        Err(error) => return contract_error_response(error),
    };
    let request = match read_bounded_app_contract::<AppQueryRequest>(&req, &mut payload).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    if request.source_installation_id != installation_id {
        return api_error(
            StatusCode::BAD_REQUEST,
            "app_installation_mismatch",
            "The query source installation must match the authenticated route.",
        );
    }
    match api
        .entity_adapter
        .owner_query(&authenticated, request, now)
        .await
    {
        Ok(page) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(page),
        Err(error) => entity_adapter_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/data/mutations`
pub async fn mutate_app_data_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(installation_id) => installation_id,
        Err(error) => return contract_error_response(error),
    };
    let command = match read_bounded_app_contract::<AppMutationCommand>(&req, &mut payload).await {
        Ok(command) => command,
        Err(response) => return response,
    };
    match api
        .entity_adapter
        .owner_mutate(&authenticated, &installation_id, command, now.clone())
        .await
    {
        Ok(receipt) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(receipt),
        Err(error) => entity_adapter_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/actions/{action_id}/invocations`
pub async fn invoke_app_action_handler(
    api: web::Data<AppPlatformApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let (installation_id, action_id) = path.into_inner();
    let installation_id = match AppInstallationId::parse(installation_id) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let action_id = match AppName::parse(action_id) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let mut invocation = match read_bounded_app_contract::<AppActionInvocation<serde_json::Value>>(
        &req,
        &mut payload,
    )
    .await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    if invocation.action_id != action_id {
        return api_error(
            StatusCode::BAD_REQUEST,
            "app_action_mismatch",
            "The action in the invocation must match the authenticated route.",
        );
    }
    invocation.caller_surface_or_execution_ref =
        match AppReference::parse(APP_HTTP_ACTION_CALLER_REF) {
            Ok(value) => value,
            Err(error) => return contract_error_response(error),
        };
    match api
        .workflow_service()
        .invoke(
            &authenticated,
            &installation_id,
            invocation,
            resources.get_ref().as_ref(),
            now,
        )
        .await
    {
        Ok(launch) => HttpResponse::Accepted()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(launch),
        Err(error) => workflow_error_response(error),
    }
}

/// `GET /api/magician/v2/apps/installations/{installation_id}/actions/{action_id}/contract`
///
/// This returns package-owned input metadata only. Mutable authority fields are
/// never projected to the client as values it must copy back.
pub async fn get_direct_app_action_contract_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let (installation_id, action_id) = path.into_inner();
    let installation_id = match AppInstallationId::parse(installation_id) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let action_id = match AppName::parse(action_id) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    match api
        .workflow_service()
        .direct_action_input_schema(&authenticated, &installation_id, &action_id, now)
        .await
    {
        Ok(input) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(AppDirectActionContractResponse { action_id, input }),
        Err(error) => workflow_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/actions/{action_id}/launch`
///
/// Owner clients provide validated input, an idempotency key, and optionally
/// the exact installation generation/package revision observed by a widget.
/// The workflow owner stamps current installation/schema/grant/action evidence
/// and canonical provenance before using the same admission path as the full
/// data plane invocation endpoint.
pub async fn launch_direct_app_action_handler(
    api: web::Data<AppPlatformApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    payload: web::Payload,
) -> HttpResponse {
    match launch_direct_app_action(api, resources, req, path, payload).await {
        Ok(launch) => HttpResponse::Accepted()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(launch),
        Err(response) => response,
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/actions/{action_id}/runs`
///
/// Supported-public counterpart of the legacy `/launch` adapter. It omits the
/// internal Artifact task id; the opaque logical run handle is the only client
/// correlation and control identity.
pub async fn launch_public_app_action_handler(
    api: web::Data<AppPlatformApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    payload: web::Payload,
) -> HttpResponse {
    match launch_direct_app_action(api, resources, req, path, payload).await {
        Ok(launch) => HttpResponse::Accepted()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(AppActionLaunchResponse {
                run_handle: launch.run_handle,
                execution_id: launch.execution_id,
                result: launch.result,
            }),
        Err(response) => response,
    }
}

async fn launch_direct_app_action(
    api: web::Data<AppPlatformApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    mut payload: web::Payload,
) -> Result<AppWorkflowLaunch, HttpResponse> {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return Err(response),
    };
    let (installation_id, action_id) = path.into_inner();
    let installation_id = match AppInstallationId::parse(installation_id) {
        Ok(value) => value,
        Err(error) => return Err(contract_error_response(error)),
    };
    let action_id = match AppName::parse(action_id) {
        Ok(value) => value,
        Err(error) => return Err(contract_error_response(error)),
    };
    let request =
        match read_bounded_app_contract::<AppDirectActionRequest>(&req, &mut payload).await {
            Ok(value) => value,
            Err(response) => return Err(response),
        };
    // Both HTTP action adapters share one route-owned provenance identity so
    // an SDK/UI fallback retry with the same idempotency key reaches the same
    // durable binding instead of conflicting on a fictitious client surface.
    let caller = match AppReference::parse(APP_HTTP_ACTION_CALLER_REF) {
        Ok(value) => value,
        Err(error) => return Err(contract_error_response(error)),
    };
    // The entire app-workflow launch state machine hangs off this one call.
    // Awaited inline it is stored in this future, which is stored in the
    // handler's, which is stored in the route's, which is stored in Actix's
    // handler service — the same object reserved in four consecutive frames of
    // a 2 MiB HTTP worker stack. Measured at 241,664 bytes per frame, it is
    // most of the budget before the workflow service is even entered. Boxing
    // keeps it on the heap so only a pointer rides the route chain.
    Box::pin(api.workflow_service().invoke_direct_input(
        &authenticated,
        &installation_id,
        &action_id,
        request.idempotency_key,
        request.input,
        request.expected_installation_binding,
        caller,
        resources.get_ref().as_ref(),
        now,
    ))
    .await
    .map_err(workflow_error_response)
}

/// `GET /api/magician/v2/apps/action-runs/{run_ref}`
pub async fn get_app_action_run_handler(
    api: web::Data<AppPlatformApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let run_locator = match supported_public_run_locator(path.into_inner()) {
        Ok(run_locator) => run_locator,
        Err(response) => return response,
    };
    let control = match api
        .workflow_service()
        .resolve_run_control(&authenticated, run_locator.as_str(), now)
        .await
    {
        Ok(control) => control,
        Err(error) => return workflow_error_response(error),
    };
    let run_handle = control.run_handle().clone();
    let task_id = control.task_id().to_owned();
    let Some(task_service) = resources.artifact_v2_service.as_ref() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "app_task_runtime_unavailable",
            "The app task runtime is unavailable.",
        );
    };
    let scope = ScopeRef::system_internal_unauthenticated(
        &authenticated.scope().principal.to_string(),
        &authenticated.scope().workspace.to_string(),
    );
    let task = match task_service.get_task(&scope, &task_id).await {
        Ok(task) => task,
        Err(error) => return workflow_error_response(error.into()),
    };
    let recurring_execution = if let Some(id) = control.recurring_execution_id() {
        match task_service.get_execution(&scope, &task_id, id).await {
            Ok(execution) => Some(execution),
            Err(magician::magician_v2::artifact_v2::ArtifactV2Error::ExecutionNotFound(_)) => None,
            Err(error) => return workflow_error_response(error.into()),
        }
    } else {
        None
    };
    let fallback_status = if control.recurring_execution_id().is_some() {
        "ready"
    } else {
        task.state.status.as_str()
    };
    let status = recurring_execution
        .as_ref()
        .map_or(fallback_status, |execution| execution.state.status.as_str());
    let Some(mut artifact_status) = app_run_status_from_task(status) else {
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "app_run_state_invalid",
            "The app task has an unsupported lifecycle state.",
        );
    };
    let execution_id = if let Some(id) = control.recurring_execution_id() {
        Some(id.to_owned())
    } else if artifact_status.is_terminal() {
        task.state.latest_root_execution_id
    } else {
        task.state.active_root_execution_id
    };
    let cancellation_generation = if let Some(execution_id) = execution_id.as_deref() {
        match api
            .workflow_service()
            .action_cancellation_projection_for_control(&control, execution_id)
            .await
        {
            Ok(Some((receipt, uncertain))) => {
                if uncertain {
                    artifact_status = AppRunStatus::Uncertain;
                } else if !artifact_status.is_terminal() {
                    artifact_status = AppRunStatus::Cancelling;
                }
                Some(receipt.generation)
            },
            Ok(None) => None,
            Err(error) => return workflow_error_response(error),
        }
    } else {
        None
    };

    // Lifecycle status is owned by the control proof above and remains
    // readable after app/source revocation. The sidecar remains the
    // authoritative completion fence and is checked on every poll; the
    // stronger A+B disclosure work runs only when that sidecar exists. A
    // policy denial withholds bytes without erasing terminal status.
    match api
        .workflow_service()
        .result_for_control(&authenticated, control)
        .await
    {
        Ok(read) => {
            let (current_handle, current_task_id, result) = read.into_parts();
            if current_handle != run_handle || current_task_id != task_id {
                return api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "app_run_identity_changed",
                    "The app run identity changed while reading its terminal result.",
                );
            }
            if let Some(result) = result {
                return app_run_snapshot_response(
                    match app_run_snapshot(
                        current_handle,
                        execution_id,
                        AppRunStatus::Completed,
                        cancellation_generation,
                        false,
                        Some(result),
                    ) {
                        Ok(snapshot) => snapshot,
                        Err(response) => return response,
                    },
                );
            }
            if artifact_status == AppRunStatus::Completed {
                return api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "app_terminal_result_missing",
                    "The app task completed without its required typed terminal result.",
                );
            }
            app_run_snapshot_response(
                match app_run_snapshot(
                    current_handle,
                    execution_id,
                    artifact_status,
                    cancellation_generation,
                    false,
                    None,
                ) {
                    Ok(snapshot) => snapshot,
                    Err(response) => return response,
                },
            )
        },
        Err(error) if error.is_result_disclosure_denial() => app_run_snapshot_response(
            match app_run_snapshot(
                run_handle,
                execution_id,
                AppRunStatus::Completed,
                cancellation_generation,
                true,
                None,
            ) {
                Ok(snapshot) => snapshot,
                Err(response) => return response,
            },
        ),
        Err(error) => workflow_error_response(error),
    }
}

/// Owner maintenance for obsolete scheduled task shells. This is not an App
/// capability and cannot delete the persistent recurring task or entity data.
pub async fn delete_legacy_scheduled_app_task_handler(
    api: web::Data<AppPlatformApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let locator = match supported_public_run_locator(path.into_inner()) {
        Ok(locator) => locator,
        Err(response) => return response,
    };
    let control = match api
        .workflow_service()
        .resolve_run_control(&authenticated, locator.as_str(), now)
        .await
    {
        Ok(control) => control,
        Err(error) => return workflow_error_response(error),
    };
    let Some(service) = resources.artifact_v2_service.as_ref() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "app_task_runtime_unavailable",
            "The app task runtime is unavailable.",
        );
    };
    let task_id = control.task_id().to_owned();
    let service = Arc::clone(service);
    let deleted =
        magician::magician_v2::execution::runtime_boundary::run_execution_job(move || async move {
            service.delete_legacy_scheduled_app_task(&control).await
        })
        .await
        .unwrap_or_else(|error| {
            Err(magician::magician_v2::artifact_v2::ArtifactV2Error::Runtime(error.to_string()))
        });
    match deleted {
        Ok(()) => HttpResponse::Ok().json(serde_json::json!({"deleted_task_id": task_id})),
        Err(error) => workflow_error_response(error.into()),
    }
}

fn supported_public_run_locator(raw: String) -> Result<AppReference, HttpResponse> {
    let run_ref = AppReference::parse(raw).map_err(contract_error_response)?;
    if !has_canonical_app_action_run_namespace(run_ref.as_str()) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "invalid_app_contract",
            "Supported-public action runs require a canonical run:app-action: reference.",
        ));
    }
    Ok(run_ref)
}

/// `POST /api/magician/v2/apps/action-runs/{run_ref}/compositions`
///
/// The source result remains inside the workflow/composition owners. The
/// authenticated client supplies only the destination, a bounded declarative
/// mapping and an idempotency key; both current app bindings and schemas are
/// reconstructed before the destination launch.
pub async fn compose_app_action_run_handler(
    api: web::Data<AppPlatformApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let source_run_ref = match supported_public_run_locator(path.into_inner()) {
        Ok(run_ref) => run_ref,
        Err(response) => return response,
    };
    let request =
        match read_bounded_app_contract::<AppActionCompositionRequest>(&req, &mut payload).await {
            Ok(request) => request,
            Err(response) => return response,
        };
    let service = AppCompositionService::new(api.workflow_service());
    match service
        .compose_action_result_for_owner(
            &authenticated,
            source_run_ref,
            request,
            resources.get_ref().as_ref(),
            now,
        )
        .await
    {
        Ok(composition) => {
            let mut response = match &composition.result {
                AppActionResultComposition::Waiting { .. }
                | AppActionResultComposition::Launched { .. } => HttpResponse::Accepted(),
                AppActionResultComposition::SourceTerminal { .. }
                | AppActionResultComposition::Unavailable { .. } => HttpResponse::Ok(),
            };
            response
                .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
                .json(composition)
        },
        Err(error) => app_composition_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/action-runs/{run_ref}/cancel`
///
/// Cancellation first authenticates the task as an app workflow in this exact
/// scope, then persists the generation-bound intent through the canonical
/// Artifact task owner. `Cancelling` means only that the intent is durable;
/// `Cancelled` requires the protected pre-I/O proof and canonical terminal
/// commit. A post-admission stop preserves the actual terminal outcome or
/// projects outcome uncertainty.
pub async fn cancel_app_action_run_handler(
    api: web::Data<AppPlatformApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let run_locator = match supported_public_run_locator(path.into_inner()) {
        Ok(run_locator) => run_locator,
        Err(response) => return response,
    };
    let body = match read_bounded_raw_body(
        &req,
        &mut payload,
        "application/json",
        APP_ACTION_CANCELLATION_BODY_BYTES,
        "app_action_cancellation",
    )
    .await
    {
        Ok(body) => body,
        Err(response) => return response,
    };
    let request: AppActionCancellationRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(_) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_app_action_cancellation",
                "The app action cancellation request is invalid.",
            );
        },
    };
    let control = match api
        .workflow_service()
        .resolve_run_control(&authenticated, run_locator.as_str(), now)
        .await
    {
        Ok(control) => control,
        Err(error) => return workflow_error_response(error),
    };
    let Some(task_service) = resources.artifact_v2_service.as_ref() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "app_task_runtime_unavailable",
            "The app task runtime is unavailable.",
        );
    };
    // Cancellation can settle the full Artifact/resource tree after publishing
    // its durable intent. Construct that work at a fresh scheduler root just
    // like execution admission, rather than below the Actix request poll chain.
    let task_service = Arc::clone(task_service);
    let cancellation =
        magician::magician_v2::execution::runtime_boundary::run_execution_job(move || async move {
            task_service.cancel_app_action_run(&control, &request).await
        })
        .await
        .unwrap_or_else(|error| {
            Err(
                magician::magician_v2::artifact_v2::ArtifactV2Error::Runtime(format!(
                    "app action cancellation worker failed to join: {error}"
                )),
            )
        });
    match cancellation {
        Ok((receipt, _task)) => {
            let mut response = if receipt.status == AppRunStatus::Cancelled {
                HttpResponse::Ok()
            } else {
                HttpResponse::Accepted()
            };
            response
                .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
                .json(receipt)
        },
        Err(magician::magician_v2::artifact_v2::ArtifactV2Error::InvalidRequest(message))
            if message == "app_action_already_terminal" =>
        {
            api_error(
                StatusCode::CONFLICT,
                "app_action_already_completed",
                "The app action is already terminal.",
            )
        },
        Err(magician::magician_v2::artifact_v2::ArtifactV2Error::InvalidRequest(message))
            if message == "app_action_not_started" =>
        {
            api_error(
                StatusCode::CONFLICT,
                "app_action_not_started",
                "The app action has no execution to cancel.",
            )
        },
        Err(magician::magician_v2::artifact_v2::ArtifactV2Error::InvalidRequest(message))
            if message == "app_action_cancellation_conflict" =>
        {
            api_error(
                StatusCode::CONFLICT,
                "app_action_cancellation_conflict",
                "A different cancellation request already owns this app run.",
            )
        },
        Err(error) => workflow_error_response(error.into()),
    }
}

/// `GET /api/magician/v2/apps/action-runs/{run_ref}/interactive-state`
///
/// First-party payload-free inspection only. This route is deliberately not
/// one of the seven supported-public SDK operations and never returns captured
/// content, provider handles, device/package IDs or raw owner coordinates.
pub async fn get_app_action_interactive_state_handler(
    api: web::Data<AppPlatformApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let run_locator = match supported_public_run_locator(path.into_inner()) {
        Ok(run_locator) => run_locator,
        Err(response) => return response,
    };
    let service = api.workflow_service();
    let control = match service
        .resolve_run_control(&authenticated, run_locator.as_str(), now)
        .await
    {
        Ok(control) => control,
        Err(error) => return workflow_error_response(error),
    };
    let Some(task_service) = resources.artifact_v2_service.as_ref() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "app_task_runtime_unavailable",
            "The app task runtime is unavailable.",
        );
    };
    let scope = ScopeRef::system_internal_unauthenticated(
        &authenticated.scope().principal.to_string(),
        &authenticated.scope().workspace.to_string(),
    );
    let task = match task_service.get_task(&scope, control.task_id()).await {
        Ok(task) => task,
        Err(error) => return workflow_error_response(error.into()),
    };
    let Some(run_status) = app_run_status_from_task(&task.state.status) else {
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "app_run_state_invalid",
            "The app task has an unsupported lifecycle state.",
        );
    };
    let Some(execution_id) = task
        .state
        .active_root_execution_id
        .as_deref()
        .or(task.state.latest_root_execution_id.as_deref())
    else {
        return api_error(
            StatusCode::CONFLICT,
            "app_action_not_started",
            "The app action has no execution to inspect.",
        );
    };
    match service
        .interactive_state_for_control(&control, execution_id, now)
        .await
    {
        Ok(mut snapshot) => {
            if run_status.is_terminal() {
                snapshot.apply_terminal_run_truth();
            }
            HttpResponse::Ok()
                .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
                .json(snapshot)
        },
        Err(error) => workflow_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/action-runs/{run_ref}/interactive-stop`
///
/// The durable workflow-control receipt is committed before signalling the
/// process-local physical owner. Success means `stop_requested`, never that a
/// session has already stopped; terminal truth remains in settlement audit.
pub async fn stop_app_action_interactive_session_handler(
    api: web::Data<AppPlatformApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let run_locator = match supported_public_run_locator(path.into_inner()) {
        Ok(run_locator) => run_locator,
        Err(response) => return response,
    };
    let body = match read_bounded_raw_body(
        &req,
        &mut payload,
        "application/json",
        APP_INTERACTIVE_STOP_BODY_BYTES,
        "app_interactive_stop",
    )
    .await
    {
        Ok(body) => body,
        Err(response) => return response,
    };
    let request: AppInteractiveStopRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(_) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_app_interactive_stop",
                "The interactive stop request is invalid.",
            );
        },
    };
    let service = api.workflow_service();
    let control = match service
        .resolve_run_control(&authenticated, run_locator.as_str(), now)
        .await
    {
        Ok(control) => control,
        Err(error) => return workflow_error_response(error),
    };
    let Some(task_service) = resources.artifact_v2_service.as_ref() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "app_task_runtime_unavailable",
            "The app task runtime is unavailable.",
        );
    };
    let scope = ScopeRef::system_internal_unauthenticated(
        &authenticated.scope().principal.to_string(),
        &authenticated.scope().workspace.to_string(),
    );
    let task = match task_service.get_task(&scope, control.task_id()).await {
        Ok(task) => task,
        Err(error) => return workflow_error_response(error.into()),
    };
    let Some(_run_status) = app_run_status_from_task(&task.state.status) else {
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "app_run_state_invalid",
            "The app task has an unsupported lifecycle state.",
        );
    };
    let Some(execution_id) = task
        .state
        .active_root_execution_id
        .as_deref()
        .or(task.state.latest_root_execution_id.as_deref())
    else {
        return api_error(
            StatusCode::CONFLICT,
            "app_action_not_started",
            "The app action has no execution to stop.",
        );
    };
    match service
        .request_interactive_stop(&control, execution_id, request)
        .await
    {
        Ok(receipt) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(receipt),
        Err(error) => workflow_error_response(error),
    }
}

/// `GET /api/magician/v2/apps/memory-candidates`
///
/// Returns a bounded, newest-first owner-review inbox. `status=proposed` is the
/// normal review view; settled rows remain inspectable without making list
/// membership an eligibility or prompt-inclusion decision.
async fn list_app_memory_candidates_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    query: web::Query<AppMemoryCandidateListQuery>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if query.limit == 0 || query.limit > MAX_APP_MEMORY_REVIEW_LIMIT {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_app_memory_review_limit",
            "The app-memory review limit must be between 1 and 16.",
        );
    }
    let after = match query.cursor.as_deref() {
        Some(encoded) => match decode_app_memory_candidate_cursor(encoded) {
            Ok(cursor)
                if cursor.scope_binding_ref == *authenticated.scope_binding_ref()
                    && cursor.authentication_revision
                        == authenticated.authentication_revision()
                    && cursor.status == query.status
                    && cursor.updated_at <= now =>
            {
                Some((cursor.updated_at, cursor.candidate_id))
            },
            _ => {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_app_memory_review_cursor",
                    "The app-memory review cursor is invalid for this scope and filter.",
                );
            },
        },
        None => None,
    };
    let fetch_limit = query.limit.saturating_add(1);
    match api
        .registry
        .list_app_memory_candidates(&authenticated, query.status, after, fetch_limit, now)
        .await
    {
        Ok(candidates) => match bounded_app_memory_candidate_list(
            candidates,
            query.limit,
            authenticated.scope_binding_ref(),
            authenticated.authentication_revision(),
            query.status,
        ) {
            Ok(page) => HttpResponse::Ok()
                .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
                .json(page),
            Err(_) => api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_memory_review_encoding_failed",
                "The app-memory review page could not be encoded.",
            ),
        },
        Err(error) => app_memory_store_error_response(error),
    }
}

/// `GET /api/magician/v2/apps/memory-candidates/{candidate_id}`
pub async fn get_app_memory_candidate_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let candidate_id = match AppReference::parse(path.into_inner()) {
        Ok(candidate_id) => candidate_id,
        Err(error) => return contract_error_response(error),
    };
    match api
        .registry
        .current_app_memory_candidate(&authenticated, candidate_id, now)
        .await
    {
        Ok(Some(candidate)) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(candidate),
        Ok(None) => api_error(
            StatusCode::NOT_FOUND,
            "app_memory_candidate_not_found",
            "The app-memory candidate does not exist in this authenticated scope.",
        ),
        Err(error) => app_memory_store_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/memory-candidates/{candidate_id}/{command}`
///
/// Only explicit owner accept/reject commands are transport-exposed. Runtime
/// stale/tombstone transitions remain store-owned consequences of source
/// mutation, disable, retention and purge.
pub async fn transition_app_memory_candidate_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let (candidate_id, command) = path.into_inner();
    let candidate_id = match AppReference::parse(candidate_id) {
        Ok(candidate_id) => candidate_id,
        Err(error) => return contract_error_response(error),
    };
    let command = match command.as_str() {
        "accept" => AppMemoryCandidateCommand::Accept,
        "reject" => AppMemoryCandidateCommand::Reject,
        _ => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_app_memory_command",
                "The app-memory command must be `accept` or `reject`.",
            );
        },
    };
    match api
        .registry
        .transition_app_memory_candidate(&authenticated, candidate_id, command, now)
        .await
    {
        Ok(candidate) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(candidate),
        Err(error) => app_memory_store_error_response(error),
    }
}

fn app_memory_store_error_response(error: AppMemoryStoreError) -> HttpResponse {
    match error {
        AppMemoryStoreError::Registry(error) => registry_error_response(error),
        AppMemoryStoreError::Contract(error) => contract_error_response(error),
        AppMemoryStoreError::CorruptCandidate(_) => api_error(
            StatusCode::NOT_FOUND,
            "app_memory_candidate_not_found",
            "The app-memory candidate does not exist in this authenticated scope.",
        ),
        AppMemoryStoreError::Eligibility(_)
        | AppMemoryStoreError::Lifecycle(_)
        | AppMemoryStoreError::Bridge(_)
        | AppMemoryStoreError::CompareAndSwapLost
        | AppMemoryStoreError::InvalidLifecycleTransition => api_error(
            StatusCode::CONFLICT,
            "app_memory_candidate_conflict",
            "The app-memory candidate is stale or cannot make that transition.",
        ),
        AppMemoryStoreError::Sqlite(_)
        | AppMemoryStoreError::Encoding(_)
        | AppMemoryStoreError::IndexProjection(_) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "app_memory_store_failed",
            "The app-memory candidate store is unavailable.",
        ),
    }
}

fn app_run_snapshot(
    run_handle: AppRunHandle,
    execution_id: Option<String>,
    status: AppRunStatus,
    cancellation_generation: Option<u64>,
    result_withheld: bool,
    result: Option<AppActionResult<serde_json::Value>>,
) -> Result<AppRunSnapshot<serde_json::Value>, HttpResponse> {
    let snapshot = AppRunSnapshot {
        protocol_version: AppProtocolVersion::V1,
        run_handle,
        execution_id,
        status,
        terminal: status.is_terminal(),
        cancellation_generation,
        result_withheld,
        result,
    };
    snapshot
        .validate_app_contract(&AppContractLimits::default())
        .map_err(contract_error_response)?;
    Ok(snapshot)
}

fn app_run_snapshot_response(snapshot: AppRunSnapshot<serde_json::Value>) -> HttpResponse {
    let mut response = if snapshot.terminal {
        HttpResponse::Ok()
    } else {
        HttpResponse::Accepted()
    };
    response
        .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
        .json(snapshot)
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/surface-mutations`
pub async fn mutate_app_surface_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(installation_id) => installation_id,
        Err(error) => return contract_error_response(error),
    };
    let request =
        match read_bounded_app_contract::<AppSurfaceMutationRequest>(&req, &mut payload).await {
            Ok(request) => request,
            Err(response) => return response,
        };
    match api
        .surface_hydration
        .mutate(&authenticated, &installation_id, request, now.clone())
        .await
    {
        Ok(receipt) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(receipt),
        Err(error) => surface_mutation_error_response(error),
    }
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct AppPackageImportResponse {
    state: &'static str,
    stage_outcome: &'static str,
    package_id: AppReference,
    source_publisher_identity: AppReference,
    semantic_version: String,
    package_content_digest: AppDigest,
    requirements: AppPackageImportRequirements,
    local_identity_resolution_required: bool,
    foreign_authority_transferred: bool,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct AppCandidatePublicationHttpReceipt {
    request_id: AppReference,
    #[serde(flatten)]
    publication: AppCandidatePublicationReceipt,
}

/// `GET /api/magician/v2/apps/installations/{installation_id}`
pub async fn get_app_installation_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(installation_id) => installation_id,
        Err(error) => return contract_error_response(error),
    };
    match api
        .registry
        .installation(&authenticated, &installation_id, now)
        .await
    {
        Ok(Some(installation)) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "no-store"))
            .json(installation),
        Ok(None) => api_error(
            StatusCode::NOT_FOUND,
            "app_installation_not_found",
            "The app installation does not exist in this authenticated scope.",
        ),
        Err(AppRegistryError::MissingRecord {
            entity: "installation",
            ..
        }) => api_error(
            StatusCode::NOT_FOUND,
            "app_installation_not_found",
            "The app installation does not exist in this authenticated scope.",
        ),
        Err(error) => registry_error_response(error),
    }
}

/// `GET /api/magician/v2/apps/installations/{installation_id}/review`
async fn get_app_installation_review_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(installation_id) => installation_id,
        Err(error) => return contract_error_response(error),
    };
    match api
        .installation_review()
        .review(&authenticated, &installation_id, now)
        .await
    {
        Ok(review) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(review),
        Err(error) => installation_review_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/update-plans`
async fn prepare_app_update_plan_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(installation_id) => installation_id,
        Err(error) => return contract_error_response(error),
    };
    let request: AppUpdatePlanRequest = match read_bounded_app_contract(&req, &mut payload).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    let review = match api
        .installation_review()
        .review(&authenticated, &installation_id, now)
        .await
    {
        Ok(review) => review,
        Err(error) => return installation_review_error_response(error),
    };
    let permission_diff = match review.permission_diff {
        Some(diff) => diff,
        None => {
            return api_error(
                StatusCode::CONFLICT,
                "app_update_plan_not_available",
                "Initial installation does not accept an update migration plan.",
            );
        },
    };
    match api
        .update_coordinator()
        .prepare_plan(
            &authenticated,
            &installation_id,
            request,
            permission_diff,
            review.requested_data_handling_policy,
            now,
        )
        .await
    {
        Ok(receipt) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(receipt),
        Err(error) => update_coordinator_error_response(error),
    }
}

/// `GET /api/magician/v2/apps/updates/{migration_run_id}`
async fn get_app_update_plan_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let migration_run_id = match AppReference::parse(path.into_inner()) {
        Ok(reference) => reference,
        Err(error) => return contract_error_response(error),
    };
    match api
        .update_coordinator()
        .plan(&authenticated, &migration_run_id, now)
        .await
    {
        Ok(Some(receipt)) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(receipt),
        Ok(None) => api_error(
            StatusCode::NOT_FOUND,
            "app_update_plan_not_found",
            "No update plan exists for that identity in the authenticated scope.",
        ),
        Err(error) => update_coordinator_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/updates/{migration_run_id}/backup`
async fn backup_app_update_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let migration_run_id = match AppReference::parse(path.into_inner()) {
        Ok(reference) => reference,
        Err(error) => return contract_error_response(error),
    };
    let passphrase = match archive_passphrase(&req) {
        Ok(Some(passphrase)) => passphrase,
        Ok(None) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "app_update_backup_passphrase_required",
                "A passphrase is required for the encrypted pre-update backup.",
            );
        },
        Err(response) => return response,
    };
    match api
        .update_coordinator()
        .record_encrypted_backup(&authenticated, &migration_run_id, passphrase, now)
        .await
    {
        Ok(receipt) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(receipt),
        Err(error) => update_coordinator_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/rollbacks/code`
async fn rollback_app_update_code_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(installation_id) => installation_id,
        Err(error) => return contract_error_response(error),
    };
    let request: AppCodeOnlyRollbackRequest =
        match read_bounded_app_contract(&req, &mut payload).await {
            Ok(request) => request,
            Err(response) => return response,
        };
    match api
        .update_coordinator()
        .rollback_code_only(&authenticated, request, now)
        .await
    {
        Ok(receipt) if receipt.installation_id == installation_id => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(receipt),
        Ok(_) => api_error(
            StatusCode::CONFLICT,
            "app_update_rollback_target_mismatch",
            "The rollback run does not belong to the route installation.",
        ),
        Err(error) => update_coordinator_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/updates/{migration_run_id}/rewind-preview`
async fn preview_app_update_data_rewind_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let migration_run_id = match AppReference::parse(path.into_inner()) {
        Ok(reference) => reference,
        Err(error) => return contract_error_response(error),
    };
    let passphrase = match archive_passphrase(&req) {
        Ok(Some(passphrase)) => passphrase,
        Ok(None) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "app_update_backup_passphrase_required",
                "The exact pre-update backup passphrase is required.",
            );
        },
        Err(response) => return response,
    };
    match api
        .update_coordinator()
        .preview_data_rewind(&authenticated, &migration_run_id, passphrase, now)
        .await
    {
        Ok(receipt) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(receipt),
        Err(error) => update_coordinator_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/updates/{migration_run_id}/rewind-commit`
async fn commit_app_update_data_rewind_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let migration_run_id = match AppReference::parse(path.into_inner()) {
        Ok(reference) => reference,
        Err(error) => return contract_error_response(error),
    };
    let request: AppDataRewindCommitRequest =
        match read_bounded_app_contract(&req, &mut payload).await {
            Ok(request) => request,
            Err(response) => return response,
        };
    if request.migration_run_id != migration_run_id {
        return api_error(
            StatusCode::CONFLICT,
            "app_update_rollback_target_mismatch",
            "The rewind request does not match the route migration run.",
        );
    }
    let passphrase = match archive_passphrase(&req) {
        Ok(Some(passphrase)) => passphrase,
        Ok(None) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "app_update_backup_passphrase_required",
                "The exact pre-update backup passphrase is required.",
            );
        },
        Err(response) => return response,
    };
    match api
        .update_coordinator()
        .commit_data_rewind(&authenticated, request, passphrase, now)
        .await
    {
        Ok(receipt) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(receipt),
        Err(error) => update_coordinator_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/approve`
async fn approve_app_installation_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(installation_id) => installation_id,
        Err(error) => return contract_error_response(error),
    };
    let request = match read_optional_approve_request(&req, &mut payload).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    match api
        .installation_review()
        .approve(&authenticated, &installation_id, request, now)
        .await
    {
        Ok(receipt) => {
            // Approval is what makes a boot-admitted system package live, and
            // the pinned system defaults are derived from live packages only.
            // Boot admission ran before any approval existed and could prove
            // nothing, so this is where the deployment's own defaults are
            // pinned; without it they would wait for a restart that never
            // helps. Awaited so the response is not ahead of the layout it
            // implies. An approval of anything else returns immediately.
            api.refresh_system_slot_defaults(&authenticated, &installation_id)
                .await;
            HttpResponse::Ok()
                .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
                .json(receipt)
        },
        Err(error) => installation_review_error_response(error),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppLifecycleControlRequest {
    expected_generation: u64,
    request_id: AppReference,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppReenableControlRequest {
    expected_generation: u64,
    request_id: AppReference,
    review_digest: AppDigest,
}

fn lifecycle_control_identity(
    authenticated: &AuthenticatedAppScope,
    installation_id: &AppInstallationId,
    expected_generation: u64,
    operation: &str,
    request_id: &AppReference,
) -> Result<(AppReference, AppDigest), AppContractError> {
    let _ = AppRevision::new(expected_generation)?;
    let material = serde_json::to_vec(&serde_json::json!({
        "domain": "magician.app.lifecycle-control.v1",
        "scope_binding_ref": authenticated.scope_binding_ref(),
        "actor_ref": authenticated.actor_ref(),
        "installation_id": installation_id,
        "expected_generation": expected_generation,
        "operation": operation,
        "request_id": request_id,
    }))
    .expect("closed lifecycle identity material serializes");
    let idempotency_key = AppDigest::blake3(&material);
    let event_id = AppReference::parse(format!(
        "lifecycle-event:{}",
        idempotency_key.as_str().trim_start_matches("blake3:")
    ))?;
    Ok((event_id, idempotency_key))
}

async fn read_lifecycle_control_request<T: DeserializeOwned>(
    req: &HttpRequest,
    payload: &mut web::Payload,
) -> Result<T, HttpResponse> {
    let bytes = read_bounded_raw_body(
        req,
        payload,
        "application/json",
        APP_LIFECYCLE_CONTROL_BODY_BYTES,
        "app_lifecycle_control_body",
    )
    .await?;
    serde_json::from_slice(&bytes).map_err(|_| {
        api_error(
            StatusCode::BAD_REQUEST,
            "invalid_app_lifecycle_control",
            "The lifecycle control request is invalid.",
        )
    })
}

// ── Owner kill-switch (app-authority remediation move 1) ────────────────────
//
// The lifecycle kernel (transition_installation / revoke_active_grant) and every
// dispatch-time fence that checks for a disabled/revoked installation were built
// and tested but had NO production caller — an owner who approved a misbehaving
// app had no route to disable, quarantine, uninstall, or revoke it, so those
// fail-closed states were unreachable. These owner-authenticated routes make the
// deny-states settable. Re-enable is mounted separately and consumes a sealed
// current review; it never enters this ordinary transition path. Overlay
// eviction happens inside the kernel via hide_computed_capability_scope.

/// Run a generation-bound lifecycle transition with a caller-retained request
/// identity. The registry outbox is the durable replay owner.
async fn run_app_installation_transition(
    api: &AppPlatformApi,
    req: &HttpRequest,
    installation_id_raw: String,
    command: AppInstallationCommand,
    operation: &'static str,
    payload: &mut web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(installation_id_raw) {
        Ok(installation_id) => installation_id,
        Err(error) => return contract_error_response(error),
    };
    let control: AppLifecycleControlRequest =
        match read_lifecycle_control_request(req, payload).await {
            Ok(control) => control,
            Err(response) => return response,
        };
    let (event_id, idempotency_key) = match lifecycle_control_identity(
        &authenticated,
        &installation_id,
        control.expected_generation,
        operation,
        &control.request_id,
    ) {
        Ok(identity) => identity,
        Err(error) => return contract_error_response(error),
    };
    match api
        .registry
        .transition_installation(
            &authenticated,
            installation_id,
            control.expected_generation,
            command,
            event_id,
            idempotency_key,
            now,
        )
        .await
    {
        Ok(receipt) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(serde_json::json!({
                "installation_id": receipt.installation_id,
                "generation": receipt.generation,
                "status": receipt.status,
                "review_identity": receipt.reenable_review_identity,
            })),
        Err(AppRegistryError::MissingRecord {
            entity: "installation",
            ..
        }) => api_error(
            StatusCode::NOT_FOUND,
            "app_installation_not_found",
            "The app installation does not exist in this authenticated scope.",
        ),
        Err(error) => registry_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/disable`
async fn disable_app_installation_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    run_app_installation_transition(
        &api,
        &req,
        path.into_inner(),
        AppInstallationCommand::Disable,
        "disable",
        &mut payload,
    )
    .await
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/quarantine`
async fn quarantine_app_installation_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    run_app_installation_transition(
        &api,
        &req,
        path.into_inner(),
        AppInstallationCommand::Quarantine,
        "quarantine",
        &mut payload,
    )
    .await
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/uninstall`
async fn uninstall_app_installation_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    run_app_installation_transition(
        &api,
        &req,
        path.into_inner(),
        AppInstallationCommand::UninstallRetain,
        "uninstall_retain",
        &mut payload,
    )
    .await
}

/// `GET /api/magician/v2/apps/installations/{installation_id}/reenable-review`
async fn get_app_installation_reenable_review_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(id) => id,
        Err(error) => return contract_error_response(error),
    };
    match api
        .installation_review()
        .review_reenable(&authenticated, &installation_id, now)
        .await
    {
        Ok(review) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(review),
        Err(error) => installation_review_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/reenable`
async fn reenable_app_installation_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(id) => id,
        Err(error) => return contract_error_response(error),
    };
    let control: AppReenableControlRequest =
        match read_lifecycle_control_request(&req, &mut payload).await {
            Ok(control) => control,
            Err(response) => return response,
        };
    let (event_id, idempotency_key) = match lifecycle_control_identity(
        &authenticated,
        &installation_id,
        control.expected_generation,
        "reenable",
        &control.request_id,
    ) {
        Ok(identity) => identity,
        Err(error) => return contract_error_response(error),
    };
    match api
        .installation_review()
        .approve_reenable(
            &authenticated,
            &installation_id,
            control.expected_generation,
            control.review_digest,
            event_id,
            idempotency_key,
            now,
        )
        .await
    {
        Ok(receipt) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(serde_json::json!({
                "installation_id": receipt.installation_id,
                "generation": receipt.generation,
                "status": receipt.status,
                "review_identity": receipt.reenable_review_identity,
            })),
        Err(error) => installation_review_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/purge-preview`
///
/// A preview is available only after retained-uninstall retractions and its
/// lifecycle projection are acknowledged. This prevents a confirmation from
/// racing destination-owned memory/retrieval deletion.
async fn preview_app_installation_purge_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let observed_at = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &observed_at) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(installation_id) => installation_id,
        Err(error) => return contract_error_response(error),
    };
    let expires_at = match observed_at
        .checked_add_signed(ChronoDuration::seconds(APP_PURGE_PREVIEW_LIFETIME_SECONDS))
    {
        Some(expires_at) => expires_at,
        None => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_purge_clock_overflow",
                "The purge preview validity window could not be represented.",
            )
        },
    };
    let preview_ref = match AppReference::parse(format!(
        "app-purge-preview:{}",
        uuid::Uuid::new_v4().simple()
    )) {
        Ok(preview_ref) => preview_ref,
        Err(error) => return contract_error_response(error),
    };
    match api
        .installation_purge
        .preview_whole_installation(
            &authenticated,
            &installation_id,
            preview_ref,
            observed_at,
            expires_at,
        )
        .await
    {
        Ok(preview) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(preview),
        Err(error) => installation_purge_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/purge`
///
/// The client echoes only the server preview identity/window and a fresh
/// idempotency key. The owner reconstructs the current trusted inventory and
/// refuses any digest or generation drift before the atomic destructive CAS.
async fn commit_app_installation_purge_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(installation_id) => installation_id,
        Err(error) => return contract_error_response(error),
    };
    let request = match read_bounded_app_contract::<AppWholeInstallationPurgeCommitRequest>(
        &req,
        &mut payload,
    )
    .await
    {
        Ok(request) => request,
        Err(response) => return response,
    };
    match api
        .installation_purge
        .commit_whole_installation(&authenticated, &installation_id, request, now)
        .await
    {
        Ok(receipt) => {
            // Retained uninstall should already have projected this teardown.
            // Repeat it only after the scoped owner accepted or exactly replayed
            // the purge; an invalid or cross-scope request must never mutate a
            // process-global custom-surface owner.
            api.custom_surfaces
                .teardown_installation(&installation_id, AppCustomSurfaceTeardown::Disable);
            HttpResponse::Ok()
                .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
                .json(receipt)
        },
        Err(error) => installation_purge_error_response(error),
    }
}

/// `GET /api/magician/v2/apps/purges/{idempotency_key}`
///
/// Response-loss recovery for the destructive commit is keyed only by the
/// caller-retained digest. The scoped purge owner returns the already-sealed
/// receipt; the route never accepts an installation or receipt substitution.
async fn get_app_installation_purge_status_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let idempotency_key = match AppDigest::parse(path.into_inner()) {
        Ok(idempotency_key) => idempotency_key,
        Err(error) => return contract_error_response(error),
    };
    match api
        .installation_purge
        .completed_whole_installation_receipt(&authenticated, &idempotency_key, now)
        .await
    {
        Ok(Some(receipt)) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(receipt),
        Ok(None) => api_error(
            StatusCode::NOT_FOUND,
            "app_purge_receipt_not_found",
            "No completed purge receipt exists for this identity in the authenticated scope.",
        ),
        Err(error) => installation_purge_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/update-begin`
///
/// Parks the installation in `UpdatePending` so a new package revision can be
/// published and reviewed against the current grant. The app stops dispatching
/// while pending — every fence requires `Enabled` — and the lifecycle reducer
/// remembers whether to return it to `Enabled` or `Disabled` afterwards.
async fn begin_app_installation_update_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    run_app_installation_transition(
        &api,
        &req,
        path.into_inner(),
        AppInstallationCommand::BeginUpdate,
        "begin_update",
        &mut payload,
    )
    .await
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/update-abort`
///
/// Returns a parked installation to whatever it was before the update began.
///
/// Shipped WITH `update-begin`, never after: a state an owner can enter and not
/// leave is the same defect this remediation exists to remove. Committing an
/// update is deliberately not here — that is a reviewed transition and goes
/// through the approve kernel.
async fn abort_app_installation_update_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(installation_id) => installation_id,
        Err(error) => return contract_error_response(error),
    };
    let control: AppLifecycleControlRequest =
        match read_lifecycle_control_request(&req, &mut payload).await {
            Ok(control) => control,
            Err(response) => return response,
        };
    let (event_id, idempotency_key) = match lifecycle_control_identity(
        &authenticated,
        &installation_id,
        control.expected_generation,
        "abort_update",
        &control.request_id,
    ) {
        Ok(identity) => identity,
        Err(error) => return contract_error_response(error),
    };
    if let Err(error) = api
        .update_coordinator()
        .abort_before_switch(&authenticated, &installation_id, now)
        .await
    {
        return update_coordinator_error_response(error);
    }
    match api
        .registry
        .transition_installation(
            &authenticated,
            installation_id,
            control.expected_generation,
            AppInstallationCommand::FailUpdate,
            event_id,
            idempotency_key,
            now,
        )
        .await
    {
        Ok(receipt) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(serde_json::json!({
                "installation_id": receipt.installation_id,
                "generation": receipt.generation,
                "status": receipt.status,
                "review_identity": receipt.reenable_review_identity,
            })),
        Err(AppRegistryError::MissingRecord {
            entity: "installation",
            ..
        }) => api_error(
            StatusCode::NOT_FOUND,
            "app_installation_not_found",
            "The app installation does not exist in this authenticated scope.",
        ),
        Err(error) => registry_error_response(error),
    }
}

/// Owner edit of an installation's memory access (`app_memory_read_v1`).
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AppMemoryAccessUpdateRequest {
    /// `edit_revision` from the GET this edit was based on.
    expected_edit_revision: u64,
    #[serde(default)]
    interactive: magician::magician_v2::apps::memory_access::AppMemoryReadSelection,
    #[serde(default)]
    background: magician::magician_v2::apps::memory_access::AppMemoryReadSelection,
}

fn memory_access_response(
    access: magician::magician_v2::apps::memory_access_store::AppMemoryReadAccess,
) -> HttpResponse {
    let tiers = magician::magician_v2::apps::memory_access::app_readable_user_memory_tiers()
        .into_iter()
        .map(|(name, readability)| serde_json::json!({"name": name, "readability": readability}))
        .collect::<Vec<_>>();
    HttpResponse::Ok().json(serde_json::json!({
        "installation_id": access.installation_id,
        "request": access.request,
        "reviewed": access.reviewed,
        "effective": access.effective,
        "edit_revision": access.edit_revision,
        "installation_enabled": access.installation_enabled,
        "user_tier_catalog": tiers,
    }))
}

/// `GET /api/magician/v2/apps/installations/{installation_id}/memory-access`
///
/// What owner memory the app requested, what was granted at review, and what
/// it can read right now, split interactive / background.
async fn get_app_installation_memory_access_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(installation_id) => installation_id,
        Err(error) => return contract_error_response(error),
    };
    match api
        .registry
        .memory_read_access(&authenticated, &installation_id, now)
        .await
    {
        Ok(Some(access)) => memory_access_response(access),
        Ok(None) | Err(AppRegistryError::MissingRecord { .. }) => api_error(
            StatusCode::NOT_FOUND,
            "app_installation_not_found",
            "The app installation does not exist in this authenticated scope.",
        ),
        Err(error) => registry_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/memory-access`
///
/// Replace the app's memory access with an owner-chosen selection, within
/// what the app requested. Compare-and-swapped on `expected_edit_revision`;
/// applies on the app's next memory read.
async fn update_app_installation_memory_access_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AppMemoryAccessUpdateRequest>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(installation_id) => installation_id,
        Err(error) => return contract_error_response(error),
    };
    let body = body.into_inner();
    let actor = authenticated.actor_ref().clone();
    match api
        .registry
        .update_memory_read_grant(
            &authenticated,
            &installation_id,
            body.expected_edit_revision,
            body.interactive,
            body.background,
            actor,
            now,
        )
        .await
    {
        Ok(access) => memory_access_response(access),
        Err(AppRegistryError::StateConflict(message)) => {
            api_error(StatusCode::CONFLICT, "app_memory_access_conflict", &message)
        },
        Err(AppRegistryError::MissingRecord { .. }) => api_error(
            StatusCode::NOT_FOUND,
            "app_installation_not_found",
            "The app installation does not exist in this authenticated scope.",
        ),
        Err(error) => registry_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/grant-revocations`
///
/// Revokes the installation's active grant without changing its lifecycle status.
/// Every dispatch-time authority recheck fails closed once `grant.revoked_at` is set.
async fn revoke_app_installation_grant_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(installation_id) => installation_id,
        Err(error) => return contract_error_response(error),
    };
    let installation = match api
        .registry
        .installation(&authenticated, &installation_id, now)
        .await
    {
        Ok(Some(installation)) => installation,
        Ok(None) => {
            return api_error(
                StatusCode::NOT_FOUND,
                "app_installation_not_found",
                "The app installation does not exist in this authenticated scope.",
            )
        },
        Err(AppRegistryError::MissingRecord { .. }) => {
            return api_error(
                StatusCode::NOT_FOUND,
                "app_installation_not_found",
                "The app installation does not exist in this authenticated scope.",
            )
        },
        Err(error) => return registry_error_response(error),
    };
    let Some(grant_revision) = installation.grant_revision else {
        return api_error(
            StatusCode::CONFLICT,
            "app_grant_not_active",
            "The app installation has no active grant to revoke.",
        );
    };
    // The current UI route predates durable grant-control identities and sends
    // an empty request. Preserve that compatibility while allowing CLI and new
    // clients to retain an exact generation/request pair for response-loss
    // replay. A supplied control is never silently weakened to the legacy path.
    let has_control_body = req
        .headers()
        .get(actix_web::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > 0)
        || req
            .headers()
            .contains_key(actix_web::http::header::TRANSFER_ENCODING);
    let (event_id, idempotency_key, expected_generation) = if has_control_body {
        let control: AppLifecycleControlRequest =
            match read_lifecycle_control_request(&req, &mut payload).await {
                Ok(control) => control,
                Err(response) => return response,
            };
        match lifecycle_control_identity(
            &authenticated,
            &installation_id,
            control.expected_generation,
            "revoke_grant",
            &control.request_id,
        ) {
            Ok((event_id, idempotency_key)) => {
                (event_id, idempotency_key, control.expected_generation)
            },
            Err(error) => return contract_error_response(error),
        }
    } else {
        let token = uuid::Uuid::new_v4().simple().to_string();
        let event_id = match AppReference::parse(format!("grant-revocation:{token}")) {
            Ok(event_id) => event_id,
            Err(error) => return contract_error_response(error),
        };
        let idempotency_key = AppDigest::blake3(format!("grant-revocation:{token}").as_bytes());
        (event_id, idempotency_key, installation.lifecycle.generation)
    };
    match api
        .registry
        .revoke_active_grant(
            &authenticated,
            installation_id,
            expected_generation,
            grant_revision,
            event_id,
            idempotency_key,
            now,
        )
        .await
    {
        Ok(receipt) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(serde_json::json!({
                "installation_id": receipt.installation_id,
                "generation": receipt.generation,
                "status": receipt.status,
            })),
        Err(error) => registry_error_response(error),
    }
}

/// `GET /api/magician/v2/apps/attempts/{attempt_id}`
pub async fn get_app_lifecycle_attempt_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let attempt_id = match AppReference::parse(path.into_inner()) {
        Ok(attempt_id) => attempt_id,
        Err(error) => return contract_error_response(error),
    };
    match api
        .registry
        .lifecycle_attempt(&authenticated, &attempt_id, now)
        .await
    {
        Ok(Some(attempt)) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "no-store"))
            .json(attempt),
        Ok(None) => api_error(
            StatusCode::NOT_FOUND,
            "app_attempt_not_found",
            "The app lifecycle attempt does not exist in this authenticated scope.",
        ),
        Err(error) => registry_error_response(error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomSurfaceHostQuery {
    document: Option<String>,
    replace_session: Option<String>,
}

#[derive(Deserialize)]
struct CustomSurfaceAssetQuery {
    session: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomSurfaceSubscriptionRequest {
    #[serde(default)]
    after_change_sequence: u64,
    #[serde(default = "default_app_entity_change_limit")]
    limit: usize,
}

/// `GET /api/magician/v2/apps/installations/{installation_id}/custom-surface`
async fn get_custom_surface_host_handler(
    api: web::Data<AppPlatformApi>,
    resources: Option<web::Data<Arc<AgentResources>>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<CustomSurfaceHostQuery>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let replace_session = match query.replace_session.as_ref() {
        Some(previous) => match AppReference::parse(previous.clone()) {
            Ok(previous) => Some(previous),
            Err(error) => return contract_error_response(error),
        },
        None => None,
    };
    let loaded = match load_enabled_custom_surface_package(&api, &authenticated, &path, now).await {
        Ok(loaded) => loaded,
        Err(response) => return response,
    };
    let document = query
        .document
        .clone()
        .unwrap_or_else(|| "surfaces/index.html".to_owned());
    let session_ref = match AppReference::parse(format!(
        "bridge:{}:{}",
        loaded.installation.installation_id.as_str(),
        uuid::Uuid::new_v4().simple()
    )) {
        Ok(session_ref) => session_ref,
        Err(error) => return contract_error_response(error),
    };
    let nonce = match AppReference::parse(format!("nonce:{}", uuid::Uuid::new_v4().simple())) {
        Ok(nonce) => nonce,
        Err(error) => return contract_error_response(error),
    };
    let host_session = authenticated.session_ref().clone();
    let Some(surface_revision) = loaded.installation.active_surface_revision else {
        return api_error(
            StatusCode::CONFLICT,
            "app_custom_surface_unavailable",
            "The installation has no active surface revision.",
        );
    };
    let Some(grant_revision) = loaded.installation.grant_revision else {
        return api_error(
            StatusCode::CONFLICT,
            "app_custom_surface_unavailable",
            "The installation has no active grant revision.",
        );
    };
    let package_ref = loaded.installation.package_revision_ref.clone();
    let bundle = loaded.package.content_digest.clone();
    let admission = surface_admission_for_enabled_installation(
        &package_ref,
        &bundle,
        loaded.installation.installation_id.clone(),
        surface_revision,
        grant_revision,
        host_session,
        session_ref,
        nonce,
        now,
    );
    let opened = if package_has_javascript(loaded.staged.candidate())
        || package_has_wasm(loaded.staged.candidate())
    {
        api.custom_surfaces
            .open_scripted_host(loaded.staged.candidate(), &document, admission, now)
    } else {
        api.custom_surfaces.open_no_script_host(
            loaded.staged.candidate(),
            &document,
            admission,
            now,
        )
    };
    match opened {
        Ok(envelope) => {
            if package_has_javascript(loaded.staged.candidate()) {
                if let Err(response) = pump_scripted_surface_host(
                    api.get_ref(),
                    &authenticated,
                    &loaded,
                    &envelope.session.session_ref,
                    resources
                        .as_ref()
                        .map(|resources| resources.get_ref().as_ref()),
                )
                .await
                {
                    api.custom_surfaces
                        .abort_worker_pump(&envelope.session.session_ref);
                    return response;
                }
            }
            let csp = AppCustomSurfaceRuntime::content_security_policy(&envelope).to_owned();
            let mut response = AppCustomSurfaceRuntime::host_response(&envelope);
            api.custom_surfaces.decorate_host_response(&mut response);
            let change_head = match api
                .entity_changes
                .read_after(
                    &authenticated,
                    &loaded.installation.installation_id,
                    envelope.session.surface_revision,
                    0,
                    1,
                    now,
                )
                .await
            {
                Ok(batch) => batch.current_change_sequence,
                Err(error) => {
                    api.custom_surfaces
                        .abort_worker_pump(&envelope.session.session_ref);
                    return entity_change_error_response(error);
                },
            };
            response.change_sequence = change_head;
            if let Some(previous) = replace_session.as_ref() {
                api.custom_surfaces.retire_replaced_session(
                    &loaded.installation.installation_id,
                    authenticated.session_ref(),
                    previous,
                    &envelope.session.session_ref,
                );
            }
            HttpResponse::Ok()
                .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
                .insert_header(("content-security-policy", csp))
                .json(response)
        },
        Err(error) => custom_surface_error_response(error),
    }
}

/// `GET /api/magician/v2/apps/installations/{installation_id}/custom-surface/assets/{tail}`
async fn get_custom_surface_asset_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<CustomSurfaceAssetQuery>,
) -> HttpResponse {
    if req.uri().path().contains('%') {
        return api_error(
            StatusCode::NOT_FOUND,
            "app_custom_surface_not_found",
            "The custom-surface asset does not exist.",
        );
    }
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let (installation_id, asset_tail) = path.into_inner();
    let loaded = match load_enabled_custom_surface_package(
        &api,
        &authenticated,
        &installation_id,
        now,
    )
    .await
    {
        Ok(loaded) => loaded,
        Err(response) => return response,
    };
    let session_ref = match AppReference::parse(query.session.clone()) {
        Ok(session_ref) => session_ref,
        Err(error) => return contract_error_response(error),
    };
    let asset_path = if asset_tail.starts_with("surfaces/") {
        asset_tail
    } else {
        format!("surfaces/{asset_tail}")
    };
    let Some(surface_revision) = loaded.installation.active_surface_revision else {
        return api_error(
            StatusCode::CONFLICT,
            "app_custom_surface_unavailable",
            "The installation has no active surface revision.",
        );
    };
    let Some(grant_revision) = loaded.installation.grant_revision else {
        return api_error(
            StatusCode::CONFLICT,
            "app_custom_surface_unavailable",
            "The installation has no active grant revision.",
        );
    };
    let package_ref = loaded.installation.package_revision_ref.clone();
    let bundle = loaded.package.content_digest.clone();
    let host_session = authenticated.session_ref().clone();
    let nonce = match AppReference::parse("nonce:asset") {
        Ok(nonce) => nonce,
        Err(error) => return contract_error_response(error),
    };
    let admission = surface_admission_for_enabled_installation(
        &package_ref,
        &bundle,
        loaded.installation.installation_id.clone(),
        surface_revision,
        grant_revision,
        host_session,
        session_ref.clone(),
        nonce,
        now,
    );
    match api.custom_surfaces.serve_asset(
        loaded.staged.candidate(),
        &session_ref,
        &asset_path,
        admission,
    ) {
        Ok(asset) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .insert_header((actix_web::http::header::CONTENT_TYPE, asset.media_type()))
            .insert_header(("x-content-type-options", "nosniff"))
            .insert_header((
                "content-security-policy",
                "default-src 'none'; connect-src 'none'",
            ))
            .body(asset.bytes),
        Err(error) => custom_surface_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/custom-surface/bridge`
async fn post_custom_surface_bridge_handler(
    api: web::Data<AppPlatformApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let loaded = match load_enabled_custom_surface_package(&api, &authenticated, &path, now).await {
        Ok(loaded) => loaded,
        Err(response) => return response,
    };
    let message: AppBridgeMessage = match read_bounded_app_contract(&req, &mut payload).await {
        Ok(message) => message,
        Err(response) => return response,
    };
    if message.installation_id.as_str() != loaded.installation.installation_id.as_str() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "app_installation_mismatch",
            "The bridge installation must match the authenticated route.",
        );
    }
    let Some(surface_revision) = loaded.installation.active_surface_revision else {
        return api_error(
            StatusCode::CONFLICT,
            "app_custom_surface_unavailable",
            "The installation has no active surface revision.",
        );
    };
    let Some(grant_revision) = loaded.installation.grant_revision else {
        return api_error(
            StatusCode::CONFLICT,
            "app_custom_surface_unavailable",
            "The installation has no active grant revision.",
        );
    };
    if let Err(error) = api.custom_surfaces.admit_bridge_for_host(
        &message,
        now,
        &loaded.installation.package_revision_ref,
        surface_revision,
        grant_revision,
        authenticated.session_ref(),
        "null",
        &AppContractLimits::default(),
    ) {
        return custom_surface_error_response(error);
    }
    execute_admitted_bridge(
        api.get_ref(),
        &authenticated,
        &message,
        Some(resources.get_ref().as_ref()),
        now,
    )
    .boxed_local()
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptedSurfaceHostQuery {
    #[serde(default)]
    route: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptedSurfaceAssetQuery {
    session: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptedSurfaceComposeRequest {
    run_ref: AppReference,
    request: AppActionCompositionRequest,
}

/// Derive the single host-UI origin the kernel CSP admits as the surface
/// frame's only permitted ancestor. The deployment's configured public
/// origin (`mobile_access.public_origin`, published process-wide with the
/// mobile enrollment config) wins when set: Host/X-Forwarded-* headers
/// are client-controlled, and a deployment with a fixed public name must
/// not let them steer or widen the frame-ancestors allowlist. The
/// connection-derived origin remains only the unset fallback (dev), and
/// the origin is never accepted from the surface or its package.
fn scripted_surface_frame_ancestors_origin(configured: Option<&str>, req: &HttpRequest) -> String {
    if let Some(origin) = configured.map(str::trim).filter(|value| !value.is_empty()) {
        return origin.trim_end_matches('/').to_owned();
    }
    let connection = req.connection_info();
    format!("{}://{}", connection.scheme(), connection.host())
}

fn scripted_surface_error_response(error: AppScriptedSurfaceHostError) -> HttpResponse {
    let (status, code) = match &error {
        AppScriptedSurfaceHostError::CapabilityDisabled => {
            (StatusCode::NOT_FOUND, "app_custom_surface_unavailable")
        },
        // The kernel CSP could not be composed for the request origin: the
        // surface cannot be served safely, so refuse rather than emit none.
        AppScriptedSurfaceHostError::CspCompositionFailed => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "app_custom_surface_unavailable",
        ),
        AppScriptedSurfaceHostError::PermissionAbsent => {
            (StatusCode::NOT_FOUND, "app_custom_surface_unavailable")
        },
        AppScriptedSurfaceHostError::EntryPointNotDeclared => {
            (StatusCode::NOT_FOUND, "app_custom_surface_not_found")
        },
        // A declared route outside the owner's grant answers exactly like
        // an undeclared one: 404, revealing only that nothing is hostable.
        AppScriptedSurfaceHostError::EntryPointNotGranted => {
            (StatusCode::NOT_FOUND, "app_custom_surface_not_found")
        },
        // The live entry document no longer matches the attested digest:
        // the grant is stale against the package and hosting refuses until
        // a fresh review re-grants it.
        AppScriptedSurfaceHostError::GrantedDigestStale => {
            (StatusCode::CONFLICT, "app_custom_surface_denied")
        },
        AppScriptedSurfaceHostError::SessionGone | AppScriptedSurfaceHostError::SessionLimit => {
            (StatusCode::CONFLICT, "app_custom_surface_unavailable")
        },
        AppScriptedSurfaceHostError::SessionCollision => {
            (StatusCode::CONFLICT, "app_custom_surface_unavailable")
        },
        AppScriptedSurfaceHostError::WatchdogTripped
        | AppScriptedSurfaceHostError::ReloadBudgetExceeded => {
            (StatusCode::CONFLICT, "app_custom_surface_denied")
        },
        AppScriptedSurfaceHostError::DigestMismatch => {
            (StatusCode::NOT_FOUND, "app_custom_surface_not_found")
        },
        AppScriptedSurfaceHostError::Sandbox(_) | AppScriptedSurfaceHostError::Asset(_) => {
            (StatusCode::CONFLICT, "app_custom_surface_denied")
        },
    };
    api_error(status, code, &error.to_string())
}

/// `GET /api/magician/v2/apps/installations/{installation_id}/custom-surface-v1/host`
///
/// Opens one scripted custom-surface session for a declared entry point
/// (plan 1.6). Fail-closed preconditions: the process-wide operator
/// switch, the package's `custom_surface` permission and declared entry
/// point, an `Enabled` installation with an `Active` surface at the exact
/// live revision. The response carries only kernel constants (sandbox,
/// CSP), the digest-keyed entry document address — minted inside its
/// session path, the one credential a served frame's opaque origin
/// can present, scoped to read-only serving of this installation's
/// reviewed surface members — and the bridge session binding: never an
/// owner credential. The minted session is bound to this request's
/// authenticated scope, which the asset route lends back to
/// credential-less frame GETs that carry only the session path segment.
async fn get_scripted_surface_host_handler(
    api: web::Data<AppPlatformApi>,
    enrollment_config: web::Data<MobileEnrollmentConfig>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScriptedSurfaceHostQuery>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let loaded = match load_enabled_custom_surface_package(&api, &authenticated, &path, now).await {
        Ok(loaded) => loaded,
        Err(response) => return response,
    };
    // An omitted route names the first declared entry point, in the
    // package's own declaration order — still a declared entry point,
    // never an undeclared document.
    let route = query.route.clone().unwrap_or_else(|| {
        loaded
            .staged
            .candidate()
            .manifest()
            .manifest()
            .app
            .custom_surface
            .as_ref()
            .and_then(|declaration| declaration.entry_points.first())
            .map(|entry| entry.route.as_str().to_owned())
            .unwrap_or_else(|| "/".to_owned())
    });
    let session_ref = match AppReference::parse(format!(
        "bridge-scripted:{}:{}",
        loaded.installation.installation_id.as_str(),
        uuid::Uuid::new_v4().simple()
    )) {
        Ok(session_ref) => session_ref,
        Err(error) => return contract_error_response(error),
    };
    let nonce = match AppReference::parse(format!("nonce:{}", uuid::Uuid::new_v4().simple())) {
        Ok(nonce) => nonce,
        Err(error) => return contract_error_response(error),
    };
    let Some(surface_revision) = loaded.installation.active_surface_revision else {
        return api_error(
            StatusCode::CONFLICT,
            "app_custom_surface_unavailable",
            "The installation has no active surface revision.",
        );
    };
    let Some(grant_revision) = loaded.installation.grant_revision else {
        return api_error(
            StatusCode::CONFLICT,
            "app_custom_surface_unavailable",
            "The installation has no active grant revision.",
        );
    };
    let package_ref = loaded.installation.package_revision_ref.clone();
    let bundle = loaded.package.content_digest.clone();
    let admission = surface_admission_for_enabled_installation(
        &package_ref,
        &bundle,
        loaded.installation.installation_id.clone(),
        surface_revision,
        grant_revision,
        authenticated.session_ref().clone(),
        session_ref,
        nonce,
        now,
    );
    // Resolve the installation's LIVE grant revision through the same
    // enabled-installation resolver the review and execution kernels use,
    // and admit only the owner-granted custom-surface entry points (plan
    // 1.6 completion). The operator switch is the process-wide control;
    // this grant is the per-installation control, and an empty grant
    // hosts nothing. A grant that cannot be resolved fails closed.
    let granted_entry_points = match AppEntityStoreService::new(api.registry.clone())
        .active_schema(&authenticated, &loaded.installation.installation_id, now)
        .await
    {
        Ok(Some(active)) => active.grant().granted_custom_surface_entry_points.clone(),
        Ok(None) => Vec::new(),
        Err(error) => {
            return api_error(
                StatusCode::CONFLICT,
                "app_custom_surface_unavailable",
                &format!("The live grant revision is unavailable: {error}"),
            )
        },
    };
    let plan = match magician_apps::apps::surface_scripted_host::compile_scripted_surface_host_plan(
        loaded.staged.candidate(),
        &route,
        &granted_entry_points,
        admission,
        &scripted_surface_frame_ancestors_origin(enrollment_config.public_origin.as_deref(), &req),
        scripted_surfaces_enabled_in_config(),
        now,
    ) {
        Ok(plan) => plan,
        Err(error) => return scripted_surface_error_response(error),
    };
    // The session carries the requesting scope from mint: the host
    // document's own authenticated scope is what a credential-less
    // frame's later asset requests re-derive (the hosted-web CF Access
    // posture — the asset route's session-bound fallback lends exactly
    // this binding back).
    let requesting_scope = AppScriptedSurfaceRequestScope {
        principal: authenticated.scope().principal.as_str().to_owned(),
        workspace: authenticated.scope().workspace.as_str().to_owned(),
    };
    match api.scripted_surfaces.open_host(plan, requesting_scope, now) {
        Ok(plan) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .insert_header(("content-security-policy", plan.csp.as_str()))
            .json(plan),
        Err(error) => scripted_surface_error_response(error),
    }
}

/// Resolve the serving scope for one scripted-surface asset request —
/// the dual path from the hosted-web/Cloudflare Access review. The
/// normal path is exactly every other apps route: verified identity plus
/// the header envelope. When — and only when — that path refuses with
/// the workspace-missing error (an interactive Cloudflare Access
/// identity has no workspace binding, and a sandboxed frame's opaque
/// origin can carry no `X-Workspace` header), the REQUIRED session path
/// segment names the live session this installation's host route minted,
/// and the session's bound requesting scope — not any header — resolves
/// the serving scope. The lookup is fail-closed: an unknown,
/// torn-down, or TTL-expired session answers `SessionGone` exactly like
/// the serving kernel (an expired entry is evicted by this lookup, so it
/// cannot keep lending its scope), and a session minted under a
/// different principal refuses as a
/// scope mismatch. No other refusal recovers, and no new query
/// parameters exist; the bridge POST keeps the plain envelope contract.
fn scripted_surface_asset_scope(
    api: &AppPlatformApi,
    req: &HttpRequest,
    query: &ScriptedSurfaceAssetQuery,
    now: &DateTime<Utc>,
) -> Result<AuthenticatedAppScope, HttpResponse> {
    match authenticate_app_scope(req, now) {
        Ok(scope) => Ok(scope),
        Err(AppScopeAuthFailure::Rejected(response)) => Err(response),
        Err(AppScopeAuthFailure::WorkspaceRequired) => {
            let session_ref =
                AppReference::parse(query.session.clone()).map_err(contract_error_response)?;
            let bound = api
                .scripted_surfaces
                .session_scope(&session_ref, *now)
                .ok_or_else(|| {
                    scripted_surface_error_response(AppScriptedSurfaceHostError::SessionGone)
                })?;
            session_bound_app_scope(req, now, &bound)
        },
    }
}

/// `GET /api/magician/v2/apps/installations/{installation_id}/custom-surface-v1/assets/{session}/{digest}/{tail}`
///
/// Serves one immutable package member for a live scripted session
/// named by the required session path segment — the reference the minted
/// entry URL embeds and relative subresources actually inherit (a URL query
/// is dropped when a relative script path resolves, and a sandboxed frame's
/// opaque origin can carry no headers). On hosted web behind
/// Cloudflare Access, where the frame can present no header envelope at
/// all, that same session segment also resolves the serving scope through
/// the session's bound requesting scope (`scripted_surface_asset_scope`
/// above). The served bytes must hash to the digest named in the
/// address; anything else fails closed, and the cache policy is
/// immutable only under verified digest addressing. EVERY served member
/// carries the kernel CSP — not just HTML — so a frame that
/// self-navigates to a script-capable member (an SVG is the canonical
/// case) still meets `connect-src 'none'`; and script-capable members
/// that are not a declared entry document are refused outright in the
/// resolver.
async fn get_scripted_surface_asset_handler(
    api: web::Data<AppPlatformApi>,
    enrollment_config: web::Data<MobileEnrollmentConfig>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    // The operator kill switch guards every scripted-surface route, not
    // only host-open: disabled refuses assets exactly like the host route.
    if !scripted_surfaces_enabled_in_config() {
        return scripted_surface_error_response(AppScriptedSurfaceHostError::CapabilityDisabled);
    }
    if req.uri().path().contains('%') || !req.query_string().is_empty() {
        return api_error(
            StatusCode::NOT_FOUND,
            "app_custom_surface_not_found",
            "The custom-surface asset does not exist.",
        );
    }
    let now = Utc::now();
    let (installation_id, asset_tail) = path.into_inner();
    let (session_ref, digest, asset_path) =
        match parse_scripted_surface_session_asset_address(&asset_tail) {
            Ok(parsed) => parsed,
            Err(error) => return scripted_surface_error_response(error),
        };
    let query = ScriptedSurfaceAssetQuery {
        session: session_ref.as_str().to_owned(),
    };
    let asset_credential = req
        .extensions()
        .get::<magician::magician_v2::auth::middleware::AuthenticatedScriptedSurfaceAsset>()
        .cloned();
    let asset_scope = match asset_credential {
        Some(credential) => Ok(credential.scope().clone()),
        None => scripted_surface_asset_scope(&api, &req, &query, &now),
    };
    let authenticated = match asset_scope {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let loaded = match load_enabled_custom_surface_package(
        &api,
        &authenticated,
        &installation_id,
        now,
    )
    .await
    {
        Ok(loaded) => loaded,
        Err(response) => return response,
    };
    let Some(surface_revision) = loaded.installation.active_surface_revision else {
        return api_error(
            StatusCode::CONFLICT,
            "app_custom_surface_unavailable",
            "The installation has no active surface revision.",
        );
    };
    let Some(grant_revision) = loaded.installation.grant_revision else {
        return api_error(
            StatusCode::CONFLICT,
            "app_custom_surface_unavailable",
            "The installation has no active grant revision.",
        );
    };
    let package_ref = loaded.installation.package_revision_ref.clone();
    let bundle = loaded.package.content_digest.clone();
    let nonce = match AppReference::parse("nonce:scripted-asset") {
        Ok(nonce) => nonce,
        Err(error) => return contract_error_response(error),
    };
    let admission = surface_admission_for_enabled_installation(
        &package_ref,
        &bundle,
        loaded.installation.installation_id.clone(),
        surface_revision,
        grant_revision,
        authenticated.session_ref().clone(),
        session_ref.clone(),
        nonce,
        now,
    );
    match api.scripted_surfaces.serve_asset(
        loaded.staged.candidate(),
        &session_ref,
        &digest,
        &asset_path,
        admission,
        now,
    ) {
        Ok((asset, cache_control)) => {
            // The kernel CSP is attached to EVERY served member, regardless
            // of media type: an SVG or other script-capable member must
            // never be servable without it (T12). Composition failure
            // fails the request — an unframable origin must never be
            // answered with an empty CSP header.
            let csp = match magician_apps::apps::custom_surface_review::custom_surface_v1_csp(
                &scripted_surface_frame_ancestors_origin(
                    enrollment_config.public_origin.as_deref(),
                    &req,
                ),
            ) {
                Ok(csp) => csp,
                Err(_) => {
                    return api_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "app_custom_surface_unavailable",
                        "The custom-surface asset cannot be served safely.",
                    );
                },
            };
            HttpResponse::Ok()
                .insert_header((actix_web::http::header::CACHE_CONTROL, cache_control))
                .insert_header((actix_web::http::header::CONTENT_TYPE, asset.media_type()))
                .insert_header(("x-content-type-options", "nosniff"))
                .insert_header(("content-security-policy", csp.as_str()))
                .body(asset.bytes)
        },
        Err(error) => scripted_surface_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/custom-surface-v1/bridge`
///
/// The web/desktop host relays its frame's postMessage bridge here under
/// the host page's own authentication (the frame holds no credentials).
/// Admission reuses the sandbox kernel (replay, sequence, origin,
/// revision, payload, watchdogs); the method set is exactly the eight
/// supported-public operations; `contract_capabilities` is answered
/// host-side from the pinned inventory digest. The session-bound scope
/// fallback never applies here: the bridge demands the full
/// authenticated header envelope plus the admitted envelope (nonce,
/// sequence, revision bindings) exactly as before.
async fn post_scripted_surface_bridge_handler(
    api: web::Data<AppPlatformApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    // The operator kill switch guards the bridge too: disabled means no
    // surface traffic at all, not merely no new host-open.
    if !scripted_surfaces_enabled_in_config() {
        return scripted_surface_error_response(AppScriptedSurfaceHostError::CapabilityDisabled);
    }
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let loaded = match load_enabled_custom_surface_package(&api, &authenticated, &path, now).await {
        Ok(loaded) => loaded,
        Err(response) => return response,
    };
    let message: AppSurfaceV1BridgeMessage =
        match read_bounded_app_contract(&req, &mut payload).await {
            Ok(message) => message,
            Err(response) => return response,
        };
    if message.installation_id.as_str() != loaded.installation.installation_id.as_str() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "app_installation_mismatch",
            "The bridge installation must match the authenticated route.",
        );
    }
    let Some(surface_revision) = loaded.installation.active_surface_revision else {
        return api_error(
            StatusCode::CONFLICT,
            "app_custom_surface_unavailable",
            "The installation has no active surface revision.",
        );
    };
    let Some(grant_revision) = loaded.installation.grant_revision else {
        return api_error(
            StatusCode::CONFLICT,
            "app_custom_surface_unavailable",
            "The installation has no active grant revision.",
        );
    };
    let limits = AppContractLimits::default();
    let admitted = match api.scripted_surfaces.admit_bridge(
        &message,
        now,
        &loaded.installation.package_revision_ref,
        surface_revision,
        grant_revision,
        &limits,
    ) {
        Ok(admitted) => admitted,
        Err(error) => return scripted_surface_error_response(error),
    };
    match message.method {
        AppSurfaceV1Method::ContractCapabilities => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(v1_contract_capabilities_reply(&limits)),
        AppSurfaceV1Method::ComposeActionRun => {
            let compose: ScriptedSurfaceComposeRequest = match serde_json::from_slice(
                &serde_json::to_vec(&message.payload).unwrap_or_default(),
            ) {
                Ok(compose) => compose,
                Err(_) => {
                    return api_error(
                        StatusCode::BAD_REQUEST,
                        "invalid_app_contract",
                        "The scripted-surface composition request is invalid.",
                    );
                },
            };
            if !has_canonical_app_action_run_namespace(compose.run_ref.as_str()) {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_app_contract",
                    "Compositions require a canonical run:app-action: reference.",
                );
            }
            let service = AppCompositionService::new(api.workflow_service());
            match service
                .compose_action_result_for_owner(
                    &authenticated,
                    compose.run_ref,
                    compose.request,
                    resources.get_ref().as_ref(),
                    now,
                )
                .boxed_local()
                .await
            {
                Ok(composition) => HttpResponse::Ok()
                    .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
                    .json(composition),
                Err(error) => app_composition_error_response(error),
            }
        },
        _ => {
            execute_admitted_bridge(
                api.get_ref(),
                &authenticated,
                &admitted,
                Some(resources.get_ref().as_ref()),
                now,
            )
            .boxed_local()
            .await
        },
    }
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/custom-surface-v1/sessions/{session_ref}/reload-note`
///
/// Record one frame reload or renderer crash against the session's
/// reload/crash budget (plan 1.6). The host page or native shell — never
/// the credential-less frame — calls this, under the same authentication
/// posture as the asset route: the full header envelope when the caller
/// carries one, the session-bound fallback for the hosted-web
/// interactive identity (the path carries the same session reference as
/// asset serving). The route reads no body, so nothing
/// unbounded is admitted. The reference is never trusted alone: the
/// installation is resolved through the caller's scope, and the kernel
/// refuses a session bound to another installation, a TTL-expired
/// session, and the budget-exceeded session (`ReloadBudgetExceeded`) —
/// mapped to the same 409 responses the bridge already teaches hosts to
/// react to.
async fn post_scripted_surface_reload_note_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    // The operator kill switch guards the reload-note route too: a
    // disabled process records no surface lifecycle traffic.
    if !scripted_surfaces_enabled_in_config() {
        return scripted_surface_error_response(AppScriptedSurfaceHostError::CapabilityDisabled);
    }
    // Early parity guard with the asset route: a percent-encoded path
    // segment names nothing this route could resolve — the kernel mints
    // canonical session references — so it is refused before any scope,
    // registry, or session work runs.
    if req.uri().path().contains('%') {
        return api_error(
            StatusCode::NOT_FOUND,
            "app_custom_surface_not_found",
            "The custom-surface session does not exist.",
        );
    }
    let now = Utc::now();
    let (installation_id, session_ref_raw) = path.into_inner();
    let session_ref = match AppReference::parse(session_ref_raw) {
        Ok(session_ref) => session_ref,
        Err(error) => return contract_error_response(error),
    };
    let query = ScriptedSurfaceAssetQuery {
        session: session_ref.as_str().to_owned(),
    };
    let authenticated = match scripted_surface_asset_scope(&api, &req, &query, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let loaded = match load_enabled_custom_surface_package(
        &api,
        &authenticated,
        &installation_id,
        now,
    )
    .await
    {
        Ok(loaded) => loaded,
        Err(response) => return response,
    };
    match api
        .scripted_surfaces
        .note_reload(&loaded.installation.installation_id, &session_ref, now)
    {
        Ok(()) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(json!({ "reload_note": "recorded" })),
        Err(error) => scripted_surface_error_response(error),
    }
}

#[derive(Debug)]
struct LoadedCustomSurfacePackage {
    installation: magician::magician_v2::apps::records::AppInstallation,
    package: magician::magician_v2::apps::records::AppPackageRevision,
    staged: magician::magician_v2::apps::package_staging::StagedAppPackage,
}

async fn load_enabled_custom_surface_package(
    api: &AppPlatformApi,
    authenticated: &AuthenticatedAppScope,
    installation_id: &str,
    now: DateTime<Utc>,
) -> Result<LoadedCustomSurfacePackage, HttpResponse> {
    let installation_id =
        AppInstallationId::parse(installation_id.to_owned()).map_err(contract_error_response)?;
    let installation = api
        .registry
        .installation(authenticated, &installation_id, now)
        .await
        .map_err(registry_error_response)?
        .ok_or_else(|| {
            api_error(
                StatusCode::NOT_FOUND,
                "app_installation_not_found",
                "The app installation does not exist in this authenticated scope.",
            )
        })?;
    if installation.lifecycle.status != AppInstallationStatus::Enabled {
        return Err(api_error(
            StatusCode::CONFLICT,
            "app_custom_surface_unavailable",
            "The installation is not enabled for custom-surface hosting.",
        ));
    }
    let package = api
        .registry
        .package_revision(authenticated, &installation.package_revision_ref, now)
        .await
        .map_err(registry_error_response)?
        .ok_or_else(|| {
            api_error(
                StatusCode::NOT_FOUND,
                "app_package_revision_not_found",
                "The package revision does not exist in this authenticated scope.",
            )
        })?;
    let staged = api
        .stager
        .load_staged_package(authenticated, package.content_digest.clone(), now)
        .await
        .map_err(|error| {
            api_error(
                StatusCode::NOT_FOUND,
                "app_custom_surface_package_missing",
                &format!("The staged package could not be re-admitted: {error}"),
            )
        })?;
    Ok(LoadedCustomSurfacePackage {
        installation,
        package,
        staged,
    })
}

async fn execute_admitted_bridge(
    api: &AppPlatformApi,
    authenticated: &AuthenticatedAppScope,
    message: &AppBridgeMessage,
    resources: Option<&AgentResources>,
    now: DateTime<Utc>,
) -> HttpResponse {
    // Every match arm shares this poll frame, including arms the request never
    // takes. Keep their child state machines on the heap: inline run-read and
    // cancellation futures previously reserved 254 KiB here before polling a
    // launch, exhausting the ordinary Actix worker stack deeper in admission.
    let limits = AppContractLimits::default();
    let payload_bytes = match serde_json::to_vec(&message.payload) {
        Ok(bytes) => bytes,
        Err(error) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "app_custom_surface_denied",
                &error.to_string(),
            );
        },
    };
    match message.method {
        AppBridgeMethod::Query => {
            let request = match decode_app_contract::<AppQueryRequest>(&payload_bytes, &limits) {
                Ok(request) => request,
                Err(error) => return contract_error_response(error),
            };
            if request.source_installation_id != message.installation_id {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "app_installation_mismatch",
                    "The query source installation must match the bridge session.",
                );
            }
            match api
                .entity_adapter
                .owner_query(authenticated, request, now)
                .boxed_local()
                .await
            {
                Ok(page) => HttpResponse::Ok()
                    .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
                    .json(page),
                Err(error) => entity_adapter_error_response(error),
            }
        },
        AppBridgeMethod::Mutate => {
            let command = match decode_app_contract::<AppMutationCommand>(&payload_bytes, &limits) {
                Ok(command) => command,
                Err(error) => return contract_error_response(error),
            };
            match api
                .entity_adapter
                .owner_mutate(authenticated, &message.installation_id, command, now)
                .boxed_local()
                .await
            {
                Ok(receipt) => HttpResponse::Ok()
                    .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
                    .json(receipt),
                Err(error) => entity_adapter_error_response(error),
            }
        },
        AppBridgeMethod::InvokeAction => {
            let Some(resources) = resources else {
                return api_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "app_custom_surface_worker",
                    "The custom-surface action owner is unavailable.",
                );
            };
            // A bridge session is transport liveness, not durable invocation
            // identity. Keep one host-owned provenance reference across frame
            // teardown/reopen so an exact idempotency retry can recover the
            // existing task instead of conflicting on an expired session ref.
            let caller = match AppReference::parse(APP_CUSTOM_SURFACE_ACTION_CALLER_REF) {
                Ok(caller) => caller,
                Err(error) => return contract_error_response(error),
            };
            // Scripted frames cannot safely mint mutable action/schema/grant
            // revisions or scope bindings. Prefer the minimal direct-owner
            // request and let the authenticated host resolve those fields at
            // admission. Retain the full invocation decoder as a compatibility
            // path for already-admitted scripted packages.
            if let Ok(request) =
                decode_app_contract::<AppDirectActionRequest>(&payload_bytes, &limits)
            {
                let Some(action_name) = message.view_or_action.as_ref() else {
                    return api_error(
                        StatusCode::BAD_REQUEST,
                        "app_action_mismatch",
                        "A direct scripted-surface launch must name its action.",
                    );
                };
                let action_id = match AppName::parse(action_name.to_string()) {
                    Ok(action_id) => action_id,
                    Err(error) => return contract_error_response(error),
                };
                // Boxed for the same reason as the HTTP launch adapter: this
                // bridge frame sits below the scripted-surface decoder on the
                // same Actix worker stack, so the launch state machine must
                // not be reserved in it.
                return match Box::pin(api.workflow_service().invoke_direct_input(
                    authenticated,
                    &message.installation_id,
                    &action_id,
                    request.idempotency_key,
                    request.input,
                    request.expected_installation_binding,
                    caller.clone(),
                    resources,
                    now,
                ))
                .await
                {
                    Ok(launch) => match custom_surface_reply_from_launch(
                        launch,
                        &message.installation_id,
                        Some(&action_id),
                    ) {
                        Ok(reply) => HttpResponse::Accepted()
                            .insert_header((
                                actix_web::http::header::CACHE_CONTROL,
                                "private, no-store",
                            ))
                            .json(reply),
                        Err(response) => response,
                    },
                    Err(error) => workflow_error_response(error),
                };
            }
            let mut invocation = match decode_app_contract::<AppActionInvocation<serde_json::Value>>(
                &payload_bytes,
                &limits,
            ) {
                Ok(invocation) => invocation,
                Err(error) => return contract_error_response(error),
            };
            if message
                .view_or_action
                .as_ref()
                .is_some_and(|action| action != &invocation.action_id)
            {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "app_action_mismatch",
                    "The action in the invocation must match the bridge message.",
                );
            }
            invocation.caller_surface_or_execution_ref = caller;
            match api
                .workflow_service()
                .invoke(
                    authenticated,
                    &message.installation_id,
                    invocation,
                    resources,
                    now,
                )
                .boxed_local()
                .await
            {
                Ok(launch) => match custom_surface_reply_from_launch(
                    launch,
                    &message.installation_id,
                    message.view_or_action.as_ref(),
                ) {
                    Ok(reply) => HttpResponse::Accepted()
                        .insert_header((
                            actix_web::http::header::CACHE_CONTROL,
                            "private, no-store",
                        ))
                        .json(reply),
                    Err(response) => response,
                },
                Err(error) => workflow_error_response(error),
            }
        },
        AppBridgeMethod::Subscribe => {
            let request: CustomSurfaceSubscriptionRequest =
                match serde_json::from_slice(&payload_bytes) {
                    Ok(request) => request,
                    Err(error) => {
                        return api_error(
                            StatusCode::BAD_REQUEST,
                            "invalid_app_contract",
                            &error.to_string(),
                        );
                    },
                };
            if request.limit == 0 || request.limit > MAX_APP_ENTITY_CHANGE_LIMIT {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_app_contract",
                    "The custom-surface subscription limit is invalid.",
                );
            }
            match api
                .entity_changes
                .read_after(
                    authenticated,
                    &message.installation_id,
                    message.surface_revision,
                    request.after_change_sequence,
                    request.limit,
                    now,
                )
                .boxed_local()
                .await
            {
                Ok(batch) => HttpResponse::Ok()
                    .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
                    .json(batch),
                Err(error) => entity_change_error_response(error),
            }
        },
        AppBridgeMethod::GetActionRun => {
            let request: AppCustomSurfaceRunReadRequest =
                match serde_json::from_slice(&payload_bytes) {
                    Ok(request) => request,
                    Err(_) => {
                        return api_error(
                            StatusCode::BAD_REQUEST,
                            "invalid_app_contract",
                            "The custom-surface run request is invalid.",
                        );
                    },
                };
            match read_custom_surface_action_run(
                api,
                authenticated,
                resources,
                &message.installation_id,
                message.view_or_action.as_ref(),
                request.run_ref,
                now,
            )
            .boxed_local()
            .await
            {
                Ok(reply) => app_custom_surface_run_reply_response(reply),
                Err(response) => response,
            }
        },
        AppBridgeMethod::WaitActionRun => {
            let request: AppCustomSurfaceRunWaitRequest =
                match serde_json::from_slice(&payload_bytes) {
                    Ok(request) => request,
                    Err(_) => {
                        return api_error(
                            StatusCode::BAD_REQUEST,
                            "invalid_app_contract",
                            "The custom-surface run wait request is invalid.",
                        );
                    },
                };
            if let Err(error) = request.validate() {
                return contract_error_response(error);
            }
            let mut last = None;
            for poll in 0..request.max_polls {
                let reply = match read_custom_surface_action_run(
                    api,
                    authenticated,
                    resources,
                    &message.installation_id,
                    message.view_or_action.as_ref(),
                    request.run_ref.clone(),
                    Utc::now(),
                )
                .boxed_local()
                .await
                {
                    Ok(reply) => reply,
                    Err(response) => return response,
                };
                let terminal = reply.terminal;
                last = Some(reply);
                if terminal || poll.saturating_add(1) >= request.max_polls {
                    break;
                }
                tokio::time::sleep(StdDuration::from_millis(request.poll_interval_ms)).await;
            }
            match last {
                Some(reply) => app_custom_surface_run_reply_response(reply),
                None => api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "app_custom_surface_worker",
                    "The custom-surface run wait produced no projection.",
                ),
            }
        },
        AppBridgeMethod::CancelActionRun => {
            let request: AppCustomSurfaceRunCancelRequest =
                match serde_json::from_slice(&payload_bytes) {
                    Ok(request) => request,
                    Err(_) => {
                        return api_error(
                            StatusCode::BAD_REQUEST,
                            "invalid_app_contract",
                            "The custom-surface cancellation request is invalid.",
                        );
                    },
                };
            match cancel_custom_surface_action_run(
                api,
                authenticated,
                resources,
                &message.installation_id,
                message.view_or_action.as_ref(),
                request,
                now,
            )
            .boxed_local()
            .await
            {
                Ok(reply) => app_custom_surface_run_reply_response(reply),
                Err(response) => response,
            }
        },
    }
}

fn app_custom_surface_run_reply_response(reply: AppCustomSurfaceRunReply) -> HttpResponse {
    let mut response = if reply.terminal {
        HttpResponse::Ok()
    } else {
        HttpResponse::Accepted()
    };
    response
        .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
        .json(reply)
}

fn custom_surface_reply_from_launch(
    launch: AppWorkflowLaunch,
    expected_installation_id: &AppInstallationId,
    expected_action_id: Option<&AppName>,
) -> Result<AppCustomSurfaceRunReply, HttpResponse> {
    if &launch.run_handle.installation_id != expected_installation_id
        || expected_action_id != Some(&launch.run_handle.action_id)
    {
        return Err(api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "app_run_identity_changed",
            "The action launch did not match the custom-surface binding.",
        ));
    }
    let run_ref = launch.run_handle.run_ref.clone();
    let (status, result, error) = match launch.result {
        Some(result) => (
            AppRunStatus::Completed,
            result.output.map(|output| output.value),
            result.error,
        ),
        None => (AppRunStatus::Queued, None, None),
    };
    let reply = AppCustomSurfaceRunReply {
        run_ref: run_ref.clone(),
        status,
        terminal: status.is_terminal(),
        result_withheld: false,
        cancellation_generation: None,
        result,
        error,
        receipt: None,
        retry_disposition: if status.is_terminal() {
            AppCustomSurfaceRetryDisposition::None
        } else {
            AppCustomSurfaceRetryDisposition::PollRun
        },
    };
    reply
        .validate_for(&run_ref)
        .map_err(contract_error_response)?;
    Ok(reply)
}

async fn read_custom_surface_action_run(
    api: &AppPlatformApi,
    authenticated: &AuthenticatedAppScope,
    resources: Option<&AgentResources>,
    expected_installation_id: &AppInstallationId,
    expected_action_id: Option<&AppName>,
    run_ref: AppReference,
    now: DateTime<Utc>,
) -> Result<AppCustomSurfaceRunReply, HttpResponse> {
    if !has_canonical_app_action_run_namespace(run_ref.as_str()) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "invalid_app_contract",
            "Custom-surface action runs require a canonical run:app-action: reference.",
        ));
    }
    let control = api
        .workflow_service()
        .resolve_run_control(authenticated, run_ref.as_str(), now)
        .await
        .map_err(workflow_error_response)?;
    if &control.run_handle().installation_id != expected_installation_id
        || expected_action_id != Some(&control.run_handle().action_id)
        || control.run_handle().run_ref != run_ref
    {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "app_action_run_not_found",
            "The action run does not belong to this custom-surface action.",
        ));
    }
    let run_handle = control.run_handle().clone();
    let task_id = control.task_id().to_owned();
    let task_service = resources
        .and_then(|resources| resources.artifact_v2_service.as_ref())
        .ok_or_else(|| {
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "app_task_runtime_unavailable",
                "The app task runtime is unavailable.",
            )
        })?;
    let scope = ScopeRef::system_internal_unauthenticated(
        &authenticated.scope().principal.to_string(),
        &authenticated.scope().workspace.to_string(),
    );
    let task = task_service
        .get_task(&scope, &task_id)
        .await
        .map_err(|error| workflow_error_response(error.into()))?;
    let mut status = app_run_status_from_task(&task.state.status).ok_or_else(|| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "app_run_state_invalid",
            "The app task has an unsupported lifecycle state.",
        )
    })?;
    let execution_id = if status.is_terminal() {
        task.state.latest_root_execution_id
    } else {
        task.state.active_root_execution_id
    };
    let cancellation_generation = if let Some(execution_id) = execution_id.as_deref() {
        match api
            .workflow_service()
            .action_cancellation_projection_for_control(&control, execution_id)
            .await
            .map_err(workflow_error_response)?
        {
            Some((receipt, uncertain)) => {
                if uncertain {
                    status = AppRunStatus::Uncertain;
                } else if !status.is_terminal() {
                    status = AppRunStatus::Cancelling;
                }
                Some(receipt.generation)
            },
            None => None,
        }
    } else {
        None
    };
    let mut result_withheld = false;
    let (result, error) = match api
        .workflow_service()
        .result_for_control(authenticated, control)
        .await
    {
        Ok(read) => {
            let (current_handle, current_task_id, result) = read.into_parts();
            if current_handle != run_handle || current_task_id != task_id {
                return Err(api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "app_run_identity_changed",
                    "The app run identity changed during custom-surface polling.",
                ));
            }
            match result {
                Some(result) => {
                    status = AppRunStatus::Completed;
                    (result.output.map(|output| output.value), result.error)
                },
                None if status == AppRunStatus::Completed => {
                    return Err(api_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "app_terminal_result_missing",
                        "The app run completed without its required typed result.",
                    ));
                },
                None => (None, None),
            }
        },
        Err(owner_error) if owner_error.is_result_disclosure_denial() => {
            status = AppRunStatus::Completed;
            result_withheld = true;
            (None, None)
        },
        Err(owner_error) => return Err(workflow_error_response(owner_error)),
    };
    let reply = AppCustomSurfaceRunReply {
        run_ref: run_ref.clone(),
        status,
        terminal: status.is_terminal(),
        result_withheld,
        cancellation_generation,
        result,
        error,
        receipt: None,
        retry_disposition: if status == AppRunStatus::Uncertain {
            AppCustomSurfaceRetryDisposition::OutcomeUncertain
        } else if status.is_terminal() {
            AppCustomSurfaceRetryDisposition::None
        } else {
            AppCustomSurfaceRetryDisposition::PollRun
        },
    };
    reply
        .validate_for(&run_ref)
        .map_err(contract_error_response)?;
    Ok(reply)
}

async fn cancel_custom_surface_action_run(
    api: &AppPlatformApi,
    authenticated: &AuthenticatedAppScope,
    resources: Option<&AgentResources>,
    expected_installation_id: &AppInstallationId,
    expected_action_id: Option<&AppName>,
    request: AppCustomSurfaceRunCancelRequest,
    now: DateTime<Utc>,
) -> Result<AppCustomSurfaceRunReply, HttpResponse> {
    if !has_canonical_app_action_run_namespace(request.run_ref.as_str()) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "invalid_app_contract",
            "Custom-surface cancellation requires a canonical run:app-action: reference.",
        ));
    }
    let control = api
        .workflow_service()
        .resolve_run_control(authenticated, request.run_ref.as_str(), now)
        .await
        .map_err(workflow_error_response)?;
    if &control.run_handle().installation_id != expected_installation_id
        || expected_action_id != Some(&control.run_handle().action_id)
        || control.run_handle().run_ref != request.run_ref
    {
        return Err(api_error(
            StatusCode::NOT_FOUND,
            "app_action_run_not_found",
            "The action run does not belong to this custom-surface action.",
        ));
    }
    let task_service = resources
        .and_then(|resources| resources.artifact_v2_service.as_ref())
        .ok_or_else(|| {
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "app_task_runtime_unavailable",
                "The app task runtime is unavailable.",
            )
        })?;
    let canonical_request = AppActionCancellationRequest {
        expected_generation: request.expected_generation,
        idempotency_key: request.idempotency_key,
    };
    let (receipt, _task) = task_service
        .cancel_app_action_run(&control, &canonical_request)
        .await
        .map_err(|error| workflow_error_response(error.into()))?;
    if receipt.run_ref != request.run_ref
        || receipt.idempotency_key != canonical_request.idempotency_key
        || receipt.generation != canonical_request.expected_generation.saturating_add(1)
    {
        return Err(api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "app_run_identity_changed",
            "The cancellation receipt did not match the custom-surface request.",
        ));
    }
    let status = receipt.status;
    let reply = AppCustomSurfaceRunReply {
        run_ref: request.run_ref.clone(),
        status,
        terminal: status.is_terminal(),
        result_withheld: false,
        cancellation_generation: Some(receipt.generation),
        result: None,
        error: None,
        receipt: Some(AppCustomSurfaceCancellationReceipt {
            generation: receipt.generation,
            idempotency_key: receipt.idempotency_key,
            status,
            requested_at: receipt.requested_at,
        }),
        retry_disposition: if status == AppRunStatus::Uncertain {
            AppCustomSurfaceRetryDisposition::OutcomeUncertain
        } else if status.is_terminal() {
            AppCustomSurfaceRetryDisposition::None
        } else {
            AppCustomSurfaceRetryDisposition::PollRun
        },
    };
    reply
        .validate_for(&request.run_ref)
        .map_err(contract_error_response)?;
    Ok(reply)
}

async fn pump_scripted_surface_host(
    api: &AppPlatformApi,
    authenticated: &AuthenticatedAppScope,
    loaded: &LoadedCustomSurfacePackage,
    session_ref: &AppReference,
    resources: Option<&AgentResources>,
) -> Result<(), HttpResponse> {
    let deadline = tokio::time::Instant::now() + StdDuration::from_secs(5);
    let limits = AppContractLimits::default();
    let surface_revision = loaded.installation.active_surface_revision.ok_or_else(|| {
        api_error(
            StatusCode::CONFLICT,
            "app_custom_surface_unavailable",
            "The installation has no active surface revision.",
        )
    })?;
    let grant_revision = loaded.installation.grant_revision.ok_or_else(|| {
        api_error(
            StatusCode::CONFLICT,
            "app_custom_surface_unavailable",
            "The installation has no active grant revision.",
        )
    })?;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(custom_surface_error_response(
                AppCustomSurfaceRuntimeError::WatchdogTripped,
            ));
        }
        let requests = api
            .custom_surfaces
            .take_worker_bridge_requests(session_ref)
            .map_err(custom_surface_error_response)?;
        for request in requests {
            // Reopen the installation/package/grant owner for every worker
            // round trip. The package loaded when the host opened is evidence
            // of that moment only; update, disable, revoke or substitution
            // between calls must retire this worker rather than extending its
            // stale authority until the pump deadline.
            let current_now = Utc::now();
            let current = match load_enabled_custom_surface_package(
                api,
                authenticated,
                loaded.installation.installation_id.as_str(),
                current_now,
            )
            .await
            {
                Ok(current) => current,
                Err(response) => {
                    api.custom_surfaces.abort_worker_pump(session_ref);
                    return Err(response);
                },
            };
            if current.installation.package_revision_ref != loaded.installation.package_revision_ref
                || current.installation.active_surface_revision != Some(surface_revision)
                || current.installation.grant_revision != Some(grant_revision)
            {
                api.custom_surfaces.abort_worker_pump(session_ref);
                return Err(custom_surface_error_response(
                    AppCustomSurfaceRuntimeError::Bridge(AppSandboxError::StaleRevision),
                ));
            }
            let message = match api.custom_surfaces.admit_worker_bridge(
                session_ref,
                &request,
                current_now,
                authenticated.session_ref(),
                &loaded.installation.package_revision_ref,
                surface_revision,
                grant_revision,
                &limits,
            ) {
                Ok(message) => message,
                Err(error) => {
                    let _ = api.custom_surfaces.complete_worker_bridge(
                        session_ref,
                        &request,
                        Err((
                            "app_custom_surface_denied",
                            "The custom-surface bridge request was refused.",
                        )),
                    );
                    api.custom_surfaces.abort_worker_pump(session_ref);
                    return Err(custom_surface_error_response(error));
                },
            };
            let response = match tokio::time::timeout_at(
                deadline,
                execute_admitted_bridge(api, authenticated, &message, resources, current_now)
                    .boxed_local(),
            )
            .await
            {
                Ok(response) => response,
                Err(_) => {
                    return Err(custom_surface_error_response(
                        AppCustomSurfaceRuntimeError::WatchdogTripped,
                    ));
                },
            };
            let status = response.status();
            let bytes = match actix_web::body::to_bytes(response.into_body()).await {
                Ok(bytes) => bytes,
                Err(_) => {
                    return Err(api_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "app_custom_surface_worker",
                        "The custom-surface bridge result was unavailable.",
                    ));
                },
            };
            let value = match AppCustomSurfaceRuntime::decode_worker_bridge_result(&bytes, &limits)
            {
                Ok(value) => value,
                Err(_) => {
                    return Err(api_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "app_custom_surface_worker",
                        "The custom-surface bridge result was invalid.",
                    ));
                },
            };
            if status.is_success()
                && AppCustomSurfaceRuntime::validate_worker_bridge_result(&message, &value).is_err()
            {
                api.custom_surfaces.abort_worker_pump(session_ref);
                return Err(api_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "app_custom_surface_worker",
                    "The custom-surface bridge result failed correlation.",
                ));
            }
            let completion = if status.is_success() {
                Ok(value)
            } else {
                Err((
                    "app_custom_surface_denied",
                    "The custom-surface bridge request was refused.",
                ))
            };
            api.custom_surfaces
                .complete_worker_bridge(session_ref, &request, completion)
                .map_err(custom_surface_error_response)?;
        }
        if api
            .custom_surfaces
            .worker_render_ready(session_ref)
            .map_err(custom_surface_error_response)?
        {
            return Ok(());
        }
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }
}

fn custom_surface_error_response(error: AppCustomSurfaceRuntimeError) -> HttpResponse {
    let (status, code) = match &error {
        AppCustomSurfaceRuntimeError::SessionGone
        | AppCustomSurfaceRuntimeError::Asset(AppSurfaceAssetError::MissingMember)
        | AppCustomSurfaceRuntimeError::Asset(AppSurfaceAssetError::NotASurfacePath) => {
            (StatusCode::NOT_FOUND, "app_custom_surface_not_found")
        },
        AppCustomSurfaceRuntimeError::WatchdogTripped
        | AppCustomSurfaceRuntimeError::SessionLimit
        | AppCustomSurfaceRuntimeError::KillableWorkerRequired => {
            (StatusCode::TOO_MANY_REQUESTS, "app_custom_surface_watchdog")
        },
        AppCustomSurfaceRuntimeError::Worker(_) => {
            (StatusCode::SERVICE_UNAVAILABLE, "app_custom_surface_worker")
        },
        AppCustomSurfaceRuntimeError::Bridge(AppSandboxError::Replay)
        | AppCustomSurfaceRuntimeError::Bridge(AppSandboxError::StaleRevision)
        | AppCustomSurfaceRuntimeError::Bridge(AppSandboxError::SessionTornDown) => {
            (StatusCode::CONFLICT, "app_custom_surface_stale")
        },
        _ => (StatusCode::BAD_REQUEST, "app_custom_surface_denied"),
    };
    api_error(status, code, &error.to_string())
}

async fn admit_uploaded_package(
    api: &AppPlatformApi,
    req: &HttpRequest,
    mut payload: web::Payload,
) -> Result<(tokio::sync::OwnedSemaphorePermit, AdmittedAppPackageArchive), HttpResponse> {
    if !content_type_matches(req, APP_PACKAGE_ARCHIVE_MEDIA_TYPE) {
        return Err(api_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_package_media_type",
            "Expected a package-only ZIP archive.",
        ));
    }
    if req
        .headers()
        .get(actix_web::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > APP_PACKAGE_ARCHIVE_MAX_BYTES)
    {
        return Err(transfer_error_response(
            AppPackageTransferError::ArchiveTooLarge,
        ));
    }

    // Admit before buffering the body. Otherwise many slow clients could each
    // retain a maximum-sized archive while merely waiting for the one bounded
    // transfer worker.
    let transfer_permit = match Arc::clone(&api.transfer_slots).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return Err(api_error(
                StatusCode::TOO_MANY_REQUESTS,
                "app_transfer_overloaded",
                "App package transfer capacity is busy; retry later.",
            ));
        },
    };

    let bytes = match tokio::time::timeout(APP_PACKAGE_UPLOAD_TIMEOUT, async move {
        let mut bytes = Vec::new();
        while let Some(chunk) = payload.next().await {
            let chunk = chunk.map_err(|error| {
                api_error(
                    StatusCode::BAD_REQUEST,
                    "package_body_read_failed",
                    &error.to_string(),
                )
            })?;
            let next_len = bytes
                .len()
                .checked_add(chunk.len())
                .ok_or_else(|| transfer_error_response(AppPackageTransferError::ArchiveTooLarge))?;
            if next_len > APP_PACKAGE_ARCHIVE_MAX_BYTES {
                return Err(transfer_error_response(
                    AppPackageTransferError::ArchiveTooLarge,
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok::<Vec<u8>, HttpResponse>(bytes)
    })
    .await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(response)) => return Err(response),
        Err(_) => {
            return Err(api_error(
                StatusCode::REQUEST_TIMEOUT,
                "app_package_upload_timeout",
                "The package upload exceeded its fixed time limit.",
            ));
        },
    };

    let admitted = match tokio::task::spawn_blocking(move || admit_package_archive(&bytes)).await {
        Ok(Ok(admitted)) => admitted,
        Ok(Err(error)) => return Err(transfer_error_response(error)),
        Err(error) => {
            return Err(api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_transfer_worker_failed",
                &error.to_string(),
            ));
        },
    };
    Ok((transfer_permit, admitted))
}

/// `POST /api/magician/v2/apps/packages/import`
pub async fn import_app_package_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let (_transfer_permit, admitted) =
        match admit_uploaded_package(api.get_ref(), &req, payload).await {
            Ok(admitted) => admitted,
            Err(response) => return response,
        };
    let (package, candidate) = admitted.into_parts();
    let staged = match api
        .stager
        .stage_candidate(&authenticated, candidate, Utc::now())
        .await
    {
        Ok(staged) => staged,
        Err(error) => return staging_error_response(error),
    };
    let stage_outcome = match staged.outcome() {
        AppPackageStageOutcome::Created => "created",
        AppPackageStageOutcome::AlreadyPresent => "already_present",
    };
    let status = if staged.outcome() == AppPackageStageOutcome::Created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    HttpResponse::build(status)
        .insert_header((actix_web::http::header::CACHE_CONTROL, "no-store"))
        .json(AppPackageImportResponse {
            state: "staged_for_local_conformance",
            stage_outcome,
            package_id: package.package_id,
            source_publisher_identity: package.publisher_identity,
            semantic_version: package.semantic_version,
            package_content_digest: package.package_content_digest,
            requirements: AppPackageImportRequirements::required_for_every_import(),
            local_identity_resolution_required: true,
            foreign_authority_transferred: false,
        })
}

/// `POST /api/magician/v2/apps/packages/candidates`
///
/// Unlike package import, this route reruns local conformance and creates the
/// inert review candidate. It still cannot approve or enable the installation.
pub async fn publish_app_candidate_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let (_transfer_permit, admitted) =
        match admit_uploaded_package(api.get_ref(), &req, payload).await {
            Ok(admitted) => admitted,
            Err(response) => return response,
        };
    let request_id = match header_value(&req, "x-magician-app-request-id")
        .ok_or_else(|| {
            api_error(
                StatusCode::BAD_REQUEST,
                "app_candidate_identity_required",
                "Candidate publication requires an exact caller-retained request identity.",
            )
        })
        .and_then(|value| AppReference::parse(value).map_err(contract_error_response))
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let expected_package_id = header_value(&req, "x-magician-app-package-id");
    let expected_source_publisher = header_value(&req, "x-magician-app-source-publisher");
    let expected_content_digest = header_value(&req, "x-magician-app-content-digest");
    let package = admitted.package();
    if expected_package_id.as_deref() != Some(package.package_id.as_str())
        || expected_source_publisher.as_deref() != Some(package.publisher_identity.as_str())
        || expected_content_digest.as_deref() != Some(package.package_content_digest.as_str())
    {
        return api_error(
            StatusCode::CONFLICT,
            "app_candidate_source_identity_mismatch",
            "Candidate publication did not match the exact staged package identities.",
        );
    }
    let target_installation = header_value(&req, "x-magician-app-target-installation");
    let target_kind = header_value(&req, "x-magician-app-attempt-kind");
    let publication =
        match (target_installation, target_kind) {
            (None, None) => {
                api.candidate_publications
                    .publish_archive_candidate(&authenticated, admitted, now)
                    .await
            },
            (Some(installation), Some(kind)) => {
                let installation = match AppInstallationId::parse(installation) {
                    Ok(value) => value,
                    Err(error) => return contract_error_response(error),
                };
                let kind = match kind.as_str() {
                "update" => AppLifecycleAttemptKind::Update,
                "reinstall" => AppLifecycleAttemptKind::Reinstall,
                _ => return api_error(
                    StatusCode::BAD_REQUEST,
                    "app_candidate_attempt_kind_invalid",
                    "Existing-installation publication requires attempt kind update or reinstall.",
                ),
            };
                api.candidate_publications
                    .publish_archive_revision_candidate(
                        &authenticated,
                        admitted,
                        installation,
                        kind,
                        now,
                    )
                    .await
            },
            _ => return api_error(
                StatusCode::BAD_REQUEST,
                "app_candidate_target_incomplete",
                "Target installation and update/reinstall attempt kind must be supplied together.",
            ),
        };
    match publication {
        Ok(receipt) => {
            let status = if receipt.publication_outcome == AppCandidatePublicationOutcome::Created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            HttpResponse::build(status)
                .insert_header((actix_web::http::header::CACHE_CONTROL, "no-store"))
                .json(AppCandidatePublicationHttpReceipt {
                    request_id,
                    publication: receipt,
                })
        },
        Err(error) => candidate_publication_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/procedure-revisions`
///
/// Publishes exact standalone procedure bytes as inert dependency evidence.
/// The server derives identity and allocates the scoped revision; this route
/// cannot install the procedure globally or grant app execution authority.
pub async fn publish_app_procedure_revision_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let bytes = match read_bounded_raw_body(
        &req,
        &mut payload,
        APP_PROCEDURE_SKILL_MEDIA_TYPE,
        APP_PROCEDURE_SKILL_MAX_BYTES,
        "app_procedure_body",
    )
    .await
    {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };
    match api
        .procedure_publications
        .publish(&authenticated, &bytes, now)
        .await
    {
        Ok(receipt) => {
            let status = if receipt.publication_outcome == AppProcedurePublicationOutcome::Created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            HttpResponse::build(status)
                .insert_header((actix_web::http::header::CACHE_CONTROL, "no-store"))
                .json(receipt)
        },
        Err(error) => procedure_publication_error_response(error),
    }
}

/// `POST /api/magician/v2/apps/capability-revisions`
///
/// Publishes exact standalone executable-capability bytes as inert
/// dependency evidence. The server derives `capability:{name}` and
/// allocates the scoped revision; this route cannot install a global
/// tool or grant dispatch authority.
pub async fn publish_app_capability_revision_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let bytes = match read_bounded_raw_body(
        &req,
        &mut payload,
        APP_CAPABILITY_SKILL_MEDIA_TYPE,
        APP_CAPABILITY_SKILL_MAX_BYTES,
        "app_capability_body",
    )
    .await
    {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };
    match api
        .capability_publications
        .publish(&authenticated, &bytes, now)
        .await
    {
        Ok(receipt) => {
            let status = if receipt.publication_outcome == AppCapabilityPublicationOutcome::Created
            {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            HttpResponse::build(status)
                .insert_header((actix_web::http::header::CACHE_CONTROL, "no-store"))
                .json(receipt)
        },
        Err(error) => capability_publication_error_response(error),
    }
}

fn archive_passphrase(req: &HttpRequest) -> Result<Option<AppArchivePassphrase>, HttpResponse> {
    req.headers()
        .get("x-magician-app-archive-passphrase")
        .map(|value| {
            AppArchivePassphrase::parse(value.as_bytes()).map_err(portable_archive_error_response)
        })
        .transpose()
}

async fn encode_exportable_package_archive(
    api: &AppPlatformApi,
    authenticated: &AuthenticatedAppScope,
    installation_id: &AppInstallationId,
    now: DateTime<Utc>,
) -> Result<Vec<u8>, HttpResponse> {
    let installation = api
        .registry
        .installation(authenticated, installation_id, now.to_owned())
        .await
        .map_err(registry_error_response)?
        .ok_or_else(|| {
            api_error(
                StatusCode::NOT_FOUND,
                "app_installation_not_found",
                "The app installation does not exist in this authenticated scope.",
            )
        })?;
    let source_state = export_source_state(installation.lifecycle.status).ok_or_else(|| {
        api_error(
            StatusCode::CONFLICT,
            "app_package_export_not_available",
            "The installation lifecycle state does not permit package export.",
        )
    })?;
    let package_revision_ref = installation.package_revision_ref;
    let package_revision = api
        .registry
        .package_revision(authenticated, &package_revision_ref, now.to_owned())
        .await
        .map_err(registry_error_response)?
        .ok_or_else(|| corrupt_registry_response("package revision"))?;
    let dependency_lock = api
        .registry
        .package_dependency_lock(authenticated, &package_revision_ref, now.to_owned())
        .await
        .map_err(registry_error_response)?
        .ok_or_else(|| corrupt_registry_response("dependency lock"))?;
    let staged = api
        .stager
        .load_staged_package(
            authenticated,
            package_revision.content_digest.clone(),
            Utc::now(),
        )
        .await
        .map_err(staging_error_response)?;
    let portable = build_package_archive_manifest(
        package_revision_ref,
        &package_revision,
        staged.candidate(),
        &dependency_lock,
        source_state,
        Vec::new(),
        &AppContractLimits::default(),
    )
    .map_err(|error| {
        api_error(
            StatusCode::CONFLICT,
            "app_package_export_rejected",
            &error.to_string(),
        )
    })?;
    tokio::task::spawn_blocking(move || encode_package_archive(&portable, staged.candidate()))
        .await
        .map_err(|error| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_transfer_worker_failed",
                &error.to_string(),
            )
        })?
        .map_err(transfer_error_response)
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/portable-exports`
pub async fn export_app_portable_archive_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let request =
        match read_bounded_app_contract::<AppPortableExportRequest>(&req, &mut payload).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    let passphrase = match archive_passphrase(&req) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if matches!(
        request.protection,
        AppArchiveProtectionRequest::Default | AppArchiveProtectionRequest::Encrypted
    ) && passphrase.is_none()
    {
        return api_error(
            StatusCode::BAD_REQUEST,
            "app_archive_passphrase_required",
            "Encrypted data and combined exports require a passphrase.",
        );
    }
    if matches!(
        request.protection,
        AppArchiveProtectionRequest::ExplicitPlaintext
    ) && passphrase.is_some()
    {
        return api_error(
            StatusCode::BAD_REQUEST,
            "app_archive_passphrase_unexpected",
            "The explicit plaintext export action does not accept a passphrase.",
        );
    }
    let transfer_permit = match Arc::clone(&api.transfer_slots).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return api_error(
                StatusCode::TOO_MANY_REQUESTS,
                "app_transfer_overloaded",
                "App archive transfer capacity is busy; retry later.",
            );
        },
    };
    let data = match api
        .data_portability
        .export_data(&authenticated, &installation_id, now.to_owned())
        .await
    {
        Ok(value) => value,
        Err(error) => return entity_portability_error_response(error),
    };
    let (logical, package_bytes) = match request.kind {
        AppPortableExportKind::Data => (AppLogicalArchive::Data { data }, None),
        AppPortableExportKind::Combined => {
            let bytes = match encode_exportable_package_archive(
                api.get_ref(),
                &authenticated,
                &installation_id,
                now.to_owned(),
            )
            .await
            {
                Ok(value) => value,
                Err(response) => return response,
            };
            let package = match admit_package_archive(&bytes) {
                Ok(value) => value.package().clone(),
                Err(error) => return transfer_error_response(error),
            };
            (AppLogicalArchive::Combined { package, data }, Some(bytes))
        },
    };
    let plaintext_approval = if matches!(
        request.protection,
        AppArchiveProtectionRequest::ExplicitPlaintext
    ) {
        let logical_digest = match logical.logical_digest(&AppContractLimits::default()) {
            Ok(value) => value,
            Err(error) => return portable_archive_error_response(error.into()),
        };
        match AppPlaintextExportApproval::from_warned_user_action(
            &authenticated,
            request.request_id.clone(),
            logical_digest,
            now.to_owned(),
            now + ChronoDuration::minutes(5),
        ) {
            Ok(value) => Some(value),
            Err(error) => return portable_archive_error_response(error.into()),
        }
    } else {
        None
    };
    let plan = match authorize_archive_write(
        &logical,
        request.protection,
        plaintext_approval.as_ref(),
        &authenticated,
        now.to_owned(),
        &AppContractLimits::default(),
    ) {
        Ok(value) => value,
        Err(error) => return portable_archive_error_response(error.into()),
    };
    let encoded = match tokio::task::spawn_blocking(move || {
        encode_app_portable_archive(
            logical,
            package_bytes.as_deref(),
            &plan,
            passphrase.as_ref(),
            &authenticated,
            now,
        )
    })
    .await
    {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => return portable_archive_error_response(error),
        Err(error) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_archive_worker_failed",
                &error.to_string(),
            )
        },
    };
    let content_length = encoded.bytes.len();
    let bytes = web::Bytes::from(encoded.bytes);
    let stream = futures_util::stream::unfold(
        (bytes, 0usize, transfer_permit),
        |(bytes, offset, permit)| async move {
            if offset >= bytes.len() {
                return None;
            }
            let end = offset.saturating_add(64 * 1_024).min(bytes.len());
            Some((
                Ok::<web::Bytes, actix_web::Error>(bytes.slice(offset..end)),
                (bytes, end, permit),
            ))
        },
    );
    HttpResponse::Ok()
        .insert_header((actix_web::http::header::CONTENT_TYPE, encoded.media_type))
        .insert_header((
            actix_web::http::header::CONTENT_LENGTH,
            content_length.to_string(),
        ))
        .insert_header(("x-magician-app-request-id", request.request_id.as_str()))
        .insert_header((
            "x-magician-app-logical-digest",
            encoded.receipt.logical_payload_digest.as_str(),
        ))
        .insert_header((
            "x-magician-app-envelope-digest",
            encoded.receipt.envelope_header_digest.as_str(),
        ))
        .insert_header((
            "x-magician-app-ciphertext-digest",
            encoded.receipt.ciphertext_digest.as_str(),
        ))
        .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
        .insert_header((
            actix_web::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}.appdata\"", installation_id),
        ))
        .streaming(stream)
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/data-imports/preview`
pub async fn preview_app_data_import_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let destination = match AppInstallationId::parse(path.into_inner()) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let request_id = match header_value(&req, "x-magician-app-request-id")
        .ok_or_else(|| {
            api_error(
                StatusCode::BAD_REQUEST,
                "app_import_request_id_required",
                "A caller-retained import request identity is required.",
            )
        })
        .and_then(|value| AppReference::parse(value).map_err(contract_error_response))
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let media_type = req
        .headers()
        .get(actix_web::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    if !matches!(
        media_type,
        Some(APP_PORTABLE_ARCHIVE_ENCRYPTED_MEDIA_TYPE | APP_PORTABLE_ARCHIVE_PLAINTEXT_MEDIA_TYPE)
    ) {
        return api_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_app_archive_media_type",
            "Expected a Magician data or combined archive.",
        );
    }
    let passphrase = match archive_passphrase(&req) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let passphrase_supplied = passphrase.is_some();
    let transfer_permit = match Arc::clone(&api.transfer_slots).try_acquire_owned() {
        Ok(value) => value,
        Err(_) => {
            return api_error(
                StatusCode::TOO_MANY_REQUESTS,
                "app_transfer_overloaded",
                "App archive transfer capacity is busy; retry later.",
            )
        },
    };
    let bytes = match read_bounded_raw_body(
        &req,
        &mut payload,
        media_type.unwrap(),
        APP_PORTABLE_ARCHIVE_MAX_BYTES,
        "app_archive",
    )
    .await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let decoded = match tokio::task::spawn_blocking(move || {
        decode_app_portable_archive(&bytes, passphrase.as_ref())
    })
    .await
    {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => return portable_archive_error_response(error),
        Err(error) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_archive_worker_failed",
                &error.to_string(),
            )
        },
    };
    let media_declares_encrypted = media_type == Some(APP_PORTABLE_ARCHIVE_ENCRYPTED_MEDIA_TYPE);
    if decoded.receipt.encrypted != media_declares_encrypted
        || (!decoded.receipt.encrypted && passphrase_supplied)
    {
        return api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "app_archive_media_mismatch",
            "The app archive envelope did not match its declared protection mode.",
        );
    }
    drop(transfer_permit);
    let package_payload_present = decoded.package_archive.is_some();
    let source = match decoded.logical {
        AppLogicalArchive::Data { data } | AppLogicalArchive::Combined { data, .. } => data,
        AppLogicalArchive::Package { .. } => {
            return api_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "app_data_archive_required",
                "Package-only archives use the package import route.",
            );
        },
    };
    let preview = match api
        .data_portability
        .preview_import(&authenticated, &destination, &source, now.to_owned())
        .await
    {
        Ok(value) => value,
        Err(error) => return entity_portability_error_response(error),
    };
    let new_expires_at = now + ChronoDuration::seconds(APP_PORTABILITY_APPROVAL_LIFETIME_SECONDS);
    {
        let mut sessions = api
            .data_import_sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions.retain(|_, session| session.expires_at > now);
        let key = preview.preview_digest.to_string();
        if !sessions.contains_key(&key) && sessions.len() >= APP_PORTABILITY_MAX_PENDING_IMPORTS {
            return api_error(
                StatusCode::TOO_MANY_REQUESTS,
                "app_import_preview_capacity",
                "Too many app data imports are awaiting review.",
            );
        }
        let prior = sessions.get(&key).cloned();
        // Once an opaque approval exists its original expiry cannot be
        // extended by replaying the preview transport.
        let expires_at = prior
            .as_ref()
            .filter(|value| value.approval.is_some())
            .map_or(new_expires_at, |value| value.expires_at);
        sessions.insert(
            key,
            AppDataImportHttpSession {
                scope_binding_ref: authenticated.scope_binding_ref().clone(),
                destination_installation_id: destination,
                source,
                preview: preview.clone(),
                approval_ref: prior.as_ref().and_then(|value| value.approval_ref.clone()),
                approval: prior.and_then(|value| value.approval),
                expires_at,
            },
        );
    }
    HttpResponse::Ok()
        .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
        .json(AppDataImportPreviewHttpReceipt {
            request_id,
            archive_receipt: decoded.receipt,
            package_payload_present,
            foreign_authority_transferred: false,
            preview,
        })
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/data-imports/approve`
pub async fn approve_app_data_import_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let destination = match AppInstallationId::parse(path.into_inner()) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let request =
        match read_bounded_app_contract::<AppDataImportApprovalRequest>(&req, &mut payload).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    let key = request.preview_digest.to_string();
    let session = api
        .data_import_sessions
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
        .cloned();
    let Some(session) = session else {
        return api_error(
            StatusCode::NOT_FOUND,
            "app_import_preview_not_found",
            "The reviewed data-import preview is unavailable or expired.",
        );
    };
    if session.expires_at <= now
        || session.scope_binding_ref != *authenticated.scope_binding_ref()
        || session.destination_installation_id != destination
        || session.preview.preview_digest != request.preview_digest
    {
        return api_error(
            StatusCode::CONFLICT,
            "app_import_preview_stale",
            "The data-import preview no longer matches this authenticated destination.",
        );
    }
    if let (Some(approval_ref), Some(_)) = (session.approval_ref.clone(), session.approval.as_ref())
    {
        return HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(AppDataImportApprovalHttpReceipt {
                request_id: request.request_id,
                approval_ref,
                preview_digest: request.preview_digest,
                expires_at: session.expires_at,
            });
    }
    let digest =
        AppDigest::blake3(format!("{}\0{}", request.request_id, request.preview_digest).as_bytes());
    let approval_ref = match AppReference::parse(format!(
        "approval:data-import:{}",
        digest.as_str().trim_start_matches("blake3:")
    )) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let approval = match api.data_portability.approve_import(
        &authenticated,
        approval_ref.clone(),
        &session.preview,
        now.to_owned(),
        session.expires_at,
    ) {
        Ok(value) => value,
        Err(error) => return entity_portability_error_response(error),
    };
    {
        let mut sessions = api
            .data_import_sessions
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(current) = sessions.get_mut(&key) else {
            return api_error(
                StatusCode::CONFLICT,
                "app_import_preview_stale",
                "The data-import preview changed before approval committed.",
            );
        };
        if current.expires_at <= now
            || current.scope_binding_ref != *authenticated.scope_binding_ref()
            || current.destination_installation_id != destination
            || current.preview.preview_digest != request.preview_digest
        {
            return api_error(
                StatusCode::CONFLICT,
                "app_import_preview_stale",
                "The data-import preview changed before approval committed.",
            );
        }
        if let (Some(current_ref), Some(_)) =
            (current.approval_ref.clone(), current.approval.as_ref())
        {
            return HttpResponse::Ok()
                .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
                .json(AppDataImportApprovalHttpReceipt {
                    request_id: request.request_id,
                    approval_ref: current_ref,
                    preview_digest: request.preview_digest,
                    expires_at: current.expires_at,
                });
        }
        current.approval_ref = Some(approval_ref.clone());
        current.approval = Some(approval);
    }
    HttpResponse::Ok()
        .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
        .json(AppDataImportApprovalHttpReceipt {
            request_id: request.request_id,
            approval_ref,
            preview_digest: request.preview_digest,
            expires_at: session.expires_at,
        })
}

/// `POST /api/magician/v2/apps/installations/{installation_id}/data-imports/commit`
pub async fn commit_app_data_import_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let destination = match AppInstallationId::parse(path.into_inner()) {
        Ok(value) => value,
        Err(error) => return contract_error_response(error),
    };
    let request =
        match read_bounded_app_contract::<AppDataImportCommitRequest>(&req, &mut payload).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    let key = request.preview_digest.to_string();
    let session = api
        .data_import_sessions
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
        .cloned();
    let Some(session) = session else {
        // The canonical commit stores its receipt transactionally. Recover it
        // directly after process restart/session eviction; this read can only
        // replay an already completed import and cannot authorize a new one.
        return match api
            .data_portability
            .committed_import_receipt(&authenticated, &destination, &request.preview_digest, now)
            .await
        {
            Ok(Some(receipt)) if receipt.approval_ref == request.approval_ref => HttpResponse::Ok()
                .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
                .json(AppDataImportCommitHttpReceipt {
                    request_id: request.request_id,
                    foreign_authority_transferred: false,
                    receipt,
                }),
            Ok(Some(_)) => api_error(
                StatusCode::CONFLICT,
                "app_import_approval_mismatch",
                "The data-import commit did not match the reviewed destination and preview.",
            ),
            Ok(None) => api_error(
                StatusCode::NOT_FOUND,
                "app_import_approval_not_found",
                "The reviewed data-import approval is unavailable or expired.",
            ),
            Err(error) => entity_portability_error_response(error),
        };
    };
    if session.scope_binding_ref != *authenticated.scope_binding_ref()
        || session.destination_installation_id != destination
        || session.preview.preview_digest != request.preview_digest
        || session.approval_ref.as_ref() != Some(&request.approval_ref)
    {
        return api_error(
            StatusCode::CONFLICT,
            "app_import_approval_mismatch",
            "The data-import commit did not match the reviewed destination and preview.",
        );
    }
    let Some(approval) = session.approval else {
        return api_error(
            StatusCode::CONFLICT,
            "app_import_approval_required",
            "The data-import preview must be approved before commit.",
        );
    };
    let receipt = match api
        .data_portability
        .commit_import(
            &authenticated,
            session.source,
            session.preview,
            approval,
            now,
        )
        .await
    {
        Ok(value) => value,
        Err(error) => return entity_portability_error_response(error),
    };
    HttpResponse::Ok()
        .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
        .json(AppDataImportCommitHttpReceipt {
            request_id: request.request_id,
            foreign_authority_transferred: false,
            receipt,
        })
}

/// `GET /api/magician/v2/apps/installations/{installation_id}/package-export`
pub async fn export_app_package_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let installation_id = match AppInstallationId::parse(path.into_inner()) {
        Ok(installation_id) => installation_id,
        Err(error) => return contract_error_response(error),
    };
    let installation = match api
        .registry
        .installation(&authenticated, &installation_id, now.to_owned())
        .await
    {
        Ok(Some(installation)) => installation,
        Ok(None) => {
            return api_error(
                StatusCode::NOT_FOUND,
                "app_installation_not_found",
                "The app installation does not exist in this authenticated scope.",
            );
        },
        Err(error) => return registry_error_response(error),
    };
    let source_state = match export_source_state(installation.lifecycle.status) {
        Some(state) => state,
        None => {
            return api_error(
                StatusCode::CONFLICT,
                "app_package_export_not_available",
                "The installation lifecycle state does not permit package export.",
            );
        },
    };
    let package_revision_ref = installation.package_revision_ref;
    let package_revision = match api
        .registry
        .package_revision(&authenticated, &package_revision_ref, now.to_owned())
        .await
    {
        Ok(Some(revision)) => revision,
        Ok(None) => return corrupt_registry_response("package revision"),
        Err(error) => return registry_error_response(error),
    };
    let dependency_lock = match api
        .registry
        .package_dependency_lock(&authenticated, &package_revision_ref, now.to_owned())
        .await
    {
        Ok(Some(lock)) => lock,
        Ok(None) => return corrupt_registry_response("dependency lock"),
        Err(error) => return registry_error_response(error),
    };
    // Acquire before loading immutable bundle bytes. The permit is moved into
    // the response stream and remains held until the final ZIP chunk is
    // released, so slow downloads cannot multiply full-package memory.
    let transfer_permit = match Arc::clone(&api.transfer_slots).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return api_error(
                StatusCode::TOO_MANY_REQUESTS,
                "app_transfer_overloaded",
                "App package transfer capacity is busy; retry later.",
            );
        },
    };
    let staged = match api
        .stager
        .load_staged_package(
            &authenticated,
            package_revision.content_digest.clone(),
            Utc::now(),
        )
        .await
    {
        Ok(staged) => staged,
        Err(error) => return staging_error_response(error),
    };
    let portable = match build_package_archive_manifest(
        package_revision_ref,
        &package_revision,
        staged.candidate(),
        &dependency_lock,
        source_state,
        Vec::new(),
        &AppContractLimits::default(),
    ) {
        Ok(portable) => portable,
        Err(error) => {
            return api_error(
                StatusCode::CONFLICT,
                "app_package_export_rejected",
                &error.to_string(),
            );
        },
    };
    let encoded = match tokio::task::spawn_blocking(move || {
        encode_package_archive(&portable, staged.candidate())
    })
    .await
    {
        Ok(Ok(encoded)) => encoded,
        Ok(Err(error)) => return transfer_error_response(error),
        Err(error) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_transfer_worker_failed",
                &error.to_string(),
            );
        },
    };
    let content_length = encoded.len();
    let encoded = web::Bytes::from(encoded);
    let stream = futures_util::stream::unfold(
        (encoded, 0usize, transfer_permit),
        |(encoded, offset, permit)| async move {
            if offset >= encoded.len() {
                return None;
            }
            let end = offset.saturating_add(64 * 1_024).min(encoded.len());
            let chunk = encoded.slice(offset..end);
            Some((
                Ok::<web::Bytes, actix_web::Error>(chunk),
                (encoded, end, permit),
            ))
        },
    );
    HttpResponse::Ok()
        .insert_header((
            actix_web::http::header::CONTENT_TYPE,
            APP_PACKAGE_ARCHIVE_MEDIA_TYPE,
        ))
        .insert_header((
            actix_web::http::header::CONTENT_LENGTH,
            content_length.to_string(),
        ))
        .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
        .insert_header((
            actix_web::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}.app.zip\"", installation_id),
        ))
        .streaming(stream)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoringToolsHttpQuery {
    #[serde(default)]
    app_eligible: bool,
    #[serde(default)]
    kind: Option<String>,
}

impl AppPlatformApi {
    fn authoring_roots(&self, authenticated: &AuthenticatedAppScope) -> AuthoringDiscoveryRoots {
        let principal = authenticated.scope().principal.as_str();
        let workspace = authenticated.scope().workspace.as_str();
        AuthoringDiscoveryRoots::for_workspace_scope(&self.workspace, principal, workspace)
    }
}

/// `GET /api/magician/v2/apps/authoring/tools`
async fn list_authoring_tools_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    query: web::Query<AuthoringToolsHttpQuery>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let kind = match query
        .kind
        .as_deref()
        .map(AuthoringToolKind::parse_filter)
        .transpose()
    {
        Ok(kind) => kind,
        Err(error) => return authoring_catalog_error_response(error),
    };
    let list = list_authoring_tools(
        &api.authoring_roots(&authenticated),
        AuthoringToolListFilter {
            app_eligible_only: query.app_eligible,
            kind,
        },
    );
    HttpResponse::Ok()
        .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
        .json(list)
}

/// `GET /api/magician/v2/apps/authoring/tools/{name}`
async fn show_authoring_tool_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match show_authoring_tool(&api.authoring_roots(&authenticated), path.as_str()) {
        Ok(shown) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .json(shown),
        Err(error) => authoring_catalog_error_response(error),
    }
}

/// `GET /api/magician/v2/apps/authoring/agents`
async fn list_authoring_agents_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    HttpResponse::Ok()
        .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
        .json(list_authoring_agents(&api.authoring_roots(&authenticated)))
}

/// `GET /api/magician/v2/apps/authoring/personalities`
async fn list_authoring_personalities_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    HttpResponse::Ok()
        .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
        .json(list_authoring_personalities(
            &api.authoring_roots(&authenticated),
        ))
}

/// `GET /api/magician/v2/apps/authoring/procedures`
async fn list_authoring_procedures_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    HttpResponse::Ok()
        .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
        .json(list_authoring_procedures(
            &api.authoring_roots(&authenticated),
        ))
}

fn authoring_catalog_error_response(error: AuthoringCatalogError) -> HttpResponse {
    match error {
        AuthoringCatalogError::InvalidFilter(reason) => api_error(
            StatusCode::BAD_REQUEST,
            "authoring_catalog_invalid_filter",
            &reason,
        ),
        AuthoringCatalogError::ToolNotFound(name) => api_error(
            StatusCode::NOT_FOUND,
            "authoring_tool_not_found",
            &format!("authoring catalog has no tool named `{name}`"),
        ),
        AuthoringCatalogError::AmbiguousPrimitive(name) => api_error(
            StatusCode::CONFLICT,
            "authoring_primitive_ambiguous",
            &format!("authoring primitive alias `{name}` is ambiguous; use its exact identity"),
        ),
        AuthoringCatalogError::CatalogUnavailable => api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "authoring_catalog_unavailable",
            "authoring primitive catalog is unavailable",
        ),
    }
}

fn macos_pairing_error_response(error: AppMacosPairingControlError) -> HttpResponse {
    match error {
        AppMacosPairingControlError::InvalidRequest => api_error(
            StatusCode::BAD_REQUEST,
            "app_macos_pairing_request_invalid",
            "The macOS pairing control request is invalid.",
        ),
        AppMacosPairingControlError::StoreUnavailable => api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "app_macos_pairing_store_unavailable",
            "The scoped macOS pairing lifecycle is unavailable.",
        ),
        AppMacosPairingControlError::DesktopUnavailable => api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "app_macos_pairing_desktop_unavailable",
            "The exact local macOS pairing owner is unavailable.",
        ),
        AppMacosPairingControlError::InvalidDesktopResponse => api_error(
            StatusCode::BAD_GATEWAY,
            "app_macos_pairing_desktop_response_invalid",
            "The local macOS pairing owner returned an invalid typed response.",
        ),
    }
}

fn app_memory_contribution_projection_error_response(
    error: AppMemoryContributionProjectionError,
) -> HttpResponse {
    match error {
        AppMemoryContributionProjectionError::Destination(error) if error.is_transient() => {
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "app_memory_destination_temporarily_unavailable",
                "The app-memory destination owner is temporarily unavailable; retry the exact request.",
            )
        },
        AppMemoryContributionProjectionError::Destination(_) => api_error(
            StatusCode::CONFLICT,
            "app_memory_owner_decision_conflict",
            "The exact app-memory owner transition was refused because its proposal, destination head, scope, or signed desktop identity is no longer current.",
        ),
        AppMemoryContributionProjectionError::Source(_) => api_error(
            StatusCode::CONFLICT,
            "app_memory_source_journal_conflict",
            "The app-memory source journal is not current.",
        ),
        AppMemoryContributionProjectionError::InvalidDestinationResult(_) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "app_memory_owner_transition_failed",
            "The app-memory destination owner returned an invalid transition result.",
        ),
    }
}

async fn supported_public_contract_capabilities_route(req: HttpRequest) -> HttpResponse {
    trace_supported_public_request(AppPublicOperationId::ContractCapabilities);
    canonicalize_supported_public_response(
        AppPublicOperationId::ContractCapabilities,
        app_contract_capabilities_handler(req).await,
    )
    .await
}

async fn supported_public_query_data_route(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    payload: web::Payload,
) -> HttpResponse {
    trace_supported_public_request(AppPublicOperationId::QueryData);
    canonicalize_supported_public_response(
        AppPublicOperationId::QueryData,
        query_app_data_handler(api, req, path, payload).await,
    )
    .await
}

async fn supported_public_mutate_data_route(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    payload: web::Payload,
) -> HttpResponse {
    trace_supported_public_request(AppPublicOperationId::MutateData);
    canonicalize_supported_public_response(
        AppPublicOperationId::MutateData,
        mutate_app_data_handler(api, req, path, payload).await,
    )
    .await
}

async fn supported_public_launch_action_route(
    api: web::Data<AppPlatformApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    payload: web::Payload,
) -> HttpResponse {
    trace_supported_public_request(AppPublicOperationId::LaunchAction);
    canonicalize_supported_public_response(
        AppPublicOperationId::LaunchAction,
        launch_public_app_action_handler(api, resources, req, path, payload).await,
    )
    .await
}

async fn supported_public_get_action_run_route(
    api: web::Data<AppPlatformApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    trace_supported_public_request(AppPublicOperationId::GetActionRun);
    canonicalize_supported_public_response(
        AppPublicOperationId::GetActionRun,
        get_app_action_run_handler(api, resources, req, path).await,
    )
    .await
}

async fn supported_public_compose_action_run_route(
    api: web::Data<AppPlatformApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    path: web::Path<String>,
    payload: web::Payload,
) -> HttpResponse {
    trace_supported_public_request(AppPublicOperationId::ComposeActionRun);
    canonicalize_supported_public_response(
        AppPublicOperationId::ComposeActionRun,
        compose_app_action_run_handler(api, resources, req, path, payload).await,
    )
    .await
}

async fn supported_public_cancel_action_run_route(
    api: web::Data<AppPlatformApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    path: web::Path<String>,
    payload: web::Payload,
) -> HttpResponse {
    trace_supported_public_request(AppPublicOperationId::CancelActionRun);
    canonicalize_supported_public_response(
        AppPublicOperationId::CancelActionRun,
        cancel_app_action_run_handler(api, resources, req, path, payload).await,
    )
    .await
}

async fn supported_public_read_entity_changes_route(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: Result<web::Query<AppEntityChangesQuery>, actix_web::Error>,
) -> HttpResponse {
    trace_supported_public_request(AppPublicOperationId::ReadEntityChanges);
    canonicalize_supported_public_response(
        AppPublicOperationId::ReadEntityChanges,
        app_entity_changes_handler(api, req, path, query).await,
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppIndicatorListQuery {
    limit: Option<usize>,
}

/// `POST /api/magician/v2/apps/widgets/render-batch`
///
/// The body names declarations and client capabilities only. Query contracts
/// stay in the host-registered compiled plan and cannot be supplied by HTTP.
async fn render_app_widget_batch_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    mut payload: web::Payload,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let bytes = match read_bounded_raw_body(
        &req,
        &mut payload,
        "application/json",
        APP_WIDGET_RENDER_MAX_REQUEST_BYTES,
        "app_widget_render_body",
    )
    .await
    {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };
    let request: AppWidgetRenderBatchRequest = match serde_json::from_slice(&bytes) {
        Ok(request) => request,
        Err(_) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_app_widget_render_request",
                "The widget render request is invalid.",
            )
        },
    };
    let if_none_match = req
        .headers()
        .get(actix_web::http::header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok());
    match api
        .widget_runtime
        .render_batch(&authenticated, request, if_none_match, now)
        .await
    {
        Ok(AppWidgetRenderBatchOutcome::Modified(response)) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .insert_header((
                actix_web::http::header::ETAG,
                format!("\"{}\"", response.etag),
            ))
            .insert_header((
                "X-App-Widget-Refresh-After",
                response.refresh_after.to_rfc3339(),
            ))
            .json(response),
        Ok(AppWidgetRenderBatchOutcome::NotModified {
            etag,
            refresh_after,
        }) => HttpResponse::NotModified()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .insert_header((actix_web::http::header::ETAG, format!("\"{etag}\"")))
            .insert_header(("X-App-Widget-Refresh-After", refresh_after.to_rfc3339()))
            .finish(),
        Err(error) => widget_runtime_error_response(error),
    }
}

/// `GET /api/magician/v2/apps/indicators`
///
/// This path reads the bounded materialization only; evaluation and app-store
/// reads are owned by the due-work worker seam.
async fn list_app_indicators_handler(
    api: web::Data<AppPlatformApi>,
    req: HttpRequest,
    query: Result<web::Query<AppIndicatorListQuery>, actix_web::Error>,
) -> HttpResponse {
    let now = Utc::now();
    let authenticated = match authenticated_app_scope(&req, &now) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let query = match query {
        Ok(query) => query,
        Err(_) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_app_indicator_query",
                "The indicator query is invalid.",
            )
        },
    };
    let limit = query
        .limit
        .unwrap_or(APP_INDICATOR_MAX_PAGE_ITEMS)
        .clamp(1, APP_INDICATOR_MAX_PAGE_ITEMS);
    let if_none_match = req
        .headers()
        .get(actix_web::http::header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok());
    match api
        .widget_runtime
        .indicators(&authenticated, limit, if_none_match, now)
    {
        Ok(AppIndicatorListOutcome::Modified(response)) => HttpResponse::Ok()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .insert_header((
                actix_web::http::header::ETAG,
                format!("\"{}\"", response.etag),
            ))
            .json(response),
        Ok(AppIndicatorListOutcome::NotModified { etag }) => HttpResponse::NotModified()
            .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
            .insert_header((actix_web::http::header::ETAG, format!("\"{etag}\"")))
            .finish(),
        Err(error) => widget_runtime_error_response(error),
    }
}

pub fn configure_app_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/apps")
            .configure(cleanup::configure)
            .route(
                supported_public_route_path(
                    AppPublicOperationId::ContractCapabilities,
                    AppHttpMethod::Get,
                ),
                web::get().to(supported_public_contract_capabilities_route),
            )
            .route("/directory", web::get().to(app_directory_handler))
            .route(
                "/background-behaviors",
                web::get().to(get_app_background_behaviors_handler),
            )
            .route(
                "/background-behaviors/policy",
                web::put().to(put_app_background_behavior_policy_handler),
            )
            .route(
                "/background-behaviors/{installation_id}/{behavior_id}/retry",
                web::post().to(retry_app_background_behavior_handler),
            )
            .route(
                "/widgets/render-batch",
                web::post().to(render_app_widget_batch_handler),
            )
            .route("/indicators", web::get().to(list_app_indicators_handler))
            .route(
                "/slots/resolve-batch",
                web::post().to(resolve_app_slots_batch_handler),
            )
            .route("/slots/{slot_id}", web::get().to(resolve_app_slot_handler))
            .route(
                "/slot-assignments",
                web::get().to(list_app_slot_assignments_handler),
            )
            .route(
                "/slot-assignments",
                web::post().to(mutate_app_slot_assignment_handler),
            )
            .route(
                "/macos-pairing",
                web::get().to(get_app_macos_pairing_handler),
            )
            .route(
                "/macos-pairing/setup",
                web::post().to(begin_app_macos_pairing_handler),
            )
            .route(
                "/macos-pairing/advance",
                web::post().to(advance_app_macos_pairing_handler),
            )
            .route(
                "/macos-pairing/revoke",
                web::post().to(revoke_app_macos_pairing_handler),
            )
            .route(
                "/macos-pairing/reset/challenge",
                web::post().to(begin_app_macos_pairing_reset_handler),
            )
            .route(
                "/macos-pairing/reset",
                web::post().to(complete_app_macos_pairing_reset_handler),
            )
            .route(
                "/memory-contributions/owner-reviews",
                web::get().to(list_app_memory_owner_reviews_handler),
            )
            .route(
                "/memory-contributions/state",
                web::get().to(list_app_memory_contribution_state_handler),
            )
            .route(
                "/memory-contributions/owner-decisions",
                web::post().to(apply_app_memory_owner_decision_handler),
            )
            .route(
                "/installations/{installation_id}/claims-decisions/owner-decisions",
                web::post().to(apply_app_claims_owner_decision_handler),
            )
            .route(
                "/installations/{installation_id}/meeting-controls/owner-decisions",
                web::post().to(apply_app_meeting_control_owner_decision_handler),
            )
            .route(
                "/installations/{installation_id}/claims-ingests/apply",
                web::post().to(apply_app_staged_ingest_handler),
            )
            .route(
                "/authoring/tools",
                web::get().to(list_authoring_tools_handler),
            )
            .route(
                "/authoring/tools/{name}",
                web::get().to(show_authoring_tool_handler),
            )
            .route(
                "/authoring/agents",
                web::get().to(list_authoring_agents_handler),
            )
            .route(
                "/authoring/personalities",
                web::get().to(list_authoring_personalities_handler),
            )
            .route(
                "/authoring/procedures",
                web::get().to(list_authoring_procedures_handler),
            )
            .route(
                "/packages/import",
                web::post().to(import_app_package_handler),
            )
            .route(
                "/packages/candidates",
                web::post().to(publish_app_candidate_handler),
            )
            .route(
                "/procedure-revisions",
                web::post().to(publish_app_procedure_revision_handler),
            )
            .route(
                "/capability-revisions",
                web::post().to(publish_app_capability_revision_handler),
            )
            .route(
                "/installations/{installation_id}",
                web::get().to(get_app_installation_handler),
            )
            .route(
                "/installations/{installation_id}/review",
                web::get().to(get_app_installation_review_handler),
            )
            .route(
                "/installations/{installation_id}/portable-exports",
                web::post().to(export_app_portable_archive_handler),
            )
            .route(
                "/installations/{installation_id}/data-imports/preview",
                web::post().to(preview_app_data_import_handler),
            )
            .route(
                "/installations/{installation_id}/data-imports/approve",
                web::post().to(approve_app_data_import_handler),
            )
            .route(
                "/installations/{installation_id}/data-imports/commit",
                web::post().to(commit_app_data_import_handler),
            )
            .route(
                "/installations/{installation_id}/approve",
                web::post().to(approve_app_installation_handler),
            )
            .route(
                "/installations/{installation_id}/update-plans",
                web::post().to(prepare_app_update_plan_handler),
            )
            .route(
                "/updates/{migration_run_id}",
                web::get().to(get_app_update_plan_handler),
            )
            .route(
                "/updates/{migration_run_id}/backup",
                web::post().to(backup_app_update_handler),
            )
            .route(
                "/installations/{installation_id}/rollbacks/code",
                web::post().to(rollback_app_update_code_handler),
            )
            .route(
                "/updates/{migration_run_id}/rewind-preview",
                web::post().to(preview_app_update_data_rewind_handler),
            )
            .route(
                "/updates/{migration_run_id}/rewind-commit",
                web::post().to(commit_app_update_data_rewind_handler),
            )
            .route(
                "/installations/{installation_id}/disable",
                web::post().to(disable_app_installation_handler),
            )
            .route(
                "/installations/{installation_id}/quarantine",
                web::post().to(quarantine_app_installation_handler),
            )
            .route(
                "/installations/{installation_id}/uninstall",
                web::post().to(uninstall_app_installation_handler),
            )
            .route(
                "/installations/{installation_id}/reenable-review",
                web::get().to(get_app_installation_reenable_review_handler),
            )
            .route(
                "/installations/{installation_id}/reenable",
                web::post().to(reenable_app_installation_handler),
            )
            .route(
                "/installations/{installation_id}/purge-preview",
                web::post().to(preview_app_installation_purge_handler),
            )
            .route(
                "/installations/{installation_id}/purge",
                web::post().to(commit_app_installation_purge_handler),
            )
            .route(
                "/purges/{idempotency_key}",
                web::get().to(get_app_installation_purge_status_handler),
            )
            .route(
                "/installations/{installation_id}/grant-revocations",
                web::post().to(revoke_app_installation_grant_handler),
            )
            .route(
                "/installations/{installation_id}/memory-access",
                web::get().to(get_app_installation_memory_access_handler),
            )
            .route(
                "/installations/{installation_id}/memory-access",
                web::post().to(update_app_installation_memory_access_handler),
            )
            .route(
                "/installations/{installation_id}/update-begin",
                web::post().to(begin_app_installation_update_handler),
            )
            .route(
                "/installations/{installation_id}/update-abort",
                web::post().to(abort_app_installation_update_handler),
            )
            .route(
                "/installations/{installation_id}/package-export",
                web::get().to(export_app_package_handler),
            )
            .route(
                supported_public_route_path(AppPublicOperationId::QueryData, AppHttpMethod::Post),
                web::post().to(supported_public_query_data_route),
            )
            .route(
                supported_public_route_path(AppPublicOperationId::MutateData, AppHttpMethod::Post),
                web::post().to(supported_public_mutate_data_route),
            )
            .route(
                "/installations/{installation_id}/actions/{action_id}/invocations",
                web::post().to(invoke_app_action_handler),
            )
            .route(
                "/installations/{installation_id}/actions/{action_id}/contract",
                web::get().to(get_direct_app_action_contract_handler),
            )
            .route(
                "/installations/{installation_id}/actions/{action_id}/launch",
                web::post().to(launch_direct_app_action_handler),
            )
            .route(
                supported_public_route_path(
                    AppPublicOperationId::LaunchAction,
                    AppHttpMethod::Post,
                ),
                web::post().to(supported_public_launch_action_route),
            )
            .route(
                supported_public_route_path(AppPublicOperationId::GetActionRun, AppHttpMethod::Get),
                web::get().to(supported_public_get_action_run_route),
            )
            .route(
                supported_public_route_path(
                    AppPublicOperationId::ComposeActionRun,
                    AppHttpMethod::Post,
                ),
                web::post().to(supported_public_compose_action_run_route),
            )
            .route(
                supported_public_route_path(
                    AppPublicOperationId::CancelActionRun,
                    AppHttpMethod::Post,
                ),
                web::post().to(supported_public_cancel_action_run_route),
            )
            .route(
                "/maintenance/action-runs/{run_ref}",
                web::delete().to(delete_legacy_scheduled_app_task_handler),
            )
            .route(
                "/action-runs/{run_ref}/interactive-state",
                web::get().to(get_app_action_interactive_state_handler),
            )
            .route(
                "/action-runs/{run_ref}/interactive-stop",
                web::post().to(stop_app_action_interactive_session_handler),
            )
            .route(
                "/memory-candidates",
                web::get().to(list_app_memory_candidates_handler),
            )
            .route(
                "/memory-candidates/{candidate_id}",
                web::get().to(get_app_memory_candidate_handler),
            )
            .route(
                "/memory-candidates/{candidate_id}/{command}",
                web::post().to(transition_app_memory_candidate_handler),
            )
            .route(
                "/installations/{installation_id}/surface-mutations",
                web::post().to(mutate_app_surface_handler),
            )
            .route(
                supported_public_route_path(
                    AppPublicOperationId::ReadEntityChanges,
                    AppHttpMethod::Get,
                ),
                web::get().to(supported_public_read_entity_changes_route),
            )
            .route(
                "/installations/{installation_id}/directory-activity",
                web::post().to(app_directory_activity_handler),
            )
            .route(
                "/installations/{installation_id}/surfaces",
                web::get().to(hydrate_app_surface_handler),
            )
            .route(
                "/installations/{installation_id}/surfaces/{surface_tail:.*}",
                web::get().to(hydrate_app_surface_handler),
            )
            .route(
                "/attempts/{attempt_id}",
                web::get().to(get_app_lifecycle_attempt_handler),
            )
            .route(
                "/installations/{installation_id}/custom-surface",
                web::get().to(get_custom_surface_host_handler),
            )
            .route(
                "/installations/{installation_id}/custom-surface/assets/{asset_tail:.*}",
                web::get().to(get_custom_surface_asset_handler),
            )
            .route(
                "/installations/{installation_id}/custom-surface/bridge",
                web::post().to(post_custom_surface_bridge_handler),
            )
            .route(
                "/installations/{installation_id}/custom-surface-v1/host",
                web::get().to(get_scripted_surface_host_handler),
            )
            .route(
                "/installations/{installation_id}/custom-surface-v1/assets/{asset_tail:.*}",
                web::get().to(get_scripted_surface_asset_handler),
            )
            .route(
                "/installations/{installation_id}/custom-surface-v1/bridge",
                web::post().to(post_scripted_surface_bridge_handler),
            )
            .route(
                "/installations/{installation_id}/custom-surface-v1/sessions/{session_ref}/reload-note",
                web::post().to(post_scripted_surface_reload_note_handler),
            ),
    );
}

fn supported_public_route_path(
    id: AppPublicOperationId,
    expected_method: AppHttpMethod,
) -> &'static str {
    let operation = supported_public_operation(id);
    assert_eq!(
        operation.method, expected_method,
        "supported-public route method differs from its canonical operation inventory"
    );
    operation.path
}

/// `GET /api/magician/v2/apps/contract-capabilities`
///
/// This negotiates public compatibility only. It needs a verified session but
/// intentionally does not select a principal/workspace, enumerate installed
/// primitives, or project any authority-bearing evidence.
pub async fn app_contract_capabilities_handler(req: HttpRequest) -> HttpResponse {
    let now = Utc::now();
    let identity = match req.extensions().get::<VerifiedRequestIdentity>().cloned() {
        Some(identity) => identity,
        None => {
            return api_error(
                StatusCode::UNAUTHORIZED,
                "authenticated_app_session_required",
                "A verified browser, paired-device, or local loopback session is required.",
            );
        },
    };
    let verified_at = identity.verified_at();
    if now < verified_at || now - verified_at > ChronoDuration::seconds(5) {
        return api_error(
            StatusCode::UNAUTHORIZED,
            "stale_authenticated_request",
            "The authenticated request identity is stale.",
        );
    }
    let limits = AppContractLimits::default();
    HttpResponse::Ok()
        .insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"))
        .json(AppContractCapabilities::current(
            AppContractCapabilityLimits {
                max_document_bytes: limits.max_document_bytes() as u64,
                max_json_depth: limits.max_json_depth() as u64,
                max_json_nodes: limits.max_json_nodes() as u64,
                max_value_bytes: limits.max_value_bytes() as u64,
                max_value_nodes: limits.max_value_nodes() as u64,
                max_collection_items: limits.max_collection_items() as u64,
                max_predicate_nodes: limits.max_predicate_nodes() as u64,
                max_predicate_depth: limits.max_predicate_depth() as u64,
                max_page_rows: limits.max_page_rows() as u64,
                max_entity_change_page_rows: MAX_APP_ENTITY_CHANGE_LIMIT as u64,
            },
        ))
}

fn workflow_error_response(error: AppWorkflowError) -> HttpResponse {
    let (status, code) = match &error {
        AppWorkflowError::MissingInstallation
        | AppWorkflowError::MissingPackageRevision
        | AppWorkflowError::MissingAction
        | AppWorkflowError::MissingWorkflow
        | AppWorkflowError::MissingWorkflowPrompt
        | AppWorkflowError::MissingPackageLock
        | AppWorkflowError::NotWorkflowTask => (StatusCode::NOT_FOUND, "app_workflow_not_found"),
        AppWorkflowError::StaleInvocation(_)
        | AppWorkflowError::TaskBindingConflict
        | AppWorkflowError::CommitIntentConflict
        | AppWorkflowError::StaleRuntimeAuthority
        | AppWorkflowError::StaleWorkflowPersonality => {
            (StatusCode::CONFLICT, "app_workflow_stale")
        },
        AppWorkflowError::InteractiveSessionUnavailable => {
            (StatusCode::CONFLICT, "app_interactive_session_unavailable")
        },
        AppWorkflowError::InteractiveSessionTerminal => {
            (StatusCode::CONFLICT, "app_interactive_session_terminal")
        },
        AppWorkflowError::InteractiveStopConflict
        | AppWorkflowError::ActionCancellationConflict
        | AppWorkflowError::Registry(AppRegistryError::CompareAndSwapLost(
            "protected workflow control head",
        ))
        | AppWorkflowError::Registry(AppRegistryError::CompareAndSwapLost(
            "protected workflow run-state head",
        )) => (StatusCode::CONFLICT, "app_interactive_stop_conflict"),
        AppWorkflowError::InteractiveStopRequestInvalid => {
            (StatusCode::BAD_REQUEST, "invalid_app_interactive_stop")
        },
        AppWorkflowError::ActionCancellationRequested => {
            (StatusCode::CONFLICT, "app_action_cancelling")
        },
        AppWorkflowError::InstallationUnavailable
        | AppWorkflowError::RunnerUnavailable
        | AppWorkflowError::RunnerMissingDeclaredTool
        | AppWorkflowError::GrantMissingDeclaredTool
        | AppWorkflowError::BackgroundExecutionDenied
        | AppWorkflowError::BackgroundLaunchAuthorityRequired
        | AppWorkflowError::BackgroundLaunchAuthorityMismatch
        | AppWorkflowError::BackgroundLaunchAuthorityExpired
        | AppWorkflowError::BackgroundBehaviorOperationStepBindingRequired
        | AppWorkflowError::ToolDenied(_)
        | AppWorkflowError::ReadOnlyMutation
        | AppWorkflowError::MutationTargetDenied(_) => {
            (StatusCode::FORBIDDEN, "app_workflow_not_authorized")
        },
        AppWorkflowError::Contract(_)
        | AppWorkflowError::Manifest(_)
        | AppWorkflowError::UnsupportedInputSource
        | AppWorkflowError::InvalidWorkflowPrompt
        | AppWorkflowError::TerminalPayloadTooLarge
        | AppWorkflowError::ReadOnlyOutputRequired
        | AppWorkflowError::MutationRequired
        | AppWorkflowError::EmptyMutationPreconditions
        | AppWorkflowError::InvalidSourceArtifact(_)
        | AppWorkflowError::UnknownSourceArtifact => {
            (StatusCode::BAD_REQUEST, "app_workflow_invalid")
        },
        AppWorkflowError::ProjectionHandleTooLarge => {
            (StatusCode::PAYLOAD_TOO_LARGE, "app_projection_too_large")
        },
        AppWorkflowError::ProjectionHandleExpired => (StatusCode::GONE, "app_projection_expired"),
        AppWorkflowError::ProjectionHandleScopeMismatch => {
            (StatusCode::FORBIDDEN, "app_projection_not_authorized")
        },
        AppWorkflowError::ProjectionHandleStoreUnavailable
        | AppWorkflowError::ProjectionHandleStoreOverflow => (
            StatusCode::SERVICE_UNAVAILABLE,
            "app_projection_store_unavailable",
        ),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "app_workflow_failed"),
    };
    if status == StatusCode::INTERNAL_SERVER_ERROR {
        tracing::error!(
            error_code = code,
            error = %error,
            "app workflow request failed inside the governed runtime"
        );
        api_error(
            status,
            code,
            "The app workflow could not be completed safely.",
        )
    } else {
        api_error(status, code, &error.to_string())
    }
}

fn app_composition_error_response(error: AppComposeInvokeError) -> HttpResponse {
    match error {
        AppComposeInvokeError::Workflow(error) => workflow_error_response(error),
        AppComposeInvokeError::Contract(_)
        | AppComposeInvokeError::Mapping(_)
        | AppComposeInvokeError::Composition(_)
        | AppComposeInvokeError::EmptySourceResult
        | AppComposeInvokeError::MissingSourceField(_)
        | AppComposeInvokeError::SourceShapeConflict(_)
        | AppComposeInvokeError::UnsupportedSourceRelations => api_error(
            StatusCode::BAD_REQUEST,
            "invalid_app_action_composition",
            "The action composition contract is invalid.",
        ),
        AppComposeInvokeError::Boundary(_) => api_error(
            StatusCode::FORBIDDEN,
            "app_action_composition_not_authorized",
            "The action composition is not authorized in this scope.",
        ),
        AppComposeInvokeError::MissingSourceRecord
        | AppComposeInvokeError::AmbiguousSourceRecord
        | AppComposeInvokeError::SourceRecordRequired
        | AppComposeInvokeError::MissingSourceProvenance
        | AppComposeInvokeError::AmbiguousSourceProvenance
        | AppComposeInvokeError::MissingSourceContract
        | AppComposeInvokeError::MissingDestinationInstallation
        | AppComposeInvokeError::DestinationUnavailable
        | AppComposeInvokeError::DestinationChanged
        | AppComposeInvokeError::MissingDestinationPackage
        | AppComposeInvokeError::MissingDestinationAction
        | AppComposeInvokeError::MissingDestinationWorkflow => api_error(
            StatusCode::CONFLICT,
            "app_action_composition_unavailable",
            "The action composition cannot be completed from the current reviewed state.",
        ),
        _ => {
            tracing::error!(
                error_code = "app_action_composition_failed",
                "supported-public app composition failed"
            );
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_action_composition_failed",
                "The action composition could not be completed safely.",
            )
        },
    }
}

/// The one app-scope refusal a caller may recover from: the verified
/// identity carries no workspace binding and the request asserted none
/// (`app_workspace_required`) — the hosted-web Cloudflare Access
/// interactive-identity shape. Every other refusal (unauthenticated,
/// stale, mismatched) stands exactly as answered.
enum AppScopeAuthFailure {
    WorkspaceRequired,
    Rejected(HttpResponse),
}

fn app_workspace_required_response() -> HttpResponse {
    api_error(
        StatusCode::BAD_REQUEST,
        "app_workspace_required",
        "Select an explicit workspace for this authenticated owner.",
    )
}

/// Typed core of [`authenticated_app_scope`]: the same extraction, the
/// same refusals, except the workspace-missing arm is distinguishable so
/// the scripted-surface asset route can recover from exactly that one
/// failure (through a live session's bound scope) and nothing else.
fn authenticate_app_scope(
    req: &HttpRequest,
    now: &chrono::DateTime<Utc>,
) -> Result<AuthenticatedAppScope, AppScopeAuthFailure> {
    let identity = req
        .extensions()
        .get::<VerifiedRequestIdentity>()
        .cloned()
        .ok_or_else(|| {
            AppScopeAuthFailure::Rejected(api_error(
                StatusCode::UNAUTHORIZED,
                "authenticated_app_session_required",
                "A verified browser, paired-device, or local loopback session is required.",
            ))
        })?;
    let asserted_principal = header_value(req, "X-Principal");
    if asserted_principal
        .as_deref()
        .is_some_and(|principal| principal != identity.principal())
    {
        return Err(AppScopeAuthFailure::Rejected(api_error(
            StatusCode::FORBIDDEN,
            "app_scope_mismatch",
            "The requested app scope does not match the authenticated identity.",
        )));
    }
    let asserted_workspace = header_value(req, "X-Workspace");
    let workspace = match identity.workspace() {
        Some(bound) => {
            if asserted_workspace
                .as_deref()
                .is_some_and(|asserted| asserted != bound)
            {
                return Err(AppScopeAuthFailure::Rejected(api_error(
                    StatusCode::FORBIDDEN,
                    "app_scope_mismatch",
                    "The requested app scope does not match the authenticated identity.",
                )));
            }
            bound.to_owned()
        },
        None => asserted_workspace.ok_or(AppScopeAuthFailure::WorkspaceRequired)?,
    };
    bind_verified_app_scope(req, &identity, workspace, now).map_err(AppScopeAuthFailure::Rejected)
}

pub(crate) fn authenticated_app_scope(
    req: &HttpRequest,
    now: &chrono::DateTime<Utc>,
) -> Result<AuthenticatedAppScope, HttpResponse> {
    authenticate_app_scope(req, now).map_err(|failure| match failure {
        AppScopeAuthFailure::WorkspaceRequired => app_workspace_required_response(),
        AppScopeAuthFailure::Rejected(response) => response,
    })
}

/// Mint the bound transport session for an already-verified identity and
/// a resolved workspace. Shared by the header-driven path above and the
/// session-bound hosted-web path below, so the freshness window, the
/// transport class, and the request binding are the SAME kernel code,
/// not a copy.
fn bind_verified_app_scope(
    req: &HttpRequest,
    identity: &VerifiedRequestIdentity,
    workspace: String,
    now: &chrono::DateTime<Utc>,
) -> Result<AuthenticatedAppScope, HttpResponse> {
    let scope = AppScope {
        principal: AppReference::parse(identity.principal().to_owned())
            .map_err(contract_error_response)?,
        workspace: AppReference::parse(workspace).map_err(contract_error_response)?,
    };
    let scope_binding_ref = scope_binding_ref(&scope).map_err(contract_error_response)?;
    let actor_ref = AppReference::parse(format!("actor:{}", identity.actor_fingerprint()))
        .map_err(contract_error_response)?;
    let session_ref = AppReference::parse(format!("session:{}", identity.session_fingerprint()))
        .map_err(contract_error_response)?;
    let authentication_revision =
        AppRevision::new(identity.authentication_revision()).map_err(|_| {
            api_error(
                StatusCode::UNAUTHORIZED,
                "invalid_authentication_revision",
                "The authenticated session revision is invalid.",
            )
        })?;
    let issued_at = identity.verified_at();
    if now < &issued_at || now.to_owned() - issued_at.to_owned() > ChronoDuration::seconds(5) {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "stale_authenticated_request",
            "The authenticated request identity is stale.",
        ));
    }
    let expires_at = now.to_owned() + ChronoDuration::seconds(APP_ROUTE_AUTHORITY_LIFETIME_SECONDS);
    let transport = match identity.authentication() {
        VerifiedRequestAuthentication::TrustedLoopbackSingleUser => {
            let peer_ip = req.peer_addr().map(|address| address.ip()).ok_or_else(|| {
                api_error(
                    StatusCode::UNAUTHORIZED,
                    "loopback_peer_required",
                    "The local app session has no verified loopback peer.",
                )
            })?;
            VerifiedAppTransportSession::from_trusted_loopback(
                peer_ip,
                true,
                scope.clone(),
                scope_binding_ref,
                actor_ref,
                session_ref,
                authentication_revision,
                issued_at,
                expires_at,
            )
        },
        VerifiedRequestAuthentication::CloudflareAccess
        | VerifiedRequestAuthentication::PairedDevice
        | VerifiedRequestAuthentication::MagicianBearer => {
            VerifiedAppTransportSession::from_verified_session(
                scope.clone(),
                scope_binding_ref,
                actor_ref,
                session_ref,
                authentication_revision,
                issued_at,
                expires_at,
            )
        },
    }
    .map_err(boundary_error_response)?;
    transport
        .bind_request(Some(&scope), now)
        .map_err(boundary_error_response)
}

/// Construct the serving scope from a live scripted session's bound
/// requesting scope instead of the header envelope (the hosted-web
/// Cloudflare Access posture). The principal still comes from the
/// VERIFIED identity and must be the principal the session was minted
/// under — a session reference presented by a different owner refuses —
/// and a workspace header, if the request carries one at all, must agree
/// with the bound workspace. Nothing here mints authority the host route
/// did not already authenticate at host-open; the bridge POST never uses
/// this path.
fn session_bound_app_scope(
    req: &HttpRequest,
    now: &chrono::DateTime<Utc>,
    bound: &AppScriptedSurfaceRequestScope,
) -> Result<AuthenticatedAppScope, HttpResponse> {
    let identity = req
        .extensions()
        .get::<VerifiedRequestIdentity>()
        .cloned()
        .ok_or_else(|| {
            api_error(
                StatusCode::UNAUTHORIZED,
                "authenticated_app_session_required",
                "A verified browser, paired-device, or local loopback session is required.",
            )
        })?;
    if header_value(req, "X-Principal")
        .as_deref()
        .is_some_and(|principal| principal != identity.principal())
        || identity.principal() != bound.principal
    {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "app_scope_mismatch",
            "The requested app scope does not match the authenticated identity.",
        ));
    }
    if header_value(req, "X-Workspace")
        .as_deref()
        .is_some_and(|workspace| workspace != bound.workspace)
    {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "app_scope_mismatch",
            "The requested app scope does not match the authenticated identity.",
        ));
    }
    bind_verified_app_scope(req, &identity, bound.workspace.clone(), now)
}

fn scope_binding_ref(scope: &AppScope) -> Result<AppScopeBindingRef, AppContractError> {
    let digest = AppDigest::blake3(format!("{}\0{}", scope.principal, scope.workspace).as_bytes());
    AppScopeBindingRef::parse(format!(
        "scope_{}",
        digest.as_str().trim_start_matches("blake3:")
    ))
}

fn header_value(req: &HttpRequest, name: &str) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn content_type_matches(req: &HttpRequest, expected: &str) -> bool {
    req.headers()
        .get(actix_web::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}

async fn read_bounded_raw_body(
    req: &HttpRequest,
    payload: &mut web::Payload,
    media_type: &str,
    maximum: usize,
    error_prefix: &str,
) -> Result<Vec<u8>, HttpResponse> {
    if !content_type_matches(req, media_type) {
        return Err(api_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            &format!("unsupported_{error_prefix}_media_type"),
            &format!("Expected {media_type}."),
        ));
    }
    if req
        .headers()
        .get(actix_web::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > maximum)
    {
        return Err(api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            &format!("{error_prefix}_too_large"),
            "The request body exceeds its fixed byte limit.",
        ));
    }
    tokio::time::timeout(APP_DATA_BODY_TIMEOUT, async {
        let mut bytes = Vec::new();
        while let Some(chunk) = payload.next().await {
            let chunk = chunk.map_err(|_| {
                api_error(
                    StatusCode::BAD_REQUEST,
                    &format!("{error_prefix}_read_failed"),
                    "The request body could not be read.",
                )
            })?;
            let next = bytes.len().checked_add(chunk.len()).ok_or_else(|| {
                api_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    &format!("{error_prefix}_too_large"),
                    "The request body exceeds its fixed byte limit.",
                )
            })?;
            if next > maximum {
                return Err(api_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    &format!("{error_prefix}_too_large"),
                    "The request body exceeds its fixed byte limit.",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok::<Vec<u8>, HttpResponse>(bytes)
    })
    .await
    .map_err(|_| {
        api_error(
            StatusCode::REQUEST_TIMEOUT,
            &format!("{error_prefix}_timeout"),
            "The request body exceeded its fixed read deadline.",
        )
    })?
}

async fn read_bounded_app_contract<T>(
    req: &HttpRequest,
    payload: &mut web::Payload,
) -> Result<T, HttpResponse>
where
    T: DeserializeOwned + ValidateAppContract,
{
    if !content_type_matches(req, "application/json") {
        return Err(api_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_app_data_media_type",
            "Expected application/json.",
        ));
    }
    let limits = AppContractLimits::default();
    let maximum = limits.max_document_bytes();
    if req
        .headers()
        .get(actix_web::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > maximum)
    {
        return Err(api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "app_data_body_too_large",
            "The app data contract exceeds its fixed document limit.",
        ));
    }
    let bytes = tokio::time::timeout(APP_DATA_BODY_TIMEOUT, async {
        let mut bytes = Vec::new();
        while let Some(chunk) = payload.next().await {
            let chunk = chunk.map_err(|_| {
                api_error(
                    StatusCode::BAD_REQUEST,
                    "app_data_body_read_failed",
                    "The app data request body could not be read.",
                )
            })?;
            let next = bytes.len().checked_add(chunk.len()).ok_or_else(|| {
                api_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "app_data_body_too_large",
                    "The app data contract exceeds its fixed document limit.",
                )
            })?;
            if next > maximum {
                return Err(api_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "app_data_body_too_large",
                    "The app data contract exceeds its fixed document limit.",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok::<Vec<u8>, HttpResponse>(bytes)
    })
    .await
    .map_err(|_| {
        api_error(
            StatusCode::REQUEST_TIMEOUT,
            "app_data_body_timeout",
            "The app data request body exceeded its fixed read deadline.",
        )
    })??;
    decode_app_contract(&bytes, &limits).map_err(contract_error_response)
}

async fn read_optional_approve_request(
    req: &HttpRequest,
    payload: &mut web::Payload,
) -> Result<AppInstallationApproveRequest, HttpResponse> {
    // A Content-Length header that is present and explicitly zero means no
    // body was sent. Its *absence* does not mean no body — a chunked
    // (Transfer-Encoding) request has no Content-Length at all but can still
    // carry a real JSON payload that must not be silently ignored and
    // replaced with the "grant everything" default. So only short-circuit on
    // an explicit zero; otherwise fall through and actually read the
    // payload, and let an empty read (not an absent header) decide.
    let content_length_is_explicit_zero = req
        .headers()
        .get(actix_web::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        == Some(0);
    if content_length_is_explicit_zero {
        return Ok(AppInstallationApproveRequest::default());
    }
    let bytes = read_bounded_raw_body(
        req,
        payload,
        "application/json",
        AppContractLimits::default().max_document_bytes(),
        "app_approve_body",
    )
    .await?;
    if bytes.is_empty() {
        return Ok(AppInstallationApproveRequest::default());
    }
    serde_json::from_slice(&bytes).map_err(|error| {
        api_error(
            StatusCode::BAD_REQUEST,
            "invalid_app_approve_request",
            &format!("The approval request is invalid: {error}"),
        )
    })
}

fn export_source_state(status: AppInstallationStatus) -> Option<AppExportSourceState> {
    match status {
        AppInstallationStatus::Enabled => Some(AppExportSourceState::Enabled),
        AppInstallationStatus::Disabled => Some(AppExportSourceState::Disabled),
        AppInstallationStatus::Quarantined => Some(AppExportSourceState::Quarantined),
        AppInstallationStatus::UninstalledRetained => {
            Some(AppExportSourceState::UninstalledRetained)
        },
        AppInstallationStatus::ReadyForReview
        | AppInstallationStatus::UpdatePending
        | AppInstallationStatus::Purged => None,
    }
}

fn boundary_error_response(error: AppBoundaryError) -> HttpResponse {
    api_error(
        StatusCode::FORBIDDEN,
        "app_authority_rejected",
        &error.to_string(),
    )
}

fn widget_runtime_error_response(error: AppWidgetRuntimeError) -> HttpResponse {
    match error {
        AppWidgetRuntimeError::InvalidRenderRequest
        | AppWidgetRuntimeError::DuplicateDeclaration
        | AppWidgetRuntimeError::UnsupportedSchemaVersion => api_error(
            StatusCode::BAD_REQUEST,
            "invalid_app_widget_render_request",
            "The widget render request is invalid or uses an unsupported schema version.",
        ),
        AppWidgetRuntimeError::Authentication(_) => api_error(
            StatusCode::UNAUTHORIZED,
            "app_widget_authentication_expired",
            "The authenticated app scope is no longer live.",
        ),
        AppWidgetRuntimeError::Registry(AppRegistryError::Overloaded) => api_error(
            StatusCode::TOO_MANY_REQUESTS,
            "app_widget_runtime_overloaded",
            "The widget runtime is at its bounded concurrency limit.",
        ),
        AppWidgetRuntimeError::PackageAdmission(AppPackageStagingError::Overloaded) => api_error(
            StatusCode::TOO_MANY_REQUESTS,
            "app_widget_runtime_overloaded",
            "The widget runtime is at its bounded concurrency limit.",
        ),
        AppWidgetRuntimeError::MissingInstallation
        | AppWidgetRuntimeError::MissingPackageRevision
        | AppWidgetRuntimeError::StaleInstallationBinding
        | AppWidgetRuntimeError::StalePackageBinding => api_error(
            StatusCode::CONFLICT,
            "app_widget_runtime_stale",
            "The widget declaration is no longer current.",
        ),
        AppWidgetRuntimeError::ProjectionTooLarge | AppWidgetRuntimeError::ResponseTooLarge => {
            api_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "app_widget_projection_too_large",
                "The widget projection exceeds its fixed render ceiling.",
            )
        },
        AppWidgetRuntimeError::InvalidCompiledPlan(_)
        | AppWidgetRuntimeError::UnsupportedCompiledDeclaration(_)
        | AppWidgetRuntimeError::ScopePlanCapacityExceeded
        | AppWidgetRuntimeError::PackageAdmissionUnavailable
        | AppWidgetRuntimeError::PackageEvidenceInvalid
        | AppWidgetRuntimeError::PackageAdmission(_)
        | AppWidgetRuntimeError::Contract(_)
        | AppWidgetRuntimeError::Encoding(_)
        | AppWidgetRuntimeError::Entity(_)
        | AppWidgetRuntimeError::Registry(_) => {
            tracing::error!(
                error = %error,
                "host widget runtime failed closed"
            );
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_widget_runtime_failed",
                "The widget runtime could not complete the request safely.",
            )
        },
    }
}

fn contract_error_response(error: AppContractError) -> HttpResponse {
    api_error(
        StatusCode::BAD_REQUEST,
        "invalid_app_contract",
        &error.to_string(),
    )
}

fn staging_error_response(error: AppPackageStagingError) -> HttpResponse {
    let status = match &error {
        AppPackageStagingError::Authentication(_) => StatusCode::UNAUTHORIZED,
        AppPackageStagingError::Overloaded => StatusCode::TOO_MANY_REQUESTS,
        AppPackageStagingError::Manifest(_)
        | AppPackageStagingError::UnsafeSource(_)
        | AppPackageStagingError::SourceChanged => StatusCode::UNPROCESSABLE_ENTITY,
        AppPackageStagingError::ContentConflict | AppPackageStagingError::ScopeCollision => {
            StatusCode::CONFLICT
        },
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let message = if status == StatusCode::INTERNAL_SERVER_ERROR {
        "The package store could not complete the request."
    } else {
        // Validation failures describe only caller-owned package content.
        // Filesystem paths and SQLite details stay behind the boundary.
        return api_error(status, "app_package_staging_failed", &error.to_string());
    };
    api_error(status, "app_package_staging_failed", message)
}

fn candidate_publication_error_response(error: AppCandidatePublicationError) -> HttpResponse {
    match error {
        AppCandidatePublicationError::Authentication(error) => api_error(
            StatusCode::UNAUTHORIZED,
            "app_candidate_authentication_failed",
            &error.to_string(),
        ),
        AppCandidatePublicationError::Staging(error) => staging_error_response(error),
        AppCandidatePublicationError::Registry(error) => registry_error_response(error),
        validation_error @ (AppCandidatePublicationError::Conformance(_)
        | AppCandidatePublicationError::Contract(_)
        | AppCandidatePublicationError::DependencyLock(_)
        | AppCandidatePublicationError::ToolCatalog(_)
        | AppCandidatePublicationError::Verification(_)) => api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "app_candidate_conformance_failed",
            &validation_error.to_string(),
        ),
        identity_error @ AppCandidatePublicationError::Identity(_) => api_error(
            StatusCode::CONFLICT,
            "app_candidate_identity_conflict",
            &identity_error.to_string(),
        ),
        internal_error @ (AppCandidatePublicationError::WorkerTerminated(_)
        | AppCandidatePublicationError::PartialPublication
        | AppCandidatePublicationError::EmbeddedContract(_)) => {
            tracing::error!(
                error = %internal_error,
                "app candidate publication failed inside the trusted boundary"
            );
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_candidate_publication_failed",
                "The app candidate could not be published safely.",
            )
        },
    }
}

fn portable_archive_error_response(error: AppPortableArchiveTransferError) -> HttpResponse {
    match error {
        AppPortableArchiveTransferError::ArchiveTooLarge => api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "app_archive_too_large",
            "The app archive exceeds its fixed byte ceiling.",
        ),
        AppPortableArchiveTransferError::InvalidPassphrase => api_error(
            StatusCode::BAD_REQUEST,
            "app_archive_passphrase_invalid",
            "The archive passphrase does not meet the fixed input requirements.",
        ),
        AppPortableArchiveTransferError::InvalidEnvelope
        | AppPortableArchiveTransferError::AuthenticationFailed
        | AppPortableArchiveTransferError::PackageBindingMismatch
        | AppPortableArchiveTransferError::Contract(_)
        | AppPortableArchiveTransferError::Package(_) => api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "app_archive_rejected",
            "The app archive failed authentication, validation, or package binding.",
        ),
        internal @ (AppPortableArchiveTransferError::Json(_)
        | AppPortableArchiveTransferError::Crypto) => {
            tracing::error!(error = %internal, "app portable archive operation failed");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_archive_operation_failed",
                "The app archive operation could not be completed safely.",
            )
        },
    }
}

fn entity_portability_error_response(error: AppEntityPortabilityError) -> HttpResponse {
    let (status, code, disclose) = match &error {
        AppEntityPortabilityError::MissingInstallation
        | AppEntityPortabilityError::MissingScopedStore => {
            (StatusCode::NOT_FOUND, "app_portability_not_found", false)
        },
        AppEntityPortabilityError::ScopeMismatch => {
            (StatusCode::FORBIDDEN, "app_portability_scope_denied", false)
        },
        AppEntityPortabilityError::EmptyDataSet
        | AppEntityPortabilityError::UnsupportedSourceState
        | AppEntityPortabilityError::PreviewSourceMismatch
        | AppEntityPortabilityError::DestinationGenerationConflict
        | AppEntityPortabilityError::IncompletePreview
        | AppEntityPortabilityError::ImportIdentityConflict
        | AppEntityPortabilityError::ReviewedMergeExecutorUnavailable
        | AppEntityPortabilityError::FutureSourceTimestamp
        | AppEntityPortabilityError::Portability(_) => {
            (StatusCode::CONFLICT, "app_portability_conflict", true)
        },
        AppEntityPortabilityError::Contract(_)
        | AppEntityPortabilityError::Schema(_)
        | AppEntityPortabilityError::Mutation(_)
        | AppEntityPortabilityError::UnknownEntity(_)
        | AppEntityPortabilityError::ExportLimit => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "app_portability_rejected",
            true,
        ),
        AppEntityPortabilityError::Registry(_)
        | AppEntityPortabilityError::Store(_)
        | AppEntityPortabilityError::Sqlite(_)
        | AppEntityPortabilityError::Json(_)
        | AppEntityPortabilityError::MissingPackage
        | AppEntityPortabilityError::MissingSchema
        | AppEntityPortabilityError::CorruptSchemaBinding
        | AppEntityPortabilityError::CorruptRecord
        | AppEntityPortabilityError::CorruptReference
        | AppEntityPortabilityError::CounterOverflow => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "app_portability_failed",
            false,
        ),
    };
    if status == StatusCode::INTERNAL_SERVER_ERROR {
        tracing::error!(error = %error, "app data portability owner failed");
    }
    let disclosed_message = disclose.then(|| error.to_string());
    api_error(
        status,
        code,
        disclosed_message
            .as_deref()
            .unwrap_or("The app portability request could not be completed."),
    )
}

fn capability_publication_error_response(error: AppCapabilityPublicationError) -> HttpResponse {
    match error {
        AppCapabilityPublicationError::Capability(error) => api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "app_capability_conformance_failed",
            &error.to_string(),
        ),
        AppCapabilityPublicationError::Registry(error) => registry_error_response(error),
    }
}

fn procedure_publication_error_response(error: AppProcedurePublicationError) -> HttpResponse {
    match error {
        AppProcedurePublicationError::Procedure(error) => api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "app_procedure_conformance_failed",
            &error.to_string(),
        ),
        AppProcedurePublicationError::Registry(error) => registry_error_response(error),
    }
}

fn transfer_error_response(error: AppPackageTransferError) -> HttpResponse {
    let status = match &error {
        AppPackageTransferError::ArchiveTooLarge | AppPackageTransferError::TooManyEntries => {
            StatusCode::PAYLOAD_TOO_LARGE
        },
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    };
    api_error(status, "invalid_app_package_archive", &error.to_string())
}

fn installation_review_error_response(error: AppInstallationReviewError) -> HttpResponse {
    match error {
        AppInstallationReviewError::Authentication(_) => api_error(
            StatusCode::UNAUTHORIZED,
            "authenticated_app_session_required",
            "A verified browser, paired-device, or local loopback session is required.",
        ),
        AppInstallationReviewError::NotFound => api_error(
            StatusCode::NOT_FOUND,
            "app_installation_not_found",
            "The app installation does not exist in this authenticated scope.",
        ),
        AppInstallationReviewError::NotReady(reason) => api_error(
            StatusCode::CONFLICT,
            "app_installation_not_ready_for_review",
            &format!("The installation is not waiting for owner review: {reason}"),
        ),
        AppInstallationReviewError::InvalidGrant(reason) => {
            api_error(StatusCode::BAD_REQUEST, "invalid_app_grant", &reason)
        },
        AppInstallationReviewError::Contract(error) => contract_error_response(error),
        AppInstallationReviewError::Manifest(error) => api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "app_review_manifest_invalid",
            &error.to_string(),
        ),
        AppInstallationReviewError::Registry(error) => registry_error_response(error),
        AppInstallationReviewError::Update(error) => update_coordinator_error_response(error),
        // The review could not read the grant already in force, so it cannot
        // say what this update would change. Deliberately distinct from the
        // conflict below: nothing is wrong with the candidate package — we
        // simply cannot produce the comparison the owner needs, and rendering
        // the request without it would read as "nothing changed".
        AppInstallationReviewError::EntityStore(error) => api_error(
            StatusCode::CONFLICT,
            "app_installation_grant_unreadable",
            &format!(
                "The grant currently in force could not be read, so the change this update \
                 would make cannot be shown: {error}"
            ),
        ),
        AppInstallationReviewError::CustomSurfaceReview(reason) => api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_app_custom_surface_review",
            &reason.to_string(),
        ),
        // Seven unrelated failures — package staging, schema and surface
        // compilation, approval invariants, the dependency lock, the agent
        // definition store, agent capability resolution — deliberately reach
        // the owner as one message, because their detail describes server
        // internals rather than anything an owner can act on.
        //
        // Collapsing them in the *log* as well was not deliberate, and it made
        // this failure undiagnosable: the cause was discarded here, the review
        // service emits no traces of its own, and the owner-facing text names
        // none of the seven. Record the real error before answering. The owner
        // still sees the generic message; the operator gets the cause.
        AppInstallationReviewError::Staging(_)
        | AppInstallationReviewError::Schema(_)
        | AppInstallationReviewError::Surface(_)
        | AppInstallationReviewError::Approval(_)
        | AppInstallationReviewError::PackageLock(_)
        | AppInstallationReviewError::Definition(_)
        | AppInstallationReviewError::AgentCapability(_) => {
            tracing::warn!(
                cause = %error,
                variant = ?error,
                "app installation review failed; owner sees the generic package-evidence message"
            );
            api_error(
                StatusCode::CONFLICT,
                "app_installation_review_failed",
                "The reviewed installation could not be approved from the current package evidence.",
            )
        },
    }
}

fn update_coordinator_error_response(error: AppUpdateCoordinatorError) -> HttpResponse {
    match error {
        AppUpdateCoordinatorError::Authentication(_) => api_error(
            StatusCode::UNAUTHORIZED,
            "authenticated_app_session_required",
            "A verified browser, paired-device, or local loopback session is required.",
        ),
        AppUpdateCoordinatorError::StaleCandidate(reason) => api_error(
            StatusCode::CONFLICT,
            "app_update_evidence_stale",
            &format!("The update evidence is stale: {reason}"),
        ),
        AppUpdateCoordinatorError::BackupRequired => api_error(
            StatusCode::CONFLICT,
            "app_update_backup_required",
            "The exact encrypted pre-update backup must be recorded before approval.",
        ),
        AppUpdateCoordinatorError::DestructiveConfirmationRequired => api_error(
            StatusCode::CONFLICT,
            "app_update_destructive_confirmation_required",
            "The reviewed destructive migration must be confirmed explicitly.",
        ),
        other @ (AppUpdateCoordinatorError::DestinationMismatch(_)
        | AppUpdateCoordinatorError::DryRunFailed(_)
        | AppUpdateCoordinatorError::Migration(_)
        | AppUpdateCoordinatorError::Schema(_)
        | AppUpdateCoordinatorError::Contract(_)) => api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "app_update_plan_invalid",
            &other.to_string(),
        ),
        other @ (AppUpdateCoordinatorError::Archive(_)
        | AppUpdateCoordinatorError::ArchivePolicy(_)
        | AppUpdateCoordinatorError::Portability(_)
        | AppUpdateCoordinatorError::BackupIo(_)) => api_error(
            StatusCode::CONFLICT,
            "app_update_backup_failed",
            &other.to_string(),
        ),
        AppUpdateCoordinatorError::Registry(AppRegistryError::MissingRecord { entity, .. })
            if entity == "installation" =>
        {
            api_error(
                StatusCode::NOT_FOUND,
                "app_installation_not_found",
                "The app installation does not exist in this authenticated scope.",
            )
        },
        AppUpdateCoordinatorError::Registry(error) => registry_error_response(error),
        other @ (AppUpdateCoordinatorError::Store(_)
        | AppUpdateCoordinatorError::Mutation(_)
        | AppUpdateCoordinatorError::Staging(_)
        | AppUpdateCoordinatorError::Sqlite(_)
        | AppUpdateCoordinatorError::Json(_)
        | AppUpdateCoordinatorError::Corrupt(_)) => api_error(
            StatusCode::CONFLICT,
            "app_update_coordinator_failed",
            &other.to_string(),
        ),
    }
}

fn installation_purge_error_response(error: AppInstallationPurgeError) -> HttpResponse {
    match error {
        AppInstallationPurgeError::InstallationNotRetained => api_error(
            StatusCode::CONFLICT,
            "app_purge_requires_retained_uninstall",
            "The installation must first be uninstalled with retained data before it can be purged.",
        ),
        AppInstallationPurgeError::DestinationRetractionsPending { .. }
        | AppInstallationPurgeError::LifecycleDeliveryPending(_) => api_error(
            StatusCode::CONFLICT,
            "app_purge_settlement_pending",
            "The retained-uninstall memory, retrieval, or lifecycle retractions are still settling; retry the exact preview request.",
        ),
        AppInstallationPurgeError::InventoryChanged
        | AppInstallationPurgeError::GenerationConflict { .. } => api_error(
            StatusCode::CONFLICT,
            "app_purge_preview_stale",
            "The installation generation or exact purge inventory changed; request and review a fresh preview.",
        ),
        AppInstallationPurgeError::InvalidCommitRequest
        | AppInstallationPurgeError::UnissuedPreview => api_error(
            StatusCode::BAD_REQUEST,
            "app_purge_commit_invalid",
            "The purge confirmation is invalid or its reviewed preview has expired.",
        ),
        AppInstallationPurgeError::PreviewQuotaExceeded => api_error(
            StatusCode::TOO_MANY_REQUESTS,
            "app_purge_preview_limit",
            "Too many unexpired purge previews exist for this installation.",
        ),
        AppInstallationPurgeError::PurgeReplayCollision
        | AppInstallationPurgeError::ScopeMismatch => api_error(
            StatusCode::CONFLICT,
            "app_purge_identity_conflict",
            "The purge identity, scope, or durable receipt does not match this request.",
        ),
        AppInstallationPurgeError::Contract(error) => contract_error_response(error),
        AppInstallationPurgeError::Registry(error) => registry_error_response(error),
        AppInstallationPurgeError::Sqlite(_)
        | AppInstallationPurgeError::Json(_)
        | AppInstallationPurgeError::Retention(_)
        | AppInstallationPurgeError::IndexProjection(_)
        | AppInstallationPurgeError::CounterOverflow
        | AppInstallationPurgeError::CorruptAccounting => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "app_purge_owner_failed",
            "The purge owner could not settle the exact reviewed inventory.",
        ),
    }
}

fn registry_error_response(error: AppRegistryError) -> HttpResponse {
    let status = match &error {
        AppRegistryError::Authentication(_) => StatusCode::UNAUTHORIZED,
        AppRegistryError::Overloaded => StatusCode::TOO_MANY_REQUESTS,
        AppRegistryError::MissingRecord { .. } => StatusCode::NOT_FOUND,
        AppRegistryError::IdentityConflict { .. }
        | AppRegistryError::StateConflict(_)
        | AppRegistryError::ScopeCollision
        | AppRegistryError::GenerationConflict { .. }
        | AppRegistryError::CompareAndSwapLost(_) => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    if status == StatusCode::INTERNAL_SERVER_ERROR {
        api_error(
            status,
            "app_registry_failed",
            "The app registry could not complete the request.",
        )
    } else {
        api_error(status, "app_registry_failed", &error.to_string())
    }
}

fn app_directory_error_response(error: AppDirectoryError) -> HttpResponse {
    match error {
        query_error @ (AppDirectoryError::InvalidCursor | AppDirectoryError::InvalidQuery(_)) => {
            api_error(
                StatusCode::BAD_REQUEST,
                "invalid_app_directory_query",
                &query_error.to_string(),
            )
        },
        AppDirectoryError::Contract(error) => contract_error_response(error),
        AppDirectoryError::InstallationUnavailable => api_error(
            StatusCode::NOT_FOUND,
            "app_directory_installation_unavailable",
            "The app installation is not available in this authenticated scope.",
        ),
        AppDirectoryError::TargetUnavailable => api_error(
            StatusCode::CONFLICT,
            "app_directory_target_stale",
            "The selected app view or action is no longer current; reload Apps.",
        ),
        AppDirectoryError::Registry(error) => registry_error_response(error),
        AppDirectoryError::Surface(_)
        | AppDirectoryError::Sqlite(_)
        | AppDirectoryError::Corrupt(_) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "app_directory_failed",
            "The Apps directory could not be loaded safely.",
        ),
    }
}

fn surface_hydration_error_response(error: AppSurfaceHydrationError) -> HttpResponse {
    match error {
        AppSurfaceHydrationError::NotFound | AppSurfaceHydrationError::InvalidRoute => api_error(
            StatusCode::NOT_FOUND,
            "app_surface_not_found",
            "The app surface does not exist in this authenticated scope.",
        ),
        AppSurfaceHydrationError::InvalidReadIntent => api_error(
            StatusCode::BAD_REQUEST,
            "invalid_app_surface_read_intent",
            "The requested sort is not declared by this app surface.",
        ),
        AppSurfaceHydrationError::RevisionChanged => api_error(
            StatusCode::CONFLICT,
            "app_surface_stale",
            "The app surface changed while it was loading; reload it.",
        ),
        AppSurfaceHydrationError::TreeRecordLimitExceeded => api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "app_tree_record_limit_exceeded",
            "This tree is too large for one safe view. Partition it into smaller trees.",
        ),
        AppSurfaceHydrationError::Entity(error) => entity_adapter_error_response(error),
        AppSurfaceHydrationError::Registry(AppRegistryError::Overloaded) => {
            let mut response = api_error(
                StatusCode::TOO_MANY_REQUESTS,
                "app_surface_overloaded",
                "The bounded app surface lane is busy; retry later.",
            );
            response.headers_mut().insert(
                actix_web::http::header::RETRY_AFTER,
                actix_web::http::header::HeaderValue::from_static("1"),
            );
            response
        },
        AppSurfaceHydrationError::Registry(error) => registry_error_response(error),
        AppSurfaceHydrationError::CorruptGeneration(_)
        | AppSurfaceHydrationError::Compiler(_)
        | AppSurfaceHydrationError::Contract(_)
        | AppSurfaceHydrationError::Sqlite(_) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "app_surface_hydration_failed",
            "The app surface could not be hydrated safely.",
        ),
    }
}

fn surface_mutation_error_response(error: AppSurfaceMutationError) -> HttpResponse {
    match error {
        AppSurfaceMutationError::RevisionChanged => api_error(
            StatusCode::CONFLICT,
            "app_surface_stale",
            "The app surface changed before the edit could be applied; reload it.",
        ),
        AppSurfaceMutationError::ReadOnlySurface => api_error(
            StatusCode::FORBIDDEN,
            "app_surface_read_only",
            "This app surface is read-only.",
        ),
        AppSurfaceMutationError::Hydration(error) => surface_hydration_error_response(error),
        AppSurfaceMutationError::Entity(AppEntityAdapterError::Mutation(
            AppEntityMutationError::Boundary(AppBoundaryError::StaleProjectedAuthority),
        )) => api_error(
            StatusCode::CONFLICT,
            "app_surface_stale",
            "The app surface changed before the edit could be applied; reload it.",
        ),
        AppSurfaceMutationError::Entity(error) => entity_adapter_error_response(error),
        AppSurfaceMutationError::Contract(error) => contract_error_response(error),
    }
}

fn entity_change_error_response(error: AppEntityChangeError) -> HttpResponse {
    match error {
        AppEntityChangeError::NotFound => api_error(
            StatusCode::NOT_FOUND,
            "app_surface_not_found",
            "The app surface does not exist in this authenticated scope.",
        ),
        AppEntityChangeError::SurfaceRevisionChanged => api_error(
            StatusCode::CONFLICT,
            "app_surface_stale",
            "The app surface changed while its updates were loading; reload it.",
        ),
        AppEntityChangeError::CursorAheadOfHead { .. } => api_error(
            StatusCode::CONFLICT,
            "app_change_cursor_ahead",
            "The app change cursor is ahead of the durable head; reload the surface.",
        ),
        AppEntityChangeError::InvalidLimit => api_error(
            StatusCode::BAD_REQUEST,
            "invalid_app_change_limit",
            &error.to_string(),
        ),
        AppEntityChangeError::Registry(AppRegistryError::Overloaded) => {
            let mut response = api_error(
                StatusCode::TOO_MANY_REQUESTS,
                "app_surface_overloaded",
                "The bounded app surface lane is busy; retry later.",
            );
            response.headers_mut().insert(
                actix_web::http::header::RETRY_AFTER,
                actix_web::http::header::HeaderValue::from_static("1"),
            );
            response
        },
        AppEntityChangeError::Registry(error) => registry_error_response(error),
        AppEntityChangeError::CorruptInstallation(_)
        | AppEntityChangeError::CorruptHistory(_)
        | AppEntityChangeError::Contract(_)
        | AppEntityChangeError::Sqlite(_)
        | AppEntityChangeError::Encoding(_) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "app_entity_changes_failed",
            "The app surface changes could not be read safely.",
        ),
    }
}

fn entity_adapter_error_response(error: AppEntityAdapterError) -> HttpResponse {
    let (status, code, expose_message) = match &error {
        AppEntityAdapterError::Boundary(_) => {
            (StatusCode::FORBIDDEN, "app_data_authority_rejected", false)
        },
        AppEntityAdapterError::MissingInstallation => {
            (StatusCode::NOT_FOUND, "app_installation_not_found", false)
        },
        AppEntityAdapterError::StaleSurfaceRevision
        | AppEntityAdapterError::StaleWorkflowBinding => {
            (StatusCode::CONFLICT, "app_surface_stale", false)
        },
        AppEntityAdapterError::PersonalAgentProjectionDenied
        | AppEntityAdapterError::PersonalAgentSearchDenied
        | AppEntityAdapterError::PersonalAgentFieldDenied(_)
        | AppEntityAdapterError::UnknownEntity(_)
        | AppEntityAdapterError::UnknownRelation(_) => {
            (StatusCode::FORBIDDEN, "app_data_projection_denied", false)
        },
        AppEntityAdapterError::Contract(_) => {
            (StatusCode::BAD_REQUEST, "invalid_app_data_contract", true)
        },
        AppEntityAdapterError::Json(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "app_data_encoding_failed",
            false,
        ),
        AppEntityAdapterError::Store(store) => match store {
            AppEntityStoreError::Registry(AppRegistryError::Overloaded) => {
                (StatusCode::TOO_MANY_REQUESTS, "app_data_overloaded", false)
            },
            AppEntityStoreError::MissingInstallation | AppEntityStoreError::MissingScopedStore => {
                (StatusCode::NOT_FOUND, "app_data_not_found", false)
            },
            AppEntityStoreError::MissingCursor => {
                (StatusCode::GONE, "app_data_cursor_unavailable", false)
            },
            AppEntityStoreError::Boundary(_) | AppEntityStoreError::PersonalAgentPolicyDenied => {
                (StatusCode::FORBIDDEN, "app_data_authority_rejected", false)
            },
            AppEntityStoreError::Query(AppQuerySemanticsError::CursorExpired) => {
                (StatusCode::GONE, "app_data_cursor_unavailable", false)
            },
            AppEntityStoreError::Query(AppQuerySemanticsError::CursorSnapshotStale) => {
                (StatusCode::CONFLICT, "app_data_stale", false)
            },
            AppEntityStoreError::Query(AppQuerySemanticsError::CursorIdentityMismatch) => {
                (StatusCode::BAD_REQUEST, "invalid_app_data_cursor", false)
            },
            AppEntityStoreError::Query(
                AppQuerySemanticsError::CursorStorageCorrupt
                | AppQuerySemanticsError::InvalidDatasetGeneration
                | AppQuerySemanticsError::InvalidCursorWindow
                | AppQuerySemanticsError::Encoding(_),
            ) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_data_query_failed",
                false,
            ),
            AppEntityStoreError::Contract(_)
            | AppEntityStoreError::Query(_)
            | AppEntityStoreError::UnknownEntity(_)
            | AppEntityStoreError::UnknownField(_)
            | AppEntityStoreError::UnknownRelation(_)
            | AppEntityStoreError::InvalidPredicate
            | AppEntityStoreError::EmptyProjection => {
                (StatusCode::BAD_REQUEST, "invalid_app_data_query", true)
            },
            AppEntityStoreError::KeysetIndexRequired => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "app_data_keyset_index_required",
                true,
            ),
            AppEntityStoreError::SnapshotTooLarge
            | AppEntityStoreError::QueryScanTooLarge
            | AppEntityStoreError::SourceProjectionTooLarge
            | AppEntityStoreError::RelationProjectionTooLarge => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "app_data_limit_exceeded",
                true,
            ),
            AppEntityStoreError::CursorCapacityExceeded => (
                StatusCode::TOO_MANY_REQUESTS,
                "app_data_cursor_capacity",
                false,
            ),
            AppEntityStoreError::StaleDatasetGeneration
            | AppEntityStoreError::CursorSnapshotUnavailable
            | AppEntityStoreError::InstallationNotEnabled(_) => {
                (StatusCode::CONFLICT, "app_data_stale", false)
            },
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_data_query_failed",
                false,
            ),
        },
        AppEntityAdapterError::Mutation(mutation) => match mutation {
            AppEntityMutationError::Registry(AppRegistryError::Overloaded) => {
                (StatusCode::TOO_MANY_REQUESTS, "app_data_overloaded", false)
            },
            AppEntityMutationError::MissingInstallation
            | AppEntityMutationError::RecordNotFound
            | AppEntityMutationError::RelationNotFound => {
                (StatusCode::NOT_FOUND, "app_data_not_found", false)
            },
            AppEntityMutationError::Boundary(_) => {
                (StatusCode::FORBIDDEN, "app_data_authority_rejected", false)
            },
            AppEntityMutationError::Contract(_)
            | AppEntityMutationError::InvalidPatch
            | AppEntityMutationError::UnknownEntity(_)
            | AppEntityMutationError::UnknownRelation(_)
            | AppEntityMutationError::AmbiguousRelation(_)
            // A caller-chosen record id inside the store's own `rec_` namespace
            // is a malformed request, not a server fault.
            | AppEntityMutationError::ReservedRecordId
            | AppEntityMutationError::EmptyCommit => {
                (StatusCode::BAD_REQUEST, "invalid_app_data_mutation", true)
            },
            AppEntityMutationError::SchemaRevisionConflict
            | AppEntityMutationError::RecordAlreadyExists
            | AppEntityMutationError::RecordRevisionConflict
            | AppEntityMutationError::RecordDeleted
            | AppEntityMutationError::RecordNotDeleted
            | AppEntityMutationError::RelationEndpointDeleted
            | AppEntityMutationError::RelationRevisionConflict
            | AppEntityMutationError::RelationAlreadyExists
            | AppEntityMutationError::IdempotencyConflict
            | AppEntityMutationError::ReferencedRecordDeleteDenied
            | AppEntityMutationError::ReferenceCycleDenied => {
                (StatusCode::CONFLICT, "app_data_conflict", true)
            },
            AppEntityMutationError::StorageCeilingExceeded
            | AppEntityMutationError::IncomingReferenceLimit
            | AppEntityMutationError::MutationExpansionLimit
            | AppEntityMutationError::CycleCheckLimit
            | AppEntityMutationError::ReferenceTargetUnavailable { .. } => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "app_data_limit_exceeded",
                true,
            ),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "app_data_mutation_failed",
                false,
            ),
        },
    };
    let message = if expose_message {
        error.to_string()
    } else {
        match status {
            StatusCode::NOT_FOUND => {
                "The requested app data is not available in this authenticated scope.".to_owned()
            },
            StatusCode::FORBIDDEN => {
                "The current authenticated authority does not permit this app data operation."
                    .to_owned()
            },
            StatusCode::TOO_MANY_REQUESTS => {
                "The bounded app data lane is busy; retry later.".to_owned()
            },
            StatusCode::GONE => {
                "The app data cursor is no longer available; restart the query.".to_owned()
            },
            StatusCode::CONFLICT => "The app data operation is stale; reload and retry.".to_owned(),
            _ => "The app data service could not complete the request.".to_owned(),
        }
    };
    let mut response = api_error(status, code, &message);
    if status == StatusCode::TOO_MANY_REQUESTS {
        response.headers_mut().insert(
            actix_web::http::header::RETRY_AFTER,
            actix_web::http::header::HeaderValue::from_static("1"),
        );
    }
    response
}

fn corrupt_registry_response(missing: &str) -> HttpResponse {
    api_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "app_registry_incomplete",
        &format!("The installation is missing its {missing}."),
    )
}

fn api_error(status: StatusCode, code: &str, message: &str) -> HttpResponse {
    HttpResponse::build(status)
        .insert_header((actix_web::http::header::CACHE_CONTROL, "no-store"))
        .json(json!({ "error": code, "message": message }))
}

#[derive(Deserialize)]
struct AppLegacyPublicErrorBody {
    error: String,
    message: String,
}

fn supported_public_error_semantics(
    operation: AppPublicOperationId,
    status: StatusCode,
    reason: Option<&str>,
) -> (AppErrorCode, AppErrorDisposition) {
    let reason = reason.unwrap_or_default();
    if matches!(
        reason,
        "authenticated_app_session_required"
            | "app_scope_mismatch"
            | "stale_authenticated_request"
            | "invalid_authentication_revision"
            | "loopback_peer_required"
    ) {
        return (
            AppErrorCode::NotAuthorized,
            AppErrorDisposition::Reauthorize,
        );
    }
    if matches!(
        reason,
        "app_authority_rejected"
            | "app_data_authority_rejected"
            | "app_data_projection_denied"
            | "app_workflow_not_authorized"
            | "app_projection_not_authorized"
            | "app_action_composition_not_authorized"
    ) {
        return (
            AppErrorCode::PolicyDenied,
            AppErrorDisposition::UserActionRequired,
        );
    }
    if reason == "app_workspace_required" {
        return (
            AppErrorCode::NotAuthorized,
            AppErrorDisposition::UserActionRequired,
        );
    }
    if matches!(
        reason,
        "app_surface_stale"
            | "app_data_stale"
            | "app_data_cursor_unavailable"
            | "app_workflow_stale"
            | "app_projection_expired"
            | "app_change_cursor_ahead"
            | "app_action_composition_unavailable"
    ) || matches!(
        (operation, reason),
        (
            AppPublicOperationId::CancelActionRun,
            "app_action_not_started" | "app_action_already_completed"
        )
    ) {
        return (
            AppErrorCode::StaleRevision,
            AppErrorDisposition::RefreshAndRetry,
        );
    }
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => (
            AppErrorCode::NotAuthorized,
            AppErrorDisposition::Reauthorize,
        ),
        StatusCode::NOT_FOUND => (AppErrorCode::NotFound, AppErrorDisposition::Terminal),
        StatusCode::CONFLICT => (AppErrorCode::Conflict, AppErrorDisposition::Terminal),
        StatusCode::GONE => (
            AppErrorCode::StaleRevision,
            AppErrorDisposition::RefreshAndRetry,
        ),
        StatusCode::PAYLOAD_TOO_LARGE => (
            AppErrorCode::ResourceExhausted,
            AppErrorDisposition::Terminal,
        ),
        StatusCode::UNPROCESSABLE_ENTITY if reason.contains("limit") => (
            AppErrorCode::ResourceExhausted,
            AppErrorDisposition::Terminal,
        ),
        StatusCode::UNPROCESSABLE_ENTITY => {
            (AppErrorCode::SchemaMismatch, AppErrorDisposition::Terminal)
        },
        StatusCode::TOO_MANY_REQUESTS => (
            AppErrorCode::RateLimited,
            AppErrorDisposition::RetrySameInput,
        ),
        StatusCode::SERVICE_UNAVAILABLE => (
            AppErrorCode::Unavailable,
            AppErrorDisposition::RetrySameInput,
        ),
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => {
            (AppErrorCode::Timeout, AppErrorDisposition::RetrySameInput)
        },
        status if status.is_server_error() => {
            (AppErrorCode::Internal, AppErrorDisposition::Terminal)
        },
        _ => (AppErrorCode::InvalidRequest, AppErrorDisposition::Terminal),
    }
}

fn bounded_public_error_message(message: &str) -> String {
    if message.len() <= 4_096 {
        return message.to_owned();
    }
    let mut boundary = 4_096;
    while !message.is_char_boundary(boundary) {
        boundary -= 1;
    }
    message[..boundary].to_owned()
}

fn trace_supported_public_request(operation: AppPublicOperationId) {
    AppTraceEvent::new(
        AppTraceStage::Request,
        AppTraceOperation::from_supported_public(operation),
    )
    .emit();
}

fn supported_public_trace_status(
    status: StatusCode,
    disposition: Option<AppErrorDisposition>,
) -> (AppTraceOutcome, AppTraceRetryClass) {
    let outcome = if status.is_success() {
        AppTraceOutcome::Completed
    } else if disposition == Some(AppErrorDisposition::OutcomeUncertain) {
        AppTraceOutcome::Uncertain
    } else if matches!(
        disposition,
        Some(AppErrorDisposition::Reauthorize | AppErrorDisposition::UserActionRequired)
    ) || matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
    {
        AppTraceOutcome::Denied
    } else if matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::REQUEST_TIMEOUT
            | StatusCode::GATEWAY_TIMEOUT
    ) {
        AppTraceOutcome::Unavailable
    } else {
        AppTraceOutcome::Failed
    };
    let retry_class = match disposition {
        None | Some(AppErrorDisposition::Terminal) => AppTraceRetryClass::None,
        Some(AppErrorDisposition::RetrySameInput) => AppTraceRetryClass::SameInput,
        Some(AppErrorDisposition::RefreshAndRetry) => AppTraceRetryClass::Refresh,
        Some(AppErrorDisposition::Reauthorize) => AppTraceRetryClass::Reauthorize,
        Some(AppErrorDisposition::UserActionRequired) => AppTraceRetryClass::UserAction,
        Some(AppErrorDisposition::OutcomeUncertain) => AppTraceRetryClass::ReconcileUncertain,
    };
    (outcome, retry_class)
}

fn trace_supported_public_outcome(
    operation: AppPublicOperationId,
    status: StatusCode,
    disposition: Option<AppErrorDisposition>,
) {
    let (outcome, retry_class) = supported_public_trace_status(status, disposition);
    AppTraceEvent::new(
        AppTraceStage::Publish,
        AppTraceOperation::from_supported_public(operation),
    )
    .outcome(outcome)
    .retry_class(retry_class)
    .emit();
}

async fn canonicalize_supported_public_response(
    operation: AppPublicOperationId,
    response: HttpResponse,
) -> HttpResponse {
    if response.status().is_success() {
        trace_supported_public_outcome(operation, response.status(), None);
        return response;
    }
    let status = response.status();
    let retry_after_header = response
        .headers()
        .get(actix_web::http::header::RETRY_AFTER)
        .cloned();
    let parsed_retry_after_ms = retry_after_header
        .as_ref()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .and_then(|seconds| seconds.checked_mul(1_000))
        .filter(|delay| *delay > 0);
    let body = to_bytes(response.into_body()).await.ok();
    let legacy = body
        .as_deref()
        .and_then(|body| serde_json::from_slice::<AppLegacyPublicErrorBody>(body).ok());
    let operation_contract = supported_public_operation(operation);
    let reason = legacy
        .as_ref()
        .map(|error| error.error.as_str())
        .filter(|reason| {
            operation_contract
                .errors
                .iter()
                .any(|candidate| candidate == reason)
        });
    let (code, disposition) = supported_public_error_semantics(operation, status, reason);
    let retry_after_ms = parsed_retry_after_ms
        .filter(|_| matches!(code, AppErrorCode::RateLimited | AppErrorCode::Unavailable));
    let message = if status.is_server_error() {
        "The supported-public app operation could not be completed safely.".to_owned()
    } else {
        bounded_public_error_message(
            legacy
                .as_ref()
                .map(|error| error.message.as_str())
                .unwrap_or("The supported-public app operation was rejected."),
        )
    };
    let mut details = BTreeMap::from([(
        AppName::parse("operation_id").expect("constant public error detail key"),
        json!(operation.as_str()),
    )]);
    if let Some(reason) = reason {
        details.insert(
            AppName::parse("reason").expect("constant public error detail key"),
            json!(reason),
        );
    }
    let envelope = AppErrorEnvelope {
        code,
        disposition,
        message,
        details,
        retry_after_ms,
    };
    debug_assert!(
        envelope
            .validate_app_contract(&AppContractLimits::default())
            .is_ok(),
        "supported-public error mapping must remain a valid canonical envelope"
    );
    trace_supported_public_outcome(operation, status, Some(disposition));
    let mut canonical = HttpResponse::build(status);
    canonical.insert_header((actix_web::http::header::CACHE_CONTROL, "private, no-store"));
    if let Some(retry_after) = retry_after_header {
        canonical.insert_header((actix_web::http::header::RETRY_AFTER, retry_after));
    }
    canonical.json(envelope)
}

#[cfg(test)]
mod provider_free_native_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use actix_web::{middleware::from_fn, test, App};
    use tempfile::TempDir;

    use magician::magician_v2::apps::authoring::{
        run_app_authoring_command, AppAuthoringCommand, AppCheckArgs,
    };
    use magician::magician_v2::apps::entity_store::tests::{
        compiled_schema, seed_enabled_installation, seed_records,
    };
    use magician::magician_v2::apps::manifest::{
        build_app_package_candidate, tests::valid_skill_document, AppBundleMember,
        AppPackageCandidate, AppPackageLimits,
    };
    use magician::magician_v2::apps::models::{
        AppFieldPath, AppMutationAtomicity, AppMutationOperation, AppName, AppOrderDirection,
        AppProtocolVersion, AppQueryOrder,
    };
    use magician::magician_v2::apps::package_transfer::tests::archive_bytes_fixture;
    use magician::magician_v2::apps::records::AppGrantedCustomSurfaceEntryPoint;
    use magician::magician_v2::apps::records::AppInstallation;
    use magician::magician_v2::apps::registry::tests::{authenticated_scope, time};
    use magician::magician_v2::apps::system_boot_admission::resolve_system_package_inventory;
    use magician::magician_v2::apps::tool_catalog::AppReviewedToolCatalog;
    use magician::magician_v2::cloudflare_access::verify_access_middleware;
    use magician_apps::apps::custom_surface_review::reviewed_custom_surface_request;
    use magician_apps::apps::registry_lifecycle::AppLifecycleEventKind;
    use magician_apps::apps::surface_runtime::surface_admission_for_enabled_installation;
    use magician_apps::apps::surface_scripted_host::compile_scripted_surface_host_plan;

    #[::core::prelude::v1::test]
    fn background_behavior_supervisor_restart_backoff_is_bounded() {
        assert_eq!(
            app_background_behavior_restart_delay(0),
            StdDuration::from_secs(1)
        );
        assert_eq!(
            app_background_behavior_restart_delay(4),
            StdDuration::from_secs(16)
        );
        assert_eq!(
            app_background_behavior_restart_delay(5),
            APP_BACKGROUND_BEHAVIOR_RESTART_MAX
        );
        assert_eq!(
            app_background_behavior_restart_delay(u32::MAX),
            APP_BACKGROUND_BEHAVIOR_RESTART_MAX
        );
    }

    #[::core::prelude::v1::test]
    fn custom_surface_action_caller_is_host_owned_and_session_independent() {
        let caller = AppReference::parse(APP_CUSTOM_SURFACE_ACTION_CALLER_REF)
            .expect("static custom-surface caller reference");
        assert_eq!(caller.as_str(), "surface:app-custom-surface-data-plane");
        assert!(!caller.as_str().contains("session"));

        let source = include_str!("apps_api.rs");
        let bridge = source
            .split("async fn execute_admitted_bridge")
            .nth(1)
            .expect("bridge owner");
        let invoke = bridge
            .split("AppBridgeMethod::InvokeAction")
            .nth(1)
            .expect("invoke branch")
            .split("AppBridgeMethod::Subscribe")
            .next()
            .expect("invoke branch end");
        assert!(invoke.contains("AppReference::parse(APP_CUSTOM_SURFACE_ACTION_CALLER_REF)"));
        assert!(invoke.contains("invocation.caller_surface_or_execution_ref = caller"));
        assert!(!invoke.contains("message.session_ref.clone()"));
    }

    #[::core::prelude::v1::test]
    fn scripted_surface_bridge_dispatch_keeps_headroom_on_default_worker_stacks() {
        let temp = canonical_tempdir();
        let api = AppPlatformApi::new(ArtifactV2Workspace::new(temp.path()));
        let owner = authenticated_scope("owner", "private");
        let message = AppBridgeMessage {
            schema_version: 1,
            request_id: AppReference::parse("request:stack-check").unwrap(),
            sequence: 1,
            method: AppBridgeMethod::InvokeAction,
            origin: "null".to_owned(),
            session_ref: AppReference::parse("session:stack-check").unwrap(),
            nonce: AppReference::parse("nonce:stack-check").unwrap(),
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            package_revision_ref: AppReference::parse("package:stack-check").unwrap(),
            surface_revision: AppRevision::new(1).unwrap(),
            grant_revision: AppRevision::new(1).unwrap(),
            view_or_action: Some(AppName::parse("sync_sessions").unwrap()),
            payload: json!({}),
        };
        let future = execute_admitted_bridge(&api, &owner, &message, None, Utc::now());
        let bytes = std::mem::size_of_val(&future);
        assert!(
            bytes < 16 * 1024,
            "bridge dispatch reserves {bytes} bytes before the workflow starts"
        );
    }

    async fn read_http_response_json<T: DeserializeOwned>(response: HttpResponse) -> T {
        let body = to_bytes(response.into_body()).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn custom_surface_launch(installation: &str, action: &str) -> AppWorkflowLaunch {
        AppWorkflowLaunch {
            run_handle: AppRunHandle {
                protocol_version: AppProtocolVersion::V1,
                run_ref: AppReference::parse("run:app-action:surface-test").unwrap(),
                installation_id: AppInstallationId::parse(installation).unwrap(),
                action_id: AppName::parse(action).unwrap(),
            },
            task_id: "task-must-not-cross-worker".to_owned(),
            execution_id: Some("execution-must-not-cross-worker".to_owned()),
            result: None,
        }
    }

    #[actix_web::test]
    async fn custom_surface_launch_reply_strips_owner_ids_and_checks_install_action() {
        let install = AppInstallationId::parse("install_1").unwrap();
        let action = AppName::parse("build").unwrap();
        let reply = custom_surface_reply_from_launch(
            custom_surface_launch("install_1", "build"),
            &install,
            Some(&action),
        )
        .expect("bound launch");
        let encoded = serde_json::to_string(&reply).expect("reply");
        assert!(!encoded.contains("task-must-not-cross-worker"));
        assert!(!encoded.contains("execution-must-not-cross-worker"));
        assert_eq!(
            reply.retry_disposition,
            AppCustomSurfaceRetryDisposition::PollRun
        );
        assert!(custom_surface_reply_from_launch(
            custom_surface_launch("install_other", "build"),
            &install,
            Some(&action),
        )
        .is_err());
        assert!(custom_surface_reply_from_launch(
            custom_surface_launch("install_1", "other"),
            &install,
            Some(&action),
        )
        .is_err());
    }

    #[actix_web::test]
    async fn supported_public_trace_status_uses_only_closed_outcomes_and_retry_classes() {
        assert_eq!(
            supported_public_trace_status(StatusCode::OK, None),
            (AppTraceOutcome::Completed, AppTraceRetryClass::None)
        );
        assert_eq!(
            supported_public_trace_status(
                StatusCode::FORBIDDEN,
                Some(AppErrorDisposition::UserActionRequired),
            ),
            (AppTraceOutcome::Denied, AppTraceRetryClass::UserAction)
        );
        assert_eq!(
            supported_public_trace_status(
                StatusCode::SERVICE_UNAVAILABLE,
                Some(AppErrorDisposition::RetrySameInput),
            ),
            (AppTraceOutcome::Unavailable, AppTraceRetryClass::SameInput,)
        );
        assert_eq!(
            supported_public_trace_status(
                StatusCode::CONFLICT,
                Some(AppErrorDisposition::RefreshAndRetry),
            ),
            (AppTraceOutcome::Failed, AppTraceRetryClass::Refresh)
        );
        assert_eq!(
            supported_public_trace_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                Some(AppErrorDisposition::OutcomeUncertain),
            ),
            (
                AppTraceOutcome::Uncertain,
                AppTraceRetryClass::ReconcileUncertain,
            )
        );
    }

    fn canonical_tempdir() -> TempDir {
        let root = std::fs::canonicalize(std::env::temp_dir()).expect("canonical temporary root");
        tempfile::tempdir_in(root).expect("temporary directory")
    }

    fn copy_directory_tree(source: &std::path::Path, destination: &std::path::Path) {
        std::fs::create_dir_all(destination).expect("create copied package directory");
        for entry in std::fs::read_dir(source).expect("read source package directory") {
            let entry = entry.expect("read source package entry");
            let source_path = entry.path();
            let destination_path = destination.join(entry.file_name());
            if entry.file_type().expect("read package entry type").is_dir() {
                copy_directory_tree(&source_path, &destination_path);
            } else {
                std::fs::copy(&source_path, &destination_path).expect("copy package member");
            }
        }
    }

    fn rewrite_seed_version(package_root: &std::path::Path, from: &str, to: &str) {
        let manifest_path = package_root.join("SKILL.md");
        let manifest = std::fs::read_to_string(&manifest_path).expect("read seed manifest");
        let from_line = format!("version: {from}");
        assert!(
            manifest.contains(&from_line),
            "seed manifest does not contain expected {from_line}"
        );
        std::fs::write(
            &manifest_path,
            manifest.replacen(&from_line, &format!("version: {to}"), 1),
        )
        .expect("rewrite seed version");
        run_app_authoring_command(&AppAuthoringCommand::Check(AppCheckArgs {
            path: package_root.to_path_buf(),
            write_generated: true,
        }))
        .expect("regenerate package artifacts after version change");
    }

    /// Reproduce the production migration that made every built-in package
    /// unavailable: an older immutable package revision is already persisted,
    /// then the binary's primitive action bindings change. A patch-bumped seed
    /// must publish with the new lock, become owner-enabled, and still reach
    /// Boot admission enumerates every scope directory, and a reserved sink is
    /// a directory like any other. Admitting into one copied the whole system
    /// package set and an app store into a bucket with no owner to approve an
    /// app and no surface to run it — measured at 7.5 MB of byte-identical
    /// bundles per sink. Enumerating them is fine; admitting into them is not.
    #[actix_web::test]
    async fn boot_admission_skips_reserved_sinks_and_keeps_every_tenant() {
        use magician::magician_v2::transport_log::{
            QUARANTINE_PRINCIPAL, QUARANTINE_WORKSPACE, SYSTEM_PRINCIPAL, SYSTEM_WORKSPACE,
        };

        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        // `list_scopes` is a directory scan, so a bare directory is a scope.
        for (principal, workspace_name) in [
            (DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE),
            ("alice", "default"),
            (SYSTEM_PRINCIPAL, SYSTEM_WORKSPACE),
            (QUARANTINE_PRINCIPAL, QUARANTINE_WORKSPACE),
        ] {
            std::fs::create_dir_all(workspace.scope_root(principal, workspace_name))
                .expect("scope directory");
        }

        let reports = AppPlatformApi::new(workspace)
            .admit_system_packages_at_boot(AppSystemPackageSettings::default())
            .await;
        let visited = reports
            .iter()
            .map(|summary| {
                (
                    summary.scope_principal.as_str(),
                    summary.scope_workspace.as_str(),
                )
            })
            .collect::<std::collections::BTreeSet<_>>();

        assert!(
            visited.contains(&(DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE)),
            "the default scope is the one a single-user deployment uses: {visited:?}"
        );
        assert!(
            visited.contains(&("alice", "default")),
            "an ordinary tenant must still be admitted: {visited:?}"
        );
        assert!(
            !visited.contains(&(SYSTEM_PRINCIPAL, SYSTEM_WORKSPACE)),
            "the scopeless-event sink has no owner and no surface: {visited:?}"
        );
        assert!(
            !visited.contains(&(QUARANTINE_PRINCIPAL, QUARANTINE_WORKSPACE)),
            "the quarantine sink has no owner and no surface: {visited:?}"
        );
    }

    /// its pinned slot; preserving the old version's stale lock at either
    /// publication boundary makes this test fail before the slot assertion.
    #[actix_web::test]
    async fn a_version_bumped_seed_relocks_after_a_stale_published_revision_and_pins_its_slot() {
        let temp = canonical_tempdir();
        let seed_root = temp.path().join("seed");
        let package_root = seed_root.join("system/meetings/app");
        // Exactly one package, because the admission assertions below count on
        // it — copying the whole seed would admit every shipped system package
        // and break `admitted() == 1`. The bootstrap material review needs is
        // copied alongside it, not the other packages.
        let shipped_seed =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician_data_v3");
        copy_directory_tree(&shipped_seed.join("system/meetings/app"), &package_root);
        // Review resolves every workflow's runner from the scope's agents;
        // these workflows declare none, so all eleven fall back to
        // `DEFAULT_APP_WORKFLOW_AGENT`, whose definition is seeded from these
        // template roots. Without them the review fails on the runner long
        // before the slot assertion this test exists for.
        for bootstrap in ["agent_templates", "trust_policy_templates", "db_templates"] {
            let source = shipped_seed.join("system").join(bootstrap);
            if source.is_dir() {
                copy_directory_tree(&source, &seed_root.join("system").join(bootstrap));
            }
        }
        // `anonymous/default` is `DEFAULT_SCOPE_PRINCIPAL`/`DEFAULT_SCOPE_WORKSPACE`
        // — the scope this test reviews in — and the shipped seed scaffolds it,
        // `agent_runtime/agents` included. Copy that too: the definition store
        // reads through this layout, and a seed root without it fails the read
        // rather than falling back to the templates.
        let shipped_scopes = shipped_seed.join("scopes");
        if shipped_scopes.is_dir() {
            copy_directory_tree(&shipped_scopes, &seed_root.join("scopes"));
        }

        // Materialize the package version and generated bytes an earlier
        // binary published, then lock it without today's primitive bindings.
        let manifest_path = package_root.join("SKILL.md");
        let shipped_manifest = std::fs::read_to_string(&manifest_path).unwrap();
        let shipped_version = shipped_manifest
            .lines()
            .find_map(|line| line.strip_prefix("version: "))
            .expect("shipped version");
        // The previous package used auto workflows. Native transaction recipes
        // correctly refuse a catalog without primitive bindings, so reproduce
        // the old package shape as well as its old version.
        let (yaml, prose) = shipped_manifest
            .strip_prefix("---\n")
            .unwrap()
            .split_once("\n---\n")
            .unwrap();
        let mut old_manifest: serde_json::Value = serde_yaml::from_str(yaml).unwrap();
        for workflow in old_manifest["app"]["workflows"]
            .as_object_mut()
            .unwrap()
            .values_mut()
        {
            workflow["runner"] = json!("auto");
            workflow.as_object_mut().unwrap().remove("recipe");
            assert!(!workflow["may_mutate"].as_array().unwrap().is_empty());
            workflow["result"] =
                json!({"kind":"entity_projection", "entities":workflow["may_mutate"]});
        }
        let old_manifest = format!(
            "---\n{}---\n{prose}",
            serde_yaml::to_string(&old_manifest).unwrap()
        );
        std::fs::write(&manifest_path, old_manifest).unwrap();
        rewrite_seed_version(&package_root, shipped_version, "0.0.1");
        let workspace = ArtifactV2Workspace::new(temp.path()).with_seed_root(&seed_root);
        let api = AppPlatformApi::new(workspace.clone());
        let now = Utc::now();
        let boot_scope = system_worker_scope_for_actor(
            DEFAULT_SCOPE_PRINCIPAL,
            DEFAULT_SCOPE_WORKSPACE,
            "worker:stale-seed-fixture",
            &AppReference::parse("run:stale-seed-fixture").expect("run ref"),
            now,
        )
        .expect("system worker scope");
        let old_admission = resolve_system_package_inventory(&workspace)
            .expect("old seed inventory resolves")
            .into_iter()
            .next()
            .expect("meetings seed is present");
        let stale = api
            .candidate_publications
            .publish_trusted_system_package_with_catalog_for_test(
                &boot_scope,
                old_admission,
                AppReviewedToolCatalog::resolver_snapshot(),
                now,
            )
            .await
            .expect("the previous binary publishes its immutable lock");

        // Ship the deliberate patch bump and let the real boot owner resolve
        // today's embedded catalog. This is the transition restart alone did
        // not provide when the seed remained at 0.1.1.
        std::fs::write(&manifest_path, &shipped_manifest).unwrap();
        run_app_authoring_command(&AppAuthoringCommand::Check(AppCheckArgs {
            path: package_root.clone(),
            write_generated: true,
        }))
        .expect("regenerate the current native package");
        let reports = api
            .admit_system_packages_at_boot(AppSystemPackageSettings::default())
            .await;
        assert_eq!(
            reports.len(),
            1,
            "the fixture has exactly the default scope"
        );
        let failures = reports[0]
            .report
            .failures()
            .map(|(package, error)| format!("{package}: {error}"))
            .collect::<Vec<_>>();
        assert!(
            failures.is_empty(),
            "re-lock admission failed: {failures:?}"
        );
        assert_eq!(reports[0].report.admitted(), 1);
        let current = reports[0].report.outcomes[0]
            .result
            .as_ref()
            .expect("the bumped seed publishes");
        assert_ne!(current.package_revision_ref, stale.package_revision_ref);
        assert_ne!(current.dependency_lock_digest, stale.dependency_lock_digest);

        // Admission is intentionally inert. The normal owner approval must
        // still enable the new revision and make its shipped default visible.
        let approval_time = Utc::now();
        let owner =
            magician::magician_v2::apps::authority::AuthenticatedAppScope::from_verified_session(
                magician::magician_v2::apps::records::AppScope {
                    principal: AppReference::parse(DEFAULT_SCOPE_PRINCIPAL).expect("principal"),
                    workspace: AppReference::parse(DEFAULT_SCOPE_WORKSPACE).expect("workspace"),
                },
                magician::magician_v2::apps::models::AppScopeBindingRef::parse("scope_1")
                    .expect("scope binding"),
                AppReference::parse("actor:owner").expect("actor"),
                AppReference::parse("session:stale-seed-migration").expect("session"),
                magician::magician_v2::apps::models::AppRevision::new(1).expect("revision"),
                approval_time,
                approval_time + ChronoDuration::minutes(30),
            )
            .expect("owner session");
        let review = api
            .installation_review()
            .review(&owner, &current.installation_id, Utc::now())
            .await
            .expect("review bumped seed");
        api.installation_review()
            .approve(
                &owner,
                &current.installation_id,
                magician_apps::apps::installation_review::AppInstallationApproveRequest {
                    review_material_digest: Some(review.workflow_material_digest),
                    ..Default::default()
                },
                Utc::now(),
            )
            .await
            .expect("approve bumped seed");
        let maintained = api
            .refresh_system_slot_defaults(&owner, &current.installation_id)
            .await
            .expect("approval pins the bumped seed defaults");
        assert!(maintained.pinned_slots > 0);

        let inventory = api
            .widget_runtime()
            .snapshot(&owner, Utc::now())
            .await
            .expect("live widget inventory");
        let slot_id =
            AppSlotId::for_page_region("/observe", &AppName::parse("capture").expect("region"))
                .expect("slot id");
        let resolved = api
            .slot_assignments
            .resolve_slot(owner.scope(), DEFAULT_SCOPE_PRINCIPAL, &slot_id, &inventory)
            .expect("the meetings slot resolves");
        assert!(resolved.pinned_system_default);
        assert!(resolved.widget.is_some());
    }

    /// The contested-slot warning stays wired into the boot owner.
    ///
    /// `TrustedSystemSlotDefaultSet::contested_slots` exists for exactly one
    /// caller — this path, naming the manifests whose author must settle the
    /// conflict. The shipped seed root really did contest a slot on every boot
    /// until `town_square` gave `page: /, slot: ambient` up; while the accessor
    /// had no caller the drop was completely silent and no test failed, so the
    /// pin belongs on the call site, not on the accessor. The seed bytes are
    /// pinned separately by
    /// `system_boot_admission::no_two_shipped_system_widgets_pin_the_same_slot_default`;
    /// this one keeps the warning wired for the conflict that seed cannot
    /// prevent — a future one. Bounded at `record` so the warning cannot drift
    /// after the point the set is handed away.
    #[::core::prelude::v1::test]
    fn boot_admission_names_the_packages_that_contest_a_slot_default() {
        let source = include_str!("apps_api.rs");
        let boot = source
            .split("pub async fn admit_system_packages_at_boot")
            .nth(1)
            .expect("boot owner")
            .split("async fn resolve_admitted_slot_defaults")
            .next()
            .expect("boot owner end");
        let recorded = boot
            .split("Ok(scope_defaults) =>")
            .nth(1)
            .expect("resolved defaults arm")
            .split("slot_defaults.record(")
            .next()
            .expect("resolved defaults arm end");
        assert!(recorded.contains("scope_defaults.contested_slots()"));
        assert!(recorded.contains("tracing::warn!"));
    }

    /// An owner can enable every boot-admitted system package.
    ///
    /// With the schema default (`enable_at_boot: false`), boot admission stops
    /// at `ready_for_review`: approval is what grants an app authority, and an
    /// ordinary background worker cannot be recorded as the grantor. The
    /// deployment may opt into the narrower trusted-system host path separately.
    ///
    /// So this is the real end-to-end claim: the deployment's packages are
    /// admitted with no human involvement, and one owner-authenticated approval
    /// each — the same call `POST /apps/installations/{id}/approve` makes —
    /// turns them on. Approval is durable, so it is once per deployment, not
    /// once per boot.
    #[actix_web::test]
    async fn an_owner_can_enable_every_boot_admitted_system_package() {
        let temp = canonical_tempdir();
        let seed = std::fs::canonicalize(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician_data_v3"),
        )
        .expect("repo seed root");
        let workspace = ArtifactV2Workspace::new(temp.path()).with_seed_root(&seed);
        let api = AppPlatformApi::new(workspace);

        let reports = api
            .admit_system_packages_at_boot(AppSystemPackageSettings::default())
            .await;
        let admission_failures = reports
            .iter()
            .flat_map(|summary| summary.report.failures())
            .map(|(package, error)| format!("{package}: {error}"))
            .collect::<Vec<_>>();
        assert!(
            admission_failures.is_empty(),
            "boot admission refused packages:\n  {}",
            admission_failures.join("\n  ")
        );

        // A live owner session, exactly what the approve route carries. The
        // shared `authenticated_scope` helper is pinned to a fixed epoch, so it
        // is not live at `Utc::now()`.
        let now = Utc::now();
        let owner =
            magician::magician_v2::apps::authority::AuthenticatedAppScope::from_verified_session(
                magician::magician_v2::apps::records::AppScope {
                    principal: AppReference::parse(DEFAULT_SCOPE_PRINCIPAL).expect("principal"),
                    workspace: AppReference::parse(DEFAULT_SCOPE_WORKSPACE).expect("workspace"),
                },
                magician::magician_v2::apps::models::AppScopeBindingRef::parse("scope_1")
                    .expect("scope binding"),
                AppReference::parse("actor:owner").expect("actor"),
                AppReference::parse("session:1").expect("session"),
                magician::magician_v2::apps::models::AppRevision::new(1).expect("revision"),
                now,
                now + ChronoDuration::minutes(30),
            )
            .expect("owner session");
        let mut enabled = Vec::new();
        for summary in &reports {
            for outcome in &summary.report.outcomes {
                let Ok(receipt) = &outcome.result else {
                    continue;
                };
                let review = api
                    .installation_review()
                    .review(&owner, &receipt.installation_id, Utc::now())
                    .await
                    .unwrap_or_else(|error| panic!("reviewing {}: {error}", outcome.package_dir));
                let request =
                    magician_apps::apps::installation_review::AppInstallationApproveRequest {
                        review_material_digest: Some(review.workflow_material_digest.clone()),
                        ..Default::default()
                    };
                let approved = api
                    .installation_review()
                    .approve(&owner, &receipt.installation_id, request, Utc::now())
                    .await
                    .unwrap_or_else(|error| panic!("approving {}: {error}", outcome.package_dir));
                assert_eq!(
                    approved.status,
                    magician_apps::apps::lifecycle::AppInstallationStatus::Enabled,
                    "{} should be enabled after approval",
                    outcome.package_dir
                );
                enabled.push(outcome.package_dir.clone());
            }
        }
        for expected in ["meetings", "town_square", "claims_review", "learning"] {
            assert!(
                enabled.contains(&expected.to_owned()),
                "`{expected}` was not enabled; got {enabled:?}"
            );
        }
    }

    /// The shipped opt-in must carry trusted seed bytes all the way through
    /// host approval, widget registration and default-slot maintenance without
    /// borrowing an interactive owner session for the mutation.
    #[actix_web::test]
    async fn configured_boot_enablement_surfaces_every_system_package_and_pins_defaults() {
        let temp = canonical_tempdir();
        let seed = std::fs::canonicalize(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician_data_v3"),
        )
        .expect("repo seed root");
        let workspace = ArtifactV2Workspace::new(temp.path()).with_seed_root(&seed);
        let api = AppPlatformApi::new(workspace);

        let reports = api
            .admit_system_packages_at_boot(AppSystemPackageSettings {
                admit_at_boot: true,
                enable_at_boot: true,
            })
            .await;
        assert_eq!(reports.len(), 1, "the fixture has only the default scope");
        let report = &reports[0];
        assert_eq!(report.report.admitted(), report.report.outcomes.len());
        assert!(
            report.enablement_failures.is_empty(),
            "host enablement failed: {:?}",
            report.enablement_failures
        );

        let now = Utc::now();
        let owner =
            magician::magician_v2::apps::authority::AuthenticatedAppScope::from_verified_session(
                magician::magician_v2::apps::records::AppScope {
                    principal: AppReference::parse(DEFAULT_SCOPE_PRINCIPAL).expect("principal"),
                    workspace: AppReference::parse(DEFAULT_SCOPE_WORKSPACE).expect("workspace"),
                },
                magician::magician_v2::apps::models::AppScopeBindingRef::parse("scope_1")
                    .expect("scope binding"),
                AppReference::parse("actor:owner").expect("actor"),
                AppReference::parse("session:boot-enablement-read").expect("session"),
                magician::magician_v2::apps::models::AppRevision::new(1).expect("revision"),
                now,
                now + ChronoDuration::minutes(30),
            )
            .expect("owner read scope");
        let enabled = api
            .registry
            .enabled_installations_bounded(&owner, 64, Utc::now())
            .await
            .expect("enabled installation inventory");
        assert_eq!(enabled.len(), report.report.admitted());

        let repeated = api
            .admit_system_packages_at_boot(AppSystemPackageSettings {
                admit_at_boot: true,
                enable_at_boot: true,
            })
            .await;
        assert!(repeated.iter().all(|summary| {
            summary.report.failures().next().is_none() && summary.enablement_failures.is_empty()
        }));

        let inventory = api
            .widget_runtime()
            .snapshot(&owner, Utc::now())
            .await
            .expect("live widget inventory");
        let visible_system_packages = inventory
            .packages
            .iter()
            .filter(|package| package.has_trusted_system_provenance())
            .count();
        assert_eq!(visible_system_packages, report.report.admitted());

        let slot_id =
            AppSlotId::for_page_region("/observe", &AppName::parse("capture").expect("region"))
                .expect("slot id");
        let resolved = api
            .slot_assignments
            .resolve_slot(owner.scope(), DEFAULT_SCOPE_PRINCIPAL, &slot_id, &inventory)
            .expect("the meetings slot resolves");
        assert!(resolved.pinned_system_default);
        assert!(resolved.widget.is_some());

        let installation_ids = report
            .report
            .outcomes
            .iter()
            .filter_map(|outcome| outcome.result.as_ref().ok())
            .map(|receipt| receipt.installation_id.clone())
            .collect::<Vec<_>>();
        assert!(installation_ids.len() >= 2);
        let host = report
            .report
            .host_grantor_scope(&installation_ids[0], Utc::now())
            .expect("host grantor for admitted package");
        let error = api
            .installation_review()
            .approve(
                &host,
                &installation_ids[1],
                AppInstallationApproveRequest::default(),
                Utc::now(),
            )
            .await
            .expect_err("a host grantor for one package cannot approve another");
        assert!(matches!(error, AppInstallationReviewError::InvalidGrant(_)));

        let meetings_id = report
            .report
            .outcomes
            .iter()
            .find(|outcome| outcome.package_dir == "meetings")
            .and_then(|outcome| outcome.result.as_ref().ok())
            .map(|receipt| receipt.installation_id.clone())
            .expect("meetings installation");
        let meetings = enabled
            .iter()
            .find(|installation| installation.installation_id == meetings_id)
            .expect("enabled meetings installation");
        api.registry
            .transition_installation(
                &owner,
                meetings_id.clone(),
                meetings.lifecycle.generation,
                AppInstallationCommand::Disable,
                AppReference::parse("event:disable-meetings").expect("event id"),
                AppDigest::blake3(b"disable-meetings"),
                Utc::now(),
            )
            .await
            .expect("owner disables meetings");
        let after_owner_disable = api
            .admit_system_packages_at_boot(AppSystemPackageSettings {
                admit_at_boot: true,
                enable_at_boot: true,
            })
            .await;
        assert!(after_owner_disable
            .iter()
            .all(|summary| summary.enablement_failures.is_empty()));
        let meetings = api
            .registry
            .installation(&owner, &meetings_id, Utc::now())
            .await
            .expect("read meetings after boot")
            .expect("meetings survives disable");
        assert_eq!(
            meetings.lifecycle.status,
            AppInstallationStatus::Disabled,
            "boot opt-in must not reverse an owner disable"
        );
        let inventory = api
            .widget_runtime()
            .snapshot(&owner, Utc::now())
            .await
            .expect("inventory after owner disable");
        let resolved = api
            .slot_assignments
            .resolve_slot(owner.scope(), DEFAULT_SCOPE_PRINCIPAL, &slot_id, &inventory)
            .expect("disabled meetings slot resolves as empty");
        assert!(!resolved.pinned_system_default);
        assert!(resolved.widget.is_none());
    }

    /// A boot that could not realize one of its own packages must not be
    /// allowed to mint a pinned-default set.
    ///
    /// Maintenance replaces `workspace_defaults` wholesale, so a set missing a
    /// package reads as "retire that package's slots" — which is the right
    /// reading only when the package left the seed root, and the wrong one
    /// when it simply lost a lock this boot. The completeness proof is what
    /// separates the two, so it is pinned here rather than trusted to the
    /// caller's ordering.
    #[::core::prelude::v1::test]
    fn an_unrealized_system_package_refuses_the_pinned_default_mint() {
        let none: Vec<(String, String)> = Vec::new();
        assert!(
            unrealized_system_packages(std::iter::empty::<&str>(), &none).is_empty(),
            "a boot with no failures is the only one that may mint a set"
        );

        let enablement = vec![("meetings".to_owned(), "approve: lock held".to_owned())];
        assert_eq!(
            unrealized_system_packages(std::iter::empty::<&str>(), &enablement),
            vec!["meetings"],
            "an enablement failure alone must refuse the mint"
        );
        assert_eq!(
            unrealized_system_packages(["learning"].into_iter(), &enablement),
            vec!["learning", "meetings"],
            "both failure lists count toward completeness"
        );
        assert_eq!(
            unrealized_system_packages(["meetings"].into_iter(), &enablement),
            vec!["meetings"],
            "a package named by both lists is still one unrealized package"
        );

        // The resolver cannot refuse what it was never told about, and an
        // empty slice compiles, so the boot owner's handover is pinned too.
        let source = include_str!("apps_api.rs");
        let boot = source
            .split("pub async fn admit_system_packages_at_boot")
            .nth(1)
            .expect("boot owner")
            .split("async fn resolve_admitted_slot_defaults")
            .next()
            .expect("boot owner end");
        assert!(boot.contains(
            ".resolve_admitted_slot_defaults(&authenticated, &report, &enablement_failures)"
        ));
    }

    /// Boot admission leaves every package inert.
    ///
    /// Proves the two acts really are separate — that seed provenance buys
    /// class and not power. This is the property that keeps a compromised seed
    /// root from being a running app, so it is asserted rather than assumed.
    #[actix_web::test]
    async fn admission_without_enablement_leaves_every_package_inert() {
        let temp = canonical_tempdir();
        let seed = std::fs::canonicalize(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician_data_v3"),
        )
        .expect("repo seed root");
        let workspace = ArtifactV2Workspace::new(temp.path()).with_seed_root(&seed);
        let api = AppPlatformApi::new(workspace);

        let reports = api
            .admit_system_packages_at_boot(AppSystemPackageSettings {
                admit_at_boot: true,
                enable_at_boot: false,
            })
            .await;
        assert!(!reports.is_empty(), "packages should still be admitted");

        let now = Utc::now();
        let authenticated = system_worker_scope_for_actor(
            DEFAULT_SCOPE_PRINCIPAL,
            DEFAULT_SCOPE_WORKSPACE,
            "worker:test",
            &AppReference::parse("run:test").expect("run ref"),
            now,
        )
        .expect("test scope");
        let installations = api
            .registry
            .enabled_installations_bounded(&authenticated, 64, now)
            .await
            .expect("enabled installations");
        assert!(
            installations.is_empty(),
            "admission alone must not enable anything, found {}",
            installations.len()
        );
    }

    /// A resolver that sees nothing: the shape a scope is left in when the
    /// deployment's system packages exist and are enabled and still never reach
    /// the picker, because widget registration failed inside the resolver and
    /// was swallowed there at debug level.
    #[derive(Debug)]
    struct BlindSlotInventory;

    #[async_trait::async_trait]
    impl AppSlotInventoryResolver for BlindSlotInventory {
        async fn snapshot(
            &self,
            _authenticated: &AuthenticatedAppScope,
            _now: DateTime<Utc>,
        ) -> Result<AppSlotInventorySnapshot, AppSlotInventoryError> {
            Ok(AppSlotInventorySnapshot::empty())
        }
    }

    /// A scope that can see none of the bundles this boot admitted must not be
    /// read as a deployment that pins nothing.
    ///
    /// `maintain_system_defaults` replaces the pinned set wholesale, so an `Ok`
    /// empty set is an instruction to retire every workspace default. The
    /// picker lists only enabled, currently registered installations, and a
    /// registration failure is swallowed inside the resolver — so this absence
    /// leaves no failure for the completeness check to refuse on, and used to
    /// blank every non-customized slot in the scope until some later boot
    /// happened to resolve one.
    #[actix_web::test]
    async fn a_scope_blind_to_every_admitted_bundle_refuses_to_retire_its_defaults() {
        let temp = canonical_tempdir();
        let seed = std::fs::canonicalize(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician_data_v3"),
        )
        .expect("repo seed root");
        let workspace = ArtifactV2Workspace::new(temp.path()).with_seed_root(&seed);
        let api = AppPlatformApi::new(workspace);

        let reports = api
            .admit_system_packages_at_boot(AppSystemPackageSettings {
                admit_at_boot: true,
                enable_at_boot: true,
            })
            .await;
        let summary = reports
            .iter()
            .find(|summary| {
                summary.scope_principal == DEFAULT_SCOPE_PRINCIPAL
                    && summary.scope_workspace == DEFAULT_SCOPE_WORKSPACE
            })
            .expect("default scope was boot-admitted");
        assert_eq!(summary.report.failures().count(), 0);
        assert!(summary.enablement_failures.is_empty());
        assert!(summary.report.admitted() > 0);

        // Replace the healthy inventory only after activation, reproducing a
        // registration failure rather than an intentionally inert scope.
        api.set_slot_inventory_resolver(Arc::new(BlindSlotInventory));

        let now = Utc::now();
        let authenticated = system_worker_scope_for_actor(
            DEFAULT_SCOPE_PRINCIPAL,
            DEFAULT_SCOPE_WORKSPACE,
            "worker:test",
            &AppReference::parse("run:test").expect("run ref"),
            now,
        )
        .expect("test scope");

        let error = api
            .resolve_admitted_slot_defaults(&authenticated, &summary.report, &[])
            .await
            .expect_err("a scope blind to every admitted bundle must refuse");
        assert!(
            error.contains("cannot account for"),
            "refused for the wrong reason: {error}"
        );
    }

    /// An empty inventory is not itself evidence of failure. When every
    /// admitted installation is still awaiting review, the registry accounts
    /// for every absence and the empty set is the truthful default set.
    #[actix_web::test]
    async fn an_inert_system_inventory_resolves_an_empty_default_set() {
        let temp = canonical_tempdir();
        let seed = std::fs::canonicalize(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician_data_v3"),
        )
        .expect("repo seed root");
        let workspace = ArtifactV2Workspace::new(temp.path()).with_seed_root(&seed);
        let api = AppPlatformApi::new(workspace);
        api.set_slot_inventory_resolver(Arc::new(BlindSlotInventory));

        let now = Utc::now();
        let authenticated = system_worker_scope_for_actor(
            DEFAULT_SCOPE_PRINCIPAL,
            DEFAULT_SCOPE_WORKSPACE,
            "worker:test",
            &AppReference::parse("run:test").expect("run ref"),
            now,
        )
        .expect("test scope");
        let report = admit_system_packages(
            &api.candidate_publications,
            &api.workspace,
            &authenticated,
            now,
        )
        .await
        .expect("the repo seed root resolves");
        assert_eq!(report.failures().count(), 0);
        assert!(report.admitted() > 0);

        api.resolve_admitted_slot_defaults(&authenticated, &report, &[])
            .await
            .expect("inert package absence is fully accounted for");

        let missing = report
            .outcomes
            .iter()
            .find_map(|outcome| outcome.result.as_ref().ok())
            .map(|receipt| receipt.installation_id.clone())
            .expect("one admitted installation");
        let missing_for_write = missing.clone();
        let write_at = Utc::now();
        api.registry
            .execute_scoped_test_write(&authenticated, &write_at, move |connection, _scope| {
                connection.execute(
                    "DELETE FROM app_lifecycle_attempts WHERE installation_id = ?1",
                    rusqlite::params![missing_for_write.as_str()],
                )?;
                connection.execute(
                    "DELETE FROM app_installations WHERE installation_id = ?1",
                    rusqlite::params![missing_for_write.as_str()],
                )?;
                Ok(())
            })
            .await
            .expect("remove one installation to model a partial registry");
        let error = api
            .resolve_admitted_slot_defaults(&authenticated, &report, &[])
            .await
            .expect_err("a missing installation cannot explain an absent widget");
        assert!(
            error.contains(&format!("{missing} (missing)")),
            "missing installation was not identified: {error}"
        );
    }

    /// A deployment that ships no system package also legitimately pins
    /// nothing; this is what retires a default whose package left the seed
    /// root entirely.
    #[actix_web::test]
    async fn a_deployment_that_ships_no_system_package_still_resolves_an_empty_set() {
        let temp = canonical_tempdir();
        // An explicit empty seed root rather than an absent one: leaving it
        // unset falls through to the process-wide default seed root, which
        // another test in this binary may have installed.
        let workspace =
            ArtifactV2Workspace::new(temp.path()).with_seed_root(temp.path().join("bare-seed"));
        let api = AppPlatformApi::new(workspace);
        api.set_slot_inventory_resolver(Arc::new(BlindSlotInventory));

        let now = Utc::now();
        let authenticated = system_worker_scope_for_actor(
            DEFAULT_SCOPE_PRINCIPAL,
            DEFAULT_SCOPE_WORKSPACE,
            "worker:test",
            &AppReference::parse("run:test").expect("run ref"),
            now,
        )
        .expect("test scope");
        let report = admit_system_packages(
            &api.candidate_publications,
            &api.workspace,
            &authenticated,
            now,
        )
        .await
        .expect("a missing seed root admits nothing rather than failing");
        assert_eq!(report.trusted_inventory().admitted_bundles(), 0);

        api.resolve_admitted_slot_defaults(&authenticated, &report, &[])
            .await
            .expect("a deployment that ships nothing may say that it pins nothing");
    }

    /// A resolver that sees every trusted-system package but one.
    ///
    /// Not a fabricated snapshot: it delegates to the real widget runtime and
    /// drops one installation, which is precisely what a swallowed widget
    /// registration leaves behind — the package is admitted, approved and
    /// enabled, and the only trace of its absence is a debug line inside
    /// `reconcile_scope_registrations`.
    struct HalfBlindSlotInventory {
        inner: AppWidgetRuntime,
        hidden: AppInstallationId,
    }

    #[async_trait::async_trait]
    impl AppSlotInventoryResolver for HalfBlindSlotInventory {
        async fn snapshot(
            &self,
            authenticated: &AuthenticatedAppScope,
            now: DateTime<Utc>,
        ) -> Result<AppSlotInventorySnapshot, AppSlotInventoryError> {
            let mut snapshot = self.inner.snapshot(authenticated, now).await?;
            snapshot
                .packages
                .retain(|package| package.binding.installation_id != self.hidden);
            // The picker must lose the widget with it, or the snapshot fails
            // its own validation: a candidate with no package state is refused.
            snapshot
                .picker
                .retain(|candidate| candidate.widget.package.installation_id != self.hidden);
            Ok(snapshot)
        }
    }

    /// A snapshot that omits one admitted system package must not retire that
    /// package's pinned slots.
    ///
    /// `maintain_system_defaults` replaces `workspace_defaults` wholesale, so a
    /// set minted from a partial snapshot is an instruction to delete the
    /// omitted package's pins for every user in the scope who had not
    /// customized them — counted as ordinary `retired_slots` maintenance, with
    /// nothing in the log to say a package went missing. The all-blind floor
    /// cannot catch this: it is satisfied the moment *one* package is visible.
    #[actix_web::test]
    async fn a_snapshot_missing_one_admitted_package_refuses_to_retire_its_defaults() {
        let temp = canonical_tempdir();
        let seed = std::fs::canonicalize(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician_data_v3"),
        )
        .expect("repo seed root");
        let workspace = ArtifactV2Workspace::new(temp.path()).with_seed_root(&seed);
        let api = AppPlatformApi::new(workspace);

        let reports = api
            .admit_system_packages_at_boot(AppSystemPackageSettings::default())
            .await;
        let installations = reports
            .iter()
            .flat_map(|summary| summary.report.outcomes.iter())
            .filter_map(|outcome| outcome.result.as_ref().ok())
            .map(|receipt| receipt.installation_id.clone())
            .collect::<Vec<_>>();
        assert!(
            installations.len() >= 2,
            "a partial snapshot needs more than one admitted package, got {}",
            installations.len()
        );

        // A live owner session, exactly what the approve route carries.
        let now = Utc::now();
        let owner =
            magician::magician_v2::apps::authority::AuthenticatedAppScope::from_verified_session(
                magician::magician_v2::apps::records::AppScope {
                    principal: AppReference::parse(DEFAULT_SCOPE_PRINCIPAL).expect("principal"),
                    workspace: AppReference::parse(DEFAULT_SCOPE_WORKSPACE).expect("workspace"),
                },
                magician::magician_v2::apps::models::AppScopeBindingRef::parse("scope_1")
                    .expect("scope binding"),
                AppReference::parse("actor:owner").expect("actor"),
                AppReference::parse("session:1").expect("session"),
                magician::magician_v2::apps::models::AppRevision::new(1).expect("revision"),
                now,
                now + ChronoDuration::minutes(30),
            )
            .expect("owner session");

        for installation_id in &installations {
            let review = api
                .installation_review()
                .review(&owner, installation_id, Utc::now())
                .await
                .unwrap_or_else(|error| panic!("reviewing {installation_id}: {error}"));
            let request = magician_apps::apps::installation_review::AppInstallationApproveRequest {
                review_material_digest: Some(review.workflow_material_digest.clone()),
                ..Default::default()
            };
            api.installation_review()
                .approve(&owner, installation_id, request, Utc::now())
                .await
                .unwrap_or_else(|error| panic!("approving {installation_id}: {error}"));
        }

        // The healthy state the completeness proof is measured against: an
        // approved system package reaches the picker. Asserted rather than
        // assumed, because if it were false the guard below would freeze every
        // deployment's defaults instead of protecting them.
        let inventory = api
            .widget_runtime()
            .snapshot(&owner, Utc::now())
            .await
            .expect("live widget inventory");
        let visible = inventory
            .packages
            .iter()
            .filter(|package| package.has_trusted_system_provenance())
            .map(|package| package.binding.installation_id.clone())
            .collect::<BTreeSet<_>>();
        for installation_id in &installations {
            assert!(
                visible.contains(installation_id),
                "{installation_id} is approved and enabled and still absent from the picker"
            );
        }

        // Hide exactly one, then re-pin off an installation that is still
        // visible — the shape of an owner re-approving one package while
        // another silently drops out of the picker.
        api.set_slot_inventory_resolver(Arc::new(HalfBlindSlotInventory {
            inner: api.widget_runtime(),
            hidden: installations[1].clone(),
        }));
        assert!(
            api.refresh_system_slot_defaults(&owner, &installations[0])
                .await
                .is_none(),
            "a snapshot missing an enabled admitted package must refuse the mint rather than \
             retire that package's pinned slots for every user in the scope"
        );
    }

    /// An owner approval pins the deployment's system slot defaults, with no
    /// restart and no second boot.
    ///
    /// This remains the interactive path for deployments that leave
    /// `enable_at_boot` off. Admission publishes inert `ready_for_review`
    /// installations, and an ordinary background worker cannot be the grantor;
    /// one later owner approval must therefore refresh the defaults immediately
    /// rather than waiting for another boot.
    ///
    /// The assertion is deliberately what a user resolves, not what maintenance
    /// returned: a pinned default nobody can render is not a surface.
    #[actix_web::test]
    async fn an_owner_approval_pins_the_system_slot_defaults_without_a_restart() {
        let temp = canonical_tempdir();
        let seed = std::fs::canonicalize(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician_data_v3"),
        )
        .expect("repo seed root");
        let workspace = ArtifactV2Workspace::new(temp.path()).with_seed_root(&seed);
        let api = AppPlatformApi::new(workspace);

        let reports = api
            .admit_system_packages_at_boot(AppSystemPackageSettings::default())
            .await;
        let admission_failures = reports
            .iter()
            .flat_map(|summary| summary.report.failures())
            .map(|(package, error)| format!("{package}: {error}"))
            .collect::<Vec<_>>();
        assert!(
            admission_failures.is_empty(),
            "boot admission refused packages:\n  {}",
            admission_failures.join("\n  ")
        );
        let installations = reports
            .iter()
            .flat_map(|summary| summary.report.outcomes.iter())
            .filter_map(|outcome| outcome.result.as_ref().ok())
            .map(|receipt| receipt.installation_id.clone())
            .collect::<Vec<_>>();
        assert!(
            installations.len() >= 4,
            "expected the seeded system packages to be admitted, got {}",
            installations.len()
        );

        // A live owner session, exactly what the approve route carries.
        let now = Utc::now();
        let owner =
            magician::magician_v2::apps::authority::AuthenticatedAppScope::from_verified_session(
                magician::magician_v2::apps::records::AppScope {
                    principal: AppReference::parse(DEFAULT_SCOPE_PRINCIPAL).expect("principal"),
                    workspace: AppReference::parse(DEFAULT_SCOPE_WORKSPACE).expect("workspace"),
                },
                magician::magician_v2::apps::models::AppScopeBindingRef::parse("scope_1")
                    .expect("scope binding"),
                AppReference::parse("actor:owner").expect("actor"),
                AppReference::parse("session:1").expect("session"),
                magician::magician_v2::apps::models::AppRevision::new(1).expect("revision"),
                now,
                now + ChronoDuration::minutes(30),
            )
            .expect("owner session");

        // Before any approval the scope is blind to every admitted bundle, so
        // the refresh must refuse rather than write an empty set over it.
        assert!(
            api.refresh_system_slot_defaults(&owner, &installations[0])
                .await
                .is_none(),
            "an approval-free deployment must not mint a pinned default set"
        );

        let mut maintained = None;
        for installation_id in &installations {
            let review = api
                .installation_review()
                .review(&owner, installation_id, Utc::now())
                .await
                .unwrap_or_else(|error| panic!("reviewing {installation_id}: {error}"));
            let request = magician_apps::apps::installation_review::AppInstallationApproveRequest {
                review_material_digest: Some(review.workflow_material_digest.clone()),
                ..Default::default()
            };
            api.installation_review()
                .approve(&owner, installation_id, request, Utc::now())
                .await
                .unwrap_or_else(|error| panic!("approving {installation_id}: {error}"));
            if let Some(outcome) = api
                .refresh_system_slot_defaults(&owner, installation_id)
                .await
            {
                maintained = Some(outcome);
            }
        }
        let maintained = maintained.expect("an owner approval pins the admitted defaults");
        assert!(
            maintained.pinned_slots > 0,
            "the seed root pins slots no other seed package contests"
        );

        let scope = magician::magician_v2::apps::records::AppScope {
            principal: AppReference::parse(DEFAULT_SCOPE_PRINCIPAL).expect("principal"),
            workspace: AppReference::parse(DEFAULT_SCOPE_WORKSPACE).expect("workspace"),
        };
        let widget_runtime = api.widget_runtime();
        let inventory = widget_runtime
            .snapshot(&owner, Utc::now())
            .await
            .expect("live widget inventory");
        // `meetings` alone claims this one; `page: /, slot: ambient` is
        // contested by `town_square` and is pinned to nobody by design.
        let slot_id =
            AppSlotId::for_page_region("/observe", &AppName::parse("capture").expect("region"))
                .expect("slot id");
        let resolved = api
            .slot_assignments
            .resolve_slot(&scope, DEFAULT_SCOPE_PRINCIPAL, &slot_id, &inventory)
            .expect("the slot resolves");
        assert!(
            resolved.pinned_system_default,
            "the deployment's own widget must reach the slot it pins"
        );
        assert!(
            resolved.widget.is_some(),
            "a pinned default nobody can render is not a surface: {:?}",
            resolved.hidden_reason
        );

        // And the other side of the gate: approving something this boot never
        // admitted moves nothing, whatever its own manifest claims to be.
        let stranger = AppInstallationId::parse("install_stranger").expect("installation id");
        assert!(
            api.refresh_system_slot_defaults(&owner, &stranger)
                .await
                .is_none(),
            "only a package this deployment admitted may re-pin its system defaults"
        );
    }

    /// The approve route stays the maintainer's second caller.
    ///
    /// The failure this pins is silent by construction: the refresh returning
    /// `None` is the ordinary answer for every non-system approval, so a route
    /// that stopped calling it at all would keep passing every test that does
    /// not read the route itself — which is exactly how the maintainer came to
    /// have only a boot-time caller. Bounded to the success arm, because a
    /// refused approval must pin nothing.
    #[::core::prelude::v1::test]
    fn the_approve_route_repins_the_system_slot_defaults() {
        let source = include_str!("apps_api.rs");
        let handler = source
            .split("async fn approve_app_installation_handler")
            .nth(1)
            .expect("approve handler")
            .split("async fn ")
            .next()
            .expect("approve handler end");
        let approved = handler
            .split("Ok(receipt) =>")
            .nth(1)
            .expect("approved arm")
            .split("Err(error) =>")
            .next()
            .expect("approved arm end");
        assert!(
            approved.contains("api.refresh_system_slot_defaults(&authenticated, &installation_id)"),
            "an approval that does not re-pin leaves the deployment's own \
             widgets unsurfaced until the process restarts"
        );
    }

    #[actix_web::test]
    async fn contract_capabilities_require_a_fresh_verified_session_and_match_inventory() {
        let anonymous = test::init_service(App::new().configure(configure_app_routes)).await;
        let request = test::TestRequest::get()
            .uri("/apps/contract-capabilities")
            .to_request();
        let response = test::call_service(&anonymous, request).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let error: AppErrorEnvelope = test::read_body_json(response).await;
        assert_eq!(error.code, AppErrorCode::NotAuthorized);
        assert_eq!(error.disposition, AppErrorDisposition::Reauthorize);
        assert_eq!(
            error.details[&AppName::parse("operation_id").unwrap()],
            "contract_capabilities"
        );
        assert_eq!(
            error.details[&AppName::parse("reason").unwrap()],
            "authenticated_app_session_required"
        );

        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let authenticated = test::init_service(
            App::new()
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let request = test::TestRequest::get()
            .uri("/apps/contract-capabilities")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let response = test::call_service(&authenticated, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(actix_web::http::header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("private, no-store")
        );
        let capabilities: AppContractCapabilities = test::read_body_json(response).await;
        assert!(capabilities.validate_static_inventory());
        assert_eq!(
            capabilities.operations.len(),
            magician_app_contract::SUPPORTED_PUBLIC_APP_OPERATIONS.len()
        );
        assert!(!capabilities.sdk_compatibility.generated_by_is_authority);
        assert!(capabilities
            .operations
            .iter()
            .all(|operation| !operation.path.contains("custom-surface")));
    }

    #[actix_web::test]
    async fn macos_pairing_control_is_authenticated_and_rejects_hostname_rebinding() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let anonymous = test::init_service(
            App::new()
                .app_data(web::Data::new(AppPlatformApi::new(workspace.clone())))
                .configure(configure_app_routes),
        )
        .await;
        let request = test::TestRequest::get()
            .uri("/apps/macos-pairing")
            .to_request();
        let response = test::call_service(&anonymous, request).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let authenticated = test::init_service(
            App::new()
                .app_data(web::Data::new(AppPlatformApi::new(workspace)))
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let request = test::TestRequest::post()
            .uri("/apps/macos-pairing/setup")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .insert_header((actix_web::http::header::CONTENT_TYPE, "application/json"))
            .set_payload(
                serde_json::to_vec(&json!({
                    "action_url": "http://localhost:3017/host/apps/macos/action",
                    "bundle_id": "com.example.Editor"
                }))
                .unwrap(),
            )
            .to_request();
        let response = test::call_service(&authenticated, request).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn every_supported_public_inventory_entry_is_registered_with_its_method() {
        let app = test::init_service(App::new().configure(configure_app_routes)).await;
        for operation in magician_app_contract::SUPPORTED_PUBLIC_APP_OPERATIONS {
            let uri = format!("/apps{}", operation.path)
                .replace("{installation_id}", "install_contract_probe")
                .replace("{action_id}", "action_probe")
                .replace("{run_ref}", "run:contract-probe");
            let method = match operation.method {
                AppHttpMethod::Get => actix_web::http::Method::GET,
                AppHttpMethod::Post => actix_web::http::Method::POST,
            };
            let request = test::TestRequest::default()
                .method(method)
                .uri(&uri)
                .to_request();
            let response = test::call_service(&app, request).await;
            assert_ne!(response.status(), StatusCode::NOT_FOUND, "{uri}");
            assert_ne!(response.status(), StatusCode::METHOD_NOT_ALLOWED, "{uri}");
        }
    }

    #[actix_web::test]
    async fn slot_settings_routes_are_registered_with_closed_methods() {
        let app = test::init_service(App::new().configure(configure_app_routes)).await;
        for (method, uri) in [
            (actix_web::http::Method::GET, "/apps/slots/page:2f:review"),
            (actix_web::http::Method::POST, "/apps/slots/resolve-batch"),
            (actix_web::http::Method::GET, "/apps/slot-assignments"),
            (actix_web::http::Method::POST, "/apps/slot-assignments"),
        ] {
            let response = test::call_service(
                &app,
                test::TestRequest::default()
                    .method(method)
                    .uri(uri)
                    .to_request(),
            )
            .await;
            assert_ne!(response.status(), StatusCode::NOT_FOUND, "{uri}");
            assert_ne!(response.status(), StatusCode::METHOD_NOT_ALLOWED, "{uri}");
        }
    }

    #[actix_web::test]
    async fn supported_public_run_locator_rejects_legacy_bare_task_ids_as_json() {
        let response = supported_public_run_locator("task_app_legacy_locator".to_owned())
            .expect_err("the public adapter must not accept a bare Artifact task id");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response
                .headers()
                .get(actix_web::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"], "invalid_app_contract");

        assert!(supported_public_run_locator(
            "run:app-action:task_app_canonical_locator".to_owned()
        )
        .is_ok());
    }

    #[actix_web::test]
    async fn entity_change_query_extractor_failures_use_the_public_json_error() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = web::Data::new(AppPlatformApi::new(workspace));
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(api)
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;

        for uri in [
            "/apps/installations/install_contract_probe/entity-changes",
            "/apps/installations/install_contract_probe/entity-changes?surface_revision=abc",
            "/apps/installations/install_contract_probe/entity-changes?surface_revision=1&invented=true",
        ] {
            let request = test::TestRequest::get()
                .uri(uri)
                .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
                .to_request();
            let response = test::call_service(&app, request).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(
                response
                    .headers()
                    .get(actix_web::http::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok()),
                Some("application/json"),
                "{uri}"
            );
            let body: serde_json::Value = test::read_body_json(response).await;
            assert_eq!(body["code"], "invalid_request", "{uri}");
            assert_eq!(body["disposition"], "terminal", "{uri}");
            assert_eq!(body["details"]["operation_id"], "read_entity_changes", "{uri}");
            assert_eq!(body["details"]["reason"], "invalid_app_contract", "{uri}");
            assert!(body.get("error").is_none(), "{uri}");
        }
    }

    #[actix_web::test]
    async fn supported_public_error_mapping_is_operation_aware_and_preserves_retry_delay() {
        for operation in magician_app_contract::SUPPORTED_PUBLIC_APP_OPERATIONS {
            let response = canonicalize_supported_public_response(
                operation.id,
                api_error(
                    StatusCode::UNAUTHORIZED,
                    "authenticated_app_session_required",
                    "A verified app session is required.",
                ),
            )
            .await;
            let envelope: AppErrorEnvelope = read_http_response_json(response).await;
            assert_eq!(envelope.code, AppErrorCode::NotAuthorized);
            assert_eq!(envelope.disposition, AppErrorDisposition::Reauthorize);
            assert_eq!(
                envelope.details[&AppName::parse("operation_id").unwrap()],
                operation.id.as_str()
            );
            assert_eq!(
                envelope.details[&AppName::parse("reason").unwrap()],
                "authenticated_app_session_required"
            );
        }

        assert_eq!(
            supported_public_error_semantics(
                AppPublicOperationId::QueryData,
                StatusCode::BAD_REQUEST,
                Some("app_workspace_required"),
            ),
            (
                AppErrorCode::NotAuthorized,
                AppErrorDisposition::UserActionRequired,
            )
        );
        let mut legacy = api_error(
            StatusCode::TOO_MANY_REQUESTS,
            "app_data_overloaded",
            "The bounded app data lane is busy; retry later.",
        );
        legacy.headers_mut().insert(
            actix_web::http::header::RETRY_AFTER,
            actix_web::http::header::HeaderValue::from_static("2"),
        );
        let response =
            canonicalize_supported_public_response(AppPublicOperationId::MutateData, legacy).await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let envelope: AppErrorEnvelope = read_http_response_json(response).await;
        assert_eq!(envelope.code, AppErrorCode::RateLimited);
        assert_eq!(envelope.disposition, AppErrorDisposition::RetrySameInput);
        assert_eq!(envelope.retry_after_ms, Some(2_000));
        assert_eq!(
            envelope.details[&AppName::parse("operation_id").unwrap()],
            "mutate_data"
        );
        assert_eq!(
            envelope.details[&AppName::parse("reason").unwrap()],
            "app_data_overloaded"
        );

        let response = canonicalize_supported_public_response(
            AppPublicOperationId::ComposeActionRun,
            api_error(
                StatusCode::CONFLICT,
                "app_action_composition_unavailable",
                "The reviewed destination changed.",
            ),
        )
        .await;
        let envelope: AppErrorEnvelope = read_http_response_json(response).await;
        assert_eq!(envelope.code, AppErrorCode::StaleRevision);
        assert_eq!(envelope.disposition, AppErrorDisposition::RefreshAndRetry);

        let response = canonicalize_supported_public_response(
            AppPublicOperationId::QueryData,
            api_error(
                StatusCode::CONFLICT,
                "app_workflow_stale",
                "A reason from another operation must not cross the boundary.",
            ),
        )
        .await;
        let envelope: AppErrorEnvelope = read_http_response_json(response).await;
        assert_eq!(envelope.code, AppErrorCode::Conflict);
        assert_eq!(envelope.disposition, AppErrorDisposition::Terminal);
        assert!(!envelope
            .details
            .contains_key(&AppName::parse("reason").unwrap()));
    }

    #[actix_web::test]
    async fn authoring_catalog_lists_compiled_tools_and_default_agent() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = web::Data::new(AppPlatformApi::new(workspace));
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(api)
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let request = test::TestRequest::get()
            .uri("/apps/authoring/tools?app_eligible=true&kind=compiled")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["status"], "ok");
        let names: Vec<&str> = body["items"]
            .as_array()
            .expect("items")
            .iter()
            .filter_map(|item| item["name"].as_str())
            .collect();
        assert!(names.contains(&"content_read"));
        assert!(names.contains(&"http") || names.contains(&"files"));

        let shown = test::TestRequest::get()
            .uri("/apps/authoring/tools/content_read")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let shown = test::call_service(&app, shown).await;
        assert_eq!(shown.status(), StatusCode::OK);
        let shown: serde_json::Value = test::read_body_json(shown).await;
        assert_eq!(shown["name"], "content_read");
        assert_eq!(shown["app_eligible"], true);
        assert_eq!(shown["lock_review_required"], false);

        let agents = test::TestRequest::get()
            .uri("/apps/authoring/agents")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let agents = test::call_service(&app, agents).await;
        assert_eq!(agents.status(), StatusCode::OK);
        let agents: serde_json::Value = test::read_body_json(agents).await;
        let agent_names: Vec<&str> = agents["items"]
            .as_array()
            .expect("agents")
            .iter()
            .filter_map(|item| item["name"].as_str())
            .collect();
        assert!(agent_names.contains(&"personal-assistant"));
    }

    /// Reachability invariant (app-authority remediation move 1).
    ///
    /// The dispatch-time fences fail closed on a disabled/quarantined/revoked
    /// installation, but before this move NO production caller could SET those
    /// states — `transition_installation` / `revoke_active_grant` had only test
    /// callers, so the deny-states were unreachable and the "owner control"
    /// promise ended at the approve button (defect C3).
    ///
    /// This test asserts every owner kill-switch route is wired to its handler:
    /// each POST for a nonexistent installation must reach the handler and return
    /// the handler's OWN `app_installation_not_found` code. An unregistered route
    /// would instead yield actix's bare 404 with no such body — so deleting or
    /// forgetting to wire any of these routes fails this test. The kernel tests in
    /// `registry_lifecycle` separately prove the handler's call actually sets the
    /// deny-state; together they prove the state is reachable from production.
    #[actix_web::test]
    async fn owner_kill_switch_routes_are_wired_to_their_handlers() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = web::Data::new(AppPlatformApi::new(workspace));
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(api)
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;

        for route in [
            "/apps/installations/install_missing/disable",
            "/apps/installations/install_missing/quarantine",
            "/apps/installations/install_missing/uninstall",
            "/apps/installations/install_missing/grant-revocations",
            // `update-begin` parks an installation; `update-abort` is what gets
            // it back. Both are asserted here so the parked state can never
            // become a place an owner can enter and not leave.
            "/apps/installations/install_missing/update-begin",
            "/apps/installations/install_missing/update-abort",
        ] {
            let request = test::TestRequest::post()
                .uri(route)
                .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
                .set_json(json!({
                    "expected_generation": 1,
                    "request_id": "lifecycle-request:missing-installation-route",
                }))
                .to_request();
            let response = test::call_service(&app, request).await;
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{route} should reach the handler and report the missing installation"
            );
            let body: serde_json::Value = test::read_body_json(response).await;
            assert_eq!(
                body["error"], "app_installation_not_found",
                "{route} is not wired to its owner kill-switch handler (a fail-closed \
                 deny-state with no production caller — defect C3); body={body}"
            );
        }

        let purge_status = test::TestRequest::get()
            .uri(&format!("/apps/purges/blake3:{}", "a".repeat(64)))
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let purge_status = test::call_service(&app, purge_status).await;
        assert_eq!(purge_status.status(), StatusCode::NOT_FOUND);
        let body: serde_json::Value = test::read_body_json(purge_status).await;
        assert_eq!(body["error"], "app_purge_receipt_not_found");
    }

    #[actix_web::test]
    async fn grant_revocation_control_replays_exactly_and_rejects_stale_substitution() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = AppPlatformApi::new(workspace);
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(
            &api.registry,
            digest,
            schema,
            AppInstallationStatus::Enabled,
        )
        .await;
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(api))
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;

        let detail = test::TestRequest::get()
            .uri("/apps/installations/install_1")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let detail = test::call_service(&app, detail).await;
        let detail: serde_json::Value = test::read_body_json(detail).await;
        let generation = detail["lifecycle"]["generation"]
            .as_u64()
            .expect("installation generation");
        let body = json!({
            "expected_generation": generation,
            "request_id": "lifecycle-request:grant-revoke-test",
        });
        let invoke = || {
            test::TestRequest::post()
                .uri("/apps/installations/install_1/grant-revocations")
                .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
                .set_json(&body)
                .to_request()
        };
        let first = test::call_service(&app, invoke()).await;
        let first_status = first.status();
        let first: serde_json::Value = test::read_body_json(first).await;
        assert_eq!(first_status, StatusCode::OK, "unexpected body: {first}");
        assert_eq!(first["generation"], generation + 1);
        assert_eq!(first["status"], "quarantined");

        let replay = test::call_service(&app, invoke()).await;
        assert_eq!(replay.status(), StatusCode::OK);
        let replay: serde_json::Value = test::read_body_json(replay).await;
        assert_eq!(replay, first);

        let stale = test::TestRequest::post()
            .uri("/apps/installations/install_1/grant-revocations")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .set_json(json!({
                "expected_generation": generation,
                "request_id": "lifecycle-request:substitution",
            }))
            .to_request();
        assert_eq!(
            test::call_service(&app, stale).await.status(),
            StatusCode::CONFLICT
        );
    }

    #[actix_web::test]
    async fn app_run_projection_keeps_resumable_states_nonterminal() {
        for status in [
            "pending",
            "ready",
            "planning",
            "running",
            "paused",
            "deferred",
            "waiting_for_user",
            "blocked",
            "cancelling",
        ] {
            let projected = app_run_status_from_task(status).expect("known app task state");
            assert!(!projected.is_terminal(), "{status}");
        }
        for status in [
            "completed",
            "failed",
            "cancelled",
            "canceled",
            "archived",
            "uncertain",
        ] {
            let projected = app_run_status_from_task(status).expect("known app task state");
            assert!(projected.is_terminal(), "{status}");
        }
        assert!(app_run_status_from_task("invented_state").is_none());
    }

    #[actix_web::test]
    async fn completed_run_status_requires_result_or_explicit_policy_withholding() {
        let run_handle = AppRunHandle {
            protocol_version: AppProtocolVersion::V1,
            run_ref: AppReference::parse(format!("run:app-action:task_app_{}", "a".repeat(64)))
                .unwrap(),
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            action_id: AppName::parse("condense").unwrap(),
        };
        assert!(app_run_snapshot(
            run_handle.clone(),
            None,
            AppRunStatus::Completed,
            None,
            false,
            None,
        )
        .is_err());
        let withheld =
            app_run_snapshot(run_handle, None, AppRunStatus::Completed, None, true, None).unwrap();
        assert!(withheld.terminal);
        assert!(withheld.result_withheld);
        assert!(withheld.result.is_none());
    }

    #[actix_web::test]
    async fn app_memory_review_cursor_is_exactly_scope_and_filter_bound() {
        let cursor = AppMemoryCandidateListCursor {
            version: 1,
            scope_binding_ref: AppScopeBindingRef::parse("scope_review_1").unwrap(),
            authentication_revision: AppRevision::new(3).unwrap(),
            status: Some(AppMemoryCandidateStatus::Accepted),
            updated_at: time(7),
            candidate_id: AppReference::parse("memory:candidate:7").unwrap(),
        };
        let encoded = encode_app_memory_candidate_cursor(&cursor).unwrap();
        let decoded = decode_app_memory_candidate_cursor(&encoded).unwrap();
        assert_eq!(decoded.scope_binding_ref, cursor.scope_binding_ref);
        assert_eq!(
            decoded.authentication_revision,
            cursor.authentication_revision
        );
        assert_eq!(decoded.status, cursor.status);
        assert_eq!(decoded.updated_at, cursor.updated_at);
        assert_eq!(decoded.candidate_id, cursor.candidate_id);

        let mut bytes = URL_SAFE_NO_PAD.decode(&encoded).unwrap();
        let mut forged: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        forged["version"] = json!(2);
        bytes = serde_json::to_vec(&forged).unwrap();
        assert!(decode_app_memory_candidate_cursor(&URL_SAFE_NO_PAD.encode(bytes)).is_err());
    }

    fn data_query(installation_id: &str) -> AppQueryRequest {
        AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: AppInstallationId::parse(installation_id).unwrap(),
            entity: AppName::parse("item").unwrap(),
            select: vec![
                AppFieldPath::parse("title").unwrap(),
                AppFieldPath::parse("status").unwrap(),
            ],
            predicate: None,
            order: vec![AppQueryOrder {
                field: AppFieldPath::parse("status").unwrap(),
                direction: AppOrderDirection::Ascending,
            }],
            cursor: None,
            limit: 100,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("owner_http_query").unwrap(),
        }
    }

    #[actix_web::test]
    async fn projection_worker_scope_is_exact_and_delivery_backoff_is_bounded() {
        let run_ref = AppReference::parse("run:projection-test").unwrap();
        let now = time(1);
        let scope = system_worker_scope("anonymous", "default", &run_ref, now).unwrap();
        assert_eq!(scope.scope().principal.as_str(), "anonymous");
        assert_eq!(scope.scope().workspace.as_str(), "default");
        assert_eq!(
            scope.authentication(),
            magician::magician_v2::apps::authority::AppScopeAuthentication::SystemWorker
        );
        assert_eq!(retry_delay(1), StdDuration::from_secs(1));
        assert_eq!(retry_delay(u32::MAX), StdDuration::from_secs(60));
    }

    #[actix_web::test]
    async fn cursor_ahead_conflict_tells_surface_clients_to_reload() {
        let response = entity_change_error_response(AppEntityChangeError::CursorAheadOfHead {
            after_change_sequence: 9,
            current_change_sequence: 4,
        });
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"], "app_change_cursor_ahead");
        assert!(body["message"].as_str().unwrap().contains("reload"));
    }

    #[actix_web::test]
    async fn workflow_internal_errors_do_not_expose_runtime_details() {
        let response = workflow_error_response(AppWorkflowError::WorkerTerminated(
            "/private/operator/path: provider credential failed".to_owned(),
        ));
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"], "app_workflow_failed");
        assert_eq!(
            body["message"],
            "The app workflow could not be completed safely."
        );
        assert!(!body.to_string().contains("/private/operator/path"));

        let missing = workflow_error_response(AppWorkflowError::MissingAction);
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let terminal = workflow_error_response(AppWorkflowError::InteractiveSessionTerminal);
        assert_eq!(terminal.status(), StatusCode::CONFLICT);
        let invalid = workflow_error_response(AppWorkflowError::InteractiveStopRequestInvalid);
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        let raced = workflow_error_response(AppWorkflowError::Registry(
            AppRegistryError::CompareAndSwapLost("protected workflow run-state head"),
        ));
        assert_eq!(raced.status(), StatusCode::CONFLICT);
        let body = actix_web::body::to_bytes(raced.into_body()).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"], "app_interactive_stop_conflict");
    }

    #[actix_web::test]
    async fn stale_widget_installation_precondition_requests_a_refresh() {
        let response =
            workflow_error_response(AppWorkflowError::StaleInvocation("installation_binding"));
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"], "app_workflow_stale");
    }

    async fn rewrite_installation_status(
        registry: &AppRegistryService,
        status: AppInstallationStatus,
        at: chrono::DateTime<Utc>,
    ) {
        let authority_time = at.clone();
        registry
            .execute_scoped_test_write(
                &authenticated_scope("anonymous", "default"),
                &authority_time,
                move |connection, _| {
                    let bytes: Vec<u8> = connection.query_row(
                        "SELECT record_json FROM app_installations WHERE installation_id = 'install_1'",
                        [],
                        |row| row.get(0),
                    )?;
                    let mut installation: AppInstallation = decode_app_contract(
                        &bytes,
                        &AppContractLimits::default(),
                    )
                    .expect("seeded installation must remain valid");
                    installation.lifecycle.status = status;
                    installation.lifecycle.generation = installation
                        .lifecycle
                        .generation
                        .checked_add(1)
                        .expect("test generation must not overflow");
                    installation.updated_at = at.clone();
                    match status {
                        AppInstallationStatus::UninstalledRetained => {
                            installation.uninstalled_at = Some(at.clone());
                        },
                        AppInstallationStatus::Purged => {
                            installation.purged_at = Some(at.clone());
                        },
                        _ => {},
                    }
                    installation
                        .validate_app_contract(&AppContractLimits::default())
                        .expect("rewritten installation must satisfy the lifecycle contract");
                    connection.execute(
                        "UPDATE app_installations
                            SET lifecycle_status = ?1, lifecycle_generation = ?2,
                                record_json = ?3, updated_at = ?4
                          WHERE installation_id = ?5",
                        rusqlite::params![
                            match status {
                                AppInstallationStatus::UninstalledRetained => "uninstalled_retained",
                                AppInstallationStatus::Purged => "purged",
                                _ => panic!("test helper supports only retained and purged states"),
                            },
                            i64::try_from(installation.lifecycle.generation)
                                .expect("test generation fits SQLite"),
                            serde_json::to_vec(&installation)
                                .expect("installation serialization succeeds"),
                            at.to_rfc3339(),
                            installation.installation_id.as_str(),
                        ],
                    )?;
                    Ok(())
                },
            )
            .await
            .expect("installation status rewrite succeeds");
    }

    fn data_mutation() -> AppMutationCommand {
        AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: AppReference::parse("mutation:http-owner-create").unwrap(),
            atomicity: AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: AppRevision::new(1).unwrap(),
            operations: vec![AppMutationOperation::Create {
                entity: AppName::parse("item").unwrap(),
                temporary_id: AppName::parse("http_owner_create").unwrap(),
                record_id: None,
                payload: serde_json::json!({"title": "Created over HTTP", "status": "open"}),
            }],
            expected_record_revisions: Vec::new(),
        }
    }

    #[actix_web::test]
    async fn stale_surface_fence_races_are_reported_as_reloadable_conflicts() {
        let response = surface_mutation_error_response(AppSurfaceMutationError::Entity(
            AppEntityAdapterError::Mutation(AppEntityMutationError::Boundary(
                AppBoundaryError::StaleProjectedAuthority,
            )),
        ));
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[actix_web::test]
    async fn forged_scope_headers_without_verified_identity_are_rejected() {
        let temp = canonical_tempdir();
        let api = web::Data::new(AppPlatformApi::new(ArtifactV2Workspace::new(temp.path())));
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(api)
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let request = test::TestRequest::post()
            .uri("/apps/packages/import")
            .peer_addr(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9)),
                4444,
            ))
            .insert_header(("X-Principal", "victim"))
            .insert_header(("X-Workspace", "private"))
            .insert_header((
                actix_web::http::header::CONTENT_TYPE,
                APP_PACKAGE_ARCHIVE_MEDIA_TYPE,
            ))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn loopback_fallback_cannot_select_another_scope() {
        let temp = canonical_tempdir();
        let api = web::Data::new(AppPlatformApi::new(ArtifactV2Workspace::new(temp.path())));
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(api)
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let request = test::TestRequest::post()
            .uri("/apps/packages/import")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .insert_header(("X-Principal", "other"))
            .insert_header(("X-Workspace", "other"))
            .insert_header((
                actix_web::http::header::CONTENT_TYPE,
                APP_PACKAGE_ARCHIVE_MEDIA_TYPE,
            ))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn package_import_stages_bytes_but_cannot_create_registry_authority() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let database = workspace.app_store_db_path("anonymous", "default");
        let api = web::Data::new(AppPlatformApi::new(workspace));
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(api)
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let request = test::TestRequest::post()
            .uri("/apps/packages/import")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .insert_header((
                actix_web::http::header::CONTENT_TYPE,
                APP_PACKAGE_ARCHIVE_MEDIA_TYPE,
            ))
            .set_payload(archive_bytes_fixture())
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["state"], "staged_for_local_conformance");
        assert_eq!(body["foreign_authority_transferred"], false);
        assert_eq!(body["requirements"]["run_local_conformance"], true);
        assert_eq!(body["requirements"]["run_local_permission_review"], true);
        assert_eq!(body["requirements"]["foreign_grants_transfer"], false);
        assert!(!database.exists());
    }

    fn packed_scaffold_archive(root: &std::path::Path, name: &str) -> Vec<u8> {
        use magician::magician_v2::apps::authoring::{
            run_app_authoring_command, AppAuthoringCommand, AppCatalogDiscoverArgs, AppInitArgs,
            AppPackArgs,
        };
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

    #[actix_web::test]
    async fn owner_can_review_and_approve_a_ready_for_review_candidate() {
        let temp = canonical_tempdir();
        let archive = packed_scaffold_archive(temp.path(), "http-review-notes");
        let admitted = admit_package_archive(&archive).expect("package identities");
        let package_id = admitted.package().package_id.to_string();
        let source_publisher = admitted.package().publisher_identity.to_string();
        let content_digest = admitted.package().package_content_digest.to_string();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = web::Data::new(AppPlatformApi::new(workspace));
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(api)
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;

        let published = test::TestRequest::post()
            .uri("/apps/packages/candidates")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .insert_header((
                actix_web::http::header::CONTENT_TYPE,
                APP_PACKAGE_ARCHIVE_MEDIA_TYPE,
            ))
            .insert_header(("x-magician-app-request-id", "candidate-request:http-review"))
            .insert_header(("x-magician-app-package-id", package_id))
            .insert_header(("x-magician-app-source-publisher", source_publisher))
            .insert_header(("x-magician-app-content-digest", content_digest))
            .set_payload(archive)
            .to_request();
        let published = test::call_service(&app, published).await;
        assert_eq!(published.status(), StatusCode::CREATED);
        let published: serde_json::Value = test::read_body_json(published).await;
        assert_eq!(published["state"], "ready_for_review");
        assert_eq!(published["activation_authority_granted"], false);
        let installation_id = published["installation_id"]
            .as_str()
            .expect("installation_id")
            .to_owned();

        let review = test::TestRequest::get()
            .uri(&format!("/apps/installations/{installation_id}/review"))
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let review = test::call_service(&app, review).await;
        assert_eq!(review.status(), StatusCode::OK);
        let review: serde_json::Value = test::read_body_json(review).await;
        assert_eq!(review["name"], "http-review-notes");
        assert_eq!(review["attempt_kind"], "initial_install");

        let approved = test::TestRequest::post()
            .uri(&format!("/apps/installations/{installation_id}/approve"))
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .set_json(json!({
                "review_material_digest": review["workflow_material_digest"],
            }))
            .to_request();
        let approved = test::call_service(&app, approved).await;
        let approved_status = approved.status();
        let approved: serde_json::Value = test::read_body_json(approved).await;
        assert_eq!(
            approved_status,
            StatusCode::OK,
            "unexpected body: {approved}"
        );
        assert_eq!(approved["status"], "enabled");
        assert_eq!(approved["outcome"], "enabled");

        let loaded = test::TestRequest::get()
            .uri(&format!("/apps/installations/{installation_id}"))
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let loaded = test::call_service(&app, loaded).await;
        assert_eq!(loaded.status(), StatusCode::OK);
        let loaded: serde_json::Value = test::read_body_json(loaded).await;
        assert_eq!(loaded["lifecycle"]["status"], "enabled");

        let directory = test::TestRequest::get()
            .uri("/apps/directory?section=installed&limit=12")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let directory = test::call_service(&app, directory).await;
        assert_eq!(directory.status(), StatusCode::OK);
        let directory: serde_json::Value = test::read_body_json(directory).await;
        let names: Vec<&str> = directory["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .filter_map(|entry| entry["name"].as_str())
            .collect();
        assert!(names.contains(&"http-review-notes"));
    }

    #[actix_web::test]
    async fn candidate_publication_requires_fresh_server_conformance() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = web::Data::new(AppPlatformApi::new(workspace));
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(api)
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let archive = archive_bytes_fixture();
        let admitted = admit_package_archive(&archive).expect("package identities");
        let request = test::TestRequest::post()
            .uri("/apps/packages/candidates")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .insert_header((
                actix_web::http::header::CONTENT_TYPE,
                APP_PACKAGE_ARCHIVE_MEDIA_TYPE,
            ))
            .insert_header(("x-magician-app-request-id", "candidate-request:stale"))
            .insert_header((
                "x-magician-app-package-id",
                admitted.package().package_id.to_string(),
            ))
            .insert_header((
                "x-magician-app-source-publisher",
                admitted.package().publisher_identity.to_string(),
            ))
            .insert_header((
                "x-magician-app-content-digest",
                admitted.package().package_content_digest.to_string(),
            ))
            .set_payload(archive)
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["error"], "app_candidate_conformance_failed");
    }

    #[actix_web::test]
    async fn candidate_publication_rejects_a_mismatched_staged_source_identity() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = web::Data::new(AppPlatformApi::new(workspace));
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(api)
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let archive = archive_bytes_fixture();
        let admitted = admit_package_archive(&archive).expect("package identities");
        let request = test::TestRequest::post()
            .uri("/apps/packages/candidates")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .insert_header((
                actix_web::http::header::CONTENT_TYPE,
                APP_PACKAGE_ARCHIVE_MEDIA_TYPE,
            ))
            .insert_header(("x-magician-app-request-id", "candidate-request:mismatch"))
            .insert_header(("x-magician-app-package-id", "package:not-the-upload"))
            .insert_header((
                "x-magician-app-source-publisher",
                admitted.package().publisher_identity.to_string(),
            ))
            .insert_header((
                "x-magician-app-content-digest",
                admitted.package().package_content_digest.to_string(),
            ))
            .set_payload(archive)
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["error"], "app_candidate_source_identity_mismatch");
    }

    #[actix_web::test]
    async fn standalone_procedure_publication_allocates_identity_without_activation() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = web::Data::new(AppPlatformApi::new(workspace));
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(api)
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let bytes = b"---\nname: external-summary\nversion: 1.0.0\ndescription: Summarize reviewed input.\nmetadata:\n  magician:\n    skill_type: procedure\n---\nProduce one concise summary.\n";
        let request = test::TestRequest::post()
            .uri("/apps/procedure-revisions")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .insert_header((
                actix_web::http::header::CONTENT_TYPE,
                APP_PROCEDURE_SKILL_MEDIA_TYPE,
            ))
            .set_payload(bytes.as_slice())
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["state"], "immutable_revision_published");
        assert_eq!(body["dependency_ref"], "skill:external-summary");
        assert_eq!(body["semantic_version"], "1.0.0");
        assert_eq!(body["revision"], 1);
        assert_eq!(body["publication_outcome"], "created");
        assert_eq!(body["global_skill_catalog_published"], false);
        assert_eq!(body["activation_authority_granted"], false);
    }

    #[actix_web::test]
    async fn standalone_procedure_transport_rejects_wrong_kind_and_media_type() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = web::Data::new(AppPlatformApi::new(workspace));
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(api)
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let bytes = b"---\nname: external-tool\nversion: 1.0.0\ndescription: Not a procedure.\nmetadata:\n  magician:\n    skill_type: tool\n---\nRun it.\n";
        let wrong_kind = test::TestRequest::post()
            .uri("/apps/procedure-revisions")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .insert_header((
                actix_web::http::header::CONTENT_TYPE,
                APP_PROCEDURE_SKILL_MEDIA_TYPE,
            ))
            .set_payload(bytes.as_slice())
            .to_request();
        let response = test::call_service(&app, wrong_kind).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["error"], "app_procedure_conformance_failed");

        let wrong_media = test::TestRequest::post()
            .uri("/apps/procedure-revisions")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .insert_header((actix_web::http::header::CONTENT_TYPE, "application/json"))
            .set_payload(bytes.as_slice())
            .to_request();
        let response = test::call_service(&app, wrong_media).await;
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[actix_web::test]
    async fn standalone_capability_publication_allocates_identity_without_activation() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = web::Data::new(AppPlatformApi::new(workspace));
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(api)
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let bytes = br#"---
name: next-step
version: 1.0.0
description: Rank the next learning step.
metadata:
  magician:
    skill_type: tool
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires: {bins: [next-step]}
      runtime:
        protocol: cli
        command_prefix: []
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v1
      actions:
        rank:
          description: Rank the next step.
          fixed_args: [rank]
---
Return one ranked next step.
"#;
        let request = test::TestRequest::post()
            .uri("/apps/capability-revisions")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .insert_header((
                actix_web::http::header::CONTENT_TYPE,
                APP_CAPABILITY_SKILL_MEDIA_TYPE,
            ))
            .set_payload(bytes.as_slice())
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["state"], "immutable_revision_published");
        assert_eq!(body["dependency_ref"], "capability:next-step");
        assert_eq!(body["publication_outcome"], "created");
        assert_eq!(body["global_skill_catalog_published"], false);
        assert_eq!(body["activation_authority_granted"], false);
    }

    #[actix_web::test]
    async fn standalone_capability_publication_rejects_a_non_usr_tool_document() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = web::Data::new(AppPlatformApi::new(workspace));
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(api)
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let bytes = b"---\nname: next-step\nversion: 1.0.0\ndescription: Rank the next learning step.\nmetadata:\n  magician:\n    skill_type: tool\n---\nReturn one ranked next step.\n";
        let request = test::TestRequest::post()
            .uri("/apps/capability-revisions")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .insert_header((
                actix_web::http::header::CONTENT_TYPE,
                APP_CAPABILITY_SKILL_MEDIA_TYPE,
            ))
            .set_payload(bytes.as_slice())
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["error"], "app_capability_conformance_failed");
    }

    #[actix_web::test]
    async fn owner_data_query_uses_verified_scope_and_the_canonical_store_service() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = AppPlatformApi::new(workspace);
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(
            &api.registry,
            digest,
            schema,
            AppInstallationStatus::Enabled,
        )
        .await;
        seed_records(&api.registry).await;
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(api))
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let request = test::TestRequest::post()
            .uri("/apps/installations/install_1/data/query")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .set_json(data_query("install_1"))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let page: magician::magician_v2::apps::models::AppQueryPage =
            test::read_body_json(response).await;
        assert_eq!(page.envelope.installation_id.as_str(), "install_1");
        assert!(!page.envelope.value.is_empty());
    }

    #[actix_web::test]
    async fn active_surface_route_returns_the_compiled_envelope_and_canonical_indexed_page() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = AppPlatformApi::new(workspace);
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(
            &api.registry,
            digest,
            schema,
            AppInstallationStatus::Enabled,
        )
        .await;
        seed_records(&api.registry).await;
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(api))
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let request = test::TestRequest::get()
            .uri("/apps/installations/install_1/surfaces")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["binding"]["app_local_route"], "/");
        assert_eq!(body["surface"]["interaction_mode"], "app");
        assert_eq!(body["page"]["envelope"]["installation_id"], "install_1");
        assert!(!body["page"]["envelope"]["value"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(
            body["page"]["envelope"]["value"][0]["fields"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["status", "title"]
        );
    }

    #[actix_web::test]
    async fn surface_mutation_derives_entity_and_rejects_stale_or_smuggled_authority() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = AppPlatformApi::new(workspace);
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(
            &api.registry,
            digest,
            schema,
            AppInstallationStatus::Enabled,
        )
        .await;
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(api))
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;

        let hydration_request = test::TestRequest::get()
            .uri("/apps/installations/install_1/surfaces")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let hydration_response = test::call_service(&app, hydration_request).await;
        let hydration: serde_json::Value = test::read_body_json(hydration_response).await;
        let surface_revision = hydration["surface"]["surface_revision"].clone();
        let view_id = hydration["surface"]["view_id"].clone();

        let create = test::TestRequest::post()
            .uri("/apps/installations/install_1/surface-mutations")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .set_json(json!({
                "protocol_version": "1",
                "surface_revision": surface_revision,
                "view_id": view_id,
                "client_mutation_id": "surface-client:test-create",
                "operation": {
                    "kind": "create",
                    "values": {"title": "Created from surface", "status": "open"}
                }
            }))
            .to_request();
        let create_response = test::call_service(&app, create).await;
        assert_eq!(create_response.status(), StatusCode::OK);
        let receipt: serde_json::Value = test::read_body_json(create_response).await;
        assert_eq!(receipt["origin"]["kind"], "surface");
        assert_eq!(receipt["committed_record_revisions"][0]["entity"], "item");

        let stale = test::TestRequest::post()
            .uri("/apps/installations/install_1/surface-mutations")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .set_json(json!({
                "protocol_version": "1",
                "surface_revision": 999,
                "view_id": hydration["surface"]["view_id"],
                "client_mutation_id": "surface-client:test-stale",
                "operation": {
                    "kind": "create",
                    "values": {"title": "Stale", "status": "open"}
                }
            }))
            .to_request();
        assert_eq!(
            test::call_service(&app, stale).await.status(),
            StatusCode::CONFLICT
        );

        let smuggled = test::TestRequest::post()
            .uri("/apps/installations/install_1/surface-mutations")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .set_json(json!({
                "protocol_version": "1",
                "surface_revision": hydration["surface"]["surface_revision"],
                "view_id": hydration["surface"]["view_id"],
                "client_mutation_id": "surface-client:test-smuggled",
                "entity": "other",
                "operation": {
                    "kind": "create",
                    "values": {"title": "Smuggled", "status": "open"}
                }
            }))
            .to_request();
        assert_eq!(
            test::call_service(&app, smuggled).await.status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[actix_web::test]
    async fn surface_sort_accepts_only_projected_sortable_fields() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = AppPlatformApi::new(workspace);
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(
            &api.registry,
            digest,
            schema,
            AppInstallationStatus::Enabled,
        )
        .await;
        seed_records(&api.registry).await;
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(api))
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let valid = test::TestRequest::get()
            .uri(
                "/apps/installations/install_1/surfaces?sort_field=title&sort_direction=descending",
            )
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        assert_eq!(
            test::call_service(&app, valid).await.status(),
            StatusCode::OK
        );
        let invalid = test::TestRequest::get()
            .uri("/apps/installations/install_1/surfaces?sort_field=undeclared&sort_direction=ascending")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        assert_eq!(
            test::call_service(&app, invalid).await.status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[actix_web::test]
    async fn directory_is_installation_backed_metadata_only_and_pins_current_views() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = AppPlatformApi::new(workspace);
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(
            &api.registry,
            digest,
            schema,
            AppInstallationStatus::Enabled,
        )
        .await;
        seed_records(&api.registry).await;
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(api))
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;

        let request = test::TestRequest::get()
            .uri("/apps/directory?section=installed&limit=12")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let response = test::call_service(&app, request).await;
        let response_status = response.status();
        let response_body = test::read_body(response).await;
        assert_eq!(
            response_status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&response_body)
        );
        let directory: serde_json::Value = serde_json::from_slice(&response_body).unwrap();
        assert_eq!(directory["entries"].as_array().unwrap().len(), 1);
        assert_eq!(directory["entries"][0]["record_count"], 2);
        assert!(!serde_json::to_string(&directory).unwrap().contains("Alpha"));
        let view_id = directory["entries"][0]["views"][0]["view_id"]
            .as_str()
            .unwrap()
            .to_owned();

        let pin = test::TestRequest::post()
            .uri("/apps/installations/install_1/directory-activity")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .set_json(json!({
                "kind": "pin",
                "target_kind": "view",
                "target_id": view_id,
                "pinned": true
            }))
            .to_request();
        let pin_response = test::call_service(&app, pin).await;
        assert_eq!(pin_response.status(), StatusCode::OK);

        let pinned = test::TestRequest::get()
            .uri("/apps/directory?section=pinned&pinned_target_kind=view&limit=12")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let pinned_response = test::call_service(&app, pinned).await;
        assert_eq!(pinned_response.status(), StatusCode::OK);
        let pinned_directory: serde_json::Value = test::read_body_json(pinned_response).await;
        assert_eq!(pinned_directory["entries"][0]["views"][0]["pinned"], true);

        let invalid_target_filter = test::TestRequest::get()
            .uri("/apps/directory?section=installed&pinned_target_kind=view&limit=12")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        assert_eq!(
            test::call_service(&app, invalid_target_filter)
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );

        let record_search = test::TestRequest::get()
            .uri("/apps/directory?section=installed&search=Alpha&limit=12")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let record_search_response = test::call_service(&app, record_search).await;
        let record_search_directory: serde_json::Value =
            test::read_body_json(record_search_response).await;
        assert!(record_search_directory["entries"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[actix_web::test]
    async fn disabled_installation_conceals_its_surface_route() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = AppPlatformApi::new(workspace);
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(
            &api.registry,
            digest,
            schema,
            AppInstallationStatus::Disabled,
        )
        .await;
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(api))
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let disabled_directory = test::TestRequest::get()
            .uri("/apps/directory?section=disabled&limit=12")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let disabled_response = test::call_service(&app, disabled_directory).await;
        let disabled_status = disabled_response.status();
        let disabled_body = test::read_body(disabled_response).await;
        assert_eq!(
            disabled_status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&disabled_body)
        );
        let disabled_page: serde_json::Value = serde_json::from_slice(&disabled_body).unwrap();
        assert_eq!(disabled_page["entries"].as_array().map(Vec::len), Some(1));
        assert_eq!(disabled_page["entries"][0]["status"], "disabled");
        assert!(disabled_page["entries"][0]["views"]
            .as_array()
            .is_some_and(Vec::is_empty));
        let request = test::TestRequest::get()
            .uri("/apps/installations/install_1/surfaces")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn retained_and_purged_installations_move_through_directory_placement() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = AppPlatformApi::new(workspace);
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(
            &api.registry,
            digest,
            schema,
            AppInstallationStatus::Enabled,
        )
        .await;
        rewrite_installation_status(
            &api.registry,
            AppInstallationStatus::UninstalledRetained,
            time(4),
        )
        .await;
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(api.clone()))
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;

        let recovery = test::TestRequest::get()
            .uri("/apps/directory?section=recovery&limit=12")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let recovery_response = test::call_service(&app, recovery).await;
        let recovery_status = recovery_response.status();
        let recovery_body = test::read_body(recovery_response).await;
        assert_eq!(
            recovery_status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&recovery_body)
        );
        let recovery_page: serde_json::Value = serde_json::from_slice(&recovery_body).unwrap();
        assert_eq!(recovery_page["entries"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            recovery_page["entries"][0]["status"],
            "uninstalled_retained"
        );
        assert!(recovery_page["entries"][0]["views"]
            .as_array()
            .is_some_and(Vec::is_empty));

        rewrite_installation_status(&api.registry, AppInstallationStatus::Purged, time(5)).await;
        let after_purge = test::TestRequest::get()
            .uri("/apps/directory?section=recovery&limit=12")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let after_purge_response = test::call_service(&app, after_purge).await;
        assert_eq!(after_purge_response.status(), StatusCode::OK);
        let after_purge_page: serde_json::Value = test::read_body_json(after_purge_response).await;
        assert!(after_purge_page["entries"]
            .as_array()
            .is_some_and(Vec::is_empty));
    }

    #[actix_web::test]
    async fn corrupt_surface_envelope_fails_closed_without_leaking_registry_details() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = AppPlatformApi::new(workspace);
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(
            &api.registry,
            digest,
            schema,
            AppInstallationStatus::Enabled,
        )
        .await;
        api.registry
            .execute_scoped_test_write(
                &authenticated_scope("anonymous", "default"),
                &time(4),
                |connection, _| {
                    connection.execute(
                        "UPDATE app_surface_generation_members SET envelope_json = ?1",
                        [b"{}".as_slice()],
                    )?;
                    Ok(())
                },
            )
            .await
            .unwrap();
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(api))
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let request = test::TestRequest::get()
            .uri("/apps/installations/install_1/surfaces")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["error"], "app_surface_hydration_failed");
        assert!(!body["message"].as_str().unwrap().contains("envelope_json"));
    }

    #[actix_web::test]
    async fn surface_generation_set_digest_tamper_fails_before_indexed_hydration() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = AppPlatformApi::new(workspace);
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(
            &api.registry,
            digest,
            schema,
            AppInstallationStatus::Enabled,
        )
        .await;
        api.registry
            .execute_scoped_test_write(
                &authenticated_scope("anonymous", "default"),
                &time(4),
                |connection, _| {
                    let tampered_digest = AppDigest::blake3(b"tampered-surface-generation");
                    connection.execute(
                        "UPDATE app_surface_generations SET compiled_set_digest = ?1",
                        [tampered_digest.as_str()],
                    )?;
                    Ok(())
                },
            )
            .await
            .unwrap();
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(api))
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let request = test::TestRequest::get()
            .uri("/apps/installations/install_1/surfaces")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["error"], "app_surface_hydration_failed");
    }

    #[actix_web::test]
    async fn oversized_persisted_surface_blob_is_rejected_before_blob_materialization() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = AppPlatformApi::new(workspace);
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(
            &api.registry,
            digest,
            schema,
            AppInstallationStatus::Enabled,
        )
        .await;
        api.registry
            .execute_scoped_test_write(
                &authenticated_scope("anonymous", "default"),
                &time(4),
                |connection, _| {
                    connection.execute(
                        "UPDATE app_surface_generation_members
                            SET envelope_json = zeroblob(?1)",
                        [
                            i64::try_from(AppContractLimits::default().max_value_bytes() + 1)
                                .unwrap(),
                        ],
                    )?;
                    Ok(())
                },
            )
            .await
            .unwrap();
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(api))
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let request = test::TestRequest::get()
            .uri("/apps/installations/install_1/surfaces")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["error"], "app_surface_stale");
    }

    #[actix_web::test]
    async fn owner_data_route_rejects_a_body_that_names_another_installation() {
        let temp = canonical_tempdir();
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(AppPlatformApi::new(
                    ArtifactV2Workspace::new(temp.path()),
                )))
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let request = test::TestRequest::post()
            .uri("/apps/installations/install_1/data/query")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .set_json(data_query("install_2"))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn owner_data_mutation_mints_owner_provenance_and_returns_the_canonical_receipt() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = AppPlatformApi::new(workspace);
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(
            &api.registry,
            digest,
            schema,
            AppInstallationStatus::Enabled,
        )
        .await;
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(api))
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let request = test::TestRequest::post()
            .uri("/apps/installations/install_1/data/mutations")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .set_json(data_mutation())
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let receipt: magician::magician_v2::apps::records::AppMutationReceipt =
            test::read_body_json(response).await;
        assert!(matches!(
            receipt.origin,
            magician::magician_v2::apps::records::AppMutationOrigin::OwnerApi {
                request_ref,
                ..
            } if request_ref.as_str() == "mutation:http-owner-create"
        ));
    }

    #[actix_web::test]
    async fn owner_data_route_rejects_deep_json_before_transport_deserialization() {
        let temp = canonical_tempdir();
        let verifier: web::Data<
            Option<Arc<magician::magician_v2::cloudflare_access::AccessVerifier>>,
        > = web::Data::new(None);
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(AppPlatformApi::new(
                    ArtifactV2Workspace::new(temp.path()),
                )))
                .app_data(verifier)
                .wrap(from_fn(verify_access_middleware))
                .configure(configure_app_routes),
        )
        .await;
        let depth = AppContractLimits::default()
            .max_json_depth()
            .saturating_add(2);
        let mut body = String::from(
            r#"{"protocol_version":"v1","idempotency_key":"mutation:deep","atomicity":"all_or_nothing","expected_schema_revision":1,"operations":[{"kind":"create","entity":"item","temporary_id":"deep","payload":"#,
        );
        for _ in 0..depth {
            body.push_str("{\"nested\":");
        }
        body.push_str("null");
        for _ in 0..depth {
            body.push('}');
        }
        body.push_str("}],\"expected_record_revisions\":[]}");
        let request = test::TestRequest::post()
            .uri("/apps/installations/install_1/data/mutations")
            .peer_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4444))
            .insert_header((actix_web::http::header::CONTENT_TYPE, "application/json"))
            .set_payload(body)
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// The receipt publisher's mutation has to be one the store will actually
    /// accept: the declared entity, a named row id outside the host's own
    /// namespace, and the destination's field set as the payload. Every way of
    /// getting that wrong fails the same silent way — the publish warns and the
    /// package's receipts view simply stays empty — so it is pinned here rather
    /// than discovered in a console.
    #[::core::prelude::v1::test]
    fn a_meeting_control_receipt_publishes_one_named_row_in_the_declared_entity() {
        let hex = "b".repeat(64);
        let receipt: AppMeetingControlReceipt = serde_json::from_value(json!({
            "receipt_id": format!("receipt:{hex}"),
            "decision_id": format!("blake3:{hex}"),
            "verb": "listen",
            "outcome": "started",
            "session_id": "listen-abc",
            "thread_id": "meeting-acme-2026-09-04",
            "applied_at": "2026-09-04T10:00:00Z",
        }))
        .expect("a control receipt round-trips its own wire shape");
        let revision = AppRevision::new(1).expect("schema revision");
        let command = app_meeting_control_receipt_mutation(&receipt, revision)
            .expect("a receipt keyed by a real decision publishes");

        assert_eq!(command.expected_schema_revision, revision);
        assert_eq!(command.atomicity, AppMutationAtomicity::AllOrNothing);
        assert_eq!(command.operations.len(), 1);
        let AppMutationOperation::Create {
            entity,
            record_id,
            payload,
            ..
        } = &command.operations[0]
        else {
            panic!("a receipt is published as a create, so a second publish collides");
        };
        assert_eq!(entity.as_str(), CONTROL_RECEIPT_ENTITY);
        let record_id = record_id
            .as_ref()
            .expect("the row is named by its decision, never host-minted");
        assert_eq!(record_id.as_str(), format!("receipt-{hex}"));
        assert!(
            !record_id.as_str().starts_with("rec_"),
            "the store's own record-id namespace is reserved against callers"
        );
        // The row is the destination's receipt, field for field: nothing here
        // gets to reword what the signed decision recorded.
        assert_eq!(payload, &serde_json::to_value(&receipt).expect("row"));
        assert_eq!(payload["decision_id"], json!(format!("blake3:{hex}")));

        // A decision id outside the ledger's digest shape publishes nothing,
        // rather than a row under an id this function invented.
        let mut forged = receipt.clone();
        forged.decision_id = "sha256:not-a-decision".to_owned();
        assert!(app_meeting_control_receipt_mutation(&forged, revision).is_none());

        command
            .validate_app_contract(&AppContractLimits::default())
            .expect("the publisher's mutation satisfies the same contract the route enforces");
    }

    /// The claims sibling of the pin above, and the whole point of E1: the
    /// projector mints a row in a host ledger the app cannot read, so unless
    /// this mutation is one the store accepts, the package's `/receipts` view
    /// stays empty for the life of the installation and nothing says why.
    #[::core::prelude::v1::test]
    fn a_review_receipt_publishes_one_named_row_in_the_declared_entity() {
        let row = |target_kind: &str| -> ProjectedReviewReceipt {
            serde_json::from_value(json!({
                "receipt_id": "receipt-9f2c",
                "decision_id": "decision:one",
                "target_kind": target_kind,
                "target_id": "claim-1",
                "outcome": "applied",
                "actor_ref": "owner@example.com",
                "destination_revision": 2,
                "applied_at": "2026-09-04T10:00:00Z",
            }))
            .expect("a projected receipt round-trips its own wire shape")
        };
        let claim = row("claim");
        let revision = AppRevision::new(1).expect("schema revision");
        let command = app_claims_review_receipt_mutation(&claim, revision)
            .expect("a projected row publishes");

        assert_eq!(command.expected_schema_revision, revision);
        assert_eq!(command.atomicity, AppMutationAtomicity::AllOrNothing);
        assert_eq!(command.operations.len(), 1);
        let AppMutationOperation::Create {
            entity,
            record_id,
            payload,
            ..
        } = &command.operations[0]
        else {
            panic!("a receipt is published as a create, so a second publish collides");
        };
        assert_eq!(entity.as_str(), REVIEW_RECEIPT_ENTITY);
        let record_id = record_id
            .as_ref()
            .expect("the row is named by its decision, never host-minted");
        assert_eq!(record_id.as_str(), claim.package_record_id());
        assert!(
            !record_id.as_str().starts_with("rec_"),
            "the store's own record-id namespace is reserved against callers"
        );
        // The row is the destination's projected receipt, field for field:
        // nothing here gets to reword what the register decided.
        assert_eq!(payload, &serde_json::to_value(&claim).expect("row"));

        // The two registers namespace decision ids independently, so one id in
        // both must occupy two rows. Sharing one would let the second publish
        // collide and be swallowed as "already there".
        let commitment = row("commitment");
        assert_ne!(claim.package_record_id(), commitment.package_record_id());
        let commitment_command = app_claims_review_receipt_mutation(&commitment, revision)
            .expect("the commitment row publishes too");
        assert_ne!(command.idempotency_key, commitment_command.idempotency_key);

        command
            .validate_app_contract(&AppContractLimits::default())
            .expect("the publisher's mutation satisfies the same contract the route enforces");
        commitment_command
            .validate_app_contract(&AppContractLimits::default())
            .expect("and so does its sibling");
    }

    /// The publication hands a destination the SCOPE's decision ledger — every
    /// receipted claims and commitment decision, including ones taken through
    /// other installations. The store fence it crosses cannot bound that: it
    /// never asks which entity the owner may write, so declaring a
    /// `review_receipt` entity would otherwise be the whole admission. This
    /// pins the fence that replaces it, because the failure it prevents is
    /// silent in both directions — an ungranted package quietly filling up with
    /// other apps' decisions, or a fence renamed out from under the only
    /// package that legitimately holds one.
    #[::core::prelude::v1::test]
    fn a_review_receipt_publication_requires_the_owner_granted_evidence_read() {
        let required =
            app_review_receipt_publication_capability().expect("the fence names a real capability");
        // Exactly the `dependency_ref` the first-party claims-review package
        // carries (`magician_data_v3/system/claims_review/app/.magician/`), so
        // a rename that orphans this fence fails here rather than in a console.
        assert_eq!(required.as_str(), "capability:evidence_data");

        assert!(
            grant_carries_scope_wide_evidence_read(&[required.clone()]),
            "the grant the owner reviewed for the register read is the admission"
        );

        // An owner who granted the package nothing, or narrowed the register
        // read away, has given it no scope-wide view of the register at all —
        // and a projected receipt is that view in another shape.
        assert!(!grant_carries_scope_wide_evidence_read(&[]));

        // Near misses are not the grant. `granted_tools` is stored canonical,
        // so a lookalike name is a different capability, never this one.
        let lookalike = AppReference::parse("capability:evidence_data_override")
            .expect("a lookalike capability reference parses");
        let unrelated =
            AppReference::parse("capability:content_search").expect("an unrelated capability");
        assert!(!grant_carries_scope_wide_evidence_read(&[
            lookalike,
            unrelated.clone()
        ]));
        assert!(grant_carries_scope_wide_evidence_read(&[
            unrelated, required
        ]));
    }

    #[actix_web::test]
    async fn caller_correctable_data_errors_are_not_reported_as_server_failures() {
        let unknown_field = entity_adapter_error_response(AppEntityAdapterError::Store(
            AppEntityStoreError::UnknownField("unknown".to_owned()),
        ));
        assert_eq!(unknown_field.status(), StatusCode::BAD_REQUEST);

        let unknown_entity = entity_adapter_error_response(AppEntityAdapterError::Mutation(
            AppEntityMutationError::UnknownEntity("unknown".to_owned()),
        ));
        assert_eq!(unknown_entity.status(), StatusCode::BAD_REQUEST);

        let unavailable_reference = entity_adapter_error_response(AppEntityAdapterError::Mutation(
            AppEntityMutationError::ReferenceTargetUnavailable {
                field: "parent".to_owned(),
            },
        ));
        assert_eq!(
            unavailable_reference.status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );

        let oversized_scan = entity_adapter_error_response(AppEntityAdapterError::Store(
            AppEntityStoreError::QueryScanTooLarge,
        ));
        assert_eq!(oversized_scan.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let missing_cursor = entity_adapter_error_response(AppEntityAdapterError::Store(
            AppEntityStoreError::MissingCursor,
        ));
        assert_eq!(missing_cursor.status(), StatusCode::GONE);

        let expired_cursor = entity_adapter_error_response(AppEntityAdapterError::Store(
            AppEntityStoreError::Query(AppQuerySemanticsError::CursorExpired),
        ));
        assert_eq!(expired_cursor.status(), StatusCode::GONE);

        let stale_cursor = entity_adapter_error_response(AppEntityAdapterError::Store(
            AppEntityStoreError::Query(AppQuerySemanticsError::CursorSnapshotStale),
        ));
        assert_eq!(stale_cursor.status(), StatusCode::CONFLICT);

        let corrupt_cursor = entity_adapter_error_response(AppEntityAdapterError::Store(
            AppEntityStoreError::Query(AppQuerySemanticsError::CursorStorageCorrupt),
        ));
        assert_eq!(corrupt_cursor.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let cursor_capacity = entity_adapter_error_response(AppEntityAdapterError::Store(
            AppEntityStoreError::CursorCapacityExceeded,
        ));
        assert_eq!(cursor_capacity.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            cursor_capacity
                .headers()
                .get(actix_web::http::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("1")
        );
    }

    // ------------------------------------------------------------------
    // Session-bound scope for custom-surface assets (the hosted-web CF
    // Access posture): a served frame's GET carries only the minted
    // session path segment, so the session — not any header — names the
    // workspace.

    /// A request exactly as a served frame sends it: a verified
    /// Cloudflare Access interactive identity (no workspace binding) and
    /// no header envelope at all.
    fn access_identity_request(
        principal: &str,
        verified_at: &chrono::DateTime<chrono::Utc>,
    ) -> HttpRequest {
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/apps/installations/install_1/custom-surface-v1/assets/x")
            .to_http_request();
        request
            .extensions_mut()
            .insert(VerifiedRequestIdentity::for_test_at(
                principal,
                None,
                VerifiedRequestAuthentication::CloudflareAccess,
                verified_at.to_owned(),
            ));
        request
    }

    /// The custom-surface package shape the kernel tests compile against
    /// (one declared, granted entry point plus its sibling script).
    fn surface_package() -> AppPackageCandidate {
        let manifest = valid_skill_document()
            .replacen(
                "    app_sdk_version: \"1\"\n",
                "    app_sdk_version: \"1\"\n    required_features: [custom_surfaces_v1]\n",
                1,
            )
            .replacen(
                "app:\n",
                &format!(
                    "app:\n  permissions: [custom_surface]\n  custom_surface:\n    \
                     entry_points:\n{}",
                    "      - route: /canvas\n        document: surfaces/canvas.html\n"
                ),
                1,
            );
        build_app_package_candidate(
            vec![
                AppBundleMember::regular_file("SKILL.md", manifest.into_bytes()).unwrap(),
                AppBundleMember::regular_file("workflows/build.md", b"Build a plan.".to_vec())
                    .unwrap(),
                AppBundleMember::regular_file("assets/icon.svg", b"<svg/>".to_vec()).unwrap(),
                AppBundleMember::regular_file(
                    "vendor/skills/summarize/SKILL.md",
                    b"---\nname: summarize\nversion: 2.1.0\n---\n".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file(
                    "vendor/skills/summarize/bin/summarize.py",
                    b"print('summary');\n".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file(
                    "surfaces/canvas.html",
                    b"<html><body>canvas</body></html>".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file(
                    "surfaces/canvas.js",
                    b"console.log('canvas');".to_vec(),
                )
                .unwrap(),
            ],
            &AppPackageLimits::default(),
        )
        .expect("candidate")
    }

    /// Mint one live scripted session on the API's runtime, bound to the
    /// given requesting scope — what the host route's `open_host` call
    /// does under the host document's own authentication.
    fn open_surface_session(
        api: &AppPlatformApi,
        session_suffix: &str,
        bound: AppScriptedSurfaceRequestScope,
    ) -> AppReference {
        open_surface_session_at(api, session_suffix, bound, time(1))
    }

    fn open_surface_session_at(
        api: &AppPlatformApi,
        session_suffix: &str,
        bound: AppScriptedSurfaceRequestScope,
        now: DateTime<Utc>,
    ) -> AppReference {
        let package = surface_package();
        let package_ref = AppReference::parse("package-revision:reading-list").unwrap();
        let bundle = package.bundle_digest().clone();
        let request =
            reviewed_custom_surface_request(package.manifest().manifest(), package.members())
                .expect("hydrate")
                .expect("declared");
        let granted: Vec<AppGrantedCustomSurfaceEntryPoint> = request
            .entry_points
            .iter()
            .map(|entry| AppGrantedCustomSurfaceEntryPoint {
                route: entry.route.clone(),
                document: entry.document.clone(),
                document_digest: entry.document_digest.clone(),
            })
            .collect();
        let session_ref =
            AppReference::parse(format!("bridge-scripted:install_1:{session_suffix}")).unwrap();
        let plan = compile_scripted_surface_host_plan(
            &package,
            "/canvas",
            &granted,
            surface_admission_for_enabled_installation(
                &package_ref,
                &bundle,
                AppInstallationId::parse("install_1").unwrap(),
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                AppReference::parse("host:session-1").unwrap(),
                session_ref.clone(),
                AppReference::parse("nonce:1").unwrap(),
                now,
            ),
            "https://home.magicbeans.ai",
            true,
            now,
        )
        .expect("host plan");
        api.scripted_surfaces
            .open_host(plan, bound, now)
            .expect("open host");
        session_ref
    }

    /// The production stack previously refused frame GETs at the bearer gate,
    /// before the asset route could inspect the live session in their path.
    #[actix_web::test]
    async fn scripted_surface_asset_credential_crosses_auth_layers_without_becoming_api_authority()
    {
        use magician::config::{AuthConfig, AuthMode};
        use magician::magician_v2::auth::middleware::{
            authenticate_request, AuthenticatedScriptedSurfaceAsset,
        };
        use magician::magician_v2::auth::AuthStore;

        let temp = canonical_tempdir();
        let api = web::Data::new(AppPlatformApi::new(ArtifactV2Workspace::new(temp.path())));
        let session = open_surface_session_at(
            &api,
            "http",
            AppScriptedSurfaceRequestScope {
                principal: "owner".to_owned(),
                workspace: "private".to_owned(),
            },
            Utc::now(),
        );
        let package = surface_package();
        let digest = package
            .members()
            .iter()
            .find(|member| member.path().as_str() == "surfaces/canvas.html")
            .unwrap()
            .content_digest();
        let entry = magician_apps::apps::surface_scripted_host::scripted_surface_asset_address(
            &AppInstallationId::parse("install_1").unwrap(),
            &session,
            digest,
            "surfaces/canvas.html",
        );
        let auth = crate::auth_api::auth_runtime(
            Arc::new(AuthStore::open(temp.path()).unwrap()),
            AuthConfig {
                mode: AuthMode::Credentials,
                ..AuthConfig::default()
            },
        );
        let app = test::init_service(
            App::new()
                .app_data(auth)
                .app_data(web::Data::new(api.scripted_surface_authenticator()))
                .wrap(from_fn(verify_access_middleware))
                .wrap(from_fn(authenticate_request))
                .default_service(web::to(|req: HttpRequest| async move {
                    // An asset proof must never make a bridge/data/action request
                    // look like an ordinary signed-in caller.
                    assert!(authenticated_app_scope(&req, &Utc::now()).is_err());
                    assert!(req.headers().get("x-principal").is_none());
                    let extensions = req.extensions();
                    let proof = extensions
                        .get::<AuthenticatedScriptedSurfaceAsset>()
                        .unwrap();
                    HttpResponse::Ok().json(json!({
                        "principal": proof.scope().scope().principal.as_str(),
                        "workspace": proof.scope().scope().workspace.as_str(),
                    }))
                })),
        )
        .await;

        for uri in [&entry, &entry.replace("canvas.html", "canvas.js")] {
            let response = test::call_service(
                &app,
                test::TestRequest::get()
                    .uri(uri)
                    .peer_addr("127.0.0.1:12345".parse().unwrap())
                    .insert_header(("X-Principal", "victim"))
                    .insert_header(("X-Workspace", "other"))
                    .to_request(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let body: serde_json::Value = test::read_body_json(response).await;
            assert_eq!(body, json!({"principal":"owner", "workspace":"private"}));
        }
        for uri in [
            format!("{entry}?session=anything"),
            entry.replace("/install_1/", "/install_other/"),
            entry.replace(session.as_str(), "bridge:unknown"),
            entry.replace("canvas.html", "%63anvas.html"),
            "/api/magician/v2/auth/session".to_owned(),
            "/api/magician/v2/apps/installations/install_1/custom-surface-v1/host".to_owned(),
            "/api/magician/v2/apps/installations/install_1/custom-surface-v1/bridge".to_owned(),
        ] {
            let response =
                test::try_call_service(&app, test::TestRequest::get().uri(&uri).to_request()).await;
            assert_eq!(
                response
                    .err()
                    .expect("request must be refused")
                    .as_response_error()
                    .status_code(),
                StatusCode::UNAUTHORIZED,
                "{uri}"
            );
        }
        for header in ["Bearer invalid", "Basic invalid", ""] {
            let response = test::try_call_service(
                &app,
                test::TestRequest::get()
                    .uri(&entry)
                    .insert_header(("Authorization", header))
                    .to_request(),
            )
            .await;
            assert_eq!(
                response
                    .err()
                    .expect("explicit invalid bearer must be refused")
                    .as_response_error()
                    .status_code(),
                StatusCode::UNAUTHORIZED
            );
        }
        let response =
            test::try_call_service(&app, test::TestRequest::post().uri(&entry).to_request()).await;
        assert_eq!(
            response
                .err()
                .expect("asset POST must be refused")
                .as_response_error()
                .status_code(),
            StatusCode::UNAUTHORIZED
        );
        api.scripted_surfaces.teardown_for_lifecycle_event(
            &AppInstallationId::parse("install_1").unwrap(),
            AppLifecycleEventKind::InstallationDisabled,
        );
        let response =
            test::try_call_service(&app, test::TestRequest::get().uri(&entry).to_request()).await;
        assert_eq!(
            response
                .err()
                .expect("torn-down session must be refused")
                .as_response_error()
                .status_code(),
            StatusCode::UNAUTHORIZED
        );
    }

    /// Hosted-web posture: a frame GET carrying ONLY the minted session
    /// path segment resolves its serving scope from the session's bound
    /// requesting scope — not from any header, because the request
    /// carries none the route could read.
    #[actix_web::test]
    async fn scripted_surface_assets_serve_under_the_session_bound_scope() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = AppPlatformApi::new(workspace);
        let bound = AppScriptedSurfaceRequestScope {
            principal: "owner@host.example".to_owned(),
            workspace: "workspace:default".to_owned(),
        };
        let session_ref = open_surface_session(&api, "serve", bound);
        let query =
            web::Query::from_query(&format!("session={}", session_ref.as_str())).expect("query");
        let scope = scripted_surface_asset_scope(
            &api,
            &access_identity_request("owner@host.example", &time(2)),
            &query,
            &time(2),
        )
        .expect("the session's bound scope serves the credential-less frame");
        assert_eq!(scope.scope().principal.as_str(), "owner@host.example");
        assert_eq!(scope.scope().workspace.as_str(), "workspace:default");
    }

    /// The fallback is fail-closed: a session minted by another owner
    /// refuses as a scope mismatch, and unknown or torn-down sessions
    /// refuse exactly like the serving kernel. The bridge POST contract
    /// is unchanged: no envelope still answers `app_workspace_required`.
    #[actix_web::test]
    async fn scripted_surface_session_scope_refuses_mismatches_and_dead_sessions() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = AppPlatformApi::new(workspace);
        let bound = AppScriptedSurfaceRequestScope {
            principal: "owner@host.example".to_owned(),
            workspace: "workspace:default".to_owned(),
        };
        let session_ref = open_surface_session(&api, "guard", bound);

        // Wrong principal: another verified Access identity presenting a
        // session reference it did not mint.
        let query = web::Query::from_query(&format!("session={}", session_ref.as_str())).unwrap();
        let mismatch = scripted_surface_asset_scope(
            &api,
            &access_identity_request("attacker@host.example", &time(2)),
            &query,
            &time(2),
        )
        .expect_err("a session minted by another owner refuses");
        assert_eq!(mismatch.status(), StatusCode::FORBIDDEN);
        let body: serde_json::Value = read_http_response_json(mismatch).await;
        assert_eq!(body["error"], "app_scope_mismatch");

        // Unknown session: no live session, no scope to lend.
        let unknown = web::Query::from_query("session=bridge-scripted:install_1:unknown").unwrap();
        let gone = scripted_surface_asset_scope(
            &api,
            &access_identity_request("owner@host.example", &Utc::now()),
            &unknown,
            &Utc::now(),
        )
        .expect_err("an unknown session reference refuses");
        assert_eq!(gone.status(), StatusCode::CONFLICT);
        let body: serde_json::Value = read_http_response_json(gone).await;
        assert_eq!(body["error"], "app_custom_surface_unavailable");

        // Torn-down session: the same refusal as the serving kernel.
        api.scripted_surfaces.teardown_for_lifecycle_event(
            &AppInstallationId::parse("install_1").unwrap(),
            AppLifecycleEventKind::InstallationUpdated,
        );
        let dead = scripted_surface_asset_scope(
            &api,
            &access_identity_request("owner@host.example", &time(2)),
            &query,
            &time(2),
        )
        .expect_err("a torn-down session refuses");
        assert_eq!(dead.status(), StatusCode::CONFLICT);
        let body: serde_json::Value = read_http_response_json(dead).await;
        assert_eq!(body["error"], "app_custom_surface_unavailable");

        // TTL-expired session: the same refusal — an expired entry lends
        // no scope, and the lookup evicts it, so the frame cannot keep
        // its session-bound serving credential alive past the TTL.
        let session_ref = open_surface_session(
            &api,
            "expired",
            AppScriptedSurfaceRequestScope {
                principal: "owner@host.example".to_owned(),
                workspace: "workspace:default".to_owned(),
            },
        );
        let query = web::Query::from_query(&format!("session={}", session_ref.as_str())).unwrap();
        let expired = scripted_surface_asset_scope(
            &api,
            &access_identity_request(
                "owner@host.example",
                &(time(1) + ChronoDuration::minutes(15)),
            ),
            &query,
            &(time(1) + ChronoDuration::minutes(15)),
        )
        .expect_err("a TTL-expired session refuses");
        assert_eq!(expired.status(), StatusCode::CONFLICT);
        let body: serde_json::Value = read_http_response_json(expired).await;
        assert_eq!(body["error"], "app_custom_surface_unavailable");

        // The bridge POST keeps the plain envelope contract: no session
        // fallback exists there, and a workspace-less identity still
        // answers the pre-existing 400.
        let refused = authenticated_app_scope(
            &access_identity_request("owner@host.example", &Utc::now()),
            &Utc::now(),
        )
        .expect_err("the bridge POST still demands the header envelope");
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = read_http_response_json(refused).await;
        assert_eq!(body["error"], "app_workspace_required");

        // With a workspace binding on the identity itself (the paired
        // device or loopback shape), the normal path still resolves and
        // the fallback never runs.
        let request = test::TestRequest::get().uri("/x").to_http_request();
        request
            .extensions_mut()
            .insert(VerifiedRequestIdentity::for_test(
                "owner@host.example",
                Some("workspace:default"),
                VerifiedRequestAuthentication::PairedDevice,
            ));
        assert!(authenticated_app_scope(&request, &Utc::now()).is_ok());
    }

    /// A session bound to the wrong workspace cannot reach another
    /// workspace's installation: the lent scope resolves from the
    /// session, and the scope-checked installation lookup refuses it —
    /// while the same composition under the installation's own workspace
    /// passes the scope check (stopping only at the staged package,
    /// which the SQL seed does not stage).
    #[actix_web::test]
    async fn a_wrong_workspace_session_cannot_reach_another_workspaces_installation() {
        let temp = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let api = AppPlatformApi::new(workspace);
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(
            &api.registry,
            digest,
            schema,
            AppInstallationStatus::Enabled,
        )
        .await;
        let wrong_workspace = AppScriptedSurfaceRequestScope {
            principal: "anonymous".to_owned(),
            workspace: "workspace:elsewhere".to_owned(),
        };
        let session_ref = open_surface_session(&api, "cross", wrong_workspace);
        let query = web::Query::from_query(&format!("session={}", session_ref.as_str())).unwrap();
        let scope = scripted_surface_asset_scope(
            &api,
            &access_identity_request("anonymous", &time(2)),
            &query,
            &time(2),
        )
        .expect("the scope resolves from the session; the refusal comes at the registry");
        assert_eq!(scope.scope().workspace.as_str(), "workspace:elsewhere");
        let refused = load_enabled_custom_surface_package(&api, &scope, "install_1", time(2))
            .await
            .expect_err("the installation is outside the lent scope");
        assert_eq!(refused.status(), StatusCode::NOT_FOUND);
        let body: serde_json::Value = read_http_response_json(refused).await;
        assert_eq!(body["error"], "app_installation_not_found");

        let right_workspace = AppScriptedSurfaceRequestScope {
            principal: "anonymous".to_owned(),
            workspace: "default".to_owned(),
        };
        let session_ref = open_surface_session(&api, "home", right_workspace);
        let query = web::Query::from_query(&format!("session={}", session_ref.as_str())).unwrap();
        let scope = scripted_surface_asset_scope(
            &api,
            &access_identity_request("anonymous", &time(2)),
            &query,
            &time(2),
        )
        .expect("the home-workspace session resolves");
        let stopped = load_enabled_custom_surface_package(&api, &scope, "install_1", time(2))
            .await
            .expect_err("the SQL seed stages no package content");
        assert_eq!(stopped.status(), StatusCode::NOT_FOUND);
        let body: serde_json::Value = read_http_response_json(stopped).await;
        assert_eq!(body["error"], "app_custom_surface_package_missing");
    }

    /// The reload-note route carries the asset route's early
    /// percent-encoded-path refusal: a `%` anywhere in the path (a
    /// rewritten or encoded session segment) is refused 404 before any
    /// scope, registry, or session work runs. The kill switch reads the
    /// live config file, so the test points `MAGICIAN_CONFIG_PATH` at a
    /// minimal enabled config — validated by the same shallow pass the
    /// switch itself uses — to keep the handler call hermetic.
    #[actix_web::test]
    async fn reload_note_refuses_percent_encoded_paths_before_any_work() {
        let temp = canonical_tempdir();
        let config = temp.path().join("magician-config.yaml");
        std::fs::write(
            &config,
            "app_platform:\n  custom_surfaces_v1:\n    enabled: true\n",
        )
        .expect("write config");
        let previous = std::env::var_os("MAGICIAN_CONFIG_PATH");
        std::env::set_var("MAGICIAN_CONFIG_PATH", &config);
        let request = test::TestRequest::post()
            .uri("/api/magician/v2/apps/installations/install_1/custom-surface-v1/sessions/%2e%2e/reload-note")
            .to_http_request();
        let response = post_scripted_surface_reload_note_handler(
            web::Data::new(AppPlatformApi::new(ArtifactV2Workspace::new(temp.path()))),
            request,
            web::Path::from(("install_1".to_owned(), "%2e%2e".to_owned())),
        )
        .await;
        // Leave the process env exactly as the test found it before any
        // assertion can fail.
        match previous {
            Some(value) => std::env::set_var("MAGICIAN_CONFIG_PATH", value),
            None => std::env::remove_var("MAGICIAN_CONFIG_PATH"),
        }
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body: serde_json::Value = read_http_response_json(response).await;
        assert_eq!(body["error"], "app_custom_surface_not_found");
    }

    /// The frame-ancestors origin the kernel CSP pins: the configured
    /// public origin wins when the deployment has one — Host and
    /// X-Forwarded-* are client-controlled and must never steer or widen
    /// the frame-ancestors allowlist — and the connection-derived origin
    /// remains only the unset fallback (dev).
    #[actix_web::test]
    async fn frame_ancestors_prefers_the_configured_public_origin_over_connection_info() {
        let request = test::TestRequest::get()
            .uri("/api/magician/v2/apps/installations/install_1/custom-surface-v1/host")
            .insert_header((actix_web::http::header::HOST, "conn.example"))
            .insert_header(("x-forwarded-proto", "https"))
            .insert_header(("x-forwarded-host", "forwarded.example"))
            .to_http_request();
        // Configured wins, exactly as configured — spoofed forwarded
        // headers change nothing.
        assert_eq!(
            scripted_surface_frame_ancestors_origin(Some("https://home.magicbeans.ai"), &request),
            "https://home.magicbeans.ai"
        );
        // A trailing slash would name a different origin than the bare
        // one; the fallback composition keeps the origin exact.
        assert_eq!(
            scripted_surface_frame_ancestors_origin(Some("https://home.magicbeans.ai/"), &request),
            "https://home.magicbeans.ai"
        );
        assert_eq!(
            scripted_surface_frame_ancestors_origin(Some("  "), &request),
            scripted_surface_frame_ancestors_origin(None, &request)
        );
        // Unset (dev): the connection-info derivation is unchanged.
        let expected = {
            let connection = request.connection_info();
            format!("{}://{}", connection.scheme(), connection.host())
        };
        assert_eq!(
            scripted_surface_frame_ancestors_origin(None, &request),
            expected
        );
    }

    fn staged_ingest_request(ours: &[&str], counterparty: &[&str]) -> AppStagedIngestApplyRequest {
        AppStagedIngestApplyRequest {
            record_id: "record-1".to_string(),
            record_revision: 1,
            effective_speaker: "founder@example.com".to_string(),
            ours: ours.iter().map(|name| name.to_string()).collect(),
            counterparty: counterparty.iter().map(|name| name.to_string()).collect(),
            occurred_at: Utc::now(),
            consequence_class: None,
        }
    }

    /// The roster is the security boundary of the staged-ingest path, so both
    /// of its refusals are pinned here rather than left to the register to
    /// discover: a room with nobody on our side can only produce an act with
    /// an empty review queue while looking like it worked, and a name on both
    /// sides means the identity register cannot say whose words those were.
    #[::core::prelude::v1::test]
    fn a_staged_ingest_roster_refuses_an_empty_or_contradictory_side() {
        use magician::magician_v2::evidence::transcript_ingestion::StagedIngestSide;

        assert!(staged_ingest_roster(&staged_ingest_request(&[], &["alice@example.com"])).is_err());
        assert!(staged_ingest_roster(&staged_ingest_request(
            &["founder@example.com"],
            &["founder@example.com"]
        ))
        .is_err());

        let roster = staged_ingest_roster(&staged_ingest_request(
            &["founder@example.com"],
            &["alice@example.com"],
        ))
        .expect("a two-sided roster");
        assert_eq!(
            roster.side_of("founder@example.com"),
            Some(StagedIngestSide::Ours)
        );
        assert_eq!(
            roster.side_of("alice@example.com"),
            Some(StagedIngestSide::Counterparty)
        );
        assert_eq!(
            roster.side_of("nobody@example.com"),
            None,
            "an unplaced name has no side; the apply refuses rather than guessing one"
        );
    }

    /// The class is never guessed. An unrecognised word is refused instead of
    /// read as the mildest class, and `private_local` is refused outright: the
    /// words in a staged room already reached somebody, so the one class that
    /// needs no gate would put an outward act outside every later review.
    #[::core::prelude::v1::test]
    fn a_staged_ingest_consequence_class_is_closed() {
        assert_eq!(
            staged_ingest_consequence_class(None),
            Ok(ConsequenceClass::BoundedCommunication)
        );
        assert_eq!(
            staged_ingest_consequence_class(Some(" Commitment_Or_Transaction ")),
            Ok(ConsequenceClass::CommitmentOrTransaction)
        );
        assert!(staged_ingest_consequence_class(Some("private_local")).is_err());
        assert!(staged_ingest_consequence_class(Some("")).is_err());
    }
}

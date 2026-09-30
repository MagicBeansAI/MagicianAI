//! Pure, transport-independent public contracts for the Magician Apps platform.
//!
//! This crate intentionally has no dependency on Actix, storage, providers,
//! filesystems, registries, or execution services. Runtime owners may consume
//! these DTOs and descriptors, but authority is always reconstructed by the
//! server rather than accepted from a public contract value.

use std::collections::HashSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub mod android_owner;
pub mod contribution;
pub mod llm_operations;
pub mod macos_host;

/// Version of the embedded component/data-plane schema bundle used by app
/// packages, lock evidence, and authoring compatibility checks.
///
/// This advances independently from the supported-public HTTP/OpenAPI bundle.
pub const APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION: &str = "1.0.0";

/// Version of the supported-public HTTP/OpenAPI/capabilities contract.
pub const APP_SUPPORTED_PUBLIC_CONTRACT_VERSION: &str = "1.5.0";

/// Backwards-compatible name for the original component/data-plane contract
/// axis. New code must use the explicitly named version constant for its axis.
#[deprecated(
    note = "use APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION or APP_SUPPORTED_PUBLIC_CONTRACT_VERSION"
)]
pub const APP_CONTRACT_VERSION: &str = APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION;

/// Current app-package manifest schema. This is not a language SDK version.
pub const APP_MANIFEST_SCHEMA_VERSION: &str = "1.0";

/// Deprecated authoring-tool generation marker accepted on legacy manifests.
///
/// New packages declare required platform features and may record a
/// non-authoritative `generated_by` semantic version instead.
pub const APP_AUTHORING_SDK_VERSION: &str = "1";

/// Informational semantic version emitted by the current authoring generator.
pub const APP_AUTHORING_SDK_SEMVER: &str = "1.0.0";

pub const APP_JSON_SCHEMA_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// Canonical namespace for opaque supported-public action-run references.
pub const APP_ACTION_RUN_REF_PREFIX: &str = "run:app-action:";

pub fn has_canonical_app_action_run_namespace(value: &str) -> bool {
    value
        .strip_prefix(APP_ACTION_RUN_REF_PREFIX)
        .is_some_and(|suffix| !suffix.is_empty())
}

/// Version of the canonical app data plane, independent of HTTP and SDKs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
pub enum AppProtocolVersion {
    #[serde(rename = "1")]
    V1,
}

impl AppProtocolVersion {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V1 => "1",
        }
    }

    pub const fn supported() -> &'static [Self] {
        &[Self::V1]
    }
}

/// Machine-interpretable recovery posture for canonical app failures.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppErrorDisposition {
    Terminal,
    RetrySameInput,
    RefreshAndRetry,
    Reauthorize,
    UserActionRequired,
    OutcomeUncertain,
}

/// Stable cross-adapter error families. Concrete HTTP error strings remain a
/// finer-grained compatibility surface and are listed per operation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppErrorCode {
    InvalidRequest,
    NotAuthorized,
    NotFound,
    Conflict,
    StaleRevision,
    SchemaMismatch,
    PolicyDenied,
    ResourceExhausted,
    RateLimited,
    Unavailable,
    Timeout,
    Canceled,
    ExternalOutcomeUncertain,
    Internal,
}

/// The legacy HTTP error body still emitted by internal and compatibility Apps routes.
///
/// This is compatibility truth, not a supported-public contract: the
/// inventoried public routes use the typed `AppErrorEnvelope`. Extra fields are
/// accepted so an existing internal reader can ignore additive diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AppLegacyHttpErrorResponse {
    pub error: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppPublicOperationId {
    ContractCapabilities,
    QueryData,
    MutateData,
    LaunchAction,
    GetActionRun,
    ComposeActionRun,
    CancelActionRun,
    ReadEntityChanges,
}

impl AppPublicOperationId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ContractCapabilities => "contract_capabilities",
            Self::QueryData => "query_data",
            Self::MutateData => "mutate_data",
            Self::LaunchAction => "launch_action",
            Self::GetActionRun => "get_action_run",
            Self::ComposeActionRun => "compose_action_run",
            Self::CancelActionRun => "cancel_action_run",
            Self::ReadEntityChanges => "read_entity_changes",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "UPPERCASE")]
pub enum AppHttpMethod {
    Get,
    Post,
}

impl AppHttpMethod {
    pub const fn as_openapi_key(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Post => "post",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppOperationAuthPosture {
    /// A middleware-verified browser, paired-device, or loopback identity.
    VerifiedSession,
    /// A verified identity plus a server-bound principal/workspace scope.
    VerifiedOwnerScope,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppOperationIdempotency {
    ReadOnly,
    ClientKeyed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppOperationParameterLocation {
    Path,
    Query,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppPublicParameterDescriptor {
    pub name: &'static str,
    pub location: AppOperationParameterLocation,
    pub required: bool,
    pub schema_type: &'static str,
    pub schema_format: Option<&'static str>,
    pub minimum: Option<u64>,
    pub maximum: Option<u64>,
    pub default: Option<u64>,
    pub description: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AppPublicParameter {
    pub name: String,
    pub location: AppOperationParameterLocation,
    pub required: bool,
    pub schema_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<u64>,
    pub description: String,
}

impl From<&AppPublicParameterDescriptor> for AppPublicParameter {
    fn from(value: &AppPublicParameterDescriptor) -> Self {
        Self {
            name: value.name.to_owned(),
            location: value.location,
            required: value.required,
            schema_type: value.schema_type.to_owned(),
            schema_format: value.schema_format.map(str::to_owned),
            minimum: value.minimum,
            maximum: value.maximum,
            default: value.default,
            description: value.description.to_owned(),
        }
    }
}

/// Immutable source metadata for a supported-public route.
///
/// API registration consumes the path and asserts the method from this same
/// descriptor. Generated inventory and OpenAPI documents consume the complete
/// descriptor, avoiding a second handwritten route catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppPublicOperationDescriptor {
    pub id: AppPublicOperationId,
    pub method: AppHttpMethod,
    pub path: &'static str,
    pub summary: &'static str,
    pub auth: AppOperationAuthPosture,
    pub idempotency: AppOperationIdempotency,
    pub parameters: &'static [AppPublicParameterDescriptor],
    pub request_schema: Option<&'static str>,
    pub success_statuses: &'static [u16],
    pub response_schema: &'static str,
    pub request_example: Option<&'static str>,
    pub response_example: Option<&'static str>,
    pub errors: &'static [&'static str],
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AppPublicOperation {
    pub operation_id: String,
    pub method: AppHttpMethod,
    pub path: String,
    pub summary: String,
    pub auth: AppOperationAuthPosture,
    pub idempotency: AppOperationIdempotency,
    pub parameters: Vec<AppPublicParameter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_schema: Option<String>,
    pub success_statuses: Vec<u16>,
    pub response_schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_example: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_example: Option<String>,
    pub errors: Vec<String>,
}

impl From<&AppPublicOperationDescriptor> for AppPublicOperation {
    fn from(value: &AppPublicOperationDescriptor) -> Self {
        Self {
            operation_id: value.id.as_str().to_owned(),
            method: value.method,
            path: value.path.to_owned(),
            summary: value.summary.to_owned(),
            auth: value.auth,
            idempotency: value.idempotency,
            parameters: value.parameters.iter().map(Into::into).collect(),
            request_schema: value.request_schema.map(str::to_owned),
            success_statuses: value.success_statuses.to_vec(),
            response_schema: value.response_schema.to_owned(),
            request_example: value.request_example.map(str::to_owned),
            response_example: value.response_example.map(str::to_owned),
            errors: value
                .errors
                .iter()
                .map(|error| (*error).to_owned())
                .collect(),
        }
    }
}

const INSTALLATION_PATH_PARAMETER: AppPublicParameterDescriptor = AppPublicParameterDescriptor {
    name: "installation_id",
    location: AppOperationParameterLocation::Path,
    required: true,
    schema_type: "string",
    schema_format: None,
    minimum: None,
    maximum: None,
    default: None,
    description: "Opaque installation identity in the authenticated owner scope.",
};
const ACTION_PATH_PARAMETER: AppPublicParameterDescriptor = AppPublicParameterDescriptor {
    name: "action_id",
    location: AppOperationParameterLocation::Path,
    required: true,
    schema_type: "string",
    schema_format: None,
    minimum: None,
    maximum: None,
    default: None,
    description: "Manifest-declared action name.",
};
const RUN_PATH_PARAMETER: AppPublicParameterDescriptor = AppPublicParameterDescriptor {
    name: "run_ref",
    location: AppOperationParameterLocation::Path,
    required: true,
    schema_type: "string",
    schema_format: None,
    minimum: None,
    maximum: None,
    default: None,
    description: "Opaque logical action-run reference in the canonical run:app-action: namespace; it carries no authority.",
};
const AFTER_CHANGE_SEQUENCE_PARAMETER: AppPublicParameterDescriptor =
    AppPublicParameterDescriptor {
        name: "after_change_sequence",
        location: AppOperationParameterLocation::Query,
        required: false,
        schema_type: "integer",
        schema_format: None,
        minimum: Some(0),
        maximum: None,
        default: None,
        description: "Last applied durable change sequence, or zero for the initial page.",
    };
const SURFACE_REVISION_PARAMETER: AppPublicParameterDescriptor = AppPublicParameterDescriptor {
    name: "surface_revision",
    location: AppOperationParameterLocation::Query,
    required: true,
    schema_type: "integer",
    schema_format: None,
    minimum: Some(1),
    maximum: None,
    default: None,
    description: "Positive surface revision whose projection is being refreshed.",
};
const CHANGE_LIMIT_PARAMETER: AppPublicParameterDescriptor = AppPublicParameterDescriptor {
    name: "limit",
    location: AppOperationParameterLocation::Query,
    required: false,
    schema_type: "integer",
    schema_format: None,
    minimum: Some(1),
    maximum: Some(128),
    default: Some(64),
    description: "Bounded page size; the server default is 64 and the maximum is 128.",
};

/// Canonical supported-public operation inventory.
///
/// Internal, dormant, custom-surface bridge, installation-review, and raw
/// authority-shaped invocation routes are intentionally absent.
pub const SUPPORTED_PUBLIC_APP_OPERATIONS: &[AppPublicOperationDescriptor] = &[
    AppPublicOperationDescriptor {
        id: AppPublicOperationId::ContractCapabilities,
        method: AppHttpMethod::Get,
        path: "/contract-capabilities",
        summary: "Negotiate the supported Apps public contract.",
        auth: AppOperationAuthPosture::VerifiedSession,
        idempotency: AppOperationIdempotency::ReadOnly,
        parameters: &[],
        request_schema: None,
        success_statuses: &[200],
        response_schema: "AppContractCapabilities",
        request_example: None,
        response_example: Some("contract_capabilities"),
        errors: &[
            "authenticated_app_session_required",
            "stale_authenticated_request",
        ],
    },
    AppPublicOperationDescriptor {
        id: AppPublicOperationId::QueryData,
        method: AppHttpMethod::Post,
        path: "/installations/{installation_id}/data/query",
        summary: "Query admitted records owned by one installation.",
        auth: AppOperationAuthPosture::VerifiedOwnerScope,
        idempotency: AppOperationIdempotency::ReadOnly,
        parameters: &[INSTALLATION_PATH_PARAMETER],
        request_schema: Some("AppQueryRequest"),
        success_statuses: &[200],
        response_schema: "AppQueryPage",
        request_example: Some("query_request"),
        response_example: Some("query_page"),
        errors: &[
            "authenticated_app_session_required", "app_scope_mismatch",
            "app_workspace_required", "stale_authenticated_request",
            "invalid_authentication_revision", "loopback_peer_required",
            "app_authority_rejected", "invalid_app_contract",
            "unsupported_app_data_media_type", "app_data_body_too_large",
            "app_data_body_read_failed", "app_data_body_timeout",
            "app_installation_mismatch", "app_data_authority_rejected",
            "app_installation_not_found", "app_surface_stale",
            "app_data_projection_denied", "invalid_app_data_contract",
            "app_data_encoding_failed", "app_data_overloaded", "app_data_not_found",
            "app_data_cursor_unavailable", "app_data_stale", "invalid_app_data_cursor",
            "app_data_query_failed", "invalid_app_data_query", "app_data_limit_exceeded",
            "app_data_cursor_capacity", "app_data_keyset_index_required",
        ],
    },
    AppPublicOperationDescriptor {
        id: AppPublicOperationId::MutateData,
        method: AppHttpMethod::Post,
        path: "/installations/{installation_id}/data/mutations",
        summary: "Commit an idempotent optimistic mutation batch.",
        auth: AppOperationAuthPosture::VerifiedOwnerScope,
        idempotency: AppOperationIdempotency::ClientKeyed,
        parameters: &[INSTALLATION_PATH_PARAMETER],
        request_schema: Some("AppMutationCommand"),
        success_statuses: &[200],
        response_schema: "AppMutationReceipt",
        request_example: Some("mutation_command"),
        response_example: None,
        errors: &[
            "authenticated_app_session_required", "app_scope_mismatch",
            "app_workspace_required", "stale_authenticated_request",
            "invalid_authentication_revision", "loopback_peer_required",
            "app_authority_rejected", "invalid_app_contract",
            "unsupported_app_data_media_type", "app_data_body_too_large",
            "app_data_body_read_failed", "app_data_body_timeout",
            "app_data_authority_rejected", "app_installation_not_found",
            "app_surface_stale", "app_data_projection_denied",
            "invalid_app_data_contract", "app_data_encoding_failed", "app_data_overloaded",
            "app_data_not_found", "invalid_app_data_mutation", "app_data_conflict",
            "app_data_limit_exceeded", "app_data_mutation_failed",
        ],
    },
    AppPublicOperationDescriptor {
        id: AppPublicOperationId::LaunchAction,
        method: AppHttpMethod::Post,
        path: "/installations/{installation_id}/actions/{action_id}/runs",
        summary: "Launch an admitted action from minimal owner-supplied input.",
        auth: AppOperationAuthPosture::VerifiedOwnerScope,
        idempotency: AppOperationIdempotency::ClientKeyed,
        parameters: &[INSTALLATION_PATH_PARAMETER, ACTION_PATH_PARAMETER],
        request_schema: Some("AppDirectActionRequest"),
        success_statuses: &[202],
        response_schema: "AppActionLaunchResponse",
        request_example: None,
        response_example: Some("action_launch"),
        errors: &[
            "authenticated_app_session_required", "app_scope_mismatch",
            "app_workspace_required", "stale_authenticated_request",
            "invalid_authentication_revision", "loopback_peer_required",
            "app_authority_rejected", "unsupported_app_data_media_type",
            "app_data_body_too_large", "app_data_body_read_failed", "app_data_body_timeout",
            "invalid_app_contract", "app_workflow_not_found", "app_workflow_stale",
            "app_workflow_not_authorized", "app_workflow_invalid", "app_workflow_failed",
            "app_projection_too_large", "app_projection_expired",
            "app_projection_not_authorized", "app_projection_store_unavailable",
        ],
    },
    AppPublicOperationDescriptor {
        id: AppPublicOperationId::GetActionRun,
        method: AppHttpMethod::Get,
        path: "/action-runs/{run_ref}",
        summary: "Read the typed lifecycle and currently disclosable result for a logical run.",
        auth: AppOperationAuthPosture::VerifiedOwnerScope,
        idempotency: AppOperationIdempotency::ReadOnly,
        parameters: &[RUN_PATH_PARAMETER],
        request_schema: None,
        success_statuses: &[200, 202],
        response_schema: "AppRunSnapshot",
        request_example: None,
        response_example: Some("action_run_completed"),
        errors: &[
            "authenticated_app_session_required", "app_scope_mismatch",
            "app_workspace_required", "stale_authenticated_request",
            "invalid_authentication_revision", "loopback_peer_required",
            "app_authority_rejected", "invalid_app_contract",
            "app_workflow_not_found", "app_workflow_stale",
            "app_workflow_not_authorized", "app_workflow_invalid",
            "app_task_runtime_unavailable", "app_run_state_invalid",
            "app_run_identity_changed", "app_terminal_result_missing",
            "app_workflow_failed",
        ],
    },
    AppPublicOperationDescriptor {
        id: AppPublicOperationId::ComposeActionRun,
        method: AppHttpMethod::Post,
        path: "/action-runs/{run_ref}/compositions",
        summary: "Map one canonical action result through a bounded reviewed action chain and read cursor-bound progress.",
        auth: AppOperationAuthPosture::VerifiedOwnerScope,
        idempotency: AppOperationIdempotency::ClientKeyed,
        parameters: &[RUN_PATH_PARAMETER],
        request_schema: Some("AppActionCompositionRequest"),
        success_statuses: &[200, 202],
        response_schema: "AppActionResultComposition",
        request_example: Some("action_composition_request"),
        response_example: Some("action_composition_waiting"),
        errors: &[
            "authenticated_app_session_required", "app_scope_mismatch",
            "app_workspace_required", "stale_authenticated_request",
            "invalid_authentication_revision", "loopback_peer_required",
            "app_authority_rejected", "invalid_app_contract",
            "unsupported_app_data_media_type", "app_data_body_too_large",
            "app_data_body_read_failed", "app_data_body_timeout",
            "invalid_app_action_composition", "app_action_composition_not_authorized",
            "app_action_composition_unavailable", "app_action_composition_failed",
            "app_workflow_not_found", "app_workflow_stale",
            "app_workflow_not_authorized", "app_workflow_invalid",
            "app_task_runtime_unavailable", "app_workflow_failed",
        ],
    },
    AppPublicOperationDescriptor {
        id: AppPublicOperationId::CancelActionRun,
        method: AppHttpMethod::Post,
        path: "/action-runs/{run_ref}/cancel",
        summary: "Request generation-bound cancellation of one logical action run.",
        auth: AppOperationAuthPosture::VerifiedOwnerScope,
        idempotency: AppOperationIdempotency::ClientKeyed,
        parameters: &[RUN_PATH_PARAMETER],
        request_schema: Some("AppActionCancellationRequest"),
        success_statuses: &[200, 202],
        response_schema: "AppActionCancellationReceipt",
        request_example: Some("action_cancellation_request"),
        response_example: Some("action_cancellation_receipt"),
        errors: &[
            "authenticated_app_session_required", "app_scope_mismatch",
            "app_workspace_required", "stale_authenticated_request",
            "invalid_authentication_revision", "loopback_peer_required",
            "app_authority_rejected", "invalid_app_contract",
            "unsupported_app_action_cancellation_media_type",
            "app_action_cancellation_body_too_large",
            "app_action_cancellation_body_read_failed",
            "app_action_cancellation_body_timeout",
            "invalid_app_action_cancellation", "app_workflow_not_found",
            "app_workflow_stale", "app_workflow_not_authorized",
            "app_action_not_started", "app_action_already_completed",
            "app_action_cancellation_conflict", "app_task_runtime_unavailable",
            "app_action_cancellation_failed",
        ],
    },
    AppPublicOperationDescriptor {
        id: AppPublicOperationId::ReadEntityChanges,
        method: AppHttpMethod::Get,
        path: "/installations/{installation_id}/entity-changes",
        summary: "Read a bounded identifier-only durable change page.",
        auth: AppOperationAuthPosture::VerifiedOwnerScope,
        idempotency: AppOperationIdempotency::ReadOnly,
        parameters: &[
            INSTALLATION_PATH_PARAMETER,
            AFTER_CHANGE_SEQUENCE_PARAMETER,
            SURFACE_REVISION_PARAMETER,
            CHANGE_LIMIT_PARAMETER,
        ],
        request_schema: None,
        success_statuses: &[200],
        response_schema: "AppEntityChangeBatch",
        request_example: None,
        response_example: None,
        errors: &[
            "authenticated_app_session_required", "app_scope_mismatch",
            "app_workspace_required", "stale_authenticated_request",
            "invalid_authentication_revision", "loopback_peer_required",
            "app_authority_rejected", "invalid_app_contract", "app_surface_not_found",
            "app_surface_stale", "app_change_cursor_ahead", "invalid_app_change_limit",
            "app_surface_overloaded", "app_registry_failed", "app_entity_changes_failed",
        ],
    },
];

pub fn supported_public_operation(
    id: AppPublicOperationId,
) -> &'static AppPublicOperationDescriptor {
    SUPPORTED_PUBLIC_APP_OPERATIONS
        .iter()
        .find(|operation| operation.id == id)
        .expect("every public operation id used by route registration is inventoried")
}

pub fn public_operation_inventory() -> Vec<AppPublicOperation> {
    SUPPORTED_PUBLIC_APP_OPERATIONS
        .iter()
        .map(Into::into)
        .collect()
}

fn operation_inventory_digest(operations: &[AppPublicOperation]) -> String {
    let encoded =
        serde_json::to_vec(operations).expect("static public operation inventory is serializable");
    format!("blake3:{}", blake3::hash(&encoded).to_hex())
}

pub fn public_operation_inventory_digest() -> String {
    operation_inventory_digest(&public_operation_inventory())
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum AppManifestFeature {
    TypedEntitiesV1,
    DeclarativeViewsV1,
    GovernedActionsV1,
    ImmutableDependenciesV1,
    OwnerDataPlaneV1,
    DurableActionRunsV1,
    ContributionPortsV1,
    AttentionLanesV1,
    LlmOperationsV1,
    /// Scripted custom interactive surfaces (plan 1.6,
    /// `CustomInteractiveSurfacesV1`): static web assets from the
    /// package's own immutable `surfaces/` tree hosted in a sandboxed
    /// frame whose only authority is the eight supported-public bridge
    /// operations. Absent the feature, executable `surfaces/` members
    /// stay refused exactly as before — the feature is a review gate,
    /// never implicit.
    CustomSurfacesV1,
    /// Declarative, host-rendered app widgets and ambient indicators plus
    /// internal-app navigation metadata. The sibling distribution vocabulary
    /// is independent because a system ops package need not be surfaced. The
    /// feature adds review material only; render, evaluator, slot, and route
    /// consumers must still reconstruct live installation authority server-side.
    AppWidgetsV1,
    /// Durable host-executed scheduled behaviors. The manifest declaration is
    /// review material only: an enabled installation still needs an exact
    /// owner-narrowed behavior grant and every fire must reopen live authority.
    AppBehaviorsV1,
    /// Durable, host-projected first-party events may request an exact
    /// event-triggered behavior. The declaration is inert until an owner
    /// grants the complete source/projection/action/resource binding.
    AppEventBehaviorsV1,
    /// Workflow-local, one-way briefing/escalation ports into the owner's
    /// notification funnel. Questions and response authority are deliberately
    /// outside V1.
    AppOwnerNotificationsV1,
    /// Owner-controlled reads of the owner's memory (named user tiers and
    /// named agents' learned memory). The manifest block is only a request:
    /// the owner grants a subset, split between interactive and background
    /// runs, and can narrow or revoke it at any time.
    AppMemoryReadV1,
}

/// Package distribution claim. `System` is review material, never proof of
/// trusted provenance: upload/install paths must reject it unless a separate
/// host-controlled, digest-pinned boot admission has established the class.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    Serialize,
    Deserialize,
    JsonSchema,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum AppManifestDistribution {
    System,
    #[default]
    Installable,
}

/// Manifest-declared app permission (closed vocabulary).
///
/// A permission names a capability class the package requests; the
/// declaration block that gives it reviewable substance lives in the
/// manifest body next to the permission list (V1: `app.custom_surface`).
/// Unknown permission strings fail to decode — there is no
/// request-everything escape, and an unlisted capability behaves exactly
/// as it did before the vocabulary existed.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum AppManifestPermission {
    /// Host scripted `surfaces/` entry documents in a sandboxed frame
    /// behind the `custom_surfaces_v1` feature and the owner's
    /// per-entry-point review grant.
    CustomSurface,
}

impl AppManifestPermission {
    pub const fn supported() -> &'static [Self] {
        &[Self::CustomSurface]
    }
}

/// Informational generator identity. It is never accepted as platform
/// capability, authority, admission evidence, or a compatibility decision.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestGeneratedBy {
    pub sdk: String,
    pub version: String,
}

impl AppManifestFeature {
    pub const fn supported() -> &'static [Self] {
        &[
            Self::TypedEntitiesV1,
            Self::DeclarativeViewsV1,
            Self::GovernedActionsV1,
            Self::ImmutableDependenciesV1,
            Self::OwnerDataPlaneV1,
            Self::DurableActionRunsV1,
            Self::ContributionPortsV1,
            Self::AttentionLanesV1,
            Self::LlmOperationsV1,
            Self::CustomSurfacesV1,
            Self::AppWidgetsV1,
            Self::AppBehaviorsV1,
            Self::AppEventBehaviorsV1,
            Self::AppOwnerNotificationsV1,
            Self::AppMemoryReadV1,
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AppContractCapabilityLimits {
    pub max_document_bytes: u64,
    pub max_json_depth: u64,
    pub max_json_nodes: u64,
    pub max_value_bytes: u64,
    pub max_value_nodes: u64,
    pub max_collection_items: u64,
    pub max_predicate_nodes: u64,
    pub max_predicate_depth: u64,
    pub max_page_rows: u64,
    pub max_entity_change_page_rows: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppSdkCompatibilityPolicy {
    /// Until the first stable SDK ships, only the current contract is a
    /// supported generation target. This must be widened deliberately.
    #[serde(alias = "current_contract_only_pre_release")]
    CurrentContractOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AppSdkCompatibilityWindow {
    pub policy: AppSdkCompatibilityPolicy,
    pub supported_contract_versions: Vec<String>,
    pub generated_by_is_authority: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AppContractDeprecation {
    pub field: String,
    pub replacement: String,
    pub removal_contract_major: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AppContractCapabilities {
    pub schema_version: u32,
    pub contract_version: String,
    pub supported_protocol_versions: Vec<AppProtocolVersion>,
    pub supported_manifest_schema_versions: Vec<String>,
    pub supported_manifest_features: Vec<AppManifestFeature>,
    pub json_schema_dialect: String,
    pub limits: AppContractCapabilityLimits,
    pub sdk_compatibility: AppSdkCompatibilityWindow,
    pub deprecations: Vec<AppContractDeprecation>,
    pub operation_inventory_digest: String,
    pub operations: Vec<AppPublicOperation>,
}

impl AppContractCapabilities {
    pub fn current(limits: AppContractCapabilityLimits) -> Self {
        Self {
            schema_version: 1,
            contract_version: APP_SUPPORTED_PUBLIC_CONTRACT_VERSION.to_owned(),
            supported_protocol_versions: AppProtocolVersion::supported().to_vec(),
            supported_manifest_schema_versions: vec![APP_MANIFEST_SCHEMA_VERSION.to_owned()],
            supported_manifest_features: AppManifestFeature::supported().to_vec(),
            json_schema_dialect: APP_JSON_SCHEMA_DIALECT.to_owned(),
            limits,
            sdk_compatibility: AppSdkCompatibilityWindow {
                policy: AppSdkCompatibilityPolicy::CurrentContractOnly,
                supported_contract_versions: vec![APP_SUPPORTED_PUBLIC_CONTRACT_VERSION.to_owned()],
                generated_by_is_authority: false,
            },
            deprecations: vec![AppContractDeprecation {
                field: "metadata.magician.app_sdk_version".to_owned(),
                replacement: "metadata.magician.required_features for compatibility; \
                              metadata.magician.generated_by for diagnostics"
                    .to_owned(),
                removal_contract_major: None,
            }],
            operation_inventory_digest: public_operation_inventory_digest(),
            operations: public_operation_inventory(),
        }
    }

    pub fn validate_static_inventory(&self) -> bool {
        let ids = self
            .operations
            .iter()
            .map(|operation| operation.operation_id.as_str())
            .collect::<HashSet<_>>();
        let canonical = public_operation_inventory();
        ids.len() == self.operations.len()
            && self.operations == canonical
            && self.operation_inventory_digest == operation_inventory_digest(&self.operations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_no_authority_keys(value: &serde_json::Value) {
        match value {
            serde_json::Value::Array(values) => {
                for value in values {
                    assert_no_authority_keys(value);
                }
            },
            serde_json::Value::Object(object) => {
                for key in object.keys() {
                    assert!(
                        !matches!(
                            key.as_str(),
                            "principal"
                                | "workspace"
                                | "grant_revision"
                                | "task_id"
                                | "credential"
                                | "access_token"
                        ),
                        "capability response exposes authority-bearing key `{key}`"
                    );
                }
                for value in object.values() {
                    assert_no_authority_keys(value);
                }
            },
            _ => {},
        }
    }

    fn limits() -> AppContractCapabilityLimits {
        AppContractCapabilityLimits {
            max_document_bytes: 1_048_576,
            max_json_depth: 32,
            max_json_nodes: 20_000,
            max_value_bytes: 262_144,
            max_value_nodes: 8_000,
            max_collection_items: 256,
            max_predicate_nodes: 128,
            max_predicate_depth: 16,
            max_page_rows: 200,
            max_entity_change_page_rows: 128,
        }
    }

    #[test]
    fn supported_public_operation_identity_and_route_pairs_are_unique() {
        let mut ids = HashSet::new();
        let mut routes = HashSet::new();
        for operation in SUPPORTED_PUBLIC_APP_OPERATIONS {
            assert!(ids.insert(operation.id.as_str()));
            assert!(routes.insert((operation.method, operation.path)));
            assert!(!operation.errors.is_empty());
            assert!(!operation.success_statuses.is_empty());
            assert!(operation
                .success_statuses
                .iter()
                .all(|status| (200..300).contains(status)));
        }
    }

    #[test]
    fn public_action_run_namespace_never_accepts_a_bare_task_locator() {
        assert!(has_canonical_app_action_run_namespace(
            "run:app-action:task_app_fixture"
        ));
        assert!(!has_canonical_app_action_run_namespace("task_app_fixture"));
        assert!(!has_canonical_app_action_run_namespace(
            APP_ACTION_RUN_REF_PREFIX
        ));
    }

    #[test]
    fn capabilities_are_an_exact_projection_of_the_static_inventory() {
        let capabilities = AppContractCapabilities::current(limits());
        assert_eq!(APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION, "1.0.0");
        assert_eq!(APP_SUPPORTED_PUBLIC_CONTRACT_VERSION, "1.5.0");
        assert_eq!(capabilities.contract_version, "1.5.0");
        assert_eq!(
            capabilities.sdk_compatibility.supported_contract_versions,
            vec!["1.5.0".to_owned()]
        );
        assert!(capabilities.validate_static_inventory());
        assert_eq!(
            capabilities.operations.len(),
            SUPPORTED_PUBLIC_APP_OPERATIONS.len()
        );
        assert!(!capabilities.sdk_compatibility.generated_by_is_authority);
        assert_eq!(
            capabilities.sdk_compatibility.policy,
            AppSdkCompatibilityPolicy::CurrentContractOnly
        );
        assert_eq!(
            serde_json::to_value(&capabilities.sdk_compatibility.policy).unwrap(),
            "current_contract_only"
        );
        assert_no_authority_keys(&serde_json::to_value(capabilities).unwrap());
    }

    #[test]
    fn capabilities_reject_tampered_operation_metadata() {
        let mut capabilities = AppContractCapabilities::current(limits());
        capabilities.operations[0].path = "/different-path".to_owned();
        capabilities.operation_inventory_digest =
            operation_inventory_digest(&capabilities.operations);

        assert!(!capabilities.validate_static_inventory());
    }

    #[test]
    fn legacy_http_error_readers_may_ignore_additive_fields() {
        let error: AppLegacyHttpErrorResponse = serde_json::from_value(serde_json::json!({
            "error": "app_data_unavailable",
            "message": "Unavailable.",
            "future_diagnostic": "ignored"
        }))
        .unwrap();
        assert_eq!(error.error, "app_data_unavailable");
    }

    #[test]
    fn capability_readers_may_ignore_additive_top_level_fields() {
        let mut value = serde_json::to_value(AppContractCapabilities::current(limits())).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future_addition".to_owned(), serde_json::json!(true));
        let decoded: AppContractCapabilities = serde_json::from_value(value).unwrap();
        assert!(decoded.validate_static_inventory());
    }

    #[test]
    fn unknown_capability_enums_require_an_explicit_contract_upgrade() {
        let mut value = serde_json::to_value(AppContractCapabilities::current(limits())).unwrap();
        value["supported_manifest_features"][0] =
            serde_json::json!("future_unnegotiated_feature_v9");

        assert!(serde_json::from_value::<AppContractCapabilities>(value).is_err());
    }

    #[test]
    fn llm_operations_feature_is_additive_and_round_trips_its_wire_name() {
        let feature: AppManifestFeature =
            serde_json::from_value(serde_json::json!("llm_operations_v1")).unwrap();
        assert_eq!(feature, AppManifestFeature::LlmOperationsV1);
        assert_eq!(
            serde_json::to_value(AppManifestFeature::LlmOperationsV1).unwrap(),
            serde_json::json!("llm_operations_v1")
        );
        assert!(AppManifestFeature::supported().contains(&AppManifestFeature::LlmOperationsV1));
        // Fail-closed wire admission: an unnegotiated future variant must
        // not decode as a feature.
        let rejected =
            serde_json::from_value::<AppManifestFeature>(serde_json::json!("llm_operations_v2"));
        assert!(rejected.is_err());
    }

    #[test]
    fn custom_surfaces_feature_is_additive_and_round_trips_its_wire_name() {
        let feature: AppManifestFeature =
            serde_json::from_value(serde_json::json!("custom_surfaces_v1")).unwrap();
        assert_eq!(feature, AppManifestFeature::CustomSurfacesV1);
        assert_eq!(
            serde_json::to_value(AppManifestFeature::CustomSurfacesV1).unwrap(),
            serde_json::json!("custom_surfaces_v1")
        );
        assert!(AppManifestFeature::supported().contains(&AppManifestFeature::CustomSurfacesV1));
        // Fail-closed wire admission: an unnegotiated variant (including
        // the unversioned bare permission word) must not decode as a
        // feature.
        for rejected in ["custom_surfaces_v2", "custom_surface", "custom_surfaces"] {
            assert!(
                serde_json::from_value::<AppManifestFeature>(serde_json::json!(rejected)).is_err(),
                "{rejected}"
            );
        }
    }

    #[test]
    fn app_widgets_feature_and_distribution_are_closed_wire_vocabularies() {
        let feature: AppManifestFeature =
            serde_json::from_value(serde_json::json!("app_widgets_v1")).unwrap();
        assert_eq!(feature, AppManifestFeature::AppWidgetsV1);
        assert_eq!(
            serde_json::to_value(AppManifestFeature::AppWidgetsV1).unwrap(),
            serde_json::json!("app_widgets_v1")
        );
        assert!(AppManifestFeature::supported().contains(&AppManifestFeature::AppWidgetsV1));
        for rejected in ["app_widgets", "app_widgets_v2", "widgets_v1"] {
            assert!(
                serde_json::from_value::<AppManifestFeature>(serde_json::json!(rejected)).is_err(),
                "{rejected}"
            );
        }

        assert_eq!(
            AppManifestDistribution::default(),
            AppManifestDistribution::Installable
        );
        for (wire, expected) in [
            ("installable", AppManifestDistribution::Installable),
            ("system", AppManifestDistribution::System),
        ] {
            let decoded: AppManifestDistribution =
                serde_json::from_value(serde_json::json!(wire)).unwrap();
            assert_eq!(decoded, expected);
            assert_eq!(
                serde_json::to_value(decoded).unwrap(),
                serde_json::json!(wire)
            );
        }
        for rejected in ["internal", "bundled", "system_v1"] {
            assert!(
                serde_json::from_value::<AppManifestDistribution>(serde_json::json!(rejected))
                    .is_err(),
                "{rejected}"
            );
        }
    }

    #[test]
    fn app_behaviors_feature_is_additive_and_closed() {
        let feature: AppManifestFeature =
            serde_json::from_value(serde_json::json!("app_behaviors_v1")).unwrap();
        assert_eq!(feature, AppManifestFeature::AppBehaviorsV1);
        assert!(AppManifestFeature::supported().contains(&feature));
        for rejected in ["app_behaviors", "app_behaviors_v2", "behaviors_v1"] {
            assert!(
                serde_json::from_value::<AppManifestFeature>(serde_json::json!(rejected)).is_err()
            );
        }
    }

    #[test]
    fn event_behavior_and_owner_notification_features_are_independent_closed_vocabularies() {
        for (wire, expected) in [
            (
                "app_event_behaviors_v1",
                AppManifestFeature::AppEventBehaviorsV1,
            ),
            (
                "app_owner_notifications_v1",
                AppManifestFeature::AppOwnerNotificationsV1,
            ),
        ] {
            let feature: AppManifestFeature =
                serde_json::from_value(serde_json::json!(wire)).unwrap();
            assert_eq!(feature, expected);
            assert!(AppManifestFeature::supported().contains(&feature));
        }
        for rejected in [
            "app_events_v1",
            "app_event_behaviors_v2",
            "owner_notifications_v1",
        ] {
            assert!(
                serde_json::from_value::<AppManifestFeature>(serde_json::json!(rejected)).is_err()
            );
        }
    }

    #[test]
    fn manifest_permissions_are_a_closed_vocabulary() {
        let permission: AppManifestPermission =
            serde_json::from_value(serde_json::json!("custom_surface")).unwrap();
        assert_eq!(permission, AppManifestPermission::CustomSurface);
        assert_eq!(
            serde_json::to_value(AppManifestPermission::CustomSurface).unwrap(),
            serde_json::json!("custom_surface")
        );
        assert_eq!(
            AppManifestPermission::supported(),
            &[AppManifestPermission::CustomSurface]
        );
        // Unknown permissions — including the feature wire name, which is
        // a different axis — fail closed instead of decoding as something.
        for rejected in [
            "custom_surfaces_v1",
            "network",
            "browser",
            "custom_surface_v2",
        ] {
            assert!(
                serde_json::from_value::<AppManifestPermission>(serde_json::json!(rejected))
                    .is_err(),
                "{rejected}"
            );
        }
    }
}

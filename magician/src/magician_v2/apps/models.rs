//! Canonical, transport-neutral app data-plane records.
//!
//! The wire contract is intentionally flat where user input could otherwise
//! create recursive Rust values. In particular, query predicates use an arena
//! of indexed nodes so validation and destruction do not consume the native
//! call stack.

use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    num::NonZeroU64,
};

use chrono::{DateTime, Utc};
pub use magician_app_contract::{
    has_canonical_app_action_run_namespace, AppErrorCode, AppErrorDisposition, AppProtocolVersion,
    APP_ACTION_RUN_REF_PREFIX, APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION,
    APP_SUPPORTED_PUBLIC_CONTRACT_VERSION,
};
use schemars::JsonSchema;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[cfg(any(test, feature = "test-fixtures"))]
use crate::magician_v2::json_traversal::canonical_json_bytes;
use crate::magician_v2::json_traversal::{
    canonical_json_blake3_hex, exact_json_encoded_len, inspect_json_bounded,
    json_bytes_nesting_is_bounded, json_bytes_nodes_are_bounded,
};

/// A positive, monotonically increasing contract revision.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(transparent)]
pub struct AppRevision(NonZeroU64);

impl AppRevision {
    pub fn new(value: u64) -> Result<Self, AppContractError> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or_else(|| AppContractError::invalid("revision", "must be greater than zero"))
    }

    pub fn get(self) -> u64 {
        self.0.get()
    }
}

macro_rules! bounded_string_type {
    ($name:ident, $label:literal, $validator:ident) => {
        #[derive(
            Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
        )]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: impl Into<String>) -> Result<Self, AppContractError> {
                let value = value.into();
                $validator($label, &value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = AppContractError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::parse(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

bounded_string_type!(AppInstallationId, "installation_id", validate_opaque_id);
bounded_string_type!(AppScopeBindingRef, "scope_binding_ref", validate_opaque_id);
bounded_string_type!(AppRecordId, "record_id", validate_opaque_id);
bounded_string_type!(AppReference, "reference", validate_reference);
bounded_string_type!(AppDigest, "digest", validate_digest);
bounded_string_type!(AppName, "name", validate_name);
bounded_string_type!(AppFieldPath, "field_path", validate_field_path);

impl AppDigest {
    /// Construct the canonical digest representation used by app contracts.
    pub fn blake3(bytes: &[u8]) -> Self {
        Self(format!("blake3:{}", blake3::hash(bytes).to_hex()))
    }

    pub fn blake3_canonical_json(value: &Value) -> Result<Self, serde_json::Error> {
        canonical_json_blake3_hex(value).map(|hex| Self(format!("blake3:{hex}")))
    }
}

fn validate_opaque_id(field: &'static str, value: &str) -> Result<(), AppContractError> {
    validate_ascii_token(field, value, 128, false)
}

fn validate_reference(field: &'static str, value: &str) -> Result<(), AppContractError> {
    validate_ascii_token(field, value, 192, true)
}

fn validate_digest(field: &'static str, value: &str) -> Result<(), AppContractError> {
    let Some(hex) = value.strip_prefix("blake3:") else {
        return Err(AppContractError::invalid(
            field,
            "must use the canonical blake3:<64 lowercase hex> form",
        ));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(AppContractError::invalid(
            field,
            "must use the canonical blake3:<64 lowercase hex> form",
        ));
    }
    Ok(())
}

fn validate_name(field: &'static str, value: &str) -> Result<(), AppContractError> {
    if value.is_empty() || value.len() > 64 {
        return Err(AppContractError::invalid(
            field,
            "must contain between 1 and 64 bytes",
        ));
    }
    let mut bytes = value.bytes();
    if !bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(AppContractError::invalid(
            field,
            "must begin with an ASCII letter or digit and contain only letters, digits, '_' or '-'",
        ));
    }
    Ok(())
}

fn validate_field_path(field: &'static str, value: &str) -> Result<(), AppContractError> {
    if value.is_empty() || value.len() > 256 {
        return Err(AppContractError::invalid(
            field,
            "must contain between 1 and 256 bytes",
        ));
    }
    let mut segment_count = 0usize;
    for segment in value.split('.') {
        segment_count = segment_count.saturating_add(1);
        validate_name(field, segment)?;
    }
    if segment_count > 16 {
        return Err(AppContractError::invalid(
            field,
            "must not contain more than 16 segments",
        ));
    }
    Ok(())
}

fn validate_ascii_token(
    field: &'static str,
    value: &str,
    max_bytes: usize,
    extended: bool,
) -> Result<(), AppContractError> {
    if value.is_empty() || value.len() > max_bytes {
        return Err(AppContractError::invalid(
            field,
            format!("must contain between 1 and {max_bytes} bytes"),
        ));
    }
    let mut bytes = value.bytes();
    if !bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(AppContractError::invalid(
            field,
            "must begin with an ASCII letter or digit",
        ));
    }
    let valid = bytes.all(|byte| {
        byte.is_ascii_alphanumeric()
            || matches!(byte, b'_' | b'-' | b'.')
            || (extended && matches!(byte, b':' | b'/' | b'@' | b'#'))
    });
    if !valid {
        return Err(AppContractError::invalid(
            field,
            "contains unsupported characters",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppDataSource {
    UserInput,
    AppStore,
    AppAction,
    ArtifactProjection,
    ExternalAdapter,
    Import,
    BrokeredTransfer,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum AppDataClassification {
    Public,
    Ordinary,
    Personal,
    Sensitive,
    Secret,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum AppModelProcessing {
    None,
    LocalOnly,
    RemoteAllowed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppHandlingLabels {
    pub classification: AppDataClassification,
    pub model_processing: AppModelProcessing,
    pub policy_digest: AppDigest,
    pub provenance_digest: AppDigest,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppSourceRefKind {
    EntityRecord,
    EntityField,
    Artifact,
    ExternalReceipt,
    MutationReceipt,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppSourceRef {
    pub kind: AppSourceRefKind,
    pub reference: AppReference,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<AppRevision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<AppFieldPath>,
}

/// Canonical data/provenance carrier. This contains no bearer authority;
/// adapters must independently resolve `scope_binding_ref` and every revision.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppDataEnvelope<T> {
    pub protocol_version: AppProtocolVersion,
    pub source: AppDataSource,
    pub scope_binding_ref: AppScopeBindingRef,
    pub installation_id: AppInstallationId,
    pub package_revision_ref: AppReference,
    pub schema_revision: AppRevision,
    pub grant_revision: AppRevision,
    pub value_schema_ref: AppReference,
    pub value: T,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<AppSourceRef>,
    pub handling_labels: AppHandlingLabels,
    pub content_digest: AppDigest,
    pub produced_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

/// Artifact V2 projection carried as the value of an [`AppDataEnvelope`].
///
/// The surrounding envelope supplies scope, handling labels and provenance;
/// this descriptor supplies the exact immutable artifact, media and schema
/// identity. An artifact reference embedded in arbitrary JSON is not an
/// equivalent contract.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppArtifactProjection {
    pub artifact_ref: AppReference,
    pub artifact_revision: AppRevision,
    pub media_type: String,
    pub byte_len: u64,
    pub content_digest: AppDigest,
    pub value_schema_ref: AppReference,
}

/// A non-recursive, bounded query predicate arena.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppPredicate {
    pub root: u16,
    pub nodes: Vec<AppPredicateNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppPredicateNode {
    All {
        children: Vec<u16>,
    },
    Any {
        children: Vec<u16>,
    },
    Not {
        child: u16,
    },
    Compare {
        field: AppFieldPath,
        operator: AppComparisonOperator,
        value: Value,
    },
    In {
        field: AppFieldPath,
        values: Vec<Value>,
    },
    IsNull {
        field: AppFieldPath,
        #[serde(default)]
        negated: bool,
    },
}

impl AppPredicateNode {
    fn child_count(&self) -> usize {
        match self {
            Self::All { children } | Self::Any { children } => children.len(),
            Self::Not { .. } => 1,
            Self::Compare { .. } | Self::In { .. } | Self::IsNull { .. } => 0,
        }
    }

    fn child_at(&self, index: usize) -> Option<usize> {
        match self {
            Self::All { children } | Self::Any { children } => {
                children.get(index).copied().map(usize::from)
            },
            Self::Not { child } if index == 0 => Some(usize::from(*child)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppComparisonOperator {
    Equal,
    NotEqual,
    LessThan,
    LessThanOrEqual,
    GreaterThan,
    GreaterThanOrEqual,
    Contains,
    StartsWith,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppOrderDirection {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppQueryOrder {
    pub field: AppFieldPath,
    pub direction: AppOrderDirection,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRelationExpansion {
    pub relation: AppName,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub select: Vec<AppFieldPath>,
    pub max_depth: u16,
    pub max_rows: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppQueryRequest {
    /// Live keyset pages seek through current indexed heads; snapshots freeze membership.
    #[serde(default, skip_serializing_if = "AppQueryPagination::is_snapshot")]
    pub pagination: AppQueryPagination,
    pub protocol_version: AppProtocolVersion,
    pub source_installation_id: AppInstallationId,
    pub entity: AppName,
    pub select: Vec<AppFieldPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<AppPredicate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<AppQueryOrder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<AppReference>,
    pub limit: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relation_expansions: Vec<AppRelationExpansion>,
    pub purpose: AppName,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppQueryPagination {
    #[default]
    Snapshot,
    Keyset,
}

impl AppQueryPagination {
    fn is_snapshot(&self) -> bool {
        *self == Self::Snapshot
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppRecordProjection {
    pub entity: AppName,
    pub record_id: AppRecordId,
    pub record_revision: AppRevision,
    pub fields: BTreeMap<AppFieldPath, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppQueryPage {
    pub envelope: AppDataEnvelope<Vec<AppRecordProjection>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<AppReference>,
    pub result_schema_ref: AppReference,
}

/// Short-lived reference to a server-held query projection. It is an opaque
/// rejection token, not bearer authority: every resolver also requires the
/// original authenticated scope/execution and current store revalidation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppProjectionHandle {
    pub handle_ref: AppReference,
    pub expires_at: DateTime<Utc>,
}

/// Personal-agent query/search result. The bounded page remains useful for
/// reasoning and display, while composition passes the small handle instead
/// of echoing the complete page back through model arguments.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppPersonalAgentQueryResult {
    pub projection_handle: AppProjectionHandle,
    pub page: AppQueryPage,
}

/// Exact record snapshot sealed into a brokered-transfer receipt. Field values
/// stay out of the receipt; their canonical digest lets the source store prove
/// that the current head still matches before each consequential B effect.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppSourceRecordFence {
    pub entity: AppName,
    pub record_id: AppRecordId,
    pub record_revision: AppRevision,
    /// Fields whose selected values were transferred and whose exact values
    /// are sealed below.
    pub fields: Vec<AppFieldPath>,
    /// Complete root-query policy influence set: selected, predicate and order
    /// fields. This may be wider than `fields`; it carries no values but lets
    /// later workflow boundaries recompute the same source policy taint.
    #[serde(default)]
    pub policy_influence_fields: Vec<AppFieldPath>,
    pub selected_values_digest: AppDigest,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppExpectedRecordRevision {
    pub entity: AppName,
    pub record_id: AppRecordId,
    pub revision: AppRevision,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppMutationOperation {
    Create {
        entity: AppName,
        temporary_id: AppName,
        /// Caller-chosen record id, when the caller needs to be able to name
        /// the row again later.
        ///
        /// Omitted — the default and the norm — the store mints
        /// `rec_<hex>` from the mutation key and the `temporary_id`. That id is
        /// stable under replay, which is what makes a retried create an exact
        /// replay rather than a second row, but it is unknowable to whoever
        /// wrote the package: it depends on the mutation key.
        ///
        /// That is a problem for exactly one shape, and it is why this field
        /// exists. A declared behavior resolves its input by record id
        /// (`AppManifestBehaviorInputSelector`), so a package that owns a
        /// singleton must be able to write a row it can name in its own
        /// manifest. Without this, no literal an author can spell ever
        /// resolves, and the behavior fails to find its source record forever.
        ///
        /// Naming a row does not widen what a caller may write. The id is
        /// still confined to an entity the caller is authorized to mutate, a
        /// create against an id that already exists is refused rather than
        /// treated as an overwrite, and the host-minted `rec_` namespace is
        /// reserved so a caller cannot aim at an id the store is about to mint.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        record_id: Option<AppRecordId>,
        payload: Value,
    },
    Update {
        entity: AppName,
        record_id: AppRecordId,
        patch: Value,
    },
    Delete {
        entity: AppName,
        record_id: AppRecordId,
    },
    Restore {
        entity: AppName,
        record_id: AppRecordId,
    },
    CreateRelation {
        relation: AppName,
        from_record_id: AppRecordId,
        to_record_id: AppRecordId,
        expected_from_revision: AppRevision,
        expected_to_revision: AppRevision,
    },
    DeleteRelation {
        relation: AppName,
        from_record_id: AppRecordId,
        to_record_id: AppRecordId,
        expected_from_revision: AppRevision,
        expected_to_revision: AppRevision,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppMutationAtomicity {
    AllOrNothing,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppMutationCommand {
    pub protocol_version: AppProtocolVersion,
    pub idempotency_key: AppReference,
    pub atomicity: AppMutationAtomicity,
    pub expected_schema_revision: AppRevision,
    pub operations: Vec<AppMutationOperation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_record_revisions: Vec<AppExpectedRecordRevision>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppActionInvocation<I> {
    pub protocol_version: AppProtocolVersion,
    pub idempotency_key: AppReference,
    pub action_id: AppName,
    pub action_revision: AppRevision,
    pub input: AppDataEnvelope<I>,
    pub requested_result_schema_ref: AppReference,
    pub caller_surface_or_execution_ref: AppReference,
}

/// Optional client-observed installation precondition for a direct app action.
/// It carries no authority: the server compares it with live registry state or
/// an exact sealed replay and still owns every admitted revision and policy.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDirectActionExpectedInstallationBinding {
    pub generation: u64,
    pub package_revision_ref: AppReference,
}

/// Minimal owner-client request for a direct app action. Client-selected
/// schema, grant, policy and provenance fields are intentionally absent; the
/// optional installation binding is only a stale-render precondition. The
/// server resolves and rechecks all executable authority at admission.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppDirectActionRequest {
    pub idempotency_key: AppReference,
    pub input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_installation_binding: Option<AppDirectActionExpectedInstallationBinding>,
}

/// Stable server-owned identity for one logical app action run.
///
/// The handle contains no authority: every consumer must authenticate its
/// scope and reopen the durable workflow binding for status/control. Returning
/// result bytes additionally requires current installation/grant/schema and
/// source-policy revalidation. `run_ref` remains stable when the runtime
/// creates a replacement or delegated execution for the same idempotent action.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct AppRunHandle {
    pub protocol_version: AppProtocolVersion,
    pub run_ref: AppReference,
    pub installation_id: AppInstallationId,
    pub action_id: AppName,
}

/// Supported-public launch response for one logical app action.
///
/// Internal task identifiers are deliberately absent. A client correlates,
/// polls, and controls the action exclusively through the opaque run handle;
/// execution identity is diagnostic and may change across retry/delegation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppActionLaunchResponse<O> {
    pub run_handle: AppRunHandle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<AppActionResult<O>>,
}

/// Stable public lifecycle for one logical app run.
///
/// Artifact V2 owns a richer internal execution state machine. This smaller
/// enum is the app protocol projection: clients never infer terminality from
/// an HTTP status code or from an open-ended internal status string.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppRunStatus {
    Queued,
    Planning,
    Running,
    Paused,
    Deferred,
    Waiting,
    Blocked,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
    Archived,
    Uncertain,
}

impl AppRunStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Archived | Self::Uncertain
        )
    }
}

/// Project the closed Artifact V2 task lifecycle vocabulary into the public
/// app-run protocol. Keeping this mapping in the contract kernel prevents
/// status, composition and control adapters from assigning different
/// terminality to the same durable task state.
pub fn app_run_status_from_task(status: &str) -> Option<AppRunStatus> {
    Some(match status {
        "pending" | "ready" | "queued" | "starting" => AppRunStatus::Queued,
        "planning" => AppRunStatus::Planning,
        "running" => AppRunStatus::Running,
        "paused" => AppRunStatus::Paused,
        "deferred" => AppRunStatus::Deferred,
        value if value.starts_with("waiting_") || value == "waiting" => AppRunStatus::Waiting,
        "blocked" => AppRunStatus::Blocked,
        "cancelling" | "cancel_requested" | "cancellation_requested" => AppRunStatus::Cancelling,
        "completed" => AppRunStatus::Completed,
        "failed" => AppRunStatus::Failed,
        "cancelled" | "canceled" => AppRunStatus::Cancelled,
        "archived" => AppRunStatus::Archived,
        "uncertain" => AppRunStatus::Uncertain,
        _ => return None,
    })
}

/// Canonical status/result projection returned by every app-run read.
///
/// `task_id` is deliberately absent. It remains only on the legacy launch
/// adapter; this response is addressed and correlated exclusively by
/// `run_handle`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppRunSnapshot<O> {
    pub protocol_version: AppProtocolVersion,
    pub run_handle: AppRunHandle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    pub status: AppRunStatus,
    pub terminal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation_generation: Option<u64>,
    /// A terminal result exists but current app/source policy forbids
    /// disclosing its bytes through this request. This is distinct from a
    /// missing/corrupt result and lets status remain available after
    /// revocation without weakening the result boundary.
    pub result_withheld: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<AppActionResult<O>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppActionStatus {
    Completed,
    Waiting,
    Failed,
    Uncertain,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppActionResult<O> {
    pub protocol_version: AppProtocolVersion,
    pub action_id: AppName,
    /// Stable logical run identity, never a particular execution attempt.
    pub run_ref: AppReference,
    pub status: AppActionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<AppDataEnvelope<O>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mutation_receipt_refs: Vec<AppReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_effect_receipt_refs: Vec<AppReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AppErrorEnvelope>,
}

/// Model-facing projection of a canonical app result.
///
/// The durable [`AppActionResult`] deliberately retains complete provenance,
/// source references, handling labels and effect receipts for replay and audit.
/// A composition tool must not serialize that internal envelope merely because
/// the result value itself is eligible for the calling model. This projection
/// carries only the typed value and stable lifecycle identity; the canonical
/// result remains available through the policy-aware app-run owner.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppModelActionResult<O> {
    pub protocol_version: AppProtocolVersion,
    pub action_id: AppName,
    pub run_ref: AppReference,
    pub status: AppActionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<O>,
    /// Content-free signal that the canonical result sealed at least one
    /// mutation or external-effect receipt. Receipt identities stay internal.
    pub effect_committed: bool,
}

impl<O> AppActionResult<O> {
    pub fn into_model_projection(self) -> AppModelActionResult<O> {
        AppModelActionResult {
            protocol_version: self.protocol_version,
            action_id: self.action_id,
            run_ref: self.run_ref,
            status: self.status,
            output: self.output.map(|output| output.value),
            effect_committed: !self.mutation_receipt_refs.is_empty()
                || !self.external_effect_receipt_refs.is_empty(),
        }
    }
}

/// Bounded, machine-interpretable failure detail shared by every app adapter.
/// `message` is display context, never the semantic error contract.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppErrorEnvelope {
    pub code: AppErrorCode,
    pub disposition: AppErrorDisposition,
    pub message: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<AppName, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

/// Provider-free admission ceilings shared by every transport adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppContractLimits {
    max_document_bytes: usize,
    max_json_depth: usize,
    max_json_nodes: usize,
    max_value_bytes: usize,
    max_value_nodes: usize,
    max_collection_items: usize,
    max_predicate_nodes: usize,
    max_predicate_depth: usize,
    max_page_rows: usize,
}

impl Default for AppContractLimits {
    fn default() -> Self {
        Self {
            max_document_bytes: 1_048_576,
            max_json_depth: 32,
            max_json_nodes: 20_000,
            max_value_bytes: 262_144,
            max_value_nodes: 8_000,
            max_collection_items: 256,
            max_predicate_nodes: 128,
            max_predicate_depth: 16,
            max_page_rows: 200,
        }
    }
}

impl AppContractLimits {
    pub const fn max_document_bytes(&self) -> usize {
        self.max_document_bytes
    }

    pub const fn max_json_depth(&self) -> usize {
        self.max_json_depth
    }

    pub const fn max_json_nodes(&self) -> usize {
        self.max_json_nodes
    }

    pub const fn max_value_bytes(&self) -> usize {
        self.max_value_bytes
    }

    pub const fn max_value_nodes(&self) -> usize {
        self.max_value_nodes
    }

    pub const fn max_collection_items(&self) -> usize {
        self.max_collection_items
    }

    pub const fn max_predicate_nodes(&self) -> usize {
        self.max_predicate_nodes
    }

    pub const fn max_predicate_depth(&self) -> usize {
        self.max_predicate_depth
    }

    pub const fn max_page_rows(&self) -> usize {
        self.max_page_rows
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn with_max_collection_items_for_test(mut self, value: usize) -> Self {
        assert!(value <= Self::default().max_collection_items);
        self.max_collection_items = value;
        self
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn with_max_value_nodes_for_test(mut self, value: usize) -> Self {
        assert!(value <= Self::default().max_value_nodes);
        self.max_value_nodes = value;
        self
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppContractError {
    #[error("app contract exceeds the {limit} byte document ceiling")]
    DocumentTooLarge { limit: usize },
    #[error(
        "app contract exceeds the {limit} level JSON depth ceiling or is structurally malformed"
    )]
    JsonDepthExceeded { limit: usize },
    #[error("app contract exceeds the {limit} JSON node ceiling")]
    JsonNodeLimitExceeded { limit: usize },
    #[error("app contract JSON is invalid: {message}")]
    InvalidJson { message: String },
    #[error("invalid app contract field `{field}`: {message}")]
    InvalidField {
        field: &'static str,
        message: String,
    },
}

impl AppContractError {
    pub fn invalid(field: &'static str, message: impl Into<String>) -> Self {
        Self::InvalidField {
            field,
            message: message.into(),
        }
    }
}

pub trait ValidateAppContract {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError>;
}

/// Decode only after an iterative byte-level depth/node preflight. This keeps
/// recursive Serde work within the app contract's small admitted depth.
pub fn decode_app_contract<T>(
    bytes: &[u8],
    limits: &AppContractLimits,
) -> Result<T, AppContractError>
where
    T: DeserializeOwned + ValidateAppContract,
{
    if bytes.len() > limits.max_document_bytes {
        return Err(AppContractError::DocumentTooLarge {
            limit: limits.max_document_bytes,
        });
    }
    if !json_bytes_nesting_is_bounded(bytes, limits.max_json_depth) {
        return Err(AppContractError::JsonDepthExceeded {
            limit: limits.max_json_depth,
        });
    }
    if !json_bytes_nodes_are_bounded(bytes, limits.max_json_nodes) {
        return Err(AppContractError::JsonNodeLimitExceeded {
            limit: limits.max_json_nodes,
        });
    }
    let contract: T =
        serde_json::from_slice(bytes).map_err(|error| AppContractError::InvalidJson {
            message: error.to_string(),
        })?;
    contract.validate_app_contract(limits)?;
    Ok(contract)
}

pub fn decode_bounded_json_value(
    bytes: &[u8],
    limits: &AppContractLimits,
) -> Result<Value, AppContractError> {
    if bytes.len() > limits.max_value_bytes {
        return Err(AppContractError::DocumentTooLarge {
            limit: limits.max_value_bytes,
        });
    }
    if !json_bytes_nesting_is_bounded(bytes, limits.max_json_depth) {
        return Err(AppContractError::JsonDepthExceeded {
            limit: limits.max_json_depth,
        });
    }
    if !json_bytes_nodes_are_bounded(bytes, limits.max_json_nodes) {
        return Err(AppContractError::JsonNodeLimitExceeded {
            limit: limits.max_json_nodes,
        });
    }
    let value = serde_json::from_slice(bytes).map_err(|error| AppContractError::InvalidJson {
        message: error.to_string(),
    })?;
    validate_json_value(&value, limits)?;
    Ok(value)
}

impl ValidateAppContract for AppQueryRequest {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        validate_nonempty_bounded("select", self.select.len(), limits.max_collection_items)?;
        validate_bounded("order", self.order.len(), limits.max_collection_items)?;
        validate_bounded(
            "relation_expansions",
            self.relation_expansions.len(),
            limits.max_collection_items,
        )?;
        validate_unique("select", &self.select)?;
        validate_unique_by("order", &self.order, |order| order.field.clone())?;
        validate_unique_by(
            "relation_expansions",
            &self.relation_expansions,
            |expansion| expansion.relation.clone(),
        )?;
        if self.limit == 0
            || usize::try_from(self.limit).unwrap_or(usize::MAX) > limits.max_page_rows
        {
            return Err(AppContractError::invalid(
                "limit",
                format!("must be between 1 and {}", limits.max_page_rows),
            ));
        }
        for expansion in &self.relation_expansions {
            validate_nonempty_bounded(
                "relation.select",
                expansion.select.len(),
                limits.max_collection_items,
            )?;
            validate_unique("relation.select", &expansion.select)?;
            if expansion.max_depth == 0
                || usize::from(expansion.max_depth) > limits.max_predicate_depth
            {
                return Err(AppContractError::invalid(
                    "relation.max_depth",
                    format!("must be between 1 and {}", limits.max_predicate_depth),
                ));
            }
            if expansion.max_rows == 0
                || usize::try_from(expansion.max_rows).unwrap_or(usize::MAX) > limits.max_page_rows
            {
                return Err(AppContractError::invalid(
                    "relation.max_rows",
                    format!("must be between 1 and {}", limits.max_page_rows),
                ));
            }
        }
        if let Some(predicate) = &self.predicate {
            validate_predicate(predicate, limits)?;
        }
        Ok(())
    }
}

impl ValidateAppContract for AppArtifactProjection {
    fn validate_app_contract(&self, _limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.media_type.is_empty() || self.media_type.len() > 255 {
            return Err(AppContractError::invalid(
                "media_type",
                "must contain between 1 and 255 bytes",
            ));
        }
        let Some((media_class, media_subtype)) = self.media_type.split_once('/') else {
            return Err(AppContractError::invalid(
                "media_type",
                "must be a canonical type/subtype value",
            ));
        };
        let valid_token = |token: &str| {
            !token.is_empty()
                && token.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric()
                        || matches!(
                            byte,
                            b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                        )
                })
        };
        if !valid_token(media_class)
            || !valid_token(media_subtype)
            || self
                .media_type
                .bytes()
                .any(|byte| byte.is_ascii_uppercase())
        {
            return Err(AppContractError::invalid(
                "media_type",
                "must be a lowercase canonical media type without parameters",
            ));
        }
        Ok(())
    }
}

impl ValidateAppContract for AppMutationCommand {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        validate_nonempty_bounded(
            "operations",
            self.operations.len(),
            limits.max_collection_items,
        )?;
        validate_bounded(
            "expected_record_revisions",
            self.expected_record_revisions.len(),
            limits.max_collection_items,
        )?;

        let mut expected = HashSet::with_capacity(self.expected_record_revisions.len());
        for record in &self.expected_record_revisions {
            if !expected.insert((&record.entity, &record.record_id)) {
                return Err(AppContractError::invalid(
                    "expected_record_revisions",
                    "contains a duplicate entity/record pair",
                ));
            }
        }

        let mut temporary_ids = HashSet::new();
        let mut touched_records = HashSet::new();
        // Named creates, tracked separately from `touched_records`. They
        // belong in the collision check below but NOT in the
        // `expected_record_revisions` coverage check: a create asserts the row
        // is ABSENT, and `AppRevision` is non-zero, so there is no revision a
        // caller could name for a row that does not exist yet. Folding the two
        // uses into one set made an id-addressable create impossible to
        // satisfy -- omit the entry and coverage fails, supply one and it
        // claims a revision the row cannot have.
        let mut created_records = HashSet::new();
        let mut touched_relations = HashSet::new();
        for operation in &self.operations {
            match operation {
                AppMutationOperation::Create {
                    entity,
                    temporary_id,
                    record_id,
                    ..
                } => {
                    if !temporary_ids.insert(temporary_id) {
                        return Err(AppContractError::invalid(
                            "operations",
                            "contains a duplicate create temporary_id",
                        ));
                    }
                    // A named create targets a specific row, so it collides
                    // with the other operations in the batch the same way an
                    // update does. Two creates naming one id, or a create and
                    // an update naming it, would otherwise reach the store and
                    // fail there rather than at the contract edge.
                    if let Some(record_id) = record_id {
                        if !touched_records.insert((entity, record_id)) {
                            return Err(AppContractError::invalid(
                                "operations",
                                "mutates the same existing record more than once",
                            ));
                        }
                        created_records.insert((entity, record_id));
                    }
                },
                AppMutationOperation::Update {
                    entity, record_id, ..
                }
                | AppMutationOperation::Delete { entity, record_id }
                | AppMutationOperation::Restore { entity, record_id } => {
                    if !touched_records.insert((entity, record_id)) {
                        return Err(AppContractError::invalid(
                            "operations",
                            "mutates the same existing record more than once",
                        ));
                    }
                },
                AppMutationOperation::CreateRelation {
                    relation,
                    from_record_id,
                    to_record_id,
                    ..
                }
                | AppMutationOperation::DeleteRelation {
                    relation,
                    from_record_id,
                    to_record_id,
                    ..
                } => {
                    if from_record_id == to_record_id {
                        return Err(AppContractError::invalid(
                            "operations",
                            "relation endpoints must identify two distinct records",
                        ));
                    }
                    if !touched_relations.insert((relation, from_record_id, to_record_id)) {
                        return Err(AppContractError::invalid(
                            "operations",
                            "mutates the same relation edge more than once",
                        ));
                    }
                },
            }
        }
        // Coverage is about rows that already existed. A named create is in
        // `touched_records` for collision detection only, so subtract it here.
        let expected_coverage = touched_records
            .difference(&created_records)
            .copied()
            .collect::<HashSet<_>>();
        if expected_coverage.len() != expected.len()
            || expected_coverage
                .iter()
                .any(|record| !expected.contains(record))
        {
            return Err(AppContractError::invalid(
                "expected_record_revisions",
                "must exactly cover every updated, deleted or restored record",
            ));
        }

        let mut aggregate_nodes = 0usize;
        let mut aggregate_bytes = 0usize;
        for operation in &self.operations {
            let value = match operation {
                AppMutationOperation::Create { payload, .. } => Some(payload),
                AppMutationOperation::Update { patch, .. } => Some(patch),
                AppMutationOperation::Delete { .. }
                | AppMutationOperation::Restore { .. }
                | AppMutationOperation::CreateRelation { .. }
                | AppMutationOperation::DeleteRelation { .. } => None,
            };
            if let Some(value) = value {
                if !value.is_object() {
                    return Err(AppContractError::invalid(
                        "operations",
                        "create payloads and update patches must be JSON objects",
                    ));
                }
                if matches!(operation, AppMutationOperation::Update { .. })
                    && value.as_object().is_some_and(|object| object.is_empty())
                {
                    return Err(AppContractError::invalid(
                        "operations",
                        "update patches must change at least one declared field",
                    ));
                }
                let (nodes, bytes) = validate_json_value(value, limits)?;
                aggregate_nodes = aggregate_nodes.saturating_add(nodes);
                aggregate_bytes = aggregate_bytes.saturating_add(bytes);
            }
        }
        if aggregate_nodes > limits.max_value_nodes {
            return Err(AppContractError::invalid(
                "operations",
                format!(
                    "payloads exceed {} aggregate JSON nodes",
                    limits.max_value_nodes
                ),
            ));
        }
        if aggregate_bytes > limits.max_value_bytes {
            return Err(AppContractError::invalid(
                "operations",
                format!("payloads exceed {} aggregate bytes", limits.max_value_bytes),
            ));
        }
        Ok(())
    }
}

impl ValidateAppContract for AppDataEnvelope<Value> {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        validate_value_envelope(self, limits)?;
        Ok(())
    }
}

impl ValidateAppContract for AppDataEnvelope<AppArtifactProjection> {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        validate_envelope_metadata(self, limits)?;
        self.value.validate_app_contract(limits)?;
        validate_envelope_content_digest(self, limits)
    }
}

impl ValidateAppContract for AppQueryPage {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        validate_envelope_metadata(&self.envelope, limits)?;
        validate_bounded(
            "envelope.value",
            self.envelope.value.len(),
            limits.max_page_rows,
        )?;
        for record in &self.envelope.value {
            validate_bounded(
                "record.fields",
                record.fields.len(),
                limits.max_collection_items,
            )?;
            for value in record.fields.values() {
                validate_json_value(value, limits)?;
            }
        }
        validate_envelope_content_digest(&self.envelope, limits)?;
        if self.result_schema_ref != self.envelope.value_schema_ref {
            return Err(AppContractError::invalid(
                "result_schema_ref",
                "must match the envelope value schema",
            ));
        }
        Ok(())
    }
}

impl ValidateAppContract for AppActionInvocation<Value> {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        self.input.validate_app_contract(limits)
    }
}

impl ValidateAppContract for AppDirectActionRequest {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        validate_json_value(&self.input, limits)?;
        if self
            .expected_installation_binding
            .as_ref()
            .is_some_and(|binding| binding.generation == 0)
        {
            return Err(AppContractError::invalid(
                "expected_installation_binding.generation",
                "must be greater than zero when the expected binding is present",
            ));
        }
        Ok(())
    }
}

impl ValidateAppContract for AppErrorEnvelope {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.message.is_empty() || self.message.len() > 4_096 {
            return Err(AppContractError::invalid(
                "error.message",
                "must contain between 1 and 4096 bytes",
            ));
        }
        validate_bounded(
            "error.details",
            self.details.len(),
            limits.max_collection_items,
        )?;
        let mut aggregate_nodes = 0usize;
        let mut aggregate_bytes = 0usize;
        for detail in self.details.values() {
            let (nodes, bytes) = validate_json_value(detail, limits)?;
            aggregate_nodes = aggregate_nodes.saturating_add(nodes);
            aggregate_bytes = aggregate_bytes.saturating_add(bytes);
        }
        if aggregate_nodes > limits.max_value_nodes || aggregate_bytes > limits.max_value_bytes {
            return Err(AppContractError::invalid(
                "error.details",
                "exceeds the aggregate value ceiling",
            ));
        }
        if self.retry_after_ms == Some(0) {
            return Err(AppContractError::invalid(
                "error.retry_after_ms",
                "must be greater than zero when present",
            ));
        }
        if self.retry_after_ms.is_some()
            && !matches!(
                self.code,
                AppErrorCode::RateLimited | AppErrorCode::Unavailable
            )
        {
            return Err(AppContractError::invalid(
                "error.retry_after_ms",
                "is supported only for rate_limited or unavailable errors",
            ));
        }
        if (self.code == AppErrorCode::ExternalOutcomeUncertain)
            != (self.disposition == AppErrorDisposition::OutcomeUncertain)
        {
            return Err(AppContractError::invalid(
                "error.disposition",
                "external_outcome_uncertain and outcome_uncertain must be paired",
            ));
        }
        Ok(())
    }
}

impl ValidateAppContract for AppActionResult<Value> {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        validate_bounded(
            "mutation_receipt_refs",
            self.mutation_receipt_refs.len(),
            limits.max_collection_items,
        )?;
        validate_bounded(
            "external_effect_receipt_refs",
            self.external_effect_receipt_refs.len(),
            limits.max_collection_items,
        )?;
        validate_unique("mutation_receipt_refs", &self.mutation_receipt_refs)?;
        validate_unique(
            "external_effect_receipt_refs",
            &self.external_effect_receipt_refs,
        )?;
        if let Some(output) = &self.output {
            output.validate_app_contract(limits)?;
        }
        if let Some(error) = &self.error {
            error.validate_app_contract(limits)?;
        }
        match self.status {
            AppActionStatus::Completed => {
                if self.error.is_some() {
                    return Err(AppContractError::invalid(
                        "error",
                        "completed actions cannot carry an error",
                    ));
                }
                if self.output.is_none()
                    && self.mutation_receipt_refs.is_empty()
                    && self.external_effect_receipt_refs.is_empty()
                {
                    return Err(AppContractError::invalid(
                        "status",
                        "completed actions require typed output or an effect receipt",
                    ));
                }
            },
            AppActionStatus::Waiting => {
                if self.error.is_some() {
                    return Err(AppContractError::invalid(
                        "error",
                        "waiting actions cannot carry a terminal error",
                    ));
                }
                if self.output.is_some()
                    || !self.mutation_receipt_refs.is_empty()
                    || !self.external_effect_receipt_refs.is_empty()
                {
                    return Err(AppContractError::invalid(
                        "status",
                        "waiting actions cannot claim final output or committed effects",
                    ));
                }
            },
            AppActionStatus::Failed => {
                if self.error.is_none() {
                    return Err(AppContractError::invalid(
                        "error",
                        "failed actions require a typed error",
                    ));
                }
                if self.output.is_some()
                    || !self.mutation_receipt_refs.is_empty()
                    || !self.external_effect_receipt_refs.is_empty()
                {
                    return Err(AppContractError::invalid(
                        "status",
                        "failed actions cannot claim output or committed effects; use uncertain \
                         when an external outcome may have occurred",
                    ));
                }
            },
            AppActionStatus::Uncertain => {
                let uncertain_error = self.error.as_ref().is_some_and(|error| {
                    error.code == AppErrorCode::ExternalOutcomeUncertain
                        && error.disposition == AppErrorDisposition::OutcomeUncertain
                });
                if !uncertain_error || self.external_effect_receipt_refs.is_empty() {
                    return Err(AppContractError::invalid(
                        "status",
                        "uncertain actions require a typed uncertain error and external-effect \
                         receipt",
                    ));
                }
            },
        }
        Ok(())
    }
}

impl ValidateAppContract for AppRunHandle {
    fn validate_app_contract(&self, _limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.protocol_version != AppProtocolVersion::V1 {
            return Err(AppContractError::invalid(
                "protocol_version",
                "unsupported app run handle protocol version",
            ));
        }
        if !has_canonical_app_action_run_namespace(self.run_ref.as_str()) {
            return Err(AppContractError::invalid(
                "run_ref",
                "app run handles must use the run:app-action: namespace",
            ));
        }
        Ok(())
    }
}

impl<O> ValidateAppContract for AppRunSnapshot<O>
where
    AppActionResult<O>: ValidateAppContract,
{
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.protocol_version != AppProtocolVersion::V1 {
            return Err(AppContractError::invalid(
                "protocol_version",
                "unsupported app run snapshot protocol version",
            ));
        }
        self.run_handle.validate_app_contract(limits)?;
        if self.terminal != self.status.is_terminal() {
            return Err(AppContractError::invalid(
                "terminal",
                "must exactly match the typed app run status",
            ));
        }
        if self.cancellation_generation == Some(0) {
            return Err(AppContractError::invalid(
                "cancellation_generation",
                "must be a positive durable control generation",
            ));
        }
        if let Some(execution_id) = self.execution_id.as_deref() {
            if execution_id.is_empty() || execution_id.len() > 256 {
                return Err(AppContractError::invalid(
                    "execution_id",
                    "must contain between 1 and 256 bytes",
                ));
            }
        }
        if let Some(result) = self.result.as_ref() {
            if self.result_withheld {
                return Err(AppContractError::invalid(
                    "result_withheld",
                    "cannot be true when a typed result is included",
                ));
            }
            result.validate_app_contract(limits)?;
            if result.run_ref != self.run_handle.run_ref
                || result.action_id != self.run_handle.action_id
            {
                return Err(AppContractError::invalid(
                    "result",
                    "belongs to another app run or action",
                ));
            }
            let expected_status = match result.status {
                AppActionStatus::Completed => AppRunStatus::Completed,
                AppActionStatus::Waiting => AppRunStatus::Waiting,
                AppActionStatus::Failed => AppRunStatus::Failed,
                AppActionStatus::Uncertain => AppRunStatus::Uncertain,
            };
            if self.status != expected_status {
                return Err(AppContractError::invalid(
                    "status",
                    "does not match the typed action result",
                ));
            }
        } else if self.status == AppRunStatus::Completed && !self.result_withheld {
            return Err(AppContractError::invalid(
                "result",
                "completed app runs require their typed result or an explicit policy-withheld \
                 marker",
            ));
        } else if self.result_withheld && self.status != AppRunStatus::Completed {
            return Err(AppContractError::invalid(
                "result_withheld",
                "is valid only for a completed app run",
            ));
        }
        Ok(())
    }
}

fn validate_envelope_metadata<T>(
    envelope: &AppDataEnvelope<T>,
    limits: &AppContractLimits,
) -> Result<(), AppContractError> {
    validate_bounded(
        "source_refs",
        envelope.source_refs.len(),
        limits.max_collection_items,
    )?;
    let mut source_identities = HashSet::with_capacity(envelope.source_refs.len());
    for source in &envelope.source_refs {
        if !source_identities.insert((source.kind, &source.reference, source.revision)) {
            return Err(AppContractError::invalid(
                "source_refs",
                "contains a duplicate source identity",
            ));
        }
        validate_bounded(
            "source_ref.fields",
            source.fields.len(),
            limits.max_collection_items,
        )?;
        validate_unique("source_ref.fields", &source.fields)?;
        match source.kind {
            AppSourceRefKind::EntityRecord | AppSourceRefKind::EntityField
                if source.revision.is_none() =>
            {
                return Err(AppContractError::invalid(
                    "source_ref.revision",
                    "entity sources require an exact revision",
                ));
            },
            AppSourceRefKind::EntityField if source.fields.is_empty() => {
                return Err(AppContractError::invalid(
                    "source_ref.fields",
                    "entity_field sources require at least one selected field",
                ));
            },
            _ => {},
        }
    }
    if envelope
        .expires_at
        .as_ref()
        .is_some_and(|expires_at| expires_at <= &envelope.produced_at)
    {
        return Err(AppContractError::invalid(
            "expires_at",
            "must be later than produced_at",
        ));
    }
    Ok(())
}

fn validate_envelope_content_digest<T: Serialize>(
    envelope: &AppDataEnvelope<T>,
    limits: &AppContractLimits,
) -> Result<(), AppContractError> {
    let value = serde_json::to_value(&envelope.value).map_err(|error| {
        AppContractError::invalid(
            "content_digest",
            format!("cannot canonically encode envelope value: {error}"),
        )
    })?;
    validate_json_value(&value, limits)?;
    let actual = AppDigest::blake3_canonical_json(&value).map_err(|error| {
        AppContractError::invalid(
            "content_digest",
            format!("cannot canonically encode envelope value: {error}"),
        )
    })?;
    if actual != envelope.content_digest {
        return Err(AppContractError::invalid(
            "content_digest",
            "does not match the canonical envelope value",
        ));
    }
    Ok(())
}

/// Revalidate a JSON envelope once and return metrics that downstream joins
/// can reuse without traversing or serializing the same untrusted value again.
pub fn validate_value_envelope(
    envelope: &AppDataEnvelope<Value>,
    limits: &AppContractLimits,
) -> Result<(usize, usize), AppContractError> {
    validate_envelope_metadata(envelope, limits)?;
    let metrics = validate_json_value(&envelope.value, limits)?;
    let actual = AppDigest::blake3_canonical_json(&envelope.value).map_err(|error| {
        AppContractError::invalid(
            "content_digest",
            format!("cannot canonically encode envelope value: {error}"),
        )
    })?;
    if actual != envelope.content_digest {
        return Err(AppContractError::invalid(
            "content_digest",
            "does not match the canonical envelope value",
        ));
    }
    Ok(metrics)
}

fn validate_predicate(
    predicate: &AppPredicate,
    limits: &AppContractLimits,
) -> Result<(), AppContractError> {
    if predicate.nodes.is_empty() || predicate.nodes.len() > limits.max_predicate_nodes {
        return Err(AppContractError::invalid(
            "predicate.nodes",
            format!(
                "must contain between 1 and {} nodes",
                limits.max_predicate_nodes
            ),
        ));
    }
    let root = usize::from(predicate.root);
    if root >= predicate.nodes.len() {
        return Err(AppContractError::invalid(
            "predicate.root",
            "references a missing node",
        ));
    }

    for node in &predicate.nodes {
        match node {
            AppPredicateNode::All { children } | AppPredicateNode::Any { children } => {
                validate_nonempty_bounded(
                    "predicate.children",
                    children.len(),
                    limits.max_collection_items,
                )?;
                validate_unique("predicate.children", children)?;
            },
            AppPredicateNode::Compare { value, .. } => validate_query_scalar(value)?,
            AppPredicateNode::In { values, .. } => {
                validate_nonempty_bounded(
                    "predicate.values",
                    values.len(),
                    limits.max_collection_items,
                )?;
                for value in values {
                    validate_query_scalar(value)?;
                }
            },
            AppPredicateNode::Not { .. } | AppPredicateNode::IsNull { .. } => {},
        }
        for index in 0..node.child_count() {
            if node
                .child_at(index)
                .is_none_or(|child| child >= predicate.nodes.len())
            {
                return Err(AppContractError::invalid(
                    "predicate.children",
                    "references a missing node",
                ));
            }
        }
    }

    // Iterative depth-first validation detects cycles without recursive Rust
    // values or call-stack growth. Unreachable nodes are rejected so two
    // encodings of the same logical predicate cannot carry ignored content.
    let mut colors = vec![0u8; predicate.nodes.len()];
    let mut subtree_depths = vec![0usize; predicate.nodes.len()];
    let mut stack = Vec::with_capacity(predicate.nodes.len().min(limits.max_predicate_depth));
    colors[root] = 1;
    stack.push((root, 0usize));
    while let Some((node_index, next_child)) = stack.pop() {
        let node = &predicate.nodes[node_index];
        if next_child >= node.child_count() {
            let max_child_depth = (0..node.child_count())
                .filter_map(|index| node.child_at(index))
                .map(|child| subtree_depths[child])
                .max()
                .unwrap_or(0);
            let depth = max_child_depth.saturating_add(1);
            if depth > limits.max_predicate_depth {
                return Err(AppContractError::invalid(
                    "predicate",
                    format!("exceeds {} levels", limits.max_predicate_depth),
                ));
            }
            subtree_depths[node_index] = depth;
            colors[node_index] = 2;
            continue;
        }
        let child = node.child_at(next_child).expect("references checked above");
        stack.push((node_index, next_child.saturating_add(1)));
        match colors[child] {
            0 => {
                colors[child] = 1;
                stack.push((child, 0));
            },
            1 => {
                return Err(AppContractError::invalid("predicate", "contains a cycle"));
            },
            _ => {},
        }
    }
    if colors.contains(&0) {
        return Err(AppContractError::invalid(
            "predicate.nodes",
            "contains unreachable nodes",
        ));
    }
    Ok(())
}

fn validate_query_scalar(value: &Value) -> Result<(), AppContractError> {
    if value.is_null() || value.is_array() || value.is_object() {
        return Err(AppContractError::invalid(
            "predicate.value",
            "must be a non-null JSON scalar",
        ));
    }
    if value.as_str().is_some_and(|text| text.len() > 4_096) {
        return Err(AppContractError::invalid(
            "predicate.value",
            "string exceeds 4096 bytes",
        ));
    }
    Ok(())
}

pub fn validate_json_value(
    value: &Value,
    limits: &AppContractLimits,
) -> Result<(usize, usize), AppContractError> {
    let metrics = inspect_json_bounded(value, limits.max_value_nodes).ok_or_else(|| {
        AppContractError::invalid(
            "value",
            format!("exceeds {} JSON nodes", limits.max_value_nodes),
        )
    })?;
    if metrics.max_depth > limits.max_json_depth {
        return Err(AppContractError::invalid(
            "value",
            format!("exceeds {} JSON levels", limits.max_json_depth),
        ));
    }
    let bytes = exact_json_encoded_len(value);
    if bytes > limits.max_value_bytes {
        return Err(AppContractError::invalid(
            "value",
            format!("exceeds {} encoded bytes", limits.max_value_bytes),
        ));
    }
    Ok((metrics.nodes, bytes))
}

pub fn validate_bounded(
    field: &'static str,
    length: usize,
    max: usize,
) -> Result<(), AppContractError> {
    if length > max {
        return Err(AppContractError::invalid(
            field,
            format!("contains {length} items; maximum is {max}"),
        ));
    }
    Ok(())
}

pub fn validate_nonempty_bounded(
    field: &'static str,
    length: usize,
    max: usize,
) -> Result<(), AppContractError> {
    if length == 0 {
        return Err(AppContractError::invalid(field, "must not be empty"));
    }
    validate_bounded(field, length, max)
}

fn validate_unique<T>(field: &'static str, values: &[T]) -> Result<(), AppContractError>
where
    T: Eq + std::hash::Hash,
{
    let mut unique = HashSet::with_capacity(values.len());
    if values.iter().any(|value| !unique.insert(value)) {
        return Err(AppContractError::invalid(field, "contains duplicates"));
    }
    Ok(())
}

fn validate_unique_by<T, K, F>(
    field: &'static str,
    values: &[T],
    key: F,
) -> Result<(), AppContractError>
where
    K: Eq + std::hash::Hash,
    F: Fn(&T) -> K,
{
    let mut unique = HashSet::with_capacity(values.len());
    if values.iter().any(|value| !unique.insert(key(value))) {
        return Err(AppContractError::invalid(field, "contains duplicates"));
    }
    Ok(())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {

    /// A named create needs no expected revision, but still collides.
    ///
    /// `record_id` on `Create` exists so a caller can address the row again --
    /// a migration re-running, a receipt projector writing one row per signed
    /// decision. Both were unusable while a named create counted toward
    /// `expected_record_revisions`: a create asserts the row is ABSENT, and
    /// `AppRevision` is non-zero, so no revision could be named for it.
    #[test]
    fn a_named_create_is_exempt_from_revision_coverage_but_not_from_collision() {
        let entity = AppName::parse("receipt").expect("entity");
        let record = AppRecordId::parse("rec_named").expect("record id");
        let named_create = |temporary_id: &str| AppMutationOperation::Create {
            entity: entity.clone(),
            temporary_id: AppName::parse(temporary_id).expect("temporary id"),
            record_id: Some(record.clone()),
            payload: json!({}),
        };
        let command = |operations: Vec<AppMutationOperation>| AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: AppReference::parse("receipt:one").expect("key"),
            atomicity: AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: AppRevision::new(1).expect("revision"),
            operations,
            expected_record_revisions: Vec::new(),
        };

        let limits = AppContractLimits::default();
        command(vec![named_create("a")])
            .validate_app_contract(&limits)
            .expect("a named create asserts absence, not a revision");

        // Two creates naming the same row still collide -- the collision check
        // is the reason a named create is tracked at all.
        let collision = command(vec![named_create("a"), named_create("b")])
            .validate_app_contract(&limits)
            .expect_err("two creates naming one row must collide");
        assert!(
            collision.to_string().contains("more than once"),
            "unexpected error: {collision}"
        );
    }

    use serde_json::json;

    use super::*;

    fn envelope<T: Serialize>(value: T) -> AppDataEnvelope<T> {
        let digest_value = serde_json::to_value(&value).expect("serializable envelope test value");
        let content_digest = AppDigest::blake3(
            &canonical_json_bytes(&digest_value).expect("canonical envelope test value"),
        );
        AppDataEnvelope {
            protocol_version: AppProtocolVersion::V1,
            source: AppDataSource::AppAction,
            scope_binding_ref: AppScopeBindingRef::parse("scope_1").unwrap(),
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            package_revision_ref: AppReference::parse("package:1").unwrap(),
            schema_revision: AppRevision::new(1).unwrap(),
            grant_revision: AppRevision::new(1).unwrap(),
            value_schema_ref: AppReference::parse("schema:clip").unwrap(),
            value,
            source_refs: Vec::new(),
            handling_labels: AppHandlingLabels {
                classification: AppDataClassification::Ordinary,
                model_processing: AppModelProcessing::None,
                policy_digest: AppDigest::blake3(b"policy"),
                provenance_digest: AppDigest::blake3(b"provenance"),
            },
            content_digest,
            produced_at: "2026-08-14T00:00:00Z".parse().unwrap(),
            expires_at: None,
        }
    }

    fn query(predicate: Option<AppPredicate>) -> AppQueryRequest {
        AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: AppInstallationId::parse("install_1").unwrap(),
            entity: AppName::parse("clip").unwrap(),
            select: vec![AppFieldPath::parse("title").unwrap()],
            predicate,
            order: Vec::new(),
            cursor: None,
            limit: 50,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("browse").unwrap(),
        }
    }

    #[test]
    fn predicate_cycles_and_unreachable_nodes_fail_closed() {
        let cycle = AppPredicate {
            root: 0,
            nodes: vec![
                AppPredicateNode::Not { child: 1 },
                AppPredicateNode::Not { child: 0 },
            ],
        };
        let error = query(Some(cycle))
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err();
        assert!(error.to_string().contains("cycle"));

        let unreachable = AppPredicate {
            root: 0,
            nodes: vec![
                AppPredicateNode::IsNull {
                    field: AppFieldPath::parse("title").unwrap(),
                    negated: false,
                },
                AppPredicateNode::IsNull {
                    field: AppFieldPath::parse("ignored").unwrap(),
                    negated: false,
                },
            ],
        };
        let error = query(Some(unreachable))
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err();
        assert!(error.to_string().contains("unreachable"));
    }

    #[test]
    fn compare_and_in_predicates_reject_null_as_a_canonical_scalar() {
        let field = AppFieldPath::parse("title").unwrap();
        for node in [
            AppPredicateNode::Compare {
                field: field.clone(),
                operator: AppComparisonOperator::Equal,
                value: Value::Null,
            },
            AppPredicateNode::In {
                field,
                values: vec![Value::String("present".to_owned()), Value::Null],
            },
        ] {
            let error = query(Some(AppPredicate {
                root: 0,
                nodes: vec![node],
            }))
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err();
            assert!(error.to_string().contains("non-null JSON scalar"));
        }
    }

    #[test]
    fn predicate_depth_is_checked_iteratively() {
        let limits = AppContractLimits {
            max_predicate_depth: 8,
            ..AppContractLimits::default()
        };
        let nodes = (0u16..16)
            .map(|index| {
                if index == 15 {
                    AppPredicateNode::IsNull {
                        field: AppFieldPath::parse("title").unwrap(),
                        negated: false,
                    }
                } else {
                    AppPredicateNode::Not { child: index + 1 }
                }
            })
            .collect();
        let error = query(Some(AppPredicate { root: 0, nodes }))
            .validate_app_contract(&limits)
            .unwrap_err();
        assert!(error.to_string().contains("exceeds 8 levels"));
    }

    #[test]
    fn predicate_depth_counts_a_shared_subtree_on_every_path() {
        let limits = AppContractLimits {
            max_predicate_depth: 3,
            ..AppContractLimits::default()
        };
        let field = AppFieldPath::parse("title").unwrap();
        let predicate = AppPredicate {
            root: 0,
            nodes: vec![
                AppPredicateNode::All {
                    children: vec![1, 2],
                },
                AppPredicateNode::Not { child: 3 },
                AppPredicateNode::Not { child: 4 },
                AppPredicateNode::IsNull {
                    field,
                    negated: false,
                },
                AppPredicateNode::Not { child: 3 },
            ],
        };
        assert!(query(Some(predicate))
            .validate_app_contract(&limits)
            .unwrap_err()
            .to_string()
            .contains("exceeds 3 levels"));
    }

    #[test]
    fn raw_admission_rejects_hostile_depth_before_serde() {
        let limits = AppContractLimits {
            max_json_depth: 8,
            ..AppContractLimits::default()
        };
        let hostile = format!("{}0{}", "[".repeat(512), "]".repeat(512));
        let result = std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(move || decode_app_contract::<AppQueryRequest>(hostile.as_bytes(), &limits))
            .unwrap()
            .join()
            .unwrap();
        assert!(matches!(
            result,
            Err(AppContractError::JsonDepthExceeded { .. })
        ));
    }

    #[test]
    #[should_panic]
    fn test_limit_profiles_cannot_widen_production_admission() {
        let _ = AppContractLimits::default()
            .with_max_value_nodes_for_test(AppContractLimits::default().max_value_nodes() + 1);
    }

    #[test]
    fn unknown_wire_fields_are_rejected() {
        let body = json!({
            "protocol_version": "1",
            "source_installation_id": "install_1",
            "entity": "clip",
            "select": ["title"],
            "limit": 10,
            "relation_expansions": [],
            "purpose": "browse",
            "caller_selected_principal": "forged"
        });
        let error = decode_app_contract::<AppQueryRequest>(
            serde_json::to_string(&body).unwrap().as_bytes(),
            &AppContractLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(error, AppContractError::InvalidJson { .. }));
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn mutation_requires_unique_expected_revisions_and_bounded_payloads() {
        let revision = AppExpectedRecordRevision {
            entity: AppName::parse("clip").unwrap(),
            record_id: AppRecordId::parse("record_1").unwrap(),
            revision: AppRevision::new(1).unwrap(),
        };
        let command = AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: AppReference::parse("mutation-key:duplicate-revisions").unwrap(),
            atomicity: AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: AppRevision::new(1).unwrap(),
            operations: vec![AppMutationOperation::Update {
                entity: AppName::parse("clip").unwrap(),
                record_id: AppRecordId::parse("record_1").unwrap(),
                patch: json!({"title": "shorter"}),
            }],
            expected_record_revisions: vec![revision.clone(), revision],
        };
        let error = command
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err();
        assert!(error.to_string().contains("duplicate"));
    }

    #[test]
    fn mutation_payload_shape_is_not_left_to_adapter_dialects() {
        let command = AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: AppReference::parse("mutation-key:payload-shape").unwrap(),
            atomicity: AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: AppRevision::new(1).unwrap(),
            operations: vec![AppMutationOperation::Create {
                entity: AppName::parse("clip").unwrap(),
                temporary_id: AppName::parse("new_clip").unwrap(),
                record_id: None,
                payload: json!(["not", "an", "entity"]),
            }],
            expected_record_revisions: Vec::new(),
        };
        assert!(command
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err()
            .to_string()
            .contains("JSON objects"));
    }

    #[test]
    fn mutation_optimistic_revisions_exactly_cover_existing_record_writes() {
        let command = AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: AppReference::parse("mutation-key:missing-revision").unwrap(),
            atomicity: AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: AppRevision::new(1).unwrap(),
            operations: vec![AppMutationOperation::Delete {
                entity: AppName::parse("clip").unwrap(),
                record_id: AppRecordId::parse("record_1").unwrap(),
            }],
            expected_record_revisions: Vec::new(),
        };
        assert!(command
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err()
            .to_string()
            .contains("exactly cover"));
    }

    #[test]
    fn app_run_snapshot_owns_terminality_and_exact_result_identity() {
        let run_handle = AppRunHandle {
            protocol_version: AppProtocolVersion::V1,
            run_ref: AppReference::parse(format!("run:app-action:task_app_{}", "a".repeat(64)))
                .unwrap(),
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            action_id: AppName::parse("condense").unwrap(),
        };
        let blocked = AppRunSnapshot::<Value> {
            protocol_version: AppProtocolVersion::V1,
            run_handle: run_handle.clone(),
            execution_id: Some("execution_1".to_owned()),
            status: AppRunStatus::Blocked,
            terminal: false,
            cancellation_generation: None,
            result_withheld: false,
            result: None,
        };
        blocked
            .validate_app_contract(&AppContractLimits::default())
            .unwrap();
        assert!(AppRunSnapshot {
            terminal: true,
            ..blocked.clone()
        }
        .validate_app_contract(&AppContractLimits::default())
        .is_err());

        let completed_result = AppActionResult {
            protocol_version: AppProtocolVersion::V1,
            action_id: run_handle.action_id.clone(),
            run_ref: run_handle.run_ref.clone(),
            status: AppActionStatus::Completed,
            output: Some(envelope(json!({"ok": true}))),
            mutation_receipt_refs: Vec::new(),
            external_effect_receipt_refs: Vec::new(),
            error: None,
        };
        AppRunSnapshot {
            protocol_version: AppProtocolVersion::V1,
            run_handle: run_handle.clone(),
            execution_id: Some("execution_1".to_owned()),
            status: AppRunStatus::Completed,
            terminal: true,
            cancellation_generation: None,
            result_withheld: false,
            result: Some(completed_result.clone()),
        }
        .validate_app_contract(&AppContractLimits::default())
        .unwrap();
        assert!(AppRunSnapshot {
            protocol_version: AppProtocolVersion::V1,
            run_handle,
            execution_id: None,
            status: AppRunStatus::Completed,
            terminal: true,
            cancellation_generation: None,
            result_withheld: false,
            result: None::<AppActionResult<Value>>,
        }
        .validate_app_contract(&AppContractLimits::default())
        .is_err());
        AppRunSnapshot {
            protocol_version: AppProtocolVersion::V1,
            run_handle: blocked.run_handle,
            execution_id: None,
            status: AppRunStatus::Completed,
            terminal: true,
            cancellation_generation: None,
            result_withheld: true,
            result: None::<AppActionResult<Value>>,
        }
        .validate_app_contract(&AppContractLimits::default())
        .unwrap();
    }

    #[test]
    fn artifact_task_status_projection_preserves_app_terminality() {
        for status in ["failed", "cancelled", "archived", "uncertain"] {
            assert!(app_run_status_from_task(status)
                .expect("known terminal task status")
                .is_terminal());
        }
        for status in ["ready", "running", "waiting_for_input", "blocked"] {
            assert!(!app_run_status_from_task(status)
                .expect("known nonterminal task status")
                .is_terminal());
        }
        assert!(app_run_status_from_task("invented_state").is_none());
    }

    #[test]
    fn completed_action_cannot_be_successful_prose_only() {
        let result = AppActionResult::<Value> {
            protocol_version: AppProtocolVersion::V1,
            action_id: AppName::parse("condense").unwrap(),
            run_ref: AppReference::parse("run_1").unwrap(),
            status: AppActionStatus::Completed,
            output: None,
            mutation_receipt_refs: Vec::new(),
            external_effect_receipt_refs: Vec::new(),
            error: None,
        };
        let error = result
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("typed output or an effect receipt"));
    }

    #[test]
    fn action_failure_and_uncertainty_have_one_typed_cross_client_meaning() {
        let failed = AppActionResult::<Value> {
            protocol_version: AppProtocolVersion::V1,
            action_id: AppName::parse("condense").unwrap(),
            run_ref: AppReference::parse("run_1").unwrap(),
            status: AppActionStatus::Failed,
            output: None,
            mutation_receipt_refs: Vec::new(),
            external_effect_receipt_refs: Vec::new(),
            error: None,
        };
        assert!(failed
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err()
            .to_string()
            .contains("typed error"));

        let terminal_error = AppErrorEnvelope {
            code: AppErrorCode::InvalidRequest,
            disposition: AppErrorDisposition::Terminal,
            message: "Invalid action input".to_owned(),
            details: BTreeMap::new(),
            retry_after_ms: None,
        };
        for invalid in [
            AppActionResult::<Value> {
                status: AppActionStatus::Waiting,
                output: Some(envelope(json!({"premature": true}))),
                ..failed.clone()
            },
            AppActionResult::<Value> {
                status: AppActionStatus::Waiting,
                mutation_receipt_refs: vec![AppReference::parse("mutation:1").unwrap()],
                ..failed.clone()
            },
            AppActionResult::<Value> {
                status: AppActionStatus::Failed,
                output: Some(envelope(json!({"partial": true}))),
                error: Some(terminal_error.clone()),
                ..failed.clone()
            },
            AppActionResult::<Value> {
                status: AppActionStatus::Failed,
                external_effect_receipt_refs: vec![AppReference::parse("effect:failed").unwrap()],
                error: Some(terminal_error),
                ..failed.clone()
            },
        ] {
            assert!(invalid
                .validate_app_contract(&AppContractLimits::default())
                .unwrap_err()
                .to_string()
                .contains("cannot claim"));
        }

        let uncertain = AppActionResult::<Value> {
            status: AppActionStatus::Uncertain,
            external_effect_receipt_refs: vec![AppReference::parse("effect:1").unwrap()],
            error: Some(AppErrorEnvelope {
                code: AppErrorCode::ExternalOutcomeUncertain,
                disposition: AppErrorDisposition::OutcomeUncertain,
                message: "Provider accepted the request but its outcome is unknown".to_owned(),
                details: BTreeMap::new(),
                retry_after_ms: None,
            }),
            ..failed
        };
        uncertain
            .validate_app_contract(&AppContractLimits::default())
            .unwrap();
    }

    #[test]
    fn error_retry_delay_is_limited_to_retryable_transport_families() {
        let limits = AppContractLimits::default();
        let mut error = AppErrorEnvelope {
            code: AppErrorCode::Conflict,
            disposition: AppErrorDisposition::Terminal,
            message: "The immutable idempotency binding conflicts.".to_owned(),
            details: BTreeMap::new(),
            retry_after_ms: Some(1_000),
        };
        assert!(error
            .validate_app_contract(&limits)
            .unwrap_err()
            .to_string()
            .contains("rate_limited or unavailable"));

        error.code = AppErrorCode::RateLimited;
        error.disposition = AppErrorDisposition::RetrySameInput;
        error.validate_app_contract(&limits).unwrap();
        error.code = AppErrorCode::Unavailable;
        error.validate_app_contract(&limits).unwrap();
    }

    #[test]
    fn envelope_sources_are_revision_exact_and_digests_are_canonical() {
        assert!(AppDigest::parse("digest:invented").is_err());
        AppDigest::parse(AppDigest::blake3(b"accepted").as_str().to_owned()).unwrap();

        let mut value = envelope(json!({"title": "clip"}));
        value.source_refs.push(AppSourceRef {
            kind: AppSourceRefKind::EntityField,
            reference: AppReference::parse("record:1").unwrap(),
            revision: None,
            fields: vec![AppFieldPath::parse("title").unwrap()],
        });
        assert!(value
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err()
            .to_string()
            .contains("exact revision"));

        value.source_refs[0].revision = Some(AppRevision::new(1).unwrap());
        value.source_refs[0].fields.clear();
        assert!(value
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err()
            .to_string()
            .contains("at least one selected field"));
        value.source_refs[0]
            .fields
            .push(AppFieldPath::parse("title").unwrap());
        value
            .validate_app_contract(&AppContractLimits::default())
            .unwrap();
    }

    #[test]
    fn envelope_validation_recomputes_content_digest_for_actions_and_query_pages() {
        let limits = AppContractLimits::default();
        let mut forged = envelope(json!({"title": "clip"}));
        forged.content_digest = AppDigest::blake3(b"caller assertion");
        let invocation = AppActionInvocation {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: AppReference::parse("action-key:condense:1").unwrap(),
            action_id: AppName::parse("condense").unwrap(),
            action_revision: AppRevision::new(1).unwrap(),
            input: forged,
            requested_result_schema_ref: AppReference::parse("schema:clip").unwrap(),
            caller_surface_or_execution_ref: AppReference::parse("surface:1").unwrap(),
        };
        assert!(invocation
            .validate_app_contract(&limits)
            .unwrap_err()
            .to_string()
            .contains("content_digest"));

        let record = AppRecordProjection {
            entity: AppName::parse("clip").unwrap(),
            record_id: AppRecordId::parse("record_1").unwrap(),
            record_revision: AppRevision::new(1).unwrap(),
            fields: BTreeMap::from([(AppFieldPath::parse("title").unwrap(), json!("clip"))]),
        };
        let mut page = AppQueryPage {
            envelope: envelope(vec![record]),
            next_cursor: None,
            result_schema_ref: AppReference::parse("schema:clip").unwrap(),
        };
        page.envelope.content_digest = AppDigest::blake3(b"forged page digest");
        assert!(page
            .validate_app_contract(&limits)
            .unwrap_err()
            .to_string()
            .contains("content_digest"));
    }

    #[test]
    fn direct_action_expected_installation_binding_is_optional_but_closed() {
        let legacy: AppDirectActionRequest = serde_json::from_value(serde_json::json!({
            "idempotency_key": "action-key:legacy:1",
            "input": {}
        }))
        .unwrap();
        assert!(legacy.expected_installation_binding.is_none());
        legacy
            .validate_app_contract(&AppContractLimits::default())
            .unwrap();

        assert!(
            serde_json::from_value::<AppDirectActionRequest>(serde_json::json!({
                "idempotency_key": "action-key:partial:1",
                "input": {},
                "expected_installation_binding": { "generation": 4 }
            }))
            .is_err()
        );

        let zero: AppDirectActionRequest = serde_json::from_value(serde_json::json!({
            "idempotency_key": "action-key:zero:1",
            "input": {},
            "expected_installation_binding": {
                "generation": 0,
                "package_revision_ref": "package:revision:1"
            }
        }))
        .unwrap();
        assert!(zero
            .validate_app_contract(&AppContractLimits::default())
            .is_err());
    }
}

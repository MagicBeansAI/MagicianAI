//! Canonical, surface-neutral projections of tool results.
//!
//! The execution result remains the source of truth. This module derives a
//! bounded model view without cutting serialized JSON, plus independent spoken
//! and display descriptors. Projection selection is based only on a versioned
//! contract id supplied by capability metadata; tool and family names never
//! participate in contract selection.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use magicllm::ConservativeOllamaEstimator;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::magician_v2::json_traversal::{
    canonical_json_bytes as stack_safe_canonical_json_bytes,
    canonicalize_json as stack_safe_canonicalize_json, canonicalize_json_owned, inspect_json,
    json_encoded_len, MAX_RETAINED_JSON_DEPTH,
};

pub const TOOL_RESULT_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const GENERIC_JSON_CONTRACT_V1: &str = "generic_json_v1";
pub const SCALAR_OR_OBJECT_CONTRACT_V1: &str = "scalar_or_object_v1";
pub const RANKED_RECORDS_CONTRACT_V1: &str = "ranked_records_v1";
pub const TABULAR_ROWS_CONTRACT_V1: &str = "tabular_rows_v1";
pub const DOCUMENT_SPANS_CONTRACT_V1: &str = "document_spans_v1";
pub const ARTIFACT_MANIFEST_CONTRACT_V1: &str = "artifact_manifest_v1";
pub const TASK_RECEIPT_CONTRACT_V1: &str = "task_receipt_v1";
pub const ERROR_CONTRACT_V1: &str = "error_v1";

const MIN_MODEL_PROJECTION_BYTES: usize = 192;

/// Validated, stable identifier owned by capability metadata.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectionContractId(String);

impl ProjectionContractId {
    pub fn new(value: impl Into<String>) -> Result<Self, ProjectionError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 96
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            && value.as_bytes()[0].is_ascii_lowercase();
        if !valid {
            return Err(ProjectionError::InvalidContractId(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectionContractId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for ProjectionContractId {
    type Err = ProjectionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl Serialize for ProjectionContractId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProjectionContractId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ContractIdVisitor;

        impl<'de> Visitor<'de> for ContractIdVisitor {
            type Value = ProjectionContractId;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a lowercase versioned projection contract id")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                ProjectionContractId::new(value).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(ContractIdVisitor)
    }
}

/// RFC 6901 JSON pointer used by declarative projection contracts.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectionPath(String);

impl ProjectionPath {
    pub fn root() -> Self {
        Self(String::new())
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, ProjectionError> {
        let value = value.into();
        if !value.is_empty() && !value.starts_with('/') {
            Err(ProjectionError::InvalidProjectionPath(value))
        } else if value.as_bytes().iter().enumerate().any(|(index, byte)| {
            *byte == b'~'
                && !matches!(
                    value.as_bytes().get(index + 1),
                    Some(next) if *next == b'0' || *next == b'1'
                )
        }) {
            Err(ProjectionError::InvalidProjectionPath(value))
        } else {
            Ok(Self(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn from_segments(segments: &[String]) -> Self {
        if segments.is_empty() {
            return Self::root();
        }
        Self(format!(
            "/{}",
            segments
                .iter()
                .map(|segment| segment.replace('~', "~0").replace('/', "~1"))
                .collect::<Vec<_>>()
                .join("/")
        ))
    }
}

impl Serialize for ProjectionPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProjectionPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ProjectionPathVisitor;

        impl<'de> Visitor<'de> for ProjectionPathVisitor {
            type Value = ProjectionPath;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an empty root path or an RFC 6901 JSON pointer")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                ProjectionPath::parse(value).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(ProjectionPathVisitor)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionBudget {
    pub max_serialized_bytes: usize,
    pub max_estimated_tokens: usize,
    pub max_records: usize,
    pub max_depth: usize,
    pub max_scalar_bytes: usize,
    #[serde(default = "default_spoken_chars")]
    pub max_spoken_chars: usize,
}

const fn default_spoken_chars() -> usize {
    420
}

impl ProjectionBudget {
    pub fn validate(&self) -> Result<(), ProjectionError> {
        if self.max_serialized_bytes < MIN_MODEL_PROJECTION_BYTES {
            return Err(ProjectionError::InvalidBudget(format!(
                "max_serialized_bytes must be at least {MIN_MODEL_PROJECTION_BYTES}"
            )));
        }
        if self.max_estimated_tokens == 0 {
            return Err(ProjectionError::InvalidBudget(
                "max_estimated_tokens must be greater than zero".to_owned(),
            ));
        }
        if self.max_records == 0 {
            return Err(ProjectionError::InvalidBudget(
                "max_records must be greater than zero".to_owned(),
            ));
        }
        if self.max_depth == 0 {
            return Err(ProjectionError::InvalidBudget(
                "max_depth must be greater than zero".to_owned(),
            ));
        }
        if self.max_depth > MAX_RETAINED_JSON_DEPTH {
            return Err(ProjectionError::InvalidBudget(format!(
                "max_depth must not exceed {MAX_RETAINED_JSON_DEPTH}"
            )));
        }
        if self.max_scalar_bytes == 0 {
            return Err(ProjectionError::InvalidBudget(
                "max_scalar_bytes must be greater than zero".to_owned(),
            ));
        }
        if self.max_spoken_chars == 0 {
            return Err(ProjectionError::InvalidBudget(
                "max_spoken_chars must be greater than zero".to_owned(),
            ));
        }
        Ok(())
    }
}

impl Default for ProjectionBudget {
    fn default() -> Self {
        Self {
            max_serialized_bytes: 8 * 1024,
            max_estimated_tokens: 2_048,
            max_records: 32,
            max_depth: 8,
            max_scalar_bytes: 2 * 1024,
            max_spoken_chars: default_spoken_chars(),
        }
    }
}

pub trait ProjectionTokenEstimator: Send + Sync {
    fn estimate_tokens(&self, serialized_json: &[u8]) -> usize;
}

/// Shared conservative fallback used by MagicLLM request preflight.
/// Provider-specific estimators can still be injected by callers, but the
/// default must not admit a projection that the common preflight estimator
/// would consider over budget.
#[derive(Clone, Copy, Debug, Default)]
pub struct ConservativeTokenEstimator;

impl ProjectionTokenEstimator for ConservativeTokenEstimator {
    fn estimate_tokens(&self, serialized_json: &[u8]) -> usize {
        ConservativeOllamaEstimator
            .estimate_serialized_bytes(serialized_json)
            .try_into()
            .unwrap_or(usize::MAX)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResultIdentity {
    pub tool_name: String,
    pub tool_call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub scope_digest: String,
    pub authority_revision: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcomeStatus {
    Succeeded,
    Partial,
    Failed,
    Denied,
    Cancelled,
    Pending,
    RequiresApproval,
    TimedOut,
    Revoked,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutcome {
    pub status: ToolOutcomeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default)]
    pub retryable: bool,
}

impl ToolOutcome {
    pub fn succeeded() -> Self {
        Self {
            status: ToolOutcomeStatus::Succeeded,
            code: None,
            message: None,
            retryable: false,
        }
    }

    pub fn with_status(status: ToolOutcomeStatus) -> Self {
        Self {
            status,
            code: None,
            message: None,
            retryable: false,
        }
    }

    pub fn is_failure(&self) -> bool {
        matches!(
            self.status,
            ToolOutcomeStatus::Failed
                | ToolOutcomeStatus::Denied
                | ToolOutcomeStatus::Cancelled
                | ToolOutcomeStatus::TimedOut
                | ToolOutcomeStatus::Revoked
                | ToolOutcomeStatus::Unknown
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedResultRef {
    /// Opaque storage identity. It is not an authority-bearing credential.
    pub result_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultRetentionClass {
    TaskExecution,
    ChatSession,
    VoiceSession,
    Ephemeral,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawResultDescriptor {
    pub content_ref: ScopedResultRef,
    pub content_hash: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub retention_class: ResultRetentionClass,
}

impl RawResultDescriptor {
    pub fn validate(&self) -> Result<(), ProjectionError> {
        if self.content_ref.result_ref.trim().is_empty() {
            return Err(ProjectionError::InvalidRawDescriptor(
                "result_ref must not be empty".to_owned(),
            ));
        }
        if self.content_hash.trim().is_empty() {
            return Err(ProjectionError::InvalidRawDescriptor(
                "content_hash must not be empty".to_owned(),
            ));
        }
        if self.media_type.trim().is_empty() {
            return Err(ProjectionError::InvalidRawDescriptor(
                "media_type must not be empty".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DisplayResultProjection {
    Inline {
        value: Value,
        content_hash: String,
    },
    Referenced {
        content_ref: ScopedResultRef,
        content_hash: String,
        media_type: String,
        size_bytes: u64,
    },
    InlineAndReferenced {
        value: Value,
        content_ref: ScopedResultRef,
        content_hash: String,
        media_type: String,
        size_bytes: u64,
    },
}

impl DisplayResultProjection {
    pub fn referenced(raw: &RawResultDescriptor) -> Self {
        Self::Referenced {
            content_ref: raw.content_ref.clone(),
            content_hash: raw.content_hash.clone(),
            media_type: raw.media_type.clone(),
            size_bytes: raw.size_bytes,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpokenProjectionSource {
    Authored,
    StructuredField,
    OutcomeFallback,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpokenResultProjection {
    pub text: String,
    pub source: SpokenProjectionSource,
    pub complete_scalar: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionStrategy {
    Complete,
    GenericStructured,
    ContractStructured,
    ReferenceOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionOmissionKind {
    Budget,
    RecordLimit,
    DepthLimit,
    OversizedScalar,
    OversizedRecord,
    CompactedDetails,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionOmission {
    pub path: ProjectionPath,
    pub kind: ProjectionOmissionKind,
    pub original_bytes: usize,
    pub omitted_units: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_index: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExcerptBoundary {
    Paragraph,
    Sentence,
    Word,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionExcerpt {
    pub path: ProjectionPath,
    pub text: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub original_bytes: usize,
    pub boundary: ExcerptBoundary,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelProjectionMetadata {
    pub contract_id: ProjectionContractId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_contract_id: Option<ProjectionContractId>,
    pub used_generic_fallback: bool,
    pub complete: bool,
    pub complete_units_only: bool,
    pub included_records: usize,
    pub omitted_records: usize,
    pub omitted_fields: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omissions: Vec<ProjectionOmission>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excerpts: Vec<ProjectionExcerpt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_result_ref: Option<ScopedResultRef>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelResultProjection {
    pub value: Value,
    pub strategy: ProjectionStrategy,
    pub included_records: usize,
    pub omitted_records: usize,
    pub omitted_fields: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<ScopedResultRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionMetrics {
    pub raw_bytes: usize,
    pub model_bytes: usize,
    pub estimated_model_tokens: usize,
    pub spoken_characters: usize,
    pub included_records: usize,
    pub omitted_records: usize,
    pub omitted_fields: usize,
    pub maximum_input_depth: usize,
    pub contract_fallback: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectedToolResultV1 {
    pub schema_version: u16,
    pub identity: ToolResultIdentity,
    pub outcome: ToolOutcome,
    pub model: ModelResultProjection,
    /// Exact governed app-result rejection token. It never authorizes itself;
    /// resumed transcripts must also prove server-side run membership and
    /// current app disclosure authority before using `model.value`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_result_checkpoint:
        Option<crate::magician_v2::apps::tool_disclosure::AppToolResultCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spoken: Option<SpokenResultProjection>,
    pub display: DisplayResultProjection,
    pub raw: RawResultDescriptor,
    pub metrics: ProjectionMetrics,
}

impl ProjectedToolResultV1 {
    /// Rejects a record deserialized into the V1 Rust shape with a mismatched
    /// wire version. Persistence readers should call this before replay.
    pub fn validate_schema_version(&self) -> Result<(), ProjectionError> {
        if self.schema_version == TOOL_RESULT_PROJECTION_SCHEMA_VERSION {
            Ok(())
        } else {
            Err(ProjectionError::UnsupportedSchemaVersion(
                self.schema_version,
            ))
        }
    }
}

/// Return the model projection after the independent outbound credential
/// sanitizer has run. Every provider-facing replay must use this helper rather
/// than reading `projection.model.value` directly. The stored projection stays
/// immutable for audit/display parity; only the outbound clone is guarded.
pub fn provider_safe_model_value(projection: &ProjectedToolResultV1) -> Value {
    if let Some(checkpoint) = projection.app_result_checkpoint.as_ref() {
        let exact =
            crate::magician_v2::json_traversal::canonical_json_bytes(&projection.model.value);
        if match exact.as_deref() {
            Ok(bytes) => !checkpoint.matches_content_bytes(bytes),
            Err(_) => true,
        } {
            return serde_json::json!({
                "status": "error",
                "code": "app_result_checkpoint_mismatch",
            });
        }
        // The server label is bound to these exact canonical bytes and the
        // app model guard has already constrained the physical destination.
        // Running a second generic sanitizer here would feed different bytes
        // than the checkpoint authorizes.
        return crate::magician_v2::json_traversal::clone_json_iteratively(&projection.model.value);
    }
    crate::magician_v2::secrets::injection::sanitize_json_for_provider(&projection.model.value)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionContractSpec {
    pub contract_id: ProjectionContractId,
    #[serde(default)]
    pub records_paths: BTreeSet<ProjectionPath>,
    #[serde(default)]
    pub excerpt_paths: BTreeSet<ProjectionPath>,
    #[serde(default)]
    pub priority_fields: Vec<String>,
    /// Complete scalar fields that a capability explicitly permits as a
    /// spoken summary. The generic projector never guesses these from common
    /// names such as `summary` or `title`.
    #[serde(default)]
    pub spoken_fields: Vec<String>,
    /// Scalar fields in each group are admitted as one unit when at least two
    /// are present. This prevents semantically paired facts such as
    /// relationship/value from being separated by a budget boundary.
    #[serde(default)]
    pub atomic_field_groups: Vec<Vec<String>>,
}

impl ProjectionContractSpec {
    pub fn generic() -> Self {
        Self {
            contract_id: ProjectionContractId::new(GENERIC_JSON_CONTRACT_V1)
                .expect("built-in contract id is valid"),
            records_paths: BTreeSet::new(),
            excerpt_paths: BTreeSet::new(),
            // The generic contract is deliberately semantics-free. Domain
            // priorities and atomic relationships belong to capability-owned
            // metadata, never to guesses based on common field names.
            priority_fields: Vec::new(),
            spoken_fields: Vec::new(),
            atomic_field_groups: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), ProjectionError> {
        let mut unique = BTreeSet::new();
        for field in &self.priority_fields {
            if field.trim().is_empty() || field.contains('/') || !unique.insert(field) {
                return Err(ProjectionError::InvalidContractSpec(format!(
                    "contract {} has an invalid or duplicate priority field `{field}`",
                    self.contract_id
                )));
            }
        }
        let mut unique_spoken = BTreeSet::new();
        for field in &self.spoken_fields {
            if field.trim().is_empty() || field.contains('/') || !unique_spoken.insert(field) {
                return Err(ProjectionError::InvalidContractSpec(format!(
                    "contract {} has an invalid or duplicate spoken field `{field}`",
                    self.contract_id
                )));
            }
        }
        for group in &self.atomic_field_groups {
            if group.len() < 2 {
                return Err(ProjectionError::InvalidContractSpec(format!(
                    "contract {} has an atomic field group with fewer than two fields",
                    self.contract_id
                )));
            }
            let mut group_unique = BTreeSet::new();
            for field in group {
                if field.trim().is_empty() || field.contains('/') || !group_unique.insert(field) {
                    return Err(ProjectionError::InvalidContractSpec(format!(
                        "contract {} has an invalid or duplicate atomic field `{field}`",
                        self.contract_id
                    )));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ProjectionContractRegistry {
    contracts: BTreeMap<ProjectionContractId, ProjectionContractSpec>,
    fallback_id: ProjectionContractId,
}

impl Default for ProjectionContractRegistry {
    fn default() -> Self {
        let fallback = ProjectionContractSpec::generic();
        let fallback_id = fallback.contract_id.clone();
        let mut registry = Self {
            contracts: BTreeMap::from([(fallback_id.clone(), fallback)]),
            fallback_id,
        };
        for contract in builtin_contracts() {
            registry
                .register(contract)
                .expect("built-in projection contracts are unique and valid");
        }
        registry
    }
}

impl ProjectionContractRegistry {
    pub fn register(&mut self, contract: ProjectionContractSpec) -> Result<(), ProjectionError> {
        contract.validate()?;
        if self.contracts.contains_key(&contract.contract_id) {
            return Err(ProjectionError::DuplicateContract(
                contract.contract_id.to_string(),
            ));
        }
        self.contracts
            .insert(contract.contract_id.clone(), contract);
        Ok(())
    }

    /// Install a capability-owned contract for one projection invocation.
    ///
    /// Capability metadata carries the complete contract, not just a name.
    /// Replacing the built-in entry in an invocation-local registry ensures
    /// its declared record paths, priority fields, and atomic groups are
    /// actually honored without mutating process-global behavior.
    pub fn register_override(
        &mut self,
        contract: ProjectionContractSpec,
    ) -> Result<(), ProjectionError> {
        contract.validate()?;
        self.contracts
            .insert(contract.contract_id.clone(), contract);
        Ok(())
    }

    pub fn get(&self, contract_id: &ProjectionContractId) -> Option<&ProjectionContractSpec> {
        self.contracts.get(contract_id)
    }

    pub fn resolve(
        &self,
        requested: Option<&ProjectionContractId>,
    ) -> ResolvedProjectionContract<'_> {
        match requested.and_then(|id| self.contracts.get(id)) {
            Some(contract) => ResolvedProjectionContract {
                contract,
                requested_contract_id: requested.cloned(),
                used_generic_fallback: false,
            },
            None => ResolvedProjectionContract {
                contract: self
                    .contracts
                    .get(&self.fallback_id)
                    .expect("projection registry always contains its fallback"),
                requested_contract_id: requested.cloned(),
                used_generic_fallback: requested.is_some(),
            },
        }
    }

    pub fn contract_ids(&self) -> impl Iterator<Item = &ProjectionContractId> {
        self.contracts.keys()
    }
}

#[derive(Clone, Debug)]
pub struct ResolvedProjectionContract<'a> {
    pub contract: &'a ProjectionContractSpec,
    pub requested_contract_id: Option<ProjectionContractId>,
    pub used_generic_fallback: bool,
}

pub struct ToolResultProjectionRequest<'a> {
    pub identity: ToolResultIdentity,
    pub outcome: ToolOutcome,
    pub raw_result: &'a Value,
    pub raw: RawResultDescriptor,
    pub display: DisplayResultProjection,
    pub spoken_hint: Option<&'a str>,
    pub contract_id: Option<&'a ProjectionContractId>,
    pub budget: ProjectionBudget,
}

#[derive(Debug, Error)]
pub enum ProjectionError {
    #[error("invalid projection contract id `{0}`")]
    InvalidContractId(String),
    #[error("invalid projection path `{0}`")]
    InvalidProjectionPath(String),
    #[error("invalid projection budget: {0}")]
    InvalidBudget(String),
    #[error("invalid projection contract: {0}")]
    InvalidContractSpec(String),
    #[error("invalid canonical raw-result descriptor: {0}")]
    InvalidRawDescriptor(String),
    #[error("projection contract `{0}` is already registered")]
    DuplicateContract(String),
    #[error("unsupported tool-result projection schema version `{0}`")]
    UnsupportedSchemaVersion(u16),
    #[error(
        "projection budget is too small: requires at least {minimum_required_bytes} bytes and {minimum_required_tokens} estimated tokens"
    )]
    BudgetTooSmall {
        minimum_required_bytes: usize,
        minimum_required_tokens: usize,
    },
    #[error("could not serialize tool result projection: {0}")]
    Serialization(#[from] serde_json::Error),
}

pub struct ToolResultProjector<E = ConservativeTokenEstimator> {
    registry: ProjectionContractRegistry,
    token_estimator: E,
}

impl Default for ToolResultProjector<ConservativeTokenEstimator> {
    fn default() -> Self {
        Self {
            registry: ProjectionContractRegistry::default(),
            token_estimator: ConservativeTokenEstimator,
        }
    }
}

impl<E> ToolResultProjector<E>
where
    E: ProjectionTokenEstimator,
{
    pub fn new(registry: ProjectionContractRegistry, token_estimator: E) -> Self {
        Self {
            registry,
            token_estimator,
        }
    }

    pub fn registry(&self) -> &ProjectionContractRegistry {
        &self.registry
    }

    pub fn project(
        &self,
        request: ToolResultProjectionRequest<'_>,
    ) -> Result<ProjectedToolResultV1, ProjectionError> {
        request.budget.validate()?;
        request.raw.validate()?;
        let resolved = self.registry.resolve(request.contract_id);
        let raw_metrics = inspect_json(request.raw_result);
        let raw_bytes = json_encoded_len(request.raw_result)?;
        let maximum_input_depth = raw_metrics.max_depth;

        let spoken = derive_spoken_projection(
            request.spoken_hint,
            request.raw_result,
            &request.outcome,
            request.budget.max_spoken_chars,
            &request.raw,
            resolved.contract,
        );

        let contract_fallback = resolved.used_generic_fallback;
        let model_build = build_model_projection(
            request.raw_result,
            &request.outcome,
            &request.raw.content_ref,
            resolved,
            &request.budget,
            &self.token_estimator,
        )?;

        Ok(ProjectedToolResultV1 {
            schema_version: TOOL_RESULT_PROJECTION_SCHEMA_VERSION,
            identity: request.identity,
            outcome: request.outcome,
            model: ModelResultProjection {
                value: model_build.value,
                strategy: model_build.strategy,
                included_records: model_build.included_records,
                omitted_records: model_build.omitted_records,
                omitted_fields: model_build.omitted_fields,
                continuation: model_build.continuation,
            },
            app_result_checkpoint: None,
            metrics: ProjectionMetrics {
                raw_bytes,
                model_bytes: model_build.model_bytes,
                estimated_model_tokens: model_build.estimated_tokens,
                spoken_characters: spoken
                    .as_ref()
                    .map(|projection| projection.text.chars().count())
                    .unwrap_or_default(),
                included_records: model_build.included_records,
                omitted_records: model_build.omitted_records,
                omitted_fields: model_build.omitted_fields,
                maximum_input_depth,
                contract_fallback,
            },
            spoken,
            display: request.display,
            raw: request.raw,
        })
    }
}

#[derive(Debug)]
enum AtomicUnitValue<'a> {
    /// Ordinary fields and array records remain borrowed from the durable raw
    /// result until the selector proves that they fit the model projection.
    Borrowed(&'a Value),
    /// Contract-declared atomic field groups synthesize one shallow object so
    /// the fields remain all-or-nothing during admission.
    Owned(Value),
}

impl AtomicUnitValue<'_> {
    fn as_value(&self) -> &Value {
        match self {
            Self::Borrowed(value) => *value,
            Self::Owned(value) => value,
        }
    }
}

#[derive(Debug)]
struct AtomicUnit<'a> {
    path: Vec<String>,
    record_index: Option<usize>,
    record_count: usize,
    field_count: usize,
    merge_object: bool,
    value: AtomicUnitValue<'a>,
    ordering: UnitOrdering,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UnitOrdering {
    priority: usize,
    traversal_index: usize,
}

#[derive(Default)]
struct CollectedUnits<'a> {
    units: Vec<AtomicUnit<'a>>,
    forced_omissions: Vec<ProjectionOmission>,
    excerpts: Vec<ProjectionExcerpt>,
    /// See `excerpt_cap_hint`. Applies at excerpt paths only.
    excerpt_cap: Option<usize>,
}

struct ModelBuild {
    value: Value,
    strategy: ProjectionStrategy,
    included_records: usize,
    omitted_records: usize,
    omitted_fields: usize,
    continuation: Option<ScopedResultRef>,
    model_bytes: usize,
    estimated_tokens: usize,
}

#[derive(Serialize)]
struct ModelWire<'a> {
    outcome: ModelOutcomeWire<'a>,
    data: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    projection: Option<&'a ModelProjectionMetadata>,
}

/// Provider-facing outcome shape. The durable projection keeps the complete
/// typed [`ToolOutcome`]; the model view omits a false default so simple tool
/// results do not pay a recurring token/latency tax for internal bookkeeping.
#[derive(Serialize)]
struct ModelOutcomeWire<'a> {
    status: ToolOutcomeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'a str>,
    #[serde(skip_serializing_if = "is_false")]
    retryable: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn build_model_projection<E: ProjectionTokenEstimator>(
    raw: &Value,
    outcome: &ToolOutcome,
    full_result_ref: &ScopedResultRef,
    resolved: ResolvedProjectionContract<'_>,
    budget: &ProjectionBudget,
    estimator: &E,
) -> Result<ModelBuild, ProjectionError> {
    let (model_outcome, outcome_omissions) = bounded_model_outcome(outcome, budget)?;
    if outcome_omissions.is_empty() && complete_value_allowed(raw, budget) {
        let metadata = ModelProjectionMetadata {
            contract_id: resolved.contract.contract_id.clone(),
            requested_contract_id: resolved.requested_contract_id.clone(),
            used_generic_fallback: resolved.used_generic_fallback,
            complete: true,
            complete_units_only: true,
            included_records: count_array_entries(raw),
            omitted_records: 0,
            omitted_fields: 0,
            omissions: Vec::new(),
            excerpts: Vec::new(),
            full_result_ref: None,
        };
        // Completeness, contract identity, metrics, and schema version remain
        // on `ProjectedToolResultV1`. Repeating them inside a complete model
        // view conveys no evidence and made small tool calls materially larger.
        if let Some((model_bytes, estimated_tokens)) =
            measure_model_wire_if_fits(&model_outcome, raw, None, budget, estimator)?
        {
            // Materialize the complete model value only after the borrowed wire
            // proves it fits. Large shallow objects otherwise cloned in full
            // before immediately falling through to partial projection.
            let value = model_wire_value(&model_outcome, raw, None)?;
            return Ok(ModelBuild {
                value,
                strategy: ProjectionStrategy::Complete,
                included_records: metadata.included_records,
                omitted_records: 0,
                omitted_fields: 0,
                continuation: None,
                model_bytes,
                estimated_tokens,
            });
        }
    }

    let mut collected = collect_atomic_units(raw, resolved.contract, budget)?;
    collected.forced_omissions.extend(outcome_omissions);
    collected.units.sort_by(|left, right| {
        left.ordering
            .priority
            .cmp(&right.ordering.priority)
            .then_with(|| {
                left.ordering
                    .traversal_index
                    .cmp(&right.ordering.traversal_index)
            })
    });

    let mut admitted = Vec::<AtomicUnit<'_>>::new();
    let mut admitted_data = empty_projection_data(raw);
    let mut rejected = collected.forced_omissions;
    let mut admitted_records = 0usize;
    let empty_metadata = ModelProjectionMetadata {
        contract_id: resolved.contract.contract_id.clone(),
        requested_contract_id: resolved.requested_contract_id.clone(),
        used_generic_fallback: resolved.used_generic_fallback,
        complete: false,
        complete_units_only: true,
        included_records: 0,
        omitted_records: 0,
        omitted_fields: 0,
        omissions: Vec::new(),
        excerpts: Vec::new(),
        full_result_ref: Some(full_result_ref.clone()),
    };

    for unit in collected.units {
        if unit.record_index.is_some()
            && admitted_records.saturating_add(unit.record_count) > budget.max_records
        {
            rejected.push(omission_for_unit(
                &unit,
                ProjectionOmissionKind::RecordLimit,
            )?);
            continue;
        }
        insert_unit(&mut admitted_data, &unit);
        if measure_model_wire_if_fits(
            &model_outcome,
            &admitted_data,
            Some(&empty_metadata),
            budget,
            estimator,
        )?
        .is_some()
        {
            if unit.record_index.is_some() {
                admitted_records += unit.record_count;
            }
            admitted.push(unit);
        } else {
            remove_unit(&mut admitted_data, &unit, raw);
            let kind = if unit.record_index.is_some() {
                ProjectionOmissionKind::OversizedRecord
            } else {
                ProjectionOmissionKind::Budget
            };
            rejected.push(omission_for_unit(&unit, kind)?);
        }
    }

    let mut excerpts = collected.excerpts;
    let mut compact_omission_details = false;
    loop {
        let omissions = if compact_omission_details {
            compact_omissions(&rejected)?
        } else {
            rejected.clone()
        };
        let included_records = admitted.iter().map(|unit| unit.record_count).sum();
        let omitted_records = rejected
            .iter()
            .filter(|omission| omission.record_index.is_some())
            .map(|omission| omission.omitted_units)
            .sum();
        let omitted_fields = rejected
            .iter()
            .filter(|omission| omission.record_index.is_none())
            .map(|omission| omission.omitted_units)
            .sum();
        let metadata = ModelProjectionMetadata {
            contract_id: resolved.contract.contract_id.clone(),
            requested_contract_id: resolved.requested_contract_id.clone(),
            used_generic_fallback: resolved.used_generic_fallback,
            complete: false,
            complete_units_only: true,
            included_records,
            omitted_records,
            omitted_fields,
            omissions,
            excerpts: excerpts.clone(),
            full_result_ref: Some(full_result_ref.clone()),
        };
        if let Some((model_bytes, estimated_tokens)) = measure_model_wire_if_fits(
            &model_outcome,
            &admitted_data,
            Some(&metadata),
            budget,
            estimator,
        )? {
            let value = model_wire_value(&model_outcome, &admitted_data, Some(&metadata))?;
            let strategy = if admitted.is_empty() && excerpts.is_empty() {
                ProjectionStrategy::ReferenceOnly
            } else if resolved.contract.contract_id.as_str() == GENERIC_JSON_CONTRACT_V1 {
                ProjectionStrategy::GenericStructured
            } else {
                ProjectionStrategy::ContractStructured
            };
            return Ok(ModelBuild {
                value,
                strategy,
                included_records,
                omitted_records,
                omitted_fields,
                continuation: Some(full_result_ref.clone()),
                model_bytes,
                estimated_tokens,
            });
        }

        if !compact_omission_details && rejected.len() > 1 {
            compact_omission_details = true;
            continue;
        }
        if shrink_or_remove_last_excerpt(&mut excerpts) {
            continue;
        }
        if let Some(unit) = admitted.pop() {
            remove_unit(&mut admitted_data, &unit, raw);
            rejected.push(omission_for_unit(&unit, ProjectionOmissionKind::Budget)?);
            compact_omission_details = rejected.len() > 1;
            continue;
        }

        let encoded = model_wire_canonical_bytes(&model_outcome, &admitted_data, Some(&metadata))?;
        let bytes = encoded.len();
        let tokens = estimator.estimate_tokens(&encoded);
        return Err(ProjectionError::BudgetTooSmall {
            minimum_required_bytes: bytes,
            minimum_required_tokens: tokens,
        });
    }
}

/// Preserve some boundary-aligned document evidence when envelope metadata
/// makes the initially selected excerpt too large. Dropping the entire excerpt
/// on the first failed fit turns a useful document projection into a
/// reference-only result even when a shorter complete sentence would fit.
fn shrink_or_remove_last_excerpt(excerpts: &mut Vec<ProjectionExcerpt>) -> bool {
    let Some(last) = excerpts.last_mut() else {
        return false;
    };
    if last.text.len() <= 1 {
        excerpts.pop();
        return true;
    }
    let target_bytes = last.text.len().div_ceil(2);
    let Some((shorter, boundary)) = boundary_aligned_excerpt(&last.text, target_bytes) else {
        excerpts.pop();
        return true;
    };
    if shorter.len() >= last.text.len() {
        excerpts.pop();
        return true;
    }
    let shorter_len = shorter.len();
    last.text.truncate(shorter_len);
    last.end_byte = last.start_byte.saturating_add(last.text.len());
    last.boundary = boundary;
    true
}

fn bounded_model_outcome(
    outcome: &ToolOutcome,
    budget: &ProjectionBudget,
) -> Result<(ToolOutcome, Vec<ProjectionOmission>), serde_json::Error> {
    let mut model_outcome = outcome.clone();
    let mut omissions = Vec::new();
    let outcome_message_limit = budget.max_scalar_bytes.min(
        budget
            .max_serialized_bytes
            .saturating_sub(MIN_MODEL_PROJECTION_BYTES)
            .div_ceil(2),
    );
    if let Some(code) = outcome
        .code
        .as_ref()
        .filter(|code| code.len() > budget.max_scalar_bytes)
    {
        omissions.push(omission_for_value(
            &["outcome".to_owned(), "code".to_owned()],
            &Value::String(code.clone()),
            ProjectionOmissionKind::OversizedScalar,
            None,
        )?);
        model_outcome.code = None;
    }
    if let Some(message) = outcome
        .message
        .as_ref()
        .filter(|message| message.len() > outcome_message_limit)
    {
        omissions.push(omission_for_value(
            &["outcome".to_owned(), "message".to_owned()],
            &Value::String(message.clone()),
            ProjectionOmissionKind::OversizedScalar,
            None,
        )?);
        model_outcome.message = None;
    }
    Ok((model_outcome, omissions))
}

#[cfg(any(test, feature = "test-fixtures"))]
thread_local! {
    static MODEL_WIRE_VALUE_MATERIALIZATIONS: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

fn model_wire_value(
    outcome: &ToolOutcome,
    data: &Value,
    metadata: Option<&ModelProjectionMetadata>,
) -> Result<Value, serde_json::Error> {
    #[cfg(any(test, feature = "test-fixtures"))]
    MODEL_WIRE_VALUE_MATERIALIZATIONS.with(|count| count.set(count.get().saturating_add(1)));
    serde_json::to_value(ModelWire {
        outcome: ModelOutcomeWire {
            status: outcome.status,
            code: outcome.code.as_deref(),
            message: outcome.message.as_deref(),
            retryable: outcome.retryable,
        },
        data,
        projection: metadata,
    })
    .map(canonicalize_owned_json)
}

/// Serialize the canonical model wire without first cloning `data` into a
/// second `serde_json::Value`. The projection selector invokes this for every
/// candidate unit, so routing the large data lane through `to_value` made the
/// selection loop quadratic in both copied bytes and temporary heap use.
///
/// The top-level field order below is the lexicographic order produced by
/// `canonicalize_owned_json`, and every component is independently emitted by
/// the same canonical writer. This therefore remains byte-for-byte equivalent
/// to `canonical_json_bytes(model_wire_value(...))` while borrowing the large
/// data tree.
fn model_wire_canonical_bytes(
    outcome: &ToolOutcome,
    data: &Value,
    metadata: Option<&ModelProjectionMetadata>,
) -> Result<Vec<u8>, serde_json::Error> {
    let data_bytes = canonical_json_bytes(data)?;
    let outcome_value = serde_json::to_value(ModelOutcomeWire {
        status: outcome.status,
        code: outcome.code.as_deref(),
        message: outcome.message.as_deref(),
        retryable: outcome.retryable,
    })?;
    let outcome_bytes = canonical_json_bytes(&outcome_value)?;
    let projection_value = metadata.map(serde_json::to_value).transpose()?;
    let projection_bytes = projection_value
        .as_ref()
        .map(canonical_json_bytes)
        .transpose()?;

    let mut encoded = Vec::with_capacity(
        data_bytes
            .len()
            .saturating_add(outcome_bytes.len())
            .saturating_add(projection_bytes.as_ref().map_or(0, Vec::len))
            .saturating_add(48),
    );
    encoded.extend_from_slice(b"{\"data\":");
    encoded.extend_from_slice(&data_bytes);
    encoded.extend_from_slice(b",\"outcome\":");
    encoded.extend_from_slice(&outcome_bytes);
    if let Some(projection_bytes) = projection_bytes {
        encoded.extend_from_slice(b",\"projection\":");
        encoded.extend_from_slice(&projection_bytes);
    }
    encoded.push(b'}');
    Ok(encoded)
}

fn measure_model_wire_if_fits<E: ProjectionTokenEstimator>(
    outcome: &ToolOutcome,
    data: &Value,
    metadata: Option<&ModelProjectionMetadata>,
    budget: &ProjectionBudget,
    estimator: &E,
) -> Result<Option<(usize, usize)>, serde_json::Error> {
    let bytes = model_wire_canonical_bytes(outcome, data, metadata)?;
    let byte_count = bytes.len();
    let token_count = estimator.estimate_tokens(&bytes);
    Ok(
        (byte_count <= budget.max_serialized_bytes && token_count <= budget.max_estimated_tokens)
            .then_some((byte_count, token_count)),
    )
}

/// The reserved top-level key a producer uses to shape its own projection.
/// Never projected to the model itself.
pub const PROJECTION_HINTS_FIELD: &str = "projection_hints";

/// A producer's cap on the excerpts cut at the contract's `excerpt_paths`, in
/// bytes. Bounded by the surface's scalar budget — a producer can only ask
/// for less than the surface allows, never more.
///
/// The case it exists for: a read whose page is held whole in a working set
/// wants the model to see a short head of it and search for the rest, but the
/// raw record must keep the full text and its content hash. The producer
/// cannot shrink the text without breaking the hash; it can ask the
/// projection to.
fn excerpt_cap_hint(raw: &Value, budget: &ProjectionBudget) -> Option<usize> {
    raw.get(PROJECTION_HINTS_FIELD)?
        .get("excerpt_max_bytes")?
        .as_u64()
        .and_then(|bytes| usize::try_from(bytes).ok())
        .filter(|bytes| *bytes > 0)
        .map(|bytes| bytes.min(budget.max_scalar_bytes))
}

fn complete_value_allowed(value: &Value, budget: &ProjectionBudget) -> bool {
    json_depth(value) <= budget.max_depth
        && count_array_entries(value) <= budget.max_records
        && maximum_scalar_bytes(value) <= budget.max_scalar_bytes
        // A producer that capped its excerpts is asking for less than the
        // whole value; the whole value is not allowed however small it is.
        && excerpt_cap_hint(value, budget).is_none()
}

/// The size a string at `path` may reach and still be admitted whole. At an
/// excerpt path with a producer cap, the cap; everywhere else the surface's
/// scalar budget.
fn excerpt_bound_for(
    path: &[String],
    _text: &str,
    contract: &ProjectionContractSpec,
    budget: &ProjectionBudget,
    excerpt_cap: Option<usize>,
) -> usize {
    match excerpt_cap {
        Some(cap)
            if contract
                .excerpt_paths
                .contains(&ProjectionPath::from_segments(path)) =>
        {
            cap.min(budget.max_scalar_bytes)
        },
        _ => budget.max_scalar_bytes,
    }
}

fn collect_atomic_units<'a>(
    raw: &'a Value,
    contract: &ProjectionContractSpec,
    budget: &ProjectionBudget,
) -> Result<CollectedUnits<'a>, serde_json::Error> {
    let mut collected = CollectedUnits {
        excerpt_cap: excerpt_cap_hint(raw, budget),
        ..CollectedUnits::default()
    };
    let mut traversal_index = 0usize;
    collect_value(
        raw,
        &mut Vec::new(),
        0,
        contract,
        budget,
        &mut traversal_index,
        &mut collected,
    )?;
    Ok(collected)
}

#[allow(clippy::too_many_arguments)]
fn collect_value<'a>(
    value: &'a Value,
    path: &mut Vec<String>,
    depth: usize,
    contract: &ProjectionContractSpec,
    budget: &ProjectionBudget,
    traversal_index: &mut usize,
    collected: &mut CollectedUnits<'a>,
) -> Result<(), serde_json::Error> {
    if depth > budget.max_depth {
        collected.forced_omissions.push(omission_for_value(
            path,
            value,
            ProjectionOmissionKind::DepthLimit,
            None,
        )?);
        return Ok(());
    }

    // The producer's projection hints shape the projection; they are not
    // part of it.
    if depth == 1 && path.len() == 1 && path[0] == PROJECTION_HINTS_FIELD {
        return Ok(());
    }

    match value {
        Value::Object(object) if !object.is_empty() => {
            let grouped_fields = collect_atomic_field_groups(
                object,
                path,
                depth,
                contract,
                budget,
                traversal_index,
                collected,
            );
            let mut fields = object.iter().collect::<Vec<_>>();
            fields.sort_by(|(left_key, left_value), (right_key, right_value)| {
                compare_fields(left_key, left_value, right_key, right_value, contract)
            });
            for (key, child) in fields {
                if grouped_fields.contains(key) {
                    continue;
                }
                path.push(key.to_string());
                collect_value(
                    child,
                    path,
                    depth + 1,
                    contract,
                    budget,
                    traversal_index,
                    collected,
                )?;
                path.pop();
            }
        },
        Value::Array(values) => {
            if values.is_empty() {
                push_atomic_unit(
                    path,
                    None,
                    value,
                    field_priority(path.last().map(String::as_str), value, contract),
                    traversal_index,
                    collected,
                );
                return Ok(());
            }
            for (index, record) in values.iter().enumerate() {
                if depth + json_depth(record) > budget.max_depth {
                    collected.forced_omissions.push(omission_for_value(
                        path,
                        record,
                        ProjectionOmissionKind::DepthLimit,
                        Some(index),
                    )?);
                } else if maximum_scalar_bytes(record) > budget.max_scalar_bytes {
                    collected.forced_omissions.push(omission_for_value(
                        path,
                        record,
                        ProjectionOmissionKind::OversizedRecord,
                        Some(index),
                    )?);
                } else {
                    let array_path = ProjectionPath::from_segments(path);
                    let structural_priority =
                        field_priority(path.last().map(String::as_str), value, contract);
                    let priority = if contract.records_paths.contains(&array_path) {
                        structural_priority.min(contract.priority_fields.len().saturating_add(50))
                    } else {
                        structural_priority
                    };
                    push_atomic_unit(
                        path,
                        Some(index),
                        record,
                        priority,
                        traversal_index,
                        collected,
                    );
                }
            }
        },
        Value::String(text)
            if text.len()
                > excerpt_bound_for(path, text, contract, budget, collected.excerpt_cap) =>
        {
            collected.forced_omissions.push(omission_for_value(
                path,
                value,
                ProjectionOmissionKind::OversizedScalar,
                None,
            )?);
            let projection_path = ProjectionPath::from_segments(path);
            if contract.excerpt_paths.contains(&projection_path) {
                let cap = collected
                    .excerpt_cap
                    .map_or(budget.max_scalar_bytes, |cap| {
                        cap.min(budget.max_scalar_bytes)
                    });
                if let Some((excerpt, boundary)) = boundary_aligned_excerpt(text, cap) {
                    collected.excerpts.push(ProjectionExcerpt {
                        path: projection_path,
                        text: excerpt.to_owned(),
                        start_byte: 0,
                        end_byte: excerpt.len(),
                        original_bytes: text.len(),
                        boundary,
                    });
                }
            }
        },
        scalar
            if !scalar.is_array()
                && !scalar.is_object()
                && maximum_scalar_bytes(scalar) > budget.max_scalar_bytes =>
        {
            collected.forced_omissions.push(omission_for_value(
                path,
                scalar,
                ProjectionOmissionKind::OversizedScalar,
                None,
            )?);
        },
        _ => push_atomic_unit(
            path,
            None,
            value,
            field_priority(path.last().map(String::as_str), value, contract),
            traversal_index,
            collected,
        ),
    }
    Ok(())
}

fn push_atomic_unit<'a>(
    path: &[String],
    record_index: Option<usize>,
    value: &'a Value,
    priority: usize,
    traversal_index: &mut usize,
    collected: &mut CollectedUnits<'a>,
) {
    collected.units.push(AtomicUnit {
        path: path.to_vec(),
        record_index,
        record_count: record_index
            .map(|_| 1usize.saturating_add(count_array_entries(value)))
            .unwrap_or_default(),
        field_count: usize::from(record_index.is_none()),
        merge_object: false,
        value: AtomicUnitValue::Borrowed(value),
        ordering: UnitOrdering {
            priority,
            traversal_index: *traversal_index,
        },
    });
    *traversal_index += 1;
}

#[allow(clippy::too_many_arguments)]
fn collect_atomic_field_groups<'a>(
    object: &'a Map<String, Value>,
    path: &[String],
    depth: usize,
    contract: &ProjectionContractSpec,
    budget: &ProjectionBudget,
    traversal_index: &mut usize,
    collected: &mut CollectedUnits<'a>,
) -> BTreeSet<String> {
    if depth >= budget.max_depth {
        return BTreeSet::new();
    }
    let mut grouped_fields = BTreeSet::new();
    for group in &contract.atomic_field_groups {
        let eligible = group
            .iter()
            .filter_map(|field| {
                let value = object.get(field)?;
                (!grouped_fields.contains(field)
                    && !value.is_array()
                    && !value.is_object()
                    && maximum_scalar_bytes(value) <= budget.max_scalar_bytes)
                    .then_some((field, value))
            })
            .collect::<Vec<_>>();
        if eligible.len() < 2 {
            continue;
        }
        let mut grouped_value = Map::new();
        let mut priority = usize::MAX;
        for (field, value) in eligible {
            priority = priority.min(field_priority(Some(field.as_str()), value, contract));
            grouped_value.insert(field.to_string(), canonicalize_json(value));
            grouped_fields.insert(field.to_string());
        }
        let field_count = grouped_value.len();
        collected.units.push(AtomicUnit {
            path: path.to_vec(),
            record_index: None,
            record_count: 0,
            field_count,
            merge_object: true,
            value: AtomicUnitValue::Owned(Value::Object(grouped_value)),
            ordering: UnitOrdering {
                priority,
                traversal_index: *traversal_index,
            },
        });
        *traversal_index += 1;
    }
    grouped_fields
}

fn compare_fields(
    left_key: &str,
    left_value: &Value,
    right_key: &str,
    right_value: &Value,
    contract: &ProjectionContractSpec,
) -> Ordering {
    field_priority(Some(left_key), left_value, contract)
        .cmp(&field_priority(Some(right_key), right_value, contract))
        .then_with(|| left_key.cmp(right_key))
}

fn field_priority(key: Option<&str>, value: &Value, contract: &ProjectionContractSpec) -> usize {
    let Some(key) = key else {
        return 0;
    };
    if let Some(position) = contract
        .priority_fields
        .iter()
        .position(|field| field == key)
    {
        return position;
    }
    // Generic ordering is structural, never name-based: retain inexpensive
    // complete scalar metadata before descending into objects and admitting
    // potentially large array records. Canonical key order remains the tie
    // breaker within each structural class.
    let structural_offset = if value.is_array() {
        2
    } else if value.is_object() {
        1
    } else {
        0
    };
    contract
        .priority_fields
        .len()
        .saturating_add(structural_offset)
}

fn empty_projection_data(raw: &Value) -> Value {
    match raw {
        Value::Object(_) => Value::Object(Map::new()),
        Value::Array(_) => Value::Array(Vec::new()),
        _ => Value::Null,
    }
}

fn insert_unit(root: &mut Value, unit: &AtomicUnit<'_>) {
    let unit_value = unit.value.as_value();
    if unit.merge_object {
        let target = ensure_object_at_path(root, &unit.path);
        if let Value::Object(fields) = unit_value {
            for (key, value) in fields {
                target.insert(
                    key.clone(),
                    crate::magician_v2::json_traversal::clone_json_iteratively(value),
                );
            }
        }
        return;
    }
    if unit.path.is_empty() {
        match unit.record_index {
            Some(_) => {
                if !root.is_array() {
                    *root = Value::Array(Vec::new());
                }
                root.as_array_mut()
                    .expect("root initialized as array")
                    .push(crate::magician_v2::json_traversal::clone_json_iteratively(
                        unit_value,
                    ));
            },
            None => *root = crate::magician_v2::json_traversal::clone_json_iteratively(unit_value),
        }
        return;
    }

    let mut cursor = root;
    for (index, segment) in unit.path.iter().enumerate() {
        let is_leaf = index + 1 == unit.path.len();
        if !cursor.is_object() {
            *cursor = Value::Object(Map::new());
        }
        let object = cursor
            .as_object_mut()
            .expect("cursor initialized as object");
        if is_leaf {
            if unit.record_index.is_some() {
                let entry = object
                    .entry(segment.clone())
                    .or_insert_with(|| Value::Array(Vec::new()));
                if !entry.is_array() {
                    *entry = Value::Array(Vec::new());
                }
                entry
                    .as_array_mut()
                    .expect("entry initialized as array")
                    .push(crate::magician_v2::json_traversal::clone_json_iteratively(
                        unit_value,
                    ));
            } else {
                object.insert(
                    segment.clone(),
                    crate::magician_v2::json_traversal::clone_json_iteratively(unit_value),
                );
            }
            return;
        }
        cursor = object
            .entry(segment.clone())
            .or_insert_with(|| Value::Object(Map::new()));
    }
}

/// Undo the immediately preceding `insert_unit` without rebuilding and
/// recloning every already-admitted unit. Units are evaluated in traversal
/// order, grouped fields are disjoint, and array records append at the tail,
/// making this rollback exact. The only recursion follows a projection path,
/// whose length is admission-bounded by `ProjectionBudget::max_depth`.
fn remove_unit(root: &mut Value, unit: &AtomicUnit<'_>, raw: &Value) {
    let unit_value = unit.value.as_value();
    if unit.path.is_empty() {
        if unit.record_index.is_some() {
            if let Value::Array(items) = root {
                if let Some(removed) = items.pop() {
                    crate::magician_v2::json_traversal::discard_json_iteratively(removed);
                }
            }
        } else if unit.merge_object {
            if let (Value::Object(target), Value::Object(fields)) = (root, unit_value) {
                for key in fields.keys() {
                    if let Some(removed) = target.remove(key) {
                        crate::magician_v2::json_traversal::discard_json_iteratively(removed);
                    }
                }
            }
        } else {
            let removed = std::mem::replace(root, empty_projection_data(raw));
            crate::magician_v2::json_traversal::discard_json_iteratively(removed);
        }
        return;
    }

    if unit.merge_object {
        if let Some(target) = object_at_projection_path_mut(root, &unit.path) {
            if let Value::Object(fields) = unit_value {
                for key in fields.keys() {
                    if let Some(removed) = target.remove(key) {
                        crate::magician_v2::json_traversal::discard_json_iteratively(removed);
                    }
                }
            }
        }
        prune_empty_projection_path(root, &unit.path);
        return;
    }

    let (parent_path, leaf) = unit.path.split_at(unit.path.len().saturating_sub(1));
    if let Some(parent) = object_at_projection_path_mut(root, parent_path) {
        if unit.record_index.is_some() {
            let remove_leaf = match parent.get_mut(&leaf[0]) {
                Some(Value::Array(items)) => {
                    if let Some(removed) = items.pop() {
                        crate::magician_v2::json_traversal::discard_json_iteratively(removed);
                    }
                    items.is_empty()
                },
                _ => false,
            };
            if remove_leaf {
                parent.remove(&leaf[0]);
            }
        } else if let Some(removed) = parent.remove(&leaf[0]) {
            crate::magician_v2::json_traversal::discard_json_iteratively(removed);
        }
    }
    prune_empty_projection_path(root, parent_path);
}

fn object_at_projection_path_mut<'a>(
    root: &'a mut Value,
    path: &[String],
) -> Option<&'a mut Map<String, Value>> {
    let mut cursor = root;
    for segment in path {
        cursor = cursor.as_object_mut()?.get_mut(segment)?;
    }
    cursor.as_object_mut()
}

fn prune_empty_projection_path(value: &mut Value, path: &[String]) -> bool {
    let Value::Object(map) = value else {
        return false;
    };
    let Some((segment, remaining)) = path.split_first() else {
        return map.is_empty();
    };
    let child_is_empty = map
        .get_mut(segment)
        .is_some_and(|child| prune_empty_projection_path(child, remaining));
    if child_is_empty {
        map.remove(segment);
    }
    map.is_empty()
}

fn ensure_object_at_path<'a>(root: &'a mut Value, path: &[String]) -> &'a mut Map<String, Value> {
    let mut cursor = root;
    for segment in path {
        if !cursor.is_object() {
            *cursor = Value::Object(Map::new());
        }
        cursor = cursor
            .as_object_mut()
            .expect("cursor initialized as object")
            .entry(segment.clone())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    if !cursor.is_object() {
        *cursor = Value::Object(Map::new());
    }
    cursor
        .as_object_mut()
        .expect("target initialized as object")
}

fn omission_for_unit(
    unit: &AtomicUnit<'_>,
    kind: ProjectionOmissionKind,
) -> Result<ProjectionOmission, serde_json::Error> {
    let mut omission =
        omission_for_value(&unit.path, unit.value.as_value(), kind, unit.record_index)?;
    if unit.record_index.is_none() {
        omission.omitted_units = unit.field_count.max(1);
    }
    Ok(omission)
}

fn omission_for_value(
    path: &[String],
    value: &Value,
    kind: ProjectionOmissionKind,
    record_index: Option<usize>,
) -> Result<ProjectionOmission, serde_json::Error> {
    Ok(ProjectionOmission {
        path: ProjectionPath::from_segments(path),
        kind,
        original_bytes: json_encoded_len(value)?,
        omitted_units: record_index
            .map(|_| 1usize.saturating_add(count_array_entries(value)))
            .unwrap_or(1),
        record_index,
    })
}

fn compact_omissions(
    omissions: &[ProjectionOmission],
) -> Result<Vec<ProjectionOmission>, serde_json::Error> {
    if omissions.len() <= 1 {
        return Ok(omissions.to_vec());
    }
    Ok(vec![ProjectionOmission {
        path: ProjectionPath::root(),
        kind: ProjectionOmissionKind::CompactedDetails,
        original_bytes: omissions.iter().map(|item| item.original_bytes).sum(),
        omitted_units: omissions.iter().map(|item| item.omitted_units).sum(),
        record_index: None,
    }])
}

fn derive_spoken_projection(
    authored_hint: Option<&str>,
    raw: &Value,
    outcome: &ToolOutcome,
    max_chars: usize,
    raw_descriptor: &RawResultDescriptor,
    contract: &ProjectionContractSpec,
) -> Option<SpokenResultProjection> {
    if let Some(hint) = authored_hint.map(str::trim).filter(|hint| !hint.is_empty()) {
        if hint.chars().count() <= max_chars {
            return Some(SpokenResultProjection {
                text: hint.to_owned(),
                source: SpokenProjectionSource::Authored,
                complete_scalar: true,
            });
        }
    }

    if let Value::Object(object) = raw {
        for field in &contract.spoken_fields {
            if let Some(text) = object.get(field).and_then(Value::as_str) {
                let text = text.trim();
                if !text.is_empty() && text.chars().count() <= max_chars {
                    return Some(SpokenResultProjection {
                        text: text.to_owned(),
                        source: SpokenProjectionSource::StructuredField,
                        complete_scalar: true,
                    });
                }
            }
        }
    }

    let details_available = !raw_descriptor.content_ref.result_ref.trim().is_empty();
    let text = match outcome.status {
        ToolOutcomeStatus::Succeeded if details_available => {
            "The tool completed. Full details are available on screen."
        },
        ToolOutcomeStatus::Succeeded => "The tool completed.",
        ToolOutcomeStatus::Partial if details_available => {
            "The tool returned a partial result. Full details are available on screen."
        },
        ToolOutcomeStatus::Partial => "The tool returned a partial result.",
        ToolOutcomeStatus::Failed => "The tool failed.",
        ToolOutcomeStatus::Denied => "The tool request was denied.",
        ToolOutcomeStatus::Cancelled => "The tool was cancelled.",
        ToolOutcomeStatus::Pending => "The tool is still working.",
        ToolOutcomeStatus::RequiresApproval => "The tool is waiting for approval.",
        ToolOutcomeStatus::TimedOut => "The tool timed out.",
        ToolOutcomeStatus::Revoked => "Access to the tool result was revoked.",
        ToolOutcomeStatus::Unknown => "The tool returned an unrecognized outcome.",
    };
    (text.chars().count() <= max_chars).then(|| SpokenResultProjection {
        text: text.to_owned(),
        source: SpokenProjectionSource::OutcomeFallback,
        complete_scalar: true,
    })
}

fn boundary_aligned_excerpt(text: &str, max_bytes: usize) -> Option<(&str, ExcerptBoundary)> {
    if text.is_empty() || max_bytes == 0 {
        return None;
    }
    let hard_end = text
        .char_indices()
        .take_while(|(index, character)| index + character.len_utf8() <= max_bytes)
        .map(|(index, character)| index + character.len_utf8())
        .last()?;
    let candidate = &text[..hard_end];

    if let Some(index) = candidate.rfind("\n\n") {
        let end = index + 2;
        return (end > 0).then_some((&text[..end], ExcerptBoundary::Paragraph));
    }
    for marker in [". ", "! ", "? ", ".\n", "!\n", "?\n"] {
        if let Some(index) = candidate.rfind(marker) {
            let end = index + 1;
            return (end > 0).then_some((&text[..end], ExcerptBoundary::Sentence));
        }
    }
    candidate
        .rfind(char::is_whitespace)
        .filter(|end| *end > 0)
        .map(|end| (&text[..end], ExcerptBoundary::Word))
}

fn canonicalize_json(value: &Value) -> Value {
    stack_safe_canonicalize_json(value)
}

fn canonicalize_owned_json(value: Value) -> Value {
    canonicalize_json_owned(value)
}

fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>, serde_json::Error> {
    stack_safe_canonical_json_bytes(value)
}

fn json_depth(value: &Value) -> usize {
    inspect_json(value).max_depth
}

fn count_array_entries(value: &Value) -> usize {
    inspect_json(value).array_entries
}

fn maximum_scalar_bytes(value: &Value) -> usize {
    inspect_json(value).maximum_scalar_bytes
}

fn builtin_contracts() -> Vec<ProjectionContractSpec> {
    let contract = |id: &str, records: &[&str], excerpts: &[&str], priority: &[&str]| {
        ProjectionContractSpec {
            contract_id: ProjectionContractId::new(id).expect("built-in contract id is valid"),
            records_paths: records
                .iter()
                .map(|path| ProjectionPath::parse(*path).expect("built-in path is valid"))
                .collect(),
            excerpt_paths: excerpts
                .iter()
                .map(|path| ProjectionPath::parse(*path).expect("built-in path is valid"))
                .collect(),
            priority_fields: priority.iter().map(|field| (*field).to_owned()).collect(),
            spoken_fields: match id {
                SCALAR_OR_OBJECT_CONTRACT_V1 => vec![
                    "speech_live".to_owned(),
                    "voice_summary".to_owned(),
                    "answer".to_owned(),
                    "summary".to_owned(),
                ],
                TASK_RECEIPT_CONTRACT_V1 | ERROR_CONTRACT_V1 => {
                    vec!["voice_summary".to_owned(), "message".to_owned()]
                },
                _ => Vec::new(),
            },
            atomic_field_groups: match id {
                TASK_RECEIPT_CONTRACT_V1 => vec![vec![
                    "status".to_owned(),
                    "state".to_owned(),
                    "task_id".to_owned(),
                    "execution_id".to_owned(),
                ]],
                ERROR_CONTRACT_V1 => vec![vec![
                    "status".to_owned(),
                    "code".to_owned(),
                    "retryable".to_owned(),
                ]],
                // Domain field relationships are capability-owned. Built-in
                // structural contracts must not guess that two commonly
                // named fields form one semantic fact.
                _ => Vec::new(),
            },
        }
    };
    vec![
        contract(
            SCALAR_OR_OBJECT_CONTRACT_V1,
            &[],
            &[],
            &["status", "answer", "value", "message", "error", "code"],
        ),
        contract(
            RANKED_RECORDS_CONTRACT_V1,
            &["/results", "/records", "/matches"],
            &[],
            &[
                "status",
                "results",
                "records",
                "matches",
                "id",
                "key",
                "relationship",
                "value",
                "source",
                "confidence",
            ],
        ),
        contract(
            TABULAR_ROWS_CONTRACT_V1,
            &["/rows"],
            &[],
            &["status", "columns", "rows", "summary"],
        ),
        contract(
            DOCUMENT_SPANS_CONTRACT_V1,
            &[],
            &["/text", "/content", "/body"],
            &[
                "status", "title", "url", "summary", "text", "content", "body",
            ],
        ),
        contract(
            ARTIFACT_MANIFEST_CONTRACT_V1,
            &["/artifacts"],
            &[],
            &[
                "status",
                "artifacts",
                "name",
                "media_type",
                "size_bytes",
                "content_ref",
            ],
        ),
        contract(
            TASK_RECEIPT_CONTRACT_V1,
            &[],
            &[],
            &[
                "status",
                "state",
                "task_id",
                "execution_id",
                "message",
                "error",
            ],
        ),
        contract(
            ERROR_CONTRACT_V1,
            &[],
            // Short error fields are preserved whole. Oversized diagnostic
            // dumps are omitted with an authenticated full-result reference;
            // replaying a large prefix adds latency and can obscure the typed
            // outcome/code without preserving a complete semantic unit.
            &[],
            &["status", "error", "code", "message", "retryable"],
        ),
    ]
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use serde_json::json;

    #[derive(Clone, Copy)]
    struct ByteEstimator;

    impl ProjectionTokenEstimator for ByteEstimator {
        fn estimate_tokens(&self, serialized_json: &[u8]) -> usize {
            serialized_json.len()
        }
    }

    fn identity(tool_name: &str) -> ToolResultIdentity {
        ToolResultIdentity {
            tool_name: tool_name.to_owned(),
            tool_call_id: "call-1".to_owned(),
            execution_id: Some("execution-1".to_owned()),
            task_id: None,
            scope_digest: "scope-digest".to_owned(),
            authority_revision: "authority-v7".to_owned(),
        }
    }

    fn raw_descriptor() -> RawResultDescriptor {
        RawResultDescriptor {
            content_ref: ScopedResultRef {
                result_ref: "result_ref_opaque".to_owned(),
                cursor: None,
            },
            content_hash: "raw-hash".to_owned(),
            media_type: "application/json".to_owned(),
            size_bytes: 9_000,
            retention_class: ResultRetentionClass::ChatSession,
        }
    }

    #[test]
    fn borrowed_model_wire_serialization_matches_canonical_wire_exactly() {
        let outcome = ToolOutcome {
            status: ToolOutcomeStatus::Partial,
            code: Some("partial_fixture".to_string()),
            message: Some("Some records remain available by reference.".to_string()),
            retryable: true,
        };
        let data = json!({
            "z": [{"nested": true}],
            "a": "first"
        });
        let metadata = ModelProjectionMetadata {
            contract_id: ProjectionContractId::new(GENERIC_JSON_CONTRACT_V1).unwrap(),
            requested_contract_id: None,
            used_generic_fallback: false,
            complete: false,
            complete_units_only: true,
            included_records: 1,
            omitted_records: 2,
            omitted_fields: 3,
            omissions: Vec::new(),
            excerpts: Vec::new(),
            full_result_ref: Some(raw_descriptor().content_ref),
        };

        for projection in [None, Some(&metadata)] {
            let owned = model_wire_value(&outcome, &data, projection).unwrap();
            assert_eq!(
                model_wire_canonical_bytes(&outcome, &data, projection).unwrap(),
                canonical_json_bytes(&owned).unwrap()
            );
        }
    }

    #[test]
    fn oversized_complete_candidate_is_measured_before_model_value_materialization() {
        let raw = Value::Object(
            (0..32)
                .map(|index| (format!("field_{index:02}"), json!("x".repeat(256))))
                .collect(),
        );
        MODEL_WIRE_VALUE_MATERIALIZATIONS.with(|count| count.set(0));

        let result = project_with(&raw, 2_048, None, "borrowed-complete-probe")
            .expect("oversized complete candidate falls back to a partial projection");

        assert_ne!(result.model.strategy, ProjectionStrategy::Complete);
        MODEL_WIRE_VALUE_MATERIALIZATIONS.with(|count| {
            assert_eq!(
                count.get(),
                1,
                "only the accepted final projection may be materialized"
            );
        });
    }

    #[test]
    fn rejected_candidate_rollback_restores_incremental_projection_exactly() {
        let raw = json!({"outer": {}, "rows": []});
        let ordering = |traversal_index| UnitOrdering {
            priority: traversal_index,
            traversal_index,
        };
        let scalar = AtomicUnit {
            path: vec!["outer".to_string(), "keep".to_string()],
            record_index: None,
            record_count: 0,
            field_count: 1,
            merge_object: false,
            value: AtomicUnitValue::Owned(json!("retained")),
            ordering: ordering(0),
        };
        let rejected_group = AtomicUnit {
            path: vec!["outer".to_string()],
            record_index: None,
            record_count: 0,
            field_count: 2,
            merge_object: true,
            value: AtomicUnitValue::Owned(json!({"large_a": "x".repeat(4096), "large_b": 2})),
            ordering: ordering(1),
        };
        let record = AtomicUnit {
            path: vec!["rows".to_string()],
            record_index: Some(0),
            record_count: 1,
            field_count: 0,
            merge_object: false,
            value: AtomicUnitValue::Owned(json!({"id": "row-1"})),
            ordering: ordering(2),
        };

        let mut candidate = empty_projection_data(&raw);
        insert_unit(&mut candidate, &scalar);
        let retained = canonical_json_bytes(&candidate).unwrap();

        insert_unit(&mut candidate, &rejected_group);
        remove_unit(&mut candidate, &rejected_group, &raw);
        assert_eq!(canonical_json_bytes(&candidate).unwrap(), retained);

        insert_unit(&mut candidate, &record);
        remove_unit(&mut candidate, &record, &raw);
        assert_eq!(canonical_json_bytes(&candidate).unwrap(), retained);

        remove_unit(&mut candidate, &scalar, &raw);
        assert_eq!(candidate, empty_projection_data(&raw));
    }

    #[test]
    fn default_estimator_matches_magicllm_preflight_and_closes_three_byte_gap() {
        let serialized = vec![b'x'; 501];
        let projection_estimate = ConservativeTokenEstimator.estimate_tokens(&serialized);
        let preflight_estimate: usize = ConservativeOllamaEstimator
            .estimate_serialized_bytes(&serialized)
            .try_into()
            .unwrap();

        assert_eq!(projection_estimate, preflight_estimate);
        assert_eq!(projection_estimate, 251);
        assert!(serialized.len().div_ceil(3) <= 200);
        assert!(projection_estimate > 200);
    }

    #[test]
    fn default_projector_remeasures_final_envelope_with_shared_token_boundary() {
        let raw = json!({
            "records": (0..24)
                .map(|index| json!({
                    "id": format!("record-{index}"),
                    "relationship": "fixture",
                    "value": "complete value with enough bytes to exercise token admission",
                }))
                .collect::<Vec<_>>()
        });
        let descriptor = raw_descriptor();
        let projection_budget = ProjectionBudget {
            max_serialized_bytes: 4_096,
            max_estimated_tokens: 600,
            max_records: 24,
            max_depth: 8,
            max_scalar_bytes: 1_024,
            max_spoken_chars: 128,
        };
        let result = ToolResultProjector::default()
            .project(ToolResultProjectionRequest {
                identity: identity("token-boundary"),
                outcome: ToolOutcome::succeeded(),
                raw_result: &raw,
                display: DisplayResultProjection::referenced(&descriptor),
                raw: descriptor,
                spoken_hint: None,
                contract_id: Some(&ProjectionContractId::new(RANKED_RECORDS_CONTRACT_V1).unwrap()),
                budget: projection_budget.clone(),
            })
            .unwrap();
        let bytes = canonical_json_bytes(&result.model.value).unwrap();
        assert_eq!(
            result.metrics.estimated_model_tokens,
            ConservativeTokenEstimator.estimate_tokens(&bytes)
        );
        assert!(result.metrics.model_bytes <= projection_budget.max_serialized_bytes);
        assert!(result.metrics.estimated_model_tokens <= projection_budget.max_estimated_tokens);
    }

    #[test]
    fn invocation_local_contract_override_keeps_all_capability_metadata() {
        let contract_id = ProjectionContractId::new(RANKED_RECORDS_CONTRACT_V1).unwrap();
        let custom = ProjectionContractSpec {
            contract_id: contract_id.clone(),
            records_paths: BTreeSet::from([ProjectionPath::parse("/custom_rows").unwrap()]),
            excerpt_paths: BTreeSet::from([ProjectionPath::parse("/document").unwrap()]),
            priority_fields: vec!["custom_rows".to_string(), "rank".to_string()],
            spoken_fields: vec!["voice_summary".to_string()],
            atomic_field_groups: vec![vec!["rank".to_string(), "value".to_string()]],
        };
        let mut registry = ProjectionContractRegistry::default();
        registry.register_override(custom.clone()).unwrap();

        assert_eq!(registry.get(&contract_id), Some(&custom));
    }

    fn budget(bytes: usize) -> ProjectionBudget {
        ProjectionBudget {
            max_serialized_bytes: bytes,
            max_estimated_tokens: bytes,
            max_records: 32,
            max_depth: 8,
            max_scalar_bytes: 1_024,
            max_spoken_chars: 420,
        }
    }

    fn project_with(
        raw: &Value,
        bytes: usize,
        contract: Option<&ProjectionContractId>,
        tool_name: &str,
    ) -> Result<ProjectedToolResultV1, ProjectionError> {
        let descriptor = raw_descriptor();
        ToolResultProjector::new(ProjectionContractRegistry::default(), ByteEstimator).project(
            ToolResultProjectionRequest {
                identity: identity(tool_name),
                outcome: ToolOutcome::succeeded(),
                raw_result: raw,
                display: DisplayResultProjection::referenced(&descriptor),
                raw: descriptor,
                spoken_hint: None,
                contract_id: contract,
                budget: budget(bytes),
            },
        )
    }

    fn metadata(result: &ProjectedToolResultV1) -> ModelProjectionMetadata {
        serde_json::from_value(result.model.value["projection"].clone())
            .expect("projection metadata")
    }

    #[test]
    fn contract_ids_validate_on_construction_and_deserialization() {
        assert!(ProjectionContractId::new("ranked_records_v1").is_ok());
        assert!(ProjectionContractId::new("RankedRecords").is_err());
        assert!(ProjectionContractId::new("1_contract").is_err());
        assert!(serde_json::from_str::<ProjectionContractId>("\"bad-id\"").is_err());
        assert!(ProjectionPath::parse("/valid/~0key/~1path").is_ok());
        assert!(ProjectionPath::parse("missing-leading-slash").is_err());
        assert!(serde_json::from_str::<ProjectionPath>("\"/dangling~\"").is_err());
    }

    #[test]
    fn registry_contains_versioned_builtins_and_rejects_duplicates() {
        let mut registry = ProjectionContractRegistry::default();
        let ids = registry
            .contract_ids()
            .map(ProjectionContractId::as_str)
            .collect::<BTreeSet<_>>();
        assert!(ids.contains(GENERIC_JSON_CONTRACT_V1));
        assert!(ids.contains(RANKED_RECORDS_CONTRACT_V1));
        assert!(ids.contains(DOCUMENT_SPANS_CONTRACT_V1));
        assert!(matches!(
            registry.register(ProjectionContractSpec::generic()),
            Err(ProjectionError::DuplicateContract(_))
        ));
    }

    #[test]
    fn unknown_contract_falls_back_without_looking_at_tool_name() {
        let raw = json!({"answer": "same"});
        let unknown = ProjectionContractId::new("unknown_v9").unwrap();
        let first = project_with(&raw, 2_000, Some(&unknown), "search_memory").unwrap();
        let second = project_with(&raw, 2_000, Some(&unknown), "totally_different").unwrap();
        assert!(first.metrics.contract_fallback);
        assert!(second.metrics.contract_fallback);
        assert_eq!(first.model.value, second.model.value);
    }

    #[test]
    fn complete_small_value_preserves_exact_json() {
        let raw = json!({"z": 1, "answer": "exact", "nested": {"ok": true}});
        let result = project_with(&raw, 4_000, None, "example").unwrap();
        assert_eq!(result.model.strategy, ProjectionStrategy::Complete);
        assert_eq!(result.model.value["data"], canonicalize_json(&raw));
        assert!(result.model.value.get("projection").is_none());
        assert!(result.model.value.get("schema_version").is_none());
        assert_eq!(result.model.value["outcome"]["status"], "succeeded");
        assert!(result.model.value["outcome"].get("retryable").is_none());
        assert!(result.model.continuation.is_none());
        assert!(result.validate_schema_version().is_ok());

        let mut future = result;
        future.schema_version = 2;
        assert!(matches!(
            future.validate_schema_version(),
            Err(ProjectionError::UnsupportedSchemaVersion(2))
        ));
    }

    #[test]
    fn ranked_records_are_admitted_atomically_in_source_order() {
        let contract = ProjectionContractId::new(RANKED_RECORDS_CONTRACT_V1).unwrap();
        let raw = json!({
            "status": "ok",
            "results": [
                {"id": "one", "relationship": "spouse", "value": "A complete value", "padding": "x".repeat(400)},
                {"id": "two", "relationship": "birthday", "value": "1988-03-02", "padding": "y".repeat(400)},
                {"id": "three", "value": "third", "padding": "z".repeat(400)}
            ]
        });
        let result = project_with(&raw, 1_100, Some(&contract), "memory-any-name").unwrap();
        let records = result.model.value["data"]["results"]
            .as_array()
            .expect("projected records");
        assert!(!records.is_empty());
        for record in records {
            assert!(record.get("id").is_some());
            assert!(record.get("value").is_some());
        }
        assert!(result.model.omitted_records > 0);
        assert_eq!(
            result.model.included_records + result.model.omitted_records,
            3
        );
    }

    #[test]
    fn declared_atomic_priority_fields_are_one_admission_unit() {
        let raw = json!({
            "relationship": "spouse birthday",
            "value": "1988-03-02",
            "unrelated": "x".repeat(200)
        });
        let contract = ProjectionContractSpec {
            contract_id: ProjectionContractId::new("declared_atomic_fixture_v1").unwrap(),
            records_paths: BTreeSet::new(),
            excerpt_paths: BTreeSet::new(),
            priority_fields: vec!["relationship".to_owned(), "value".to_owned()],
            spoken_fields: Vec::new(),
            atomic_field_groups: vec![vec!["relationship".to_owned(), "value".to_owned()]],
        };
        let collected = collect_atomic_units(&raw, &contract, &budget(2_000)).unwrap();
        let semantic_unit = collected
            .units
            .iter()
            .find(|unit| unit.merge_object)
            .expect("relationship/value atomic group");
        assert_eq!(semantic_unit.field_count, 2);
        assert_eq!(
            semantic_unit.value.as_value()["relationship"],
            "spouse birthday"
        );
        assert_eq!(semantic_unit.value.as_value()["value"], "1988-03-02");
        assert!(!collected.units.iter().any(|unit| {
            unit.path
                .last()
                .is_some_and(|field| field == "relationship" || field == "value")
        }));
    }

    #[test]
    fn ordinary_atomic_units_borrow_raw_records_until_admission() {
        let raw = json!({
            "rows": [
                {"id": "row-1", "payload": "x".repeat(8_000)},
                {"id": "row-2", "payload": "y".repeat(8_000)}
            ]
        });
        let contract = ProjectionContractSpec::generic();
        let mut projection_budget = budget(1_024);
        projection_budget.max_scalar_bytes = 16_000;
        let collected = collect_atomic_units(&raw, &contract, &projection_budget)
            .expect("collect borrowed projection units");
        let records = raw["rows"].as_array().expect("raw records");

        for (index, record) in records.iter().enumerate() {
            let unit = collected
                .units
                .iter()
                .find(|unit| unit.record_index == Some(index))
                .expect("record has one atomic unit");
            match &unit.value {
                AtomicUnitValue::Borrowed(value) => {
                    assert!(std::ptr::eq(*value, record));
                },
                AtomicUnitValue::Owned(_) => {
                    panic!("ordinary records must not be cloned during collection")
                },
            }
        }
    }

    #[test]
    fn generic_contract_uses_canonical_structure_without_domain_field_guesses() {
        let raw = json!({
            "token": "pagination-cursor-27",
            "relationship": "spouse birthday",
            "value": "1988-03-02",
            "rows": [{"id": 1}],
            "alpha": "first"
        });
        let contract = ProjectionContractSpec::generic();
        assert!(contract.priority_fields.is_empty());
        assert!(contract.atomic_field_groups.is_empty());

        let collected = collect_atomic_units(&raw, &contract, &budget(4_000)).unwrap();
        assert!(!collected.units.iter().any(|unit| unit.merge_object));
        let paths = collected
            .units
            .iter()
            .map(|unit| unit.path.join("/"))
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec!["alpha", "relationship", "token", "value", "rows"]
        );
    }

    #[test]
    fn structural_builtins_do_not_invent_domain_atomic_groups() {
        let registry = ProjectionContractRegistry::default();
        for contract_id in [
            GENERIC_JSON_CONTRACT_V1,
            SCALAR_OR_OBJECT_CONTRACT_V1,
            RANKED_RECORDS_CONTRACT_V1,
            TABULAR_ROWS_CONTRACT_V1,
            DOCUMENT_SPANS_CONTRACT_V1,
            ARTIFACT_MANIFEST_CONTRACT_V1,
        ] {
            let contract = registry
                .resolve(Some(&ProjectionContractId::new(contract_id).unwrap()))
                .contract;
            assert!(
                contract.atomic_field_groups.is_empty(),
                "{contract_id} must stay domain-neutral"
            );
        }
    }

    #[test]
    fn record_limit_is_exact_and_typed() {
        let raw = json!({"rows": [{"id": 1}, {"id": 2}, {"id": 3}]});
        let mut limited = budget(4_000);
        limited.max_records = 2;
        let descriptor = raw_descriptor();
        let result = ToolResultProjector::new(ProjectionContractRegistry::default(), ByteEstimator)
            .project(ToolResultProjectionRequest {
                identity: identity("data"),
                outcome: ToolOutcome::succeeded(),
                raw_result: &raw,
                display: DisplayResultProjection::referenced(&descriptor),
                raw: descriptor,
                spoken_hint: None,
                contract_id: None,
                budget: limited,
            })
            .unwrap();
        assert_eq!(result.model.included_records, 2);
        assert_eq!(result.model.omitted_records, 1);
        assert!(metadata(&result)
            .omissions
            .iter()
            .any(|item| item.kind == ProjectionOmissionKind::RecordLimit));
    }

    #[test]
    fn oversized_scalar_is_never_left_under_original_field_as_a_prefix() {
        let raw = json!({"status": "ok", "secretly_long": "token ".repeat(400)});
        let mut projection_budget = budget(1_600);
        projection_budget.max_scalar_bytes = 100;
        let descriptor = raw_descriptor();
        let result = ToolResultProjector::new(ProjectionContractRegistry::default(), ByteEstimator)
            .project(ToolResultProjectionRequest {
                identity: identity("anything"),
                outcome: ToolOutcome::succeeded(),
                raw_result: &raw,
                display: DisplayResultProjection::referenced(&descriptor),
                raw: descriptor,
                spoken_hint: None,
                contract_id: None,
                budget: projection_budget,
            })
            .unwrap();
        assert!(result.model.value["data"].get("secretly_long").is_none());
        assert!(metadata(&result)
            .omissions
            .iter()
            .any(|item| item.kind == ProjectionOmissionKind::OversizedScalar));
    }

    #[test]
    fn document_contract_emits_boundary_aligned_typed_excerpt() {
        let contract = ProjectionContractId::new(DOCUMENT_SPANS_CONTRACT_V1).unwrap();
        let text = "First complete sentence. Second complete sentence. Third sentence is long.";
        let raw = json!({"title": "Document", "text": text.repeat(20)});
        let mut projection_budget = budget(2_000);
        projection_budget.max_scalar_bytes = 80;
        let descriptor = raw_descriptor();
        let result = ToolResultProjector::new(ProjectionContractRegistry::default(), ByteEstimator)
            .project(ToolResultProjectionRequest {
                identity: identity("reader-renamed-without-effect"),
                outcome: ToolOutcome::succeeded(),
                raw_result: &raw,
                display: DisplayResultProjection::referenced(&descriptor),
                raw: descriptor,
                spoken_hint: None,
                contract_id: Some(&contract),
                budget: projection_budget,
            })
            .unwrap();
        let projection = metadata(&result);
        let excerpt = projection.excerpts.first().expect("typed excerpt");
        assert_eq!(excerpt.path.as_str(), "/text");
        assert_eq!(
            &raw["text"].as_str().unwrap()[..excerpt.end_byte],
            excerpt.text
        );
        assert!(matches!(
            excerpt.boundary,
            ExcerptBoundary::Sentence | ExcerptBoundary::Paragraph | ExcerptBoundary::Word
        ));
        assert!(result.model.value["data"].get("text").is_none());
    }

    /// A producer whose page is held whole elsewhere may ask the projection
    /// for a short head of it. The cap applies at the contract's excerpt paths
    /// only, is bounded by the surface, and turns off the whole-value fast
    /// path — a 7,000-byte text under the 8 KiB scalar budget would otherwise
    /// reach the model whole however small the cap. The raw record is not
    /// touched: the hash over the full text still holds.
    #[test]
    fn a_producer_can_cap_its_own_excerpts_without_touching_the_record() {
        let contract = ProjectionContractId::new(DOCUMENT_SPANS_CONTRACT_V1).unwrap();
        // Small enough to fit the whole-value path under this test's
        // byte-per-token estimator, large enough that a 600-byte cap bites.
        let text = "First complete sentence. Second complete sentence. ".repeat(60); // ~3,000 bytes
        let descriptor = raw_descriptor();
        let projection_budget = ProjectionBudget {
            max_serialized_bytes: 24_576,
            max_estimated_tokens: 6_144,
            max_records: 20,
            max_depth: 8,
            max_scalar_bytes: 8_192,
            max_spoken_chars: 128,
        };
        let project = |raw: &Value| {
            ToolResultProjector::new(ProjectionContractRegistry::default(), ByteEstimator)
                .project(ToolResultProjectionRequest {
                    identity: identity("capped-reader"),
                    outcome: ToolOutcome::succeeded(),
                    raw_result: raw,
                    display: DisplayResultProjection::referenced(&descriptor),
                    raw: descriptor.clone(),
                    spoken_hint: None,
                    contract_id: Some(&contract),
                    budget: projection_budget.clone(),
                })
                .unwrap()
        };

        // Without a hint the value fits the budget and goes whole.
        let plain = json!({"title": "Page", "text": text});
        let whole = project(&plain);
        assert_eq!(
            whole.model.value["data"]["text"].as_str().map(str::len),
            Some(text.len()),
            "under the budget the whole text reaches the model"
        );

        // With a hint the text is excerpted at the cap, the hint itself is
        // not projected, and nothing else changes.
        let capped = json!({
            "title": "Page",
            "text": text,
            PROJECTION_HINTS_FIELD: {"excerpt_max_bytes": 600},
        });
        let result = project(&capped);
        assert!(
            result.model.value["data"].get("text").is_none(),
            "the full text is out"
        );
        assert!(
            result.model.value["data"]
                .get(PROJECTION_HINTS_FIELD)
                .is_none(),
            "hints are not content"
        );
        assert_eq!(result.model.value["data"]["title"], "Page");
        let excerpt = metadata(&result)
            .excerpts
            .first()
            .expect("a capped excerpt")
            .clone();
        assert_eq!(excerpt.path.as_str(), "/text");
        assert!(
            excerpt.text.len() <= 600,
            "excerpt was {} bytes",
            excerpt.text.len()
        );
        assert!(text.starts_with(&excerpt.text));
        assert_eq!(excerpt.original_bytes, text.len());

        // A cap above the surface's scalar budget is the surface's budget.
        let generous = json!({
            "title": "Page",
            "text": "A sentence with a boundary. ".repeat(800), // ~22 KB, over the scalar budget
            PROJECTION_HINTS_FIELD: {"excerpt_max_bytes": 1_000_000},
        });
        let result = project(&generous);
        let excerpt = metadata(&result).excerpts.first().expect("excerpt").clone();
        assert!(excerpt.text.len() <= projection_budget.max_scalar_bytes);
    }

    #[test]
    fn oversized_document_excerpt_shrinks_at_a_boundary_before_becoming_reference_only() {
        let original = "First complete sentence. Second complete sentence. Third complete sentence. Fourth complete sentence.";
        let mut excerpts = vec![ProjectionExcerpt {
            path: ProjectionPath::parse("/text").unwrap(),
            text: original.to_owned(),
            start_byte: 0,
            end_byte: original.len(),
            original_bytes: original.len() * 10,
            boundary: ExcerptBoundary::Sentence,
        }];

        assert!(shrink_or_remove_last_excerpt(&mut excerpts));
        let excerpt = excerpts.first().expect("a shorter excerpt survives");
        assert!(excerpt.text.len() < original.len());
        assert!(original.starts_with(&excerpt.text));
        assert_eq!(excerpt.end_byte, excerpt.text.len());
        assert!(matches!(
            excerpt.boundary,
            ExcerptBoundary::Sentence | ExcerptBoundary::Paragraph | ExcerptBoundary::Word
        ));
    }

    #[test]
    fn depth_limit_omits_the_complete_subtree() {
        let raw = json!({"a": {"b": {"c": {"d": "value"}}}});
        let mut projection_budget = budget(2_000);
        projection_budget.max_depth = 2;
        let descriptor = raw_descriptor();
        let result = ToolResultProjector::new(ProjectionContractRegistry::default(), ByteEstimator)
            .project(ToolResultProjectionRequest {
                identity: identity("nested"),
                outcome: ToolOutcome::succeeded(),
                raw_result: &raw,
                display: DisplayResultProjection::referenced(&descriptor),
                raw: descriptor,
                spoken_hint: None,
                contract_id: None,
                budget: projection_budget,
            })
            .unwrap();
        assert!(metadata(&result)
            .omissions
            .iter()
            .any(|item| item.kind == ProjectionOmissionKind::DepthLimit));
        assert_ne!(result.model.value["data"], raw);
    }

    #[test]
    fn default_stack_deeply_nested_tool_output_projects_without_recursion() {
        let mut raw = Value::String("deep evidence".to_string());
        for _ in 0..2_048 {
            raw = Value::Array(vec![raw]);
        }
        let mut projection_budget = budget(4_000);
        projection_budget.max_depth = 16;
        let descriptor = raw_descriptor();
        let result = ToolResultProjector::new(ProjectionContractRegistry::default(), ByteEstimator)
            .project(ToolResultProjectionRequest {
                identity: identity("adversarial-depth"),
                outcome: ToolOutcome::succeeded(),
                raw_result: &raw,
                display: DisplayResultProjection::referenced(&descriptor),
                raw: descriptor,
                spoken_hint: None,
                contract_id: None,
                budget: projection_budget,
            })
            .expect("deep external JSON must be projected without native recursion");

        assert_eq!(result.metrics.maximum_input_depth, 2_048);
        assert!(metadata(&result)
            .omissions
            .iter()
            .any(|item| item.kind == ProjectionOmissionKind::DepthLimit));
        crate::magician_v2::json_traversal::discard_json_iteratively(raw);
    }

    #[test]
    fn projection_budget_cannot_reenable_unbounded_retained_depth() {
        let mut projection_budget = budget(4_000);
        projection_budget.max_depth = MAX_RETAINED_JSON_DEPTH + 1;
        assert!(matches!(
            projection_budget.validate(),
            Err(ProjectionError::InvalidBudget(message))
                if message.contains("must not exceed")
        ));
    }

    #[test]
    fn serialized_model_respects_byte_and_token_budgets_after_metadata() {
        let raw = json!({
            "records": (0..40).map(|index| json!({"id": index, "value": "x".repeat(90)})).collect::<Vec<_>>()
        });
        let result = project_with(&raw, 1_200, None, "bounded").unwrap();
        let bytes = canonical_json_bytes(&result.model.value).unwrap();
        assert!(bytes.len() <= 1_200);
        assert!(result.metrics.estimated_model_tokens <= 1_200);
        assert_eq!(result.metrics.model_bytes, bytes.len());
    }

    #[test]
    fn tight_budget_uses_reference_only_instead_of_chopping_json() {
        let raw = json!({"payload": "large value with complete words. ".repeat(200)});
        let mut projection_budget = budget(700);
        projection_budget.max_scalar_bytes = 32;
        let descriptor = raw_descriptor();
        let result = ToolResultProjector::new(ProjectionContractRegistry::default(), ByteEstimator)
            .project(ToolResultProjectionRequest {
                identity: identity("large"),
                outcome: ToolOutcome::succeeded(),
                raw_result: &raw,
                display: DisplayResultProjection::referenced(&descriptor),
                raw: descriptor,
                spoken_hint: None,
                contract_id: None,
                budget: projection_budget,
            })
            .unwrap();
        assert_eq!(result.model.strategy, ProjectionStrategy::ReferenceOnly);
        assert_eq!(
            result.model.continuation.as_ref().unwrap().result_ref,
            "result_ref_opaque"
        );
        let serialized = serde_json::to_string(&result.model.value).unwrap();
        assert!(serde_json::from_str::<Value>(&serialized).is_ok());
    }

    #[test]
    fn impossible_budget_fails_explicitly() {
        let raw = json!({"payload": "x".repeat(500)});
        let mut projection_budget = budget(MIN_MODEL_PROJECTION_BYTES);
        projection_budget.max_scalar_bytes = 1;
        let descriptor = raw_descriptor();
        let error = ToolResultProjector::new(ProjectionContractRegistry::default(), ByteEstimator)
            .project(ToolResultProjectionRequest {
                identity: identity("large"),
                outcome: ToolOutcome::succeeded(),
                raw_result: &raw,
                display: DisplayResultProjection::referenced(&descriptor),
                raw: descriptor,
                spoken_hint: None,
                contract_id: None,
                budget: projection_budget,
            })
            .unwrap_err();
        assert!(matches!(error, ProjectionError::BudgetTooSmall { .. }));
    }

    #[test]
    fn spoken_projection_uses_only_complete_authored_or_structured_scalars() {
        let raw = json!({"voice_summary": "A complete answer", "payload": "x".repeat(500)});
        let contract = ProjectionContractId::new(SCALAR_OR_OBJECT_CONTRACT_V1).unwrap();
        let result = project_with(&raw, 2_000, Some(&contract), "voice").unwrap();
        assert_eq!(
            result.spoken,
            Some(SpokenResultProjection {
                text: "A complete answer".to_owned(),
                source: SpokenProjectionSource::StructuredField,
                complete_scalar: true,
            })
        );

        let long_hint = "long ".repeat(200);
        let descriptor = raw_descriptor();
        let fallback =
            ToolResultProjector::new(ProjectionContractRegistry::default(), ByteEstimator)
                .project(ToolResultProjectionRequest {
                    identity: identity("voice"),
                    outcome: ToolOutcome::succeeded(),
                    raw_result: &json!({"payload": true}),
                    display: DisplayResultProjection::referenced(&descriptor),
                    raw: descriptor,
                    spoken_hint: Some(&long_hint),
                    contract_id: None,
                    budget: budget(2_000),
                })
                .unwrap();
        assert_eq!(
            fallback.spoken.unwrap().source,
            SpokenProjectionSource::OutcomeFallback
        );

        let generic = project_with(
            &json!({"voice_summary": "domain text, not declared narration"}),
            2_000,
            None,
            "generic-voice",
        )
        .unwrap();
        assert_eq!(
            generic.spoken.unwrap().source,
            SpokenProjectionSource::OutcomeFallback
        );
    }

    #[test]
    fn display_and_raw_descriptors_are_independent_from_model_budget() {
        let raw = json!({"small": true});
        let descriptor = raw_descriptor();
        let display = DisplayResultProjection::InlineAndReferenced {
            value: json!({"complete": "display payload"}),
            content_ref: descriptor.content_ref.clone(),
            content_hash: descriptor.content_hash.clone(),
            media_type: descriptor.media_type.clone(),
            size_bytes: descriptor.size_bytes,
        };
        let result = ToolResultProjector::new(ProjectionContractRegistry::default(), ByteEstimator)
            .project(ToolResultProjectionRequest {
                identity: identity("display"),
                outcome: ToolOutcome::succeeded(),
                raw_result: &raw,
                raw: descriptor.clone(),
                display: display.clone(),
                spoken_hint: None,
                contract_id: None,
                budget: budget(1_000),
            })
            .unwrap();
        assert_eq!(result.display, display);
        assert_eq!(result.raw, descriptor);
    }

    #[test]
    fn projection_rejects_a_false_full_result_reference() {
        let raw = json!({"value": "complete"});
        let mut descriptor = raw_descriptor();
        descriptor.content_ref.result_ref.clear();
        let error = ToolResultProjector::new(ProjectionContractRegistry::default(), ByteEstimator)
            .project(ToolResultProjectionRequest {
                identity: identity("display"),
                outcome: ToolOutcome::succeeded(),
                raw_result: &raw,
                display: DisplayResultProjection::referenced(&descriptor),
                raw: descriptor,
                spoken_hint: None,
                contract_id: None,
                budget: budget(1_000),
            })
            .unwrap_err();
        assert!(matches!(error, ProjectionError::InvalidRawDescriptor(_)));
    }

    #[test]
    fn outbound_provider_guard_redacts_credentials_without_mutating_projection_or_private_facts() {
        let raw = json!({ "ordinary": true });
        let mut projection = project_with(&raw, 2_000, None, "guard").unwrap();
        projection.model.value = json!({
            "authorization": "Bearer provider-secret",
            "nested": {
                "access_token": "provider-token",
                "spouse_birthday": "May 8"
            }
        });
        let stored = projection.model.value.clone();

        let guarded = provider_safe_model_value(&projection);

        assert_eq!(guarded["authorization"], "Bearer [REDACTED]");
        assert_eq!(guarded["nested"]["access_token"], "[REDACTED]");
        assert_eq!(guarded["nested"]["spouse_birthday"], "May 8");
        assert_eq!(projection.model.value, stored);
    }

    #[test]
    fn canonical_projection_is_byte_stable_across_object_insertion_order() {
        let left: Value = serde_json::from_str(r#"{"z":1,"a":{"y":2,"b":3}}"#).unwrap();
        let right: Value = serde_json::from_str(r#"{"a":{"b":3,"y":2},"z":1}"#).unwrap();
        let first = project_with(&left, 2_000, None, "stable").unwrap();
        let second = project_with(&right, 2_000, None, "stable").unwrap();
        assert_eq!(
            canonical_json_bytes(&first.model.value).unwrap(),
            canonical_json_bytes(&second.model.value).unwrap()
        );
    }

    #[test]
    fn unicode_emoji_combining_marks_and_rtl_survive_as_complete_scalars() {
        let raw = json!({
            "emoji": "👩🏽‍💻",
            "combining": "e\u{301}",
            "rtl": "مرحبا بالعالم"
        });
        let result = project_with(&raw, 2_000, None, "unicode").unwrap();
        assert_eq!(result.model.value["data"], canonicalize_json(&raw));
    }

    #[test]
    fn exact_status_survives_when_payload_is_omitted() {
        let raw = json!({"diagnostic": "x".repeat(8_000)});
        let outcome = ToolOutcome {
            status: ToolOutcomeStatus::RequiresApproval,
            code: Some("approval_required".to_owned()),
            message: None,
            retryable: false,
        };
        let descriptor = raw_descriptor();
        let mut projection_budget = budget(800);
        projection_budget.max_scalar_bytes = 40;
        let result = ToolResultProjector::new(ProjectionContractRegistry::default(), ByteEstimator)
            .project(ToolResultProjectionRequest {
                identity: identity("approval"),
                outcome: outcome.clone(),
                raw_result: &raw,
                display: DisplayResultProjection::referenced(&descriptor),
                raw: descriptor,
                spoken_hint: None,
                contract_id: None,
                budget: projection_budget,
            })
            .unwrap();
        assert_eq!(result.outcome, outcome);
        assert_eq!(result.model.value["outcome"]["status"], "requires_approval");
        assert_eq!(result.model.value["outcome"]["code"], "approval_required");
    }

    #[test]
    fn long_outcome_diagnostic_is_typed_omission_not_a_prefix() {
        let raw = json!({"status": "failed"});
        let outcome = ToolOutcome {
            status: ToolOutcomeStatus::Failed,
            code: Some("remote_failure".to_owned()),
            message: Some("diagnostic sentence. ".repeat(300)),
            retryable: true,
        };
        let descriptor = raw_descriptor();
        let mut projection_budget = budget(1_000);
        projection_budget.max_scalar_bytes = 80;
        let result = ToolResultProjector::new(ProjectionContractRegistry::default(), ByteEstimator)
            .project(ToolResultProjectionRequest {
                identity: identity("failure"),
                outcome,
                raw_result: &raw,
                display: DisplayResultProjection::referenced(&descriptor),
                raw: descriptor,
                spoken_hint: None,
                contract_id: None,
                budget: projection_budget,
            })
            .unwrap();
        assert!(result.model.value["outcome"].get("message").is_none());
        assert!(metadata(&result).omissions.iter().any(|item| {
            item.path.as_str() == "/outcome/message"
                && item.kind == ProjectionOmissionKind::OversizedScalar
        }));
        assert!(!result.model.value.to_string().contains("content_hash"));
    }

    #[test]
    fn error_contract_omits_oversized_diagnostic_without_excerpting_noise() {
        let raw = json!({
            "status": "error",
            "code": "upstream_timeout",
            "message": "diagnostic ".repeat(3_000),
        });
        let contract = ProjectionContractId::new(ERROR_CONTRACT_V1).unwrap();
        let descriptor = raw_descriptor();
        let mut projection_budget = budget(12_000);
        projection_budget.max_scalar_bytes = 1_000;
        let result = ToolResultProjector::new(ProjectionContractRegistry::default(), ByteEstimator)
            .project(ToolResultProjectionRequest {
                identity: identity("api"),
                outcome: ToolOutcome::with_status(ToolOutcomeStatus::Failed),
                raw_result: &raw,
                display: DisplayResultProjection::referenced(&descriptor),
                raw: descriptor,
                spoken_hint: None,
                contract_id: Some(&contract),
                budget: projection_budget,
            })
            .unwrap();
        assert_eq!(result.model.value["data"]["code"], "upstream_timeout");
        assert_eq!(result.model.value["outcome"]["status"], "failed");
        assert!(metadata(&result).excerpts.is_empty());
        assert!(metadata(&result).full_result_ref.is_some());
        assert!(!result.model.value.to_string().contains("diagnostic"));
    }

    #[test]
    fn generic_continuation_projection_retains_cursor_and_complete_entries() {
        let raw = json!({
            "status": "ok",
            "page": {
                "schema_version": 1,
                "reconstruction_version": 1,
                "content_ref": {"result_ref": "result_ref_opaque"},
                "selection_paths": ["/rows"],
                "entries": (0..20).map(|rank| json!({
                    "reconstruction_path": format!("/rows/{rank}"),
                    "kind": "complete_value",
                    "value": {"rank": rank, "sku": format!("SKU-{rank}"), "padding": "x".repeat(160)}
                })).collect::<Vec<_>>(),
                "page_start": 20,
                "total_records": 100,
                "total_entries": 100,
                "next_cursor": "opaque-next-page"
            },
            "lossless_reconstruction": true,
            "complete_records_only": true
        });
        let result = project_with(&raw, 2_400, None, "read_result").unwrap();
        assert_eq!(
            result.model.value["data"]["page"]["next_cursor"],
            "opaque-next-page"
        );
        let entries = result.model.value["data"]["page"]["entries"]
            .as_array()
            .expect("at least one complete continuation entry");
        assert!(!entries.is_empty());
        assert!(entries.iter().all(Value::is_object));
    }

    #[test]
    fn arbitrary_json_trees_remain_parseable_deterministic_and_bounded() {
        for seed in 0..128_u64 {
            let mut rng = StdRng::seed_from_u64(seed);
            let raw = random_json(&mut rng, 0, 5);
            let first = project_with(&raw, 2_400, None, "property-a").unwrap();
            let second = project_with(&raw, 2_400, None, "property-b").unwrap();
            let first_bytes = canonical_json_bytes(&first.model.value).unwrap();
            let second_bytes = canonical_json_bytes(&second.model.value).unwrap();
            assert_eq!(first_bytes, second_bytes, "seed {seed}");
            assert!(first_bytes.len() <= 2_400, "seed {seed}");
            assert!(
                serde_json::from_slice::<Value>(&first_bytes).is_ok(),
                "seed {seed}"
            );
            if first.model.strategy == ProjectionStrategy::Complete {
                assert!(first.model.value.get("projection").is_none(), "seed {seed}");
            } else {
                assert_eq!(metadata(&first).complete_units_only, true, "seed {seed}");
            }
        }
    }

    fn random_json(rng: &mut StdRng, depth: usize, max_depth: usize) -> Value {
        if depth >= max_depth {
            return random_scalar(rng);
        }
        match rng.gen_range(0..6) {
            0..=2 => random_scalar(rng),
            3 => Value::Array(
                (0..rng.gen_range(0..6))
                    .map(|_| random_json(rng, depth + 1, max_depth))
                    .collect(),
            ),
            _ => {
                let mut object = Map::new();
                for index in 0..rng.gen_range(0..6) {
                    object.insert(
                        format!("field_{index}_{}", rng.gen_range(0..8)),
                        random_json(rng, depth + 1, max_depth),
                    );
                }
                Value::Object(object)
            },
        }
    }

    fn random_scalar(rng: &mut StdRng) -> Value {
        match rng.gen_range(0..5) {
            0 => Value::Null,
            1 => Value::Bool(rng.gen()),
            2 => json!(rng.gen_range(-10_000_i64..10_000_i64)),
            3 => json!(format!("value-{}-👩🏽‍💻", rng.gen::<u64>())),
            _ => json!("paragraph sentence. ".repeat(rng.gen_range(1..80))),
        }
    }
}

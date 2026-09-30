//! Bounded workflow values and the versioned recipe IR foundation.
//!
//! This module deliberately stops before lowering or execution. Source
//! declarations are hostile-input DTOs; compiled schemas, validated values and
//! compiled recipes are minted only after exact, stack-safe validation and are
//! not deserializable. Lowering is owned by the crate-private recipe lifecycle;
//! nothing in this module grants authority or makes a recipe ready.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::{self, Write},
};

use chrono::{SecondsFormat, Utc};
use serde::{de, Deserialize, Deserializer, Serialize};
use thiserror::Error;

use super::{
    models::{
        AppContractError, AppDataClassification, AppDigest, AppFieldPath, AppHandlingLabels,
        AppModelProcessing, AppName, AppReference, AppRevision, AppSourceRefKind,
    },
    query_semantics::AppQueryScalarKind,
    value_mapping::{
        apply_compiled_value_mapping, compile_value_mapping, AppCompiledValueMapping,
        AppRegisteredScalarConversion, AppValueFieldContract, AppValueMappingError,
        AppValueMappingOperation, AppValueSchemaContract,
    },
};

pub const APP_WORKFLOW_VALUE_SCHEMA_VERSION: &str = "magician.app-workflow-value-schema.v1";
pub const APP_WORKFLOW_VALUE_VERSION: &str = "magician.app-workflow-value.v1";
pub const APP_RECIPE_IR_VERSION: &str = "magician.app-recipe-ir.v1";
pub const APP_RECIPE_BUNDLE_VERSION: &str = "magician.app-recipe-bundle.v1";

const MAX_SCHEMA_NODES: usize = 256;
const MAX_VALUE_NODES: usize = 8_000;
const MAX_RECIPE_NODES: usize = 256;
const MAX_SCHEMA_DEPTH: usize = 32;
const MAX_VALUE_DEPTH: usize = 32;
const MAX_RECIPE_DEPTH: usize = 32;
const MAX_SCHEMA_EDGES: usize = 1_024;
const MAX_VALUE_EDGES: usize = 16_000;
const MAX_RECIPE_EDGES: usize = 1_024;
const MAX_RECORD_FIELDS: usize = 64;
const MAX_UNION_VARIANTS: usize = 32;
const MAX_ARRAY_ITEMS: u16 = 256;
const MAX_TEXT_BYTES: u32 = 262_144;
const MAX_SCHEMA_CANONICAL_BYTES: usize = 128 * 1024;
const MAX_VALUE_CANONICAL_BYTES: usize = 512 * 1024;
const MAX_RECIPE_CANONICAL_BYTES: usize = 256 * 1024;
const MAX_PROVENANCE_REFS: usize = 256;
const MAX_RESOURCE_REFS: usize = 256;
const MAX_MEDIA_TYPES: usize = 16;
const MAX_MEDIA_TYPE_BYTES: usize = 128;
const MAX_AUTHORITY_REFS: usize = 64;
const MAX_RECIPE_FAN_OUT: usize = 32;
const MAX_RETRY_ATTEMPTS: u16 = 8;
const MAX_NODE_ACTIVE_MILLIS: u64 = 24 * 60 * 60 * 1_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppWorkflowValueSchemaVersion {
    V1,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppRecipeVersion {
    V1,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppRecipeBundleVersion {
    V1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppWorkflowHandlingFloor {
    pub classification: AppDataClassification,
    pub model_processing: AppModelProcessing,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppWorkflowValueSchemaSource {
    pub version: AppWorkflowValueSchemaVersion,
    pub root: u16,
    pub handling_floor: AppWorkflowHandlingFloor,
    pub nodes: Vec<AppWorkflowValueTypeNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppWorkflowRecordField {
    pub value_type: u16,
    /// Absence is permitted only when this is false. A present JSON-like null
    /// remains illegal unless the referenced type is explicitly `nullable`.
    pub required: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppWorkflowReceiptKind {
    Mutation,
    ExternalEffect,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppWorkflowValueTypeNode {
    Unit,
    Boolean,
    Integer,
    Decimal,
    Text {
        max_bytes: u32,
    },
    Markdown {
        max_bytes: u32,
    },
    Enum {
        values: BTreeSet<AppName>,
    },
    Timestamp,
    EntityReference {
        entity: AppName,
    },
    OpaqueReference,
    /// A logical, non-bearer resource handle. The kind is package-declared;
    /// the referenced descriptor remains in the protected resource sidecar.
    ResourceRef {
        resource_kind: AppName,
    },
    EntityProjectionRef {
        entity: AppName,
        value_schema_ref: AppReference,
    },
    ArtifactRef {
        value_schema_ref: AppReference,
        max_bytes: u64,
        media_types: BTreeSet<String>,
    },
    ReceiptRef {
        receipt_kind: AppWorkflowReceiptKind,
    },
    Nullable {
        value_type: u16,
    },
    Array {
        items: u16,
        min_items: u16,
        max_items: u16,
    },
    Record {
        fields: BTreeMap<AppName, AppWorkflowRecordField>,
    },
    /// The discriminator is a literal field name. Variant names are its exact
    /// closed value set; there is no default or expression evaluator.
    TaggedUnion {
        discriminator: AppName,
        variants: BTreeMap<AppName, u16>,
    },
}

/// Content-addressed schema produced by [`compile_workflow_value_schema`].
/// Its normalized source is retained for exact validation and recovery.
#[derive(Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppCompiledWorkflowValueSchema {
    schema_version: &'static str,
    schema_ref: AppReference,
    content_digest: AppDigest,
    canonical_encoded_len: u64,
    source: AppWorkflowValueSchemaSource,
}

impl fmt::Debug for AppCompiledWorkflowValueSchema {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppCompiledWorkflowValueSchema")
            .field("schema_ref", &self.schema_ref)
            .field("content_digest", &self.content_digest)
            .field("canonical_encoded_len", &self.canonical_encoded_len)
            .field("node_count", &self.source.nodes.len())
            .finish()
    }
}

impl AppCompiledWorkflowValueSchema {
    pub fn schema_ref(&self) -> &AppReference {
        &self.schema_ref
    }

    pub fn content_digest(&self) -> &AppDigest {
        &self.content_digest
    }

    pub fn canonical_encoded_len(&self) -> u64 {
        self.canonical_encoded_len
    }

    pub fn source(&self) -> &AppWorkflowValueSchemaSource {
        &self.source
    }

    /// Stream the exact v1 canonical source bytes used for identity. The
    /// compiled wrapper fields are intentionally excluded from that identity.
    pub fn write_canonical<W: Write>(&self, writer: W) -> Result<(), AppRecipeIrError> {
        write_canonical_stream(writer, &self.source)
    }

    pub(crate) fn root_node(&self) -> &AppWorkflowValueTypeNode {
        &self.source.nodes[usize::from(self.source.root)]
    }
}

/// Compile and normalize a source schema. Arena declaration order is not part
/// of identity: nodes are reordered by deterministic semantic traversal from
/// the root before the streaming canonical digest is computed.
pub fn compile_workflow_value_schema(
    source: AppWorkflowValueSchemaSource,
) -> Result<AppCompiledWorkflowValueSchema, AppRecipeIrError> {
    if source.version != AppWorkflowValueSchemaVersion::V1 {
        return Err(AppRecipeIrError::UnsupportedSchemaVersion);
    }
    validate_schema_nodes(&source)?;
    let source = normalize_schema_arena(source)?;
    let (content_digest, canonical_encoded_len) =
        stream_canonical_identity("workflow value schema", &source, MAX_SCHEMA_CANONICAL_BYTES)?;
    let schema_ref = AppReference::parse(format!("workflow-schema:{}", content_digest.as_str()))?;
    Ok(AppCompiledWorkflowValueSchema {
        schema_version: APP_WORKFLOW_VALUE_SCHEMA_VERSION,
        schema_ref,
        content_digest,
        canonical_encoded_len: canonical_encoded_len as u64,
        source,
    })
}

#[derive(Clone, Serialize, PartialEq, Eq)]
#[serde(try_from = "String", into = "String")]
pub struct AppWorkflowDecimal(String);

impl fmt::Debug for AppWorkflowDecimal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AppWorkflowDecimal(..)")
    }
}

impl AppWorkflowDecimal {
    pub fn parse(raw: impl AsRef<str>) -> Result<Self, AppRecipeIrError> {
        normalize_decimal(raw.as_ref()).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AppWorkflowDecimal {
    type Error = AppRecipeIrError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<AppWorkflowDecimal> for String {
    fn from(value: AppWorkflowDecimal) -> Self {
        value.0
    }
}

impl<'de> Deserialize<'de> for AppWorkflowDecimal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[derive(Clone, Serialize, PartialEq, Eq)]
#[serde(try_from = "String", into = "String")]
pub struct AppWorkflowTimestamp(String);

impl fmt::Debug for AppWorkflowTimestamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AppWorkflowTimestamp(..)")
    }
}

impl AppWorkflowTimestamp {
    pub fn parse(raw: impl AsRef<str>) -> Result<Self, AppRecipeIrError> {
        if raw.as_ref().len() > 64 {
            return Err(AppRecipeIrError::InvalidTimestamp);
        }
        let parsed = chrono::DateTime::parse_from_rfc3339(raw.as_ref())
            .map_err(|_| AppRecipeIrError::InvalidTimestamp)?;
        Ok(Self(
            parsed
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::AutoSi, true),
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AppWorkflowTimestamp {
    type Error = AppRecipeIrError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<AppWorkflowTimestamp> for String {
    fn from(value: AppWorkflowTimestamp) -> Self {
        value.0
    }
}

impl<'de> Deserialize<'de> for AppWorkflowTimestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppWorkflowValueNode {
    Unit,
    Null,
    Boolean { value: bool },
    Integer { value: i64 },
    Decimal { value: AppWorkflowDecimal },
    Text { value: String },
    Enum { value: AppName },
    Timestamp { value: AppWorkflowTimestamp },
    OpaqueReference { value: AppReference },
    Resource { reference: AppReference },
    Array { items: Vec<u16> },
    Record { fields: BTreeMap<AppName, u16> },
    TaggedUnion { tag: AppName, value: u16 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppWorkflowProvenanceEntry {
    pub kind: AppSourceRefKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<AppRevision>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub fields: BTreeSet<AppFieldPath>,
    pub handling_labels: AppHandlingLabels,
    pub content_digest: AppDigest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppWorkflowResourceRef {
    EntityProjection {
        entity: AppName,
        revision: AppRevision,
        value_schema_ref: AppReference,
        content_digest: AppDigest,
        handling_labels: AppHandlingLabels,
    },
    Artifact {
        revision: AppRevision,
        value_schema_ref: AppReference,
        media_type: String,
        byte_len: u64,
        content_digest: AppDigest,
        handling_labels: AppHandlingLabels,
    },
    Receipt {
        receipt_kind: AppWorkflowReceiptKind,
        content_digest: AppDigest,
        handling_labels: AppHandlingLabels,
    },
    Resource {
        resource_kind: AppName,
        content_digest: AppDigest,
        handling_labels: AppHandlingLabels,
    },
}

impl AppWorkflowResourceRef {
    fn handling_labels(&self) -> &AppHandlingLabels {
        match self {
            Self::EntityProjection {
                handling_labels, ..
            }
            | Self::Artifact {
                handling_labels, ..
            }
            | Self::Receipt {
                handling_labels, ..
            }
            | Self::Resource {
                handling_labels, ..
            } => handling_labels,
        }
    }
}

/// Untrusted, flat-arena workflow value. It intentionally has no `Debug`
/// implementation so logs cannot accidentally render text payloads.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppWorkflowValueSource {
    pub version: AppWorkflowValueSchemaVersion,
    pub schema_ref: AppReference,
    pub root: u16,
    pub nodes: Vec<AppWorkflowValueNode>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provenance: BTreeMap<AppReference, AppWorkflowProvenanceEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resources: BTreeMap<AppReference, AppWorkflowResourceRef>,
    pub handling_labels: AppHandlingLabels,
}

/// Validated, canonical value. This is data, not authority, but remains
/// non-deserializable so callers cannot claim validation by wire construction.
#[derive(Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppValidatedWorkflowValue {
    value_version: &'static str,
    value_digest: AppDigest,
    canonical_encoded_len: u64,
    source: AppWorkflowValueSource,
}

impl fmt::Debug for AppValidatedWorkflowValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppValidatedWorkflowValue")
            .field("schema_ref", &self.source.schema_ref)
            .field("value_digest", &self.value_digest)
            .field("canonical_encoded_len", &self.canonical_encoded_len)
            .field("node_count", &self.source.nodes.len())
            .field("provenance_count", &self.source.provenance.len())
            .field("resource_count", &self.source.resources.len())
            .finish()
    }
}

impl AppValidatedWorkflowValue {
    pub fn schema_ref(&self) -> &AppReference {
        &self.source.schema_ref
    }

    pub fn value_digest(&self) -> &AppDigest {
        &self.value_digest
    }

    pub fn canonical_encoded_len(&self) -> u64 {
        self.canonical_encoded_len
    }

    pub fn handling_labels(&self) -> &AppHandlingLabels {
        &self.source.handling_labels
    }

    pub fn source(&self) -> &AppWorkflowValueSource {
        &self.source
    }

    pub fn write_canonical<W: Write>(&self, writer: W) -> Result<(), AppRecipeIrError> {
        write_canonical_stream(writer, &self.source)
    }

    /// Move the already-validated flat arena into a bounded streaming
    /// persistence owner. This stays crate-private so no external caller can
    /// separate the source DTO from the validation proof and reinterpret it.
    pub(crate) fn into_source(self) -> AppWorkflowValueSource {
        self.source
    }
}

pub fn validate_workflow_value(
    schema: &AppCompiledWorkflowValueSchema,
    source: AppWorkflowValueSource,
) -> Result<AppValidatedWorkflowValue, AppRecipeIrError> {
    if source.version != AppWorkflowValueSchemaVersion::V1 {
        return Err(AppRecipeIrError::UnsupportedValueVersion);
    }
    if source.schema_ref != schema.schema_ref {
        return Err(AppRecipeIrError::SchemaSubstitution);
    }
    if source.provenance.len() > MAX_PROVENANCE_REFS {
        return Err(AppRecipeIrError::ProvenanceLimit);
    }
    if source.resources.len() > MAX_RESOURCE_REFS {
        return Err(AppRecipeIrError::ResourceRefLimit);
    }
    validate_label_join(
        &source.handling_labels,
        &schema.source.handling_floor,
        source
            .provenance
            .values()
            .map(|entry| &entry.handling_labels)
            .chain(
                source
                    .resources
                    .values()
                    .map(AppWorkflowResourceRef::handling_labels),
            ),
    )?;
    validate_provenance_refs(&source.provenance)?;
    validate_resource_refs(&source.resources)?;
    validate_value_nodes(&source, schema)?;
    let source = normalize_value_arena(source)?;
    let (value_digest, canonical_encoded_len) =
        stream_canonical_identity("workflow value", &source, MAX_VALUE_CANONICAL_BYTES)?;
    Ok(AppValidatedWorkflowValue {
        value_version: APP_WORKFLOW_VALUE_VERSION,
        value_digest,
        canonical_encoded_len: canonical_encoded_len as u64,
        source,
    })
}

/// Convert one bounded JSON carrier into the closed workflow-value algebra.
/// The walk is iterative: hostile nesting can fail at the schema ceiling but
/// can never consume the Rust call stack. Resource-bearing schema nodes are
/// server-owned and therefore cannot be minted from caller JSON.
pub(crate) fn validate_json_workflow_value(
    schema: &AppCompiledWorkflowValueSchema,
    value: serde_json::Value,
    handling_labels: AppHandlingLabels,
    provenance: BTreeMap<AppReference, AppWorkflowProvenanceEntry>,
) -> Result<AppValidatedWorkflowValue, AppRecipeIrError> {
    enum Work {
        Visit(serde_json::Value, u16, usize),
        Array(usize),
        Record(Vec<AppName>),
        Tagged(AppName),
    }

    let mut work = vec![Work::Visit(value, schema.source.root, 0)];
    let mut nodes = Vec::<AppWorkflowValueNode>::new();
    let mut completed = Vec::<u16>::new();
    macro_rules! finish {
        ($node:expr) => {{
            let index =
                u16::try_from(nodes.len()).map_err(|_| AppRecipeIrError::ArenaNodeLimit {
                    domain: "workflow value",
                    limit: MAX_VALUE_NODES,
                })?;
            nodes.push($node);
            completed.push(index);
        }};
    }
    while let Some(item) = work.pop() {
        match item {
            Work::Visit(value, type_index, depth) => {
                if depth > MAX_VALUE_DEPTH {
                    return Err(AppRecipeIrError::ArenaDepth {
                        domain: "workflow value",
                        limit: MAX_VALUE_DEPTH,
                    });
                }
                let value_type = schema
                    .source
                    .nodes
                    .get(usize::from(type_index))
                    .ok_or(AppRecipeIrError::InvalidArenaReference)?;
                match value_type {
                    AppWorkflowValueTypeNode::Nullable { value_type } if value.is_null() => {
                        finish!(AppWorkflowValueNode::Null);
                    },
                    AppWorkflowValueTypeNode::Nullable { value_type } => {
                        work.push(Work::Visit(value, *value_type, depth.saturating_add(1)));
                    },
                    AppWorkflowValueTypeNode::Unit if value.is_null() => {
                        finish!(AppWorkflowValueNode::Unit);
                    },
                    AppWorkflowValueTypeNode::Boolean => finish!(AppWorkflowValueNode::Boolean {
                        value: value.as_bool().ok_or(AppRecipeIrError::ValueTypeMismatch)?,
                    }),
                    AppWorkflowValueTypeNode::Integer => finish!(AppWorkflowValueNode::Integer {
                        value: value.as_i64().ok_or(AppRecipeIrError::ValueTypeMismatch)?,
                    }),
                    AppWorkflowValueTypeNode::Decimal => {
                        let raw = match value {
                            serde_json::Value::Number(number) => number.to_string(),
                            serde_json::Value::String(value) => value,
                            _ => return Err(AppRecipeIrError::ValueTypeMismatch),
                        };
                        finish!(AppWorkflowValueNode::Decimal {
                            value: AppWorkflowDecimal::parse(raw)?,
                        });
                    },
                    AppWorkflowValueTypeNode::Text { max_bytes }
                    | AppWorkflowValueTypeNode::Markdown { max_bytes } => {
                        let value = value
                            .as_str()
                            .ok_or(AppRecipeIrError::ValueTypeMismatch)?
                            .to_owned();
                        if value.len() > *max_bytes as usize {
                            return Err(AppRecipeIrError::ValueTypeMismatch);
                        }
                        finish!(AppWorkflowValueNode::Text { value });
                    },
                    AppWorkflowValueTypeNode::EntityReference { .. } => {
                        let value = value
                            .as_str()
                            .ok_or(AppRecipeIrError::ValueTypeMismatch)?
                            .to_owned();
                        if value.is_empty() || value.len() > 256 {
                            return Err(AppRecipeIrError::ValueTypeMismatch);
                        }
                        finish!(AppWorkflowValueNode::Text { value });
                    },
                    AppWorkflowValueTypeNode::Enum { values } => {
                        let value = AppName::parse(
                            value
                                .as_str()
                                .ok_or(AppRecipeIrError::ValueTypeMismatch)?
                                .to_owned(),
                        )?;
                        if !values.contains(&value) {
                            return Err(AppRecipeIrError::ValueTypeMismatch);
                        }
                        finish!(AppWorkflowValueNode::Enum { value });
                    },
                    AppWorkflowValueTypeNode::Timestamp => {
                        let value = value.as_str().ok_or(AppRecipeIrError::ValueTypeMismatch)?;
                        finish!(AppWorkflowValueNode::Timestamp {
                            value: AppWorkflowTimestamp::parse(value)?,
                        });
                    },
                    AppWorkflowValueTypeNode::OpaqueReference => {
                        let value = AppReference::parse(
                            value
                                .as_str()
                                .ok_or(AppRecipeIrError::ValueTypeMismatch)?
                                .to_owned(),
                        )?;
                        if !is_opaque_logical_ref(&value, "ref:") {
                            return Err(AppRecipeIrError::NonLogicalResourceRef(value.to_string()));
                        }
                        finish!(AppWorkflowValueNode::OpaqueReference { value });
                    },
                    AppWorkflowValueTypeNode::Array {
                        items,
                        min_items,
                        max_items,
                    } => {
                        let values = match value {
                            serde_json::Value::Array(values) => values,
                            _ => return Err(AppRecipeIrError::ValueTypeMismatch),
                        };
                        if values.len() < usize::from(*min_items)
                            || values.len() > usize::from(*max_items)
                        {
                            return Err(AppRecipeIrError::ValueTypeMismatch);
                        }
                        work.push(Work::Array(values.len()));
                        for value in values.into_iter().rev() {
                            work.push(Work::Visit(value, *items, depth.saturating_add(1)));
                        }
                    },
                    AppWorkflowValueTypeNode::Record { fields } => {
                        let mut object = match value {
                            serde_json::Value::Object(object) => object,
                            _ => return Err(AppRecipeIrError::ValueTypeMismatch),
                        };
                        if object.keys().any(|key| {
                            AppName::parse(key.clone())
                                .ok()
                                .is_none_or(|name| !fields.contains_key(&name))
                        }) {
                            return Err(AppRecipeIrError::ValueTypeMismatch);
                        }
                        let mut present = Vec::new();
                        for (name, field) in fields {
                            match object.remove(name.as_str()) {
                                Some(value) => {
                                    present.push((name.clone(), field.value_type, value))
                                },
                                None if field.required => {
                                    return Err(AppRecipeIrError::ValueTypeMismatch);
                                },
                                None => {},
                            }
                        }
                        let names = present
                            .iter()
                            .map(|(name, _, _)| name.clone())
                            .collect::<Vec<_>>();
                        work.push(Work::Record(names));
                        for (_, value_type, value) in present.into_iter().rev() {
                            work.push(Work::Visit(value, value_type, depth.saturating_add(1)));
                        }
                    },
                    AppWorkflowValueTypeNode::TaggedUnion {
                        discriminator,
                        variants,
                    } => {
                        let mut object = match value {
                            serde_json::Value::Object(object) => object,
                            _ => return Err(AppRecipeIrError::ValueTypeMismatch),
                        };
                        if object.len() != 2 || discriminator.as_str() == "value" {
                            return Err(AppRecipeIrError::ValueTypeMismatch);
                        }
                        let tag = AppName::parse(
                            object
                                .remove(discriminator.as_str())
                                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                                .ok_or(AppRecipeIrError::ValueTypeMismatch)?,
                        )?;
                        let value_type = variants
                            .get(&tag)
                            .copied()
                            .ok_or(AppRecipeIrError::ValueTypeMismatch)?;
                        let payload = object
                            .remove("value")
                            .ok_or(AppRecipeIrError::ValueTypeMismatch)?;
                        work.push(Work::Tagged(tag));
                        work.push(Work::Visit(payload, value_type, depth.saturating_add(1)));
                    },
                    AppWorkflowValueTypeNode::EntityProjectionRef { .. }
                    | AppWorkflowValueTypeNode::ArtifactRef { .. }
                    | AppWorkflowValueTypeNode::ReceiptRef { .. }
                    | AppWorkflowValueTypeNode::ResourceRef { .. }
                    | AppWorkflowValueTypeNode::Unit => {
                        return Err(AppRecipeIrError::ValueTypeMismatch);
                    },
                }
            },
            Work::Array(length) => {
                if length > completed.len() {
                    return Err(AppRecipeIrError::ValueTypeMismatch);
                }
                let start = completed.len() - length;
                let children = completed.drain(start..).collect::<Vec<_>>();
                finish!(AppWorkflowValueNode::Array { items: children });
            },
            Work::Record(names) => {
                if names.len() > completed.len() {
                    return Err(AppRecipeIrError::ValueTypeMismatch);
                }
                let start = completed.len() - names.len();
                let children = completed.drain(start..).collect::<Vec<_>>();
                let mut fields = BTreeMap::new();
                for (name, index) in names.into_iter().zip(children) {
                    fields.insert(name, index);
                }
                finish!(AppWorkflowValueNode::Record { fields });
            },
            Work::Tagged(tag) => {
                let value = completed.pop().ok_or(AppRecipeIrError::ValueTypeMismatch)?;
                finish!(AppWorkflowValueNode::TaggedUnion { tag, value });
            },
        }
        if nodes.len() > MAX_VALUE_NODES {
            return Err(AppRecipeIrError::ArenaNodeLimit {
                domain: "workflow value",
                limit: MAX_VALUE_NODES,
            });
        }
    }
    if completed.len() != 1 {
        return Err(AppRecipeIrError::ValueTypeMismatch);
    }
    let root = completed.pop().ok_or(AppRecipeIrError::ValueTypeMismatch)?;
    validate_workflow_value(
        schema,
        AppWorkflowValueSource {
            version: AppWorkflowValueSchemaVersion::V1,
            schema_ref: schema.schema_ref().clone(),
            root,
            nodes,
            provenance,
            resources: BTreeMap::new(),
            handling_labels,
        },
    )
}

/// Validate an ordinary JSON carrier against the canonical workflow schema
/// without accepting caller-provided policy or provenance claims. This is the
/// manifest/action/bridge shape gate; resource-bearing roots remain
/// server-owned and therefore fail here.
pub(crate) fn validate_json_workflow_value_shape(
    schema: &AppCompiledWorkflowValueSchema,
    value: &serde_json::Value,
) -> Result<(), AppRecipeIrError> {
    let labels = AppHandlingLabels {
        classification: schema.source.handling_floor.classification,
        model_processing: schema.source.handling_floor.model_processing,
        policy_digest: AppDigest::blake3(b"workflow-schema-shape-policy-v1"),
        provenance_digest: AppDigest::blake3(b"workflow-schema-shape-provenance-v1"),
    };
    validate_json_workflow_value(schema, value.clone(), labels, BTreeMap::new()).map(|_| ())
}

/// Project the validated flat algebra to its closed JSON carrier without
/// recursive descent. Logical resource references remain opaque; resource
/// descriptors and provenance evidence stay in the protected result envelope.
pub(crate) fn workflow_value_to_json(
    schema: &AppCompiledWorkflowValueSchema,
    value: &AppValidatedWorkflowValue,
) -> Result<serde_json::Value, AppRecipeIrError> {
    if value.schema_ref() != schema.schema_ref() {
        return Err(AppRecipeIrError::SchemaSubstitution);
    }
    enum Work {
        Visit(u16, u16, usize),
        Array(usize),
        Record(Vec<AppName>),
        Tagged(AppName, AppName),
    }
    let mut work = vec![Work::Visit(value.source.root, schema.source.root, 0)];
    let mut completed = Vec::<serde_json::Value>::new();
    while let Some(item) = work.pop() {
        match item {
            Work::Visit(value_index, type_index, depth) => {
                if depth > MAX_VALUE_DEPTH {
                    return Err(AppRecipeIrError::ArenaDepth {
                        domain: "workflow value",
                        limit: MAX_VALUE_DEPTH,
                    });
                }
                let node = value
                    .source
                    .nodes
                    .get(usize::from(value_index))
                    .ok_or(AppRecipeIrError::InvalidArenaReference)?;
                let value_type = schema
                    .source
                    .nodes
                    .get(usize::from(type_index))
                    .ok_or(AppRecipeIrError::InvalidArenaReference)?;
                match (node, value_type) {
                    (AppWorkflowValueNode::Unit, AppWorkflowValueTypeNode::Unit)
                    | (AppWorkflowValueNode::Null, AppWorkflowValueTypeNode::Nullable { .. }) => {
                        completed.push(serde_json::Value::Null);
                    },
                    (_, AppWorkflowValueTypeNode::Nullable { value_type }) => work.push(
                        Work::Visit(value_index, *value_type, depth.saturating_add(1)),
                    ),
                    (
                        AppWorkflowValueNode::Boolean { value },
                        AppWorkflowValueTypeNode::Boolean,
                    ) => {
                        completed.push(serde_json::Value::Bool(*value));
                    },
                    (
                        AppWorkflowValueNode::Integer { value },
                        AppWorkflowValueTypeNode::Integer,
                    ) => {
                        completed.push(serde_json::Value::Number((*value).into()));
                    },
                    (
                        AppWorkflowValueNode::Decimal { value },
                        AppWorkflowValueTypeNode::Decimal,
                    ) => {
                        let number = value
                            .as_str()
                            .parse::<serde_json::Number>()
                            .map_err(|_| AppRecipeIrError::InvalidDecimal)?;
                        completed.push(serde_json::Value::Number(number));
                    },
                    (
                        AppWorkflowValueNode::Text { value },
                        AppWorkflowValueTypeNode::Text { .. }
                        | AppWorkflowValueTypeNode::Markdown { .. }
                        | AppWorkflowValueTypeNode::EntityReference { .. },
                    ) => {
                        completed.push(serde_json::Value::String(value.clone()));
                    },
                    (
                        AppWorkflowValueNode::Enum { value },
                        AppWorkflowValueTypeNode::Enum { .. },
                    ) => completed.push(serde_json::Value::String(value.to_string())),
                    (
                        AppWorkflowValueNode::Timestamp { value },
                        AppWorkflowValueTypeNode::Timestamp,
                    ) => completed.push(serde_json::Value::String(value.as_str().to_owned())),
                    (
                        AppWorkflowValueNode::OpaqueReference { value },
                        AppWorkflowValueTypeNode::OpaqueReference,
                    ) => completed.push(serde_json::Value::String(value.to_string())),
                    (
                        AppWorkflowValueNode::Resource { reference },
                        AppWorkflowValueTypeNode::EntityProjectionRef { .. }
                        | AppWorkflowValueTypeNode::ArtifactRef { .. }
                        | AppWorkflowValueTypeNode::ReceiptRef { .. }
                        | AppWorkflowValueTypeNode::ResourceRef { .. },
                    ) => completed.push(serde_json::Value::String(reference.to_string())),
                    (
                        AppWorkflowValueNode::Array { items },
                        AppWorkflowValueTypeNode::Array {
                            items: item_type, ..
                        },
                    ) => {
                        work.push(Work::Array(items.len()));
                        for item in items.iter().rev() {
                            work.push(Work::Visit(*item, *item_type, depth.saturating_add(1)));
                        }
                    },
                    (
                        AppWorkflowValueNode::Record { fields },
                        AppWorkflowValueTypeNode::Record {
                            fields: type_fields,
                        },
                    ) => {
                        let names = fields.keys().cloned().collect::<Vec<_>>();
                        work.push(Work::Record(names.clone()));
                        for name in names.into_iter().rev() {
                            let child = fields
                                .get(&name)
                                .copied()
                                .ok_or(AppRecipeIrError::ValueTypeMismatch)?;
                            let child_type = type_fields
                                .get(&name)
                                .map(|field| field.value_type)
                                .ok_or(AppRecipeIrError::ValueTypeMismatch)?;
                            work.push(Work::Visit(child, child_type, depth.saturating_add(1)));
                        }
                    },
                    (
                        AppWorkflowValueNode::TaggedUnion { tag, value },
                        AppWorkflowValueTypeNode::TaggedUnion {
                            discriminator,
                            variants,
                        },
                    ) => {
                        let child_type = variants
                            .get(tag)
                            .copied()
                            .ok_or(AppRecipeIrError::ValueTypeMismatch)?;
                        work.push(Work::Tagged(discriminator.clone(), tag.clone()));
                        work.push(Work::Visit(*value, child_type, depth.saturating_add(1)));
                    },
                    _ => return Err(AppRecipeIrError::ValueTypeMismatch),
                }
            },
            Work::Array(length) => {
                if length > completed.len() {
                    return Err(AppRecipeIrError::ValueTypeMismatch);
                }
                let start = completed.len() - length;
                let values = completed.drain(start..).collect::<Vec<_>>();
                completed.push(serde_json::Value::Array(values));
            },
            Work::Record(names) => {
                if names.len() > completed.len() {
                    return Err(AppRecipeIrError::ValueTypeMismatch);
                }
                let start = completed.len() - names.len();
                let values = completed.drain(start..).collect::<Vec<_>>();
                let object = names
                    .into_iter()
                    .zip(values)
                    .map(|(name, value)| (name.as_str().to_owned(), value))
                    .collect::<serde_json::Map<_, _>>();
                completed.push(serde_json::Value::Object(object));
            },
            Work::Tagged(discriminator, tag) => {
                let payload = completed.pop().ok_or(AppRecipeIrError::ValueTypeMismatch)?;
                let mut object = serde_json::Map::new();
                object.insert(
                    discriminator.as_str().to_owned(),
                    serde_json::Value::String(tag.as_str().to_owned()),
                );
                object.insert("value".to_owned(), payload);
                completed.push(serde_json::Value::Object(object));
            },
        }
    }
    if completed.len() != 1 {
        return Err(AppRecipeIrError::ValueTypeMismatch);
    }
    completed.pop().ok_or(AppRecipeIrError::ValueTypeMismatch)
}

/// Mint a typed array of server-observed entity projections. Each array item
/// is an opaque logical reference whose protected descriptor binds the exact
/// entity, revision, schema, digest and labels; raw record ids never enter the
/// recipe value or its public JSON projection.
// Retained as the canonical constructor for the declared array-of-projections
// schema shape; current V1 action results use the scalar projection path.
#[allow(dead_code)]
pub(crate) fn validate_entity_projection_array(
    schema: &AppCompiledWorkflowValueSchema,
    projections: Vec<(AppReference, AppWorkflowResourceRef)>,
    handling_labels: AppHandlingLabels,
    provenance: BTreeMap<AppReference, AppWorkflowProvenanceEntry>,
) -> Result<AppValidatedWorkflowValue, AppRecipeIrError> {
    let AppWorkflowValueTypeNode::Array {
        items,
        min_items,
        max_items,
    } = schema.root_node()
    else {
        return Err(AppRecipeIrError::ValueTypeMismatch);
    };
    if projections.len() < usize::from(*min_items) || projections.len() > usize::from(*max_items) {
        return Err(AppRecipeIrError::ValueTypeMismatch);
    }
    if !matches!(
        schema.source.nodes.get(usize::from(*items)),
        Some(AppWorkflowValueTypeNode::EntityProjectionRef { .. })
    ) {
        return Err(AppRecipeIrError::ValueTypeMismatch);
    }
    let mut nodes = Vec::with_capacity(projections.len().saturating_add(1));
    let mut resources = BTreeMap::new();
    let mut item_refs = Vec::with_capacity(projections.len());
    for (reference, resource) in projections {
        let index = u16::try_from(nodes.len()).map_err(|_| AppRecipeIrError::ArenaNodeLimit {
            domain: "workflow value",
            limit: MAX_VALUE_NODES,
        })?;
        nodes.push(AppWorkflowValueNode::Resource {
            reference: reference.clone(),
        });
        item_refs.push(index);
        if resources.insert(reference, resource).is_some() {
            return Err(AppRecipeIrError::ResourceTypeMismatch);
        }
    }
    let root = u16::try_from(nodes.len()).map_err(|_| AppRecipeIrError::ArenaNodeLimit {
        domain: "workflow value",
        limit: MAX_VALUE_NODES,
    })?;
    nodes.push(AppWorkflowValueNode::Array { items: item_refs });
    validate_workflow_value(
        schema,
        AppWorkflowValueSource {
            version: AppWorkflowValueSchemaVersion::V1,
            schema_ref: schema.schema_ref().clone(),
            root,
            nodes,
            provenance,
            resources,
            handling_labels,
        },
    )
}

pub(crate) fn validate_entity_projection_value(
    schema: &AppCompiledWorkflowValueSchema,
    reference: AppReference,
    resource: AppWorkflowResourceRef,
    handling_labels: AppHandlingLabels,
    provenance: BTreeMap<AppReference, AppWorkflowProvenanceEntry>,
) -> Result<AppValidatedWorkflowValue, AppRecipeIrError> {
    if !matches!(
        schema.root_node(),
        AppWorkflowValueTypeNode::EntityProjectionRef { .. }
    ) {
        return Err(AppRecipeIrError::ValueTypeMismatch);
    }
    validate_workflow_value(
        schema,
        AppWorkflowValueSource {
            version: AppWorkflowValueSchemaVersion::V1,
            schema_ref: schema.schema_ref().clone(),
            root: 0,
            nodes: vec![AppWorkflowValueNode::Resource {
                reference: reference.clone(),
            }],
            provenance,
            resources: BTreeMap::from([(reference, resource)]),
            handling_labels,
        },
    )
}

/// Extract one already-validated entity projection without turning its opaque
/// reference into lookup authority. The entity store's private locator owner
/// must still resolve and revalidate that reference before any Get result.
pub(crate) fn exact_entity_projection_resource(
    schema: &AppCompiledWorkflowValueSchema,
    value: &AppValidatedWorkflowValue,
) -> Result<(AppReference, AppWorkflowResourceRef), AppRecipeIrError> {
    if value.schema_ref() != schema.schema_ref()
        || !matches!(
            schema.root_node(),
            AppWorkflowValueTypeNode::EntityProjectionRef { .. }
        )
    {
        return Err(AppRecipeIrError::SchemaSubstitution);
    }
    let AppWorkflowValueNode::Resource { reference } = value
        .source
        .nodes
        .get(usize::from(value.source.root))
        .ok_or(AppRecipeIrError::InvalidArenaReference)?
    else {
        return Err(AppRecipeIrError::ValueTypeMismatch);
    };
    let resource = value
        .source
        .resources
        .get(reference)
        .filter(|resource| matches!(resource, AppWorkflowResourceRef::EntityProjection { .. }))
        .ok_or(AppRecipeIrError::ResourceTypeMismatch)?;
    Ok((reference.clone(), resource.clone()))
}

/// Join the exact results of a bounded Parallel node into its fixed-length
/// array result. Branch order is the lowered semantic-key order, never task
/// completion order. The join copies only validated value arenas and merges
/// their protected provenance/resource descriptors without turning opaque
/// logical references into lookup authority.
pub(crate) fn validate_parallel_workflow_values(
    schema: &AppCompiledWorkflowValueSchema,
    branches: Vec<AppValidatedWorkflowValue>,
) -> Result<AppValidatedWorkflowValue, AppRecipeIrError> {
    let AppWorkflowValueTypeNode::Array {
        min_items,
        max_items,
        ..
    } = schema.root_node()
    else {
        return Err(AppRecipeIrError::ParallelOutputSchema);
    };
    if branches.len() < usize::from(*min_items) || branches.len() > usize::from(*max_items) {
        return Err(AppRecipeIrError::ParallelOutputSchema);
    }

    let mut nodes = Vec::new();
    let mut item_roots = Vec::with_capacity(branches.len());
    let mut provenance = BTreeMap::new();
    let mut resources = BTreeMap::new();
    let mut labels = Vec::with_capacity(branches.len());
    for branch in branches {
        let source = branch.into_source();
        let offset = u16::try_from(nodes.len()).map_err(|_| AppRecipeIrError::ArenaNodeLimit {
            domain: "parallel workflow value",
            limit: MAX_VALUE_NODES,
        })?;
        let root = source
            .root
            .checked_add(offset)
            .ok_or(AppRecipeIrError::InvalidArenaReference)?;
        item_roots.push(root);
        for node in source.nodes {
            nodes.push(offset_workflow_value_node(node, offset)?);
        }
        for (reference, entry) in source.provenance {
            if provenance
                .insert(reference.clone(), entry.clone())
                .is_some_and(|existing| existing != entry)
            {
                return Err(AppRecipeIrError::InvalidProvenanceRef(
                    reference.to_string(),
                ));
            }
        }
        for (reference, resource) in source.resources {
            if resources
                .insert(reference, resource.clone())
                .is_some_and(|existing| existing != resource)
            {
                return Err(AppRecipeIrError::ResourceTypeMismatch);
            }
        }
        labels.push(source.handling_labels);
    }
    let root = u16::try_from(nodes.len()).map_err(|_| AppRecipeIrError::ArenaNodeLimit {
        domain: "parallel workflow value",
        limit: MAX_VALUE_NODES,
    })?;
    nodes.push(AppWorkflowValueNode::Array { items: item_roots });
    let handling_labels = AppHandlingLabels {
        classification: labels
            .iter()
            .map(|label| label.classification)
            .max()
            .unwrap_or(schema.source.handling_floor.classification),
        model_processing: labels
            .iter()
            .map(|label| label.model_processing)
            .min()
            .unwrap_or(schema.source.handling_floor.model_processing),
        policy_digest: AppDigest::blake3_canonical_json(&serde_json::json!({
            "schema": "magician.app-recipe-parallel-policy-join.v1",
            "output_schema_ref": schema.schema_ref(),
            "contributors": labels.iter().map(|label| &label.policy_digest).collect::<Vec<_>>(),
        }))
        .map_err(|error| AppRecipeIrError::Encoding(error.to_string()))?,
        provenance_digest: AppDigest::blake3_canonical_json(&serde_json::json!({
            "schema": "magician.app-recipe-parallel-provenance-join.v1",
            "output_schema_ref": schema.schema_ref(),
            "contributors": labels.iter().map(|label| &label.provenance_digest).collect::<Vec<_>>(),
            "provenance": &provenance,
            "resources": &resources,
        }))
        .map_err(|error| AppRecipeIrError::Encoding(error.to_string()))?,
    };
    validate_workflow_value(
        schema,
        AppWorkflowValueSource {
            version: AppWorkflowValueSchemaVersion::V1,
            schema_ref: schema.schema_ref().clone(),
            root,
            nodes,
            provenance,
            resources,
            handling_labels,
        },
    )
}

fn offset_workflow_value_node(
    mut node: AppWorkflowValueNode,
    offset: u16,
) -> Result<AppWorkflowValueNode, AppRecipeIrError> {
    let shift = |index: &mut u16| -> Result<(), AppRecipeIrError> {
        *index = index
            .checked_add(offset)
            .ok_or(AppRecipeIrError::InvalidArenaReference)?;
        Ok(())
    };
    match &mut node {
        AppWorkflowValueNode::Array { items } => {
            for item in items {
                shift(item)?;
            }
        },
        AppWorkflowValueNode::Record { fields } => {
            for child in fields.values_mut() {
                shift(child)?;
            }
        },
        AppWorkflowValueNode::TaggedUnion { value, .. } => shift(value)?,
        _ => {},
    }
    Ok(node)
}

pub(crate) fn entity_projection_contract(
    schema: &AppCompiledWorkflowValueSchema,
) -> Option<(&AppName, &AppReference)> {
    match schema.root_node() {
        AppWorkflowValueTypeNode::EntityProjectionRef {
            entity,
            value_schema_ref,
        } => Some((entity, value_schema_ref)),
        _ => None,
    }
}

// The rich schema compiler admits this shape even though no current workflow
// result owner emits it yet.
#[allow(dead_code)]
pub(crate) fn entity_projection_array_contract(
    schema: &AppCompiledWorkflowValueSchema,
) -> Option<(&AppName, &AppReference)> {
    let AppWorkflowValueTypeNode::Array { items, .. } = schema.root_node() else {
        return None;
    };
    match schema.source.nodes.get(usize::from(*items))? {
        AppWorkflowValueTypeNode::EntityProjectionRef {
            entity,
            value_schema_ref,
        } => Some((entity, value_schema_ref)),
        _ => None,
    }
}

pub(crate) fn tagged_union_selection(
    schema: &AppCompiledWorkflowValueSchema,
    value: &AppValidatedWorkflowValue,
) -> Option<(AppName, AppName)> {
    if value.schema_ref() != schema.schema_ref() {
        return None;
    }
    let AppWorkflowValueTypeNode::TaggedUnion { discriminator, .. } = schema.root_node() else {
        return None;
    };
    let AppWorkflowValueNode::TaggedUnion { tag, .. } =
        value.source.nodes.get(usize::from(value.source.root))?
    else {
        return None;
    };
    Some((discriminator.clone(), tag.clone()))
}

/// Select one exact Switch payload while preserving the parent's labels,
/// provenance and only the resource descriptors reachable from that payload.
pub(crate) fn project_tagged_union_payload(
    tagged_schema: &AppCompiledWorkflowValueSchema,
    payload_schema: &AppCompiledWorkflowValueSchema,
    value: &AppValidatedWorkflowValue,
    discriminator: &AppName,
    expected_tag: &AppName,
) -> Result<AppValidatedWorkflowValue, AppRecipeIrError> {
    if value.schema_ref() != tagged_schema.schema_ref() {
        return Err(AppRecipeIrError::SchemaSubstitution);
    }
    let AppWorkflowValueTypeNode::TaggedUnion {
        discriminator: actual_discriminator,
        variants,
    } = tagged_schema.root_node()
    else {
        return Err(AppRecipeIrError::ValueTypeMismatch);
    };
    let AppWorkflowValueNode::TaggedUnion { tag, value: child } = value
        .source
        .nodes
        .get(usize::from(value.source.root))
        .ok_or(AppRecipeIrError::InvalidArenaReference)?
    else {
        return Err(AppRecipeIrError::ValueTypeMismatch);
    };
    if actual_discriminator != discriminator || tag != expected_tag || variants.get(tag).is_none() {
        return Err(AppRecipeIrError::ValueTypeMismatch);
    }
    let order = canonical_index_order(*child, value.source.nodes.len(), |index| {
        value_children(&value.source.nodes[index])
    });
    let remap = index_remap(&order, value.source.nodes.len())?;
    let mut nodes = Vec::with_capacity(order.len());
    let mut used_resources = BTreeSet::new();
    for old in order {
        let mut node = value.source.nodes[old].clone();
        if let AppWorkflowValueNode::Resource { reference } = &node {
            used_resources.insert(reference.clone());
        }
        remap_value_children(&mut node, &remap)?;
        nodes.push(node);
    }
    let resources = value
        .source
        .resources
        .iter()
        .filter(|(reference, _)| used_resources.contains(*reference))
        .map(|(reference, resource)| (reference.clone(), resource.clone()))
        .collect();
    validate_workflow_value(
        payload_schema,
        AppWorkflowValueSource {
            version: AppWorkflowValueSchemaVersion::V1,
            schema_ref: payload_schema.schema_ref().clone(),
            root: 0,
            nodes,
            provenance: value.source.provenance.clone(),
            resources,
            handling_labels: value.source.handling_labels.clone(),
        },
    )
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppRecipeEffectClass {
    None,
    ReadOnly,
    Reasoning,
    InternalMutation,
    ExternalEffect,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppRecipeIdempotencyContract {
    NotApplicable,
    Intrinsic,
    ReviewedKey { contract_digest: AppDigest },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppRecipeUncertaintyContract {
    Impossible,
    ReceiptBound,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRecipeEffectContract {
    pub class: AppRecipeEffectClass,
    pub idempotency: AppRecipeIdempotencyContract,
    pub uncertainty: AppRecipeUncertaintyContract,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRecipeAuthorityContract {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primitive_ref: Option<AppReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_ref: Option<AppReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_app_ref: Option<AppReference>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub required_grant_refs: BTreeSet<AppReference>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub resource_scope_refs: BTreeSet<AppReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRecipeNodeResourceCeiling {
    pub max_active_millis: u64,
    pub max_input_bytes: u64,
    pub max_output_bytes: u64,
    pub max_cost_microusd: u64,
    pub max_tool_calls: u16,
    pub max_parallelism: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppRecipeRetryContract {
    None,
    Bounded {
        max_attempts: u16,
        initial_backoff_millis: u64,
        max_backoff_millis: u64,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppRecipeCancellationMode {
    Propagate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRecipeCancellationContract {
    pub mode: AppRecipeCancellationMode,
    pub acknowledgement_timeout_millis: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppRecipeProvenanceJoin {
    Preserve,
    Intersection,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppRecipeOutputKind {
    Unit,
    TypedValue,
    EntityProjection,
    Artifact,
    Receipt,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppRecipeOutputAuthority {
    Authoritative,
    Derived,
    FeedbackOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRecipeOutputContract {
    pub kind: AppRecipeOutputKind,
    pub schema_ref: AppReference,
    pub authority: AppRecipeOutputAuthority,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppRecipeDeferredNodeKind {
    BoundedForEach,
    WaitEvent,
    WaitUser,
    Approval,
    Timer,
    Handoff,
    Contribution,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppRecipeNodeKind {
    Query {
        entity: AppName,
    },
    Get {
        entity: AppName,
    },
    Map {
        mapping_digest: AppDigest,
        /// Exact pure mapping body installed inside the immutable Recipe
        /// member. Lowering recompiles these bytes and refuses a digest-only
        /// declaration, so the digest can never stand in for executable code.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        operations: Vec<AppValueMappingOperation>,
    },
    Validate,
    Reconcile {
        declaration: super::reconciliation::AppReconciliation,
    },
    StoreTransaction {
        sources: BTreeMap<AppName, super::reconciliation::AppReconcileSource>,
        program: super::store_transaction::AppStoreTransactionDeclaration,
    },
    ContextualRound {
        source: super::reconciliation::AppReconcileSource,
        cursor_parameter: AppName,
        max_source_pages: u16,
        program: super::contextual_round_declaration::AppRoundProgramDeclaration,
    },
    CallTool,
    InvokeAction,
    Mutate {
        entities: BTreeSet<AppName>,
    },
    RunProcedure,
    AgentAsTool,
    Sequence {
        steps: Vec<AppName>,
    },
    Parallel {
        branches: BTreeMap<AppName, AppName>,
    },
    Switch {
        discriminator: AppName,
        cases: BTreeMap<AppName, AppName>,
    },
    EmitValue,
    EmitArtifact,
    EmitReceipt,
    Retry {
        child: AppName,
    },
    MarkUncertain {
        child: AppName,
    },
    Deferred {
        node: AppRecipeDeferredNodeKind,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRecipeNode {
    pub input_schema_ref: AppReference,
    pub output: AppRecipeOutputContract,
    pub node: AppRecipeNodeKind,
    pub effect: AppRecipeEffectContract,
    pub authority: AppRecipeAuthorityContract,
    pub resources: AppRecipeNodeResourceCeiling,
    pub retry: AppRecipeRetryContract,
    pub cancellation: AppRecipeCancellationContract,
    pub provenance_join: AppRecipeProvenanceJoin,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRecipeGraphCeiling {
    pub max_nodes: u16,
    pub max_edges: u16,
    pub max_depth: u16,
    pub max_fan_out: u16,
    pub max_parallelism: u16,
    pub max_payload_bytes: u64,
    pub max_active_millis: u64,
    pub max_cost_microusd: u64,
    pub max_tool_calls: u16,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppRecipeMigrationPolicy {
    RecompileRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRecipeEvolutionContract {
    pub topology_revision: AppRevision,
    pub migration: AppRecipeMigrationPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predecessor_recipe_ref: Option<AppReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRecipeIrSource {
    pub version: AppRecipeVersion,
    pub input_schema_ref: AppReference,
    pub output: AppRecipeOutputContract,
    pub root: AppName,
    pub nodes: BTreeMap<AppName, AppRecipeNode>,
    pub ceilings: AppRecipeGraphCeiling,
    pub evolution: AppRecipeEvolutionContract,
}

/// One immutable package member containing the exact schema catalog and recipe
/// graph. Names are declaration keys only; every schema identity is content
/// addressed and every declaration must be referenced by the graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRecipeBundleSource {
    pub version: AppRecipeBundleVersion,
    pub schemas: BTreeMap<AppName, AppWorkflowValueSchemaSource>,
    pub recipe: AppRecipeIrSource,
}

#[derive(Clone)]
pub struct AppCompiledRecipeBundle {
    bundle_version: &'static str,
    bundle_digest: AppDigest,
    schemas: BTreeMap<AppReference, AppCompiledWorkflowValueSchema>,
    recipe: AppCompiledRecipeIr,
}

impl fmt::Debug for AppCompiledRecipeBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppCompiledRecipeBundle")
            .field("bundle_version", &self.bundle_version)
            .field("bundle_digest", &self.bundle_digest)
            .field("recipe_ref", self.recipe.recipe_ref())
            .field("topology_digest", self.recipe.topology_digest())
            .field("schema_count", &self.schemas.len())
            .finish()
    }
}

impl AppCompiledRecipeBundle {
    pub fn bundle_digest(&self) -> &AppDigest {
        &self.bundle_digest
    }

    pub fn recipe(&self) -> &AppCompiledRecipeIr {
        &self.recipe
    }

    pub fn schema(&self, schema_ref: &AppReference) -> Option<&AppCompiledWorkflowValueSchema> {
        self.schemas.get(schema_ref)
    }

    pub fn schemas(&self) -> impl Iterator<Item = &AppCompiledWorkflowValueSchema> {
        self.schemas.values()
    }
}

pub fn compile_recipe_bundle(
    source: AppRecipeBundleSource,
) -> Result<AppCompiledRecipeBundle, AppRecipeIrError> {
    if source.version != AppRecipeBundleVersion::V1 || source.schemas.is_empty() {
        return Err(AppRecipeIrError::UnsupportedRecipeBundleVersion);
    }
    let mut schemas = BTreeMap::new();
    for schema_source in source.schemas.into_values() {
        let schema = compile_workflow_value_schema(schema_source)?;
        if schemas
            .insert(schema.schema_ref().clone(), schema)
            .is_some()
        {
            return Err(AppRecipeIrError::DuplicateRecipeBundleSchema);
        }
    }
    let schema_refs = schemas.values().collect::<Vec<_>>();
    let recipe = compile_recipe_ir(source.recipe, &schema_refs)?;
    let mut referenced = BTreeSet::from([
        recipe.source().input_schema_ref.clone(),
        recipe.source().output.schema_ref.clone(),
    ]);
    for node in recipe.source().nodes.values() {
        referenced.insert(node.input_schema_ref.clone());
        referenced.insert(node.output.schema_ref.clone());
    }
    if referenced.len() != schemas.len()
        || referenced
            .iter()
            .any(|schema_ref| !schemas.contains_key(schema_ref))
    {
        return Err(AppRecipeIrError::UnreferencedRecipeBundleSchema);
    }
    let (bundle_digest, _) = stream_canonical_identity(
        "compiled recipe bundle",
        &serde_json::json!({
            "version": APP_RECIPE_BUNDLE_VERSION,
            "recipe_ref": recipe.recipe_ref(),
            "topology_digest": recipe.topology_digest(),
            "schemas": schemas
                .iter()
                .map(|(schema_ref, schema)| (schema_ref, schema.content_digest()))
                .collect::<BTreeMap<_, _>>(),
        }),
        MAX_RECIPE_CANONICAL_BYTES,
    )?;
    Ok(AppCompiledRecipeBundle {
        bundle_version: APP_RECIPE_BUNDLE_VERSION,
        bundle_digest,
        schemas,
        recipe,
    })
}

/// Validated recipe identity. This type itself remains inert: the crate-private
/// lowering owner must bind it to an exact immutable workflow authority fence,
/// and the catalog/readiness surfaces remain independently gated.
#[derive(Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppCompiledRecipeIr {
    recipe_version: &'static str,
    recipe_ref: AppReference,
    topology_digest: AppDigest,
    canonical_encoded_len: u64,
    source: AppRecipeIrSource,
}

impl fmt::Debug for AppCompiledRecipeIr {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppCompiledRecipeIr")
            .field("recipe_ref", &self.recipe_ref)
            .field("topology_digest", &self.topology_digest)
            .field("canonical_encoded_len", &self.canonical_encoded_len)
            .field("node_count", &self.source.nodes.len())
            .finish()
    }
}

impl AppCompiledRecipeIr {
    pub fn recipe_ref(&self) -> &AppReference {
        &self.recipe_ref
    }

    pub fn topology_digest(&self) -> &AppDigest {
        &self.topology_digest
    }

    pub fn canonical_encoded_len(&self) -> u64 {
        self.canonical_encoded_len
    }

    pub fn source(&self) -> &AppRecipeIrSource {
        &self.source
    }

    pub fn write_canonical<W: Write>(&self, writer: W) -> Result<(), AppRecipeIrError> {
        write_canonical_stream(writer, &self.source)
    }
}

pub fn compile_recipe_ir(
    source: AppRecipeIrSource,
    schemas: &[&AppCompiledWorkflowValueSchema],
) -> Result<AppCompiledRecipeIr, AppRecipeIrError> {
    if source.version != AppRecipeVersion::V1 {
        return Err(AppRecipeIrError::UnsupportedRecipeVersion);
    }
    let mut catalog = BTreeMap::new();
    for schema in schemas {
        if catalog.insert(schema.schema_ref.clone(), *schema).is_some() {
            return Err(AppRecipeIrError::DuplicateSchemaRef(
                schema.schema_ref.to_string(),
            ));
        }
    }
    validate_recipe(&source, &catalog)?;
    let (topology_digest, canonical_encoded_len) =
        stream_canonical_identity("recipe IR", &source, MAX_RECIPE_CANONICAL_BYTES)?;
    let recipe_ref = AppReference::parse(format!("recipe:{}", topology_digest.as_str()))?;
    Ok(AppCompiledRecipeIr {
        recipe_version: APP_RECIPE_IR_VERSION,
        recipe_ref,
        topology_digest,
        canonical_encoded_len: canonical_encoded_len as u64,
        source,
    })
}

fn validate_schema_nodes(source: &AppWorkflowValueSchemaSource) -> Result<(), AppRecipeIrError> {
    validate_index_arena(
        "workflow schema",
        source.root,
        source.nodes.len(),
        MAX_SCHEMA_NODES,
        MAX_SCHEMA_EDGES,
        MAX_SCHEMA_DEPTH,
        true,
        |index| schema_children(&source.nodes[index]),
    )?;
    for node in &source.nodes {
        match node {
            AppWorkflowValueTypeNode::Text { max_bytes }
            | AppWorkflowValueTypeNode::Markdown { max_bytes } => {
                if *max_bytes == 0 || *max_bytes > MAX_TEXT_BYTES {
                    return Err(AppRecipeIrError::InvalidTextCeiling);
                }
            },
            AppWorkflowValueTypeNode::Enum { values } => {
                if values.is_empty() || values.len() > MAX_UNION_VARIANTS {
                    return Err(AppRecipeIrError::InvalidEnum);
                }
            },
            AppWorkflowValueTypeNode::EntityProjectionRef {
                value_schema_ref, ..
            } => {
                if !is_value_schema_ref(value_schema_ref) {
                    return Err(AppRecipeIrError::InvalidResourceSchemaRef);
                }
            },
            AppWorkflowValueTypeNode::ArtifactRef {
                value_schema_ref,
                max_bytes,
                media_types,
            } => {
                if !is_value_schema_ref(value_schema_ref)
                    || *max_bytes == 0
                    || media_types.is_empty()
                    || media_types.len() > MAX_MEDIA_TYPES
                    || media_types.iter().any(|media| !valid_media_type(media))
                {
                    return Err(AppRecipeIrError::InvalidArtifactType);
                }
            },
            AppWorkflowValueTypeNode::Array {
                min_items,
                max_items,
                ..
            } => {
                if min_items > max_items || *max_items > MAX_ARRAY_ITEMS {
                    return Err(AppRecipeIrError::InvalidArrayBounds);
                }
            },
            AppWorkflowValueTypeNode::Record { fields } => {
                if fields.is_empty() || fields.len() > MAX_RECORD_FIELDS {
                    return Err(AppRecipeIrError::RecordFieldLimit);
                }
            },
            AppWorkflowValueTypeNode::TaggedUnion {
                discriminator,
                variants,
            } => {
                if variants.is_empty()
                    || variants.len() > MAX_UNION_VARIANTS
                    || discriminator.as_str() == "value"
                    || variants.contains_key(discriminator)
                {
                    return Err(AppRecipeIrError::InvalidTaggedUnion);
                }
            },
            _ => {},
        }
    }
    Ok(())
}

fn normalize_schema_arena(
    mut source: AppWorkflowValueSchemaSource,
) -> Result<AppWorkflowValueSchemaSource, AppRecipeIrError> {
    let order = canonical_index_order(source.root, source.nodes.len(), |index| {
        schema_children(&source.nodes[index])
    });
    let remap = index_remap(&order, source.nodes.len())?;
    let mut normalized = Vec::with_capacity(source.nodes.len());
    for old in order {
        let mut node = source.nodes[old].clone();
        remap_schema_children(&mut node, &remap)?;
        normalized.push(node);
    }
    source.root = 0;
    source.nodes = normalized;
    Ok(source)
}

fn schema_children(node: &AppWorkflowValueTypeNode) -> Vec<u16> {
    match node {
        AppWorkflowValueTypeNode::Nullable { value_type } => vec![*value_type],
        AppWorkflowValueTypeNode::Array { items, .. } => vec![*items],
        AppWorkflowValueTypeNode::Record { fields } => {
            fields.values().map(|field| field.value_type).collect()
        },
        AppWorkflowValueTypeNode::TaggedUnion { variants, .. } => {
            variants.values().copied().collect()
        },
        _ => Vec::new(),
    }
}

fn remap_schema_children(
    node: &mut AppWorkflowValueTypeNode,
    remap: &[u16],
) -> Result<(), AppRecipeIrError> {
    let mapped = |index: u16| {
        remap
            .get(usize::from(index))
            .copied()
            .ok_or(AppRecipeIrError::InvalidArenaReference)
    };
    match node {
        AppWorkflowValueTypeNode::Nullable { value_type } => *value_type = mapped(*value_type)?,
        AppWorkflowValueTypeNode::Array { items, .. } => *items = mapped(*items)?,
        AppWorkflowValueTypeNode::Record { fields } => {
            for field in fields.values_mut() {
                field.value_type = mapped(field.value_type)?;
            }
        },
        AppWorkflowValueTypeNode::TaggedUnion { variants, .. } => {
            for value_type in variants.values_mut() {
                *value_type = mapped(*value_type)?;
            }
        },
        _ => {},
    }
    Ok(())
}

fn validate_value_nodes(
    source: &AppWorkflowValueSource,
    schema: &AppCompiledWorkflowValueSchema,
) -> Result<(), AppRecipeIrError> {
    validate_index_arena(
        "workflow value",
        source.root,
        source.nodes.len(),
        MAX_VALUE_NODES,
        MAX_VALUE_EDGES,
        MAX_VALUE_DEPTH,
        false,
        |index| value_children(&source.nodes[index]),
    )?;
    let mut stack = vec![(source.root, schema.source.root, 0usize)];
    let mut used_resources = BTreeSet::new();
    while let Some((value_index, type_index, depth)) = stack.pop() {
        if depth > MAX_VALUE_DEPTH {
            return Err(AppRecipeIrError::ArenaDepth {
                domain: "workflow value",
                limit: MAX_VALUE_DEPTH,
            });
        }
        let value = source
            .nodes
            .get(usize::from(value_index))
            .ok_or(AppRecipeIrError::InvalidArenaReference)?;
        let value_type = schema
            .source
            .nodes
            .get(usize::from(type_index))
            .ok_or(AppRecipeIrError::InvalidArenaReference)?;
        match (value, value_type) {
            (AppWorkflowValueNode::Unit, AppWorkflowValueTypeNode::Unit)
            | (AppWorkflowValueNode::Boolean { .. }, AppWorkflowValueTypeNode::Boolean)
            | (AppWorkflowValueNode::Integer { .. }, AppWorkflowValueTypeNode::Integer)
            | (AppWorkflowValueNode::Decimal { .. }, AppWorkflowValueTypeNode::Decimal)
            | (AppWorkflowValueNode::Timestamp { .. }, AppWorkflowValueTypeNode::Timestamp)
            | (
                AppWorkflowValueNode::OpaqueReference { .. },
                AppWorkflowValueTypeNode::OpaqueReference,
            ) => {},
            (AppWorkflowValueNode::Enum { value }, AppWorkflowValueTypeNode::Enum { values })
                if values.contains(value) => {},
            (AppWorkflowValueNode::Null, AppWorkflowValueTypeNode::Nullable { .. }) => {},
            (_, AppWorkflowValueTypeNode::Nullable { value_type }) => {
                stack.push((value_index, *value_type, depth.saturating_add(1)));
            },
            (
                AppWorkflowValueNode::Text { value },
                AppWorkflowValueTypeNode::Text { max_bytes }
                | AppWorkflowValueTypeNode::Markdown { max_bytes },
            ) if value.len() <= *max_bytes as usize => {},
            (
                AppWorkflowValueNode::Text { value },
                AppWorkflowValueTypeNode::EntityReference { .. },
            ) if !value.is_empty() && value.len() <= 256 => {},
            (
                AppWorkflowValueNode::Resource { reference },
                AppWorkflowValueTypeNode::EntityProjectionRef {
                    entity,
                    value_schema_ref,
                },
            ) => match source.resources.get(reference) {
                Some(AppWorkflowResourceRef::EntityProjection {
                    entity: actual_entity,
                    value_schema_ref: actual_schema,
                    ..
                }) if actual_entity == entity && actual_schema == value_schema_ref => {
                    used_resources.insert(reference.clone());
                },
                _ => return Err(AppRecipeIrError::ResourceTypeMismatch),
            },
            (
                AppWorkflowValueNode::Resource { reference },
                AppWorkflowValueTypeNode::ArtifactRef {
                    value_schema_ref,
                    max_bytes,
                    media_types,
                },
            ) => match source.resources.get(reference) {
                Some(AppWorkflowResourceRef::Artifact {
                    value_schema_ref: actual_schema,
                    media_type,
                    byte_len,
                    ..
                }) if actual_schema == value_schema_ref
                    && byte_len <= max_bytes
                    && media_types.contains(media_type) =>
                {
                    used_resources.insert(reference.clone());
                },
                _ => return Err(AppRecipeIrError::ResourceTypeMismatch),
            },
            (
                AppWorkflowValueNode::Resource { reference },
                AppWorkflowValueTypeNode::ReceiptRef { receipt_kind },
            ) => match source.resources.get(reference) {
                Some(AppWorkflowResourceRef::Receipt {
                    receipt_kind: actual_kind,
                    ..
                }) if actual_kind == receipt_kind => {
                    used_resources.insert(reference.clone());
                },
                _ => return Err(AppRecipeIrError::ResourceTypeMismatch),
            },
            (
                AppWorkflowValueNode::Resource { reference },
                AppWorkflowValueTypeNode::ResourceRef { resource_kind },
            ) => match source.resources.get(reference) {
                Some(AppWorkflowResourceRef::Resource {
                    resource_kind: actual_kind,
                    ..
                }) if actual_kind == resource_kind => {
                    used_resources.insert(reference.clone());
                },
                _ => return Err(AppRecipeIrError::ResourceTypeMismatch),
            },
            (
                AppWorkflowValueNode::Array { items },
                AppWorkflowValueTypeNode::Array {
                    items: item_type,
                    min_items,
                    max_items,
                },
            ) if items.len() >= usize::from(*min_items)
                && items.len() <= usize::from(*max_items) =>
            {
                for item in items.iter().rev() {
                    stack.push((*item, *item_type, depth.saturating_add(1)));
                }
            },
            (
                AppWorkflowValueNode::Record { fields },
                AppWorkflowValueTypeNode::Record {
                    fields: type_fields,
                },
            ) => {
                if fields.len() > MAX_RECORD_FIELDS
                    || fields.keys().any(|name| !type_fields.contains_key(name))
                    || type_fields
                        .iter()
                        .any(|(name, field)| field.required && !fields.contains_key(name))
                {
                    return Err(AppRecipeIrError::ValueTypeMismatch);
                }
                for (name, child) in fields.iter().rev() {
                    let field = type_fields
                        .get(name)
                        .ok_or(AppRecipeIrError::ValueTypeMismatch)?;
                    stack.push((*child, field.value_type, depth.saturating_add(1)));
                }
            },
            (
                AppWorkflowValueNode::TaggedUnion { tag, value },
                AppWorkflowValueTypeNode::TaggedUnion { variants, .. },
            ) => {
                let variant = variants
                    .get(tag)
                    .ok_or(AppRecipeIrError::ValueTypeMismatch)?;
                stack.push((*value, *variant, depth.saturating_add(1)));
            },
            _ => return Err(AppRecipeIrError::ValueTypeMismatch),
        }
    }
    if used_resources.len() != source.resources.len() {
        return Err(AppRecipeIrError::UnusedResourceRef);
    }
    Ok(())
}

fn normalize_value_arena(
    mut source: AppWorkflowValueSource,
) -> Result<AppWorkflowValueSource, AppRecipeIrError> {
    let order = canonical_index_order(source.root, source.nodes.len(), |index| {
        value_children(&source.nodes[index])
    });
    let remap = index_remap(&order, source.nodes.len())?;
    let mut normalized = Vec::with_capacity(source.nodes.len());
    for old in order {
        let mut node = source.nodes[old].clone();
        remap_value_children(&mut node, &remap)?;
        normalized.push(node);
    }
    source.root = 0;
    source.nodes = normalized;
    Ok(source)
}

fn value_children(node: &AppWorkflowValueNode) -> Vec<u16> {
    match node {
        AppWorkflowValueNode::Array { items } => items.clone(),
        AppWorkflowValueNode::Record { fields } => fields.values().copied().collect(),
        AppWorkflowValueNode::TaggedUnion { value, .. } => vec![*value],
        _ => Vec::new(),
    }
}

fn remap_value_children(
    node: &mut AppWorkflowValueNode,
    remap: &[u16],
) -> Result<(), AppRecipeIrError> {
    let mapped = |index: u16| {
        remap
            .get(usize::from(index))
            .copied()
            .ok_or(AppRecipeIrError::InvalidArenaReference)
    };
    match node {
        AppWorkflowValueNode::Array { items } => {
            for item in items {
                *item = mapped(*item)?;
            }
        },
        AppWorkflowValueNode::Record { fields } => {
            for value in fields.values_mut() {
                *value = mapped(*value)?;
            }
        },
        AppWorkflowValueNode::TaggedUnion { value, .. } => *value = mapped(*value)?,
        _ => {},
    }
    Ok(())
}

fn validate_resource_refs(
    resources: &BTreeMap<AppReference, AppWorkflowResourceRef>,
) -> Result<(), AppRecipeIrError> {
    for (reference, resource) in resources {
        let prefix = match resource {
            AppWorkflowResourceRef::EntityProjection { .. } => "entity:",
            AppWorkflowResourceRef::Artifact { media_type, .. } => {
                if !valid_media_type(media_type) {
                    return Err(AppRecipeIrError::InvalidArtifactType);
                }
                "artifact:"
            },
            AppWorkflowResourceRef::Receipt { .. } => "receipt:",
            AppWorkflowResourceRef::Resource { .. } => "resource:",
        };
        if !is_opaque_logical_ref(reference, prefix) {
            return Err(AppRecipeIrError::NonLogicalResourceRef(
                reference.to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_provenance_refs(
    provenance: &BTreeMap<AppReference, AppWorkflowProvenanceEntry>,
) -> Result<(), AppRecipeIrError> {
    for (reference, entry) in provenance {
        let (prefix, requires_revision, permits_fields) = match entry.kind {
            AppSourceRefKind::EntityRecord => ("entity:", true, false),
            AppSourceRefKind::EntityField => ("entity:", true, true),
            AppSourceRefKind::Artifact => ("artifact:", true, false),
            AppSourceRefKind::ExternalReceipt | AppSourceRefKind::MutationReceipt => {
                ("receipt:", false, false)
            },
        };
        if !is_opaque_logical_ref(reference, prefix)
            || requires_revision != entry.revision.is_some()
            || entry.fields.len() > MAX_RECORD_FIELDS
            || (permits_fields && entry.fields.is_empty())
            || (!permits_fields && !entry.fields.is_empty())
        {
            return Err(AppRecipeIrError::InvalidProvenanceRef(
                reference.to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_label_join<'a>(
    output: &AppHandlingLabels,
    schema_floor: &AppWorkflowHandlingFloor,
    contributors: impl Iterator<Item = &'a AppHandlingLabels>,
) -> Result<(), AppRecipeIrError> {
    if output.classification < schema_floor.classification
        || output.model_processing > schema_floor.model_processing
    {
        return Err(AppRecipeIrError::HandlingLabelDowngrade);
    }
    for contributor in contributors {
        if output.classification < contributor.classification
            || output.model_processing > contributor.model_processing
        {
            return Err(AppRecipeIrError::HandlingLabelDowngrade);
        }
    }
    Ok(())
}

fn validate_recipe(
    source: &AppRecipeIrSource,
    schemas: &BTreeMap<AppReference, &AppCompiledWorkflowValueSchema>,
) -> Result<(), AppRecipeIrError> {
    validate_graph_ceiling(&source.ceilings)?;
    let input_schema = schema_by_ref(schemas, &source.input_schema_ref)?;
    let output_schema = schema_by_ref(schemas, &source.output.schema_ref)?;
    validate_output_kind(&source.output, output_schema)?;
    let root = source
        .nodes
        .get(&source.root)
        .ok_or_else(|| AppRecipeIrError::UnknownRecipeNode(source.root.to_string()))?;
    if root.input_schema_ref != source.input_schema_ref || root.output != source.output {
        return Err(AppRecipeIrError::RecipeBoundaryMismatch);
    }
    if source.nodes.is_empty()
        || source.nodes.len() > usize::from(source.ceilings.max_nodes)
        || source.nodes.len() > MAX_RECIPE_NODES
    {
        return Err(AppRecipeIrError::RecipeNodeLimit);
    }
    validate_recipe_graph(source)?;
    let mut aggregate_active = 0u64;
    let mut aggregate_cost = 0u64;
    let mut aggregate_payload = 0u64;
    let mut aggregate_tool_calls = 0u64;
    for (node_id, node) in &source.nodes {
        let input = schema_by_ref(schemas, &node.input_schema_ref)?;
        let output = schema_by_ref(schemas, &node.output.schema_ref)?;
        validate_output_kind(&node.output, output)?;
        validate_node_contract(node_id, node, source, schemas, input, output)?;
        aggregate_active = aggregate_active
            .checked_add(node.resources.max_active_millis)
            .ok_or(AppRecipeIrError::RecipeResourceOverflow)?;
        aggregate_cost = aggregate_cost
            .checked_add(node.resources.max_cost_microusd)
            .ok_or(AppRecipeIrError::RecipeResourceOverflow)?;
        aggregate_payload = aggregate_payload
            .checked_add(node.resources.max_input_bytes)
            .and_then(|value| value.checked_add(node.resources.max_output_bytes))
            .ok_or(AppRecipeIrError::RecipeResourceOverflow)?;
        aggregate_tool_calls = aggregate_tool_calls
            .checked_add(u64::from(node.resources.max_tool_calls))
            .ok_or(AppRecipeIrError::RecipeResourceOverflow)?;
    }
    if aggregate_active > source.ceilings.max_active_millis
        || aggregate_cost > source.ceilings.max_cost_microusd
        || aggregate_payload > source.ceilings.max_payload_bytes
        || aggregate_tool_calls > u64::from(source.ceilings.max_tool_calls)
    {
        return Err(AppRecipeIrError::RecipeResourceOverflow);
    }
    if source.evolution.topology_revision.get() == 1
        && source.evolution.predecessor_recipe_ref.is_some()
        || source.evolution.topology_revision.get() > 1
            && source.evolution.predecessor_recipe_ref.is_none()
        || source
            .evolution
            .predecessor_recipe_ref
            .as_ref()
            .is_some_and(|reference| !is_digest_logical_ref(reference, "recipe:"))
    {
        return Err(AppRecipeIrError::InvalidEvolutionContract);
    }
    let _ = input_schema;
    Ok(())
}

fn validate_graph_ceiling(ceiling: &AppRecipeGraphCeiling) -> Result<(), AppRecipeIrError> {
    if ceiling.max_nodes == 0
        || usize::from(ceiling.max_nodes) > MAX_RECIPE_NODES
        || ceiling.max_edges == 0
        || usize::from(ceiling.max_edges) > MAX_RECIPE_EDGES
        || ceiling.max_depth == 0
        || usize::from(ceiling.max_depth) > MAX_RECIPE_DEPTH
        || ceiling.max_fan_out == 0
        || usize::from(ceiling.max_fan_out) > MAX_RECIPE_FAN_OUT
        || ceiling.max_parallelism == 0
        || usize::from(ceiling.max_parallelism) > MAX_RECIPE_FAN_OUT
        || ceiling.max_payload_bytes == 0
        || ceiling.max_payload_bytes > MAX_VALUE_CANONICAL_BYTES as u64
        || ceiling.max_active_millis == 0
        || ceiling.max_active_millis
            > MAX_NODE_ACTIVE_MILLIS.saturating_mul(MAX_RECIPE_NODES as u64)
        || usize::from(ceiling.max_tool_calls) > MAX_RECIPE_NODES
    {
        return Err(AppRecipeIrError::InvalidGraphCeiling);
    }
    Ok(())
}

fn validate_recipe_graph(source: &AppRecipeIrSource) -> Result<(), AppRecipeIrError> {
    let mut states = BTreeMap::<AppName, u8>::new();
    let mut indegree = BTreeMap::<AppName, usize>::new();
    let mut edge_count = 0usize;
    let mut stack = vec![(source.root.clone(), false)];
    let mut postorder = Vec::with_capacity(source.nodes.len());
    while let Some((node_id, exiting)) = stack.pop() {
        if exiting {
            states.insert(node_id.clone(), 2);
            postorder.push(node_id);
            continue;
        }
        match states.get(&node_id).copied().unwrap_or(0) {
            1 => return Err(AppRecipeIrError::RecipeCycle(node_id.to_string())),
            2 => continue,
            _ => {},
        }
        let node = source
            .nodes
            .get(&node_id)
            .ok_or_else(|| AppRecipeIrError::UnknownRecipeNode(node_id.to_string()))?;
        states.insert(node_id.clone(), 1);
        stack.push((node_id.clone(), true));
        let children = recipe_children(&node.node);
        edge_count = edge_count.saturating_add(children.len());
        if children.len() > usize::from(source.ceilings.max_fan_out)
            || children.len() > MAX_RECIPE_FAN_OUT
        {
            return Err(AppRecipeIrError::RecipeFanOut);
        }
        for child in children.into_iter().rev() {
            if !source.nodes.contains_key(&child) {
                return Err(AppRecipeIrError::UnknownRecipeNode(child.to_string()));
            }
            *indegree.entry(child.clone()).or_default() += 1;
            stack.push((child, false));
        }
    }
    if edge_count > usize::from(source.ceilings.max_edges) || edge_count > MAX_RECIPE_EDGES {
        return Err(AppRecipeIrError::RecipeEdgeLimit);
    }
    if states.len() != source.nodes.len() {
        return Err(AppRecipeIrError::UnreachableRecipeNode);
    }
    if source
        .nodes
        .keys()
        .any(|node| node != &source.root && indegree.get(node).copied().unwrap_or(0) != 1)
    {
        return Err(AppRecipeIrError::SharedRecipeNode);
    }
    postorder.reverse();
    let mut depth = BTreeMap::from([(source.root.clone(), 1usize)]);
    for node_id in postorder {
        let node_depth = depth.get(&node_id).copied().unwrap_or(1);
        if node_depth > usize::from(source.ceilings.max_depth) || node_depth > MAX_RECIPE_DEPTH {
            return Err(AppRecipeIrError::RecipeDepth);
        }
        let node = source
            .nodes
            .get(&node_id)
            .ok_or_else(|| AppRecipeIrError::UnknownRecipeNode(node_id.to_string()))?;
        for child in recipe_children(&node.node) {
            let candidate = node_depth.saturating_add(1);
            depth
                .entry(child)
                .and_modify(|current| *current = (*current).max(candidate))
                .or_insert(candidate);
        }
    }
    Ok(())
}

fn validate_node_contract(
    node_id: &AppName,
    node: &AppRecipeNode,
    recipe: &AppRecipeIrSource,
    schemas: &BTreeMap<AppReference, &AppCompiledWorkflowValueSchema>,
    input: &AppCompiledWorkflowValueSchema,
    output: &AppCompiledWorkflowValueSchema,
) -> Result<(), AppRecipeIrError> {
    if node.resources.max_active_millis == 0
        || node.resources.max_active_millis > MAX_NODE_ACTIVE_MILLIS
        || node.resources.max_input_bytes == 0
        || node.resources.max_input_bytes > recipe.ceilings.max_payload_bytes
        || node.resources.max_output_bytes == 0
        || node.resources.max_output_bytes > recipe.ceilings.max_payload_bytes
        || node.resources.max_parallelism == 0
        || node.resources.max_parallelism > recipe.ceilings.max_parallelism
        || node.cancellation.acknowledgement_timeout_millis == 0
        || node.cancellation.acknowledgement_timeout_millis > node.resources.max_active_millis
        || node.authority.required_grant_refs.len() > MAX_AUTHORITY_REFS
        || node.authority.resource_scope_refs.len() > MAX_AUTHORITY_REFS
    {
        return Err(AppRecipeIrError::InvalidNodeCeiling(node_id.to_string()));
    }
    match &node.retry {
        AppRecipeRetryContract::None => {},
        AppRecipeRetryContract::Bounded {
            max_attempts,
            initial_backoff_millis,
            max_backoff_millis,
        } => {
            if !(2..=MAX_RETRY_ATTEMPTS).contains(max_attempts)
                || *initial_backoff_millis == 0
                || initial_backoff_millis > max_backoff_millis
                || *max_backoff_millis > node.resources.max_active_millis
            {
                return Err(AppRecipeIrError::InvalidRetryContract);
            }
        },
    }
    let invokes_owned_child = matches!(&node.node, AppRecipeNodeKind::StoreTransaction { sources, .. } if !sources.is_empty())
        || matches!(
            &node.node,
            AppRecipeNodeKind::CallTool
                | AppRecipeNodeKind::Reconcile { .. }
                | AppRecipeNodeKind::ContextualRound { .. }
                | AppRecipeNodeKind::InvokeAction
                | AppRecipeNodeKind::RunProcedure
                | AppRecipeNodeKind::AgentAsTool
        );
    if invokes_owned_child != (node.resources.max_tool_calls > 0)
        || (!matches!(
            &node.node,
            AppRecipeNodeKind::Parallel { .. } | AppRecipeNodeKind::ContextualRound { .. }
        ) && node.resources.max_parallelism != 1)
    {
        return Err(AppRecipeIrError::InvalidNodeCeiling(node_id.to_string()));
    }
    if node.provenance_join != AppRecipeProvenanceJoin::Preserve {
        return Err(AppRecipeIrError::UnsupportedProvenanceJoinV1);
    }
    if matches!(
        &node.node,
        AppRecipeNodeKind::Map { .. }
            | AppRecipeNodeKind::Validate
            | AppRecipeNodeKind::RunProcedure
            | AppRecipeNodeKind::AgentAsTool
            | AppRecipeNodeKind::EmitValue
            | AppRecipeNodeKind::EmitReceipt
            | AppRecipeNodeKind::MarkUncertain { .. }
    ) && node.output.authority != AppRecipeOutputAuthority::Derived
    {
        return Err(AppRecipeIrError::OutputAuthorityEscalation(
            node_id.to_string(),
        ));
    }
    if let AppRecipeNodeKind::Mutate { entities } = &node.node {
        if entities.is_empty() || entities.len() > MAX_RECORD_FIELDS {
            return Err(AppRecipeIrError::InvalidMutationTargets);
        }
    }
    validate_effect_contract(node_id, node)?;
    validate_authority_shape(node_id, node)?;
    match &node.node {
        AppRecipeNodeKind::StoreTransaction { sources, program } => {
            program
                .validate()
                .map_err(|_| AppRecipeIrError::InvalidMutationTargets)?;
            let mut result_source = super::reconciliation::result_schema_source();
            result_source.handling_floor = output.source.handling_floor.clone();
            let expected_output = compile_workflow_value_schema(result_source)?;
            let AppWorkflowValueTypeNode::Record {
                fields: input_fields,
            } = &input.source.nodes[usize::from(input.source.root)]
            else {
                return Err(AppRecipeIrError::InvalidMutationTargets);
            };
            if recipe.nodes.len() != 1
                || &recipe.root != node_id
                || node.output.kind != AppRecipeOutputKind::TypedValue
                || node.output.authority != AppRecipeOutputAuthority::Derived
                || expected_output.content_digest() != output.content_digest()
                || sources.len() > 64
                || program.source_parameters.iter().any(|(name, parameters)| {
                    sources
                        .iter()
                        .find(|(source_name, _)| source_name.as_str() == name)
                        .is_none_or(|(_, source)| {
                            parameters.keys().any(|parameter| {
                                !source
                                    .parameters
                                    .keys()
                                    .chain(source.input_parameters.keys())
                                    .any(|declared| declared.as_str() == parameter)
                            })
                        })
                })
                || node.resources.max_tool_calls as usize != sources.len()
                || sources.iter().any(|(name, source)| {
                    program
                        .queries
                        .iter()
                        .any(|query| query.name == name.as_str())
                        || matches!(name.as_str(), "participant" | "round")
                        || source
                            .input_parameters
                            .values()
                            .any(|field| !input_fields.contains_key(field))
                        || source
                            .when_input_present
                            .as_ref()
                            .is_some_and(|field| !input_fields.contains_key(field))
                        || !is_catalog_identity(&source.primitive_ref, "primitive:")
                        || !is_catalog_identity(&source.action_ref, "primitive-action:")
                        || !super::reconciliation::valid_source_parameters(
                            &source.parameters,
                            &source.input_parameters,
                        )
                })
            {
                return Err(AppRecipeIrError::InvalidMutationTargets);
            }
        },
        AppRecipeNodeKind::ContextualRound {
            source,
            cursor_parameter,
            max_source_pages,
            program,
        } => {
            program
                .validate()
                .map_err(|_| AppRecipeIrError::InvalidMutationTargets)?;
            let mut result_source = super::reconciliation::result_schema_source();
            result_source.handling_floor = output.source.handling_floor.clone();
            let expected_output = compile_workflow_value_schema(result_source)?;
            let AppWorkflowValueTypeNode::Record {
                fields: input_fields,
            } = &input.source.nodes[usize::from(input.source.root)]
            else {
                return Err(AppRecipeIrError::InvalidMutationTargets);
            };
            if recipe.nodes.len() != 1
                || &recipe.root != node_id
                || node.output.kind != AppRecipeOutputKind::TypedValue
                || node.output.authority != AppRecipeOutputAuthority::Derived
                || expected_output.content_digest() != output.content_digest()
                || *max_source_pages == 0
                || node.resources.max_tool_calls != *max_source_pages
                || node.resources.max_parallelism != program.limits.max_concurrent
                || source.when_input_present.is_some()
                || !matches!(
                    source.rows,
                    super::reconciliation::AppReconcileSourceRows::Page { .. }
                )
                || source
                    .input_parameters
                    .values()
                    .any(|field| !input_fields.contains_key(field))
                || source.primitive_ref
                    != *node
                        .authority
                        .primitive_ref
                        .as_ref()
                        .ok_or(AppRecipeIrError::InvalidMutationTargets)?
                || source.action_ref
                    != *node
                        .authority
                        .action_ref
                        .as_ref()
                        .ok_or(AppRecipeIrError::InvalidMutationTargets)?
                || !super::reconciliation::valid_source_parameters(
                    &source.parameters,
                    &source.input_parameters,
                )
                || !super::reconciliation::valid_source_parameters(
                    &BTreeMap::from([(cursor_parameter.clone(), serde_json::json!("cursor"))]),
                    &BTreeMap::new(),
                )
            {
                return Err(AppRecipeIrError::InvalidMutationTargets);
            }
            for name in program
                .mutation_entities()
                .into_iter()
                .chain(
                    program
                        .queries
                        .iter()
                        .flat_map(|query| [query.name.as_str(), query.entity.as_str()]),
                )
                .chain(std::iter::once(program.semantic_step.as_str()))
            {
                AppName::parse(name).map_err(|_| AppRecipeIrError::InvalidMutationTargets)?;
            }
        },
        AppRecipeNodeKind::Reconcile { declaration } => {
            declaration
                .validate()
                .map_err(|_| AppRecipeIrError::InvalidMutationTargets)?;
            let mut result_source = super::reconciliation::result_schema_source();
            result_source.handling_floor = output.source.handling_floor.clone();
            let expected_output = compile_workflow_value_schema(result_source)?;
            let AppWorkflowValueTypeNode::Record {
                fields: input_fields,
            } = &input.source.nodes[usize::from(input.source.root)]
            else {
                return Err(AppRecipeIrError::InvalidMutationTargets);
            };
            if recipe.nodes.len() != 1
                || &recipe.root != node_id
                || node.output.kind != AppRecipeOutputKind::TypedValue
                || node.resources.max_tool_calls != 1 + declaration.sources.len() as u16
                || node.output.authority != AppRecipeOutputAuthority::Derived
                || expected_output.content_digest() != output.content_digest()
                || declaration
                    .input_parameters
                    .values()
                    .any(|field| !input_fields.contains_key(field))
                || declaration.sources.values().any(|source| {
                    source
                        .input_parameters
                        .values()
                        .any(|field| !input_fields.contains_key(field))
                        || !is_catalog_identity(&source.primitive_ref, "primitive:")
                        || !is_catalog_identity(&source.action_ref, "primitive-action:")
                })
            {
                return Err(AppRecipeIrError::InvalidMutationTargets);
            }
        },
        AppRecipeNodeKind::Deferred { node } => {
            return Err(AppRecipeIrError::UnsupportedNode(*node));
        },
        AppRecipeNodeKind::Query { .. }
            if node.output.kind != AppRecipeOutputKind::EntityProjection =>
        {
            return Err(AppRecipeIrError::OutputKindMismatch);
        },
        AppRecipeNodeKind::Get { entity } => {
            let input_projection = entity_projection_contract(input);
            let output_projection = entity_projection_contract(output);
            if node.output.kind != AppRecipeOutputKind::EntityProjection
                || node.input_schema_ref != node.output.schema_ref
                || input_projection != output_projection
                || input_projection.is_none_or(|(declared, _)| declared != entity)
            {
                return Err(AppRecipeIrError::GetProjectionContract);
            }
        },
        AppRecipeNodeKind::Map {
            mapping_digest,
            operations,
        } => {
            if node.output.kind != AppRecipeOutputKind::TypedValue {
                return Err(AppRecipeIrError::OutputKindMismatch);
            }
            let mapping = compile_recipe_value_mapping(input, output, operations.clone())?;
            if mapping.mapping_digest() != mapping_digest {
                return Err(AppRecipeIrError::MappingDigestSubstitution);
            }
        },
        AppRecipeNodeKind::Validate | AppRecipeNodeKind::EmitValue
            if node.output.kind != AppRecipeOutputKind::TypedValue
                || node.input_schema_ref != node.output.schema_ref =>
        {
            return Err(AppRecipeIrError::SchemaSubstitution);
        },
        AppRecipeNodeKind::Mutate { .. } if node.output.kind != AppRecipeOutputKind::Receipt => {
            return Err(AppRecipeIrError::OutputKindMismatch);
        },
        AppRecipeNodeKind::EmitReceipt
            if node.output.kind != AppRecipeOutputKind::Receipt
                || node.input_schema_ref != node.output.schema_ref =>
        {
            return Err(AppRecipeIrError::SchemaSubstitution);
        },
        AppRecipeNodeKind::EmitArtifact if node.output.kind != AppRecipeOutputKind::Artifact => {
            return Err(AppRecipeIrError::OutputKindMismatch);
        },
        AppRecipeNodeKind::Sequence { steps } => {
            if steps.is_empty() {
                return Err(AppRecipeIrError::RecipeFanOut);
            }
            let first = recipe_node(recipe, &steps[0])?;
            if first.input_schema_ref != node.input_schema_ref {
                return Err(AppRecipeIrError::SchemaSubstitution);
            }
            let mut previous = &first.output;
            for child_id in &steps[1..] {
                let child = recipe_node(recipe, child_id)?;
                if child.input_schema_ref != previous.schema_ref {
                    return Err(AppRecipeIrError::SchemaSubstitution);
                }
                previous = &child.output;
            }
            if previous != &node.output {
                return Err(AppRecipeIrError::SchemaSubstitution);
            }
        },
        AppRecipeNodeKind::Parallel { branches } => {
            if branches.len() < 2 || branches.len() > usize::from(node.resources.max_parallelism) {
                return Err(AppRecipeIrError::RecipeFanOut);
            }
            let AppWorkflowValueTypeNode::Array {
                items,
                min_items,
                max_items,
            } = output.root_node()
            else {
                return Err(AppRecipeIrError::ParallelOutputSchema);
            };
            if usize::from(*min_items) != branches.len()
                || usize::from(*max_items) != branches.len()
            {
                return Err(AppRecipeIrError::ParallelOutputSchema);
            }
            for child_id in branches.values() {
                let child = recipe_node(recipe, child_id)?;
                if child.input_schema_ref != node.input_schema_ref {
                    return Err(AppRecipeIrError::SchemaSubstitution);
                }
                if output_authority_rank(node.output.authority)
                    > output_authority_rank(child.output.authority)
                {
                    return Err(AppRecipeIrError::OutputAuthorityEscalation(
                        node_id.to_string(),
                    ));
                }
                let child_output = schema_by_ref(schemas, &child.output.schema_ref)?;
                if !schema_floor_dominates(output, child_output)
                    || !schema_nodes_equivalent(
                        output,
                        *items,
                        child_output,
                        child_output.source.root,
                    )
                {
                    return Err(AppRecipeIrError::ParallelOutputSchema);
                }
            }
        },
        AppRecipeNodeKind::Switch {
            discriminator,
            cases,
        } => {
            let AppWorkflowValueTypeNode::TaggedUnion {
                discriminator: schema_discriminator,
                variants,
            } = input.root_node()
            else {
                return Err(AppRecipeIrError::SwitchInputSchema);
            };
            if schema_discriminator != discriminator || cases.keys().ne(variants.keys()) {
                return Err(AppRecipeIrError::SwitchInputSchema);
            }
            for (tag, child_id) in cases {
                let child = recipe_node(recipe, child_id)?;
                let child_input = schema_by_ref(schemas, &child.input_schema_ref)?;
                let variant = variants
                    .get(tag)
                    .ok_or(AppRecipeIrError::SwitchInputSchema)?;
                if !schema_nodes_equivalent(input, *variant, child_input, child_input.source.root) {
                    return Err(AppRecipeIrError::SwitchInputSchema);
                }
                let child_output = schema_by_ref(schemas, &child.output.schema_ref)?;
                if child.output.kind != node.output.kind
                    || child.output.authority != node.output.authority
                    || !schema_floor_dominates(output, child_output)
                    || !schema_nodes_equivalent(
                        output,
                        output.source.root,
                        child_output,
                        child_output.source.root,
                    )
                {
                    return Err(AppRecipeIrError::SchemaSubstitution);
                }
            }
        },
        AppRecipeNodeKind::Retry { child } => {
            let child = recipe_node(recipe, child)?;
            if child.input_schema_ref != node.input_schema_ref || child.output != node.output {
                return Err(AppRecipeIrError::SchemaSubstitution);
            }
            if !matches!(&node.retry, AppRecipeRetryContract::Bounded { .. }) {
                return Err(AppRecipeIrError::InvalidRetryContract);
            }
            if child.effect.class > AppRecipeEffectClass::ReadOnly {
                return Err(AppRecipeIrError::EffectfulRetryUnsupportedInV1);
            }
        },
        AppRecipeNodeKind::MarkUncertain { child } => {
            let child = recipe_node(recipe, child)?;
            if child.input_schema_ref != node.input_schema_ref
                || node.output.kind != AppRecipeOutputKind::TypedValue
            {
                return Err(AppRecipeIrError::UncertainOutputSchema);
            }
            let AppWorkflowValueTypeNode::TaggedUnion {
                discriminator,
                variants,
            } = output.root_node()
            else {
                return Err(AppRecipeIrError::UncertainOutputSchema);
            };
            let completed = AppName::parse("completed")?;
            let uncertain = AppName::parse("uncertain")?;
            if discriminator.as_str() != "status"
                || variants.len() != 2
                || !variants.contains_key(&completed)
                || !variants.contains_key(&uncertain)
            {
                return Err(AppRecipeIrError::UncertainOutputSchema);
            }
            let child_output = schema_by_ref(schemas, &child.output.schema_ref)?;
            if !schema_floor_dominates(output, child_output)
                || !schema_nodes_equivalent(
                    output,
                    variants[&completed],
                    child_output,
                    child_output.source.root,
                )
                || !matches!(
                    &output.source.nodes[usize::from(variants[&uncertain])],
                    AppWorkflowValueTypeNode::ReceiptRef {
                        receipt_kind: AppWorkflowReceiptKind::ExternalEffect
                    }
                )
            {
                return Err(AppRecipeIrError::UncertainOutputSchema);
            }
        },
        _ => {
            if !matches!(&node.retry, AppRecipeRetryContract::None) {
                return Err(AppRecipeIrError::InvalidRetryContract);
            }
        },
    }
    Ok(())
}

/// Compile a Recipe mapping against the exact workflow schema identities.
/// Recipe v1 intentionally admits only flat records of closed scalar leaves;
/// no expression evaluator, object callback, resource dereference, array walk
/// or recursive transform can enter this owner.
pub(crate) fn compile_recipe_value_mapping(
    source: &AppCompiledWorkflowValueSchema,
    target: &AppCompiledWorkflowValueSchema,
    operations: Vec<AppValueMappingOperation>,
) -> Result<AppCompiledValueMapping, AppRecipeIrError> {
    validate_recipe_mapping_total(source, target, &operations)?;
    let source_contract = workflow_value_mapping_schema(source)?;
    let target_contract = workflow_value_mapping_schema(target)?;
    compile_value_mapping(&source_contract, &target_contract, operations).map_err(Into::into)
}

fn validate_recipe_mapping_total(
    source: &AppCompiledWorkflowValueSchema,
    target: &AppCompiledWorkflowValueSchema,
    operations: &[AppValueMappingOperation],
) -> Result<(), AppRecipeIrError> {
    for operation in operations {
        match operation {
            AppValueMappingOperation::Select {
                source: source_path,
                target: target_path,
            } => {
                let source_leaf = recipe_mapping_leaf(source, source_path)?;
                let target_leaf = recipe_mapping_leaf(target, target_path)?;
                if matches!(
                    (source_leaf, target_leaf),
                    (
                        AppWorkflowValueTypeNode::Text {
                            max_bytes: source_max
                        },
                        AppWorkflowValueTypeNode::Text {
                            max_bytes: target_max
                        }
                    ) if source_max > target_max
                ) || matches!(
                    (source_leaf, target_leaf),
                    (
                        AppWorkflowValueTypeNode::Markdown {
                            max_bytes: source_max
                        },
                        AppWorkflowValueTypeNode::Markdown {
                            max_bytes: target_max
                        }
                    ) if source_max > target_max
                ) {
                    return Err(AppRecipeIrError::NonTotalMapping);
                }
            },
            AppValueMappingOperation::Convert { conversion, .. }
                if conversion != &AppRegisteredScalarConversion::IntegerToDecimal =>
            {
                // Text-to-timestamp parsing and text-to-markdown constraint
                // changes are deterministic but partial; Recipe Map admits
                // only transformations that are total over the source schema.
                return Err(AppRecipeIrError::NonTotalMapping);
            },
            AppValueMappingOperation::Constant {
                target: target_path,
                value,
            } => {
                let target_leaf = recipe_mapping_leaf(target, target_path)?;
                let violates_target = match target_leaf {
                    AppWorkflowValueTypeNode::Integer => {
                        !value.is_null() && value.as_i64().is_none()
                    },
                    AppWorkflowValueTypeNode::Text { max_bytes } => {
                        value.as_str().is_some_and(|value| {
                            u64::try_from(value.len()).unwrap_or(u64::MAX) > u64::from(*max_bytes)
                        })
                    },
                    AppWorkflowValueTypeNode::Markdown { max_bytes } => {
                        value.as_str().is_some_and(|value| {
                            u64::try_from(value.len()).unwrap_or(u64::MAX) > u64::from(*max_bytes)
                        })
                    },
                    AppWorkflowValueTypeNode::EntityReference { .. } => value
                        .as_str()
                        .is_some_and(|value| value.is_empty() || value.len() > 256),
                    _ => false,
                };
                if violates_target {
                    return Err(AppRecipeIrError::NonTotalMapping);
                }
            },
            AppValueMappingOperation::MapEnum {
                source: source_path,
                target: target_path,
                values,
            } => {
                let AppWorkflowValueTypeNode::Enum {
                    values: source_values,
                } = recipe_mapping_leaf(source, source_path)?
                else {
                    return Err(AppRecipeIrError::UnsupportedMappingSchema);
                };
                let AppWorkflowValueTypeNode::Enum {
                    values: target_values,
                } = recipe_mapping_leaf(target, target_path)?
                else {
                    return Err(AppRecipeIrError::UnsupportedMappingSchema);
                };
                if values.keys().collect::<BTreeSet<_>>()
                    != source_values.iter().collect::<BTreeSet<_>>()
                    || values.values().any(|value| !target_values.contains(value))
                {
                    return Err(AppRecipeIrError::NonTotalMapping);
                }
            },
            AppValueMappingOperation::Convert { .. } => {},
        }
    }
    Ok(())
}

fn recipe_mapping_leaf<'a>(
    schema: &'a AppCompiledWorkflowValueSchema,
    path: &AppFieldPath,
) -> Result<&'a AppWorkflowValueTypeNode, AppRecipeIrError> {
    let AppWorkflowValueTypeNode::Record { fields } = schema.root_node() else {
        return Err(AppRecipeIrError::UnsupportedMappingSchema);
    };
    let name = AppName::parse(path.as_str().to_owned())?;
    let field = fields
        .get(&name)
        .ok_or(AppRecipeIrError::UnsupportedMappingSchema)?;
    let mut leaf = schema
        .source
        .nodes
        .get(usize::from(field.value_type))
        .ok_or(AppRecipeIrError::InvalidArenaReference)?;
    if let AppWorkflowValueTypeNode::Nullable { value_type } = leaf {
        leaf = schema
            .source
            .nodes
            .get(usize::from(*value_type))
            .ok_or(AppRecipeIrError::InvalidArenaReference)?;
    }
    Ok(leaf)
}

/// Execute only an already digest-verified Recipe mapping and revalidate the
/// result through the destination workflow schema while preserving the exact
/// input labels and provenance. Mapping never receives resource descriptors.
pub(crate) fn apply_recipe_value_mapping(
    source_schema: &AppCompiledWorkflowValueSchema,
    target_schema: &AppCompiledWorkflowValueSchema,
    expected_digest: &AppDigest,
    operations: Vec<AppValueMappingOperation>,
    input: &AppValidatedWorkflowValue,
) -> Result<AppValidatedWorkflowValue, AppRecipeIrError> {
    if !input.source.resources.is_empty() {
        return Err(AppRecipeIrError::MappingResourceInput);
    }
    validate_recipe_mapping_total(source_schema, target_schema, &operations)?;
    let source_contract = workflow_value_mapping_schema(source_schema)?;
    let target_contract = workflow_value_mapping_schema(target_schema)?;
    let mapping = compile_value_mapping(&source_contract, &target_contract, operations)?;
    if mapping.mapping_digest() != expected_digest {
        return Err(AppRecipeIrError::MappingDigestSubstitution);
    }
    let json = workflow_value_to_json(source_schema, input)?;
    let mapped = apply_compiled_value_mapping(&mapping, &source_contract, &target_contract, &json)?;
    validate_json_workflow_value(
        target_schema,
        mapped,
        input.source.handling_labels.clone(),
        input.source.provenance.clone(),
    )
}

pub(crate) fn workflow_value_mapping_schema(
    schema: &AppCompiledWorkflowValueSchema,
) -> Result<AppValueSchemaContract, AppRecipeIrError> {
    let AppWorkflowValueTypeNode::Record { fields } = schema.root_node() else {
        return Err(AppRecipeIrError::UnsupportedMappingSchema);
    };
    let mut compiled_fields = BTreeMap::new();
    for (name, field) in fields {
        let mut node = schema
            .source
            .nodes
            .get(usize::from(field.value_type))
            .ok_or(AppRecipeIrError::InvalidArenaReference)?;
        let nullable = if let AppWorkflowValueTypeNode::Nullable { value_type } = node {
            node = schema
                .source
                .nodes
                .get(usize::from(*value_type))
                .ok_or(AppRecipeIrError::InvalidArenaReference)?;
            true
        } else {
            false
        };
        let (kind, enum_values) = match node {
            AppWorkflowValueTypeNode::Boolean => (AppQueryScalarKind::Boolean, None),
            AppWorkflowValueTypeNode::Integer => (AppQueryScalarKind::Integer, None),
            AppWorkflowValueTypeNode::Decimal => (AppQueryScalarKind::Decimal, None),
            AppWorkflowValueTypeNode::Text { .. } => (AppQueryScalarKind::Text, None),
            AppWorkflowValueTypeNode::Markdown { .. } => (AppQueryScalarKind::Markdown, None),
            AppWorkflowValueTypeNode::EntityReference { .. } => {
                (AppQueryScalarKind::Reference, None)
            },
            AppWorkflowValueTypeNode::Enum { values } => {
                (AppQueryScalarKind::Enum, Some(values.clone()))
            },
            AppWorkflowValueTypeNode::Timestamp => (AppQueryScalarKind::Timestamp, None),
            _ => return Err(AppRecipeIrError::UnsupportedMappingSchema),
        };
        let path = AppFieldPath::parse(name.as_str().to_owned())?;
        compiled_fields.insert(
            path,
            match enum_values {
                Some(values) => {
                    AppValueFieldContract::from_recipe_enum(values, field.required, nullable)
                },
                None => AppValueFieldContract::from_recipe_scalar(kind, field.required, nullable),
            },
        );
    }
    AppValueSchemaContract::from_exact_compiled_fields(
        schema.schema_ref().clone(),
        schema.content_digest().clone(),
        compiled_fields,
    )
    .map_err(Into::into)
}

fn validate_effect_contract(
    node_id: &AppName,
    node: &AppRecipeNode,
) -> Result<(), AppRecipeIrError> {
    let fixed = match &node.node {
        AppRecipeNodeKind::Query { .. } | AppRecipeNodeKind::Get { .. } => {
            Some(AppRecipeEffectClass::ReadOnly)
        },
        AppRecipeNodeKind::Map { .. }
        | AppRecipeNodeKind::Validate
        | AppRecipeNodeKind::Sequence { .. }
        | AppRecipeNodeKind::Parallel { .. }
        | AppRecipeNodeKind::Switch { .. }
        | AppRecipeNodeKind::EmitValue
        | AppRecipeNodeKind::EmitReceipt
        | AppRecipeNodeKind::Retry { .. }
        | AppRecipeNodeKind::MarkUncertain { .. }
        | AppRecipeNodeKind::Deferred { .. } => Some(AppRecipeEffectClass::None),
        AppRecipeNodeKind::Mutate { .. }
        | AppRecipeNodeKind::EmitArtifact
        | AppRecipeNodeKind::Reconcile { .. }
        | AppRecipeNodeKind::StoreTransaction { .. }
        | AppRecipeNodeKind::ContextualRound { .. } => Some(AppRecipeEffectClass::InternalMutation),
        AppRecipeNodeKind::RunProcedure | AppRecipeNodeKind::AgentAsTool => {
            Some(AppRecipeEffectClass::Reasoning)
        },
        AppRecipeNodeKind::CallTool | AppRecipeNodeKind::InvokeAction => None,
    };
    if fixed.is_some_and(|fixed| fixed != node.effect.class) {
        return Err(AppRecipeIrError::EffectClassMismatch(node_id.to_string()));
    }
    match node.effect.class {
        AppRecipeEffectClass::None => {
            if !matches!(
                &node.effect.idempotency,
                AppRecipeIdempotencyContract::NotApplicable
            ) || node.effect.uncertainty != AppRecipeUncertaintyContract::Impossible
            {
                return Err(AppRecipeIrError::InvalidEffectContract(node_id.to_string()));
            }
        },
        AppRecipeEffectClass::ReadOnly => {
            if !matches!(
                &node.effect.idempotency,
                AppRecipeIdempotencyContract::Intrinsic
            ) || node.effect.uncertainty != AppRecipeUncertaintyContract::Impossible
            {
                return Err(AppRecipeIrError::InvalidEffectContract(node_id.to_string()));
            }
        },
        AppRecipeEffectClass::Reasoning
        | AppRecipeEffectClass::InternalMutation
        | AppRecipeEffectClass::ExternalEffect => {
            if matches!(
                &node.effect.idempotency,
                AppRecipeIdempotencyContract::NotApplicable
            ) || node.effect.uncertainty != AppRecipeUncertaintyContract::ReceiptBound
            {
                return Err(AppRecipeIrError::InvalidEffectContract(node_id.to_string()));
            }
        },
    }
    Ok(())
}

fn validate_authority_shape(
    node_id: &AppName,
    node: &AppRecipeNode,
) -> Result<(), AppRecipeIrError> {
    let authority = &node.authority;
    let valid = match &node.node {
        AppRecipeNodeKind::CallTool
        | AppRecipeNodeKind::Reconcile { .. }
        | AppRecipeNodeKind::ContextualRound { .. } => {
            authority.primitive_ref.is_some()
                && authority.action_ref.is_some()
                && authority.target_app_ref.is_none()
        },
        AppRecipeNodeKind::RunProcedure | AppRecipeNodeKind::AgentAsTool => {
            authority.primitive_ref.is_some()
                && authority.action_ref.is_none()
                && authority.target_app_ref.is_none()
        },
        AppRecipeNodeKind::InvokeAction => {
            authority.primitive_ref.is_none()
                && authority.target_app_ref.is_some()
                && authority.action_ref.is_some()
        },
        AppRecipeNodeKind::Query { .. }
        | AppRecipeNodeKind::Get { .. }
        | AppRecipeNodeKind::StoreTransaction { .. }
        | AppRecipeNodeKind::Mutate { .. } => {
            authority.primitive_ref.is_none()
                && authority.action_ref.is_none()
                && authority.target_app_ref.is_none()
                && !authority.required_grant_refs.is_empty()
        },
        _ => {
            authority.primitive_ref.is_none()
                && authority.action_ref.is_none()
                && authority.target_app_ref.is_none()
                && authority.required_grant_refs.is_empty()
                && authority.resource_scope_refs.is_empty()
        },
    };
    if !valid {
        return Err(AppRecipeIrError::AuthorityShapeMismatch(
            node_id.to_string(),
        ));
    }
    let refs_valid = authority
        .required_grant_refs
        .iter()
        .all(|reference| is_opaque_logical_ref(reference, "grant:"))
        && authority
            .resource_scope_refs
            .iter()
            .all(|reference| is_opaque_logical_ref(reference, "resource-scope:"))
        && match &node.node {
            AppRecipeNodeKind::CallTool
            | AppRecipeNodeKind::Reconcile { .. }
            | AppRecipeNodeKind::ContextualRound { .. } => {
                authority
                    .primitive_ref
                    .as_ref()
                    .is_some_and(|reference| is_catalog_identity(reference, "primitive:"))
                    && authority.action_ref.as_ref().is_some_and(|reference| {
                        is_catalog_identity(reference, "primitive-action:")
                    })
            },
            AppRecipeNodeKind::RunProcedure | AppRecipeNodeKind::AgentAsTool => authority
                .primitive_ref
                .as_ref()
                .is_some_and(|reference| is_catalog_identity(reference, "primitive:")),
            AppRecipeNodeKind::InvokeAction => {
                authority.target_app_ref.as_ref().is_some_and(|reference| {
                    is_opaque_logical_ref(reference, "package:")
                        || is_opaque_logical_ref(reference, "package-revision:")
                }) && authority
                    .action_ref
                    .as_ref()
                    .is_some_and(|reference| is_named_logical_ref(reference, "app-action:"))
            },
            _ => true,
        };
    if !refs_valid {
        return Err(AppRecipeIrError::InvalidAuthorityReference(
            node_id.to_string(),
        ));
    }
    Ok(())
}

fn output_authority_rank(authority: AppRecipeOutputAuthority) -> u8 {
    match authority {
        AppRecipeOutputAuthority::FeedbackOnly => 0,
        AppRecipeOutputAuthority::Derived => 1,
        AppRecipeOutputAuthority::Authoritative => 2,
    }
}

fn schema_floor_dominates(
    output: &AppCompiledWorkflowValueSchema,
    input: &AppCompiledWorkflowValueSchema,
) -> bool {
    output.source.handling_floor.classification >= input.source.handling_floor.classification
        && output.source.handling_floor.model_processing
            <= input.source.handling_floor.model_processing
}

fn validate_output_kind(
    output: &AppRecipeOutputContract,
    schema: &AppCompiledWorkflowValueSchema,
) -> Result<(), AppRecipeIrError> {
    let matches = match output.kind {
        AppRecipeOutputKind::Unit => matches!(schema.root_node(), AppWorkflowValueTypeNode::Unit),
        AppRecipeOutputKind::TypedValue => !matches!(
            schema.root_node(),
            AppWorkflowValueTypeNode::Unit
                | AppWorkflowValueTypeNode::EntityProjectionRef { .. }
                | AppWorkflowValueTypeNode::ArtifactRef { .. }
                | AppWorkflowValueTypeNode::ReceiptRef { .. }
                | AppWorkflowValueTypeNode::ResourceRef { .. }
        ),
        AppRecipeOutputKind::EntityProjection => matches!(
            schema.root_node(),
            AppWorkflowValueTypeNode::EntityProjectionRef { .. }
        ),
        AppRecipeOutputKind::Artifact => {
            matches!(
                schema.root_node(),
                AppWorkflowValueTypeNode::ArtifactRef { .. }
            )
        },
        AppRecipeOutputKind::Receipt => {
            matches!(
                schema.root_node(),
                AppWorkflowValueTypeNode::ReceiptRef { .. }
            )
        },
    };
    if matches {
        Ok(())
    } else {
        Err(AppRecipeIrError::OutputKindMismatch)
    }
}

fn schema_by_ref<'a>(
    schemas: &'a BTreeMap<AppReference, &'a AppCompiledWorkflowValueSchema>,
    schema_ref: &AppReference,
) -> Result<&'a AppCompiledWorkflowValueSchema, AppRecipeIrError> {
    schemas
        .get(schema_ref)
        .copied()
        .ok_or_else(|| AppRecipeIrError::UnknownSchemaRef(schema_ref.to_string()))
}

fn recipe_node<'a>(
    recipe: &'a AppRecipeIrSource,
    node: &AppName,
) -> Result<&'a AppRecipeNode, AppRecipeIrError> {
    recipe
        .nodes
        .get(node)
        .ok_or_else(|| AppRecipeIrError::UnknownRecipeNode(node.to_string()))
}

fn recipe_children(node: &AppRecipeNodeKind) -> Vec<AppName> {
    match node {
        AppRecipeNodeKind::Sequence { steps } => steps.clone(),
        AppRecipeNodeKind::Parallel { branches } => branches.values().cloned().collect(),
        AppRecipeNodeKind::Switch { cases, .. } => cases.values().cloned().collect(),
        AppRecipeNodeKind::Retry { child } | AppRecipeNodeKind::MarkUncertain { child } => {
            vec![child.clone()]
        },
        _ => Vec::new(),
    }
}

fn schema_nodes_equivalent(
    left: &AppCompiledWorkflowValueSchema,
    left_root: u16,
    right: &AppCompiledWorkflowValueSchema,
    right_root: u16,
) -> bool {
    let mut stack = vec![(left_root, right_root)];
    let mut seen = BTreeSet::new();
    while let Some((left_index, right_index)) = stack.pop() {
        if !seen.insert((left_index, right_index)) {
            continue;
        }
        let Some(left_node) = left.source.nodes.get(usize::from(left_index)) else {
            return false;
        };
        let Some(right_node) = right.source.nodes.get(usize::from(right_index)) else {
            return false;
        };
        match (left_node, right_node) {
            (AppWorkflowValueTypeNode::Unit, AppWorkflowValueTypeNode::Unit)
            | (AppWorkflowValueTypeNode::Boolean, AppWorkflowValueTypeNode::Boolean)
            | (AppWorkflowValueTypeNode::Integer, AppWorkflowValueTypeNode::Integer)
            | (AppWorkflowValueTypeNode::Decimal, AppWorkflowValueTypeNode::Decimal)
            | (AppWorkflowValueTypeNode::Timestamp, AppWorkflowValueTypeNode::Timestamp)
            | (
                AppWorkflowValueTypeNode::OpaqueReference,
                AppWorkflowValueTypeNode::OpaqueReference,
            ) => {},
            (
                AppWorkflowValueTypeNode::Enum { values: left },
                AppWorkflowValueTypeNode::Enum { values: right },
            ) if left == right => {},
            (
                AppWorkflowValueTypeNode::ResourceRef {
                    resource_kind: left,
                },
                AppWorkflowValueTypeNode::ResourceRef {
                    resource_kind: right,
                },
            ) if left == right => {},
            (
                AppWorkflowValueTypeNode::Text { max_bytes: left },
                AppWorkflowValueTypeNode::Text { max_bytes: right },
            ) if left == right => {},
            (
                AppWorkflowValueTypeNode::Markdown { max_bytes: left },
                AppWorkflowValueTypeNode::Markdown { max_bytes: right },
            ) if left == right => {},
            (
                AppWorkflowValueTypeNode::EntityReference { entity: left },
                AppWorkflowValueTypeNode::EntityReference { entity: right },
            ) if left == right => {},
            (
                AppWorkflowValueTypeNode::EntityProjectionRef {
                    entity: left_entity,
                    value_schema_ref: left_schema,
                },
                AppWorkflowValueTypeNode::EntityProjectionRef {
                    entity: right_entity,
                    value_schema_ref: right_schema,
                },
            ) if left_entity == right_entity && left_schema == right_schema => {},
            (
                AppWorkflowValueTypeNode::ArtifactRef {
                    value_schema_ref: left_schema,
                    max_bytes: left_bytes,
                    media_types: left_media,
                },
                AppWorkflowValueTypeNode::ArtifactRef {
                    value_schema_ref: right_schema,
                    max_bytes: right_bytes,
                    media_types: right_media,
                },
            ) if left_schema == right_schema
                && left_bytes == right_bytes
                && left_media == right_media => {},
            (
                AppWorkflowValueTypeNode::ReceiptRef { receipt_kind: left },
                AppWorkflowValueTypeNode::ReceiptRef {
                    receipt_kind: right,
                },
            ) if left == right => {},
            (
                AppWorkflowValueTypeNode::Nullable { value_type: left },
                AppWorkflowValueTypeNode::Nullable { value_type: right },
            ) => stack.push((*left, *right)),
            (
                AppWorkflowValueTypeNode::Array {
                    items: left_items,
                    min_items: left_min,
                    max_items: left_max,
                },
                AppWorkflowValueTypeNode::Array {
                    items: right_items,
                    min_items: right_min,
                    max_items: right_max,
                },
            ) if left_min == right_min && left_max == right_max => {
                stack.push((*left_items, *right_items));
            },
            (
                AppWorkflowValueTypeNode::Record { fields: left },
                AppWorkflowValueTypeNode::Record { fields: right },
            ) if left.keys().eq(right.keys()) => {
                for (name, left_field) in left {
                    let Some(right_field) = right.get(name) else {
                        return false;
                    };
                    if left_field.required != right_field.required {
                        return false;
                    }
                    stack.push((left_field.value_type, right_field.value_type));
                }
            },
            (
                AppWorkflowValueTypeNode::TaggedUnion {
                    discriminator: left_discriminator,
                    variants: left,
                },
                AppWorkflowValueTypeNode::TaggedUnion {
                    discriminator: right_discriminator,
                    variants: right,
                },
            ) if left_discriminator == right_discriminator && left.keys().eq(right.keys()) => {
                for (name, left_variant) in left {
                    let Some(right_variant) = right.get(name) else {
                        return false;
                    };
                    stack.push((*left_variant, *right_variant));
                }
            },
            _ => return false,
        }
    }
    true
}

fn validate_index_arena<F>(
    domain: &'static str,
    root: u16,
    node_count: usize,
    max_nodes: usize,
    max_edges: usize,
    max_depth: usize,
    allow_shared: bool,
    children: F,
) -> Result<(), AppRecipeIrError>
where
    F: Fn(usize) -> Vec<u16>,
{
    if node_count == 0 || node_count > max_nodes || usize::from(root) >= node_count {
        return Err(AppRecipeIrError::ArenaNodeLimit {
            domain,
            limit: max_nodes,
        });
    }
    let mut state = vec![0u8; node_count];
    let mut indegree = vec![0usize; node_count];
    let mut postorder = Vec::with_capacity(node_count);
    let mut edge_count = 0usize;
    let mut stack = vec![(usize::from(root), false)];
    while let Some((index, exiting)) = stack.pop() {
        if exiting {
            state[index] = 2;
            postorder.push(index);
            continue;
        }
        match state[index] {
            1 => return Err(AppRecipeIrError::ArenaCycle { domain }),
            2 => continue,
            _ => {},
        }
        state[index] = 1;
        stack.push((index, true));
        let node_children = children(index);
        edge_count = edge_count.saturating_add(node_children.len());
        if edge_count > max_edges {
            return Err(AppRecipeIrError::ArenaEdgeLimit {
                domain,
                limit: max_edges,
            });
        }
        for child in node_children.into_iter().rev() {
            let child = usize::from(child);
            if child >= node_count {
                return Err(AppRecipeIrError::InvalidArenaReference);
            }
            indegree[child] = indegree[child].saturating_add(1);
            stack.push((child, false));
        }
    }
    if state.iter().any(|state| *state == 0) {
        return Err(AppRecipeIrError::UnreachableArenaNode { domain });
    }
    if !allow_shared
        && indegree
            .iter()
            .enumerate()
            .any(|(index, degree)| index != usize::from(root) && *degree != 1)
    {
        return Err(AppRecipeIrError::SharedValueNode);
    }
    postorder.reverse();
    let mut depth = vec![0usize; node_count];
    depth[usize::from(root)] = 1;
    for index in postorder {
        if depth[index] > max_depth {
            return Err(AppRecipeIrError::ArenaDepth {
                domain,
                limit: max_depth,
            });
        }
        for child in children(index) {
            let child = usize::from(child);
            depth[child] = depth[child].max(depth[index].saturating_add(1));
        }
    }
    Ok(())
}

fn canonical_index_order<F>(root: u16, node_count: usize, children: F) -> Vec<usize>
where
    F: Fn(usize) -> Vec<u16>,
{
    let mut seen = vec![false; node_count];
    let mut order = Vec::with_capacity(node_count);
    let mut stack = vec![usize::from(root)];
    while let Some(index) = stack.pop() {
        if seen[index] {
            continue;
        }
        seen[index] = true;
        order.push(index);
        let node_children = children(index);
        for child in node_children.into_iter().rev() {
            stack.push(usize::from(child));
        }
    }
    order
}

fn index_remap(order: &[usize], node_count: usize) -> Result<Vec<u16>, AppRecipeIrError> {
    let mut remap = vec![u16::MAX; node_count];
    for (new, old) in order.iter().copied().enumerate() {
        remap[old] = u16::try_from(new).map_err(|_| AppRecipeIrError::InvalidArenaReference)?;
    }
    if remap.iter().any(|index| *index == u16::MAX) {
        return Err(AppRecipeIrError::UnreachableArenaNode {
            domain: "normalization",
        });
    }
    Ok(remap)
}

fn normalize_decimal(raw: &str) -> Result<String, AppRecipeIrError> {
    if raw.is_empty()
        || raw.len() > 64
        || raw.starts_with('+')
        || raw.bytes().any(|byte| matches!(byte, b'e' | b'E'))
    {
        return Err(AppRecipeIrError::InvalidDecimal);
    }
    let (negative, unsigned) = raw
        .strip_prefix('-')
        .map_or((false, raw), |value| (true, value));
    let mut pieces = unsigned.split('.');
    let whole = pieces.next().unwrap_or_default();
    let fraction = pieces.next();
    if pieces.next().is_some()
        || whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.is_some_and(|fraction| {
            fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return Err(AppRecipeIrError::InvalidDecimal);
    }
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let fraction = fraction.unwrap_or_default().trim_end_matches('0');
    let is_zero = whole == "0" && fraction.is_empty();
    let mut normalized = String::with_capacity(raw.len());
    if negative && !is_zero {
        normalized.push('-');
    }
    normalized.push_str(whole);
    if !fraction.is_empty() {
        normalized.push('.');
        normalized.push_str(fraction);
    }
    Ok(normalized)
}

fn valid_media_type(value: &str) -> bool {
    if value.is_empty()
        || value.len() > MAX_MEDIA_TYPE_BYTES
        || value.contains("//")
        || value.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return false;
    }
    let Some((kind, subtype)) = value.split_once('/') else {
        return false;
    };
    !kind.is_empty()
        && !subtype.is_empty()
        && kind.bytes().chain(subtype.bytes()).all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.')
        })
}

fn is_opaque_logical_ref(reference: &AppReference, prefix: &str) -> bool {
    let value = reference.as_str();
    value.strip_prefix(prefix).is_some_and(|rest| {
        let forbidden_segment = rest.split(':').any(|segment| {
            matches!(
                segment.to_ascii_lowercase().as_str(),
                "http"
                    | "https"
                    | "task"
                    | "execution"
                    | "token"
                    | "bearer"
                    | "secret"
                    | "credential"
            )
        });
        !rest.is_empty()
            && !rest.contains('/')
            && !rest.contains('@')
            && !rest.contains('#')
            && !forbidden_segment
    })
}

fn is_digest_logical_ref(reference: &AppReference, prefix: &str) -> bool {
    reference
        .as_str()
        .strip_prefix(prefix)
        .is_some_and(|digest| AppDigest::parse(digest.to_string()).is_ok())
}

fn is_catalog_identity(reference: &AppReference, prefix: &str) -> bool {
    let Some(rest) = reference.as_str().strip_prefix(prefix) else {
        return false;
    };
    let Some((kind, digest_hex)) = rest.split_once(':') else {
        return false;
    };
    !kind.is_empty()
        && kind
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && digest_hex.len() == 64
        && digest_hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn is_named_logical_ref(reference: &AppReference, prefix: &str) -> bool {
    reference
        .as_str()
        .strip_prefix(prefix)
        .is_some_and(|name| AppName::parse(name).is_ok())
}

fn is_value_schema_ref(reference: &AppReference) -> bool {
    is_digest_logical_ref(reference, "schema:")
        || is_digest_logical_ref(reference, "workflow-schema:")
}

struct BoundedDigestWriter {
    hasher: blake3::Hasher,
    bytes: usize,
    limit: usize,
    exceeded: bool,
}

impl BoundedDigestWriter {
    fn new(limit: usize) -> Self {
        Self {
            hasher: blake3::Hasher::new(),
            bytes: 0,
            limit,
            exceeded: false,
        }
    }
}

impl Write for BoundedDigestWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(next) = self.bytes.checked_add(bytes.len()) else {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "canonical byte ceiling",
            ));
        };
        if next > self.limit {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "canonical byte ceiling",
            ));
        }
        self.hasher.update(bytes);
        self.bytes = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn stream_canonical_identity<T: Serialize>(
    domain: &'static str,
    value: &T,
    limit: usize,
) -> Result<(AppDigest, usize), AppRecipeIrError> {
    // These source types contain only structs, enums, vectors whose order is
    // semantic, and BTree maps/sets. Compact Serde JSON is therefore the v1
    // canonical encoding and can be streamed without a payload-sized clone.
    let mut sink = BoundedDigestWriter::new(limit);
    if let Err(error) = serde_json::to_writer(&mut sink, value) {
        if sink.exceeded {
            return Err(AppRecipeIrError::CanonicalByteLimit { domain, limit });
        }
        return Err(AppRecipeIrError::Encoding(error.to_string()));
    }
    let digest = AppDigest::parse(format!("blake3:{}", sink.hasher.finalize().to_hex()))?;
    Ok((digest, sink.bytes))
}

fn write_canonical_stream<W: Write, T: Serialize>(
    writer: W,
    value: &T,
) -> Result<(), AppRecipeIrError> {
    serde_json::to_writer(writer, value)
        .map_err(|error| AppRecipeIrError::Encoding(error.to_string()))
}

#[derive(Debug, Error)]
pub enum AppRecipeIrError {
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error(transparent)]
    ValueMapping(#[from] AppValueMappingError),
    #[error("workflow schema version is unsupported; explicit migration/recompile is required")]
    UnsupportedSchemaVersion,
    #[error("workflow value version is unsupported; explicit migration/recompile is required")]
    UnsupportedValueVersion,
    #[error("recipe version is unsupported; explicit migration/recompile is required")]
    UnsupportedRecipeVersion,
    #[error("recipe bundle version is unsupported; explicit migration/recompile is required")]
    UnsupportedRecipeBundleVersion,
    #[error("recipe bundle contains duplicate content-addressed schema declarations")]
    DuplicateRecipeBundleSchema,
    #[error("every recipe bundle schema must be referenced by the exact recipe graph")]
    UnreferencedRecipeBundleSchema,
    #[error("{domain} must contain at most {limit} reachable nodes")]
    ArenaNodeLimit { domain: &'static str, limit: usize },
    #[error("{domain} must contain at most {limit} edges")]
    ArenaEdgeLimit { domain: &'static str, limit: usize },
    #[error("{domain} exceeds the {limit}-level depth ceiling")]
    ArenaDepth { domain: &'static str, limit: usize },
    #[error("{domain} contains a cycle")]
    ArenaCycle { domain: &'static str },
    #[error("{domain} contains an unreachable definition")]
    UnreachableArenaNode { domain: &'static str },
    #[error("arena contains an out-of-range node reference")]
    InvalidArenaReference,
    #[error("workflow values must be trees; shared value nodes are refused")]
    SharedValueNode,
    #[error("text type must declare a positive bounded byte ceiling")]
    InvalidTextCeiling,
    #[error("enum type must contain a bounded, non-empty closed value set")]
    InvalidEnum,
    #[error("array type has invalid min/max item ceilings")]
    InvalidArrayBounds,
    #[error("record type must contain a bounded, non-empty property set")]
    RecordFieldLimit,
    #[error(
        "tagged union must have a distinct exact discriminator and a bounded closed variant set"
    )]
    InvalidTaggedUnion,
    #[error("artifact type/reference has an invalid media type or byte ceiling")]
    InvalidArtifactType,
    #[error("resource type must reference an exact content-addressed value schema")]
    InvalidResourceSchemaRef,
    #[error("workflow value schema identity was substituted")]
    SchemaSubstitution,
    #[error("workflow value does not match its exact schema")]
    ValueTypeMismatch,
    #[error("workflow value resource does not match its declared resource type")]
    ResourceTypeMismatch,
    #[error("workflow value contains an unreferenced resource descriptor")]
    UnusedResourceRef,
    #[error("workflow value provenance exceeds its item ceiling")]
    ProvenanceLimit,
    #[error("workflow value resources exceed their item ceiling")]
    ResourceRefLimit,
    #[error("resource reference `{0}` is not an opaque typed logical reference")]
    NonLogicalResourceRef(String),
    #[error("provenance reference `{0}` is not an exact typed logical source")]
    InvalidProvenanceRef(String),
    #[error("workflow value handling labels downgrade a schema, source or resource label")]
    HandlingLabelDowngrade,
    #[error("decimal must be a bounded plain base-10 value")]
    InvalidDecimal,
    #[error("timestamp must be valid RFC3339")]
    InvalidTimestamp,
    #[error("{domain} canonical encoding exceeds {limit} bytes")]
    CanonicalByteLimit { domain: &'static str, limit: usize },
    #[error("failed to encode canonical recipe/value material: {0}")]
    Encoding(String),
    #[error("duplicate compiled schema reference `{0}`")]
    DuplicateSchemaRef(String),
    #[error("unknown compiled schema reference `{0}`")]
    UnknownSchemaRef(String),
    #[error("recipe root does not match its exact input/output boundary")]
    RecipeBoundaryMismatch,
    #[error("recipe graph ceiling is invalid or exceeds the platform maximum")]
    InvalidGraphCeiling,
    #[error("recipe exceeds its node ceiling")]
    RecipeNodeLimit,
    #[error("recipe exceeds its edge ceiling")]
    RecipeEdgeLimit,
    #[error("recipe exceeds its depth ceiling")]
    RecipeDepth,
    #[error("recipe exceeds its fan-out or parallelism ceiling")]
    RecipeFanOut,
    #[error("recipe contains a cycle through `{0}`")]
    RecipeCycle(String),
    #[error("recipe references unknown node `{0}`")]
    UnknownRecipeNode(String),
    #[error("recipe contains an unreachable node")]
    UnreachableRecipeNode,
    #[error("recipe nodes cannot be shared by multiple control owners in v1")]
    SharedRecipeNode,
    #[error("recipe node `{0}` has an invalid mandatory resource/cancellation ceiling")]
    InvalidNodeCeiling(String),
    #[error("recipe aggregate resource ceiling is exceeded")]
    RecipeResourceOverflow,
    #[error("recipe node `{0}` declares the wrong effect class")]
    EffectClassMismatch(String),
    #[error("recipe node `{0}` has an invalid idempotency/uncertainty contract")]
    InvalidEffectContract(String),
    #[error("recipe node `{0}` has an invalid authority shape")]
    AuthorityShapeMismatch(String),
    #[error("recipe node `{0}` contains a non-logical or wrong-namespace authority reference")]
    InvalidAuthorityReference(String),
    #[error("recipe node `{0}` attempts to strengthen derived/composite output authority")]
    OutputAuthorityEscalation(String),
    #[error("v1 supports only provenance preservation; reviewed intersection lowering is absent")]
    UnsupportedProvenanceJoinV1,
    #[error("mutate must name a non-empty bounded entity set")]
    InvalidMutationTargets,
    #[error("recipe output kind does not match its exact schema")]
    OutputKindMismatch,
    #[error("recipe Get must revalidate and preserve one exact entity projection schema")]
    GetProjectionContract,
    #[error("recipe Map supports only a flat record of closed scalar fields")]
    UnsupportedMappingSchema,
    #[error("recipe Map mapping body does not match its declared canonical digest")]
    MappingDigestSubstitution,
    #[error("recipe Map transformation is partial over its declared source schema")]
    NonTotalMapping,
    #[error("recipe Map cannot transform logical resource descriptors")]
    MappingResourceInput,
    #[error("parallel output must be an exact fixed-length array of branch result types")]
    ParallelOutputSchema,
    #[error("switch input must be an exact closed tagged union")]
    SwitchInputSchema,
    #[error("mark_uncertain output must be the exact completed/uncertain tagged union")]
    UncertainOutputSchema,
    #[error("retry contract is valid only on the retry control node")]
    InvalidRetryContract,
    #[error("v1 refuses retry around reasoning, mutation or external-effect nodes")]
    EffectfulRetryUnsupportedInV1,
    #[error("recipe node `{0:?}` is explicitly deferred and unsupported in v1")]
    UnsupportedNode(AppRecipeDeferredNodeKind),
    #[error("recipe topology revision/predecessor migration contract is invalid")]
    InvalidEvolutionContract,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn name(value: &str) -> AppName {
        AppName::parse(value).unwrap()
    }

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).unwrap()
    }

    fn digest(seed: &str) -> AppDigest {
        AppDigest::blake3(seed.as_bytes())
    }

    fn schema_reference(seed: &str) -> AppReference {
        AppReference::parse(format!("schema:{}", digest(seed).as_str())).unwrap()
    }

    fn catalog_reference(prefix: &str, kind: &str, seed: &str) -> AppReference {
        let digest = digest(seed);
        AppReference::parse(format!(
            "{prefix}{kind}:{}",
            digest.as_str().strip_prefix("blake3:").unwrap()
        ))
        .unwrap()
    }

    fn revision(value: u64) -> AppRevision {
        AppRevision::new(value).unwrap()
    }

    fn labels(
        classification: AppDataClassification,
        model_processing: AppModelProcessing,
        seed: &str,
    ) -> AppHandlingLabels {
        AppHandlingLabels {
            classification,
            model_processing,
            policy_digest: digest(&format!("policy-{seed}")),
            provenance_digest: digest(&format!("provenance-{seed}")),
        }
    }

    fn record_schema(kind: AppWorkflowValueTypeNode) -> AppCompiledWorkflowValueSchema {
        compile_workflow_value_schema(AppWorkflowValueSchemaSource {
            version: AppWorkflowValueSchemaVersion::V1,
            root: 0,
            handling_floor: AppWorkflowHandlingFloor {
                classification: AppDataClassification::Ordinary,
                model_processing: AppModelProcessing::LocalOnly,
            },
            nodes: vec![
                AppWorkflowValueTypeNode::Record {
                    fields: BTreeMap::from([(
                        name("value"),
                        AppWorkflowRecordField {
                            value_type: 1,
                            required: true,
                        },
                    )]),
                },
                kind,
            ],
        })
        .unwrap()
    }

    fn receipt_schema(kind: AppWorkflowReceiptKind) -> AppCompiledWorkflowValueSchema {
        compile_workflow_value_schema(AppWorkflowValueSchemaSource {
            version: AppWorkflowValueSchemaVersion::V1,
            root: 0,
            handling_floor: AppWorkflowHandlingFloor {
                classification: AppDataClassification::Ordinary,
                model_processing: AppModelProcessing::LocalOnly,
            },
            nodes: vec![AppWorkflowValueTypeNode::ReceiptRef { receipt_kind: kind }],
        })
        .unwrap()
    }

    #[test]
    fn parallel_join_is_semantic_ordered_bounded_and_monotone() {
        let item = record_schema(AppWorkflowValueTypeNode::Text { max_bytes: 64 });
        let output = compile_workflow_value_schema(AppWorkflowValueSchemaSource {
            version: AppWorkflowValueSchemaVersion::V1,
            root: 0,
            handling_floor: AppWorkflowHandlingFloor {
                classification: AppDataClassification::Ordinary,
                model_processing: AppModelProcessing::LocalOnly,
            },
            nodes: vec![
                AppWorkflowValueTypeNode::Array {
                    items: 1,
                    min_items: 2,
                    max_items: 2,
                },
                AppWorkflowValueTypeNode::Record {
                    fields: BTreeMap::from([(
                        name("value"),
                        AppWorkflowRecordField {
                            value_type: 2,
                            required: true,
                        },
                    )]),
                },
                AppWorkflowValueTypeNode::Text { max_bytes: 64 },
            ],
        })
        .unwrap();
        let first = validate_json_workflow_value(
            &item,
            serde_json::json!({"value": "alpha"}),
            labels(
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
                "alpha",
            ),
            BTreeMap::new(),
        )
        .unwrap();
        let second = validate_json_workflow_value(
            &item,
            serde_json::json!({"value": "omega"}),
            labels(
                AppDataClassification::Sensitive,
                AppModelProcessing::LocalOnly,
                "omega",
            ),
            BTreeMap::new(),
        )
        .unwrap();

        let joined = validate_parallel_workflow_values(&output, vec![first, second]).unwrap();
        assert_eq!(
            workflow_value_to_json(&output, &joined).unwrap(),
            serde_json::json!([{"value": "alpha"}, {"value": "omega"}])
        );
        assert_eq!(
            joined.handling_labels().classification,
            AppDataClassification::Sensitive
        );
        assert!(matches!(
            validate_parallel_workflow_values(&output, Vec::new()),
            Err(AppRecipeIrError::ParallelOutputSchema)
        ));
    }

    fn typed_output(schema: &AppCompiledWorkflowValueSchema) -> AppRecipeOutputContract {
        AppRecipeOutputContract {
            kind: AppRecipeOutputKind::TypedValue,
            schema_ref: schema.schema_ref().clone(),
            authority: AppRecipeOutputAuthority::Derived,
        }
    }

    fn node_resources() -> AppRecipeNodeResourceCeiling {
        AppRecipeNodeResourceCeiling {
            max_active_millis: 1_000,
            max_input_bytes: 4_096,
            max_output_bytes: 4_096,
            max_cost_microusd: 0,
            max_tool_calls: 0,
            max_parallelism: 1,
        }
    }

    fn no_authority() -> AppRecipeAuthorityContract {
        AppRecipeAuthorityContract {
            primitive_ref: None,
            action_ref: None,
            target_app_ref: None,
            required_grant_refs: BTreeSet::new(),
            resource_scope_refs: BTreeSet::new(),
        }
    }

    fn pure_node(
        node: AppRecipeNodeKind,
        input: &AppCompiledWorkflowValueSchema,
        output: &AppCompiledWorkflowValueSchema,
    ) -> AppRecipeNode {
        AppRecipeNode {
            input_schema_ref: input.schema_ref().clone(),
            output: typed_output(output),
            node,
            effect: AppRecipeEffectContract {
                class: AppRecipeEffectClass::None,
                idempotency: AppRecipeIdempotencyContract::NotApplicable,
                uncertainty: AppRecipeUncertaintyContract::Impossible,
            },
            authority: no_authority(),
            resources: node_resources(),
            retry: AppRecipeRetryContract::None,
            cancellation: AppRecipeCancellationContract {
                mode: AppRecipeCancellationMode::Propagate,
                acknowledgement_timeout_millis: 100,
            },
            provenance_join: AppRecipeProvenanceJoin::Preserve,
        }
    }

    fn recipe(
        input: &AppCompiledWorkflowValueSchema,
        output: &AppCompiledWorkflowValueSchema,
        root: &str,
        nodes: BTreeMap<AppName, AppRecipeNode>,
    ) -> AppRecipeIrSource {
        AppRecipeIrSource {
            version: AppRecipeVersion::V1,
            input_schema_ref: input.schema_ref().clone(),
            output: typed_output(output),
            root: name(root),
            ceilings: AppRecipeGraphCeiling {
                max_nodes: 16,
                max_edges: 16,
                max_depth: 8,
                max_fan_out: 8,
                max_parallelism: 4,
                max_payload_bytes: 64 * 1024,
                max_active_millis: 16_000,
                max_cost_microusd: 1_000,
                max_tool_calls: 16,
            },
            evolution: AppRecipeEvolutionContract {
                topology_revision: revision(1),
                migration: AppRecipeMigrationPolicy::RecompileRequired,
                predecessor_recipe_ref: None,
            },
            nodes,
        }
    }

    #[test]
    fn schema_identity_ignores_non_semantic_arena_declaration_order() {
        let floor = AppWorkflowHandlingFloor {
            classification: AppDataClassification::Ordinary,
            model_processing: AppModelProcessing::LocalOnly,
        };
        let left = compile_workflow_value_schema(AppWorkflowValueSchemaSource {
            version: AppWorkflowValueSchemaVersion::V1,
            root: 0,
            handling_floor: floor.clone(),
            nodes: vec![
                AppWorkflowValueTypeNode::Record {
                    fields: BTreeMap::from([
                        (
                            name("count"),
                            AppWorkflowRecordField {
                                value_type: 2,
                                required: true,
                            },
                        ),
                        (
                            name("title"),
                            AppWorkflowRecordField {
                                value_type: 1,
                                required: true,
                            },
                        ),
                    ]),
                },
                AppWorkflowValueTypeNode::Text { max_bytes: 128 },
                AppWorkflowValueTypeNode::Integer,
            ],
        })
        .unwrap();
        let right = compile_workflow_value_schema(AppWorkflowValueSchemaSource {
            version: AppWorkflowValueSchemaVersion::V1,
            root: 2,
            handling_floor: floor,
            nodes: vec![
                AppWorkflowValueTypeNode::Integer,
                AppWorkflowValueTypeNode::Text { max_bytes: 128 },
                AppWorkflowValueTypeNode::Record {
                    fields: BTreeMap::from([
                        (
                            name("title"),
                            AppWorkflowRecordField {
                                value_type: 1,
                                required: true,
                            },
                        ),
                        (
                            name("count"),
                            AppWorkflowRecordField {
                                value_type: 0,
                                required: true,
                            },
                        ),
                    ]),
                },
            ],
        })
        .unwrap();
        assert_eq!(left.schema_ref(), right.schema_ref());
        assert_eq!(left.content_digest(), right.content_digest());
    }

    #[test]
    fn bounded_value_preserves_optional_nullable_and_label_floor() {
        let schema = compile_workflow_value_schema(AppWorkflowValueSchemaSource {
            version: AppWorkflowValueSchemaVersion::V1,
            root: 0,
            handling_floor: AppWorkflowHandlingFloor {
                classification: AppDataClassification::Personal,
                model_processing: AppModelProcessing::LocalOnly,
            },
            nodes: vec![
                AppWorkflowValueTypeNode::Record {
                    fields: BTreeMap::from([
                        (
                            name("note"),
                            AppWorkflowRecordField {
                                value_type: 2,
                                required: false,
                            },
                        ),
                        (
                            name("title"),
                            AppWorkflowRecordField {
                                value_type: 1,
                                required: true,
                            },
                        ),
                    ]),
                },
                AppWorkflowValueTypeNode::Text { max_bytes: 64 },
                AppWorkflowValueTypeNode::Nullable { value_type: 3 },
                AppWorkflowValueTypeNode::Text { max_bytes: 64 },
            ],
        })
        .unwrap();
        let value = validate_workflow_value(
            &schema,
            AppWorkflowValueSource {
                version: AppWorkflowValueSchemaVersion::V1,
                schema_ref: schema.schema_ref().clone(),
                root: 2,
                nodes: vec![
                    AppWorkflowValueNode::Null,
                    AppWorkflowValueNode::Text {
                        value: "bounded".to_string(),
                    },
                    AppWorkflowValueNode::Record {
                        fields: BTreeMap::from([(name("title"), 1), (name("note"), 0)]),
                    },
                ],
                provenance: BTreeMap::new(),
                resources: BTreeMap::new(),
                handling_labels: labels(
                    AppDataClassification::Sensitive,
                    AppModelProcessing::LocalOnly,
                    "value",
                ),
            },
        )
        .unwrap();
        assert_eq!(value.schema_ref(), schema.schema_ref());
        assert!(value.canonical_encoded_len() > 0);
    }

    #[test]
    fn schema_substitution_is_rejected_before_value_admission() {
        let schema = record_schema(AppWorkflowValueTypeNode::Text { max_bytes: 64 });
        let error = validate_workflow_value(
            &schema,
            AppWorkflowValueSource {
                version: AppWorkflowValueSchemaVersion::V1,
                schema_ref: reference("workflow-schema:substituted"),
                root: 0,
                nodes: vec![AppWorkflowValueNode::Unit],
                provenance: BTreeMap::new(),
                resources: BTreeMap::new(),
                handling_labels: labels(
                    AppDataClassification::Ordinary,
                    AppModelProcessing::LocalOnly,
                    "substitution",
                ),
            },
        )
        .unwrap_err();
        assert!(matches!(error, AppRecipeIrError::SchemaSubstitution));
    }

    #[test]
    fn schema_cycle_and_excessive_depth_are_stack_safely_rejected() {
        let cyclic = compile_workflow_value_schema(AppWorkflowValueSchemaSource {
            version: AppWorkflowValueSchemaVersion::V1,
            root: 0,
            handling_floor: AppWorkflowHandlingFloor {
                classification: AppDataClassification::Ordinary,
                model_processing: AppModelProcessing::LocalOnly,
            },
            nodes: vec![AppWorkflowValueTypeNode::Nullable { value_type: 0 }],
        })
        .unwrap_err();
        assert!(matches!(cyclic, AppRecipeIrError::ArenaCycle { .. }));

        let mut nodes = (0..=MAX_SCHEMA_DEPTH)
            .map(|index| AppWorkflowValueTypeNode::Nullable {
                value_type: u16::try_from(index + 1).unwrap(),
            })
            .collect::<Vec<_>>();
        nodes.push(AppWorkflowValueTypeNode::Text { max_bytes: 8 });
        let deep = compile_workflow_value_schema(AppWorkflowValueSchemaSource {
            version: AppWorkflowValueSchemaVersion::V1,
            root: 0,
            handling_floor: AppWorkflowHandlingFloor {
                classification: AppDataClassification::Ordinary,
                model_processing: AppModelProcessing::LocalOnly,
            },
            nodes,
        })
        .unwrap_err();
        assert!(matches!(deep, AppRecipeIrError::ArenaDepth { .. }));
    }

    #[test]
    fn aggregate_canonical_value_bytes_are_bounded_without_a_byte_clone() {
        let schema = compile_workflow_value_schema(AppWorkflowValueSchemaSource {
            version: AppWorkflowValueSchemaVersion::V1,
            root: 0,
            handling_floor: AppWorkflowHandlingFloor {
                classification: AppDataClassification::Ordinary,
                model_processing: AppModelProcessing::LocalOnly,
            },
            nodes: vec![
                AppWorkflowValueTypeNode::Array {
                    items: 1,
                    min_items: MAX_ARRAY_ITEMS,
                    max_items: MAX_ARRAY_ITEMS,
                },
                AppWorkflowValueTypeNode::Text { max_bytes: 4_096 },
            ],
        })
        .unwrap();
        let mut nodes = vec![AppWorkflowValueNode::Array {
            items: (1..=MAX_ARRAY_ITEMS).collect(),
        }];
        nodes.extend((0..MAX_ARRAY_ITEMS).map(|_| AppWorkflowValueNode::Text {
            value: "x".repeat(2_048),
        }));
        let error = validate_workflow_value(
            &schema,
            AppWorkflowValueSource {
                version: AppWorkflowValueSchemaVersion::V1,
                schema_ref: schema.schema_ref().clone(),
                root: 0,
                nodes,
                provenance: BTreeMap::new(),
                resources: BTreeMap::new(),
                handling_labels: labels(
                    AppDataClassification::Ordinary,
                    AppModelProcessing::LocalOnly,
                    "oversize",
                ),
            },
        )
        .unwrap_err();
        assert!(matches!(error, AppRecipeIrError::CanonicalByteLimit { .. }));
    }

    #[test]
    fn simple_closed_recipe_gets_a_deterministic_inert_identity() {
        let schema = record_schema(AppWorkflowValueTypeNode::Text { max_bytes: 64 });
        let root = name("root");
        let source = recipe(
            &schema,
            &schema,
            "root",
            BTreeMap::from([(
                root,
                pure_node(AppRecipeNodeKind::Validate, &schema, &schema),
            )]),
        );
        let first = compile_recipe_ir(source.clone(), &[&schema]).unwrap();
        let second = compile_recipe_ir(source, &[&schema]).unwrap();
        assert_eq!(first.recipe_ref(), second.recipe_ref());
        assert_eq!(first.topology_digest(), second.topology_digest());
    }

    #[test]
    fn recipe_schema_substitution_and_cycle_fail_closed() {
        let text = record_schema(AppWorkflowValueTypeNode::Text { max_bytes: 64 });
        let integer = record_schema(AppWorkflowValueTypeNode::Integer);
        let root = pure_node(
            AppRecipeNodeKind::Sequence {
                steps: vec![name("first"), name("second")],
            },
            &text,
            &integer,
        );
        let first = pure_node(AppRecipeNodeKind::Validate, &text, &text);
        let second = pure_node(AppRecipeNodeKind::Validate, &integer, &integer);
        let substituted = compile_recipe_ir(
            recipe(
                &text,
                &integer,
                "root",
                BTreeMap::from([
                    (name("root"), root),
                    (name("first"), first),
                    (name("second"), second),
                ]),
            ),
            &[&text, &integer],
        )
        .unwrap_err();
        assert!(matches!(substituted, AppRecipeIrError::SchemaSubstitution));

        let cyclic_root = pure_node(
            AppRecipeNodeKind::Sequence {
                steps: vec![name("root")],
            },
            &text,
            &text,
        );
        let cyclic = compile_recipe_ir(
            recipe(
                &text,
                &text,
                "root",
                BTreeMap::from([(name("root"), cyclic_root)]),
            ),
            &[&text],
        )
        .unwrap_err();
        assert!(matches!(cyclic, AppRecipeIrError::RecipeCycle(_)));
    }

    #[test]
    fn deferred_nodes_and_effectful_retry_are_explicitly_unsupported_in_v1() {
        let schema = record_schema(AppWorkflowValueTypeNode::Text { max_bytes: 64 });
        let deferred = compile_recipe_ir(
            recipe(
                &schema,
                &schema,
                "root",
                BTreeMap::from([(
                    name("root"),
                    pure_node(
                        AppRecipeNodeKind::Deferred {
                            node: AppRecipeDeferredNodeKind::WaitUser,
                        },
                        &schema,
                        &schema,
                    ),
                )]),
            ),
            &[&schema],
        )
        .unwrap_err();
        assert!(matches!(
            deferred,
            AppRecipeIrError::UnsupportedNode(AppRecipeDeferredNodeKind::WaitUser)
        ));

        let mut child = pure_node(AppRecipeNodeKind::AgentAsTool, &schema, &schema);
        child.effect = AppRecipeEffectContract {
            class: AppRecipeEffectClass::Reasoning,
            idempotency: AppRecipeIdempotencyContract::ReviewedKey {
                contract_digest: digest("reviewed-idempotency"),
            },
            uncertainty: AppRecipeUncertaintyContract::ReceiptBound,
        };
        child.authority.primitive_ref =
            Some(catalog_reference("primitive:", "agent", "reviewed-agent"));
        child.resources.max_tool_calls = 1;
        let mut root = pure_node(
            AppRecipeNodeKind::Retry {
                child: name("child"),
            },
            &schema,
            &schema,
        );
        root.retry = AppRecipeRetryContract::Bounded {
            max_attempts: 2,
            initial_backoff_millis: 10,
            max_backoff_millis: 20,
        };
        let retry = compile_recipe_ir(
            recipe(
                &schema,
                &schema,
                "root",
                BTreeMap::from([(name("root"), root), (name("child"), child)]),
            ),
            &[&schema],
        )
        .unwrap_err();
        assert!(matches!(
            retry,
            AppRecipeIrError::EffectfulRetryUnsupportedInV1
        ));
    }

    #[test]
    fn resource_labels_cannot_be_downgraded() {
        let resource_schema = compile_workflow_value_schema(AppWorkflowValueSchemaSource {
            version: AppWorkflowValueSchemaVersion::V1,
            root: 0,
            handling_floor: AppWorkflowHandlingFloor {
                classification: AppDataClassification::Ordinary,
                model_processing: AppModelProcessing::LocalOnly,
            },
            nodes: vec![AppWorkflowValueTypeNode::ArtifactRef {
                value_schema_ref: schema_reference("artifact-schema-one"),
                max_bytes: 1_024,
                media_types: BTreeSet::from(["application/json".to_string()]),
            }],
        })
        .unwrap();
        let resource_ref = reference("artifact:logical-one");
        let error = validate_workflow_value(
            &resource_schema,
            AppWorkflowValueSource {
                version: AppWorkflowValueSchemaVersion::V1,
                schema_ref: resource_schema.schema_ref().clone(),
                root: 0,
                nodes: vec![AppWorkflowValueNode::Resource {
                    reference: resource_ref.clone(),
                }],
                provenance: BTreeMap::new(),
                resources: BTreeMap::from([(
                    resource_ref,
                    AppWorkflowResourceRef::Artifact {
                        revision: revision(1),
                        value_schema_ref: schema_reference("artifact-schema-one"),
                        media_type: "application/json".to_string(),
                        byte_len: 64,
                        content_digest: digest("artifact"),
                        handling_labels: labels(
                            AppDataClassification::Sensitive,
                            AppModelProcessing::None,
                            "resource",
                        ),
                    },
                )]),
                handling_labels: labels(
                    AppDataClassification::Ordinary,
                    AppModelProcessing::LocalOnly,
                    "output",
                ),
            },
        )
        .unwrap_err();
        assert!(matches!(error, AppRecipeIrError::HandlingLabelDowngrade));
    }

    #[test]
    fn authority_fields_reject_url_path_and_wrong_namespace_substitution() {
        let schema = record_schema(AppWorkflowValueTypeNode::Text { max_bytes: 64 });
        let mut call = pure_node(AppRecipeNodeKind::CallTool, &schema, &schema);
        call.authority.primitive_ref = Some(reference("https://provider.example/tool"));
        call.authority.action_ref = Some(catalog_reference(
            "primitive-action:",
            "platform",
            "reviewed-action",
        ));
        call.resources.max_tool_calls = 1;
        let error = compile_recipe_ir(
            recipe(
                &schema,
                &schema,
                "root",
                BTreeMap::from([(name("root"), call)]),
            ),
            &[&schema],
        )
        .unwrap_err();
        assert!(matches!(
            error,
            AppRecipeIrError::InvalidAuthorityReference(_)
        ));

        let provenance_error = validate_workflow_value(
            &schema,
            AppWorkflowValueSource {
                version: AppWorkflowValueSchemaVersion::V1,
                schema_ref: schema.schema_ref().clone(),
                root: 0,
                nodes: vec![
                    AppWorkflowValueNode::Record {
                        fields: BTreeMap::from([(name("value"), 1)]),
                    },
                    AppWorkflowValueNode::Text {
                        value: "safe payload".to_string(),
                    },
                ],
                provenance: BTreeMap::from([(
                    reference("https://provider.example/source"),
                    AppWorkflowProvenanceEntry {
                        kind: AppSourceRefKind::Artifact,
                        revision: Some(revision(1)),
                        fields: BTreeSet::new(),
                        handling_labels: labels(
                            AppDataClassification::Ordinary,
                            AppModelProcessing::LocalOnly,
                            "hostile-provenance",
                        ),
                        content_digest: digest("hostile-provenance"),
                    },
                )]),
                resources: BTreeMap::new(),
                handling_labels: labels(
                    AppDataClassification::Ordinary,
                    AppModelProcessing::LocalOnly,
                    "hostile-output",
                ),
            },
        )
        .unwrap_err();
        assert!(matches!(
            provenance_error,
            AppRecipeIrError::InvalidProvenanceRef(_)
        ));
    }

    #[test]
    fn caller_asserted_output_authority_and_provenance_join_fail_closed() {
        let schema = record_schema(AppWorkflowValueTypeNode::Text { max_bytes: 64 });
        let mut mapped = pure_node(
            AppRecipeNodeKind::Map {
                mapping_digest: digest("mapping"),
                operations: Vec::new(),
            },
            &schema,
            &schema,
        );
        mapped.output.authority = AppRecipeOutputAuthority::Authoritative;
        let mapped_output = mapped.output.clone();
        let mut mapped_recipe = recipe(
            &schema,
            &schema,
            "root",
            BTreeMap::from([(name("root"), mapped)]),
        );
        mapped_recipe.output = mapped_output;
        let authority_error = compile_recipe_ir(mapped_recipe, &[&schema]).unwrap_err();
        assert!(matches!(
            authority_error,
            AppRecipeIrError::OutputAuthorityEscalation(_)
        ));

        let mut intersected = pure_node(AppRecipeNodeKind::Validate, &schema, &schema);
        intersected.provenance_join = AppRecipeProvenanceJoin::Intersection;
        let provenance_error = compile_recipe_ir(
            recipe(
                &schema,
                &schema,
                "root",
                BTreeMap::from([(name("root"), intersected)]),
            ),
            &[&schema],
        )
        .unwrap_err();
        assert!(matches!(
            provenance_error,
            AppRecipeIrError::UnsupportedProvenanceJoinV1
        ));

        let parallel_output = compile_workflow_value_schema(AppWorkflowValueSchemaSource {
            version: AppWorkflowValueSchemaVersion::V1,
            root: 0,
            handling_floor: AppWorkflowHandlingFloor {
                classification: AppDataClassification::Ordinary,
                model_processing: AppModelProcessing::LocalOnly,
            },
            nodes: vec![
                AppWorkflowValueTypeNode::Array {
                    items: 1,
                    min_items: 2,
                    max_items: 2,
                },
                AppWorkflowValueTypeNode::Record {
                    fields: BTreeMap::from([(
                        name("value"),
                        AppWorkflowRecordField {
                            value_type: 2,
                            required: true,
                        },
                    )]),
                },
                AppWorkflowValueTypeNode::Text { max_bytes: 64 },
            ],
        })
        .unwrap();
        let branch_one = pure_node(AppRecipeNodeKind::Validate, &schema, &schema);
        let branch_two = pure_node(AppRecipeNodeKind::Validate, &schema, &schema);
        let mut parallel = pure_node(
            AppRecipeNodeKind::Parallel {
                branches: BTreeMap::from([
                    (name("left"), name("branch-one")),
                    (name("right"), name("branch-two")),
                ]),
            },
            &schema,
            &parallel_output,
        );
        parallel.output.authority = AppRecipeOutputAuthority::Authoritative;
        parallel.resources.max_parallelism = 2;
        let parallel_contract = parallel.output.clone();
        let mut parallel_recipe = recipe(
            &schema,
            &parallel_output,
            "root",
            BTreeMap::from([
                (name("root"), parallel),
                (name("branch-one"), branch_one),
                (name("branch-two"), branch_two),
            ]),
        );
        parallel_recipe.output = parallel_contract;
        let parallel_error =
            compile_recipe_ir(parallel_recipe, &[&schema, &parallel_output]).unwrap_err();
        assert!(matches!(
            parallel_error,
            AppRecipeIrError::OutputAuthorityEscalation(_)
        ));
    }

    #[test]
    fn recipe_map_binds_the_installed_body_to_its_exact_digest() {
        let schema = record_schema(AppWorkflowValueTypeNode::Text { max_bytes: 64 });
        let operations = vec![AppValueMappingOperation::Select {
            source: AppFieldPath::parse("value").unwrap(),
            target: AppFieldPath::parse("value").unwrap(),
        }];
        let mapping_digest = compile_recipe_value_mapping(&schema, &schema, operations.clone())
            .unwrap()
            .mapping_digest()
            .clone();
        let valid = recipe(
            &schema,
            &schema,
            "root",
            BTreeMap::from([(
                name("root"),
                pure_node(
                    AppRecipeNodeKind::Map {
                        mapping_digest,
                        operations: operations.clone(),
                    },
                    &schema,
                    &schema,
                ),
            )]),
        );
        compile_recipe_ir(valid, &[&schema]).unwrap();

        let substituted = recipe(
            &schema,
            &schema,
            "root",
            BTreeMap::from([(
                name("root"),
                pure_node(
                    AppRecipeNodeKind::Map {
                        mapping_digest: digest("substituted-mapping"),
                        operations,
                    },
                    &schema,
                    &schema,
                ),
            )]),
        );
        assert!(matches!(
            compile_recipe_ir(substituted, &[&schema]),
            Err(AppRecipeIrError::MappingDigestSubstitution)
        ));

        let narrow = record_schema(AppWorkflowValueTypeNode::Text { max_bytes: 8 });
        assert!(matches!(
            compile_recipe_value_mapping(
                &schema,
                &narrow,
                vec![AppValueMappingOperation::Select {
                    source: AppFieldPath::parse("value").unwrap(),
                    target: AppFieldPath::parse("value").unwrap(),
                }],
            ),
            Err(AppRecipeIrError::NonTotalMapping)
        ));
    }

    #[test]
    fn recipe_enum_mapping_is_closed_total_and_uses_the_canonical_schema_graph() {
        let source = record_schema(AppWorkflowValueTypeNode::Enum {
            values: BTreeSet::from([name("open"), name("done")]),
        });
        let target = record_schema(AppWorkflowValueTypeNode::Enum {
            values: BTreeSet::from([name("active"), name("closed")]),
        });
        let operation = AppValueMappingOperation::MapEnum {
            source: AppFieldPath::parse("value").unwrap(),
            target: AppFieldPath::parse("value").unwrap(),
            values: BTreeMap::from([
                (name("open"), name("active")),
                (name("done"), name("closed")),
            ]),
        };
        let compiled = compile_recipe_value_mapping(&source, &target, vec![operation.clone()])
            .expect("the canonical enum graph produces one total mapping contract");
        let input = validate_json_workflow_value(
            &source,
            serde_json::json!({ "value": "open" }),
            labels(
                AppDataClassification::Ordinary,
                AppModelProcessing::LocalOnly,
                "enum-map",
            ),
            BTreeMap::new(),
        )
        .unwrap();
        let output = apply_recipe_value_mapping(
            &source,
            &target,
            compiled.mapping_digest(),
            vec![operation],
            &input,
        )
        .unwrap();
        assert_eq!(
            workflow_value_to_json(&target, &output).unwrap(),
            serde_json::json!({ "value": "active" })
        );

        assert!(matches!(
            compile_recipe_value_mapping(
                &source,
                &target,
                vec![AppValueMappingOperation::MapEnum {
                    source: AppFieldPath::parse("value").unwrap(),
                    target: AppFieldPath::parse("value").unwrap(),
                    values: BTreeMap::from([(name("open"), name("active"))]),
                }],
            ),
            Err(AppRecipeIrError::NonTotalMapping)
        ));
    }

    #[test]
    fn mutate_requires_a_nonempty_bounded_entity_set() {
        let input = record_schema(AppWorkflowValueTypeNode::Text { max_bytes: 64 });
        let receipt = receipt_schema(AppWorkflowReceiptKind::Mutation);
        let receipt_output = AppRecipeOutputContract {
            kind: AppRecipeOutputKind::Receipt,
            schema_ref: receipt.schema_ref().clone(),
            authority: AppRecipeOutputAuthority::Authoritative,
        };
        let mut mutate = pure_node(
            AppRecipeNodeKind::Mutate {
                entities: BTreeSet::new(),
            },
            &input,
            &receipt,
        );
        mutate.output = receipt_output.clone();
        mutate.effect = AppRecipeEffectContract {
            class: AppRecipeEffectClass::InternalMutation,
            idempotency: AppRecipeIdempotencyContract::Intrinsic,
            uncertainty: AppRecipeUncertaintyContract::ReceiptBound,
        };
        mutate
            .authority
            .required_grant_refs
            .insert(reference("grant:entity-mutate"));
        let mut source = recipe(
            &input,
            &receipt,
            "root",
            BTreeMap::from([(name("root"), mutate)]),
        );
        source.output = receipt_output;
        let error = compile_recipe_ir(source, &[&input, &receipt]).unwrap_err();
        assert!(matches!(error, AppRecipeIrError::InvalidMutationTargets));
    }

    #[test]
    fn emit_value_and_receipt_cannot_mint_authority_or_change_receipt_schema() {
        let value = record_schema(AppWorkflowValueTypeNode::Text { max_bytes: 64 });
        let receipt = receipt_schema(AppWorkflowReceiptKind::ExternalEffect);

        let mut emit_value = pure_node(AppRecipeNodeKind::EmitValue, &value, &value);
        emit_value.output.authority = AppRecipeOutputAuthority::Authoritative;
        let emit_value_output = emit_value.output.clone();
        let mut emit_value_recipe = recipe(
            &value,
            &value,
            "root",
            BTreeMap::from([(name("root"), emit_value)]),
        );
        emit_value_recipe.output = emit_value_output;
        let value_error = compile_recipe_ir(emit_value_recipe, &[&value]).unwrap_err();
        assert!(matches!(
            value_error,
            AppRecipeIrError::OutputAuthorityEscalation(_)
        ));

        let receipt_output = AppRecipeOutputContract {
            kind: AppRecipeOutputKind::Receipt,
            schema_ref: receipt.schema_ref().clone(),
            authority: AppRecipeOutputAuthority::Derived,
        };
        let mut substituted = pure_node(AppRecipeNodeKind::EmitReceipt, &value, &receipt);
        substituted.output = receipt_output.clone();
        let mut substituted_recipe = recipe(
            &value,
            &receipt,
            "root",
            BTreeMap::from([(name("root"), substituted)]),
        );
        substituted_recipe.output = receipt_output.clone();
        let substitution_error =
            compile_recipe_ir(substituted_recipe, &[&value, &receipt]).unwrap_err();
        assert!(matches!(
            substitution_error,
            AppRecipeIrError::SchemaSubstitution
        ));

        let mut strengthened = pure_node(AppRecipeNodeKind::EmitReceipt, &receipt, &receipt);
        strengthened.output = AppRecipeOutputContract {
            authority: AppRecipeOutputAuthority::Authoritative,
            ..receipt_output
        };
        let strengthened_output = strengthened.output.clone();
        let mut strengthened_recipe = recipe(
            &receipt,
            &receipt,
            "root",
            BTreeMap::from([(name("root"), strengthened)]),
        );
        strengthened_recipe.output = strengthened_output;
        let authority_error = compile_recipe_ir(strengthened_recipe, &[&receipt]).unwrap_err();
        assert!(matches!(
            authority_error,
            AppRecipeIrError::OutputAuthorityEscalation(_)
        ));
    }
}

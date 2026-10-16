//! Content-addressed value schemas and bounded deterministic mapping contracts.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::{
    models::{
        validate_json_value, AppContractLimits, AppDigest, AppFieldPath, AppName, AppRecordId,
        AppReference,
    },
    policy::{
        join_app_content, AppHandlingConstraint, AppJoinedContent, AppPolicyError,
        RevalidatedAppEnvelope,
    },
    query_semantics::AppQueryScalarKind,
};
use crate::magician_v2::json_traversal::canonical_json_bytes;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppValueFieldContract {
    pub kind: AppQueryScalarKind,
    pub required: bool,
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub enum_values: BTreeSet<AppName>,
}

impl AppValueFieldContract {
    pub(crate) fn from_recipe_scalar(
        kind: AppQueryScalarKind,
        required: bool,
        nullable: bool,
    ) -> Self {
        Self {
            kind,
            required,
            nullable,
            enum_values: BTreeSet::new(),
        }
    }

    pub(crate) fn from_recipe_enum(
        enum_values: BTreeSet<AppName>,
        required: bool,
        nullable: bool,
    ) -> Self {
        Self {
            kind: AppQueryScalarKind::Enum,
            required,
            nullable,
            enum_values,
        }
    }
}

/// Compiler-minted, content-addressed value-schema identity.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppValueSchemaContract {
    schema_ref: AppReference,
    content_digest: AppDigest,
    fields: BTreeMap<AppFieldPath, AppValueFieldContract>,
}

impl AppValueSchemaContract {
    pub fn from_compiled_fields(
        fields: BTreeMap<AppFieldPath, AppValueFieldContract>,
    ) -> Result<Self, AppValueMappingError> {
        validate_compiled_fields(&fields)?;
        let bytes = canonical_json_bytes(&serde_json::to_value(&fields)?)?;
        let content_digest = AppDigest::blake3(&bytes);
        let schema_ref = AppReference::parse(format!("schema:{}", content_digest.as_str()))?;
        Ok(Self {
            schema_ref,
            content_digest,
            fields,
        })
    }

    /// Reuse the deterministic mapping compiler for an already compiled
    /// workflow-value schema without replacing that schema's exact identity.
    /// Only crate-owned compilers can call this constructor; transport input
    /// cannot mint an executable schema contract from a reference alone.
    pub(crate) fn from_exact_compiled_fields(
        schema_ref: AppReference,
        content_digest: AppDigest,
        fields: BTreeMap<AppFieldPath, AppValueFieldContract>,
    ) -> Result<Self, AppValueMappingError> {
        validate_compiled_fields(&fields)?;
        Ok(Self {
            schema_ref,
            content_digest,
            fields,
        })
    }

    pub fn schema_ref(&self) -> &AppReference {
        &self.schema_ref
    }

    pub fn content_digest(&self) -> &AppDigest {
        &self.content_digest
    }

    pub fn field(&self, path: &AppFieldPath) -> Option<&AppValueFieldContract> {
        self.fields.get(path)
    }

    pub fn fields(&self) -> impl Iterator<Item = (&AppFieldPath, &AppValueFieldContract)> {
        self.fields.iter()
    }
}

fn validate_compiled_fields(
    fields: &BTreeMap<AppFieldPath, AppValueFieldContract>,
) -> Result<(), AppValueMappingError> {
    let limit = AppContractLimits::default().max_collection_items();
    if fields.is_empty() || fields.len() > limit {
        return Err(AppValueMappingError::FieldLimit { limit });
    }
    let paths = fields.keys().collect::<Vec<_>>();
    for (index, field) in paths.iter().enumerate() {
        if paths[..index]
            .iter()
            .any(|prior| field_paths_overlap(prior, field))
        {
            return Err(AppValueMappingError::OverlappingSchemaField(
                field.to_string(),
            ));
        }
    }
    for (field, contract) in fields {
        if (contract.kind == AppQueryScalarKind::Enum) != !contract.enum_values.is_empty()
            || contract.enum_values.len() > limit
        {
            return Err(AppValueMappingError::InvalidEnumField(field.to_string()));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppValueSchemaCompatibility {
    Exact,
    BackwardCompatible,
    MappingRequired,
    Incompatible,
}

pub fn value_schema_compatibility(
    source: &AppValueSchemaContract,
    target: &AppValueSchemaContract,
) -> AppValueSchemaCompatibility {
    if source.content_digest == target.content_digest {
        return AppValueSchemaCompatibility::Exact;
    }
    let mut mapping_required = false;
    for (target_path, target_field) in &target.fields {
        let Some(source_field) = source.fields.get(target_path) else {
            if target_field.required {
                return AppValueSchemaCompatibility::MappingRequired;
            }
            continue;
        };
        if source_field.kind != target_field.kind {
            if registered_conversion(source_field.kind, target_field.kind).is_some() {
                mapping_required = true;
            } else {
                return AppValueSchemaCompatibility::Incompatible;
            }
        }
        if source_field.kind == AppQueryScalarKind::Enum
            && !source_field
                .enum_values
                .is_subset(&target_field.enum_values)
        {
            mapping_required = true;
        }
        if target_field.required && !source_field.required {
            mapping_required = true;
        }
        if source_field.nullable && !target_field.nullable {
            mapping_required = true;
        }
    }
    if mapping_required {
        AppValueSchemaCompatibility::MappingRequired
    } else {
        AppValueSchemaCompatibility::BackwardCompatible
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppRegisteredScalarConversion {
    IntegerToDecimal,
    TextToMarkdown,
    TextToTimestamp,
}

fn registered_conversion(
    source: AppQueryScalarKind,
    target: AppQueryScalarKind,
) -> Option<AppRegisteredScalarConversion> {
    match (source, target) {
        (AppQueryScalarKind::Integer, AppQueryScalarKind::Decimal) => {
            Some(AppRegisteredScalarConversion::IntegerToDecimal)
        },
        (AppQueryScalarKind::Text, AppQueryScalarKind::Markdown) => {
            Some(AppRegisteredScalarConversion::TextToMarkdown)
        },
        (AppQueryScalarKind::Text, AppQueryScalarKind::Timestamp) => {
            Some(AppRegisteredScalarConversion::TextToTimestamp)
        },
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppValueMappingOperation {
    Select {
        source: AppFieldPath,
        target: AppFieldPath,
    },
    Constant {
        target: AppFieldPath,
        value: Value,
    },
    Convert {
        source: AppFieldPath,
        target: AppFieldPath,
        conversion: AppRegisteredScalarConversion,
    },
    MapEnum {
        source: AppFieldPath,
        target: AppFieldPath,
        values: BTreeMap<AppName, AppName>,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppCompiledValueMapping {
    source_schema_ref: AppReference,
    target_schema_ref: AppReference,
    operations: Vec<AppValueMappingOperation>,
    mapping_digest: AppDigest,
}

impl AppCompiledValueMapping {
    pub fn source_schema_ref(&self) -> &AppReference {
        &self.source_schema_ref
    }

    pub fn target_schema_ref(&self) -> &AppReference {
        &self.target_schema_ref
    }

    pub fn mapping_digest(&self) -> &AppDigest {
        &self.mapping_digest
    }

    pub fn operations(&self) -> &[AppValueMappingOperation] {
        &self.operations
    }
}

pub fn compile_value_mapping(
    source: &AppValueSchemaContract,
    target: &AppValueSchemaContract,
    mut operations: Vec<AppValueMappingOperation>,
) -> Result<AppCompiledValueMapping, AppValueMappingError> {
    let limits = AppContractLimits::default();
    if operations.is_empty() || operations.len() > limits.max_collection_items() {
        return Err(AppValueMappingError::OperationLimit {
            limit: limits.max_collection_items(),
        });
    }
    let mut targets = BTreeSet::new();
    for operation in &operations {
        let target_path =
            match operation {
                AppValueMappingOperation::Select {
                    source: path,
                    target: selected_target,
                } => {
                    let source_field = source
                        .fields
                        .get(path)
                        .ok_or_else(|| AppValueMappingError::UnknownSource(path.to_string()))?;
                    let target_field = target.fields.get(selected_target).ok_or_else(|| {
                        AppValueMappingError::UnknownTarget(selected_target.to_string())
                    })?;
                    if source_field.kind != target_field.kind {
                        return Err(AppValueMappingError::ConversionRequired {
                            source_path: path.to_string(),
                            target_path: selected_target.to_string(),
                        });
                    }
                    ensure_source_can_satisfy_target(
                        path,
                        source_field,
                        selected_target,
                        target_field,
                    )?;
                    if source_field.kind == AppQueryScalarKind::Enum
                        && !source_field
                            .enum_values
                            .is_subset(&target_field.enum_values)
                    {
                        return Err(AppValueMappingError::EnumMappingRequired {
                            source_path: path.to_string(),
                            target_path: selected_target.to_string(),
                        });
                    }
                    selected_target
                },
                AppValueMappingOperation::Constant {
                    target: path,
                    value,
                } => {
                    let target_field = target
                        .fields
                        .get(path)
                        .ok_or_else(|| AppValueMappingError::UnknownTarget(path.to_string()))?;
                    validate_value_kind(value, target_field)?;
                    validate_json_value(value, &limits)?;
                    path
                },
                AppValueMappingOperation::Convert {
                    source: source_path,
                    target: target_path,
                    conversion,
                } => {
                    let source_field = source.fields.get(source_path).ok_or_else(|| {
                        AppValueMappingError::UnknownSource(source_path.to_string())
                    })?;
                    let target_field = target.fields.get(target_path).ok_or_else(|| {
                        AppValueMappingError::UnknownTarget(target_path.to_string())
                    })?;
                    if registered_conversion(source_field.kind, target_field.kind)
                        != Some(*conversion)
                    {
                        return Err(AppValueMappingError::UnregisteredConversion);
                    }
                    ensure_source_can_satisfy_target(
                        source_path,
                        source_field,
                        target_path,
                        target_field,
                    )?;
                    target_path
                },
                AppValueMappingOperation::MapEnum {
                    source: source_path,
                    target: target_path,
                    values,
                } => {
                    let source_field = source.fields.get(source_path).ok_or_else(|| {
                        AppValueMappingError::UnknownSource(source_path.to_string())
                    })?;
                    let target_field = target.fields.get(target_path).ok_or_else(|| {
                        AppValueMappingError::UnknownTarget(target_path.to_string())
                    })?;
                    if source_field.kind != AppQueryScalarKind::Enum
                        || target_field.kind != AppQueryScalarKind::Enum
                        || values.len() != source_field.enum_values.len()
                        || values.len() > limits.max_collection_items()
                        || source_field
                            .enum_values
                            .iter()
                            .any(|value| !values.contains_key(value))
                        || values
                            .values()
                            .any(|value| !target_field.enum_values.contains(value))
                    {
                        return Err(AppValueMappingError::InvalidEnumMapping);
                    }
                    ensure_source_can_satisfy_target(
                        source_path,
                        source_field,
                        target_path,
                        target_field,
                    )?;
                    target_path
                },
            };
        if !targets.insert(target_path) {
            return Err(AppValueMappingError::DuplicateTarget(
                target_path.to_string(),
            ));
        }
        if targets
            .iter()
            .any(|existing| *existing != target_path && field_paths_overlap(existing, target_path))
        {
            return Err(AppValueMappingError::OverlappingTarget(
                target_path.to_string(),
            ));
        }
    }
    for (path, contract) in &target.fields {
        if contract.required && !targets.contains(path) {
            return Err(AppValueMappingError::MissingRequiredTarget(
                path.to_string(),
            ));
        }
    }
    operations.sort_by(|left, right| operation_target(left).cmp(operation_target(right)));
    #[derive(Serialize)]
    struct MappingIdentity<'a> {
        source_schema_ref: &'a AppReference,
        target_schema_ref: &'a AppReference,
        operations: &'a [AppValueMappingOperation],
    }
    let bytes = canonical_json_bytes(&serde_json::to_value(MappingIdentity {
        source_schema_ref: source.schema_ref(),
        target_schema_ref: target.schema_ref(),
        operations: &operations,
    })?)?;
    Ok(AppCompiledValueMapping {
        source_schema_ref: source.schema_ref().clone(),
        target_schema_ref: target.schema_ref().clone(),
        operations,
        mapping_digest: AppDigest::blake3(&bytes),
    })
}

fn ensure_source_can_satisfy_target(
    source_path: &AppFieldPath,
    source: &AppValueFieldContract,
    target_path: &AppFieldPath,
    target: &AppValueFieldContract,
) -> Result<(), AppValueMappingError> {
    if (target.required && !source.required) || (source.nullable && !target.nullable) {
        return Err(AppValueMappingError::NonTotalSourceMapping {
            source_path: source_path.to_string(),
            target_path: target_path.to_string(),
        });
    }
    Ok(())
}

fn operation_target(operation: &AppValueMappingOperation) -> &AppFieldPath {
    match operation {
        AppValueMappingOperation::Select { target, .. }
        | AppValueMappingOperation::Constant { target, .. }
        | AppValueMappingOperation::Convert { target, .. }
        | AppValueMappingOperation::MapEnum { target, .. } => target,
    }
}

fn field_paths_overlap(left: &AppFieldPath, right: &AppFieldPath) -> bool {
    let left = left.as_str();
    let right = right.as_str();
    left.strip_prefix(right)
        .is_some_and(|tail| tail.starts_with('.'))
        || right
            .strip_prefix(left)
            .is_some_and(|tail| tail.starts_with('.'))
}

/// Apply an already-compiled flat mapping. Field traversal is iterative and
/// bounded by `AppFieldPath`; mapping execution never evaluates code, templates
/// or model-authored expressions.
pub fn apply_compiled_value_mapping(
    mapping: &AppCompiledValueMapping,
    source_schema: &AppValueSchemaContract,
    target_schema: &AppValueSchemaContract,
    input: &Value,
) -> Result<Value, AppValueMappingError> {
    if mapping.source_schema_ref != source_schema.schema_ref
        || mapping.target_schema_ref != target_schema.schema_ref
    {
        return Err(AppValueMappingError::SchemaIdentityMismatch);
    }
    if !input.is_object() {
        return Err(AppValueMappingError::InputMustBeObject);
    }
    validate_value_against_schema(input, source_schema)?;

    let mut output = serde_json::Map::new();
    for operation in &mapping.operations {
        let mapped = match operation {
            AppValueMappingOperation::Select { source, target } => {
                read_path_optional(input, source).map(|value| (target, value.clone()))
            },
            AppValueMappingOperation::Constant { target, value } => Some((target, value.clone())),
            AppValueMappingOperation::Convert {
                source,
                target,
                conversion,
            } => match read_path_optional(input, source) {
                Some(Value::Null) => Some((target, Value::Null)),
                Some(value) => Some((target, apply_conversion(value, *conversion)?)),
                None => None,
            },
            AppValueMappingOperation::MapEnum {
                source,
                target,
                values,
            } => match read_path_optional(input, source) {
                Some(Value::Null) => Some((target, Value::Null)),
                Some(value) => {
                    let value = value
                        .as_str()
                        .and_then(|value| AppName::parse(value).ok())
                        .and_then(|value| values.get(&value))
                        .ok_or(AppValueMappingError::InvalidConversionInput)?;
                    Some((target, Value::String(value.to_string())))
                },
                None => None,
            },
        };
        if let Some((target, value)) = mapped {
            write_path(&mut output, target, value)?;
        }
    }

    let output = Value::Object(output);
    validate_value_against_schema(&output, target_schema)?;
    Ok(output)
}

/// Validate a deterministic or model-derived candidate against the exact
/// compiler-minted destination schema. Model-derived callers must additionally
/// pass the accepted value through `policy::join_app_content` so the complete
/// source-policy/provenance join remains attached.
pub fn validate_value_against_schema(
    value: &Value,
    schema: &AppValueSchemaContract,
) -> Result<(), AppValueMappingError> {
    let limits = AppContractLimits::default();
    validate_json_value(value, &limits)?;
    let object = value
        .as_object()
        .ok_or(AppValueMappingError::InputMustBeObject)?;
    let mut stack = object
        .iter()
        .map(|(segment, value)| (segment.clone(), value))
        .collect::<Vec<_>>();
    let mut leaves = BTreeSet::new();
    while let Some((path, value)) = stack.pop() {
        let path = AppFieldPath::parse(path)?;
        if let Some(object) = value.as_object() {
            if object.is_empty() {
                return Err(AppValueMappingError::UnknownOutputField(path.to_string()));
            }
            for (segment, child) in object {
                stack.push((format!("{}.{segment}", path.as_str()), child));
            }
            continue;
        }
        let contract = schema
            .fields
            .get(&path)
            .ok_or_else(|| AppValueMappingError::UnknownOutputField(path.to_string()))?;
        validate_value_kind(value, contract)?;
        leaves.insert(path);
    }
    for (path, contract) in &schema.fields {
        if contract.required && !leaves.contains(path) {
            return Err(AppValueMappingError::MissingRequiredTarget(
                path.to_string(),
            ));
        }
    }
    Ok(())
}

/// The only Phase-0 model-derived composition seam. Successful schema
/// validation is not enough: the returned value is always the canonical joined
/// content carrying every source's restrictive policy and provenance.
pub fn join_model_derived_value(
    inputs: &[RevalidatedAppEnvelope<'_>],
    derived_value: Value,
    target_schema: &AppValueSchemaContract,
    constraint: &AppHandlingConstraint,
    limits: &AppContractLimits,
) -> Result<AppJoinedContent, AppModelDerivedValueError> {
    validate_value_against_schema(&derived_value, target_schema)?;
    join_app_content(inputs, derived_value, constraint, limits).map_err(Into::into)
}

fn read_path_optional<'a>(mut value: &'a Value, path: &AppFieldPath) -> Option<&'a Value> {
    for segment in path.as_str().split('.') {
        value = value.as_object()?.get(segment)?;
    }
    Some(value)
}

fn write_path(
    root: &mut serde_json::Map<String, Value>,
    path: &AppFieldPath,
    value: Value,
) -> Result<(), AppValueMappingError> {
    let mut segments = path.as_str().split('.').peekable();
    let mut current = root;
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            current.insert(segment.to_owned(), value);
            return Ok(());
        }
        let child = current
            .entry(segment.to_owned())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        current = child
            .as_object_mut()
            .ok_or_else(|| AppValueMappingError::TargetPathConflict(path.to_string()))?;
    }
    Err(AppValueMappingError::TargetPathConflict(path.to_string()))
}

fn apply_conversion(
    value: &Value,
    conversion: AppRegisteredScalarConversion,
) -> Result<Value, AppValueMappingError> {
    match conversion {
        AppRegisteredScalarConversion::IntegerToDecimal => value
            .as_i64()
            .map(Value::from)
            .or_else(|| value.as_u64().map(Value::from))
            .ok_or(AppValueMappingError::InvalidConversionInput),
        AppRegisteredScalarConversion::TextToMarkdown => value
            .as_str()
            .map(|value| Value::String(value.to_owned()))
            .ok_or(AppValueMappingError::InvalidConversionInput),
        AppRegisteredScalarConversion::TextToTimestamp => {
            let text = value
                .as_str()
                .ok_or(AppValueMappingError::InvalidConversionInput)?;
            let timestamp = chrono::DateTime::parse_from_rfc3339(text)
                .map_err(|_| AppValueMappingError::InvalidConversionInput)?
                .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true);
            Ok(Value::String(timestamp))
        },
    }
}

fn validate_value_kind(
    value: &Value,
    field: &AppValueFieldContract,
) -> Result<(), AppValueMappingError> {
    if value.is_null() {
        return if field.nullable {
            Ok(())
        } else {
            Err(AppValueMappingError::InvalidConstant)
        };
    }
    let valid = match field.kind {
        AppQueryScalarKind::Text | AppQueryScalarKind::Markdown => value.is_string(),
        AppQueryScalarKind::Enum => value.as_str().is_some_and(|value| {
            AppName::parse(value)
                .ok()
                .is_some_and(|value| field.enum_values.contains(&value))
        }),
        AppQueryScalarKind::Reference => value
            .as_str()
            .is_some_and(|value| AppRecordId::parse(value).is_ok()),
        AppQueryScalarKind::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
        AppQueryScalarKind::Decimal => value.is_number(),
        AppQueryScalarKind::Boolean => value.is_boolean(),
        AppQueryScalarKind::Timestamp => value
            .as_str()
            .is_some_and(|value| chrono::DateTime::parse_from_rfc3339(value).is_ok()),
    };
    if valid {
        Ok(())
    } else {
        Err(AppValueMappingError::InvalidConstant)
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppValueMappingError {
    #[error(transparent)]
    Contract(#[from] super::models::AppContractError),
    #[error("value schema must contain between 1 and {limit} fields")]
    FieldLimit { limit: usize },
    #[error("value schema field `{0}` overlaps another scalar field path")]
    OverlappingSchemaField(String),
    #[error("value schema enum field `{0}` has an empty, oversized or misplaced value set")]
    InvalidEnumField(String),
    #[error("value mapping must contain between 1 and {limit} operations")]
    OperationLimit { limit: usize },
    #[error("value mapping references unknown source field `{0}`")]
    UnknownSource(String),
    #[error("value mapping references unknown target field `{0}`")]
    UnknownTarget(String),
    #[error(
        "value mapping from `{source_path}` to `{target_path}` requires an explicit conversion"
    )]
    ConversionRequired {
        source_path: String,
        target_path: String,
    },
    #[error(
        "value mapping from `{source_path}` to `{target_path}` is not total for every valid \
         source value"
    )]
    NonTotalSourceMapping {
        source_path: String,
        target_path: String,
    },
    #[error(
        "enum mapping from `{source_path}` to `{target_path}` requires an explicit total value map"
    )]
    EnumMappingRequired {
        source_path: String,
        target_path: String,
    },
    #[error("value mapping requests an unregistered scalar conversion")]
    UnregisteredConversion,
    #[error("value mapping enum map must exactly cover source values with valid target values")]
    InvalidEnumMapping,
    #[error("value mapping writes target field `{0}` more than once")]
    DuplicateTarget(String),
    #[error("value mapping target field `{0}` overlaps another target path")]
    OverlappingTarget(String),
    #[error("value mapping omits required target field `{0}`")]
    MissingRequiredTarget(String),
    #[error("value mapping constant does not match the target field contract")]
    InvalidConstant,
    #[error("value mapping schema identity does not match the compiled source and target")]
    SchemaIdentityMismatch,
    #[error("value mapping input must be a JSON object")]
    InputMustBeObject,
    #[error("value mapping target path `{0}` conflicts with another mapped object path")]
    TargetPathConflict(String),
    #[error("value mapping conversion input does not satisfy its registered conversion")]
    InvalidConversionInput,
    #[error("mapped or model-derived value contains unknown field `{0}`")]
    UnknownOutputField(String),
    #[error("failed to encode value mapping identity: {0}")]
    Encoding(String),
}

impl From<serde_json::Error> for AppValueMappingError {
    fn from(error: serde_json::Error) -> Self {
        Self::Encoding(error.to_string())
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppModelDerivedValueError {
    #[error(transparent)]
    Mapping(#[from] AppValueMappingError),
    #[error(transparent)]
    Policy(#[from] AppPolicyError),
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn path(value: &str) -> AppFieldPath {
        AppFieldPath::parse(value).unwrap()
    }

    fn schema(fields: &[(&str, AppQueryScalarKind, bool)]) -> AppValueSchemaContract {
        AppValueSchemaContract::from_compiled_fields(
            fields
                .iter()
                .map(|(name, kind, required)| {
                    (
                        path(name),
                        AppValueFieldContract {
                            kind: *kind,
                            required: *required,
                            nullable: false,
                            enum_values: BTreeSet::new(),
                        },
                    )
                })
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn schema_identity_is_content_addressed_and_order_independent() {
        let left = schema(&[
            ("title", AppQueryScalarKind::Text, true),
            ("count", AppQueryScalarKind::Integer, false),
        ]);
        let right = schema(&[
            ("count", AppQueryScalarKind::Integer, false),
            ("title", AppQueryScalarKind::Text, true),
        ]);
        assert_eq!(left, right);
        assert!(left.schema_ref().as_str().starts_with("schema:blake3:"));
    }

    #[test]
    fn compatibility_distinguishes_exact_safe_mapping_and_incompatible() {
        let source = schema(&[("count", AppQueryScalarKind::Integer, true)]);
        assert_eq!(
            value_schema_compatibility(&source, &source),
            AppValueSchemaCompatibility::Exact
        );
        let decimal = schema(&[("count", AppQueryScalarKind::Decimal, true)]);
        assert_eq!(
            value_schema_compatibility(&source, &decimal),
            AppValueSchemaCompatibility::MappingRequired
        );
        let boolean = schema(&[("count", AppQueryScalarKind::Boolean, true)]);
        assert_eq!(
            value_schema_compatibility(&source, &boolean),
            AppValueSchemaCompatibility::Incompatible
        );

        let optional_source = AppValueSchemaContract::from_compiled_fields(BTreeMap::from([(
            path("count"),
            AppValueFieldContract {
                kind: AppQueryScalarKind::Integer,
                required: false,
                nullable: false,
                enum_values: BTreeSet::new(),
            },
        )]))
        .unwrap();
        assert_eq!(
            value_schema_compatibility(&optional_source, &source),
            AppValueSchemaCompatibility::MappingRequired
        );
    }

    #[test]
    fn mapping_is_flat_bounded_and_accepts_only_registered_conversions() {
        let source = schema(&[("count", AppQueryScalarKind::Integer, true)]);
        let target = schema(&[("amount", AppQueryScalarKind::Decimal, true)]);
        let compiled = compile_value_mapping(
            &source,
            &target,
            vec![AppValueMappingOperation::Convert {
                source: path("count"),
                target: path("amount"),
                conversion: AppRegisteredScalarConversion::IntegerToDecimal,
            }],
        )
        .unwrap();
        assert!(compiled.mapping_digest.as_str().starts_with("blake3:"));
        static_assertions::assert_not_impl_any!(
            AppValueSchemaContract: serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppCompiledValueMapping: serde::de::DeserializeOwned
        );
    }

    #[test]
    fn required_nullable_fields_are_valid_and_mapping_executes_without_code() {
        let source = schema(&[("source.count", AppQueryScalarKind::Integer, true)]);
        let target = AppValueSchemaContract::from_compiled_fields(BTreeMap::from([(
            path("result.amount"),
            AppValueFieldContract {
                kind: AppQueryScalarKind::Decimal,
                required: true,
                nullable: true,
                enum_values: BTreeSet::new(),
            },
        )]))
        .unwrap();
        let compiled = compile_value_mapping(
            &source,
            &target,
            vec![AppValueMappingOperation::Convert {
                source: path("source.count"),
                target: path("result.amount"),
                conversion: AppRegisteredScalarConversion::IntegerToDecimal,
            }],
        )
        .unwrap();
        let output = apply_compiled_value_mapping(
            &compiled,
            &source,
            &target,
            &serde_json::json!({"source": {"count": 7}}),
        )
        .unwrap();
        assert_eq!(output, serde_json::json!({"result": {"amount": 7}}));
        static_assertions::assert_not_impl_any!(
            AppValueFieldContract: serde::de::DeserializeOwned
        );
    }

    #[test]
    fn schema_compiler_rejects_overlapping_scalar_paths() {
        assert!(matches!(
            AppValueSchemaContract::from_compiled_fields(BTreeMap::from([
                (
                    path("result"),
                    AppValueFieldContract {
                        kind: AppQueryScalarKind::Text,
                        required: true,
                        nullable: false,
                        enum_values: BTreeSet::new(),
                    },
                ),
                (
                    path("result.title"),
                    AppValueFieldContract {
                        kind: AppQueryScalarKind::Text,
                        required: true,
                        nullable: false,
                        enum_values: BTreeSet::new(),
                    },
                ),
            ])),
            Err(AppValueMappingError::OverlappingSchemaField(_))
        ));
    }

    #[test]
    fn model_derived_candidate_cannot_smuggle_unknown_fields() {
        let target = schema(&[("title", AppQueryScalarKind::Text, true)]);
        assert!(
            validate_value_against_schema(&serde_json::json!({"title": "safe"}), &target,).is_ok()
        );
        assert!(matches!(
            validate_value_against_schema(
                &serde_json::json!({"title": "safe", "hidden": "extra"}),
                &target,
            ),
            Err(AppValueMappingError::UnknownOutputField(_))
        ));

        let constraint = AppHandlingConstraint {
            classification_floor: super::super::models::AppDataClassification::Ordinary,
            model_processing: super::super::models::AppModelProcessing::None,
            policy_digest: AppDigest::blake3(b"target-policy"),
            purpose: AppReference::parse("purpose:mapping").unwrap(),
            audience_ref: AppReference::parse("audience:owner").unwrap(),
        };
        assert!(matches!(
            join_model_derived_value(
                &[],
                serde_json::json!({"title": "schema-valid but unprovenanced"}),
                &target,
                &constraint,
                &AppContractLimits::default(),
            ),
            Err(AppModelDerivedValueError::Policy(
                AppPolicyError::UnlabeledContent
            ))
        ));
    }

    #[test]
    fn mapping_identity_is_independent_of_declaration_order() {
        let source = schema(&[
            ("first", AppQueryScalarKind::Text, true),
            ("second", AppQueryScalarKind::Text, true),
        ]);
        let target = schema(&[
            ("alpha", AppQueryScalarKind::Text, true),
            ("beta", AppQueryScalarKind::Text, true),
        ]);
        let first = AppValueMappingOperation::Select {
            source: path("first"),
            target: path("alpha"),
        };
        let second = AppValueMappingOperation::Select {
            source: path("second"),
            target: path("beta"),
        };
        let forward =
            compile_value_mapping(&source, &target, vec![first.clone(), second.clone()]).unwrap();
        let reverse = compile_value_mapping(&source, &target, vec![second, first]).unwrap();
        assert_eq!(forward, reverse);
    }

    #[test]
    fn enum_mapping_is_total_and_uses_only_declared_target_values() {
        let enum_field = |values: &[&str]| AppValueFieldContract {
            kind: AppQueryScalarKind::Enum,
            required: true,
            nullable: false,
            enum_values: values
                .iter()
                .map(|value| AppName::parse(*value).unwrap())
                .collect(),
        };
        let source = AppValueSchemaContract::from_compiled_fields(BTreeMap::from([(
            path("status"),
            enum_field(&["new", "done"]),
        )]))
        .unwrap();
        let target = AppValueSchemaContract::from_compiled_fields(BTreeMap::from([(
            path("state"),
            enum_field(&["open", "closed"]),
        )]))
        .unwrap();
        let operation = AppValueMappingOperation::MapEnum {
            source: path("status"),
            target: path("state"),
            values: BTreeMap::from([
                (
                    AppName::parse("new").unwrap(),
                    AppName::parse("open").unwrap(),
                ),
                (
                    AppName::parse("done").unwrap(),
                    AppName::parse("closed").unwrap(),
                ),
            ]),
        };
        let compiled = compile_value_mapping(&source, &target, vec![operation]).unwrap();
        let output = apply_compiled_value_mapping(
            &compiled,
            &source,
            &target,
            &serde_json::json!({"status": "new"}),
        )
        .unwrap();
        assert_eq!(output, serde_json::json!({"state": "open"}));

        assert!(matches!(
            compile_value_mapping(
                &source,
                &target,
                vec![AppValueMappingOperation::MapEnum {
                    source: path("status"),
                    target: path("state"),
                    values: BTreeMap::from([(
                        AppName::parse("new").unwrap(),
                        AppName::parse("open").unwrap(),
                    )]),
                }],
            ),
            Err(AppValueMappingError::InvalidEnumMapping)
        ));
    }

    #[test]
    fn compiler_rejects_non_total_optional_nullable_and_enum_selects() {
        let optional_source = AppValueSchemaContract::from_compiled_fields(BTreeMap::from([(
            path("title"),
            AppValueFieldContract {
                kind: AppQueryScalarKind::Text,
                required: false,
                nullable: false,
                enum_values: BTreeSet::new(),
            },
        )]))
        .unwrap();
        let required_target = schema(&[("headline", AppQueryScalarKind::Text, true)]);
        assert!(matches!(
            compile_value_mapping(
                &optional_source,
                &required_target,
                vec![AppValueMappingOperation::Select {
                    source: path("title"),
                    target: path("headline"),
                }],
            ),
            Err(AppValueMappingError::NonTotalSourceMapping { .. })
        ));

        let nullable_source = AppValueSchemaContract::from_compiled_fields(BTreeMap::from([(
            path("count"),
            AppValueFieldContract {
                kind: AppQueryScalarKind::Integer,
                required: true,
                nullable: true,
                enum_values: BTreeSet::new(),
            },
        )]))
        .unwrap();
        let nonnullable_target = schema(&[("amount", AppQueryScalarKind::Decimal, true)]);
        assert!(matches!(
            compile_value_mapping(
                &nullable_source,
                &nonnullable_target,
                vec![AppValueMappingOperation::Convert {
                    source: path("count"),
                    target: path("amount"),
                    conversion: AppRegisteredScalarConversion::IntegerToDecimal,
                }],
            ),
            Err(AppValueMappingError::NonTotalSourceMapping { .. })
        ));

        let enum_field = |values: &[&str]| AppValueFieldContract {
            kind: AppQueryScalarKind::Enum,
            required: true,
            nullable: false,
            enum_values: values
                .iter()
                .map(|value| AppName::parse(*value).unwrap())
                .collect(),
        };
        let source = AppValueSchemaContract::from_compiled_fields(BTreeMap::from([(
            path("status"),
            enum_field(&["new", "done"]),
        )]))
        .unwrap();
        let target = AppValueSchemaContract::from_compiled_fields(BTreeMap::from([(
            path("state"),
            enum_field(&["new"]),
        )]))
        .unwrap();
        assert!(matches!(
            compile_value_mapping(
                &source,
                &target,
                vec![AppValueMappingOperation::Select {
                    source: path("status"),
                    target: path("state"),
                }],
            ),
            Err(AppValueMappingError::EnumMappingRequired { .. })
        ));
    }

    #[test]
    fn optional_sources_are_omitted_and_nullable_conversions_propagate_null() {
        let source = AppValueSchemaContract::from_compiled_fields(BTreeMap::from([
            (
                path("title"),
                AppValueFieldContract {
                    kind: AppQueryScalarKind::Text,
                    required: false,
                    nullable: false,
                    enum_values: BTreeSet::new(),
                },
            ),
            (
                path("count"),
                AppValueFieldContract {
                    kind: AppQueryScalarKind::Integer,
                    required: true,
                    nullable: true,
                    enum_values: BTreeSet::new(),
                },
            ),
        ]))
        .unwrap();
        let target = AppValueSchemaContract::from_compiled_fields(BTreeMap::from([
            (
                path("headline"),
                AppValueFieldContract {
                    kind: AppQueryScalarKind::Text,
                    required: false,
                    nullable: false,
                    enum_values: BTreeSet::new(),
                },
            ),
            (
                path("amount"),
                AppValueFieldContract {
                    kind: AppQueryScalarKind::Decimal,
                    required: true,
                    nullable: true,
                    enum_values: BTreeSet::new(),
                },
            ),
        ]))
        .unwrap();
        let mapping = compile_value_mapping(
            &source,
            &target,
            vec![
                AppValueMappingOperation::Select {
                    source: path("title"),
                    target: path("headline"),
                },
                AppValueMappingOperation::Convert {
                    source: path("count"),
                    target: path("amount"),
                    conversion: AppRegisteredScalarConversion::IntegerToDecimal,
                },
            ],
        )
        .unwrap();
        assert_eq!(
            apply_compiled_value_mapping(
                &mapping,
                &source,
                &target,
                &serde_json::json!({"count": null}),
            )
            .unwrap(),
            serde_json::json!({"amount": null})
        );
        assert!(matches!(
            apply_compiled_value_mapping(
                &mapping,
                &source,
                &target,
                &serde_json::json!({"count": null, "invented": "field"}),
            ),
            Err(AppValueMappingError::UnknownOutputField(_))
        ));
    }
}

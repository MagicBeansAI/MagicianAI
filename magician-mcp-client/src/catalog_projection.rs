//! Atomic, policy-first projection of SDK-validated MCP descriptors.
//!
//! Discovery remains owned by `McpClient`; this module performs no transport, OAuth,
//! authorization, product publication, or tool execution. It first applies the trusted
//! local policy, then validates every eligible descriptor and removes untrusted
//! JSON-Schema annotations plus model-unsafe regex prose. A snapshot is returned only
//! after every eligible entry passes.

use std::{collections::BTreeSet, error::Error, fmt};

use serde::Serialize;
use serde_json::{Map, Value};
use tool_runtime_core::mcp_catalog_policy::{
    McpCatalogPolicyContract, McpEffectiveToolPolicy, McpToolPublicationDecision,
};

use crate::{
    validation::measure_object, McpClientLimits, McpToolDescriptor, McpToolHints, McpToolId,
};

pub const MCP_PROJECTED_CATALOG_V1: &str = "magician-mcp.projected-catalog.v1";
const FALLBACK_MODEL_DESCRIPTION: &str = "A governed external tool.";
const MAX_SCHEMA_PROPERTY_NAME_BYTES: usize = 256;
const MAX_SCHEMA_PATTERN_BYTES: usize = 1024;
const MAX_SCHEMA_SCALAR_STRING_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpCatalogProjectionErrorCode {
    PolicyRejected,
    TooManyTools,
    DuplicateRemoteName,
    InvalidLocalName,
    LocalNameCollision,
    UnsupportedInputSchema,
    UnsupportedOutputSchema,
    CatalogTooLarge,
}

/// Stable value-free projection failure. Remote names, schema keys, and annotation
/// values never enter the diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct McpCatalogProjectionError {
    pub code: McpCatalogProjectionErrorCode,
    pub field: &'static str,
    pub message: &'static str,
}

impl McpCatalogProjectionError {
    const fn new(
        code: McpCatalogProjectionErrorCode,
        field: &'static str,
        message: &'static str,
    ) -> Self {
        Self {
            code,
            field,
            message,
        }
    }
}

impl fmt::Display for McpCatalogProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for McpCatalogProjectionError {}

/// Complete immutable result for one skill-owned namespace.
///
/// The type deliberately has no blanket serialization path. Model-facing serialization
/// is available only through `McpProjectedTool::model_definition`, which excludes remote
/// titles, descriptions, hints, output schemas, ids, and policy internals.
#[derive(PartialEq)]
pub struct McpProjectedCatalog {
    schema_version: &'static str,
    namespace: String,
    tools: Vec<McpProjectedTool>,
    accounted_bytes: usize,
}

impl McpProjectedCatalog {
    pub fn schema_version(&self) -> &'static str {
        self.schema_version
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn tools(&self) -> &[McpProjectedTool] {
        &self.tools
    }

    /// Conservative payload accounting retained from the policy-first projection.
    /// It includes local names, trusted descriptions, and raw input/output schemas
    /// before annotation stripping, so later product aggregation cannot hide remote
    /// amplification behind normalization.
    pub fn accounted_bytes(&self) -> usize {
        self.accounted_bytes
    }

    #[cfg(test)]
    pub(crate) fn rewrite_local_name_for_test(&mut self, index: usize, local_name: String) {
        self.tools[index].local_name = local_name;
    }
}

impl fmt::Debug for McpProjectedCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpProjectedCatalog")
            .field("schema_version", &self.schema_version)
            .field("namespace", &self.namespace)
            .field("tool_count", &self.tools.len())
            .field("accounted_bytes", &self.accounted_bytes)
            .finish()
    }
}

#[derive(PartialEq)]
pub struct McpProjectedTool {
    local_name: String,
    remote_id: McpToolId,
    input_schema: Map<String, Value>,
    output_schema: Option<Map<String, Value>>,
    remote_hints: McpToolHints,
    policy: McpEffectiveToolPolicy,
}

impl fmt::Debug for McpProjectedTool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpProjectedTool")
            .field("local_name", &"<redacted>")
            .field("has_output_schema", &self.output_schema.is_some())
            .field("risk", &self.policy.risk())
            .finish()
    }
}

impl McpProjectedTool {
    pub fn local_name(&self) -> &str {
        &self.local_name
    }

    pub fn remote_id(&self) -> &McpToolId {
        &self.remote_id
    }

    pub fn trusted_description(&self) -> Option<&str> {
        self.policy.trusted_description()
    }

    pub fn input_schema(&self) -> &Map<String, Value> {
        &self.input_schema
    }

    pub fn output_schema(&self) -> Option<&Map<String, Value>> {
        self.output_schema.as_ref()
    }

    /// Display-only remote hints. They are structurally absent from `policy()` and the
    /// model-facing definition and therefore cannot lower local authorization controls.
    pub fn remote_hints(&self) -> &McpToolHints {
        &self.remote_hints
    }

    pub fn policy(&self) -> &McpEffectiveToolPolicy {
        &self.policy
    }

    pub fn model_definition(&self) -> McpModelToolDefinition<'_> {
        McpModelToolDefinition {
            name: &self.local_name,
            description: self
                .policy
                .trusted_description()
                .unwrap_or(FALLBACK_MODEL_DESCRIPTION),
            parameters: &self.input_schema,
        }
    }
}

/// The only serializable 5D2 model-catalog view.
#[derive(Serialize)]
pub struct McpModelToolDefinition<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a Map<String, Value>,
}

impl fmt::Debug for McpModelToolDefinition<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpModelToolDefinition")
            .field("name", &"<redacted>")
            .field("description_bytes", &self.description.len())
            .field("parameter_count", &self.parameters.len())
            .finish()
    }
}

impl<'a> McpModelToolDefinition<'a> {
    pub fn name(&self) -> &'a str {
        self.name
    }

    pub fn description(&self) -> &'a str {
        self.description
    }

    pub fn parameters(&self) -> &'a Map<String, Value> {
        self.parameters
    }
}

/// Apply one trusted local policy to a complete SDK-validated descriptor snapshot.
///
/// The function mutates no product or client state. All work stays in temporary local
/// collections; any error drops the candidate and returns no partial snapshot.
pub fn project_mcp_catalog(
    descriptors: Vec<McpToolDescriptor>,
    policy: &McpCatalogPolicyContract,
) -> Result<McpProjectedCatalog, McpCatalogProjectionError> {
    let limits = policy.catalog_limits();
    let schema_limits = McpClientLimits {
        max_schema_bytes: limits.max_schema_bytes,
        max_json_depth: limits.max_schema_depth,
        max_json_nodes: limits.max_schema_nodes,
        ..McpClientLimits::default()
    };
    let mut tools = Vec::new();
    let mut remote_names = BTreeSet::new();
    let mut local_names = BTreeSet::new();
    let mut catalog_bytes = 0usize;
    let mut shared_policy_charged = false;
    add_catalog_bytes(
        &mut catalog_bytes,
        policy.local_namespace().len(),
        limits.max_catalog_bytes,
    )?;

    for descriptor in descriptors {
        let decision = policy
            .decision(descriptor.id.remote_name())
            .map_err(|_| policy_rejected())?;
        let McpToolPublicationDecision::Publish(effective_policy) = decision else {
            continue;
        };
        if tools.len() >= limits.max_tools {
            return Err(McpCatalogProjectionError::new(
                McpCatalogProjectionErrorCode::TooManyTools,
                "tools",
                "the eligible MCP tool count exceeds its local catalog limit",
            ));
        }
        if !remote_names.insert(descriptor.id.remote_name().to_owned()) {
            return Err(McpCatalogProjectionError::new(
                McpCatalogProjectionErrorCode::DuplicateRemoteName,
                "remote_tool_name",
                "the eligible MCP catalog contains a duplicate remote name",
            ));
        }
        let local_name = policy
            .local_tool_name(descriptor.id.remote_name())
            .map_err(|_| {
                McpCatalogProjectionError::new(
                    McpCatalogProjectionErrorCode::InvalidLocalName,
                    "local_tool_name",
                    "an eligible MCP tool cannot be represented by the local naming contract",
                )
            })?;
        if !local_names.insert(local_name.clone()) {
            return Err(McpCatalogProjectionError::new(
                McpCatalogProjectionErrorCode::LocalNameCollision,
                "local_tool_name",
                "eligible MCP tools collide in the local namespace",
            ));
        }

        add_catalog_bytes(
            &mut catalog_bytes,
            local_name.len(),
            limits.max_catalog_bytes,
        )?;
        add_catalog_bytes(
            &mut catalog_bytes,
            descriptor.id.remote_name().len(),
            limits.max_catalog_bytes,
        )?;
        if !shared_policy_charged {
            add_catalog_bytes(
                &mut catalog_bytes,
                effective_policy.shared_policy_accounted_bytes(),
                limits.max_catalog_bytes,
            )?;
            shared_policy_charged = true;
        }
        add_catalog_bytes(
            &mut catalog_bytes,
            effective_policy.tool_policy_accounted_bytes(),
            limits.max_catalog_bytes,
        )?;

        let input_measurement = measure_object(
            &descriptor.input_schema,
            limits.max_schema_bytes,
            &schema_limits,
            "projected MCP input schema",
        )
        .map_err(|_| unsupported_input_schema())?;
        add_catalog_bytes(
            &mut catalog_bytes,
            input_measurement.bytes,
            limits.max_catalog_bytes,
        )?;
        validate_supported_schema(&descriptor.input_schema, SchemaRoot::Input, &limits)
            .map_err(|_| unsupported_input_schema())?;

        let output_measurement = descriptor
            .output_schema
            .as_ref()
            .map(|schema| {
                measure_object(
                    schema,
                    limits.max_schema_bytes,
                    &schema_limits,
                    "projected MCP output schema",
                )
            })
            .transpose()
            .map_err(|_| unsupported_output_schema())?;
        if let Some(measurement) = output_measurement {
            add_catalog_bytes(
                &mut catalog_bytes,
                measurement.bytes,
                limits.max_catalog_bytes,
            )?;
        }
        if let Some(schema) = descriptor.output_schema.as_ref() {
            validate_supported_schema(schema, SchemaRoot::Output, &limits)
                .map_err(|_| unsupported_output_schema())?;
        }

        let McpToolDescriptor {
            id,
            title: _,
            description: _,
            input_schema,
            output_schema,
            hints,
        } = descriptor;
        let input_schema =
            strip_schema_annotations(input_schema).map_err(|_| unsupported_input_schema())?;
        let output_schema = output_schema
            .map(strip_schema_annotations)
            .transpose()
            .map_err(|_| unsupported_output_schema())?;

        tools.push(McpProjectedTool {
            local_name,
            remote_id: id,
            input_schema,
            output_schema,
            remote_hints: hints,
            policy: effective_policy,
        });
    }

    tools.sort_by(|left, right| left.local_name.cmp(&right.local_name));
    Ok(McpProjectedCatalog {
        schema_version: MCP_PROJECTED_CATALOG_V1,
        namespace: policy.local_namespace().to_owned(),
        tools,
        accounted_bytes: catalog_bytes,
    })
}

fn add_catalog_bytes(
    total: &mut usize,
    addition: usize,
    maximum: usize,
) -> Result<(), McpCatalogProjectionError> {
    *total = total.checked_add(addition).ok_or_else(catalog_too_large)?;
    if *total > maximum {
        return Err(catalog_too_large());
    }
    Ok(())
}

fn catalog_too_large() -> McpCatalogProjectionError {
    McpCatalogProjectionError::new(
        McpCatalogProjectionErrorCode::CatalogTooLarge,
        "catalog",
        "the projected MCP catalog exceeds its aggregate local byte limit",
    )
}

fn policy_rejected() -> McpCatalogProjectionError {
    McpCatalogProjectionError::new(
        McpCatalogProjectionErrorCode::PolicyRejected,
        "policy",
        "the trusted local MCP policy could not be applied",
    )
}

fn unsupported_input_schema() -> McpCatalogProjectionError {
    McpCatalogProjectionError::new(
        McpCatalogProjectionErrorCode::UnsupportedInputSchema,
        "input_schema",
        "an eligible MCP input schema is outside the supported bounded subset",
    )
}

fn unsupported_output_schema() -> McpCatalogProjectionError {
    McpCatalogProjectionError::new(
        McpCatalogProjectionErrorCode::UnsupportedOutputSchema,
        "output_schema",
        "an eligible MCP output schema is outside the supported bounded subset",
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchemaRoot {
    Input,
    Output,
}

fn validate_supported_schema(
    root: &Map<String, Value>,
    root_kind: SchemaRoot,
    limits: &tool_runtime_core::manifest_synthesis::SynthesizedMcpCatalogLimits,
) -> Result<(), ()> {
    let mut stack = vec![(root, true)];
    let mut schema_nodes = 0usize;
    while let Some((schema, is_root)) = stack.pop() {
        schema_nodes = schema_nodes.checked_add(1).ok_or(())?;
        if schema_nodes > limits.max_schema_nodes {
            return Err(());
        }
        for key in schema.keys() {
            if !is_supported_schema_keyword(key) && !is_stripped_annotation_keyword(key) {
                return Err(());
            }
        }
        if is_root && root_kind == SchemaRoot::Input && !is_exact_object_type(schema.get("type")) {
            return Err(());
        }
        if let Some(value) = schema.get("type") {
            validate_schema_type(value)?;
        }
        validate_keyword_type_compatibility(schema)?;

        let properties = match schema.get("properties") {
            Some(Value::Object(properties)) => Some(properties),
            Some(_) => return Err(()),
            None => None,
        };
        if let Some(properties) = properties {
            for (name, child) in properties {
                if !is_portable_schema_property_name(name) {
                    return Err(());
                }
                stack.push((child.as_object().ok_or(())?, false));
            }
        }
        if let Some(required) = schema.get("required") {
            let required = required.as_array().ok_or(())?;
            let properties = properties.ok_or(())?;
            let mut names = BTreeSet::new();
            for name in required {
                let name = name.as_str().ok_or(())?;
                if !is_portable_schema_property_name(name)
                    || !properties.contains_key(name)
                    || !names.insert(name)
                {
                    return Err(());
                }
            }
        }
        if let Some(value) = schema.get("additionalProperties") {
            match value {
                Value::Bool(_) => {},
                Value::Object(child) => stack.push((child, false)),
                _ => return Err(()),
            }
        }
        if let Some(value) = schema.get("items") {
            stack.push((value.as_object().ok_or(())?, false));
        }
        for keyword in ["anyOf", "oneOf", "allOf"] {
            if let Some(value) = schema.get(keyword) {
                let alternatives = value.as_array().ok_or(())?;
                if alternatives.is_empty() {
                    return Err(());
                }
                for alternative in alternatives {
                    stack.push((alternative.as_object().ok_or(())?, false));
                }
            }
        }
        if let Some(value) = schema.get("not") {
            stack.push((value.as_object().ok_or(())?, false));
        }
        let enum_values = if let Some(value) = schema.get("enum") {
            let values = value.as_array().ok_or(())?;
            if values.is_empty() {
                return Err(());
            }
            let mut unique = BTreeSet::new();
            for value in values {
                if !schema_accepts_scalar(schema.get("type"), value)
                    || !unique.insert(safe_scalar_key(value).ok_or(())?)
                {
                    return Err(());
                }
            }
            Some(unique)
        } else {
            None
        };
        if let Some(value) = schema.get("const") {
            if !schema_accepts_scalar(schema.get("type"), value) {
                return Err(());
            }
            let key = safe_scalar_key(value).ok_or(())?;
            if enum_values
                .as_ref()
                .is_some_and(|values| !values.contains(&key))
            {
                return Err(());
            }
        }
        for keyword in ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"] {
            if schema
                .get(keyword)
                .is_some_and(|value| value.as_f64().is_none_or(|number| !number.is_finite()))
            {
                return Err(());
            }
        }
        if schema.get("multipleOf").is_some_and(|value| {
            value
                .as_f64()
                .is_none_or(|number| !number.is_finite() || number <= 0.0)
        }) {
            return Err(());
        }
        validate_numeric_bounds(schema)?;
        for keyword in [
            "minLength",
            "maxLength",
            "minItems",
            "maxItems",
            "minProperties",
            "maxProperties",
        ] {
            if schema
                .get(keyword)
                .is_some_and(|value| value.as_u64().is_none())
            {
                return Err(());
            }
        }
        for (minimum, maximum) in [
            ("minLength", "maxLength"),
            ("minItems", "maxItems"),
            ("minProperties", "maxProperties"),
        ] {
            if let (Some(minimum), Some(maximum)) = (
                schema.get(minimum).and_then(Value::as_u64),
                schema.get(maximum).and_then(Value::as_u64),
            ) {
                if minimum > maximum {
                    return Err(());
                }
            }
        }
        if let Some(value) = schema.get("pattern") {
            let pattern = value.as_str().ok_or(())?;
            if pattern.len() > MAX_SCHEMA_PATTERN_BYTES || pattern.chars().any(char::is_control) {
                return Err(());
            }
        }
        if let Some(value) = schema.get("format") {
            let format = value.as_str().ok_or(())?;
            if !matches!(
                format,
                "date-time"
                    | "date"
                    | "time"
                    | "duration"
                    | "email"
                    | "hostname"
                    | "ipv4"
                    | "ipv6"
                    | "uuid"
                    | "uri"
                    | "uri-reference"
            ) {
                return Err(());
            }
        }
        if schema
            .get("uniqueItems")
            .is_some_and(|value| !value.is_boolean())
        {
            return Err(());
        }
    }
    Ok(())
}

fn is_exact_object_type(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::String(value)) if value == "object")
}

fn is_portable_schema_property_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SCHEMA_PROPERTY_NAME_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
}

fn validate_keyword_type_compatibility(schema: &Map<String, Value>) -> Result<(), ()> {
    for (schema_type, keywords) in [
        (
            "object",
            &[
                "properties",
                "required",
                "additionalProperties",
                "minProperties",
                "maxProperties",
            ][..],
        ),
        (
            "array",
            &["items", "minItems", "maxItems", "uniqueItems"][..],
        ),
        (
            "string",
            &["minLength", "maxLength", "pattern", "format"][..],
        ),
        (
            "number",
            &[
                "minimum",
                "maximum",
                "exclusiveMinimum",
                "exclusiveMaximum",
                "multipleOf",
            ][..],
        ),
    ] {
        if keywords.iter().any(|keyword| schema.contains_key(*keyword))
            && schema
                .get("type")
                .is_some_and(|value| !schema_type_is_compatible(value, schema_type))
        {
            return Err(());
        }
    }
    Ok(())
}

fn schema_type_is_compatible(value: &Value, expected: &str) -> bool {
    schema_type_contains(value, expected)
        || (expected == "number" && schema_type_contains(value, "integer"))
}

fn schema_type_contains(value: &Value, expected: &str) -> bool {
    match value {
        Value::String(value) => value == expected,
        Value::Array(values) => values.iter().any(|value| value.as_str() == Some(expected)),
        _ => false,
    }
}

fn validate_schema_type(value: &Value) -> Result<(), ()> {
    match value {
        Value::String(value) => is_supported_type(value).then_some(()).ok_or(()),
        Value::Array(values) if !values.is_empty() => {
            let mut types = BTreeSet::new();
            for value in values {
                let value = value.as_str().ok_or(())?;
                if !is_supported_type(value) || !types.insert(value) {
                    return Err(());
                }
            }
            Ok(())
        },
        _ => Err(()),
    }
}

fn is_supported_type(value: &str) -> bool {
    matches!(
        value,
        "null" | "boolean" | "object" | "array" | "number" | "integer" | "string"
    )
}

fn safe_scalar_key(value: &Value) -> Option<String> {
    match value {
        Value::Null => Some("null".to_owned()),
        Value::Bool(value) => Some(format!("bool:{value}")),
        Value::Number(value) if value.as_f64().is_some_and(f64::is_finite) => {
            Some(format!("number:{value}"))
        },
        Value::String(value) => (value.len() <= MAX_SCHEMA_SCALAR_STRING_BYTES
            && !value.chars().any(char::is_control))
        .then(|| format!("string:{value}")),
        Value::Number(_) | Value::Array(_) | Value::Object(_) => None,
    }
}

fn schema_accepts_scalar(schema_type: Option<&Value>, value: &Value) -> bool {
    let scalar_type = match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) | Value::Object(_) => return false,
    };
    schema_type.is_none_or(|schema_type| {
        schema_type_contains(schema_type, scalar_type)
            || (scalar_type == "integer" && schema_type_contains(schema_type, "number"))
    })
}

fn validate_numeric_bounds(schema: &Map<String, Value>) -> Result<(), ()> {
    let lower_bounds = [("minimum", false), ("exclusiveMinimum", true)];
    let upper_bounds = [("maximum", false), ("exclusiveMaximum", true)];
    for (lower, lower_exclusive) in lower_bounds {
        let Some(lower) = schema.get(lower).and_then(Value::as_f64) else {
            continue;
        };
        for (upper, upper_exclusive) in upper_bounds {
            let Some(upper) = schema.get(upper).and_then(Value::as_f64) else {
                continue;
            };
            if lower > upper || (lower == upper && (lower_exclusive || upper_exclusive)) {
                return Err(());
            }
        }
    }
    Ok(())
}

fn is_supported_schema_keyword(value: &str) -> bool {
    matches!(
        value,
        "type"
            | "properties"
            | "required"
            | "additionalProperties"
            | "items"
            | "enum"
            | "const"
            | "anyOf"
            | "oneOf"
            | "allOf"
            | "not"
            | "minimum"
            | "maximum"
            | "exclusiveMinimum"
            | "exclusiveMaximum"
            | "multipleOf"
            | "minLength"
            | "maxLength"
            | "pattern"
            | "format"
            | "minItems"
            | "maxItems"
            | "uniqueItems"
            | "minProperties"
            | "maxProperties"
    )
}

fn is_stripped_annotation_keyword(value: &str) -> bool {
    matches!(
        value,
        "$schema"
            | "$comment"
            | "title"
            | "description"
            | "default"
            | "examples"
            | "deprecated"
            | "readOnly"
            | "writeOnly"
    )
}

fn is_model_omitted_schema_keyword(value: &str) -> bool {
    is_stripped_annotation_keyword(value) || value == "pattern"
}

#[derive(Debug, Clone, Copy)]
enum SanitizeContext {
    Schema,
    Properties,
    SchemaArray,
    Literal,
}

enum SanitizeWork {
    Visit(Value, SanitizeContext),
    FinishObject(Vec<String>),
    FinishArray(usize),
}

fn strip_schema_annotations(schema: Map<String, Value>) -> Result<Map<String, Value>, ()> {
    let mut work = vec![SanitizeWork::Visit(
        Value::Object(schema),
        SanitizeContext::Schema,
    )];
    let mut completed = Vec::new();
    while let Some(item) = work.pop() {
        match item {
            SanitizeWork::Visit(value, SanitizeContext::Literal) => completed.push(value),
            SanitizeWork::Visit(Value::Object(values), SanitizeContext::Schema) => {
                let mut children = Vec::new();
                for (key, value) in values {
                    if is_model_omitted_schema_keyword(&key) {
                        continue;
                    }
                    let context = match key.as_str() {
                        "properties" => SanitizeContext::Properties,
                        "anyOf" | "oneOf" | "allOf" => SanitizeContext::SchemaArray,
                        "items" | "not" => SanitizeContext::Schema,
                        "additionalProperties" if value.is_object() => SanitizeContext::Schema,
                        _ => SanitizeContext::Literal,
                    };
                    children.push((key, value, context));
                }
                let keys = children.iter().map(|(key, _, _)| key.clone()).collect();
                work.push(SanitizeWork::FinishObject(keys));
                for (_, value, context) in children.into_iter().rev() {
                    work.push(SanitizeWork::Visit(value, context));
                }
            },
            SanitizeWork::Visit(Value::Object(values), SanitizeContext::Properties) => {
                let children = values.into_iter().collect::<Vec<_>>();
                let keys = children.iter().map(|(key, _)| key.clone()).collect();
                work.push(SanitizeWork::FinishObject(keys));
                for (_, value) in children.into_iter().rev() {
                    work.push(SanitizeWork::Visit(value, SanitizeContext::Schema));
                }
            },
            SanitizeWork::Visit(Value::Array(values), SanitizeContext::SchemaArray) => {
                let length = values.len();
                work.push(SanitizeWork::FinishArray(length));
                for value in values.into_iter().rev() {
                    work.push(SanitizeWork::Visit(value, SanitizeContext::Schema));
                }
            },
            SanitizeWork::Visit(_, _) => return Err(()),
            SanitizeWork::FinishObject(keys) => {
                if completed.len() < keys.len() {
                    return Err(());
                }
                let values = completed.split_off(completed.len() - keys.len());
                completed.push(Value::Object(keys.into_iter().zip(values).collect()));
            },
            SanitizeWork::FinishArray(length) => {
                if completed.len() < length {
                    return Err(());
                }
                let values = completed.split_off(completed.len() - length);
                completed.push(Value::Array(values));
            },
        }
    }
    if completed.len() != 1 {
        return Err(());
    }
    match completed.pop() {
        Some(Value::Object(schema)) => Ok(schema),
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        thread,
    };

    use serde_json::json;
    use static_assertions::assert_not_impl_any;
    use tool_runtime_core::{
        manifest::{
            ApprovalClass, AuthContract, McpDiscoveryPolicy, McpToolPolicy, McpToolRiskClass,
            McpTransport, PolicyFloor, RuntimeLimits, RuntimeProtocol, RuntimeRequirements,
            SkillRuntimeContract, SkillRuntimeContractVersion,
        },
        manifest_validation::validate_skill_runtime_contract,
        mcp_catalog_policy::McpCatalogPolicyContract,
    };

    use super::*;

    fn policy(discovery: McpDiscoveryPolicy) -> McpCatalogPolicyContract {
        let contract = SkillRuntimeContract {
            schema_version: SkillRuntimeContractVersion::v1(),
            requires: RuntimeRequirements::default(),
            runtime: RuntimeProtocol::Mcp {
                transport: McpTransport::StreamableHttp {
                    endpoint: "https://provider.example/mcp".to_owned(),
                },
                discovery,
                limits: RuntimeLimits::default(),
            },
            auth: AuthContract::default(),
            policy_floor: PolicyFloor {
                approval: ApprovalClass::Ordinary,
                required_grants: BTreeSet::from(["base-grant".to_owned()]),
                resource_scopes: BTreeSet::new(),
                required_resource_authorities: BTreeSet::new(),
            },
        };
        let validated = validate_skill_runtime_contract(&contract).expect("valid contract");
        McpCatalogPolicyContract::compile("provider", validated).expect("compiled policy")
    }

    fn descriptor(name: &str, input_schema: Value) -> McpToolDescriptor {
        McpToolDescriptor {
            id: McpToolId::new(name.to_owned(), 1, 1),
            title: Some("REMOTE TITLE CANARY".to_owned()),
            description: Some("REMOTE DESCRIPTION CANARY".to_owned()),
            input_schema: input_schema.as_object().expect("object schema").clone(),
            output_schema: None,
            hints: McpToolHints::default(),
        }
    }

    fn object_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "REMOTE PROPERTY DESCRIPTION CANARY",
                    "default": "REMOTE DEFAULT CANARY",
                    "minLength": 1
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    #[test]
    fn policy_filters_denied_tools_before_untrusted_schema_interpretation() {
        let policy = policy(McpDiscoveryPolicy {
            allow_tools: BTreeSet::from(["safe".to_owned(), "denied".to_owned()]),
            deny_tools: BTreeSet::from(["denied".to_owned()]),
            ..McpDiscoveryPolicy::default()
        });
        let catalog = project_mcp_catalog(
            vec![
                descriptor("denied", json!({"$ref": "https://attacker.invalid/schema"})),
                descriptor("safe", object_schema()),
            ],
            &policy,
        )
        .expect("denied schema must not influence publication");

        assert_eq!(catalog.tools().len(), 1);
        assert_eq!(catalog.tools()[0].local_name(), "provider.safe");
    }

    #[test]
    fn model_view_uses_only_trusted_prose_and_stripped_supported_schema() {
        let mut tool_policies = BTreeMap::new();
        tool_policies.insert(
            "write".to_owned(),
            McpToolPolicy {
                risk: McpToolRiskClass::WorkspaceWrite,
                trusted_description: Some("Update the selected trusted record.".to_owned()),
                ..McpToolPolicy::default()
            },
        );
        let policy = policy(McpDiscoveryPolicy {
            allow_tools: BTreeSet::from(["write".to_owned()]),
            tool_policies,
            ..McpDiscoveryPolicy::default()
        });
        let mut remote = descriptor("write", object_schema());
        remote.hints = McpToolHints {
            read_only: Some(true),
            destructive: Some(false),
            idempotent: Some(true),
            open_world: Some(false),
        };
        let catalog = project_mcp_catalog(vec![remote], &policy).unwrap();
        let tool = &catalog.tools()[0];
        let model_definition = tool.model_definition();
        assert_eq!(model_definition.name(), "provider.write");
        assert_eq!(
            model_definition.description(),
            "Update the selected trusted record."
        );
        assert_eq!(model_definition.parameters(), tool.input_schema());
        let model_json = serde_json::to_string(&model_definition).unwrap();

        assert!(model_json.contains("Update the selected trusted record."));
        assert!(!model_json.contains("REMOTE TITLE CANARY"));
        assert!(!model_json.contains("REMOTE DESCRIPTION CANARY"));
        assert!(!model_json.contains("REMOTE PROPERTY DESCRIPTION CANARY"));
        assert!(!model_json.contains("REMOTE DEFAULT CANARY"));
        assert!(!model_json.contains("read_only"));
        assert!(!model_json.contains("hints"));
        assert!(tool.remote_hints().read_only.unwrap());
        assert!(tool
            .policy()
            .required_approvals()
            .contains(&ApprovalClass::DelegatedWorkspaceWrite));
    }

    #[test]
    fn any_eligible_invalid_schema_rejects_the_whole_candidate_with_value_free_error() {
        let policy = policy(McpDiscoveryPolicy::default());
        let error = project_mcp_catalog(
            vec![
                descriptor("valid", object_schema()),
                descriptor(
                    "invalid-secret-canary",
                    json!({
                        "type": "object",
                        "$ref": "https://secret-canary.invalid/schema"
                    }),
                ),
            ],
            &policy,
        )
        .expect_err("unsupported schema must reject atomically");

        assert_eq!(
            error.code,
            McpCatalogProjectionErrorCode::UnsupportedInputSchema
        );
        let diagnostic = serde_json::to_string(&error).unwrap();
        assert!(!diagnostic.contains("invalid-secret-canary"));
        assert!(!diagnostic.contains("secret-canary"));
        assert!(!diagnostic.contains("$ref"));
    }

    #[test]
    fn required_names_formats_bounds_and_output_schema_fail_closed() {
        let policy = policy(McpDiscoveryPolicy::default());
        for schema in [
            json!({"type": "object", "properties": {}, "required": ["missing"]}),
            json!({"type": "object", "properties": {"unsafe instruction": {"type": "string"}}}),
            json!({"type": "object", "properties": {"x": {"type": "string", "format": "provider-private"}}}),
            json!({"type": "object", "properties": {"x": {"type": "string", "pattern": "unsafe\npattern"}}}),
            json!({"type": "object", "properties": {"x": {"type": "string", "pattern": "x".repeat(MAX_SCHEMA_PATTERN_BYTES + 1)}}}),
            json!({"type": "object", "properties": {"x": {"type": "string", "minLength": 3, "maxLength": 2}}}),
            json!({"type": "object", "properties": {"x": {"type": "string", "minimum": 1}}}),
            json!({"type": "object", "properties": {"x": {"type": "number", "minimum": 2, "exclusiveMaximum": 2}}}),
            json!({"type": ["object", "object"]}),
            json!({"type": "object", "properties": {"x": {"type": "string", "enum": ["unsafe\nvalue"]}}}),
            json!({"type": "object", "properties": {"x": {"type": "string", "enum": ["same", "same"]}}}),
            json!({"type": "object", "properties": {"x": {"type": "string", "enum": ["allowed"], "const": "different"}}}),
            json!({"type": "object", "properties": {"x": {"type": "string", "enum": [1]}}}),
            json!({"type": "object", "properties": {"x": {"type": "integer", "enum": [1.5]}}}),
            json!({"type": "object", "properties": {"x": {"type": "string", "enum": ["x".repeat(MAX_SCHEMA_SCALAR_STRING_BYTES + 1)]}}}),
        ] {
            assert_eq!(
                project_mcp_catalog(vec![descriptor("bad", schema)], &policy)
                    .unwrap_err()
                    .code,
                McpCatalogProjectionErrorCode::UnsupportedInputSchema
            );
        }

        let mut bad_output = descriptor("bad-output", object_schema());
        bad_output.output_schema = Some(
            json!({"$defs": {"x": {"type": "string"}}})
                .as_object()
                .unwrap()
                .clone(),
        );
        assert_eq!(
            project_mcp_catalog(vec![bad_output], &policy)
                .unwrap_err()
                .code,
            McpCatalogProjectionErrorCode::UnsupportedOutputSchema
        );
    }

    #[test]
    fn duplicate_remote_names_and_nonportable_eligible_names_fail_closed() {
        let policy = policy(McpDiscoveryPolicy::default());
        assert_eq!(
            project_mcp_catalog(
                vec![
                    descriptor("same", object_schema()),
                    descriptor("same", object_schema()),
                ],
                &policy,
            )
            .unwrap_err()
            .code,
            McpCatalogProjectionErrorCode::DuplicateRemoteName
        );
        assert_eq!(
            project_mcp_catalog(vec![descriptor("not portable", object_schema())], &policy,)
                .unwrap_err()
                .code,
            McpCatalogProjectionErrorCode::InvalidLocalName
        );
    }

    #[test]
    fn projection_is_deterministic_and_supports_nested_composition_without_recursion() {
        let policy = policy(McpDiscoveryPolicy::default());
        let composed = json!({
            "type": "object",
            "properties": {
                "choice": {
                    "anyOf": [
                        {"type": "string", "enum": ["a", "b"]},
                        {"type": "integer", "minimum": 0}
                    ]
                },
                "tags": {
                    "type": "array",
                    "items": {"type": "string", "pattern": "^[a-z]+$", "format": "uuid"},
                    "maxItems": 8,
                    "uniqueItems": true
                }
            },
            "additionalProperties": {"type": "string"}
        });
        let first = project_mcp_catalog(
            vec![
                descriptor("zeta", composed.clone()),
                descriptor("alpha", composed.clone()),
            ],
            &policy,
        )
        .unwrap();
        let second = project_mcp_catalog(
            vec![
                descriptor("alpha", composed.clone()),
                descriptor("zeta", composed),
            ],
            &policy,
        )
        .unwrap();

        assert_eq!(first, second);
        assert_eq!(first.tools()[0].local_name(), "provider.alpha");
        assert_eq!(first.tools()[1].local_name(), "provider.zeta");
        assert!(!serde_json::to_string(&first.tools()[0].model_definition())
            .unwrap()
            .contains("pattern"));
    }

    #[test]
    fn retained_local_policy_strings_are_included_in_catalog_accounting() {
        let plain_policy = policy(McpDiscoveryPolicy::default());
        let plain =
            project_mcp_catalog(vec![descriptor("write", object_schema())], &plain_policy).unwrap();

        let trusted_description = "Trusted write operation.";
        let mut tool_policies = BTreeMap::new();
        tool_policies.insert(
            "write".to_owned(),
            McpToolPolicy {
                trusted_description: Some(trusted_description.to_owned()),
                additional_required_grants: BTreeSet::from(["write-grant".to_owned()]),
                additional_resource_scopes: BTreeSet::from(["write-scope".to_owned()]),
                additional_required_resource_authorities: BTreeSet::from([
                    "write-authority".to_owned()
                ]),
                ..McpToolPolicy::default()
            },
        );
        let rich_policy = policy(McpDiscoveryPolicy {
            tool_policies,
            ..McpDiscoveryPolicy::default()
        });
        let rich =
            project_mcp_catalog(vec![descriptor("write", object_schema())], &rich_policy).unwrap();

        assert_eq!(
            rich.accounted_bytes() - plain.accounted_bytes(),
            trusted_description.len()
                + "write-grant".len()
                + "write-scope".len()
                + "write-authority".len()
        );
    }

    #[test]
    fn descriptor_and_projected_debug_surfaces_omit_remote_prose_and_schema_values() {
        let remote = descriptor(
            "debug-canary",
            json!({
                "type": "object",
                "properties": {"secret_canary": {"type": "string", "enum": ["value-canary"]}}
            }),
        );
        let descriptor_debug = format!("{remote:?}");
        assert!(!descriptor_debug.contains("REMOTE"));
        assert!(!descriptor_debug.contains("secret_canary"));
        assert!(!descriptor_debug.contains("value-canary"));

        let catalog =
            project_mcp_catalog(vec![remote], &policy(McpDiscoveryPolicy::default())).unwrap();
        let catalog_debug = format!("{catalog:?}");
        let tool_debug = format!("{:?}", catalog.tools()[0]);
        let model_debug = format!("{:?}", catalog.tools()[0].model_definition());
        for debug in [catalog_debug, tool_debug, model_debug] {
            assert!(!debug.contains("secret_canary"));
            assert!(!debug.contains("value-canary"));
            assert!(!debug.contains("REMOTE"));
        }
    }

    #[test]
    fn aggregate_raw_schema_budget_rejects_annotation_amplification() {
        let policy = policy(McpDiscoveryPolicy::default());
        let annotation = "x".repeat(250 * 1024);
        let descriptors = (0..40)
            .map(|index| {
                descriptor(
                    &format!("tool-{index}"),
                    json!({"type": "object", "description": annotation}),
                )
            })
            .collect();
        assert_eq!(
            project_mcp_catalog(descriptors, &policy).unwrap_err().code,
            McpCatalogProjectionErrorCode::CatalogTooLarge
        );
    }

    #[test]
    fn maximum_catalog_projects_on_a_small_stack() {
        thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(|| {
                let policy = policy(McpDiscoveryPolicy::default());
                let descriptors = (0..512)
                    .map(|index| descriptor(&format!("tool-{index:03}"), object_schema()))
                    .collect();
                let catalog = project_mcp_catalog(descriptors, &policy).unwrap();
                assert_eq!(catalog.tools().len(), 512);
                assert_eq!(catalog.tools()[0].local_name(), "provider.tool-000");
                assert_eq!(catalog.tools()[511].local_name(), "provider.tool-511");
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn only_the_explicit_model_view_is_serializable() {
        assert_not_impl_any!(McpToolId: Serialize);
        assert_not_impl_any!(McpToolDescriptor: Serialize);
        assert_not_impl_any!(McpProjectedCatalog: Serialize, Clone);
        assert_not_impl_any!(McpProjectedTool: Serialize, Clone);
    }
}

use std::collections::HashSet;
use std::io::{self, Write};

use rmcp::model::{
    CallToolResponse, CreateTaskResult, GetTaskResult, InputRequests, ServerPeerInfo, Task,
    TaskPayload, TaskStatus, Tool,
};
use serde_json::{Map, Value};

use crate::{
    continuation::ValidatedPending, McpClientError, McpClientLimits, McpConnectionInfo,
    McpMrtrPresentationCapabilities, McpToolCallResult, McpToolDescriptor, McpToolHints, McpToolId,
    RMCP_SDK_VERSION,
};

pub(crate) enum ValidatedCallResponse {
    Complete(McpToolCallResult),
    Pending(ValidatedPending),
}

/// Measure compact JSON serialization without retaining a second payload buffer.
pub(crate) fn measure_serialized_request<T: serde::Serialize>(
    value: &T,
    maximum: usize,
) -> Result<usize, McpClientError> {
    struct CountingWriter {
        bytes: usize,
        maximum: usize,
    }

    impl Write for CountingWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            let Some(next) = self.bytes.checked_add(buffer.len()) else {
                return Err(io::Error::new(
                    io::ErrorKind::FileTooLarge,
                    "request too large",
                ));
            };
            if next > self.maximum {
                return Err(io::Error::new(
                    io::ErrorKind::FileTooLarge,
                    "request too large",
                ));
            }
            self.bytes = next;
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = CountingWriter { bytes: 0, maximum };
    serde_json::to_writer(&mut writer, value).map_err(|_| {
        McpClientError::RequestRejected("MCP request serialization was rejected".to_owned())
    })?;
    Ok(writer.bytes)
}

pub(crate) fn project_connection_info(
    peer: &ServerPeerInfo,
    limits: &McpClientLimits,
    transport: &'static str,
) -> Result<McpConnectionInfo, McpClientError> {
    let server_name = peer.server_info.as_ref().map(|info| info.name.clone());
    let server_version = peer.server_info.as_ref().map(|info| info.version.clone());
    validate_identifier_text(
        "server name",
        server_name.as_deref(),
        limits.max_tool_title_bytes,
    )?;
    validate_identifier_text(
        "server version",
        server_version.as_deref(),
        limits.max_tool_title_bytes,
    )?;
    validate_optional_text(
        "server instructions",
        peer.instructions.as_deref(),
        limits.max_tool_description_bytes,
    )?;

    let capabilities = serde_json::to_value(&peer.capabilities)
        .map_err(|error| McpClientError::ResponseRejected(error.to_string()))?;
    measure_json(
        &capabilities,
        limits.max_schema_bytes,
        limits.max_json_depth,
        limits.max_json_nodes,
        "server capabilities",
    )
    .map_err(as_response_rejection)?;

    Ok(McpConnectionInfo {
        transport,
        protocol_version: peer.protocol_version.to_string(),
        server_name,
        server_version,
        instructions: peer.instructions.clone(),
        capabilities,
        sdk_version: RMCP_SDK_VERSION,
    })
}

#[cfg(test)]
pub(crate) fn project_tools(
    tools: Vec<Tool>,
    limits: &McpClientLimits,
) -> Result<(Vec<McpToolDescriptor>, HashSet<String>), McpClientError> {
    let mut projected = Vec::new();
    let mut names = HashSet::new();
    let mut catalog_bytes = 0usize;
    project_tool_page(
        tools,
        limits,
        &mut projected,
        &mut names,
        &mut catalog_bytes,
        1,
        1,
    )?;
    Ok((projected, names))
}

pub(crate) fn project_tool_page(
    tools: Vec<Tool>,
    limits: &McpClientLimits,
    projected: &mut Vec<McpToolDescriptor>,
    names: &mut HashSet<String>,
    catalog_bytes: &mut usize,
    client_instance_id: u64,
    discovery_generation: u64,
) -> Result<(), McpClientError> {
    let total_tools = projected.len().saturating_add(tools.len());
    if total_tools > limits.max_tool_count {
        return Err(McpClientError::CatalogRejected(format!(
            "server returned {} tools; limit is {}",
            total_tools, limits.max_tool_count
        )));
    }
    projected.reserve(tools.len().min(limits.max_tool_count));
    names.reserve(tools.len().min(limits.max_tool_count));

    for tool in tools {
        let name = tool.name.into_owned();
        if name.is_empty() {
            return Err(McpClientError::CatalogRejected(
                "server returned an empty tool name".to_owned(),
            ));
        }
        if name.chars().any(char::is_control) {
            return Err(McpClientError::CatalogRejected(
                "tool name contains control characters".to_owned(),
            ));
        }
        add_budget(
            catalog_bytes,
            name.len(),
            limits.max_catalog_bytes,
            "MCP catalog",
        )?;
        if name.len() > limits.max_tool_name_bytes {
            return Err(McpClientError::CatalogRejected(format!(
                "tool name exceeds {} bytes",
                limits.max_tool_name_bytes
            )));
        }
        if !names.insert(name.clone()) {
            return Err(McpClientError::CatalogRejected(
                "duplicate remote tool name".to_owned(),
            ));
        }

        validate_identifier_text(
            "tool title",
            tool.title.as_deref(),
            limits.max_tool_title_bytes,
        )?;
        let description = tool.description.map(|value| value.into_owned());
        validate_optional_text(
            "tool description",
            description.as_deref(),
            limits.max_tool_description_bytes,
        )?;
        add_budget(
            catalog_bytes,
            tool.title.as_ref().map_or(0, String::len),
            limits.max_catalog_bytes,
            "MCP catalog",
        )?;
        add_budget(
            catalog_bytes,
            description.as_ref().map_or(0, String::len),
            limits.max_catalog_bytes,
            "MCP catalog",
        )?;

        let input_schema = measure_object(
            tool.input_schema.as_ref(),
            limits.max_schema_bytes,
            limits,
            "tool input schema",
        )?;
        add_budget(
            catalog_bytes,
            input_schema.bytes,
            limits.max_catalog_bytes,
            "MCP catalog",
        )?;

        let output_schema_bytes = tool
            .output_schema
            .as_ref()
            .map(|schema| {
                measure_object(
                    schema.as_ref(),
                    limits.max_schema_bytes,
                    limits,
                    "tool output schema",
                )
            })
            .transpose()?
            .map_or(0, |measurement| measurement.bytes);
        add_budget(
            catalog_bytes,
            output_schema_bytes,
            limits.max_catalog_bytes,
            "MCP catalog",
        )?;

        let hints = tool
            .annotations
            .map(|annotations| McpToolHints {
                read_only: annotations.read_only_hint,
                destructive: annotations.destructive_hint,
                idempotent: annotations.idempotent_hint,
                open_world: annotations.open_world_hint,
            })
            .unwrap_or_default();

        projected.push(McpToolDescriptor {
            id: McpToolId::new(name, client_instance_id, discovery_generation),
            title: tool.title,
            description,
            input_schema: tool.input_schema.as_ref().clone(),
            output_schema: tool.output_schema.map(|schema| schema.as_ref().clone()),
            hints,
        });
    }

    Ok(())
}

pub(crate) fn project_call_response(
    response: CallToolResponse,
    limits: &McpClientLimits,
    presentation_capabilities: McpMrtrPresentationCapabilities,
    task_lifecycle_negotiated: bool,
) -> Result<ValidatedCallResponse, McpClientError> {
    match response {
        CallToolResponse::Complete(result) => {
            if result.content.len() > limits.max_json_nodes {
                return Err(McpClientError::ResponseRejected(format!(
                    "tool result contains {} content blocks; node limit is {}",
                    result.content.len(),
                    limits.max_json_nodes
                )));
            }
            let mut content = Vec::with_capacity(result.content.len().min(1_024));
            let mut total_bytes = 0usize;
            let mut total_nodes = 0usize;
            for block in result.content {
                let value = serde_json::to_value(block)
                    .map_err(|error| McpClientError::ResponseRejected(error.to_string()))?;
                let measurement = measure_json(
                    &value,
                    limits.max_result_bytes,
                    limits.max_json_depth,
                    limits.max_json_nodes,
                    "tool content",
                )
                .map_err(as_response_rejection)?;
                add_budget(
                    &mut total_bytes,
                    measurement.bytes,
                    limits.max_result_bytes,
                    "MCP tool result",
                )
                .map_err(as_response_rejection)?;
                add_count_budget(
                    &mut total_nodes,
                    measurement.nodes,
                    limits.max_json_nodes,
                    "MCP tool result JSON nodes",
                )
                .map_err(as_response_rejection)?;
                content.push(value);
            }
            if let Some(structured) = result.structured_content.as_ref() {
                let measurement = measure_json(
                    structured,
                    limits.max_result_bytes,
                    limits.max_json_depth,
                    limits.max_json_nodes,
                    "structured tool result",
                )
                .map_err(as_response_rejection)?;
                add_budget(
                    &mut total_bytes,
                    measurement.bytes,
                    limits.max_result_bytes,
                    "MCP tool result",
                )
                .map_err(as_response_rejection)?;
                add_count_budget(
                    &mut total_nodes,
                    measurement.nodes,
                    limits.max_json_nodes,
                    "MCP tool result JSON nodes",
                )
                .map_err(as_response_rejection)?;
            }
            Ok(ValidatedCallResponse::Complete(McpToolCallResult {
                result_type: result
                    .result_type
                    .as_ref()
                    .map_or_else(|| "complete".to_owned(), |kind| kind.as_str().to_owned()),
                content,
                structured_content: result.structured_content,
                is_error: result.is_error.unwrap_or(false),
            }))
        },
        CallToolResponse::InputRequired(result) => {
            let payload_bytes =
                validate_input_required(&result, limits, presentation_capabilities)?;
            Ok(ValidatedCallResponse::Pending(
                ValidatedPending::InputRequired {
                    result,
                    payload_bytes,
                },
            ))
        },
        CallToolResponse::Task(result) => {
            if !task_lifecycle_negotiated {
                return Err(McpClientError::ContinuationCapabilityUnsupported);
            }
            let payload_bytes = validate_task_seed(&result, limits)?;
            Ok(ValidatedCallResponse::Pending(ValidatedPending::Task {
                result,
                payload_bytes,
            }))
        },
        _ => Err(McpClientError::ResponseRejected(
            "SDK returned an unsupported tool response variant".to_owned(),
        )),
    }
}

fn validate_input_required(
    result: &rmcp::model::InputRequiredResult,
    limits: &McpClientLimits,
    presentation_capabilities: McpMrtrPresentationCapabilities,
) -> Result<usize, McpClientError> {
    let input_count = result.input_requests.as_ref().map_or(0, InputRequests::len);
    if input_count == 0 && result.request_state.is_none() {
        return Err(McpClientError::ResponseRejected(
            "input-required result contains neither input requests nor request state".to_owned(),
        ));
    }
    validate_input_requests(
        result.input_requests.as_ref(),
        limits,
        presentation_capabilities,
    )?;
    validate_pending(result, limits)
}

fn validate_input_requests(
    requests: Option<&InputRequests>,
    limits: &McpClientLimits,
    presentation_capabilities: McpMrtrPresentationCapabilities,
) -> Result<(), McpClientError> {
    let input_count = requests.map_or(0, InputRequests::len);
    if input_count > limits.max_mrtr_inputs {
        return Err(McpClientError::ResponseRejected(format!(
            "input-required result contains {input_count} inputs; limit is {}",
            limits.max_mrtr_inputs
        )));
    }
    if let Some(inputs) = requests {
        for (key, request) in inputs {
            if key.is_empty()
                || key.len() > limits.max_tool_name_bytes
                || key.chars().any(char::is_control)
            {
                return Err(McpClientError::ResponseRejected(
                    "input-required result contains an invalid request key".to_owned(),
                ));
            }
            if !presentation_capabilities.supports_input_request(request) {
                return Err(McpClientError::ContinuationCapabilityUnsupported);
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_task_seed(
    result: &CreateTaskResult,
    limits: &McpClientLimits,
) -> Result<usize, McpClientError> {
    if !result.result_type.is_task() || result.task.status != TaskStatus::Working {
        return Err(McpClientError::ResponseRejected(
            "task creation result has an invalid initial state".to_owned(),
        ));
    }
    validate_task_metadata(&result.task, limits)?;
    validate_pending(result, limits)
}

pub(crate) fn validate_task_poll_result(
    result: &GetTaskResult,
    limits: &McpClientLimits,
    presentation_capabilities: McpMrtrPresentationCapabilities,
) -> Result<usize, McpClientError> {
    if !result.result_type.is_complete() || result.task.task.status != result.task.payload.status()
    {
        return Err(McpClientError::ResponseRejected(
            "task status response has an invalid discriminator".to_owned(),
        ));
    }
    validate_task_metadata(&result.task.task, limits)?;
    if let TaskPayload::InputRequired { input_requests } = &result.task.payload {
        if input_requests.is_empty() {
            return Err(McpClientError::ResponseRejected(
                "task input-required status contains no input requests".to_owned(),
            ));
        }
        validate_input_requests(Some(input_requests), limits, presentation_capabilities)?;
    }
    validate_pending(result, limits)
}

fn validate_task_metadata(task: &Task, limits: &McpClientLimits) -> Result<(), McpClientError> {
    if task.task_id.is_empty()
        || task.task_id.len() > limits.max_task_id_bytes
        || task.task_id.chars().any(char::is_control)
    {
        return Err(McpClientError::ResponseRejected(
            "task response contains an invalid task identifier".to_owned(),
        ));
    }
    for timestamp in [&task.created_at, &task.last_updated_at] {
        if timestamp.is_empty() || timestamp.len() > 128 || timestamp.chars().any(char::is_control)
        {
            return Err(McpClientError::ResponseRejected(
                "task response contains an invalid timestamp".to_owned(),
            ));
        }
    }
    if task.status_message.as_ref().is_some_and(|message| {
        message.len() > limits.max_task_status_message_bytes
            || message.chars().any(char::is_control)
    }) {
        return Err(McpClientError::ResponseRejected(
            "task response contains an invalid status message".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_call_arguments(
    arguments: &Map<String, Value>,
    limits: &McpClientLimits,
) -> Result<usize, McpClientError> {
    measure_object(
        arguments,
        limits.max_request_bytes,
        limits,
        "tool arguments",
    )
    .map(|measurement| measurement.bytes)
    .map_err(|error| McpClientError::RequestRejected(error.to_string()))
}

/// Drop an owned JSON argument map without recursive destructor calls.
pub(crate) fn drop_json_map_iterative(arguments: Map<String, Value>) {
    drop_json_value_iterative(Value::Object(arguments));
}

pub(crate) fn drop_json_value_iterative(root: Value) {
    enum Children {
        Array(std::vec::IntoIter<Value>),
        Object(serde_json::map::IntoIter),
    }

    impl Children {
        fn next_value(&mut self) -> Option<Value> {
            match self {
                Self::Array(values) => values.next(),
                Self::Object(values) => values.next().map(|(_, value)| value),
            }
        }
    }

    let mut stack = Vec::new();
    match root {
        Value::Array(values) => stack.push(Children::Array(values.into_iter())),
        Value::Object(values) => stack.push(Children::Object(values.into_iter())),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => return,
    }
    while let Some(children) = stack.last_mut() {
        let Some(value) = children.next_value() else {
            stack.pop();
            continue;
        };
        match value {
            Value::Array(values) => stack.push(Children::Array(values.into_iter())),
            Value::Object(values) => stack.push(Children::Object(values.into_iter())),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {},
        }
    }
}

fn validate_pending(
    value: &impl serde::Serialize,
    limits: &McpClientLimits,
) -> Result<usize, McpClientError> {
    let payload = serde_json::to_value(value)
        .map_err(|error| McpClientError::ResponseRejected(error.to_string()))?;
    let measurement = measure_json(
        &payload,
        limits.max_result_bytes,
        limits.max_json_depth,
        limits.max_json_nodes,
        "pending MCP result",
    )
    .map_err(as_response_rejection);
    drop_json_value_iterative(payload);
    measurement.map(|measurement| measurement.bytes)
}

fn validate_optional_text(
    label: &str,
    value: Option<&str>,
    max_bytes: usize,
) -> Result<(), McpClientError> {
    if value.is_some_and(|value| value.len() > max_bytes) {
        return Err(McpClientError::CatalogRejected(format!(
            "{label} exceeds {max_bytes} bytes"
        )));
    }
    Ok(())
}

fn validate_identifier_text(
    label: &str,
    value: Option<&str>,
    max_bytes: usize,
) -> Result<(), McpClientError> {
    validate_optional_text(label, value, max_bytes)?;
    if value.is_some_and(|value| value.chars().any(char::is_control)) {
        return Err(McpClientError::CatalogRejected(format!(
            "{label} contains control characters"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct JsonMeasurement {
    pub(crate) bytes: usize,
    pub(crate) nodes: usize,
}

pub(crate) fn measure_object(
    object: &Map<String, Value>,
    max_bytes: usize,
    limits: &McpClientLimits,
    label: &str,
) -> Result<JsonMeasurement, McpClientError> {
    ensure_pending_nodes(1, 0, object.len(), limits.max_json_nodes, label)?;
    if limits.max_json_depth < 2 && !object.is_empty() {
        return Err(McpClientError::CatalogRejected(format!(
            "{label} exceeds JSON depth {}",
            limits.max_json_depth
        )));
    }
    let mut initial_bytes = 2usize.saturating_add(object.len().saturating_sub(1));
    for key in object.keys() {
        initial_bytes = initial_bytes
            .saturating_add(json_string_wire_len(key))
            .saturating_add(1);
        ensure_byte_budget(initial_bytes, max_bytes, label)?;
    }
    let stack = object.values().map(|value| (value, 2usize)).collect();
    measure_json_stack(
        stack,
        initial_bytes,
        1,
        max_bytes,
        limits.max_json_depth,
        limits.max_json_nodes,
        label,
    )
}

pub(crate) fn measure_json(
    root: &Value,
    max_bytes: usize,
    max_depth: usize,
    max_nodes: usize,
    label: &str,
) -> Result<JsonMeasurement, McpClientError> {
    measure_json_stack(
        vec![(root, 1usize)],
        0,
        0,
        max_bytes,
        max_depth,
        max_nodes,
        label,
    )
}

fn measure_json_stack(
    mut stack: Vec<(&Value, usize)>,
    mut bytes: usize,
    mut nodes: usize,
    max_bytes: usize,
    max_depth: usize,
    max_nodes: usize,
    label: &str,
) -> Result<JsonMeasurement, McpClientError> {
    ensure_byte_budget(bytes, max_bytes, label)?;

    while let Some((value, depth)) = stack.pop() {
        nodes = nodes.saturating_add(1);
        if nodes > max_nodes {
            return Err(McpClientError::CatalogRejected(format!(
                "{label} exceeds {max_nodes} JSON nodes"
            )));
        }
        if depth > max_depth {
            return Err(McpClientError::CatalogRejected(format!(
                "{label} exceeds JSON depth {max_depth}"
            )));
        }

        let own_bytes = match value {
            Value::Null => 4,
            Value::Bool(true) => 4,
            Value::Bool(false) => 5,
            Value::Number(number) => number.to_string().len(),
            Value::String(text) => json_string_wire_len(text),
            Value::Array(values) => {
                ensure_pending_nodes(nodes, stack.len(), values.len(), max_nodes, label)?;
                if depth >= max_depth && !values.is_empty() {
                    return Err(McpClientError::CatalogRejected(format!(
                        "{label} exceeds JSON depth {max_depth}"
                    )));
                }
                stack.extend(values.iter().map(|value| (value, depth.saturating_add(1))));
                values.len().saturating_sub(1).saturating_add(2)
            },
            Value::Object(values) => {
                ensure_pending_nodes(nodes, stack.len(), values.len(), max_nodes, label)?;
                if depth >= max_depth && !values.is_empty() {
                    return Err(McpClientError::CatalogRejected(format!(
                        "{label} exceeds JSON depth {max_depth}"
                    )));
                }
                stack.extend(
                    values
                        .values()
                        .map(|value| (value, depth.saturating_add(1))),
                );
                let mut object_bytes = values.len().saturating_sub(1).saturating_add(2);
                for key in values.keys() {
                    object_bytes = object_bytes
                        .saturating_add(json_string_wire_len(key))
                        .saturating_add(1);
                    ensure_byte_budget(bytes.saturating_add(object_bytes), max_bytes, label)?;
                }
                object_bytes
            },
        };
        bytes = bytes.saturating_add(own_bytes);
        ensure_byte_budget(bytes, max_bytes, label)?;
    }
    Ok(JsonMeasurement { bytes, nodes })
}

fn ensure_pending_nodes(
    visited: usize,
    pending: usize,
    additional: usize,
    maximum: usize,
    label: &str,
) -> Result<(), McpClientError> {
    if visited.saturating_add(pending).saturating_add(additional) > maximum {
        return Err(McpClientError::CatalogRejected(format!(
            "{label} exceeds {maximum} JSON nodes"
        )));
    }
    Ok(())
}

fn ensure_byte_budget(bytes: usize, maximum: usize, label: &str) -> Result<(), McpClientError> {
    if bytes > maximum {
        return Err(McpClientError::CatalogRejected(format!(
            "{label} exceeds {maximum} bytes"
        )));
    }
    Ok(())
}

pub(crate) fn json_string_wire_len(value: &str) -> usize {
    let escaped = value.as_bytes().iter().fold(0usize, |length, byte| {
        let encoded = match byte {
            b'"' | b'\\' | b'\x08' | b'\t' | b'\n' | b'\x0c' | b'\r' => 2,
            0x00..=0x1f => 6,
            _ => 1,
        };
        length.saturating_add(encoded)
    });
    escaped.saturating_add(2)
}

fn add_budget(
    current: &mut usize,
    addition: usize,
    maximum: usize,
    label: &str,
) -> Result<(), McpClientError> {
    *current = current.saturating_add(addition);
    if *current > maximum {
        return Err(McpClientError::CatalogRejected(format!(
            "{label} exceeds {maximum} bytes"
        )));
    }
    Ok(())
}

fn add_count_budget(
    current: &mut usize,
    addition: usize,
    maximum: usize,
    label: &str,
) -> Result<(), McpClientError> {
    *current = current.saturating_add(addition);
    if *current > maximum {
        return Err(McpClientError::CatalogRejected(format!(
            "{label} exceeds {maximum}"
        )));
    }
    Ok(())
}

fn as_response_rejection(error: McpClientError) -> McpClientError {
    match error {
        McpClientError::CatalogRejected(message) => McpClientError::ResponseRejected(message),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    #[allow(deprecated)]
    use rmcp::model::{InputRequest, InputRequiredResult, ListRootsRequest, Tool};
    use serde_json::json;

    use super::*;

    fn tool(name: &str) -> Tool {
        Tool::new(
            name.to_owned(),
            "fixture",
            Arc::new(
                serde_json::from_value(json!({
                    "type": "object",
                    "properties": {"query": {"type": "string"}}
                }))
                .unwrap(),
            ),
        )
    }

    #[test]
    fn rejects_duplicate_remote_tool_names() {
        let canary = "canary-private-remote-tool-name";
        let error = project_tools(vec![tool(canary), tool(canary)], &Default::default())
            .expect_err("duplicate names must fail closed");
        let diagnostic = error.to_string();
        assert!(diagnostic.contains("duplicate remote tool name"));
        assert!(!diagnostic.contains(canary));
    }

    #[test]
    fn rejects_catalogs_above_the_tool_limit() {
        let limits = McpClientLimits {
            max_tool_count: 1,
            ..Default::default()
        };
        assert!(project_tools(vec![tool("one"), tool("two")], &limits).is_err());
    }

    #[test]
    fn iterative_json_measurement_rejects_depth_without_recursion() {
        let value = json!({"a": {"b": {"c": true}}});
        let error =
            measure_json(&value, 1024, 2, 100, "fixture").expect_err("depth must be bounded");
        assert!(error.to_string().contains("JSON depth"));
    }

    #[test]
    fn annotations_are_preserved_only_as_hints() {
        let annotated = tool("read").with_annotations(
            rmcp::model::ToolAnnotations::new()
                .read_only(true)
                .destructive(false),
        );
        let (tools, _) = project_tools(vec![annotated], &Default::default()).unwrap();
        assert_eq!(tools[0].hints.read_only, Some(true));
        assert_eq!(tools[0].hints.destructive, Some(false));
    }

    #[test]
    fn control_characters_in_tool_names_fail_closed() {
        let error = project_tools(vec![tool("unsafe\nname")], &Default::default()).unwrap_err();
        assert!(error.to_string().contains("control characters"));
    }

    #[test]
    fn oversized_call_arguments_are_request_failures() {
        let limits = McpClientLimits {
            max_request_bytes: 8,
            ..Default::default()
        };
        let arguments = serde_json::from_value(json!({"message": "too long"})).unwrap();
        let error = validate_call_arguments(&arguments, &limits).unwrap_err();
        assert!(matches!(error, McpClientError::RequestRejected(_)));
    }

    #[test]
    fn json_measurement_matches_compact_serde_json_encoding_with_escapes() {
        let value = json!({
            "quote\"slash\\control\u{0001}": ["line\nfeed", true, null, 123.5],
            "unicode": "नमस्ते"
        });
        let encoded = serde_json::to_vec(&value).unwrap();
        let measurement = measure_json(&value, encoded.len(), 16, 100, "fixture").unwrap();
        assert_eq!(measurement.bytes, encoded.len());
        assert!(measure_json(&value, encoded.len() - 1, 16, 100, "fixture").is_err());
    }

    #[test]
    fn serialized_request_measurement_is_exact_and_stops_at_the_byte_ceiling() {
        let value = json!({"message": "canary\nrequest", "count": 3});
        let expected = serde_json::to_vec(&value).unwrap().len();
        assert_eq!(
            measure_serialized_request(&value, expected).unwrap(),
            expected
        );
        let error = measure_serialized_request(&value, expected - 1).unwrap_err();
        assert!(matches!(error, McpClientError::RequestRejected(_)));
        assert!(!error.to_string().contains("canary"));
    }

    #[test]
    fn wide_json_is_rejected_by_node_budget_before_children_are_visited() {
        let value = Value::Array(vec![Value::Null; 10_000]);
        let error = measure_json(&value, usize::MAX, 4, 8, "wide fixture").unwrap_err();
        assert!(error.to_string().contains("JSON nodes"));
    }

    #[test]
    fn projection_enforces_duplicates_and_catalog_budget_across_pages() {
        let limits = McpClientLimits {
            max_catalog_bytes: 512,
            ..Default::default()
        };
        let mut projected = Vec::new();
        let mut names = HashSet::new();
        let mut catalog_bytes = 0;
        let canary = "canary-private-paginated-tool-name";
        project_tool_page(
            vec![tool(canary)],
            &limits,
            &mut projected,
            &mut names,
            &mut catalog_bytes,
            1,
            1,
        )
        .unwrap();
        let error = project_tool_page(
            vec![tool(canary)],
            &limits,
            &mut projected,
            &mut names,
            &mut catalog_bytes,
            1,
            1,
        )
        .unwrap_err();
        let diagnostic = error.to_string();
        assert!(diagnostic.contains("duplicate remote tool name"));
        assert!(!diagnostic.contains(canary));
    }

    #[test]
    fn deeply_nested_owned_arguments_reject_and_drop_on_a_small_stack() {
        let mut nested = Value::Null;
        for _ in 0..20_000 {
            nested = Value::Array(vec![nested]);
        }
        let mut arguments = Map::new();
        arguments.insert("deep".to_owned(), nested);
        std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(move || {
                let error = validate_call_arguments(&arguments, &Default::default()).unwrap_err();
                assert!(matches!(error, McpClientError::RequestRejected(_)));
                drop_json_map_iterative(arguments);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    #[allow(deprecated)]
    fn input_required_keys_and_count_are_bounded_before_retention() {
        let mut requests = std::collections::BTreeMap::new();
        requests.insert(
            "valid".to_owned(),
            InputRequest::ListRoots(ListRootsRequest::default()),
        );
        requests.insert(
            "second".to_owned(),
            InputRequest::ListRoots(ListRootsRequest::default()),
        );
        let result = InputRequiredResult::from_input_requests(requests);
        let limits = McpClientLimits {
            max_mrtr_inputs: 1,
            ..Default::default()
        };
        let roots = McpMrtrPresentationCapabilities::new().enable_roots();
        assert!(project_call_response(
            CallToolResponse::InputRequired(result),
            &limits,
            roots,
            false
        )
        .is_err());

        let mut requests = std::collections::BTreeMap::new();
        requests.insert(
            "invalid\nkey".to_owned(),
            InputRequest::ListRoots(ListRootsRequest::default()),
        );
        assert!(project_call_response(
            CallToolResponse::InputRequired(InputRequiredResult::from_input_requests(requests)),
            &McpClientLimits::default(),
            roots,
            false,
        )
        .is_err());
    }

    #[test]
    fn task_results_require_negotiation_and_valid_seed_state() {
        let working = CreateTaskResult::new(Task::new(
            "private-task",
            TaskStatus::Working,
            "2026-08-07T00:00:00Z",
            "2026-08-07T00:00:01Z",
        ));
        assert!(matches!(
            project_call_response(
                CallToolResponse::Task(working.clone()),
                &McpClientLimits::default(),
                McpMrtrPresentationCapabilities::default(),
                false,
            ),
            Err(McpClientError::ContinuationCapabilityUnsupported)
        ));
        assert!(project_call_response(
            CallToolResponse::Task(working),
            &McpClientLimits::default(),
            McpMrtrPresentationCapabilities::default(),
            true,
        )
        .is_ok());

        let terminal_seed = CreateTaskResult::new(Task::new(
            "private-task",
            TaskStatus::Completed,
            "2026-08-07T00:00:00Z",
            "2026-08-07T00:00:01Z",
        ));
        assert!(project_call_response(
            CallToolResponse::Task(terminal_seed),
            &McpClientLimits::default(),
            McpMrtrPresentationCapabilities::default(),
            true,
        )
        .is_err());
    }

    #[test]
    #[allow(deprecated)]
    fn task_input_requests_enforce_the_installed_presenters() {
        let requests = InputRequests::from([(
            "private-root".to_owned(),
            InputRequest::ListRoots(ListRootsRequest::default()),
        )]);
        let result = GetTaskResult::new(rmcp::model::DetailedTask::new(
            Task::new(
                "private-task",
                TaskStatus::InputRequired,
                "2026-08-07T00:00:00Z",
                "2026-08-07T00:00:01Z",
            ),
            TaskPayload::InputRequired {
                input_requests: requests,
            },
        ));
        assert!(matches!(
            validate_task_poll_result(
                &result,
                &McpClientLimits::default(),
                McpMrtrPresentationCapabilities::default(),
            ),
            Err(McpClientError::ContinuationCapabilityUnsupported)
        ));
        assert!(validate_task_poll_result(
            &result,
            &McpClientLimits::default(),
            McpMrtrPresentationCapabilities::new().enable_roots(),
        )
        .is_ok());
    }
}

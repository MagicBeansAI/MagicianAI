//! Execution-native request adapter.
//!
//! Converts [`ExecutionNativeRequest`] into router calls and projects
//! [`ExecutionNativeResponse`] from the raw router output.

use std::sync::Arc;

use anyhow::Result;
use magicllm::prelude::{LLMMessage as RouterMessage, LLMToolSpec as RouterToolSpec};
use serde_json::Value;

use super::native_types::{
    ExecutionNativeRequest, ExecutionNativeResponse, ExecutionToolCall, NativeExecutionTool,
};
use crate::magician_v2::query_analysis::operation_llm_router::{LLMOperation, OperationLlmRouter};

/// Converts [`NativeExecutionTool`] specs to router-compatible [`RouterToolSpec`].
fn to_router_tool_specs(tools: &[NativeExecutionTool]) -> Vec<RouterToolSpec> {
    tools
        .iter()
        .map(|t| RouterToolSpec {
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: t.parameters.clone(),
        })
        .collect()
}

/// Send an execution-native request through the router and return a projected response.
pub async fn execute_native_request(
    router: &Arc<OperationLlmRouter>,
    request: &ExecutionNativeRequest,
    viewport: Option<(u32, u32)>,
) -> Result<ExecutionNativeResponse> {
    let operation = LLMOperation::Other(request.operation.clone());
    let tool_specs = to_router_tool_specs(&request.tools);

    let raw = router
        .generate_for_execution_native_tools(
            &operation,
            Some(&request.system_prompt),
            &request.user_prompt,
            tool_specs,
            request.model_override.as_deref(),
            request.images.as_deref(),
            request.tool_choice_override.clone(),
            viewport,
        )
        .await?;

    // Project to execution-native response
    let mut response = ExecutionNativeResponse::unadmitted(
        raw.tool_calls
            .into_iter()
            .map(|tc| ExecutionToolCall {
                id: tc.id,
                name: tc.name,
                arguments: tc.arguments,
            })
            .collect(),
    );
    response.text = raw.text;
    response.reasoning_text = raw.reasoning_text;
    response.response_id = raw.response_id;
    response.finish_reason = raw.finish_reason;
    response.prompt_tokens = raw.prompt_tokens;
    response.completion_tokens = raw.completion_tokens;
    response.cached_tokens = raw.cached_tokens;
    response.cache_creation_tokens = raw.cache_creation_tokens;
    response.provider = raw.provider;
    response.model = raw.model;
    response.reasoning_tokens = raw.reasoning_tokens;
    response.profile = raw.profile;
    response.telemetry = raw.telemetry;
    response.admit_tool_arguments();
    Ok(response)
}

/// Multi-turn variant: send pre-built messages (including
/// `Assistant(tool_calls)` and `User(tool_results)` history) and project
/// the response into [`ExecutionNativeResponse`].
///
/// Used by the inner-loop runner so the model sees its own tool_use /
/// tool_result history natively across iterations, instead of having past
/// turns flattened into a freshly-rebuilt user prompt.
pub async fn execute_native_messages_request(
    router: &std::sync::Arc<OperationLlmRouter>,
    operation: &str,
    messages: Vec<RouterMessage>,
    tools: &[NativeExecutionTool],
    model_override: Option<&str>,
    tool_choice_override: Option<Value>,
) -> Result<ExecutionNativeResponse> {
    execute_native_messages_request_with_chain(
        router,
        operation,
        messages,
        tools,
        model_override,
        tool_choice_override,
        None,
        None,
    )
    .await
}

/// Variant that threads an opaque provider continuation id across inner-loop
/// turns. The legacy parameter name is retained for compatibility; the router
/// places it in typed `LLMRequest.context_reuse` and the selected stateful
/// adapter owns the wire mapping. The runner clears it at shape, compaction,
/// resume, periodic-rebase, and error boundaries. `None` means a clean
/// bootstrap with no server continuation.
pub async fn execute_native_messages_request_with_chain(
    router: &std::sync::Arc<OperationLlmRouter>,
    operation: &str,
    messages: Vec<RouterMessage>,
    tools: &[NativeExecutionTool],
    model_override: Option<&str>,
    tool_choice_override: Option<Value>,
    previous_response_id: Option<&str>,
    viewport: Option<(u32, u32)>,
) -> Result<ExecutionNativeResponse> {
    let llm_op = LLMOperation::Other(operation.to_string());
    let tool_specs = to_router_tool_specs(tools);

    let raw = router
        .generate_for_execution_native_messages_with_chain(
            &llm_op,
            messages,
            tool_specs,
            model_override,
            tool_choice_override,
            previous_response_id,
            viewport,
        )
        .await?;

    let mut response = ExecutionNativeResponse::unadmitted(
        raw.tool_calls
            .into_iter()
            .map(|tc| ExecutionToolCall {
                id: tc.id,
                name: tc.name,
                arguments: tc.arguments,
            })
            .collect(),
    );
    response.text = raw.text;
    response.reasoning_text = raw.reasoning_text;
    response.response_id = raw.response_id;
    response.finish_reason = raw.finish_reason;
    response.prompt_tokens = raw.prompt_tokens;
    response.completion_tokens = raw.completion_tokens;
    response.cached_tokens = raw.cached_tokens;
    response.cache_creation_tokens = raw.cache_creation_tokens;
    response.provider = raw.provider;
    response.model = raw.model;
    response.reasoning_tokens = raw.reasoning_tokens;
    response.profile = raw.profile;
    response.telemetry = raw.telemetry;
    response.admit_tool_arguments();
    Ok(response)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn to_router_tool_specs_converts_correctly() {
        let tools = vec![
            NativeExecutionTool {
                name: "browser".into(),
                description: "Browse the web".into(),
                parameters: json!({"type": "object", "properties": {}}),
                is_control_tool: false,
            },
            NativeExecutionTool {
                name: "yield".into(),
                description: "Return a terminal outcome".into(),
                parameters: json!({"type": "object", "properties": {}}),
                is_control_tool: true,
            },
        ];
        let specs = to_router_tool_specs(&tools);
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].name, "browser");
        assert_eq!(specs[1].name, "yield");
        assert_eq!(specs[0].description, "Browse the web");
    }

    #[test]
    fn to_router_tool_specs_empty_input() {
        let specs = to_router_tool_specs(&[]);
        assert!(specs.is_empty());
    }

    #[test]
    fn to_router_tool_specs_preserves_parameters() {
        let params = json!({
            "type": "object",
            "properties": {
                "url": { "type": "string" },
                "timeout": { "type": "integer" }
            },
            "required": ["url"]
        });
        let tools = vec![NativeExecutionTool {
            name: "navigate".into(),
            description: "Navigate to URL".into(),
            parameters: params.clone(),
            is_control_tool: false,
        }];
        let specs = to_router_tool_specs(&tools);
        assert_eq!(specs[0].parameters, params);
    }
}

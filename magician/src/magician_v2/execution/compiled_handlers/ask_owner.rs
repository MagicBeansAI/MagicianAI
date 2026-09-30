//! `ask_owner` — envoy puts an external contact's question to the owner;
//! the owner's answer is relayed back to the contact. Compiled handler so it
//! dispatches in the reactive chat path. See `owner_relay` for the shared emit.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::user_requests::RequestOption;

use super::owner_relay::emit_envoy_owner_request;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let Some(question) = args
        .get("question")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(json!({
            "status": "error",
            "reason": "ask_owner requires a non-empty `question`",
        }));
    };
    let context_summary = args
        .get("context_summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let mut prompt = format!("An external contact asks: {question}");
    if let Some(summary) = context_summary {
        prompt.push_str(&format!("\n\nContext: {summary}"));
    }
    let options = vec![
        RequestOption {
            id: "reply".to_string(),
            label: "Reply".to_string(),
            requires_input: true,
        },
        RequestOption {
            id: "decline".to_string(),
            label: "Decline".to_string(),
            requires_input: false,
        },
    ];
    let payload = json!({ "question": question, "context_summary": context_summary });
    emit_envoy_owner_request(
        resources,
        &args,
        "ask_owner",
        "ask_owner",
        prompt,
        options,
        "decline",
        payload,
    )
    .await
}

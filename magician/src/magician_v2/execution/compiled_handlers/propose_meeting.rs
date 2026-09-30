//! `propose_meeting` — envoy proposes a meeting between an external contact and
//! the owner; the owner approves/rejects before anything is booked. On approval
//! the resolve handler creates an owner-owned personal-assistant task to book +
//! relay. Compiled handler so it dispatches in the reactive chat path.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::user_requests::RequestOption;

use super::owner_relay::emit_envoy_owner_request;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let Some(topic) = args
        .get("topic")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(json!({
            "status": "error",
            "reason": "propose_meeting requires a non-empty `topic`",
        }));
    };
    let window = args
        .get("window")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let duration = args
        .get("duration")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let context_summary = args
        .get("context_summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let mut prompt = format!("An external contact proposes a meeting about: {topic}");
    if let Some(window) = window {
        prompt.push_str(&format!("\nProposed time: {window}"));
    }
    if let Some(duration) = duration {
        prompt.push_str(&format!("\nDuration: {duration}"));
    }
    if let Some(summary) = context_summary {
        prompt.push_str(&format!("\nContext: {summary}"));
    }
    let options = vec![
        RequestOption {
            id: "approve".to_string(),
            label: "Approve & book".to_string(),
            requires_input: false,
        },
        RequestOption {
            id: "reject".to_string(),
            label: "Reject".to_string(),
            requires_input: false,
        },
    ];
    let payload = json!({
        "topic": topic,
        "window": window,
        "duration": duration,
        "context_summary": context_summary,
    });
    emit_envoy_owner_request(
        resources,
        &args,
        "propose_meeting",
        "propose_meeting",
        prompt,
        options,
        "reject",
        payload,
    )
    .await
}

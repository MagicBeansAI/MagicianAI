//! `request_owner_action` — envoy requests the owner's approval for an action
//! an external contact is asking for; the owner approves/rejects. On approval
//! the resolve handler creates an owner-owned personal-assistant task to perform
//! it + relay. Compiled handler so it dispatches in the reactive chat path.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::user_requests::RequestOption;

use super::owner_relay::emit_envoy_owner_request;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let Some(action) = args
        .get("action")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(json!({
            "status": "error",
            "reason": "request_owner_action requires a non-empty `action`",
        }));
    };
    let details = args
        .get("details")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let context_summary = args
        .get("context_summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let mut prompt =
        format!("An external contact requests this action, which needs your approval: {action}");
    if let Some(details) = details {
        prompt.push_str(&format!("\nDetails: {details}"));
    }
    if let Some(summary) = context_summary {
        prompt.push_str(&format!("\nContext: {summary}"));
    }
    let options = vec![
        RequestOption {
            id: "approve".to_string(),
            label: "Approve".to_string(),
            requires_input: false,
        },
        RequestOption {
            id: "reject".to_string(),
            label: "Reject".to_string(),
            requires_input: false,
        },
    ];
    let payload = json!({
        "action": action,
        "details": details,
        "context_summary": context_summary,
    });
    emit_envoy_owner_request(
        resources,
        &args,
        "request_owner_action",
        "request_owner_action",
        prompt,
        options,
        "reject",
        payload,
    )
    .await
}

//! `notify_owner` — send a non-blocking briefing or escalation to
//! the agent's human owner. Compiled handler so it dispatches in the reactive
//! chat path (it was previously a harness-only tool and so was silently
//! unreachable from chat). It touches no harness machinery — it only emits a
//! `UserRequest` — which is why it belongs here, not as a harness tool.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::user_requests::{RequestOption, UserRequest};

use super::shared::{require_scope_str, scope_arg_str};

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "notify_owner")?;
    let workspace = require_scope_str(&args, "__workspace", "notify_owner")?;
    let owner_agent_id = scope_arg_str(&args, "__agent_id").unwrap_or_default();

    let Some(user_request_service) = resources.user_request_service.clone() else {
        return Ok(json!({
            "status": "error",
            "reason": "notify_owner: user request service is not configured",
        }));
    };

    let Some(message) = args
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(json!({
            "status": "error",
            "reason": "notify_owner requires a non-empty `message`",
        }));
    };
    let title = args
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let kind = args
        .get("kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("briefing");
    if kind == "question" {
        return Ok(json!({
            "status": "error",
            "reason": "notify_owner: `question` is not supported by the non-blocking notification path",
        }));
    }
    if !matches!(kind, "briefing" | "escalation") {
        return Ok(json!({
            "status": "error",
            "reason": format!("notify_owner: unsupported kind `{kind}`"),
        }));
    }
    let severity = args
        .get("severity")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("info");
    if !matches!(severity, "info" | "warning" | "critical") {
        return Ok(json!({
            "status": "error",
            "reason": format!("notify_owner: unsupported severity `{severity}`"),
        }));
    }
    let timeout_secs = args
        .get("timeout_secs")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(86_400)
        .min(86_400);

    let request = UserRequest {
        id: String::new(),
        request_type: format!("harness.notify_owner.{kind}"),
        question: render_owner_notification_question(title, message, kind),
        options: vec![RequestOption {
            id: "acknowledge".to_string(),
            label: "Acknowledge".to_string(),
            requires_input: false,
        }],
        principal,
        workspace,
        context: json!({
            "owner_agent_id": owner_agent_id,
            "title": title,
            "message": message,
            "kind": kind,
            "severity": severity,
        }),
        source: "harness".to_string(),
        execution_id: scope_arg_str(&args, "__execution_id"),
        task_id: scope_arg_str(&args, "__task_id"),
        timeout_secs,
        default_on_timeout: "acknowledge".to_string(),
        created_at: 0,
        sensitive: None,
    };

    let submission = match user_request_service.submit_nonblocking(request).await {
        Ok(submission) => submission,
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("notify_owner: durable submission failed: {error}"),
            }));
        },
    };

    Ok(json!({
        "status": "queued",
        "request_id": submission.request_id(),
        "kind": kind,
        "severity": severity,
        "delivery": "durable_non_blocking_user_request",
    }))
}

fn render_owner_notification_question(title: Option<&str>, message: &str, kind: &str) -> String {
    let label = match kind {
        "briefing" => "Owner briefing",
        "escalation" => "Owner escalation",
        _ => "Owner notification",
    };
    match title {
        Some(title) => format!("{label}: {title}\n\n{message}"),
        None => format!("{label}\n\n{message}"),
    }
}

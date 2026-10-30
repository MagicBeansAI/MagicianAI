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

/// The claim check of a notification sent from a task run, with the run it
/// was checked against, or `None` outside a task run.
struct CheckedNotification {
    task_id: String,
    execution_id: String,
    check: crate::magician_v2::artifact_v2::synthesis::OwnerTextCheck,
}

async fn checked_notification(
    resources: &AgentResources,
    principal: &str,
    workspace: &str,
    args: &Value,
    message: &str,
) -> Option<CheckedNotification> {
    use crate::magician_v2::artifact_v2::{
        service::ScopeRef,
        synthesis::{check_owner_text_detailed, load_execution_grounding_corpus, OwnerTextReview},
    };
    let task_id = scope_arg_str(args, "__task_id")?;
    let execution_id = scope_arg_str(args, "__execution_id")?;
    let scope = ScopeRef::system_internal_unauthenticated(principal, workspace);
    let corpus = load_execution_grounding_corpus(
        &resources.artifact_workspace,
        &scope,
        &task_id,
        &execution_id,
    )
    .await;
    let router = resources.operation_llm_router.as_ref().map(|router| {
        router.with_scope_context(Some(magicllm::LlmScope::new(
            principal.to_string(),
            workspace.to_string(),
        )))
    });
    let prompt_manager = crate::magician_v2::prompts::global_prompt_manager();
    let review = match (router.as_ref(), prompt_manager.as_deref()) {
        (Some(router), Some(prompt_manager)) => OwnerTextReview::Llm {
            router,
            prompt_manager,
        },
        _ => OwnerTextReview::Figures,
    };
    let check = check_owner_text_detailed(
        Arc::new(corpus),
        message,
        "",
        review,
        "notify_owner_grounding",
    )
    .await;
    Some(CheckedNotification {
        task_id,
        execution_id,
        check,
    })
}

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
    // `warn` is what the VibeDev Autopilot directive used to say, and what a
    // model writes unprompted. Refusing it lost the very report that says a
    // run needs the owner.
    let severity = if severity.eq_ignore_ascii_case("warn") {
        "warning"
    } else {
        severity
    };
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

    // A notification reaches the owner (and their channels) as the agent's
    // claim: when it was sent from a task run, it is checked against that
    // run's tool results first and goes out under the caveat if it does not
    // verify.
    let checked = checked_notification(&resources, &principal, &workspace, &args, message).await;
    let message = checked
        .as_ref()
        .and_then(|checked| checked.check.text.as_deref())
        .unwrap_or(message);
    let scope = crate::magician_v2::artifact_v2::service::ScopeRef::system_internal_unauthenticated(
        &principal, &workspace,
    );

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

    // What the check removed or left unconfirmed, and why, kept on the run
    // (`owner_text_claim_checks.json`) for the request it changed.
    if let Some(checked) = checked.as_ref() {
        crate::magician_v2::artifact_v2::synthesis::record_owner_text_check(
            &resources.artifact_workspace,
            &scope,
            &checked.task_id,
            &checked.execution_id,
            "notify_owner",
            Some(submission.request_id()),
            &checked.check,
        )
        .await;
    }

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

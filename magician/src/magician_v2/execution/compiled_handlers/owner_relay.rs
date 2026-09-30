//! Shared helpers for the envoy owner-consent tools (`ask_owner`,
//! `propose_meeting`, `request_owner_action`).
//!
//! These are chat-facing tools: when the `envoy` agent (handling a guest on a
//! per-sender thread) calls one, it surfaces an owner-consent request via
//! `UserRequestService` — the same path `notify_owner` rides. They touch NO
//! harness machinery (no HarnessScope, no program state), which is why they
//! live on the compiled-handler path rather than as harness tools.
//!
//! The relay of the owner's decision BACK to the contact is driven from the
//! HITL resolve handler (`respond_hitl_handler`'s `user_request` arm), keyed on
//! `request_type == "envoy.<kind>"`, which recovers the routing `context`
//! populated below.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::user_requests::{RequestOption, UserRequest};

use super::shared::{require_scope_str, scope_arg_str};

/// Parse a guest thread id `ext:<channel>:<address>` into its parts. The
/// address may itself contain `:`, so only the first two segments are split.
pub fn parse_guest_thread(thread_id: &str, tool: &str) -> Result<(String, String), String> {
    let rest = thread_id.strip_prefix("ext:").ok_or_else(|| {
        format!(
            "{tool}: only available on an external-contact thread \
             (expected `ext:<channel>:<address>`, got `{thread_id}`)"
        )
    })?;
    let (channel, address) = rest
        .split_once(':')
        .ok_or_else(|| format!("{tool}: malformed guest thread `{thread_id}`"))?;
    if channel.trim().is_empty() || address.trim().is_empty() {
        return Err(format!(
            "{tool}: guest thread `{thread_id}` is missing a channel or address"
        ));
    }
    Ok((channel.to_string(), address.to_string()))
}

/// Build + fire an envoy owner-consent `UserRequest`. Fire-and-forget exactly
/// like `notify_owner`: `ask()` surfaces the request to the owner's global
/// `/attention` and snapshots it to disk (restart-durable). The relay back to
/// the contact happens later in the resolve handler — NOT in this future (it
/// does not survive a restart).
#[allow(clippy::too_many_arguments)]
pub async fn emit_envoy_owner_request(
    resources: Arc<AgentResources>,
    args: &Value,
    request_kind: &str,
    tool: &str,
    question: String,
    options: Vec<RequestOption>,
    default_on_timeout: &str,
    payload: Value,
) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(args, "__principal", tool)?;
    let workspace = require_scope_str(args, "__workspace", tool)?;
    let owner_agent_id = scope_arg_str(args, "__agent_id").unwrap_or_default();

    let Some(user_request_service) = resources.user_request_service.clone() else {
        return Ok(json!({
            "status": "error",
            "reason": format!("{tool}: user request service is not configured"),
        }));
    };
    // The guest thread (recipient identity) comes from the execution context,
    // NEVER from the LLM — the contact must be structural.
    let Some(guest_thread_id) = scope_arg_str(args, "__ui_thread_id") else {
        return Ok(json!({
            "status": "error",
            "reason": format!("{tool}: missing guest thread (no __ui_thread_id); only valid in an external-contact turn"),
        }));
    };
    let (channel, address) = match parse_guest_thread(&guest_thread_id, tool) {
        Ok(parts) => parts,
        Err(reason) => return Ok(json!({ "status": "error", "reason": reason })),
    };

    let request = UserRequest {
        id: String::new(),
        request_type: format!("envoy.{request_kind}"),
        question,
        options,
        principal,
        workspace,
        // Routing context the resolve handler recovers to relay the owner's
        // decision back to the EXACT contact. `channel` + `address` are stored
        // discretely (the `address` may contain `:`) so the recipient is never
        // re-derived by splitting a packed id.
        context: json!({
            "request_kind": request_kind,
            "channel": channel.clone(),
            "address": address,
            "guest_thread_id": guest_thread_id,
            "origin_agent_id": "envoy",
            "owner_agent_id": owner_agent_id,
            "payload": payload,
        }),
        source: "envoy".to_string(),
        execution_id: scope_arg_str(args, "__execution_id"),
        task_id: scope_arg_str(args, "__task_id"),
        timeout_secs: 86_400,
        default_on_timeout: default_on_timeout.to_string(),
        created_at: 0,
        sensitive: None,
    };

    tokio::spawn(async move {
        let _ = user_request_service.ask(request).await;
    });

    Ok(json!({
        "status": "queued",
        "request_kind": request_kind,
        "channel": channel,
        "delivery": "owner_attention",
    }))
}

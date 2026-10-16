use std::{sync::Arc, time::Duration};

use serde_json::{json, Value};

use super::shared::require_scope_str;
use crate::magician_v2::{
    content_sources::RetrievalAuthority,
    execution::{agent_resources::AgentResources, error::ExecutionError},
    user_requests::{RequestOption, UserRequest},
};

const AUTHENTICATED_READ_ACTION: &str = "browser.cdp.read";
const AUTHENTICATED_INTERACTION_ACTION: &str = "browser.cdp.interact_handoff";

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "authorize_content_read")?;
    let workspace = require_scope_str(&args, "__workspace", "authorize_content_read")?;
    // Before the owner is asked. Both authorities this handler grants read the
    // page as him, so an agent whose browser ceiling excludes the owner's Chrome
    // must not put an approval prompt in front of him for something it could not
    // use — an approval granted and then refused downstream wastes his decision
    // and teaches him the prompts are noise.
    super::shared::refuse_identity_bearing_retrieval(&resources, &args, "authorize_content_read")
        .await?;
    let (authority, action_id, authority_label, expected_result, question_verb, request_type) =
        match args
            .get("authority")
            .and_then(Value::as_str)
            .unwrap_or("authenticated_read")
        {
            "authenticated_read" => (
                RetrievalAuthority::AuthenticatedRead,
                AUTHENTICATED_READ_ACTION,
                "authenticated_read",
                "Read page text from one dedicated Magicutor CDP tab without interacting with the \
                 page, then return the extracted text to the assistant model configured for this \
                 task.",
                "read one page",
                "authenticated_content_read_approval",
            ),
            "authenticated_interact" => (
                RetrievalAuthority::AuthenticatedInteract,
                AUTHENTICATED_INTERACTION_ACTION,
                "authenticated_interact",
                "Transfer one domain-bound Magicutor CDP session to the browser agent and return \
                 page observations to the assistant model configured for this task; each page \
                 effect keeps its existing verification and HITL controls.",
                "interact with one site",
                "authenticated_content_interaction_approval",
            ),
            other => {
                return Err(ExecutionError::Step(format!(
                    "unsupported content browser authority `{other}`"
                )))
            },
        };
    let target_url = args
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ExecutionError::Step("authorize_content_read requires `url`".into()))?;
    let parsed = url::Url::parse(target_url)
        .map_err(|error| ExecutionError::Step(format!("invalid authorization URL: {error}")))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(ExecutionError::Step(
            "authorization URL must be an absolute credential-free HTTP URL".into(),
        ));
    }
    let domain = parsed
        .host_str()
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| ExecutionError::Step("authorization URL requires a domain".into()))?;
    let service = resources
        .user_request_service
        .clone()
        .ok_or_else(|| ExecutionError::Step("owner approval service is unavailable".into()))?;
    let response = service
        .ask(UserRequest {
            id: String::new(),
            request_type: request_type.into(),
            question: format!(
                "Allow the assistant to {question_verb} using your signed-in browser session on \
                 {domain} and return private page content to the assistant model configured for \
                 this task?"
            ),
            options: vec![
                RequestOption {
                    id: "approve_once".into(),
                    label: "Approve Once".into(),
                    requires_input: false,
                },
                RequestOption {
                    id: "deny".into(),
                    label: "Deny".into(),
                    requires_input: false,
                },
            ],
            principal: principal.clone(),
            workspace: workspace.clone(),
            context: json!({
                "kind": request_type,
                "domain": domain,
                "action_id": action_id,
                "target_fingerprint": blake3::hash(target_url.as_bytes()).to_hex().to_string(),
                "authority": authority_label,
                "private_content_to_assistant": true,
                "expected_result": expected_result,
            }),
            source: "content_acquisition".into(),
            execution_id: args
                .get("__execution_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            task_id: args
                .get("__task_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            timeout_secs: 300,
            default_on_timeout: "deny".into(),
            created_at: 0,
            sensitive: None,
        })
        .await;
    if response.decision != "approve_once" {
        return Ok(json!({
            "status": "denied",
            "domain": domain,
            "authority": authority_label,
        }));
    }

    let resolver = resources.content_acquisition_resolver().ok_or_else(|| {
        ExecutionError::Step("content acquisition resolver is not configured".into())
    })?;
    let acquisition = resolver
        .resolve(principal, workspace)
        .await
        .map_err(|error| ExecutionError::Step(format!("resolving content acquisition: {error}")))?;
    let config = resources.magician_config_snapshot().content_acquisition;
    let grant = acquisition
        .retrieval_controller(config.progressive_retrieval)
        .issue_authority_grant(
            authority,
            &domain,
            action_id,
            true,
            Duration::from_secs(config.browser.approval_ttl_secs),
        )
        .map_err(|error| {
            ExecutionError::Step(format!("issuing browser approval grant: {error}"))
        })?;

    Ok(json!({
        "status": "approved",
        "authority_grant_id": grant.id,
        "authority": authority_label,
        "domain": grant.domain,
        "action_id": grant.action_id,
        "private_content_to_assistant": grant.private_content_to_assistant,
        "expires_at_ms": grant.expires_at_ms,
        "next": if authority == RetrievalAuthority::AuthenticatedRead {
            "Call content_read with maximum_authority=authenticated_read, allowed_actions=[browser.cdp.read], and this authority_grant_id."
        } else {
            "Call content_read with maximum_authority=authenticated_interact, allowed_actions=[browser.cdp.interact_handoff], and this authority_grant_id; then continue the returned typed handoff with the browser tool."
        },
    }))
}

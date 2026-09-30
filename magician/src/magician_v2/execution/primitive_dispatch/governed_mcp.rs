//! Provider-neutral governed MCP runtime.
//!
//! Remote framing, discovery, pagination, calls, OAuth discovery/registration,
//! PKCE, refresh, and callback exchange stay inside the official SDK boundary in
//! `magician-mcp-client`. This module owns only product scope/profile resolution,
//! trusted local catalog policy, approval-compatible arguments, and Resource
//! Authority settlement. No provider identity is compiled into this executor.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use magician_mcp_client::{
    project_mcp_catalog, McpClient, McpClientConfig, McpOAuthClientIdentity, McpOAuthCoordinator,
    McpToolCallOutcome, McpToolCallResult, McpTransportConfig, StreamableHttpTransportConfig,
};
use rust_decimal::Decimal;
use serde_json::{json, Map, Value};
use tool_runtime_core::{
    credential_profiles::{
        CanonicalCredentialUrl, CredentialProfileBinding, CredentialProfileKey, CredentialScope,
    },
    manifest::{
        ApprovalClass, AuthKind, McpCommercePolicy, McpJsonBooleanCondition, McpMoneyUnit,
        McpToolRiskClass, McpTransport, PolicyFloor, ProfileSelection, RuntimeProtocol,
    },
    manifest_validation::validate_skill_runtime_contract,
    mcp_catalog_policy::McpCatalogPolicyContract,
};

use super::{
    exec_ctx::PrimitiveExecCtx, governed_runtime::GovernedRouteAdmission,
    runner::PrimitiveToolResult,
};
use crate::magician_v2::{
    execution::capability::GovernedRuntimeImplementation,
    mcp_oauth::McpOAuthApi,
    resource_authority::spend_session::{
        admit, MissingBudgetPolicy, SpendAdmission, SpendHold, SpendIntent, SpendOwnerPolicy,
    },
    secrets::{mcp_oauth_vault::SecretStoreMcpOAuthVault, SecretAuditEvent, SecretStore},
    user_requests::{RequestOption, UserRequest},
};

const MAX_ARGUMENT_JSON_BYTES: usize = 16 * 1024 * 1024;
const MAX_RESULT_JSON_BYTES: usize = 32 * 1024 * 1024;
const MAX_TOTAL_SCAN_NODES: usize = 100_000;
const MAX_TOTAL_SCAN_DEPTH: usize = 32;
const MAX_EMBEDDED_JSON_BYTES: usize = 1024 * 1024;

pub(crate) async fn dispatch_governed_mcp(
    _admission: GovernedRouteAdmission,
    runtime: Arc<GovernedRuntimeImplementation>,
    capability_id: String,
    action_id: String,
    arguments: Value,
    exec_ctx: PrimitiveExecCtx,
) -> Result<PrimitiveToolResult, String> {
    let audit_store = exec_ctx
        .secret_store
        .clone()
        .ok_or_else(|| "governed MCP audit authority is unavailable".to_owned())?;
    // Captured BEFORE `exec_ctx` is moved into the dispatch below. The audit
    // that runs after the call still has to name the attempt it audited.
    let effect_id = exec_ctx.effect_id.clone();
    record_mcp_audit(
        &audit_store,
        &capability_id,
        &action_id,
        "admitted",
        effect_id.as_deref(),
    )?;
    let cancellation = exec_ctx.cancellation_token.clone();
    let cancelled_before_dispatch = cancellation
        .as_ref()
        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
    let result = if cancelled_before_dispatch {
        Err("governed MCP operation was cancelled before dispatch".to_owned())
    } else {
        // Do not drop an in-flight MCP future on cancellation. A remote side effect
        // may already have been accepted, and checkout settlement must always run to
        // commit, roll back, or preserve its Resource Authority reservation. Bounded
        // SDK request timeouts remain authoritative once dispatch begins.
        execute_mcp(
            runtime,
            capability_id.clone(),
            action_id.clone(),
            arguments,
            exec_ctx,
        )
        .await
    };
    let outcome = if cancelled_before_dispatch {
        "cancelled"
    } else if result.is_ok() {
        "succeeded"
    } else {
        "failed"
    };
    if let Err(audit_error) = record_mcp_audit(
        &audit_store,
        &capability_id,
        &action_id,
        outcome,
        effect_id.as_deref(),
    ) {
        return match result {
            Ok(mut completed) => {
                // The remote operation may already have committed. Converting it
                // into an ordinary failure would invite an agent to retry a real
                // message, order, or payment. Preserve the successful outcome and
                // make the durability fault explicit for operator recovery.
                tracing::error!(
                    capability_id,
                    action_id,
                    effect_id = ?effect_id,
                    "governed MCP terminal audit failed after remote completion"
                );
                if !completed.stderr.is_empty() {
                    completed.stderr.push('\n');
                }
                completed.stderr.push_str(
                    "Terminal audit persistence failed after remote completion; do not retry this operation automatically.",
                );
                Ok(completed)
            },
            Err(error) => Err(format!("{error}; {audit_error}")),
        };
    }
    result
}

async fn execute_mcp(
    runtime: Arc<GovernedRuntimeImplementation>,
    capability_id: String,
    action_id: String,
    arguments: Value,
    exec_ctx: PrimitiveExecCtx,
) -> Result<PrimitiveToolResult, String> {
    let started = Instant::now();
    if runtime.actions.is_some() {
        return Err("governed MCP package unexpectedly contains CLI actions".to_owned());
    }
    let package = &runtime.package;
    let validated = validate_skill_runtime_contract(&package.contract)
        .map_err(|_| "governed MCP contract validation failed".to_owned())?;
    let RuntimeProtocol::Mcp {
        transport,
        discovery,
        limits,
    } = &package.contract.runtime
    else {
        return Err("governed MCP executor received a non-MCP package".to_owned());
    };
    let McpTransport::StreamableHttp { endpoint: base } = transport else {
        return Err("governed remote MCP execution currently requires Streamable HTTP".to_owned());
    };
    if package.contract.auth.kind != AuthKind::OAuthSession {
        return Err("governed remote MCP execution requires an OAuth session".to_owned());
    }
    let oauth = discovery
        .oauth
        .as_ref()
        .ok_or_else(|| "governed remote MCP OAuth policy is unavailable".to_owned())?;
    let mut arguments = into_bounded_object(arguments)?;
    let endpoint = select_endpoint(package, discovery, base, &mut arguments)?;
    let profile_alias = select_profile(package, &mut arguments)?;
    let (principal, workspace) = required_mcp_scope(&exec_ctx)?;
    let scope = CredentialScope::new(principal, workspace)
        .map_err(|_| "governed MCP scope is invalid".to_owned())?;
    let provider = package
        .contract
        .auth
        .provider
        .as_deref()
        .ok_or_else(|| "governed MCP provider binding is unavailable".to_owned())?;
    let profile = CredentialProfileKey::new(
        scope.clone(),
        provider,
        profile_alias,
        CredentialProfileBinding::McpOauth {
            resource_url: CanonicalCredentialUrl::new(&endpoint.url)
                .map_err(|_| "governed MCP resource binding is invalid".to_owned())?,
            authorization_issuer: CanonicalCredentialUrl::new(&oauth.authorization_issuer)
                .map_err(|_| "governed MCP issuer binding is invalid".to_owned())?,
        },
    )
    .map_err(|_| "governed MCP profile binding is invalid".to_owned())?;
    let store = exec_ctx
        .secret_store
        .clone()
        .ok_or_else(|| "governed MCP credential vault is unavailable".to_owned())?;
    let vault = Arc::new(SecretStoreMcpOAuthVault::new(Arc::clone(&store)));
    let oauth_api = McpOAuthApi::shared()
        .ok_or_else(|| "governed MCP OAuth callback broker is unavailable".to_owned())?;
    let coordinator = Arc::new(
        McpOAuthCoordinator::new(
            profile,
            vault,
            oauth_api.callback_base_url(),
            McpOAuthClientIdentity::DynamicPublic,
        )
        .and_then(|coordinator| coordinator.with_client_name("Magican MCP Client"))
        .and_then(|coordinator| coordinator.with_scopes(oauth.scopes.iter().cloned()))
        .map_err(|error| format!("governed MCP OAuth configuration failed: {}", error.message))?,
    );
    match action_id.as_str() {
        "status" => {
            register_coordinator(&oauth_api, Arc::clone(&coordinator))?;
            let status = coordinator
                .status()
                .await
                .map_err(|error| format!("governed MCP status failed: {}", error.message))?;
            return success(started, json!({"action": "status", "status": status}));
        },
        "auth_start" => {
            let pending = oauth_api
                .register_and_begin(coordinator)
                .await
                .map_err(|error| format!("governed MCP authorization failed: {}", error.message))?;
            return success(
                started,
                json!({
                    "action": "auth_start",
                    "status": "authorization_pending",
                    "browser_opened": pending.browser_opened()
                }),
            );
        },
        "clear_auth" => {
            register_coordinator(&oauth_api, Arc::clone(&coordinator))?;
            let audit = coordinator
                .logout_local()
                .await
                .map_err(|error| format!("governed MCP local logout failed: {}", error.message))?;
            return success(
                started,
                json!({"action": "clear_auth", "status": "cleared", "audit": audit}),
            );
        },
        "list_tools" | "call_tool" => {},
        _ => return Err("governed MCP action is unavailable".to_owned()),
    }

    register_coordinator(&oauth_api, Arc::clone(&coordinator))?;
    let bearer = coordinator
        .bearer_token()
        .await
        .map_err(|error| format!("governed MCP authorization is required: {}", error.message))?;
    let mut config = McpClientConfig::new(McpTransportConfig::StreamableHttp(
        StreamableHttpTransportConfig::new(endpoint.url.clone()).with_bearer_token(bearer),
    ));
    if let Some(timeout) = limits.timeout_secs {
        let timeout = u64::from(timeout);
        config.connect_timeout = Duration::from_secs(timeout.min(120));
        config.request_timeout = Duration::from_secs(timeout.min(600));
    }
    let mut client = McpClient::connect(config)
        .await
        .map_err(|error| format!("governed MCP connection failed: {error}"))?;
    let descriptors = match client.discover_tools().await {
        Ok(descriptors) => descriptors,
        Err(error) => {
            let _ = client.close().await;
            return Err(format!("governed MCP discovery failed: {error}"));
        },
    };
    let policy = match McpCatalogPolicyContract::compile(&capability_id, validated) {
        Ok(policy) => policy,
        Err(_) => {
            let _ = client.close().await;
            return Err("governed MCP catalog policy compilation failed".to_owned());
        },
    };
    let catalog = match project_mcp_catalog(descriptors, &policy) {
        Ok(catalog) => catalog,
        Err(error) => {
            let _ = client.close().await;
            return Err(format!("governed MCP catalog projection failed: {error}"));
        },
    };

    if action_id == "list_tools" {
        let tools = catalog
            .tools()
            .iter()
            .map(|tool| tool.model_definition())
            .collect::<Vec<_>>();
        let result = json!({
            "action": "list_tools",
            "status": "ok",
            "namespace": catalog.namespace(),
            "tools": tools
        });
        let _ = client.close().await;
        return success(started, result);
    }

    let tool_name = match take_required_string(&mut arguments, "tool_name") {
        Ok(tool_name) => tool_name,
        Err(error) => {
            let _ = client.close().await;
            return Err(error);
        },
    };
    let arguments_json =
        take_optional_string(&mut arguments, "arguments_json").unwrap_or_else(|| "{}".to_owned());
    let remote_arguments = match parse_arguments_json(&arguments_json) {
        Ok(arguments) => arguments,
        Err(error) => {
            let _ = client.close().await;
            return Err(error);
        },
    };
    let Some(tool) = catalog
        .tools()
        .iter()
        .find(|tool| tool.local_name() == tool_name || tool.remote_id().remote_name() == tool_name)
    else {
        let _ = client.close().await;
        return Err(
            "governed MCP tool is not present in the projected discovery snapshot".to_owned(),
        );
    };
    let checkout = discovery.commerce.as_ref().is_some_and(|commerce| {
        is_checkout_tool(commerce, tool.remote_id().remote_name(), &remote_arguments)
    });
    if let Err(error) = validate_dynamic_tool_policy(
        &package.contract.policy_floor,
        tool.policy(),
        discovery.commerce.as_ref(),
        checkout,
    ) {
        let _ = client.close().await;
        return Err(error);
    }
    let caller_risk = take_optional_string(&mut arguments, "risk").unwrap_or_default();
    let intent_summary = take_optional_string(&mut arguments, "intent_summary").unwrap_or_default();
    if checkout {
        if caller_risk != "checkout_or_payment" {
            let _ = client.close().await;
            return Err("checkout-like MCP calls require risk=checkout_or_payment".to_owned());
        }
        if intent_summary.trim().len() < 12 {
            let _ = client.close().await;
            return Err("checkout-like MCP calls require a meaningful intent_summary".to_owned());
        }
    }
    let required_approval = tool
        .policy()
        .required_approvals()
        .iter()
        .max()
        .copied()
        .unwrap_or(ApprovalClass::Ordinary);
    if required_approval != ApprovalClass::Ordinary
        && !request_tool_approval(
            &exec_ctx,
            principal,
            workspace,
            &capability_id,
            tool.remote_id().remote_name(),
            required_approval,
            intent_summary.trim(),
            discovery.commerce.as_ref().filter(|_| checkout),
            &arguments,
            endpoint.alias.as_deref(),
        )
        .await
    {
        let _ = client.close().await;
        return Err("governed MCP action was not approved".to_owned());
    }

    let outcome = if let Some(commerce) = discovery.commerce.as_ref().filter(|_| checkout) {
        call_commerce_tool(
            &mut client,
            &catalog,
            tool,
            remote_arguments,
            commerce,
            endpoint.alias.as_deref(),
            &arguments,
            &exec_ctx,
            &capability_id,
        )
        .await
    } else {
        client
            .call_tool(tool.remote_id(), remote_arguments)
            .await
            .map_err(|error| format!("governed MCP tool call failed: {error}"))
    };
    let projected = match outcome {
        Ok(outcome) => project_call_outcome(outcome),
        Err(error) => {
            let _ = client.close().await;
            return Err(error);
        },
    };
    let _ = client.close().await;
    let result = projected?;
    success(
        started,
        json!({"action": "call_tool", "status": "ok", "result": result}),
    )
}

/// Record one governed-MCP execution.
///
/// `effect_id` is what makes the record answerable. Without it the audit says a
/// `tools/call` happened but not WHICH attempt, so the
/// may-already-have-committed path below cannot be resolved after the fact —
/// an operator sees a warning and has to guess.
///
/// Deliberately NOT sent to the remote as an HTTP `Idempotency-Key`. That
/// header is not part of the MCP specification, so a server has no obligation
/// to honour it: sending one would leak an identifier and buy no
/// deduplication. The key's value here is local answerability. Far-side
/// deduplication for MCP would have to be a `tools/call` protocol concern, not
/// a transport header bolted underneath it.
fn record_mcp_audit(
    store: &SecretStore,
    capability_id: &str,
    action_id: &str,
    outcome: &str,
    effect_id: Option<&str>,
) -> Result<(), String> {
    store
        .try_audit_event(
            SecretAuditEvent::new("governed_mcp_execution")
                .with_tool(capability_id)
                .with_action(action_id)
                .with_detail(json!({"outcome": outcome, "effect_id": effect_id}).to_string()),
        )
        .map_err(|_| "governed MCP audit failed".to_owned())
}

#[allow(clippy::too_many_arguments)]
async fn request_tool_approval(
    exec_ctx: &PrimitiveExecCtx,
    principal: &str,
    workspace: &str,
    capability_id: &str,
    remote_tool: &str,
    approval: ApprovalClass,
    intent_summary: &str,
    commerce: Option<&McpCommercePolicy>,
    controls: &Map<String, Value>,
    endpoint_alias: Option<&str>,
) -> bool {
    let Some(service) = exec_ctx.user_request_service.as_ref() else {
        return false;
    };
    let amount = commerce.and_then(|policy| controls.get(&policy.amount_parameter));
    let summary = if intent_summary.is_empty() {
        format!("Call the reviewed `{remote_tool}` MCP tool once.")
    } else {
        intent_summary.to_owned()
    };
    let amount_line = amount
        .and_then(decimal_from_value)
        .zip(commerce)
        .map(|(amount, policy)| format!("\nReviewed amount: {amount} {}", policy.commodity))
        .unwrap_or_default();
    let response = service
        .ask(UserRequest {
            id: String::new(),
            request_type: "mcp_tool_approval".to_owned(),
            question: format!(
                "Approve this action once?\n\n{summary}\nTool: {remote_tool}{amount_line}"
            ),
            options: vec![
                RequestOption {
                    id: "approve_once".to_owned(),
                    label: "Approve Once".to_owned(),
                    requires_input: false,
                },
                RequestOption {
                    id: "deny".to_owned(),
                    label: "Deny".to_owned(),
                    requires_input: false,
                },
            ],
            principal: principal.to_owned(),
            workspace: workspace.to_owned(),
            context: json!({
                "kind": "mcp_tool_approval",
                "capability_id": capability_id,
                "remote_tool": remote_tool,
                "approval_class": approval,
                "intent_summary": summary,
                "endpoint_alias": endpoint_alias,
                "reviewed_amount": amount,
                "owner_agent_id": exec_ctx.agent_id.clone(),
            }),
            source: "governed_mcp".to_owned(),
            execution_id: exec_ctx.execution_id.clone(),
            task_id: exec_ctx.task_id.clone(),
            timeout_secs: 300,
            default_on_timeout: "deny".to_owned(),
            created_at: 0,
            sensitive: None,
        })
        .await;
    response.decision == "approve_once"
}

fn register_coordinator(
    api: &McpOAuthApi,
    coordinator: Arc<McpOAuthCoordinator>,
) -> Result<(), String> {
    api.register_coordinator(coordinator)
        .map_err(|error| format!("governed MCP OAuth registration failed: {}", error.message))
}

fn into_bounded_object(value: Value) -> Result<Map<String, Value>, String> {
    if !bounded_value_shape(&value) {
        return Err("governed MCP arguments exceed local JSON limits".to_owned());
    }
    let encoded = serde_json::to_vec(&value)
        .map_err(|_| "governed MCP arguments are not serializable".to_owned())?;
    if encoded.len() > MAX_ARGUMENT_JSON_BYTES {
        return Err("governed MCP arguments exceed the local size limit".to_owned());
    }
    value
        .as_object()
        .cloned()
        .ok_or_else(|| "governed MCP arguments must be an object".to_owned())
}

struct SelectedEndpoint {
    url: String,
    alias: Option<String>,
}

fn select_endpoint(
    package: &tool_runtime_core::manifest_parser::SkillRuntimePackage,
    discovery: &tool_runtime_core::manifest::McpDiscoveryPolicy,
    base: &str,
    arguments: &mut Map<String, Value>,
) -> Result<SelectedEndpoint, String> {
    if discovery.endpoint_aliases.is_empty() {
        return Ok(SelectedEndpoint {
            url: base.to_owned(),
            alias: None,
        });
    }
    let parameter = package
        .catalog
        .mcp_endpoint_parameter
        .as_deref()
        .unwrap_or("endpoint_alias");
    let selected = take_optional_string(arguments, parameter)
        .or_else(|| discovery.default_endpoint_alias.clone())
        .ok_or_else(|| "governed MCP endpoint alias is required".to_owned())?;
    let url = discovery
        .endpoint_aliases
        .get(&selected)
        .cloned()
        .ok_or_else(|| "governed MCP endpoint alias is not declared".to_owned())?;
    Ok(SelectedEndpoint {
        url,
        alias: Some(selected),
    })
}

fn select_profile(
    package: &tool_runtime_core::manifest_parser::SkillRuntimePackage,
    arguments: &mut Map<String, Value>,
) -> Result<String, String> {
    match &package.contract.auth.profile_selection {
        ProfileSelection::Fixed { alias } => Ok(alias.clone()),
        ProfileSelection::Selectable { default } => {
            let parameter = package
                .projected_profile_parameter()
                .ok_or_else(|| "governed MCP profile parameter is unavailable".to_owned())?;
            take_optional_string(arguments, parameter.name)
                .or_else(|| default.clone())
                .ok_or_else(|| "governed MCP profile alias is required".to_owned())
        },
        ProfileSelection::None => Ok("default".to_owned()),
        ProfileSelection::Implicit => {
            Err("governed MCP OAuth requires an explicit local profile".to_owned())
        },
    }
}

fn take_required_string(arguments: &mut Map<String, Value>, name: &str) -> Result<String, String> {
    take_optional_string(arguments, name)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("governed MCP parameter `{name}` is required"))
}

fn take_optional_string(arguments: &mut Map<String, Value>, name: &str) -> Option<String> {
    arguments
        .remove(name)
        .and_then(|value| value.as_str().map(str::to_owned))
}

fn parse_arguments_json(source: &str) -> Result<Map<String, Value>, String> {
    if source.len() > MAX_ARGUMENT_JSON_BYTES || !bounded_json_shape(source.as_bytes()) {
        return Err("governed MCP arguments_json exceeds local JSON limits".to_owned());
    }
    let value: Value = serde_json::from_str(source)
        .map_err(|_| "governed MCP arguments_json must be valid JSON".to_owned())?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| "governed MCP arguments_json must contain an object".to_owned())
}

fn bounded_json_shape(bytes: &[u8]) -> bool {
    let mut depth = 0usize;
    let mut nodes = 1usize;
    let mut in_string = false;
    let mut escaped = false;
    for byte in bytes {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match *byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth = depth.saturating_add(1);
                nodes = nodes.saturating_add(1);
                if depth > MAX_TOTAL_SCAN_DEPTH || nodes > MAX_TOTAL_SCAN_NODES {
                    return false;
                }
            },
            b'}' | b']' => {
                let Some(next) = depth.checked_sub(1) else {
                    return false;
                };
                depth = next;
            },
            b',' | b':' => {
                nodes = nodes.saturating_add(1);
                if nodes > MAX_TOTAL_SCAN_NODES {
                    return false;
                }
            },
            _ => {},
        }
    }
    !in_string && depth == 0
}

fn is_checkout_tool(
    policy: &McpCommercePolicy,
    remote_name: &str,
    arguments: &Map<String, Value>,
) -> bool {
    if policy.final_tools.contains(remote_name) {
        return true;
    }
    if policy
        .conditional_final_tools
        .get(remote_name)
        .is_some_and(|condition| condition_matches(condition, arguments))
    {
        return true;
    }
    let lowered = remote_name.to_ascii_lowercase();
    policy
        .checkout_name_terms
        .iter()
        .any(|term| lowered.contains(term))
}

fn validate_dynamic_tool_policy(
    floor: &PolicyFloor,
    policy: &tool_runtime_core::mcp_catalog_policy::McpEffectiveToolPolicy,
    commerce: Option<&McpCommercePolicy>,
    checkout: bool,
) -> Result<(), String> {
    if policy
        .required_grants()
        .any(|grant| !floor.required_grants.contains(grant))
    {
        return Err(
            "governed MCP tool requires a dynamic grant that this executor cannot prove".to_owned(),
        );
    }
    if policy.resource_scopes().any(|scope| {
        !floor.resource_scopes.contains(scope)
            && !(checkout && commerce.is_some_and(|value| value.resource_scope == scope))
    }) {
        return Err(
            "governed MCP tool requires a dynamic resource scope outside the active boundary"
                .to_owned(),
        );
    }
    if policy.required_resource_authorities().any(|authority| {
        !floor.required_resource_authorities.contains(authority)
            && !(checkout
                && commerce.is_some_and(|value| value.required_resource_authority == authority))
    }) {
        return Err(
            "governed MCP tool requires a dynamic Resource Authority outside the active boundary"
                .to_owned(),
        );
    }
    Ok(())
}

fn condition_matches(condition: &McpJsonBooleanCondition, arguments: &Map<String, Value>) -> bool {
    let mut segments = condition.pointer.split('/');
    if segments.next() != Some("") {
        return false;
    }
    let Some(first) = segments.next() else {
        return false;
    };
    let first = decode_json_pointer_segment(first);
    let Some(mut current) = arguments.get(first.as_ref()) else {
        return false;
    };
    for segment in segments {
        let segment = decode_json_pointer_segment(segment);
        current = match current {
            Value::Object(map) => match map.get(segment.as_ref()) {
                Some(value) => value,
                None => return false,
            },
            Value::Array(values) => match segment
                .parse::<usize>()
                .ok()
                .and_then(|index| values.get(index))
            {
                Some(value) => value,
                None => return false,
            },
            _ => return false,
        };
    }
    current.as_bool() == Some(condition.equals)
}

fn decode_json_pointer_segment(segment: &str) -> std::borrow::Cow<'_, str> {
    if !segment.contains('~') {
        return std::borrow::Cow::Borrowed(segment);
    }
    std::borrow::Cow::Owned(segment.replace("~1", "/").replace("~0", "~"))
}

#[allow(clippy::too_many_arguments)]
async fn call_commerce_tool(
    client: &mut McpClient,
    catalog: &magician_mcp_client::McpProjectedCatalog,
    final_tool: &magician_mcp_client::McpProjectedTool,
    arguments: Map<String, Value>,
    policy: &McpCommercePolicy,
    endpoint_alias: Option<&str>,
    controls: &Map<String, Value>,
    exec_ctx: &PrimitiveExecCtx,
    capability_id: &str,
) -> Result<McpToolCallOutcome, String> {
    if final_tool.policy().risk() != McpToolRiskClass::Commerce {
        return Err(
            "checkout-like MCP tool is not declared as commerce by local policy".to_owned(),
        );
    }
    let declared = controls
        .get(&policy.amount_parameter)
        .and_then(decimal_from_value)
        .filter(|amount| *amount > Decimal::ZERO)
        .ok_or_else(|| "checkout-like MCP call requires a positive reviewed amount".to_owned())?;
    let declared_minor = (declared * Decimal::from(100u32)).round_dp(0);
    if policy
        .max_order_minor
        .is_some_and(|maximum| declared_minor > Decimal::from(maximum))
    {
        return Err("checkout-like MCP call exceeds the local per-order ceiling".to_owned());
    }

    let verified_minor =
        verify_live_cart(client, catalog, policy, endpoint_alias, declared_minor).await?;
    let reservation = reserve_commerce(
        exec_ctx,
        capability_id,
        &policy.commodity,
        verified_minor / Decimal::from(100u32),
    )
    .await?;
    let call = client.call_tool(final_tool.remote_id(), arguments).await;
    match call {
        Ok(McpToolCallOutcome::Complete(result)) if result.is_error => {
            reservation.rollback().await?;
            Ok(McpToolCallOutcome::Complete(result))
        },
        Ok(outcome @ McpToolCallOutcome::Complete(_)) => {
            reservation.commit().await?;
            Ok(outcome)
        },
        Ok(outcome @ (McpToolCallOutcome::InputRequired(_) | McpToolCallOutcome::Task(_))) => {
            // Preserve the reservation: the provider accepted work but has not
            // produced a terminal result. Recovery reconciles stale reservations.
            reservation.persist().await;
            Ok(outcome)
        },
        Err(error) => {
            // Once the final request is sent, a transport error is ambiguous.
            // Commit the hold and return a non-retry success so the app/chat
            // layer cannot dispatch a second checkout.
            reservation.commit().await?;
            Ok(McpToolCallOutcome::Complete(McpToolCallResult {
                result_type: "text".to_owned(),
                content: vec![json!({
                    "outcome": "complete",
                    "ambiguous_transport": true,
                    "note": "governed MCP checkout outcome is ambiguous after dispatch; the operation may already have been accepted, so do not retry it automatically",
                    "error": error.to_string(),
                })],
                structured_content: None,
                is_error: false,
            }))
        },
    }
}

async fn verify_live_cart(
    client: &McpClient,
    catalog: &magician_mcp_client::McpProjectedCatalog,
    policy: &McpCommercePolicy,
    endpoint_alias: Option<&str>,
    declared_minor: Decimal,
) -> Result<Decimal, String> {
    let candidate = catalog.tools().iter().find(|tool| {
        let name = tool.remote_id().remote_name().to_ascii_lowercase();
        policy
            .cart_name_terms
            .iter()
            .any(|term| name.contains(term))
            && policy
                .cart_read_verb_terms
                .iter()
                .any(|term| name.contains(term))
    });
    let Some(candidate) = candidate else {
        return if policy.allow_cartless_checkout
            || endpoint_alias.is_some_and(|alias| policy.cartless_endpoint_aliases.contains(alias))
        {
            Ok(declared_minor)
        } else {
            Err("checkout blocked because no trusted live cart-read tool was discovered".to_owned())
        };
    };
    let outcome = client
        .call_tool(candidate.remote_id(), Map::new())
        .await
        .map_err(|_| "checkout blocked because the live cart could not be read".to_owned())?;
    let McpToolCallOutcome::Complete(result) = outcome else {
        return if policy.allow_unverified_cart {
            Ok(declared_minor)
        } else {
            Err("checkout blocked because the live cart read did not complete".to_owned())
        };
    };
    if result.is_error {
        return if policy.allow_unverified_cart {
            Ok(declared_minor)
        } else {
            Err("checkout blocked because the live cart returned an error".to_owned())
        };
    }
    let live_minor = extract_total_minor(&result, policy).ok_or_else(|| {
        "checkout blocked because no trusted total could be parsed from the live cart".to_owned()
    })?;
    if live_minor <= Decimal::ZERO {
        return Err("checkout blocked because the live cart total is not positive".to_owned());
    }
    let percentage =
        live_minor * Decimal::from(policy.percentage_tolerance_bps) / Decimal::from(10_000u32);
    let tolerance = Decimal::from(policy.absolute_tolerance_minor).max(percentage);
    if (live_minor - declared_minor).abs() > tolerance {
        return Err(
            "checkout blocked because the reviewed amount does not match the live cart".to_owned(),
        );
    }
    Ok(live_minor)
}

fn extract_total_minor(result: &McpToolCallResult, policy: &McpCommercePolicy) -> Option<Decimal> {
    let mut best = None;
    let mut remaining_nodes = MAX_TOTAL_SCAN_NODES;
    let mut remaining_embedded_bytes = MAX_EMBEDDED_JSON_BYTES;
    let mut embedded_values = Vec::new();
    if let Some(structured) = result.structured_content.as_ref() {
        if !scan_amounts(
            structured,
            policy,
            &mut best,
            &mut remaining_nodes,
            &mut remaining_embedded_bytes,
            Some(&mut embedded_values),
        ) {
            return None;
        }
    }
    for content in &result.content {
        if !scan_amounts(
            content,
            policy,
            &mut best,
            &mut remaining_nodes,
            &mut remaining_embedded_bytes,
            Some(&mut embedded_values),
        ) {
            return None;
        }
    }
    for embedded in &embedded_values {
        if !scan_amounts(
            embedded,
            policy,
            &mut best,
            &mut remaining_nodes,
            &mut remaining_embedded_bytes,
            None,
        ) {
            return None;
        }
    }
    best.map(|(_, amount)| to_minor(amount, policy.cart_amount_unit))
}

fn scan_amounts(
    root: &Value,
    policy: &McpCommercePolicy,
    best: &mut Option<(usize, Decimal)>,
    remaining_nodes: &mut usize,
    remaining_embedded_bytes: &mut usize,
    mut embedded_values: Option<&mut Vec<Value>>,
) -> bool {
    let mut stack = vec![(root, 0usize)];
    while let Some((value, depth)) = stack.pop() {
        let Some(next_remaining) = remaining_nodes.checked_sub(1) else {
            return false;
        };
        *remaining_nodes = next_remaining;
        if depth > MAX_TOTAL_SCAN_DEPTH {
            return false;
        }
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    let normalized = key
                        .chars()
                        .filter(|character| character.is_ascii_alphanumeric())
                        .flat_map(char::to_lowercase)
                        .collect::<String>();
                    if let Some(priority) = policy
                        .total_field_priority
                        .iter()
                        .position(|candidate| candidate == &normalized)
                    {
                        if let Some(amount) = decimal_from_value(child) {
                            match best.as_ref() {
                                None => *best = Some((priority, amount)),
                                Some((current, _)) if priority < *current => {
                                    *best = Some((priority, amount));
                                },
                                Some((current, current_amount))
                                    if priority == *current && amount > *current_amount =>
                                {
                                    *best = Some((priority, amount));
                                },
                                _ => {},
                            }
                        }
                    }
                    stack.push((child, depth.saturating_add(1)));
                }
            },
            Value::Array(values) => {
                for child in values {
                    stack.push((child, depth.saturating_add(1)));
                }
            },
            Value::String(text)
                if embedded_values.is_some()
                    && (text.trim_start().starts_with('{')
                        || text.trim_start().starts_with('['))
                    && bounded_json_shape(text.as_bytes()) =>
            {
                let Some(next_remaining) = remaining_embedded_bytes.checked_sub(text.len()) else {
                    return false;
                };
                *remaining_embedded_bytes = next_remaining;
                if let Ok(embedded) = serde_json::from_str::<Value>(text) {
                    if let Some(values) = embedded_values.as_deref_mut() {
                        values.push(embedded);
                    }
                }
            },
            _ => {},
        }
    }
    true
}

fn bounded_value_shape(value: &Value) -> bool {
    let mut stack = vec![(value, 0usize)];
    let mut visited = 0usize;
    while let Some((value, depth)) = stack.pop() {
        visited = visited.saturating_add(1);
        if visited > MAX_TOTAL_SCAN_NODES || depth > MAX_TOTAL_SCAN_DEPTH {
            return false;
        }
        match value {
            Value::Object(map) => {
                for child in map.values() {
                    stack.push((child, depth.saturating_add(1)));
                }
            },
            Value::Array(values) => {
                for child in values {
                    stack.push((child, depth.saturating_add(1)));
                }
            },
            _ => {},
        }
    }
    true
}

fn to_minor(amount: Decimal, unit: McpMoneyUnit) -> Decimal {
    match unit {
        McpMoneyUnit::Major => (amount * Decimal::from(100u32)).round_dp(0),
        McpMoneyUnit::Minor => amount.round_dp(0),
    }
}

fn decimal_from_value(value: &Value) -> Option<Decimal> {
    match value {
        Value::Number(number) => number.to_string().parse().ok(),
        Value::String(text) => {
            let normalized = text
                .trim()
                .trim_start_matches(|character| character == '₹' || character == '$')
                .replace(',', "");
            normalized.parse().ok()
        },
        _ => None,
    }
}

struct CommerceReservation {
    hold: SpendHold,
}

impl CommerceReservation {
    async fn commit(self) -> Result<(), String> {
        self.hold
            .commit(None)
            .await
            .map_err(|_| "commerce reservation commit failed".to_owned())
    }

    async fn rollback(self) -> Result<(), String> {
        self.hold
            .rollback()
            .await
            .map_err(|_| "commerce reservation rollback failed".to_owned())
    }

    async fn persist(&self) {
        self.hold.persist().await;
    }
}

fn required_mcp_scope(exec_ctx: &PrimitiveExecCtx) -> Result<(&str, &str), String> {
    let principal = exec_ctx
        .principal
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "governed MCP principal is unavailable".to_owned())?;
    let workspace = exec_ctx
        .workspace
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "governed MCP workspace is unavailable".to_owned())?;
    Ok((principal, workspace))
}

async fn reserve_commerce(
    exec_ctx: &PrimitiveExecCtx,
    capability_id: &str,
    commodity: &str,
    amount: Decimal,
) -> Result<CommerceReservation, String> {
    let (principal, workspace) = required_mcp_scope(exec_ctx)
        .map_err(|_| "checkout blocked because principal or workspace is unavailable".to_owned())?;
    let authority = exec_ctx
        .compiled_dispatch_authority
        .as_ref()
        .ok_or_else(|| "checkout blocked because Resource Authority is unavailable".to_owned())?;
    let agent_id = exec_ctx
        .agent_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "checkout blocked because agent identity is unavailable".to_owned())?;
    let active_owner = exec_ctx
        .chat_session_id
        .as_deref()
        .map(|session| format!("chat:{session}"))
        .or_else(|| {
            exec_ctx
                .execution_id
                .as_deref()
                .map(|id| format!("execution:{id}"))
        })
        .unwrap_or_else(|| format!("agent:{agent_id}"));
    let admission = admit(
        authority.resolver().as_ref(),
        SpendIntent {
            principal: principal.to_owned(),
            workspace: workspace.to_owned(),
            agent_id: agent_id.to_string(),
            tool_name: capability_id.to_string(),
            commodity: commodity.to_string(),
            amount,
            missing_budget: MissingBudgetPolicy::Reject,
            owner: SpendOwnerPolicy::Authorized { active_owner },
        },
    )
    .await
    .map_err(|error| match error {
        crate::magician_v2::resource_authority::spend_session::SpendSessionError::Disabled => {
            "checkout blocked because Resource Authority is unavailable".to_owned()
        },
        crate::magician_v2::resource_authority::spend_session::SpendSessionError::NoBudget {
            ..
        } => "checkout blocked because no matching Resource Authority budget exists".to_owned(),
        crate::magician_v2::resource_authority::spend_session::SpendSessionError::Gate(_) => {
            "checkout blocked by Resource Authority budget policy".to_owned()
        },
    })?;
    match admission {
        SpendAdmission::Reserved(hold) => Ok(CommerceReservation { hold }),
        SpendAdmission::Uncounted => {
            Err("checkout blocked because Resource Authority is unavailable".to_owned())
        },
    }
}

fn non_terminal_mcp_projection(continuation: &'static str) -> Value {
    json!({
        "outcome": "complete",
        "remote_error": false,
        "continuation": continuation,
        "note": "the remote MCP server has not produced a terminal result, but the operation may already have been accepted, so do not retry it automatically"
    })
}

fn project_call_outcome(outcome: McpToolCallOutcome) -> Result<Value, String> {
    let value = match outcome {
        McpToolCallOutcome::Complete(result) => json!({
            "outcome": "complete",
            "remote_error": result.is_error,
            "result": result
        }),
        // Returning Err here used to invert a commerce persist/commit into a
        // retryable tool failure. Project a non-retry success envelope instead.
        McpToolCallOutcome::InputRequired(_) => non_terminal_mcp_projection("input_required"),
        McpToolCallOutcome::Task(_) => non_terminal_mcp_projection("task"),
    };
    if serde_json::to_vec(&value)
        .map_err(|_| "governed MCP result serialization failed".to_owned())?
        .len()
        > MAX_RESULT_JSON_BYTES
    {
        return Err("governed MCP result exceeds the local size limit".to_owned());
    }
    Ok(value)
}

fn success(started: Instant, value: Value) -> Result<PrimitiveToolResult, String> {
    let stdout = serde_json::to_string(&value)
        .map_err(|_| "governed MCP result serialization failed".to_owned())?;
    if stdout.len() > MAX_RESULT_JSON_BYTES {
        return Err("governed MCP result exceeds the local size limit".to_owned());
    }
    Ok(PrimitiveToolResult {
        success: true,
        stdout,
        stderr: String::new(),
        parsed_json: Some(value),
        artifacts: Vec::new(),
        elapsed_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use tool_runtime_core::manifest::{McpJsonBooleanCondition, McpMoneyUnit};

    fn commerce() -> McpCommercePolicy {
        McpCommercePolicy {
            commodity: "INR".to_owned(),
            resource_scope: "commerce".to_owned(),
            required_resource_authority: "resource_authority".to_owned(),
            amount_parameter: "order_amount_inr".to_owned(),
            final_tools: std::collections::BTreeSet::from(["checkout".to_owned()]),
            conditional_final_tools: std::collections::BTreeMap::from([(
                "create_order".to_owned(),
                McpJsonBooleanCondition {
                    pointer: "/confirmOrder".to_owned(),
                    equals: true,
                },
            )]),
            checkout_name_terms: std::collections::BTreeSet::from(["payment".to_owned()]),
            cart_name_terms: std::collections::BTreeSet::from(["cart".to_owned()]),
            cart_read_verb_terms: std::collections::BTreeSet::from(["get".to_owned()]),
            total_field_priority: vec!["finalamount".to_owned(), "total".to_owned()],
            cart_amount_unit: McpMoneyUnit::Major,
            absolute_tolerance_minor: 2500,
            percentage_tolerance_bps: 500,
            max_order_minor: Some(100_000),
            allow_cartless_checkout: false,
            cartless_endpoint_aliases: std::collections::BTreeSet::new(),
            allow_unverified_cart: false,
        }
    }

    #[test]
    fn non_terminal_mcp_projection_is_a_non_retry_success() {
        let value = non_terminal_mcp_projection("input_required");
        assert_eq!(value["outcome"], "complete");
        assert_eq!(value["remote_error"], false);
        assert_eq!(value["continuation"], "input_required");
        assert!(value["note"].as_str().unwrap().contains("do not retry"));
    }

    #[test]
    fn mcp_scope_fails_closed_without_principal_or_workspace() {
        let empty = PrimitiveExecCtx::default_for_runtime();
        assert!(required_mcp_scope(&empty).is_err());
        let principal_only =
            PrimitiveExecCtx::default_for_runtime().with_scope(Some("alice".into()), None, None);
        assert!(required_mcp_scope(&principal_only).is_err());
        let blank = PrimitiveExecCtx::default_for_runtime().with_scope(
            Some("  ".into()),
            Some("default".into()),
            None,
        );
        assert!(required_mcp_scope(&blank).is_err());
        let ok = PrimitiveExecCtx::default_for_runtime().with_scope(
            Some("alice".into()),
            Some("home".into()),
            None,
        );
        assert_eq!(required_mcp_scope(&ok).unwrap(), ("alice", "home"));
    }

    #[test]
    fn checkout_classification_is_local_exact_and_conservative() {
        let policy = commerce();
        assert!(is_checkout_tool(&policy, "checkout", &Map::new()));
        assert!(is_checkout_tool(
            &policy,
            "create_order",
            &Map::from_iter([("confirmOrder".to_owned(), Value::Bool(true))])
        ));
        assert!(!is_checkout_tool(
            &policy,
            "create_order",
            &Map::from_iter([("confirmOrder".to_owned(), Value::Bool(false))])
        ));
        assert!(is_checkout_tool(&policy, "open_payment_link", &Map::new()));
    }

    #[test]
    fn bounded_json_shape_rejects_deep_or_amplified_inputs_without_recursion() {
        let deep = format!(
            "{}0{}",
            "[".repeat(MAX_TOTAL_SCAN_DEPTH + 1),
            "]".repeat(MAX_TOTAL_SCAN_DEPTH + 1)
        );
        assert!(!bounded_json_shape(deep.as_bytes()));
        assert!(bounded_json_shape(br#"{"cart":{"total":42}}"#));
    }

    #[test]
    fn total_extraction_prefers_reviewed_fields_and_respects_units() {
        let result = McpToolCallResult {
            result_type: "result".to_owned(),
            content: vec![json!({"type": "text", "text": "{\"total\": 500}"})],
            structured_content: Some(json!({"total": 999, "finalAmount": 842.50})),
            is_error: false,
        };
        assert_eq!(
            extract_total_minor(&result, &commerce()),
            Some(Decimal::new(84250, 0))
        );
    }

    #[test]
    fn total_extraction_uses_the_largest_value_at_the_best_exact_priority() {
        let result = McpToolCallResult {
            result_type: "result".to_owned(),
            content: Vec::new(),
            structured_content: Some(json!({
                "items": [{"finalAmount": 125.0}, {"finalAmount": 275.0}],
                "summary": {"finalAmount": 400.0}
            })),
            is_error: false,
        };
        assert_eq!(
            extract_total_minor(&result, &commerce()),
            Some(Decimal::new(40_000, 0))
        );
    }
}

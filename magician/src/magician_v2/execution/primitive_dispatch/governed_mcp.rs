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
    project_mcp_catalog, McpClient, McpClientConfig, McpOAuthCoordinator, McpToolCallOutcome,
    McpToolCallResult, McpTransportConfig, StreamableHttpTransportConfig,
};
use rust_decimal::Decimal;
use serde_json::{json, Map, Value};
use tool_runtime_core::{
    credential_profiles::CredentialScope,
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
    mcp_oauth_binding::{mcp_oauth_binding, McpOAuthBindingError},
    resource_authority::spend_session::{
        admit, MissingBudgetPolicy, SpendAdmission, SpendHold, SpendIntent, SpendOwnerPolicy,
    },
    secrets::{SecretAuditEvent, SecretStore},
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
    scope_paths: Option<crate::magician_v2::artifact_v2::capabilities::CapabilityScopePaths>,
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
            scope_paths,
        )
        .await
    };
    let outcome = if cancelled_before_dispatch {
        "cancelled"
    } else if result.as_ref().is_ok_and(is_dispatch_unknown) {
        "dispatch_unknown"
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
    scope_paths: Option<crate::magician_v2::artifact_v2::capabilities::CapabilityScopePaths>,
) -> Result<PrimitiveToolResult, String> {
    let started = Instant::now();
    if runtime.actions.is_some() {
        return Err("governed MCP package unexpectedly contains CLI actions".to_owned());
    }
    let package = &runtime.package;
    validate_skill_runtime_contract(&package.contract)
        .map_err(|_| "governed MCP contract validation failed".to_owned())?;
    let RuntimeProtocol::Mcp {
        transport,
        discovery,
        limits,
    } = &package.contract.runtime
    else {
        return Err("governed MCP executor received a non-MCP package".to_owned());
    };
    if let McpTransport::Stdio { executable, args } = transport {
        let paths = scope_paths
            .as_ref()
            .ok_or("governed stdio MCP requires scoped filesystem authority")?;
        let config = super::mcp_stdio::client_config(&runtime, &exec_ctx, paths, executable, args)?;
        return execute_connected_mcp(
            &runtime,
            &capability_id,
            &action_id,
            into_bounded_object(arguments)?,
            &exec_ctx,
            config,
            None,
            None,
            started,
        )
        .await;
    }
    if !matches!(transport, McpTransport::StreamableHttp { .. }) {
        return Err("governed remote MCP execution currently requires Streamable HTTP".to_owned());
    }
    if package.contract.auth.kind == AuthKind::None {
        // This is an explicitly public endpoint, never an OAuth fallback. Keep
        // scope, catalog policy, approvals, resources and audit on the same path.
        required_mcp_scope(&exec_ctx)?;
        let mut arguments = into_bounded_object(arguments)?;
        let (selected, _) = take_binding_selectors(package, discovery, &mut arguments)?;
        let binding =
            super::public_mcp::public_mcp_binding(&package.contract, selected.as_deref())?;
        match action_id.as_str() {
            "status" | "auth_start" | "clear_auth" => {
                let public_profile = match prepare_public_profile(&runtime, &exec_ctx) {
                    Ok(Some(url)) => {
                        json!({"status":"published", "url":url, "reachability_verified":false})
                    },
                    Ok(None) => Value::Null,
                    Err(reason) => json!({"status":"setup_required", "reason":reason}),
                };
                return success(
                    started,
                    json!({
                        "action": action_id, "status": "authentication_not_required",
                        "authorization_started": false, "connection_verified": false,
                        "public_profile": public_profile
                    }),
                );
            },
            "list_tools" | "call_tool" => {},
            _ => return Err("governed MCP action is unavailable".to_owned()),
        }
        let credential_binding = json!({"endpoint": binding.endpoint, "profile": null});
        return execute_connected_mcp(
            &runtime,
            &capability_id,
            &action_id,
            arguments,
            &exec_ctx,
            binding.config,
            binding.alias.as_deref(),
            Some(&credential_binding),
            started,
        )
        .await;
    }
    if package.contract.auth.kind != AuthKind::OAuthSession {
        return Err("governed remote MCP execution requires an OAuth session".to_owned());
    }
    if discovery.oauth.is_none() {
        return Err("governed remote MCP OAuth policy is unavailable".to_owned());
    }
    let mut arguments = into_bounded_object(arguments)?;
    let (endpoint_alias, profile_alias) =
        take_binding_selectors(package, discovery, &mut arguments)?;
    let (principal, workspace) = required_mcp_scope(&exec_ctx)?;
    let scope = CredentialScope::new(principal, workspace)
        .map_err(|_| "governed MCP scope is invalid".to_owned())?;
    // Connect (Settings -> Service accounts) builds its binding through the
    // same helper, so a run finds exactly the login Connect saved.
    let binding = mcp_oauth_binding(
        package,
        scope.clone(),
        endpoint_alias.as_deref(),
        profile_alias.as_deref(),
        runtime.oauth_callback_mode,
    )
    .map_err(binding_error)?;
    let credential_binding =
        json!({"endpoint":binding.endpoint_url,"profile":binding.profile_alias});
    let store = exec_ctx
        .secret_store
        .clone()
        .ok_or_else(|| "governed MCP credential vault is unavailable".to_owned())?;
    let oauth_api = McpOAuthApi::shared()
        .ok_or_else(|| "governed MCP OAuth callback broker is unavailable".to_owned())?;
    let coordinator = Arc::new(
        binding
            .coordinator(Arc::clone(&store), &oauth_api)
            .map_err(|error| {
                format!("governed MCP OAuth configuration failed: {}", error.message)
            })?,
    );
    match action_id.as_str() {
        "status" => {
            // A pending flow already owns its exact coordinator. Status must
            // observe that binding without replacing it or starting a new flow.
            if let Err(error) = oauth_api.register_coordinator(Arc::clone(&coordinator)) {
                if error.code
                    != crate::magician_v2::mcp_oauth::McpOAuthProductErrorCode::PendingAuthorizationConflict
                {
                    return Err(format!("governed MCP OAuth registration failed: {}", error.message));
                }
            }
            let status = oauth_api
                .credential_status(&coordinator.callback_binding_id(), &scope)
                .await
                .map_err(|error| format!("governed MCP status failed: {}", error.message))?;
            return success(
                started,
                json!({"action": "status", "status": status, "authorization_started": false}),
            );
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
        StreamableHttpTransportConfig::new(binding.endpoint_url.clone()).with_bearer_token(bearer),
    ));
    if let Some(timeout) = limits.timeout_secs {
        let timeout = u64::from(timeout);
        config.connect_timeout = Duration::from_secs(timeout.min(120));
        config.request_timeout =
            Duration::from_secs(timeout.min(super::MAX_MCP_REQUEST_TIMEOUT_SECS));
    }
    execute_connected_mcp(
        &runtime,
        &capability_id,
        &action_id,
        arguments,
        &exec_ctx,
        config,
        binding.endpoint_alias.as_deref(),
        Some(&credential_binding),
        started,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn execute_connected_mcp(
    runtime: &GovernedRuntimeImplementation,
    capability_id: &str,
    action_id: &str,
    mut arguments: Map<String, Value>,
    exec_ctx: &PrimitiveExecCtx,
    config: McpClientConfig,
    endpoint_alias: Option<&str>,
    credential_binding: Option<&Value>,
    started: Instant,
) -> Result<PrimitiveToolResult, String> {
    let package = &runtime.package;
    let validated = validate_skill_runtime_contract(&package.contract)
        .map_err(|_| "governed MCP contract validation failed".to_owned())?;
    let RuntimeProtocol::Mcp { discovery, .. } = &package.contract.runtime else {
        return Err("governed MCP runtime is unavailable".into());
    };
    let (principal, workspace) = required_mcp_scope(exec_ctx)?;
    if let Some(bindings) = &runtime.mcp_bindings {
        bindings.validate()?;
    }
    let public_profile_url = if action_id == "call_tool" {
        prepare_public_profile(runtime, exec_ctx)?
    } else {
        None
    };
    let quote_credential = match &config.transport {
        McpTransportConfig::StreamableHttp(http) => http
            .bearer_token
            .as_ref()
            .map(|token| token.session_fingerprint()),
        _ => None,
    };
    let quote_account = blake3::hash(
        &serde_json::to_vec(&json!({
            "principal":principal,"workspace":workspace,"capability":capability_id,
            "endpoint_alias":endpoint_alias,"credential_binding":credential_binding,
        }))
        .map_err(|_| "MCP quote account cannot be pinned")?,
    )
    .to_hex()
    .to_string();
    let quote_scope = blake3::hash(
        &serde_json::to_vec(&json!({
            "principal": principal, "workspace": workspace, "capability": capability_id,
            "endpoint_alias": endpoint_alias, "credential_binding": credential_binding,
            "credential_session": quote_credential, "contract": package.contract,
            "bindings": runtime.mcp_bindings
        }))
        .map_err(|_| "MCP cart quote scope cannot be pinned")?,
    )
    .to_hex()
    .to_string();
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

    // Every discovery already holds the full projected catalog; keep the
    // pack's tool card current so later prompts carry it (`mcp_affordances`).
    {
        let card_tools = catalog
            .tools()
            .iter()
            .filter_map(|tool| serde_json::to_value(tool.model_definition()).ok())
            .collect::<Vec<_>>();
        crate::magician_v2::execution::mcp_affordances::record_catalog(
            &exec_ctx.storage_base_path,
            exec_ctx.principal.as_deref(),
            exec_ctx.workspace.as_deref(),
            &capability_id,
            &catalog.namespace().to_string(),
            &card_tools,
        );
    }

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

    if action_id == "status" {
        let _ = client.close().await;
        return success(
            started,
            json!({"action":"status", "status":"connected", "tools":catalog.tools().len()}),
        );
    }
    if action_id != "call_tool" {
        let _ = client.close().await;
        return Err("MCP action is unavailable for this transport".into());
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
    let mut remote_arguments = match parse_arguments_json(&arguments_json) {
        Ok(arguments) => Value::Object(arguments),
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
    if let (Some(profile), Some(profile_url)) = (
        runtime
            .mcp_bindings
            .as_ref()
            .and_then(|b| b.public_profile.as_ref()),
        public_profile_url.as_deref(),
    ) {
        if let Err(error) = profile.bind(&mut remote_arguments, profile_url) {
            let _ = client.close().await;
            return Err(error);
        }
    }
    if let Err(error) = super::mcp_stdio::bind_paths(
        runtime,
        exec_ctx,
        tool.remote_id().remote_name(),
        &mut remote_arguments,
    ) {
        let _ = client.close().await;
        return Err(error);
    }
    let Some(native_arguments) = remote_arguments.as_object() else {
        let _ = client.close().await;
        return Err("governed MCP arguments must remain a JSON object".into());
    };
    let tool_bindings = runtime
        .mcp_bindings
        .as_ref()
        .and_then(|b| b.tools.get(tool.remote_id().remote_name()));
    let quote_binding = tool_bindings
        .and_then(|b| b.commerce_quote.as_ref())
        .filter(|b| b.applies(endpoint_alias));
    let checkout_binding = tool_bindings
        .and_then(|b| b.commerce_checkout.as_ref())
        .filter(|b| b.applies(endpoint_alias));
    let quoted_endpoint = runtime.mcp_bindings.as_ref().is_some_and(|bindings| {
        bindings.tools.values().any(|binding| {
            binding
                .commerce_quote
                .as_ref()
                .is_some_and(|quote| quote.applies(endpoint_alias))
        })
    });
    let checkout = discovery.commerce.as_ref().is_some_and(|commerce| {
        is_checkout_tool(commerce, tool.remote_id().remote_name(), native_arguments)
    });
    if quoted_endpoint && checkout && checkout_binding.is_none() {
        let _ = client.close().await;
        return Err("this endpoint requires a locally reviewed quote binding for checkout".into());
    }
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
    // Duration grants cover a reviewed MCP action in normal and God modes.
    // Checkout retains its amount/transaction-bound approval on every dispatch.
    let approval_session = (!requires_one_shot_approval(required_approval, checkout))
        .then(|| crate::magician_v2::god_mode::tool_approval_session(exec_ctx))
        .flatten();
    let approved = if required_approval == ApprovalClass::Ordinary && !checkout {
        true
    } else if let Some(session) = approval_session {
        let integration_binding = serde_json::to_string(
            &json!({"contract":package.contract,"bindings":runtime.mcp_bindings,
                "credentials":credential_binding,"endpoint_alias":endpoint_alias}),
        )
        .map_err(|_| "MCP integration cannot be pinned for approval")?;
        let policy_binding = serde_json::to_string(&json!({"integration":integration_binding,
                "tool":tool.model_definition(),"risk":tool.policy().risk(),
                "approvals":tool.policy().required_approvals()}))
        .map_err(|_| "MCP policy cannot be pinned for approval")?;
        let account = credential_binding
            .and_then(|value| value.get("profile"))
            .and_then(Value::as_str);
        let label_parts: Vec<_> = [endpoint_alias, account].into_iter().flatten().collect();
        let integration_label = if label_parts.is_empty() {
            capability_id.to_owned()
        } else {
            format!("{capability_id} ({})", label_parts.join(" / "))
        };
        crate::magician_v2::god_mode::authorize_tool_approval(
            &session,
            crate::magician_v2::god_mode::GodAction::Mcp {
                capability: capability_id,
                tool: tool.remote_id().remote_name(),
                policy_binding: &policy_binding,
                integration_binding: &integration_binding,
                integration_label: &integration_label,
                arguments: &remote_arguments,
                summary: intent_summary.trim(),
            },
            exec_ctx,
        )
        .await
        .is_ok()
    } else {
        request_tool_approval(
            &exec_ctx,
            principal,
            workspace,
            &capability_id,
            tool.remote_id().remote_name(),
            required_approval,
            intent_summary.trim(),
            &remote_arguments,
            discovery.commerce.as_ref().filter(|_| checkout),
            &arguments,
            endpoint_alias,
        )
        .await
    };
    if !approved {
        let _ = client.close().await;
        return Err("governed MCP action was not approved".to_owned());
    }

    if exec_ctx
        .cancellation_token
        .as_ref()
        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
    {
        let _ = client.close().await;
        return Err("governed MCP operation was cancelled before tool dispatch".into());
    }

    let outward = match super::mcp_stdio::prepare_outward(
        runtime,
        exec_ctx,
        capability_id,
        tool.remote_id().remote_name(),
        &remote_arguments,
    )
    .await
    {
        Ok(outward) => outward,
        Err(error) => {
            let _ = client.close().await;
            return Err(error);
        },
    };
    if outward.as_ref().is_some_and(|outward| outward.captured) {
        let _ = client.close().await;
        return success(
            started,
            json!({"status":"prepared", "captured":true, "sent":false}),
        );
    }
    // From here an uncaptured outward act is marked dispatching: the request
    // may leave at any point below. Every failure after this line settles the
    // act explicitly, and one that may follow the send answers with the
    // non-retry envelope — a plain `Err` reads as retryable and sends twice.
    let Value::Object(remote_arguments) = remote_arguments else {
        let _ = client.close().await;
        let error = "governed MCP arguments must remain a JSON object";
        if let Some(outward) = outward {
            outward.settle_not_sent(error);
        }
        return Err(error.into());
    };
    let quote_arguments = quote_binding.map(|_| Value::Object(remote_arguments.clone()));
    // Every unclassified or side-effecting call can change a provider cart,
    // including newly discovered actions covered by a whole-tool grant.
    let mut quote_update = if quote_binding.is_some()
        || (quoted_endpoint && !checkout && tool.policy().risk() != McpToolRiskClass::ReadOnly)
    {
        match super::mcp_commerce::begin_quote_update(
            &exec_ctx.storage_base_path,
            &quote_account,
            &quote_scope,
            tool.remote_id().remote_name(),
        ) {
            Ok(update) => Some(update),
            Err(error) => {
                let _ = client.close().await;
                if let Some(outward) = outward {
                    outward.settle_not_sent(&error);
                }
                return Err(error);
            },
        }
    } else {
        None
    };
    let outcome = if let Some(commerce) = discovery.commerce.as_ref().filter(|_| checkout) {
        call_commerce_tool(
            &mut client,
            &catalog,
            tool,
            remote_arguments,
            commerce,
            endpoint_alias,
            &arguments,
            &exec_ctx,
            &capability_id,
            checkout_binding,
            &quote_scope,
            &quote_account,
        )
        .await
    } else {
        // A local refusal by the client (tool not in the snapshot, argument or
        // request limits, no continuation slot) never left. Once the request
        // was handed to the transport, a failure cannot say whether the
        // remote acted on it.
        client
            .call_tool_reporting_dispatch(tool.remote_id(), remote_arguments)
            .await
            .map_err(|(error, sent)| tool_call_error(&error, sent))
    };
    let _ = client.close().await;
    let (mut result, outward) = match read_back_call(started, outcome, outward, checkout) {
        ReadBack::Projected { result, outward } => (result, outward),
        ReadBack::Finished(finished) => return finished,
    };
    if let (Some(binding), Some(native_arguments), Some(update)) =
        (quote_binding, quote_arguments, quote_update.as_mut())
    {
        let recorded = if result.get("remote_error").and_then(Value::as_bool) == Some(false) {
            super::mcp_commerce::record_quote(
                update,
                binding,
                &native_arguments,
                &super::mcp_bindings::native_documents(&result),
            )
        } else {
            Err("cart tool did not complete successfully".into())
        };
        result["commerce_quote"] = match recorded {
            Ok(id) => json!({"status":"ready", "cart_id":id, "expires_in_secs":600}),
            Err(error) => json!({"status":"unavailable", "reason":error}),
        };
    }
    if let Some(binding) = runtime
        .mcp_bindings
        .as_ref()
        .and_then(|b| b.tools.get(tool.remote_id().remote_name()))
    {
        super::mcp_bindings::redact_result(&mut result, &binding.redact_fields);
    }
    // An outward act or a checkout has reached the remote by now: the message
    // may be sent, the order may be placed. Neither may come back retryable.
    let outward_sent = outward.is_some() || checkout;
    if let Some(outward) = outward {
        outward.settle(&result);
    }
    let artifacts = super::mcp_stdio::result_artifacts(
        runtime,
        exec_ctx,
        tool.remote_id().remote_name(),
        &result,
    );
    let mut completed = match success(
        started,
        json!({"action":"call_tool", "status":"ok", "result":result,
        "artifacts":artifacts.iter().map(|artifact| json!({"path":artifact.original_path,"mime":artifact.content_type})).collect::<Vec<_>>() }),
    ) {
        Ok(completed) => completed,
        // The act is settled above; only this envelope failed. The send
        // happened, so the answer must still not read as retryable.
        Err(error) if outward_sent => {
            tracing::error!(%error, capability_id, checkout, "governed MCP outward or checkout result envelope failed after dispatch; do not retry");
            return unreadable_outward_success(started, &error);
        },
        Err(error) => return Err(error),
    };
    completed.artifacts = artifacts;
    Ok(completed)
}

fn prepare_public_profile(
    runtime: &GovernedRuntimeImplementation,
    exec_ctx: &PrimitiveExecCtx,
) -> Result<Option<String>, String> {
    let Some(profile) = runtime
        .mcp_bindings
        .as_ref()
        .and_then(|b| b.public_profile.as_ref())
    else {
        return Ok(None);
    };
    // Initialized from this engine's reviewed public origin, including for
    // anonymous MCPs. The caller cannot choose a host or another identity.
    let broker = McpOAuthApi::shared().ok_or("public MCP profile origin is unavailable")?;
    let callback = url::Url::parse(broker.callback_base_url())
        .map_err(|_| "public MCP profile origin is invalid")?;
    profile
        .publish(
            &exec_ctx.storage_base_path,
            &callback.origin().ascii_serialization(),
        )
        .map(Some)
}

/// Where a tool call failed relative to its request leaving this process.
enum ToolCallError {
    /// Refused locally; the request never left.
    NotSent(String),
    /// The request was handed to the transport. The remote may have acted on it.
    Unknown(String),
}

/// Classify a client `call_tool` failure by whether the request was sent
/// (`McpClient::call_tool_reporting_dispatch`).
fn tool_call_error(error: &magician_mcp_client::McpClientError, sent: bool) -> ToolCallError {
    let message = format!("governed MCP tool call failed: {error}");
    if sent {
        ToolCallError::Unknown(message)
    } else {
        ToolCallError::NotSent(message)
    }
}

/// The read-back of a call whose request may have left.
enum ReadBack {
    /// A projected result, with the outward attempt still to settle against it.
    Projected {
        result: Value,
        outward: Option<super::mcp_stdio::OutwardAttempt>,
    },
    /// No result can be read back; this is the call's answer.
    Finished(Result<PrimitiveToolResult, String>),
}

/// Project a call's outcome, or settle the outward attempt when no result can
/// be read back.
///
/// A refusal before the send (`NotSent`) is always a plain `Err`; with an
/// outward attempt it also records the act failed. A failure after the request
/// may have left — a transport error, a reservation commit/rollback failure
/// after a checkout's final call, or a result the remote produced that exceeds
/// the local size limit — answers with the non-retry envelope whenever the
/// request was an outward act (recorded as unknown) or a `checkout` (the order
/// may exist, outward binding or not). Any other call keeps the plain `Err` it
/// always had.
fn read_back_call(
    started: Instant,
    outcome: Result<McpToolCallOutcome, ToolCallError>,
    outward: Option<super::mcp_stdio::OutwardAttempt>,
    checkout: bool,
) -> ReadBack {
    let error = match outcome {
        Ok(outcome) => match project_call_outcome(outcome) {
            Ok(result) => return ReadBack::Projected { result, outward },
            // The remote answered; only reading its answer back failed.
            Err(error) => ToolCallError::Unknown(error),
        },
        Err(error) => error,
    };
    ReadBack::Finished(match (outward, error) {
        (None, ToolCallError::Unknown(error)) if checkout => {
            tracing::error!(%error, "governed MCP checkout result unreadable after the final call; do not retry");
            unreadable_outward_success(started, &error)
        },
        (None, ToolCallError::NotSent(error) | ToolCallError::Unknown(error)) => Err(error),
        (Some(outward), ToolCallError::NotSent(error)) => {
            outward.settle_not_sent(&error);
            Err(error)
        },
        (Some(outward), ToolCallError::Unknown(error)) => {
            outward.settle_unknown(&error);
            unreadable_outward_success(started, &error)
        },
    })
}

/// The envelope for an outward call or checkout that may have left but whose
/// result could not be read back.
///
/// It must say neither "done" nor "failed":
///
/// - Not done: `outcome` is `unknown` and `sent` is `unknown`, the top-level
///   `status` is `dispatch_unknown` rather than `ok`, and there is no
///   `remote_error: false` asserting the remote succeeded, so no layer reads it
///   as a delivered message or a placed order.
/// - Not failed: it is still a successful `PrimitiveToolResult`. A
///   `success: false` result is folded into an agent-actionable failure
///   (`capability_invoker::fold_primitive_result`), which reads as "the send
///   failed" and invites the model to send again; a plain `Err` may be retried
///   automatically. The churn steer flags `status: error|failed` and
///   `remote_error: true`, which this deliberately never carries.
fn unreadable_outward_projection(error: &str) -> Value {
    json!({
        "outcome": "unknown",
        "sent": "unknown",
        "retry": false,
        "continuation": "dispatch_unknown",
        "error": error,
        "note": "the MCP call may already have been accepted (a message sent or an order placed), but its result could not be read back; its outcome is unknown, so do not retry it automatically and do not report it as done — check with the provider or the owner first"
    })
}

fn unreadable_outward_success(
    started: Instant,
    error: &str,
) -> Result<PrimitiveToolResult, String> {
    success(
        started,
        json!({"action":"call_tool", "status":"dispatch_unknown",
            "result":unreadable_outward_projection(error), "artifacts":[]}),
    )
}

/// Whether a governed-MCP answer is the dispatch-unknown envelope, so the
/// terminal audit names it rather than calling it a success.
fn is_dispatch_unknown(result: &PrimitiveToolResult) -> bool {
    result
        .parsed_json
        .as_ref()
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str)
        == Some("dispatch_unknown")
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

fn requires_one_shot_approval(approval: ApprovalClass, checkout: bool) -> bool {
    checkout || approval == ApprovalClass::CommerceCheckout
}

#[cfg(test)]
mod approval_duration_tests {
    use super::*;

    #[test]
    fn transaction_approval_never_uses_a_duration_grant() {
        assert!(!requires_one_shot_approval(
            ApprovalClass::ConditionalExternalSideEffect,
            false
        ));
        assert!(requires_one_shot_approval(
            ApprovalClass::ConditionalExternalSideEffect,
            true
        ));
        assert!(requires_one_shot_approval(
            ApprovalClass::CommerceCheckout,
            false
        ));
        assert!(requires_one_shot_approval(
            ApprovalClass::CommerceCheckout,
            true
        ));
        assert!(requires_one_shot_approval(ApprovalClass::Ordinary, true));
    }
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
    remote_arguments: &Value,
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
                "Approve this action once?\n\n{summary}\nTool: {remote_tool}{amount_line}\n\nArguments:\n{}",
                serde_json::to_string_pretty(remote_arguments).unwrap_or_default()
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
                "arguments": remote_arguments,
                "endpoint_alias": endpoint_alias,
                "reviewed_amount": amount,
                "owner_agent_id": exec_ctx.agent_id.clone(),
            }),
            source: "governed_mcp".to_owned(),
            execution_id: exec_ctx.execution_id.clone(),
            task_id: exec_ctx.task_id.clone(),
            // Long enough for an owner away from the screen; an unanswered
            // request still denies.
            timeout_secs: 1800,
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

/// Remove the model-selectable endpoint/profile choices from the call
/// arguments. The binding itself is built by `mcp_oauth_binding`, shared with
/// Service-accounts Connect.
fn take_binding_selectors(
    package: &tool_runtime_core::manifest_parser::SkillRuntimePackage,
    discovery: &tool_runtime_core::manifest::McpDiscoveryPolicy,
    arguments: &mut Map<String, Value>,
) -> Result<(Option<String>, Option<String>), String> {
    let endpoint_alias = if discovery.endpoint_aliases.is_empty() {
        None
    } else {
        let parameter = package
            .catalog
            .mcp_endpoint_parameter
            .as_deref()
            .unwrap_or("endpoint_alias");
        take_optional_string(arguments, parameter)
    };
    let profile_alias = match &package.contract.auth.profile_selection {
        ProfileSelection::Selectable { .. } => {
            let parameter = package
                .projected_profile_parameter()
                .ok_or_else(|| "governed MCP profile parameter is unavailable".to_owned())?;
            take_optional_string(arguments, parameter.name)
        },
        ProfileSelection::Fixed { .. } | ProfileSelection::None | ProfileSelection::Implicit => {
            None
        },
    };
    Ok((endpoint_alias, profile_alias))
}

fn binding_error(error: McpOAuthBindingError) -> String {
    match error {
        McpOAuthBindingError::NotRemoteMcp | McpOAuthBindingError::NotStreamableHttp => {
            "governed remote MCP execution currently requires Streamable HTTP"
        },
        McpOAuthBindingError::ProviderMissing => "governed MCP provider binding is unavailable",
        McpOAuthBindingError::OAuthPolicyMissing => {
            "governed remote MCP OAuth policy is unavailable"
        },
        McpOAuthBindingError::CallbackPolicyInvalid => {
            "governed MCP browser sign-in callback policy is invalid"
        },
        McpOAuthBindingError::EndpointAliasRequired => "governed MCP endpoint alias is required",
        McpOAuthBindingError::EndpointAliasUndeclared => {
            "governed MCP endpoint alias is not declared"
        },
        McpOAuthBindingError::ProfileAliasRequired => "governed MCP profile alias is required",
        McpOAuthBindingError::ProfileImplicit => {
            "governed MCP OAuth requires an explicit local profile"
        },
        McpOAuthBindingError::ResourceInvalid => "governed MCP resource binding is invalid",
        McpOAuthBindingError::IssuerInvalid => "governed MCP issuer binding is invalid",
        McpOAuthBindingError::ProfileInvalid => "governed MCP profile binding is invalid",
    }
    .to_owned()
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
    checkout_binding: Option<&super::mcp_commerce::CheckoutBinding>,
    quote_scope: &str,
    quote_account: &str,
) -> Result<McpToolCallOutcome, ToolCallError> {
    // Everything up to the final `call_tool` is a local refusal: the order
    // request has not left.
    if final_tool.policy().risk() != McpToolRiskClass::Commerce {
        return Err(ToolCallError::NotSent(
            "checkout-like MCP tool is not declared as commerce by local policy".to_owned(),
        ));
    }
    let declared = controls
        .get(&policy.amount_parameter)
        .and_then(decimal_from_value)
        .filter(|amount| {
            *amount > Decimal::ZERO || checkout_binding.is_some() && *amount == Decimal::ZERO
        })
        .ok_or_else(|| {
            ToolCallError::NotSent(
                "checkout-like MCP call requires a positive reviewed amount".to_owned(),
            )
        })?;
    let raw_minor = declared.checked_mul(Decimal::from(100u32)).ok_or_else(|| {
        ToolCallError::NotSent("checkout amount exceeds local numeric bounds".into())
    })?;
    if checkout_binding.is_some() && !raw_minor.fract().is_zero() {
        return Err(ToolCallError::NotSent(
            "checkout amount must use whole minor currency units".into(),
        ));
    }
    let declared_minor = raw_minor.round_dp(0);
    if policy
        .max_order_minor
        .is_some_and(|maximum| declared_minor > Decimal::from(maximum))
    {
        return Err(ToolCallError::NotSent(
            "checkout-like MCP call exceeds the local per-order ceiling".to_owned(),
        ));
    }

    let native_arguments = Value::Object(arguments.clone());
    let mut quote_claim = checkout_binding
        .map(|binding| {
            super::mcp_commerce::claim_quote(
                &exec_ctx.storage_base_path,
                quote_account,
                quote_scope,
                binding,
                &native_arguments,
                declared_minor,
            )
        })
        .transpose()
        .map_err(ToolCallError::NotSent)?;
    let verified_minor = if let Some(claim) = quote_claim.as_mut() {
        claim.amount_minor
    } else {
        verify_live_cart(client, catalog, policy, endpoint_alias, declared_minor)
            .await
            .map_err(ToolCallError::NotSent)?
    };
    let reservation = if verified_minor.is_zero() {
        // Only an explicitly bound, native zero-price quote can reach here.
        CommerceReservation { hold: None }
    } else {
        match reserve_commerce(
            exec_ctx,
            capability_id,
            &policy.commodity,
            verified_minor / Decimal::from(100u32),
        )
        .await
        {
            Ok(reservation) => reservation,
            Err(error) => {
                if let Some(claim) = quote_claim.as_mut() {
                    super::mcp_commerce::release_claim(claim);
                }
                return Err(ToolCallError::NotSent(error));
            },
        }
    };
    let call = client
        .call_tool_reporting_dispatch(final_tool.remote_id(), arguments)
        .await;
    // A client-side refusal before the send placed no order: release the hold
    // and refuse plainly. Past the send, a settlement failure cannot undo it.
    if let Err((error, false)) = &call {
        if let Some(claim) = quote_claim.as_mut() {
            super::mcp_commerce::release_claim(claim);
        }
        let error = tool_call_error(error, false);
        if let Err(rollback) = reservation.rollback().await {
            tracing::error!(%rollback, "checkout hold could not be released after a refusal before send");
        }
        return Err(error);
    }
    let mut call = call.map_err(|(error, _)| error);
    if let (Some(binding), Ok(McpToolCallOutcome::Complete(result))) = (checkout_binding, &mut call)
    {
        if !result.is_error {
            match super::mcp_commerce::checkout_succeeded(
                binding,
                &super::mcp_commerce::documents(result),
            ) {
                Some(true) => {},
                Some(false) => result.is_error = true,
                None => {
                    reservation.commit().await.map_err(ToolCallError::Unknown)?;
                    return Err(ToolCallError::Unknown("checkout returned no verifiable success indicator; check the existing order before retrying".into()));
                },
            }
        }
    }
    match call {
        Ok(McpToolCallOutcome::Complete(result)) if result.is_error => {
            reservation
                .rollback()
                .await
                .map_err(ToolCallError::Unknown)?;
            Ok(McpToolCallOutcome::Complete(result))
        },
        Ok(outcome @ McpToolCallOutcome::Complete(_)) => {
            reservation.commit().await.map_err(ToolCallError::Unknown)?;
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
            // Commit the hold (the order may exist) and answer through the
            // dispatch-unknown envelope (`read_back_call` with `checkout`), so
            // the app/chat layer neither dispatches a second checkout nor
            // reports this one as placed.
            reservation.commit().await.map_err(ToolCallError::Unknown)?;
            Err(ToolCallError::Unknown(format!(
                "governed MCP checkout outcome is ambiguous after dispatch: {error}"
            )))
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
    hold: Option<SpendHold>,
}

impl CommerceReservation {
    async fn commit(self) -> Result<(), String> {
        let Some(hold) = self.hold else {
            return Ok(());
        };
        hold.commit(None)
            .await
            .map_err(|_| "commerce reservation commit failed".to_owned())
    }

    async fn rollback(self) -> Result<(), String> {
        let Some(hold) = self.hold else {
            return Ok(());
        };
        hold.rollback()
            .await
            .map_err(|_| "commerce reservation rollback failed".to_owned())
    }

    async fn persist(&self) {
        if let Some(hold) = &self.hold {
            hold.persist().await;
        }
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
        SpendAdmission::Reserved(hold) => Ok(CommerceReservation { hold: Some(hold) }),
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

    /// An outward act exactly as `prepare_outward` leaves it for a live send:
    /// prepared, dispatching, then dispatch-unknown awaiting its receipt.
    fn dispatched_outward_attempt(
        root: &std::path::Path,
    ) -> (
        crate::magician_v2::evidence::OutwardAssertionStore,
        crate::magician_v2::evidence::OutwardScope,
        String,
        super::super::mcp_stdio::OutwardAttempt,
    ) {
        use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
        use crate::magician_v2::evidence::{OutwardAssertionStore, OutwardScope};

        let store = OutwardAssertionStore::new(ArtifactV2Workspace::new(root));
        let scope = OutwardScope::new("anonymous", "default");
        let binding: super::super::mcp_bindings::McpOutwardBinding = serde_json::from_value(
            json!({"channel": "whatsapp", "provider": "whatsapp", "recipients": ["/to"]}),
        )
        .expect("outward binding");
        let act = store
            .prepare_dispatch(
                &scope,
                "whatsapp",
                "send_message",
                binding.channel,
                "agent",
                br#"{"to":"a@s.whatsapp.net","text":"hi"}"#,
                vec!["a@s.whatsapp.net".to_owned()],
                None,
                binding.consequence_class,
                "t0",
            )
            .expect("prepare");
        let act_ref = act.outward_act_ref;
        store
            .mark_dispatching(&scope, &act_ref, "t1")
            .expect("dispatching");
        store
            .mark_dispatch_unknown(
                &scope,
                &act_ref,
                "MCP dispatched; awaiting a provider receipt",
                "t1",
            )
            .expect("dispatch unknown");
        let reader = OutwardAssertionStore::new(ArtifactV2Workspace::new(root));
        let attempt = super::super::mcp_stdio::OutwardAttempt::for_test(
            store,
            scope.clone(),
            act_ref.clone(),
            binding,
        );
        (reader, scope, act_ref, attempt)
    }

    fn oversize_outcome() -> McpToolCallOutcome {
        McpToolCallOutcome::Complete(McpToolCallResult {
            result_type: "result".to_owned(),
            content: vec![json!({
                "type": "text",
                "text": "x".repeat(MAX_RESULT_JSON_BYTES + 1)
            })],
            structured_content: None,
            is_error: false,
        })
    }

    fn assert_non_retry_envelope(answer: ReadBack) {
        let ReadBack::Finished(answer) = answer else {
            panic!("no result can be read back, so the call must be finished here");
        };
        let answer =
            answer.expect("a possible outward send must never come back as a retryable error");
        // Not a failure: `success: false` folds into an agent-actionable
        // "failed", which invites the model to send again.
        assert!(answer.success);
        assert!(is_dispatch_unknown(&answer));
        let value = answer.parsed_json.as_ref().expect("envelope");
        // Not a success either: nothing in it reads as done.
        assert_eq!(value["status"], "dispatch_unknown");
        assert_ne!(value["status"], "ok");
        assert_eq!(value["result"]["outcome"], "unknown");
        assert_eq!(value["result"]["sent"], "unknown");
        assert_eq!(value["result"]["retry"], false);
        assert!(value["result"].get("remote_error").is_none());
        assert_eq!(value["result"]["continuation"], "dispatch_unknown");
        assert!(value["result"]["note"]
            .as_str()
            .unwrap()
            .contains("do not retry"));
        // The agent reads stdout; it carries the same answer.
        let stdout: Value = serde_json::from_str(&answer.stdout).expect("stdout json");
        assert_eq!(&stdout, value);
    }

    /// The remote accepted the send and answered with more than the local
    /// limit allows. Retrying would send the message again, so the answer is
    /// the non-retry envelope and the act is recorded as an open question.
    #[test]
    fn an_oversize_result_after_an_outward_send_never_invites_a_retry() {
        use crate::magician_v2::evidence::OutwardActStatus;

        let tmp = tempfile::tempdir().expect("temp dir");
        let (reader, scope, act_ref, attempt) = dispatched_outward_attempt(tmp.path());
        let before = reader.load_act_history(&scope, &act_ref).unwrap().len();

        let answer = read_back_call(Instant::now(), Ok(oversize_outcome()), Some(attempt), false);
        if let ReadBack::Finished(Ok(completed)) = &answer {
            let error = completed.parsed_json.as_ref().unwrap()["result"]["error"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            assert!(error.contains("size limit"), "{error}");
        }
        assert_non_retry_envelope(answer);

        let history = reader.load_act_history(&scope, &act_ref).unwrap();
        assert_eq!(
            history.len(),
            before + 1,
            "the read-back failure is recorded explicitly"
        );
        assert_eq!(history.last(), Some(&OutwardActStatus::DispatchUnknown));
        let act = reader.load_act(&scope, &act_ref).unwrap().unwrap();
        assert_eq!(act.status, OutwardActStatus::DispatchUnknown);
        assert!(act.provider_message_id.is_none());

        // With no outward act in flight the same failure stays an ordinary error.
        assert!(matches!(
            read_back_call(Instant::now(), Ok(oversize_outcome()), None, false),
            ReadBack::Finished(Err(error)) if error.contains("size limit")
        ));
    }

    #[test]
    fn a_transport_error_after_an_outward_send_never_invites_a_retry() {
        use crate::magician_v2::evidence::OutwardActStatus;

        let tmp = tempfile::tempdir().expect("temp dir");
        let (reader, scope, act_ref, attempt) = dispatched_outward_attempt(tmp.path());
        assert_non_retry_envelope(read_back_call(
            Instant::now(),
            Err(ToolCallError::Unknown(
                "governed MCP tool call failed: broken pipe".into(),
            )),
            Some(attempt),
            false,
        ));
        assert_eq!(
            reader.load_act(&scope, &act_ref).unwrap().unwrap().status,
            OutwardActStatus::DispatchUnknown
        );
    }

    #[test]
    fn a_refusal_before_the_send_fails_the_act_and_stays_an_error() {
        use crate::magician_v2::evidence::OutwardActStatus;

        let tmp = tempfile::tempdir().expect("temp dir");
        let (reader, scope, act_ref, attempt) = dispatched_outward_attempt(tmp.path());
        assert!(matches!(
            read_back_call(
                Instant::now(),
                Err(ToolCallError::NotSent("cart could not be verified".into())),
                Some(attempt),
                false,
            ),
            ReadBack::Finished(Err(_))
        ));
        assert_eq!(
            reader.load_act(&scope, &act_ref).unwrap().unwrap().status,
            OutwardActStatus::Failed
        );
    }

    /// A checkout's final call reached the remote and the order may have been
    /// placed; the answer is larger than the local limit. With no outward
    /// binding at all, a retryable error would place a second order, so the
    /// answer is the non-retry envelope.
    #[test]
    fn an_oversize_result_after_a_checkout_never_invites_a_retry() {
        let answer = read_back_call(Instant::now(), Ok(oversize_outcome()), None, true);
        if let ReadBack::Finished(Ok(completed)) = &answer {
            let error = completed.parsed_json.as_ref().unwrap()["result"]["error"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            assert!(error.contains("size limit"), "{error}");
        }
        assert_non_retry_envelope(answer);

        // A reservation commit/rollback failure after the final call is the same.
        assert_non_retry_envelope(read_back_call(
            Instant::now(),
            Err(ToolCallError::Unknown(
                "commerce reservation commit failed".into(),
            )),
            None,
            true,
        ));

        // A refusal before the final call never left: it stays an error.
        assert!(matches!(
            read_back_call(
                Instant::now(),
                Err(ToolCallError::NotSent(
                    "checkout-like MCP call exceeds the local per-order ceiling".into()
                )),
                None,
                true,
            ),
            ReadBack::Finished(Err(_))
        ));
    }

    /// The client refuses some calls before anything is sent (tool outside the
    /// snapshot, argument/request limits, no continuation slot). Those placed no
    /// message and no order: the act is failed and the answer an ordinary
    /// error. Only a failure after the request left is an open question.
    #[test]
    fn a_client_refusal_before_send_fails_the_act_and_a_transport_failure_does_not() {
        use crate::magician_v2::evidence::OutwardActStatus;
        use magician_mcp_client::McpClientError;

        for refusal in [
            McpClientError::ToolNotDiscovered,
            McpClientError::ContinuationCapacityExceeded,
        ] {
            assert!(matches!(
                tool_call_error(&refusal, false),
                ToolCallError::NotSent(_)
            ));
            let tmp = tempfile::tempdir().expect("temp dir");
            let (reader, scope, act_ref, attempt) = dispatched_outward_attempt(tmp.path());
            assert!(matches!(
                read_back_call(
                    Instant::now(),
                    Err(tool_call_error(&refusal, false)),
                    Some(attempt),
                    false,
                ),
                ReadBack::Finished(Err(_))
            ));
            assert_eq!(
                reader.load_act(&scope, &act_ref).unwrap().unwrap().status,
                OutwardActStatus::Failed
            );
            // A checkout refused by the client placed no order either.
            assert!(matches!(
                read_back_call(
                    Instant::now(),
                    Err(tool_call_error(&refusal, false)),
                    None,
                    true
                ),
                ReadBack::Finished(Err(_))
            ));
        }

        let tmp = tempfile::tempdir().expect("temp dir");
        let (reader, scope, act_ref, attempt) = dispatched_outward_attempt(tmp.path());
        let timeout = tool_call_error(&McpClientError::CallTimeout, true);
        assert!(matches!(timeout, ToolCallError::Unknown(_)));
        assert_non_retry_envelope(read_back_call(
            Instant::now(),
            Err(timeout),
            Some(attempt),
            false,
        ));
        assert_eq!(
            reader.load_act(&scope, &act_ref).unwrap().unwrap().status,
            OutwardActStatus::DispatchUnknown
        );
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

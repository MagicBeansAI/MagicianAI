//! Helpers shared across compiled handlers.
//!
//! Scope params (`__principal`, `__workspace`, `__agent_id`) are runtime-owned.
//! The generic provider overwrites its bound principal/workspace, while each
//! authenticated dispatch surface injects the active source agent explicitly;
//! model-authored `__*` arguments are removed before either path arrives here.

use serde_json::Value;

use crate::magician_v2::execution::error::ExecutionError;

/// Pull a required runtime-owned string param. Used for bound scope and source
/// identity; absence is a fail-closed dispatch error.
pub fn require_scope_str(
    args: &Value,
    key: &str,
    tool_name: &str,
) -> Result<String, ExecutionError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            ExecutionError::Step(format!(
                "{tool_name} handler missing scope param `{key}` (should have been injected by GenericCompiledProvider::execute)",
            ))
        })
}

/// Pull an optional runtime-owned string param for handlers that genuinely
/// support operation without an active agent identity.
pub fn scope_arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|v| !v.is_empty())
}

pub fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|v| !v.is_empty())
}

/// Enforce the active agent's browser-transport ceiling for a compiled handler
/// that launches a browser outside primitive dispatch.
///
/// `__agent_id`, principal, and workspace are runtime-owned inputs. Calls with
/// no agent identity are owner/system routes and therefore have no agent
/// ceiling to consult; an agent route with a missing definition, unreadable
/// store, or invalid ceiling fails closed.
pub async fn require_browser_transport(
    resources: &std::sync::Arc<crate::magician_v2::execution::agent_resources::AgentResources>,
    args: &Value,
    caller: &str,
    requested: &crate::magician_v2::execution::primitive_dispatch::browser::ConnectionMode,
) -> Result<(), crate::magician_v2::execution::error::ExecutionError> {
    use crate::magician_v2::execution::{
        error::ExecutionError, primitive_dispatch::browser::BrowserTransportCeiling,
    };

    let Some((agent_id, record)) =
        agent_definition_for_browser_ceiling(resources, args, caller).await?
    else {
        return Ok(());
    };
    let ceiling = BrowserTransportCeiling::parse(&record.definition.browser_transports)
        .map_err(|error| ExecutionError::Step(format!("{caller}: {error:#}")))?;
    ceiling
        .resolve(requested.clone())
        .map(|_| ())
        .map_err(|error| ExecutionError::Step(format!("{caller}: agent `{agent_id}`: {error:#}")))
}

async fn agent_definition_for_browser_ceiling(
    resources: &std::sync::Arc<crate::magician_v2::execution::agent_resources::AgentResources>,
    args: &Value,
    caller: &str,
) -> Result<
    Option<(
        String,
        crate::magician_v2::agents::definition_store::DefinitionRecord,
    )>,
    crate::magician_v2::execution::error::ExecutionError,
> {
    use crate::magician_v2::execution::error::ExecutionError;

    let Some(agent_id) = scope_arg_str(args, "__agent_id") else {
        return Ok(None);
    };
    let store = match (
        scope_arg_str(args, "__principal"),
        scope_arg_str(args, "__workspace"),
    ) {
        (Some(principal), Some(workspace)) => resources
            .agent_definition_store
            .for_scope(&principal, &workspace),
        _ => (*resources.agent_definition_store).clone(),
    };
    let record = store.get_definition(&agent_id).await.map_err(|error| {
        ExecutionError::Step(format!(
            "{caller}: the agent definition store could not be read, so `{agent_id}`'s browser \
             transport ceiling is unknown and cannot be treated as permitted: {error}"
        ))
    })?;
    let record = record.ok_or_else(|| {
        ExecutionError::Step(format!(
            "{caller}: no definition for agent `{agent_id}`, so its browser transport ceiling \
             cannot be read"
        ))
    })?;
    Ok(Some((agent_id, record)))
}

/// Refuse an identity-bearing retrieval authority to an agent whose browser
/// ceiling forbids the owner's Chrome.
///
/// # Why this exists outside the browser dispatch
///
/// `browser_transports` is enforced in `primitive_dispatch::dispatch`, which is
/// where a `browser` tool call resolves its transport. The authenticated
/// retrieval reader never goes through it: `content_read` reaches
/// `BrowserContentReader::read_with_engine`, which builds its own session from
/// `RetrievalBrowserMode::AuthenticatedCdpRead` — and that mode attaches to the
/// owner's signed-in Chrome exactly as `ConnectionMode::Cdp` does.
///
/// So without this an agent declaring `browser_transports: [headless, headed]`
/// is refused the owner's browser through one door and handed it through
/// another, and the ceiling's stated guarantee is false. `web-researcher` holds
/// `content_read`, so this is reachable rather than theoretical.
///
/// # Where it is called, and why twice
///
/// At the point authority is REQUESTED (`authorize_content_read`) so a barred
/// agent never puts an approval prompt in front of the owner for something it
/// could not use, and at the point it is EXERCISED (`content_read`) because that
/// is the enforcement. One rule, one definition, two gates — an approval granted
/// and then refused downstream would waste the owner's decision.
///
/// An agent id that names no definition, or a definition store that cannot be
/// read, refuses. An unreadable ceiling is not an absent one.
pub async fn refuse_identity_bearing_retrieval(
    resources: &std::sync::Arc<crate::magician_v2::execution::agent_resources::AgentResources>,
    args: &Value,
    caller: &str,
) -> Result<(), crate::magician_v2::execution::error::ExecutionError> {
    use crate::magician_v2::execution::{
        error::ExecutionError, primitive_dispatch::browser::BrowserTransportCeiling,
    };

    let Some((agent_id, record)) =
        agent_definition_for_browser_ceiling(resources, args, caller).await?
    else {
        return Ok(());
    };
    let ceiling = BrowserTransportCeiling::parse(&record.definition.browser_transports)
        .map_err(|error| ExecutionError::Step(format!("{caller}: {error:#}")))?;
    if ceiling.permits_owner_browser() {
        return Ok(());
    }
    Err(ExecutionError::Step(format!(
        "NOT READ — `{agent_id}` declares `browser_transports: {}`, which excludes the owner's \
         signed-in browser, and an authenticated retrieval reads the page as him. Ask for a \
         public read, or delegate to an agent whose ceiling allows it.",
        ceiling.label()
    )))
}

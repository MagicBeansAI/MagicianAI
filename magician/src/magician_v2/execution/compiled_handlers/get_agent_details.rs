//! `get_agent_details` — lean, universal capability lookup for an agent.
//!
//! Given an `agent_id` reachable in the caller's scope, returns the agent's
//! GUIDE (persona + description) and its CANONICAL LEAF NAMES
//! (`<pack>__<action>`) plus its own delegation targets — just enough for an
//! orchestrating LLM to decide whether to delegate to it. Deliberately lean:
//! the delegation roster shows only name + description, and this tool fetches
//! the concrete capabilities on demand. For heavy harness-management detail
//! (recent tasks / episodes / runtime status) use the `inspect_agent` tool.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

fn normalize_agent_lookup_token(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "get_agent_details")?;
    let workspace = require_scope_str(&args, "__workspace", "get_agent_details")?;
    let Some(requested_agent_id) = args
        .get("agent_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(json!({
            "status": "error",
            "reason": "get_agent_details requires a non-empty `agent_id` argument.",
        }));
    };

    // Resolve the agent definition in the caller's scope. The store on
    // `AgentResources` is already scope-bound, so an `agent_id` outside this
    // principal/workspace simply resolves to `None` (callers can only inspect
    // agents they can reach — e.g. their delegation targets).
    let scoped = resources
        .agent_definition_store
        .for_scope(&principal, &workspace);
    let record = match scoped.get_definition(requested_agent_id).await {
        Ok(Some(record)) => record,
        Ok(None) => {
            let requested = normalize_agent_lookup_token(requested_agent_id);
            match scoped.list_definitions().await {
                Ok(records) => match records.into_iter().find(|record| {
                    let definition = &record.definition;
                    normalize_agent_lookup_token(&definition.name) == requested
                        || definition
                            .aliases
                            .iter()
                            .any(|alias| normalize_agent_lookup_token(alias) == requested)
                }) {
                    Some(record) => record,
                    None => {
                        return Ok(json!({
                            "status": "error",
                            "reason": format!("agent `{requested_agent_id}` was not found in this scope"),
                        }));
                    },
                },
                Err(error) => {
                    return Ok(json!({
                        "status": "error",
                        "reason": format!("failed to resolve agent `{requested_agent_id}`: {error}"),
                    }));
                },
            }
        },
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("failed to load agent `{requested_agent_id}`: {error}"),
            }));
        },
    };

    let definition = &record.definition;
    let Some(source_agent_id) = args
        .get("__agent_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(json!({
            "status": "error",
            "reason": "get_agent_details requires a runtime-bound source agent",
        }));
    };
    // Surface-only agents remain hidden from every other caller, but the
    // currently bound agent may inspect its own scoped definition. Apply this
    // after resolving the unforgeable runtime source id; doing it before the
    // self check would make Loom's own introspection unusable.
    if definition.agent_id != source_agent_id
        && !definition
            .invocation_policy
            .is_discoverable_by_exact_lookup()
    {
        return Ok(json!({
            "status": "error",
            "reason": format!("agent `{requested_agent_id}` was not found in this scope"),
        }));
    }
    let records = match scoped.list_definitions().await {
        Ok(records) => records,
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("failed to resolve source agent policy: {error}"),
            }));
        },
    };
    let Some(source_definition) = records
        .iter()
        .find(|candidate| candidate.definition.agent_id == source_agent_id)
        .map(|candidate| &candidate.definition)
    else {
        return Ok(json!({
            "status": "error",
            "reason": "source agent is unavailable in this scope",
        }));
    };
    let disabled = crate::magician_v2::agents::disabled_agent_hierarchy(
        records.iter().map(|candidate| &candidate.definition),
    );
    if disabled.contains(source_agent_id) || disabled.contains(&definition.agent_id) {
        return Ok(json!({
            "status": "error",
            "reason": format!("agent `{requested_agent_id}` was not found in this scope"),
        }));
    }
    let mut reachable =
        crate::magician_v2::agents::resolve_effective_delegation_target_ids_for_surface(
            source_definition,
            records.iter().map(|candidate| &candidate.definition),
            &disabled,
            crate::magician_v2::agents::InvocationSurface::Delegation,
        )
        .into_iter()
        .collect::<HashSet<_>>();
    reachable.extend(
        crate::magician_v2::agents::resolve_effective_delegation_target_ids_for_surface(
            source_definition,
            records.iter().map(|candidate| &candidate.definition),
            &disabled,
            crate::magician_v2::agents::InvocationSurface::Handover,
        ),
    );
    // An agent may always inspect its own scoped definition. Other agents must
    // be exact, currently reachable delegation targets; this preserves the
    // useful self-introspection path without reopening ambient/global lookup.
    if definition.agent_id != source_agent_id && !reachable.contains(&definition.agent_id) {
        return Ok(json!({
            "status": "error",
            "reason": format!("agent `{requested_agent_id}` was not found in this scope"),
        }));
    }
    let agent_id = definition.agent_id.as_str();

    // Expand the agent's tool/pack allowlist into the same canonical
    // `<pack>__<action>` leaf names the orchestrator would dispatch. Falls back
    // to the bare allowlist entry when no index is installed or for legacy /
    // skillshub packs the index does not promote.
    let index = resources.tool_index();
    let registered_pack_names = index.as_ref().map(|index| index.pack_names());
    let effective_tools = crate::magician_v2::chat::service::resolved_chat_tools_from_definition(
        definition,
        registered_pack_names.as_deref(),
    );
    let mut canonical_tool_names: Vec<String> = Vec::new();
    for pack in &effective_tools {
        let leaves = index
            .as_deref()
            .map(|idx| idx.leaf_names_for_pack(pack))
            .unwrap_or_default();
        if leaves.is_empty() {
            canonical_tool_names.push(pack.clone());
        } else {
            canonical_tool_names.extend(leaves);
        }
    }

    let effective_delegation_targets =
        crate::magician_v2::agents::resolve_effective_delegation_target_ids_for_surface(
            definition,
            records.iter().map(|candidate| &candidate.definition),
            &disabled,
            crate::magician_v2::agents::InvocationSurface::Delegation,
        );
    let effective_handover_targets =
        crate::magician_v2::agents::resolve_effective_delegation_target_ids_for_surface(
            definition,
            records.iter().map(|candidate| &candidate.definition),
            &disabled,
            crate::magician_v2::agents::InvocationSurface::Handover,
        );

    Ok(json!({
        "status": "ok",
        "agent_id": agent_id,
        "requested_agent_id": requested_agent_id,
        "name": definition.name,
        "aliases": definition.aliases,
        "description": definition.description,
        "guide": definition.persona,
        "canonical_tool_names": canonical_tool_names,
        "tool_policy_basis": "resolved_scope_registry",
        "delegation_targets": effective_delegation_targets,
        "handover_targets": effective_handover_targets,
    }))
}

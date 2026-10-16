//! `list_agents` — read-only listing of the caller and its currently reachable
//! delegation/handover targets.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "list_agents")?;
    let workspace = require_scope_str(&args, "__workspace", "list_agents")?;
    let source_agent_id = require_scope_str(&args, "__agent_id", "list_agents")?;

    let scoped = resources
        .agent_definition_store
        .for_scope(&principal, &workspace);
    match scoped.list_definitions().await {
        Ok(records) => {
            let disabled = crate::magician_v2::agents::disabled_agent_hierarchy(
                records.iter().map(|record| &record.definition),
            );
            let Some(source_definition) = records
                .iter()
                .find(|record| record.definition.agent_id == source_agent_id)
                .map(|record| &record.definition)
            else {
                return Ok(json!({
                    "status": "error",
                    "reason": "source agent is unavailable in this scope",
                }));
            };
            if disabled.contains(&source_agent_id) {
                return Ok(json!({
                    "status": "error",
                    "reason": "source agent is unavailable in this scope",
                }));
            }
            let mut reachable =
                crate::magician_v2::agents::resolve_effective_delegation_target_ids_for_surface(
                    source_definition,
                    records.iter().map(|record| &record.definition),
                    &disabled,
                    crate::magician_v2::agents::InvocationSurface::Delegation,
                )
                .into_iter()
                .collect::<HashSet<_>>();
            reachable.extend(
                crate::magician_v2::agents::resolve_effective_delegation_target_ids_for_surface(
                    source_definition,
                    records.iter().map(|record| &record.definition),
                    &disabled,
                    crate::magician_v2::agents::InvocationSurface::Handover,
                ),
            );
            reachable.insert(source_agent_id.clone());
            let registered_pack_names = resources.tool_index().map(|index| index.pack_names());
            let agents: Vec<Value> = records
                .into_iter()
                .filter(|record| {
                    !disabled.contains(&record.definition.agent_id)
                        && reachable.contains(&record.definition.agent_id)
                        && (record.definition.agent_id == source_agent_id
                            || (!record.definition.is_system_agent()
                                && record.definition.invocation_policy.is_ambient()))
                })
                .map(|record| {
                    let definition = record.definition;
                    let effective_tools =
                        crate::magician_v2::chat::service::resolved_chat_tools_from_definition(
                            &definition,
                            registered_pack_names.as_deref(),
                        );
                    json!({
                        "agent_id": definition.agent_id,
                        "name": definition.name,
                        "description": definition.description,
                        "kind": format!("{:?}", definition.kind),
                        "disabled": definition.disabled,
                        "is_primary": definition.is_primary,
                        // Never expose the raw YAML allowlist as if it were the
                        // callable surface: trust universals and whole-tool
                        // denies are part of the effective definition policy.
                        "tools": effective_tools,
                        "tool_policy_basis": "resolved_scope_registry",
                    })
                })
                .collect();
            Ok(json!({
                "status": "ok",
                "count": agents.len(),
                "agents": agents,
            }))
        },
        Err(error) => Ok(json!({
            "status": "error",
            "reason": format!("list_agents failed: {error}"),
        })),
    }
}

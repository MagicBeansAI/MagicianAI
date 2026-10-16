//! `find_agents_for_capability` — the inverse of `get_agent_details`.
//!
//! Given a capability (a skill/pack name like `comic-strip`, or a canonical
//! `<pack>__<action>` leaf), returns every agent in the caller's scope that
//! owns it. This lets an orchestrating LLM route each part of a request to the
//! agent that actually owns the needed capability — instead of guessing, or
//! dumping a whole multi-capability ask onto one agent that can only do part of
//! it. Lean by design: returns `agent_id` + `name` + `description` + how the
//! capability matched, mirroring `get_agent_details`'s leniency.
//!
//! Ownership is computed from each agent's `tools:` allowlist expanded into the
//! same canonical `<pack>__<action>` leaves `get_agent_details` produces, so a
//! match works whether the caller names the pack (`comic-strip`) or a specific
//! leaf. Scope-bound: the definition store is already principal/workspace
//! scoped, so callers only see agents they can reach.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

/// Hyphens, underscores, and spaces collapse so `web_research` and
/// `web-researcher` compare as the same family.
pub fn normalize_capability_token(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|ch| match ch {
            '_' | ' ' => '-',
            other => other,
        })
        .collect::<String>()
        .to_ascii_lowercase()
}

pub fn identity_owns_capability(
    needle: &str,
    agent_id: &str,
    name: &str,
    aliases: &[String],
) -> bool {
    let needle = normalize_capability_token(needle);
    if needle.is_empty() {
        return false;
    }
    let candidates = std::iter::once(agent_id)
        .chain(std::iter::once(name))
        .chain(aliases.iter().map(String::as_str));
    for candidate in candidates {
        let token = normalize_capability_token(candidate);
        if token.is_empty() {
            continue;
        }
        if token == needle {
            return true;
        }
        // `web_research` must find `web-researcher`. Do not reverse-contains:
        // query `vc researcher` must not hit Sleuth's alias `researcher`.
        if needle.len() >= 5 && token.contains(&needle) {
            return true;
        }
    }
    false
}

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let Some(capability) = args
        .get("capability")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(json!({
            "status": "error",
            "reason": "find_agents_for_capability requires a non-empty `capability` argument (a skill/pack name like `comic-strip`, or a `<pack>__<action>` leaf).",
        }));
    };
    let needle = capability.to_ascii_lowercase();
    let principal = require_scope_str(&args, "__principal", "find_agents_for_capability")?;
    let workspace = require_scope_str(&args, "__workspace", "find_agents_for_capability")?;
    let Some(source_agent_id) = args
        .get("__agent_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(json!({
            "status": "error",
            "reason": "find_agents_for_capability requires a runtime-bound source agent",
        }));
    };

    let scoped = resources
        .agent_definition_store
        .for_scope(&principal, &workspace);
    let records = match scoped.list_definitions().await {
        Ok(records) => records,
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("failed to list agents in scope: {error}"),
            }));
        },
    };

    let index = resources.tool_index();
    let registered_pack_names = index.as_ref().map(|index| index.pack_names());
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
    let disabled = crate::magician_v2::agents::disabled_agent_hierarchy(
        records.iter().map(|record| &record.definition),
    );
    if disabled.contains(source_agent_id) {
        return Ok(json!({
            "status": "error",
            "reason": "source agent is unavailable in this scope",
        }));
    }
    let mut reachable = crate::magician_v2::agents::resolve_effective_delegation_target_ids(
        source_definition,
        records.iter().map(|record| &record.definition),
        &disabled,
    )
    .into_iter()
    .collect::<HashSet<_>>();
    reachable.insert(source_agent_id.to_string());
    let mut owners: Vec<Value> = Vec::new();
    for record in &records {
        let definition = &record.definition;
        // System agents are never delegation targets — skip them so the
        // roster matches what `delegate_to_agent` can actually reach.
        if !reachable.contains(&definition.agent_id) {
            continue;
        }

        // Pack/leaf names first (comic-strip, content_search). Then agent
        // identity: `web_research` must find `web-researcher` / Sleuth /
        // `researcher`, matching get_agent_details alias leniency.
        let mut matched_via: Option<String> = None;
        let effective_tools =
            crate::magician_v2::chat::service::resolved_chat_tools_from_definition(
                definition,
                registered_pack_names.as_deref(),
            );
        for pack in &effective_tools {
            if pack.to_ascii_lowercase() == needle
                || normalize_capability_token(pack) == normalize_capability_token(&needle)
            {
                matched_via = Some(pack.clone());
                break;
            }
            let leaves = index
                .as_deref()
                .map(|idx| idx.leaf_names_for_pack(pack))
                .unwrap_or_default();
            if let Some(leaf) = leaves.iter().find(|leaf| {
                leaf.to_ascii_lowercase() == needle
                    || normalize_capability_token(leaf) == normalize_capability_token(&needle)
            }) {
                matched_via = Some(leaf.clone());
                break;
            }
        }
        if matched_via.is_none()
            && identity_owns_capability(
                &needle,
                &definition.agent_id,
                &definition.name,
                &definition.aliases,
            )
        {
            matched_via = Some(definition.agent_id.clone());
        }

        if let Some(via) = matched_via {
            owners.push(json!({
                "agent_id": definition.agent_id,
                "name": definition.name,
                "description": definition.description,
                "owns_via": via,
            }));
        }
    }

    Ok(json!({
        "status": "ok",
        "capability": capability,
        "owner_count": owners.len(),
        "owners": owners,
    }))
}

#[cfg(test)]
mod tests {
    use super::{identity_owns_capability, normalize_capability_token};

    #[test]
    fn web_research_matches_web_researcher_identity() {
        let aliases = vec![
            "sleuth".to_string(),
            "researcher".to_string(),
            "wr".to_string(),
        ];
        assert!(identity_owns_capability(
            "web_research",
            "web-researcher",
            "Sleuth",
            &aliases,
        ));
        assert!(identity_owns_capability(
            "web-researcher",
            "web-researcher",
            "Sleuth",
            &aliases,
        ));
        assert!(identity_owns_capability(
            "sleuth",
            "web-researcher",
            "Sleuth",
            &aliases,
        ));
        assert!(identity_owns_capability(
            "researcher",
            "web-researcher",
            "Sleuth",
            &aliases,
        ));
        assert!(!identity_owns_capability(
            "web",
            "web-researcher",
            "Sleuth",
            &aliases,
        ));
        assert!(
            !identity_owns_capability("vc researcher", "web-researcher", "Sleuth", &aliases,),
            "Sleuth alias researcher must not steal vc-researcher queries"
        );
        assert_eq!(normalize_capability_token("web_research"), "web-research");
    }
}

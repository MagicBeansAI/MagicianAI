//! `switch_personality` — first compiled tool migrated to the
//! handler-registry pattern (Phase 0.8c-4).
//!
//! Loads the named personality preset from the workspace skills layer
//! and writes its parsed fields to the agent-scope
//! `personality_profile` memory tier. The write is keyed by
//! `(principal, workspace, agent_id)` so the new personality persists
//! across every future run of the agent — chat or autonomous.
//!
//! `args.mode = "list"` (or `"?"`) returns the available presets
//! without writing anything.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::memory::V3MemoryTierRecord;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::{require_scope_str, scope_arg_str};

/// Compiled-handler entry point. Signature matches
/// `compiled_providers::CompiledHandler` so it can be registered via
/// the `compiled_handler!` macro in `default_compiled_handler_registry()`.
pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "switch_personality")?;
    let workspace = require_scope_str(&args, "__workspace", "switch_personality")?;
    let agent_id = scope_arg_str(&args, "__agent_id").ok_or_else(|| {
        ExecutionError::Step(
            "switch_personality handler missing `agent_id` (passed as `__agent_id` from autonomous, or `agent_id` from chat bridge injection)".to_string(),
        )
    })?;

    let mode = match args
        .get("mode")
        .and_then(Value::as_str)
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
    {
        Some(m) => m,
        None => {
            return Ok(json!({
                "status": "error",
                "reason": "switch_personality requires `mode`",
            }));
        },
    };

    let workspace_skills_dir = resources.scope_skills_root(&principal, &workspace);
    let extra_skills_dirs = crate::magician_v2::config_extras::extra_skills_dirs();
    let mut search: Vec<&Path> = vec![workspace_skills_dir.as_path()];
    for dir in &extra_skills_dirs {
        search.push(dir.as_path());
    }

    // list-mode short-circuit: enumerate available personality presets
    // without writing anything.
    if mode == "list" || mode == "?" {
        let mut available = crate::magician_v2::skills::list_personality_mode_names(&search);
        available.sort();
        return Ok(json!({
            "status": "ok",
            "action": "list",
            "available_modes": available,
        }));
    }

    // Mirror the ChatRuntimeTool's char validation — lower-case
    // letters / digits / hyphens / underscores only.
    if !mode
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Ok(json!({
            "status": "error",
            "reason": format!(
                "Invalid mode '{mode}'. Use letters, digits, hyphens, and underscores only."
            ),
        }));
    }

    let parsed = match crate::magician_v2::skills::lookup_personality_mode(&search, &mode) {
        Some(spec) => crate::magician_v2::skills::personality_spec_to_fields(&spec),
        None => {
            let mut available = crate::magician_v2::skills::list_personality_mode_names(&search);
            available.sort();
            return Ok(json!({
                "status": "error",
                "reason": format!("Personality preset '{mode}' not found."),
                "available_modes": available,
            }));
        },
    };

    let scoped = resources
        .agent_definition_store
        .for_scope(&principal, &workspace);
    let definition = match scoped.get_definition(&agent_id).await {
        Ok(Some(record)) => record.definition,
        Ok(None) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("Agent '{}' has no definition in this scope.", agent_id),
            }));
        },
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("Failed to load agent definition: {error}"),
            }));
        },
    };

    let mut fields = HashMap::new();
    for (key, value) in &parsed {
        fields.insert(key.clone(), Value::String(value.clone()));
    }
    let record = V3MemoryTierRecord {
        schema_version: "v3".to_string(),
        record_type: "memory_tier".to_string(),
        principal: Some(principal.clone()),
        workspace: Some(workspace.clone()),
        agent_id: Some(agent_id.clone()),
        tier_name: "personality_profile".to_string(),
        tier_scope: crate::magician_v2::agents::memory_tiers::TierScope::Agent,
        goal_id: None,
        last_updated: Utc::now(),
        fields,
    };

    let memory_service = resources
        .memory_resolver
        .resolve_for_scope(&principal, &workspace)
        .map_err(|e| {
            ExecutionError::Step(format!("switch_personality memory resolver failed: {e}"))
        })?;

    match memory_service
        .save_native_tier_by_name(
            &agent_id,
            "personality_profile",
            &definition.memory_tiers,
            None,
            &record,
        )
        .await
    {
        Ok(()) => {
            let active_mode = parsed
                .get("active_mode")
                .cloned()
                .unwrap_or_else(|| mode.clone());
            Ok(json!({
                "status": "ok",
                "mode": active_mode,
                "agent_id": agent_id,
                "source": "skill",
            }))
        },
        Err(error) => Ok(json!({
            "status": "error",
            "reason": format!("Failed to save personality profile: {error}"),
        })),
    }
}

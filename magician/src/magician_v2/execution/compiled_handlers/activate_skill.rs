//! `activate_skill` — universal: load a procedure playbook into the
//! agent's prompt and persist it as the active skill on an agent-scope
//! memory tier. Phase 0.8c-6.
//!
//! Single source of truth replaces the two pre-Phase-0.8c stores:
//! - Chat `ChatSession.active_procedure_skill` (per-session, persistent)
//! - Autonomous `AgenticContext.scratch.active_procedure_skill` (per-execution, in-memory)
//!
//! Both surfaces now write to the `active_procedure_skill` agent-scope
//! memory tier keyed by `(principal, workspace, agent_id)`. Subsequent
//! prompt builds (chat + autonomous) read the tier and re-resolve the
//! skill body at injection time, so a skill edit on disk takes effect
//! on the next turn without re-activation.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use serde_json::{json, Value};

use crate::magician_v2::agents::memory_tiers::TierScope;
use crate::magician_v2::artifact_v2::memory::V3MemoryTierRecord;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::skills::{
    active_procedure_skill_tier_def, read_active_procedure_skill_name_from_tier,
};

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "activate_skill")?;
    let workspace = require_scope_str(&args, "__workspace", "activate_skill")?;
    let agent_id = require_scope_str(&args, "__agent_id", "activate_skill")?;

    let Some(name) = args
        .get("name")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Ok(json!({
            "status": "error",
            "reason": "activate_skill requires a non-empty `name`",
        }));
    };

    // Skill search path: per-scope workspace skills + configured `paths` extras.
    let workspace_skills_dir = resources.scope_skills_root(&principal, &workspace);
    let mut skill_search: Vec<PathBuf> = vec![workspace_skills_dir.clone()];
    skill_search.extend(crate::magician_v2::config_extras::extra_skills_dirs());

    // Load the agent definition to get the procedure-skill allowlist.
    let scoped = resources
        .agent_definition_store
        .for_scope(&principal, &workspace);
    let agent_tools = match scoped.get_definition(&agent_id).await {
        Ok(Some(record)) => record.definition.tools,
        Ok(None) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("Agent '{agent_id}' has no definition in this scope."),
            }));
        },
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("Failed to load agent definition: {error}"),
            }));
        },
    };
    let allowed_catalog = crate::magician_v2::skills::agent_procedure_skill_catalog(
        &workspace_skills_dir,
        &agent_tools,
    );
    let skill_allowlist: Vec<String> = allowed_catalog
        .iter()
        .map(|(name, _)| name.clone())
        .collect();

    // Read the current active skill name from the memory tier so the
    // resolver can classify the outcome as first / replaced / idempotent.
    let memory_service = resources
        .memory_resolver
        .resolve_for_scope(&principal, &workspace)
        .map_err(|e| ExecutionError::Step(format!("activate_skill memory resolver failed: {e}")))?;
    let previous_name =
        read_active_procedure_skill_name_from_tier(&memory_service, &agent_id).await;

    let outcome = crate::magician_v2::skills::resolve_and_activate_procedure_skill(
        &name,
        &agent_id,
        &skill_allowlist,
        &workspace_skills_dir,
        previous_name.as_deref(),
    );

    let outcome = match outcome {
        Ok(o) => o,
        Err(error) => {
            return Ok(activate_skill_error_response(&error));
        },
    };

    let skill = outcome.skill().clone();
    let activation = match &outcome {
        crate::magician_v2::skills::ActivationOutcome::First { activation, .. } => activation,
        crate::magician_v2::skills::ActivationOutcome::Replaced { activation, .. } => activation,
        crate::magician_v2::skills::ActivationOutcome::Idempotent { activation, .. } => activation,
    };

    // Persist `{name, activated_at}` to the `active_procedure_skill`
    // agent-scope memory tier. Body is re-resolved at prompt-build
    // time so a skill edit on disk takes effect on the next turn
    // without re-activation.
    let mut fields = HashMap::new();
    fields.insert("name".to_string(), Value::String(skill.name.clone()));
    fields.insert(
        "activated_at".to_string(),
        Value::String(Utc::now().to_rfc3339()),
    );
    let tier_def = active_procedure_skill_tier_def();
    let record = V3MemoryTierRecord {
        schema_version: "v3".to_string(),
        record_type: "memory_tier".to_string(),
        principal: Some(principal.clone()),
        workspace: Some(workspace.clone()),
        agent_id: Some(agent_id.clone()),
        tier_name: "active_procedure_skill".to_string(),
        tier_scope: TierScope::Agent,
        goal_id: None,
        last_updated: Utc::now(),
        fields,
    };
    if let Err(error) = memory_service
        .save_native_tier(&agent_id, &tier_def, None, &record)
        .await
    {
        return Ok(json!({
            "status": "error",
            "reason": format!("Failed to persist active procedure skill: {error}"),
        }));
    }

    let (status_message, reasoning) = match &outcome {
        crate::magician_v2::skills::ActivationOutcome::First { .. } => (
            format!("Activated procedure skill '{}'.", skill.name),
            "first",
        ),
        crate::magician_v2::skills::ActivationOutcome::Replaced { previous, .. } => (
            format!(
                "Activated procedure skill '{}' (replaced previous: '{}').",
                skill.name, previous
            ),
            "replaced",
        ),
        crate::magician_v2::skills::ActivationOutcome::Idempotent { .. } => (
            format!(
                "Procedure skill '{}' was already active; idempotent no-op.",
                skill.name
            ),
            "idempotent",
        ),
    };

    let ephemeral_names: Vec<String> = activation
        .ephemeral_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    let path_additions: Vec<String> = activation
        .path_additions
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    Ok(json!({
        "status": "ok",
        "skill": skill.name,
        "active_procedure_skill": skill.name,
        "outcome": reasoning,
        "message": status_message,
        "steering_message": activation.steering_message,
        "ephemeral_tools": ephemeral_names,
        "path_additions": path_additions,
        "previous_active": previous_name,
        "note": "Playbook is now active. Its body will be re-injected into your prompt every turn until you call deactivate_skill (or activate_skill with a different name, which auto-replaces).",
    }))
}

fn activate_skill_error_response(error: &crate::magician_v2::skills::ActivateSkillError) -> Value {
    use crate::magician_v2::skills::ActivateSkillError;
    match error {
        ActivateSkillError::NotFound {
            name,
            available_procedures,
        } => json!({
            "status": "error",
            "reason": format!(
                "skill '{name}' not found in workspace skills layer or any configured `paths` extras"
            ),
            "available_procedures": available_procedures,
        }),
        ActivateSkillError::NotAllowlisted {
            resolved_name,
            agent_id,
            allowed_procedure_skills,
        } => json!({
            "status": "error",
            "reason": format!(
                "skill '{resolved_name}' is not in agent '{agent_id}'s tools allowlist; add it to the agent definition first"
            ),
            "agent_id": agent_id,
            "allowed_procedure_skills": allowed_procedure_skills,
        }),
        _ => json!({
            "status": "error",
            "reason": format!("{error}"),
        }),
    }
}

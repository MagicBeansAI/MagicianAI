//! `review_program_missions` — the CEO board-review read tool (P3 of
//! `docs/archive/plans/2026-07-10-ceo-decomposition-design.md`).
//!
//! Closes the decomposition loop: after missions land in officer programs and
//! the officers run their cycles, the CEO consolidates outcomes into a board
//! review. This tool gathers, per program with a managed `## Missions (CEO)`
//! section: the missions body, the bound officers (agents whose focus areas
//! bind the program), and each officer's program RUNTIME STATE (the harness
//! writes it every cycle under `programs/state/` — `last_run_summary`,
//! `open_loops`, `blocked`, `next_action_hints`).
//!
//! READ-ONLY by design: no approval gate, no writes, no triggers. The CEO's
//! judgement (what to escalate, what to re-decompose, what to celebrate)
//! stays in the agent run; the tool only assembles the evidence.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::agents::autonomous_goal::focus_area_goal_id;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::harness::program::ProgramLoader;
use crate::magician_v2::harness::program_doc::{list_program_docs, read_program_doc, ProgramDoc};

use super::shared::require_scope_str;

fn invalid(msg: impl Into<String>) -> ExecutionError {
    ExecutionError::Step(format!("review_program_missions: {}", msg.into()))
}

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "review_program_missions")?;
    let workspace = require_scope_str(&args, "__workspace", "review_program_missions")?;

    let program_filter = args
        .get("program")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty());

    review(
        &resources.artifact_workspace,
        &resources.agent_definition_store,
        &principal,
        &workspace,
        program_filter,
    )
    .await
}

async fn review(
    ws: &crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
    definitions: &crate::magician_v2::agents::AgentDefinitionStore,
    principal: &str,
    workspace: &str,
    program_filter: Option<&str>,
) -> Result<Value, ExecutionError> {
    // programs to review: one when asked, else every program that currently
    // carries a managed missions section
    let mut docs: Vec<ProgramDoc> = Vec::new();
    match program_filter {
        Some(name) => {
            let doc = read_program_doc(ws, &principal, &workspace, name)
                .await
                .map_err(|err| invalid(format!("invalid program: {err}")))?
                .ok_or_else(|| invalid(format!("program {name:?} does not exist")))?;
            docs.push(doc);
        },
        None => {
            let listed = list_program_docs(ws, &principal, &workspace)
                .await
                .map_err(|err| invalid(format!("failed to list programs: {err}")))?;
            for summary in listed {
                if let Ok(Some(doc)) =
                    read_program_doc(ws, &principal, &workspace, &summary.name).await
                {
                    if doc.missions_section.is_some() {
                        docs.push(doc);
                    }
                }
            }
        },
    }

    let definitions = definitions
        .for_scope(principal, workspace)
        .list_persisted_definitions()
        .await
        .map_err(|err| invalid(format!("failed to list agent definitions: {err}")))?;
    let loader = ProgramLoader::new(ws.clone());

    let mut reports: Vec<Value> = Vec::new();
    for doc in &docs {
        let mut officer_reports: Vec<Value> = Vec::new();
        for record in &definitions {
            let definition = &record.definition;
            let Some(config) = definition.autonomous_config.as_ref() else {
                continue;
            };
            for focus_area in config
                .focus_areas
                .iter()
                .filter(|fa| fa.program.as_deref() == Some(doc.name.as_str()))
            {
                let goal_id = focus_area_goal_id(&definition.agent_id, focus_area);
                let state = match loader
                    .load_for_focus_area(&principal, &workspace, definition, Some(focus_area))
                    .await
                {
                    Ok(Some(loaded)) => loader
                        .load_runtime_state(&principal, &workspace, &loaded, Some(&goal_id))
                        .await
                        .ok()
                        .flatten(),
                    _ => None,
                };
                officer_reports.push(json!({
                    "agent_id": definition.agent_id,
                    "focus_area": focus_area.name,
                    "goal_id": goal_id,
                    "state": state.map(|s| json!({
                        "current_phase": s.current_phase,
                        "current_step": s.current_step,
                        "last_run_summary": s.last_run_summary,
                        "open_loops": s.open_loops,
                        "blocked": s.blocked,
                        "next_action_hints": s.next_action_hints,
                        "updated_at": s.updated_at.to_rfc3339(),
                        "updated_by": s.updated_by,
                    })),
                }));
            }
        }
        reports.push(json!({
            "program": doc.name,
            "title": doc.title,
            "missions_section": doc.missions_section,
            "officers": officer_reports,
        }));
    }

    Ok(json!({
        "programs": reports,
        "note": if reports.is_empty() {
            "no programs carry a managed `## Missions (CEO)` section yet — nothing to review"
        } else {
            "officer `state` is the persisted runtime state for this scoped program and goal; null means no state was found, not proof that the officer never ran"
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::{
        agents::{AgentDefinition, AgentDefinitionStore},
        artifact_v2::workspace::ArtifactV2Workspace,
    };

    #[tokio::test]
    async fn mission_reviews_use_workspace_bindings_not_another_workspaces_definition() {
        let dir = tempfile::tempdir().unwrap();
        let ws = ArtifactV2Workspace::new(dir.path());
        let store = AgentDefinitionStore::with_workspace_layout(ws.clone());
        for (workspace, focus) in [
            ("workspace-a", "current focus"),
            ("workspace-b", "old review"),
        ] {
            let definition = AgentDefinition::from_yaml_str(&format!(
                r#"
agent_id: analyst
name: Analyst
persona: Analyst
kind: personal
principal: owner
workspace: {workspace}
harness: {{}}
autonomous_config:
  schedule: "0 7 * * *"
  focus_areas:
    - name: {focus}
      priority: medium
      description: Review work
      program: plan.md
"#
            ))
            .unwrap();
            store
                .for_scope("owner", workspace)
                .create_definition(definition)
                .await
                .unwrap();
            let root = ws.program_specs_root("owner", workspace);
            tokio::fs::create_dir_all(&root).await.unwrap();
            tokio::fs::write(
                root.join("plan.md"),
                "# Plan\n\n## Missions (CEO)\nReview\n",
            )
            .await
            .unwrap();
        }
        let report = review(&ws, &store, "owner", "workspace-a", Some("plan.md"))
            .await
            .unwrap();
        let officers = report["programs"][0]["officers"].as_array().unwrap();
        assert_eq!(officers.len(), 1);
        assert_eq!(officers[0]["focus_area"], "current focus");
        assert!(!report.to_string().contains("old review"));
        assert!(officers[0]["state"].is_null());
        assert!(!report["note"].as_str().unwrap().contains("has not run"));
    }
}

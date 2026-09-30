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

    let ws = &resources.artifact_workspace;

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

    let definitions = resources
        .agent_definition_store
        .list_definitions()
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
            "officer `state` is each program's runtime state (written every harness cycle); null = the officer has not run since the missions landed"
        },
    }))
}

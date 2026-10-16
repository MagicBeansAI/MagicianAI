//! `propose_program_missions` — the CEO decomposition tool (P1 of
//! `docs/archive/plans/2026-07-10-ceo-decomposition-design.md`).
//!
//! Turns an approved mission breakdown into the target program's managed
//! `## Missions (CEO)` section via `harness::program_doc::ProgramDocEditor`
//! (replace-or-append only; provenance-stamped; history snapshot before every
//! write; existing programs only). The tool is granted ONLY to the CEO and is
//! gated `requires_approval` in its definition, so every proposal pauses as an
//! approval — in the Fleet Civilization game that surfaces as the CEO walking
//! to the plaza with a [!].
//!
//! Guards live in the TOOL CONTRACT, not the prompt: 1..=5 missions, required
//! fields, existing program, per-program cooldown (one applied decomposition
//! per hour), and the editor's own 4KB section cap.
//!
//! P1.5: on apply, bound officers are MOBILIZED IMMEDIATELY — one goal-cycle
//! trigger per officer (first matching focus area), best-effort via the
//! injected `AgentResources::agent_runtime`. When the runtime handle is
//! absent (minimal boots), officers still consume the missions on their next
//! autonomous cycle; the response says which behaviour applied.

use std::sync::Arc;

use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::fs;

use crate::magician_v2::agents::autonomous_goal::focus_area_goal_id;
use crate::magician_v2::agents::types::GoalSource;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::harness::program_doc::{read_program_doc, ProgramDocEditor};

use super::shared::require_scope_str;

/// One applied decomposition per program per this many seconds.
const PROGRAM_COOLDOWN_SECS: i64 = 3600;
const MAX_MISSIONS: usize = 5;

#[derive(Debug, Clone, Deserialize)]
struct MissionArg {
    title: String,
    objective: String,
    success_criteria: String,
    #[serde(default)]
    priority: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProposeArgs {
    program: String,
    missions: Vec<MissionArg>,
    rationale: String,
}

fn invalid(msg: impl Into<String>) -> ExecutionError {
    ExecutionError::Step(format!("propose_program_missions: {}", msg.into()))
}

/// Contract validation — pure, unit-tested below.
fn validate(args: &ProposeArgs) -> Result<(), String> {
    if args.missions.is_empty() || args.missions.len() > MAX_MISSIONS {
        return Err(format!(
            "missions must contain 1..={MAX_MISSIONS} entries (got {})",
            args.missions.len()
        ));
    }
    if args.rationale.trim().is_empty() {
        return Err("rationale is required".to_string());
    }
    for (i, m) in args.missions.iter().enumerate() {
        if m.title.trim().is_empty()
            || m.objective.trim().is_empty()
            || m.success_criteria.trim().is_empty()
        {
            return Err(format!(
                "mission {} is missing title/objective/success_criteria",
                i + 1
            ));
        }
    }
    Ok(())
}

/// Render the managed-section body — pure, unit-tested below.
fn render_missions(args: &ProposeArgs) -> String {
    let mut out = String::new();
    out.push_str(&format!("_Rationale: {}_\n", args.rationale.trim()));
    for m in &args.missions {
        let priority = m
            .priority
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .unwrap_or("medium");
        out.push_str(&format!(
            "\n- **{}** ({priority})\n  - Objective: {}\n  - Success: {}\n",
            m.title.trim(),
            m.objective.trim(),
            m.success_criteria.trim()
        ));
    }
    out
}

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "propose_program_missions")?;
    let workspace = require_scope_str(&args, "__workspace", "propose_program_missions")?;
    let agent_id = require_scope_str(&args, "__agent_id", "propose_program_missions")?;

    let parsed: ProposeArgs = serde_json::from_value(args.clone())
        .map_err(|err| invalid(format!("bad arguments: {err}")))?;
    validate(&parsed).map_err(invalid)?;

    let ws = &resources.artifact_workspace;

    // the target must be a REAL program in this scope (never creates files)
    let doc = read_program_doc(ws, &principal, &workspace, &parsed.program)
        .await
        .map_err(|err| invalid(format!("invalid program: {err}")))?;
    if doc.is_none() {
        return Err(invalid(format!(
            "program {:?} does not exist in this scope's programs/",
            parsed.program
        )));
    }

    // cooldown: newest history snapshot for this program within the window
    let history_dir = ws.programs_root(&principal, &workspace).join(".history");
    let stem = parsed.program.trim_end_matches(".md").to_string();
    if let Ok(mut entries) = fs::read_dir(&history_dir).await {
        let now = Utc::now();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with(&format!("{stem}-")) {
                continue;
            }
            if let Ok(meta) = entry.metadata().await {
                if let Ok(modified) = meta.modified() {
                    let modified: chrono::DateTime<Utc> = modified.into();
                    if (now - modified).num_seconds() < PROGRAM_COOLDOWN_SECS {
                        return Err(invalid(format!(
                            "program {:?} was updated less than an hour ago — cooldown active",
                            parsed.program
                        )));
                    }
                }
            }
        }
    }

    let body = render_missions(&parsed);
    let editor = ProgramDocEditor::new(ws.clone());
    let applied = editor
        .apply_managed_section(
            &principal,
            &workspace,
            &parsed.program,
            &body,
            &format!("agent {agent_id}"),
        )
        .await
        .map_err(|err| {
            ExecutionError::Step(format!("propose_program_missions apply failed: {err}"))
        })?;

    // bound officers: agents whose focus areas bind this program. P1.5: with
    // the runtime handle available, each officer is MOBILIZED NOW (the human
    // gate already happened at the approval) — one trigger per officer, on
    // its first matching focus area. Best-effort: a trigger that can't be
    // admitted still leaves the missions in the program for the next cycle.
    let mut officers: Vec<String> = Vec::new();
    // `triggered` = officers that STARTED NOW this cycle (receipt.task_id is
    // Some — a StartNow admission). `deferred` = officers whose trigger was
    // Duplicate/Queued/QueueFull (receipt.task_id is None): a cycle was already
    // in flight or the per-scope trigger queue was full, so they consume the
    // missions on their next autonomous cycle rather than starting immediately.
    let mut triggered: Vec<Value> = Vec::new();
    let mut deferred: Vec<Value> = Vec::new();
    if let Ok(records) = resources.agent_definition_store.list_definitions().await {
        for record in records {
            let definition = record.definition;
            if definition.agent_id == agent_id {
                continue;
            }
            let Some(config) = definition.autonomous_config.as_ref() else {
                continue;
            };
            let Some(focus_area) = config
                .focus_areas
                .iter()
                .find(|fa| fa.program.as_deref() == Some(parsed.program.as_str()))
            else {
                continue;
            };
            officers.push(definition.agent_id.clone());
            if let Some(runtime) = resources.agent_runtime.as_ref() {
                let goal_id = focus_area_goal_id(&definition.agent_id, focus_area);
                let receipt = runtime
                    .trigger_goal_awaitable_in_scope(
                        &principal,
                        &workspace,
                        &definition.agent_id,
                        &goal_id,
                        GoalSource::User,
                    )
                    .await;
                // Only a StartNow admission yields Some(task_id); Duplicate/
                // Queued/QueueFull all return task_id: None.
                if receipt.task_id.is_some() {
                    triggered.push(json!({
                        "agent_id": definition.agent_id,
                        "goal_id": goal_id,
                        "cycle_id": receipt.cycle_id,
                        "task_id": receipt.task_id,
                    }));
                } else {
                    deferred.push(json!({
                        "agent_id": definition.agent_id,
                        "goal_id": goal_id,
                        "cycle_id": receipt.cycle_id,
                        "status": "queued_or_deferred",
                    }));
                }
            }
        }
    }
    officers.sort();
    officers.dedup();

    // "Mobilized now" is true only when at least one officer actually STARTED
    // this cycle — a batch that was entirely Duplicate/Queued/QueueFull must not
    // claim immediate mobilization.
    let mobilized_now = !triggered.is_empty();
    let note = match (mobilized_now, deferred.is_empty()) {
        (true, true) => "missions applied and all bound officers mobilized immediately (goal cycles started now)".to_string(),
        (true, false) => format!(
            "missions applied; {} officer(s) mobilized immediately and {} already had a cycle in flight or the trigger queue was full — they consume the missions on their next autonomous cycle",
            triggered.len(),
            deferred.len()
        ),
        (false, false) => "missions applied; every bound officer already had a cycle in flight or the trigger queue was full — they consume the missions on their next autonomous cycle".to_string(),
        (false, true) => "missions landed in the program's managed section; bound officers consume them on their next autonomous cycle".to_string(),
    };
    Ok(json!({
        "applied": true,
        "program": applied.program,
        "history_snapshot": applied.history_snapshot,
        "missions": parsed.missions.len(),
        "bound_officers": officers,
        "mobilized_now": mobilized_now,
        "triggered": triggered,
        "deferred": deferred,
        "note": note,
    }))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn args(missions: usize) -> ProposeArgs {
        ProposeArgs {
            program: "product_strategy.md".into(),
            rationale: "focus the quarter".into(),
            missions: (0..missions)
                .map(|i| MissionArg {
                    title: format!("M{i}"),
                    objective: "do the thing".into(),
                    success_criteria: "thing is done".into(),
                    priority: if i == 0 { Some("high".into()) } else { None },
                })
                .collect(),
        }
    }

    #[test]
    fn validates_counts_and_required_fields() {
        assert!(validate(&args(1)).is_ok());
        assert!(validate(&args(5)).is_ok());
        assert!(validate(&args(0)).is_err());
        assert!(validate(&args(6)).is_err());

        let mut bad = args(2);
        bad.missions[1].objective = "  ".into();
        assert!(validate(&bad).is_err());

        let mut no_rationale = args(1);
        no_rationale.rationale = String::new();
        assert!(validate(&no_rationale).is_err());
    }

    #[test]
    fn renders_missions_with_priorities_and_rationale() {
        let body = render_missions(&args(2));
        assert!(body.contains("_Rationale: focus the quarter_"));
        assert!(body.contains("**M0** (high)"));
        assert!(body.contains("**M1** (medium)"));
        assert!(body.contains("Objective: do the thing"));
        assert!(body.contains("Success: thing is done"));
    }
}

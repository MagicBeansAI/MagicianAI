//! Read-only enforcement for plan runs (a VibeDev Discuss run).
//!
//! A plan run (`AgenticContext::plan_run`) changes nothing: its agents read,
//! plan, and hand off. This module names what such a run **may** call; every
//! other tool is refused, so a writer added later is denied without anyone
//! having to remember this list (allowlist semantics, fail closed).
//!
//! One classification serves every path that can reach a tool:
//! - the native catalog (`native_integration::build_catalog_context`) and the
//!   plane's `PlaneGrant::permits` ask [`admits_tool`] by name;
//! - the native dispatch gate (`validate_action_against_allowlists`) and the
//!   plane's `tools/call` ask [`action_refusal`] of the lowered action, which
//!   also checks the arguments of a tool that is read-only for some calls
//!   only (`files` / `delegation_files` with `action: read|exists|list`).
//!
//! `run_coding_task` is admitted because a plan run forces it into plan mode:
//! the compiled-pack dispatch stamps [`PLAN_RUN_PARAM`] from the run's flag,
//! and the handler then runs `plan_only`, whatever the model passed.
//!
//! Shell (`shell`, `delegation_shell`, `dugite`, `http`, `duckdb`) is refused:
//! there is no read-only shell sandbox to run it in, and a command line cannot
//! be classified as read-only by inspection.

use crate::magician_v2::execution::actions::{ExecutableAction, FileAction};
use serde_json::Value;

/// Runtime parameter the compiled-pack dispatch stamps for a plan run. Like
/// every `__*` key it is stripped from model arguments first, so a model can
/// neither forge nor drop it.
pub const PLAN_RUN_PARAM: &str = "__plan_run";

/// What a tool may do in a plan run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanRunEffect {
    /// Run control or reporting: ends, pauses, delegates, loads tools, or
    /// tells the owner something. Changes no project state itself.
    Control,
    /// Only reads.
    ReadOnly,
    /// Read-only only for these values of its `action` argument.
    ReadOnlyActions(&'static [&'static str]),
    /// Would write, but runs in plan mode in a plan run (`run_coding_task`).
    ForcedPlanMode,
}

const FILE_READ_ACTIONS: &[&str] = &["read", "exists", "list"];

/// Run controls and owner reporting.
const CONTROL_TOOLS: &[&str] = &[
    "yield",
    "need_user_input",
    "goal_reached",
    "cannot_proceed",
    "spawn_sub_goal",
    "delegate_to_agent",
    "handover_to_agent",
    "tool_search",
    "submit_plan",
    "read_result",
    "notify_owner",
];

/// Tools that only read: files, search, web reads, memory and run listings.
const READ_ONLY_TOOLS: &[&str] = &[
    "read_file",
    "glob",
    "grep",
    "content_search",
    "content_read",
    "web_fetch",
    "web_search",
    "search_memory",
    "list_memory_tiers",
    "list_tasks",
    "list_agents",
    "inspect_agent",
    "get_agent_details",
    "find_agents_for_capability",
    "list_artifacts",
    "list_scheduled_tasks",
    "list_episodes",
    "list_proposals",
    "get_active_executions",
    "get_execution_history",
    "read_program_state",
    "read_trace",
    "system_status",
    "time_math",
];

/// The classification. `None` means the tool is not available in a plan run.
pub fn plan_run_effect(tool: &str) -> Option<PlanRunEffect> {
    let tool = tool.trim();
    if CONTROL_TOOLS.contains(&tool) {
        return Some(PlanRunEffect::Control);
    }
    if READ_ONLY_TOOLS.contains(&tool) {
        return Some(PlanRunEffect::ReadOnly);
    }
    match tool {
        "files" | "delegation_files" => Some(PlanRunEffect::ReadOnlyActions(FILE_READ_ACTIONS)),
        "run_coding_task" => Some(PlanRunEffect::ForcedPlanMode),
        _ => None,
    }
}

/// Whether `tool` may be offered to (and called by) a plan run at all. A
/// `<pack>__<action>` leaf is admitted when the leaf itself is classified, or
/// when its pack is read-only for that action.
pub fn admits_tool(tool: &str) -> bool {
    let tool = tool.trim();
    if plan_run_effect(tool).is_some() {
        return true;
    }
    match tool.split_once("__") {
        Some((pack, action)) if !pack.is_empty() && !action.is_empty() => {
            match plan_run_effect(pack) {
                Some(PlanRunEffect::ReadOnly) => true,
                Some(PlanRunEffect::ReadOnlyActions(allowed)) => allowed.contains(&action),
                _ => false,
            }
        },
        _ => false,
    }
}

fn refusal(tool: &str) -> String {
    format!(
        "`{tool}` is not available in a plan (Discuss) run: the run is read-only and changes \
         nothing. Read the project, call run_coding_task (it runs in plan mode here), or yield \
         your plan."
    )
}

/// Why this concrete call is refused in a plan run, or `None` when it may run.
pub fn action_refusal(action: &ExecutableAction) -> Option<String> {
    match action {
        ExecutableAction::Pack {
            capability_name,
            resolved_params,
            ..
        } => {
            let tool = capability_name.trim();
            match plan_run_effect(tool) {
                Some(PlanRunEffect::ReadOnlyActions(allowed)) => {
                    let requested = resolved_params
                        .get("action")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .unwrap_or_default();
                    (!allowed.contains(&requested)).then(|| {
                        format!(
                            "`{tool}` action `{requested}` is not available in a plan (Discuss) \
                             run: only {} are, because the run is read-only.",
                            allowed.join(", ")
                        )
                    })
                },
                Some(_) => None,
                None if admits_tool(tool) => None,
                None => Some(refusal(tool)),
            }
        },
        ExecutableAction::File(file) => match file {
            FileAction::Read { .. } | FileAction::Exists { .. } | FileAction::List { .. } => None,
            _ => Some(refusal("files")),
        },
        ExecutableAction::Bash(_) => Some(refusal("shell")),
        ExecutableAction::Http(_) => Some(refusal("http")),
        ExecutableAction::DuckDb(_) => Some(refusal("duckdb")),
        // Run control: delegated children and sub-goals inherit the plan flag,
        // and sleeping changes nothing.
        ExecutableAction::SpawnSubGoal { .. }
        | ExecutableAction::DelegateToAgent { .. }
        | ExecutableAction::HandoverToAgent { .. }
        | ExecutableAction::SleepUntil { .. } => None,
    }
}

/// Tools the native catalog offers outside an agent's capability list (the
/// structural controls). The catalog denies the ones a plan run does not
/// admit; capability-list tools are filtered by [`admits_tool`] directly.
pub const STRUCTURAL_CATALOG_TOOLS: &[&str] = &[
    "yield",
    "need_user_input",
    "spawn_sub_goal",
    "submit_plan",
    "read_result",
    "export_result",
    "delegate_to_agent",
    "handover_to_agent",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::capability::ImplementationType;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn pack(name: &str, params: &[(&str, Value)]) -> ExecutableAction {
        ExecutableAction::Pack {
            capability_name: name.to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: name.to_string(),
            },
            resolved_params: params
                .iter()
                .map(|(key, value)| (key.to_string(), value.clone()))
                .collect::<HashMap<_, _>>(),
        }
    }

    #[test]
    fn unknown_and_writing_tools_are_refused_by_default() {
        for tool in [
            "apply_code_proposal",
            "write_file",
            "edit_file",
            "shell",
            "delegation_shell",
            "dugite",
            "dugite__run",
            "http",
            "run_project_checks",
            "run_task",
            "export_result",
            "open_pr",
            "deploy_app",
            "a_tool_added_next_year",
        ] {
            assert!(!admits_tool(tool), "{tool} must not be admitted");
            assert!(action_refusal(&pack(tool, &[])).is_some(), "{tool}");
        }
    }

    #[test]
    fn control_reads_and_the_coding_engine_are_admitted() {
        for tool in [
            "yield",
            "need_user_input",
            "delegate_to_agent",
            "handover_to_agent",
            "tool_search",
            "notify_owner",
            "read_file",
            "grep",
            "glob",
            "web_search",
            "files",
            "delegation_files",
            "run_coding_task",
        ] {
            assert!(admits_tool(tool), "{tool} must be admitted");
        }
    }

    #[test]
    fn file_tools_are_read_only_per_call() {
        for action in ["read", "exists", "list"] {
            assert!(
                action_refusal(&pack("delegation_files", &[("action", json(action))])).is_none(),
                "{action}"
            );
            assert!(admits_tool(&format!("files__{action}")));
        }
        for action in ["write", "append", "delete", "copy", "move", "mkdir"] {
            assert!(
                action_refusal(&pack("delegation_files", &[("action", json(action))])).is_some(),
                "{action}"
            );
            assert!(action_refusal(&pack("files", &[("action", json(action))])).is_some());
            assert!(!admits_tool(&format!("files__{action}")));
        }
        assert!(
            action_refusal(&pack("files", &[])).is_some(),
            "a missing action is not a read"
        );
    }

    #[test]
    fn builtin_action_lanes_follow_the_same_rule() {
        let read = ExecutableAction::File(FileAction::Read {
            path: PathBuf::from("a.txt"),
            encoding: None,
        });
        assert!(action_refusal(&read).is_none());
        let write = ExecutableAction::File(FileAction::Write {
            path: PathBuf::from("a.txt"),
            content: String::new(),
            create_dirs: true,
        });
        assert!(action_refusal(&write).is_some());
        let handover = ExecutableAction::HandoverToAgent {
            target_agent_id: "engineer".to_string(),
            context: "plan it".to_string(),
        };
        assert!(action_refusal(&handover).is_none());
    }

    fn json(value: &str) -> Value {
        Value::String(value.to_string())
    }
}

//! The plane's own hot tier — Magician's control surface, not the loop's files.
//!
//! Task 3 of `docs/plans/2026-08-23-magician-plane-vertical-slice-plan.md`.
//! A foreign harness in a terminal already has Bash/Read/Write. Advertising
//! those as the hot list is a second-rate copy of its own toolbox. The plane
//! advertises runs, memory, agents, apps, monitors, skills — and loads leaves
//! through `tool_search` (Task 5).
//!
//! Two profiles, because one list cannot serve both consumers:
//!
//! - **spawned-bare** (`--tools ""`): the harness has no native tools, so the
//!   governed file/HTTP leaves stay hot. Raw shell is deliberately absent:
//!   arbitrary command execution would let the harness invoke `claude`,
//!   `codex`, or another credential-bearing harness and bypass the plane's
//!   non-negotiable anti-nesting floor.
//! - **terminal**: control surface only.
//!
//! Course-correction from the plan's "build the loop catalog then re-tier":
//! `build_flat_loop_tools` always injects `yield` / `need_user_input` /
//! `spawn_sub_goal` / `read_result`, and needs a populated `merged_agent_tools`
//! plus a live `ToolIndex` of the whole deferred universe. The plane's tests
//! inject a *candidate* index; a default `AgenticContext` has neither. Projecting
//! from the profile list + grant.permits + loaded set is the same filter the
//! re-tier would apply, without advertising a **wider** surface than the loop
//! (control verbs never enter; `NEVER_ON_THE_PLANE` is the grant floor).
//!
//! A conversation grant may also carry the native chat mouth's own tools
//! (`PlaneGrant::bridged_tools`): those are hot under the mouth's own schema
//! and dispatch through the mouth bridge, never through the index.

use std::collections::{BTreeSet, HashSet};

use magicllm::types::LLMToolSpec;
use serde_json::{json, Value};

use super::grant::{PlaneCatalogProfile, PlaneGrant};
use crate::config::PlaneConfig;
use crate::magician_v2::execution::flat_loop::{
    selected_tool_names_from_query, ToolIndex, ToolIndexEntry,
};

/// Lifecycle verbs the loop keeps hot. A foreign harness must never see them:
/// they are how Magician's *own* loop yields, not MCP tools.
pub const PLANE_CONTROL_VERBS: &[&str] =
    &["yield", "need_user_input", "spawn_sub_goal", "read_result"];

/// File/shell leaves the loop keeps hot. Terminal profile strips them.
pub const PLANE_SPAWNED_BARE_LEAVES: &[&str] = &[
    "read_file",
    "write_file",
    "edit_file",
    "grep",
    "glob",
    "http",
];

/// Magician's control surface — what makes connecting to the plane worth doing.
pub const PLANE_CONTROL_HOT: &[&str] = &[
    "create_task",
    "run_task",
    "list_tasks",
    "get_task_details",
    "task_state",
    "update_task",
    "refine_task",
    "stop_task",
    "get_active_executions",
    "get_execution_history",
    "search_memory",
    "list_episodes",
    "list_memory_tiers",
    "save_preference",
    "list_agents",
    "inspect_agent",
    "app_discover",
    "app_data_query",
    "app_action_compose",
    "app_action_invoke",
    "create_monitor",
    "update_monitor",
    "preview_monitor",
    "activate_skill",
    "switch_personality",
    "tool_search",
    "session_ledger",
    "request_user_input",
    "wait_for_run",
];

/// The delegation surface — advertised only to a grant whose agent actually has
/// somewhere to delegate.
///
/// `flat_loop::catalog` has always gated these on `has_delegation_targets`, so
/// Magician's own loop never offers them to a leaf worker. This list did not,
/// and it is what an MCP session sees, so a switched engine was offered both
/// regardless: a desktop run on `mac-operator` (`delegation_targets: []`) had
/// grok look up who owns `macos-ui-automation`, find `mac-operator` — itself —
/// delegate there three times for the dispatcher to refuse each one with
/// `InvalidDelegationTarget`, then abandon the plane and spend eleven minutes
/// producing nothing, with the tool it needed in its hand the whole time. The
/// two catalogs must agree about what exists.
pub const PLANE_DELEGATION_HOT: &[&str] = &[
    "delegate_to_agent",
    "get_agent_details",
    "find_agents_for_capability",
];

/// Built-in hot names for a profile, delegation included — the whole built-in
/// surface, for callers that have no grant to judge delegation by (the chat
/// mouth's own catalog, documentation, tests).
pub fn builtin_hot_names(profile: PlaneCatalogProfile) -> Vec<&'static str> {
    let mut names = builtin_hot_names_without_delegation(profile);
    names.extend_from_slice(PLANE_DELEGATION_HOT);
    names
}

/// The built-in surface minus the delegation tools.
pub fn builtin_hot_names_without_delegation(profile: PlaneCatalogProfile) -> Vec<&'static str> {
    match profile {
        PlaneCatalogProfile::Terminal => PLANE_CONTROL_HOT.to_vec(),
        PlaneCatalogProfile::SpawnedBare => {
            let mut names = PLANE_CONTROL_HOT.to_vec();
            names.extend_from_slice(PLANE_SPAWNED_BARE_LEAVES);
            names
        },
    }
}

/// Built-in hot names for one grant: the delegation tools only when its agent
/// has a target to delegate to, as `flat_loop::catalog` already decides.
fn builtin_hot_names_for_grant(grant: &PlaneGrant) -> Vec<&'static str> {
    let mut names = builtin_hot_names_without_delegation(grant.catalog_profile);
    if !grant.ctx.delegation_targets.is_empty() {
        names.extend_from_slice(PLANE_DELEGATION_HOT);
    }
    names
}

fn hot_names_for(grant: &PlaneGrant, config: Option<&PlaneConfig>) -> Vec<String> {
    let delegates = !grant.ctx.delegation_targets.is_empty();
    if let Some(config) = config {
        let overlay = match grant.catalog_profile {
            PlaneCatalogProfile::Terminal => &config.terminal.hot,
            PlaneCatalogProfile::SpawnedBare => &config.spawned_bare.hot,
        };
        if !overlay.is_empty() {
            // An operator's overlay chooses the surface, but a tool whose every
            // call must fail is not a choice: a delegation name stays out for an
            // agent with no target, exactly as the built-in list decides.
            return overlay
                .iter()
                .filter(|name| delegates || !PLANE_DELEGATION_HOT.contains(&name.as_str()))
                .cloned()
                .collect();
        }
    }
    builtin_hot_names_for_grant(grant)
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn is_control_verb(name: &str) -> bool {
    PLANE_CONTROL_VERBS.iter().any(|verb| *verb == name)
}

/// Names `tools/list` will advertise for this grant.
pub fn advertised_names(grant: &PlaneGrant) -> Vec<String> {
    advertised_names_configured(grant, None)
}

pub fn advertised_names_configured(
    grant: &PlaneGrant,
    config: Option<&PlaneConfig>,
) -> Vec<String> {
    plane_tools_list_configured(grant, config)
        .into_iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// MCP `tools/list` entries: name, description, inputSchema.
pub fn plane_tools_list(grant: &PlaneGrant) -> Vec<Value> {
    plane_tools_list_configured(grant, None)
}

/// MCP wire projection for a grant that can actually dispatch the advertised
/// tools. Terminal grants attach the boot-installed executors at the door;
/// minimal boots without them keep an honestly empty runnable catalog.
pub fn plane_runnable_tools_list_configured(
    grant: &PlaneGrant,
    config: Option<&PlaneConfig>,
) -> Vec<Value> {
    if grant.executors.is_none() {
        return Vec::new();
    }
    plane_tools_list_configured(grant, config)
}

pub fn plane_tools_list_configured(grant: &PlaneGrant, config: Option<&PlaneConfig>) -> Vec<Value> {
    let mut names: BTreeSet<String> = hot_names_for(grant, config).into_iter().collect();
    names.extend(
        grant
            .loaded_tool_names()
            .into_iter()
            .filter(|name| grant.tool_index.get(name).is_some()),
    );
    // A bridged name is the native mouth's own tool on this grant: it is
    // advertised once, below, with the schema the mouth advertises, never
    // through the plane's index entry for a hot name it happens to share.
    let mut tools: Vec<Value> = names
        .into_iter()
        .filter(|name| {
            grant.permits(name) && !is_control_verb(name) && !grant.bridged_tools.contains_key(name)
        })
        .filter(|name| {
            !matches!(name.as_str(), "request_user_input" | "wait_for_run")
                || (grant.elicitation_enabled && !grant.live_harness_turn)
        })
        .map(|name| mcp_tool_entry(&name, grant.tool_index.as_ref()))
        .collect();
    // Bridged tools are hot: subject to the grant's allowlist only, since
    // they never go through the index, and outside the control-verb and
    // elicitation filters, which are about the plane's own verbs.
    tools.extend(
        grant
            .bridged_tools
            .iter()
            .filter(|(name, _)| grant.permits(name))
            .map(|(_, spec)| bridged_tool_entry(spec)),
    );
    tools
}

/// Dispatch authorization is the exact projected catalog, not merely the
/// grant's coarse allowlist. A client may call a hot tool or a tool loaded by
/// this grant's `tool_search`; knowing the name of a hidden/deferred tool is
/// not authority to invoke it.
pub fn is_callable_configured(
    grant: &PlaneGrant,
    config: Option<&PlaneConfig>,
    name: &str,
) -> bool {
    plane_tools_list_configured(grant, config)
        .into_iter()
        .any(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
}

fn mcp_tool_entry(name: &str, index: &ToolIndex) -> Value {
    // These handlers belong to the plane. The compiled tool_search has a
    // different contract (including max_results and no whole-pack loading).
    if let Some(entry) = index.get(name).filter(|_| {
        !matches!(
            name,
            "tool_search"
                | "session_ledger"
                | "request_user_input"
                | "wait_for_run"
                | "delegate_to_agent"
        )
    }) {
        return json!({
            "name": entry.name,
            "description": entry.description,
            "inputSchema": entry.parameters_schema,
        });
    }
    json!({
        "name": name,
        "description": control_tool_description(name),
        "inputSchema": control_tool_schema(name),
    })
}

/// The MCP entry for a bridged tool: the native mouth's own name,
/// description, and parameter schema, so the swapped mouth sees exactly the
/// definition the native mouth's dispatcher validates against.
fn bridged_tool_entry(spec: &LLMToolSpec) -> Value {
    json!({
        "name": spec.name,
        "description": spec.description,
        "inputSchema": spec.parameters,
    })
}

fn control_tool_description(name: &str) -> &'static str {
    match name {
        "tool_search" => {
            "Search permitted Magician tools by keyword, or load tools by name (`select:tool`). A successful select replaces this grant's loaded tools with the matching packs; the control tools stay available. Keyword search does not load tools."
        }
        "session_ledger" => "Read this MCP session's recent tool-call outcomes. Arguments and tool results are not retained in the ledger.",
        "request_user_input" => "Ask the user of this MCP session for text, a choice, multiple selections, or a form. Returns their typed answer; submitting input does not authorize another action. Credentials and rich reviews require Magician's UI.",
        "wait_for_run" => "Wait for an execution launched by this MCP session, answering its human-input questions here. Call again after a waiting result. Cancelling the wait leaves the run in its current state.",
        "delegate_to_agent" => "Hand one or more units of work to other Magician agents as child executions of this run. This turn ends when you call it; Magician runs the children, and your next turn resumes this conversation with their deliverables. Use list_agents or find_agents_for_capability to pick target_agent_id, and put the full brief in context.",
        "create_task" => "Create a Magician task.",
        "run_task" => "Start a Magician task execution.",
        "list_tasks" => "List Magician tasks in this grant's scope.",
        "get_task_details" => "Read one Magician task.",
        "task_state" => "Read or patch durable task state.",
        "update_task" => "Update a Magician task.",
        "refine_task" => "Refine a Magician task's plan.",
        "stop_task" => "Stop a running Magician task.",
        "get_active_executions" => "List executions currently running in this scope.",
        "get_execution_history" => "Read the execution record for a run.",
        "search_memory" => "Search Magician memory.",
        "list_episodes" => "List memory episodes.",
        "list_memory_tiers" => "List memory tiers.",
        "save_preference" => "Save an owner preference.",
        "list_agents" => "List agents in this scope.",
        "get_agent_details" => "Read one agent definition.",
        "find_agents_for_capability" => "Find agents that grant a capability.",
        "inspect_agent" => "Inspect a live agent.",
        "app_discover" => "Discover installed Magician apps.",
        "app_data_query" => "Query an app's data.",
        "app_action_compose" => "Compose an app action.",
        "app_action_invoke" => "Invoke an app action.",
        "create_monitor" => "Create a recurring monitor.",
        "update_monitor" => "Update a recurring monitor.",
        "preview_monitor" => "Preview a monitor contract without creating it.",
        "activate_skill" => "Activate a procedure skill.",
        "switch_personality" => "Switch the active personality.",
        "read_file" => "Read a file in Magician's sandbox.",
        "write_file" => "Write a file in Magician's sandbox.",
        "edit_file" => "Edit a file in Magician's sandbox.",
        "grep" => "Search file contents.",
        "glob" => "Match file paths.",
        // The desktop relay sits on loopback (`127.0.0.1:3017/host/ax/…`) and
        // will drive this Mac for anyone who posts to it. Naming it keeps the
        // choice informed rather than accidental: an agent that wants the
        // desktop gets a compacted tree and a saved screenshot through the
        // skill, where the raw endpoint answers with the whole accessibility
        // tree (179 KB for one window, against ~3 KB) — one comparison run
        // took five times as long for the same answer by hand-rolling it.
        "http" => "Make an HTTP request. For desktop control prefer the \
                   `macos-ui-automation` skill over posting to the host relay on \
                   loopback: same driver, compacted tree, screenshot on disk.",
        _ => "A Magician plane tool.",
    }
}

fn control_tool_schema(name: &str) -> Value {
    if name == "request_user_input" {
        let option = json!({"type":"object","properties":{
            "id":{"type":"string"},"label":{"type":"string"},"description":{"type":"string"}
        },"required":["id","label"],"additionalProperties":false});
        return json!({"type":"object","properties":{
            "question":{"type":"string","minLength":1,"maxLength":16384},
            "input_type":{"type":"string","enum":["text","guidance","choice","multi_choice","confirmation","form"],"default":"text"},
            "options":{"type":"array","items":option,"maxItems":128},
            "allow_other":{"type":"boolean","default":false},
            "min_selections":{"type":"integer","minimum":0,"default":0},
            "max_selections":{"type":"integer","minimum":0,"default":0,"description":"Zero means no upper limit."},
            "questions":{"type":"array","minItems":1,"maxItems":32,"items":{
                "type":"object","properties":{
                    "id":{"type":"string"},"prompt":{"type":"string"},
                    "input_type":{"type":"string","enum":["text","choice","multi_choice"],"default":"text"},
                    "options":{"type":"array","items":option,"maxItems":128}
                },"required":["id","prompt"],"additionalProperties":false
            }}
        },"required":["question"],"additionalProperties":false});
    }
    if name == "wait_for_run" {
        return json!({"type":"object","properties":{
            "execution_id":{"type":"string","minLength":1},
            "timeout_secs":{"type":"integer","minimum":1,"maximum":300,"default":300}
        },"required":["execution_id"],"additionalProperties":false});
    }
    if name == "delegate_to_agent" {
        // Only what a harness can honour. The native request also carries
        // `required_capability` and `expected_artifacts`; a harness has no
        // roster of what an agent owns and no contract for artifact names,
        // and a thorough one filled both — "files" as a required capability
        // had the target refused for not owning a pack it discovers at run
        // time, and named artifacts had a child that yielded its values
        // inline re-run by the refinement gate. Neither is a harness's to say.
        return json!({"type":"object","properties":{
            "targets":{"type":"array","minItems":1,"maxItems":8,"items":{
                "type":"object","properties":{
                    "target_agent_id":{"type":"string","minLength":1,"description":"Canonical agent id of the agent that will do this unit of work."},
                    "context":{"type":"string","minLength":1,"description":"The complete brief for that agent: what to do, what to report back, and any values it needs."},
                    "input_data":{"type":"object","description":"Structured input the child receives verbatim."}
                },"required":["target_agent_id","context"],"additionalProperties":false}}
        },"required":["targets"],"additionalProperties":false});
    }
    if name == "tool_search" {
        return json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "select:Tool1,Tool2 or keyword search over the deferred catalog."
                }
            },
            "required": ["query"],
            "additionalProperties": false
        });
    }
    if name == "session_ledger" {
        return json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        });
    }
    json!({"type": "object"})
}

// `plane_tools_call` lives in `dispatch.rs` so `tools/call` shares
// `execute_action` with the loop. This module keeps `tool_search` and the
// catalog projection.

/// Grant-scoped `tool_search`.
///
/// Course-correction from the plan's `record_loaded_tools_from_action`
/// (`executor.rs`): that function is on the file a parallel agent owns, and
/// the grant already holds `ctx.scratch.loaded_tools` — an `Arc<Mutex<…>>`
/// scoped to this run. A load is therefore grant-local without a working-set
/// key and without touching the executor.
///
/// Whole-pack: selecting one leaf of a pack loads every leaf of that pack in
/// the index. Unload-on-switch: a successful select **replaces** the set —
/// except on a grant whose Deferred hands were preloaded at mint
/// (`preloaded_deferred`), where it merges: that mouth cannot re-list tools,
/// so it still believes every preloaded name is callable. A select that
/// matches nothing does not replace and does not signal change.
pub async fn plane_tool_search(index: &ToolIndex, grant: &PlaneGrant, arguments: &Value) -> Value {
    let query = arguments
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if query.is_empty() {
        return json!({
            "isError": true,
            "content": [{"type": "text", "text": "tool_search requires a `query` argument."}],
        });
    }

    if let Some(selected) = selected_tool_names_from_query(query) {
        let loaded = expand_selected_packs(index, grant, &selected);
        let changed = if loaded.is_empty() {
            false
        } else {
            let previous = grant.loaded_tool_names();
            let mut next: HashSet<String> = loaded.iter().cloned().collect();
            if grant.preloaded_deferred {
                next.extend(previous.iter().cloned());
            }
            let changed = previous != next;
            grant.replace_loaded_tools(next);
            changed
        };
        let matches: Vec<Value> = index
            .select(&if loaded.is_empty() {
                selected
            } else {
                loaded.clone()
            })
            .into_iter()
            .filter(|entry| grant.permits(&entry.name) && !is_control_verb(&entry.name))
            .map(entry_to_match)
            .collect();
        let text = serde_json::to_string_pretty(&json!({
            "status": "ok",
            "mode": "select",
            "query": query,
            "matches": matches,
        }))
        .unwrap_or_else(|_| "{\"status\":\"ok\"}".to_string());
        return json!({
            "isError": false,
            "content": [{"type": "text", "text": text}],
            "_meta": {"toolsListChanged": changed},
        });
    }

    // Filter before truncating: forbidden high-ranked hits must not hide a
    // lower-ranked permitted tool in a narrowly attenuated terminal grant.
    let hits = index.search(query, index.len());
    let matches: Vec<Value> = hits
        .into_iter()
        .filter(|entry| grant.permits(&entry.name) && !is_control_verb(&entry.name))
        .take(12)
        .map(entry_to_match)
        .collect();
    let text = serde_json::to_string_pretty(&json!({
        "status": "ok",
        "mode": "search",
        "query": query,
        "matches": matches,
    }))
    .unwrap_or_else(|_| "{\"status\":\"ok\"}".to_string());
    json!({
        "isError": false,
        "content": [{"type": "text", "text": text}],
        "_meta": {"toolsListChanged": false},
    })
}

fn expand_selected_packs(
    index: &ToolIndex,
    grant: &PlaneGrant,
    selected: &[String],
) -> Vec<String> {
    let mut packs = BTreeSet::new();
    let mut accepted = BTreeSet::new();
    for name in selected {
        let Some(entry) = index.get(name) else {
            // Not a leaf: a pack named by the name a harness knows it by
            // (`select:browser`). The index holds leaves, never a bare pack
            // entry, so this is the only way a pack-name select can load.
            if !index.leaf_names_for_pack(name).is_empty() {
                packs.insert(name.clone());
            }
            continue;
        };
        if !grant.permits(&entry.name) || is_control_verb(&entry.name) {
            continue;
        }
        packs.insert(entry.pack_name.clone());
        accepted.insert(entry.name.clone());
    }
    let mut loaded = BTreeSet::new();
    for pack in packs {
        for leaf in index.leaf_names_for_pack(&pack) {
            if grant.permits(&leaf) && !is_control_verb(&leaf) {
                loaded.insert(leaf);
            }
        }
    }
    loaded.extend(accepted);
    loaded.into_iter().collect()
}

fn entry_to_match(entry: &ToolIndexEntry) -> Value {
    let plane_owned = matches!(
        entry.name.as_str(),
        "tool_search"
            | "session_ledger"
            | "request_user_input"
            | "wait_for_run"
            | "delegate_to_agent"
    );
    json!({
        "name": entry.name,
        "description": if plane_owned {
            control_tool_description(&entry.name)
        } else {
            entry.description.as_str()
        },
        "parameters": if plane_owned {
            control_tool_schema(&entry.name)
        } else {
            entry.parameters_schema.clone()
        },
        "pack": entry.pack_name,
    })
}

#[cfg(test)]
pub(crate) fn test_index_with(names: &[&str]) -> ToolIndex {
    ToolIndex::from_entries(names.iter().copied().map(test_index_entry).collect())
}

#[cfg(test)]
fn test_index_entry(name: &str) -> ToolIndexEntry {
    let pack = name
        .split_once("__")
        .map(|(pack, _)| pack)
        .unwrap_or(name)
        .to_string();
    ToolIndexEntry {
        name: name.to_string(),
        description: format!("{name} plane fixture"),
        parameters_schema: json!({"type": "object"}),
        search_hint: None,
        pack_guide_excerpt: None,
        pack_name: pack,
        result_projection: None,
        group: "pack",
    }
}

#[cfg(test)]
pub(crate) fn test_grant_with_index(names: &[&str]) -> PlaneGrant {
    test_profile_grant_with_index(PlaneCatalogProfile::Terminal, names)
}

#[cfg(test)]
fn test_terminal_grant_with_index(names: &[&str]) -> PlaneGrant {
    test_profile_grant_with_index(PlaneCatalogProfile::Terminal, names)
}

#[cfg(test)]
fn test_spawned_bare_grant_with_index(names: &[&str]) -> PlaneGrant {
    test_profile_grant_with_index(PlaneCatalogProfile::SpawnedBare, names)
}

#[cfg(test)]
fn test_profile_grant_with_index(profile: PlaneCatalogProfile, names: &[&str]) -> PlaneGrant {
    let mut ctx = crate::magician_v2::execution::agentic::AgenticContext::default();
    ctx.execution_id = Some("exec-1".to_string());
    ctx.agent_id = Some("personal-assistant".to_string());
    let mut grant = PlaneGrant::for_test("exec-1");
    grant.ctx = ctx;
    grant.catalog_profile = profile;
    grant.tool_index = std::sync::Arc::new(test_index_with(names));
    grant
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plane_owned_schemas_take_precedence_over_compiled_definitions() {
        let index = crate::magician_v2::execution::flat_loop::build_tool_index(
            crate::magician_v2::execution::compiled_providers::embedded_compiled_pack_defs_ref(),
        );
        let search = mcp_tool_entry("tool_search", &index);
        assert_eq!(search["inputSchema"]["required"], json!(["query"]));
        assert!(search["inputSchema"]["properties"]
            .get("max_results")
            .is_none());
        assert!(search["description"]
            .as_str()
            .unwrap()
            .contains("Keyword search does not load"));
        let selected = entry_to_match(index.get("tool_search").unwrap());
        assert_eq!(selected["parameters"], search["inputSchema"]);
        assert_eq!(selected["description"], search["description"]);
        let ledger = mcp_tool_entry("session_ledger", &index);
        assert_eq!(ledger["inputSchema"]["properties"], json!({}));
        assert_eq!(ledger["inputSchema"]["additionalProperties"], false);
    }

    #[test]
    fn removed_deferred_tools_do_not_reappear_with_a_generic_schema() {
        let mut grant = test_grant_with_index(&["duckdb__query"]);
        grant.replace_loaded_tools(HashSet::from(["duckdb__query".into()]));
        assert!(advertised_names(&grant).contains(&"duckdb__query".to_string()));
        grant.tool_index = std::sync::Arc::new(test_index_with(&[]));
        assert!(!advertised_names(&grant).contains(&"duckdb__query".to_string()));
    }

    #[tokio::test]
    async fn search_filters_before_its_limit_and_never_loads_control_verbs() {
        let mut entries: Vec<_> = (0..13)
            .map(|n| test_index_entry(&format!("lookup_{n}")))
            .collect();
        let mut allowed = test_index_entry("z_allowed");
        allowed.description = "lookup".into();
        entries.push(allowed);
        entries.push(test_index_entry("yield"));
        entries.push(test_index_entry("run_coding_task"));
        let index = ToolIndex::from_entries(entries);
        let mut grant = test_grant_with_index(&[]);
        grant.allowed_tools = vec!["z_allowed".into(), "yield".into(), "run_coding_task".into()];
        let result = plane_tool_search(&index, &grant, &json!({"query": "lookup"})).await;
        let result: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(result["matches"].as_array().unwrap().len(), 1);
        assert_eq!(result["matches"][0]["name"], "z_allowed");
        for query in [
            "yield",
            "run_coding_task",
            "select:yield,run_coding_task,lookup_0",
        ] {
            let result = plane_tool_search(&index, &grant, &json!({"query": query})).await;
            let result: Value =
                serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(result["matches"], json!([]));
            assert!(grant.loaded_tool_names().is_empty());
        }
    }

    /// `flat_loop::catalog` gates the delegation tools on the agent having a
    /// target; this list did not, and it is what an MCP session sees. A worker
    /// with `delegation_targets: []` was offered `delegate_to_agent` and
    /// `find_agents_for_capability`, whose every call the dispatcher must
    /// refuse — and one switched engine spent a whole run discovering that.
    #[test]
    fn the_delegation_tools_are_advertised_only_to_an_agent_with_a_target() {
        let index = &[
            "create_task",
            "delegate_to_agent",
            "find_agents_for_capability",
            "get_agent_details",
            "macos-ui-automation__call",
        ];

        let worker = test_terminal_grant_with_index(index);
        assert!(worker.ctx.delegation_targets.is_empty());
        let names = advertised_names(&worker);
        for delegation in PLANE_DELEGATION_HOT {
            assert!(
                !names.contains(&delegation.to_string()),
                "a worker with no target was offered {delegation}: {names:?}"
            );
        }
        assert!(
            names.contains(&"create_task".to_string()),
            "the rest of the control surface is untouched: {names:?}"
        );

        let mut coordinator = test_terminal_grant_with_index(index);
        coordinator.ctx.delegation_targets = vec![
            crate::magician_v2::execution::agentic::delegation_dispatch::DelegationTarget {
                agent_id: "mac-operator".to_string(),
                name: "Bolt".to_string(),
                aliases: Vec::new(),
                description: "desktop".to_string(),
                tools: vec!["macos-ui-automation".to_string()],
                allowed_invocation_surfaces: Vec::new(),
            },
        ];
        let names = advertised_names(&coordinator);
        for delegation in PLANE_DELEGATION_HOT {
            assert!(
                names.contains(&delegation.to_string()),
                "an agent with a target keeps {delegation}: {names:?}"
            );
        }
    }

    /// An operator's yaml overlay chooses the surface, but a tool whose every
    /// call must fail is not a choice.
    #[test]
    fn an_overlay_cannot_hand_a_worker_the_delegation_tools() {
        let mut config = PlaneConfig::default();
        config.terminal.hot = vec![
            "create_task".to_string(),
            "delegate_to_agent".to_string(),
            "find_agents_for_capability".to_string(),
        ];
        let worker = test_terminal_grant_with_index(&[
            "create_task",
            "delegate_to_agent",
            "find_agents_for_capability",
        ]);
        let names = advertised_names_configured(&worker, Some(&config));
        assert!(names.contains(&"create_task".to_string()), "{names:?}");
        assert!(
            !names.contains(&"delegate_to_agent".to_string()),
            "{names:?}"
        );
        assert!(
            !names.contains(&"find_agents_for_capability".to_string()),
            "{names:?}"
        );
    }

    #[test]
    fn the_terminal_profiles_hot_tier_is_the_control_surface_not_the_loops_file_tier() {
        let names = advertised_names(&test_terminal_grant_with_index(&[
            "create_task",
            "run_task",
            "search_memory",
            "shell",
            "read_file",
        ]));
        for control in ["create_task", "run_task", "search_memory"] {
            assert!(
                names.contains(&control.to_string()),
                "control tool missing: {names:?}"
            );
        }
        for leaf in ["shell", "read_file"] {
            assert!(
                !names.contains(&leaf.to_string()),
                "leaf tool {leaf} advertised hot"
            );
        }
    }

    /// A run driven by a switched engine can hand work to another agent. The
    /// verb is plane-owned, so it is advertised on a live harness turn — where
    /// `wait_for_run` is not — and its schema asks for the targets and nothing
    /// else.
    #[test]
    fn a_live_harness_turn_can_delegate_to_an_agent() {
        let mut grant = test_terminal_grant_with_index(&["create_task"]);
        grant.live_harness_turn = true;
        // A run is offered the verb only when it has somewhere to send the
        // work; the gate is `delegation_targets`, so this run has one.
        grant.ctx.delegation_targets = vec![
            crate::magician_v2::execution::agentic::delegation_dispatch::DelegationTarget {
                agent_id: "researcher".to_string(),
                name: "Scout".to_string(),
                aliases: Vec::new(),
                description: "reads fixtures".to_string(),
                tools: Vec::new(),
                allowed_invocation_surfaces: Vec::new(),
            },
        ];
        let tools = plane_tools_list(&grant);
        let entry = tools
            .iter()
            .find(|tool| tool["name"] == json!("delegate_to_agent"))
            .expect("a switched engine's run can delegate to an agent");
        let required = entry["inputSchema"]["required"]
            .as_array()
            .expect("the delegation schema names what it requires");
        assert!(required.contains(&json!("targets")), "{entry}");
        // A harness is offered only what it can honour: no capability
        // ownership claims, no artifact-name contracts.
        let target = &entry["inputSchema"]["properties"]["targets"]["items"]["properties"];
        assert!(target.get("required_capability").is_none(), "{target}");
        assert!(target.get("expected_artifacts").is_none(), "{target}");
    }

    #[test]
    fn the_spawned_bare_profile_still_advertises_hands() {
        let names = advertised_names(&test_spawned_bare_grant_with_index(&[
            "shell",
            "read_file",
            "write_file",
        ]));
        for leaf in ["read_file", "write_file"] {
            assert!(
                names.contains(&leaf.to_string()),
                "a bare harness cannot fall back to native tools: {names:?}"
            );
        }
        assert!(
            !names.contains(&"shell".to_string()),
            "raw shell can nest a credential-bearing harness and bypass the plane floor"
        );
    }

    #[test]
    fn the_two_profiles_differ() {
        let terminal = advertised_names(&test_terminal_grant_with_index(&["shell", "read_file"]));
        let spawned =
            advertised_names(&test_spawned_bare_grant_with_index(&["shell", "read_file"]));
        assert_ne!(
            terminal, spawned,
            "a silent identical pair would ship the terminal list to a stripped harness"
        );
        assert!(spawned.len() > terminal.len());
    }

    #[test]
    fn the_hot_tier_is_useful_without_any_notification() {
        let names = advertised_names(&test_grant_with_index(&["create_task", "run_task"]));
        assert!(names.len() >= 2 && names.contains(&"tool_search".to_string()));
    }

    #[test]
    fn control_verbs_are_never_advertised() {
        let names = advertised_names(&test_grant_with_index(&["create_task"]));
        for verb in PLANE_CONTROL_VERBS {
            assert!(
                !names.contains(&verb.to_string()),
                "leaked control verb {verb}"
            );
        }
    }

    #[test]
    fn the_denied_families_are_never_advertised() {
        let names = advertised_names(&test_grant_with_index(&["create_task", "run_coding_task"]));
        assert!(
            !names.contains(&"run_coding_task".to_string()),
            "nested harness advertised"
        );
    }

    #[test]
    fn every_advertised_tool_is_a_complete_mcp_entry() {
        for tool in plane_tools_list(&test_grant_with_index(&["create_task"])) {
            assert!(tool["name"].as_str().is_some_and(|n| !n.is_empty()));
            assert!(tool["description"].as_str().is_some_and(|d| !d.is_empty()));
            assert!(tool["inputSchema"].is_object(), "{tool}");
        }
    }

    #[test]
    fn a_config_overlay_replaces_the_builtin_list() {
        let grant = test_terminal_grant_with_index(&["create_task"]);
        let config = PlaneConfig {
            spawned_bare: Default::default(),
            terminal: crate::config::PlaneProfileConfig {
                hot: vec!["search_memory".to_string()],
            },
        };
        let names = advertised_names_configured(&grant, Some(&config));
        assert_eq!(names, vec!["search_memory".to_string()]);
    }

    #[tokio::test]
    async fn tool_search_is_advertised_and_returns_a_schema() {
        let index = test_index_with(&["read_file", "duckdb__query"]);
        let grant = test_grant_with_index(&["read_file", "duckdb__query"]);

        let names: Vec<String> = plane_tools_list(&grant)
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();
        assert!(names.contains(&"tool_search".to_string()), "{names:?}");

        let result =
            plane_tool_search(&index, &grant, &json!({"query": "select:duckdb__query"})).await;
        assert_eq!(result["isError"], json!(false));
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("duckdb__query"), "{text}");
    }

    #[tokio::test]
    async fn a_successful_search_loads_the_tool_into_this_grants_hot_tier() {
        let index = test_index_with(&["read_file", "duckdb__query"]);
        let grant = test_grant_with_index(&["read_file", "duckdb__query"]);

        let before: Vec<String> = plane_tools_list(&grant)
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();
        assert!(!before.contains(&"duckdb__query".to_string()));

        plane_tool_search(&index, &grant, &json!({"query": "select:duckdb__query"})).await;

        let after: Vec<String> = plane_tools_list(&grant)
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();
        assert!(
            after.contains(&"duckdb__query".to_string()),
            "search did not load: {after:?}"
        );
    }

    /// A chat-harness grant as production mints it, with an allowlist and
    /// whatever index the caller attached.
    fn conversation_grant(allowed: &[&str]) -> PlaneGrant {
        let mut ctx = crate::magician_v2::execution::agentic::AgenticContext::default();
        ctx.principal = Some("owner".to_string());
        ctx.workspace = Some("home".to_string());
        ctx.agent_id = Some("personal-assistant".to_string());
        let mut grant = PlaneGrant::for_conversation(
            ctx,
            "sess-search".to_string(),
            tokio_util::sync::CancellationToken::new(),
        );
        grant.allowed_tools = allowed.iter().map(|s| s.to_string()).collect();
        grant
    }

    /// The bug the harness eval found: a conversation grant minted without the
    /// turn's index cannot load anything, so a Deferred allowlist name is inert.
    #[tokio::test]
    async fn a_conversation_grant_without_an_index_cannot_load_a_deferred_tool() {
        let grant = conversation_grant(&["duckdb__query"]);
        plane_tool_search(
            grant.tool_index.as_ref(),
            &grant,
            &json!({"query": "select:duckdb__query"}),
        )
        .await;
        let names: Vec<String> = plane_tools_list(&grant)
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();
        assert!(
            !names.contains(&"duckdb__query".to_string()),
            "loaded without an index: {names:?}"
        );
        assert!(!is_callable_configured(&grant, None, "duckdb__query"));
    }

    #[tokio::test]
    async fn a_conversation_grant_with_the_turns_index_loads_a_deferred_tool_by_search() {
        let index = std::sync::Arc::new(test_index_with(&["read_file", "duckdb__query"]));
        let grant =
            conversation_grant(&["duckdb__query"]).with_tool_index(std::sync::Arc::clone(&index));

        assert!(
            !is_callable_configured(&grant, None, "duckdb__query"),
            "callable before load"
        );
        plane_tool_search(
            grant.tool_index.as_ref(),
            &grant,
            &json!({"query": "select:duckdb__query"}),
        )
        .await;
        let names: Vec<String> = plane_tools_list(&grant)
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();
        assert!(
            names.contains(&"duckdb__query".to_string()),
            "search did not load: {names:?}"
        );
        assert!(
            is_callable_configured(&grant, None, "duckdb__query"),
            "loaded but not callable"
        );
    }

    #[tokio::test]
    async fn loading_is_scoped_to_the_grant_that_searched() {
        let index = test_index_with(&["duckdb__query"]);
        let a = test_grant_with_index(&["duckdb__query"]);
        let b = test_grant_with_index(&["duckdb__query"]);

        plane_tool_search(&index, &a, &json!({"query": "select:duckdb__query"})).await;

        let b_names: Vec<String> = plane_tools_list(&b)
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();
        assert!(
            !b_names.contains(&"duckdb__query".to_string()),
            "grant B saw A's load"
        );
    }

    #[tokio::test]
    async fn a_successful_search_signals_that_the_tool_list_changed() {
        let index = test_index_with(&["duckdb__query"]);
        let grant = test_grant_with_index(&["duckdb__query"]);
        let result =
            plane_tool_search(&index, &grant, &json!({"query": "select:duckdb__query"})).await;
        assert_eq!(result["_meta"]["toolsListChanged"], json!(true), "{result}");
    }

    #[tokio::test]
    async fn a_search_that_selects_nothing_does_not_signal_a_change() {
        let index = test_index_with(&["duckdb__query"]);
        let grant = test_grant_with_index(&["duckdb__query"]);
        let result =
            plane_tool_search(&index, &grant, &json!({"query": "select:no_such_tool"})).await;
        assert_ne!(result["_meta"]["toolsListChanged"], json!(true), "{result}");
    }

    #[tokio::test]
    async fn selecting_one_leaf_loads_the_whole_pack() {
        let index = test_index_with(&["duckdb__query", "duckdb__run"]);
        let grant = test_grant_with_index(&["duckdb__query", "duckdb__run"]);
        plane_tool_search(&index, &grant, &json!({"query": "select:duckdb__query"})).await;
        let after = advertised_names(&grant);
        assert!(after.contains(&"duckdb__query".to_string()), "{after:?}");
        assert!(
            after.contains(&"duckdb__run".to_string()),
            "whole-pack load missed sibling: {after:?}"
        );
    }

    /// A harness selects by the name it knows — the pack's (`select:browser`).
    /// The index holds leaves, never a bare pack entry, so that select used to
    /// match nothing and the harness concluded the tool did not exist.
    #[tokio::test]
    async fn selecting_a_pack_by_its_name_loads_its_leaves() {
        let index = test_index_with(&["browser__open", "browser__snapshot", "duckdb__query"]);
        let grant = test_grant_with_index(&["browser__open", "browser__snapshot", "duckdb__query"]);
        let result = plane_tool_search(&index, &grant, &json!({"query": "select:browser"})).await;
        let after = advertised_names(&grant);
        assert!(
            after.contains(&"browser__open".to_string()),
            "{after:?} {result}"
        );
        assert!(
            after.contains(&"browser__snapshot".to_string()),
            "{after:?}"
        );
        assert!(
            !after.contains(&"duckdb__query".to_string()),
            "another pack stays unloaded: {after:?}"
        );
    }

    #[tokio::test]
    async fn a_second_select_replaces_the_previous_pack() {
        let index = test_index_with(&["duckdb__query", "browser__open"]);
        let grant = test_grant_with_index(&["duckdb__query", "browser__open"]);
        plane_tool_search(&index, &grant, &json!({"query": "select:duckdb__query"})).await;
        plane_tool_search(&index, &grant, &json!({"query": "select:browser__open"})).await;
        let after = advertised_names(&grant);
        assert!(after.contains(&"browser__open".to_string()), "{after:?}");
        assert!(
            !after.contains(&"duckdb__query".to_string()),
            "unload-on-switch left the previous pack: {after:?}"
        );
    }

    fn bridged_specs(names: &[&str]) -> std::collections::BTreeMap<String, LLMToolSpec> {
        names
            .iter()
            .map(|name| {
                (
                    name.to_string(),
                    LLMToolSpec {
                        name: name.to_string(),
                        description: format!("{name} on the native mouth"),
                        parameters: json!({
                            "type": "object",
                            "properties": {"title": {"type": "string"}},
                            "required": ["title"],
                        }),
                    },
                )
            })
            .collect()
    }

    fn noop_bridge() -> crate::magician_v2::execution::plane::ChatMouthBridge {
        std::sync::Arc::new(
            |_call_id: String,
             _name: String,
             _arguments: Value,
             _cancel: tokio_util::sync::CancellationToken|
             -> futures_util::future::BoxFuture<'static, Value> {
                Box::pin(async { json!({"status": "ok"}) })
            },
        )
    }

    fn entry_named<'a>(tools: &'a [Value], name: &str) -> Option<&'a Value> {
        tools
            .iter()
            .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
    }

    /// A bridged tool is advertised with the native mouth's own schema and
    /// is callable; a grant without it lists nothing of the kind. The
    /// grant's allowlist still bounds it — set directly beside an allowlist
    /// that lacks the name, it is neither listed nor callable, which is why
    /// the mint goes through `with_bridged_tools`. A bridged name that is
    /// also a plane hot name is advertised once, under the mouth's schema;
    /// `read_result`, a plane control verb, is advertised when bridged.
    #[test]
    fn bridged_tools_are_advertised_with_their_own_schema() {
        let specs = bridged_specs(&["create_chat_thread", "create_task", "read_result"]);
        let grant = conversation_grant(&["read_file", "tool_search"])
            .with_bridged_tools(specs.clone(), noop_bridge());
        let tools = plane_tools_list(&grant);
        for name in ["create_chat_thread", "create_task", "read_result"] {
            let entry = entry_named(&tools, name).unwrap_or_else(|| panic!("{name} listed"));
            assert_eq!(entry["inputSchema"], specs[name].parameters, "{entry}");
            assert_eq!(entry["description"], json!(specs[name].description));
            assert_eq!(
                tools
                    .iter()
                    .filter(|tool| tool["name"] == json!(name))
                    .count(),
                1,
                "{name} advertised once"
            );
            assert!(
                is_callable_configured(&grant, None, name),
                "{name} callable"
            );
        }
        assert!(entry_named(&tools, "read_file").is_some());

        let without = conversation_grant(&["read_file", "tool_search"]);
        let names = advertised_names(&without);
        for name in ["create_chat_thread", "read_result"] {
            assert!(!names.contains(&name.to_string()), "{names:?}");
            assert!(!is_callable_configured(&without, None, name));
        }

        let mut denied = conversation_grant(&["read_file"]);
        denied.bridged_tools = bridged_specs(&["create_chat_thread"]);
        denied.mouth_bridge = Some(noop_bridge());
        assert!(!advertised_names(&denied).contains(&"create_chat_thread".to_string()));
        assert!(!is_callable_configured(&denied, None, "create_chat_thread"));

        let terminal = test_grant_with_index(&["create_task"])
            .with_bridged_tools(bridged_specs(&["create_chat_thread"]), noop_bridge());
        let listed = plane_tools_list(&terminal);
        let entry = entry_named(&listed, "create_chat_thread")
            .expect("an open allowlist admits the bridged name");
        assert_eq!(entry["inputSchema"]["required"], json!(["title"]));
    }

    /// A mouth that cannot re-list tools saw its preloaded hands in its
    /// first `tools/list`; a later select must add to that set, never
    /// swap it out from under the mouth.
    #[tokio::test]
    async fn a_select_on_a_preloaded_grant_merges_instead_of_replacing() {
        let index = test_index_with(&["duckdb__query", "browser__open"]);
        let mut grant = test_grant_with_index(&["duckdb__query", "browser__open"]);
        grant.replace_loaded_tools(HashSet::from(["duckdb__query".to_string()]));
        grant.preloaded_deferred = true;
        let result =
            plane_tool_search(&index, &grant, &json!({"query": "select:browser__open"})).await;
        assert_eq!(result["_meta"]["toolsListChanged"], json!(true), "{result}");
        assert_eq!(
            grant.loaded_tool_names(),
            HashSet::from(["duckdb__query".to_string(), "browser__open".to_string()])
        );
        let after = advertised_names(&grant);
        assert!(after.contains(&"browser__open".to_string()), "{after:?}");
        assert!(
            after.contains(&"duckdb__query".to_string()),
            "merge dropped a preloaded hand the mouth can still see: {after:?}"
        );

        // Re-selecting what is already loaded is not a change.
        let again =
            plane_tool_search(&index, &grant, &json!({"query": "select:duckdb__query"})).await;
        assert_eq!(again["_meta"]["toolsListChanged"], json!(false), "{again}");
    }
}

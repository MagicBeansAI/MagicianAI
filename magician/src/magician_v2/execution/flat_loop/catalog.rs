//! Flat-loop catalog builder: produces the hot tier (full schemas, eager) and
//! the deferred tier (bare names) for a given agent context. See
//! `docs/plans/2026-05-28-hot-deferred-tool-classification.md`.
//!
//! The hot tier reuses the existing control-tool builders and
//! `build_pack_capability_tool` from `agentic::native_catalog`, so a hot tool's
//! schema is byte-identical to what the live catalog emits. The deferred tier
//! lists every leaf the agent can reach (universals + per-agent allowlist,
//! expanded pack→primitives via the [`ToolIndex`]) minus the hot names.

use std::collections::HashSet;

use crate::magician_v2::execution::agentic::native_catalog::{
    build_delegate_to_agent_tool, build_handover_to_agent_tool, build_need_user_input_tool,
    build_pack_capability_tool, build_read_result_tool, build_spawn_sub_goal_tool,
    build_yield_tool, CatalogBuildContext,
};
use crate::magician_v2::execution::agentic::native_types::NativeExecutionTool;
use crate::magician_v2::execution::flat_loop::tool_index::ToolIndex;

/// Hot candidates. A pack tool is emitted only when the resolved owner grants
/// that pack/leaf; the fixed list controls eagerness, not authorization. Mirrors CC's hot
/// tier (Bash/Edit/Glob/Grep/Read/Write/ToolSearch) plus our native control
/// tools and `http`. `http` is hot because the legacy `action_type: "http"`
/// built-in lane was retired (everything is a tool now) and `http` is the one
/// former lane with no other always-hot equivalent (bash→`shell`,
/// file→`read_file`/`write_file`). `web_search` / `content_search` are hot
/// so a first-turn "search the web" does not fall through to `shell`/`http`
/// HTML scraping (2026-09-11 Instinct voice turn). Native controls are
/// built directly; pack tools are fetched from the index.
pub const ALWAYS_HOT_TOOL_NAMES: &[&str] = &[
    "yield",
    "need_user_input",
    "spawn_sub_goal",
    "read_result",
    "tool_search",
    "shell",
    "read_file",
    "write_file",
    "edit_file",
    "grep",
    "glob",
    "http",
    "web_search",
    "content_search",
];

/// Pack-backed hot tools (everything in [`ALWAYS_HOT_TOOL_NAMES`] except the
/// `yield` control tool). Schemas come from the index.
const HOT_PACK_TOOL_NAMES: &[&str] = &[
    "tool_search",
    "shell",
    "read_file",
    "write_file",
    "edit_file",
    "grep",
    "glob",
    "http",
    "web_search",
    "content_search",
];

/// One deferred tool: bare name + group label + optional search hint. No schema
/// (the LLM fetches it via `tool_search`).
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct DeferredEntry {
    pub name: String,
    pub group: &'static str,
    pub search_hint: Option<String>,
}

/// The two-tier flat catalog.
#[derive(Debug, Clone, Default)]
pub struct FlatToolCatalog {
    pub hot: Vec<NativeExecutionTool>,
    pub deferred: Vec<DeferredEntry>,
}

fn catalog_tool_denied(ctx: &CatalogBuildContext, name: &str) -> bool {
    ctx.denied_tool_names.iter().any(|denied| {
        crate::magician_v2::agents::types::tool_name_matches_block_entry(name, denied)
            || (matches!(
                name,
                "spawn_sub_goal" | "delegate_to_agent" | "handover_to_agent"
            ) && denied.trim() == "orchestrator")
    })
}

fn tool_is_directly_reachable(
    ctx: &CatalogBuildContext,
    index: &ToolIndex,
    tool_name: &str,
) -> bool {
    ctx.direct_capabilities.iter().any(|(grant, _, _)| {
        grant == tool_name
            || index
                .get(tool_name)
                .is_some_and(|entry| entry.pack_name == *grant)
    })
}

/// Conditionally-hot tools, by name, given the agent context. (Used for
/// documentation/testing; [`build_flat_loop_tools`] inlines the same gates.)
pub fn conditional_hot_tools(ctx: &CatalogBuildContext) -> Vec<&'static str> {
    let mut out = Vec::new();
    if ctx.has_delegation_targets {
        out.push("delegate_to_agent");
        if ctx
            .direct_capabilities
            .iter()
            .any(|(name, _, _)| name == "get_agent_details")
        {
            out.push("get_agent_details");
        }
        if ctx
            .direct_capabilities
            .iter()
            .any(|(name, _, _)| name == "find_agents_for_capability")
        {
            out.push("find_agents_for_capability");
        }
    }
    if !ctx.available_procedure_skills.is_empty()
        && ctx
            .direct_capabilities
            .iter()
            .any(|(name, _, _)| name == "activate_skill")
    {
        out.push("activate_skill");
    }
    if !ctx.available_procedure_skills.is_empty()
        && ctx
            .direct_capabilities
            .iter()
            .any(|(name, _, _)| name == "deactivate_skill")
    {
        out.push("deactivate_skill");
    }
    out
}

/// Dedup-and-filter helper for the deferred tier. Skips names already hot or
/// already pushed.
fn push_deferred(
    name: String,
    group: &'static str,
    hint: Option<String>,
    hot_names: &HashSet<&str>,
    seen: &mut HashSet<String>,
    out: &mut Vec<DeferredEntry>,
) {
    if hot_names.contains(name.as_str()) || !seen.insert(name.clone()) {
        return;
    }
    out.push(DeferredEntry {
        name,
        group,
        search_hint: hint,
    });
}

/// Build the flat catalog for an agent context against the process tool index.
/// `loaded_tools` are deferred tools the agent has already fetched via
/// `tool_search` this execution. Because decisions are native function calls, a
/// tool is only callable when it's in the hot tier (the request's `tools:`
/// array), so loaded tools are promoted from the deferred tier into the hot tier
/// here — that's what makes "load with tool_search, then call it" actually work.
pub fn build_flat_loop_tools(
    ctx: &CatalogBuildContext,
    index: &ToolIndex,
    loaded_tools: &[String],
) -> FlatToolCatalog {
    let mut hot: Vec<NativeExecutionTool> = Vec::new();

    // --- Hot tier ---
    // Native control tools are always hot. They do not live in ToolIndex, so
    // advertising them as deferred makes `tool_search(select:...)` impossible.
    if !catalog_tool_denied(ctx, "yield") {
        hot.push(build_yield_tool());
    }
    if !catalog_tool_denied(ctx, "need_user_input") {
        hot.push(build_need_user_input_tool());
    }
    if !catalog_tool_denied(ctx, "spawn_sub_goal") {
        hot.push(build_spawn_sub_goal_tool());
    }
    if !catalog_tool_denied(ctx, "read_result") {
        hot.push(build_read_result_tool());
    }

    // The hot pack tools, full schema from the index. FIX #4: when the
    // coordinator grant-gate is active, withhold any of these the agent was not
    // actually granted — `tool_search` always stays (it is the deferred-tool
    // loader). This stops a delegate-only VibeDev coordinator from being handed
    // an always-hot, unsandboxed `shell`/`write_file`/`edit_file` it never asked
    // for (the live-repo self-serve write vector). No-op when the gate is `None`
    // (every non-coordinator agent), so behavior is unchanged elsewhere.
    for name in HOT_PACK_TOOL_NAMES {
        if catalog_tool_denied(ctx, name) || !tool_is_directly_reachable(ctx, index, name) {
            continue;
        }
        if let Some(granted) = ctx.delegate_only_grant_gate.as_ref() {
            if *name != "tool_search" && !granted.contains(*name) {
                continue;
            }
        }
        if let Some(entry) = index.get(name) {
            hot.extend(build_pack_capability_tool(
                &entry.name,
                &entry.description,
                &entry.parameters_schema,
            ));
        }
    }

    // Conditional: delegate (control builder) + get_agent_details (index pack)
    // when delegation targets exist; activate_skill (index pack) when the agent
    // has procedure skills.
    if ctx.has_delegation_targets {
        if !catalog_tool_denied(ctx, "delegate_to_agent") {
            hot.push(build_delegate_to_agent_tool());
        }
        if !catalog_tool_denied(ctx, "handover_to_agent") {
            hot.push(build_handover_to_agent_tool());
        }
        if tool_is_directly_reachable(ctx, index, "get_agent_details") {
            if let Some(entry) = index.get("get_agent_details") {
                hot.extend(build_pack_capability_tool(
                    &entry.name,
                    &entry.description,
                    &entry.parameters_schema,
                ));
            }
        }
        if tool_is_directly_reachable(ctx, index, "find_agents_for_capability") {
            if let Some(entry) = index.get("find_agents_for_capability") {
                hot.extend(build_pack_capability_tool(
                    &entry.name,
                    &entry.description,
                    &entry.parameters_schema,
                ));
            }
        }
    }
    if !ctx.available_procedure_skills.is_empty() {
        for name in ["activate_skill", "deactivate_skill"] {
            if tool_is_directly_reachable(ctx, index, name) {
                if let Some(entry) = index.get(name) {
                    hot.extend(build_pack_capability_tool(
                        &entry.name,
                        &entry.description,
                        &entry.parameters_schema,
                    ));
                }
            }
        }
    }

    // Latency: pre-bind the coding-execution tools into the HOT tier for an agent
    // that DIRECTLY grants them (in `direct_capabilities`). Otherwise a coding
    // engineer's `run_coding_task` sits in the deferred tier, so its FIRST LLM turn
    // is wasted on a `tool_search` round-trip just to discover the tool before it
    // can call it (~one full model turn before Pi even starts). Pre-binding lets it
    // dispatch on turn 1. Self-scoping: only agents that grant these get them hot —
    // non-coding agents and the delegate-only coordinator (which never *directly*
    // grants the coding triad — it only inherits them) are unaffected. The deferred
    // tier below already excludes anything now hot, so this just promotes a tier.
    const CODING_HOT_TOOL_NAMES: &[&str] = &[
        "run_coding_task",
        "apply_code_proposal",
        "run_project_checks",
        "list_proposals",
    ];
    for name in CODING_HOT_TOOL_NAMES {
        if hot.iter().any(|tool| tool.name == *name) {
            continue;
        }
        if !ctx
            .direct_capabilities
            .iter()
            .any(|(cap, _, _)| cap == name)
        {
            continue;
        }
        if let Some(entry) = index.get(name) {
            hot.extend(build_pack_capability_tool(
                &entry.name,
                &entry.description,
                &entry.parameters_schema,
            ));
        }
    }

    // Promote tool_search-loaded tools into the hot tier so they become
    // natively callable. Computed before `hot_names` so they're also excluded
    // from the deferred tier below. Only index-known names are promotable; a
    // name already hot (e.g. the agent loaded a universal that's always hot) is
    // skipped by the dedup check.
    for name in loaded_tools {
        if catalog_tool_denied(ctx, name) || !tool_is_directly_reachable(ctx, index, name) {
            continue;
        }
        if hot.iter().any(|t| &t.name == name) {
            continue;
        }
        if let Some(entry) = index.get(name) {
            // FIX #4: under the coordinator grant-gate, never promote a loaded
            // tool whose owning pack was not granted — defense-in-depth so a
            // withheld mutator cannot sneak back into the hot tier even if it
            // was somehow loaded. `tool_search` is always promotable.
            if let Some(granted) = ctx.delegate_only_grant_gate.as_ref() {
                if name.as_str() != "tool_search"
                    && !granted.contains(name.as_str())
                    && !granted.contains(entry.pack_name.as_str())
                {
                    continue;
                }
            }
            hot.extend(build_pack_capability_tool(
                &entry.name,
                &entry.description,
                &entry.parameters_schema,
            ));
        }
    }

    let hot_names: HashSet<&str> = hot.iter().map(|t| t.name.as_str()).collect();

    // --- Deferred tier ---
    let mut seen: HashSet<String> = HashSet::new();
    let mut deferred: Vec<DeferredEntry> = Vec::new();

    // Every reachable pack capability, expanded to its leaves via the index.
    for (pack_or_leaf, _desc, _schema) in &ctx.direct_capabilities {
        if catalog_tool_denied(ctx, pack_or_leaf) {
            continue;
        }
        // FIX #4: under the coordinator grant-gate, only the agent's own granted
        // packs reach the deferred tier — so `tool_search` cannot surface (and
        // thus cannot load) a universal-substrate mutator (`shell`/`files`/`http`
        // etc.) that a delegate-only coordinator was never granted. Withheld
        // packs simply never appear in the catalog the LLM sees. Control tools
        // (`need_user_input`/`spawn_sub_goal`/`handover_to_agent`) are pushed
        // after this loop and are unaffected.
        if let Some(granted) = ctx.delegate_only_grant_gate.as_ref() {
            if !granted.contains(pack_or_leaf.as_str()) {
                continue;
            }
        }
        let leaves = index.leaf_names_for_pack(pack_or_leaf);
        if leaves.is_empty() {
            // Not a known pack in the index (e.g. a legacy in-memory tool):
            // surface the bare name as-is, classified as a pack tool.
            push_deferred(
                pack_or_leaf.clone(),
                "pack",
                None,
                &hot_names,
                &mut seen,
                &mut deferred,
            );
        } else {
            for leaf in leaves {
                let (group, hint) = index
                    .get(&leaf)
                    .map(|e| (e.group, e.search_hint.clone()))
                    .unwrap_or(("pack", None));
                // A denied or withheld leaf must not be advertised either. `tool_search`
                // loads from these names, so a name left here is a tool the
                // model can still fetch and still be refused for — the wasted
                // iteration, moved one step later rather than removed.
                if catalog_tool_denied(ctx, &leaf) || leaf_is_withheld(index, &leaf) {
                    continue;
                }
                push_deferred(leaf, group, hint, &hot_names, &mut seen, &mut deferred);
            }
        }
    }

    FlatToolCatalog { hot, deferred }
}

/// Whether a leaf may not be offered to an autonomous run at all — §4A, applied
/// to the deferred tier.
///
/// Asked through the same projection [`build_pack_capability_tool`] applies, so
/// a leaf cannot be withheld from the callable tier while still being
/// discoverable, or the other way round. A leaf the index does not know is not
/// withheld: there is no schema to judge, and refusing on ignorance would drop
/// tools for a reason that has nothing to do with reaching people.
fn leaf_is_withheld(index: &ToolIndex, leaf: &str) -> bool {
    use crate::magician_v2::execution::restricted_toolset::{
        project_capability_tool, ToolProjection,
    };

    index.get(leaf).is_some_and(|entry| {
        matches!(
            project_capability_tool(&entry.name, &entry.parameters_schema),
            ToolProjection::Withheld { .. }
        )
    })
}

/// Promote explicitly selected surface-initial leaves from deferred metadata
/// into the provider-hot tier. The leaf must already be present in `deferred`,
/// which is the authorization-filtered result of [`build_flat_loop_tools`]; a
/// requested but unauthorized/unknown name is therefore a no-op.
pub fn promote_surface_initial_hot_tools(
    catalog: &mut FlatToolCatalog,
    index: &ToolIndex,
    requested_names: &[&str],
) -> usize {
    let mut promoted = 0usize;
    for requested in requested_names {
        if catalog.hot.iter().any(|tool| tool.name == *requested) {
            continue;
        }
        let Some(position) = catalog
            .deferred
            .iter()
            .position(|entry| entry.name == *requested)
        else {
            continue;
        };
        let Some(entry) = index.get(requested) else {
            continue;
        };
        // A withheld tool is not a promotion. It stays out of the hot tier and
        // out of the deferred names, so the count reports what the model can
        // actually call rather than what was asked for.
        let Some(tool) =
            build_pack_capability_tool(&entry.name, &entry.description, &entry.parameters_schema)
        else {
            catalog.deferred.remove(position);
            continue;
        };
        catalog.hot.push(tool);
        catalog.deferred.remove(position);
        promoted += 1;
    }
    promoted
}

/// Build the admitted flat surface eagerly for callers without a deferred-tool
/// loader. Indexed packs retain their concrete action schemas; execution-local
/// tools retain their supplied schemas without entering the shared index.
pub fn build_eager_flat_loop_tools(
    ctx: &CatalogBuildContext,
    index: &ToolIndex,
) -> Vec<NativeExecutionTool> {
    let mut catalog = build_flat_loop_tools(ctx, index, &[]);
    let names = catalog
        .deferred
        .iter()
        .map(|entry| entry.name.clone())
        .collect::<Vec<_>>();
    let requested = names.iter().map(String::as_str).collect::<Vec<_>>();
    promote_surface_initial_hot_tools(&mut catalog, index, &requested);
    for entry in catalog.deferred {
        // Only names that survived the normal catalog's grant/deny projection
        // can reach this fallback. Indexed leaves never fall back to a pack's
        // empty routing schema when their concrete schema was withheld.
        if index.get(&entry.name).is_some() {
            continue;
        }
        if let Some((name, description, schema)) = ctx
            .direct_capabilities
            .iter()
            .find(|(name, _, _)| name == &entry.name)
        {
            catalog
                .hot
                .extend(build_pack_capability_tool(name, description, schema));
        }
    }
    catalog.hot
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::agentic::native_catalog::CatalogBuildContext;
    use crate::magician_v2::execution::capability::CapabilityPackDefinition;
    use crate::magician_v2::execution::flat_loop::tool_index::{build_tool_index, ToolIndex};
    use serde_json::json;

    fn universal(name: &str) -> CapabilityPackDefinition {
        serde_yaml::from_str(&format!(
            "name: {name}\ndescription: {name} tool.\nparameters: []\nimplementation:\n  type: compiled\n  provider_name: {name}\n"
        ))
        .unwrap()
    }

    fn duckdb_pack() -> CapabilityPackDefinition {
        serde_yaml::from_str(
            "name: duckdb\ndescription: duck\nparameters: []\nnative_action_schemas:\n  query:\n    description: q\n    parameters: [sql]\n    required: [sql]\n    parameter_overrides:\n      sql: {type: string}\nimplementation:\n  type: primitive\n  provider_name: duckdb\n",
        )
        .unwrap()
    }

    fn full_index() -> ToolIndex {
        let names = [
            "tool_search",
            "shell",
            "read_file",
            "write_file",
            "edit_file",
            "grep",
            "glob",
            "activate_skill",
            "deactivate_skill",
            "get_agent_details",
            "save_preference",
        ];
        let mut packs: Vec<_> = names.iter().map(|n| universal(n)).collect();
        packs.push(duckdb_pack());
        build_tool_index(&packs)
    }

    fn ctx(direct: Vec<&str>, delegation: bool, skills: bool) -> CatalogBuildContext {
        CatalogBuildContext {
            allowed_action_types: None,
            credentials_enabled: false,
            has_delegation_targets: delegation,
            direct_capabilities: direct
                .into_iter()
                .map(|n| {
                    (
                        n.to_string(),
                        format!("{n} desc"),
                        json!({"type":"object","properties":{}}),
                    )
                })
                .collect(),
            is_chat_mode: false,
            available_procedure_skills: if skills {
                vec![("foo".into(), "bar".into())]
            } else {
                vec![]
            },
            denied_tool_names: Vec::new(),
            delegate_only_grant_gate: None,
        }
    }

    #[test]
    fn eager_catalog_preserves_leaf_and_execution_local_schemas() {
        let idx = full_index();
        let mut context = ctx(vec!["duckdb"], false, false);
        let local_schema = json!({"type":"object","properties":{"entity":{"type":"string"}},"required":["entity"]});
        context.direct_capabilities.push((
            "local_query".into(),
            "Read the admitted store".into(),
            local_schema.clone(),
        ));
        let tools = build_eager_flat_loop_tools(&context, &idx);
        assert!(tools.iter().any(|tool| tool.name == "duckdb__query"));
        assert!(!tools.iter().any(|tool| tool.name == "duckdb"));
        assert!(!tools.iter().any(|tool| tool.name == "shell"));
        let emitted = &tools
            .iter()
            .find(|tool| tool.name == "local_query")
            .unwrap()
            .parameters;
        assert_eq!(
            emitted["properties"]["entity"],
            local_schema["properties"]["entity"]
        );
        assert_eq!(emitted["required"], local_schema["required"]);
        assert_eq!(emitted["type"], "object");
        for metadata in ["thinking", "decision_metadata", "task_state_action"] {
            assert!(
                emitted["properties"].get(metadata).is_some(),
                "execution metadata: {metadata}"
            );
        }

        context.denied_tool_names = vec!["duckdb__query".into(), "local_query".into()];
        let denied = build_eager_flat_loop_tools(&context, &idx);
        assert!(!denied
            .iter()
            .any(|tool| tool.name == "duckdb__query" || tool.name == "local_query"));
    }

    #[test]
    fn denied_pack_leaf_stays_out_of_deferred_loaded_and_eager_catalogs() {
        let mut pack = duckdb_pack();
        pack.native_action_schemas.insert(
            "inspect".into(),
            pack.native_action_schemas["query"].clone(),
        );
        let index = build_tool_index(&[pack]);
        let mut context = ctx(vec!["duckdb"], false, false);
        context.denied_tool_names = vec!["duckdb__query".into()];

        let deferred = build_flat_loop_tools(&context, &index, &[]);
        assert!(!deferred
            .deferred
            .iter()
            .any(|tool| tool.name == "duckdb__query"));
        assert!(deferred
            .deferred
            .iter()
            .any(|tool| tool.name == "duckdb__inspect"));

        let loaded = build_flat_loop_tools(
            &context,
            &index,
            &["duckdb__query".into(), "duckdb__inspect".into()],
        );
        assert!(!loaded.hot.iter().any(|tool| tool.name == "duckdb__query"));
        assert!(loaded.hot.iter().any(|tool| tool.name == "duckdb__inspect"));

        let eager = build_eager_flat_loop_tools(&context, &index);
        assert!(!eager.iter().any(|tool| tool.name == "duckdb__query"));
        assert!(eager.iter().any(|tool| tool.name == "duckdb__inspect"));
    }

    #[test]
    fn hot_tier_includes_resolved_hot_pack_grants() {
        let idx = full_index();
        let cat = build_flat_loop_tools(
            &ctx(
                vec![
                    "tool_search",
                    "shell",
                    "read_file",
                    "write_file",
                    "edit_file",
                    "grep",
                    "glob",
                    "save_preference",
                ],
                false,
                false,
            ),
            &idx,
            &[],
        );
        let hot: Vec<&str> = cat.hot.iter().map(|t| t.name.as_str()).collect();
        for expected in [
            "yield",
            "need_user_input",
            "spawn_sub_goal",
            "tool_search",
            "shell",
            "read_file",
            "write_file",
            "edit_file",
            "grep",
            "glob",
        ] {
            assert!(hot.contains(&expected), "missing hot tool {expected}");
        }
        assert!(!hot.contains(&"delegate_to_agent"));
        assert!(!hot.contains(&"activate_skill"));
        assert!(
            cat.hot
                .iter()
                .find(|t| t.name == "yield")
                .unwrap()
                .is_control_tool
        );
        assert!(
            cat.hot
                .iter()
                .find(|t| t.name == "need_user_input")
                .unwrap()
                .is_control_tool
        );
        assert!(
            cat.hot
                .iter()
                .find(|t| t.name == "spawn_sub_goal")
                .unwrap()
                .is_control_tool
        );
    }

    #[test]
    fn surface_initial_hot_promotion_is_authorization_bound_and_exact() {
        let idx = full_index();
        let mut catalog = build_flat_loop_tools(
            &ctx(vec!["tool_search", "save_preference"], false, false),
            &idx,
            &[],
        );
        assert!(catalog
            .deferred
            .iter()
            .any(|entry| entry.name == "save_preference"));
        assert_eq!(
            promote_surface_initial_hot_tools(
                &mut catalog,
                &idx,
                &["save_preference", "search_memory"]
            ),
            1
        );
        assert!(catalog
            .hot
            .iter()
            .any(|tool| tool.name == "save_preference"));
        assert!(!catalog
            .deferred
            .iter()
            .any(|entry| entry.name == "save_preference"));
        assert!(!catalog.hot.iter().any(|tool| tool.name == "search_memory"));
    }

    #[test]
    fn conditional_hots_appear_when_enabled() {
        let idx = full_index();
        let cat = build_flat_loop_tools(
            &ctx(
                vec![
                    "get_agent_details",
                    "activate_skill",
                    "deactivate_skill",
                    "save_preference",
                ],
                true,
                true,
            ),
            &idx,
            &[],
        );
        let hot: Vec<&str> = cat.hot.iter().map(|t| t.name.as_str()).collect();
        assert!(hot.contains(&"delegate_to_agent"));
        assert!(hot.contains(&"handover_to_agent"));
        assert!(hot.contains(&"get_agent_details"));
        assert!(hot.contains(&"activate_skill"));
        assert!(hot.contains(&"deactivate_skill"));
    }

    #[test]
    fn hot_and_loaded_tools_cannot_cross_direct_reachability() {
        let idx = full_index();
        let cat = build_flat_loop_tools(
            &ctx(vec!["save_preference"], true, false),
            &idx,
            &["shell".to_string(), "get_agent_details".to_string()],
        );
        let hot: Vec<&str> = cat.hot.iter().map(|tool| tool.name.as_str()).collect();

        assert!(!hot.contains(&"shell"));
        assert!(!hot.contains(&"get_agent_details"));
        assert!(hot.contains(&"delegate_to_agent"));
    }

    #[test]
    fn deferred_tier_lists_reachable_leaves_minus_hot() {
        let idx = full_index();
        let cat = build_flat_loop_tools(
            &ctx(vec!["duckdb", "save_preference"], false, false),
            &idx,
            &[],
        );
        let deferred: Vec<&str> = cat.deferred.iter().map(|d| d.name.as_str()).collect();
        assert!(deferred.contains(&"duckdb__query"));
        assert!(deferred.contains(&"save_preference"));
        assert!(!deferred.contains(&"need_user_input"));
        assert!(!deferred.contains(&"spawn_sub_goal"));
        assert!(!deferred.contains(&"shell"));
        assert!(!deferred.contains(&"yield"));
    }

    #[test]
    fn handover_is_deferred_only_with_delegation() {
        let idx = full_index();
        let no = build_flat_loop_tools(&ctx(vec![], false, false), &idx, &[]);
        assert!(!no.hot.iter().any(|d| d.name == "handover_to_agent"));
        assert!(!no.deferred.iter().any(|d| d.name == "handover_to_agent"));
        let yes = build_flat_loop_tools(&ctx(vec![], true, false), &idx, &[]);
        assert!(yes.hot.iter().any(|d| d.name == "handover_to_agent"));
        assert!(!yes.deferred.iter().any(|d| d.name == "handover_to_agent"));
    }

    #[test]
    fn conditional_hot_tools_helper_matches_gates() {
        assert!(conditional_hot_tools(&ctx(vec![], false, false)).is_empty());
        assert_eq!(
            conditional_hot_tools(&ctx(
                vec![
                    "get_agent_details",
                    "find_agents_for_capability",
                    "activate_skill",
                    "deactivate_skill"
                ],
                true,
                true
            )),
            vec![
                "delegate_to_agent",
                "get_agent_details",
                "find_agents_for_capability",
                "activate_skill",
                "deactivate_skill"
            ]
        );
    }

    /// A pack that reaches people, in the shape the shipped skills actually
    /// declare: a typed sending action and a typed read, each accepting an
    /// `extra_args` passthrough, plus an argv-only `raw`.
    fn mail_pack() -> CapabilityPackDefinition {
        serde_yaml::from_str(
            "name: gmail\ndescription: mail\nparameters: []\nnative_action_schemas:\n  send:\n    description: send mail\n    parameters: [to, subject, body, extra_args]\n    required: [to, subject, body]\n  triage:\n    description: read mail\n    parameters: [query, extra_args]\n  raw:\n    description: raw argv\n    parameters: [args]\n    required: [args]\nimplementation:\n  type: primitive\n  provider_name: gmail\n",
        )
        .expect("mail pack")
    }

    fn hot_named<'a>(catalog: &'a FlatToolCatalog, name: &str) -> &'a NativeExecutionTool {
        catalog
            .hot
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("`{name}` is not in the hot tier"))
    }

    fn property_names(tool: &NativeExecutionTool) -> Vec<String> {
        let mut names: Vec<String> = tool.parameters["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .cloned()
            .collect();
        names.sort();
        names
    }

    /// A leaf with no bindable form is never advertised, in either tier.
    ///
    /// `gmail__raw` requires an argv passthrough, so `restrict` refuses every
    /// call of it and the dispatch gate returns "NOT SENT". Leaving its NAME in
    /// the deferred tier does not soften that — `tool_search` would load it, the
    /// model would compose an act, and the refusal would arrive one iteration
    /// later. Pins that both tiers agree.
    #[test]
    fn a_leaf_with_no_bindable_form_is_advertised_nowhere() {
        let index = build_tool_index(&[mail_pack(), universal("tool_search")]);
        let catalog = build_flat_loop_tools(
            &ctx(vec!["gmail", "tool_search"], false, false),
            &index,
            &["gmail__raw".to_string()],
        );

        assert!(
            !catalog.hot.iter().any(|tool| tool.name == "gmail__raw"),
            "a tool refused on every call must not be loadable"
        );
        assert!(
            !catalog
                .deferred
                .iter()
                .any(|entry| entry.name == "gmail__raw"),
            "a tool refused on every call must not be discoverable either"
        );
        // The leaves that CAN be composed are still there — the narrowing must
        // not read as "outward capabilities are switched off".
        assert!(catalog
            .deferred
            .iter()
            .any(|entry| entry.name == "gmail__triage"));
    }

    /// A loaded sending leaf is handed to the model without its escape hatch.
    ///
    /// This is the substitution: `extra_args` is declared by the shipped skill
    /// and refused by the dispatch gate, so the only honest schema is one that
    /// never mentions it. A regression here restores the wasted iteration — the
    /// model composes raw argv because the schema invited it to.
    #[test]
    fn a_loaded_sending_leaf_is_offered_only_in_its_bindable_form() {
        let index = build_tool_index(&[mail_pack(), universal("tool_search")]);
        let catalog = build_flat_loop_tools(
            &ctx(vec!["gmail", "tool_search"], false, false),
            &index,
            &["gmail__send".to_string()],
        );

        let send = hot_named(&catalog, "gmail__send");
        assert!(
            !property_names(send).contains(&"extra_args".to_string()),
            "the escape hatch must not be offered: {:?}",
            property_names(send)
        );
        for expected in ["to", "subject", "body"] {
            assert!(
                property_names(send).contains(&expected.to_string()),
                "`{expected}` is part of the bindable form and must survive"
            );
        }
        assert!(
            send.description.contains("bindable form"),
            "the model must be told what changed, not left to infer it: {}",
            send.description
        );
    }

    /// A read on a sending capability keeps the parameters it reads with.
    ///
    /// The closed set names what may be SENT. Applying it to `gmail__triage`
    /// would leave a search tool with no way to say what to search for — a
    /// refusal wearing the costume of a restriction.
    #[test]
    fn a_read_on_a_sending_capability_keeps_its_own_parameters() {
        let index = build_tool_index(&[mail_pack(), universal("tool_search")]);
        let catalog = build_flat_loop_tools(
            &ctx(vec!["gmail", "tool_search"], false, false),
            &index,
            &["gmail__triage".to_string()],
        );

        let triage = hot_named(&catalog, "gmail__triage");
        assert!(property_names(triage).contains(&"query".to_string()));
        assert!(!property_names(triage).contains(&"extra_args".to_string()));
    }

    /// Nothing that reaches nobody is reshaped.
    ///
    /// The narrowing is scoped to capabilities that can transmit. If it ever
    /// widened, every pack in the catalog would silently lose parameters, and
    /// the failure would look like a model that suddenly cannot call anything.
    #[test]
    fn a_pack_that_reaches_nobody_is_projected_unchanged() {
        let index = build_tool_index(&[duckdb_pack(), universal("tool_search")]);
        let catalog = build_flat_loop_tools(
            &ctx(vec!["duckdb", "tool_search"], false, false),
            &index,
            &["duckdb__query".to_string()],
        );

        let query = hot_named(&catalog, "duckdb__query");
        assert!(property_names(query).contains(&"sql".to_string()));
        assert_eq!(query.description, "q");
    }
}

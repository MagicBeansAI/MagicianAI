//! Emit production-resolved tool catalogs for the live authorization evaluator.
//!
//! The Python evaluator deliberately does not recreate Magician's policy
//! rules. This executable supplies both the pre-policy candidate catalog and
//! the catalog produced by `resolve_effective_tool_policy_snapshot`, including
//! constrained delegation target schemas and the immutable snapshot id.

use std::collections::BTreeSet;

use magician::magician_v2::agents::{
    AgentDefinition, AgentInvocationContext, FeatureMode, InvocationSourceKind, InvocationSurface,
};
use magician::magician_v2::execution::agentic::native_catalog::{
    build_delegate_to_agent_tool, build_need_user_input_tool, build_spawn_sub_goal_tool,
    build_yield_tool,
};
use magician::magician_v2::execution::agentic::{
    resolve_effective_tool_policy_snapshot, NativeExecutionTool, SnapshotResolutionInput,
};
use serde::Serialize;

#[derive(Serialize)]
struct CatalogExport {
    schema_version: u32,
    generated_by: &'static str,
    scenarios: Vec<ScenarioCatalog>,
}

#[derive(Serialize)]
struct ScenarioCatalog {
    name: &'static str,
    snapshot_id: String,
    baseline_tools: Vec<NativeExecutionTool>,
    candidate_tools: Vec<NativeExecutionTool>,
    dispatch_tool_names: BTreeSet<String>,
    deferred_tool_names: BTreeSet<String>,
    delegation_targets: BTreeSet<String>,
    handover_targets: BTreeSet<String>,
}

fn tool(name: &str, description: &str, properties: serde_json::Value) -> NativeExecutionTool {
    let required = properties
        .as_object()
        .map(|values| values.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    NativeExecutionTool {
        name: name.to_string(),
        description: description.to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        }),
        is_control_tool: false,
    }
}

fn search_memory_tool() -> NativeExecutionTool {
    tool(
        "search_memory",
        "Search relevant durable memory without mutating it.",
        serde_json::json!({"query": {"type": "string"}}),
    )
}

fn tool_search_tool() -> NativeExecutionTool {
    tool(
        "tool_search",
        "Search authorized deferred tools. Use query select:ToolName to load that tool's family.",
        serde_json::json!({
            "query": {"type": "string"},
            "max_results": {"type": "integer", "minimum": 1, "maximum": 10}
        }),
    )
}

fn browser_open_tool() -> NativeExecutionTool {
    tool(
        "browser__open",
        "Open the requested URL in the browser.",
        serde_json::json!({"url": {"type": "string"}}),
    )
}

fn browser_click_tool() -> NativeExecutionTool {
    tool(
        "browser__click",
        "Click a browser element by reference.",
        serde_json::json!({"element_ref": {"type": "string"}}),
    )
}

fn delegate_to_chat_tool() -> NativeExecutionTool {
    let mut tool = tool(
        "delegate_to_chat",
        "Hand this live turn to Magician chat, which may use Magician tools; speak Magician's returned speech.",
        serde_json::json!({"intent": {"type": "string"}}),
    );
    tool.is_control_tool = true;
    tool
}

fn screen_draw_tool() -> NativeExecutionTool {
    tool(
        "screen-draw",
        "Draw one narrated Tutor storyboard step on the active overlay.",
        serde_json::json!({
            "shape_json": {"type": "string"},
            "narration": {"type": "string"},
            "tutor_step_label": {"type": "string"}
        }),
    )
}

fn mutator_tool(name: &str, description: &str) -> NativeExecutionTool {
    tool(
        name,
        description,
        serde_json::json!({"request": {"type": "string"}}),
    )
}

fn delegate_tool_with_targets(targets: &[&str]) -> NativeExecutionTool {
    let mut tool = build_delegate_to_agent_tool();
    let target_schema = tool
        .parameters
        .pointer_mut("/properties/delegation_targets/items/properties/target_agent_id")
        .and_then(serde_json::Value::as_object_mut)
        .expect("delegate target schema");
    target_schema.insert(
        "enum".to_string(),
        serde_json::Value::Array(
            targets
                .iter()
                .map(|target| serde_json::Value::String((*target).to_string()))
                .collect(),
        ),
    );
    tool
}

fn definition(agent_id: &str, extra: &str) -> AgentDefinition {
    AgentDefinition::from_yaml_str(&format!(
        "agent_id: {agent_id}\nname: Eval {agent_id}\npersona: Production policy eval fixture\ntrust_level: local\n{extra}"
    ))
    .expect("valid eval definition")
}

fn invocation(
    agent_id: &str,
    surface: InvocationSurface,
    feature_mode: FeatureMode,
    source_kind: InvocationSourceKind,
) -> AgentInvocationContext {
    AgentInvocationContext {
        principal: "eval-owner".to_string(),
        workspace: "default".to_string(),
        source_agent_id: match source_kind {
            InvocationSourceKind::Delegated | InvocationSourceKind::Handover => {
                Some("personal-assistant".to_string())
            },
            _ => None,
        },
        target_agent_id: agent_id.to_string(),
        surface,
        feature_mode,
        source_kind,
        chat_session_id: Some(format!("eval-{agent_id}")),
        chat_turn_id: Some("catalog-export".to_string()),
    }
}

fn resolve_catalog(
    name: &'static str,
    definition: AgentDefinition,
    invocation: AgentInvocationContext,
    baseline_tools: Vec<NativeExecutionTool>,
    direct: &[&str],
    runtime: &[&str],
    deferred: &[&str],
    delegate_owned: &[&str],
    delegation_targets: &[&str],
) -> ScenarioCatalog {
    resolve_projected_catalog(
        name,
        definition,
        invocation,
        baseline_tools.clone(),
        baseline_tools,
        direct,
        runtime,
        deferred,
        delegate_owned,
        delegation_targets,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_projected_catalog(
    name: &'static str,
    definition: AgentDefinition,
    invocation: AgentInvocationContext,
    baseline_tools: Vec<NativeExecutionTool>,
    candidate_provider_tools: Vec<NativeExecutionTool>,
    direct: &[&str],
    runtime: &[&str],
    deferred: &[&str],
    delegate_owned: &[&str],
    delegation_targets: &[&str],
) -> ScenarioCatalog {
    let snapshot = resolve_effective_tool_policy_snapshot(
        &definition,
        invocation,
        SnapshotResolutionInput {
            provider_tools: candidate_provider_tools,
            runtime_tool_names: runtime.iter().map(|name| (*name).to_string()).collect(),
            deferred_tool_names: deferred.iter().map(|name| (*name).to_string()).collect(),
            direct_reachable_tool_names: direct.iter().map(|name| (*name).to_string()).collect(),
            delegate_owned_tool_names: delegate_owned
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
            delegation_target_ids: delegation_targets
                .iter()
                .map(|target| (*target).to_string())
                .collect(),
            ..Default::default()
        },
    )
    .expect("production policy snapshot");

    ScenarioCatalog {
        name,
        snapshot_id: snapshot.snapshot_id.clone(),
        baseline_tools,
        candidate_tools: snapshot.provider_specs.clone(),
        dispatch_tool_names: snapshot.dispatch_tool_names.clone(),
        deferred_tool_names: snapshot.deferred_tools.keys().cloned().collect(),
        delegation_targets: snapshot.delegation_targets.keys().cloned().collect(),
        handover_targets: snapshot.handover_targets.keys().cloned().collect(),
    }
}

fn main() {
    let ordinary_baseline = vec![
        search_memory_tool(),
        build_yield_tool(),
        screen_draw_tool(),
        delegate_tool_with_targets(&["web-researcher", "brainstorm-facilitator"]),
    ];
    let tutor_baseline = vec![
        screen_draw_tool(),
        build_yield_tool(),
        delegate_tool_with_targets(&["mac-operator", "brainstorm-facilitator"]),
    ];
    let wildcard_baseline = vec![
        delegate_tool_with_targets(&["web-researcher", "brainstorm-facilitator"]),
        build_yield_tool(),
    ];
    let explicit_baseline = vec![
        delegate_tool_with_targets(&["image-analyst", "web-researcher", "brainstorm-facilitator"]),
        build_yield_tool(),
    ];
    let thinking_map_baseline = vec![
        search_memory_tool(),
        build_yield_tool(),
        mutator_tool("shell", "Run a shell command."),
        mutator_tool("create_task", "Create a durable task."),
        delegate_tool_with_targets(&["web-researcher", "brainstorm-facilitator"]),
    ];
    let denied_baseline = vec![
        search_memory_tool(),
        build_yield_tool(),
        mutator_tool("shell", "Run a shell command."),
        mutator_tool("files", "Mutate a workspace file."),
        mutator_tool("delete_task", "Delete a durable task."),
        delegate_tool_with_targets(&["worker", "brainstorm-facilitator"]),
    ];
    let browser_full_baseline = vec![
        tool_search_tool(),
        search_memory_tool(),
        browser_open_tool(),
        browser_click_tool(),
    ];
    let chat_initial_hot = vec![tool_search_tool(), search_memory_tool()];
    let browser_loaded = vec![
        tool_search_tool(),
        search_memory_tool(),
        browser_open_tool(),
        browser_click_tool(),
    ];
    let voice_full_baseline = vec![
        tool_search_tool(),
        search_memory_tool(),
        browser_open_tool(),
        browser_click_tool(),
        delegate_to_chat_tool(),
    ];
    let voice_initial_hot = vec![
        tool_search_tool(),
        search_memory_tool(),
        delegate_to_chat_tool(),
    ];
    let voice_loaded = vec![
        tool_search_tool(),
        search_memory_tool(),
        browser_open_tool(),
        browser_click_tool(),
        delegate_to_chat_tool(),
    ];
    let task_full_baseline = vec![
        build_yield_tool(),
        build_need_user_input_tool(),
        build_spawn_sub_goal_tool(),
        tool_search_tool(),
        search_memory_tool(),
        browser_open_tool(),
        browser_click_tool(),
    ];
    let task_initial_hot = vec![
        build_yield_tool(),
        build_need_user_input_tool(),
        build_spawn_sub_goal_tool(),
        tool_search_tool(),
        search_memory_tool(),
    ];
    let task_loaded = vec![
        build_yield_tool(),
        build_need_user_input_tool(),
        build_spawn_sub_goal_tool(),
        tool_search_tool(),
        search_memory_tool(),
        browser_open_tool(),
        browser_click_tool(),
    ];

    let scenarios = vec![
        resolve_catalog(
            "ordinary_incidental_tutor_text",
            definition("personal-assistant", "delegation_targets:\n  - '*'\n"),
            invocation(
                "personal-assistant",
                InvocationSurface::Chat,
                FeatureMode::None,
                InvocationSourceKind::ChatInline,
            ),
            ordinary_baseline,
            &["search_memory", "screen-draw"],
            &[],
            &[],
            &[],
            &[],
        ),
        resolve_catalog(
            "typed_tutor_first_storyboard_step",
            definition("personal-assistant", "delegation_targets:\n  - '*'\n"),
            invocation(
                "personal-assistant",
                InvocationSurface::Tutor,
                FeatureMode::Tutor,
                InvocationSourceKind::ProductFeature,
            ),
            tutor_baseline,
            &["screen-draw"],
            &["screen-draw"],
            &[],
            &[],
            &[],
        ),
        resolve_catalog(
            "ordinary_wildcard_excludes_loom",
            definition("personal-assistant", "delegation_targets:\n  - '*'\n"),
            invocation(
                "personal-assistant",
                InvocationSurface::Chat,
                FeatureMode::None,
                InvocationSourceKind::ChatInline,
            ),
            wildcard_baseline,
            &[],
            &[],
            &[],
            &[],
            &["web-researcher"],
        ),
        resolve_catalog(
            "explicit_authorized_delegate",
            definition("personal-assistant", "delegation_targets:\n  - '*'\n"),
            invocation(
                "personal-assistant",
                InvocationSurface::Delegation,
                FeatureMode::None,
                InvocationSourceKind::Delegated,
            ),
            explicit_baseline,
            &[],
            &[],
            &[],
            &[],
            &["image-analyst"],
        ),
        resolve_catalog(
            "typed_thinking_map_frontier",
            definition(
                "brainstorm-facilitator",
                "tools:\n  - search_memory\n  - yield\ninvocation_policy:\n  discoverability: surface_only\n  delegation: none\n  allowed_direct_surfaces:\n    - thinking_map\n",
            ),
            invocation(
                "brainstorm-facilitator",
                InvocationSurface::ThinkingMap,
                FeatureMode::Brainstorm,
                InvocationSourceKind::ProductFeature,
            ),
            thinking_map_baseline,
            &["search_memory"],
            &[],
            &[],
            &[],
            &[],
        ),
        resolve_catalog(
            "denied_mutator_fails_closed",
            definition(
                "read-only-worker",
                "denied_tools:\n  - shell\n  - files\n  - delete_task\n  - delegate_to_agent\n  - handover_to_agent\n  - orchestrate_pipeline\n  - spawn_sub_goal\ndelegation_targets:\n  - '*'\n",
            ),
            invocation(
                "read-only-worker",
                InvocationSurface::Task,
                FeatureMode::None,
                InvocationSourceKind::Autonomous,
            ),
            denied_baseline,
            &["shell", "files", "delete_task"],
            &[],
            &[],
            &[],
            &["worker"],
        ),
        resolve_projected_catalog(
            "chat_deferred_browser_discovery",
            definition("personal-assistant", "tools:\n  - browser\n"),
            invocation(
                "personal-assistant",
                InvocationSurface::Chat,
                FeatureMode::None,
                InvocationSourceKind::ChatInline,
            ),
            browser_full_baseline,
            chat_initial_hot.clone(),
            &["tool_search", "search_memory", "browser__open", "browser__click"],
            &[],
            &["browser__open", "browser__click"],
            &[],
            &[],
        ),
        resolve_projected_catalog(
            "chat_loaded_browser_family_action",
            definition("personal-assistant", "tools:\n  - browser\n"),
            invocation(
                "personal-assistant",
                InvocationSurface::Chat,
                FeatureMode::None,
                InvocationSourceKind::ChatInline,
            ),
            browser_loaded.clone(),
            browser_loaded.clone(),
            &["tool_search", "search_memory", "browser__open", "browser__click"],
            &[],
            &[],
            &[],
            &[],
        ),
        resolve_projected_catalog(
            "chat_current_turn_context_quality_parity",
            definition("personal-assistant", "tools:\n  - browser\n"),
            invocation(
                "personal-assistant",
                InvocationSurface::Chat,
                FeatureMode::None,
                InvocationSourceKind::ChatInline,
            ),
            browser_loaded.clone(),
            chat_initial_hot.clone(),
            &["tool_search", "search_memory", "browser__open", "browser__click"],
            &[],
            &["browser__open", "browser__click"],
            &[],
            &[],
        ),
        resolve_projected_catalog(
            "voice_deep_recall_is_initial_hot",
            definition("personal-assistant", "tools:\n  - browser\n"),
            invocation(
                "personal-assistant",
                InvocationSurface::RealtimeVoice,
                FeatureMode::None,
                InvocationSourceKind::Direct,
            ),
            voice_full_baseline.clone(),
            voice_initial_hot.clone(),
            &[
                "tool_search",
                "search_memory",
                "browser__open",
                "browser__click",
                "delegate_to_chat",
            ],
            &[],
            &["browser__open", "browser__click"],
            &[],
            &[],
        ),
        resolve_projected_catalog(
            "voice_deferred_browser_discovery",
            definition("personal-assistant", "tools:\n  - browser\n"),
            invocation(
                "personal-assistant",
                InvocationSurface::RealtimeVoice,
                FeatureMode::None,
                InvocationSourceKind::Direct,
            ),
            voice_full_baseline.clone(),
            voice_initial_hot.clone(),
            &[
                "tool_search",
                "search_memory",
                "browser__open",
                "browser__click",
                "delegate_to_chat",
            ],
            &[],
            &["browser__open", "browser__click"],
            &[],
            &[],
        ),
        resolve_projected_catalog(
            "voice_loaded_browser_family_action",
            definition("personal-assistant", "tools:\n  - browser\n"),
            invocation(
                "personal-assistant",
                InvocationSurface::RealtimeVoice,
                FeatureMode::None,
                InvocationSourceKind::Direct,
            ),
            voice_loaded.clone(),
            voice_loaded,
            &[
                "tool_search",
                "search_memory",
                "browser__open",
                "browser__click",
                "delegate_to_chat",
            ],
            &[],
            &[],
            &[],
            &[],
        ),
        resolve_projected_catalog(
            "voice_current_turn_context_precedes_response",
            definition("personal-assistant", "tools:\n  - browser\n"),
            invocation(
                "personal-assistant",
                InvocationSurface::RealtimeVoice,
                FeatureMode::None,
                InvocationSourceKind::Direct,
            ),
            voice_full_baseline,
            voice_initial_hot,
            &[
                "tool_search",
                "search_memory",
                "browser__open",
                "browser__click",
                "delegate_to_chat",
            ],
            &[],
            &["browser__open", "browser__click"],
            &[],
            &[],
        ),
        resolve_projected_catalog(
            "task_deferred_browser_discovery",
            definition("task-worker", "tools:\n  - browser\n"),
            invocation(
                "task-worker",
                InvocationSurface::Task,
                FeatureMode::None,
                InvocationSourceKind::Autonomous,
            ),
            task_full_baseline,
            task_initial_hot.clone(),
            &[
                "yield",
                "need_user_input",
                "spawn_sub_goal",
                "tool_search",
                "search_memory",
                "browser__open",
                "browser__click",
            ],
            &[],
            &["browser__open", "browser__click"],
            &[],
            &[],
        ),
        resolve_projected_catalog(
            "task_loaded_browser_family_action",
            definition("task-worker", "tools:\n  - browser\n"),
            invocation(
                "task-worker",
                InvocationSurface::Task,
                FeatureMode::None,
                InvocationSourceKind::Autonomous,
            ),
            task_loaded.clone(),
            task_loaded.clone(),
            &[
                "yield",
                "need_user_input",
                "spawn_sub_goal",
                "tool_search",
                "search_memory",
                "browser__open",
                "browser__click",
            ],
            &[],
            &[],
            &[],
            &[],
        ),
        resolve_projected_catalog(
            "task_checkpoint_context_quality_parity",
            definition("task-worker", "tools:\n  - browser\n"),
            invocation(
                "task-worker",
                InvocationSurface::Task,
                FeatureMode::None,
                InvocationSourceKind::Autonomous,
            ),
            task_loaded.clone(),
            task_initial_hot.clone(),
            &[
                "yield",
                "need_user_input",
                "spawn_sub_goal",
                "tool_search",
                "search_memory",
                "browser__open",
                "browser__click",
            ],
            &[],
            &["browser__open", "browser__click"],
            &[],
            &[],
        ),
    ];

    let export = CatalogExport {
        schema_version: 1,
        generated_by: "magician::effective_tool_policy_snapshot",
        scenarios,
    };
    serde_json::to_writer_pretty(std::io::stdout(), &export).expect("write catalog JSON");
    println!();
}

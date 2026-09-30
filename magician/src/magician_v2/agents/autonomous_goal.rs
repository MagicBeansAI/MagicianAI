//! Goal construction and trust context for autonomous agentic execution.
//!
//! This module assembles the inputs needed to run an autonomous cycle through
//! the agentic execution engine: goal prompt, trust context, tool scoping, and
//! context overrides.

use std::sync::Arc;

use tracing::warn;

use crate::magician_v2::agents::memory_tier_interpreter::MemoryTierInterpreter;
use crate::magician_v2::agents::types::{
    AgentDefinition, AgentKind, AutonomousConfig, FocusArea, TrustLevel,
};
use crate::magician_v2::agents::TrustPolicyEnforcer;
use crate::magician_v2::execution::{
    builtin_action_types::builtin_action_type_for_tool_name,
    tool_catalog_prompt::has_direct_pack_tools, AgenticContextOverrides, AutonomousPromptControls,
    PromptAgentKind, PromptIdentityContext,
};
use crate::magician_v2::orchestrator::v2_orchestrator::AgenticTrustContext;
use crate::magician_v2::query_analysis::operation_llm_router::OperationRoutingOverrides;

// ============================================================================
// Goal Construction
// ============================================================================

/// Filter focus areas to those matching the active schedule.
///
/// When `active_schedule` is `Some(expr)`, returns only focus areas whose
/// effective schedule equals `expr`. When `None`, returns all focus areas
/// (backward compat for callers that don't use per-area scheduling).
pub fn filter_focus_areas_by_schedule<'a>(
    config: &'a AutonomousConfig,
    active_schedule: Option<&str>,
) -> Vec<&'a FocusArea> {
    match active_schedule {
        Some(sched) => config
            .focus_areas
            .iter()
            .filter(|a| a.effective_schedule(&config.schedule) == sched)
            .collect(),
        None => config.focus_areas.iter().collect(),
    }
}

pub fn focus_area_goal_id(agent_id: &str, focus_area: &FocusArea) -> String {
    format!("harness:{agent_id}:{}", focus_area.goal_slug())
}

pub fn resolve_focus_area_for_goal_id<'a>(
    definition: &'a AgentDefinition,
    goal_id: &str,
) -> Option<&'a FocusArea> {
    definition
        .autonomous_config
        .as_ref()?
        .focus_areas
        .iter()
        .find(|focus_area| focus_area_goal_id(&definition.agent_id, focus_area) == goal_id)
}

pub fn resolve_focus_area_goal_description(
    definition: &AgentDefinition,
    goal_id: &str,
) -> Option<String> {
    let focus_area = resolve_focus_area_for_goal_id(definition, goal_id)?;
    let mut sections = vec![format!(
        "Focus Area: {}\n\n{}",
        focus_area.name.trim(),
        focus_area.description.trim()
    )];

    let priority = match focus_area.priority {
        crate::magician_v2::agents::types::FocusAreaPriority::Low => "low",
        crate::magician_v2::agents::types::FocusAreaPriority::Medium => "medium",
        crate::magician_v2::agents::types::FocusAreaPriority::High => "high",
    };
    sections.push(format!("Priority: {priority}"));

    if let Some(program) = focus_area
        .program
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        sections.push(format!("Program: {program}"));
    } else if let Some(program_section) = definition
        .harness
        .as_ref()
        .and_then(|config| config.program_section.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        sections.push(format!("Program Section: {program_section}"));
    }

    if let Some(scope) = focus_area.scope.as_ref().filter(|value| !value.is_empty()) {
        sections.push(format!("Scope: {}", scope.join(", ")));
    }

    Some(sections.join("\n"))
}

pub fn resolve_focus_area_task_title(
    definition: &AgentDefinition,
    goal_id: &str,
) -> Option<String> {
    resolve_focus_area_for_goal_id(definition, goal_id)
        .map(|focus_area| format!("{} / {}", definition.agent_id, focus_area.name))
}

/// Build the goal prompt for an autonomous cycle.
///
/// Assembles:
/// - Agent persona
/// - Focus areas from `autonomous_config` (filtered by `active_schedule`)
/// - Rendered memory tiers (loaded from storage)
/// - Available tools summary
///
/// When `active_schedule` is `Some(expr)`, only focus areas whose effective
/// schedule matches `expr` are included. When `None`, all focus areas are
/// included (backward compatibility).
pub async fn build_autonomous_goal(
    definition: &AgentDefinition,
    memory_interpreter: &MemoryTierInterpreter,
    // Tool catalog now travels through the function-calling tools
    // array. The prose `## Available Tools` section was removed; this
    // parameter is retained to avoid churning the (multiple) call
    // sites that still pass it. Prefixed with `_` to silence the
    // unused-variable warning.
    _merged_agent_tools: &[runtime_core::ToolInfo],
    active_schedule: Option<&str>,
) -> String {
    let mut sections = Vec::new();

    // 1. Persona
    sections.push(format!("## Identity\n\n{}", definition.persona));

    // 2. Focus areas (filtered by active schedule)
    if let Some(ref config) = definition.autonomous_config {
        let areas = filter_focus_areas_by_schedule(config, active_schedule);
        if !areas.is_empty() {
            let mut focus = String::from("## Focus Areas\n");
            for area in &areas {
                focus.push_str(&format!(
                    "\n- **{}** (priority: {:?}): {}",
                    area.name, area.priority, area.description
                ));
            }
            sections.push(focus);
        }
    }

    // 3. Rendered memory tiers (best-effort — empty on failure)
    //    Personality profile is extracted and promoted to its own top-level section
    //    so the agent's voice, expression triggers, and suppression rules are prominent.
    let goal_id = format!("{}:autonomous", definition.agent_id);
    match memory_interpreter
        .load_and_render(
            &definition.agent_id,
            Some(&goal_id),
            &definition.memory_tiers,
        )
        .await
    {
        Ok(rendered) if !rendered.is_empty() => {
            // Extract personality_profile into its own section (if present)
            let mut personality_content = None;
            let mut mem = String::from("## Memory Context\n");
            for (tier_name, content) in &rendered {
                if !content.is_empty() {
                    if tier_name == "personality_profile" {
                        personality_content = Some(content.clone());
                    } else {
                        mem.push_str(&format!("\n### {}\n{}\n", tier_name, content));
                    }
                }
            }
            // Personality gets its own top-level section before memory context
            if let Some(personality) = personality_content {
                sections.push(format!("## Personality\n\n{}", personality));
            }
            // Only add memory context if there are non-personality tiers
            if mem.len() > "## Memory Context\n".len() {
                sections.push(mem);
            }
        },
        Ok(_) => {}, // no memory content — skip
        Err(e) => {
            warn!(
                agent_id = %definition.agent_id,
                error = %e,
                "Failed to load memory tiers for autonomous goal; continuing without memory"
            );
        },
    }

    // The function-calling tools array delivers the tool catalog
    // (names, descriptions, parameter schemas) natively. The previous
    // prose `## Available Tools` section restated what was already
    // delivered through that channel and is therefore omitted — see
    // `tool_catalog_prompt` module docs for the rationale.

    // 5. Instruction
    sections.push(
        "## Instruction\n\n\
         You are running an autonomous cycle. Review your focus areas and memory context, \
         then take concrete actions to make progress. Create tasks for work that should be \
         delegated. Report completion when you have made meaningful progress or determined \
         no further action is needed this cycle."
            .to_string(),
    );

    sections.join("\n\n")
}

// `build_autonomous_tools_section` deleted — the function-calling tools
// array carries the catalog. See `tool_catalog_prompt` module docs.

// ============================================================================
// Trust Context
// ============================================================================

/// Default autonomous prompt traits applied when no override is specified.
const DEFAULT_AUTONOMOUS_TRAITS: &[&str] = &["autonomous", "proactive"];

/// Default creative latitude for autonomous cycles (0.0 = conservative, 1.0 = creative).
const DEFAULT_CREATIVE_LATITUDE: f32 = 0.5;

/// Build the `AgenticTrustContext` for an autonomous cycle.
///
/// Uses agent definition fields for trust level, policies, approval rules,
/// and observation config. The `goal_id` is static per agent (`"{agent_id}:autonomous"`),
/// while `cycle_id` is unique per execution.
pub fn build_autonomous_trust_context(
    definition: &AgentDefinition,
    cycle_id: &str,
    trust_policies_path: std::path::PathBuf,
    preloaded_trust_enforcer: Option<Arc<TrustPolicyEnforcer>>,
    llm_routing_overrides: Option<OperationRoutingOverrides>,
) -> AgenticTrustContext {
    let goal_id = format!("{}:autonomous", definition.agent_id);

    // Prompt identity — autonomous personal agent
    let prompt_identity = PromptIdentityContext {
        agent_kind: Some(PromptAgentKind::User),
        base_persona: Some(definition.persona.clone()),
        source_agent_id: Some(definition.agent_id.clone()),
        source_agent_name: Some(definition.name.trim().to_string()),
        source_agent_aliases: crate::magician_v2::presentation_identity::bounded_agent_aliases(
            &definition.aliases,
        ),
        source_agent_persona: None,
        autonomous_controls: Some(AutonomousPromptControls {
            traits: DEFAULT_AUTONOMOUS_TRAITS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            creative_latitude: Some(DEFAULT_CREATIVE_LATITUDE),
            voice: None,
        }),
    };

    AgenticTrustContext {
        trust_level: TrustLevel::canonicalized_value(&definition.trust_level.0),
        trust_policies_path,
        preloaded_trust_enforcer,
        approval_rules: crate::magician_v2::agents::approval::harness_merged_approval_rules(
            definition,
        ),
        agent_id: Some(definition.agent_id.clone()),
        goal_id: Some(goal_id),
        cycle_id: Some(cycle_id.to_string()),
        llm_routing_overrides,
        prompt_identity: Some(prompt_identity),
    }
}

// ============================================================================
// Tool Scoping
// ============================================================================

/// Map agent tools list → allowed built-in action types.
///
/// Mapping:
/// - built-in file/http/bash tool names and aliases map to their
///   canonical action lanes
/// - `browser` and `duckdb` map to the pack `tool` lane because they are now
///   inner-loop packs, not built-in action lanes
/// - Any pack capability tool → `"tool"`
///
/// Empty tools list = all action types (backward compat).
pub fn derive_allowed_action_types(
    tools: &[String],
    has_direct_capabilities: bool,
) -> Option<Vec<String>> {
    if tools.is_empty() {
        return None; // no restriction
    }

    let mut seen = std::collections::HashSet::new();
    let mut types = Vec::new();
    for tool in tools {
        let normalized = tool.trim().to_ascii_lowercase();
        let mapped = (normalized == "browser")
            .then_some("tool")
            .or_else(|| builtin_action_type_for_tool_name(tool))
            .or_else(|| has_direct_capabilities.then_some("tool"));
        if let Some(mapped) = mapped {
            if seen.insert(mapped) {
                types.push(mapped.to_string());
            }
        }
    }

    // If there are pack capabilities available, ensure "tool" is allowed
    if has_direct_capabilities && seen.insert("tool") {
        types.push("tool".to_string());
    }

    Some(types)
}

// ============================================================================
// Context Overrides
// ============================================================================

/// Build `AgenticContextOverrides` for an autonomous execution cycle.
///
/// Combines tool scoping, task limits, tenancy, and task execution context
/// from the agent definition and bootstrap task.
///
/// `merged_agent_tools` is the authoritative tool list (own + delegate, with
/// whitelist/blacklist applied), resolved by the orchestrator. It is used for
/// both planning prompts and runtime capability enforcement.
pub fn build_autonomous_overrides(
    definition: &AgentDefinition,
    principal: Option<String>,
    workspace: Option<String>,
    task_id: Option<String>,
    execution_id: Option<String>,
    api_port: Option<u16>,
    merged_agent_tools: Vec<runtime_core::ToolInfo>,
) -> AgenticContextOverrides {
    let has_direct_capabilities = has_direct_pack_tools(&merged_agent_tools);

    let max_tasks = definition
        .autonomous_config
        .as_ref()
        .map(|c| c.max_tasks_per_cycle);

    AgenticContextOverrides {
        seeded_sensitive_inputs: Vec::new(),
        plane_attenuation: None,
        run_engine_pin: None,
        server_owned_fresh_launch: false,
        server_owned_fresh_launch_nonce: None,
        server_owned_fresh_launch_updated_at: None,
        interrupted_runtime_recovery: false,
        interrupted_runtime_recovery_source_segment: None,
        interrupted_runtime_recovery_source_revision: None,
        interrupted_runtime_preseed_updated_at: None,
        interrupted_runtime_preseed_token: None,
        allowed_action_types: derive_allowed_action_types(
            &definition.tools,
            has_direct_capabilities,
        ),
        env_mode: None,
        max_spawned_tasks: max_tasks,
        pipeline_stages: None,
        delegate_single_in_context: false,
        // Autonomous cycles never run the VibeDev coding coordinator.
        coding_coordinator_run: false,
        principal,
        workspace,
        invocation_context_override: None,
        work_authority: None,
        task_id,
        execution_id,
        stateless_resume_source_segment: None,
        stateless_retry_due_at: None,
        root_execution_id: None,
        task_output_mode: None,
        api_port,
        agent_id: None, // Agent ID is set via trust context in the autonomous path
        success_criteria: None,
        llm_routing_overrides: None,
        execution_llm_routing_overrides: None,
        initial_url: None,
        merged_agent_tools,
        preserve_initial_tool_scope: false,
        denied_tool_params: definition.denied_tool_params.clone(),
        expected_artifact_declarations: Vec::new(),
        spend_token_ids: Vec::new(),
        active_owner_agent_id: None,
        owner_stack: Vec::new(),
        chat_inline: false,
        work_budget_secs: None,
        plan_mode_enabled: false,
        accept_in_scope_enabled: false,
        // Autonomous cycles run in their own execution context; the
        // chat-inline session-id propagation does not apply here.
        browser_session_id_override: None,
        // Autonomous cycles already get prompt_identity via the
        // trust_context path on `execute_agentic_direct_with_outcome`,
        // but populating here too keeps overrides self-sufficient and
        // makes the override applicable on any caller that builds an
        // autonomous override block.
        prompt_identity: Some(prompt_identity_from_definition(definition)),
        app_disclosure_guard: None,
        app_agent_tool_result_declaration: None,
    }
}

/// Build `AgenticContextOverrides` for a direct agent execution path.
///
/// This is the shared builder for direct agentic runs that already know the
/// owning agent definition and resolved merged tool surface. It keeps tool
/// scoping, allowlists, deny rules, and owner metadata aligned across chat-inline,
/// delegated child execution, and task-backed direct execution paths.
pub fn build_direct_agent_overrides(
    definition: &AgentDefinition,
    principal: Option<String>,
    workspace: Option<String>,
    task_id: Option<String>,
    execution_id: Option<String>,
    active_owner_agent_id: Option<String>,
    owner_stack: Vec<String>,
    success_criteria: Option<String>,
    initial_url: Option<String>,
    expected_artifact_declarations: Vec<crate::magician_v2::agents::types::ArtifactDeclaration>,
    spend_token_ids: Vec<String>,
    merged_agent_tools: Vec<runtime_core::ToolInfo>,
) -> AgenticContextOverrides {
    let has_direct_capabilities = has_direct_pack_tools(&merged_agent_tools);

    AgenticContextOverrides {
        seeded_sensitive_inputs: Vec::new(),
        plane_attenuation: None,
        run_engine_pin: None,
        server_owned_fresh_launch: false,
        server_owned_fresh_launch_nonce: None,
        server_owned_fresh_launch_updated_at: None,
        interrupted_runtime_recovery: false,
        interrupted_runtime_recovery_source_segment: None,
        interrupted_runtime_recovery_source_revision: None,
        interrupted_runtime_preseed_updated_at: None,
        interrupted_runtime_preseed_token: None,
        allowed_action_types: derive_allowed_action_types(
            &definition.tools,
            has_direct_capabilities,
        ),
        env_mode: None,
        max_spawned_tasks: None,
        pipeline_stages: None,
        delegate_single_in_context: false,
        // Direct (non-VibeDev-coordinator) agent runs never arm the coding coordinator.
        coding_coordinator_run: false,
        principal,
        workspace,
        invocation_context_override: None,
        work_authority: None,
        task_id,
        execution_id,
        stateless_resume_source_segment: None,
        stateless_retry_due_at: None,
        root_execution_id: None,
        task_output_mode: None,
        api_port: None,
        agent_id: Some(definition.agent_id.clone()),
        success_criteria,
        llm_routing_overrides: definition
            .llm_routing
            .as_ref()
            .map(OperationRoutingOverrides::from_llm_routing_config)
            .and_then(OperationRoutingOverrides::normalized),
        execution_llm_routing_overrides: None,
        initial_url,
        merged_agent_tools,
        preserve_initial_tool_scope: false,
        denied_tool_params: definition.denied_tool_params.clone(),
        expected_artifact_declarations,
        spend_token_ids,
        active_owner_agent_id: active_owner_agent_id.or_else(|| Some(definition.agent_id.clone())),
        owner_stack,
        chat_inline: false,
        work_budget_secs: None,
        plan_mode_enabled: false,
        accept_in_scope_enabled: false,
        // Direct (non-chat) executions get a fresh per-execution
        // browser session. Chat-inline callers patch this in via the
        // ChatInline branch in `runtime.rs` after `build_direct_agent_overrides`.
        browser_session_id_override: None,
        // Carry the agent's persona into the override so the chat-inline
        // path (which calls `execute_agentic_direct_with_outcome` with
        // `trust_context: None`) doesn't lose it.
        // `prompt_identity_for_agent_definition` lives in v2_orchestrator,
        // not here, so we construct the equivalent inline to avoid the
        // dependency cycle (agents → orchestrator → agents).
        prompt_identity: Some(prompt_identity_from_definition(definition)),
        app_disclosure_guard: None,
        app_agent_tool_result_declaration: None,
    }
}

/// Construct a `PromptIdentityContext` from an `AgentDefinition`. Same
/// shape as `v2_orchestrator::prompt_identity_for_agent_definition` —
/// duplicated here to avoid an agents→orchestrator dependency cycle.
pub fn prompt_identity_from_definition(definition: &AgentDefinition) -> PromptIdentityContext {
    let base_persona = if definition.persona.trim().is_empty() {
        None
    } else {
        Some(definition.persona.trim().to_string())
    };
    let kind = Some(match definition.kind {
        AgentKind::Personal => PromptAgentKind::User,
        _ => PromptAgentKind::System,
    });
    PromptIdentityContext {
        agent_kind: kind,
        base_persona: base_persona.clone(),
        source_agent_id: Some(definition.agent_id.clone()),
        source_agent_name: Some(definition.name.trim().to_string()),
        source_agent_aliases: crate::magician_v2::presentation_identity::bounded_agent_aliases(
            &definition.aliases,
        ),
        source_agent_persona: base_persona,
        autonomous_controls: None,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::agents::types::FocusAreaPriority;

    // -----------------------------------------------------------------------
    // filter_focus_areas_by_schedule tests
    // -----------------------------------------------------------------------

    fn make_config_with_areas() -> AutonomousConfig {
        AutonomousConfig {
            schedule: "0 */4 * * *".to_string(),
            focus_areas: vec![
                FocusArea {
                    name: "hourly".to_string(),
                    description: "Runs hourly".to_string(),
                    priority: FocusAreaPriority::High,
                    schedule: Some("0 * * * *".to_string()),
                    program: None,
                    scope: None,
                },
                FocusArea {
                    name: "weekly".to_string(),
                    description: "Runs weekly".to_string(),
                    priority: FocusAreaPriority::Medium,
                    schedule: Some("0 2 * * 1".to_string()),
                    program: None,
                    scope: None,
                },
                FocusArea {
                    name: "default".to_string(),
                    description: "Uses top-level schedule".to_string(),
                    priority: FocusAreaPriority::Low,
                    schedule: None,
                    program: None,
                    scope: None,
                },
            ],
            max_tasks_per_cycle: 3,
            max_steps_per_plan: 10,
        }
    }

    #[test]
    fn test_filter_none_returns_all() {
        let config = make_config_with_areas();
        let areas = filter_focus_areas_by_schedule(&config, None);
        assert_eq!(areas.len(), 3);
    }

    #[test]
    fn test_filter_by_hourly_schedule() {
        let config = make_config_with_areas();
        let areas = filter_focus_areas_by_schedule(&config, Some("0 * * * *"));
        assert_eq!(areas.len(), 1);
        assert_eq!(areas[0].name, "hourly");
    }

    #[test]
    fn test_filter_by_top_level_schedule() {
        let config = make_config_with_areas();
        let areas = filter_focus_areas_by_schedule(&config, Some("0 */4 * * *"));
        assert_eq!(areas.len(), 1);
        assert_eq!(areas[0].name, "default");
    }

    #[test]
    fn test_filter_by_nonexistent_schedule() {
        let config = make_config_with_areas();
        let areas = filter_focus_areas_by_schedule(&config, Some("0 0 * * *"));
        assert!(areas.is_empty());
    }

    #[test]
    fn test_focus_area_goal_id_uses_harness_prefix_and_slug() {
        let area = FocusArea {
            name: "Morning Briefing".to_string(),
            description: "Daily CEO briefing".to_string(),
            priority: FocusAreaPriority::High,
            schedule: None,
            program: None,
            scope: None,
        };
        assert_eq!(
            focus_area_goal_id("ceo", &area),
            "harness:ceo:morning-briefing"
        );
    }

    #[test]
    fn test_resolve_focus_area_goal_description_uses_focus_area_and_harness_context() {
        let definition = AgentDefinition {
            // Empty: every transport. The restriction is opt-in.
            browser_transports: Vec::new(),
            agent_id: "ceo".to_string(),
            version: 1,
            name: "CEO".to_string(),
            aliases: Vec::new(),
            wake_spellings: Vec::new(),
            description: String::new(),
            app_tool: None,
            persona: "Lead the company".to_string(),
            kind: crate::magician_v2::agents::types::AgentKind::Personal,
            disabled: false,
            tools: Vec::new(),
            excluded_tools: Vec::new(),
            denied_tools: Vec::new(),
            denied_tool_params: std::collections::HashMap::new(),
            constraints: Default::default(),
            trust_level: Default::default(),
            memory_tiers: Vec::new(),
            memory_consolidation: Vec::new(),
            prompt_pipeline: None,
            circuit_breaker: None,
            feedback_loops: Vec::new(),
            notification_rules: Vec::new(),
            retention: None,
            llm_routing: None,
            strategy: None,
            state_machines: std::collections::HashMap::new(),
            principal: Some("principal".to_string()),
            workspace: Some("workspace".to_string()),
            autonomous_config: Some(AutonomousConfig {
                schedule: "0 */4 * * *".to_string(),
                focus_areas: vec![FocusArea {
                    name: "Morning Briefing".to_string(),
                    description: "Summarize the previous day and set next actions.".to_string(),
                    priority: FocusAreaPriority::High,
                    schedule: None,
                    program: None,
                    scope: Some(vec!["*".to_string()]),
                }],
                max_tasks_per_cycle: 3,
                max_steps_per_plan: 10,
            }),
            harness: Some(crate::magician_v2::agents::types::HarnessConfig {
                program_section: Some("company".to_string()),
            }),
            is_primary: false,
            onboarding_completed: false,
            readable_agents: Vec::new(),
            default_personality: None,
            user_memory_isolation: Default::default(),
            delegation_targets: Vec::new(),
            invocation_policy: Default::default(),
            auto_surface_policy: None,
            chat_inline: None,
            social_persona: None,
        };

        let goal_id = "harness:ceo:morning-briefing";
        let description =
            resolve_focus_area_goal_description(&definition, goal_id).expect("goal description");
        assert!(description.contains("Focus Area: Morning Briefing"));
        assert!(description.contains("Program Section: company"));
        assert!(description.contains("Scope: *"));
    }

    // -----------------------------------------------------------------------
    // derive_allowed_action_types tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_derive_allowed_action_types_empty_tools() {
        assert_eq!(derive_allowed_action_types(&[], false), None);
    }

    #[test]
    fn test_derive_allowed_action_types_browser_only() {
        let tools = vec!["browser".to_string()];
        let result = derive_allowed_action_types(&tools, false).unwrap();
        assert_eq!(result, vec!["tool"]);
    }

    #[test]
    fn test_derive_allowed_action_types_deduplicates() {
        let tools = vec!["browser".to_string(), "websearch".to_string()];
        let result = derive_allowed_action_types(&tools, true).unwrap();
        assert_eq!(result, vec!["tool"]);
    }

    #[test]
    fn test_derive_allowed_action_types_with_capabilities() {
        let tools = vec!["browser".to_string(), "bash".to_string()];
        let result = derive_allowed_action_types(&tools, true).unwrap();
        assert!(result.contains(&"bash".to_string()));
        assert!(result.contains(&"tool".to_string()));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_derive_allowed_action_types_maps_duckdb_to_pack_tool_lane() {
        let tools = vec!["duckdb".to_string()];
        let result = derive_allowed_action_types(&tools, true).unwrap();
        assert_eq!(result, vec!["tool"]);
    }

    #[test]
    fn test_derive_allowed_action_types_unknown_tool_adds_tool_type() {
        let tools = vec!["jq".to_string()];
        let result = derive_allowed_action_types(&tools, true).unwrap();
        assert_eq!(result, vec!["tool"]);
    }

    #[test]
    fn test_derive_allowed_action_types_unknown_tool_without_direct_capabilities_skips_tool() {
        let tools = vec!["jq".to_string()];
        let result = derive_allowed_action_types(&tools, false).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn direct_agent_overrides_preserve_scoped_operation_routing() {
        let definition: AgentDefinition = serde_yaml::from_str(
            r#"
agent_id: web-researcher
name: Web Researcher
persona: Research efficiently.
llm_routing:
  operations:
    agentic_decision:
      profile: gpt6luna-responses-toolsany
"#,
        )
        .expect("parse agent definition");

        let overrides = build_direct_agent_overrides(
            &definition,
            Some("anonymous".to_string()),
            Some("default".to_string()),
            Some("task-1".to_string()),
            Some("execution-1".to_string()),
            None,
            Vec::new(),
            None,
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );

        let routing = overrides
            .llm_routing_overrides
            .expect("direct execution must carry scoped LLM routing");
        assert_eq!(
            routing
                .operations
                .get("agentic_decision")
                .and_then(|endpoint| endpoint.profile_name()),
            Some("gpt6luna-responses-toolsany")
        );
    }

    // `test_build_autonomous_tools_section_uses_shared_compact_catalog`
    // removed: the section it covered (`## Available Tools` prose menu)
    // is gone. The tool catalog now travels exclusively through the
    // function-calling tools array; see `tool_catalog_prompt` module
    // docs for the rationale.
}

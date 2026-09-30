//! Definition-driven harness registration (plan workstream 3.5,
//! docs/plans/2026-08-26-platform-layering-and-app-extraction-plan.md).
//!
//! What a definition's `harness:` block implies used to be encoded at the
//! call sites that gate on it: `v2_orchestrator` hardcoded the injected
//! tool list behind a `kind == Personal && harness.is_some()` check, and
//! `artifact_v2::service` hardcoded the learning-reflection enrichment
//! behind `harness.is_some()`. This module is that gating as ONE
//! registration the definition drives — the officer roster
//! (`magician_data_v3/system/agent_templates/agents/{ceo,cto,cmo,cpo,cro,
//! harness-sre}` + the scope-seeded programs such as
//! `programs/harness_reliability.md`) is pure data that opts in through
//! its definition fields (`kind: personal` + a `harness:` block), with no
//! core special-casing left at the consumption sites. The harness STORES
//! (episode/trace/backlog/anomaly/program state) stay core Layer 1; only
//! the gating is definition-driven.
//!
//! Behavior is identical to the pre-3.5 inline gates for every existing
//! definition. The registration is additive by construction: a definition
//! without a harness block (or a non-personal agent, which validation
//! already forbids from carrying one) gets [`HarnessToolGrant::None`] —
//! harness tools stripped — exactly as before; a personal definition with
//! a `harness:` block gets the full coordinator registration. Narrower
//! per-definition registrations extend HERE (on `HarnessConfig`) when a
//! field-driven form is needed — the call sites below never regain
//! harness-specific conditionals.

use crate::magician_v2::agents::types::{AgentDefinition, AgentKind};

use super::HARNESS_TOOL_NAMES;

/// The owner-escalation handler injected alongside the coordinator tool
/// registration. It migrated out of `HARNESS_TOOL_NAMES` to a plain
/// compiled handler (it only emits a UserRequest), but harness
/// coordinators still get it auto-injected so their owner-escalation path
/// is unchanged. The envoy-specific ask_owner / propose_meeting /
/// request_owner_action are NOT part of any registration here — they are
/// stranger-facing and granted only to envoy.
pub const COORDINATOR_ESCALATION_TOOL_NAME: &str = "notify_owner";

/// The harness tool registration one definition's `harness:` block drives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessToolGrant {
    /// The full coordinator registration: every `HARNESS_TOOL_NAMES` tool
    /// plus the owner-escalation handler above, unioned onto whatever the
    /// definition's own `tools:` allowlist already granted (and still
    /// subject to `excluded_tools`/`denied_tools`).
    Coordinator,
    /// No harness registration: harness tools are stripped from the
    /// definition's visible surface.
    None,
}

impl HarnessToolGrant {
    /// Whether this definition carries a harness registration at all.
    pub fn is_registered(&self) -> bool {
        matches!(self, Self::Coordinator)
    }

    /// The tool names this registration injects.
    pub fn tool_names(&self) -> Vec<String> {
        match self {
            Self::Coordinator => HARNESS_TOOL_NAMES
                .iter()
                .map(|name| (*name).to_string())
                .chain(std::iter::once(
                    COORDINATOR_ESCALATION_TOOL_NAME.to_string(),
                ))
                .collect(),
            Self::None => Vec::new(),
        }
    }

    /// Whether a given tool name belongs to this registration.
    pub fn grants(&self, tool_name: &str) -> bool {
        match self {
            Self::Coordinator => {
                HARNESS_TOOL_NAMES.contains(&tool_name)
                    || tool_name == COORDINATOR_ESCALATION_TOOL_NAME
            },
            Self::None => false,
        }
    }
}

/// The harness tool registration a definition drives: personal agents with
/// a `harness:` block ride the coordinator registration; everyone else
/// (non-personal agents — which validation forbids from carrying a harness
/// block anyway — and harness-less definitions) gets none and has harness
/// tools stripped.
pub fn harness_tool_grant_for_definition(definition: &AgentDefinition) -> HarnessToolGrant {
    if definition.kind == AgentKind::Personal && definition.harness.is_some() {
        HarnessToolGrant::Coordinator
    } else {
        HarnessToolGrant::None
    }
}

/// Whether terminal-execution learning reflection should enrich its
/// context with the harness program document and its runtime state
/// (`ProgramLoader`). Driven by the same `harness:` block — the pre-3.5
/// gate was `definition.harness.is_some()`, unchanged.
pub fn reflects_program_runtime_state(definition: &AgentDefinition) -> bool {
    definition.harness.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(kind: AgentKind, harness: bool) -> AgentDefinition {
        // `read_trace` keeps the worker variant valid (workers must declare
        // explicit tools).
        let yaml = if harness {
            format!(
                "agent_id: \"a\"\nname: \"A\"\npersona: \"p\"\nkind: \"{kind}\"\ntools:\n  - read_trace\nharness: {{}}\n"
            )
        } else {
            format!(
                "agent_id: \"a\"\nname: \"A\"\npersona: \"p\"\nkind: \"{kind}\"\ntools:\n  - read_trace\n"
            )
        };
        AgentDefinition::from_yaml_str(&yaml).expect("definition should parse")
    }

    #[test]
    fn personal_agents_with_a_harness_block_ride_the_coordinator_registration() {
        let grant = harness_tool_grant_for_definition(&definition(AgentKind::Personal, true));
        assert_eq!(grant, HarnessToolGrant::Coordinator);
        assert!(grant.is_registered());
        // The registration is the pre-3.5 injected list verbatim: every
        // HARNESS_TOOL_NAMES tool plus the notify_owner escalation handler.
        let names = grant.tool_names();
        for name in HARNESS_TOOL_NAMES {
            assert!(names.contains(&(*name).to_string()), "missing {name}");
        }
        assert!(names.contains(&COORDINATOR_ESCALATION_TOOL_NAME.to_string()));
        assert_eq!(names.len(), HARNESS_TOOL_NAMES.len() + 1);
        assert!(grant.grants("create_task"));
        assert!(grant.grants(COORDINATOR_ESCALATION_TOOL_NAME));
        // Reflection enrichment is driven by the same block.
        assert!(reflects_program_runtime_state(&definition(
            AgentKind::Personal,
            true
        )));
    }

    #[test]
    fn harness_less_definitions_get_no_registration() {
        for kind in [AgentKind::Personal, AgentKind::Worker] {
            let grant = harness_tool_grant_for_definition(&definition(kind.clone(), false));
            assert_eq!(grant, HarnessToolGrant::None, "kind {kind}");
            assert!(!grant.is_registered());
            assert!(grant.tool_names().is_empty());
            assert!(!grant.grants("create_task"));
            assert!(!grant.grants("list_agents"));
            assert!(!reflects_program_runtime_state(&definition(
                kind.clone(),
                false
            )));
        }
    }

    #[test]
    fn non_personal_agents_never_ride_the_harness_registration() {
        // Validation rejects worker definitions that carry a harness block;
        // the registration is fail-closed for them anyway.
        let mut worker = definition(AgentKind::Personal, true);
        worker.kind = AgentKind::Worker;
        let grant = harness_tool_grant_for_definition(&worker);
        assert_eq!(grant, HarnessToolGrant::None);
        assert!(!grant.is_registered());
    }
}

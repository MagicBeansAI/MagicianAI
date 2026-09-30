//! AgentSkills v1 loader and activation.
//!
//! Magician adopts the AgentSkills v1 spec (https://agentskills.io/specification)
//! verbatim. Our extensions live exclusively in `metadata.magician.*` per the
//! spec's free-form metadata convention.
//!
//! The runtime branches on which `metadata.magician.*` block is present:
//! - `metadata.magician.personality` block → personality-mode (writes into the
//!   `personality_profile` memory tier when activated via `switch_personality`).
//! - No block (default) → procedure (LLM-pickable from the agent's tool
//!   catalog; activation injects body and registers `scripts/` as ephemeral
//!   tools, prepends `bin/` to PATH).
//!
//! Agent definitions are intentionally NOT skills — they live as YAML under
//! `magician_data_v3/system/agent_templates/agents/<name>/definition.agent.yaml`.
//! The portable-skill format only carries ~6 of the ~50 fields an agent
//! definition needs (memory tiers, consolidation pipelines, prompt pipeline,
//! circuit breaker, state machines, …), so squeezing them in would obscure
//! more than it would unify.
//!
//! See `docs/components/magician/skills-spec.md` for the full schema and loader
//! contract.

pub mod activation;
pub mod active;
pub mod catalog;
pub mod deps;
pub mod embedded_extensions;
pub mod loader;
pub mod manifest;
pub mod path_rewrite;
pub mod personality_mode;
pub mod runner;
pub mod scope_loader;
pub mod scripts;

pub use activation::{activate_procedure_skill, EphemeralTool, ScriptRuntime, SkillActivation};
pub use active::{
    active_procedure_skill_tier_def, read_active_procedure_skill_from_tier,
    read_active_procedure_skill_name_from_tier, resolve_and_activate_procedure_skill,
    resolve_colocated_procedure_skill_for_pack, ActivateSkillError, ActivationOutcome,
    ActiveProcedureSkill,
};
pub use catalog::{build_skill_descriptors, SkillDescriptor};
pub use loader::SkillLoader;
pub use manifest::{
    InferredKind, MagicianMetadata, PersonalitySpec, SkillManifest, SkillMetadata, SkillRequires,
};
pub use personality_mode::{
    list_personality_mode_names, lookup_personality_mode, personality_spec_to_fields,
};
pub use runner::{run_ephemeral, RunContext, RunOutput};
pub use scope_loader::{
    agent_procedure_skill_catalog, discover_procedure_skills_for_scope,
    procedure_tool_infos_for_agent, resolve_skill_name, skill_manifest_to_tool_info,
};

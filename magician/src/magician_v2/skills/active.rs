//! Active-procedure-skill state shared by chat + agentic outer loop.
//!
//! Holds the *body* of the currently-active procedure playbook so prompt
//! builders can re-inject it into every turn / iteration without re-loading
//! from disk. Single-active invariant — replacing or clearing happens through
//! `activate_procedure_skill_by_name` / `clear`.
//!
//! Persistence (Phase 0.8c-7+):
//! Both chat and autonomous now read the active-skill *name* from the
//! `active_procedure_skill` agent-scope memory tier. The skill body is
//! re-resolved from disk on every prompt build, so a SKILL.md edit takes
//! effect on the next turn without re-activation. The compiled handlers
//! `activate_skill` / `deactivate_skill` are the only writers.

use anyhow::Result;
use std::collections::BTreeMap;
use std::path::Path;

use crate::magician_v2::agents::{
    AgentMemoryService, MemoryTierDefinition, RenderConfig, RetentionMode, TierScope,
};

use super::activation::{activate_procedure_skill, SkillActivation};
use super::loader::SkillLoader;
use super::manifest::InferredKind;
use super::scope_loader::{agent_procedure_skill_catalog, resolve_skill_name};

/// Canonical definition of the `active_procedure_skill` agent-scope memory
/// tier. Shared by the `activate_skill` / `deactivate_skill` compiled
/// handlers and the prompt-builder readers so writer + reader agree on
/// scope, retention, and storage format.
///
/// Synthetic by design: agents don't need to declare this tier in their
/// YAML — it's an always-on rail for procedure-skill activation.
pub fn active_procedure_skill_tier_def() -> MemoryTierDefinition {
    MemoryTierDefinition {
        name: "active_procedure_skill".to_string(),
        scope: TierScope::Agent,
        description: "Currently-active procedure-skill playbook for this agent (set by activate_skill / cleared by deactivate_skill). Prompt builders re-read this tier each turn and re-resolve the skill body from disk.".to_string(),
        schema: BTreeMap::new(),
        render: RenderConfig {
            format: "json".to_string(),
            template: String::new(),
        },
        retention: RetentionMode::Forever,
    }
}

/// Read the active-skill *name* from the memory tier. Returns `None`
/// when no record exists, the record has no `name` field, or the name is
/// empty.
pub async fn read_active_procedure_skill_name_from_tier(
    memory_service: &AgentMemoryService,
    agent_id: &str,
) -> Option<String> {
    let tier_def = active_procedure_skill_tier_def();
    let record = memory_service
        .load_native_tier(agent_id, &tier_def, None)
        .await
        .ok()
        .flatten()?;
    record
        .fields
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// Read the active skill from the memory tier and re-resolve its body
/// from disk. Returns `None` when no skill is active, the skill no
/// longer exists on disk, discovery fails, or the body is empty. Callers
/// log + skip on `None` rather than fail the prompt build.
pub async fn read_active_procedure_skill_from_tier(
    memory_service: &AgentMemoryService,
    agent_id: &str,
    workspace_skills_dir: &Path,
) -> Option<ActiveProcedureSkill> {
    let name = read_active_procedure_skill_name_from_tier(memory_service, agent_id).await?;
    let mut search: Vec<std::path::PathBuf> = vec![workspace_skills_dir.to_path_buf()];
    search.extend(crate::magician_v2::config_extras::extra_skills_dirs());
    let manifests = SkillLoader::new(search).discover().ok()?;
    let resolved = resolve_skill_name(&name, &manifests)?;
    let manifest = manifests.iter().find(|m| m.name == resolved)?;
    if manifest.inferred_kind() != InferredKind::Procedure {
        return None;
    }
    let body = manifest.body().ok()?.trim().to_string();
    if body.is_empty() {
        return None;
    }
    Some(ActiveProcedureSkill {
        name: manifest.name.clone(),
        body,
    })
}

/// Currently-active procedure playbook for an execution / session.
///
/// `name` is the canonical (kebab-case) skill name. `body` is the skill's
/// SKILL.md body — injected into the next prompt build as the
/// `## ACTIVE PROCEDURE PLAYBOOK` section.
#[derive(Debug, Clone)]
pub struct ActiveProcedureSkill {
    pub name: String,
    pub body: String,
}

/// Outcome of an activation attempt — distinguishes "first activation",
/// "replaced previous", and "idempotent re-activation" so the dispatcher can
/// surface the right tool-result message back to the LLM.
#[derive(Debug, Clone)]
pub enum ActivationOutcome {
    First {
        skill: ActiveProcedureSkill,
        activation: SkillActivation,
    },
    Replaced {
        skill: ActiveProcedureSkill,
        previous: String,
        activation: SkillActivation,
    },
    Idempotent {
        skill: ActiveProcedureSkill,
        activation: SkillActivation,
    },
}

impl ActivationOutcome {
    pub fn skill(&self) -> &ActiveProcedureSkill {
        match self {
            Self::First { skill, .. }
            | Self::Replaced { skill, .. }
            | Self::Idempotent { skill, .. } => skill,
        }
    }
}

/// Errors that can arise during the resolve + allowlist + activate path.
#[derive(Debug, thiserror::Error)]
pub enum ActivateSkillError {
    #[error("skill discovery failed: {0}")]
    Discovery(#[source] anyhow::Error),
    #[error("skill '{name}' not found in workspace skills layer or any configured `paths` extras")]
    NotFound {
        name: String,
        available_procedures: Vec<String>,
    },
    #[error(
        "skill '{name}' is a personality-mode; use switch_personality(mode=\"{name}\") instead"
    )]
    PersonalityMode { name: String },
    #[error(
        "skill '{resolved_name}' is not in agent `{agent_id}`'s tools allowlist; \
         add it to the agent definition first"
    )]
    NotAllowlisted {
        resolved_name: String,
        agent_id: String,
        allowed_procedure_skills: Vec<String>,
    },
    #[error("activation failed: {0}")]
    Activation(#[source] anyhow::Error),
}

/// Resolve a skill by name (handling legacy aliases) against the agent's
/// allowlist and activate it. Returns the activation outcome — caller is
/// responsible for storing the new `ActiveProcedureSkill` somewhere the next
/// prompt build will see it.
///
/// `previous_active_name` is the *current* active skill's name (if any). Used
/// only to classify the outcome as `Idempotent` / `Replaced` / `First`.
///
/// The same resolve + allowlist logic powers chat's `activate_skill` tool —
/// keep them aligned. Chat additionally persists to `chat_store`; this helper
/// stays storage-agnostic.
pub fn resolve_and_activate_procedure_skill(
    requested_name: &str,
    agent_id: &str,
    agent_tools_allowlist: &[String],
    workspace_skills_dir: &Path,
    previous_active_name: Option<&str>,
) -> Result<ActivationOutcome, ActivateSkillError> {
    let mut skill_search: Vec<std::path::PathBuf> = vec![workspace_skills_dir.to_path_buf()];
    skill_search.extend(crate::magician_v2::config_extras::extra_skills_dirs());

    let manifests = SkillLoader::new(skill_search)
        .discover()
        .map_err(ActivateSkillError::Discovery)?;

    let resolved = resolve_skill_name(requested_name, &manifests).map(str::to_string);
    let Some(manifest) = resolved
        .as_deref()
        .and_then(|resolved_name| manifests.iter().find(|m| m.name == resolved_name))
    else {
        let available: Vec<String> = manifests
            .iter()
            .filter(|m| m.inferred_kind() == InferredKind::Procedure)
            .map(|m| m.name.clone())
            .collect();
        return Err(ActivateSkillError::NotFound {
            name: requested_name.to_string(),
            available_procedures: available,
        });
    };

    if manifest.inferred_kind() == InferredKind::PersonalityMode {
        return Err(ActivateSkillError::PersonalityMode {
            name: requested_name.to_string(),
        });
    }

    let allowed_catalog =
        agent_procedure_skill_catalog(workspace_skills_dir, agent_tools_allowlist);
    let resolved_name = manifest.name.clone();
    if !allowed_catalog.iter().any(|(n, _)| n == &resolved_name) {
        let allowed_names: Vec<String> = allowed_catalog.iter().map(|(n, _)| n.clone()).collect();
        return Err(ActivateSkillError::NotAllowlisted {
            resolved_name,
            agent_id: agent_id.to_string(),
            allowed_procedure_skills: allowed_names,
        });
    }

    let activation = activate_procedure_skill(manifest).map_err(ActivateSkillError::Activation)?;

    let skill = ActiveProcedureSkill {
        name: activation.skill_name.clone(),
        body: activation.steering_message.clone(),
    };

    Ok(match previous_active_name {
        Some(prev) if prev == skill.name => ActivationOutcome::Idempotent { skill, activation },
        Some(prev) => ActivationOutcome::Replaced {
            skill,
            previous: prev.to_string(),
            activation,
        },
        None => ActivationOutcome::First { skill, activation },
    })
}

/// Resolve a procedure skill colocated with a tool pack of the same name
/// and return it as an `ActiveProcedureSkill`. Used to auto-surface a
/// pack's `SKILL.md` playbook to the inner loop when the outer loop did
/// not explicitly `activate_skill` it.
///
/// Unlike [`resolve_and_activate_procedure_skill`], this path:
/// - **skips the agent-tools allowlist check** — the agent is already
///   authorised to dispatch the pack; rendering the pack's own
///   documentation does not widen capability surface.
/// - **skips `requires.bins` check** — we only render the body; the
///   `scripts/` + `bin/` activation belongs to explicit `activate_skill`
///   flows.
/// - **silently returns `None`** when no colocated `SKILL.md` exists or
///   discovery fails. The inner loop falls back to seeing only the
///   schema descriptions, exactly like today.
///
/// Discovery uses the same search paths as the explicit-activation
/// helper: the workspace skills dir plus every entry in
/// `extra_skills_dirs()`. Matching is by `manifest.name == pack_name`.
///
/// Returns `None` for personality-mode skills (they're not procedures)
/// and for skills with an empty body.
pub fn resolve_colocated_procedure_skill_for_pack(
    pack_name: &str,
    workspace_skills_dir: &Path,
) -> Option<ActiveProcedureSkill> {
    let mut search: Vec<std::path::PathBuf> = vec![workspace_skills_dir.to_path_buf()];
    search.extend(crate::magician_v2::config_extras::extra_skills_dirs());
    let manifests = match SkillLoader::new(search).discover() {
        Ok(m) => m,
        Err(err) => {
            tracing::warn!(
                target: "skills.auto_couple",
                pack = pack_name,
                error = %err,
                "skill discovery failed during auto-couple; falling back to no playbook"
            );
            return None;
        },
    };
    let manifest = manifests
        .iter()
        .find(|m| m.name == pack_name && m.inferred_kind() == InferredKind::Procedure)?;
    let body = match manifest.body() {
        Ok(body) => body.trim().to_string(),
        Err(err) => {
            tracing::warn!(
                target: "skills.auto_couple",
                pack = pack_name,
                source_dir = %manifest.source_dir.display(),
                error = %err,
                "failed to read colocated SKILL.md body; falling back to no playbook"
            );
            return None;
        },
    };
    if body.is_empty() {
        return None;
    }
    Some(ActiveProcedureSkill {
        name: manifest.name.clone(),
        body,
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod auto_couple_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_skill(parent: &Path, name: &str, body: &str) {
        let p = parent.join(name);
        fs::create_dir_all(&p).unwrap();
        fs::write(
            p.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: d\n---\n{body}"),
        )
        .unwrap();
    }

    #[test]
    fn returns_none_when_no_skill_md_exists() {
        let dir = TempDir::new().unwrap();
        let result = resolve_colocated_procedure_skill_for_pack("metabase", dir.path());
        assert!(result.is_none());
    }

    #[test]
    fn surfaces_skill_when_colocated_in_workspace_dir() {
        let dir = TempDir::new().unwrap();
        write_skill(
            dir.path(),
            "metabase",
            "# Metabase playbook\nUse find_search first.",
        );
        let result = resolve_colocated_procedure_skill_for_pack("metabase", dir.path()).unwrap();
        assert_eq!(result.name, "metabase");
        assert!(result.body.contains("find_search"));
    }

    #[test]
    fn ignores_personality_mode_skills() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("witty");
        fs::create_dir_all(&p).unwrap();
        fs::write(
            p.join("SKILL.md"),
            "---\nname: witty\ndescription: d\n\
             metadata:\n  magician:\n    personality:\n      active_mode: witty\n      voice: x\n---\n# body\n",
        )
        .unwrap();
        assert!(resolve_colocated_procedure_skill_for_pack("witty", dir.path()).is_none());
    }

    #[test]
    fn returns_none_when_body_is_empty() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("emptypack");
        fs::create_dir_all(&p).unwrap();
        fs::write(
            p.join("SKILL.md"),
            "---\nname: emptypack\ndescription: d\n---\n   \n",
        )
        .unwrap();
        assert!(resolve_colocated_procedure_skill_for_pack("emptypack", dir.path()).is_none());
    }

    #[test]
    fn pack_name_mismatch_returns_none() {
        let dir = TempDir::new().unwrap();
        write_skill(dir.path(), "metabase", "# body");
        assert!(resolve_colocated_procedure_skill_for_pack("postgres", dir.path()).is_none());
    }
}

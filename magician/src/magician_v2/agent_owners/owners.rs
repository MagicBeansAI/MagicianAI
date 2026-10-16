//! Cataloged Task 14 agent, memory, learning, and index owners.

use std::path::{Path, PathBuf};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentOwner {
    AgentDefinitions,
    AgentRuntime,
    MemoryCanonical,
    MemoryIndex,
    LearningProcedures,
    LearningProcedureIndex,
    LearningHistories,
    SkillEvolution,
}

impl AgentOwner {
    pub const ALL: [AgentOwner; 8] = [
        Self::AgentDefinitions,
        Self::MemoryIndex,
        Self::MemoryCanonical,
        Self::LearningProcedureIndex,
        Self::LearningProcedures,
        Self::LearningHistories,
        Self::SkillEvolution,
        Self::AgentRuntime,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::AgentDefinitions => "agent_definitions",
            Self::AgentRuntime => "agent_runtime",
            Self::MemoryCanonical => "memory_canonical",
            Self::MemoryIndex => "memory_index",
            Self::LearningProcedures => "learning_procedures",
            Self::LearningProcedureIndex => "learning_procedure_index",
            Self::LearningHistories => "learning_histories",
            Self::SkillEvolution => "skill_evolution",
        }
    }

    pub fn sample_rel(self) -> &'static str {
        match self {
            Self::AgentDefinitions => "agent_runtime/agents/agent-1/definition.agent.yaml",
            Self::AgentRuntime => "agent_runtime/agents/agent-1/state/status.json",
            Self::MemoryCanonical => "memory/agents/agent-1/tiers/core.json",
            Self::MemoryIndex => "memory/index/manifest.json",
            Self::LearningProcedures => "learning/procedures/active/proc-1.yaml",
            Self::LearningProcedureIndex => "learning/procedures/index/manifest.json",
            Self::LearningHistories => "learning/events/evt-1.json",
            Self::SkillEvolution => "skill_evolution/proposals/p-1.json",
        }
    }

    pub fn allows(self, rel: &str) -> bool {
        if rel.starts_with('/') || Path::new(rel).is_absolute() {
            return false;
        }
        let parts: Vec<&str> = rel.split('/').filter(|part| !part.is_empty()).collect();
        if parts.is_empty()
            || parts
                .iter()
                .any(|part| *part == "." || *part == ".." || part.contains('\0'))
        {
            return false;
        }
        match self {
            Self::AgentDefinitions => definition_record(&parts),
            Self::AgentRuntime => runtime_record(&parts),
            Self::MemoryCanonical => memory_canonical(&parts),
            Self::MemoryIndex => prefix(&parts, &["memory", "index"], 3),
            Self::LearningProcedures => {
                prefix(&parts, &["learning", "procedures"], 3) && parts[2] != "index"
            },
            Self::LearningProcedureIndex => prefix(&parts, &["learning", "procedures", "index"], 4),
            Self::LearningHistories => prefix(&parts, &["learning"], 2) && parts[1] != "procedures",
            Self::SkillEvolution => {
                prefix(&parts, &["skill_evolution"], 2) || leftover_skill_evolution(&parts)
            },
        }
    }

    pub fn claiming(rel: &str) -> Option<AgentOwner> {
        Self::ALL.into_iter().find(|owner| owner.allows(rel))
    }

    pub fn root(
        self,
        workspace: &ArtifactV2Workspace,
        principal: &str,
        workspace_name: &str,
    ) -> PathBuf {
        workspace.scope_root(principal, workspace_name)
    }
}

fn prefix(parts: &[&str], head: &[&str], min_len: usize) -> bool {
    parts.len() >= min_len && parts.get(..head.len()) == Some(head)
}

fn leftover_skill_evolution(parts: &[&str]) -> bool {
    prefix(parts, &["capability_evolution"], 2)
        && !matches!(
            parts[1],
            "capability_packs.json" | ".catalog.lock" | "pack_promotion_audit.jsonl"
        )
        && !prefix(parts, &["capability_evolution", "rollback"], 3)
}

fn definition_record(parts: &[&str]) -> bool {
    if !prefix(parts, &["agent_runtime", "agents"], 3) {
        return false;
    }
    if parts[2] == ".definition_store.write.lock" {
        return true;
    }
    parts.len() >= 4 && (parts[3] == "definition.agent.yaml" || parts[3] == "definitions")
}

fn runtime_record(parts: &[&str]) -> bool {
    if !prefix(parts, &["agent_runtime"], 2) || definition_record(parts) {
        return false;
    }
    if prefix(parts, &["agent_runtime", "system"], 2) {
        return false;
    }
    if prefix(parts, &["agent_runtime", "agents"], 5) && parts[3] == "memory" {
        return false;
    }
    true
}

fn memory_canonical(parts: &[&str]) -> bool {
    if prefix(parts, &["memory", "index"], 3) {
        return false;
    }
    if prefix(parts, &["memory"], 2) {
        return true;
    }
    prefix(parts, &["agent_runtime", "agents"], 5) && parts[3] == "memory"
}

pub fn walk_files(scope_root: &Path, owner: AgentOwner) -> Vec<String> {
    let mut out = Vec::new();
    let _ = collect(scope_root, scope_root, owner, &mut out);
    out.sort();
    out.dedup();
    out.into_iter().filter(|rel| owner.allows(rel)).collect()
}

fn collect(
    scope_root: &Path,
    dir: &Path,
    owner: AgentOwner,
    out: &mut Vec<String>,
) -> anyhow::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if meta.file_type().is_symlink() {
                continue;
            }
        }
        if path.is_dir() {
            collect(scope_root, &path, owner, out)?;
        } else if path.is_file() {
            if let Ok(rel) = path.strip_prefix(scope_root) {
                let rel = rel.to_string_lossy().replace('\\', "/");
                if owner.allows(&rel) {
                    out.push(rel);
                }
            }
        }
    }
    Ok(())
}

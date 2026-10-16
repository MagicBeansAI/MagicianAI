//! Builds tool-catalog descriptors from loaded skills.
//!
//! Only procedure-kind skills appear in the agent's tool catalog.
//! Personality-modes are reachable via `switch_personality` and stay out of
//! the catalog.

use super::manifest::{InferredKind, SkillManifest};

/// Tool-catalog entry for a procedure skill.
#[derive(Debug, Clone)]
pub struct SkillDescriptor {
    pub name: String,
    pub description: String,
    pub source_dir: std::path::PathBuf,
    /// Spec's `allowed-tools` field; scopes which other tools/skills the LLM
    /// may call while this skill is active.
    pub allowed_tools: Option<String>,
}

/// Build descriptors for every procedure-kind skill in `manifests`.
/// Personality-modes are filtered out.
pub fn build_skill_descriptors(manifests: &[SkillManifest]) -> Vec<SkillDescriptor> {
    manifests
        .iter()
        .filter(|m| m.inferred_kind() == InferredKind::Procedure)
        .map(|m| SkillDescriptor {
            name: m.name.clone(),
            description: m.description.clone(),
            source_dir: m.source_dir.clone(),
            allowed_tools: m.allowed_tools.clone(),
        })
        .collect()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::skills::loader::SkillLoader;
    use std::fs;

    fn write_skill(parent: &std::path::Path, name: &str, body: &str) {
        let p = parent.join(name);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("SKILL.md"), body).unwrap();
    }

    #[test]
    fn only_procedure_skills_appear_in_descriptors() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(
            dir.path(),
            "review",
            "---\nname: review\ndescription: review code\n---\n",
        );
        write_skill(
            dir.path(),
            "witty",
            "---\nname: witty\ndescription: witty persona\n\
             metadata:\n  magician:\n    personality:\n      active_mode: witty\n      voice: x\n---\n",
        );

        let manifests = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap();
        let descriptors = build_skill_descriptors(&manifests);
        let names: Vec<&str> = descriptors.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["review"]);
    }

    #[test]
    fn descriptor_propagates_allowed_tools_field() {
        let dir = tempfile::tempdir().unwrap();
        write_skill(
            dir.path(),
            "code-review",
            "---\nname: code-review\ndescription: d\nallowed-tools: \"Read Bash(git:*)\"\n---\n",
        );
        let manifests = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap();
        let d = &build_skill_descriptors(&manifests)[0];
        assert_eq!(d.allowed_tools.as_deref(), Some("Read Bash(git:*)"));
    }
}

//! Procedure-skill activation.
//!
//! Activation = check `requires.bins` resolve, load the body lazily, register
//! `scripts/` as ephemeral tools, expose `bin/` for PATH injection.
//!
//! Personality-mode and agent-definition skills do NOT go through this path
//! (see `personality_mode` and `agent_def` modules in later phases).

use anyhow::Result;
use std::path::PathBuf;

use super::deps::check_required_bins;
use super::manifest::{InferredKind, SkillManifest};
use super::scripts::register_scripts;

/// Result of activating a procedure skill.
#[derive(Debug, Clone)]
pub struct SkillActivation {
    pub skill_name: String,
    /// Skill body — injected as a steering message into the next prompt.
    pub steering_message: String,
    /// Each script in `scripts/` becomes an ephemeral tool the LLM can pick.
    pub ephemeral_tools: Vec<EphemeralTool>,
    /// `<skill-dir>/bin/` if it exists; the runner prepends this to PATH for
    /// every active skill so cross-skill bin lookup works.
    pub path_additions: Vec<PathBuf>,
}

/// Synthetic tool descriptor for an LLM-pickable script inside a skill's
/// `scripts/` dir.
#[derive(Debug, Clone)]
pub struct EphemeralTool {
    /// `<skill>__<script-stem>`.
    pub name: String,
    pub script_path: PathBuf,
    pub runtime: ScriptRuntime,
    /// The skill's resolved root — magician injects this as
    /// `MAGICIAN_SKILL_DIR` when the script runs.
    pub skill_dir: PathBuf,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptRuntime {
    Bash,
    Python,
    Node,
}

/// Activate a procedure skill. Errors if:
/// - The skill's `requires.bins` are not resolvable.
/// - The body cannot be read.
/// - The skill is not actually a procedure (kind mismatch).
pub fn activate_procedure_skill(manifest: &SkillManifest) -> Result<SkillActivation> {
    if manifest.inferred_kind() != InferredKind::Procedure {
        anyhow::bail!(
            "skill '{}' is not a procedure (kind: {:?}); use the dedicated \
             personality-mode or agent-definition activation paths",
            manifest.name,
            manifest.inferred_kind(),
        );
    }
    check_required_bins(manifest)?;
    let body = manifest.body()?.trim().to_string();
    let scripts_dir = manifest.source_dir.join("scripts");
    let ephemeral_tools = register_scripts(&scripts_dir, &manifest.name, &manifest.source_dir)?;
    // The `bin/` PATH addition is rewritten for this environment (identity
    // natively) so a container resolves the skill's vendored binaries via the
    // real skillshub dir rather than the dangling scope symlinks.
    let bin_dir = super::path_rewrite::rewrite_skill_dir(&manifest.source_dir).join("bin");
    let path_additions = if bin_dir.is_dir() {
        vec![bin_dir]
    } else {
        Vec::new()
    };
    Ok(SkillActivation {
        skill_name: manifest.name.clone(),
        steering_message: body,
        ephemeral_tools,
        path_additions,
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::skills::loader::SkillLoader;
    use std::fs;

    fn write_procedure_with_scripts_and_bin(parent: &std::path::Path, name: &str) {
        let p = parent.join(name);
        fs::create_dir_all(p.join("scripts")).unwrap();
        fs::create_dir_all(p.join("bin")).unwrap();
        fs::write(
            p.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: d\n---\n# body\nWhen invoked, do X.\n"),
        )
        .unwrap();
        fs::write(p.join("scripts/run.sh"), "#!/bin/sh\necho ok\n").unwrap();
        fs::write(p.join("bin/example"), "#!/bin/sh\necho hi\n").unwrap();
    }

    #[test]
    fn activate_returns_body_scripts_and_path_additions() {
        let dir = tempfile::tempdir().unwrap();
        write_procedure_with_scripts_and_bin(dir.path(), "toolskill");

        let manifests = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap();
        let activation = activate_procedure_skill(&manifests[0]).unwrap();

        assert!(activation.steering_message.contains("When invoked, do X."));
        assert_eq!(activation.ephemeral_tools.len(), 1);
        assert_eq!(activation.ephemeral_tools[0].name, "toolskill__run");
        assert_eq!(activation.path_additions.len(), 1);
        assert!(activation.path_additions[0].ends_with("toolskill/bin"));
    }

    #[test]
    fn activate_plain_skill_returns_body_and_zero_ephemeral_tools() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("plain");
        fs::create_dir_all(&p).unwrap();
        fs::write(
            p.join("SKILL.md"),
            "---\nname: plain\ndescription: d\n---\n# body\nFollow these steps.\n",
        )
        .unwrap();

        let manifests = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap();
        let activation = activate_procedure_skill(&manifests[0]).unwrap();
        assert!(activation.steering_message.contains("Follow these steps."));
        assert!(activation.ephemeral_tools.is_empty());
        assert!(activation.path_additions.is_empty());
    }

    #[test]
    fn activate_rejects_non_procedure_kind() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("witty");
        fs::create_dir_all(&p).unwrap();
        fs::write(
            p.join("SKILL.md"),
            "---\nname: witty\ndescription: d\n\
             metadata:\n  magician:\n    personality:\n      active_mode: witty\n      voice: x\n---\n",
        )
        .unwrap();

        let manifests = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap();
        let err = activate_procedure_skill(&manifests[0]).unwrap_err();
        assert!(format!("{err:#}").contains("not a procedure"));
    }

    #[test]
    fn activate_fails_when_required_bin_missing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("needs-missing");
        fs::create_dir_all(&p).unwrap();
        fs::write(
            p.join("SKILL.md"),
            "---\nname: needs-missing\ndescription: d\n\
             metadata:\n  magician:\n    requires:\n      bins: [\"a-bin-that-does-not-exist\"]\n---\n",
        )
        .unwrap();
        let manifests = SkillLoader::new(vec![dir.path().to_path_buf()])
            .discover()
            .unwrap();
        let err = activate_procedure_skill(&manifests[0]).unwrap_err();
        assert!(format!("{err:#}").contains("a-bin-that-does-not-exist"));
    }
}

//! Cataloged Task 16A subprocess and skill scratch owners.

use std::path::{Path, PathBuf};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SubprocessOwner {
    WorkdirsScratch,
    SkillWorking,
}

impl SubprocessOwner {
    pub const ALL: [SubprocessOwner; 2] = [Self::WorkdirsScratch, Self::SkillWorking];

    pub fn id(self) -> &'static str {
        match self {
            Self::WorkdirsScratch => "workdirs_scratch",
            Self::SkillWorking => "subprocess_skill_working",
        }
    }

    pub fn sample_rel(self) -> &'static str {
        match self {
            Self::WorkdirsScratch => "workdirs/meeting_capture_markers/session.json",
            Self::SkillWorking => "workdirs/skill-working/sample/outputs/slot.bin",
        }
    }

    pub fn allows(self, rel: &str) -> bool {
        if rel.starts_with('/') || std::path::Path::new(rel).is_absolute() {
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
            Self::WorkdirsScratch => {
                prefix(&parts, &["workdirs"], 2) && parts.get(1) != Some(&"skill-working")
            },
            Self::SkillWorking => prefix(&parts, &["workdirs", "skill-working"], 3),
        }
    }

    pub fn claiming(rel: &str) -> Option<SubprocessOwner> {
        if Self::SkillWorking.allows(rel) {
            Some(Self::SkillWorking)
        } else if Self::WorkdirsScratch.allows(rel) {
            Some(Self::WorkdirsScratch)
        } else {
            None
        }
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

pub fn walk_files(scope_root: &Path, owner: SubprocessOwner) -> Vec<String> {
    let mut out = Vec::new();
    let _ = collect(scope_root, scope_root, owner, &mut out);
    out.sort();
    out.dedup();
    out.into_iter().filter(|rel| owner.allows(rel)).collect()
}

fn collect(
    scope_root: &Path,
    dir: &Path,
    owner: SubprocessOwner,
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

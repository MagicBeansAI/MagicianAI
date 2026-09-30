//! Cataloged Task 12 work-spine owners and the relative paths they may hold.

use std::path::{Path, PathBuf};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorkOwner {
    TaskRecords,
    InternalTasks,
    TaskPlans,
    Executions,
    TaskPlanningRecovery,
    PauseStates,
    ListIndex,
    RuntimeResumeRecovery,
    RestrictedHmacCatalogs,
}

impl WorkOwner {
    pub const ALL: [WorkOwner; 9] = [
        Self::TaskRecords,
        Self::InternalTasks,
        Self::TaskPlans,
        Self::Executions,
        Self::TaskPlanningRecovery,
        Self::PauseStates,
        Self::ListIndex,
        Self::RuntimeResumeRecovery,
        Self::RestrictedHmacCatalogs,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::TaskRecords => "task_records",
            Self::InternalTasks => "internal_tasks",
            Self::TaskPlans => "task_plans",
            Self::Executions => "executions",
            Self::TaskPlanningRecovery => "task_planning_recovery",
            Self::PauseStates => "pause_states",
            Self::ListIndex => "list_index",
            Self::RuntimeResumeRecovery => "runtime_resume_recovery",
            Self::RestrictedHmacCatalogs => "restricted_hmac_catalogs",
        }
    }

    pub fn sample_rel(self) -> &'static str {
        match self {
            Self::TaskRecords => "tasks/task-1/manifest.json",
            Self::InternalTasks => "internal_tasks/task-1/manifest.json",
            Self::TaskPlans => "tasks/task-1/plans/plan-1.json",
            Self::Executions => "tasks/task-1/executions/ex-1/state.json",
            Self::TaskPlanningRecovery => "task_planning_recovery/receipt.json",
            Self::PauseStates => "runtime/pause_states/pause-1.json",
            Self::ListIndex => "ui/indexes/list_index.db",
            Self::RuntimeResumeRecovery => "restricted/runtime_resume_recovery/entry.json",
            Self::RestrictedHmacCatalogs => "restricted/accepted_runtime_launch/entry.json",
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
            Self::TaskRecords => {
                prefix(&parts, &["tasks"], 3)
                    && parts[2] != "outputs"
                    && parts[2] != "executions"
                    && parts[2] != "plans"
            },
            Self::InternalTasks => {
                prefix(&parts, &["internal_tasks"], 3)
                    && parts[2] != "outputs"
                    && parts[2] != "executions"
                    && parts[2] != "plans"
            },
            Self::TaskPlans => {
                (prefix(&parts, &["tasks"], 4) && parts[2] == "plans")
                    || (prefix(&parts, &["internal_tasks"], 4) && parts[2] == "plans")
            },
            Self::Executions => execution_record(&parts),
            Self::TaskPlanningRecovery => prefix(&parts, &["task_planning_recovery"], 2),
            Self::PauseStates => prefix(&parts, &["runtime", "pause_states"], 3),
            Self::ListIndex => parts == ["ui", "indexes", "list_index.db"],
            Self::RuntimeResumeRecovery => {
                prefix(&parts, &["restricted", "runtime_resume_recovery"], 3)
            },
            Self::RestrictedHmacCatalogs => restricted_hmac_catalog(&parts),
        }
    }

    pub fn claiming(rel: &str) -> Option<WorkOwner> {
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

fn execution_excluded(name: &str) -> bool {
    matches!(
        name,
        "outputs" | "observations" | "recordings" | "artifacts"
    )
}

const RESTRICTED_HMAC_DIRS: &[&str] = &[
    "accepted_runtime_launch",
    "task_recipe_replays",
    "execution_routing",
    "plane_attenuation",
    "operator_steers",
    "manual_resume_transactions",
    "delegated_child_recovery",
    "execution_pipeline_rosters",
    "execution_pipeline_progress",
    "pipeline_terminal_settlements",
    "delegation_round_progress",
    "llm_capture",
];

fn restricted_hmac_catalog(parts: &[&str]) -> bool {
    prefix(parts, &["restricted"], 3) && RESTRICTED_HMAC_DIRS.contains(&parts[1])
}

fn execution_record(parts: &[&str]) -> bool {
    if (prefix(parts, &["tasks"], 5) || prefix(parts, &["internal_tasks"], 5))
        && parts[2] == "executions"
    {
        return !execution_excluded(parts[4]);
    }
    if prefix(parts, &["executions"], 3) {
        return !execution_excluded(parts[2]);
    }
    false
}

pub fn walk_files(scope_root: &Path, owner: WorkOwner) -> Vec<String> {
    let mut out = Vec::new();
    let _ = collect(scope_root, scope_root, owner, &mut out);
    out.sort();
    out.dedup();
    out.into_iter().filter(|rel| owner.allows(rel)).collect()
}

fn collect(
    scope_root: &Path,
    dir: &Path,
    owner: WorkOwner,
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

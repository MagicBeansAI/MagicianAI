//! Cataloged Task 10 object owners and the relative paths they may hold.

use std::path::{Path, PathBuf};

/// One cataloged immutable-bytes owner. Local layout stays under the scope root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ObjectOwner {
    AppPackages,
    AppAttachments,
    AppExports,
    AppCaptures,
    AppEvaluations,
    TaskOutputs,
    ExecutionOutputs,
    ExecutionObservations,
    ExecutionDownloads,
    ExecutionRecordings,
    LlmTraceJournal,
    LlmRestrictedJournal,
    SkillsScope,
}

impl ObjectOwner {
    pub const ALL: [ObjectOwner; 13] = [
        Self::AppPackages,
        Self::AppAttachments,
        Self::AppExports,
        Self::AppCaptures,
        Self::AppEvaluations,
        Self::TaskOutputs,
        Self::ExecutionOutputs,
        Self::ExecutionObservations,
        Self::ExecutionDownloads,
        Self::ExecutionRecordings,
        Self::LlmTraceJournal,
        Self::LlmRestrictedJournal,
        Self::SkillsScope,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::AppPackages => "app_packages",
            Self::AppAttachments => "app_attachments",
            Self::AppExports => "app_exports",
            Self::AppCaptures => "app_captures",
            Self::AppEvaluations => "app_evaluations",
            Self::TaskOutputs => "task_outputs",
            Self::ExecutionOutputs => "execution_outputs",
            Self::ExecutionObservations => "execution_observations",
            Self::ExecutionDownloads => "execution_downloads",
            Self::ExecutionRecordings => "execution_recordings",
            Self::LlmTraceJournal => "llm_trace_journal",
            Self::LlmRestrictedJournal => "llm_restricted_journal",
            Self::SkillsScope => "skills_scope",
        }
    }

    pub fn sample_rel(self) -> &'static str {
        match self {
            Self::AppPackages => "apps/packages/digest/payload.bin",
            Self::AppAttachments => "apps/attachments/note.bin",
            Self::AppExports => "apps/exports/bundle.zip",
            Self::AppCaptures => "apps/captures/prompt.json",
            Self::AppEvaluations => "apps/evaluations/run.json",
            Self::TaskOutputs => "tasks/task-1/outputs/result.bin",
            Self::ExecutionOutputs => "tasks/task-1/executions/ex-1/outputs/tool.bin",
            Self::ExecutionObservations => "tasks/task-1/executions/ex-1/observations/obs.png",
            Self::ExecutionDownloads => "tasks/task-1/executions/ex-1/artifacts/downloads/file.bin",
            Self::ExecutionRecordings => "tasks/task-1/executions/ex-1/recordings/clip.webm",
            Self::LlmTraceJournal => "analytics/llm_trace_journal/accepted.jsonl",
            Self::LlmRestrictedJournal => "analytics/llm_restricted_journal/accepted.jsonl",
            Self::SkillsScope => "skills/pack/skill.md",
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
            Self::AppPackages => prefix(&parts, &["apps", "packages"], 3),
            Self::AppAttachments => prefix(&parts, &["apps", "attachments"], 3),
            Self::AppExports => prefix(&parts, &["apps", "exports"], 3),
            Self::AppCaptures => prefix(&parts, &["apps", "captures"], 3),
            Self::AppEvaluations => prefix(&parts, &["apps", "evaluations"], 3),
            Self::TaskOutputs => {
                (prefix(&parts, &["tasks"], 4) && parts[2] == "outputs")
                    || (prefix(&parts, &["internal_tasks"], 4) && parts[2] == "outputs")
            },
            Self::ExecutionOutputs => {
                (prefix(&parts, &["tasks"], 6) && parts[2] == "executions" && parts[4] == "outputs")
                    || (prefix(&parts, &["executions"], 4) && parts[2] == "outputs")
            },
            Self::ExecutionObservations => {
                (prefix(&parts, &["tasks"], 6)
                    && parts[2] == "executions"
                    && parts[4] == "observations")
                    || (prefix(&parts, &["executions"], 4) && parts[2] == "observations")
            },
            Self::ExecutionDownloads => {
                (prefix(&parts, &["tasks"], 7)
                    && parts[2] == "executions"
                    && parts[4] == "artifacts"
                    && parts[5] == "downloads")
                    || (prefix(&parts, &["executions"], 5)
                        && parts[2] == "artifacts"
                        && parts[3] == "downloads")
            },
            Self::ExecutionRecordings => {
                (prefix(&parts, &["tasks"], 6)
                    && parts[2] == "executions"
                    && parts[4] == "recordings")
                    || (prefix(&parts, &["executions"], 4) && parts[2] == "recordings")
            },
            Self::LlmTraceJournal => prefix(&parts, &["analytics", "llm_trace_journal"], 3),
            Self::LlmRestrictedJournal => {
                prefix(&parts, &["analytics", "llm_restricted_journal"], 3)
            },
            Self::SkillsScope => prefix(&parts, &["skills"], 2),
        }
    }

    pub fn walk_roots(self, scope_root: &Path) -> Vec<PathBuf> {
        match self {
            Self::AppPackages => vec![scope_root.join("apps").join("packages")],
            Self::AppAttachments => vec![scope_root.join("apps").join("attachments")],
            Self::AppExports => vec![scope_root.join("apps").join("exports")],
            Self::AppCaptures => vec![scope_root.join("apps").join("captures")],
            Self::AppEvaluations => vec![scope_root.join("apps").join("evaluations")],
            Self::TaskOutputs => child_subdir(scope_root, &["tasks", "internal_tasks"], "outputs"),
            Self::ExecutionOutputs => {
                let mut roots = child_subdir(scope_root, &["executions"], "outputs");
                roots.extend(nested_execution_subdir(scope_root, "outputs"));
                roots
            },
            Self::ExecutionObservations => {
                let mut roots = child_subdir(scope_root, &["executions"], "observations");
                roots.extend(nested_execution_subdir(scope_root, "observations"));
                roots
            },
            Self::ExecutionDownloads => {
                let mut roots = nested_artifacts_subdir(scope_root, "downloads");
                roots.extend(child_artifacts_subdir(scope_root, "downloads"));
                roots
            },
            Self::ExecutionRecordings => {
                let mut roots = child_subdir(scope_root, &["executions"], "recordings");
                roots.extend(nested_execution_subdir(scope_root, "recordings"));
                roots
            },
            Self::LlmTraceJournal => {
                vec![scope_root.join("analytics").join("llm_trace_journal")]
            },
            Self::LlmRestrictedJournal => {
                vec![scope_root.join("analytics").join("llm_restricted_journal")]
            },
            Self::SkillsScope => vec![scope_root.join("skills")],
        }
    }
}

fn prefix(parts: &[&str], head: &[&str], min_len: usize) -> bool {
    parts.len() >= min_len && parts.get(..head.len()) == Some(head)
}

fn child_subdir(scope_root: &Path, parents: &[&str], leaf: &str) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for parent in parents {
        let dir = scope_root.join(parent);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                roots.push(path.join(leaf));
            }
        }
    }
    roots
}

fn child_artifacts_subdir(scope_root: &Path, leaf: &str) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let executions = scope_root.join("executions");
    let Ok(entries) = std::fs::read_dir(&executions) else {
        return roots;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            roots.push(path.join("artifacts").join(leaf));
        }
    }
    roots
}

fn nested_artifacts_subdir(scope_root: &Path, leaf: &str) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let tasks = scope_root.join("tasks");
    let Ok(task_entries) = std::fs::read_dir(&tasks) else {
        return roots;
    };
    for task in task_entries.flatten() {
        let executions = task.path().join("executions");
        let Ok(exec_entries) = std::fs::read_dir(&executions) else {
            continue;
        };
        for execution in exec_entries.flatten() {
            let path = execution.path();
            if path.is_dir() {
                roots.push(path.join("artifacts").join(leaf));
            }
        }
    }
    roots
}

fn nested_execution_subdir(scope_root: &Path, leaf: &str) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let tasks = scope_root.join("tasks");
    let Ok(task_entries) = std::fs::read_dir(&tasks) else {
        return roots;
    };
    for task in task_entries.flatten() {
        let executions = task.path().join("executions");
        let Ok(exec_entries) = std::fs::read_dir(&executions) else {
            continue;
        };
        for execution in exec_entries.flatten() {
            let path = execution.path();
            if path.is_dir() {
                roots.push(path.join(leaf));
            }
        }
    }
    roots
}

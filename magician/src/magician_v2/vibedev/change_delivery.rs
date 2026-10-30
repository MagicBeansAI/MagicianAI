//! How a VibeDev run's changes land, recorded as a runtime fact on the
//! execution that decided it.
//!
//! The server decides a coding run's change delivery from its own records:
//! the coordinator's dispatch (`artifact_v2::service::vibedev_run_trust_mode`)
//! and `run_coding_task`'s trusted workspace or fallback. The decision used to
//! reach the coordinator only as prose in its goal, so a true report ("landed
//! in the project folder with no branch because it is not a git repository")
//! had nothing Magician-authored to be checked against. The decision is now
//! also written beside the execution (`change_delivery.json`). The claim check
//! reads it for the execution and every execution it delegated to
//! (`artifact_v2::synthesis::build_execution_grounding_corpus`) as a runtime
//! record.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

use super::trusted_workspace::{validate_trusted_repo, EffectiveTrustMode, TrustedRepoError};

/// The record's file, in the execution's directory.
pub const CHANGE_DELIVERY_RECORD_FILE: &str = "change_delivery.json";
pub const CHANGE_DELIVERY_RECORD_SCHEMA: u32 = 1;

/// The change-delivery mode the run uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeDeliveryMode {
    Original,
    /// The chain's own git clone on a `vibedev/…` branch; changes are commits.
    TrustedBranch,
    /// A trusted project run on a sandboxed copy, because its folder cannot
    /// back a clone.
    SandboxedFallback,
    /// A sandboxed project's run: changes are proposals applied to the folder.
    Sandboxed,
}

impl ChangeDeliveryMode {
    pub fn from_effective(mode: &EffectiveTrustMode) -> Self {
        match mode {
            EffectiveTrustMode::Original => Self::Original,
            EffectiveTrustMode::Trusted => Self::TrustedBranch,
            EffectiveTrustMode::SandboxedFallback { .. } => Self::SandboxedFallback,
            EffectiveTrustMode::Sandboxed => Self::Sandboxed,
        }
    }
}

/// The project folder's git state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderGitState {
    RepositoryRoot,
    NotARepository,
    /// Below a repository's root.
    InsideRepository,
    NotADirectory,
    /// This machine has no git Magician can run.
    NoGit,
}

/// A folder's git state, the repository root when there is one, and why, as
/// one clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderGit {
    pub state: FolderGitState,
    pub repository_root: Option<PathBuf>,
    pub reason: String,
}

/// [`FolderGit`] for `folder`, by the rule trusted projects are held to
/// (`trusted_workspace::validate_trusted_repo`). Runs `git` once.
pub fn folder_git(folder: &Path) -> FolderGit {
    if !runtime_core::process::git_available() {
        return FolderGit {
            state: FolderGitState::NoGit,
            repository_root: None,
            reason: "this machine has no git Magician can run".to_string(),
        };
    }
    match validate_trusted_repo(folder) {
        Ok(root) => FolderGit {
            state: FolderGitState::RepositoryRoot,
            reason: format!("`{}` is the root of a git repository", root.display()),
            repository_root: Some(root),
        },
        Err(error) => {
            let (state, repository_root) = match &error {
                TrustedRepoError::NotARepository { .. } => (FolderGitState::NotARepository, None),
                TrustedRepoError::InsideRepository { root, .. } => {
                    (FolderGitState::InsideRepository, Some(root.clone()))
                },
                TrustedRepoError::NotADirectory { .. } => (FolderGitState::NotADirectory, None),
            };
            FolderGit {
                state,
                repository_root,
                reason: error.to_string(),
            }
        },
    }
}

/// One execution's change-delivery decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeDeliveryRecord {
    pub schema_version: u32,
    pub mode: ChangeDeliveryMode,
    pub project_folder: PathBuf,
    pub git_state: FolderGitState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_root: Option<PathBuf>,
    /// Why the folder has that git state, as one clause.
    pub git_reason: String,
    /// Why a trusted project's run is sandboxed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    /// A trusted run's branch, once known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Which seam decided it: `coordinator_dispatch` or `run_coding_task`.
    pub decided_by: String,
    pub recorded_at: String,
}

impl ChangeDeliveryRecord {
    pub fn new(
        mode: ChangeDeliveryMode,
        project_folder: PathBuf,
        git: FolderGit,
        fallback_reason: Option<String>,
        branch: Option<String>,
        decided_by: &str,
    ) -> Self {
        Self {
            schema_version: CHANGE_DELIVERY_RECORD_SCHEMA,
            mode,
            project_folder,
            git_state: git.state,
            repository_root: git.repository_root,
            git_reason: git.reason,
            fallback_reason,
            branch,
            decided_by: decided_by.to_string(),
            recorded_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// The coordinator's decision: the effective mode and the project folder,
    /// with the folder's git state checked now.
    pub fn decided(mode: &EffectiveTrustMode, project_folder: &Path, decided_by: &str) -> Self {
        let fallback_reason = match mode {
            EffectiveTrustMode::SandboxedFallback { reason, .. } => Some(reason.clone()),
            _ => None,
        };
        Self::new(
            ChangeDeliveryMode::from_effective(mode),
            project_folder.to_path_buf(),
            folder_git(project_folder),
            fallback_reason,
            None,
            decided_by,
        )
    }

    /// A trusted project's run that fell back to a sandboxed copy: the
    /// fallback's reason is the folder's git state, checked now.
    pub fn sandboxed_fallback(project_folder: &Path, decided_by: &str) -> Self {
        let git = folder_git(project_folder);
        let reason = git.reason.clone();
        Self::new(
            ChangeDeliveryMode::SandboxedFallback,
            project_folder.to_path_buf(),
            git,
            Some(reason),
            None,
            decided_by,
        )
    }

    /// A trusted run whose branch exists: its repository is the folder.
    pub fn trusted_branch(real_repo: &Path, branch: &str, decided_by: &str) -> Self {
        Self::new(
            ChangeDeliveryMode::TrustedBranch,
            real_repo.to_path_buf(),
            FolderGit {
                state: FolderGitState::RepositoryRoot,
                repository_root: Some(real_repo.to_path_buf()),
                reason: format!("`{}` is the root of a git repository", real_repo.display()),
            },
            None,
            Some(branch.to_string()),
            decided_by,
        )
    }

    /// The record's line of evidence, in plain words.
    pub fn grounding_line(&self) -> String {
        let mode = match self.mode {
            ChangeDeliveryMode::Original => "an original-folder run: edits are reviewed before they change the current checkout; there is no clone, proposal, automatic commit or merge".to_string(),
            ChangeDeliveryMode::TrustedBranch => "a trusted branch run: the coding works in the \
                 chain's own git clone and commits on a `vibedev/…` branch"
                .to_string(),
            ChangeDeliveryMode::SandboxedFallback => format!(
                "a sandboxed run of a trusted project, because {}: the coding works on a \
                 sandboxed copy and its changes are proposals applied to the project folder",
                self.fallback_reason
                    .as_deref()
                    .unwrap_or("its folder cannot back a git clone")
            ),
            ChangeDeliveryMode::Sandboxed => "a sandboxed run: the coding works on a sandboxed \
                 copy and its changes are proposals applied to the project folder"
                .to_string(),
        };
        let root = self
            .repository_root
            .as_deref()
            .unwrap_or(self.project_folder.as_path())
            .display();
        let git = match self.git_state {
            FolderGitState::RepositoryRoot => {
                format!("the project folder is the root of the git repository `{root}`")
            },
            FolderGitState::NotARepository => "the project folder is not a git repository".into(),
            FolderGitState::InsideRepository => {
                format!("the project folder is inside the git repository `{root}`, not its root")
            },
            FolderGitState::NotADirectory => {
                "the project folder is not an existing directory".into()
            },
            FolderGitState::NoGit => "this machine has no git Magician can run".into(),
        };
        // The facts a report states lead the line, so a cut evidence budget
        // keeps them.
        let branch = match (self.mode, self.branch.as_deref()) {
            (ChangeDeliveryMode::TrustedBranch, Some(branch)) => {
                format!(" The run's branch is `{branch}`.")
            },
            (ChangeDeliveryMode::TrustedBranch, None) => String::new(),
            _ if self.git_state != FolderGitState::RepositoryRoot => format!(
                " This run has no git branch and makes no git commits, because {git}; its \
                 changes land directly in the project folder."
            ),
            _ => String::new(),
        };
        format!(
            "Project folder `{}`: {git}.{branch} Magician decided how this run's changes land: \
             {mode}. ({})",
            self.project_folder.display(),
            self.git_reason
        )
    }

    pub fn path(
        workspace: &ArtifactV2Workspace,
        principal: &str,
        scope_workspace: &str,
        task_id: &str,
        execution_id: &str,
    ) -> PathBuf {
        workspace
            .execution_dir(principal, scope_workspace, task_id, execution_id)
            .join(CHANGE_DELIVERY_RECORD_FILE)
    }

    /// Write the record on the execution (the latest decision wins). A failed
    /// write is logged: it costs grounding evidence, never the run.
    pub async fn write(
        &self,
        workspace: &ArtifactV2Workspace,
        principal: &str,
        scope_workspace: &str,
        task_id: &str,
        execution_id: &str,
    ) {
        let path = Self::path(workspace, principal, scope_workspace, task_id, execution_id);
        if let Err(error) = workspace.write_json_atomic_path(&path, self).await {
            tracing::warn!(
                task_id,
                execution_id,
                error = %error,
                "[VIBEDEV] could not record the run's change delivery"
            );
        }
    }

    /// The execution's record, if one was written.
    pub async fn load(
        workspace: &ArtifactV2Workspace,
        principal: &str,
        scope_workspace: &str,
        task_id: &str,
        execution_id: &str,
    ) -> Option<Self> {
        let path = Self::path(workspace, principal, scope_workspace, task_id, execution_id);
        workspace.read_json_path::<Self, _>(&path).await.ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(
        mode: ChangeDeliveryMode,
        state: FolderGitState,
        reason: &str,
    ) -> ChangeDeliveryRecord {
        ChangeDeliveryRecord::new(
            mode,
            PathBuf::from("/owner/notes"),
            FolderGit {
                state,
                repository_root: (state == FolderGitState::RepositoryRoot)
                    .then(|| PathBuf::from("/owner/notes")),
                reason: reason.to_string(),
            },
            None,
            None,
            "coordinator_dispatch",
        )
    }

    #[test]
    fn a_folder_that_is_not_a_repository_says_there_is_no_branch() {
        let line = record(
            ChangeDeliveryMode::Sandboxed,
            FolderGitState::NotARepository,
            "`/owner/notes` is not a git repository",
        )
        .grounding_line();
        assert!(
            line.contains("the project folder is not a git repository"),
            "{line}"
        );
        assert!(line.contains("no git branch"), "{line}");
        assert!(line.contains("`/owner/notes`"), "{line}");
    }

    #[test]
    fn a_repository_root_never_says_there_is_no_branch() {
        let line = record(
            ChangeDeliveryMode::Sandboxed,
            FolderGitState::RepositoryRoot,
            "`/owner/notes` is the root of a git repository",
        )
        .grounding_line();
        assert!(line.contains("root of the git repository"), "{line}");
        assert!(!line.contains("no git branch"), "{line}");
        assert!(!line.contains("not a git repository"), "{line}");
        let trusted = ChangeDeliveryRecord::trusted_branch(
            Path::new("/owner/site"),
            "vibedev/fix-login",
            "run_coding_task",
        )
        .grounding_line();
        assert!(
            trusted.contains("The run's branch is `vibedev/fix-login`."),
            "{trusted}"
        );
    }

    #[test]
    fn the_record_round_trips_with_snake_case_states() {
        let record = record(
            ChangeDeliveryMode::SandboxedFallback,
            FolderGitState::NotARepository,
            "`/owner/notes` is not a git repository",
        );
        let value = serde_json::to_value(&record).expect("serialize");
        assert_eq!(value["mode"], "sandboxed_fallback");
        assert_eq!(value["git_state"], "not_a_repository");
        let back: ChangeDeliveryRecord = serde_json::from_value(value).expect("deserialize");
        assert_eq!(back, record);
    }
}

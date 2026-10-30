//! VibeDev trusted repo mode: one git clone per coding chain.
//!
//! Plan: `docs/archive/plans/2026-10-07-vibedev-trusted-repo-mode.md` (§4.2–4.7).
//!
//! A project marked `trusted` does not run in the persistent shadow copy. The
//! chain's root task gets its own clone of the project repo on its own branch,
//! and every turn of the chain runs there with git available. Changes land as
//! commits on that branch; after each turn the branch is fetched into the
//! owner's repo so it shows up in their editor and tools. The owner's checkout
//! — its `HEAD`, index and working tree — is never touched here.
//!
//! **Why a `--shared` clone and not `git worktree`.** A worktree keeps its
//! index and `HEAD` under `<real>/.git/worktrees/<name>/` and its commits in
//! `<real>/.git/objects`/`refs`. The coding sandbox denies writes to the real
//! repo, and a hole wide enough for commits is wide enough to rewrite every
//! other branch. A `--shared` clone has its own `.git` inside the run dir and
//! borrows the real repo's objects read-only through `objects/info/alternates`,
//! so every write the engine makes stays inside the run dir and creation costs
//! no object copy.
//!
//! Everything here runs host-side, outside the coding sandbox: it is Magician
//! acting on the owner's behalf, not the engine.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::magician_v2::execution::coding_engine::coding_sandbox_root;

pub const TRUSTED_RUN_WORKSPACE_SCHEMA: u32 = 1;

/// Branch namespace for chain branches, in the clone and in the owner's repo.
pub const TRUSTED_BRANCH_PREFIX: &str = "vibedev/";

/// Where `run_coding_task` materializes attachments inside the working dir.
/// Excluded in every clone so an attachment is never committed.
const ATTACHMENT_EXCLUDE: &str = "/.cache/magician/";

/// The durable record of one chain's workspace, beside the clone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedRunWorkspace {
    pub schema_version: u32,
    pub root_task_id: String,
    pub project_id: String,
    /// The owner's repository — the clone's source, never written here except
    /// by [`publish_branch`] adding the chain's branch ref.
    pub real_repo: PathBuf,
    pub workspace_dir: PathBuf,
    pub branch: String,
    /// The branch the chain started from; `None` when the real repo was on a
    /// detached `HEAD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    /// The committed tip the chain started from. Uncommitted changes in the
    /// owner's checkout are deliberately not part of a run (plan D2).
    pub base_sha: String,
    pub created_at_ms: i64,
    /// Set once the owner merged the branch ([`merge_into_base`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged: Option<TrustedMergeRecord>,
    /// Set once a pull request was opened ([`open_pull_request`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request_url: Option<String>,
    /// Each finished turn's end commit and the engine session it ran in, so a
    /// rewind can rewind the agent's conversation along with the code.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub turns: Vec<TrustedTurnRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedTurnRecord {
    /// The branch head when the turn ended (after the end-of-turn commit).
    pub commit: String,
    pub task_id: String,
    /// `pi`, `claude`, `codex`, … — a resume is only queued for its own engine.
    pub engine: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_session_id: Option<String>,
    pub at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedMergeRecord {
    pub target: String,
    pub merged_sha: String,
    pub strategy: MergeStrategy,
    pub merged_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    FastForward,
    MergeCommit,
}

/// What [`ensure_run_workspace`] needs to know about the project.
pub struct TrustedRunRequest<'a> {
    pub scope_root: &'a Path,
    pub root_task_id: &'a str,
    pub project_id: &'a str,
    pub project_name: &'a str,
    pub real_repo: &'a Path,
    /// The project's configured base branch; `None` uses the real repo's
    /// current branch.
    pub base_branch: Option<&'a str>,
}

/// `<coding sandbox root>/coding_engine/runs` — beside the shadows, so
/// `MAGICIAN_CODING_SANDBOX_ROOT` moves both, and under the storage base by
/// default, where the coding sandbox allows writes.
pub fn trusted_runs_root(scope_root: &Path) -> PathBuf {
    coding_sandbox_root(scope_root)
        .join("coding_engine")
        .join("runs")
}

fn record_path(runs_root: &Path, root_task_id: &str) -> PathBuf {
    runs_root.join(format!("{root_task_id}.json"))
}

/// A task id as a single safe path segment.
fn safe_segment(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || !trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("`{value}` is not a usable run id"));
    }
    Ok(trimmed.to_string())
}

/// `vibedev/<project-slug>-<8 hex of the root task>`.
pub fn trusted_branch_name(project_name: &str, root_task_id: &str) -> String {
    let mut slug = String::new();
    for c in project_name.trim().to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= 32 {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    // A crew's integration chain (`crew_<hex>`) is named `crew-<hex>`, so its
    // eight characters are id, not the prefix.
    let (kind, id) = match root_task_id.strip_prefix("crew_") {
        Some(rest) => ("crew-", rest),
        None => ("", root_task_id.trim_start_matches("task_")),
    };
    let id: String = id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    let tail = if id.is_empty() {
        String::new()
    } else {
        format!("{kind}{id}")
    };
    match (slug.is_empty(), tail.is_empty()) {
        (false, false) => format!("{TRUSTED_BRANCH_PREFIX}{slug}-{tail}"),
        (true, false) => format!("{TRUSTED_BRANCH_PREFIX}run-{tail}"),
        (false, true) => format!("{TRUSTED_BRANCH_PREFIX}{slug}"),
        (true, true) => format!("{TRUSTED_BRANCH_PREFIX}run"),
    }
}

/// Run `git` in `dir`. Inherited `GIT_DIR`-style variables are removed so the
/// service's own environment can never point a command at another repo, and
/// prompts are off so a credential helper cannot hang a host-side call.
/// Paths print unquoted (`core.quotePath=false`): a quoted `"caf\303\251.txt"`
/// would come back from a phone as a pathspec that matches nothing.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new(runtime_core::process::git_program())
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.quotePath=false"])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed in `{}`: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Read the chain's record, if one exists.
pub fn save_run_workspace(scope_root: &Path, record: &TrustedRunWorkspace) -> Result<(), String> {
    write_record(&trusted_runs_root(scope_root), record)
}

pub fn load_run_workspace(scope_root: &Path, root_task_id: &str) -> Option<TrustedRunWorkspace> {
    let root = safe_segment(root_task_id).ok()?;
    let bytes = std::fs::read(record_path(&trusted_runs_root(scope_root), &root)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_record(runs_root: &Path, record: &TrustedRunWorkspace) -> Result<(), String> {
    let path = record_path(runs_root, &record.root_task_id);
    let bytes = serde_json::to_vec_pretty(record).map_err(|error| error.to_string())?;
    crate::magician_v2::artifact_v2::io::write_bytes_atomic_sync(&path, &bytes)
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// Make `folder` a git repository with one commit of its current files on
/// `branch` (the owner asked, from project health). Uses the owner's git
/// identity when one is configured, else commits as Magician. Refuses a folder
/// that already is, or is inside, a repository.
pub fn init_repository(folder: &Path, branch: &str) -> Result<(), String> {
    if git(folder, &["rev-parse", "--show-toplevel"]).is_ok() {
        return Err(format!(
            "`{}` is already inside a git repository",
            folder.display()
        ));
    }
    git(folder, &["init", "--quiet", "--initial-branch", branch])?;
    git(folder, &["add", "--all"])?;
    let has_identity = git(folder, &["config", "user.email"]).is_ok();
    let mut commit: Vec<&str> = Vec::new();
    if !has_identity {
        commit.extend([
            "-c",
            "user.name=Magician",
            "-c",
            "user.email=magician@localhost",
        ]);
    }
    commit.extend([
        "commit",
        "--quiet",
        "--allow-empty",
        "-m",
        "Initial commit (made by Magician for VibeDev)",
    ]);
    git(folder, &commit)?;
    Ok(())
}

/// Whether `branch` exists as a local branch of the repository at `repo`.
pub fn branch_exists(repo: &Path, branch: &str) -> bool {
    git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
}

/// The repository root of `path`, which must be a non-bare work tree whose
/// root is `path` itself — a subdirectory of a repo is not a project root.
pub fn validate_trusted_repo(path: &Path) -> Result<PathBuf, TrustedRepoError> {
    let canonical = path
        .canonicalize()
        .map_err(|error| TrustedRepoError::NotADirectory {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    let top = git(&canonical, &["rev-parse", "--show-toplevel"]).map_err(|_| {
        TrustedRepoError::NotARepository {
            path: canonical.clone(),
        }
    })?;
    let top =
        PathBuf::from(top)
            .canonicalize()
            .map_err(|error| TrustedRepoError::NotADirectory {
                path: canonical.clone(),
                reason: error.to_string(),
            })?;
    if top != canonical {
        return Err(TrustedRepoError::InsideRepository {
            path: canonical,
            root: top,
        });
    }
    Ok(canonical)
}

/// Why a folder cannot be a trusted project's repository. Callers branch on
/// the kind (project health offers `git init` only for
/// [`TrustedRepoError::NotARepository`]); the message is for people.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustedRepoError {
    NotADirectory {
        path: PathBuf,
        reason: String,
    },
    NotARepository {
        path: PathBuf,
    },
    /// A folder below a repository's root.
    InsideRepository {
        path: PathBuf,
        root: PathBuf,
    },
}

impl std::fmt::Display for TrustedRepoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotADirectory { path, reason } => {
                write!(
                    f,
                    "`{}` is not an existing directory: {reason}",
                    path.display()
                )
            },
            Self::NotARepository { path } => {
                write!(f, "`{}` is not a git repository", path.display())
            },
            Self::InsideRepository { path, root } => write!(
                f,
                "`{}` is inside the git repository `{}`; a trusted project must be the \
                 repository root",
                path.display(),
                root.display()
            ),
        }
    }
}

impl std::error::Error for TrustedRepoError {}

impl From<TrustedRepoError> for String {
    fn from(error: TrustedRepoError) -> Self {
        error.to_string()
    }
}

/// How a VibeDev coding run will actually work, whatever the project's
/// `trust` setting says.
///
/// One decision, read by both sides that must agree: `run_coding_task`
/// picks the workspace with it at dispatch, and the coordinator's goal
/// describes the run with it (so a trusted project that falls back is never
/// told "no diff approval" while its run raises one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectiveTrustMode {
    /// The original project folder; every edit is reviewed before apply.
    Original,
    /// The chain's own git clone on a `vibedev/…` branch; changes are commits.
    Trusted,
    /// A trusted project whose folder cannot back a clone: the run works on a
    /// sandboxed copy and returns its changes as a diff for review.
    SandboxedFallback {
        /// Why, as one clause ("`/x` is not a git repository").
        reason: String,
        /// What brings branch runs back, as one clause.
        remedy: &'static str,
    },
    /// A sandboxed project, a plan run, or a chain that started sandboxed.
    Sandboxed,
}

impl EffectiveTrustMode {
    /// The owner-facing note for a fallback run, or `None` otherwise.
    pub fn fallback_note(&self, project_name: &str) -> Option<String> {
        match self {
            Self::SandboxedFallback { reason, remedy } => Some(format!(
                "Project “{project_name}” is trusted, but {reason}, so this run works on a \
                 sandboxed copy and returns its changes for review. Branch runs come back once \
                 {remedy}."
            )),
            _ => None,
        }
    }
}

/// The folder a project's runs work on: its absolute `repo_path`, a relative
/// one under `workspace_root`, or `workspace_root` itself.
pub fn project_repo_folder(repo_path: Option<&str>, workspace_root: &Path) -> PathBuf {
    match repo_path
        .map(str::trim)
        .filter(|path| !path.is_empty() && *path != ".")
    {
        Some(path) if Path::new(path).is_absolute() => PathBuf::from(path),
        Some(path) => workspace_root.join(path),
        None => workspace_root.to_path_buf(),
    }
}

/// The mode a run of `project` will use.
///
/// - `has_run_workspace`: the chain's root already has a trusted workspace
///   record. A chain keeps the mode it started with, so this wins even if the
///   project's trust changed since.
/// - `chain_depth`: turns above this one on the chain (0 for a root). A
///   follow-up without a record belongs to a chain that ran sandboxed.
/// - `plan_run`: a Discuss run never starts a clone.
///
/// Otherwise a trusted project needs git and a folder that is a repository's
/// root; without them it falls back with the reason.
pub fn effective_trust_mode(
    project: &super::projects::VibeDevProjectRecord,
    workspace_root: &Path,
    has_run_workspace: bool,
    chain_depth: usize,
    plan_run: bool,
) -> EffectiveTrustMode {
    effective_trust_mode_with_git(
        project,
        workspace_root,
        has_run_workspace,
        chain_depth,
        plan_run,
        runtime_core::process::git_available(),
    )
}

fn effective_trust_mode_with_git(
    project: &super::projects::VibeDevProjectRecord,
    workspace_root: &Path,
    has_run_workspace: bool,
    chain_depth: usize,
    plan_run: bool,
    git_available: bool,
) -> EffectiveTrustMode {
    if has_run_workspace {
        return EffectiveTrustMode::Trusted;
    }
    if project.trust == super::projects::VibeDevProjectTrust::Original && chain_depth == 0 {
        return EffectiveTrustMode::Original;
    }
    if project.trust != super::projects::VibeDevProjectTrust::Trusted || chain_depth > 0 || plan_run
    {
        return EffectiveTrustMode::Sandboxed;
    }
    if !git_available {
        return EffectiveTrustMode::SandboxedFallback {
            reason: "this machine has no git Magician can run".to_string(),
            remedy: "git is installed (`make setup-skill-bins` installs the bundled git)",
        };
    }
    let folder = project_repo_folder(project.repo_path.as_deref(), workspace_root);
    match validate_trusted_repo(&folder) {
        Ok(_) => EffectiveTrustMode::Trusted,
        Err(error) => EffectiveTrustMode::SandboxedFallback {
            remedy: match error {
                TrustedRepoError::InsideRepository { .. } => {
                    "the project points at that repository's root folder"
                },
                _ => "the folder is a git repository",
            },
            reason: error.to_string(),
        },
    }
}

/// Whether a sandboxed run can keep its applied changes on a git branch of
/// the project's folder — the same rule [`validate_trusted_repo`] holds a
/// trusted project to: git must run here and the folder must be a
/// repository's root. A folder *inside* a repository gets no branch, because
/// switching that repository's branch would move files outside the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectFolderGit {
    /// The folder is the root of a git repository (canonical path).
    RepositoryRoot(PathBuf),
    /// No branch can be made there; why, as one clause.
    NoBranch(String),
}

/// [`ProjectFolderGit`] for `folder`, decided server-side.
pub fn project_folder_git(folder: &Path) -> ProjectFolderGit {
    project_folder_git_with(folder, runtime_core::process::git_available())
}

fn project_folder_git_with(folder: &Path, git_available: bool) -> ProjectFolderGit {
    if !git_available {
        return ProjectFolderGit::NoBranch("this machine has no git Magician can run".to_string());
    }
    match validate_trusted_repo(folder) {
        Ok(root) => ProjectFolderGit::RepositoryRoot(root),
        Err(TrustedRepoError::InsideRepository { path, root }) => {
            ProjectFolderGit::NoBranch(format!(
                "`{}` is inside the git repository `{}` rather than its root, and switching \
                 that repository's branch would move files outside the project",
                path.display(),
                root.display()
            ))
        },
        Err(error) => ProjectFolderGit::NoBranch(error.to_string()),
    }
}

/// Every file on the branch a chain would start from — `base_branch`, else the
/// repo's current branch — as repo-relative paths. A crew's planner reads them
/// so its lanes claim paths that exist.
pub fn base_branch_files(repo: &Path, base_branch: Option<&str>) -> Result<Vec<String>, String> {
    let repo = validate_trusted_repo(repo)?;
    let base_ref = match base_branch
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
    {
        Some(branch) => format!("refs/heads/{branch}^{{commit}}"),
        None => "HEAD^{commit}".to_string(),
    };
    let listing = git(&repo, &["ls-tree", "-r", "--name-only", &base_ref])?;
    Ok(listing
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

/// Create the chain's clone and branch, or return the existing one.
///
/// Idempotent per root task: a follow-up turn gets the same workspace. A record
/// whose clone has gone missing is rebuilt — from the chain's branch when it
/// was published to the real repo, so earlier turns' commits survive, else
/// from the recorded base.
pub fn ensure_run_workspace(
    request: &TrustedRunRequest<'_>,
) -> Result<TrustedRunWorkspace, String> {
    let root = safe_segment(request.root_task_id)?;
    let runs_root = trusted_runs_root(request.scope_root);
    std::fs::create_dir_all(&runs_root)
        .map_err(|error| format!("{}: {error}", runs_root.display()))?;
    let real_repo = validate_trusted_repo(request.real_repo)?;
    let workspace_dir = runs_root.join(&root);

    if let Some(existing) = load_run_workspace(request.scope_root, &root) {
        if existing.real_repo != real_repo {
            return Err(format!(
                "chain `{root}` was started against `{}`, not `{}`",
                existing.real_repo.display(),
                real_repo.display()
            ));
        }
        if workspace_dir.join(".git").is_dir() {
            return Ok(existing);
        }
        let published = git(
            &real_repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{}^{{commit}}", existing.branch),
            ],
        )
        .ok();
        let start = published.unwrap_or_else(|| existing.base_sha.clone());
        clone_at(&real_repo, &workspace_dir, &existing.branch, &start)?;
        return Ok(existing);
    }

    let base_branch = match request.base_branch.map(str::trim).filter(|b| !b.is_empty()) {
        Some(branch) => Some(branch.to_string()),
        None => git(&real_repo, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok(),
    };
    let base_ref = base_branch
        .as_deref()
        .map(|branch| format!("refs/heads/{branch}^{{commit}}"))
        .unwrap_or_else(|| "HEAD^{commit}".to_string());
    let base_sha = git(&real_repo, &["rev-parse", "--verify", &base_ref])
        .map_err(|_| format!("base `{base_ref}` does not resolve to a commit"))?;
    let branch = trusted_branch_name(request.project_name, &root);
    clone_at(&real_repo, &workspace_dir, &branch, &base_sha)?;

    let record = TrustedRunWorkspace {
        schema_version: TRUSTED_RUN_WORKSPACE_SCHEMA,
        root_task_id: root,
        project_id: request.project_id.to_string(),
        real_repo,
        workspace_dir,
        branch,
        base_branch,
        base_sha,
        created_at_ms: chrono::Utc::now().timestamp_millis(),
        merged: None,
        pull_request_url: None,
        turns: Vec::new(),
    };
    write_record(&runs_root, &record)?;
    Ok(record)
}

/// `git clone --shared` into a temp dir, check out `branch` at `start`, then
/// rename into place, so a crash never leaves a half-made clone at the path a
/// later turn would trust.
fn clone_at(
    real_repo: &Path,
    workspace_dir: &Path,
    branch: &str,
    start: &str,
) -> Result<(), String> {
    let parent = workspace_dir
        .parent()
        .ok_or_else(|| format!("`{}` has no parent", workspace_dir.display()))?;
    let staging = parent.join(format!(
        ".clone-{}-{}",
        workspace_dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        let staging_str = staging.to_string_lossy().into_owned();
        let real_str = real_repo.to_string_lossy().into_owned();
        git(
            parent,
            &[
                "clone",
                "--shared",
                "--no-checkout",
                "--quiet",
                &real_str,
                &staging_str,
            ],
        )?;
        git(&staging, &["checkout", "--quiet", "-B", branch, start])?;
        // Signing would prompt (or block on an agent the sandbox cannot reach)
        // in the middle of an unattended turn.
        git(&staging, &["config", "commit.gpgsign", "false"])?;
        git(&staging, &["config", "tag.gpgsign", "false"])?;
        let exclude = staging.join(".git").join("info").join("exclude");
        let mut contents = std::fs::read_to_string(&exclude).unwrap_or_default();
        if !contents.lines().any(|line| line == ATTACHMENT_EXCLUDE) {
            if !contents.is_empty() && !contents.ends_with('\n') {
                contents.push('\n');
            }
            contents.push_str("# VibeDev run attachments\n");
            contents.push_str(ATTACHMENT_EXCLUDE);
            contents.push('\n');
            std::fs::write(&exclude, contents)
                .map_err(|error| format!("{}: {error}", exclude.display()))?;
        }
        if workspace_dir.exists() {
            std::fs::remove_dir_all(workspace_dir)
                .map_err(|error| format!("{}: {error}", workspace_dir.display()))?;
        }
        // A directory move, not a file write: the atomic-write helpers cover
        // files. Same-filesystem rename of a fully built clone into place is
        // the atomicity this needs: a crash leaves at worst an orphaned
        // `.clone-*` staging dir (no state in it), never a half-made clone at
        // the path a later turn would trust.
        std::fs::rename(&staging, workspace_dir)
            .map_err(|error| format!("{}: {error}", workspace_dir.display()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

/// Commit whatever the turn left uncommitted, so every turn ends on a commit.
/// Hooks are skipped: this is Magician's bookkeeping commit, and a repo hook
/// (a docs guard, a formatter) failing must not lose the turn's work. Returns
/// the new commit, or `None` when the tree was already clean.
pub fn commit_pending_changes(
    workspace: &TrustedRunWorkspace,
    message: &str,
) -> Result<Option<String>, String> {
    let dir = &workspace.workspace_dir;
    if git(dir, &["status", "--porcelain"])?.is_empty() {
        return Ok(None);
    }
    git(dir, &["add", "--all"])?;
    // The operator's configured identity when there is one (a `-c` override
    // would mask it); a placeholder only where git has none to offer.
    let has_identity = git(dir, &["config", "user.email"]).is_ok_and(|email| !email.is_empty());
    let mut args: Vec<&str> = Vec::new();
    if !has_identity {
        args.extend([
            "-c",
            "user.name=VibeDev",
            "-c",
            "user.email=vibedev@localhost",
        ]);
    }
    args.extend(["commit", "--quiet", "--no-verify", "-m", message]);
    git(dir, &args)?;
    git(dir, &["rev-parse", "HEAD"]).map(Some)
}

/// The commit the chain's branch points at in the clone.
pub fn workspace_head(workspace: &TrustedRunWorkspace) -> Result<String, String> {
    git(&workspace.workspace_dir, &["rev-parse", "HEAD"])
}

/// Commits on the branch since the base.
pub fn commits_ahead(workspace: &TrustedRunWorkspace) -> Result<u64, String> {
    git(
        &workspace.workspace_dir,
        &[
            "rev-list",
            "--count",
            &format!("{}..HEAD", workspace.base_sha),
        ],
    )?
    .parse()
    .map_err(|error| format!("rev-list count: {error}"))
}

/// Files the branch changes against its base (`base..HEAD`), sorted.
pub fn changed_files(workspace: &TrustedRunWorkspace) -> Result<Vec<String>, String> {
    Ok(git(
        &workspace.workspace_dir,
        &["diff", "--name-only", &workspace.base_sha, "HEAD"],
    )?
    .lines()
    .map(str::trim)
    .filter(|line| !line.is_empty())
    .map(str::to_string)
    .collect())
}

/// The branch's unified diff against its base, as git reports it, cut at a
/// file boundary once it passes `max_bytes` (a single oversized file is cut
/// mid-way). It goes into the turn's result so the answer check can confirm
/// what changed from git itself, not from the engine's own summary.
pub fn branch_diff(workspace: &TrustedRunWorkspace, max_bytes: usize) -> Result<String, String> {
    let patch = git(
        &workspace.workspace_dir,
        &["diff", "--unified=3", &workspace.base_sha, "HEAD"],
    )?;
    Ok(cap_patch(&patch, max_bytes))
}

fn cap_patch(patch: &str, max_bytes: usize) -> String {
    if patch.len() <= max_bytes {
        return patch.to_string();
    }
    let mut kept = String::new();
    let mut omitted_files = 0usize;
    for chunk in split_patch_by_file(&format!("{patch}\n")) {
        if omitted_files == 0 && kept.len() + chunk.len() <= max_bytes {
            kept.push_str(&chunk);
        } else if omitted_files == 0 && kept.is_empty() {
            let mut end = max_bytes;
            while !chunk.is_char_boundary(end) {
                end -= 1;
            }
            kept.push_str(&chunk[..end]);
            kept.push_str("\n… (rest of this file's diff omitted)\n");
            omitted_files += 1;
        } else {
            omitted_files += 1;
        }
    }
    if omitted_files > 0 {
        kept.push_str(&format!(
            "… diff cut at {max_bytes} bytes; {omitted_files} file(s) not shown\n"
        ));
    }
    kept
}

/// Whether the clone has nothing uncommitted or untracked.
pub fn working_tree_clean(workspace: &TrustedRunWorkspace) -> Result<bool, String> {
    Ok(git(&workspace.workspace_dir, &["status", "--porcelain"])?.is_empty())
}

/// Most changed files a turn's result names before it summarises the rest.
const MAX_LISTED_CHANGED_FILES: usize = 20;

/// The branch facts a coordinator usually repeats in its answer — which files
/// the branch changes, whether the clone is clean, and whether it was merged
/// or has a pull request — as sentences for the turn's result, so the answer
/// check finds them in the evidence instead of removing them as unconfirmed.
/// Reads the `coding.branch_updated` status object; facts it lacks are left out.
pub fn branch_fact_sentences(status: &serde_json::Value) -> String {
    let mut sentences = Vec::new();
    if let Some(files) = status.get("changed_files").and_then(|v| v.as_array()) {
        let names: Vec<&str> = files.iter().filter_map(|v| v.as_str()).collect();
        if !names.is_empty() {
            let listed = names
                .iter()
                .take(MAX_LISTED_CHANGED_FILES)
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let more = names.len().saturating_sub(MAX_LISTED_CHANGED_FILES);
            let count = names.len();
            let noun = if count == 1 { "file" } else { "files" };
            sentences.push(if more > 0 {
                format!(
                    "The branch changes {count} {noun} against the base: {listed} and {more} more."
                )
            } else {
                format!("The branch changes {count} {noun} against the base: {listed}.")
            });
        }
    }
    match status.get("working_tree_clean").and_then(|v| v.as_bool()) {
        Some(true) => {
            sentences.push("The run's working tree is clean: everything is committed.".to_string())
        },
        Some(false) => {
            sentences.push("The run's working tree still has uncommitted changes.".to_string())
        },
        None => {},
    }
    let merged = status
        .get("merged")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let pull_request = status
        .get("pull_request_url")
        .and_then(|v| v.as_str())
        .filter(|url| !url.is_empty());
    match (merged, pull_request) {
        (true, _) => sentences.push("The branch has been merged into the base.".to_string()),
        (false, Some(url)) => sentences.push(format!(
            "The branch is not merged; a pull request is open: {url}."
        )),
        (false, None) if status.get("merged").is_some() => sentences
            .push("The branch is not merged and no pull request has been opened.".to_string()),
        _ => {},
    }
    sentences.join(" ")
}

/// Fetch the chain's branch into the owner's repo as `refs/heads/<branch>`.
///
/// Only that ref is written: the owner's `HEAD`, index and working tree are
/// untouched. A non-fast-forward update is refused (git's default), as is an
/// update to the branch while the owner has it checked out — both are reported
/// to the caller, never forced.
pub fn publish_branch(workspace: &TrustedRunWorkspace) -> Result<String, String> {
    let head = workspace_head(workspace)?;
    let source = workspace.workspace_dir.to_string_lossy().into_owned();
    let refspec = format!("refs/heads/{0}:refs/heads/{0}", workspace.branch);
    git(
        &workspace.real_repo,
        &["fetch", "--quiet", &source, &refspec],
    )?;
    Ok(head)
}

// ---------------------------------------------------------------------------
// Review and landing (plan §4.8–4.9)
// ---------------------------------------------------------------------------

/// Cap on the inline patch returned for review; a larger file reports its
/// counts and an omission note instead of its body.
const MAX_FILE_PATCH_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BranchCommit {
    pub sha: String,
    pub subject: String,
    pub author: String,
    pub committed_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BranchFileDiff {
    pub path: String,
    pub status: String,
    pub additions: usize,
    pub deletions: usize,
    pub unified_diff: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BranchSummary {
    pub root_task_id: String,
    pub branch: String,
    pub base_branch: Option<String>,
    pub base_sha: String,
    pub head_sha: String,
    /// Newest first. These are the chain's checkpoints.
    pub commits: Vec<BranchCommit>,
    pub files: Vec<BranchFileDiff>,
    /// Commits on the base branch (in the owner's repo) that this branch does
    /// not have — non-zero means a merge will not fast-forward.
    pub behind_base: Option<u64>,
    /// Uncommitted changes in the clone (a turn is running, or one ended
    /// without its end-of-turn commit).
    pub dirty: bool,
    pub merged: Option<TrustedMergeRecord>,
    pub pull_request_url: Option<String>,
}

/// Split a whole patch on column-0 `diff --git` / `diff --cc` headers. A body
/// line always carries a ` `/`+`/`-`/`\` prefix, so file content cannot forge
/// a header.
fn split_patch_by_file(patch: &str) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    for line in patch.split_inclusive('\n') {
        if line.starts_with("diff --git ") || line.starts_with("diff --cc ") {
            chunks.push(String::new());
        }
        if let Some(body) = chunks.last_mut() {
            body.push_str(line);
        }
    }
    chunks
}

/// The chain's branch for review: commits since the base, the per-file diff
/// of `base..HEAD`, and how far the base has moved on since.
pub fn branch_summary(workspace: &TrustedRunWorkspace) -> Result<BranchSummary, String> {
    let dir = &workspace.workspace_dir;
    let head_sha = workspace_head(workspace)?;
    let range = format!("{}..HEAD", workspace.base_sha);

    let log = git(dir, &["log", "--format=%H%x1f%s%x1f%an%x1f%ct", &range])?;
    let commits = log
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\u{1f}');
            Some(BranchCommit {
                sha: parts.next()?.to_string(),
                subject: parts.next()?.to_string(),
                author: parts.next()?.to_string(),
                committed_at: parts.next()?.parse().ok()?,
            })
        })
        .collect();

    // Same diff machinery and rename detection for both, so entry i is chunk i.
    let listing = git(dir, &["diff", "--name-status", &workspace.base_sha, "HEAD"])?;
    let patch = git(dir, &["diff", "--unified=3", &workspace.base_sha, "HEAD"])?;
    let mut chunks = split_patch_by_file(&format!("{patch}\n")).into_iter();
    let mut files = Vec::new();
    for line in listing.lines() {
        let unified = chunks.next().unwrap_or_default();
        let mut parts = line.split('\t');
        let status = parts.next().unwrap_or("").trim().to_string();
        let path = if status.starts_with('R') || status.starts_with('C') {
            parts.nth(1)
        } else {
            parts.next()
        }
        .unwrap_or("")
        .trim()
        .to_string();
        if path.is_empty() {
            continue;
        }
        let (mut additions, mut deletions) = (0usize, 0usize);
        for diff_line in unified.lines() {
            if diff_line.starts_with("+++") || diff_line.starts_with("---") {
                continue;
            }
            if diff_line.starts_with('+') {
                additions += 1;
            } else if diff_line.starts_with('-') {
                deletions += 1;
            }
        }
        let unified_diff = if unified.len() > MAX_FILE_PATCH_BYTES {
            format!(
                "diff omitted ({} bytes; +{additions} -{deletions})\n",
                unified.len()
            )
        } else {
            unified
        };
        files.push(BranchFileDiff {
            path,
            status,
            additions,
            deletions,
            unified_diff,
        });
    }

    let behind_base = workspace.base_branch.as_deref().and_then(|base| {
        let base_ref = format!("refs/heads/{base}");
        let base_tip = git(&workspace.real_repo, &["rev-parse", "--verify", &base_ref]).ok()?;
        // The owner's base tip may hold commits the clone has never seen;
        // count in the owner's repo, which has the published branch too.
        let branch_ref = format!("refs/heads/{}", workspace.branch);
        let head_ref = if git(
            &workspace.real_repo,
            &["rev-parse", "--verify", &branch_ref],
        )
        .ok()
        .as_deref()
            == Some(head_sha.as_str())
        {
            branch_ref
        } else {
            workspace.base_sha.clone()
        };
        git(
            &workspace.real_repo,
            &["rev-list", "--count", &format!("{head_ref}..{base_tip}")],
        )
        .ok()?
        .parse()
        .ok()
    });

    Ok(BranchSummary {
        root_task_id: workspace.root_task_id.clone(),
        branch: workspace.branch.clone(),
        base_branch: workspace.base_branch.clone(),
        base_sha: workspace.base_sha.clone(),
        head_sha,
        commits,
        files,
        behind_base,
        dirty: !git(dir, &["status", "--porcelain"])?.is_empty(),
        merged: workspace.merged.clone(),
        pull_request_url: workspace.pull_request_url.clone(),
    })
}

/// G19: refuse to land a branch whose diff carries a high-confidence secret.
fn secret_scan(workspace: &TrustedRunWorkspace) -> Result<(), String> {
    let patch = git(
        &workspace.workspace_dir,
        &["diff", &workspace.base_sha, "HEAD"],
    )?;
    if crate::magician_v2::agents::memory_consolidator::redact_obvious_secrets(&patch).is_some() {
        return Err(
            "secret scan failed: the branch adds what looks like a credential. Remove it (and rewrite the commit that added it) before landing."
                .to_string(),
        );
    }
    Ok(())
}

fn ensure_landable(workspace: &TrustedRunWorkspace) -> Result<String, String> {
    if !git(&workspace.workspace_dir, &["status", "--porcelain"])?.is_empty() {
        return Err(
            "the run's clone has uncommitted changes — wait for the turn to finish".to_string(),
        );
    }
    let head = publish_branch(workspace)?;
    if head == workspace.base_sha {
        return Err("the branch has no commits to land".to_string());
    }
    secret_scan(workspace)?;
    Ok(head)
}

/// Files changed between two commits of the chain's clone, with line counts
/// and no patch bodies: the light list a phone shows before it opens one file.
/// `to` may be `HEAD`.
pub fn changed_files_between(
    workspace: &TrustedRunWorkspace,
    from: &str,
    to: &str,
) -> Result<Vec<BranchFileDiff>, String> {
    let dir = &workspace.workspace_dir;
    // Same diff machinery (and rename detection) for both, so line i of one is
    // line i of the other.
    let listing = git(dir, &["diff", "--name-status", from, to])?;
    let counts = git(dir, &["diff", "--numstat", from, to])?;
    let mut counts = counts.lines();
    let mut files = Vec::new();
    for line in listing.lines() {
        let numstat = counts.next().unwrap_or_default();
        let mut parts = line.split('\t');
        let status = parts.next().unwrap_or("").trim().to_string();
        let path = if status.starts_with('R') || status.starts_with('C') {
            parts.nth(1)
        } else {
            parts.next()
        }
        .unwrap_or("")
        .trim()
        .to_string();
        if path.is_empty() {
            continue;
        }
        let mut numbers = numstat.split('\t');
        // Binary files report `-`.
        let additions = numbers.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let deletions = numbers.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        files.push(BranchFileDiff {
            path,
            status,
            additions,
            deletions,
            unified_diff: String::new(),
        });
    }
    Ok(files)
}

/// One file's unified diff between two commits of the chain's clone, and
/// whether it was cut at the review size cap.
pub fn file_patch_between(
    workspace: &TrustedRunWorkspace,
    from: &str,
    to: &str,
    path: &str,
) -> Result<(String, bool), String> {
    if path.trim().is_empty() || path.contains('\0') {
        return Err("a file path is required".to_string());
    }
    let patch = git(
        &workspace.workspace_dir,
        // A literal path: `*` or `:/` from a client must not widen the diff.
        &[
            "--literal-pathspecs",
            "diff",
            "--unified=3",
            from,
            to,
            "--",
            path,
        ],
    )?;
    if patch.len() > MAX_FILE_PATCH_BYTES {
        let mut cut = MAX_FILE_PATCH_BYTES;
        while !patch.is_char_boundary(cut) {
            cut -= 1;
        }
        return Ok((patch[..cut].to_string(), true));
    }
    Ok((patch, false))
}

/// What [`integrate_branches`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrationOutcome {
    /// Every branch is merged into the integration branch, and that branch is
    /// published to the owner's repo.
    Merged {
        workspace: TrustedRunWorkspace,
        head: String,
    },
    /// `branch` conflicts with what is already merged. The merge was aborted:
    /// the integration branch keeps every branch merged before it, and is not
    /// published.
    Conflict {
        workspace: TrustedRunWorkspace,
        branch: String,
        files: Vec<String>,
    },
}

/// Merge several published branches (a crew's lanes) into one integration
/// branch, its own trusted chain keyed by `request.root_task_id`, starting
/// from the project's base.
///
/// Each branch is fetched from the owner's repo and merged with `--no-ff`, in
/// order; a branch already merged is skipped, so a retry after a conflict was
/// resolved resumes where it stopped. Repo hooks are skipped, as for a turn's
/// bookkeeping commit. Only the integration branch's ref is ever written to
/// the owner's repo ([`publish_branch`]); landing it is the owner's
/// [`merge_into_base`] or [`open_pull_request`], with their secret scan.
pub fn integrate_branches(
    request: &TrustedRunRequest<'_>,
    branches: &[String],
) -> Result<IntegrationOutcome, String> {
    let workspace = ensure_run_workspace(request)?;
    let dir = workspace.workspace_dir.clone();
    if !git(&dir, &["status", "--porcelain"])?.is_empty() {
        return Err("the integration clone has uncommitted changes".to_string());
    }
    let real = workspace.real_repo.to_string_lossy().into_owned();
    let has_identity = git(&dir, &["config", "user.email"]).is_ok_and(|email| !email.is_empty());
    for branch in branches {
        git(
            &dir,
            &["fetch", "--quiet", &real, &format!("refs/heads/{branch}")],
        )?;
        let tip = git(&dir, &["rev-parse", "FETCH_HEAD"])?;
        if git(&dir, &["merge-base", "--is-ancestor", &tip, "HEAD"]).is_ok() {
            continue;
        }
        let message = format!("Merge {branch}");
        let mut args: Vec<&str> = Vec::new();
        if !has_identity {
            args.extend([
                "-c",
                "user.name=VibeDev",
                "-c",
                "user.email=vibedev@localhost",
            ]);
        }
        args.extend([
            "merge",
            "--no-ff",
            "--no-verify",
            "--quiet",
            "-m",
            &message,
            &tip,
        ]);
        if let Err(error) = git(&dir, &args) {
            let files: Vec<String> = git(&dir, &["diff", "--name-only", "--diff-filter=U"])
                .map(|out| out.lines().map(str::to_string).collect())
                .unwrap_or_default();
            let _ = git(&dir, &["merge", "--abort"]);
            if files.is_empty() {
                return Err(error);
            }
            return Ok(IntegrationOutcome::Conflict {
                workspace,
                branch: branch.clone(),
                files,
            });
        }
    }
    let head = publish_branch(&workspace)?;
    Ok(IntegrationOutcome::Merged { workspace, head })
}

/// Merge the chain's branch into its base branch in the owner's repo (D3).
///
/// Fast-forward when the base has not moved; otherwise a merge commit built
/// with `merge-tree`/`commit-tree`, so no checkout is needed to compute it. A
/// conflict is refused and nothing is written. When the base branch is
/// checked out, the owner's tracked files must be clean and the update goes
/// through `git merge --ff-only` so their working tree follows; otherwise only
/// the ref moves (compare-and-swap against the tip it was computed from).
pub fn merge_into_base(workspace: &mut TrustedRunWorkspace) -> Result<TrustedMergeRecord, String> {
    if let Some(done) = workspace.merged.as_ref() {
        if workspace_head(workspace)? == workspace.base_sha {
            return Err(format!(
                "already merged into `{}` as {}",
                done.target, done.merged_sha
            ));
        }
    }
    let target = workspace.base_branch.clone().ok_or_else(|| {
        "the chain started from a detached HEAD, so it has no branch to merge into; open a pull request instead"
            .to_string()
    })?;
    let head = ensure_landable(workspace)?;
    let real = workspace.real_repo.clone();
    let target_ref = format!("refs/heads/{target}");
    let tip = git(&real, &["rev-parse", "--verify", &target_ref])?;

    let fast_forward = git(&real, &["merge-base", "--is-ancestor", &tip, &head]).is_ok();
    let (new_tip, strategy) = if fast_forward {
        (head.clone(), MergeStrategy::FastForward)
    } else {
        let tree = git(&real, &["merge-tree", "--write-tree", &tip, &head]).map_err(|_| {
            format!(
                "`{}` conflicts with `{target}`; ask the run to bring its branch up to date with `{target}` and resolve the conflict",
                workspace.branch
            )
        })?;
        let tree = tree.lines().next().unwrap_or_default().to_string();
        let message = format!("Merge branch '{}' into {target}", workspace.branch);
        let commit = git(
            &real,
            &[
                "commit-tree",
                &tree,
                "-p",
                &tip,
                "-p",
                &head,
                "-m",
                &message,
            ],
        )?;
        (commit, MergeStrategy::MergeCommit)
    };

    let checked_out =
        git(&real, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok() == Some(target.clone());
    if checked_out {
        if !git(&real, &["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
            return Err(format!(
                "`{target}` is checked out with uncommitted changes; commit or stash them, then merge"
            ));
        }
        git(&real, &["merge", "--quiet", "--ff-only", &new_tip])?;
    } else {
        git(&real, &["update-ref", &target_ref, &new_tip, &tip])?;
    }

    let record = TrustedMergeRecord {
        target,
        merged_sha: new_tip,
        strategy,
        merged_at_ms: chrono::Utc::now().timestamp_millis(),
    };
    workspace.merged = Some(record.clone());
    // The landed commits are now the base: a follow-up turn's status, diff and
    // next merge cover only what it adds, and a rewind cannot undo them.
    workspace.base_sha = head;
    Ok(record)
}

/// Push the branch to the owner's `origin` and open a pull request with `gh`.
///
/// Both run with the operator's own credentials: Magician's runtime `.env`
/// keys (its `GITHUB_TOKEN` included) are removed, so `gh` uses the owner's
/// login and `git push` their credential helper. Nothing touches the owner's
/// checkout — the push is from the clone and `gh` only reads the repo's
/// remotes.
pub fn open_pull_request(
    workspace: &mut TrustedRunWorkspace,
    title: &str,
    body: &str,
) -> Result<String, String> {
    ensure_landable(workspace)?;
    let origin = git(&workspace.real_repo, &["remote", "get-url", "origin"]).map_err(|_| {
        "the repo has no `origin` remote to open a pull request against".to_string()
    })?;
    let refspec = format!("refs/heads/{0}:refs/heads/{0}", workspace.branch);
    let mut push = operator_command(
        runtime_core::process::git_program(),
        &workspace.workspace_dir,
    );
    push.args(["push", "--quiet", &origin, &refspec]);
    operator_output("git push", push, None, OPERATOR_COMMAND_TIMEOUT)
        .and_then(|output| command_result("git push", output))?;
    // An open pull request follows the branch: the push above is what brings
    // a follow-up turn's commits into it. A stored value that is not a URL
    // (an earlier read that came back empty) is no pull request.
    if let Some(url) = workspace
        .pull_request_url
        .as_ref()
        .filter(|url| is_pull_request_url(url))
    {
        return Ok(url.clone());
    }

    let mut gh = operator_command("gh", &workspace.real_repo);
    gh.args([
        "pr",
        "create",
        "--head",
        &workspace.branch,
        "--title",
        title,
        "--body-file",
        "-",
    ]);
    if let Some(base) = workspace.base_branch.as_deref() {
        gh.args(["--base", base]);
    }
    let output = operator_output(
        "gh pr create",
        gh,
        Some(body.as_bytes()),
        OPERATOR_COMMAND_TIMEOUT,
    )?;
    let printed = command_result("gh pr create", output)?;
    let url = printed
        .lines()
        .last()
        .unwrap_or_default()
        .trim()
        .to_string();
    if !is_pull_request_url(&url) {
        // Not saved: an empty or garbled read must not stand in for the pull
        // request forever.
        let shown: String = printed.chars().take(200).collect();
        return Err(format!(
            "gh pr create finished without printing a pull request URL (output: {shown:?}); \
             check the repository's pull requests before trying again"
        ));
    }
    workspace.pull_request_url = Some(url.clone());
    Ok(url)
}

/// Whether `value` reads as a pull request's web address: an `http(s)` URL
/// with a host and no whitespace.
pub fn is_pull_request_url(value: &str) -> bool {
    let rest = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"));
    rest.is_some_and(|rest| {
        let host = rest.split('/').next().unwrap_or_default();
        !host.is_empty() && !rest.chars().any(char::is_whitespace)
    })
}

fn operator_command(program: impl AsRef<std::ffi::OsStr>, dir: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .env_remove("GH_TOKEN")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    for key in
        crate::magician_v2::execution::interactive_process::operator_terminal_env_scrub_list()
    {
        cmd.env_remove(key);
    }
    cmd
}

/// Longest a `git push` or `gh pr create` may run. A hung credential helper
/// or network must not hold the caller, or the chain's admission lock it
/// holds, for good.
const OPERATOR_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Longest the output pipes are read after the command itself exited. Past
/// it, whatever the command left running in its group is killed.
const OPERATOR_PIPE_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// Kill the process group an operator command leads (unix), taking down the
/// helpers it forked (`git-remote-https`, `ssh`, credential helpers) with it.
#[cfg(unix)]
fn kill_operator_group(child: &mut std::process::Child) {
    let pid = match i32::try_from(child.id()) {
        Ok(pid) if pid > 0 => pid,
        _ => {
            let _ = child.kill();
            return;
        },
    };
    // SAFETY: plain syscall, no memory involved. The child was spawned with
    // `process_group(0)`, so its pid is the id of a group holding only it and
    // what it forked. Before `wait` reaps it the id cannot have been reused.
    // After a reap (the pipe-grace path) the id stays reserved while any
    // member lives, which is the case this kill is for; with no member left
    // the call fails with ESRCH, unless in the few seconds since, the pid was
    // recycled as a new group's leader.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_operator_group(child: &mut std::process::Child) {
    let _ = child.kill();
}

/// Run an operator command with `stdin` fed in, and kill it at `timeout`.
///
/// The command leads its own process group (unix), so a kill takes down what
/// it forked too. Both output pipes drain on their own threads, so a chatty
/// child cannot fill one and stall. After the child exits they are read for
/// at most [`OPERATOR_PIPE_GRACE`] (within the deadline); a grandchild still
/// holding one then is killed with the group. A successful command whose
/// stdout could not be read whole is an error, not an empty success.
fn operator_output(
    label: &str,
    mut cmd: Command,
    stdin: Option<&[u8]>,
    timeout: std::time::Duration,
) -> Result<std::process::Output, String> {
    use std::io::{Read, Write};
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    // Own group, so the deadline's kill reaches the helpers the command
    // forks. This does not touch PATH, so std still spawns with posix_spawn.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let deadline = Instant::now() + timeout;
    let mut child = cmd.spawn().map_err(|error| format!("{label}: {error}"))?;
    let (sender, received) = std::sync::mpsc::channel::<(bool, Vec<u8>)>();
    let drain = |pipe: Option<Box<dyn Read + Send>>, is_stdout: bool| {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            let _ = sender.send((is_stdout, bytes));
        });
    };
    drain(
        child
            .stdout
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
        true,
    );
    drain(
        child
            .stderr
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
        false,
    );
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // On its own thread: a child that never reads cannot block the
        // write past the deadline. The pipe closes when the thread ends.
        let input = input.to_vec();
        std::thread::spawn(move || {
            let _ = pipe.write_all(&input);
        });
    }
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            },
            Ok(None) => {
                kill_operator_group(&mut child);
                let _ = child.wait();
                return Err(format!(
                    "{label} did not finish within {}s and was stopped",
                    timeout.as_secs()
                ));
            },
            Err(error) => {
                kill_operator_group(&mut child);
                let _ = child.wait();
                return Err(format!("{label}: {error}"));
            },
        }
    };
    let (mut stdout, mut stderr) = (None, None);
    let mut killed = false;
    while stdout.is_none() || stderr.is_none() {
        let left = deadline
            .saturating_duration_since(Instant::now())
            .min(OPERATOR_PIPE_GRACE)
            .max(Duration::from_secs(1));
        match received.recv_timeout(if killed { Duration::from_secs(1) } else { left }) {
            Ok((true, bytes)) => stdout = Some(bytes),
            Ok((false, bytes)) => stderr = Some(bytes),
            Err(_) if !killed => {
                // A helper the command left behind still holds a pipe: kill
                // what is left of the group so the pipe closes.
                kill_operator_group(&mut child);
                killed = true;
            },
            Err(_) => break,
        }
    }
    let Some(stdout) = stdout else {
        return Err(format!(
            "{label} exited ({status}) but its output could not be read: a process it started \
             kept the pipe open"
        ));
    };
    Ok(std::process::Output {
        status,
        stdout,
        stderr: stderr.unwrap_or_default(),
    })
}

fn command_result(label: &str, output: std::process::Output) -> Result<String, String> {
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(format!(
            "{label} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Record a finished turn on the chain's record (read-modify-write of the
/// record on disk; the caller holds the chain's admission lock).
pub fn record_turn(
    scope_root: &Path,
    turn: TrustedTurnRecord,
    root_task_id: &str,
) -> Result<(), String> {
    let mut record = load_run_workspace(scope_root, root_task_id)
        .ok_or_else(|| format!("no trusted workspace record for `{root_task_id}`"))?;
    // A turn that committed past the merged base has work that is not landed.
    if turn.commit != record.base_sha {
        record.merged = None;
    }
    // A retry of the same task that ends on the same commit replaces its
    // earlier record instead of adding a duplicate.
    match record.turns.last_mut() {
        Some(last) if last.task_id == turn.task_id && last.commit == turn.commit => *last = turn,
        _ => record.turns.push(turn),
    }
    save_run_workspace(scope_root, &record)
}

/// The turn whose session a rewind to `target` should resume: the newest
/// recorded turn whose end commit is `target` or an ancestor of it — the
/// conversation as it stood when the code was at (or before) that point.
pub fn turn_for_rewind<'a>(
    workspace: &'a TrustedRunWorkspace,
    target: &str,
) -> Option<&'a TrustedTurnRecord> {
    workspace.turns.iter().rev().find(|turn| {
        turn.commit == target
            || git(
                &workspace.workspace_dir,
                &["merge-base", "--is-ancestor", &turn.commit, target],
            )
            .is_ok()
    })
}

/// Rewind the chain to one of its commits (a checkpoint), keeping anything
/// uncommitted that does not conflict (`reset --keep`), and republish the
/// branch — a rewind is the one publish that moves the ref backwards.
/// What a rewind did: the new head, and the turn whose engine session the
/// next turn should resume so the agent's context rewinds with the code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RewindOutcome {
    pub head_sha: String,
    pub resume_turn: Option<TrustedTurnRecord>,
}

pub fn rewind_to(
    workspace: &mut TrustedRunWorkspace,
    commit: &str,
) -> Result<RewindOutcome, String> {
    let dir = workspace.workspace_dir.clone();
    let target = git(
        &dir,
        &["rev-parse", "--verify", &format!("{commit}^{{commit}}")],
    )
    .map_err(|_| format!("`{commit}` is not a commit on this branch"))?;
    let on_branch = git(&dir, &["merge-base", "--is-ancestor", &target, "HEAD"]).is_ok();
    let after_base = git(
        &dir,
        &["merge-base", "--is-ancestor", &workspace.base_sha, &target],
    )
    .is_ok();
    if !on_branch || !after_base {
        return Err(format!(
            "`{commit}` is not between the chain's base and its head"
        ));
    }
    let resume_turn = turn_for_rewind(workspace, &target).cloned();
    git(&dir, &["reset", "--quiet", "--keep", &target])?;
    let source = dir.to_string_lossy().into_owned();
    let refspec = format!("+refs/heads/{0}:refs/heads/{0}", workspace.branch);
    git(
        &workspace.real_repo,
        &["fetch", "--quiet", &source, &refspec],
    )?;
    // Turns that ended past the new head are no longer on the branch.
    workspace.turns.retain(|turn| {
        git(
            &dir,
            &["merge-base", "--is-ancestor", &turn.commit, &target],
        )
        .is_ok()
    });
    Ok(RewindOutcome {
        head_sha: target,
        resume_turn,
    })
}

/// Delete the chain's clone and record, and (by default) its published branch
/// in the owner's repo. A merged chain keeps nothing worth keeping; an
/// unmerged one loses its commits, so the UI confirms first.
pub fn discard_run_workspace(
    scope_root: &Path,
    workspace: &TrustedRunWorkspace,
    delete_branch: bool,
) -> Result<(), String> {
    if delete_branch {
        let checked_out = git(
            &workspace.real_repo,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
        )
        .ok()
            == Some(workspace.branch.clone());
        if checked_out {
            return Err(format!(
                "`{}` is checked out in your repo; switch branches before discarding it",
                workspace.branch
            ));
        }
        let branch_ref = format!("refs/heads/{}", workspace.branch);
        if git(
            &workspace.real_repo,
            &["rev-parse", "--verify", "--quiet", &branch_ref],
        )
        .is_ok()
        {
            git(&workspace.real_repo, &["update-ref", "-d", &branch_ref])?;
        }
    }
    if workspace.workspace_dir.exists() {
        std::fs::remove_dir_all(&workspace.workspace_dir)
            .map_err(|error| format!("{}: {error}", workspace.workspace_dir.display()))?;
    }
    let record = record_path(&trusted_runs_root(scope_root), &workspace.root_task_id);
    match std::fs::remove_file(&record) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("{}: {error}", record.display())),
    }
}

// ---------------------------------------------------------------------------
// Disk usage (shown in the cockpit's Branch tab)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TrustedDiskUsage {
    /// The chain's clone, `.git` included (its own objects only — the
    /// borrowed ones live in the owner's repo).
    pub workspace_bytes: u64,
    /// The shared cargo build dir every coding turn builds into.
    pub build_dir_bytes: Option<u64>,
    /// Free space on the build dir's (else the clone's) filesystem.
    pub free_bytes: Option<u64>,
}

/// Bytes under `path`, not following symlinks. Unreadable entries count as
/// zero rather than failing the whole walk.
pub fn dir_size_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

/// The clone's size, the shared build dir's size, and the free space left.
/// Walks every file, so callers cache it.
pub fn disk_usage(workspace: &TrustedRunWorkspace, build_dir: Option<&Path>) -> TrustedDiskUsage {
    TrustedDiskUsage {
        workspace_bytes: dir_size_bytes(&workspace.workspace_dir),
        build_dir_bytes: build_dir.filter(|dir| dir.is_dir()).map(dir_size_bytes),
        free_bytes: crate::magician_v2::execution::coding_engine::free_disk_bytes(
            build_dir.unwrap_or(&workspace.workspace_dir),
        ),
    }
}

// ---------------------------------------------------------------------------
// Lifecycle (plan §4.10)
// ---------------------------------------------------------------------------

/// When an idle chain's clone is removed.
#[derive(Debug, Clone, Copy)]
pub struct SweepPolicy {
    /// Any chain untouched this long.
    pub idle_after: std::time::Duration,
    /// A merged chain untouched this long — it keeps nothing worth keeping.
    pub merged_idle_after: std::time::Duration,
}

impl Default for SweepPolicy {
    fn default() -> Self {
        Self {
            idle_after: std::time::Duration::from_secs(14 * 24 * 3600),
            merged_idle_after: std::time::Duration::from_secs(24 * 3600),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SweepReport {
    /// Root task ids whose clone was removed.
    pub removed: Vec<String>,
    /// Idle long enough, but kept because their latest commits exist only in
    /// the clone (unpublished) or it has uncommitted changes.
    pub kept_unpublished: Vec<String>,
}

/// When the chain last moved: the clone's ref log (every commit, rewind and
/// checkout writes it), else its index, else the record.
fn last_touched(workspace: &TrustedRunWorkspace, record: &Path) -> Option<std::time::SystemTime> {
    let git_dir = workspace.workspace_dir.join(".git");
    [
        git_dir.join("logs").join("HEAD"),
        git_dir.join("index"),
        record.to_path_buf(),
    ]
    .iter()
    .find_map(|path| {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok()
    })
}

/// Remove the clones of idle chains. **Never loses a commit**: a clone is
/// removed only when it is clean and its head is merged or published to the
/// owner's repo. The record stays, so a later follow-up rebuilds the clone from
/// the published branch ([`ensure_run_workspace`]).
pub fn sweep_run_workspaces(
    scope_root: &Path,
    policy: SweepPolicy,
    now: std::time::SystemTime,
) -> SweepReport {
    let mut report = SweepReport::default();
    let runs_root = trusted_runs_root(scope_root);
    let Ok(entries) = std::fs::read_dir(&runs_root) else {
        return report;
    };
    for entry in entries.flatten() {
        let record_path = entry.path();
        if record_path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(workspace) = std::fs::read(&record_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<TrustedRunWorkspace>(&bytes).ok())
        else {
            continue;
        };
        if !workspace.workspace_dir.join(".git").is_dir() {
            continue;
        }
        let threshold = if workspace.merged.is_some() {
            policy.merged_idle_after
        } else {
            policy.idle_after
        };
        let idle = last_touched(&workspace, &record_path)
            .and_then(|touched| now.duration_since(touched).ok())
            .unwrap_or_default();
        if idle < threshold {
            continue;
        }
        let clean = git(&workspace.workspace_dir, &["status", "--porcelain"])
            .is_ok_and(|status| status.is_empty());
        let head = workspace_head(&workspace).ok();
        let published = git(
            &workspace.real_repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{}", workspace.branch),
            ],
        )
        .ok();
        let landed =
            workspace.merged.is_some() && head.as_deref() == Some(workspace.base_sha.as_str());
        let safe = clean && head.is_some() && (landed || published == head);
        if !safe {
            report.kept_unpublished.push(workspace.root_task_id.clone());
            continue;
        }
        match std::fs::remove_dir_all(&workspace.workspace_dir) {
            Ok(()) => report.removed.push(workspace.root_task_id.clone()),
            Err(error) => tracing::warn!(
                dir = %workspace.workspace_dir.display(),
                %error,
                "trusted run sweep: could not remove the clone"
            ),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) -> String {
        git(dir, args).unwrap_or_else(|error| panic!("{error}"))
    }

    /// A real repo with one commit on `main` and an uncommitted change.
    fn real_repo(root: &Path) -> PathBuf {
        let repo = root.join("real");
        std::fs::create_dir_all(&repo).unwrap();
        run(&repo, &["init", "--quiet", "--initial-branch", "main"]);
        run(&repo, &["config", "user.name", "Owner"]);
        run(&repo, &["config", "user.email", "owner@example.com"]);
        run(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        run(&repo, &["add", "README.md"]);
        run(&repo, &["commit", "--quiet", "-m", "initial"]);
        std::fs::write(repo.join("README.md"), "hello\nuncommitted\n").unwrap();
        repo.canonicalize().unwrap()
    }

    fn request<'a>(scope: &'a Path, repo: &'a Path, root: &'a str) -> TrustedRunRequest<'a> {
        TrustedRunRequest {
            scope_root: scope,
            root_task_id: root,
            project_id: "proj-1",
            project_name: "My App!",
            real_repo: repo,
            base_branch: None,
        }
    }

    #[test]
    fn branch_names_are_namespaced_slugs() {
        assert_eq!(
            trusted_branch_name("My App!", "task_0123456789abcdef"),
            "vibedev/my-app-01234567"
        );
        assert_eq!(trusted_branch_name("", "task_abc"), "vibedev/run-abc");
        assert_eq!(
            trusted_branch_name("My App!", "crew_89abcdef01234567"),
            "vibedev/my-app-crew-89abcdef"
        );
    }

    #[test]
    fn base_branch_files_lists_the_committed_tree_of_the_base_branch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = real_repo(tmp.path());
        run(&repo, &["checkout", "--quiet", "-b", "feature"]);
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/app.js"), "app\n").unwrap();
        run(&repo, &["add", "src/app.js"]);
        run(&repo, &["commit", "--quiet", "-m", "feature"]);
        // Never committed: not part of any branch.
        std::fs::write(repo.join("scratch.txt"), "x\n").unwrap();

        assert_eq!(
            base_branch_files(&repo, Some("main")).unwrap(),
            vec!["README.md"]
        );
        // No base branch: the repo's current branch.
        assert_eq!(
            base_branch_files(&repo, None).unwrap(),
            vec!["README.md", "src/app.js"]
        );
        assert!(base_branch_files(&repo, Some("missing")).is_err());
        assert!(base_branch_files(&tmp.path().join("nowhere"), None).is_err());
    }

    #[test]
    fn a_chain_gets_a_clone_on_its_own_branch_from_the_committed_base() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = real_repo(tmp.path());
        let scope = tmp.path().join("scope");
        let ws = ensure_run_workspace(&request(&scope, &repo, "task_aaaa1111")).expect("ensure");

        assert_eq!(ws.branch, "vibedev/my-app-aaaa1111");
        assert_eq!(ws.base_branch.as_deref(), Some("main"));
        assert_eq!(ws.base_sha, run(&repo, &["rev-parse", "main"]));
        assert!(
            ws.workspace_dir.join(".git").is_dir(),
            "its own .git, not a worktree file"
        );
        assert_eq!(
            run(&ws.workspace_dir, &["symbolic-ref", "--short", "HEAD"]),
            ws.branch
        );
        // The committed tip, without the owner's uncommitted edit (D2).
        assert_eq!(
            std::fs::read_to_string(ws.workspace_dir.join("README.md")).unwrap(),
            "hello\n"
        );
        // Objects are borrowed, not copied.
        assert!(ws
            .workspace_dir
            .join(".git/objects/info/alternates")
            .is_file());
        // A follow-up turn gets the same workspace.
        assert_eq!(
            ensure_run_workspace(&request(&scope, &repo, "task_aaaa1111")).unwrap(),
            ws
        );
    }

    #[test]
    fn a_turn_ends_on_a_commit_and_publishing_never_touches_the_owner_checkout() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = real_repo(tmp.path());
        let scope = tmp.path().join("scope");
        let ws = ensure_run_workspace(&request(&scope, &repo, "task_bbbb2222")).unwrap();
        let owner_head = run(&repo, &["rev-parse", "HEAD"]);
        let owner_status = run(&repo, &["status", "--porcelain"]);

        std::fs::write(ws.workspace_dir.join("feature.txt"), "new\n").unwrap();
        // An attachment dropped where run_coding_task materializes them.
        let attachments = ws.workspace_dir.join(".cache/magician/vibedev_attachments");
        std::fs::create_dir_all(&attachments).unwrap();
        std::fs::write(attachments.join("spec.pdf"), "pdf").unwrap();

        let commit = commit_pending_changes(&ws, "vibedev: end of turn 1")
            .unwrap()
            .expect("a commit");
        assert_eq!(commits_ahead(&ws).unwrap(), 1);
        let tracked = run(&ws.workspace_dir, &["ls-files"]);
        assert!(tracked.contains("feature.txt"));
        assert!(
            !tracked.contains("spec.pdf"),
            "attachments are never committed"
        );
        assert_eq!(commit_pending_changes(&ws, "noop").unwrap(), None);

        assert_eq!(publish_branch(&ws).unwrap(), commit);
        assert_eq!(
            run(&repo, &["rev-parse", &format!("refs/heads/{}", ws.branch)]),
            commit
        );
        assert_eq!(run(&repo, &["rev-parse", "HEAD"]), owner_head);
        assert_eq!(run(&repo, &["status", "--porcelain"]), owner_status);
        assert_eq!(run(&repo, &["symbolic-ref", "--short", "HEAD"]), "main");
    }

    #[test]
    fn a_lost_clone_is_rebuilt_from_the_published_branch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = real_repo(tmp.path());
        let scope = tmp.path().join("scope");
        let ws = ensure_run_workspace(&request(&scope, &repo, "task_cccc3333")).unwrap();
        std::fs::write(ws.workspace_dir.join("kept.txt"), "kept\n").unwrap();
        let commit = commit_pending_changes(&ws, "turn 1").unwrap().unwrap();
        publish_branch(&ws).unwrap();

        std::fs::remove_dir_all(&ws.workspace_dir).unwrap();
        let rebuilt = ensure_run_workspace(&request(&scope, &repo, "task_cccc3333")).unwrap();
        assert_eq!(rebuilt, ws);
        assert_eq!(workspace_head(&rebuilt).unwrap(), commit);
        assert!(rebuilt.workspace_dir.join("kept.txt").is_file());
    }

    /// A chain with one committed turn, published, on a clean real repo.
    fn landed_turn(tmp: &Path, root: &str) -> (PathBuf, PathBuf, TrustedRunWorkspace) {
        let repo = real_repo(tmp);
        run(&repo, &["checkout", "--quiet", "--", "README.md"]);
        let scope = tmp.join("scope");
        let ws = ensure_run_workspace(&request(&scope, &repo, root)).unwrap();
        std::fs::write(ws.workspace_dir.join("feature.txt"), "feature\n").unwrap();
        commit_pending_changes(&ws, "add feature").unwrap();
        publish_branch(&ws).unwrap();
        (repo, scope, ws)
    }

    #[test]
    fn the_branch_summary_lists_commits_and_the_diff_against_the_base() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_repo, _scope, ws) = landed_turn(tmp.path(), "task_dddd4444");
        let summary = branch_summary(&ws).unwrap();
        assert_eq!(summary.commits.len(), 1);
        assert_eq!(summary.commits[0].subject, "add feature");
        assert_eq!(summary.head_sha, summary.commits[0].sha);
        assert_eq!(summary.files.len(), 1);
        assert_eq!(summary.files[0].path, "feature.txt");
        assert_eq!(summary.files[0].status, "A");
        assert_eq!(
            (summary.files[0].additions, summary.files[0].deletions),
            (1, 0)
        );
        assert!(summary.files[0]
            .unified_diff
            .starts_with("diff --git a/feature.txt"));
        assert_eq!(summary.behind_base, Some(0));
        assert!(!summary.dirty);
    }

    #[test]
    fn merge_fast_forwards_a_checked_out_base_and_refuses_a_dirty_one() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (repo, _scope, mut ws) = landed_turn(tmp.path(), "task_eeee5555");
        std::fs::write(repo.join("README.md"), "owner edit\n").unwrap();
        let err = merge_into_base(&mut ws).unwrap_err();
        assert!(err.contains("uncommitted changes"), "{err}");
        assert!(ws.merged.is_none());

        run(&repo, &["checkout", "--quiet", "--", "README.md"]);
        let merged = merge_into_base(&mut ws).unwrap();
        assert_eq!(merged.strategy, MergeStrategy::FastForward);
        assert_eq!(run(&repo, &["rev-parse", "main"]), merged.merged_sha);
        assert!(
            repo.join("feature.txt").is_file(),
            "the working tree follows"
        );
        assert!(merge_into_base(&mut ws)
            .unwrap_err()
            .contains("already merged"));
    }

    #[test]
    fn a_retried_turn_on_the_same_commit_replaces_its_record() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_repo, scope, ws) = landed_turn(tmp.path(), "task_cdcd3434");
        save_run_workspace(&scope, &ws).unwrap();
        let head = workspace_head(&ws).unwrap();
        let turn = |task: &str, session: Option<&str>| TrustedTurnRecord {
            commit: head.clone(),
            task_id: task.to_string(),
            engine: "grok_acp".to_string(),
            native_session_id: session.map(str::to_string),
            at_ms: 0,
        };
        record_turn(&scope, turn("task_follow", None), "task_cdcd3434").unwrap();
        record_turn(&scope, turn("task_follow", Some("sess-1")), "task_cdcd3434").unwrap();
        let turns = load_run_workspace(&scope, "task_cdcd3434").unwrap().turns;
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].native_session_id.as_deref(), Some("sess-1"));
        // A different task at the same commit is its own turn.
        record_turn(&scope, turn("task_next", Some("sess-2")), "task_cdcd3434").unwrap();
        assert_eq!(
            load_run_workspace(&scope, "task_cdcd3434")
                .unwrap()
                .turns
                .len(),
            2
        );
    }

    #[test]
    fn a_follow_up_after_a_merge_reviews_and_lands_only_its_own_commits() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (repo, scope, mut ws) = landed_turn(tmp.path(), "task_abab1212");
        let first = merge_into_base(&mut ws).unwrap();
        save_run_workspace(&scope, &ws).unwrap();
        assert_eq!(ws.base_sha, first.merged_sha);
        assert_eq!(commits_ahead(&ws).unwrap(), 0);

        // The next turn in the chain commits again on the same branch.
        std::fs::write(ws.workspace_dir.join("feature.txt"), "feature v2\n").unwrap();
        commit_pending_changes(&ws, "refine feature").unwrap();
        publish_branch(&ws).unwrap();
        let head = workspace_head(&ws).unwrap();
        record_turn(
            &scope,
            TrustedTurnRecord {
                commit: head.clone(),
                task_id: "task_follow".to_string(),
                engine: "pi".to_string(),
                native_session_id: None,
                at_ms: 0,
            },
            "task_abab1212",
        )
        .unwrap();
        let mut ws = load_run_workspace(&scope, "task_abab1212").unwrap();
        assert!(ws.merged.is_none(), "new commits are not landed yet");
        assert_eq!(commits_ahead(&ws).unwrap(), 1);
        let summary = branch_summary(&ws).unwrap();
        assert_eq!(summary.commits.len(), 1);
        assert_eq!(summary.commits[0].subject, "refine feature");

        let second = merge_into_base(&mut ws).unwrap();
        assert_eq!(second.merged_sha, head);
        assert_eq!(run(&repo, &["rev-parse", "main"]), head);
        assert!(merge_into_base(&mut ws)
            .unwrap_err()
            .contains("already merged"));
    }

    #[test]
    fn merge_makes_a_merge_commit_when_the_base_moved_and_only_moves_the_ref_when_not_checked_out()
    {
        let tmp = tempfile::TempDir::new().unwrap();
        let (repo, _scope, mut ws) = landed_turn(tmp.path(), "task_ffff6666");
        std::fs::write(repo.join("other.txt"), "owner\n").unwrap();
        run(&repo, &["add", "other.txt"]);
        run(&repo, &["commit", "--quiet", "-m", "owner work"]);
        let owner_tip = run(&repo, &["rev-parse", "main"]);
        run(&repo, &["checkout", "--quiet", "-b", "elsewhere"]);
        assert_eq!(branch_summary(&ws).unwrap().behind_base, Some(1));

        let merged = merge_into_base(&mut ws).unwrap();
        assert_eq!(merged.strategy, MergeStrategy::MergeCommit);
        let parents = run(&repo, &["rev-list", "--parents", "-n", "1", "main"]);
        assert!(parents.contains(&owner_tip), "{parents}");
        assert!(parents.contains(&workspace_head(&ws).unwrap()), "{parents}");
        assert_eq!(
            run(&repo, &["symbolic-ref", "--short", "HEAD"]),
            "elsewhere",
            "the owner's checkout did not move"
        );
        assert!(!repo.join("feature.txt").exists());
    }

    #[test]
    fn a_conflicting_merge_is_refused_and_writes_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = real_repo(tmp.path());
        run(&repo, &["checkout", "--quiet", "--", "README.md"]);
        let scope = tmp.path().join("scope");
        let mut ws = ensure_run_workspace(&request(&scope, &repo, "task_99990000")).unwrap();
        std::fs::write(ws.workspace_dir.join("README.md"), "from the run\n").unwrap();
        commit_pending_changes(&ws, "run edit").unwrap();
        std::fs::write(repo.join("README.md"), "from the owner\n").unwrap();
        run(&repo, &["commit", "--quiet", "-am", "owner edit"]);
        let tip = run(&repo, &["rev-parse", "main"]);

        let err = merge_into_base(&mut ws).unwrap_err();
        assert!(err.contains("conflicts"), "{err}");
        assert_eq!(run(&repo, &["rev-parse", "main"]), tip);
        assert!(ws.merged.is_none());
    }

    fn lane(tmp: &Path, repo: &Path, root: &str, file: &str, text: &str) -> String {
        let scope = tmp.join("scope");
        let ws = ensure_run_workspace(&request(&scope, repo, root)).unwrap();
        std::fs::write(ws.workspace_dir.join(file), text).unwrap();
        commit_pending_changes(&ws, &format!("lane {root}")).unwrap();
        publish_branch(&ws).unwrap();
        ws.branch
    }

    #[test]
    fn lanes_merge_into_one_published_branch_and_a_conflict_stops_cleanly() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = real_repo(tmp.path());
        run(&repo, &["checkout", "--quiet", "--", "README.md"]);
        let scope = tmp.path().join("scope");
        let api = lane(tmp.path(), &repo, "task_aaaa0001", "api.txt", "api\n");
        let ui = lane(tmp.path(), &repo, "task_bbbb0002", "ui.txt", "ui\n");

        let crew = request(&scope, &repo, "crew_cccc0003");
        let merged = integrate_branches(&crew, &[api.clone(), ui.clone()]).unwrap();
        let IntegrationOutcome::Merged { workspace, head } = merged else {
            panic!("expected a clean merge");
        };
        assert_eq!(run(&repo, &["rev-parse", &workspace.branch]), head);
        let files = run(&repo, &["ls-tree", "--name-only", &workspace.branch]);
        assert!(
            files.contains("api.txt") && files.contains("ui.txt"),
            "{files}"
        );
        // A retry merges nothing new and lands on the same head.
        let again = integrate_branches(&crew, &[api.clone(), ui.clone()]).unwrap();
        assert!(matches!(again, IntegrationOutcome::Merged { head: same, .. } if same == head));

        // A lane that edits the same lines as an earlier lane conflicts.
        let first = lane(tmp.path(), &repo, "task_dddd0004", "README.md", "first\n");
        let second = lane(tmp.path(), &repo, "task_eeee0005", "README.md", "second\n");
        let other = request(&scope, &repo, "crew_ffff0006");
        let outcome = integrate_branches(&other, &[first, second.clone()]).unwrap();
        let IntegrationOutcome::Conflict {
            workspace,
            branch,
            files,
        } = outcome
        else {
            panic!("expected a conflict");
        };
        assert_eq!(branch, second);
        assert_eq!(files, vec!["README.md".to_string()]);
        assert!(run(&workspace.workspace_dir, &["status", "--porcelain"]).is_empty());
        assert!(
            git(
                &repo,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{}", workspace.branch)
                ]
            )
            .is_err(),
            "a conflicted integration is not published"
        );
    }

    #[test]
    fn a_range_lists_files_with_counts_and_serves_one_patch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_repo, _scope, ws) = landed_turn(tmp.path(), "task_aaaa1111");
        let files = changed_files_between(&ws, &ws.base_sha, "HEAD").unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "feature.txt");
        assert_eq!(files[0].status, "A");
        assert_eq!((files[0].additions, files[0].deletions), (1, 0));
        assert!(files[0].unified_diff.is_empty());
        let (patch, truncated) =
            file_patch_between(&ws, &ws.base_sha, "HEAD", "feature.txt").unwrap();
        assert!(patch.contains("+feature"), "{patch}");
        assert!(!truncated);
        assert!(file_patch_between(&ws, &ws.base_sha, "HEAD", " ").is_err());
    }

    #[test]
    fn a_non_ascii_path_round_trips_and_a_pattern_matches_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_repo, _scope, ws) = landed_turn(tmp.path(), "task_bbbb2222");
        std::fs::write(ws.workspace_dir.join("caf\u{e9}.txt"), "cafe\n").unwrap();
        commit_pending_changes(&ws, "add cafe").unwrap();
        let files = changed_files_between(&ws, &ws.base_sha, "HEAD").unwrap();
        assert!(
            files.iter().any(|file| file.path == "caf\u{e9}.txt"),
            "{files:?}"
        );
        let (patch, _) = file_patch_between(&ws, &ws.base_sha, "HEAD", "caf\u{e9}.txt").unwrap();
        assert!(patch.contains("+cafe"), "{patch}");
        let (wide, _) = file_patch_between(&ws, &ws.base_sha, "HEAD", "*").unwrap();
        assert!(
            wide.is_empty(),
            "a pathspec pattern widened the diff: {wide}"
        );
    }

    #[test]
    fn a_branch_carrying_a_credential_is_not_landed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (repo, _scope, mut ws) = landed_turn(tmp.path(), "task_12121212");
        // Assembled at runtime so this source file carries no key-shaped text.
        let key = format!("{}{}", "AKIA", "ABCDEFGHIJKLMNOP");
        std::fs::write(
            ws.workspace_dir.join("config.env"),
            format!("AWS_KEY={key}\n"),
        )
        .unwrap();
        commit_pending_changes(&ws, "oops").unwrap();
        let tip = run(&repo, &["rev-parse", "main"]);
        let err = merge_into_base(&mut ws).unwrap_err();
        assert!(err.contains("secret scan"), "{err}");
        assert_eq!(run(&repo, &["rev-parse", "main"]), tip);
    }

    #[test]
    fn rewind_moves_the_branch_back_to_a_checkpoint_and_republishes_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (repo, _scope, mut ws) = landed_turn(tmp.path(), "task_34343434");
        let first = workspace_head(&ws).unwrap();
        std::fs::write(ws.workspace_dir.join("second.txt"), "2\n").unwrap();
        commit_pending_changes(&ws, "second").unwrap();
        publish_branch(&ws).unwrap();

        assert_eq!(rewind_to(&mut ws, &first).unwrap().head_sha, first);
        assert_eq!(workspace_head(&ws).unwrap(), first);
        assert!(!ws.workspace_dir.join("second.txt").exists());
        assert_eq!(
            run(&repo, &["rev-parse", &format!("refs/heads/{}", ws.branch)]),
            first
        );
        assert!(rewind_to(&mut ws, &run(&repo, &["rev-parse", "main~0"]))
            .is_ok_and(|outcome| outcome.head_sha == ws.base_sha));
        assert!(rewind_to(&mut ws, "no-such-commit").is_err());
    }

    #[test]
    fn a_rewind_resumes_the_session_of_the_turn_at_that_commit_and_forgets_later_turns() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_repo, scope, mut ws) = landed_turn(tmp.path(), "task_78787878");
        let turn = |commit: String, session: &str| TrustedTurnRecord {
            commit,
            task_id: "task_x".to_string(),
            engine: "pi".to_string(),
            native_session_id: Some(session.to_string()),
            at_ms: 0,
        };
        let first = workspace_head(&ws).unwrap();
        record_turn(&scope, turn(first.clone(), "sess-1"), &ws.root_task_id).unwrap();
        std::fs::write(ws.workspace_dir.join("mid.txt"), "m\n").unwrap();
        let mid = commit_pending_changes(&ws, "engine commit mid-turn")
            .unwrap()
            .unwrap();
        std::fs::write(ws.workspace_dir.join("end.txt"), "e\n").unwrap();
        let second = commit_pending_changes(&ws, "turn 2 end").unwrap().unwrap();
        record_turn(&scope, turn(second, "sess-2"), &ws.root_task_id).unwrap();
        publish_branch(&ws).unwrap();
        ws = load_run_workspace(&scope, &ws.root_task_id).unwrap();
        assert_eq!(ws.turns.len(), 2);

        // A commit inside turn 2 resumes the conversation as turn 1 left it.
        let outcome = rewind_to(&mut ws, &mid).unwrap();
        assert_eq!(
            outcome
                .resume_turn
                .and_then(|t| t.native_session_id)
                .as_deref(),
            Some("sess-1")
        );
        assert_eq!(ws.turns.len(), 1, "turn 2 is no longer on the branch");
        assert_eq!(ws.turns[0].commit, first);
    }

    #[test]
    fn discard_removes_the_clone_record_and_published_branch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (repo, scope, ws) = landed_turn(tmp.path(), "task_56565656");
        discard_run_workspace(&scope, &ws, true).unwrap();
        assert!(!ws.workspace_dir.exists());
        assert!(load_run_workspace(&scope, &ws.root_task_id).is_none());
        assert!(git(
            &repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{}", ws.branch)
            ]
        )
        .is_err());
    }

    #[test]
    fn the_sweep_removes_only_idle_clones_whose_commits_are_safe_elsewhere() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (repo, scope, published) = landed_turn(tmp.path(), "task_aa11aa11");
        // A second chain whose latest commit was never published.
        let unpublished = ensure_run_workspace(&request(&scope, &repo, "task_bb22bb22")).unwrap();
        std::fs::write(unpublished.workspace_dir.join("only-here.txt"), "x\n").unwrap();
        commit_pending_changes(&unpublished, "never published").unwrap();

        let policy = SweepPolicy {
            idle_after: std::time::Duration::from_secs(3600),
            merged_idle_after: std::time::Duration::from_secs(60),
        };
        let now = std::time::SystemTime::now();
        // Fresh: nothing goes.
        assert_eq!(
            sweep_run_workspaces(&scope, policy, now),
            SweepReport::default()
        );

        let later = now + std::time::Duration::from_secs(2 * 3600);
        let report = sweep_run_workspaces(&scope, policy, later);
        assert_eq!(report.removed, vec![published.root_task_id.clone()]);
        assert_eq!(
            report.kept_unpublished,
            vec![unpublished.root_task_id.clone()]
        );
        assert!(!published.workspace_dir.exists());
        assert!(unpublished.workspace_dir.join("only-here.txt").is_file());

        // The record survived, so the next turn gets the clone back at the
        // published head.
        let rebuilt = ensure_run_workspace(&request(&scope, &repo, "task_aa11aa11")).unwrap();
        assert_eq!(
            workspace_head(&rebuilt).unwrap(),
            run(
                &repo,
                &["rev-parse", &format!("refs/heads/{}", published.branch)]
            )
        );
    }

    #[test]
    fn disk_usage_counts_the_clone_and_the_build_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_repo, _scope, ws) = landed_turn(tmp.path(), "task_90909090");
        let build = tmp.path().join("build");
        std::fs::create_dir_all(build.join("debug")).unwrap();
        std::fs::write(build.join("debug/big"), vec![0u8; 4096]).unwrap();
        let usage = disk_usage(&ws, Some(&build));
        assert!(usage.workspace_bytes > 0);
        assert_eq!(usage.build_dir_bytes, Some(4096));
        assert_eq!(disk_usage(&ws, None).build_dir_bytes, None);
    }

    #[test]
    fn only_a_repository_root_can_be_trusted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = real_repo(tmp.path());
        let sub = repo.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let err = validate_trusted_repo(&sub).unwrap_err();
        assert!(
            matches!(err, TrustedRepoError::InsideRepository { .. }),
            "{err}"
        );
        assert!(err.to_string().contains("repository root"), "{err}");
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert!(matches!(
            validate_trusted_repo(&plain).unwrap_err(),
            TrustedRepoError::NotARepository { .. }
        ));
        assert!(matches!(
            validate_trusted_repo(&tmp.path().join("gone")).unwrap_err(),
            TrustedRepoError::NotADirectory { .. }
        ));
        assert!(safe_segment("../escape").is_err());
    }

    use crate::magician_v2::vibedev::projects::VibeDevProjectRecord;

    fn trust_project(trust: &str, repo_path: &Path) -> VibeDevProjectRecord {
        serde_json::from_value(serde_json::json!({
            "project_id": "p-1", "name": "Rehearsal", "chat_thread_id": "vibedev",
            "chat_session_id": "s-1", "created_at_ms": 0, "updated_at_ms": 0,
            "trust": trust, "repo_path": repo_path.display().to_string()
        }))
        .unwrap()
    }

    #[test]
    fn effective_trust_mode_follows_what_the_dispatcher_can_do() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = real_repo(tmp.path());
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let mode =
            |project: &VibeDevProjectRecord, has_ws: bool, depth: usize, plan: bool, git: bool| {
                effective_trust_mode_with_git(project, tmp.path(), has_ws, depth, plan, git)
            };

        let trusted_repo = trust_project("trusted", &repo);
        assert_eq!(
            mode(&trusted_repo, false, 0, false, true),
            EffectiveTrustMode::Trusted
        );
        // A plan run or a follow-up of a sandboxed chain never starts a clone.
        assert_eq!(
            mode(&trusted_repo, false, 0, true, true),
            EffectiveTrustMode::Sandboxed
        );
        assert_eq!(
            mode(&trusted_repo, false, 2, false, true),
            EffectiveTrustMode::Sandboxed
        );

        let trusted_plain = trust_project("trusted", &plain);
        let fallback = mode(&trusted_plain, false, 0, false, true);
        let EffectiveTrustMode::SandboxedFallback { reason, remedy } = &fallback else {
            panic!("expected a fallback, got {fallback:?}");
        };
        assert!(reason.contains("is not a git repository"), "{reason}");
        assert_eq!(*remedy, "the folder is a git repository");
        let note = fallback.fallback_note("Rehearsal").unwrap();
        assert!(
            note.contains("sandboxed copy") && note.contains("for review"),
            "{note}"
        );

        let sub = repo.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let inside = mode(&trust_project("trusted", &sub), false, 0, false, true);
        assert!(
            matches!(
                inside,
                EffectiveTrustMode::SandboxedFallback {
                    remedy: "the project points at that repository's root folder",
                    ..
                }
            ),
            "{inside:?}"
        );

        let no_git = mode(&trusted_repo, false, 0, false, false);
        let EffectiveTrustMode::SandboxedFallback { reason, .. } = &no_git else {
            panic!("expected a fallback, got {no_git:?}");
        };
        assert!(reason.contains("no git"), "{reason}");

        // A chain keeps the mode it started with.
        assert_eq!(
            mode(&trusted_plain, true, 3, false, false),
            EffectiveTrustMode::Trusted
        );
        let sandboxed = trust_project("sandboxed", &repo);
        assert_eq!(
            mode(&sandboxed, false, 0, false, true),
            EffectiveTrustMode::Sandboxed
        );
        assert_eq!(
            mode(&sandboxed, true, 0, false, true),
            EffectiveTrustMode::Trusted
        );
        assert_eq!(EffectiveTrustMode::Trusted.fallback_note("x"), None);
    }

    #[test]
    fn a_project_folder_gets_a_branch_only_at_a_repository_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = real_repo(tmp.path());
        assert_eq!(
            project_folder_git_with(&repo, true),
            ProjectFolderGit::RepositoryRoot(repo.canonicalize().unwrap())
        );

        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let ProjectFolderGit::NoBranch(why) = project_folder_git_with(&plain, true) else {
            panic!("a plain folder has no branch");
        };
        assert!(why.contains("is not a git repository"), "{why}");

        let sub = repo.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let ProjectFolderGit::NoBranch(why) = project_folder_git_with(&sub, true) else {
            panic!("a folder inside a repository has no branch of its own");
        };
        assert!(
            why.contains("inside the git repository") && why.contains("outside the project"),
            "{why}"
        );

        let ProjectFolderGit::NoBranch(why) = project_folder_git_with(&repo, false) else {
            panic!("no git, no branch");
        };
        assert!(why.contains("no git"), "{why}");
    }

    #[test]
    fn project_repo_folder_resolves_like_a_run() {
        let ws = Path::new("/ws");
        assert_eq!(project_repo_folder(None, ws), PathBuf::from("/ws"));
        assert_eq!(project_repo_folder(Some(" . "), ws), PathBuf::from("/ws"));
        assert_eq!(
            project_repo_folder(Some("app"), ws),
            PathBuf::from("/ws/app")
        );
        assert_eq!(
            project_repo_folder(Some("/abs/app"), ws),
            PathBuf::from("/abs/app")
        );
    }

    #[test]
    fn capped_patch_keeps_whole_files_and_says_what_it_left_out() {
        let one = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-x\n+y\n";
        let two = "diff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-p\n+q\n";
        let patch = format!("{one}{two}");
        assert_eq!(cap_patch(&patch, 10_000), patch);
        let capped = cap_patch(&patch, one.len() + 5);
        assert!(capped.starts_with(one));
        assert!(!capped.contains("b.txt"));
        assert!(capped.ends_with("1 file(s) not shown\n"));
        let cut = cap_patch(&patch, 20);
        assert!(cut.starts_with("diff --git a/a.txt b"));
        assert!(cut.contains("rest of this file's diff omitted"));
    }

    #[test]
    fn branch_facts_name_files_cleanliness_and_merge_state() {
        let facts = branch_fact_sentences(&serde_json::json!({
            "changed_files": ["docs/a.md"],
            "working_tree_clean": true,
            "merged": false,
            "pull_request_url": null,
        }));
        assert_eq!(
            facts,
            "The branch changes 1 file against the base: `docs/a.md`. The run's working tree is clean: everything is committed. The branch is not merged and no pull request has been opened."
        );
        let many: Vec<String> = (0..23).map(|i| format!("f{i}.rs")).collect();
        let facts = branch_fact_sentences(&serde_json::json!({
            "changed_files": many,
            "working_tree_clean": false,
            "merged": false,
            "pull_request_url": "https://example.test/pr/1",
        }));
        assert!(facts.starts_with("The branch changes 23 files against the base: `f0.rs`"));
        assert!(facts.contains("and 3 more."));
        assert!(facts.contains("still has uncommitted changes"));
        assert!(facts.ends_with("a pull request is open: https://example.test/pr/1."));
        // An older status object without these fields adds nothing.
        assert_eq!(
            branch_fact_sentences(&serde_json::json!({"branch": "b"})),
            ""
        );
    }

    #[test]
    fn an_operator_command_gets_its_input_and_is_killed_at_its_deadline() {
        let mut cat = Command::new("cat");
        cat.current_dir(std::env::temp_dir());
        let output = operator_output(
            "cat",
            cat,
            Some(b"pull request body"),
            std::time::Duration::from_secs(10),
        )
        .expect("cat runs");
        assert_eq!(command_result("cat", output).unwrap(), "pull request body");

        let mut sleep = Command::new("sleep");
        sleep.arg("30");
        let started = std::time::Instant::now();
        let error = operator_output(
            "git push",
            sleep,
            None,
            std::time::Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(error.contains("git push did not finish"), "{error}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[test]
    fn a_helper_left_holding_the_pipe_is_killed_with_the_command_group() {
        // `sleep` stays in the shell's group and holds stdout after the shell
        // exits, the way a credential helper can.
        let mut sh = Command::new("sh");
        sh.args(["-c", "sleep 30 & echo done"]);
        let started = std::time::Instant::now();
        let output = operator_output("git push", sh, None, std::time::Duration::from_secs(2))
            .expect("the output is read once the group is killed");
        assert_eq!(command_result("git push", output).unwrap(), "done");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn only_a_web_address_counts_as_a_pull_request() {
        assert!(is_pull_request_url("https://github.com/o/r/pull/12"));
        assert!(is_pull_request_url("http://git.local/o/r/pull/1"));
        assert!(!is_pull_request_url(""));
        assert!(!is_pull_request_url("https://"));
        assert!(!is_pull_request_url(
            "Creating pull request for b into main"
        ));
        assert!(!is_pull_request_url("https://github.com/o/r/pull/12 extra"));
        assert!(!is_pull_request_url("ftp://github.com/o/r/pull/12"));
    }
}

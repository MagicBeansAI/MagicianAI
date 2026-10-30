//! # Native Executors for Non-Browser Actions
//!
//! This module provides native Rust executors for File, HTTP, and Bash actions.
//! These execute directly without MCP overhead, providing 10-100x faster performance.
//!
//! ## Performance Comparison
//! | Operation | RPC Tool Call | Native Rust |
//! |-----------|---------------|-------------|
//! | File write | ~50-100ms | ~1-5ms |
//! | HTTP GET | ~100-200ms | ~10-50ms |
//! | Bash command | ~100-150ms | ~10-30ms |

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use runtime_core::{FileSandboxConfig, FileSandboxMode, ShellSandboxConfig, ShellSandboxMode};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::timeout;
use tracing::{debug, info, warn};

use super::actions::{ActionResult, BashAction, FileAction, HttpAction, HttpMethod};
use super::ExecutionError;
use crate::magician_v2::analytics::llm_tool_lineage::LlmToolLineageIdentity;
use crate::magician_v2::realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent};

// ============================================================================
// File Executor
// ============================================================================

tokio::task_local! {
    /// Supplied by the dispatcher from runtime identity, never from tool arguments.
    pub(crate) static TASK_SHELL_ROOT: PathBuf;
}

tokio::task_local! {
    /// The agent this dispatch acts as, from runtime identity (the active
    /// owner before the context's agent id), never from tool arguments. Set
    /// for every agentic action and every compiled-tool dispatch; `None`, or
    /// not set at all (a spawned task, a runtime path), is no agent: a
    /// non-member of every goal workspace (`goal_workspace_access`).
    pub(crate) static ACTING_AGENT: Option<String>;
}

tokio::task_local! {
    /// The agents whose goal workspace READ access this dispatch carries,
    /// because they delegated to the acting agent
    /// (`goal_workspace_access::read_delegators_for`). Set beside
    /// [`ACTING_AGENT`] from server-set identity; not set is none.
    pub(crate) static READ_DELEGATORS: Vec<String>;
}

/// The agents the current dispatch carries read access from, or none.
pub(crate) fn current_read_delegators() -> Vec<String> {
    READ_DELEGATORS.try_with(Clone::clone).unwrap_or_default()
}

/// Run `fut` carrying `delegators`' goal workspace read access
/// ([`READ_DELEGATORS`]).
pub async fn with_read_delegators<F: std::future::Future>(
    delegators: Vec<String>,
    fut: F,
) -> F::Output {
    READ_DELEGATORS.scope(delegators, fut).await
}

/// Run `fut` acting as `agent` (the [`ACTING_AGENT`] scope). Public so code
/// compiled into another crate's module tree (the plane turn engine in the
/// harness contract test) can set it without the crate-private static.
pub async fn with_acting_agent<F: std::future::Future>(agent: Option<String>, fut: F) -> F::Output {
    ACTING_AGENT.scope(agent, fut).await
}

/// The agent the current dispatch acts as, or `None` (no agent).
pub(crate) fn current_acting_agent() -> Option<String> {
    ACTING_AGENT.try_with(Clone::clone).ok().flatten()
}

/// The acting agent for an agentic action: the active owner first, so a
/// delegated child that kept its parent's `agent_id` acts as itself.
pub(crate) fn acting_agent_from_context(
    active_owner_agent_id: Option<&str>,
    agent_id: Option<&str>,
) -> Option<String> {
    active_owner_agent_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .or_else(|| agent_id.map(str::trim).filter(|id| !id.is_empty()))
        .map(str::to_string)
}

/// The acting agent for a compiled-tool dispatch claiming `claimed`. The one
/// already in scope wins: it was set by the agentic dispatch from the run's
/// active owner, which is authoritative, while `claimed` comes from the run
/// identity's agent id and can lag behind it. A scope holding no agent
/// stays no agent, so a dispatch never widens identity. `claimed` applies
/// only when nothing is in scope (chat, runtime callers).
pub(crate) fn compiled_acting_agent(claimed: Option<&str>) -> Option<String> {
    let claimed = claimed.map(str::trim).filter(|id| !id.is_empty());
    match ACTING_AGENT.try_with(Clone::clone) {
        Ok(Some(current)) => {
            if claimed.is_some_and(|claimed| claimed != current) {
                tracing::debug!(
                    acting = %current,
                    claimed = claimed.unwrap_or_default(),
                    "[GOAL-WORKSPACES] compiled dispatch claims another agent; the active owner stands"
                );
            }
            Some(current)
        },
        Ok(None) => None,
        Err(_) => claimed.map(str::to_string),
    }
}

pub(crate) fn task_shell_sandbox(sandbox: &ShellSandboxConfig) -> ShellSandboxConfig {
    let mut scoped = sandbox.clone();
    if let Ok(root) = TASK_SHELL_ROOT.try_with(Clone::clone) {
        scoped
            .allowed_working_dirs
            .push(root.to_string_lossy().into_owned());
    }
    scoped
}

/// The current execution's task folder (runtime-supplied), or `None`.
pub(crate) fn current_task_root() -> Option<PathBuf> {
    TASK_SHELL_ROOT.try_with(Clone::clone).ok()
}

tokio::task_local! {
    /// This execution's own folder (`<task>/executions/<id>`, or the scope's
    /// execution folder outside a task), where its tool results are kept.
    /// Goal workspace reads are recorded against it, so a delegated child
    /// working in the shared task folder keeps that folder. Supplied by the
    /// dispatcher from runtime identity.
    pub(crate) static EVIDENCE_ROOT: PathBuf;
}

/// Where the current execution's goal workspace reads are recorded: its
/// own folder, else its task folder, else nowhere.
pub(crate) fn current_evidence_root() -> Option<PathBuf> {
    EVIDENCE_ROOT
        .try_with(Clone::clone)
        .ok()
        .or_else(current_task_root)
}

/// The current execution's task working directory, from the same
/// runtime-supplied task root the shell uses. `None` outside a task dispatch.
pub(crate) fn current_task_workdir() -> Option<PathBuf> {
    TASK_SHELL_ROOT.try_with(|root| root.join("workdir")).ok()
}

/// `sandbox` widened with this execution's task workdir. The runtime tells
/// agents to put downloads and generated files there and the shell may write
/// there, so the file tools must admit it too; no other task's directory is
/// reachable because the root comes from runtime identity, never arguments.
pub(crate) fn task_file_sandbox(sandbox: &FileSandboxConfig) -> FileSandboxConfig {
    let mut scoped = sandbox.clone();
    if let Some(workdir) = current_task_workdir() {
        let _ = std::fs::create_dir_all(&workdir);
        // A run-time root: it never outranks a read-only root.
        scoped
            .extra_writable_roots
            .push(workdir.to_string_lossy().into_owned());
    }
    scoped
}

/// `action` with relative paths anchored at this execution's task workdir,
/// the directory a task's shell already starts in. Without a task they keep
/// resolving against the process directory; in one, they landed in the
/// service's own checkout instead of the task (a notebook and workbook
/// written to `<repo>/magician_data_v3/…`, invisible to the task's evidence).
pub(crate) fn task_relative_file_action(action: &FileAction) -> std::borrow::Cow<'_, FileAction> {
    let Some(workdir) = current_task_workdir() else {
        return std::borrow::Cow::Borrowed(action);
    };
    let anchor = |path: &PathBuf| {
        if path.is_absolute() {
            path.clone()
        } else {
            workdir.join(path)
        }
    };
    let mut rebased = action.clone();
    match &mut rebased {
        FileAction::Read { path, .. }
        | FileAction::Write { path, .. }
        | FileAction::Append { path, .. }
        | FileAction::Delete { path, .. }
        | FileAction::Exists { path }
        | FileAction::List { path, .. }
        | FileAction::CreateDir { path } => *path = anchor(path),
        FileAction::Copy {
            source,
            destination,
        }
        | FileAction::Move {
            source,
            destination,
        } => {
            *source = anchor(source);
            *destination = anchor(destination);
        },
    }
    std::borrow::Cow::Owned(rebased)
}

/// `raw_path` as a path inside this execution's task workdir, or `None` when
/// it is elsewhere (or there is no task). Containment is checked on the
/// canonical form of the deepest existing ancestor, so `..` segments and
/// symlinks out of the workdir are refused. A relative path is anchored at
/// the workdir, as `read_file`, the `files` tool and the shell anchor it
/// ([`task_relative_file_action`]), so a file written by a relative path is
/// the one a relative read finds.
pub(crate) fn path_in_task_workdir(raw_path: &str) -> Option<PathBuf> {
    let workdir = current_task_workdir()?;
    std::fs::create_dir_all(&workdir).ok()?;
    let workdir = workdir.canonicalize().ok()?;
    let path = Path::new(raw_path.trim());
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workdir.join(path)
    };
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }
    // The policy resolver follows symlinks, dangling ones included: a link
    // the shell planted in the workdir (`ln -s /elsewhere/new.md x.md`)
    // resolves to where a write through it would land, and that is outside.
    let resolved = resolve_path_for_policy(&path).ok()?;
    relative_within(&resolved, &workdir)
        .filter(|rest| !rest.as_os_str().is_empty())
        .map(|_| resolved)
}

/// `path`'s part below `root` when `path` lies inside it: by text, else by the
/// directory identity (device and inode) of `path`'s existing ancestors. One
/// folder can be spelled two ways — `~/MagicianNotes` links to
/// `/Volumes/build/…` while the case-insensitive volume mounts as
/// `/Volumes/SSD1` — and a text-only check then calls a path inside a root
/// "outside". Identity never matches a different folder.
pub(crate) fn relative_within(path: &Path, root: &Path) -> Option<PathBuf> {
    if let Ok(rest) = path.strip_prefix(root) {
        return Some(rest.to_path_buf());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let root_meta = std::fs::metadata(root)
            .ok()
            .filter(std::fs::Metadata::is_dir)?;
        let root_id = (root_meta.dev(), root_meta.ino());
        let mut ancestor = path;
        let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
        loop {
            if let Ok(meta) = std::fs::metadata(ancestor) {
                if meta.is_dir() && (meta.dev(), meta.ino()) == root_id {
                    return Some(rest.iter().rev().collect());
                }
            }
            rest.push(ancestor.file_name()?);
            ancestor = ancestor.parent()?;
        }
    }
    #[cfg(not(unix))]
    None
}

/// Whether every file-writing tool would accept a write to `path` in the task
/// at `task_root`: the file executor ([`file_write_permitted`]) and the staged
/// `write_file` / `edit_file` writers, which take a task path only in the
/// task's workdir. A note telling an agent where to write lists only what
/// passes both, so no tool it picks refuses the folder it was given.
pub(crate) fn every_file_tool_may_write(
    task_root: &Path,
    path: &Path,
    sandbox: &FileSandboxConfig,
) -> bool {
    file_write_permitted(Some(task_root), path, sandbox)
        && TASK_SHELL_ROOT.sync_scope(task_root.to_path_buf(), || {
            path_in_task_workdir(&path.to_string_lossy()).is_some()
        })
}

pub(crate) fn task_scoped_bash_action(
    action: &BashAction,
    root: &Path,
) -> Result<BashAction, ExecutionError> {
    let workdir = root.join("workdir");
    std::fs::create_dir_all(&workdir).map_err(|error| {
        ExecutionError::Step(format!("Cannot prepare task working directory: {error}"))
    })?;
    let mut scoped = action.clone();
    scoped.working_dir = Some(match action.working_dir.as_ref() {
        Some(path) if path.is_absolute() => path.clone(),
        Some(path) => workdir.join(path),
        None => workdir,
    });
    Ok(scoped)
}

/// Execute a file action using tokio::fs
pub async fn execute_file_action(
    action: &FileAction,
    sandbox: &FileSandboxConfig,
) -> Result<ActionResult, ExecutionError> {
    let action = &*task_relative_file_action(action);
    debug!("Executing file action: {}", action.description());
    let sandbox = &task_file_sandbox(sandbox);
    authorize_file_action(action, sandbox).await?;
    #[cfg(test)]
    tests::run_toctou_hook();

    let outcome = execute_checked_file_action(action).await;
    // What decides goal workspace access may just have changed.
    if outcome.is_ok() {
        for (path, write) in file_action_touched_paths(action) {
            if write {
                crate::magician_v2::execution::goal_workspace_access::invalidate(path);
            }
        }
    }
    outcome
}

async fn authorize_file_action(
    action: &FileAction,
    sandbox: &FileSandboxConfig,
) -> Result<(), ExecutionError> {
    if let Err(refusal) = validate_file_action(action, sandbox) {
        god_mode_file_override(action, sandbox, refusal).await?;
    }
    validate_file_action_repo_fence(action)?;
    validate_file_action_runtime_store_fence(action, sandbox)?;
    validate_file_action_workspace_fence(action)
}

/// Whether [`execute_file_action`] would let an agent write `path`: the same
/// task sandbox and checks (mode, roots, read-only roots, repo, runtime-store
/// and workspace fences), without writing and without god mode's
/// ask-to-override. Text that tells an agent where it may write is built from
/// this, so it follows the policy that enforces it. `task_root` is the task a
/// dispatched call would run in (it widens the sandbox with that task's
/// workdir); `None` judges outside any task.
pub(crate) fn file_write_permitted(
    task_root: Option<&Path>,
    path: &Path,
    sandbox: &FileSandboxConfig,
) -> bool {
    let check = || {
        let action = FileAction::Write {
            path: path.to_path_buf(),
            content: String::new(),
            create_dirs: true,
        };
        let action = &*task_relative_file_action(&action);
        let sandbox = &task_file_sandbox(sandbox);
        validate_file_action(action, sandbox).is_ok()
            && validate_file_action_repo_fence(action).is_ok()
            && validate_file_action_runtime_store_fence(action, sandbox).is_ok()
            && validate_file_action_workspace_fence(action).is_ok()
    };
    match task_root {
        Some(root) => TASK_SHELL_ROOT.sync_scope(root.to_path_buf(), check),
        None => check(),
    }
}

/// The IO of [`execute_file_action`], after every fence passed.
async fn execute_checked_file_action(action: &FileAction) -> Result<ActionResult, ExecutionError> {
    match action {
        FileAction::Read { path, encoding: _ } => {
            // Read file contents, from the file actually opened: the goal
            // workspace fence checks it again by its descriptor, so a link
            // swapped since the check above cannot redirect the read.
            let content = read_verified(path).await.map_err(|e| {
                ExecutionError::Step(format!("Failed to read file '{}': {}", path.display(), e))
            })?;
            info!("Read {} bytes from {}", content.len(), path.display());
            Ok(ActionResult::text(content))
        },

        FileAction::Write {
            path,
            content,
            create_dirs,
        } => {
            // Create parent directories if needed
            if *create_dirs {
                if let Some(parent) = path.parent() {
                    if !parent.exists() {
                        tokio::fs::create_dir_all(parent).await.map_err(|e| {
                            ExecutionError::Step(format!(
                                "Failed to create parent directories for '{}': {}",
                                path.display(),
                                e
                            ))
                        })?;
                    }
                }
            }

            // Write content, into the file actually opened (checked again by
            // its descriptor).
            write_verified(path, content.as_bytes())
                .await
                .map_err(|e| {
                    ExecutionError::Step(format!(
                        "Failed to write file '{}': {}",
                        path.display(),
                        e
                    ))
                })?;
            info!("Wrote {} bytes to {}", content.len(), path.display());
            Ok(ActionResult::success())
        },

        FileAction::Append { path, content } => {
            // Open file in append mode, checked again by its descriptor.
            let mut file = open_append_verified(path).await.map_err(|e| {
                ExecutionError::Step(format!(
                    "Failed to open file '{}' for append: {}",
                    path.display(),
                    e
                ))
            })?;

            file.write_all(content.as_bytes()).await.map_err(|e| {
                ExecutionError::Step(format!(
                    "Failed to append to file '{}': {}",
                    path.display(),
                    e
                ))
            })?;
            // A tokio file finishes its write in the background: without a
            // flush the append could report success before the bytes reach
            // the file, and an immediate read sees the old content.
            file.flush().await.map_err(|e| {
                ExecutionError::Step(format!(
                    "Failed to append to file '{}': {}",
                    path.display(),
                    e
                ))
            })?;
            info!("Appended {} bytes to {}", content.len(), path.display());
            Ok(ActionResult::success())
        },

        FileAction::Delete { path, recursive } => {
            // The entry's folder, checked again by its descriptor, and the
            // delete done relative to that descriptor.
            let (folder, name) = verify_parent(
                path,
                crate::magician_v2::execution::goal_workspace_access::Op::Subtree,
            )
            .map_err(|e| {
                ExecutionError::Step(format!("Failed to delete '{}': {}", path.display(), e))
            })?;
            match remove_entry_at(&folder, &name, *recursive) {
                Ok(()) => {},
                Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {
                    if *recursive {
                        tokio::fs::remove_dir_all(path).await
                    } else {
                        tokio::fs::remove_file(path).await
                    }
                    .map_err(|e| {
                        ExecutionError::Step(format!(
                            "Failed to delete '{}': {}",
                            path.display(),
                            e
                        ))
                    })?;
                },
                Err(e) => {
                    return Err(ExecutionError::Step(format!(
                        "Failed to delete '{}': {}",
                        path.display(),
                        e
                    )))
                },
            }
            info!("Deleted {}", path.display());
            Ok(ActionResult::success())
        },

        FileAction::Copy {
            source,
            destination,
        } => {
            // Create parent directories for destination if needed
            if let Some(parent) = destination.parent() {
                if !parent.exists() {
                    tokio::fs::create_dir_all(parent).await.map_err(|e| {
                        ExecutionError::Step(format!(
                            "Failed to create parent directories for '{}': {}",
                            destination.display(),
                            e
                        ))
                    })?;
                }
            }

            // Copy file, between the files actually opened.
            copy_verified(source, destination).await.map_err(|e| {
                ExecutionError::Step(format!(
                    "Failed to copy '{}' to '{}': {}",
                    source.display(),
                    destination.display(),
                    e
                ))
            })?;
            info!("Copied {} to {}", source.display(), destination.display());
            Ok(ActionResult::success())
        },

        FileAction::Move {
            source,
            destination,
        } => {
            // Create parent directories for destination if needed
            if let Some(parent) = destination.parent() {
                if !parent.exists() {
                    tokio::fs::create_dir_all(parent).await.map_err(|e| {
                        ExecutionError::Step(format!(
                            "Failed to create parent directories for '{}': {}",
                            destination.display(),
                            e
                        ))
                    })?;
                }
            }

            // Move/rename file, between the folders actually opened.
            rename_verified(source, destination).map_err(|e| {
                ExecutionError::Step(format!(
                    "Failed to move '{}' to '{}': {}",
                    source.display(),
                    destination.display(),
                    e
                ))
            })?;
            info!("Moved {} to {}", source.display(), destination.display());
            Ok(ActionResult::success())
        },

        FileAction::Exists { path } => {
            let exists = path.exists();
            debug!("Path '{}' exists: {}", path.display(), exists);
            Ok(ActionResult::bool(exists))
        },

        FileAction::List { path, pattern } => {
            verify_dir(path).map_err(|e| {
                ExecutionError::Step(format!(
                    "Failed to read directory '{}': {}",
                    path.display(),
                    e
                ))
            })?;
            let mut entries = Vec::new();
            let mut read_dir = tokio::fs::read_dir(path).await.map_err(|e| {
                ExecutionError::Step(format!(
                    "Failed to read directory '{}': {}",
                    path.display(),
                    e
                ))
            })?;

            while let Some(entry) = read_dir.next_entry().await.map_err(|e| {
                ExecutionError::Step(format!("Failed to read directory entry: {}", e))
            })? {
                let name = entry.file_name().to_string_lossy().to_string();

                // Apply pattern filter if specified
                if let Some(pat) = pattern {
                    if !matches_glob_pattern(&name, pat) {
                        continue;
                    }
                }

                entries.push(name);
            }
            // Inside goal workspaces, a listing names only what this agent
            // may read: no workspace it is not a member of, no private goal.
            if crate::magician_v2::execution::goal_workspace_access::is_under_workspaces_root(path)
            {
                entries.retain(|name| {
                    crate::magician_v2::execution::goal_workspace_access::refusal_for_current(
                        &path.join(name),
                        crate::magician_v2::execution::goal_workspace_access::Op::Read,
                    )
                    .is_none()
                });
            }

            info!("Listed {} entries in {}", entries.len(), path.display());
            Ok(ActionResult::list(entries))
        },

        FileAction::CreateDir { path } => {
            tokio::fs::create_dir_all(path).await.map_err(|e| {
                ExecutionError::Step(format!(
                    "Failed to create directory '{}': {}",
                    path.display(),
                    e
                ))
            })?;
            info!("Created directory: {}", path.display());
            Ok(ActionResult::success())
        },
    }
}

fn validate_file_action(
    action: &FileAction,
    sandbox: &FileSandboxConfig,
) -> Result<(), ExecutionError> {
    if matches!(sandbox.mode, FileSandboxMode::Unrestricted) {
        return Ok(());
    }

    let roots = resolve_file_sandbox_roots(sandbox)?;
    if roots.writable.is_empty() && roots.read_only.is_empty() && roots.extra_writable.is_empty() {
        return Err(ExecutionError::Step(
            "File sandbox has no allowed roots configured".to_string(),
        ));
    }

    // Policy refusals of what the agent asked for (not faults): it can act on
    // them, so they are `Refused` — logged as warnings and never retried.
    let mutates = !matches!(
        action,
        FileAction::Read { .. } | FileAction::Exists { .. } | FileAction::List { .. }
    );
    if mutates && matches!(sandbox.mode, FileSandboxMode::ReadOnly) {
        return Err(ExecutionError::Refused(
            "File action is blocked in read-only file sandbox mode".to_string(),
        ));
    }
    match action {
        FileAction::Delete { .. } if !sandbox.allow_delete => {
            return Err(ExecutionError::Refused(
                "Delete operations are disabled by file sandbox policy".to_string(),
            ));
        },
        FileAction::Move { .. } if !sandbox.allow_delete => {
            return Err(ExecutionError::Refused(
                "Move operations are disabled when deletes are disallowed by file sandbox policy"
                    .to_string(),
            ));
        },
        _ => {},
    }

    // Reads need any root; writes need a writable one, and a read-only root
    // refuses them. A copy reads its source and writes its destination. A
    // subtree operation is also refused when a read-only root lies inside it.
    let subtree = file_action_subtree_paths(action);
    for (path, write) in file_action_touched_paths(action) {
        ensure_path_permitted(path, write, subtree.contains(&path), &roots)?;
    }
    for path in file_action_in_place_paths(action) {
        if let Some(refusal) = hard_link_refusal(path, sandbox) {
            return Err(ExecutionError::Step(refusal));
        }
    }
    Ok(())
}

/// Apply the native file executor's complete read policy to a path without
/// decoding it as text. Binary consumers (for example, a vision provider)
/// must call this before reading bytes so they cannot bypass allowed roots or
/// future read-side isolation fences merely because `FileAction::Read` returns
/// UTF-8 text.
pub fn validate_file_read_path(
    path: &Path,
    sandbox: &FileSandboxConfig,
) -> Result<(), ExecutionError> {
    let action = FileAction::Read {
        path: path.to_path_buf(),
        encoding: None,
    };
    validate_file_action(&action, sandbox)?;
    validate_file_action_repo_fence(&action)?;
    validate_file_action_runtime_store_fence(&action, sandbox)?;
    validate_file_action_workspace_fence(&action)
}

/// Search tools use the same owner approval and post-approval fences as a file
/// read, without decoding the search root (which may be a directory) as text.
pub(crate) async fn authorize_file_read_path(
    path: &Path,
    sandbox: &FileSandboxConfig,
) -> Result<(), ExecutionError> {
    authorize_file_action(
        &FileAction::Read {
            path: path.to_path_buf(),
            encoding: None,
        },
        sandbox,
    )
    .await
}

/// Binary producers must use the same mutation and runtime-store fences as
/// the file executor before publishing a result at their declared path.
pub(crate) fn validate_file_write_path(
    path: &Path,
    sandbox: &FileSandboxConfig,
) -> Result<(), ExecutionError> {
    let action = FileAction::Write {
        path: path.to_path_buf(),
        content: String::new(),
        create_dirs: true,
    };
    validate_file_action(&action, sandbox)?;
    validate_file_action_repo_fence(&action)?;
    validate_file_action_runtime_store_fence(&action, sandbox)?;
    validate_file_action_workspace_fence(&action)
}

/// Fences a God-mode staged edit must still honor, before stage and on apply.
/// Ordinary file-sandbox refusals are separately bound to its diff decision.
pub(crate) fn validate_staged_write_fences(
    path: &Path,
    sandbox: &FileSandboxConfig,
    agent: Option<&str>,
) -> Result<(), ExecutionError> {
    let action = FileAction::Write {
        path: path.to_path_buf(),
        content: String::new(),
        create_dirs: true,
    };
    validate_file_action_repo_fence(&action)?;
    validate_file_action_runtime_store_fence(&action, sandbox)?;
    use crate::magician_v2::execution::goal_workspace_access as access;
    if let Some(refusal) = access::hybrid_index_refusal(path)
        .or_else(|| access::refusal(path, access::Op::Write, agent))
    {
        return Err(ExecutionError::Step(refusal));
    }
    Ok(())
}

/// Always-on fences no sandbox mode or god mode lifts: goal workspace access
/// for the acting agent (`goal_workspace_access`: members, private goals, the
/// README and folder layout only the admin agent changes), and the notes
/// hybrid search index, which embeds every note. Checked on the path as
/// given (symlinks followed) and, for writes, on the entry itself.
fn validate_file_action_workspace_fence(action: &FileAction) -> Result<(), ExecutionError> {
    use crate::magician_v2::execution::goal_workspace_access as access;
    let subtree = file_action_subtree_paths(action);
    for (path, write) in file_action_touched_paths(action) {
        if let Some(refusal) = access::hybrid_index_refusal(path) {
            return Err(ExecutionError::Step(refusal));
        }
        let op = if subtree.contains(&path) {
            access::Op::Subtree
        } else if write {
            access::Op::Write
        } else {
            access::Op::Read
        };
        if let Some(refusal) = access::refusal_for_current(path, op) {
            return Err(ExecutionError::Step(refusal));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Check what was opened, not only what was named: a symlink or a parent
// folder swapped between the fence's check and the IO cannot redirect it.
// ---------------------------------------------------------------------------

/// The path the kernel holds for an open file or folder.
fn fd_real_path(file: &std::fs::File) -> std::io::Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::io::AsRawFd;
        let mut buf = vec![0u8; libc::PATH_MAX as usize];
        // SAFETY: F_GETPATH writes at most PATH_MAX bytes, NUL included,
        // into `buf`, which is PATH_MAX bytes long.
        let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) };
        if rc == -1 {
            return Err(std::io::Error::last_os_error());
        }
        let len = buf.iter().position(|byte| *byte == 0).unwrap_or(buf.len());
        buf.truncate(len);
        Ok(PathBuf::from(std::ffi::OsString::from_vec(buf)))
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = file;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no descriptor path on this platform",
        ))
    }
}

/// Refuse `file` (opened from `path`) when what it really is may not be
/// touched by the acting agent: a goal workspace it is not a member of, a
/// private goal (also by hard link), the notes index.
fn verify_opened(
    path: &Path,
    file: &std::fs::File,
    op: crate::magician_v2::execution::goal_workspace_access::Op,
) -> std::io::Result<()> {
    use crate::magician_v2::execution::goal_workspace_access as access;
    let real = match fd_real_path(file) {
        Ok(real) => real,
        // Without a descriptor path the path-level check already ran.
        Err(error) if error.kind() == std::io::ErrorKind::Unsupported => return Ok(()),
        Err(error) => return Err(error),
    };
    if let Some(refusal) =
        access::hybrid_index_refusal(&real).or_else(|| access::refusal_for_current(&real, op))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{refusal} (`{}` changed while it was checked)",
                path.display()
            ),
        ));
    }
    Ok(())
}

async fn read_verified(path: &Path) -> std::io::Result<String> {
    use tokio::io::AsyncReadExt;
    let file = std::fs::File::open(path)?;
    verify_opened(
        path,
        &file,
        crate::magician_v2::execution::goal_workspace_access::Op::Read,
    )?;
    let mut content = String::new();
    tokio::fs::File::from_std(file)
        .read_to_string(&mut content)
        .await?;
    crate::magician_v2::execution::goal_workspace_access::record_read(path);
    Ok(content)
}

/// Open `path` for writing, check what was opened (where a link at any
/// component really led), then truncate and write.
async fn write_verified(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let file = open_checked_for_write(path, false)?;
    file.set_len(0)?;
    let mut file = tokio::fs::File::from_std(file);
    file.write_all(content).await?;
    file.flush().await
}

async fn open_append_verified(path: &Path) -> std::io::Result<tokio::fs::File> {
    Ok(tokio::fs::File::from_std(open_checked_for_write(
        path, true,
    )?))
}

/// [`open_for_write`], then the descriptor check; a file the open created
/// for a refused target is removed again, so a refusal leaves nothing.
fn open_checked_for_write(path: &Path, append: bool) -> std::io::Result<std::fs::File> {
    // Create only where nothing is, not even a link (`create_new` never
    // follows one): a file this call made is then the only thing a refusal
    // may remove, and an existing file is never removed.
    let (file, created) = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => (file, true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            (open_for_write(path, append)?, false)
        },
        Err(error) => return Err(error),
    };
    if let Err(error) = verify_opened(
        path,
        &file,
        crate::magician_v2::execution::goal_workspace_access::Op::Write,
    ) {
        if created {
            remove_if_same_file(path, &file);
        }
        return Err(error);
    }
    Ok(file)
}

/// Remove `path` only while it is still the very file `file` is (same
/// device and inode, not a link): nothing another process put there since.
fn remove_if_same_file(path: &Path, file: &std::fs::File) {
    // Remove the file this call created where it actually is (the open
    // descriptor's own path), through a descriptor of its real folder, and
    // only while that name is still this very file: a folder or name
    // swapped in meanwhile is left alone.
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::io::AsRawFd;
        let _ = path;
        let (Ok(opened), Ok(real)) = (file.metadata(), fd_real_path(file)) else {
            return;
        };
        let (Some(parent), Some(name)) = (real.parent(), real.file_name()) else {
            return;
        };
        let Ok(folder) = std::fs::File::open(parent) else {
            return;
        };
        if fd_real_path(&folder).ok().as_deref() != Some(parent) {
            return;
        }
        let Ok(name) = std::ffi::CString::new(name.as_bytes()) else {
            return;
        };
        // SAFETY: `stat` is plain old data; fstatat fills it relative to an
        // open folder descriptor without following a link.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let found = unsafe {
            libc::fstatat(
                folder.as_raw_fd(),
                name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if found != 0
            || (stat.st_mode & libc::S_IFMT) != libc::S_IFREG
            || stat.st_dev as u64 != opened.dev()
            || stat.st_ino as u64 != opened.ino()
        {
            return;
        }
        // SAFETY: unlinks one entry relative to the same open descriptor.
        let _ = unsafe { libc::unlinkat(folder.as_raw_fd(), name.as_ptr(), 0) };
    }
    #[cfg(not(unix))]
    {
        let _ = (path, file);
    }
}

/// Open an existing file for writing (it was there: `create_new` said so).
fn open_for_write(path: &Path, append: bool) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    if append {
        options.append(true);
    } else {
        options.write(true);
    }
    options.open(path)
}

async fn copy_verified(source: &Path, destination: &Path) -> std::io::Result<u64> {
    let from = std::fs::File::open(source)?;
    verify_opened(
        source,
        &from,
        crate::magician_v2::execution::goal_workspace_access::Op::Read,
    )?;
    crate::magician_v2::execution::goal_workspace_access::record_read(source);
    let to = open_checked_for_write(destination, false)?;
    to.set_len(0)?;
    // As `fs::copy`: the copy keeps the source's permissions.
    to.set_permissions(from.metadata()?.permissions())?;
    let mut from = tokio::fs::File::from_std(from);
    let mut to = tokio::fs::File::from_std(to);
    let copied = tokio::io::copy(&mut from, &mut to).await?;
    to.flush().await?;
    Ok(copied)
}

/// Open `path`'s parent folder and check the entry under it as it really
/// is now: the folder by its descriptor, joined with the entry's name.
/// Returns the open folder and the entry name.
fn verify_parent(
    path: &Path,
    op: crate::magician_v2::execution::goal_workspace_access::Op,
) -> std::io::Result<(std::fs::File, std::ffi::OsString)> {
    let agent = current_acting_agent();
    verify_parent_for(path, op, agent.as_deref())
}

/// [`verify_parent`] for `agent` (the diff-approval apply judges the agent
/// that staged the edit). Fails closed: a folder whose real path cannot be
/// read is refused (but where the platform has no descriptor path at all).
pub(crate) fn verify_parent_for(
    path: &Path,
    op: crate::magician_v2::execution::goal_workspace_access::Op,
    agent: Option<&str>,
) -> std::io::Result<(std::fs::File, std::ffi::OsString)> {
    use crate::magician_v2::execution::goal_workspace_access as access;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let (Some(parent), Some(name)) = (absolute.parent(), absolute.file_name()) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path has no parent folder",
        ));
    };
    let folder = std::fs::File::open(parent)?;
    match fd_real_path(&folder) {
        Ok(real) => {
            let entry = real.join(name);
            if let Some(refusal) =
                access::hybrid_index_refusal(&entry).or_else(|| access::refusal(&entry, op, agent))
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "{refusal} (`{}` changed while it was checked)",
                        path.display()
                    ),
                ));
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {},
        Err(error) => return Err(error),
    }
    Ok((folder, name.to_os_string()))
}

/// Delete the entry `name` of the open, checked folder `folder`: a file (or
/// link) with `unlinkat`, a folder (when `recursive`) by walking it through
/// descriptors, never by path, so nothing swapped in after the check is
/// followed.
fn remove_entry_at(
    folder: &std::fs::File,
    name: &std::ffi::OsStr,
    recursive: bool,
) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::io::AsRawFd;
        let c_name = std::ffi::CString::new(name.as_bytes())?;
        let dirfd = folder.as_raw_fd();
        // SAFETY: `dirfd` is an open folder for the call, `c_name` a
        // NUL-terminated single component; `stat` is written by the call.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatat(dirfd, c_name.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) }
            == -1
        {
            return Err(std::io::Error::last_os_error());
        }
        let is_dir = (stat.st_mode & libc::S_IFMT) == libc::S_IFDIR;
        if is_dir {
            if !recursive {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "is a folder; delete it recursively",
                ));
            }
            remove_tree_at(dirfd, &c_name)?;
            // SAFETY: as above.
            if unsafe { libc::unlinkat(dirfd, c_name.as_ptr(), libc::AT_REMOVEDIR) } == -1 {
                return Err(std::io::Error::last_os_error());
            }
            return Ok(());
        }
        // SAFETY: as above.
        if unsafe { libc::unlinkat(dirfd, c_name.as_ptr(), 0) } == -1 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (folder, name, recursive);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "descriptor deletes are unix-only",
        ))
    }
}

/// Empty the folder `name` under `dirfd` without following any link.
#[cfg(unix)]
fn remove_tree_at(dirfd: libc::c_int, name: &std::ffi::CStr) -> std::io::Result<()> {
    // SAFETY: opens a folder relative to an open descriptor, refusing links.
    let fd = unsafe {
        libc::openat(
            dirfd,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fd` is a folder descriptor we own; `fdopendir` takes it over
    // and `closedir` below releases both.
    let dir = unsafe { libc::fdopendir(fd) };
    if dir.is_null() {
        let error = std::io::Error::last_os_error();
        // SAFETY: `fd` is still ours when `fdopendir` failed.
        unsafe { libc::close(fd) };
        return Err(error);
    }
    let mut names: Vec<std::ffi::CString> = Vec::new();
    loop {
        // SAFETY: `dir` is a valid stream until `closedir`.
        let entry = unsafe { libc::readdir(dir) };
        if entry.is_null() {
            break;
        }
        // SAFETY: `d_name` is NUL-terminated for a returned entry.
        let child = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
        if child.to_bytes() != b"." && child.to_bytes() != b".." {
            names.push(child.to_owned());
        }
    }
    let result = (|| -> std::io::Result<()> {
        for child in &names {
            let mut stat: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: `fd` stays open until `closedir` below.
            if unsafe { libc::fstatat(fd, child.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) }
                == -1
            {
                return Err(std::io::Error::last_os_error());
            }
            if (stat.st_mode & libc::S_IFMT) == libc::S_IFDIR {
                remove_tree_at(fd, child)?;
                // SAFETY: as above.
                if unsafe { libc::unlinkat(fd, child.as_ptr(), libc::AT_REMOVEDIR) } == -1 {
                    return Err(std::io::Error::last_os_error());
                }
            } else if unsafe { libc::unlinkat(fd, child.as_ptr(), 0) } == -1 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    })();
    // SAFETY: releases the stream and its descriptor.
    unsafe { libc::closedir(dir) };
    result
}

/// Check what `path` really is now, by descriptor, for `agent`: the file
/// (where it exists) or else its parent folder. For writers that wrote by
/// path (the diff-approval apply), which then roll back on a refusal.
pub(crate) fn verify_path_for(
    path: &Path,
    op: crate::magician_v2::execution::goal_workspace_access::Op,
    agent: Option<&str>,
) -> std::io::Result<()> {
    use crate::magician_v2::execution::goal_workspace_access as access;
    match std::fs::File::open(path) {
        Ok(file) => match fd_real_path(&file) {
            Ok(real) => match access::hybrid_index_refusal(&real)
                .or_else(|| access::refusal(&real, op, agent))
            {
                Some(refusal) => Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    refusal,
                )),
                None => Ok(()),
            },
            Err(error) if error.kind() == std::io::ErrorKind::Unsupported => Ok(()),
            Err(error) => Err(error),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            verify_parent_for(path, op, agent).map(|_| ())
        },
        Err(error) => Err(error),
    }
}

/// Rename between the two folders actually opened and checked.
fn rename_verified(source: &Path, destination: &Path) -> std::io::Result<()> {
    use crate::magician_v2::execution::goal_workspace_access::Op;
    let (from_dir, from_name) = verify_parent(source, Op::Subtree)?;
    let (to_dir, to_name) = verify_parent(destination, Op::Subtree)?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::io::AsRawFd;
        let from = std::ffi::CString::new(from_name.as_bytes())?;
        let to = std::ffi::CString::new(to_name.as_bytes())?;
        // SAFETY: both descriptors are open folders owned for the call, and
        // both names are NUL-terminated single path components.
        let rc = unsafe {
            libc::renameat(
                from_dir.as_raw_fd(),
                from.as_ptr(),
                to_dir.as_raw_fd(),
                to.as_ptr(),
            )
        };
        if rc == -1 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (from_dir, from_name, to_dir, to_name);
        std::fs::rename(source, destination)
    }
}

/// Check a folder about to be listed by its descriptor.
fn verify_dir(path: &Path) -> std::io::Result<()> {
    let folder = std::fs::File::open(path)?;
    verify_opened(
        path,
        &folder,
        crate::magician_v2::execution::goal_workspace_access::Op::Read,
    )
}

/// [`resolve_path_for_policy`] for the goal workspace fence.
pub(crate) fn resolve_path_checked(path: &Path) -> Result<PathBuf, ExecutionError> {
    resolve_path_for_policy(path)
}

/// [`resolve_leaf_for_policy`] for the goal workspace fence.
pub(crate) fn resolve_leaf_checked(path: &Path) -> Result<Option<PathBuf>, ExecutionError> {
    resolve_leaf_for_policy(path)
}

/// Isolation deny-fence for native file actions: a MUTATING file action must not
/// resolve into the live magician repo source tree. Mirrors the shell working_dir
/// fence (see `execute_bash_action_*`) so file writes/moves/deletes/dir-creates
/// cannot touch the user's real repo even when the file sandbox policy would
/// otherwise allow the path. Only mutating variants are fenced; pure reads
/// (Read/Exists/List) and the SOURCE of a Copy (read-only) are left untouched.
/// Targets are resolved via `resolve_path_for_policy` rather than `canonicalize`
/// because a freshly created file's target does not exist yet (canonicalize would
/// fail and skip the fence).
///
/// CODING-SCOPED: armed only when `coding_context_active()` — i.e. for a coding
/// agent's own file tools (and the coding engine's spawns), never for general
/// agents/skills. File actions are in-process `tokio::fs`, NOT subprocesses, so
/// the OS sandbox cannot cover them; this declarative fence is their isolation.
/// Scoping to coding context keeps the DEFAULT path byte-equivalent: the shipped
/// file sandbox permits `.` (the repo root) as writable, and legitimate general
/// flows write to in-repo data dirs outside the storage base (e.g. the prompt
/// store under `data/`) — an always-on fence would wrongly reject those. Fails
/// open when not in a coding context, unarmed, or a path cannot be resolved.
fn validate_file_action_repo_fence(action: &FileAction) -> Result<(), ExecutionError> {
    if !crate::magician_v2::execution::coding_engine::coding_context_active() {
        return Ok(());
    }
    let Some((repo_source_root, sandbox_base)) =
        crate::magician_v2::execution::coding_engine::live_repo_source_fence()
    else {
        return Ok(());
    };

    let mut targets: Vec<&Path> = Vec::new();
    match action {
        FileAction::Write { path, .. }
        | FileAction::Append { path, .. }
        | FileAction::Delete { path, .. }
        | FileAction::CreateDir { path } => targets.push(path),
        FileAction::Copy { destination, .. } => targets.push(destination),
        FileAction::Move {
            source,
            destination,
        } => {
            targets.push(source);
            targets.push(destination);
        },
        FileAction::Read { .. } | FileAction::Exists { .. } | FileAction::List { .. } => {},
    }

    for target in targets {
        if let Ok(resolved) = resolve_path_for_policy(target) {
            crate::magician_v2::execution::coding_engine::reject_repo_source_tree(
                &resolved,
                &repo_source_root,
                &sandbox_base,
            )
            .map_err(ExecutionError::Refused)?;
        }
    }

    Ok(())
}

/// Always-on isolation deny-fence: a MUTATING native file action must not target a
/// scope's runtime-INTERNAL stores — the diff-approval transaction records, the
/// code-change proposals, and the pause/approval records under
/// `<scopes>/<principal>/<workspace>/{transactions,code_change_proposals,pause_states}`.
/// Those records carry operator-approval + apply-destination data the runtime later
/// TRUSTS (e.g. a transaction's `apply_root`, which decides where an approved write
/// lands). The scopes tree is otherwise an allowed root (`augment_with_scopes_root`),
/// so without this fence an agent could overwrite a staged transaction to forge an
/// approved external destination and redirect an operator-approved write to an
/// arbitrary path. Unlike the repo fence this is armed for ALL agents (not just
/// coding runs). Reads are unaffected; only writes/moves/deletes/dir-creates are
/// fenced. The runtime writes these stores via `std::fs` directly (not through this
/// path), so legitimate runtime writes are untouched.
fn validate_file_action_runtime_store_fence(
    action: &FileAction,
    sandbox: &FileSandboxConfig,
) -> Result<(), ExecutionError> {
    // Store sub-paths relative to `<scopes_root>/<principal>/<workspace>/`. Two are
    // direct children (`transactions`, `code_change_proposals`); the pause/approval
    // store is nested under `runtime/` — matched component-wise so its real depth is
    // covered (a flat 3rd-component check would silently miss it).
    const RESERVED_STORE_SUBPATHS: &[&str] = &[
        "transactions",
        "code_change_proposals",
        "runtime/pause_states",
    ];

    let mut targets: Vec<&Path> = Vec::new();
    match action {
        FileAction::Write { path, .. }
        | FileAction::Append { path, .. }
        | FileAction::Delete { path, .. }
        | FileAction::CreateDir { path } => targets.push(path),
        FileAction::Copy { destination, .. } => targets.push(destination),
        FileAction::Move {
            source,
            destination,
        } => {
            targets.push(source);
            targets.push(destination);
        },
        FileAction::Read { .. } | FileAction::Exists { .. } | FileAction::List { .. } => {},
    }
    if targets.is_empty() {
        return Ok(());
    }

    // The scope roots the sandbox exposes are the dirs `augment_with_scopes_root`
    // added — basename `scopes`. A reserved store sits at
    // `<scopes_root>/<principal>/<workspace>/<store>`, so the store name is the 3rd
    // path component of the target relative to the scopes root. Resolving both sides
    // the same way `ensure_path_permitted` does keeps the prefix check symlink-safe.
    let scopes_roots: Vec<PathBuf> = resolve_allowed_file_roots(sandbox)
        .unwrap_or_default()
        .into_iter()
        .filter(|root| root.file_name().and_then(|name| name.to_str()) == Some("scopes"))
        .collect();
    if scopes_roots.is_empty() {
        return Ok(());
    }

    let subtree_targets = file_action_subtree_paths(action);
    for target in targets {
        let subtree = subtree_targets.contains(&target);
        let Ok(resolved) = resolve_path_for_policy(target) else {
            continue;
        };
        for scopes_root in &scopes_roots {
            if subtree && scopes_root.starts_with(&resolved) && *scopes_root != resolved {
                return Err(ExecutionError::Step(format!(
                    "File action on `{}` would take the runtime's scopes tree with it",
                    resolved.display()
                )));
            }
            if let Ok(rel) = resolved.strip_prefix(scopes_root) {
                // rel == <principal>/<workspace>/<store-relative-path>. Skip the
                // scope segments and match the store dir component-wise
                // (PathBuf::starts_with), so a NESTED store like `runtime/pause_states`
                // is caught, not only the depth-2 ones.
                let store_path: PathBuf = rel.components().skip(2).collect();
                if RESERVED_STORE_SUBPATHS
                    .iter()
                    .any(|reserved| store_path.starts_with(reserved))
                {
                    return Err(ExecutionError::Refused(format!(
                        "File action targets a runtime-internal store `{}` — the scope's transaction / approval / pause records are not agent-writable",
                        resolved.display()
                    )));
                }
                // A recursive delete or a move of a folder above a store (the
                // scopes root, a principal, a scope, its `runtime/`) takes the
                // store with it.
                if subtree
                    && RESERVED_STORE_SUBPATHS
                        .iter()
                        .any(|reserved| Path::new(reserved).starts_with(&store_path))
                {
                    return Err(ExecutionError::Step(format!(
                        "File action on `{}` would take a runtime-internal store with it — the scope's transaction / approval / pause records are not agent-writable",
                        resolved.display()
                    )));
                }
            }
        }
    }
    Ok(())
}

fn resolve_allowed_file_roots(sandbox: &FileSandboxConfig) -> Result<Vec<PathBuf>, ExecutionError> {
    resolve_configured_file_roots(&sandbox.allowed_roots)
}

fn resolve_configured_file_roots(roots: &[String]) -> Result<Vec<PathBuf>, ExecutionError> {
    let cwd = std::env::current_dir().map_err(|e| {
        ExecutionError::Step(format!(
            "Failed to resolve current working directory: {}",
            e
        ))
    })?;

    roots
        .iter()
        .map(|root| {
            let root_path = PathBuf::from(root);
            let absolute = if root_path.is_absolute() {
                root_path
            } else {
                cwd.join(root_path)
            };
            resolve_path_for_policy(&absolute)
        })
        .collect()
}

/// A file sandbox's roots, resolved the same way as the paths checked
/// against them: configured `allowed_roots` (read and write), run-time
/// `extra_writable_roots` and `read_only_roots`.
struct FileSandboxRoots {
    writable: Vec<PathBuf>,
    extra_writable: Vec<PathBuf>,
    read_only: Vec<PathBuf>,
}

/// What a sandbox lets an agent do at one resolved path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootAccess {
    ReadWrite,
    ReadOnly,
    Outside,
}

impl FileSandboxRoots {
    /// The deepest configured root containing `resolved` decides. A
    /// read-only root inside a writable one (a scope's `programs/` under the
    /// scopes tree) stays read-only, and a configured writable root inside a
    /// read-only one (a notes space's `Workspaces/`) stays writable; a tie
    /// goes to read-only. Run-time roots (task workdir, owner grants,
    /// delegation folders) never outrank a read-only root.
    fn access(&self, resolved: &Path) -> RootAccess {
        let deepest = |roots: &[PathBuf]| {
            roots
                .iter()
                .filter(|root| relative_within(resolved, root).is_some())
                .map(|root| root.components().count())
                .max()
        };
        match (
            deepest(self.writable.as_slice()),
            deepest(self.read_only.as_slice()),
        ) {
            (Some(writable), Some(read_only)) if writable > read_only => RootAccess::ReadWrite,
            (_, Some(_)) => RootAccess::ReadOnly,
            (Some(_), None) => RootAccess::ReadWrite,
            (None, None)
                if self
                    .extra_writable
                    .iter()
                    .any(|root| relative_within(resolved, root).is_some()) =>
            {
                RootAccess::ReadWrite
            },
            (None, None) => RootAccess::Outside,
        }
    }

    /// The first read-only root at or under `resolved`: what a recursive
    /// delete or a move of `resolved` would take with it.
    fn read_only_root_within(&self, resolved: &Path) -> Option<&PathBuf> {
        self.read_only
            .iter()
            .find(|root| root.starts_with(resolved))
    }

    /// ` (writable: …; read-only: …)`, so a refused agent learns where it can
    /// read and write without guessing.
    fn hint(&self) -> String {
        let list = |roots: &[&PathBuf]| {
            if roots.is_empty() {
                "none".to_string()
            } else {
                roots
                    .iter()
                    .map(|root| root.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        };
        let writable: Vec<&PathBuf> = self.writable.iter().chain(&self.extra_writable).collect();
        let read_only: Vec<&PathBuf> = self.read_only.iter().collect();
        format!(
            " (writable: {}; read-only: {})",
            list(writable.as_slice()),
            list(read_only.as_slice())
        )
    }

    /// Why mutating `path` (which resolves to `resolved`) is refused by a
    /// read-only root, or `None`. Checks the target a write lands on, the
    /// entry itself when it is a symlink (a delete or move acts on the link),
    /// and for a `subtree` operation every read-only root beneath it.
    fn read_only_violation(
        &self,
        path: &Path,
        resolved: &Path,
        subtree: bool,
    ) -> Result<Option<String>, ExecutionError> {
        let leaf = resolve_leaf_for_policy(path)?;
        for target in std::iter::once(resolved).chain(leaf.as_deref()) {
            if self.access(target) == RootAccess::ReadOnly {
                return Ok(Some(self.read_only_refusal(target)));
            }
            if subtree {
                if let Some(inner) = self.read_only_root_within(target) {
                    return Ok(Some(format!(
                        "{} contains {}, which is read-only for agents (read_only_roots){}",
                        target.display(),
                        inner.display(),
                        self.hint()
                    )));
                }
            }
        }
        Ok(None)
    }

    fn read_only_refusal(&self, resolved: &Path) -> String {
        format!(
            "{} is read-only for agents (read_only_roots){}",
            resolved.display(),
            self.hint()
        )
    }
}

fn resolve_file_sandbox_roots(
    sandbox: &FileSandboxConfig,
) -> Result<FileSandboxRoots, ExecutionError> {
    Ok(FileSandboxRoots {
        writable: resolve_allowed_file_roots(sandbox)?,
        extra_writable: resolve_configured_file_roots(&sandbox.extra_writable_roots)?,
        read_only: resolve_configured_file_roots(&sandbox.read_only_roots)?,
    })
}

/// The ` (writable: …; read-only: …)` suffix for `sandbox`, for callers
/// that raise `PathAccessDenied` themselves. Empty when the roots cannot be
/// resolved.
pub(crate) fn file_sandbox_roots_hint(sandbox: &FileSandboxConfig) -> String {
    resolve_file_sandbox_roots(sandbox)
        .map(|roots| roots.hint())
        .unwrap_or_default()
}

/// The refusal for writing `path` when one of the sandbox's
/// `read_only_roots` governs it, or `None` when the sandbox would not refuse
/// it for that reason. For writers that do not go through
/// `execute_file_action` (the staged `write_file` / `edit_file` family and
/// the diff-approval apply), so a read-only root refuses them too. Fails
/// closed: a path or root that cannot be resolved is refused.
pub(crate) fn read_only_root_refusal(path: &Path, sandbox: &FileSandboxConfig) -> Option<String> {
    read_only_refusal_for(path, false, sandbox)
}

/// [`read_only_root_refusal`], for a `subtree` operation too.
fn read_only_refusal_for(
    path: &Path,
    subtree: bool,
    sandbox: &FileSandboxConfig,
) -> Option<String> {
    if matches!(sandbox.mode, FileSandboxMode::Unrestricted) || sandbox.read_only_roots.is_empty() {
        return None;
    }
    let check = || -> Result<Option<String>, ExecutionError> {
        let roots = resolve_file_sandbox_roots(sandbox)?;
        let resolved = resolve_path_for_policy(path)?;
        roots.read_only_violation(path, &resolved, subtree)
    };
    match check() {
        Ok(refusal) => refusal,
        Err(error) => Some(format!(
            "{} cannot be checked against read_only_roots: {error}",
            path.display()
        )),
    }
}

/// The file sandbox agents run under, kept for writers that act after the
/// run: an owner-approved diff is applied from an HTTP decision, which has
/// no executor, and must still refuse a target under a read-only root.
static AGENT_FILE_SANDBOX: std::sync::RwLock<Option<FileSandboxConfig>> =
    std::sync::RwLock::new(None);

/// Record the boot file sandbox (with the scopes root already added) for
/// [`agent_file_sandbox`]. The orchestrator calls this when it is built.
pub fn register_agent_file_sandbox(sandbox: FileSandboxConfig) {
    if let Ok(mut guard) = AGENT_FILE_SANDBOX.write() {
        *guard = Some(sandbox);
    }
}

/// The shell sandbox's `allowed_working_dirs`, kept for the coding sandbox,
/// which re-opens those inside a read-only root.
static AGENT_SHELL_WORKING_DIRS: std::sync::RwLock<Vec<String>> =
    std::sync::RwLock::new(Vec::new());

/// Record the shell sandbox's `allowed_working_dirs` for
/// [`agent_writable_roots`]. The orchestrator calls this when it is built.
pub fn register_agent_shell_working_dirs(dirs: Vec<String>) {
    if let Ok(mut guard) = AGENT_SHELL_WORKING_DIRS.write() {
        *guard = dirs;
    }
}

/// `path` resolved the way the file sandbox resolves the paths it checks
/// (symlinks followed, a missing tail resolved through its deepest existing
/// ancestor), for OS profiles whose rules match resolved paths. `path` as
/// given when it cannot be resolved.
pub(crate) fn resolve_for_policy(path: &Path) -> PathBuf {
    resolve_path_for_policy(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Each configured root resolved for an OS profile, one at a time: a root
/// that fails is logged and, when absolute, kept as configured; a relative
/// one is dropped only when the process directory is gone. The process
/// directory is read only for relative roots.
fn resolve_roots_for_profile(roots: &[String], what: &str) -> Vec<PathBuf> {
    roots
        .iter()
        .filter_map(|root| {
            let path = PathBuf::from(root);
            let absolute = if path.is_absolute() {
                path
            } else {
                match std::env::current_dir() {
                    Ok(cwd) => cwd.join(path),
                    Err(error) => {
                        tracing::warn!(
                            root = %root,
                            kind = what,
                            %error,
                            "[FILE-SANDBOX] a relative root cannot be resolved without the process directory; it is left out of the OS sandbox"
                        );
                        return None;
                    },
                }
            };
            match resolve_path_for_policy(&absolute) {
                Ok(resolved) => Some(resolved),
                Err(error) => {
                    tracing::warn!(
                        root = %root,
                        kind = what,
                        %error,
                        "[FILE-SANDBOX] a root could not be resolved; the OS sandbox uses it as configured"
                    );
                    Some(absolute)
                },
            }
        })
        .collect()
}

/// The registered agent file sandbox's `read_only_roots`, resolved like the
/// paths checked against them, for the shell box and the coding sandbox.
/// Empty before boot or in `unrestricted` mode. Each root is resolved on its
/// own, so one that fails never drops the others.
pub(crate) fn agent_read_only_roots() -> Vec<PathBuf> {
    match agent_file_sandbox() {
        Some(sandbox) if !matches!(sandbox.mode, FileSandboxMode::Unrestricted) => {
            resolve_roots_for_profile(&sandbox.read_only_roots, "read-only")
        },
        _ => Vec::new(),
    }
}

/// The writable roots a coding sandbox without a write allow-list re-opens
/// inside a read-only root: the agents' configured `allowed_roots`, the
/// shell's `allowed_working_dirs`, and this execution's task root.
#[cfg_attr(
    not(any(
        target_os = "macos",
        target_os = "linux",
        test,
        feature = "test-fixtures"
    )),
    allow(dead_code)
)]
pub(crate) fn agent_writable_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = agent_file_sandbox()
        .map(|sandbox| resolve_roots_for_profile(&sandbox.allowed_roots, "writable"))
        .unwrap_or_default();
    let shell_dirs = AGENT_SHELL_WORKING_DIRS
        .read()
        .map(|guard| guard.clone())
        .unwrap_or_default();
    roots.extend(resolve_roots_for_profile(&shell_dirs, "shell working-dir"));
    if let Ok(task_root) = TASK_SHELL_ROOT.try_with(Clone::clone) {
        roots.push(resolve_for_policy(&task_root));
    }
    roots.sort();
    roots.dedup();
    roots
}

/// The registered agent file sandbox, or `None` before boot (tests).
pub(crate) fn agent_file_sandbox() -> Option<FileSandboxConfig> {
    #[cfg(any(test, feature = "test-fixtures"))]
    {
        if let Some(sandbox) = TEST_AGENT_FILE_SANDBOX.with(|cell| cell.borrow().clone()) {
            return Some(sandbox);
        }
    }
    AGENT_FILE_SANDBOX
        .read()
        .ok()
        .and_then(|guard| guard.clone())
}

#[cfg(any(test, feature = "test-fixtures"))]
thread_local! {
    static TEST_AGENT_FILE_SANDBOX: std::cell::RefCell<Option<FileSandboxConfig>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `body` on this thread with `sandbox` as the agent file sandbox,
/// without touching the process-wide one other tests may set.
#[cfg(any(test, feature = "test-fixtures"))]
pub(crate) fn with_agent_file_sandbox_for_test<T>(
    sandbox: FileSandboxConfig,
    body: impl FnOnce() -> T,
) -> T {
    /// Clears the override even when `body` panics, so a failed test cannot
    /// leak its sandbox into the next test on this thread.
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_AGENT_FILE_SANDBOX.with(|cell| *cell.borrow_mut() = None);
        }
    }
    TEST_AGENT_FILE_SANDBOX.with(|cell| *cell.borrow_mut() = Some(sandbox));
    let _reset = Reset;
    body()
}

/// Return action paths that fall outside the configured roots. This only
/// reports the recoverable root mismatch; read-only/delete policy and the
/// coding source-tree hard fence remain enforced by `execute_file_action`.
pub fn file_action_outside_allowed_roots(
    action: &FileAction,
    sandbox: &FileSandboxConfig,
) -> Result<Vec<String>, ExecutionError> {
    if matches!(sandbox.mode, FileSandboxMode::Unrestricted) {
        return Ok(Vec::new());
    }
    let roots = resolve_file_sandbox_roots(sandbox)?;
    // A read inside a read-only root is allowed, and a write there is a hard
    // refusal rather than a root the owner could approve, so neither is
    // reported.
    let mut outside = Vec::new();
    for (path, _write) in file_action_touched_paths(action) {
        let resolved = resolve_path_for_policy(path)?;
        if roots.access(&resolved) == RootAccess::Outside {
            outside.push(resolved.to_string_lossy().to_string());
        }
    }
    outside.sort();
    outside.dedup();
    Ok(outside)
}

fn ensure_path_permitted(
    path: &Path,
    write: bool,
    subtree: bool,
    roots: &FileSandboxRoots,
) -> Result<(), ExecutionError> {
    let resolved = resolve_path_for_policy(path)?;
    if roots.access(&resolved) == RootAccess::Outside {
        // Pure out-of-roots mismatch: the path is otherwise a valid target,
        // it just falls outside the currently-allowed sandbox roots. Surface
        // the RECOVERABLE `PathAccessDenied` so the executor chokepoint can
        // route it into the sandbox-override HITL (approve a folder → merge
        // into `session_file_sandbox_roots` → retry) instead of terminally
        // failing the iteration. Read-only / delete-policy / repo-fence
        // violations are handled BEFORE this call (in `validate_file_action`
        // and `validate_file_action_repo_fence`) and are terminal
        // `ExecutionError::Refused` answers, with no HITL.
        return Err(ExecutionError::PathAccessDenied {
            paths: vec![resolved.to_string_lossy().to_string()],
            roots_hint: roots.hint(),
        });
    }
    if !write {
        return Ok(());
    }
    // A policy refusal, not a missing root: approving a folder would not
    // change it, so it stays a hard `Step` and never offers the HITL.
    match roots.read_only_violation(path, &resolved, subtree)? {
        Some(refusal) => Err(ExecutionError::Step(refusal)),
        None => Ok(()),
    }
}

fn absolute_for_policy(path: &Path) -> Result<PathBuf, ExecutionError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()
        .map_err(|e| {
            ExecutionError::Step(format!(
                "Failed to resolve current working directory for '{}': {}",
                path.display(),
                e
            ))
        })?
        .join(path))
}

/// Where the directory entry `path` itself sits, without following a
/// symlink at its last component: its resolved parent plus its name. A
/// delete or move acts on that entry, not on what a link points to.
fn resolve_leaf_for_policy(path: &Path) -> Result<Option<PathBuf>, ExecutionError> {
    let absolute = absolute_for_policy(path)?;
    let (Some(parent), Some(name)) = (absolute.parent(), absolute.file_name()) else {
        return Ok(None);
    };
    Ok(Some(resolve_path_for_policy(parent)?.join(name)))
}

/// Symlinks followed while resolving one path before the check gives up.
const MAX_POLICY_SYMLINK_HOPS: usize = 40;

fn resolve_path_for_policy(path: &Path) -> Result<PathBuf, ExecutionError> {
    let mut absolute = absolute_for_policy(path)?;
    let mut hops = 0;
    'resolve: loop {
        if absolute.exists() {
            return absolute.canonicalize().map_err(|e| {
                ExecutionError::Step(format!(
                    "Failed to canonicalize path '{}': {}",
                    path.display(),
                    e
                ))
            });
        }

        // Canonicalize the nearest existing parent, then append unresolved
        // segments.
        let mut cursor = absolute.as_path();
        let mut unresolved = Vec::new();
        while !cursor.exists() {
            // A dangling symlink: `exists` follows it and says no, but a
            // write through it creates its target, so the target is what
            // the policy must check.
            if cursor
                .symlink_metadata()
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                hops += 1;
                if hops > MAX_POLICY_SYMLINK_HOPS {
                    return Err(ExecutionError::Step(format!(
                        "Path '{}' has too many symbolic links for sandbox checks",
                        path.display()
                    )));
                }
                let link = std::fs::read_link(cursor).map_err(|e| {
                    ExecutionError::Step(format!(
                        "Failed to read symbolic link '{}' for sandbox checks: {}",
                        cursor.display(),
                        e
                    ))
                })?;
                // `join` keeps an absolute link target as it is.
                let mut target = match cursor.parent() {
                    Some(parent) => parent.join(link),
                    None => link,
                };
                for segment in unresolved.iter().rev() {
                    target.push(segment);
                }
                absolute = target;
                continue 'resolve;
            }
            let segment = cursor.file_name().ok_or_else(|| {
                ExecutionError::Step(format!(
                    "Path '{}' has no existing ancestor for sandbox checks",
                    path.display()
                ))
            })?;
            unresolved.push(segment.to_os_string());
            cursor = cursor.parent().ok_or_else(|| {
                ExecutionError::Step(format!(
                    "Path '{}' has no existing ancestor for sandbox checks",
                    path.display()
                ))
            })?;
        }

        let mut canonical = cursor.canonicalize().map_err(|e| {
            ExecutionError::Step(format!(
                "Failed to canonicalize parent path for '{}': {}",
                path.display(),
                e
            ))
        })?;
        for segment in unresolved.iter().rev() {
            canonical.push(segment);
        }
        return Ok(canonical);
    }
}

/// Simple glob pattern matching (supports * and ?)
fn matches_glob_pattern(name: &str, pattern: &str) -> bool {
    let pattern_chars: Vec<char> = pattern.chars().collect();
    let name_chars: Vec<char> = name.chars().collect();
    matches_glob_recursive(&pattern_chars, &name_chars, 0, 0)
}

fn matches_glob_recursive(pattern: &[char], name: &[char], pi: usize, ni: usize) -> bool {
    if pi >= pattern.len() && ni >= name.len() {
        return true;
    }
    if pi >= pattern.len() {
        return false;
    }

    match pattern[pi] {
        '*' => {
            // Try matching zero or more characters
            for i in ni..=name.len() {
                if matches_glob_recursive(pattern, name, pi + 1, i) {
                    return true;
                }
            }
            false
        },
        '?' => {
            // Match exactly one character
            if ni < name.len() {
                matches_glob_recursive(pattern, name, pi + 1, ni + 1)
            } else {
                false
            }
        },
        c => {
            // Match literal character
            if ni < name.len() && name[ni] == c {
                matches_glob_recursive(pattern, name, pi + 1, ni + 1)
            } else {
                false
            }
        },
    }
}

// ============================================================================
// HTTP Executor
// ============================================================================

/// The `Idempotency-Key` this request should carry, if any.
///
/// Pure so the decision is testable without a network. The two ways it answers
/// `None` are each a deliberate policy, not an oversight:
///
/// 1. No `effect_id` — the dispatch is not attributable, so there is no honest
///    key to send. That means "unattributable", never "safe to repeat".
/// 2. A safe method. The key exists so a remote can tell a lost response from a
///    request that never arrived; for GET/HEAD/OPTIONS that distinction costs
///    nothing, since re-issuing is free. Sending one anyway would be a
///    per-attempt header on every read the agent performs — noise to the remote
///    and enough to defeat intermediary caching.
///
/// When it answers `Some`, the derived key REPLACES any `Idempotency-Key`
/// already on the action. That is not the obvious choice, so the reasoning: an
/// `http_post` step takes its headers from step parameters, and for an agent
/// tool call those parameters are the model's. A model-authored key is exactly
/// what this header must never be — reusing one across two different requests
/// makes a remote answer the second from the first's cached response, and the
/// second effect disappears with no error raised anywhere. Nothing at this
/// layer can tell a model-authored header from a pack author's, so the runtime
/// value wins whenever there is one, and a pack author loses nothing real: the
/// derived key is already stable across a crash-replay of one attempt and
/// distinct across a deliberate retry, which is all a correct scheme needs.
///
/// A dispatch with no `effect_id` still forwards whatever header it was given.
/// There is no runtime identity to substitute, and stripping the header would
/// break a pack that manages its own without putting anything in its place.
fn derived_idempotency_key(action: &HttpAction, effect_id: Option<&str>) -> Option<String> {
    let effect_id = effect_id?;

    let can_commit = matches!(
        action.method,
        HttpMethod::Post | HttpMethod::Put | HttpMethod::Patch | HttpMethod::Delete
    );
    if !can_commit {
        return None;
    }

    Some(LlmToolLineageIdentity::far_side_idempotency_key(effect_id))
}

/// Execute one HTTP action.
///
/// `effect_id` identifies the dispatch attempt this request belongs to. On a
/// request that can commit a change, a DERIVED key travels as `Idempotency-Key`
/// so a remote that honours it can collapse a repeat caused by a lost response
/// — see `derived_idempotency_key` for exactly when one is sent, and why the
/// raw id never is.
/// Whether the request an action describes can be constructed at all —
/// URL, method, header names and values, body. Checked before one-time
/// material is consumed for it (P4): a request that cannot be built is a
/// proven pre-dispatch failure, so the reservation is released rather than
/// spent. A request that builds may still fail to send; that is never
/// released, because nothing proves the destination did not receive it.
pub fn validate_http_request_buildable(action: &HttpAction) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .build()
        .map_err(|error| format!("Failed to create HTTP client: {error}"))?;
    build_http_request(&client, action, None)
        .build()
        .map(|_| ())
        .map_err(|error| format!("HTTP request for '{}' cannot be built: {error}", action.url))
}

fn build_http_request(
    client: &reqwest::Client,
    action: &HttpAction,
    effect_id: Option<&str>,
) -> reqwest::RequestBuilder {
    let mut request = match action.method {
        HttpMethod::Get => client.get(&action.url),
        HttpMethod::Post => client.post(&action.url),
        HttpMethod::Put => client.put(&action.url),
        HttpMethod::Patch => client.patch(&action.url),
        HttpMethod::Delete => client.delete(&action.url),
        HttpMethod::Head => client.head(&action.url),
        HttpMethod::Options => client.request(reqwest::Method::OPTIONS, &action.url),
    };

    let derived_key = derived_idempotency_key(action, effect_id);

    // Add headers
    for (key, value) in &action.headers {
        // Skipped, not overwritten later: `header()` APPENDS, so leaving this
        // in would send the request with two `Idempotency-Key` values and no
        // remote is obliged to make sense of that. Matched case-insensitively
        // because HTTP field names are.
        if derived_key.is_some() && key.eq_ignore_ascii_case("idempotency-key") {
            continue;
        }
        request = request.header(key, value);
    }

    if let Some(key) = derived_key {
        request = request.header("Idempotency-Key", key);
    }

    // Add content-type if specified
    if let Some(content_type) = &action.content_type {
        request = request.header("Content-Type", content_type);
    }

    // Add body if present
    if let Some(body) = &action.body {
        request = request.body(body.clone());
    }
    request
}

/// The origin (`scheme://host[:port]`) of a URL: the redirect check, and the
/// destination vocabulary an HTTP claim and an HTTP challenge share with the
/// browser lane's canonical origin.
pub fn http_origin(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    Some(match parsed.port() {
        Some(port) => format!("{}://{host}:{port}", parsed.scheme()),
        None => format!("{}://{host}", parsed.scheme()),
    })
}

/// The header under which an HTTP result names the URL its response actually
/// came from, when redirects moved it. A colon-initial name no real response
/// can carry.
pub const FINAL_URL_PSEUDO_HEADER: &str = ":final-url";

/// How far one `http` request may reach under the network policy.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HttpReach {
    /// `network_policy: allow`: anywhere, redirects included.
    Open,
    /// A destination the policy would refuse, let through on its own:
    /// `allow_private` lists it, or the owner allowed it in god mode. Only
    /// this host and port; a redirect anywhere else is returned, not
    /// followed, so the next request is checked (and asked for) again.
    Granted {
        host: String,
        port: u16,
        runtime_api: bool,
    },
    /// Under the policy's filtering.
    Filtered,
}

/// How far the `http` tool may reach `url`: `Open` when the policy is
/// `allow`, `Granted` when `allow_private` lists it or the owner allowed it
/// in god mode, `Filtered` to go on filtered, `Err` when the policy refuses
/// it and god mode is not on (or the owner said no).
async fn http_destination_reach(url: &str, fenced: bool) -> Result<HttpReach, ExecutionError> {
    use crate::magician_v2::execution::network_policy;
    // The runtime API is askable in God mode, on its own origin. Grant channels
    // and other protected local services retain their hard fence.
    if let Some(reason) = loopback_service_refusal(url).await {
        let parsed = url::Url::parse(url)
            .map_err(|error| ExecutionError::Step(format!("Invalid URL '{url}': {error}")))?;
        if !fenced
            && runtime_api_port(parsed.port_or_known_default().unwrap_or(0))
            && crate::magician_v2::god_mode::active()
        {
            let host = parsed.host_str().unwrap_or_default();
            let port = parsed.port_or_known_default().unwrap_or(0);
            let reach = authorize_http_destination(host, port, &reason, true).await?;
            if acting_agent_fenced() {
                return Err(ExecutionError::Refused(
                    "HTTP request blocked: this agent is fenced from a goal workspace".into(),
                ));
            }
            return Ok(reach);
        }
        return Err(ExecutionError::Step(reason));
    }
    // An agent fenced from a goal workspace reaches public addresses (and
    // `allow_private`) only, in every mode but `deny`, and god mode cannot
    // widen that: a private address may be a door past the fence.
    if fenced && network_policy::mode() != crate::config::NetworkMode::Deny {
        return fenced_destination_reach(url).await;
    }
    if network_policy::mode() == crate::config::NetworkMode::Allow {
        return Ok(HttpReach::Open);
    }
    let parsed = url::Url::parse(url)
        .map_err(|error| ExecutionError::Step(format!("Invalid URL '{url}': {error}")))?;
    let host = parsed.host_str().unwrap_or_default().to_string();
    let port = parsed.port_or_known_default().unwrap_or(0);
    let granted = HttpReach::Granted {
        host: host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string(),
        port,
        runtime_api: false,
    };
    if network_policy::allowed_private(&host, port) {
        return Ok(granted);
    }
    let Err(reason) = network_policy::resolve_checked(&host, port).await else {
        return Ok(HttpReach::Filtered);
    };
    if !crate::magician_v2::god_mode::active() {
        return Err(ExecutionError::Step(format!(
            "HTTP request blocked: {reason}"
        )));
    }
    authorize_http_destination(&host, port, &reason, false).await
}

async fn authorize_http_destination(
    host: &str,
    port: u16,
    reason: &str,
    runtime_api: bool,
) -> Result<HttpReach, ExecutionError> {
    crate::magician_v2::god_mode::authorize(crate::magician_v2::god_mode::GodAction::Network {
        host: host.trim_start_matches('[').trim_end_matches(']'),
        port,
        violation: reason,
    })
    .await
    .map(|()| HttpReach::Granted {
        host: host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string(),
        port,
        runtime_api,
    })
    .map_err(|why| {
        ExecutionError::Step(format!("HTTP request blocked: {reason} (god mode: {why})"))
    })
}

/// Where an agent fenced from a goal workspace may send an `http` request:
/// an `allow_private` destination on its own (`Granted`), or a host whose
/// every address is public (`Filtered`, and the client then resolves every
/// connection, redirects included, through `StrictPublicResolver`).
async fn fenced_destination_reach(url: &str) -> Result<HttpReach, ExecutionError> {
    use crate::magician_v2::execution::network_policy;
    let parsed = url::Url::parse(url)
        .map_err(|error| ExecutionError::Step(format!("Invalid URL '{url}': {error}")))?;
    let host = parsed
        .host_str()
        .unwrap_or_default()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port = parsed.port_or_known_default().unwrap_or(0);
    if network_policy::allowed_private(&host, port) {
        return Ok(HttpReach::Granted {
            host,
            port,
            runtime_api: false,
        });
    }
    let addrs: Vec<std::net::SocketAddr> = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => vec![std::net::SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((host.as_str(), port))
            .await
            .map(|addrs| addrs.collect())
            .unwrap_or_default(),
    };
    if addrs.is_empty()
        || addrs
            .iter()
            .any(|addr| !network_policy::is_public_unmapped(addr.ip()))
    {
        return Err(ExecutionError::Step(format!(
            "HTTP request blocked: {host}:{port} is not a public address, and this agent is fenced \
             from a goal workspace (only public hosts and allow_private are reachable)"
        )));
    }
    Ok(HttpReach::Filtered)
}

/// Only the runtime HTTP API is eligible for this God-mode override. The
/// plane's MCP and grant-delivery listeners are never ordinary HTTP targets.
fn runtime_api_port(port: u16) -> bool {
    use crate::magician_v2::execution::shell_box::{
        loopback_services, plane_mcp_port, LoopbackService,
    };
    plane_mcp_port() != Some(port)
        && !crate::magician_v2::execution::plane::grant_channel::is_channel_port(port)
        && loopback_services()
            .into_iter()
            .any(|(candidate, kind)| candidate == port && kind == LoopbackService::MagicianApi)
}

/// The local service on `port` the normal sandbox refuses over HTTP, if
/// any: Magician's own API (it would bypass the plane's gates), the plane's
/// MCP listener (harnesses only), the supervisor, Magicutor, CDP and the
/// notes server; a registered UI dev server unless the acting agent
/// is the admin (or the owner's own VibeDev session, which acts as it).
fn refused_service_on(
    port: u16,
) -> Option<crate::magician_v2::execution::shell_box::LoopbackService> {
    use crate::magician_v2::execution::shell_box::{
        loopback_services, plane_mcp_port, LoopbackService,
    };
    if plane_mcp_port() == Some(port)
        || crate::magician_v2::execution::plane::grant_channel::is_channel_port(port)
    {
        return Some(LoopbackService::MagicianApi);
    }
    let kind = loopback_services()
        .into_iter()
        .find(|(candidate, _)| *candidate == port)
        .map(|(_, kind)| kind)?;
    if kind == LoopbackService::DevServer {
        let agent = current_acting_agent();
        let admin = crate::magician_v2::execution::goal_workspace_access::admin_agent();
        if agent.as_deref() == Some(admin.as_str()) {
            return None;
        }
    }
    Some(kind)
}

/// Why `url` is refused as a local service no agent may call over HTTP
/// ([`refused_service_on`]) on an address of this host: loopback, an
/// unspecified or IPv4-mapped spelling, a name resolving there
/// (`localtest.me`, `localhost.`), or one of this host's LAN addresses.
async fn loopback_service_refusal(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let port = parsed.port_or_known_default()?;
    let kind = refused_service_on(port)?;
    let host = parsed
        .host_str()?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let local = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => is_local_address(ip),
        Err(_) => tokio::net::lookup_host((host.as_str(), port))
            .await
            .map(|mut addrs| addrs.any(|addr| is_local_address(addr.ip())))
            .unwrap_or(true),
    };
    local.then(|| {
        format!("HTTP request blocked: {host}:{port} is a local service agents may not reach ({kind:?})")
    })
}

/// Why one redirect hop to `next` is not followed, checked before it is
/// made: a scheme other than http(s); a local service port
/// ([`refused_service_on`]) on a literal address of this host, or on any
/// name (a name is not resolved inside the redirect policy, so such a hop is
/// refused rather than guessed); and, for an agent fenced from a goal
/// workspace, a literal non-public address not on `allow_private` (names
/// are vetted by `StrictPublicResolver` as they connect).
fn redirect_hop_refusal(next: &url::Url, fenced: bool) -> Option<String> {
    use crate::magician_v2::execution::network_policy;
    if !matches!(next.scheme(), "http" | "https") {
        return Some(format!(
            "redirect to a {} URL is not followed",
            next.scheme()
        ));
    }
    let host = next
        .host_str()
        .unwrap_or_default()
        .trim_start_matches('[')
        .trim_end_matches(']');
    let port = next.port_or_known_default().unwrap_or(0);
    let literal = host.parse::<std::net::IpAddr>().ok();
    if let Some(kind) = refused_service_on(port) {
        if literal.is_none_or(is_local_address) {
            return Some(format!(
                "redirect to {host}:{port} blocked: a local service agents may not reach ({kind:?})"
            ));
        }
    }
    if fenced && !network_policy::allowed_private(host, port) {
        if let Some(ip) = literal.filter(|ip| !network_policy::is_public_unmapped(*ip)) {
            return Some(format!(
                "redirect to {ip} blocked: this agent is fenced from a goal workspace and reaches public addresses only"
            ));
        }
    }
    None
}

/// Whether `ip` is an address of this host (`network_policy::is_local_address`).
fn is_local_address(ip: std::net::IpAddr) -> bool {
    crate::magician_v2::execution::network_policy::is_local_address(ip)
}

/// Whether the acting agent is fenced from a goal workspace (one exists that
/// it may not wholly read).
fn acting_agent_fenced() -> bool {
    crate::magician_v2::execution::goal_workspace_access::denies_for_current().guards_workspaces
}

/// Whether a redirect from a `Granted` destination (`host`:`port`) to `next`
/// is followed: the same host and port, or, under `public`, an IP literal the
/// policy itself lets through (public, or in `allow_private`). A hop to any
/// other name could resolve anywhere (no filtering resolver is installed for
/// a granted request), so it is not followed.
fn granted_redirect_followed(host: &str, port: u16, public: bool, next: &url::Url) -> bool {
    use crate::magician_v2::execution::network_policy;
    let next_host = next
        .host_str()
        .unwrap_or_default()
        .trim_start_matches('[')
        .trim_end_matches(']');
    let next_port = next.port_or_known_default().unwrap_or(0);
    if next_host.eq_ignore_ascii_case(host) && next_port == port {
        return true;
    }
    public
        && next_host.parse::<std::net::IpAddr>().is_ok_and(|ip| {
            network_policy::is_public_ip(ip)
                || network_policy::allowed_private(next_host, next_port)
        })
}

pub async fn execute_http_action(
    action: &HttpAction,
    effect_id: Option<&str>,
) -> Result<ActionResult, ExecutionError> {
    debug!("Executing HTTP action: {}", action.description());

    // A request carrying a user-typed credential (P4) is delivered to its
    // bound origin and nowhere else: redirects are never followed for it,
    // whatever the action asked, and a cross-origin one below ends the
    // attempt with the credential unforwarded.
    let follow_redirects = action.follow_redirects && !action.carries_credential;

    // `execution.network_policy`: a destination the policy refuses is refused,
    // or in god mode asked for (`http_destination_reach`); one let through on
    // its own follows redirects only on the same host and port. Otherwise,
    // under `public`, every connection (redirects included) resolves through a
    // public-only resolver, and an IP-literal redirect is checked as it comes.
    let fenced = acting_agent_fenced();
    let reach = http_destination_reach(&action.url, fenced).await?;
    let public =
        crate::magician_v2::execution::network_policy::mode() == crate::config::NetworkMode::Public;
    let filtered = reach == HttpReach::Filtered && (public || fenced);
    // A protected service is never reached through an unrelated redirect.
    // An explicitly approved runtime API may redirect within its own origin.
    let redirect_policy = match (follow_redirects, filtered, reach) {
        (false, _, _) => reqwest::redirect::Policy::none(),
        (
            true,
            false,
            HttpReach::Granted {
                host,
                port,
                runtime_api,
            },
        ) => {
            let approved_api_origin = runtime_api.then(|| http_origin(&action.url)).flatten();
            reqwest::redirect::Policy::custom(move |attempt| {
                let approved_api_hop = runtime_api
                    && !fenced
                    && runtime_api_port(port)
                    && approved_api_origin.as_ref().is_some_and(|origin| {
                        http_origin(attempt.url().as_str()).as_ref() == Some(origin)
                    });
                if runtime_api && !approved_api_hop {
                    attempt.error("redirect outside the approved runtime API origin blocked")
                } else if let Some(reason) =
                    redirect_hop_refusal(attempt.url(), fenced).filter(|_| !approved_api_hop)
                {
                    attempt.error(reason)
                } else if attempt.previous().len() >= 10 {
                    attempt.error("too many redirects")
                } else if granted_redirect_followed(&host, port, public && !fenced, attempt.url()) {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            })
        },
        (true, false, _) => reqwest::redirect::Policy::custom(move |attempt| {
            if let Some(reason) = redirect_hop_refusal(attempt.url(), fenced) {
                attempt.error(reason)
            } else if attempt.previous().len() >= 10 {
                attempt.error("too many redirects")
            } else {
                attempt.follow()
            }
        }),
        (true, true, _) => reqwest::redirect::Policy::custom(move |attempt| {
            if let Some(reason) = redirect_hop_refusal(attempt.url(), fenced) {
                return attempt.error(reason);
            }
            let literal_private = attempt
                .url()
                .host_str()
                .map(|host| host.trim_start_matches('[').trim_end_matches(']'))
                .and_then(|host| host.parse::<std::net::IpAddr>().ok())
                .is_some_and(|ip| {
                    !crate::magician_v2::execution::network_policy::is_public_ip(ip)
                        && !crate::magician_v2::execution::network_policy::allowed_private(
                            attempt.url().host_str().unwrap_or_default(),
                            attempt.url().port_or_known_default().unwrap_or(0),
                        )
                });
            if literal_private {
                attempt.error("redirect to a non-public address blocked by network_policy")
            } else if attempt.previous().len() >= 10 {
                attempt.error("too many redirects")
            } else {
                attempt.follow()
            }
        }),
    };

    // Build client with timeout and redirect settings
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(action.timeout_secs.unwrap_or(30)))
        .redirect(redirect_policy);
    if filtered && fenced {
        builder = builder.dns_resolver(std::sync::Arc::new(
            crate::magician_v2::execution::network_policy::StrictPublicResolver,
        ));
    } else if filtered {
        if let Some(resolver) = crate::magician_v2::execution::network_policy::http_resolver() {
            builder = builder.dns_resolver(resolver);
        }
    }
    let client = builder
        .build()
        .map_err(|e| ExecutionError::Step(format!("Failed to create HTTP client: {}", e)))?;

    let request = build_http_request(&client, action, effect_id);

    // Send request
    let response = request.send().await.map_err(|e| {
        // With its causes: a refused redirect hop says why only there.
        let mut message = e.to_string();
        let mut cause = std::error::Error::source(&e);
        while let Some(inner) = cause {
            message.push_str(": ");
            message.push_str(&inner.to_string());
            cause = inner.source();
        }
        ExecutionError::Step(format!(
            "HTTP request failed for '{}': {}",
            action.url, message
        ))
    })?;

    if action.carries_credential && response.status().is_redirection() {
        let target = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|location| {
                url::Url::parse(&action.url)
                    .ok()
                    .and_then(|base| base.join(location).ok())
                    .map(|joined| joined.to_string())
            });
        let same_origin = target
            .as_deref()
            .and_then(http_origin)
            .is_some_and(|origin| http_origin(&action.url).as_deref() == Some(origin.as_str()));
        if !same_origin {
            return Err(ExecutionError::Step(format!(
                "destination changed: '{}' answered {} with a redirect to another origin; the \
                 credential was delivered to the original destination only and was not forwarded. \
                 Re-observe the authentication state before trying again.",
                action.url,
                response.status().as_u16()
            )));
        }
    }

    let status = response.status().as_u16();
    // Where the response actually came from, when redirects moved the request
    // (a followed hop): a pseudo-header no real response can carry, so the
    // challenge detector binds the answering host, not the one asked.
    let final_url = response.url().to_string();

    // Collect headers
    let mut headers = HashMap::new();
    for (key, value) in response.headers() {
        if let Ok(v) = value.to_str() {
            headers.insert(key.as_str().to_string(), v.to_string());
        }
    }
    if final_url != action.url {
        headers.insert(FINAL_URL_PSEUDO_HEADER.to_string(), final_url);
    }

    // Get response body
    let content_type = headers
        .get("content-type")
        .cloned()
        .or_else(|| headers.get("Content-Type").cloned());
    let body = response.text().await.unwrap_or_default();
    let raw_len = body.len();
    let body = visible_http_body(content_type.as_deref(), body);

    info!(
        "HTTP {:?} {} -> {} (raw {} bytes, stored {} bytes)",
        action.method,
        action.url,
        status,
        raw_len,
        body.len()
    );

    Ok(ActionResult::Http {
        status,
        headers,
        body,
    })
}

const HTTP_BODY_MODEL_CHARS: usize = 12_000;

/// Model-visible HTTP body. Raw HTML dumps (Bing/DDG scrapes) were being
/// stored as 200KB+ strings; `read_result` then paged them as an empty JSON
/// container. Strip tags for HTML and cap length so the first page is text.
fn visible_http_body(content_type: Option<&str>, body: String) -> String {
    let html = content_type.is_some_and(|value| {
        let lower = value.to_ascii_lowercase();
        lower.contains("text/html") || lower.contains("application/xhtml")
    });
    let text = if html {
        html_to_visible_text(&body)
    } else {
        body
    };
    let count = text.chars().count();
    if count <= HTTP_BODY_MODEL_CHARS {
        return text;
    }
    let truncated: String = text.chars().take(HTTP_BODY_MODEL_CHARS).collect();
    format!("{truncated}\n…[truncated {count} chars]")
}

fn html_to_visible_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len().min(HTTP_BODY_MODEL_CHARS * 2));
    let mut in_tag = false;
    let mut skipping = false;
    let bytes = html.as_bytes();
    let lower = html.to_ascii_lowercase();
    let lower_bytes = lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !in_tag && bytes[i] == b'<' {
            in_tag = true;
            if lower[i..].starts_with("<script") || lower[i..].starts_with("<style") {
                skipping = true;
            }
            i += 1;
            continue;
        }
        if in_tag && bytes[i] == b'>' {
            in_tag = false;
            if skipping
                && i >= 7
                && (lower_bytes
                    .get(i.saturating_sub(7)..i)
                    .is_some_and(|w| w == b"/script")
                    || lower_bytes
                        .get(i.saturating_sub(6)..i)
                        .is_some_and(|w| w == b"/style"))
            {
                skipping = false;
            }
            i += 1;
            continue;
        }
        if in_tag || skipping {
            i += 1;
            continue;
        }
        let ch = html[i..].chars().next().unwrap_or(' ');
        if ch.is_whitespace() {
            if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
        i += ch.len_utf8();
    }
    out.trim().to_string()
}

// ============================================================================
// Bash Executor
// ============================================================================

/// Context for streaming shell output via the event broadcaster.
///
/// When provided to `execute_bash_action`, output lines are batched and
/// broadcast as `ShellOutputChunk` events every 100ms. The function still
/// returns `ActionResult::Text` with the full output (backward compatible).
#[derive(Clone)]
pub struct ShellStreamContext {
    pub broadcaster: Arc<RuntimeTransportBroadcaster>,
    pub execution_id: String,
    pub principal: Option<String>,
    pub workspace: Option<String>,
    /// Immutable plan/runtime step identity. Iteration attribution is always
    /// derived from this field; parsing a prior rendered `step_id` would collide
    /// with perfectly legal identifiers that already contain `-iter-`.
    pub base_step_id: String,
    pub step_id: String,
    pub step_index: usize,
    /// The run's delivered-values set (P4): every streamed chunk and the
    /// returned output are scrubbed with it before they leave this path, so
    /// a process that echoes a secret it was fed never reaches the event bus
    /// or the model with it. `None` in tests that stream nothing secret.
    pub scrub: Option<Arc<std::sync::Mutex<crate::magician_v2::secrets::KnownSecretValues>>>,
}

impl ShellStreamContext {
    /// The values to scrub, computed once per flush: the delivered set can
    /// grow while a process runs, and every flush reads the current set.
    fn scrub_replacements(&self) -> Vec<String> {
        self.scrub
            .as_ref()
            .and_then(|delivered| delivered.lock().ok())
            .map(|delivered| crate::magician_v2::secrets::known_value_replacements(&delivered))
            .unwrap_or_default()
    }

    fn scrub_text(&self, text: &str) -> String {
        let replacements = self.scrub_replacements();
        if replacements.is_empty() {
            return text.to_string();
        }
        replacements.iter().fold(text.to_string(), |acc, value| {
            acc.replace(value, "[REDACTED]")
        })
    }
}

/// Streaming caps to prevent runaway output from overwhelming the event bus.
const STREAM_BATCH_INTERVAL_MS: u64 = 100;
const STREAM_MAX_CHUNK_BYTES: usize = 50 * 1024; // 50KB per chunk
const STREAM_MAX_TOTAL_BYTES: usize = 1024 * 1024; // 1MB total
const STREAM_MAX_TOTAL_LINES: usize = 10_000;

/// Execute a bash action using tokio::process
pub async fn execute_bash_action(
    action: &BashAction,
    sandbox: &ShellSandboxConfig,
    stream_ctx: Option<ShellStreamContext>,
    // The run's delivered-values set (P4), for the path that has no streaming
    // context. A shell's stdin is a credential sink, so what the process
    // echoes back must be scrubbed on BOTH paths — the blocking one wrote the
    // raw child output to `magician.log` and into the returned error, and it is
    // the path taken whenever the run has no broadcaster, no runtime execution
    // id or no step id.
    scrub: Option<Arc<std::sync::Mutex<crate::magician_v2::secrets::KnownSecretValues>>>,
) -> Result<ActionResult, ExecutionError> {
    let (action, sandbox, unboxed) = authorize_bash_action(action, sandbox).await?;
    let action = &action;
    let boxed = (!unboxed).then_some(&sandbox);

    // If no streaming context or no output capture, use the original blocking path.
    if stream_ctx.is_none() || !action.capture_output {
        let scrub = scrub.or_else(|| stream_ctx.as_ref().and_then(|ctx| ctx.scrub.clone()));
        return execute_bash_action_blocking(action, boxed, scrub).await;
    }

    let ctx = stream_ctx.unwrap();
    execute_bash_action_streaming(action, boxed, ctx).await
}

/// Resolve task confinement and obtain approval before a worker leaves this scope.
async fn authorize_bash_action(
    action: &BashAction,
    sandbox: &ShellSandboxConfig,
) -> Result<(BashAction, ShellSandboxConfig, bool), ExecutionError> {
    let sandbox = task_shell_sandbox(sandbox);
    let task_action = TASK_SHELL_ROOT
        .try_with(|root| task_scoped_bash_action(action, root))
        .ok()
        .transpose()?;
    let action = task_action.as_ref().unwrap_or(action);
    debug!("Executing bash action: {}", action.description());

    // Hard violations always block; soft violations (unlisted binaries) also block here
    // because this path is reached only when on_violation == Fail (the Ask path is
    // handled by the executor pre-check before calling execute_bash_action).
    let refusal = validate_shell_action_hard(action, &sandbox)
        .and_then(|()| validate_shell_action_soft(action, &sandbox))
        .err();
    if refusal.is_none() {
        // A shell reads whatever its box leaves it: the run's evidence is
        // fenced like that content (`goal_workspace_access`).
        crate::magician_v2::execution::goal_workspace_access::record_possible_reads();
    }
    // The OS box (`shell_sandbox.os_enforce`) applies outside coding runs.
    // God mode takes a command out of it only after the owner allows that
    // command; with no box, only what the string checks refuse asks.
    let os_box = sandbox.os_enforce
        && crate::magician_v2::execution::shell_box::available()
        && !crate::magician_v2::execution::coding_engine::coding_context_active();
    // Goal workspaces this agent may not fully reach are fenced by the OS box
    // (or, in a coding run, the coding profile). With neither, the shell is
    // refused (fail closed), and god mode never takes such a command out of
    // the box: it may lift only the string checks' refusal.
    let workspace_fenced =
        crate::magician_v2::execution::goal_workspace_access::denies_for_current()
            .guards_workspaces;
    if workspace_fenced {
        let coding_box = crate::magician_v2::execution::coding_engine::coding_context_active()
            && crate::magician_v2::execution::coding_engine::os_sandbox_available();
        if !os_box && !coding_box {
            return Err(ExecutionError::Step(
                "Shell is unavailable to this agent here: goal workspaces need the OS sandbox \
                 (execution.shell_sandbox.os_enforce with a working sandbox-exec or bwrap) to keep \
                 them from agents that are not members. Use the files tools instead."
                    .to_string(),
            ));
        }
    }
    let unboxed = if crate::magician_v2::god_mode::active() && workspace_fenced {
        if let Some(refusal) = refusal {
            god_mode_shell_override(action, &sandbox, Some(refusal)).await?;
        }
        false
    } else if crate::magician_v2::god_mode::active() && (os_box || refusal.is_some()) {
        god_mode_shell_override(action, &sandbox, refusal).await?;
        true
    } else if let Some(refusal) = refusal {
        return Err(refusal);
    } else {
        false
    };
    Ok((action.clone(), sandbox, unboxed))
}

pub(crate) async fn prepare_managed_shell(
    action: &BashAction,
    sandbox: &ShellSandboxConfig,
) -> Result<(BashAction, tokio::process::Command), ExecutionError> {
    let (action, sandbox, unboxed) = authorize_bash_action(action, sandbox).await?;
    let command = build_bash_command(&action, (!unboxed).then_some(&sandbox))?;
    Ok((action, command))
}

/// The `sh -c <command>` invocation both bash paths spawn, short of stdio
/// and the timeout: the shell resolved against the PATH the child will
/// receive, the working directory validated, the action's env applied
/// verbatim, then the git ceiling fence. Kept spawn-free so the spawn
/// contract can be asserted without starting a process.
fn build_bash_command(
    action: &BashAction,
    // The sandbox to box the command in (`shell_box`), or `None` to run it
    // unboxed (god mode, after the owner allowed it).
    boxed: Option<&ShellSandboxConfig>,
) -> Result<tokio::process::Command, ExecutionError> {
    use std::ffi::{OsStr, OsString};

    // The action's env is model-supplied and applied unfiltered below. When
    // it carries `PATH`, a bare shell name would force std onto `fork`
    // instead of `posix_spawn` — and `spawn` is synchronous, so the timeout
    // around the wait could not rescue a forked copy hung in an atfork
    // handler. Resolve the shell against the PATH the child actually gets:
    // the model's when it supplied one, else the process PATH (see
    // `runtime_core::process`).
    let shell = runtime_core::process::resolve_program(
        OsStr::new("sh"),
        action.env.get("PATH").map(OsStr::new),
    );
    let shell_args = [
        OsString::from("-c"),
        OsString::from(action.command.as_str()),
    ];
    // The general OS box (`shell_box`) when it applies; otherwise the coding
    // OS-sandbox gate (repo read-only), identical to `sh -c <command>` when
    // that gate is off. See `os_sandbox_command`.
    let shell_box = boxed.and_then(|sandbox| {
        crate::magician_v2::execution::shell_box::wrap(shell.as_os_str(), &shell_args, sandbox)
    });
    let mut cmd = match &shell_box {
        Some(wrapped) => {
            let mut cmd = tokio::process::Command::new(&wrapped.program);
            cmd.args(&wrapped.args);
            cmd
        },
        None => crate::magician_v2::execution::coding_engine::os_sandbox_command(
            shell.as_os_str(),
            &shell_args,
        ),
    };
    super::shell_process::ProcessGroup::configure(&mut cmd);

    if let Some(dir) = &action.working_dir {
        if !dir.exists() {
            return Err(ExecutionError::Step(format!(
                "Working directory '{}' does not exist",
                dir.display()
            )));
        }
        cmd.current_dir(dir);
    }

    for (key, value) in &action.env {
        cmd.env(key, value);
    }
    // Helper subprocesses share this owned group so task cancellation also
    // reaches their workers; model-supplied env cannot detach the OCR adapter.
    cmd.env("MAGICIAN_MANAGED_SHELL", "1");
    // The box's proxy env, after the action's so a model-supplied proxy
    // cannot replace it.
    if let Some(wrapped) = &shell_box {
        for (key, value) in &wrapped.env {
            cmd.env(key, value);
        }
    }

    // Isolation: stop git from walking above the sandbox base to discover the
    // live repo's .git (defense-in-depth for shadow workspaces nested inside the
    // repo tree). Set after the action env so the fence value is authoritative.
    if let Some((_, sandbox_base)) =
        crate::magician_v2::execution::coding_engine::live_repo_source_fence()
    {
        cmd.env("GIT_CEILING_DIRECTORIES", &sandbox_base);
    }

    Ok(cmd)
}

/// Original blocking execution path (no streaming).
async fn execute_bash_action_blocking(
    action: &BashAction,
    boxed: Option<&ShellSandboxConfig>,
    scrub: Option<Arc<std::sync::Mutex<crate::magician_v2::secrets::KnownSecretValues>>>,
) -> Result<ActionResult, ExecutionError> {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;

    let mut cmd = build_bash_command(action, boxed)?;

    let timeout_secs = action
        .timeout_secs
        .unwrap_or(DEFAULT_SHELL_TIMEOUT_SECS)
        .max(1);
    let timeout_duration = Duration::from_secs(timeout_secs);

    cmd.stdin(if action.stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(if action.capture_output {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stderr(if action.capture_output {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = cmd
        .spawn()
        .map_err(|e| ExecutionError::Step(format!("Failed to spawn command: {e}")))?;
    let mut group = super::shell_process::ProcessGroup::new(&child);
    let stdin = child.stdin.take();
    let work = async {
        let input = async {
            if let (Some(payload), Some(mut stdin)) = (action.stdin.as_ref(), stdin) {
                if let Err(e) = stdin.write_all(payload.as_bytes()).await {
                    warn!("Failed to write stdin to child: {}", e);
                }
            }
        };
        // Drain output while writing input: both pipes can fill simultaneously.
        let (_, output) = tokio::join!(input, child.wait_with_output());
        output
    };
    let output = match timeout(timeout_duration, work).await {
        Ok(result) => {
            result.map_err(|e| ExecutionError::Step(format!("Failed to wait for command: {e}")))?
        },
        Err(_) => {
            return Err(ExecutionError::Step(format!(
                "Command timed out after {timeout_secs} seconds"
            )))
        },
    };
    group.terminate();

    let exit_code = output.status.code().unwrap_or(-1);
    // Scrubbed before anything reads it: the log line, the returned error and
    // the result all carry whatever the process wrote, and a tool fed a
    // credential on stdin commonly echoes it.
    let replacements = scrub
        .as_ref()
        .and_then(|delivered| delivered.lock().ok())
        .map(|delivered| crate::magician_v2::secrets::known_value_replacements(&delivered))
        .unwrap_or_default();
    let scrubbed = |text: String| -> String {
        replacements
            .iter()
            .fold(text, |acc, value| acc.replace(value, "[REDACTED]"))
    };
    let stdout = scrubbed(String::from_utf8_lossy(&output.stdout).to_string());
    let stderr = scrubbed(String::from_utf8_lossy(&output.stderr).to_string());

    if !output.status.success() {
        warn!(
            "Command failed with exit code {}: {}",
            exit_code,
            if stderr.is_empty() { &stdout } else { &stderr }
        );
        return Err(ExecutionError::Step(format!(
            "Command failed with exit code {}: {}",
            exit_code,
            if stderr.is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            }
        )));
    }

    info!(
        "Command completed with exit code {}, {} bytes output",
        exit_code,
        stdout.len()
    );

    Ok(ActionResult::text(stdout))
}

/// Streaming execution path: reads stdout/stderr line-by-line, batches output
/// every 100ms, and broadcasts `ShellOutputChunk` events via the event bus.
async fn execute_bash_action_streaming(
    action: &BashAction,
    boxed: Option<&ShellSandboxConfig>,
    ctx: ShellStreamContext,
) -> Result<ActionResult, ExecutionError> {
    use std::process::Stdio;

    let mut cmd = build_bash_command(action, boxed)?;
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    // The action's stdin payload is the process's only credential sink
    // (P4): piped and closed after the write exactly as the blocking path
    // does, never inherited from the service.
    cmd.stdin(if action.stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });

    let timeout_secs = action
        .timeout_secs
        .unwrap_or(DEFAULT_SHELL_TIMEOUT_SECS)
        .max(1);
    let timeout_duration = Duration::from_secs(timeout_secs);
    let deadline = tokio::time::Instant::now()
        .checked_add(timeout_duration)
        .ok_or_else(|| ExecutionError::AgentActionable("Shell timeout is too large".to_string()))?;

    let mut child = cmd
        .spawn()
        .map_err(|e| ExecutionError::Step(format!("Failed to spawn command: {}", e)))?;

    let mut group = super::shell_process::ProcessGroup::new(&child);
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();
    if let (Some(payload), Some(mut child_stdin)) = (action.stdin.clone(), child.stdin.take()) {
        // Written on its own task so a child that never reads cannot stall
        // the output readers; the write end drops (EOF) either way.
        tokio::spawn(async move {
            if let Err(e) = child_stdin.write_all(payload.as_bytes()).await {
                warn!("Failed to write stdin to streamed child: {}", e);
            }
            drop(child_stdin);
        });
    }

    // Channel for reader tasks to send lines to the main batching loop.
    // Tagged with stream name ("stdout" / "stderr").
    let (line_tx, mut line_rx) = tokio::sync::mpsc::channel::<(String, String)>(256);

    // Spawn stdout reader task (store handle for cleanup on early return)
    // Uses raw byte reading with `read_until` to handle invalid UTF-8 gracefully
    // (AsyncBufReadExt::lines() silently drops lines containing non-UTF-8 bytes).
    let stdout_task = if let Some(stdout) = stdout_handle {
        let tx = line_tx.clone();
        Some(tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf)
                            .trim_end_matches('\n')
                            .to_string();
                        if tx.send(("stdout".to_string(), line)).await.is_err() {
                            break;
                        }
                    },
                    Err(e) => {
                        tracing::warn!("Error reading stdout stream: {}", e);
                        break;
                    },
                }
            }
        }))
    } else {
        None
    };

    // Spawn stderr reader task (store handle for cleanup on early return)
    // Uses raw byte reading with `read_until` to handle invalid UTF-8 gracefully.
    let stderr_task = if let Some(stderr) = stderr_handle {
        let tx = line_tx.clone();
        Some(tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf)
                            .trim_end_matches('\n')
                            .to_string();
                        if tx.send(("stderr".to_string(), line)).await.is_err() {
                            break;
                        }
                    },
                    Err(e) => {
                        tracing::warn!("Error reading stderr stream: {}", e);
                        break;
                    },
                }
            }
        }))
    } else {
        None
    };

    // Drop the extra sender so the channel closes when both reader tasks finish.
    drop(line_tx);

    // Batching loop: accumulate lines and flush every 100ms.
    let mut interval = tokio::time::interval(Duration::from_millis(STREAM_BATCH_INTERVAL_MS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut sequence: u32 = 0;
    let mut total_bytes: usize = 0;
    let mut total_lines: usize = 0;
    let mut truncated = false;
    let mut is_first_chunk = true;
    let command_str = action.command.clone();

    // Pending buffers: separate stdout and stderr batches.
    let mut stdout_buf = String::new();
    let mut stderr_buf = String::new();

    // Full output collection for the return value.
    let mut all_stdout = String::new();
    let mut all_stderr = String::new();

    let mut readers_done = false;

    let stream_result = tokio::time::timeout_at(deadline, async {
        loop {
            tokio::select! {
                maybe_line = line_rx.recv() => {
                    match maybe_line {
                        Some((stream, line)) => {
                            // Always collect for the final return value.
                            if stream == "stdout" {
                                if !all_stdout.is_empty() {
                                    all_stdout.push('\n');
                                }
                                all_stdout.push_str(&line);
                            } else {
                                if !all_stderr.is_empty() {
                                    all_stderr.push('\n');
                                }
                                all_stderr.push_str(&line);
                            }

                            if !truncated {
                                total_lines += 1;
                                let line_bytes = line.len() + 1; // +1 for newline
                                total_bytes += line_bytes;

                                if total_bytes > STREAM_MAX_TOTAL_BYTES || total_lines > STREAM_MAX_TOTAL_LINES {
                                    truncated = true;
                                    let warning = if total_bytes > STREAM_MAX_TOTAL_BYTES {
                                        "[Output truncated at 1MB]".to_string()
                                    } else {
                                        "[Output truncated at 10,000 lines]".to_string()
                                    };
                                    // Flush whatever we have, then emit truncation warning.
                                    flush_stream_buffer(
                                        &ctx, &mut stdout_buf, "stdout", &mut sequence,
                                        &command_str, &mut is_first_chunk,
                                    );
                                    flush_stream_buffer(
                                        &ctx, &mut stderr_buf, "stderr", &mut sequence,
                                        &command_str, &mut is_first_chunk,
                                    );
                                    // Emit truncation as stderr chunk
                                    broadcast_chunk(
                                        &ctx, "stderr", &warning, &mut sequence,
                                        &command_str, &mut is_first_chunk, false, None,
                                    );
                                    continue;
                                }

                                let buf = if stream == "stdout" { &mut stdout_buf } else { &mut stderr_buf };
                                if !buf.is_empty() {
                                    buf.push('\n');
                                }
                                buf.push_str(&line);

                                // Immediate flush if chunk exceeds size cap
                                if buf.len() >= STREAM_MAX_CHUNK_BYTES {
                                    let stream_name = if stream == "stdout" { "stdout" } else { "stderr" };
                                    flush_stream_buffer(
                                        &ctx, buf, stream_name, &mut sequence,
                                        &command_str, &mut is_first_chunk,
                                    );
                                }
                            }
                        },
                        None => {
                            // Both reader tasks finished (channel closed).
                            readers_done = true;
                            break;
                        },
                    }
                },
                _ = interval.tick() => {
                    // Periodic flush of accumulated buffers.
                    if !truncated {
                        flush_stream_buffer(
                            &ctx, &mut stdout_buf, "stdout", &mut sequence,
                            &command_str, &mut is_first_chunk,
                        );
                        flush_stream_buffer(
                            &ctx, &mut stderr_buf, "stderr", &mut sequence,
                            &command_str, &mut is_first_chunk,
                        );
                    }
                },
            }
        }
        Ok(())
    })
    .await;

    // Convert the nested Result: outer Err = timeout, inner Ok = stream_result.
    let timed_out = stream_result.is_err();
    let mut stream_err: Option<ExecutionError> = match stream_result {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e),
        Err(_elapsed) => Some(ExecutionError::Step(format!(
            "Command timed out after {} seconds",
            timeout_secs
        ))),
    };

    // Abort reader tasks so they don't leak if we exited early (C2 fix).
    if let Some(h) = stdout_task {
        h.abort();
    }
    if let Some(h) = stderr_task {
        h.abort();
    }

    // Flush any remaining buffered data.
    if !truncated {
        flush_stream_buffer(
            &ctx,
            &mut stdout_buf,
            "stdout",
            &mut sequence,
            &command_str,
            &mut is_first_chunk,
        );
        flush_stream_buffer(
            &ctx,
            &mut stderr_buf,
            "stderr",
            &mut sequence,
            &command_str,
            &mut is_first_chunk,
        );
    }

    // Wait for the child process to exit (or kill on timeout).
    let exit_code = if readers_done && !timed_out {
        // EOF is not process completion (a worker may redirect its output).
        // Keep waiting within the SAME deadline, without a hidden five-second cap.
        match tokio::time::timeout_at(deadline, child.wait()).await {
            Ok(Ok(status)) => status.code(),
            Ok(Err(error)) => {
                stream_err = Some(ExecutionError::Step(format!(
                    "Failed to wait for command: {error}"
                )));
                None
            },
            Err(_) => {
                stream_err = Some(ExecutionError::Step(format!(
                    "Command timed out after {timeout_secs} seconds"
                )));
                group.terminate();
                let _ = child.kill().await;
                let _ = child.wait().await;
                None
            },
        }
    } else {
        // Timeout or error path — kill the owned group before reaping its leader.
        group.terminate();
        let _ = child.kill().await;
        match child.wait().await {
            Ok(status) => status.code(),
            Err(_) => None,
        }
    };

    group.terminate();

    // ALWAYS emit final chunk with exit code, even on error paths (C1 fix).
    broadcast_chunk(
        &ctx,
        "stdout",
        "",
        &mut sequence,
        &command_str,
        &mut is_first_chunk,
        true,
        exit_code,
    );

    // Now propagate any stream/timeout error.
    if let Some(err) = stream_err {
        return Err(err);
    }

    let code = exit_code.unwrap_or(-1);
    // What leaves this path — the failure text the model and the journal see,
    // the log line, the result — is scrubbed with the run's delivered set.
    let all_stdout = ctx.scrub_text(&all_stdout);
    let all_stderr = ctx.scrub_text(&all_stderr);

    if code != 0 {
        warn!(
            "Command failed with exit code {}: {}",
            code,
            if all_stderr.is_empty() {
                &all_stdout
            } else {
                &all_stderr
            }
        );
        return Err(ExecutionError::Step(format!(
            "Command failed with exit code {}: {}",
            code,
            if all_stderr.is_empty() {
                all_stdout.trim()
            } else {
                all_stderr.trim()
            }
        )));
    }

    info!(
        "Command completed with exit code {}, {} bytes output (streamed)",
        code,
        all_stdout.len()
    );

    Ok(ActionResult::text(all_stdout))
}

/// Flush a stream buffer as a `ShellOutputChunk` event if non-empty.
fn flush_stream_buffer(
    ctx: &ShellStreamContext,
    buf: &mut String,
    stream: &str,
    sequence: &mut u32,
    command: &str,
    is_first_chunk: &mut bool,
) {
    if buf.is_empty() {
        return;
    }
    let data = std::mem::take(buf);
    broadcast_chunk(
        ctx,
        stream,
        &data,
        sequence,
        command,
        is_first_chunk,
        false,
        None,
    );
}

/// Broadcast a single `ShellOutputChunk` event.
fn broadcast_chunk(
    ctx: &ShellStreamContext,
    stream: &str,
    data: &str,
    sequence: &mut u32,
    command: &str,
    is_first_chunk: &mut bool,
    is_final: bool,
    exit_code: Option<i32>,
) {
    let cmd_field = if *is_first_chunk {
        *is_first_chunk = false;
        command.to_string()
    } else {
        String::new()
    };

    let seq = *sequence;
    *sequence += 1;
    let data = ctx.scrub_text(data);
    let data = data.as_str();

    let event = RuntimeTransportEvent::ShellOutputChunk {
        execution_id: ctx.execution_id.clone(),
        principal: ctx.principal.clone(),
        workspace: ctx.workspace.clone(),
        step_id: ctx.step_id.clone(),
        step_index: ctx.step_index,
        command: cmd_field,
        stream: stream.to_string(),
        data: data.to_string(),
        sequence: seq,
        is_final,
        exit_code,
        timestamp: chrono::Utc::now().timestamp_millis(),
    };

    ctx.broadcaster.emit_transport_only(event);
}

const DEFAULT_SHELL_TIMEOUT_SECS: u64 = 60;

/// God mode (`execution.god_mode`): a command the shell sandbox refuses runs
/// when the owner allows it (once, for the session, or always). A malformed
/// or empty command is refused as before; a blocked or opaque invocation
/// offers only exact session/always scopes, bound to the bytes of any file it
/// runs, and piped or expanded code is allowed once only. Where god mode is off for the chat or
/// run (or the agent is not trusted), the sandbox's own refusal stands.
async fn god_mode_shell_override(
    action: &BashAction,
    sandbox: &ShellSandboxConfig,
    refusal: Option<ExecutionError>,
) -> Result<(), ExecutionError> {
    use crate::magician_v2::god_mode;
    let violation = match &refusal {
        Some(refusal) => format!("the shell sandbox refuses it: {refusal}"),
        None => "it would run outside the OS sandbox".to_string(),
    };
    if !god_mode::shell_scope::syntax_is_valid(&action.command) {
        return Err(ExecutionError::Step("Invalid shell syntax".to_string()));
    }
    let command_lower = strip_heredoc_body(&action.command).to_lowercase();
    let catastrophic = sandbox.blocked_command_fragments.iter().any(|blocked| {
        let blocked = blocked.to_lowercase();
        !blocked.is_empty() && command_lower.contains(&blocked)
    });
    let line = god_mode_shell_line(action);
    god_mode::authorize(god_mode::GodAction::Shell {
        command: &line,
        working_dir: action.working_dir.as_deref(),
        stdin: action.stdin.as_deref(),
        violation: &violation,
        catastrophic,
    })
    .await
    .map_err(|reason| ExecutionError::Step(format!("{violation} (god mode: {reason})")))
}

/// The line god mode reads for `action`: its command, preceded by the
/// action's env as assignments (`PATH='…' git status`), so a step whose env
/// changes what the command runs (`PATH`, `DYLD_*`, `GIT_SSH_COMMAND`) is
/// shown on the card and bound to an exact invocation grant. A key that is not a shell
/// name is passed through `env`, which is just as opaque.
fn god_mode_shell_line(action: &BashAction) -> String {
    if action.env.is_empty() {
        return action.command.clone();
    }
    let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
    let is_name = |key: &str| {
        let mut chars = key.chars();
        chars
            .next()
            .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
            && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    };
    let mut env: Vec<(&String, &String)> = action.env.iter().collect();
    env.sort();
    if env.iter().all(|(key, _)| is_name(key.as_str())) {
        let assignments: Vec<String> = env
            .iter()
            .map(|(key, value)| format!("{key}={}", quote(value.as_str())))
            .collect();
        format!("{} {}", assignments.join(" "), action.command)
    } else {
        let assignments: Vec<String> = env
            .iter()
            .map(|(key, value)| quote(&format!("{key}={value}")))
            .collect();
        format!("env {} {}", assignments.join(" "), action.command)
    }
}

/// God mode for file actions: each path the action touches is authorized
/// (reads and writes alike; a granted top directory covers both).
async fn god_mode_file_override(
    action: &FileAction,
    sandbox: &FileSandboxConfig,
    refusal: ExecutionError,
) -> Result<(), ExecutionError> {
    use crate::magician_v2::god_mode;
    if !god_mode::active() {
        return Err(refusal);
    }
    let violation = refusal.to_string();
    let subtree = file_action_subtree_paths(action);
    for (path, write) in file_action_touched_paths(action) {
        // Keep the read-only/hard-link warning on the explicit file approval;
        // God mode offers the same owner-chosen file scopes for this override.
        let read_only = write
            && (read_only_refusal_for(path, subtree.contains(&path), sandbox).is_some()
                || hard_link_refusal(path, sandbox).is_some());
        god_mode::authorize(god_mode::GodAction::Path {
            path,
            write,
            violation: &violation,
            read_only,
        })
        .await
        .map_err(|reason| ExecutionError::Step(format!("{violation} (god mode: {reason})")))?;
    }
    Ok(())
}

/// The paths a file action changes as a whole tree, not one entry: a
/// recursive delete, and both ends of a move (a rename can replace an
/// existing folder, and a moved folder becomes everything beneath the
/// destination, a read-only root that does not exist yet included).
fn file_action_subtree_paths(action: &FileAction) -> Vec<&Path> {
    match action {
        FileAction::Delete {
            path,
            recursive: true,
        } => vec![path.as_path()],
        FileAction::Move {
            source,
            destination,
        } => vec![source.as_path(), destination.as_path()],
        _ => Vec::new(),
    }
}

/// The paths a file action rewrites in place, so every name of the same
/// file sees the change: an append, a write or a copy onto an existing file.
fn file_action_in_place_paths(action: &FileAction) -> Vec<&Path> {
    match action {
        FileAction::Write { path, .. } | FileAction::Append { path, .. } => vec![path.as_path()],
        FileAction::Copy { destination, .. } => vec![destination.as_path()],
        _ => Vec::new(),
    }
}

/// The refusal for changing `path` in place when it is an existing regular
/// file with more than one hard link: another of its names may sit in a
/// read-only root (`ln programs/plan.md Workspaces/plan.md`), and an in-place
/// write would change that file too. Only when `sandbox` has read-only
/// roots: without them there is nothing to protect, and hard-linked files
/// (pnpm-style `node_modules`) stay editable.
pub(crate) fn hard_link_refusal(path: &Path, sandbox: &FileSandboxConfig) -> Option<String> {
    if sandbox.read_only_roots.is_empty() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(path).ok()?;
        (metadata.is_file() && metadata.nlink() > 1).then(|| {
            format!(
                "{} is hard-linked ({} names); agents cannot change hard-linked files",
                path.display(),
                metadata.nlink()
            )
        })
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// The paths a file action touches, and whether it writes each one.
fn file_action_touched_paths(action: &FileAction) -> Vec<(&Path, bool)> {
    match action {
        FileAction::Read { path, .. }
        | FileAction::Exists { path }
        | FileAction::List { path, .. } => vec![(path.as_path(), false)],
        FileAction::Write { path, .. }
        | FileAction::Append { path, .. }
        | FileAction::CreateDir { path }
        | FileAction::Delete { path, .. } => vec![(path.as_path(), true)],
        FileAction::Copy {
            source,
            destination,
        } => vec![(source.as_path(), false), (destination.as_path(), true)],
        // A move removes its source: a write there, not a read.
        FileAction::Move {
            source,
            destination,
        } => vec![(source.as_path(), true), (destination.as_path(), true)],
    }
}

/// Check for soft sandbox violations (unlisted binaries in `allowed_binaries`).
///
/// Returns `Some(violation_description)` when the command uses a binary not in
/// the allowlist. Returns `None` when the command is clean or `allowed_binaries`
/// is empty. Hard violations (blocked fragments, syntax, etc.) are not checked
/// here — use `validate_shell_action_hard` for those.
///
/// This is used by the executor to decide whether to pause and ask the user
/// (when `on_violation == Ask`) before the action reaches `execute_bash_action`,
/// which would hard-fail on the same violation.
/// Check for "soft" sandbox violations that respect `on_violation` (e.g. unlisted binaries).
/// Returns `None` when the command is clean or only has hard violations (which are enforced
/// unconditionally by `validate_shell_action_hard`).
pub fn check_shell_sandbox_violation(
    action: &BashAction,
    sandbox: &ShellSandboxConfig,
) -> Option<String> {
    match validate_shell_action_soft(action, sandbox) {
        Ok(()) => None,
        Err(e) => Some(e.to_string()),
    }
}

/// Hard violations — always blocked regardless of `on_violation`.
/// Covers: blocked_command_fragments, syntax errors, empty commands, working dir.
pub fn validate_shell_action_hard(
    action: &BashAction,
    sandbox: &ShellSandboxConfig,
) -> Result<(), ExecutionError> {
    // Isolation deny-fence: an EXPLICIT working_dir must not resolve into the live
    // magician repo source tree. Sandboxed shells / dugite would otherwise mutate
    // the user's real repo (M3 live test: a delegated `shell working_dir=<live
    // repo>` branched HEAD + git-add'd; DelegationShellProvider even pushes the
    // requested working_dir into allowed_working_dirs, so the allowlist below
    // would pass it). Only the explicit case is fenced — a None working_dir falls
    // through to validate_working_dir (fencing None here would reject every no-cwd
    // command, since the process CWD is the repo). Allows the scope sandbox +
    // external dirs.
    //
    // Always armed for SANDBOXED modes (the pre-existing baseline). For
    // Unrestricted mode it arms ONLY inside a coding context, so the DEFAULT path
    // stays equivalent — a general Unrestricted `working_dir=<repo>` shell (e.g.
    // running make/cargo/npm whose target/ writes land in-repo) is not newly
    // rejected; a coding agent's Unrestricted shell into the repo is fenced (and
    // the OS sandbox is the real boundary there anyway).
    let unrestricted = matches!(sandbox.mode, ShellSandboxMode::Unrestricted);
    if !unrestricted || crate::magician_v2::execution::coding_engine::coding_context_active() {
        if let Some(explicit) = action.working_dir.as_deref() {
            if let Some((repo_source_root, sandbox_base)) =
                crate::magician_v2::execution::coding_engine::live_repo_source_fence()
            {
                if let Ok(canonical) = explicit.canonicalize() {
                    crate::magician_v2::execution::coding_engine::reject_repo_source_tree(
                        &canonical,
                        &repo_source_root,
                        &sandbox_base,
                    )
                    .map_err(ExecutionError::Refused)?;
                }
            }
        }
    }

    if unrestricted {
        return Ok(());
    }

    validate_shell_syntax(&action.command)?;

    let command_for_validation = strip_heredoc_body(&action.command);

    // Blocked command fragments — always hard-block, no exceptions.
    let command_lower = command_for_validation.to_lowercase();
    for blocked in &sandbox.blocked_command_fragments {
        let blocked_lower = blocked.to_lowercase();
        if !blocked_lower.is_empty() && command_lower.contains(&blocked_lower) {
            return Err(ExecutionError::Refused(format!(
                "Command blocked by sandbox policy (matched '{}')",
                blocked
            )));
        }
    }

    let segments = split_pipeline_segments(&command_for_validation);
    if segments.is_empty() {
        return Err(ExecutionError::Step(
            "Shell command is empty or missing executable".to_string(),
        ));
    }

    for segment in segments {
        // Verify the segment has a recognisable binary.
        let _binary = first_command_binary(segment).ok_or_else(|| {
            ExecutionError::Step("Shell command is empty or missing executable".to_string())
        })?;
    }

    let working_dir = action
        .working_dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    validate_working_dir(&working_dir, sandbox)?;

    Ok(())
}

/// Soft violations — policy-controlled checks that respect `on_violation`.
/// Covers: unlisted binaries (allowed_binaries), network restrictions, read-only mutations.
fn validate_shell_action_soft(
    action: &BashAction,
    sandbox: &ShellSandboxConfig,
) -> Result<(), ExecutionError> {
    if matches!(sandbox.mode, ShellSandboxMode::Unrestricted) {
        return Ok(());
    }

    let command_for_validation = strip_heredoc_body(&action.command);
    let segments = split_pipeline_segments(&command_for_validation);

    for segment in segments {
        if let Some(first_binary) = first_command_binary(segment) {
            if !sandbox.allowed_binaries.is_empty()
                && !sandbox
                    .allowed_binaries
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(&first_binary))
            {
                return Err(ExecutionError::Refused(format!(
                    "Executable '{}' is not allowed by sandbox policy",
                    first_binary
                )));
            }
        }

        let segment_lower = segment.to_lowercase();
        if !sandbox.allow_network && uses_network_binary(&segment_lower) {
            return Err(ExecutionError::Refused(
                "Network commands are disabled by sandbox policy".to_string(),
            ));
        }

        if matches!(sandbox.mode, ShellSandboxMode::ReadOnly) && is_mutating_command(&segment_lower)
        {
            return Err(ExecutionError::Refused(
                "Command appears to modify state and is blocked in read-only mode".to_string(),
            ));
        }
    }

    Ok(())
}

fn validate_shell_syntax(command: &str) -> Result<(), ExecutionError> {
    // Quoted heredocs are literal data; unquoted ones still expand substitutions.
    // Keep commands following the delimiter in the validation surface.
    let commands = without_heredoc_bodies(command, true).replace("\\\n", "");
    let mut single = false;
    let mut double = false;
    let mut escaped = false;
    let mut chars = commands.chars().peekable();
    while let Some(ch) = chars.next() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && !single {
            escaped = true;
            continue;
        }
        if ch == '\'' && !double {
            single = !single;
            continue;
        }
        if single {
            continue;
        }
        if ch == '"' {
            double = !double;
            continue;
        }
        let token = match (ch, chars.peek().copied()) {
            ('`', _) => Some("`"),
            ('$', Some('(')) => Some("$("),
            ('$', Some('{')) => Some("${"),
            ('<', Some('(')) | ('>', Some('(')) if !double => {
                Some(if ch == '<' { "<(" } else { ">(" })
            },
            _ => None,
        };
        if let Some(token) = token {
            return Err(ExecutionError::Step(format!(
                "Shell command contains disallowed token '{}'",
                token
            )));
        }
    }
    Ok(())
}

/// Find actual heredoc operators, ignoring quoted/escaped text. The delimiter
/// uses shell quote removal, including mixed words like E'OF' and escaped EOF.
fn heredoc_delimiters(
    line: &str,
    single: &mut bool,
    double: &mut bool,
) -> Vec<(String, bool, bool)> {
    let mut chars = line.chars().peekable();
    let mut delimiters = Vec::new();
    let mut word_start = true;
    while let Some(ch) = chars.next() {
        if ch == '#' && !*single && !*double && word_start {
            break;
        }
        match ch {
            '\\' if !*single => {
                chars.next();
            },
            '\'' if !*double => *single = !*single,
            '"' if !*single => *double = !*double,
            '<' if !*single && !*double && chars.peek() == Some(&'<') => {
                chars.next();
                if chars.peek() == Some(&'<') {
                    chars.next(); // here-string, not a heredoc
                    continue;
                }
                let strip_tabs = chars.peek() == Some(&'-');
                if strip_tabs {
                    chars.next();
                }
                while chars.peek().is_some_and(|ch| ch.is_whitespace()) {
                    chars.next();
                }
                let mut delimiter = String::new();
                let mut quote = None;
                let mut literal = false;
                while let Some(&ch) = chars.peek() {
                    if quote.is_none() && (ch.is_whitespace() || ";|&<>".contains(ch)) {
                        break;
                    }
                    chars.next();
                    match ch {
                        '\'' | '"' if quote.is_none() => {
                            quote = Some(ch);
                            literal = true;
                        },
                        ch if quote == Some(ch) => quote = None,
                        '\\' if quote != Some('\'') => {
                            literal = true;
                            if let Some(next) = chars.next() {
                                delimiter.push(next);
                            }
                        },
                        _ => delimiter.push(ch),
                    }
                }
                if !delimiter.is_empty() {
                    delimiters.push((delimiter, strip_tabs, literal));
                }
            },
            _ => {},
        }
        word_start = ch.is_whitespace() || ";|&()<>".contains(ch);
    }
    delimiters
}

fn without_heredoc_bodies(command: &str, only_literal: bool) -> String {
    let mut lines = command.lines();
    let mut output = Vec::new();
    let mut single = false;
    let mut double = false;
    while let Some(line) = lines.next() {
        output.push(line.to_owned());
        for (delimiter, strip_tabs, literal) in heredoc_delimiters(line, &mut single, &mut double) {
            for body in lines.by_ref() {
                let comparison = if strip_tabs {
                    body.trim_start_matches('\t')
                } else {
                    body
                };
                if comparison == delimiter {
                    break;
                }
                if only_literal && !literal {
                    output.push(body.replace(['\'', '"'], "x"));
                }
            }
        }
    }
    output.join("\n")
}

/// Strip stdin data while retaining command lines before and after heredocs.
fn strip_heredoc_body(command: &str) -> String {
    without_heredoc_bodies(command, false)
}

/// Split a shell command on unquoted, unescaped `|` pipe characters.
///
/// Respects single quotes (`'...'`), double quotes (`"..."`), and backslash
/// escapes so that patterns like `grep -i "pdf\|invoice"` are not split on the
/// `|` inside the quoted string or after a backslash.
fn split_pipeline_segments(command: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;
    for (i, ch) in command.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => {
                escaped = true;
            },
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
            },
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
            },
            '|' if !in_single_quote && !in_double_quote => {
                let seg = command[start..i].trim();
                if !seg.is_empty() {
                    segments.push(seg);
                }
                start = i + 1;
            },
            _ => {},
        }
    }
    let seg = command[start..].trim();
    if !seg.is_empty() {
        segments.push(seg);
    }
    segments
}

fn first_command_binary(command: &str) -> Option<String> {
    let mut tokens = command.split_whitespace().peekable();

    while let Some(token) = tokens.peek().copied() {
        if is_env_assignment(token) {
            tokens.next();
            continue;
        }
        break;
    }

    let token = tokens.next()?;
    let stripped = token.trim_matches(|c: char| c == '"' || c == '\'' || c == ';' || c == '|');
    if stripped.is_empty() {
        return None;
    }
    let binary = stripped
        .rsplit('/')
        .next()
        .map(ToString::to_string)
        .unwrap_or_else(|| stripped.to_string());
    Some(binary)
}

fn is_env_assignment(token: &str) -> bool {
    if token.starts_with('-') || token.starts_with('/') || token.contains('/') {
        return false;
    }
    let mut split = token.splitn(2, '=');
    let key = split.next().unwrap_or_default();
    let value = split.next();
    !key.is_empty()
        && value.is_some()
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn uses_network_binary(command_lower: &str) -> bool {
    const NETWORK_HINTS: [&str; 9] = [
        "curl ", "wget ", "http ", "https ", "nc ", "ncat ", "ping ", "telnet ", "nmap ",
    ];
    NETWORK_HINTS
        .iter()
        .any(|hint| command_lower.contains(hint))
}

fn is_mutating_command(command_lower: &str) -> bool {
    const MUTATING_HINTS: [&str; 18] = [
        "rm ",
        "mv ",
        "cp ",
        "mkdir ",
        "touch ",
        "chmod ",
        "chown ",
        "truncate ",
        "tee ",
        "sed -i",
        "perl -pi",
        ">",
        ">>",
        "git commit",
        "git push",
        "pip install",
        "npm install",
        "cargo add",
    ];
    MUTATING_HINTS
        .iter()
        .any(|hint| command_lower.contains(hint))
}

fn validate_working_dir(dir: &Path, sandbox: &ShellSandboxConfig) -> Result<(), ExecutionError> {
    let canonical_dir = dir.canonicalize().map_err(|e| {
        ExecutionError::Step(format!(
            "Failed to canonicalize working directory '{}': {}",
            dir.display(),
            e
        ))
    })?;

    let mut allowed_roots = Vec::new();
    for configured in &sandbox.allowed_working_dirs {
        let candidate = PathBuf::from(configured);
        let resolved = if candidate.is_absolute() {
            candidate
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(candidate)
        };
        if let Ok(canonical_root) = resolved.canonicalize() {
            allowed_roots.push(canonical_root);
        }
    }

    if allowed_roots.is_empty() {
        return Ok(());
    }

    let allowed = allowed_roots
        .iter()
        .any(|root| canonical_dir.starts_with(root));

    if allowed {
        Ok(())
    } else {
        Err(ExecutionError::Refused(format!(
            "Working directory '{}' is outside sandbox allowed roots. Shell commands run \
             only inside the service's allowed roots; for a project's coding workspace use \
             run_project_checks (build, test, lint) or run_coding_task.",
            dir.display()
        )))
    }
}

// ============================================================================
// Security Utilities
// ============================================================================

/// Validate a path is within allowed boundaries (prevent directory traversal)
pub fn validate_path_security(path: &Path, allowed_roots: &[&Path]) -> Result<(), ExecutionError> {
    let canonical = path.canonicalize().map_err(|e| {
        ExecutionError::Step(format!(
            "Failed to canonicalize path '{}': {}",
            path.display(),
            e
        ))
    })?;

    // Check if path is under any allowed root
    for root in allowed_roots {
        if let Ok(root_canonical) = root.canonicalize() {
            if canonical.starts_with(&root_canonical) {
                return Ok(());
            }
        }
    }

    Err(ExecutionError::Step(format!(
        "Path '{}' is not within allowed directories",
        path.display()
    )))
}

// ============================================================================
// DuckDB Executor
// ============================================================================

use super::actions::DuckDbAction;
use duckdb::types::Value as DuckValue;
use std::sync::Mutex;

use crate::magician_v2::analytics::duckdb_safety::{
    configure_analytics_connection_checked, duckdb_value_ref_output_bytes,
    json_string_encoded_bytes, run_analytics_query_with_interrupt_timeout,
    try_analytics_duckdb_guard_for, AnalyticsDuckDbQueryError, ANALYTICS_DUCKDB_MAX_RESULT_BYTES,
    ANALYTICS_DUCKDB_MAX_RESULT_ROWS,
};

const DEFAULT_DUCKDB_TIMEOUT_SECS: u64 = 120;
const DUCKDB_EXTENSION_INIT_TIMEOUT_SECS: u64 = 30;

/// Session-scoped DuckDB connection holder.
///
/// Wraps a persistent in-memory DuckDB connection so tables/views created
/// in one query survive across subsequent queries within the same agent session.
pub struct DuckDbSession {
    conn: Mutex<duckdb::Connection>,
}

impl DuckDbSession {
    /// Create a new session with an in-memory database.
    ///
    /// Pre-loads the sqlite_scanner extension so tools like `imessage` can
    /// query SQLite databases without per-query setup.
    pub fn new() -> Result<Self, ExecutionError> {
        let init_timeout = Duration::from_secs(DEFAULT_DUCKDB_TIMEOUT_SECS);
        let Some(_duckdb_guard) = try_analytics_duckdb_guard_for(init_timeout) else {
            return Err(duckdb_capacity_timeout_error(init_timeout));
        };
        let conn = duckdb::Connection::open_in_memory().map_err(|e| {
            ExecutionError::Step(format!("Failed to open DuckDB in-memory connection: {}", e))
        })?;
        configure_analytics_connection_checked(&conn, "generic_duckdb_session").map_err(|e| {
            ExecutionError::Step(format!(
                "Failed to apply DuckDB session safety settings: {e}"
            ))
        })?;

        // Pre-load sqlite_scanner so sqlite_scan() is available immediately.
        // Failures are non-fatal — the extension may not be installed yet.
        match run_analytics_query_with_interrupt_timeout(
            &conn,
            Duration::from_secs(DUCKDB_EXTENSION_INIT_TIMEOUT_SECS),
            || conn.execute_batch("INSTALL sqlite_scanner; LOAD sqlite_scanner;"),
        ) {
            Ok(()) => {},
            Err(AnalyticsDuckDbQueryError::Query(error)) => {
                tracing::debug!("sqlite_scanner pre-load skipped: {}", error);
            },
            Err(AnalyticsDuckDbQueryError::TimedOut(source)) => {
                tracing::warn!(
                    source = ?source,
                    "sqlite_scanner pre-load timed out; continuing without the extension"
                );
            },
        }

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn connection(&self) -> &Mutex<duckdb::Connection> {
        &self.conn
    }
}

impl std::fmt::Debug for DuckDbSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DuckDbSession").finish()
    }
}

/// Execute a DuckDB action against a session-scoped connection.
///
/// If `action.database` is set, opens a file-backed connection for that query.
/// Otherwise uses the shared session connection for persistent in-memory state.
pub fn execute_duckdb_action(
    action: &DuckDbAction,
    session: &DuckDbSession,
) -> Result<ActionResult, ExecutionError> {
    let timeout = Duration::from_secs(action.timeout_secs.unwrap_or(DEFAULT_DUCKDB_TIMEOUT_SECS));
    execute_duckdb_action_with_timeout(action, session, timeout)
}

pub fn execute_duckdb_action_with_timeout(
    action: &DuckDbAction,
    session: &DuckDbSession,
    timeout: Duration,
) -> Result<ActionResult, ExecutionError> {
    let started_at = Instant::now();
    let Some(_duckdb_guard) = try_analytics_duckdb_guard_for(timeout) else {
        return Err(duckdb_capacity_timeout_error(timeout));
    };
    let Some(remaining) = timeout.checked_sub(started_at.elapsed()) else {
        return Err(duckdb_capacity_timeout_error(timeout));
    };
    execute_duckdb_action_inner(action, session, Some((Instant::now(), remaining)))
}

/// DuckDB reads files in process (`read_csv`, `read_text`, `glob`), past
/// the file fence. Its SQL may not name the notes hybrid index, nor, for an
/// agent a goal workspace is fenced from, a `Workspaces` folder. A string
/// check: a glob or a concatenated path that never spells either name is not
/// caught (see `docs/features/goal-workspaces.md`, Limits).
fn duckdb_workspace_refusal(sql: &str) -> Option<String> {
    let lower = sql.to_ascii_lowercase();
    if lower.contains("hybrid-index") {
        return Some("DuckDB may not read the notes search index; use search_notes".to_string());
    }
    (lower.contains("workspaces")
        && crate::magician_v2::execution::goal_workspace_access::denies_for_current()
            .guards_workspaces)
    .then(|| {
        "DuckDB may not read goal workspaces for this agent; use the files tools, which apply the workspace's members".to_string()
    })
}

/// Whether the acting agent is fenced from a goal workspace (DuckDB then
/// reads files only under its task folder).
fn duckdb_fenced() -> bool {
    crate::magician_v2::execution::goal_workspace_access::denies_for_current().guards_workspaces
}

/// Entries a DuckDB symlink scan of the readable folders visits.
const DUCKDB_SYMLINK_SCAN_LIMIT: usize = 10_000;

/// Close DuckDB's own file access to everything but this execution's task
/// folder (`allowed_directories`, then `enable_external_access = false` and
/// `lock_configuration`), so globs, `FROM '<path>'` scans and string-built
/// paths cannot reach a goal workspace. DuckDB matches those folders by
/// path, so a symlink inside one that leads into a workspace (or the notes
/// index) is refused first. With no task folder no file is readable.
fn restrict_duckdb_file_access(conn: &duckdb::Connection) -> Result<(), ExecutionError> {
    let allowed: Vec<PathBuf> = TASK_SHELL_ROOT
        .try_with(|root| resolve_for_policy(root))
        .ok()
        .into_iter()
        .collect();
    for root in &allowed {
        duckdb_symlink_refusal(root)?;
    }
    let list = allowed
        .iter()
        .map(|root| {
            format!(
                "'{}'",
                escape_sql_single_quoted_string(&format!("{}/", root.display()))
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let mut statements = String::new();
    if !list.is_empty() {
        statements.push_str(&format!("SET allowed_directories = [{list}];"));
    }
    statements.push_str("SET enable_external_access = false; SET lock_configuration = true;");
    conn.execute_batch(&statements).map_err(|error| {
        ExecutionError::Step(format!(
            "DuckDB could not be limited to the task folder ({error}); refusing the query"
        ))
    })
}

/// A symlink under `root` (bounded walk) that leads anywhere outside it: a
/// link to `/`, to the notes space or to a folder above a workspace would
/// let a string-built path reach a workspace past `allowed_directories`.
/// Whether a file has more than one name (a hard link).
fn duckdb_has_other_names(metadata: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.is_file() && metadata.nlink() > 1
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

fn duckdb_symlink_refusal(root: &Path) -> Result<(), ExecutionError> {
    let root = resolve_for_policy(root);
    let mut pending = vec![root.clone()];
    let mut seen = 0usize;
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > DUCKDB_SYMLINK_SCAN_LIMIT {
                return Err(ExecutionError::Step(
                    "DuckDB: the task folder is too large to check for links into goal workspaces"
                        .to_string(),
                ));
            }
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                // A dangling link is refused too: its target could be made
                // (anywhere) between this scan and the query.
                let inside =
                    std::fs::canonicalize(&path).is_ok_and(|target| target.starts_with(&root));
                if !inside {
                    return Err(ExecutionError::Step(format!(
                        "DuckDB: `{}` links out of the task folder; for this agent DuckDB reads only inside it",
                        path.display()
                    )));
                }
            } else if metadata.is_dir() {
                pending.push(path);
            } else if duckdb_has_other_names(&metadata) {
                // A hard link is another name for a file elsewhere, maybe a
                // goal the agent may not read.
                if let Some(refusal) =
                    crate::magician_v2::execution::goal_workspace_access::refusal_for_current(
                        &path,
                        crate::magician_v2::execution::goal_workspace_access::Op::Read,
                    )
                {
                    return Err(ExecutionError::Step(format!("DuckDB: {refusal}")));
                }
            }
        }
    }
    Ok(())
}

fn execute_duckdb_action_inner(
    action: &DuckDbAction,
    session: &DuckDbSession,
    budget: Option<(Instant, Duration)>,
) -> Result<ActionResult, ExecutionError> {
    debug!("Executing DuckDB action: {}", action.description());
    if let Some(refusal) = duckdb_workspace_refusal(&action.sql) {
        return Err(ExecutionError::Step(refusal));
    }
    let normalized_sql = normalize_duckdb_sql_paths(&action.sql);
    if normalized_sql != action.sql {
        debug!("Normalized DuckDB sqlite_scan() path literals before execution");
    }

    // If a specific database file is requested, open a separate connection.
    // Otherwise use the session's persistent in-memory connection.
    if let Some(ref db_path) = action.database {
        // Sandbox: restrict database paths to magician_data_v3/ using the same
        // resolve_path_for_policy infrastructure used by file action sandboxing.
        // This handles symlinks, path traversal (..), non-existent files, and absolute paths.
        let resolved = resolve_path_for_policy(std::path::Path::new(db_path))?;
        // Canonicalize the allowed root the same way resolve_path_for_policy does,
        // so both sides of the starts_with check use the same path representation.
        // Without this, macOS symlink layers (/var vs /private/var) cause false rejections.
        let raw_root = std::env::current_dir()
            .map_err(|e| {
                ExecutionError::Step(format!("Failed to get cwd for DuckDB sandbox check: {}", e))
            })?
            .join("magician_data_v3");
        let allowed_root = resolve_path_for_policy(&raw_root)?;
        if !resolved.starts_with(&allowed_root) {
            return Err(ExecutionError::Step(format!(
                "DuckDB database path '{}' resolves outside magician_data_v3/ — \
                 file-backed databases must be under magician_data_v3/",
                db_path
            )));
        }
        // Open using the resolved (canonicalized) path, not the raw user-provided string,
        // to ensure we open exactly the path that was validated.
        let conn = duckdb::Connection::open(&resolved).map_err(|e| {
            ExecutionError::Step(format!(
                "Failed to open DuckDB database '{}': {}",
                db_path, e
            ))
        })?;
        configure_analytics_connection_checked(&conn, "generic_duckdb_file").map_err(|e| {
            ExecutionError::Step(format!("Failed to apply DuckDB file safety settings: {e}"))
        })?;
        if duckdb_fenced() {
            restrict_duckdb_file_access(&conn)?;
        }
        execute_sql_on_connection_with_deadline(
            &conn,
            &normalized_sql,
            &action.output_format,
            budget,
        )
    } else if duckdb_fenced() {
        // An agent fenced from a goal workspace queries a fresh connection
        // that reads files only under its task folder (no session state
        // carries over between its queries).
        let conn = duckdb::Connection::open_in_memory().map_err(|e| {
            ExecutionError::Step(format!("Failed to open DuckDB in-memory connection: {}", e))
        })?;
        configure_analytics_connection_checked(&conn, "generic_duckdb_fenced").map_err(|e| {
            ExecutionError::Step(format!(
                "Failed to apply DuckDB session safety settings: {e}"
            ))
        })?;
        let _ = conn.execute_batch("LOAD sqlite_scanner;");
        restrict_duckdb_file_access(&conn)?;
        execute_sql_on_connection_with_deadline(
            &conn,
            &normalized_sql,
            &action.output_format,
            budget,
        )
    } else {
        let conn = session.connection().lock().map_err(|e| {
            ExecutionError::Step(format!("Failed to acquire DuckDB session lock: {}", e))
        })?;
        execute_sql_on_connection_with_deadline(
            &conn,
            &normalized_sql,
            &action.output_format,
            budget,
        )
    }
}

fn execute_sql_on_connection_with_deadline(
    conn: &duckdb::Connection,
    sql: &str,
    output_format: &str,
    budget: Option<(Instant, Duration)>,
) -> Result<ActionResult, ExecutionError> {
    let Some((started_at, timeout)) = budget else {
        return execute_sql_on_connection(conn, sql, output_format);
    };
    let Some(remaining) = timeout.checked_sub(started_at.elapsed()) else {
        return Err(duckdb_timeout_error(timeout, None));
    };

    match run_analytics_query_with_interrupt_timeout(conn, remaining, || {
        execute_sql_on_connection(conn, sql, output_format)
    }) {
        Ok(result) => Ok(result),
        Err(AnalyticsDuckDbQueryError::Query(error)) => Err(error),
        Err(AnalyticsDuckDbQueryError::TimedOut(source)) => {
            Err(duckdb_timeout_error(timeout, source))
        },
    }
}

fn duckdb_timeout_error(timeout: Duration, source: Option<ExecutionError>) -> ExecutionError {
    let detail = source.map(|error| format!(": {error}")).unwrap_or_default();
    ExecutionError::Step(format!(
        "DuckDB action timed out after {}s{detail}",
        timeout.as_secs()
    ))
}

fn duckdb_capacity_timeout_error(timeout: Duration) -> ExecutionError {
    ExecutionError::Step(format!(
        "DuckDB action timed out after {}s waiting for analytics capacity",
        timeout.as_secs()
    ))
}

fn normalize_duckdb_sql_paths(sql: &str) -> String {
    let home_dir = current_home_dir();
    let current_user = current_os_username(home_dir.as_deref());
    normalize_duckdb_sql_paths_with_context(sql, home_dir.as_deref(), current_user.as_deref())
}

fn normalize_duckdb_sql_paths_with_context(
    sql: &str,
    home_dir: Option<&str>,
    current_user: Option<&str>,
) -> String {
    const SQLITE_SCAN_FN: &[u8] = b"sqlite_scan";

    let sql_bytes = sql.as_bytes();
    let lower = sql.to_ascii_lowercase();
    let lower_bytes = lower.as_bytes();
    let mut normalized = String::with_capacity(sql.len());
    let mut last_copied = 0usize;
    let mut i = 0usize;

    while i + SQLITE_SCAN_FN.len() <= sql_bytes.len() {
        if !lower_bytes[i..].starts_with(SQLITE_SCAN_FN) {
            i += 1;
            continue;
        }
        if i > 0 && is_sql_identifier_byte(sql_bytes[i - 1]) {
            i += 1;
            continue;
        }

        let after_name = i + SQLITE_SCAN_FN.len();
        if after_name < sql_bytes.len() && is_sql_identifier_byte(sql_bytes[after_name]) {
            i += 1;
            continue;
        }

        let mut cursor = after_name;
        while cursor < sql_bytes.len() && sql_bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= sql_bytes.len() || sql_bytes[cursor] != b'(' {
            i = after_name;
            continue;
        }

        cursor += 1;
        while cursor < sql_bytes.len() && sql_bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= sql_bytes.len() || sql_bytes[cursor] != b'\'' {
            i = after_name;
            continue;
        }

        let Some((path_end, path_literal)) = parse_single_quoted_sql_string(sql, cursor) else {
            i = after_name;
            continue;
        };

        let expanded =
            expand_duckdb_path_literal_with_context(&path_literal, home_dir, current_user);
        if expanded != path_literal {
            normalized.push_str(&sql[last_copied..cursor + 1]);
            normalized.push_str(&escape_sql_single_quoted_string(&expanded));
            normalized.push('\'');
            last_copied = path_end + 1;
        }

        i = path_end + 1;
    }

    if last_copied == 0 {
        return sql.to_string();
    }

    normalized.push_str(&sql[last_copied..]);
    normalized
}

fn current_home_dir() -> Option<String> {
    std::env::var("HOME").ok().filter(|value| !value.is_empty())
}

fn current_os_username(home_dir: Option<&str>) -> Option<String> {
    std::env::var("USER")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            home_dir
                .and_then(|path| Path::new(path).file_name())
                .and_then(|segment| segment.to_str())
                .map(|segment| segment.to_string())
        })
}

fn expand_duckdb_path_literal_with_context(
    path: &str,
    home_dir: Option<&str>,
    current_user: Option<&str>,
) -> String {
    let mut expanded = path.to_string();

    if let Some(home_dir) = home_dir {
        expanded = expanded.replace("${HOME}", home_dir);
        expanded = replace_bare_shell_var(&expanded, "$HOME", home_dir);
        if expanded == "~" {
            expanded = home_dir.to_string();
        } else if let Some(stripped) = expanded.strip_prefix("~/") {
            expanded = format!("{}/{}", home_dir.trim_end_matches('/'), stripped);
        }
    }

    if let Some(current_user) = current_user {
        // Replace braced variants first (exact delimiters, no substring risk).
        expanded = expanded.replace("${CURRENT_USER}", current_user);
        expanded = expanded.replace("{CURRENT_USER}", current_user);
        expanded = expanded.replace("${USER}", current_user);
        // Replace bare $VAR only when followed by a non-identifier char to avoid
        // false substring matches (e.g., "$USER_cache" should NOT become "name_cache").
        expanded = replace_bare_shell_var(&expanded, "$CURRENT_USER", current_user);
        expanded = replace_bare_shell_var(&expanded, "$USER", current_user);
    }

    expanded
}

/// Replace bare `$VAR` only when it is NOT followed by an identifier character
/// (alphanumeric or underscore), preventing false substring matches like
/// `$USER_cache` → `name_cache`.
fn replace_bare_shell_var(input: &str, var: &str, replacement: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut remaining = input;
    while let Some(pos) = remaining.find(var) {
        let after = pos + var.len();
        let next_char = remaining.as_bytes().get(after).copied();
        let is_boundary = match next_char {
            None => true,
            Some(ch) => !(ch.is_ascii_alphanumeric() || ch == b'_'),
        };
        result.push_str(&remaining[..pos]);
        if is_boundary {
            result.push_str(replacement);
        } else {
            result.push_str(var);
        }
        remaining = &remaining[after..];
    }
    result.push_str(remaining);
    result
}

fn parse_single_quoted_sql_string(sql: &str, quote_start: usize) -> Option<(usize, String)> {
    let bytes = sql.as_bytes();
    if bytes.get(quote_start) != Some(&b'\'') {
        return None;
    }

    let mut value = String::new();
    let mut segment_start = quote_start + 1;
    let mut cursor = quote_start + 1;

    while cursor < bytes.len() {
        if bytes[cursor] == b'\'' {
            if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'\'' {
                value.push_str(&sql[segment_start..cursor]);
                value.push('\'');
                cursor += 2;
                segment_start = cursor;
                continue;
            }

            value.push_str(&sql[segment_start..cursor]);
            return Some((cursor, value));
        }

        cursor += 1;
    }

    None
}

fn escape_sql_single_quoted_string(value: &str) -> String {
    value.replace('\'', "''")
}

fn is_sql_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Run SQL on a connection and format the results.
///
/// Uses statement type detection to choose the correct execution path upfront,
/// avoiding the double-execution bug where `query_map` + `execute_batch` fallback
/// would run DML/DDL statements twice.
fn execute_sql_on_connection(
    conn: &duckdb::Connection,
    sql: &str,
    output_format: &str,
) -> Result<ActionResult, ExecutionError> {
    // Detect whether the SQL is a query (returns rows) or a statement (DDL/DML).
    // This avoids the double-execution bug: prepare+query_map executes the statement,
    // then on failure the fallback execute_batch would run it again.
    let stripped = strip_leading_sql_comments(sql);
    let trimmed = stripped.trim_start();

    // Check for RETURNING clause (DML that returns rows)
    let upper_full: String = trimmed.to_uppercase();
    let has_returning = upper_full.contains(" RETURNING ") || upper_full.ends_with(" RETURNING");

    let is_query = has_returning
        || (trimmed.len() >= 4 && {
            let upper: String = trimmed.chars().take(10).collect::<String>().to_uppercase();
            upper.starts_with("SELECT")
            || upper.starts_with("WITH")
            || upper.starts_with("SHOW")
            || upper.starts_with("DESCRIBE")
            || upper.starts_with("EXPLAIN")
            || upper.starts_with("PRAGMA")
            || upper.starts_with("FROM")      // DuckDB FROM-first syntax
            || upper.starts_with("VALUES")    // standalone VALUES returns rows
            || upper.starts_with("CALL")      // table-valued function calls
            || upper.starts_with("TABLE")     // DuckDB TABLE shorthand
            || upper.starts_with("PIVOT")     // DuckDB PIVOT returns result set
            || upper.starts_with("UNPIVOT")   // DuckDB UNPIVOT returns result set
            || upper.starts_with("SUMMARIZE") // DuckDB introspection query
        });

    if !is_query {
        // DDL/DML: execute without attempting to collect rows.
        conn.execute_batch(sql)
            .map_err(|e| ExecutionError::Step(format!("DuckDB execution error: {}", e)))?;
        return Ok(ActionResult::text("OK"));
    }

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| ExecutionError::Step(format!("DuckDB prepare error: {}", e)))?;

    let mut rows_iter = stmt
        .query([])
        .map_err(|e| ExecutionError::Step(format!("DuckDB query error: {}", e)))?;
    let stmt_ref = rows_iter
        .as_ref()
        .ok_or_else(|| ExecutionError::Step("DuckDB query did not expose metadata".to_string()))?;
    let column_count = stmt_ref.column_count();
    let column_names: Vec<String> = (0..column_count)
        .map(|i| {
            stmt_ref
                .column_name(i)
                .map_or("?".to_string(), |s| s.to_string())
        })
        .collect();

    let mut collected_rows: Vec<Vec<DuckValue>> = Vec::new();
    let repeated_column_bytes = column_names.iter().fold(0usize, |size, column| {
        size.saturating_add(json_string_encoded_bytes(column.as_bytes()))
            .saturating_add(4)
    });
    if repeated_column_bytes > ANALYTICS_DUCKDB_MAX_RESULT_BYTES {
        return Err(ExecutionError::Step(format!(
            "DuckDB result metadata exceeds the {} byte output limit",
            ANALYTICS_DUCKDB_MAX_RESULT_BYTES
        )));
    }
    let mut materialized_bytes = repeated_column_bytes;
    let mut truncated_by_rows = false;
    let mut truncated_by_bytes = false;
    'rows: while let Some(row) = rows_iter
        .next()
        .map_err(|e| ExecutionError::Step(format!("DuckDB row error: {}", e)))?
    {
        if collected_rows.len() >= ANALYTICS_DUCKDB_MAX_RESULT_ROWS {
            truncated_by_rows = true;
            break;
        }

        let mut row_bytes = repeated_column_bytes.saturating_add(8);
        let mut values: Vec<DuckValue> = Vec::with_capacity(column_count);
        for i in 0..column_count {
            let value_ref = row.get_ref(i).map_err(|error| {
                ExecutionError::Step(format!("DuckDB column {i} decode error: {error}"))
            })?;
            if let Some(value_bytes) = duckdb_value_ref_output_bytes(value_ref) {
                if materialized_bytes
                    .saturating_add(row_bytes)
                    .saturating_add(value_bytes)
                    > ANALYTICS_DUCKDB_MAX_RESULT_BYTES
                {
                    truncated_by_bytes = true;
                    break 'rows;
                }
            }

            let value = value_ref.to_owned();
            let value_bytes = duckvalue_output_bytes(&value);
            if materialized_bytes
                .saturating_add(row_bytes)
                .saturating_add(value_bytes)
                > ANALYTICS_DUCKDB_MAX_RESULT_BYTES
            {
                truncated_by_bytes = true;
                break 'rows;
            }
            row_bytes = row_bytes.saturating_add(value_bytes);
            values.push(value);
        }
        materialized_bytes = materialized_bytes.saturating_add(row_bytes);
        collected_rows.push(values);
    }

    let output = match output_format {
        "csv" => format_as_csv(&column_names, &collected_rows),
        "table" => format_as_table(&column_names, &collected_rows),
        _ => format_as_json(&column_names, &collected_rows),
    };
    let output = finalize_duckdb_output(
        output,
        truncated_by_rows,
        truncated_by_bytes,
        ANALYTICS_DUCKDB_MAX_RESULT_BYTES,
    );

    Ok(ActionResult::text(output))
}

fn finalize_duckdb_output(
    mut output: String,
    truncated_by_rows: bool,
    truncated_by_bytes: bool,
    max_bytes: usize,
) -> String {
    let truncated_by_bytes = truncated_by_bytes || output.len() > max_bytes;
    if truncated_by_rows || truncated_by_bytes {
        let reason = if truncated_by_bytes {
            format!("{max_bytes} bytes")
        } else {
            format!("{} rows", ANALYTICS_DUCKDB_MAX_RESULT_ROWS)
        };
        let notice =
            format!("\n\n[Output truncated at {reason}. Use LIMIT or WHERE to narrow results.]");
        truncate_utf8_to_len(&mut output, max_bytes.saturating_sub(notice.len()));
        if notice.len() <= max_bytes {
            output.push_str(&notice);
        }
    }

    debug_assert!(output.len() <= max_bytes);
    output
}

fn truncate_utf8_to_len(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    value.truncate(boundary);
}

fn duckvalue_output_bytes(value: &DuckValue) -> usize {
    match value {
        DuckValue::Text(text) => json_string_encoded_bytes(text.as_bytes()),
        DuckValue::Blob(bytes) => bytes
            .len()
            .saturating_add(2)
            .checked_div(3)
            .unwrap_or(usize::MAX)
            .saturating_mul(4)
            .saturating_add(2),
        DuckValue::List(values) | DuckValue::Array(values) => {
            values.iter().fold(2usize, |size, value| {
                size.saturating_add(duckvalue_output_bytes(value))
                    .saturating_add(1)
            })
        },
        DuckValue::Struct(values) => values.iter().fold(2usize, |size, (key, value)| {
            size.saturating_add(json_string_encoded_bytes(key.as_bytes()))
                .saturating_add(duckvalue_output_bytes(value))
                .saturating_add(2)
        }),
        DuckValue::Map(values) => values.iter().fold(2usize, |size, (key, value)| {
            size.saturating_add(duckvalue_output_bytes(key))
                .saturating_add(duckvalue_output_bytes(value))
                .saturating_add(2)
        }),
        DuckValue::Union(value) => duckvalue_output_bytes(value),
        _ => 48,
    }
}

/// Format DuckDB results as JSON (array of objects).
fn format_as_json(columns: &[String], rows: &[Vec<DuckValue>]) -> String {
    let json_rows: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            let mut obj = serde_json::Map::new();
            for (i, col) in columns.iter().enumerate() {
                let val = row
                    .get(i)
                    .map(duckvalue_to_json)
                    .unwrap_or(serde_json::Value::Null);
                obj.insert(col.clone(), val);
            }
            serde_json::Value::Object(obj)
        })
        .collect();

    serde_json::to_string_pretty(&json_rows).unwrap_or_else(|_| "[]".to_string())
}

/// Format DuckDB results as CSV (RFC 4180 compliant).
fn format_as_csv(columns: &[String], rows: &[Vec<DuckValue>]) -> String {
    let mut output = columns
        .iter()
        .map(|c| csv_escape(c))
        .collect::<Vec<_>>()
        .join(",");
    output.push('\n');
    for row in rows {
        let vals: Vec<String> = row
            .iter()
            .map(|v| csv_escape(&duckvalue_to_string(v)))
            .collect();
        output.push_str(&vals.join(","));
        output.push('\n');
    }
    output
}

/// RFC 4180: quote a field if it contains comma, double-quote, or newline.
fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Format DuckDB results as a text table.
fn format_as_table(columns: &[String], rows: &[Vec<DuckValue>]) -> String {
    // Calculate column widths using char count (safe for multi-byte UTF-8)
    let mut widths: Vec<usize> = columns.iter().map(|c| c.chars().count()).collect();
    for row in rows {
        for (i, val) in row.iter().enumerate() {
            if i < widths.len() {
                let len = duckvalue_to_string(val).chars().count();
                if len > widths[i] {
                    widths[i] = len;
                }
            }
        }
    }

    // Cap column widths at 40 chars
    for w in &mut widths {
        if *w > 40 {
            *w = 40;
        }
    }

    let mut output = String::new();

    // Header
    let header: Vec<String> = columns
        .iter()
        .zip(&widths)
        .map(|(c, w)| format!("{:width$}", c, width = *w))
        .collect();
    output.push_str(&header.join(" | "));
    output.push('\n');

    // Separator
    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    output.push_str(&sep.join("-+-"));
    output.push('\n');

    // Rows
    for row in rows {
        let vals: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(v, w)| {
                let s = duckvalue_to_string(v);
                let char_count = s.chars().count();
                if char_count > *w && *w > 3 {
                    let truncated: String = s.chars().take(*w - 3).collect();
                    format!("{truncated}...")
                } else if char_count > *w {
                    s.chars().take(*w).collect()
                } else {
                    format!("{:width$}", s, width = *w)
                }
            })
            .collect();
        output.push_str(&vals.join(" | "));
        output.push('\n');
    }

    if rows.is_empty() {
        output.push_str("(0 rows)\n");
    } else {
        output.push_str(&format!("({} rows)\n", rows.len()));
    }

    output
}

/// Convert a DuckDB Value to a serde_json Value.
fn duckvalue_to_json(val: &DuckValue) -> serde_json::Value {
    match val {
        DuckValue::Null => serde_json::Value::Null,
        DuckValue::Boolean(b) => serde_json::Value::Bool(*b),
        DuckValue::TinyInt(n) => serde_json::json!(n),
        DuckValue::SmallInt(n) => serde_json::json!(n),
        DuckValue::Int(n) => serde_json::json!(n),
        DuckValue::BigInt(n) => serde_json::json!(n),
        DuckValue::HugeInt(n) => serde_json::json!(n.to_string()),
        DuckValue::UTinyInt(n) => serde_json::json!(n),
        DuckValue::USmallInt(n) => serde_json::json!(n),
        DuckValue::UInt(n) => serde_json::json!(n),
        DuckValue::UBigInt(n) => serde_json::json!(n),
        DuckValue::Float(f) => serde_json::json!(f),
        DuckValue::Double(f) => serde_json::json!(f),
        DuckValue::Text(s) => serde_json::Value::String(s.clone()),
        DuckValue::Blob(b) => serde_json::Value::String(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            b,
        )),
        _ => serde_json::Value::String(format!("{:?}", val)),
    }
}

/// Convert a DuckDB Value to a display string.
fn duckvalue_to_string(val: &DuckValue) -> String {
    match val {
        DuckValue::Null => "NULL".to_string(),
        DuckValue::Boolean(b) => b.to_string(),
        DuckValue::TinyInt(n) => n.to_string(),
        DuckValue::SmallInt(n) => n.to_string(),
        DuckValue::Int(n) => n.to_string(),
        DuckValue::BigInt(n) => n.to_string(),
        DuckValue::HugeInt(n) => n.to_string(),
        DuckValue::UTinyInt(n) => n.to_string(),
        DuckValue::USmallInt(n) => n.to_string(),
        DuckValue::UInt(n) => n.to_string(),
        DuckValue::UBigInt(n) => n.to_string(),
        DuckValue::Float(f) => f.to_string(),
        DuckValue::Double(f) => f.to_string(),
        DuckValue::Text(s) => s.clone(),
        DuckValue::Blob(b) => format!("<blob {} bytes>", b.len()),
        _ => format!("{:?}", val),
    }
}

/// Strip leading SQL comments (both `--` line comments and `/* */` block comments)
/// so the `is_query` keyword detection sees the actual statement.
///
/// Known limitation: nested block comments (`/* outer /* inner */ still comment */`)
/// are not handled — the first `*/` terminates stripping, leaving residual comment
/// text visible to the keyword detector. DuckDB supports nested comments but LLMs
/// don't generate them in practice, so this is accepted as-is.
fn strip_leading_sql_comments(sql: &str) -> &str {
    let mut s = sql.trim_start();
    loop {
        if s.starts_with("--") {
            // Line comment: skip to end of line
            match s.find('\n') {
                Some(pos) => s = s[pos + 1..].trim_start(),
                None => return "", // entire string is a comment
            }
        } else if s.starts_with("/*") {
            // Block comment: skip to closing */
            match s.find("*/") {
                Some(pos) => s = s[pos + 2..].trim_start(),
                None => return "", // unclosed block comment
            }
        } else {
            return s;
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use runtime_core::{FileSandboxConfig, FileSandboxMode};
    use tempfile::TempDir;

    /// One folder spelled two ways — here through a symlink, as
    /// `~/MagicianNotes` links to `/Volumes/build/…` while the volume mounts as
    /// `/Volumes/SSD1` — is still the same root; a different folder never is.
    #[cfg(unix)]
    #[test]
    fn a_path_inside_a_root_spelled_another_way_is_still_inside() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("x")).unwrap();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        assert_eq!(
            relative_within(&alias.join("x").join("new.md"), &root),
            Some(PathBuf::from("x/new.md"))
        );
        assert_eq!(
            relative_within(&root.join("x"), &root),
            Some(PathBuf::from("x"))
        );
        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        assert_eq!(relative_within(&other.join("new.md"), &root), None);
    }

    // ========================================================================
    // Far-side idempotency — docs/components/magician/effect-identity.md
    // ========================================================================

    fn action_with_method(method: HttpMethod) -> HttpAction {
        HttpAction {
            method,
            ..HttpAction::get("https://example.invalid/resource")
        }
    }

    // ========================================================================
    // P4 Task 4.3 — authenticated dispatch delivers to the bound origin only
    // ========================================================================

    fn credential_post(url: &str, follow_redirects: bool, carries_credential: bool) -> HttpAction {
        HttpAction {
            method: HttpMethod::Post,
            url: url.to_string(),
            headers: HashMap::from([(
                "Authorization".to_string(),
                "Bearer p4-token-canary".to_string(),
            )]),
            body: Some(r#"{"code":"042917"}"#.to_string()),
            content_type: Some("application/json".to_string()),
            timeout_secs: Some(5),
            follow_redirects,
            carries_credential,
        }
    }

    #[tokio::test]
    async fn a_request_carrying_a_credential_never_follows_a_redirect() {
        // Loopback test server: hold the policy lock so a concurrent policy
        // test cannot make it `public` under this one.
        let _policy = crate::magician_v2::execution::network_policy::test_lock_async().await;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let origin = MockServer::start().await;
        let elsewhere = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", format!("{}/landing", elsewhere.uri()).as_str()),
            )
            .mount(&origin)
            .await;
        Mock::given(method("GET"))
            .and(path("/landing"))
            .respond_with(ResponseTemplate::new(200).set_body_string("landed"))
            .mount(&elsewhere)
            .await;

        // The action asked to follow redirects; the credential mark overrides
        // it and the cross-origin hop ends the attempt with nothing forwarded.
        let error = execute_http_action(
            &credential_post(&format!("{}/login", origin.uri()), true, true),
            None,
        )
        .await
        .expect_err("a cross-origin redirect ends an authenticated attempt");
        assert!(error.to_string().contains("destination changed"), "{error}");
        assert!(!error.to_string().contains("p4-token-canary"));
        assert!(
            elsewhere.received_requests().await.unwrap().is_empty(),
            "the other origin saw nothing"
        );
        assert_eq!(
            origin.received_requests().await.unwrap().len(),
            1,
            "delivered once to the bound origin"
        );

        // The same request without the mark follows as the action asked.
        let result = execute_http_action(
            &credential_post(&format!("{}/login", origin.uri()), true, false),
            None,
        )
        .await
        .expect("an ordinary request follows its redirect");
        let ActionResult::Http { status, body, .. } = result else {
            panic!("http result")
        };
        assert_eq!(status, 200);
        assert_eq!(body, "landed");
    }

    #[tokio::test]
    async fn a_same_origin_redirect_is_returned_not_followed_for_a_credential() {
        // Loopback test server: hold the policy lock so a concurrent policy
        // test cannot make it `public` under this one.
        let _policy = crate::magician_v2::execution::network_policy::test_lock_async().await;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let origin = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login"))
            .respond_with(ResponseTemplate::new(303).insert_header("Location", "/home"))
            .mount(&origin)
            .await;
        let result = execute_http_action(
            &credential_post(&format!("{}/login", origin.uri()), true, true),
            None,
        )
        .await
        .expect("a same-origin redirect is an ordinary observation");
        let ActionResult::Http {
            status, headers, ..
        } = result
        else {
            panic!("http result")
        };
        assert_eq!(status, 303);
        assert_eq!(headers.get("location").map(String::as_str), Some("/home"));
        assert_eq!(
            origin.received_requests().await.unwrap().len(),
            1,
            "not followed: the credential is sent once"
        );
    }

    // P4 Task 4.5 — the shell's only sink is stdin, on the streaming path too,
    // and what the process echoes is scrubbed before it streams or returns.
    #[tokio::test]
    async fn the_streaming_shell_path_feeds_stdin_and_scrubs_what_the_process_echoes() {
        let delivered = Arc::new(std::sync::Mutex::new(
            crate::magician_v2::secrets::KnownSecretValues::new(),
        ));
        delivered
            .lock()
            .unwrap()
            .insert("password".to_string(), "p4-stdin-canary".to_string());
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut events = broadcaster.subscribe();
        let ctx = ShellStreamContext {
            broadcaster,
            execution_id: "exec-stdin".into(),
            principal: None,
            workspace: None,
            base_step_id: "step".into(),
            step_id: "step".into(),
            step_index: 0,
            scrub: Some(Arc::clone(&delivered)),
        };
        let action = BashAction {
            command: "read pw; echo \"got:$pw\"; echo \"again:$pw\" >&2; exit 3".to_string(),
            working_dir: None,
            env: HashMap::new(),
            timeout_secs: Some(10),
            capture_output: true,
            stdin: Some("p4-stdin-canary\n".to_string()),
        };
        let error = execute_bash_action(&action, &ShellSandboxConfig::default(), Some(ctx), None)
            .await
            .expect_err("exit 3 fails the action");
        let text = error.to_string();
        assert!(
            text.contains("again:[REDACTED]"),
            "the process received its stdin and the echo is scrubbed: {text}"
        );
        assert!(!text.contains("p4-stdin-canary"), "{text}");
        // Every streamed chunk is scrubbed too.
        let mut streamed = String::new();
        while let Ok(event) = events.try_recv() {
            if let RuntimeTransportEvent::ShellOutputChunk { data, .. } = event {
                streamed.push_str(&data);
            }
        }
        assert!(streamed.contains("got:[REDACTED]"), "{streamed}");
        assert!(!streamed.contains("p4-stdin-canary"), "{streamed}");
    }

    #[test]
    fn a_request_that_cannot_be_built_is_a_proven_pre_dispatch_failure() {
        let mut action = credential_post("https://login.example.test/session", false, true);
        assert!(validate_http_request_buildable(&action).is_ok());
        action.headers.insert(
            "Authorization".to_string(),
            "Bearer bad\u{0}value".to_string(),
        );
        let reason = validate_http_request_buildable(&action)
            .expect_err("an invalid header value cannot be sent");
        assert!(reason.contains("cannot be built"), "{reason}");
        let mut action = credential_post("not a url", false, true);
        action.headers.clear();
        assert!(validate_http_request_buildable(&action).is_err());
    }

    #[test]
    fn only_requests_that_can_commit_carry_a_derived_key() {
        const EFFECT_ID: &str = "llm_call_abc:tool:toolu_01";

        for committing in [
            HttpMethod::Post,
            HttpMethod::Put,
            HttpMethod::Patch,
            HttpMethod::Delete,
        ] {
            assert!(
                derived_idempotency_key(&action_with_method(committing), Some(EFFECT_ID)).is_some(),
                "{committing:?} can commit a change, so a lost response is ambiguous"
            );
        }

        // A lost GET response costs nothing to re-issue, so a key buys the
        // caller nothing — and a per-attempt header on every read would be
        // noise to the remote and enough to defeat intermediary caching.
        for safe in [HttpMethod::Get, HttpMethod::Head, HttpMethod::Options] {
            assert!(
                derived_idempotency_key(&action_with_method(safe), Some(EFFECT_ID)).is_none(),
                "{safe:?} is safe to repeat and must not carry a key"
            );
        }
    }

    #[test]
    fn an_unattributable_dispatch_sends_no_key() {
        // No attempt behind the request means there is no honest key to send.
        // It does NOT mean the request is safe to repeat.
        assert!(derived_idempotency_key(&action_with_method(HttpMethod::Post), None).is_none());
    }

    #[test]
    fn a_key_that_arrived_in_the_action_does_not_outrank_the_runtime() {
        // An `http_post` step takes its headers from step parameters, and for
        // an agent tool call those parameters are the MODEL's. A model-authored
        // idempotency key is the one thing this header must never be: reuse one
        // across two different requests and a remote answers the second from
        // the first's cached response, losing an effect with no error raised.
        // Nothing at this layer can tell a model's header from a pack author's,
        // so the runtime value wins. HTTP field names are case-insensitive, so
        // no spelling escapes it.
        for spelling in ["Idempotency-Key", "idempotency-key", "IDEMPOTENCY-KEY"] {
            let mut action = action_with_method(HttpMethod::Post);
            action
                .headers
                .insert(spelling.to_string(), "supplied-in-the-action".to_string());

            let key = derived_idempotency_key(&action, Some("llm_call_abc:tool:toolu_01"))
                .expect("the runtime still derives its own key");
            assert_ne!(
                key, "supplied-in-the-action",
                "a key spelled {spelling} must not survive as the one we send"
            );
        }
    }

    #[test]
    fn an_unattributable_dispatch_leaves_the_actions_own_header_alone() {
        // With no attempt there is no runtime identity to substitute, and
        // stripping the header would break a pack that manages its own without
        // putting anything in its place. `execute_http_action` forwards
        // `action.headers` untouched whenever this returns `None`.
        let mut action = action_with_method(HttpMethod::Post);
        action
            .headers
            .insert("Idempotency-Key".to_string(), "pack-authored".to_string());

        assert!(derived_idempotency_key(&action, None).is_none());
    }

    #[test]
    fn the_key_sent_to_a_remote_is_derived_never_the_raw_effect_id() {
        let action = action_with_method(HttpMethod::Post);
        let key = derived_idempotency_key(&action, Some("llm_call_secret123:tool:toolu_01"))
            .expect("a POST with an attempt behind it carries a key");

        assert!(
            !key.contains("llm_call_secret123"),
            "the internal call id must not leave the process: {key}"
        );
        assert!(
            key.starts_with("mag-"),
            "expected a namespaced key, got {key}"
        );
    }

    #[test]
    fn standalone_duckdb_execution_respects_process_guard() {
        let session = DuckDbSession::new().expect("DuckDB session");
        let _guard = crate::magician_v2::analytics::duckdb_safety::analytics_duckdb_guard();
        let action = DuckDbAction {
            sql: "SELECT 1".to_string(),
            database: None,
            output_format: "json".to_string(),
            timeout_secs: Some(0),
        };

        let error = execute_duckdb_action(&action, &session).expect_err("capacity timeout");
        assert!(error.to_string().contains("waiting for analytics capacity"));
    }

    #[test]
    fn formatted_duckdb_output_is_bounded_after_encoding() {
        let output = finalize_duckdb_output("é".repeat(100), false, false, 128);

        assert!(output.len() <= 128);
        assert!(output.contains("Output truncated"));
        assert!(std::str::from_utf8(output.as_bytes()).is_ok());
    }

    #[test]
    fn test_normalize_duckdb_sql_paths_expands_sqlite_scan_path_literals() {
        let sql = "SELECT * FROM sqlite_scan('~/Library/Messages/chat.db', 'message')";
        let normalized =
            normalize_duckdb_sql_paths_with_context(sql, Some("/Users/owner"), Some("owner"));

        assert_eq!(
            normalized,
            "SELECT * FROM sqlite_scan('/Users/owner/Library/Messages/chat.db', 'message')"
        );
    }

    #[test]
    fn test_normalize_duckdb_sql_paths_expands_home_and_user_placeholders() {
        let sql = "SELECT CURRENT_USER, * FROM sqlite_scan('$HOME/Library/${CURRENT_USER}/chat.db', 'message')";
        let normalized =
            normalize_duckdb_sql_paths_with_context(sql, Some("/Users/owner"), Some("owner"));

        assert_eq!(
            normalized,
            "SELECT CURRENT_USER, * FROM sqlite_scan('/Users/owner/Library/owner/chat.db', 'message')"
        );
    }

    #[test]
    fn test_normalize_duckdb_sql_paths_leaves_non_sqlite_scan_sql_unchanged() {
        let sql = "SELECT CURRENT_USER, '~/Library/Messages/chat.db' AS path";
        let normalized =
            normalize_duckdb_sql_paths_with_context(sql, Some("/Users/owner"), Some("owner"));

        assert_eq!(normalized, sql);
    }

    #[test]
    fn test_glob_pattern_matching() {
        assert!(matches_glob_pattern("test.txt", "*.txt"));
        assert!(matches_glob_pattern("test.txt", "test.*"));
        assert!(matches_glob_pattern("test.txt", "t?st.txt"));
        assert!(matches_glob_pattern("test.txt", "*"));
        assert!(!matches_glob_pattern("test.txt", "*.rs"));
        assert!(!matches_glob_pattern("test.txt", "foo*"));
    }

    #[tokio::test]
    async fn test_file_write_and_read() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let sandbox = FileSandboxConfig {
            mode: FileSandboxMode::WorkspaceWrite,
            allowed_roots: vec![temp_dir.path().display().to_string()],
            read_only_roots: Vec::new(),
            extra_writable_roots: Vec::new(),
            allow_delete: true,
        };

        // Write
        let write_action = FileAction::Write {
            path: file_path.clone(),
            content: "Hello, World!".to_string(),
            create_dirs: true,
        };
        let result = execute_file_action(&write_action, &sandbox).await.unwrap();
        assert!(result.is_success());

        // Read
        let read_action = FileAction::Read {
            path: file_path,
            encoding: None,
        };
        let result = execute_file_action(&read_action, &sandbox).await.unwrap();
        assert_eq!(result.as_text(), Some("Hello, World!"));
    }

    #[test]
    fn file_action_reports_only_paths_outside_allowed_roots() {
        let allowed = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let sandbox = FileSandboxConfig {
            mode: FileSandboxMode::WorkspaceWrite,
            allowed_roots: vec![allowed.path().display().to_string()],
            read_only_roots: Vec::new(),
            extra_writable_roots: Vec::new(),
            allow_delete: true,
        };
        let action = FileAction::Copy {
            source: allowed.path().join("source.txt"),
            destination: outside.path().join("destination.txt"),
        };

        let violations = file_action_outside_allowed_roots(&action, &sandbox).unwrap();

        assert_eq!(
            violations,
            vec![
                resolve_path_for_policy(&outside.path().join("destination.txt"))
                    .unwrap()
                    .display()
                    .to_string()
            ]
        );
    }

    /// Read-only, delete and move policy denials refuse what the agent asked
    /// for: `Refused` (a warning, not retried), while a path outside the
    /// roots stays the recoverable `PathAccessDenied` that raises the
    /// sandbox-override HITL.
    #[test]
    fn file_policy_denials_are_refusals_and_out_of_roots_stays_path_access_denied() {
        let allowed = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let inside = allowed.path().join("notes.txt");
        let config = |mode, allow_delete| FileSandboxConfig {
            mode,
            allowed_roots: vec![allowed.path().display().to_string()],
            read_only_roots: Vec::new(),
            extra_writable_roots: Vec::new(),
            allow_delete,
        };
        let write = FileAction::Write {
            path: inside.clone(),
            content: String::new(),
            create_dirs: true,
        };
        let delete = FileAction::Delete {
            path: inside.clone(),
            recursive: false,
        };
        let moved = FileAction::Move {
            source: inside.clone(),
            destination: allowed.path().join("moved.txt"),
        };
        for (action, sandbox) in [
            (&write, config(FileSandboxMode::ReadOnly, true)),
            (&delete, config(FileSandboxMode::WorkspaceWrite, false)),
            (&moved, config(FileSandboxMode::WorkspaceWrite, false)),
        ] {
            let error = validate_file_action(action, &sandbox).unwrap_err();
            assert!(matches!(error, ExecutionError::Refused(_)), "{error:?}");
        }
        let escape = FileAction::Write {
            path: outside.path().join("notes.txt"),
            content: String::new(),
            create_dirs: true,
        };
        let error = validate_file_action(&escape, &config(FileSandboxMode::WorkspaceWrite, true))
            .unwrap_err();
        assert!(
            matches!(error, ExecutionError::PathAccessDenied { .. }),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn runtime_store_fence_denies_agent_writes_to_trusted_stores() {
        // The trusted runtime stores (transactions / code_change_proposals /
        // runtime/pause_states) sit under the agent-writable `scopes` tree; the
        // deny-fence must keep agent file writes out of them so a staged
        // transaction's trusted `apply_root` cannot be forged. Covers the nested
        // `runtime/pause_states` depth that a flat 3rd-component check missed.
        let base = TempDir::new().unwrap();
        let scopes_root = base.path().join("scopes");
        let scope = scopes_root.join("anonymous").join("default");
        for sub in [
            "transactions",
            "code_change_proposals",
            "runtime/pause_states",
            "workdirs/home",
        ] {
            std::fs::create_dir_all(scope.join(sub)).unwrap();
        }
        let sandbox = FileSandboxConfig {
            mode: FileSandboxMode::WorkspaceWrite,
            allowed_roots: vec![scopes_root.display().to_string()],
            read_only_roots: Vec::new(),
            extra_writable_roots: Vec::new(),
            allow_delete: true,
        };
        let write = |path: PathBuf| FileAction::Write {
            path,
            content: "x".to_string(),
            create_dirs: true,
        };

        for denied in [
            "transactions/t.json",
            "code_change_proposals/c.json",
            "runtime/pause_states/p.json",
        ] {
            let action = write(scope.join(denied));
            assert!(
                execute_file_action(&action, &sandbox).await.is_err(),
                "runtime-store fence must deny agent write to `{denied}`"
            );
        }

        // A legitimate in-workspace write under the same scope stays allowed.
        let allowed = write(scope.join("workdirs/home/file.txt"));
        assert!(
            execute_file_action(&allowed, &sandbox).await.is_ok(),
            "in-workspace write must not be fenced"
        );
    }

    #[tokio::test]
    async fn test_file_exists() {
        let temp_dir = TempDir::new().unwrap();
        let existing = temp_dir.path().join("exists.txt");
        let non_existing = temp_dir.path().join("not_exists.txt");
        let sandbox = FileSandboxConfig {
            mode: FileSandboxMode::WorkspaceWrite,
            allowed_roots: vec![temp_dir.path().display().to_string()],
            read_only_roots: Vec::new(),
            extra_writable_roots: Vec::new(),
            allow_delete: true,
        };

        // Create file
        std::fs::write(&existing, "test").unwrap();

        // Check existing
        let action = FileAction::Exists { path: existing };
        let result = execute_file_action(&action, &sandbox).await.unwrap();
        if let ActionResult::Bool { value } = result {
            assert!(value);
        } else {
            panic!("Expected Bool result");
        }

        // Check non-existing
        let action = FileAction::Exists { path: non_existing };
        let result = execute_file_action(&action, &sandbox).await.unwrap();
        if let ActionResult::Bool { value } = result {
            assert!(!value);
        } else {
            panic!("Expected Bool result");
        }
    }

    fn read_only_sandbox(writable: &Path, read_only: &Path) -> FileSandboxConfig {
        FileSandboxConfig {
            mode: FileSandboxMode::WorkspaceWrite,
            allowed_roots: vec![writable.display().to_string()],
            read_only_roots: vec![read_only.display().to_string()],
            extra_writable_roots: Vec::new(),
            allow_delete: true,
        }
    }

    fn assert_read_only_refusal(result: Result<ActionResult, ExecutionError>, what: &str) {
        match result {
            Err(ExecutionError::Step(message)) => assert!(
                message.contains("is read-only for agents (read_only_roots)"),
                "{what}: {message}"
            ),
            other => panic!("{what}: expected the read-only refusal, got {other:?}"),
        }
    }

    /// Program docs and notes: agents read them, and nothing they do through
    /// the file sandbox changes them.
    #[tokio::test]
    async fn read_only_roots_allow_reads_and_refuse_every_mutation() {
        let writable = TempDir::new().unwrap();
        let read_only = TempDir::new().unwrap();
        let doc = read_only.path().join("program.md");
        std::fs::write(&doc, "# Program").unwrap();
        let sandbox = read_only_sandbox(writable.path(), read_only.path());

        let read = execute_file_action(
            &FileAction::Read {
                path: doc.clone(),
                encoding: None,
            },
            &sandbox,
        )
        .await
        .expect("a read inside read_only_roots passes");
        assert_eq!(read.as_text(), Some("# Program"));
        for action in [
            FileAction::Exists { path: doc.clone() },
            FileAction::List {
                path: read_only.path().to_path_buf(),
                pattern: None,
            },
        ] {
            assert!(
                execute_file_action(&action, &sandbox).await.is_ok(),
                "{action:?} inside read_only_roots passes"
            );
        }

        let mutations = [
            FileAction::Write {
                path: doc.clone(),
                content: "rewritten".to_string(),
                create_dirs: true,
            },
            FileAction::Write {
                path: read_only.path().join("new.md"),
                content: "new".to_string(),
                create_dirs: true,
            },
            FileAction::Append {
                path: doc.clone(),
                content: "more".to_string(),
            },
            FileAction::Delete {
                path: doc.clone(),
                recursive: false,
            },
            FileAction::CreateDir {
                path: read_only.path().join("sub"),
            },
            // A move removes its source, so moving out of a read-only root
            // is a mutation there too.
            FileAction::Move {
                source: doc.clone(),
                destination: writable.path().join("moved.md"),
            },
            FileAction::Move {
                source: {
                    let source = writable.path().join("incoming.md");
                    std::fs::write(&source, "incoming").unwrap();
                    source
                },
                destination: read_only.path().join("incoming.md"),
            },
        ];
        for action in mutations {
            assert_read_only_refusal(
                execute_file_action(&action, &sandbox).await,
                &format!("{action:?}"),
            );
        }
        assert_eq!(std::fs::read_to_string(&doc).unwrap(), "# Program");
        assert!(!read_only.path().join("new.md").exists());
        assert!(!read_only.path().join("sub").exists());
        assert!(!writable.path().join("moved.md").exists());
        assert!(writable.path().join("incoming.md").exists());
    }

    /// A copy reads its source and writes its destination: out of a
    /// read-only root is fine, into one is refused.
    #[tokio::test]
    async fn read_only_roots_admit_a_copy_out_but_not_a_copy_in() {
        let writable = TempDir::new().unwrap();
        let read_only = TempDir::new().unwrap();
        let doc = read_only.path().join("program.md");
        std::fs::write(&doc, "# Program").unwrap();
        let draft = writable.path().join("draft.md");
        std::fs::write(&draft, "draft").unwrap();
        let sandbox = read_only_sandbox(writable.path(), read_only.path());

        let copied_out = writable.path().join("program-copy.md");
        execute_file_action(
            &FileAction::Copy {
                source: doc.clone(),
                destination: copied_out.clone(),
            },
            &sandbox,
        )
        .await
        .expect("a copy from a read-only root into a writable one passes");
        assert_eq!(std::fs::read_to_string(&copied_out).unwrap(), "# Program");

        assert_read_only_refusal(
            execute_file_action(
                &FileAction::Copy {
                    source: draft,
                    destination: read_only.path().join("draft.md"),
                },
                &sandbox,
            )
            .await,
            "copy into a read-only root",
        );
        assert!(!read_only.path().join("draft.md").exists());
    }

    /// The deepest root decides: a read-only root inside a writable one stays
    /// read-only (a scope's `programs/` under the scopes tree), and a writable
    /// root inside a read-only one stays writable (a notes space's
    /// `Workspaces/`).
    #[tokio::test]
    async fn the_deepest_root_decides_when_writable_and_read_only_roots_nest() {
        let base = TempDir::new().unwrap();
        let scopes = base.path().join("scopes");
        let programs = scopes.join("owner").join("default").join("programs");
        let notes = base.path().join("notes");
        let workspaces = notes.join("Workspaces");
        for dir in [&programs, &workspaces] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let sandbox = FileSandboxConfig {
            mode: FileSandboxMode::WorkspaceWrite,
            allowed_roots: vec![
                scopes.display().to_string(),
                workspaces.display().to_string(),
            ],
            read_only_roots: vec![programs.display().to_string(), notes.display().to_string()],
            extra_writable_roots: Vec::new(),
            allow_delete: true,
        };
        let write = |path: PathBuf| FileAction::Write {
            path,
            content: "x".to_string(),
            create_dirs: true,
        };

        assert_read_only_refusal(
            execute_file_action(&write(programs.join("plan.md")), &sandbox).await,
            "programs/ inside the writable scopes tree",
        );
        assert_read_only_refusal(
            execute_file_action(&write(notes.join("note.md")), &sandbox).await,
            "a note outside Workspaces/",
        );
        for writable in [
            scopes.join("owner").join("default").join("other.md"),
            workspaces.join("goal").join("status.md"),
        ] {
            assert!(
                execute_file_action(&write(writable.clone()), &sandbox)
                    .await
                    .is_ok(),
                "{} stays writable",
                writable.display()
            );
        }
        std::fs::write(notes.join("note.md"), "note").unwrap();
        assert!(execute_file_action(
            &FileAction::Read {
                path: notes.join("note.md"),
                encoding: None,
            },
            &sandbox,
        )
        .await
        .is_ok());
    }

    /// A path outside every root stays the recoverable `PathAccessDenied`,
    /// and its message names the roots that would admit it.
    #[tokio::test]
    async fn an_out_of_roots_refusal_names_the_writable_and_read_only_roots() {
        let writable = TempDir::new().unwrap();
        let read_only = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let sandbox = read_only_sandbox(writable.path(), read_only.path());
        let target = outside.path().join("secret.md");

        let error = execute_file_action(
            &FileAction::Read {
                path: target.clone(),
                encoding: None,
            },
            &sandbox,
        )
        .await
        .unwrap_err();
        let resolved = |path: &Path| resolve_path_for_policy(path).unwrap().display().to_string();
        assert!(matches!(error, ExecutionError::PathAccessDenied { .. }));
        assert_eq!(
            error.to_string(),
            format!(
                "path outside allowed file sandbox roots: {} (writable: {}; read-only: {})",
                resolved(target.as_path()),
                resolved(writable.path()),
                resolved(read_only.path())
            )
        );

        let plain = FileSandboxConfig {
            read_only_roots: Vec::new(),
            ..sandbox
        };
        let error = execute_file_action(
            &FileAction::Read {
                path: target,
                encoding: None,
            },
            &plain,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().ends_with("; read-only: none)"), "{error}");
    }

    /// The HITL preflight asks only about paths no root admits: a read in a
    /// read-only root passes, and a write there is a hard refusal no folder
    /// approval would change.
    #[test]
    fn outside_roots_report_skips_paths_a_read_only_root_governs() {
        let writable = TempDir::new().unwrap();
        let read_only = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let sandbox = read_only_sandbox(writable.path(), read_only.path());

        for action in [
            FileAction::Read {
                path: read_only.path().join("program.md"),
                encoding: None,
            },
            FileAction::Write {
                path: read_only.path().join("program.md"),
                content: String::new(),
                create_dirs: true,
            },
            FileAction::Copy {
                source: read_only.path().join("program.md"),
                destination: writable.path().join("copy.md"),
            },
        ] {
            assert!(
                file_action_outside_allowed_roots(&action, &sandbox)
                    .unwrap()
                    .is_empty(),
                "{action:?}"
            );
        }
        let action = FileAction::Copy {
            source: read_only.path().join("program.md"),
            destination: outside.path().join("copy.md"),
        };
        assert_eq!(
            file_action_outside_allowed_roots(&action, &sandbox).unwrap(),
            vec![resolve_path_for_policy(&outside.path().join("copy.md"))
                .unwrap()
                .display()
                .to_string()]
        );
    }

    #[test]
    fn read_only_root_refusal_names_paths_for_staged_writers() {
        let writable = TempDir::new().unwrap();
        let read_only = TempDir::new().unwrap();
        let sandbox = read_only_sandbox(writable.path(), read_only.path());

        let refusal = read_only_root_refusal(&read_only.path().join("program.md"), &sandbox)
            .expect("a write under a read-only root is refused");
        assert!(
            refusal.contains("program.md is read-only for agents (read_only_roots) (writable: "),
            "{refusal}"
        );
        assert!(read_only_root_refusal(&writable.path().join("draft.md"), &sandbox).is_none());
        let unrestricted = FileSandboxConfig {
            mode: FileSandboxMode::Unrestricted,
            ..sandbox
        };
        assert!(
            read_only_root_refusal(&read_only.path().join("program.md"), &unrestricted).is_none()
        );
    }

    /// A recursive delete or a move takes everything beneath with it: a
    /// writable scope folder that holds a read-only `programs/` can be
    /// neither deleted nor moved away.
    #[tokio::test]
    async fn subtree_operations_above_a_read_only_root_are_refused() {
        let base = TempDir::new().unwrap();
        let scopes = base.path().join("scopes");
        let scope = scopes.join("owner").join("default");
        let programs = scope.join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        std::fs::write(programs.join("plan.md"), "# Plan").unwrap();
        let elsewhere = base.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let sandbox = FileSandboxConfig {
            mode: FileSandboxMode::WorkspaceWrite,
            allowed_roots: vec![
                scopes.display().to_string(),
                elsewhere.display().to_string(),
            ],
            read_only_roots: vec![programs.display().to_string()],
            extra_writable_roots: Vec::new(),
            allow_delete: true,
        };

        for action in [
            FileAction::Delete {
                path: scope.clone(),
                recursive: true,
            },
            FileAction::Move {
                source: scope.clone(),
                destination: elsewhere.join("moved"),
            },
        ] {
            let error = execute_file_action(&action, &sandbox).await.unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("which is read-only for agents (read_only_roots)"),
                "{action:?}: {error}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(programs.join("plan.md")).unwrap(),
            "# Plan"
        );

        // A move onto an existing folder that holds a read-only root is refused
        // too: the rename would replace it.
        let empty_programs_parent = elsewhere.join("held");
        std::fs::create_dir_all(empty_programs_parent.join("programs")).unwrap();
        let held = FileSandboxConfig {
            read_only_roots: vec![empty_programs_parent.join("programs").display().to_string()],
            ..sandbox.clone()
        };
        let source = elsewhere.join("incoming");
        std::fs::create_dir_all(&source).unwrap();
        let error = execute_file_action(
            &FileAction::Move {
                source,
                destination: empty_programs_parent.clone(),
            },
            &held,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("which is read-only"), "{error}");

        // A recursive delete of a folder with no read-only root inside still works.
        let scratch = scope.join("scratch");
        std::fs::create_dir_all(scratch.join("deep")).unwrap();
        execute_file_action(
            &FileAction::Delete {
                path: scratch.clone(),
                recursive: true,
            },
            &sandbox,
        )
        .await
        .expect("a subtree with no read-only root inside may go");
        assert!(!scratch.exists());
    }

    /// The runtime-store fence covers subtree operations too: deleting or
    /// moving a scope takes its transaction and pause records with it.
    #[tokio::test]
    async fn runtime_store_fence_refuses_subtree_operations_above_a_store() {
        let base = TempDir::new().unwrap();
        let scopes = base.path().join("scopes");
        let scope = scopes.join("owner").join("default");
        std::fs::create_dir_all(scope.join("runtime").join("pause_states")).unwrap();
        std::fs::create_dir_all(scope.join("workdirs").join("home")).unwrap();
        let sandbox = FileSandboxConfig {
            mode: FileSandboxMode::WorkspaceWrite,
            allowed_roots: vec![scopes.display().to_string()],
            read_only_roots: Vec::new(),
            extra_writable_roots: Vec::new(),
            allow_delete: true,
        };
        for path in [scope.clone(), scope.join("runtime"), scopes.join("owner")] {
            let error = execute_file_action(
                &FileAction::Delete {
                    path: path.clone(),
                    recursive: true,
                },
                &sandbox,
            )
            .await
            .unwrap_err();
            assert!(
                error.to_string().contains("runtime-internal store"),
                "{}: {error}",
                path.display()
            );
        }
        assert!(scope.join("runtime").join("pause_states").exists());
        execute_file_action(
            &FileAction::Delete {
                path: scope.join("workdirs").join("home"),
                recursive: true,
            },
            &sandbox,
        )
        .await
        .expect("a folder that holds no store may go");
    }

    /// A dangling symlink resolves to where a write through it would land:
    /// a link in a writable folder cannot create a file in a read-only one,
    /// and a link inside a read-only folder cannot be deleted from there.
    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_cannot_carry_a_write_into_or_out_of_a_read_only_root() {
        let writable = TempDir::new().unwrap();
        let read_only = TempDir::new().unwrap();
        let sandbox = read_only_sandbox(writable.path(), read_only.path());

        let dangling = writable.path().join("link.md");
        std::os::unix::fs::symlink(read_only.path().join("new.md"), &dangling).unwrap();
        assert_read_only_refusal(
            execute_file_action(
                &FileAction::Write {
                    path: dangling.clone(),
                    content: "planted".to_string(),
                    create_dirs: true,
                },
                &sandbox,
            )
            .await,
            "write through a dangling link into a read-only root",
        );
        assert!(!read_only.path().join("new.md").exists());

        // A dangling link to a missing folder, then a file beneath it.
        let dir_link = writable.path().join("dir-link");
        std::os::unix::fs::symlink(read_only.path().join("missing"), &dir_link).unwrap();
        assert_read_only_refusal(
            execute_file_action(
                &FileAction::Write {
                    path: dir_link.join("deep.md"),
                    content: "planted".to_string(),
                    create_dirs: true,
                },
                &sandbox,
            )
            .await,
            "write beneath a dangling folder link",
        );
        assert!(!read_only.path().join("missing").exists());

        let target = writable.path().join("target.md");
        std::fs::write(&target, "target").unwrap();
        let inside = read_only.path().join("pointer.md");
        std::os::unix::fs::symlink(&target, &inside).unwrap();
        assert_read_only_refusal(
            execute_file_action(
                &FileAction::Delete {
                    path: inside.clone(),
                    recursive: false,
                },
                &sandbox,
            )
            .await,
            "delete a link that sits in a read-only root",
        );
        assert!(inside.symlink_metadata().is_ok());

        // A link to a writable file, from a writable folder, still works.
        let fine = writable.path().join("fine.md");
        std::os::unix::fs::symlink(&target, &fine).unwrap();
        execute_file_action(
            &FileAction::Write {
                path: fine,
                content: "updated".to_string(),
                create_dirs: true,
            },
            &sandbox,
        )
        .await
        .expect("a link inside writable roots stays writable");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "updated");
    }

    /// Run-time roots (owner grants, the task workdir, delegation folders)
    /// never outrank a read-only root, however deep they sit; outside every
    /// read-only root they admit writes as before.
    #[tokio::test]
    async fn a_grant_under_a_read_only_root_still_cannot_write() {
        let writable = TempDir::new().unwrap();
        let read_only = TempDir::new().unwrap();
        let granted_elsewhere = TempDir::new().unwrap();
        let nested = read_only.path().join("archive");
        std::fs::create_dir_all(&nested).unwrap();
        let mut sandbox = read_only_sandbox(writable.path(), read_only.path());
        sandbox.extra_writable_roots = vec![
            nested.display().to_string(),
            read_only.path().display().to_string(),
            granted_elsewhere.path().display().to_string(),
        ];

        for path in [nested.join("plan.md"), read_only.path().join("plan.md")] {
            assert_read_only_refusal(
                execute_file_action(
                    &FileAction::Write {
                        path: path.clone(),
                        content: "x".to_string(),
                        create_dirs: true,
                    },
                    &sandbox,
                )
                .await,
                &path.display().to_string(),
            );
            assert!(read_only_root_refusal(&path, &sandbox).is_some());
        }
        execute_file_action(
            &FileAction::Write {
                path: granted_elsewhere.path().join("ok.md"),
                content: "x".to_string(),
                create_dirs: true,
            },
            &sandbox,
        )
        .await
        .expect("a grant outside read-only roots admits writes");
        assert!(file_action_outside_allowed_roots(
            &FileAction::Read {
                path: granted_elsewhere.path().join("ok.md"),
                encoding: None,
            },
            &sandbox,
        )
        .unwrap()
        .is_empty());
    }

    /// A link the shell planted in the task workdir, dangling to a file
    /// elsewhere, is not "inside the workdir": the direct write path refuses
    /// it, and a link planted after that check still cannot carry a new file
    /// out (`create_new` does not follow it).
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dangling_workdir_symlink_cannot_create_a_file_elsewhere() {
        let root = TempDir::new().unwrap();
        let elsewhere = TempDir::new().unwrap();
        let workdir = root.path().join("workdir");
        std::fs::create_dir_all(&workdir).unwrap();
        let planted = workdir.join("x.md");
        let target = elsewhere.path().join("new.md");
        std::os::unix::fs::symlink(&target, &planted).unwrap();

        TASK_SHELL_ROOT
            .scope(root.path().to_path_buf(), async {
                assert_eq!(path_in_task_workdir(&planted.to_string_lossy()), None);
            })
            .await;
        let written =
            crate::magician_v2::execution::compiled_handlers::staged_file_edit::write_task_workdir_file(
                &planted, "x.md", "planted", true,
            );
        assert_eq!(written["status"], "error", "{written}");
        assert!(!target.exists());
    }

    /// A move onto a folder that does not exist yet is refused when a
    /// read-only root, which does not exist yet either, would sit beneath it.
    #[tokio::test]
    async fn a_move_onto_a_future_parent_of_a_read_only_root_is_refused() {
        let base = TempDir::new().unwrap();
        let source = base.path().join("staged");
        std::fs::create_dir_all(source.join("programs")).unwrap();
        std::fs::write(source.join("programs").join("plan.md"), "forged").unwrap();
        let destination = base.path().join("scope");
        let sandbox = FileSandboxConfig {
            mode: FileSandboxMode::WorkspaceWrite,
            allowed_roots: vec![base.path().display().to_string()],
            read_only_roots: vec![destination.join("programs").display().to_string()],
            extra_writable_roots: Vec::new(),
            allow_delete: true,
        };
        let error = execute_file_action(
            &FileAction::Move {
                source,
                destination: destination.clone(),
            },
            &sandbox,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("which is read-only"), "{error}");
        assert!(!destination.exists());
    }

    /// Run-time roots alone (a task workdir, a delegation folder) still admit
    /// file actions when no root is configured.
    #[tokio::test]
    async fn run_time_roots_alone_still_admit_file_actions() {
        let workdir = TempDir::new().unwrap();
        let sandbox = FileSandboxConfig {
            mode: FileSandboxMode::WorkspaceWrite,
            allowed_roots: Vec::new(),
            read_only_roots: Vec::new(),
            extra_writable_roots: vec![workdir.path().display().to_string()],
            allow_delete: true,
        };
        execute_file_action(
            &FileAction::Write {
                path: workdir.path().join("out.csv"),
                content: "rows".to_string(),
                create_dirs: true,
            },
            &sandbox,
        )
        .await
        .expect("a run-time root alone admits writes");
        let none = FileSandboxConfig {
            extra_writable_roots: Vec::new(),
            ..sandbox
        };
        let error = execute_file_action(
            &FileAction::Read {
                path: workdir.path().join("out.csv"),
                encoding: None,
            },
            &none,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("no allowed roots"), "{error}");
    }

    /// A hard link gives a read-only file a second, writable name; an
    /// in-place change through it (write, append, copy onto) is refused.
    #[cfg(unix)]
    #[tokio::test]
    async fn hard_linked_files_cannot_be_changed_in_place() {
        let writable = TempDir::new().unwrap();
        let read_only = TempDir::new().unwrap();
        let program = read_only.path().join("plan.md");
        std::fs::write(&program, "# Plan").unwrap();
        let alias = writable.path().join("plan.md");
        std::fs::hard_link(&program, &alias).unwrap();
        let other = writable.path().join("other.md");
        std::fs::write(&other, "other").unwrap();
        let sandbox = read_only_sandbox(writable.path(), read_only.path());

        for action in [
            FileAction::Write {
                path: alias.clone(),
                content: "rewritten".to_string(),
                create_dirs: true,
            },
            FileAction::Append {
                path: alias.clone(),
                content: "more".to_string(),
            },
            FileAction::Copy {
                source: other.clone(),
                destination: alias.clone(),
            },
        ] {
            let error = execute_file_action(&action, &sandbox).await.unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("agents cannot change hard-linked files"),
                "{action:?}: {error}"
            );
        }
        assert_eq!(std::fs::read_to_string(&program).unwrap(), "# Plan");
        assert!(hard_link_refusal(&other, &sandbox).is_none());

        // Without read-only roots there is nothing to protect: hard-linked
        // files (pnpm-style `node_modules`) stay editable.
        let plain = FileSandboxConfig {
            read_only_roots: Vec::new(),
            ..sandbox
        };
        assert!(hard_link_refusal(&alias, &plain).is_none());
        execute_file_action(
            &FileAction::Append {
                path: alias.clone(),
                content: " appended".to_string(),
            },
            &plain,
        )
        .await
        .expect("no read-only roots: a hard-linked file may change");
        assert_eq!(
            std::fs::read_to_string(&program).unwrap(),
            "# Plan appended"
        );
    }

    /// The OS sandboxes get every read-only root they can: none in
    /// unrestricted mode, and one root that cannot be resolved (a symlink
    /// loop) is kept as configured without dropping the others.
    #[cfg(unix)]
    #[test]
    fn agent_read_only_roots_resolve_each_root_on_its_own() {
        let base = TempDir::new().unwrap();
        let good = base.path().join("programs");
        std::fs::create_dir_all(&good).unwrap();
        let looped = base.path().join("loop-a");
        std::os::unix::fs::symlink(base.path().join("loop-b"), &looped).unwrap();
        std::os::unix::fs::symlink(&looped, base.path().join("loop-b")).unwrap();
        let sandbox = FileSandboxConfig {
            read_only_roots: vec![looped.display().to_string(), good.display().to_string()],
            ..FileSandboxConfig::default()
        };

        let roots = with_agent_file_sandbox_for_test(sandbox.clone(), agent_read_only_roots);
        assert_eq!(roots.len(), 2, "{roots:?}");
        assert!(roots.contains(&good.canonicalize().unwrap()), "{roots:?}");
        assert!(roots.contains(&looped), "{roots:?}");

        let unrestricted = FileSandboxConfig {
            mode: FileSandboxMode::Unrestricted,
            ..sandbox
        };
        assert!(with_agent_file_sandbox_for_test(unrestricted, agent_read_only_roots).is_empty());
    }

    #[test]
    fn the_test_agent_sandbox_override_is_cleared_after_a_panic() {
        let panicked = std::panic::catch_unwind(|| {
            with_agent_file_sandbox_for_test(FileSandboxConfig::default(), || {
                panic!("test body failed");
            })
        });
        assert!(panicked.is_err());
        assert!(TEST_AGENT_FILE_SANDBOX.with(|cell| cell.borrow().is_none()));
    }

    #[tokio::test]
    async fn test_file_sandbox_blocks_paths_outside_allowed_roots() {
        let temp_dir = TempDir::new().unwrap();
        let sandbox = FileSandboxConfig {
            mode: FileSandboxMode::WorkspaceWrite,
            allowed_roots: vec![temp_dir.path().display().to_string()],
            read_only_roots: Vec::new(),
            extra_writable_roots: Vec::new(),
            allow_delete: true,
        };

        let action = FileAction::Read {
            path: PathBuf::from("/etc/hosts"),
            encoding: None,
        };

        let result = execute_file_action(&action, &sandbox).await;
        assert!(result.is_err());
    }

    /// God mode: what the sandbox refuses runs once the owner allowed it for
    /// this session; with god mode off, or without a trusted session, the
    /// sandbox's refusal stands. Files follow the same rule.
    #[tokio::test]
    async fn god_mode_runs_what_the_sandbox_refuses_once_allowed() {
        use crate::magician_v2::god_mode::{self, GodSession};
        let allowed = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let sandbox = ShellSandboxConfig {
            allowed_working_dirs: vec![allowed.path().display().to_string()],
            ..ShellSandboxConfig::default()
        };
        let action = BashAction {
            command: "pwd".to_string(),
            working_dir: Some(outside.path().to_path_buf()),
            env: HashMap::new(),
            timeout_secs: Some(5),
            capture_output: true,
            stdin: None,
        };
        {
            let _off = god_mode::test_scope(false);
            assert!(GodSession::for_chat("owner", "default", "chat-1").is_none());
            let refused = execute_bash_action(&action, &sandbox, None, None).await;
            assert!(refused.is_err(), "god mode off: the sandbox refuses");
        }
        let _on = god_mode::test_scope(true);
        let session = GodSession::for_chat("owner", "default", "chat-1").unwrap();
        let no_session = execute_bash_action(&action, &sandbox, None, None)
            .await
            .unwrap_err();
        assert!(
            no_session
                .to_string()
                .contains("outside sandbox allowed roots"),
            "outside a god-mode chat or run the sandbox's refusal stands: {no_session}"
        );
        // A refusal the agent acts on, logged as a warning, naming the route.
        assert!(
            matches!(no_session, ExecutionError::Refused(_)),
            "{no_session}"
        );
        assert!(no_session.to_string().contains("run_project_checks"));

        god_mode::grant_for_session_in_test(&session, &[god_mode::program_grant("pwd")]);
        let ran = god_mode::with_session(
            Some(session.clone()),
            execute_bash_action(&action, &sandbox, None, None),
        )
        .await
        .expect("allowed for the session");
        let canonical = outside.path().canonicalize().unwrap();
        assert!(
            ran.as_text()
                .unwrap()
                .trim()
                .ends_with(canonical.file_name().unwrap().to_str().unwrap()),
            "{:?}",
            ran.as_text()
        );

        let files = FileSandboxConfig {
            mode: FileSandboxMode::WorkspaceWrite,
            allowed_roots: vec![allowed.path().display().to_string()],
            read_only_roots: Vec::new(),
            extra_writable_roots: Vec::new(),
            allow_delete: true,
        };
        let target = outside.path().join("note.txt");
        god_mode::grant_for_session_in_test(&session, &[god_mode::folder_grant(outside.path())]);
        god_mode::with_session(
            Some(session),
            execute_file_action(
                &FileAction::Write {
                    path: target.clone(),
                    content: "hi".to_string(),
                    create_dirs: false,
                },
                &files,
            ),
        )
        .await
        .expect("the folder is allowed for the session");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hi");
    }

    /// A step's env changes what its command runs (`PATH=. git`), so god mode
    /// reads it as part of the line and requires an exact invocation grant.
    #[test]
    fn god_mode_reads_the_step_env_as_part_of_the_line() {
        use crate::magician_v2::god_mode::shell_scope;
        let plain = BashAction::new("git status");
        assert_eq!(god_mode_shell_line(&plain), "git status");
        assert!(shell_scope::read(&god_mode_shell_line(&plain))
            .opaque
            .is_none());

        let with_env = BashAction::new("git status")
            .with_env("PATH", "/tmp/x")
            .with_env("GIT_SSH_COMMAND", "sh -c 'x'");
        let line = god_mode_shell_line(&with_env);
        assert_eq!(
            line,
            "GIT_SSH_COMMAND='sh -c '\\''x'\\''' PATH='/tmp/x' git status"
        );
        let reading = shell_scope::read(&line);
        assert_eq!(
            reading
                .commands
                .iter()
                .map(|c| c.program.as_str())
                .collect::<Vec<_>>(),
            ["git"],
            "the quoted values parse as values"
        );
        assert!(
            reading.opaque.is_some(),
            "an env requires an exact invocation grant"
        );

        let odd_key = BashAction::new("git status").with_env("NOT-A-NAME", "1");
        let line = god_mode_shell_line(&odd_key);
        assert_eq!(line, "env 'NOT-A-NAME=1' git status");
        assert!(shell_scope::read(&line).opaque.is_some());
    }

    /// A move removes its source, so god mode asks to write there; a copy
    /// only reads it.
    #[test]
    fn god_mode_asks_to_write_a_moved_source() {
        let (source, destination) = (PathBuf::from("/a/src.txt"), PathBuf::from("/b/dst.txt"));
        let moved = FileAction::Move {
            source: source.clone(),
            destination: destination.clone(),
        };
        assert_eq!(
            file_action_touched_paths(&moved),
            vec![(source.as_path(), true), (destination.as_path(), true)]
        );
        let copied = FileAction::Copy {
            source: source.clone(),
            destination: destination.clone(),
        };
        assert_eq!(
            file_action_touched_paths(&copied),
            vec![(source.as_path(), false), (destination.as_path(), true)]
        );
    }

    /// A destination let through on its own (`allow_private`, or god mode)
    /// follows redirects only on its host and port, or under `public` to an
    /// IP literal the policy allows; anything else stops at the 3xx.
    #[test]
    fn a_granted_destination_redirects_only_to_itself() {
        let _policy = crate::magician_v2::execution::network_policy::test_lock();
        let _public = crate::magician_v2::execution::network_policy::override_for_test(
            crate::config::NetworkPolicySettings {
                mode: crate::config::NetworkMode::Public,
                allow_private: vec!["10.0.0.9:8080".to_string()],
            },
        );
        let url = |text: &str| url::Url::parse(text).unwrap();
        let followed = |next: &str, public: bool| {
            granted_redirect_followed("nas.local", 8080, public, &url(next))
        };
        assert!(followed("http://nas.local:8080/next", true));
        assert!(
            followed("https://NAS.local:8080/next", true),
            "host names compare without case"
        );
        assert!(
            !followed("http://nas.local:9090/next", true),
            "another port"
        );
        assert!(
            !followed("http://elsewhere.example/next", true),
            "a name hop could resolve anywhere"
        );
        assert!(
            !followed("http://127.0.0.1:8080/next", true),
            "a private literal"
        );
        assert!(
            followed("http://93.184.216.34/next", true),
            "a public literal under `public`"
        );
        assert!(
            followed("http://10.0.0.9:8080/next", true),
            "a literal in `allow_private`"
        );
        assert!(
            !followed("http://93.184.216.34/next", false),
            "under `deny` nothing else"
        );
        assert!(granted_redirect_followed(
            "::1",
            8080,
            true,
            &url("http://[::1]:8080/x")
        ));
    }

    #[tokio::test]
    async fn an_allow_private_destination_does_not_follow_a_redirect_elsewhere() {
        // Loopback test servers: hold the policy lock, and make `public` for
        // this thread only.
        let _policy = crate::magician_v2::execution::network_policy::test_lock_async().await;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let origin = MockServer::start().await;
        let elsewhere = MockServer::start().await;
        let origin_port = origin.address().port();
        let _public = crate::magician_v2::execution::network_policy::override_for_test(
            crate::config::NetworkPolicySettings {
                mode: crate::config::NetworkMode::Public,
                allow_private: vec![format!("127.0.0.1:{origin_port}")],
            },
        );
        Mock::given(method("GET"))
            .and(path("/away"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", format!("{}/landing", elsewhere.uri()).as_str()),
            )
            .mount(&origin)
            .await;
        Mock::given(method("GET"))
            .and(path("/here"))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", "/landing"))
            .mount(&origin)
            .await;
        for server in [&origin, &elsewhere] {
            Mock::given(method("GET"))
                .and(path("/landing"))
                .respond_with(ResponseTemplate::new(200).set_body_string("landed"))
                .mount(server)
                .await;
        }
        let get = |url: String| HttpAction {
            method: HttpMethod::Get,
            url,
            headers: HashMap::new(),
            body: None,
            content_type: None,
            timeout_secs: Some(5),
            follow_redirects: true,
            carries_credential: false,
        };

        let away = execute_http_action(&get(format!("{}/away", origin.uri())), None)
            .await
            .expect("the 3xx is returned");
        let ActionResult::Http { status, .. } = away else {
            panic!("http result")
        };
        assert_eq!(status, 302, "a hop to another port is not followed");
        assert!(
            elsewhere.received_requests().await.unwrap().is_empty(),
            "the other server saw nothing"
        );

        let here = execute_http_action(&get(format!("{}/here", origin.uri())), None)
            .await
            .expect("a same-origin hop is followed");
        let ActionResult::Http { status, body, .. } = here else {
            panic!("http result")
        };
        assert_eq!((status, body.as_str()), (200, "landed"));
    }

    #[tokio::test]
    async fn god_mode_runtime_http_asks_and_reuses_only_owner_approved_access() {
        use crate::magician_v2::execution::shell_box::{
            register_loopback_service_for_test, LoopbackService,
        };
        use crate::magician_v2::god_mode::{self, GodSession};
        use crate::magician_v2::user_requests::service::{UserRequestService, UserResponse};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let _policy = crate::magician_v2::execution::network_policy::test_lock_async().await;
        let _god = god_mode::test_scope(true);
        let _roots =
            crate::magician_v2::execution::goal_workspace_access::install_test_roots(Vec::new());
        let api = MockServer::start().await;
        let other_api = MockServer::start().await;
        let _api_port =
            register_loopback_service_for_test(api.address().port(), LoopbackService::MagicianApi);
        let _other_port = register_loopback_service_for_test(
            other_api.address().port(),
            LoopbackService::MagicianApi,
        );
        Mock::given(method("GET"))
            .and(path("/snapshot"))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", "/ready"))
            .mount(&api)
            .await;
        Mock::given(method("GET"))
            .and(path("/ready"))
            .respond_with(ResponseTemplate::new(200).set_body_string("runtime snapshot"))
            .mount(&api)
            .await;
        Mock::given(method("GET"))
            .and(path("/away"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", format!("{}/ready", other_api.uri()).as_str()),
            )
            .mount(&api)
            .await;
        let action = HttpAction {
            method: HttpMethod::Get,
            url: format!("{}/snapshot", api.uri()),
            headers: HashMap::new(),
            body: None,
            content_type: None,
            timeout_secs: Some(5),
            follow_redirects: true,
            carries_credential: false,
        };
        // Turning the setting on alone is insufficient: a live trusted God
        // session and an owner decision are still required.
        assert!(execute_http_action(&action, None).await.is_err());
        assert!(api.received_requests().await.unwrap().is_empty());
        let session =
            GodSession::for_chat("owner", "default", "runtime-api-approval-test").unwrap();
        let service = Arc::new(UserRequestService::new(Arc::new(
            crate::magician_v2::realtime_events::RuntimeTransportBroadcaster::new(64),
        )));
        god_mode::with_test_approver(service.clone(), async {
            for decision in ["deny", "allow_once", "deny", "session:host"] {
                let before = api.received_requests().await.unwrap().len();
                let answer = async {
                    let request = tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            if let Some(request) = service.list_pending().await.into_iter().next() {
                                break request;
                            }
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("runtime HTTP must offer a God-mode decision");
                    for id in ["allow_once", "session:host", "always:host", "deny"] {
                        assert!(request.options.iter().any(|option| option.id == id));
                    }
                    assert_eq!(request.context["god_mode"], true);
                    assert!(
                        service
                            .respond(UserResponse {
                                request_id: request.id,
                                decision: decision.into(),
                                input: None,
                                channel: "web".into(),
                                sensitive: Vec::new()
                            })
                            .await
                    );
                };
                let (result, ()) = tokio::join!(
                    god_mode::with_session(
                        Some(session.clone()),
                        execute_http_action(&action, None)
                    ),
                    answer
                );
                if decision == "deny" {
                    assert!(result.is_err());
                    assert_eq!(api.received_requests().await.unwrap().len(), before);
                } else {
                    assert!(matches!(
                        result.unwrap(),
                        ActionResult::Http { status: 200, .. }
                    ));
                    assert_eq!(api.received_requests().await.unwrap().len(), before + 2);
                }
            }
            let repeat = tokio::time::timeout(
                Duration::from_secs(2),
                god_mode::with_session(Some(session.clone()), execute_http_action(&action, None)),
            )
            .await
            .unwrap();
            assert!(
                repeat.is_ok(),
                "the chosen session host grant covers a repeated call"
            );
            assert!(service.list_pending().await.is_empty());
            // A same-origin redirect works above. Another protected origin
            // requires its own direct call; a redirect cannot reach it.
            let mut away = action.clone();
            away.url = format!("{}/away", api.uri());
            let error =
                god_mode::with_session(Some(session.clone()), execute_http_action(&away, None))
                    .await
                    .unwrap_err();
            assert!(error.to_string().contains("approved runtime API origin"));
            assert!(other_api.received_requests().await.unwrap().is_empty());
            assert!(god_mode::with_session(
                Some(session),
                http_destination_reach(&action.url, true)
            )
            .await
            .is_err());
            assert!(
                service.list_pending().await.is_empty(),
                "goal-workspace fencing is never overridable"
            );
        })
        .await;
    }

    /// The OS box (`shell_sandbox.os_enforce`) on the real macOS launcher: a
    /// write inside the working-dir roots works, one outside fails however the
    /// command phrases it; in god mode a covered command runs outside the box.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn the_os_box_holds_writes_to_the_roots_and_god_mode_lifts_it() {
        use crate::magician_v2::god_mode::{self, GodSession};
        if !crate::magician_v2::execution::shell_box::available() {
            return;
        }
        let work = TempDir::new().unwrap();
        let outside = tempfile::tempdir_in(dirs::home_dir().unwrap()).unwrap();
        let sandbox = ShellSandboxConfig {
            allowed_working_dirs: vec![work.path().display().to_string()],
            os_enforce: true,
            ..ShellSandboxConfig::default()
        };
        let run = |command: String| BashAction {
            command,
            working_dir: Some(work.path().to_path_buf()),
            env: HashMap::new(),
            timeout_secs: Some(10),
            capture_output: true,
            stdin: None,
        };
        let inside = run("echo in > in.txt".to_string());
        execute_bash_action(&inside, &sandbox, None, None)
            .await
            .expect("a write in the roots");
        assert!(work.path().join("in.txt").exists());

        // `touch`, not `echo out > out.txt`: a redirected write is never
        // covered by a lasting grant (`shell_scope`).
        let escape = run(format!("cd {} && touch out.txt", outside.path().display()));
        let refused = execute_bash_action(&escape, &sandbox, None, None).await;
        assert!(refused.is_err() || !outside.path().join("out.txt").exists());
        assert!(
            !outside.path().join("out.txt").exists(),
            "the box refuses a write outside the roots"
        );

        let _on = god_mode::test_scope(true);
        let session = GodSession::for_chat("owner", "default", "box-chat").unwrap();
        god_mode::grant_for_session_in_test(
            &session,
            &[
                god_mode::program_grant("cd"),
                god_mode::program_grant("touch"),
            ],
        );
        god_mode::with_session(
            Some(session),
            execute_bash_action(&escape, &sandbox, None, None),
        )
        .await
        .expect("god mode runs a covered command outside the box");
        assert!(outside.path().join("out.txt").exists());
    }

    #[tokio::test]
    async fn test_bash_echo() {
        let action = BashAction {
            command: "echo 'Hello from bash'".to_string(),
            working_dir: None,
            env: HashMap::new(),
            timeout_secs: Some(5),
            capture_output: true,
            stdin: None,
        };

        let result = execute_bash_action(&action, &ShellSandboxConfig::default(), None, None)
            .await
            .unwrap();
        let text = result.as_text().unwrap();
        assert!(text.contains("Hello from bash"));
    }

    #[tokio::test]
    async fn god_mode_can_approve_valid_expansion_without_broadening_program_grants() {
        use crate::magician_v2::god_mode::{self, GodSession};
        use crate::magician_v2::user_requests::service::{UserRequestService, UserResponse};
        let _god = god_mode::test_scope(true);
        let _roots =
            crate::magician_v2::execution::goal_workspace_access::install_test_roots(Vec::new());
        let session = GodSession::for_chat("owner", "default", "shell-expansion-test").unwrap();
        // A pre-existing broad printf grant must not cover evaluated arguments.
        god_mode::grant_for_session_in_test(&session, &[god_mode::program_grant("printf")]);
        let action = BashAction::new("printf '%s' \"${1:-ok}\"");
        let service = Arc::new(UserRequestService::new(Arc::new(
            RuntimeTransportBroadcaster::new(64),
        )));
        god_mode::with_test_approver(service.clone(), async {
            for decision in ["deny", "allow_once"] {
                let answer = async {
                    let request = tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            if let Some(request) = service.list_pending().await.into_iter().next() {
                                break request;
                            }
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("valid substitutions must reach approval");
                    // An expansion makes the line allow-once only: no lasting
                    // grant may cover code the line evaluates at run time.
                    for id in ["allow_once", "deny"] {
                        assert!(request.options.iter().any(|option| option.id == id));
                    }
                    for id in ["session:exact", "always:exact"] {
                        assert!(!request.options.iter().any(|option| option.id == id));
                    }
                    assert!(
                        service
                            .respond(UserResponse {
                                request_id: request.id,
                                decision: decision.into(),
                                input: None,
                                channel: "web".into(),
                                sensitive: Vec::new()
                            })
                            .await
                    );
                };
                let sandbox = ShellSandboxConfig::default();
                let run = god_mode::with_session(
                    Some(session.clone()),
                    execute_bash_action(&action, &sandbox, None, None),
                );
                let (result, _) = tokio::join!(run, answer);
                if decision == "deny" {
                    assert!(result.is_err());
                } else {
                    assert_eq!(result.unwrap().as_text(), Some("ok"));
                }
            }
        })
        .await;
    }

    fn executable_shell_in(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let shell = dir.join("sh");
        std::fs::write(&shell, "#!/bin/sh\nexit 0\n").expect("script");
        let mut perms = std::fs::metadata(&shell).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&shell, perms).expect("chmod");
        shell
    }

    /// A bash step's env is model-supplied. When it carries `PATH`, the
    /// shell must reach the OS as the absolute file on THAT path — left
    /// bare, std would `fork` to search it in the child (see
    /// `runtime_core::process`) — while the env itself reaches the child
    /// untouched.
    #[test]
    fn bash_shell_is_resolved_against_the_step_env_path() {
        use std::ffi::OsStr;
        let temp = TempDir::new().unwrap();
        let shell = executable_shell_in(temp.path());
        let action = BashAction::new("true").with_env("PATH", temp.path().display().to_string());

        let cmd = build_bash_command(&action, None).expect("build");
        let std_cmd = cmd.as_std();

        assert_eq!(std_cmd.get_program(), shell.as_os_str());
        let child_path = std_cmd
            .get_envs()
            .find(|(key, _)| *key == OsStr::new("PATH"))
            .and_then(|(_, value)| value)
            .expect("the model's PATH is still set on the child");
        assert_eq!(child_path, temp.path().as_os_str());
    }

    /// Without a step-supplied `PATH` the shell resolves on the process PATH
    /// and is still handed over as an absolute file.
    #[test]
    fn bash_shell_is_absolute_on_the_process_path_without_a_step_env_path() {
        use std::ffi::OsStr;
        let action = BashAction::new("true");
        let cmd = build_bash_command(&action, None).expect("build");
        let program = PathBuf::from(cmd.as_std().get_program());
        assert!(program.is_absolute(), "{}", program.display());
        assert!(program.is_file(), "{}", program.display());
        assert_eq!(program.file_name(), Some(OsStr::new("sh")));
        assert!(cmd
            .as_std()
            .get_envs()
            .all(|(key, _)| key != OsStr::new("PATH")));
    }

    #[tokio::test]
    async fn test_bash_stdin_writes_payload_and_closes() {
        // `cat` reads stdin until EOF and echoes it back. With the
        // stdin payload set, we should see it on stdout (proves the
        // bytes were written) and the process should exit (proves
        // stdin was closed).
        let action = BashAction {
            command: "cat".to_string(),
            working_dir: None,
            env: HashMap::new(),
            timeout_secs: Some(5),
            capture_output: true,
            stdin: Some("hello from stdin\n".to_string()),
        };

        let result = execute_bash_action(&action, &ShellSandboxConfig::default(), None, None)
            .await
            .unwrap();
        let text = result.as_text().unwrap();
        assert!(
            text.contains("hello from stdin"),
            "expected stdin echo, got: {text}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_drains_output_while_writing_large_stdin() {
        let payload = "input-line\n".repeat(20_000);
        let mut action = BashAction::new("head -c 131072 /dev/zero; cat");
        action.stdin = Some(payload.clone());
        action.timeout_secs = Some(5);
        let output = execute_bash_action_blocking(&action, None, None)
            .await
            .unwrap();
        assert!(output.as_text().unwrap().ends_with(&payload));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn streaming_deadline_covers_process_work_after_output_eof() {
        let work = TempDir::new().unwrap();
        let mut action = BashAction::new("exec 1>/dev/null 2>/dev/null; sleep 6; touch finished");
        action.working_dir = Some(work.path().to_path_buf());
        action.timeout_secs = Some(1);
        let ctx = ShellStreamContext {
            broadcaster: Arc::new(RuntimeTransportBroadcaster::new(16)),
            execution_id: "exec-eof-deadline".into(),
            principal: None,
            workspace: None,
            base_step_id: "step".into(),
            step_id: "step".into(),
            step_index: 0,
            scrub: None,
        };
        let result = execute_bash_action_streaming(&action, None, ctx).await;
        assert!(result.unwrap_err().to_string().contains("timed out"));
        assert!(!work.path().join("finished").exists());
    }

    #[tokio::test]
    async fn test_bash_exit_code() {
        let action = BashAction {
            command: "exit 1".to_string(),
            working_dir: None,
            env: HashMap::new(),
            timeout_secs: Some(5),
            capture_output: true,
            stdin: None,
        };

        let result = execute_bash_action(&action, &ShellSandboxConfig::default(), None, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_bash_timeout_enforced() {
        let action = BashAction {
            command: "sleep 2".to_string(),
            working_dir: None,
            env: HashMap::new(),
            timeout_secs: Some(1),
            capture_output: true,
            stdin: None,
        };

        let result = execute_bash_action(&action, &ShellSandboxConfig::default(), None, None).await;
        assert!(result.is_err());
        let err = result.err().unwrap().to_string();
        assert!(err.contains("timed out"));
    }

    #[tokio::test]
    async fn test_shell_sandbox_blocks_subshell_escape() {
        let action = BashAction {
            command: "echo $(whoami)".to_string(),
            working_dir: None,
            env: HashMap::new(),
            timeout_secs: Some(5),
            capture_output: true,
            stdin: None,
        };

        let result = execute_bash_action(&action, &ShellSandboxConfig::default(), None, None).await;
        assert!(result.is_err());
        let err = result.err().unwrap().to_string();
        assert!(err.contains("disallowed token"));
    }

    #[test]
    fn test_split_pipeline_respects_quotes() {
        // Simple pipe
        assert_eq!(
            split_pipeline_segments("ls | grep foo"),
            vec!["ls", "grep foo"]
        );

        // Pipe inside double quotes — must NOT split
        assert_eq!(
            split_pipeline_segments(r#"grep -i "pdf\|invoice\|go" file.txt"#),
            vec![r#"grep -i "pdf\|invoice\|go" file.txt"#]
        );

        // Mixed: real pipe + quoted pipe
        assert_eq!(
            split_pipeline_segments(r#"ls -la ~/Downloads/ | grep -i "pdf\|invoice""#),
            vec!["ls -la ~/Downloads/", r#"grep -i "pdf\|invoice""#]
        );

        // Pipe inside single quotes
        assert_eq!(
            split_pipeline_segments("echo 'a|b' | cat"),
            vec!["echo 'a|b'", "cat"]
        );

        // Backslash-escaped pipe outside quotes
        assert_eq!(
            split_pipeline_segments(r"echo hello\|world"),
            vec![r"echo hello\|world"]
        );
    }

    #[test]
    fn test_strip_heredoc_body() {
        // No heredoc — unchanged
        assert_eq!(strip_heredoc_body("ls -la"), "ls -la");

        // Heredoc with body — only command line kept
        let cmd = "osascript << 'EOF'\n-- Check for dialog\ntell app \"Finder\"\nEOF";
        assert_eq!(strip_heredoc_body(cmd), "osascript << 'EOF'");

        // Heredoc with pipe in body should not affect pipeline splitting
        let cmd = "cat << EOF\nsome|data\nEOF";
        let stripped = strip_heredoc_body(cmd);
        assert_eq!(stripped, "cat << EOF");
        assert_eq!(split_pipeline_segments(&stripped), vec!["cat << EOF"]);

        // Command with pipe BEFORE heredoc
        let cmd = "generate_data | osascript << 'EOF'\nscript body\nEOF";
        let stripped = strip_heredoc_body(cmd);
        assert_eq!(
            split_pipeline_segments(&stripped),
            vec!["generate_data", "osascript << 'EOF'"]
        );
    }

    #[test]
    fn task_delivery_shell_literals_and_heredocs_preserve_substitution_checks() {
        for command in [
            r#"python3 -c 'print("- Audio: `mix.wav`; ${literal}; $(literal)")'"#,
            "python3 <<'PY'\nprint('`mix.wav` $(literal)')\nPY\necho done",
            r#"echo \`literal\`"#,
            "python3 -c '\nprint(1 << 2)\nprint(\"`notes`\")'",
        ] {
            assert!(validate_shell_syntax(command).is_ok(), "{command}");
        }
        for command in [
            "echo `id`",
            "echo \"$(id)\"",
            "cat <(id)",
            "cat <<'EOF'\nliteral\nEOF\necho $(id)",
            "cat <<EOF\n'$(id)'\nEOF",
            "cat <<EOF\n`id`\nEOF",
            "echo done # <<'EOF'\necho $(id)\nEOF",
            "echo $\\\n(id)",
        ] {
            assert!(validate_shell_syntax(command).is_err(), "{command}");
        }
        assert_eq!(
            strip_heredoc_body("cat <<'EOF'\ndata\nEOF\necho done"),
            "cat <<'EOF'\necho done"
        );
    }

    /// The file tools admit this execution's task workdir (which the runtime
    /// tells agents to use) and nothing else: not a sibling, not a symlink out
    /// of the workdir, and not the workdir outside a task dispatch.
    #[tokio::test]
    async fn file_tools_admit_only_the_current_task_workdir() {
        let root = TempDir::new().unwrap();
        let sibling = TempDir::new().unwrap();
        let workdir = root.path().join("workdir");
        std::fs::create_dir_all(&workdir).unwrap();
        std::os::unix::fs::symlink(sibling.path(), workdir.join("escape")).unwrap();
        let sandbox = FileSandboxConfig {
            mode: FileSandboxMode::WorkspaceWrite,
            allowed_roots: vec!["/nonexistent-boot-root".to_string()],
            read_only_roots: Vec::new(),
            extra_writable_roots: Vec::new(),
            allow_delete: false,
        };
        let write = |path: PathBuf| FileAction::Write {
            path,
            content: "rows".to_string(),
            create_dirs: true,
        };
        let inside = workdir.join("exports/daily.csv");
        TASK_SHELL_ROOT
            .scope(root.path().to_path_buf(), async {
                execute_file_action(&write(inside.clone()), &sandbox)
                    .await
                    .unwrap();
                assert!(
                    execute_file_action(&write(sibling.path().join("x.csv")), &sandbox)
                        .await
                        .is_err()
                );

                let canonical = |p: &Path| p.canonicalize().unwrap();
                assert_eq!(
                    path_in_task_workdir(&inside.to_string_lossy()),
                    Some(canonical(&inside))
                );
                let new_file = workdir.join("new/report.xlsx");
                assert_eq!(
                    path_in_task_workdir(&new_file.to_string_lossy()),
                    Some(canonical(&workdir).join("new/report.xlsx"))
                );
                for refused in [
                    workdir.join("escape/out.csv"),
                    workdir.join("../manifest.json"),
                    workdir.join("new/../../manifest.json"),
                    sibling.path().join("x.csv"),
                    workdir.clone(),
                ] {
                    assert_eq!(
                        path_in_task_workdir(&refused.to_string_lossy()),
                        None,
                        "{}",
                        refused.display()
                    );
                }
                // Relative paths anchor at the workdir, as `read_file` does.
                assert_eq!(
                    path_in_task_workdir("outputs/relative.csv"),
                    Some(canonical(&workdir).join("outputs/relative.csv"))
                );
                for refused in ["../manifest.json", "new/../../manifest.json", ""] {
                    assert_eq!(path_in_task_workdir(refused), None, "{refused}");
                }
                // A relative path lands in the workdir, not the process cwd.
                execute_file_action(&write(PathBuf::from("notebooks/a.py")), &sandbox)
                    .await
                    .unwrap();
                assert!(workdir.join("notebooks/a.py").is_file());
            })
            .await;
        assert_eq!(std::fs::read_to_string(&inside).unwrap(), "rows");
        assert!(
            execute_file_action(&write(workdir.join("late.csv")), &sandbox)
                .await
                .is_err()
        );
        assert_eq!(path_in_task_workdir(&inside.to_string_lossy()), None);
    }

    #[tokio::test]
    async fn task_delivery_shell_defaults_to_task_workdir_and_rejects_sibling_and_symlink_escape() {
        let root = TempDir::new().unwrap();
        let sibling = TempDir::new().unwrap();
        let nested = root.path().join("downloads/render/nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::os::unix::fs::symlink(sibling.path(), root.path().join("escape")).unwrap();
        let sandbox = ShellSandboxConfig {
            allowed_working_dirs: vec![".".to_string()],
            ..ShellSandboxConfig::default()
        };
        TASK_SHELL_ROOT
            .scope(root.path().to_path_buf(), async {
                let action = BashAction::new("printf produced > notes.txt");
                execute_bash_action(&action, &sandbox, None, None)
                    .await
                    .unwrap();
                assert_eq!(
                    std::fs::read_to_string(root.path().join("workdir/notes.txt")).unwrap(),
                    "produced"
                );
                std::fs::create_dir_all(root.path().join("workdir/relative/render")).unwrap();
                let mut relative = BashAction::new("printf nested > nested.txt");
                relative.working_dir = Some(PathBuf::from("relative/render"));
                execute_bash_action(&relative, &sandbox, None, None)
                    .await
                    .unwrap();
                assert_eq!(
                    std::fs::read_to_string(root.path().join("workdir/relative/render/nested.txt"))
                        .unwrap(),
                    "nested"
                );
                for path in [
                    &nested,
                    &sibling.path().to_path_buf(),
                    &root.path().join("escape"),
                ] {
                    let mut action = BashAction::new("pwd");
                    action.working_dir = Some(path.clone());
                    let result = execute_bash_action(&action, &sandbox, None, None).await;
                    assert_eq!(result.is_ok(), path == &nested, "{}", path.display());
                }
            })
            .await;
        let mut unscoped = BashAction::new("true");
        unscoped.working_dir = Some(nested);
        assert!(
            execute_bash_action(&unscoped, &sandbox, None, None)
                .await
                .is_err(),
            "authority must not leak after the task scope"
        );
    }

    #[test]
    fn html_http_body_is_stripped_and_capped_for_the_model() {
        let html = "<html><head><style>p{color:red}</style></head><body><script>alert(1)</script><h1>Instinct AI</h1><p>Raises more money.</p></body></html>";
        let text = visible_http_body(Some("text/html; charset=utf-8"), html.to_string());
        assert!(text.contains("Instinct AI"));
        assert!(text.contains("Raises more money."));
        assert!(!text.contains("alert"));
        assert!(!text.contains("<h1>"));
        let huge = format!("<p>{}</p>", "x".repeat(20_000));
        let capped = visible_http_body(Some("text/html"), huge);
        assert!(capped.contains("[truncated"));
        assert!(capped.len() < 20_000);
    }

    // ========================================================================
    // Goal workspaces — goal_workspace_access
    // ========================================================================

    thread_local! {
        /// Runs between the file fence's checks and the IO (TOCTOU tests).
        static TOCTOU_HOOK: std::cell::RefCell<Option<Box<dyn Fn()>>> =
            const { std::cell::RefCell::new(None) };
    }

    pub(super) fn run_toctou_hook() {
        TOCTOU_HOOK.with(|hook| {
            if let Some(hook) = hook.borrow().as_ref() {
                hook();
            }
        });
    }

    mod goal_workspaces {
        use super::*;
        use crate::magician_v2::execution::goal_workspace_access as access;

        fn unrestricted() -> FileSandboxConfig {
            FileSandboxConfig {
                mode: FileSandboxMode::Unrestricted,
                ..FileSandboxConfig::default()
            }
        }

        fn read(path: PathBuf) -> FileAction {
            FileAction::Read {
                path,
                encoding: None,
            }
        }

        fn write(path: PathBuf) -> FileAction {
            FileAction::Write {
                path,
                content: "x".to_string(),
                create_dirs: true,
            }
        }

        async fn run_as(
            agent: Option<&str>,
            action: &FileAction,
        ) -> Result<ActionResult, ExecutionError> {
            ACTING_AGENT
                .scope(
                    agent.map(str::to_string),
                    execute_file_action(action, &unrestricted()),
                )
                .await
        }

        /// A non-member reaches nothing in the workspace through the file
        /// tools, even in `unrestricted` mode, and with no identity at all.
        #[tokio::test]
        async fn a_non_member_cannot_read_write_list_or_probe_a_goal_workspace() {
            let fx = access::test_fixture();
            let _roots = access::install_test_roots(vec![fx.root.clone()]);
            let ws = fx.root.join("Magican");
            let goal = ws.join("Goals/X/x.md");
            for agent in [Some("ambassador"), None] {
                for action in [
                    read(goal.clone()),
                    write(goal.clone()),
                    write(ws.join("Facts.md")),
                    FileAction::Exists { path: goal.clone() },
                    FileAction::List {
                        path: ws.join("Goals"),
                        pattern: None,
                    },
                    FileAction::Copy {
                        source: goal.clone(),
                        destination: fx.root.parent().unwrap().join("stolen.md"),
                    },
                ] {
                    let error = run_as(agent, &action).await.unwrap_err().to_string();
                    assert!(
                        error.contains("not a member"),
                        "{agent:?} {action:?}: {error}"
                    );
                }
            }
            assert!(!fx.root.parent().unwrap().join("stolen.md").exists());
            assert!(std::fs::read_to_string(&goal).unwrap().contains("needle"));
            // The admin reads it.
            assert!(run_as(Some(access::DEFAULT_ADMIN_AGENT), &read(goal))
                .await
                .is_ok());
        }

        /// The binary read/write fences (`validate_file_read_path`,
        /// `validate_file_write_path`) apply the same rules.
        #[tokio::test]
        async fn binary_readers_and_writers_are_fenced_too() {
            let fx = access::test_fixture();
            let _roots = access::install_test_roots(vec![fx.root.clone()]);
            let goal = fx.root.join("Magican/Goals/X/x.md");
            let read_refused = ACTING_AGENT
                .scope(Some("ambassador".into()), async {
                    validate_file_read_path(&goal, &unrestricted())
                })
                .await;
            assert!(read_refused.is_err());
            let write_refused = ACTING_AGENT
                .scope(Some("cmo".into()), async {
                    validate_file_write_path(&fx.root.join("Magican/README.md"), &unrestricted())
                })
                .await;
            assert!(write_refused.is_err());
        }

        /// A member reads and writes its workspace's internal goals, but no
        /// private goal of another owner, by any path: directly, through a
        /// rename of the file or its folder, a symlink, or a hard link. It
        /// cannot change the README (members) either.
        #[tokio::test]
        async fn a_member_reaches_internal_goals_but_never_another_owners_private_goal() {
            let fx = access::test_fixture();
            let _roots = access::install_test_roots(vec![fx.root.clone()]);
            let ws = fx.root.join("Magican");
            let outside = fx.root.parent().unwrap().to_path_buf();
            let cmo = Some("cmo");

            assert!(run_as(cmo, &read(ws.join("Goals/X/x.md"))).await.is_ok());
            assert!(run_as(cmo, &write(ws.join("Goals/X/x.md"))).await.is_ok());
            assert!(run_as(cmo, &write(ws.join("Goals/X/new goal.md")))
                .await
                .is_ok());

            for private in ["Goals/X/secret.md", "Goals/X/ceo plan.md"] {
                let path = ws.join(private);
                assert!(run_as(cmo, &read(path.clone())).await.is_err(), "{private}");
                assert!(
                    run_as(cmo, &write(path.clone())).await.is_err(),
                    "{private}"
                );
                assert!(
                    run_as(
                        cmo,
                        &FileAction::Move {
                            source: path.clone(),
                            destination: outside.join("moved.md"),
                        },
                    )
                    .await
                    .is_err(),
                    "{private} moved"
                );
            }
            // Renaming the folder that holds them.
            assert!(run_as(
                cmo,
                &FileAction::Move {
                    source: ws.join("Goals/X"),
                    destination: ws.join("Goals/Y"),
                },
            )
            .await
            .is_err());
            assert!(run_as(
                cmo,
                &FileAction::Delete {
                    path: ws.join("Goals/X"),
                    recursive: true,
                },
            )
            .await
            .is_err());
            assert!(ws.join("Goals/X/secret.md").is_file());

            // A listing never names them.
            let listed = run_as(
                cmo,
                &FileAction::List {
                    path: ws.join("Goals/X"),
                    pattern: None,
                },
            )
            .await
            .unwrap();
            let ActionResult::List { items } = listed else {
                panic!("a list result");
            };
            assert!(items.iter().any(|name| name == "x.md"));
            assert!(!items
                .iter()
                .any(|name| name == "secret.md" || name == "ceo plan.md"));

            #[cfg(unix)]
            {
                let link = outside.join("link.md");
                std::os::unix::fs::symlink(ws.join("Goals/X/secret.md"), &link).unwrap();
                assert!(run_as(cmo, &read(link)).await.is_err(), "through a symlink");
                let hard = outside.join("hard.md");
                std::fs::hard_link(ws.join("Goals/X/ceo plan.md"), &hard).unwrap();
                assert!(
                    run_as(cmo, &read(hard)).await.is_err(),
                    "through a hard link"
                );
            }

            // README holds the members: only the admin changes it.
            assert!(run_as(cmo, &read(ws.join("README.md"))).await.is_ok());
            assert!(run_as(cmo, &write(ws.join("README.md"))).await.is_err());
            assert!(run_as(
                cmo,
                &FileAction::Move {
                    source: ws.join("Goals/X/x.md"),
                    destination: ws.join("README.md"),
                },
            )
            .await
            .is_err());
            assert!(std::fs::read_to_string(ws.join("README.md"))
                .unwrap()
                .contains("members: [cmo]"));
            // Nor a new workspace beside it.
            assert!(run_as(cmo, &write(fx.root.join("Mine/README.md")))
                .await
                .is_err());
            assert!(run_as(
                Some(access::DEFAULT_ADMIN_AGENT),
                &write(ws.join("README.md"))
            )
            .await
            .is_ok());
        }

        /// A delegated child keeps its parent's `agent_id`; the active owner
        /// decides who it acts as.
        #[tokio::test]
        async fn a_delegated_child_acts_as_its_active_owner_not_its_parent() {
            let fx = access::test_fixture();
            let _roots = access::install_test_roots(vec![fx.root.clone()]);
            let acting =
                acting_agent_from_context(Some("ambassador"), Some(access::DEFAULT_ADMIN_AGENT));
            assert_eq!(acting.as_deref(), Some("ambassador"));
            let goal = fx.root.join("Magican/Goals/X/x.md");
            assert!(run_as(acting.as_deref(), &read(goal)).await.is_err());
            assert_eq!(
                acting_agent_from_context(None, Some("cmo")).as_deref(),
                Some("cmo")
            );
        }

        /// No task-local identity (a spawned task, a runtime path) is no
        /// agent: a non-member.
        #[tokio::test]
        async fn a_missing_identity_is_a_non_member() {
            let fx = access::test_fixture();
            let _roots = access::install_test_roots(vec![fx.root.clone()]);
            assert!(current_acting_agent().is_none());
            let error =
                execute_file_action(&read(fx.root.join("Magican/Goals/X/x.md")), &unrestricted())
                    .await
                    .unwrap_err();
            assert!(error.to_string().contains("not a member"));
        }

        /// A compiled dispatch defers to the active owner in scope and can
        /// never widen identity.
        #[tokio::test]
        async fn compiled_dispatch_defers_to_the_active_owner() {
            assert_eq!(compiled_acting_agent(Some("cmo")).as_deref(), Some("cmo"));
            let same = ACTING_AGENT
                .scope(Some("cmo".into()), async {
                    compiled_acting_agent(Some("cmo"))
                })
                .await;
            assert_eq!(same.as_deref(), Some("cmo"));
            // A delegated ambassador whose run identity still names its
            // parent acts as itself, never as the parent.
            let conflict = ACTING_AGENT
                .scope(Some("ambassador".into()), async {
                    compiled_acting_agent(Some(
                        crate::magician_v2::execution::goal_workspace_access::DEFAULT_ADMIN_AGENT,
                    ))
                })
                .await;
            assert_eq!(conflict.as_deref(), Some("ambassador"));
            let demoted = ACTING_AGENT
                .scope(None, async { compiled_acting_agent(Some("cmo")) })
                .await;
            assert_eq!(demoted, None);
        }

        /// The notes hybrid index is unreadable to every agent, the admin
        /// included.
        #[tokio::test]
        async fn the_notes_hybrid_index_is_unreadable_to_agents() {
            let base = TempDir::new().unwrap();
            let index = base
                .path()
                .join("scopes/anonymous/default/notes/hybrid-index");
            std::fs::create_dir_all(&index).unwrap();
            std::fs::write(index.join("data.lance"), "embedded notes").unwrap();
            for agent in [Some(access::DEFAULT_ADMIN_AGENT), Some("cmo"), None] {
                assert!(run_as(agent, &read(index.join("data.lance")))
                    .await
                    .is_err());
                assert!(run_as(
                    agent,
                    &FileAction::List {
                        path: index.clone(),
                        pattern: None,
                    },
                )
                .await
                .is_err());
            }
        }

        /// Without an OS box the shell is refused to an agent a goal
        /// workspace is fenced from; the admin keeps it.
        #[tokio::test]
        async fn without_an_os_box_the_shell_is_refused_to_fenced_agents() {
            let fx = access::test_fixture();
            let _roots = access::install_test_roots(vec![fx.root.clone()]);
            let sandbox = ShellSandboxConfig {
                os_enforce: false,
                ..ShellSandboxConfig::default()
            };
            let action = BashAction {
                command: format!("cat '{}'", fx.root.join("Magican/Goals/X/x.md").display()),
                working_dir: None,
                env: HashMap::new(),
                timeout_secs: Some(10),
                capture_output: true,
                stdin: None,
            };
            let refused = ACTING_AGENT
                .scope(
                    Some("ambassador".into()),
                    execute_bash_action(&action, &sandbox, None, None),
                )
                .await
                .unwrap_err();
            assert!(refused.to_string().contains("goal workspaces"), "{refused}");
            let admin = ACTING_AGENT
                .scope(
                    Some(access::DEFAULT_ADMIN_AGENT.into()),
                    execute_bash_action(&action, &sandbox, None, None),
                )
                .await;
            assert!(admin.is_ok(), "{admin:?}");
        }

        /// A symlink swapped after the fence's check cannot redirect a read
        /// to a private goal: the descriptor is checked again.
        #[cfg(unix)]
        #[tokio::test]
        async fn a_symlink_swapped_after_the_check_cannot_read_a_private_goal() {
            let fx = access::test_fixture();
            let _roots = access::install_test_roots(vec![fx.root.clone()]);
            let outside = fx.root.parent().unwrap().to_path_buf();
            std::fs::write(outside.join("open.md"), "open").unwrap();
            let link = outside.join("swap.md");
            std::os::unix::fs::symlink(outside.join("open.md"), &link).unwrap();
            let secret = fx.root.join("Magican/Goals/X/secret.md");
            let swap_link = link.clone();
            TOCTOU_HOOK.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    let _ = std::fs::remove_file(&swap_link);
                    std::os::unix::fs::symlink(&secret, &swap_link).unwrap();
                }));
            });
            let result = run_as(Some("cmo"), &read(link.clone())).await;
            TOCTOU_HOOK.with(|hook| *hook.borrow_mut() = None);
            let error = result.expect_err("the swapped read is refused");
            assert!(!error.to_string().contains("needle"));
            assert!(
                error.to_string().contains("changed while it was checked"),
                "{error}"
            );
        }

        /// A parent folder swapped after the check cannot receive a write,
        /// and the refused write leaves no file behind.
        #[cfg(unix)]
        #[tokio::test]
        async fn a_parent_swapped_after_the_check_cannot_receive_a_write() {
            let fx = access::test_fixture();
            let _roots = access::install_test_roots(vec![fx.root.clone()]);
            let outside = fx.root.parent().unwrap().to_path_buf();
            std::fs::create_dir_all(outside.join("real")).unwrap();
            let link = outside.join("dir");
            std::os::unix::fs::symlink(outside.join("real"), &link).unwrap();
            let target_ws = fx.root.join("Magican");
            let swap_link = link.clone();
            TOCTOU_HOOK.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    let _ = std::fs::remove_file(&swap_link);
                    std::os::unix::fs::symlink(&target_ws, &swap_link).unwrap();
                }));
            });
            let result = run_as(Some("ambassador"), &write(link.join("planted.md"))).await;
            TOCTOU_HOOK.with(|hook| *hook.borrow_mut() = None);
            assert!(result.is_err());
            assert!(!fx.root.join("Magican/planted.md").exists());
        }

        /// A write refused after the open (its folder swapped into a
        /// workspace) never deletes or truncates the file already there:
        /// only a file the call itself created is removed.
        #[cfg(unix)]
        #[tokio::test]
        async fn a_refused_write_keeps_an_existing_file() {
            let fx = access::test_fixture();
            let _roots = access::install_test_roots(vec![fx.root.clone()]);
            let outside = fx.root.parent().unwrap().to_path_buf();
            std::fs::create_dir_all(outside.join("real")).unwrap();
            std::fs::write(outside.join("real/x.md"), "mine").unwrap();
            let link = outside.join("dir");
            std::os::unix::fs::symlink(outside.join("real"), &link).unwrap();
            let goals = fx.root.join("Magican/Goals/X");
            let swap_link = link.clone();
            TOCTOU_HOOK.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    let _ = std::fs::remove_file(&swap_link);
                    std::os::unix::fs::symlink(&goals, &swap_link).unwrap();
                }));
            });
            let result = run_as(Some("ambassador"), &write(link.join("x.md"))).await;
            TOCTOU_HOOK.with(|hook| *hook.borrow_mut() = None);
            assert!(result.is_err());
            let goal = std::fs::read_to_string(fx.root.join("Magican/Goals/X/x.md")).unwrap();
            assert!(goal.contains("needle"), "the goal is untouched: {goal}");
            assert_eq!(
                std::fs::read_to_string(outside.join("real/x.md")).unwrap(),
                "mine"
            );
        }

        /// A delete whose folder is swapped into a workspace after the check
        /// removes nothing there: the entry is unlinked relative to the
        /// folder descriptor that was checked.
        #[cfg(unix)]
        #[tokio::test]
        async fn a_delete_with_a_swapped_folder_removes_nothing_in_the_workspace() {
            let fx = access::test_fixture();
            let _roots = access::install_test_roots(vec![fx.root.clone()]);
            let outside = fx.root.parent().unwrap().to_path_buf();
            std::fs::create_dir_all(outside.join("real")).unwrap();
            std::fs::write(outside.join("real/x.md"), "mine").unwrap();
            let link = outside.join("dir");
            std::os::unix::fs::symlink(outside.join("real"), &link).unwrap();
            let goals = fx.root.join("Magican/Goals/X");
            let swap_link = link.clone();
            TOCTOU_HOOK.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    let _ = std::fs::remove_file(&swap_link);
                    std::os::unix::fs::symlink(&goals, &swap_link).unwrap();
                }));
            });
            let _ = run_as(
                Some("ambassador"),
                &FileAction::Delete {
                    path: link.join("x.md"),
                    recursive: false,
                },
            )
            .await;
            TOCTOU_HOOK.with(|hook| *hook.borrow_mut() = None);
            assert!(fx.root.join("Magican/Goals/X/x.md").exists());
        }

        /// A move whose destination folder is swapped into a workspace after
        /// the check is refused.
        #[cfg(unix)]
        #[tokio::test]
        async fn move_with_a_swapped_parent_is_refused() {
            let fx = access::test_fixture();
            let _roots = access::install_test_roots(vec![fx.root.clone()]);
            let outside = fx.root.parent().unwrap().to_path_buf();
            std::fs::write(outside.join("note.md"), "note").unwrap();
            std::fs::create_dir_all(outside.join("dest")).unwrap();
            let link = outside.join("into");
            std::os::unix::fs::symlink(outside.join("dest"), &link).unwrap();
            let target = fx.root.join("Magican/Goals/X");
            let swap_link = link.clone();
            TOCTOU_HOOK.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    let _ = std::fs::remove_file(&swap_link);
                    std::os::unix::fs::symlink(&target, &swap_link).unwrap();
                }));
            });
            let result = run_as(
                Some("ambassador"),
                &FileAction::Move {
                    source: outside.join("note.md"),
                    destination: link.join("note.md"),
                },
            )
            .await;
            TOCTOU_HOOK.with(|hook| *hook.borrow_mut() = None);
            assert!(result.is_err());
            assert!(!fx.root.join("Magican/Goals/X/note.md").exists());
            assert!(outside.join("note.md").exists());
        }

        fn duckdb(sql: String) -> DuckDbAction {
            DuckDbAction {
                sql,
                database: None,
                output_format: "json".to_string(),
                timeout_secs: Some(30),
            }
        }

        /// A fenced agent's DuckDB reads files only under its task folder:
        /// globs, replacement scans and links out of it are refused.
        #[test]
        fn duckdb_reads_only_the_task_folder_for_fenced_agents() {
            let fx = access::test_fixture();
            let outside = fx.root.parent().unwrap().to_path_buf();
            let task = outside.join("task");
            std::fs::create_dir_all(task.join("workdir")).unwrap();
            std::fs::write(task.join("workdir/data.csv"), "a,b\n1,2\n").unwrap();
            let session = DuckDbSession::new().expect("DuckDB session");
            let run = |agent: &str, sql: String| {
                access::with_test_roots(vec![fx.root.clone()], || {
                    TASK_SHELL_ROOT.sync_scope(task.clone(), || {
                        ACTING_AGENT.sync_scope(Some(agent.to_string()), || {
                            execute_duckdb_action(&duckdb(sql), &session)
                        })
                    })
                })
            };
            let goal = fx.root.join("Magican/Goals/X/x.md").display().to_string();
            // A glob and a replacement scan that never spell the folder name.
            let glob = format!("SELECT * FROM glob('{}/*/*/*/*/*.md')", outside.display());
            assert!(run("ambassador", glob).is_err());
            let scan = format!(
                "SELECT * FROM read_text('{}')",
                goal.replace("Workspaces", "Work' || 'spaces")
            );
            assert!(run("ambassador", scan).is_err());
            // A link inside the task folder that leads into the workspace,
            // to a parent of `Workspaces/`, to `/`, or nowhere yet: any link
            // out of the task folder refuses the query, whatever it reads.
            #[cfg(unix)]
            {
                let own_csv = format!(
                    "SELECT * FROM read_csv('{}/workdir/data.csv')",
                    task.display()
                );
                for (name, target) in [
                    ("w", fx.root.join("Magican")),
                    ("up", outside.clone()),
                    ("root", PathBuf::from("/")),
                    ("later", outside.join("not-yet")),
                ] {
                    let link = task.join("workdir").join(name);
                    std::os::unix::fs::symlink(&target, &link).unwrap();
                    let through = format!(
                        "SELECT * FROM read_text('{}/Workspaces/Magican/Goals/X/x.md')",
                        link.display()
                    );
                    assert!(run("ambassador", through).is_err(), "{name}");
                    assert!(run("ambassador", own_csv.clone()).is_err(), "{name}");
                    std::fs::remove_file(&link).unwrap();
                }
                // A link that stays inside the task folder is fine.
                std::os::unix::fs::symlink(
                    task.join("workdir/data.csv"),
                    task.join("workdir/same.csv"),
                )
                .unwrap();
                assert!(run("ambassador", own_csv.clone()).is_ok());
                std::fs::remove_file(task.join("workdir/same.csv")).unwrap();
                // A hard link to a goal the agent may not read.
                std::fs::hard_link(
                    fx.root.join("Magican/Goals/X/x.md"),
                    task.join("workdir/copy.md"),
                )
                .unwrap();
                assert!(run("ambassador", own_csv).is_err());
                std::fs::remove_file(task.join("workdir/copy.md")).unwrap();
            }
            // Its own task data still reads.
            let own = format!(
                "SELECT * FROM read_csv('{}/workdir/data.csv')",
                task.display()
            );
            assert!(run("ambassador", own).is_ok());
            // The admin is not fenced.
            let admin = format!("SELECT count(*) FROM glob('{}/*.md')", outside.display());
            assert!(run(access::DEFAULT_ADMIN_AGENT, admin).is_ok());
        }

        /// The `http` tool never reaches a local service that is a way out of
        /// every fence, Magician's own API included, under any spelling of
        /// this host: IPv4-mapped, unspecified, a trailing-dot or wildcard
        /// name resolving to loopback, or its own LAN address. A registered
        /// dev server stays open to the admin only. Remote hosts on the
        /// same ports are unchanged.
        #[tokio::test]
        async fn the_http_tool_refuses_local_services() {
            use crate::magician_v2::execution::shell_box::{
                register_loopback_service, register_magician_listener, LoopbackService,
            };
            register_loopback_service(8081, LoopbackService::Supervisor);
            register_loopback_service(3021, LoopbackService::NotesServer);
            register_loopback_service(3003, LoopbackService::Magicutor);
            register_loopback_service(47_173, LoopbackService::DevServer);
            register_magician_listener(3002);
            let mut urls = vec![
                "http://127.0.0.1:8081/".to_string(),
                "http://localhost:3021/.fs/Workspaces".to_string(),
                "http://127.0.0.1:3003/".to_string(),
                "http://127.0.0.1:3002/api/magician/v2/tasks".to_string(),
                "http://[::ffff:127.0.0.1]:8081/".to_string(),
                "http://0.0.0.0:3021/".to_string(),
                "http://localhost.:3021/".to_string(),
                "http://localtest.me:3002/".to_string(),
                "http://127.0.0.1:47173/".to_string(),
            ];
            // This host's own LAN address (no packet is sent by `connect`).
            if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
                if socket.connect("203.0.113.1:9").is_ok() {
                    if let Ok(local) = socket.local_addr() {
                        if !local.ip().is_loopback() && !local.ip().is_unspecified() {
                            urls.push(format!("http://{}:8081/", local.ip()));
                        }
                    }
                }
            }
            for url in &urls {
                assert!(
                    ACTING_AGENT
                        .scope(Some("ambassador".into()), loopback_service_refusal(url))
                        .await
                        .is_some(),
                    "{url}"
                );
            }
            // The admin reaches a project's own dev server, never the API.
            let admin = Some(access::DEFAULT_ADMIN_AGENT.to_string());
            assert!(ACTING_AGENT
                .scope(
                    admin.clone(),
                    loopback_service_refusal("http://127.0.0.1:47173/")
                )
                .await
                .is_none());
            assert!(ACTING_AGENT
                .scope(admin, loopback_service_refusal("http://127.0.0.1:3002/"))
                .await
                .is_some());
            assert!(loopback_service_refusal("http://203.0.113.5:8081/")
                .await
                .is_none());
        }

        /// Each redirect hop is checked before it is made: a local service
        /// under any spelling, a non-web scheme, and for a fenced agent a
        /// private literal address.
        #[test]
        fn a_redirect_hop_to_a_local_service_or_a_fenced_private_address_is_refused() {
            use crate::magician_v2::execution::shell_box::{
                register_loopback_service, LoopbackService,
            };
            register_loopback_service(8081, LoopbackService::Supervisor);
            let hop = |url: &str, fenced: bool| {
                redirect_hop_refusal(&url::Url::parse(url).unwrap(), fenced)
            };
            assert!(hop("http://127.0.0.1:8081/", false).is_some());
            assert!(hop("http://[::ffff:127.0.0.1]:8081/", false).is_some());
            assert!(hop("http://localtest.me:8081/", false).is_some());
            assert!(hop("file:///etc/passwd", false).is_some());
            assert!(hop("data:text/html,hi", false).is_some());
            assert!(hop("http://203.0.113.5:8081/", false).is_none());
            assert!(hop("http://10.0.0.5/", true).is_some());
            assert!(hop("http://[::ffff:10.0.0.5]/", true).is_some());
            assert!(hop("http://[fd00::1]/", true).is_some());
            assert!(hop("http://10.0.0.5/", false).is_none());
            assert!(hop("https://example.com/", true).is_none());
        }

        /// A redirect from an ordinary local origin to a local service is
        /// not followed in `allow`, where nothing else filters the hop.
        #[tokio::test]
        async fn the_http_tool_does_not_follow_a_redirect_to_a_local_service() {
            let _policy = crate::magician_v2::execution::network_policy::test_lock_async().await;
            use crate::magician_v2::execution::shell_box::{
                register_loopback_service, LoopbackService,
            };
            use wiremock::matchers::{method, path};
            use wiremock::{Mock, MockServer, ResponseTemplate};
            let origin = MockServer::start().await;
            let service = MockServer::start().await;
            register_loopback_service(service.address().port(), LoopbackService::Supervisor);
            Mock::given(method("GET"))
                .and(path("/go"))
                .respond_with(ResponseTemplate::new(302).insert_header(
                    "Location",
                    format!("http://127.0.0.1:{}/restart", service.address().port()).as_str(),
                ))
                .mount(&origin)
                .await;
            let action = HttpAction {
                method: HttpMethod::Get,
                url: format!("{}/go", origin.uri()),
                headers: HashMap::new(),
                body: None,
                content_type: None,
                timeout_secs: Some(5),
                follow_redirects: true,
                carries_credential: false,
            };
            let error = execute_http_action(&action, None)
                .await
                .expect_err("hop refused");
            assert!(error.to_string().contains("local service"), "{error}");
            assert!(service.received_requests().await.unwrap().is_empty());
        }

        /// A fenced agent's `http` request reaches public hosts only: a
        /// private or loopback address is refused in `allow` too, and god
        /// mode is never asked.
        #[tokio::test]
        async fn a_fenced_agent_http_request_reaches_public_hosts_only() {
            let _policy = crate::magician_v2::execution::network_policy::test_lock_async().await;
            let fx = access::test_fixture();
            let _roots = access::install_test_roots(vec![fx.root.clone()]);
            for url in [
                "http://10.0.0.5/",
                "http://127.0.0.1:9/",
                "http://[::ffff:192.168.1.1]/",
            ] {
                let reach = ACTING_AGENT
                    .scope(Some("ambassador".into()), http_destination_reach(url, true))
                    .await;
                assert!(reach.is_err(), "{url}");
            }
            assert!(ACTING_AGENT
                .scope(
                    Some("ambassador".into()),
                    http_destination_reach("http://1.1.1.1/", true)
                )
                .await
                .is_ok_and(|reach| reach == HttpReach::Filtered));
            assert!(
                ACTING_AGENT
                    .scope(Some("ambassador".into()), async { acting_agent_fenced() })
                    .await
            );
            assert!(
                !ACTING_AGENT
                    .scope(Some(access::DEFAULT_ADMIN_AGENT.into()), async {
                        acting_agent_fenced()
                    })
                    .await
            );
        }

        /// What an execution read from a goal workspace stays with that
        /// workspace: its task folder (the run's evidence) is closed to an
        /// agent that may not read those paths, and open to the reader.
        #[tokio::test]
        async fn evidence_of_an_admin_private_goal_read_is_closed_to_other_agents() {
            let fx = access::test_fixture();
            let _roots = access::install_test_roots(vec![fx.root.clone()]);
            let outside = fx.root.parent().unwrap().to_path_buf();
            let _reads = access::install_test_reads_file(outside.join("reads.json"));
            let task = outside.join("task-admin");
            std::fs::create_dir_all(task.join("workdir")).unwrap();
            std::fs::write(task.join("evidence.json"), "{\"content\":\"needle\"}").unwrap();
            let secret = fx.root.join("Magican/Goals/X/secret.md");
            let admin_read = TASK_SHELL_ROOT
                .scope(
                    task.clone(),
                    run_as(Some(access::DEFAULT_ADMIN_AGENT), &read(secret)),
                )
                .await;
            assert!(admin_read.is_ok());
            assert!(run_as(Some("cmo"), &read(task.join("evidence.json")))
                .await
                .is_err());
            assert!(
                run_as(Some("ambassador"), &read(task.join("evidence.json")))
                    .await
                    .is_err()
            );
            assert!(run_as(
                Some(access::DEFAULT_ADMIN_AGENT),
                &read(task.join("evidence.json"))
            )
            .await
            .is_ok());
            assert!(access::denies_for(Some("cmo"))
                .hidden_dirs
                .iter()
                .any(|dir| dir.ends_with("task-admin")));
        }

        /// Reads are recorded against the execution's own folder: the
        /// reader's evidence is closed, while a delegated child keeps the
        /// shared task folder it works in.
        #[tokio::test]
        async fn evidence_is_recorded_per_execution_so_the_task_folder_stays_open() {
            let fx = access::test_fixture();
            let _roots = access::install_test_roots(vec![fx.root.clone()]);
            let outside = fx.root.parent().unwrap().to_path_buf();
            let _reads = access::install_test_reads_file(outside.join("reads.json"));
            let task = outside.join("task-shared");
            let execution = task.join("executions/exec-admin");
            std::fs::create_dir_all(task.join("workdir")).unwrap();
            std::fs::create_dir_all(&execution).unwrap();
            std::fs::write(task.join("workdir/notes.md"), "shared").unwrap();
            std::fs::write(execution.join("evidence.json"), "needle").unwrap();
            let secret = fx.root.join("Magican/Goals/X/secret.md");
            let admin_read = TASK_SHELL_ROOT
                .scope(
                    task.clone(),
                    EVIDENCE_ROOT.scope(
                        execution.clone(),
                        run_as(Some(access::DEFAULT_ADMIN_AGENT), &read(secret)),
                    ),
                )
                .await;
            assert!(admin_read.is_ok());
            assert!(run_as(Some("cmo"), &read(execution.join("evidence.json")))
                .await
                .is_err());
            assert!(run_as(Some("cmo"), &read(task.join("workdir/notes.md")))
                .await
                .is_ok());
        }

        /// A `Workspaces/` folder nobody registered (a scope made after boot)
        /// is found from the path on its first check.
        #[tokio::test]
        async fn an_unregistered_workspaces_folder_is_fenced_on_first_check() {
            let dir = TempDir::new().unwrap();
            let root = dir
                .path()
                .canonicalize()
                .unwrap()
                .join("late-space")
                .join("Workspaces");
            access::write_fixture(&root);
            // No roots registered at all.
            let _roots = access::install_test_roots(Vec::new());
            let error = run_as(Some("ambassador"), &read(root.join("Magican/Goals/X/x.md")))
                .await
                .unwrap_err();
            assert!(error.to_string().contains("not a member"), "{error}");
        }
    }
}

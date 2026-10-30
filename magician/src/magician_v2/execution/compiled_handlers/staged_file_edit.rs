use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::workspace::{
    DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::agentic::{
    DiffApprovalFile as AgenticDiffApprovalFile, UserInputType,
};
use crate::magician_v2::execution::file_edit::snapshot::MAX_FILE_READ_BYTES;
use crate::magician_v2::execution::file_edit::transaction::{
    FileEditTransaction, ProposedEdit, TransactionScope, TransactionStore,
};

#[derive(Debug, Clone)]
pub struct ScopedWorkspacePath {
    pub principal: String,
    pub workspace: String,
    pub task_id: Option<String>,
    pub execution_id: Option<String>,
    pub scope_root: PathBuf,
    pub workspace_root: PathBuf,
    pub relative_path: PathBuf,
    pub absolute_path: PathBuf,
    /// When Some, the write targets an operator-approved root OUTSIDE the scoped
    /// workspace (approved via the sandbox-override HITL). The staged transaction
    /// records it so the diff-approval apply targets + containment-checks this
    /// root. None = the ordinary in-workspace write under `capability_home_root`.
    pub apply_root: Option<PathBuf>,
    /// Captured God-mode identity; bound to authority only after authorization.
    pub(crate) god_mode_override: Option<crate::magician_v2::god_mode::StagedFileOverride>,
}

/// Recoverable write-path denial: an absolute `file_path` fell outside the
/// scoped workspace root (`capability_home_root`) and no session-approved root
/// covers it. Distinct from the hard repo fence and from malformed-path errors:
/// this is the write-path analogue of `ExecutionError::PathAccessDenied` on
/// the read/native path, meant to be intercepted at the executor error
/// chokepoint and routed into the sandbox-override HITL (approve a folder →
/// merge into `session_file_sandbox_roots` → retry) rather than swallowed into
/// a soft `{status:error}` the LLM sees as a hard failure.
///
/// Callers should propagate this (via `?`/`downcast`) instead of stringifying
/// it; the chokepoint recognizes it by downcast OR by the stable
/// "outside scoped workspace root" message substring (mirroring the
/// `TrustPolicyDeniedError` idiom in the executor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRootDenied {
    /// The absolute path the agent tried to write, resolved lexically.
    pub path: String,
    /// The scoped workspace root the path fell outside of.
    pub workspace_root: String,
}

impl std::fmt::Display for WorkspaceRootDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "path `{}` is outside scoped workspace root `{}`",
            self.path, self.workspace_root
        )
    }
}

impl std::error::Error for WorkspaceRootDenied {}

/// True when `err` is (or wraps) a recoverable [`WorkspaceRootDenied`] write
/// denial. The executor chokepoint uses this to route the write path into the
/// same sandbox-override HITL as the read/native `PathAccessDenied` case.
pub fn is_workspace_root_denied(err: &anyhow::Error) -> bool {
    err.downcast_ref::<WorkspaceRootDenied>().is_some()
}

pub fn scope_from_args(args: &Value) -> (String, String) {
    let principal = args
        .get("__principal")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_SCOPE_PRINCIPAL)
        .to_string();
    let workspace = args
        .get("__workspace")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_SCOPE_WORKSPACE)
        .to_string();
    (principal, workspace)
}

pub fn resolve_path(
    resources: &Arc<AgentResources>,
    args: &Value,
    raw_path: &str,
) -> Result<ScopedWorkspacePath> {
    let raw_path = raw_path.trim();
    if raw_path.is_empty() {
        return Err(anyhow!("path cannot be empty"));
    }

    let (principal, workspace) = scope_from_args(args);
    let task_id = args
        .get("__task_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let execution_id = args
        .get("__execution_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let scope_root = resources
        .artifact_workspace
        .scope_root(&principal, &workspace);
    let workspace_root = resources
        .artifact_workspace
        .capability_home_root(&principal, &workspace);
    std::fs::create_dir_all(&workspace_root)
        .with_context(|| format!("create workspace root {}", workspace_root.display()))?;

    let root_abs = absolutize_lexical(&workspace_root)?;
    let input = PathBuf::from(raw_path);
    let relative_path = if input.is_absolute() {
        let input_abs = absolutize_lexical(&input)?;
        match crate::magician_v2::execution::native_executors::relative_within(
            &input_abs, &root_abs,
        ) {
            Some(rel) => normalize_relative_path(&rel)?,
            None => {
                // Outside the scoped workspace root. If the operator approved a
                // sandbox root that CONTAINS this path (sandbox-override HITL →
                // session_file_sandbox_roots), anchor the write there and record
                // `apply_root` so the diff-approval apply lands + is containment-
                // checked against that approved root. Otherwise surface the
                // RECOVERABLE typed denial so the executor chokepoint can route it
                // into the sandbox-override HITL (approve → retry). The repo fence
                // stays hard downstream; `Display` preserves the prior message.
                if let Some(mut scoped) =
                    resolve_under_approved_root(&input_abs, &principal, &workspace, &scope_root)?
                {
                    scoped.task_id = task_id;
                    scoped.execution_id = execution_id;
                    return Ok(scoped);
                }
                return Err(anyhow::Error::new(WorkspaceRootDenied {
                    path: input_abs.to_string_lossy().to_string(),
                    workspace_root: root_abs.to_string_lossy().to_string(),
                }));
            },
        }
    } else {
        normalize_relative_path(&input)?
    };
    let absolute_path = root_abs.join(&relative_path);

    Ok(ScopedWorkspacePath {
        principal,
        workspace,
        task_id,
        execution_id,
        scope_root,
        workspace_root: root_abs,
        relative_path,
        absolute_path,
        apply_root: None,
        god_mode_override: None,
    })
}

/// A staged write refused because its target sits under one of the file
/// sandbox's `read_only_roots`. Handlers report it as a refusal, not as an
/// invalid path. The diff-approval apply checks every target again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOnlyRootDenied {
    pub message: String,
}

impl std::fmt::Display for ReadOnlyRootDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ReadOnlyRootDenied {}

/// True when `err` is (or wraps) a [`ReadOnlyRootDenied`].
pub fn is_read_only_root_denied(err: &anyhow::Error) -> bool {
    err.downcast_ref::<ReadOnlyRootDenied>().is_some()
}

/// [`resolve_path`] for a path the caller will write. A path under one of the
/// file sandbox's `read_only_roots` is refused: an absolute input before it
/// is resolved (so approving a folder can never make it writable), and every
/// input, relative ones too, at its final target. Inputs that are only read
/// (media sources) use `resolve_path` directly.
pub fn resolve_write_path(
    resources: &Arc<AgentResources>,
    args: &Value,
    raw_path: &str,
) -> Result<ScopedWorkspacePath> {
    let input = Path::new(raw_path.trim());
    if input.is_absolute() {
        if let Ok(input_abs) = absolutize_lexical(input) {
            if let Some(message) = read_only_root_refusal(resources, &input_abs)
                .or_else(|| workspace_write_refusal(&input_abs))
            {
                return Err(anyhow::Error::new(ReadOnlyRootDenied { message }));
            }
        }
    }
    let scoped = resolve_path(resources, args, raw_path)?;
    if let Some(message) = read_only_root_refusal(resources, &scoped.absolute_path)
        .or_else(|| workspace_write_refusal(&scoped.absolute_path))
    {
        return Err(anyhow::Error::new(ReadOnlyRootDenied { message }));
    }
    Ok(scoped)
}

/// Resolve protected targets for God-mode review without changing file policy.
pub fn resolve_tool_write_path(
    resources: &Arc<AgentResources>,
    args: &Value,
    raw_path: &str,
) -> Result<ScopedWorkspacePath> {
    let refusal = match resolve_write_path(resources, args, raw_path) {
        Ok(mut scoped) => {
            scoped.god_mode_override =
                crate::magician_v2::god_mode::StagedFileOverride::for_current_target(
                    &scoped.absolute_path,
                    false,
                );
            return Ok(scoped);
        },
        Err(refusal) => refusal,
    };
    if !crate::magician_v2::god_mode::active()
        || !(is_workspace_root_denied(&refusal) || is_read_only_root_denied(&refusal))
    {
        return Err(refusal);
    }
    let (principal, workspace) = scope_from_args(args);
    let home = resources
        .artifact_workspace
        .capability_home_root(&principal, &workspace);
    let input = Path::new(raw_path.trim());
    let target = if input.is_absolute() {
        absolutize_lexical(input)?
    } else {
        absolutize_lexical(&home.join(normalize_relative_path(input)?))?
    };
    let external = !target.starts_with(absolutize_lexical(&home)?);
    if external && !crate::config::external_writes_enabled() {
        return Err(refusal);
    }
    crate::magician_v2::execution::native_executors::validate_staged_write_fences(
        &target,
        &session_file_sandbox(resources),
        crate::magician_v2::execution::native_executors::current_acting_agent().as_deref(),
    )?;
    let permission =
        crate::magician_v2::god_mode::StagedFileOverride::for_current_target(&target, external)
            .filter(|permission| permission.permits(&principal, &workspace, &target))
            .ok_or(refusal)?;
    // Apply containment needs an existing directory. Keep any not-yet-created
    // parent suffix in the edit's relative path (write_file create_dirs).
    let mut anchor = target
        .parent()
        .ok_or_else(|| anyhow!("file target has no parent"))?;
    while !anchor.is_dir() {
        anchor = anchor
            .parent()
            .ok_or_else(|| anyhow!("file target has no existing parent"))?;
    }
    let relative_path = normalize_relative_path(target.strip_prefix(anchor)?)?;
    Ok(ScopedWorkspacePath {
        principal: principal.clone(),
        workspace: workspace.clone(),
        task_id: args
            .get("__task_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        execution_id: args
            .get("__execution_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        scope_root: resources
            .artifact_workspace
            .scope_root(&principal, &workspace),
        workspace_root: anchor.to_path_buf(),
        relative_path,
        absolute_path: target.clone(),
        apply_root: Some(anchor.to_path_buf()),
        god_mode_override: Some(permission),
    })
}

/// The goal workspace refusal for the acting agent writing `target`
/// (`goal_workspace_access`), or the notes hybrid index. A refusal, not an
/// invalid path: approving a folder never lifts it. The diff-approval apply
/// checks every target again for the agent that staged it.
fn workspace_write_refusal(target: &Path) -> Option<String> {
    use crate::magician_v2::execution::goal_workspace_access as access;
    access::hybrid_index_refusal(target)
        .or_else(|| access::refusal_for_current(target, access::Op::Write))
}

/// This session's effective file sandbox: the boot one plus the roots the
/// owner approved, which count as run-time roots and never outrank a
/// read-only root.
fn session_file_sandbox(resources: &AgentResources) -> runtime_core::FileSandboxConfig {
    let mut sandbox = resources.file_sandbox.clone();
    if let Some(roots) =
        crate::magician_v2::execution::compiled_dispatch::current_session_file_sandbox_roots()
    {
        if let Ok(guard) = roots.lock() {
            sandbox.extra_writable_roots.extend(guard.iter().cloned());
        }
    }
    sandbox
}

/// The suffix for a staged write refused outside the scoped workspace: where
/// `write_file` / `edit_file` do take a file (this task's workdir, the scoped
/// workspace) and how to publish it. Not the file sandbox's roots: a staged
/// write outside the workspace is refused even inside them, so naming them
/// sent agents back to the folder just refused.
pub fn staged_write_refusal_hint(resources: &AgentResources, args: &Value) -> String {
    let (principal, workspace) = scope_from_args(args);
    let home = resources
        .artifact_workspace
        .capability_home_root(&principal, &workspace);
    match crate::magician_v2::execution::native_executors::current_task_workdir() {
        Some(workdir) => format!(
            " (write it in this task's working folder {} or under {}; publish a file by returning it in your outputs)",
            workdir.display(),
            home.display()
        ),
        None => format!(
            " (write it under {}; publish a file by returning it in your outputs)",
            home.display()
        ),
    }
}

/// The file sandbox's read-only refusal for writing `input_abs`, judged
/// with this session's approved roots, exactly as the native `files`
/// executor judges it.
fn read_only_root_refusal(resources: &AgentResources, input_abs: &Path) -> Option<String> {
    if resources.file_sandbox.read_only_roots.is_empty() {
        return None;
    }
    crate::magician_v2::execution::native_executors::read_only_root_refusal(
        input_abs,
        &session_file_sandbox(resources),
    )
}

/// If `input_abs` (an absolute path outside the scoped workspace root) sits under
/// one of the operator-approved session sandbox roots, return a
/// [`ScopedWorkspacePath`] anchored at that approved root with `apply_root` set —
/// so an approved external write stages and (at diff-approval apply time) lands,
/// containment-checked against the approved root. `Ok(None)` when no approved root
/// contains the path (the caller then raises the recoverable denial). The prefix
/// test is lexical over the already-`absolutize_lexical`-normalised input (so a
/// `..` escape beyond the root fails here); the canonicalizing symlink-escape
/// guard in `apply_transaction` is the authoritative second gate at apply time.
fn resolve_under_approved_root(
    input_abs: &Path,
    principal: &str,
    workspace: &str,
    scope_root: &Path,
) -> Result<Option<ScopedWorkspacePath>> {
    // Out-of-workspace writes are ON by default (config::external_writes_enabled), now
    // that the trusted-store authority binding closes the forged/replayed apply_root
    // vector (apply decisions resolve apply_root from in-process memory, never disk).
    // The kill-switch remains: when force-disabled, never anchor at an approved root,
    // so apply_root stays None for every write and no external destination is reached.
    if !crate::config::external_writes_enabled() {
        return Ok(None);
    }
    let Some(roots) =
        crate::magician_v2::execution::compiled_dispatch::current_session_file_sandbox_roots()
    else {
        return Ok(None);
    };
    let approved: Vec<String> = match roots.lock() {
        Ok(guard) => guard.iter().cloned().collect(),
        Err(_) => return Ok(None),
    };
    for root in approved {
        let root_abs = match absolutize_lexical(&PathBuf::from(&root)) {
            Ok(path) => path,
            Err(_) => continue,
        };
        if let Ok(rel) = input_abs.strip_prefix(&root_abs) {
            // The `apply_root` must be a DIRECTORY (the apply-time
            // `resolve_transaction_apply_root` requires `is_dir`). If the operator
            // approved the exact target file (empty relative path), anchor at its
            // parent directory with the file name as the relative path. Each write
            // is still gated by the per-write diff-approval, so anchoring at the
            // parent doesn't bypass operator review.
            let (anchor, relative) = if rel.as_os_str().is_empty() {
                match (root_abs.parent(), root_abs.file_name()) {
                    (Some(parent), Some(name)) => (parent.to_path_buf(), PathBuf::from(name)),
                    _ => continue,
                }
            } else {
                (root_abs.clone(), rel.to_path_buf())
            };
            let relative_path = normalize_relative_path(&relative)?;
            let absolute_path = anchor.join(&relative_path);
            return Ok(Some(ScopedWorkspacePath {
                principal: principal.to_string(),
                workspace: workspace.to_string(),
                task_id: None,
                execution_id: None,
                scope_root: scope_root.to_path_buf(),
                workspace_root: anchor.clone(),
                relative_path,
                absolute_path,
                apply_root: Some(anchor),
                god_mode_override: None,
            }));
        }
    }
    Ok(None)
}

fn absolutize_lexical(path: &Path) -> Result<PathBuf> {
    let base = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().context("read current dir")?
    };
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };

    let mut normalized = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {},
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(anyhow!("cannot normalize path `{}`", path.display()));
                }
            },
            Component::Normal(part) => normalized.push(part),
        }
    }
    Ok(normalized)
}

pub fn normalize_relative_path(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(anyhow!("path cannot be empty"));
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {},
            Component::ParentDir => {
                return Err(anyhow!(
                    "path `{}` contains `..`; only workspace-relative paths are allowed",
                    path.display()
                ));
            },
            Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!(
                    "path `{}` is not workspace-relative",
                    path.display()
                ));
            },
        }
    }

    if normalized.as_os_str().is_empty() {
        return Err(anyhow!("path cannot resolve to workspace root"));
    }
    Ok(normalized)
}

pub fn read_text_bounded(path: &Path) -> Result<String> {
    let metadata = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if metadata.len() > MAX_FILE_READ_BYTES {
        return Err(anyhow!(
            "file {} is {} bytes; refusing to read (cap {} bytes)",
            path.display(),
            metadata.len(),
            MAX_FILE_READ_BYTES
        ));
    }
    std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
}

/// Why a direct write to `target` in the task workdir is refused: a
/// read-only root of the agents' file sandbox governs it, or it is an
/// existing hard-linked file another of whose names may be read-only.
pub(crate) fn task_workdir_write_refusal(target: &Path) -> Option<String> {
    use crate::magician_v2::execution::native_executors;
    let sandbox = native_executors::agent_file_sandbox()?;
    native_executors::read_only_root_refusal(target, &sandbox)
        .or_else(|| native_executors::hard_link_refusal(target, &sandbox))
}

/// Write `content` to `target`, a path already confirmed inside the running
/// task's workdir. No diff approval: the workdir is the task's scratch space.
pub fn write_task_workdir_file(
    target: &Path,
    file_path: &str,
    content: &str,
    create_dirs: bool,
) -> Value {
    if let Some(refusal) = task_workdir_write_refusal(target) {
        return serde_json::json!({
            "status": "error",
            "reason": format!("Refused `file_path`: {refusal}"),
        });
    }
    let result = (|| -> std::io::Result<bool> {
        use std::io::Write as _;
        let existed = target.exists();
        if create_dirs {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
        }
        if existed {
            std::fs::write(target, content)?;
        } else {
            // `create_new` never follows a symlink at the leaf: one planted
            // after the containment check fails here instead of creating its
            // target somewhere else.
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(target)?
                .write_all(content.as_bytes())?;
        }
        Ok(existed)
    })();
    match result {
        Ok(existed) => serde_json::json!({
            "status": "ok",
            "file_path": file_path,
            "absolute_path": target.to_string_lossy(),
            "artifacts": [{"path": target.to_string_lossy()}],
            "byte_count": content.len(),
            "create_dirs": create_dirs,
            "action": if existed { "rewritten" } else { "created" },
            "applied": true,
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "reason": format!("Could not write `{file_path}`: {err}"),
        }),
    }
}

/// Replace `old_string` in `target`, a file already confirmed inside the
/// running task's workdir, with the same match rules as a staged edit.
pub fn edit_task_workdir_file(
    target: &Path,
    file_path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Value {
    if let Some(refusal) = task_workdir_write_refusal(target) {
        return serde_json::json!({
            "status": "error",
            "reason": format!("Refused `file_path`: {refusal}"),
        });
    }
    let original = match read_text_bounded(target) {
        Ok(text) => text,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "reason": format!("Could not read `{file_path}`: {err}"),
            })
        },
    };
    let occurrences = original.matches(old_string).count();
    if occurrences == 0 {
        return serde_json::json!({
            "status": "error",
            "reason": format!("`old_string` not found in `{file_path}`."),
        });
    }
    if occurrences > 1 && !replace_all {
        return serde_json::json!({
            "status": "error",
            "reason": format!(
                "`old_string` matches {occurrences} times in `{file_path}`. Add surrounding context to make the match unique, or set `replace_all: true`."
            ),
            "occurrences": occurrences,
        });
    }
    let updated = if replace_all {
        original.replace(old_string, new_string)
    } else {
        original.replacen(old_string, new_string, 1)
    };
    match std::fs::write(target, updated) {
        Ok(()) => serde_json::json!({
            "status": "ok",
            "file_path": file_path,
            "absolute_path": target.to_string_lossy(),
            "artifacts": [{"path": target.to_string_lossy()}],
            "replacements": if replace_all { occurrences } else { 1 },
            "applied": true,
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "reason": format!("Could not write `{file_path}`: {err}"),
        }),
    }
}

pub async fn stage_one_edit(
    resources: &Arc<AgentResources>,
    scoped: &ScopedWorkspacePath,
    rationale: impl Into<String>,
    edit: ProposedEdit,
) -> Result<Value> {
    let transaction = stage_edits(resources, scoped, rationale, vec![edit])?;
    finish_staged_edits(resources, scoped, &transaction).await
}

/// God-mode grants authorize the staged revision and apply it through the same
/// snapshot/claim path as ordinary diff review. Broader grants skip later asks.
pub async fn finish_staged_edits(
    resources: &Arc<AgentResources>,
    scoped: &ScopedWorkspacePath,
    txn: &FileEditTransaction,
) -> Result<Value> {
    if !txn.requires_god_mode {
        return Ok(pending_approval_response(txn));
    }
    use crate::magician_v2::execution::file_edit::snapshot::{
        apply_transaction_verified_decision, SnapshotStore,
    };
    use crate::magician_v2::execution::trusted_store::{
        process_authority, StoreKind, TrustedRecordKey,
    };
    use crate::magician_v2::god_mode::{self, GodAction, StagedFileOverride};
    let store = TransactionStore::new(&scoped.scope_root);
    let authority = process_authority().ok_or_else(|| anyhow!("missing file-edit authority"))?;
    let key = TrustedRecordKey {
        kind: StoreKind::Transaction,
        principal: scoped.principal.clone(),
        workspace: scoped.workspace.clone(),
        id: txn.id.as_str().to_string(),
    };
    let result = async {
        let mut permissions = Vec::new();
        for edit in &txn.edits {
            for rel in edit.affected_paths() {
                let path = scoped.workspace_root.join(rel);
                crate::magician_v2::execution::native_executors::validate_staged_write_fences(
                    &path,
                    &session_file_sandbox(resources),
                    txn.staged_by_agent.as_deref(),
                )?;
                let permission = StagedFileOverride::for_current_target(
                    &path,
                    !path.starts_with(
                        resources
                            .artifact_workspace
                            .capability_home_root(&scoped.principal, &scoped.workspace),
                    ),
                )
                .filter(|p| p.permits(&scoped.principal, &scoped.workspace, &path))
                .ok_or_else(|| anyhow!("God-mode file-edit scope changed; re-stage"))?;
                permissions.push(permission);
            }
        }
        let paths = permissions
            .iter()
            .map(|p| p.target().to_path_buf())
            .collect::<Vec<_>>();
        let mut files = serde_json::to_value(txn.diff_approval_files())?;
        // Display absolute destinations alongside the diff, including for a
        // narrow external apply root. The grant uses their real identities.
        if let Some(files) = files.as_array_mut() {
            for (file, edit) in files.iter_mut().zip(&txn.edits) {
                file["path"] = json!(scoped.workspace_root.join(edit.path()));
            }
        }
        god_mode::authorize(GodAction::FileEdit {
            paths: &paths,
            rationale: &txn.rationale,
            files: &files,
        })
        .await
        .map_err(anyhow::Error::msg)?;
        // A mode or symlink change while the card was open cannot redirect the
        // accepted revision, even when the owner chose a lasting grant.
        for (permission, edit) in permissions
            .iter()
            .zip(txn.edits.iter().flat_map(|e| e.affected_paths()))
        {
            if !permission.permits(
                &scoped.principal,
                &scoped.workspace,
                &scoped.workspace_root.join(edit),
            ) {
                return Err(anyhow!("God-mode file-edit authority changed; re-stage"));
            }
        }
        authority.bind_file_override(&key, txn.content_hash(), permissions)?;
        let applied = apply_transaction_verified_decision(
            &store,
            &SnapshotStore::new(&scoped.scope_root),
            &txn.id,
            &scoped.workspace_root,
            txn.content_hash(),
        )?;
        if !applied.transitioned() {
            return Err(anyhow!("file edit already resolved"));
        }
        Ok(json!({"status":"ok", "applied":true, "transaction_id":txn.id.as_str(), "files":files}))
    }
    .await;
    // Never leave a denied/failed God-mode edit available through a second
    // ordinary Apply action. A retry must stage and authorize a fresh revision.
    if result.is_err() {
        let _ = store.reject(&txn.id);
    }
    authority.forget_if(&key, &txn.content_hash());
    result
}

pub fn stage_edits(
    _resources: &Arc<AgentResources>,
    scoped: &ScopedWorkspacePath,
    rationale: impl Into<String>,
    edits: Vec<ProposedEdit>,
) -> Result<FileEditTransaction> {
    let store = TransactionStore::new(&scoped.scope_root);
    let scope = TransactionScope {
        principal: scoped.principal.clone(),
        workspace: scoped.workspace.clone(),
    };
    let mut txn = store.stage(scope, rationale, edits, &mut |path| {
        let rel = normalize_relative_path(path)?;
        read_text_bounded(&scoped.workspace_root.join(rel))
    })?;
    bind_transaction_origin(&mut txn, scoped);
    // Approved out-of-workspace write: record the approved root on the transaction
    // so the diff-approval apply resolves + containment-checks against it (rather
    // than the default scoped workspace root).
    if let Some(root) = &scoped.apply_root {
        txn.apply_root = Some(root.clone());
    }
    if scoped.god_mode_override.is_some() {
        txn.requires_god_mode = true;
        txn.rationale = format!(
            "[GOD MODE: {}] {}",
            scoped.absolute_path.display(),
            txn.rationale,
        );
    }
    // Persist the origin binding even for ordinary in-workspace edits. The
    // canonical HITL responder uses it to prevent a decision for this
    // transaction from finalizing a different execution supplied by a client.
    store.persist(&txn)?;
    // Trusted-store integrity: bind this staged transaction to the in-process
    // authority so the diff-approval apply takes `apply_root` (+ targets) from
    // process memory, NOT from the shell-writable on-disk JSON. Recorded for EVERY
    // staged transaction (not just approved out-of-workspace writes) because the
    // decision site fail-closes on an authority miss — so an ordinary in-workspace
    // write MUST have an entry too, else its own approval would be rejected. The
    // authority is empty after a restart by design (non-terminal records get
    // re-asked, never auto-applied from unverifiable disk).
    //
    // `content_hash` is over the serialized `edits` — the canonical description of
    // the approved effect (each edit carries its target path + new content /
    // content hash). Computed via `FileEditTransaction::content_hash` — the SAME
    // method the diff-approval decide site recomputes on the loaded on-disk record
    // and compares against this entry, so a mismatch (JSON `edits` edited out of
    // band between stage and apply) fails the apply closed. It is a stable,
    // already-materialized representation (`FileEdit: Serialize`) and degrades to
    // an empty-input hash on the (unreachable) serialize error rather than failing
    // the stage.
    if let Some(authority) = crate::magician_v2::execution::trusted_store::process_authority() {
        use crate::magician_v2::execution::trusted_store::{
            AuthorityEntry, StoreKind, TrustedRecordKey,
        };
        let target_paths: Vec<PathBuf> = txn
            .edits
            .iter()
            .map(|edit| edit.path().to_path_buf())
            .collect();
        let key = TrustedRecordKey {
            kind: StoreKind::Transaction,
            principal: scoped.principal.clone(),
            workspace: scoped.workspace.clone(),
            id: txn.id.as_str().to_string(),
        };
        authority.record_authenticated_file_edit_stage(
            key.clone(),
            AuthorityEntry {
                content_hash: txn.content_hash(),
                apply_root: scoped.apply_root.clone(),
                target_paths,
            },
            txn.task_id.clone(),
            txn.execution_id.clone(),
            txn.content_hash(),
        )?;
    } else if scoped.god_mode_override.is_some() {
        return Err(anyhow!(
            "God-mode staged edits require the process trust authority"
        ));
    }
    Ok(txn)
}

fn bind_transaction_origin(txn: &mut FileEditTransaction, scoped: &ScopedWorkspacePath) {
    txn.task_id = scoped.task_id.clone();
    txn.execution_id = scoped.execution_id.clone();
    // The agent the edit is staged for, from runtime identity: the apply
    // judges goal workspace access for it, not for whoever approves.
    txn.staged_by_agent = crate::magician_v2::execution::native_executors::current_acting_agent();
}

pub fn pending_approval_response(transaction: &FileEditTransaction) -> Value {
    let files: Vec<AgenticDiffApprovalFile> = transaction
        .diff_approval_files()
        .into_iter()
        .map(|file| AgenticDiffApprovalFile {
            path: file.path,
            status: file.status,
            additions: file.additions,
            deletions: file.deletions,
            unified_diff: file.unified_diff,
        })
        .collect();
    let stats = transaction.total_stats();
    let input_type = UserInputType::DiffApproval {
        transaction_id: Some(transaction.id.as_str().to_string()),
        proposal_id: None,
        approval_source: Some("transaction".to_string()),
        rationale: transaction.rationale.clone(),
        files: files.clone(),
    };

    json!({
        "status": "pending_approval",
        "paused": true,
        "pause_kind": "diff_approval",
        "pause_question": "Approve staged file edits?",
        "pause_hint": "Review the diff. Apply writes the staged transaction; reject discards it.",
        "pause_input_type": input_type,
        "transaction_id": transaction.id.as_str(),
        "rationale": transaction.rationale.clone(),
        "files": files,
        "stats": {
            "additions": stats.additions,
            "deletions": stats.deletions,
            "file_count": transaction.edits.len(),
        }
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn normalize_relative_rejects_traversal() {
        let err = normalize_relative_path(Path::new("../secret.txt")).unwrap_err();
        assert!(err.to_string().contains(".."));
    }

    #[test]
    fn normalize_relative_strips_curdir() {
        assert_eq!(
            normalize_relative_path(Path::new("./src/./main.rs")).unwrap(),
            PathBuf::from("src/main.rs")
        );
    }

    #[test]
    fn transaction_origin_comes_from_compiled_tool_provenance() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let store = TransactionStore::new(dir.path());
        let mut transaction = store
            .stage(
                TransactionScope {
                    principal: "principal-a".to_string(),
                    workspace: "workspace-a".to_string(),
                },
                "Update config",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("config.txt"),
                    content: "value".to_string(),
                }],
                &mut |_| unreachable!(),
            )
            .expect("transaction staged");
        let scoped = ScopedWorkspacePath {
            principal: "principal-a".to_string(),
            workspace: "workspace-a".to_string(),
            task_id: Some("task-a".to_string()),
            execution_id: Some("exec-a".to_string()),
            scope_root: dir.path().to_path_buf(),
            workspace_root: dir.path().to_path_buf(),
            relative_path: PathBuf::from("config.txt"),
            absolute_path: dir.path().join("config.txt"),
            apply_root: None,
            god_mode_override: None,
        };

        bind_transaction_origin(&mut transaction, &scoped);
        store.persist(&transaction).expect("origin persisted");
        let reloaded = store.load(&transaction.id).expect("transaction reloaded");
        assert_eq!(reloaded.task_id.as_deref(), Some("task-a"));
        assert_eq!(reloaded.execution_id.as_deref(), Some("exec-a"));
    }
}

/// Goal workspaces at staging (`goal_workspace_access`): `write_file` and
/// `edit_file` resolve through `resolve_write_path`, which refuses a target
/// the acting agent may not write, whatever path form reaches it.
#[cfg(all(test, unix))]
mod goal_workspace_tests {
    use super::*;
    use crate::magician_v2::execution::compiled_handlers::{edit_file, write_file};
    use crate::magician_v2::execution::goal_workspace_access as access;
    use crate::magician_v2::execution::native_executors::ACTING_AGENT;

    struct Setup {
        _temp: tempfile::TempDir,
        resources: Arc<AgentResources>,
        _roots: access::TestRootsGuard,
    }

    /// The fixture workspace, reached from the scoped workspace through a
    /// link `goals` → `Workspaces/Magican/Goals/X` and `ws` →
    /// `Workspaces/Magican`.
    fn setup() -> Setup {
        let temp = tempfile::tempdir().unwrap();
        let resources = access::test_resources(temp.path());
        let root = temp
            .path()
            .canonicalize()
            .unwrap()
            .join("notes")
            .join("Workspaces");
        access::write_fixture(&root);
        let home = resources
            .artifact_workspace
            .capability_home_root("owner", "default");
        std::fs::create_dir_all(&home).unwrap();
        std::os::unix::fs::symlink(root.join("Magican/Goals/X"), home.join("goals")).unwrap();
        std::os::unix::fs::symlink(root.join("Magican"), home.join("ws")).unwrap();
        let roots = access::install_test_roots(vec![root]);
        Setup {
            _temp: temp,
            resources,
            _roots: roots,
        }
    }

    fn args(file_path: &str) -> Value {
        json!({
            "__principal": "owner",
            "__workspace": "default",
            "file_path": file_path,
            "content": "rewritten\n",
            "old_string": "needle",
            "new_string": "pin",
        })
    }

    fn refused(value: &Value) -> bool {
        value["status"] == "error"
            && value["reason"]
                .as_str()
                .is_some_and(|reason| reason.starts_with("Refused"))
    }

    #[tokio::test]
    async fn write_file_and_edit_file_refuse_goals_the_agent_may_not_write() {
        let setup = setup();
        let resources = &setup.resources;
        let as_agent = |agent: &'static str, file_path: &'static str, edit: bool| {
            let resources = Arc::clone(resources);
            ACTING_AGENT.scope(Some(agent.to_string()), async move {
                if edit {
                    edit_file::handle(resources, args(file_path)).await.unwrap()
                } else {
                    write_file::handle(resources, args(file_path))
                        .await
                        .unwrap()
                }
            })
        };
        for edit in [false, true] {
            // A non-member: nothing in the workspace.
            assert!(
                refused(&as_agent("ambassador", "goals/x.md", edit).await),
                "edit={edit}"
            );
            // A member: not the user's or ceo's private goal, not the README.
            assert!(
                refused(&as_agent("cmo", "goals/secret.md", edit).await),
                "edit={edit}"
            );
            assert!(
                refused(&as_agent("cmo", "goals/ceo plan.md", edit).await),
                "edit={edit}"
            );
            assert!(
                refused(&as_agent("cmo", "ws/README.md", edit).await),
                "edit={edit}"
            );
        }
        // Its own internal goal stages for approval.
        let staged = as_agent("cmo", "goals/x.md", false).await;
        assert_eq!(staged["status"], "pending_approval", "{staged}");
        // No identity is a non-member.
        let anonymous = write_file::handle(Arc::clone(resources), args("goals/x.md"))
            .await
            .unwrap();
        assert!(refused(&anonymous), "{anonymous}");
    }

    #[tokio::test]
    async fn god_mode_keeps_goal_membership_and_private_goal_fences() {
        let _god = crate::magician_v2::god_mode::test_scope(true);
        let setup = setup();
        let session =
            crate::magician_v2::god_mode::GodSession::for_chat("owner", "default", "goal-fence");
        crate::magician_v2::god_mode::with_session_and_trust(session, None, async {
            for (agent, path) in [
                ("ambassador", "goals/x.md"),
                ("cmo", "goals/secret.md"),
                ("cmo", "ws/README.md"),
            ] {
                let response = ACTING_AGENT
                    .scope(
                        Some(agent.to_string()),
                        write_file::handle(setup.resources.clone(), args(path)),
                    )
                    .await
                    .unwrap();
                assert_eq!(response["status"], "error", "{response}");
                assert!(
                    !response["reason"]
                        .as_str()
                        .unwrap()
                        .contains("approval channel"),
                    "hard fence must reject before asking"
                );
            }
        })
        .await;
    }

    #[test]
    fn a_staged_edit_records_the_agent_it_was_staged_for() {
        let setup = setup();
        let scoped = ACTING_AGENT.sync_scope(Some("cmo".to_string()), || {
            resolve_write_path(&setup.resources, &args("goals/x.md"), "goals/x.md").unwrap()
        });
        let transaction = ACTING_AGENT.sync_scope(Some("cmo".to_string()), || {
            stage_edits(
                &setup.resources,
                &scoped,
                "update the goal",
                vec![ProposedEdit::Modify {
                    path: scoped.relative_path.clone(),
                    new_content: "updated\n".to_string(),
                }],
            )
            .unwrap()
        });
        assert_eq!(transaction.staged_by_agent.as_deref(), Some("cmo"));
        let reloaded = TransactionStore::new(&scoped.scope_root)
            .load(&transaction.id)
            .unwrap();
        assert_eq!(reloaded.staged_by_agent.as_deref(), Some("cmo"));
    }
}

#[cfg(all(test, unix))]
mod god_mode_tests {
    use super::*;
    use crate::magician_v2::execution::compiled_handlers::{edit_file, write_file};
    use crate::magician_v2::execution::file_edit::snapshot::{
        apply_transaction_verified_decision, SnapshotStore,
    };
    use crate::magician_v2::execution::file_edit::transaction::{
        DecisionTransition, TransactionId,
    };
    use crate::magician_v2::execution::goal_workspace_access as access;
    use crate::magician_v2::execution::native_executors::with_agent_file_sandbox_for_test;
    use crate::magician_v2::execution::trusted_store::{
        install_process_authority, process_authority, AuthorityEntry, StoreKind, TrustAuthority,
        TrustedRecordKey,
    };
    use crate::magician_v2::god_mode::{self, GodSession, RunTrust};
    use std::future::Future;

    fn fixture() -> (tempfile::TempDir, Arc<AgentResources>, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut resources = access::test_resources(&root);
        let programs = resources
            .artifact_workspace
            .capability_home_root("owner", "default")
            .join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        let target = programs.join("plan.md");
        std::fs::write(&target, "old plan\n").unwrap();
        Arc::make_mut(&mut resources).file_sandbox = runtime_core::FileSandboxConfig {
            mode: runtime_core::FileSandboxMode::WorkspaceWrite,
            allowed_roots: vec![root.display().to_string()],
            read_only_roots: vec![programs.display().to_string()],
            ..Default::default()
        };
        let _ = install_process_authority(Arc::new(TrustAuthority::new()));
        (temp, resources, target)
    }

    async fn stage(
        resources: Arc<AgentResources>,
        target: &Path,
        edit: bool,
        trust: &str,
    ) -> Value {
        let args = json!({"__principal":"owner", "__workspace":"default"});
        let session = GodSession::for_chat("owner", "default", "staged-file-test");
        god_mode::with_session_and_trust(session, Some(RunTrust::level(Some(trust))), async {
            let scoped =
                match resolve_tool_write_path(&resources, &args, &target.display().to_string()) {
                    Ok(scoped) => scoped,
                    Err(error) => return json!({"status":"error", "reason":error.to_string()}),
                };
            let txn = stage_edits(
                &resources,
                &scoped,
                "test edit",
                vec![ProposedEdit::Modify {
                    path: scoped.relative_path.clone(),
                    new_content: "new plan\n".into(),
                }],
            )
            .unwrap();
            if let Some(permission) = scoped.god_mode_override {
                process_authority()
                    .unwrap()
                    .bind_file_override(&key(&txn), txn.content_hash(), vec![permission])
                    .unwrap();
            }
            let _ = edit;
            pending_approval_response(&txn)
        })
        .await
    }

    fn load(
        resources: &AgentResources,
        response: &Value,
    ) -> (TransactionStore, FileEditTransaction) {
        assert_eq!(response["status"], "pending_approval", "{response}");
        let store =
            TransactionStore::new(resources.artifact_workspace.scope_root("owner", "default"));
        let id = TransactionId::parse(response["transaction_id"].as_str().unwrap()).unwrap();
        let txn = store.load(&id).unwrap();
        (store, txn)
    }

    fn key(txn: &FileEditTransaction) -> TrustedRecordKey {
        TrustedRecordKey {
            kind: StoreKind::Transaction,
            principal: txn.scope.principal.clone(),
            workspace: txn.scope.workspace.clone(),
            id: txn.id.as_str().to_string(),
        }
    }

    fn apply(
        resources: &AgentResources,
        store: &TransactionStore,
        txn: &FileEditTransaction,
    ) -> Result<DecisionTransition<FileEditTransaction>> {
        let snapshots =
            SnapshotStore::new(resources.artifact_workspace.scope_root("owner", "default"));
        with_agent_file_sandbox_for_test(resources.file_sandbox.clone(), || {
            apply_transaction_verified_decision(
                store,
                &snapshots,
                &txn.id,
                txn.apply_root.as_ref().unwrap(),
                txn.content_hash(),
            )
        })
    }

    async fn run_tool(
        resources: Arc<AgentResources>,
        target: &Path,
        edit: bool,
        chat: &str,
    ) -> Value {
        let sandbox = resources.file_sandbox.clone();
        let args = json!({"__principal":"owner", "__workspace":"default", "file_path":target,
            "content":"new plan\n", "old_string":"old plan", "new_string":"new plan"});
        let session = GodSession::for_chat("owner", "default", chat);
        let body = god_mode::with_session_and_trust(
            session,
            Some(RunTrust::level(Some("local"))),
            async {
                if edit {
                    edit_file::handle(resources, args).await
                } else {
                    write_file::handle(resources, args).await
                }
            },
        );
        let mut body = std::pin::pin!(body);
        std::future::poll_fn(|cx| {
            with_agent_file_sandbox_for_test(sandbox.clone(), || body.as_mut().poll(cx))
        })
        .await
        .unwrap()
    }

    async fn owner_answer(
        service: &crate::magician_v2::user_requests::service::UserRequestService,
        decision: &str,
    ) {
        let request = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(request) = service.list_pending().await.into_iter().next() {
                    break request;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("God-mode approval request");
        assert_eq!(request.context["chat_session_id"], "staged-file-test");
        assert!(request.context["files"][0]["unified_diff"]
            .as_str()
            .unwrap()
            .contains("+new plan"));
        for expected in ["allow_once", "session:file", "always:file", "deny"] {
            assert!(
                request.options.iter().any(|option| option.id == expected),
                "missing {expected}: {:?}",
                request.options.iter().map(|o| &o.id).collect::<Vec<_>>()
            );
        }
        assert!(
            service
                .respond(crate::magician_v2::user_requests::service::UserResponse {
                    request_id: request.id,
                    decision: decision.into(),
                    input: None,
                    channel: "web".into(),
                    sensitive: Vec::new(),
                })
                .await
        );
    }

    #[tokio::test]
    async fn god_file_choices_apply_or_deny_and_reuse_only_the_chosen_scope() {
        let _god = god_mode::test_scope(true);
        let _roots = access::install_test_roots(Vec::new());
        for decision in ["allow_once", "session:file", "always:file", "deny"] {
            let (_temp, resources, target) = fixture();
            let service = Arc::new(
                crate::magician_v2::user_requests::service::UserRequestService::new(Arc::new(
                    crate::magician_v2::realtime_events::RuntimeTransportBroadcaster::new(64),
                )),
            );
            god_mode::with_test_approver(service.clone(), async {
                let (response, ()) = tokio::join!(run_tool(resources.clone(), &target, true, "staged-file-test"), owner_answer(&service, decision));
                if decision == "deny" {
                    assert_eq!(response["status"], "error", "{response}");
                    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old plan\n");
                } else {
                    assert_eq!(response["applied"], true, "{response}");
                    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new plan\n");
                }
                std::fs::write(&target, "old plan\n").unwrap();
                if decision.starts_with("session:") || decision.starts_with("always:") {
                    // A matching grant applies the next diff without asking.
                    let response = tokio::time::timeout(std::time::Duration::from_secs(2), run_tool(resources.clone(), &target, false, "staged-file-test")).await.unwrap();
                    assert_eq!(response["applied"], true, "{response}");
                    assert!(service.list_pending().await.is_empty());
                    std::fs::write(&target, "old plan\n").unwrap();
                    if decision.starts_with("always:") {
                        let response = tokio::time::timeout(std::time::Duration::from_secs(2), run_tool(resources.clone(), &target, false, "another-chat")).await.unwrap();
                        assert_eq!(response["applied"], true, "{response}");
                    } else {
                        // A session grant does not cross into another chat.
                        let run = run_tool(resources.clone(), &target, false, "another-chat");
                        let answer = async {
                            loop {
                                if let Some(request) = service.list_pending().await.into_iter().next() {
                                    assert_eq!(request.context["chat_session_id"], "another-chat");
                                    service.respond(crate::magician_v2::user_requests::service::UserResponse { request_id:request.id, decision:"deny".into(), input:None, channel:"web".into(), sensitive:Vec::new() }).await;
                                    break;
                                }
                                tokio::task::yield_now().await;
                            }
                        };
                        let (response, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async { tokio::join!(run, answer) }).await.unwrap();
                        assert_eq!(response["status"], "error");
                    }
                } else {
                    // Once/deny do not leave behind a reusable grant.
                    let (response, ()) = tokio::join!(run_tool(resources.clone(), &target, false, "staged-file-test"), owner_answer(&service, "deny"));
                    assert_eq!(response["status"], "error");
                }
            }).await;
        }
    }

    #[tokio::test]
    async fn ordinary_edits_keep_diff_review_without_god_mode() {
        let _god = god_mode::test_scope(false);
        let _roots = access::install_test_roots(Vec::new());
        let (_temp, mut resources, _target) = fixture();
        Arc::make_mut(&mut resources)
            .file_sandbox
            .read_only_roots
            .clear();
        let target = resources
            .artifact_workspace
            .capability_home_root("owner", "default")
            .join("ordinary.txt");
        std::fs::write(&target, "old plan\n").unwrap();
        let response = run_tool(resources.clone(), &target, false, "staged-file-test").await;
        let (_, txn) = load(&resources, &response);
        assert!(!txn.requires_god_mode);
        assert_eq!(std::fs::read_to_string(target).unwrap(), "old plan\n");
    }

    #[tokio::test]
    async fn god_mode_patch_uses_the_folder_grant_for_every_file() {
        let _god = god_mode::test_scope(true);
        let _roots = access::install_test_roots(Vec::new());
        let (_temp, resources, target) = fixture();
        let second = target.parent().unwrap().join("second.md");
        std::fs::write(&second, "old plan\n").unwrap();
        let session = GodSession::for_chat("owner", "default", "patch-test").unwrap();
        god_mode::grant_for_session_in_test(
            &session,
            &[format!("dir={}", target.parent().unwrap().display())],
        );
        let patch = String::from("--- a/programs/plan.md\n+++ b/programs/plan.md\n@@ -1 +1 @@\n-old plan\n+new plan\n--- a/programs/second.md\n+++ b/programs/second.md\n@@ -1 +1 @@\n-old plan\n+new plan\n");
        let body = god_mode::with_session_and_trust(
            Some(session),
            Some(RunTrust::level(Some("local"))),
            crate::magician_v2::execution::compiled_handlers::apply_patch::handle(
                resources.clone(),
                json!({"__principal":"owner", "__workspace":"default", "patch":patch}),
            ),
        );
        let mut body = std::pin::pin!(body);
        let response = std::future::poll_fn(|cx| {
            with_agent_file_sandbox_for_test(resources.file_sandbox.clone(), || {
                body.as_mut().poll(cx)
            })
        })
        .await
        .unwrap();
        assert_eq!(response["applied"], true, "{response}");
        assert_eq!(std::fs::read_to_string(target).unwrap(), "new plan\n");
        assert_eq!(std::fs::read_to_string(second).unwrap(), "new plan\n");
    }

    #[tokio::test]
    async fn god_mode_staged_revision_applies_once() {
        let _god = god_mode::test_scope(true);
        let _roots = access::install_test_roots(Vec::new());
        for edit in [false, true] {
            let (_temp, resources, target) = fixture();
            let response = stage(resources.clone(), &target, edit, "local").await;
            let (store, txn) = load(&resources, &response);
            assert!(txn.requires_god_mode);
            assert!(txn.rationale.contains("GOD MODE:"));
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "old plan\n");
            assert!(apply(&resources, &store, &txn).unwrap().transitioned());
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "new plan\n");
            assert!(!apply(&resources, &store, &txn).unwrap().transitioned());
        }
    }

    #[tokio::test]
    async fn god_mode_off_or_reviewed_trust_keeps_read_only_file_tools_denied() {
        let _god = god_mode::test_scope(false);
        let _roots = access::install_test_roots(Vec::new());
        let (_temp, resources, target) = fixture();
        for edit in [false, true] {
            let response = stage(resources.clone(), &target, edit, "local").await;
            assert_eq!(response["status"], "error");
            assert!(response["reason"].as_str().unwrap().contains("read-only"));
        }
        god_mode::configure(&crate::config::GodModeSettings {
            enabled: true,
            ..Default::default()
        });
        for edit in [false, true] {
            let response = stage(resources.clone(), &target, edit, "reviewed").await;
            assert_eq!(response["status"], "error");
        }
        assert_eq!(std::fs::read_to_string(target).unwrap(), "old plan\n");
    }

    #[tokio::test]
    async fn god_mode_staged_edit_refuses_revocation_even_after_reenable() {
        let _god = god_mode::test_scope(true);
        let _roots = access::install_test_roots(Vec::new());
        let (_temp, resources, target) = fixture();
        let response = stage(resources.clone(), &target, false, "local").await;
        let (store, txn) = load(&resources, &response);
        god_mode::set_chat_override("staged-file-test", Some(false));
        assert!(apply(&resources, &store, &txn)
            .unwrap_err()
            .to_string()
            .contains("expired"));
        god_mode::set_chat_override("staged-file-test", None);
        assert!(apply(&resources, &store, &txn).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "old plan\n");
    }

    #[tokio::test]
    async fn god_mode_override_cannot_authorize_forged_or_recovered_transactions() {
        let _god = god_mode::test_scope(true);
        let _roots = access::install_test_roots(Vec::new());
        let (_temp, resources, target) = fixture();
        let response = stage(resources.clone(), &target, true, "local").await;
        let (store, txn) = load(&resources, &response);
        let mut forged = txn.clone();
        forged.rationale.push_str(" forged");
        store.persist(&forged).unwrap();
        assert!(apply(&resources, &store, &forged).is_err());
        store.persist(&txn).unwrap();
        // A restart can recover reviewable JSON, but no God-mode permission.
        let authority = process_authority().unwrap();
        authority.record_stage(
            key(&txn),
            AuthorityEntry {
                content_hash: txn.content_hash(),
                apply_root: txn.apply_root.clone(),
                target_paths: txn.edits.iter().map(|e| e.path().to_path_buf()).collect(),
            },
        );
        assert!(apply(&resources, &store, &txn).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "old plan\n");
    }

    #[tokio::test]
    async fn god_mode_override_refuses_a_redirected_parent_symlink_on_apply() {
        let _god = god_mode::test_scope(true);
        let _roots = access::install_test_roots(Vec::new());
        let (temp, resources, target) = fixture();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(target.parent().unwrap(), &alias).unwrap();
        let response = stage(resources.clone(), &alias.join("plan.md"), false, "local").await;
        let (store, txn) = load(&resources, &response);
        let other = temp.path().join("other");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(other.join("plan.md"), "other plan\n").unwrap();
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&other, &alias).unwrap();
        assert!(apply(&resources, &store, &txn).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "old plan\n");
        assert_eq!(
            std::fs::read_to_string(other.join("plan.md")).unwrap(),
            "other plan\n"
        );
    }
}

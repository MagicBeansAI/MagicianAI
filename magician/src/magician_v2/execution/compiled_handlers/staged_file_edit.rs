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
        match input_abs.strip_prefix(&root_abs) {
            Ok(rel) => normalize_relative_path(rel)?,
            Err(_) => {
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
    })
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

pub fn stage_one_edit(
    resources: &Arc<AgentResources>,
    scoped: &ScopedWorkspacePath,
    rationale: impl Into<String>,
    edit: ProposedEdit,
) -> Result<Value> {
    let transaction = stage_edits(resources, scoped, rationale, vec![edit])?;
    Ok(pending_approval_response(&transaction))
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
        let _ = authority.record_authenticated_file_edit_stage(
            TrustedRecordKey {
                kind: StoreKind::Transaction,
                principal: scoped.principal.clone(),
                workspace: scoped.workspace.clone(),
                id: txn.id.as_str().to_string(),
            },
            AuthorityEntry {
                content_hash: txn.content_hash(),
                apply_root: scoped.apply_root.clone(),
                target_paths,
            },
            txn.task_id.clone(),
            txn.execution_id.clone(),
            txn.content_hash(),
        );
    }
    Ok(txn)
}

fn bind_transaction_origin(txn: &mut FileEditTransaction, scoped: &ScopedWorkspacePath) {
    txn.task_id = scoped.task_id.clone();
    txn.execution_id = scoped.execution_id.clone();
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
        };

        bind_transaction_origin(&mut transaction, &scoped);
        store.persist(&transaction).expect("origin persisted");
        let reloaded = store.load(&transaction.id).expect("transaction reloaded");
        assert_eq!(reloaded.task_id.as_deref(), Some("task-a"));
        assert_eq!(reloaded.execution_id.as_deref(), Some("exec-a"));
    }
}

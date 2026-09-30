//! `run_project_checks` — let a coding agent run the project's own
//! build/test/lint/typecheck in its loop (the self-verify half of M1 Autopilot).
//!
//! Wraps the S1 check detection (`dev_server::detect_check_commands`) + a
//! one-shot runner in the per-repo cache-warm shadow (same shadow the run edits,
//! so it reflects just-applied changes + reuses warm `node_modules`). Mirrors
//! the UI's `vibedev_api::run_vibedev_check_handler`, exposed as an
//! agent-callable tool so an unattended run can verify green and fix failures.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::shared::{require_scope_str, scope_arg_str};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::coding_engine::{
    coding_shadow_root, resolve_coding_repo_binding, sync_persistent_workspace, ShadowPatchOptions,
};
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::media_seam::{detect_check_commands, CheckCommand};

const CHECK_TIMEOUT: Duration = Duration::from_secs(240);
const OUTPUT_TAIL_CHARS: usize = 6_000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProposalVerificationContext {
    proposal_id: String,
    proposal_status: String,
    real_working_dir: std::path::PathBuf,
    review_open: bool,
    terminal_success: bool,
}

fn authoritative_proposal_verification_context(
    scope_root: &std::path::Path,
    task_id: Option<&str>,
    execution_id: Option<&str>,
) -> Option<ProposalVerificationContext> {
    use crate::magician_v2::execution::file_edit::proposal::{
        CodeChangeProposalStatus, CodeChangeProposalStore,
    };

    let task_id = task_id.filter(|value| !value.is_empty())?;
    let mut proposals = CodeChangeProposalStore::new(scope_root).list_for_task(task_id);
    proposals.retain(|proposal| {
        proposal.apply_root.is_some()
            && matches!(
                proposal.status,
                CodeChangeProposalStatus::Pending
                    | CodeChangeProposalStatus::Applied
                    | CodeChangeProposalStatus::PartiallyApplied
            )
    });
    proposals.sort_by(|left, right| right.created_at.cmp(&left.created_at));

    let proposal = if let Some(execution_id) = execution_id.filter(|value| !value.is_empty()) {
        proposals
            .iter()
            .find(|proposal| proposal.execution_id.as_deref() == Some(execution_id))
            .cloned()
    } else {
        let unique_roots = proposals
            .iter()
            .filter_map(|proposal| proposal.apply_root.as_ref())
            .map(|path| path.display().to_string())
            .collect::<std::collections::BTreeSet<_>>();
        if unique_roots.len() == 1 {
            proposals.into_iter().next()
        } else {
            None
        }
    }?;

    Some(ProposalVerificationContext {
        proposal_id: proposal.id.to_string(),
        proposal_status: format!("{:?}", proposal.status).to_lowercase(),
        real_working_dir: proposal
            .apply_root
            .expect("retained proposals always have apply_root"),
        review_open: matches!(proposal.status, CodeChangeProposalStatus::Pending),
        // Derive from status (matching run_coding_task.rs): only an APPLIED proposal is
        // terminally successful. A Pending (review-open) or PartiallyApplied proposal is
        // NOT — otherwise this tool would report `terminal_success: true` for an unreviewed
        // diff, contradicting `blocked_on_review` in the same payload.
        terminal_success: matches!(proposal.status, CodeChangeProposalStatus::Applied),
    })
}

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "run_project_checks")?;
    let workspace = require_scope_str(&args, "__workspace", "run_project_checks")?;
    let repo_path = args
        .get("repo_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let explicit_real_working_dir = args
        .get("real_working_dir")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let requested_kind = args
        .get("kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let scope_root = resources
        .artifact_workspace
        .scope_root(&principal, &workspace);
    let workspace_root = resources
        .artifact_workspace
        .capability_home_root(&principal, &workspace);

    let proposal_context = authoritative_proposal_verification_context(
        &scope_root,
        scope_arg_str(&args, "__task_id").as_deref(),
        scope_arg_str(&args, "__execution_id").as_deref(),
    );
    if let Some(proposal) = proposal_context
        .as_ref()
        .filter(|proposal| proposal.review_open)
    {
        return Ok(json!({
            "status": "ok",
            "all_ok": true,
            "results": [],
            "blocked_on_review": true,
            "terminal_success": proposal.terminal_success,
            "proposal_id": proposal.proposal_id.clone(),
            "proposal_status": proposal.proposal_status.clone(),
            "real_working_dir": proposal.real_working_dir.display().to_string(),
            "note": "Verification skipped: the latest code review / diff approval is still open. Resolve it before running post-change checks.",
        }));
    }

    let authoritative_repo_path = explicit_real_working_dir
        .clone()
        .or_else(|| {
            proposal_context
                .as_ref()
                .map(|proposal| proposal.real_working_dir.display().to_string())
        })
        .or_else(|| repo_path.clone());
    let binding =
        match resolve_coding_repo_binding(&workspace_root, authoritative_repo_path.as_deref()) {
            Ok(binding) => binding,
            Err(message) => {
                return Ok(json!({ "status": "error", "reason": message }));
            },
        };
    // Admission lock (§13.3 #3 / #12): hold the per-repo shadow lock across the shadow
    // prepare + the check run, so checks never read a shadow that a concurrent same-repo
    // coding run is mid-sync/apply on. Distinct repos (distinct keys) are unaffected.
    let _shadow_guard = crate::magician_v2::execution::coding_engine::shadow_admission_lock(
        &crate::magician_v2::execution::coding_engine::persistent_shadow_key(&binding.real_path),
    )
    .await
    .lock_owned()
    .await;
    let shadow_root = coding_shadow_root(&scope_root, &binding.real_path);
    if !shadow_root.exists() {
        if let Some(parent) = shadow_root.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(error) = sync_persistent_workspace(
            &binding.real_path,
            &shadow_root,
            &ShadowPatchOptions::default(),
        ) {
            return Ok(json!({
                "status": "error",
                "reason": format!("could not prepare the shadow workspace: {error:#}"),
            }));
        }
    }

    let all = detect_check_commands(&shadow_root);
    if all.is_empty() {
        return Ok(json!({
            "status": "ok",
            "all_ok": true,
            "results": [],
            "real_working_dir": binding.real_path.display().to_string(),
            "note": "no checks detected for this project (no package.json scripts / Cargo / pytest).",
        }));
    }
    let selected: Vec<CheckCommand> = match requested_kind {
        None | Some("all") => all,
        Some(kind) => all
            .into_iter()
            .filter(|command| command.kind == kind)
            .collect(),
    };
    if selected.is_empty() {
        return Ok(json!({
            "status": "error",
            "reason": format!("no `{}` check for this project", requested_kind.unwrap_or("")),
        }));
    }

    let mut results = Vec::with_capacity(selected.len());
    let mut all_ok = true;
    for command in &selected {
        let (ok, exit_code, timed_out, output_tail) = run_one(&shadow_root, command).await;
        if !ok {
            all_ok = false;
        }
        results.push(json!({
            "kind": command.kind,
            "command": command.display,
            "ok": ok,
            "exit_code": exit_code,
            "timed_out": timed_out,
            "output_tail": output_tail,
        }));
    }
    // Major checkpoint (docs/archive/plans/2026-06-18-major-checkpoints.md): when the project checks
    // pass, mint a checkpoint for the most-recent applied-but-not-yet-checkpointed change — a
    // known-good, rewindable state. Idempotent (re-running checks with no new apply mints
    // nothing). Best-effort: a checkpoint failure never fails the checks result.
    let mut out = json!({
        "status": "ok",
        "all_ok": all_ok,
        "results": results,
        "real_working_dir": binding.real_path.display().to_string(),
    });
    if let Some(proposal) = proposal_context.as_ref() {
        out["proposal_id"] = json!(proposal.proposal_id.clone());
        out["proposal_status"] = json!(proposal.proposal_status.clone());
        out["terminal_success"] = json!(proposal.terminal_success);
    }
    if all_ok {
        if let Some(task_id) = scope_arg_str(&args, "__task_id").filter(|s| !s.is_empty()) {
            let execution_id = scope_arg_str(&args, "__execution_id").filter(|s| !s.is_empty());
            if let Some(cp) =
                mint_checkpoint_on_green(&scope_root, &binding.real_path, &task_id, execution_id)
            {
                out["checkpoint"] = json!({ "id": cp.id, "name": cp.name, "git_sha": cp.git_sha });
            }
        }
    }
    Ok(out)
}

/// Mint a major checkpoint for the newest applied-but-not-yet-checkpointed proposal of a RUN
/// (found by its task id). Used by the agent `run_project_checks` path, which knows the run's
/// task id from its args. Returns the minted checkpoint, or None when there is nothing new.
pub fn mint_checkpoint_on_green(
    scope_root: &std::path::Path,
    real_path: &std::path::Path,
    task_id: &str,
    execution_id: Option<String>,
) -> Option<crate::magician_v2::execution::file_edit::checkpoint::Checkpoint> {
    use crate::magician_v2::execution::file_edit::proposal::CodeChangeProposalStore;
    if task_id.is_empty() {
        return None;
    }
    let proposal = newest_uncheckpointed_applied(
        scope_root,
        CodeChangeProposalStore::new(scope_root).list_for_task(task_id),
    )?;
    build_major_checkpoint(
        scope_root,
        real_path,
        &proposal,
        task_id.to_string(),
        execution_id,
    )
}

/// Mint a major checkpoint for the newest applied-but-not-yet-checkpointed proposal of a
/// PROJECT REPO (matched by the proposal's `apply_root`), stamped with the PROPOSAL's OWN
/// task id. Used by the project-scoped cockpit `/check` endpoint, which has no run task id of
/// its own — so checkpoints behave identically no matter how the run was started (cockpit OR
/// raw task API), with NO dependence on the mutable `project.active_root_task_id`.
pub fn mint_checkpoint_for_repo(
    scope_root: &std::path::Path,
    real_path: &std::path::Path,
) -> Option<crate::magician_v2::execution::file_edit::checkpoint::Checkpoint> {
    use crate::magician_v2::execution::file_edit::proposal::CodeChangeProposalStore;
    let proposal = newest_uncheckpointed_applied(
        scope_root,
        CodeChangeProposalStore::new(scope_root).list_for_apply_root(real_path),
    )?;
    // Surface under the run's task (what the cockpit rail lists by) — taken from the proposal
    // itself. A proposal with no task id can't surface, so skip it.
    let task_id = proposal.task_id.clone().filter(|id| !id.is_empty())?;
    let execution_id = proposal.execution_id.clone();
    build_major_checkpoint(scope_root, real_path, &proposal, task_id, execution_id)
}

/// Pick the newest Applied/PartiallyApplied proposal from `candidates` that is not already
/// checkpointed. Idempotent: re-running checks with no new apply yields None.
fn newest_uncheckpointed_applied(
    scope_root: &std::path::Path,
    candidates: Vec<crate::magician_v2::execution::file_edit::proposal::CodeChangeProposal>,
) -> Option<crate::magician_v2::execution::file_edit::proposal::CodeChangeProposal> {
    use crate::magician_v2::execution::file_edit::checkpoint::CheckpointStore;
    use crate::magician_v2::execution::file_edit::proposal::CodeChangeProposalStatus;
    let checkpoints = CheckpointStore::new(scope_root);
    let mut applied: Vec<_> = candidates
        .into_iter()
        .filter(|p| {
            matches!(
                p.status,
                CodeChangeProposalStatus::Applied | CodeChangeProposalStatus::PartiallyApplied
            )
        })
        .filter(|p| !checkpoints.exists(p.id.as_str()))
        .collect();
    applied.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    applied.into_iter().next()
}

/// Build + persist a major checkpoint from `proposal`, capturing the real tree's git side-ref
/// as the rewind anchor. `task_id` is the run the checkpoint surfaces under.
fn build_major_checkpoint(
    scope_root: &std::path::Path,
    real_path: &std::path::Path,
    proposal: &crate::magician_v2::execution::file_edit::proposal::CodeChangeProposal,
    task_id: String,
    execution_id: Option<String>,
) -> Option<crate::magician_v2::execution::file_edit::checkpoint::Checkpoint> {
    use crate::magician_v2::execution::file_edit::checkpoint::{
        Checkpoint, CheckpointKind, CheckpointStore,
    };
    let checkpoints = CheckpointStore::new(scope_root);
    let proposal_id = proposal.id.to_string();
    let name = if proposal.summary.trim().is_empty() {
        match proposal.applied_paths.first() {
            Some(first) => {
                let extra = proposal.applied_paths.len().saturating_sub(1);
                if extra > 0 {
                    format!("{} (+{extra} files)", first.display())
                } else {
                    first.display().to_string()
                }
            },
            None => "applied change".to_string(),
        }
    } else {
        proposal.summary.clone()
    };
    let git_sha = git_side_ref_snapshot(real_path, &proposal_id, &name);
    if git_sha.is_none() {
        tracing::warn!(
            task_id = %task_id,
            checkpoint = %proposal_id,
            real_path = %real_path.display(),
            "checkpoint git side-ref snapshot returned None (no rewind anchor); minting without git_sha"
        );
    }

    let (engine, native_session_id, pi_session_id) =
        checkpoint_session_from_proposal(scope_root, proposal);
    let checkpoint = Checkpoint {
        id: proposal_id.clone(),
        name,
        kind: CheckpointKind::Major,
        proposal_id: Some(proposal_id.clone()),
        git_sha,
        snapshot_id: proposal.snapshot_id.clone(),
        engine,
        native_session_id,
        pi_session_id,
        applied_files: proposal.applied_paths.clone(),
        repo_path: Some(real_path.to_path_buf()),
        task_id,
        execution_id,
        created_at: chrono::Utc::now(),
    };
    if let Err(error) = checkpoints.create(&checkpoint) {
        tracing::warn!(
            task_id = %checkpoint.task_id,
            checkpoint = %proposal_id,
            error = %error,
            "checkpoint persist failed (checks still passed)"
        );
        return None;
    }
    tracing::info!(
        task_id = %checkpoint.task_id,
        checkpoint = %checkpoint.id,
        git_sha = ?checkpoint.git_sha,
        "minted major checkpoint on checks-pass"
    );
    Some(checkpoint)
}

fn checkpoint_session_from_proposal(
    scope_root: &std::path::Path,
    proposal: &crate::magician_v2::execution::file_edit::proposal::CodeChangeProposal,
) -> (Option<String>, Option<String>, Option<String>) {
    if let Some(continuation) = continuation_for_proposal(scope_root, proposal) {
        let engine = crate::magician_v2::execution::coding_engine::selection::engine_str(
            continuation.engine,
        )
        .to_string();
        let native = continuation.native_session_id;
        let pi = matches!(
            continuation.engine,
            crate::magician_v2::execution::coding_engine::CodingEngineKind::Pi
        )
        .then(|| native.clone());
        return (Some(engine), Some(native), pi);
    }
    magician_owned_or_legacy_session(&proposal.source_session_id)
}

fn continuation_for_proposal(
    scope_root: &std::path::Path,
    proposal: &crate::magician_v2::execution::file_edit::proposal::CodeChangeProposal,
) -> Option<crate::magician_v2::execution::coding_engine::CodingContinuationRef> {
    use crate::magician_v2::execution::coding_engine::ledger::{
        is_safe_path_segment, latest_ledger_continuation,
    };
    let task_id = proposal
        .task_id
        .as_deref()
        .filter(|id| is_safe_path_segment(id))?;
    let execution_id = proposal
        .execution_id
        .as_deref()
        .filter(|id| is_safe_path_segment(id))?;
    for root in ["tasks", "internal_tasks"] {
        let dir = scope_root
            .join(root)
            .join(task_id)
            .join("executions")
            .join(execution_id);
        if let Some(found) = latest_ledger_continuation(&dir) {
            return Some(found);
        }
    }
    None
}

fn magician_owned_or_legacy_session(
    source: &str,
) -> (Option<String>, Option<String>, Option<String>) {
    let source = source.trim();
    if source.is_empty() {
        return (None, None, None);
    }
    if source.starts_with("grok-") {
        return (Some("grok_acp".to_string()), None, None);
    }
    if source.starts_with("codex-") {
        return (Some("codex_app_server".to_string()), None, None);
    }
    if source.starts_with("claude-") {
        return (Some("claude_code".to_string()), None, None);
    }
    if source.starts_with("agy-") {
        return (Some("agy_cli".to_string()), None, None);
    }
    (
        Some("pi".to_string()),
        Some(source.to_string()),
        Some(source.to_string()),
    )
}

/// Capture the REAL working tree (tracked + untracked) into a git commit reachable ONLY via a
/// side ref (`refs/vibedev/checkpoints/<id>`) — never the user's branch, index, or working
/// files — so history stays clean but the commit object exists for a true `git checkout`
/// rewind. A throwaway `GIT_INDEX_FILE` keeps the real index/staging untouched. Returns the
/// commit sha, or None when the path isn't a git work tree / git is unavailable / nothing to
/// capture. Non-mutating by construction: only creates objects + one ref.
fn git_side_ref_snapshot(
    real_path: &std::path::Path,
    checkpoint_id: &str,
    message: &str,
) -> Option<String> {
    use std::process::Command;

    let safe_id: String = checkpoint_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let git = |index: Option<&std::path::Path>, a: &[&str]| -> Option<std::process::Output> {
        let mut command = Command::new("git");
        command.arg("-C").arg(real_path);
        if let Some(idx) = index {
            command.env("GIT_INDEX_FILE", idx);
        }
        command.args(a).output().ok()
    };

    if !git(None, &["rev-parse", "--is-inside-work-tree"])
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return None;
    }

    let tmp_index = std::env::temp_dir().join(format!(
        "vibedev-ckpt-index-{}-{}",
        std::process::id(),
        safe_id
    ));
    let _ = std::fs::remove_file(&tmp_index);
    // Seed the throwaway index from HEAD so deletions are captured; ignore failure on a fresh repo.
    let _ = git(Some(&tmp_index), &["read-tree", "HEAD"]);
    let added = git(Some(&tmp_index), &["add", "-A"])
        .map(|o| o.status.success())
        .unwrap_or(false);
    let tree = if added {
        git(Some(&tmp_index), &["write-tree"])
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        None
    };
    let _ = std::fs::remove_file(&tmp_index);
    let tree = tree?;

    let head = git(None, &["rev-parse", "HEAD"])
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let message = format!("vibedev checkpoint: {message}");
    let mut commit_args: Vec<&str> = vec!["commit-tree", &tree, "-m", &message];
    if let Some(head) = head.as_deref() {
        commit_args.push("-p");
        commit_args.push(head);
    }
    let commit = git(None, &commit_args)
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;

    let refname = format!("refs/vibedev/checkpoints/{safe_id}");
    let _ = git(None, &["update-ref", &refname, &commit]);
    Some(commit)
}

async fn run_one(
    working_dir: &std::path::Path,
    command: &CheckCommand,
) -> (bool, Option<i32>, bool, String) {
    // Sandboxed (repo read-only) when run inside a coding context — the check
    // runner is a coding tool invoked by a coding agent, so it inherits the
    // task-local coding flag; identical to `Command::new(program).args(args)`
    // for any non-coding caller. See `coding_engine::os_sandbox_command`.
    let sandbox_program = std::ffi::OsString::from(command.program.as_str());
    let sandbox_args: Vec<std::ffi::OsString> = command
        .args
        .iter()
        .map(|a| std::ffi::OsString::from(a.as_str()))
        .collect();
    let mut cmd = crate::magician_v2::execution::coding_engine::os_sandbox_command(
        sandbox_program.as_os_str(),
        &sandbox_args,
    );
    cmd.current_dir(working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("CI", "true")
        .env("NO_COLOR", "1")
        .env("FORCE_COLOR", "0");
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            return (
                false,
                None,
                false,
                format!("failed to spawn `{}`: {error}", command.display),
            );
        },
    };
    match tokio::time::timeout(CHECK_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let mut combined = String::new();
            combined.push_str(&String::from_utf8_lossy(&output.stdout));
            if !output.stderr.is_empty() {
                combined.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            (
                output.status.success(),
                output.status.code(),
                false,
                tail_chars(&combined, OUTPUT_TAIL_CHARS),
            )
        },
        Ok(Err(error)) => (
            false,
            None,
            false,
            format!("error running `{}`: {error}", command.display),
        ),
        Err(_) => (
            false,
            None,
            true,
            format!(
                "`{}` timed out after {}s",
                command.display,
                CHECK_TIMEOUT.as_secs()
            ),
        ),
    }
}

fn tail_chars(value: &str, max: usize) -> String {
    let count = value.chars().count();
    if count <= max {
        return value.to_string();
    }
    let tail: String = value.chars().skip(count - max).collect();
    format!("…(truncated {} chars)\n{tail}", count - max)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::file_edit::proposal::CodeChangeProposalStore;
    use crate::magician_v2::execution::file_edit::transaction::TransactionScope;
    use tempfile::TempDir;

    fn sample_patch() -> &'static str {
        "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n"
    }

    #[test]
    fn authoritative_proposal_verification_context_prefers_matching_execution() {
        let tempdir = TempDir::new().expect("tempdir");
        let store = CodeChangeProposalStore::new(tempdir.path());
        store
            .stage_patch_with_apply_root(
                TransactionScope {
                    principal: "user".to_string(),
                    workspace: "workspace".to_string(),
                },
                "first",
                sample_patch(),
                "session-1",
                Vec::new(),
                Some(tempdir.path().join("repo-a")),
                Some("task-1".to_string()),
                Some("exec-a".to_string()),
            )
            .expect("first proposal should stage");
        let second = store
            .stage_patch_with_apply_root(
                TransactionScope {
                    principal: "user".to_string(),
                    workspace: "workspace".to_string(),
                },
                "second",
                sample_patch(),
                "session-2",
                Vec::new(),
                Some(tempdir.path().join("repo-b")),
                Some("task-1".to_string()),
                Some("exec-b".to_string()),
            )
            .expect("second proposal should stage");

        let context = authoritative_proposal_verification_context(
            tempdir.path(),
            Some("task-1"),
            Some("exec-b"),
        )
        .expect("matching execution proposal should resolve");

        assert_eq!(context.proposal_id, second.id.to_string());
        assert_eq!(context.real_working_dir, tempdir.path().join("repo-b"));
        assert!(context.review_open);
        // A Pending / review-open proposal is NOT terminally successful.
        assert!(!context.terminal_success);
    }

    #[test]
    fn authoritative_proposal_verification_context_marks_applied_as_ready_for_checks() {
        let tempdir = TempDir::new().expect("tempdir");
        let store = CodeChangeProposalStore::new(tempdir.path());
        let proposal = store
            .stage_patch_with_apply_root(
                TransactionScope {
                    principal: "user".to_string(),
                    workspace: "workspace".to_string(),
                },
                "apply me",
                sample_patch(),
                "session-1",
                Vec::new(),
                Some(tempdir.path().join("repo")),
                Some("task-1".to_string()),
                Some("exec-1".to_string()),
            )
            .expect("proposal should stage");
        store
            .mark_applied(&proposal.id, None)
            .expect("proposal should mark applied");

        let context = authoritative_proposal_verification_context(
            tempdir.path(),
            Some("task-1"),
            Some("exec-1"),
        )
        .expect("applied proposal should resolve");

        assert_eq!(context.proposal_status, "applied");
        assert!(!context.review_open);
        assert!(context.terminal_success);
    }

    #[test]
    fn grok_checkpoint_stores_acp_session_from_continuation_not_magician_id() {
        use crate::magician_v2::execution::coding_engine::ledger::{
            attach_invocation_continuation, prepare_coding_invocation,
        };
        use crate::magician_v2::execution::coding_engine::selection::{
            CodingSelectionSource, ResolvedCodingEngineSelection, SELECTION_CONTRACT_REVISION,
        };
        use crate::magician_v2::execution::coding_engine::{
            CodingContinuationRef, CodingEngineKind,
        };

        let tempdir = TempDir::new().expect("tempdir");
        let store = CodeChangeProposalStore::new(tempdir.path());
        let proposal = store
            .stage_patch_with_apply_root(
                TransactionScope {
                    principal: "user".to_string(),
                    workspace: "workspace".to_string(),
                },
                "grok apply",
                sample_patch(),
                "grok-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                Vec::new(),
                Some(tempdir.path().join("repo")),
                Some("task-1".to_string()),
                Some("exec-1".to_string()),
            )
            .expect("stage");
        let loaded = store.mark_applied(&proposal.id, None).expect("applied");

        let execution_dir = tempdir
            .path()
            .join("tasks")
            .join("task-1")
            .join("executions")
            .join("exec-1");
        std::fs::create_dir_all(&execution_dir).expect("exec dir");
        let selection = ResolvedCodingEngineSelection {
            profile_id: "grok-default".to_string(),
            engine: CodingEngineKind::GrokAcp,
            model: None,
            reasoning_effort: None,
            readiness_revision: None,
            readiness_receipt_digest: None,
            adapter_revision: SELECTION_CONTRACT_REVISION.to_string(),
            constraint_digest: "digest".to_string(),
            selection_source: CodingSelectionSource::Fixed,
        };
        let entry = prepare_coding_invocation(
            &execution_dir,
            "exec-1",
            None,
            "digest",
            selection,
            "prompt",
            ".",
            8,
        )
        .expect("prepare");
        attach_invocation_continuation(
            &execution_dir,
            &entry.invocation_id,
            CodingContinuationRef::for_grok_session(
                "sess-acp",
                tempdir.path(),
                tempdir.path(),
                Some("task-1"),
            ),
            None,
        )
        .expect("attach");

        let repo = tempdir.path().join("repo");
        let checkpoint = build_major_checkpoint(
            tempdir.path(),
            &repo,
            &loaded,
            "task-1".to_string(),
            Some("exec-1".to_string()),
        )
        .expect("checkpoint");
        assert_eq!(checkpoint.engine.as_deref(), Some("grok_acp"));
        assert_eq!(checkpoint.native_session_id.as_deref(), Some("sess-acp"));
        assert!(checkpoint.pi_session_id.is_none());
        assert_ne!(
            checkpoint.native_session_id.as_deref(),
            Some(loaded.source_session_id.as_str())
        );
    }

    #[test]
    fn grok_magician_session_id_is_not_queued_as_native_without_continuation() {
        assert_eq!(
            magician_owned_or_legacy_session("grok-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
            (Some("grok_acp".to_string()), None, None)
        );
        assert_eq!(
            magician_owned_or_legacy_session("pi-session-1"),
            (
                Some("pi".to_string()),
                Some("pi-session-1".to_string()),
                Some("pi-session-1".to_string())
            )
        );
        assert_eq!(
            magician_owned_or_legacy_session("claude-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
            (Some("claude_code".to_string()), None, None)
        );
        assert_eq!(
            magician_owned_or_legacy_session("agy-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
            (Some("agy_cli".to_string()), None, None)
        );
    }
}

//! `apply_code_proposal` — let a coding agent apply its OWN staged
//! `CodeChangeProposal` to the working tree (the keystone of M1 Autopilot).
//!
//! Why this exists: `run_coding_task` stages a proposal in a per-repo shadow and
//! re-syncs that shadow **from the real working tree at the start of every
//! turn** (`sync_persistent_workspace`). So a proposal that is never applied to
//! the real tree is wiped on the next turn — meaning an unattended run could
//! never accumulate progress. Applying after each turn writes the change to the
//! real tree, so the next turn's resync carries it forward → iteration
//! genuinely accumulates. Apply was previously UI-only (the `diff_approval` web
//! handler); this exposes the same `proposal::apply_proposal` path as an
//! agent-callable tool, scoped + idempotent.

use std::sync::Arc;

use serde_json::{json, Value};

use super::shared::require_scope_str;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::execution::file_edit::proposal::{
    apply_proposal, apply_proposal_selected_paths, CodeChangeProposalId, CodeChangeProposalStatus,
    CodeChangeProposalStore,
};
use crate::magician_v2::execution::file_edit::snapshot::SnapshotStore;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "apply_code_proposal")?;
    let workspace = require_scope_str(&args, "__workspace", "apply_code_proposal")?;

    let proposal_id = match args
        .get("proposal_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(id) => id.to_string(),
        None => {
            return Ok(json!({
                "status": "error",
                "reason": "apply_code_proposal requires a non-empty `proposal_id` (from the run_coding_task result).",
            }));
        },
    };
    let selected_paths: Vec<String> = args
        .get("paths")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|path| path.trim().to_string())
                .filter(|path| !path.is_empty())
                .collect()
        })
        .unwrap_or_default();

    // Both stores are rooted at the scope root — the same rooting the run uses to
    // stage the proposal and the `diff_approval` web handler uses to apply it.
    let scope_root = resources
        .artifact_workspace
        .scope_root(&principal, &workspace);
    let workspace_root = resources
        .artifact_workspace
        .capability_home_root(&principal, &workspace);
    let proposals = CodeChangeProposalStore::new(&scope_root);
    let snapshots = SnapshotStore::new(&scope_root);
    // Validate the id (charset, no `..`/path separators) BEFORE it is joined as a
    // filename under `<scope>/code_change_proposals/` — mirrors the HITL apply path's
    // `CodeChangeProposalId::parse`. `from` skipped this, so a traversal-laden id could
    // load an arbitrary JSON off the load path (the authority check still fail-closes the
    // apply, but the read should be constrained too).
    let id = match CodeChangeProposalId::parse(&proposal_id) {
        Ok(id) => id,
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("invalid proposal_id `{proposal_id}`: {error:#}"),
            }));
        },
    };

    let proposal = match proposals.load(&id) {
        Ok(proposal) => proposal,
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("could not load proposal `{proposal_id}`: {error:#}"),
            }));
        },
    };

    // Idempotent: if it's no longer Pending (already applied/rejected, e.g. a
    // tab-open client applied it), do not error — report it so the agent moves on.
    if !matches!(proposal.status, CodeChangeProposalStatus::Pending) {
        return Ok(json!({
            "status": "ok",
            "applied": false,
            "already_applied": true,
            "proposal_id": proposal_id,
            "proposal_status": format!("{:?}", proposal.status),
            "reason": "proposal is no longer Pending (already applied or rejected); nothing to do.",
        }));
    }

    // TRUSTED-STORE INTEGRITY: resolve the apply root from the in-process TrustAuthority,
    // NEVER from the on-disk `proposal.apply_root`. This agent-callable apply path is now
    // bound exactly like the operator HITL path (`respond_hitl_handler` diff_approval
    // proposal arm): the on-disk proposal JSON lives in the agent-writable scopes tree and
    // `shell`/`delegation_shell` bypasses the native runtime-store deny-fence, so a
    // disk-read `apply_root` is a forge/redirect vector. Fail CLOSED on an authority miss
    // (never staged this boot → forged, or predates a restart) or a content-hash mismatch
    // (on-disk patch edited out of band). The key uses the runtime-injected scope
    // (`__principal`/`__workspace`), not the shell-forgeable on-disk `proposal.scope`.
    // `stage_patch_with_apply_root` records the authority entry for every legit proposal
    // (and boot re-anchors pending ones), so this does not affect normal coding runs.
    let Some(authority) = crate::magician_v2::execution::trusted_store::process_authority() else {
        return Ok(json!({
            "status": "error",
            "reason": "apply_code_proposal_authority_unavailable: the trust authority is not installed; cannot verify the apply destination — refusing to apply.",
        }));
    };
    let auth_key = crate::magician_v2::execution::trusted_store::TrustedRecordKey {
        kind: crate::magician_v2::execution::trusted_store::StoreKind::CodeChangeProposal,
        principal: principal.clone(),
        workspace: workspace.clone(),
        id: proposal_id.clone(),
    };
    let requested_root = match authority.get(&auth_key) {
        Some(entry) => {
            if entry.content_hash != proposal.content_hash() {
                return Ok(json!({
                    "status": "error",
                    "reason": "apply_code_proposal_content_tampered: the on-disk proposal content does not match what the coding runtime staged (edited out of band) — re-stage via run_coding_task.",
                }));
            }
            entry
                .apply_root
                .clone()
                .unwrap_or_else(|| workspace_root.clone())
        },
        None => {
            return Ok(json!({
                "status": "error",
                "reason": "apply_code_proposal_not_in_authority: this proposal was not staged by the coding runtime this boot (forged, or predates a restart) — re-stage via run_coding_task.",
            }));
        },
    };
    let apply_root = match requested_root.canonicalize() {
        Ok(root) if root.is_dir() => root,
        Ok(root) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("proposal apply_root `{}` is not a directory", root.display()),
            }));
        },
        Err(error) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("proposal apply_root `{}`: {error}", requested_root.display()),
            }));
        },
    };

    let result = if selected_paths.is_empty() {
        apply_proposal(&proposals, &snapshots, &id, &apply_root).and_then(|_| proposals.load(&id))
    } else {
        apply_proposal_selected_paths(&proposals, &snapshots, &id, &apply_root, &selected_paths)
    };

    match result {
        Ok(loaded) => {
            // Terminal — drop the authority entry so a replayed apply for the same id
            // fail-closes (mirrors the HITL apply path's `forget`).
            authority.forget(&auth_key);
            let applied_files: Vec<String> = loaded
                .applied_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            Ok(json!({
                "status": "ok",
                "applied": true,
                "proposal_id": proposal_id,
                "proposal_status": format!("{:?}", loaded.status),
                "applied_files": applied_files,
                "real_working_dir": apply_root.display().to_string(),
                "note": "Applied to the working tree. The next run_coding_task will see it (the shadow re-syncs from the working tree at turn start). Run run_project_checks to verify, then continue.",
            }))
        },
        Err(error) => Ok(json!({
            "status": "error",
            "reason": format!("apply failed for proposal `{proposal_id}`: {error:#}"),
        })),
    }
}

//! `search_notes` — find the owner's notes by what is written in them.
//!
//! Reads the notes rather than an index of them. The notes space is a folder,
//! and a second one once a provider fallback has been used. An index would be
//! a second copy of the truth that can go stale and disagree with the files
//! while still answering confidently, which is the worst failure a search can
//! have.
//!
//! Scoped to the same boundary-safe roots the Observe traversal uses, so the
//! runtime directories that share the notes root — model caches, credentials,
//! coding worktrees — cannot appear in an owner's results.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::notes::{NoteSearchRequest, NotesSettingsStore};

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "search_notes")?;
    let workspace = require_scope_str(&args, "__workspace", "search_notes")?;

    let Some(query) = super::notes_shared::text_arg(&args, "query") else {
        return Ok(super::notes_shared::argument_error(
            "search_notes requires a non-empty `query` to look for",
        ));
    };

    let store = NotesSettingsStore::with_workspace_layout(resources.artifact_workspace.clone());
    let request = NoteSearchRequest {
        query,
        limit: args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|limit| limit as usize),
        provider: super::notes_shared::text_arg(&args, "provider"),
    };

    // Goal workspaces: matches the acting agent may not read are dropped
    // after ranking and only counted (`goal_workspace_access`).
    if let Ok(roots) = store.note_provider_roots(&principal, &workspace).await {
        crate::magician_v2::execution::goal_workspace_access::register_notes_spaces(roots);
    }
    let agent = crate::magician_v2::execution::native_executors::current_acting_agent();
    // Captured here: the filter may run off this task, where the delegation
    // scope is not set.
    let delegators = crate::magician_v2::execution::native_executors::current_read_delegators();
    let task_root = crate::magician_v2::execution::native_executors::current_evidence_root();
    let admit = move |_relative: &str, absolute: &std::path::Path| {
        use crate::magician_v2::execution::goal_workspace_access as access;
        let admitted = access::refusal_with_delegators(
            absolute,
            access::Op::Read,
            agent.as_deref(),
            &delegators,
        )
        .is_none();
        if admitted {
            // A hit from a goal workspace stays with it (the task's evidence).
            access::record_read_in(task_root.as_deref(), absolute);
        }
        admitted
    };
    match store
        .search_notes_for(&principal, &workspace, request, &admit)
        .await
    {
        Ok((results, withheld)) => Ok(json!({
            "success": true,
            "hits": results.hits,
            "hit_count": results.hits.len(),
            // How many matches sit where this agent may not read (goal
            // workspaces it is not a member of, private goals). Never named.
            "withheld_count": withheld,
            "query_terms": results.query_terms,
            "scanned_notes": results.scanned_notes,
            // Both reported rather than hidden: "nothing found" means something
            // different when the scan stopped early or the cap trimmed results,
            // and a caller that cannot tell those apart will report the wrong
            // thing to the owner.
            "scan_truncated": results.scan_truncated,
            "more_available": results.more_available,
        })),
        Err(error) => Ok(super::notes_shared::read_error("search_notes", error)),
    }
}

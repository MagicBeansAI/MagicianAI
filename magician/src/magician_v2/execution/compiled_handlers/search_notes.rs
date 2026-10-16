//! `search_notes` — find the owner's notes by what is written in them.
//!
//! Reads the notes rather than an index of them. The notes space is a folder,
//! and a second one once a provider fallback has been used; SilverBullet
//! publishes no search API to defer to. An index would be a second copy of the
//! truth that can go stale and disagree with the files while still answering
//! confidently, which is the worst failure a search can have.
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

    match store.search_notes(&principal, &workspace, request).await {
        Ok(results) => Ok(json!({
            "success": true,
            "hits": results.hits,
            "hit_count": results.hits.len(),
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

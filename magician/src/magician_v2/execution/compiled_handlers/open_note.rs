//! `open_note` — find an existing note and hand back the link that opens it.
//!
//! The provider layer had `open-root`, which opens the whole folder. That is
//! the wrong granularity for an agent that has just told the owner about one
//! page: "it's in your notes somewhere" is not an answer. `resolve_note`
//! searches the configured providers and returns the URL for that page.
//!
//! Read-only by construction — it takes no write lock and creates nothing, so
//! asking about a note that does not exist leaves nothing behind.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::notes::NotesSettingsStore;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "open_note")?;
    let workspace = require_scope_str(&args, "__workspace", "open_note")?;

    let Some(path) = super::notes_shared::text_arg(&args, "path") else {
        return Ok(super::notes_shared::argument_error(
            "open_note requires a non-empty `path` relative to the notes space",
        ));
    };

    let store = NotesSettingsStore::with_workspace_layout(resources.artifact_workspace.clone());
    match store.resolve_note(&principal, &workspace, &path).await {
        Ok(location) => Ok(json!({
            "status": "ok",
            "path": location.path,
            "absolute_path": location.absolute_path,
            "provider": location.provider,
            "open_url": location.open_url,
        })),
        Err(error) => Ok(super::notes_shared::read_error("open_note", error)),
    }
}

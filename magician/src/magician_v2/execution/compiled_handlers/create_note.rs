//! `create_note` — write a new note into the owner's configured Notes space.
//!
//! Phase 4 of the Notes plan shipped the whole publishing layer — provider
//! selection, fallback, disclosure modes, asset bundles — behind HTTP routes,
//! and nothing put a tool in front of it. The capability existed and was
//! unreachable from the agents it was built for. This is that surface.
//!
//! The store is constructed here from the workspace already on `AgentResources`
//! rather than threaded through as a new dependency. `NotesSettingsStore` holds
//! no state beyond that workspace, and every provider write serializes on a
//! process-wide lock inside `notes`, so this instance and the one behind the
//! HTTP API are the same writer by construction.

use std::sync::Arc;

use serde_json::Value;

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::notes::{CreateNoteRequest, NotesSettingsStore};

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "create_note")?;
    let workspace = require_scope_str(&args, "__workspace", "create_note")?;

    let Some(title) = super::notes_shared::text_arg(&args, "title") else {
        return Ok(super::notes_shared::argument_error(
            "create_note requires a non-empty `title`",
        ));
    };
    let Some(body) = super::notes_shared::text_arg(&args, "body") else {
        return Ok(super::notes_shared::argument_error(
            "create_note requires a non-empty `body`",
        ));
    };

    let store = NotesSettingsStore::with_workspace_layout(resources.artifact_workspace.clone());
    let request = CreateNoteRequest {
        title,
        body,
        provider: super::notes_shared::text_arg(&args, "provider"),
        target_dir: super::notes_shared::text_arg(&args, "target_dir"),
    };

    match store.create_note(&principal, &workspace, request).await {
        Ok(note) => Ok(super::notes_shared::note_result(note)),
        Err(error) => Ok(super::notes_shared::write_error("create_note", error)),
    }
}

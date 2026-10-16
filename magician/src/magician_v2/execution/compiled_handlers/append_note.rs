//! `append_note` — add to an existing note, or to today's daily page.
//!
//! Distinct from `create_note` because appending is the common case for a
//! running record — a daily log, a thread of findings — and expressing it as
//! read-modify-write through `create_note` would race any other writer and
//! could lose whatever arrived in between. The provider layer owns the append.

use std::sync::Arc;

use serde_json::Value;

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::notes::{AppendNoteRequest, NotesSettingsStore};

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "append_note")?;
    let workspace = require_scope_str(&args, "__workspace", "append_note")?;

    let Some(body) = super::notes_shared::text_arg(&args, "body") else {
        return Ok(super::notes_shared::argument_error(
            "append_note requires a non-empty `body`",
        ));
    };

    let store = NotesSettingsStore::with_workspace_layout(resources.artifact_workspace.clone());
    let request = AppendNoteRequest {
        title: super::notes_shared::text_arg(&args, "title"),
        body,
        provider: super::notes_shared::text_arg(&args, "provider"),
        target_path: super::notes_shared::text_arg(&args, "target_path"),
    };

    match store.append_note(&principal, &workspace, request).await {
        Ok(note) => Ok(super::notes_shared::note_result(note)),
        Err(error) => Ok(super::notes_shared::write_error("append_note", error)),
    }
}

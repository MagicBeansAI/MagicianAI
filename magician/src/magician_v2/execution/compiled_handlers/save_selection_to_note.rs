//! `save_selection_to_note` — file a passage the owner is looking at, with its
//! source.
//!
//! Distinct from `append_note`, which takes prose the agent wrote. This takes
//! text that belongs to something else — a page, a document, a message — and
//! keeps the attribution attached. A quote filed without its source is a quote
//! the owner cannot check later, which is most of why they wanted it kept.
//!
//! Shares `capture_selection` with the browser extension and the desktop
//! overlay, so a capture reads the same however it arrived and the retry
//! contract is identical. The capture decision itself — marker, daily-page
//! naming, the idempotent append plan — lives behind the notes projection
//! seam (`notes_projection`, plan 2.3), shared unchanged with
//! `/notes/capture-selection`.

use std::sync::Arc;

use serde_json::Value;

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::notes::{CaptureSelectionRequest, NotesSettingsStore};

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "save_selection_to_note")?;
    let workspace = require_scope_str(&args, "__workspace", "save_selection_to_note")?;

    let Some(text) = super::notes_shared::text_arg(&args, "text") else {
        return Ok(super::notes_shared::argument_error(
            "save_selection_to_note requires non-empty `text` to save",
        ));
    };

    let store = NotesSettingsStore::with_workspace_layout(resources.artifact_workspace.clone());
    let request = CaptureSelectionRequest {
        text,
        source_url: super::notes_shared::text_arg(&args, "source_url"),
        source_title: super::notes_shared::text_arg(&args, "source_title"),
        source_app: super::notes_shared::text_arg(&args, "source_app"),
        target_path: super::notes_shared::text_arg(&args, "target_path"),
        provider: super::notes_shared::text_arg(&args, "provider"),
        // Deliberately not model-supplied. The id exists so a surface that
        // resends after a lost response files once; a model inventing one
        // could instead suppress a second capture it genuinely meant to make.
        capture_id: None,
    };

    match store
        .capture_selection(&principal, &workspace, request)
        .await
    {
        Ok(note) => Ok(super::notes_shared::note_result(note)),
        Err(error) => Ok(super::notes_shared::write_error(
            "save_selection_to_note",
            error,
        )),
    }
}

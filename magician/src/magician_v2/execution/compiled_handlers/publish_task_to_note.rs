//! `publish_task_to_note` — publish a settled task's record into Notes.
//!
//! Phase 4 built the whole publishing path: disclosure modes, selected asset
//! bundles, a stable projection registry so republishing repairs a page rather
//! than duplicating it. It was reachable from a task card and from HTTP, and
//! from no agent. This is that surface.
//!
//! The store owns the rules — scope ownership and "only a settled task may be
//! published" are enforced in `publish_task_note`, not re-implemented here, so
//! the tool and the card cannot drift into disagreeing about what is publishable.
//! The projection itself (page naming, tags, timeline bounding, rendering)
//! lives behind the notes projection seam (plan 2.3), shared unchanged with
//! the HTTP publish/backfill handlers.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::artifact_v2::service::ScopeRef;
use crate::magician_v2::artifact_v2::V3ReadApi;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::notes::{NotesSettingsStore, PublishTaskNoteRequest};
use crate::magician_v2::notes_projection::publish_mode_from_str;

use super::shared::require_scope_str;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "publish_task_to_note")?;
    let workspace = require_scope_str(&args, "__workspace", "publish_task_to_note")?;

    let Some(service) = resources.artifact_v2_service.as_ref() else {
        return Ok(super::notes_shared::argument_error(
            "publish_task_to_note requires the task service, which is not configured",
        ));
    };
    let Some(task_id) = super::notes_shared::text_arg(&args, "task_id") else {
        return Ok(super::notes_shared::argument_error(
            "publish_task_to_note requires a non-empty `task_id`",
        ));
    };

    let mode = match super::notes_shared::text_arg(&args, "mode").as_deref() {
        None => None,
        Some(value) => match publish_mode_from_str(value) {
            Some(mode) => Some(mode),
            None => {
                return Ok(super::notes_shared::argument_error(&format!(
                    "publish_task_to_note `mode` must be compact, standard, or diagnostic (got `{value}`)"
                )));
            },
        },
    };

    let scope = ScopeRef::system_internal_unauthenticated(&principal.clone(), &workspace.clone());
    let task = match V3ReadApi::get_task(service.as_ref(), &scope, &task_id).await {
        Ok(record) => record,
        Err(error) => {
            return Ok(super::notes_shared::task_lookup_error(
                "publish_task_to_note",
                error,
            ));
        },
    };

    let store = NotesSettingsStore::with_workspace_layout(resources.artifact_workspace.clone());
    let request = PublishTaskNoteRequest {
        provider: super::notes_shared::text_arg(&args, "provider"),
        mode,
        include_assets: args.get("include_assets").and_then(Value::as_bool),
    };

    match store
        .publish_task_note(&principal, &workspace, &task, request)
        .await
    {
        Ok(entry) => Ok(json!({
            "status": "ok",
            "task_id": task_id,
            "note_path": entry.note_path,
            "title": entry.title,
            "provider": entry.provider,
            "requested_provider": entry.requested_provider,
            "used_fallback": entry.used_fallback,
            "fallback_reason": entry.fallback_reason,
            "mode": entry.mode.as_str(),
            "published_at": entry.published_at,
        })),
        Err(error) => Ok(super::notes_shared::write_error(
            "publish_task_to_note",
            error,
        )),
    }
}

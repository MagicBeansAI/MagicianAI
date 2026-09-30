//! `screenshot_preview` — capture a PNG of the project's running dev-server
//! preview and register it as a `#vibedev` attachment (the keystone of M5
//! visual self-correction).
//!
//! Why this exists: an engineer agent can edit + `apply_code_proposal` +
//! `run_project_checks`, but it is BLIND to how the result actually *looks*.
//! This tool drives the pinned `agent-browser` headlessly against the project's
//! live preview URL (`dev_server_manager().status(project_id).local_url`),
//! captures a screenshot, and writes it into the chat-session attachment layout
//! that `run_coding_task` already materializes into Pi's `images[]`. So the
//! visual self-correction loop is, with NO new vision op (Pi is the critic via
//! the existing image-attachment pass-through):
//!
//!   screenshot_preview → run_coding_task(attachment_ids=[…], "critique the UI")
//!     → apply_code_proposal → run_project_checks → screenshot_preview (verify)
//!
//! It writes the index + PNG at the workspace-layout level (the exact paths
//! `run_coding_task::materialize_coding_attachments` reads) under a dedicated
//! per-project synthetic session, so it needs no `ChatService` and never races a
//! real chat session. Returns `{ok:false, …}` (never a hard error) when the
//! preview is not running, so an unattended run skips visual self-correction
//! gracefully on non-web changes.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use uuid::Uuid;

use super::shared::{require_browser_transport, require_scope_str};
use crate::magician_v2::browser_engine_analytics::BrowserEngineAnalyticsContext;
use crate::magician_v2::chat::models::{
    ChatSessionFileIndex, ChatSessionFileOrigin, ChatSessionFileRecord,
};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::execution::primitive_dispatch::browser::session::{
    resolve_browser_engine_plan, AgentBrowserSession, ConnectionMode,
};
use crate::magician_v2::media_seam::dev_server_manager;

/// A cold headless launch + navigate + capture can take a while; bound it so a
/// hung browser never wedges the agent's turn.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(60);
/// Keep the most-recent N screenshots per project's synthetic attachment
/// session; older PNGs + index rows are pruned so disk use stays bounded across
/// many runs. ≥ the M5 per-run iteration cap (3) with headroom for re-verifies.
const MAX_RETAINED_SHOTS: usize = 12;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "screenshot_preview")?;
    let workspace = require_scope_str(&args, "__workspace", "screenshot_preview")?;
    let _agent_id = require_scope_str(&args, "__agent_id", "screenshot_preview")?;

    let project_id = match args
        .get("project_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(id) => id.to_string(),
        None => {
            return Ok(json!({
                "ok": false,
                "reason": "screenshot_preview requires a non-empty `project_id`.",
            }));
        },
    };
    // Default to the viewport (above-the-fold) shot — the honest "what a user
    // first sees" framing for visual critique. `full_page:true` captures the
    // whole scrollable page when the agent wants the long view.
    let full_page = args
        .get("full_page")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // 1. Resolve the live preview URL. If the dev server isn't running, skip
    //    gracefully — the agent should start it or skip visual self-correction.
    let local_url = match dev_server_manager()
        .status(&project_id)
        .await
        .and_then(|status| status.local_url)
        .filter(|url| !url.trim().is_empty())
    {
        Some(url) => url,
        None => {
            return Ok(json!({
                "ok": false,
                "reason": format!(
                    "no running dev-server preview for project `{project_id}`; start the dev server (or skip visual self-correction for non-web changes)."
                ),
                "project_id": project_id,
            }));
        },
    };

    // 2. Resolve the pinned agent-browser CLI for this scope + its engine env
    //    (the same resolver the inner-loop browser dispatch uses).
    let storage_root = resources.artifact_workspace.base_root();
    let cli_path = match AgentBrowserSession::resolve_cli_path_for_scope(
        Some(storage_root),
        Some(principal.as_str()),
        Some(workspace.as_str()),
    ) {
        Ok(path) => path,
        Err(error) => {
            return Ok(json!({
                "ok": false,
                "reason": format!(
                    "agent-browser CLI not resolvable: {error}. Run `make setup-agent-browser`."
                ),
            }));
        },
    };
    let mode = ConnectionMode::Headless;
    require_browser_transport(&resources, &args, "screenshot_preview", &mode).await?;
    let browser_engine = resources
        .magician_config_snapshot()
        .content_acquisition
        .browser
        .engine;
    let engine_plan = match resolve_browser_engine_plan(
        storage_root,
        principal.as_str(),
        workspace.as_str(),
        &mode,
        browser_engine.as_deref(),
    ) {
        Ok(engine_plan) => engine_plan,
        Err(error) => {
            return Ok(json!({
                "ok": false,
                "reason": format!("configured browser engine is unavailable: {error}"),
            }));
        },
    };

    // Stable per-project synthetic attachment session — isolated from real chat
    // sessions (no ChatService, no race), reused across runs so retention works.
    let attachment_session_id = sanitize_session_component(&format!("vibedev-shots-{project_id}"));
    let token = Uuid::new_v4().simple().to_string();
    let short = &token[..12];
    let stored_name = format!("vibedev_shot_{short}.png");
    let attachment_id = format!("att_{short}");

    let outputs_dir = resources.artifact_workspace.chat_session_outputs_dir(
        &principal,
        &workspace,
        &attachment_session_id,
    );
    if let Err(error) = std::fs::create_dir_all(&outputs_dir) {
        return Ok(json!({
            "ok": false,
            "reason": format!(
                "could not prepare attachment outputs dir `{}`: {error}",
                outputs_dir.display()
            ),
        }));
    }
    let screenshot_path = outputs_dir.join(&stored_name);

    // 3. Capture: headless `open <url>` (via ensure_connected) → `screenshot
    //    [--full] <abs path>`. Unique session id so concurrent captures never
    //    collide; the headless browser is closed best-effort after, win or lose.
    let thread_id = format!("vibedevshot-{short}");
    let session = match AgentBrowserSession::new(&thread_id, mode, cli_path) {
        Ok(session) => session
            .with_engine_plan(engine_plan)
            .with_analytics_context(
                BrowserEngineAnalyticsContext::for_scope(
                    storage_root,
                    principal.as_str(),
                    workspace.as_str(),
                    None,
                    None,
                )
                .with_work("preview_project", project_id.clone()),
            )
            .with_initial_url(Some(local_url.clone())),
        Err(error) => {
            return Ok(json!({
                "ok": false,
                "reason": format!("could not create browser session: {error}"),
            }));
        },
    };

    let capture = tokio::time::timeout(
        CAPTURE_TIMEOUT,
        capture_screenshot(&session, &screenshot_path, full_page),
    )
    .await;
    let _ = session.run_cli_command(&["close"]).await;

    match capture {
        Ok(Ok(())) => {},
        Ok(Err(error)) => {
            let _ = std::fs::remove_file(&screenshot_path);
            return Ok(
                json!({ "ok": false, "reason": format!("screenshot capture failed: {error}") }),
            );
        },
        Err(_) => {
            let _ = std::fs::remove_file(&screenshot_path);
            return Ok(json!({
                "ok": false,
                "reason": format!("screenshot capture timed out after {}s", CAPTURE_TIMEOUT.as_secs()),
            }));
        },
    }

    let size = match std::fs::metadata(&screenshot_path) {
        Ok(meta) if meta.len() > 0 => meta.len(),
        _ => {
            let _ = std::fs::remove_file(&screenshot_path);
            return Ok(json!({
                "ok": false,
                "reason": "screenshot file was not produced (empty or missing).",
            }));
        },
    };

    // 4. Register the PNG in the chat-session file index — the read side of
    //    run_coding_task's `#vibedev` attachment materialization. The record
    //    shape mirrors ChatService::register_attachment exactly so the run reads
    //    it identically.
    let record = ChatSessionFileRecord {
        id: attachment_id.clone(),
        stored_name: stored_name.clone(),
        original_name: format!("preview-{project_id}.png"),
        mime_type: "image/png".to_string(),
        size,
        label: Some(format!(
            "Preview screenshot ({})",
            if full_page { "full page" } else { "viewport" }
        )),
        screen_capture: None,
        prompt_image: true,
        origin: ChatSessionFileOrigin::Attachment,
        source_task_output_id: None,
        source_task_id: None,
        created_at: chrono::Utc::now().timestamp_millis(),
    };
    if let Err(error) = append_attachment_record(
        &outputs_dir,
        &index_path(&resources, &principal, &workspace, &attachment_session_id),
        record,
    ) {
        let _ = std::fs::remove_file(&screenshot_path);
        return Ok(json!({
            "ok": false,
            "reason": format!("could not index the screenshot attachment: {error}"),
        }));
    }

    Ok(json!({
        "ok": true,
        "project_id": project_id,
        "preview_url": local_url,
        "screenshot_path": screenshot_path.display().to_string(),
        "attachment_id": attachment_id,
        "attachment_session_id": attachment_session_id,
        "full_page": full_page,
        "note": "Pass attachment_ids=[attachment_id] + attachment_session_id to run_coding_task so Pi sees this screenshot and critiques the rendered UI; then apply_code_proposal + screenshot_preview again to verify. Requires a coding profile that supports image inputs.",
    }))
}

/// `ensure_connected` opens the preview URL headlessly; then capture the PNG to
/// an absolute path. A non-zero agent-browser exit is a hard capture error here
/// (unlike the inner loop, there is no structured stdout to salvage).
async fn capture_screenshot(
    session: &AgentBrowserSession,
    path: &Path,
    full_page: bool,
) -> Result<(), String> {
    session
        .ensure_connected()
        .await
        .map_err(|error| format!("could not open the preview URL: {error:#}"))?;
    let path_str = path
        .to_str()
        .ok_or_else(|| "screenshot path is not valid UTF-8".to_string())?;
    let mut cli_args: Vec<&str> = vec!["screenshot"];
    if full_page {
        cli_args.push("--full");
    }
    cli_args.push(path_str);
    let result = session
        .run_cli_command(&cli_args)
        .await
        .map_err(|error| format!("screenshot command failed: {error:#}"))?;
    if !result.success {
        return Err(format!(
            "agent-browser screenshot exited non-zero: {}",
            result.stderr.trim()
        ));
    }
    Ok(())
}

fn index_path(
    resources: &AgentResources,
    principal: &str,
    workspace: &str,
    session_id: &str,
) -> std::path::PathBuf {
    resources
        .artifact_workspace
        .chat_session_file_index_path(principal, workspace, session_id)
}

/// Append the screenshot record to the session file index, then enforce
/// retention (keep the newest `MAX_RETAINED_SHOTS`, delete older PNGs + rows).
/// Only this tool writes to the synthetic per-project session, so pruning by
/// index order is safe.
fn append_attachment_record(
    outputs_dir: &Path,
    index_path: &Path,
    record: ChatSessionFileRecord,
) -> Result<(), String> {
    if let Some(parent) = index_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create index dir: {error}"))?;
    }
    let mut index: ChatSessionFileIndex = match std::fs::read_to_string(index_path) {
        Ok(content) => serde_json::from_str(&content)
            .map_err(|error| format!("could not parse existing index: {error}"))?,
        Err(_) => ChatSessionFileIndex::default(),
    };
    index.files.push(record);
    if index.files.len() > MAX_RETAINED_SHOTS {
        let remove = index.files.len() - MAX_RETAINED_SHOTS;
        for old in index.files.drain(0..remove) {
            let _ = std::fs::remove_file(outputs_dir.join(&old.stored_name));
        }
    }
    let serialized = serde_json::to_string_pretty(&index)
        .map_err(|error| format!("could not serialize index: {error}"))?;
    std::fs::write(index_path, serialized)
        .map_err(|error| format!("could not write index `{}`: {error}", index_path.display()))?;
    Ok(())
}

/// Collapse to a single safe path component (`[A-Za-z0-9_-]`), so the synthetic
/// session id is always a valid single-component dir name.
fn sanitize_session_component(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "vibedev-shots".to_string()
    } else {
        trimmed.to_string()
    }
}

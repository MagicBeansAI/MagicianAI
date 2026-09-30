//! The notes/tasks projection seam (plan workstream 2.3,
//! docs/plans/2026-08-26-platform-layering-and-app-extraction-plan.md).
//!
//! Two product features ride this seam: task-pages-in-notes (the
//! `publish_task_to_note` tool plus the notes publish/backfill/promote HTTP
//! handlers) and save-selection-to-notes (the `save_selection_to_note` tool
//! plus `POST /notes/capture-selection`). Both are projections over the
//! notes provider — product logic (Layer 2) with a one-way dependency on
//! the provider layer (Layer 1) in `notes.rs` — and they are seam-registered
//! modules, not app packages: no stated packaging benefit, and packaging
//! would imply a notes-UI rewrite.
//!
//! What moved here in 2.3, behavior-identical:
//! - The capture projection. `capture_append_plan` is the whole idempotent
//!   `capture-selection` decision — selection validation, capture-id safety,
//!   daily-page naming, and the quoted Markdown block with its provenance
//!   and marker. `already_captured_note_ref` is the resend-once answer.
//! - The task-page projection: page naming, tags, projection date, asset
//!   file names, timeline bounding, and the full Markdown rendering.
//! - The memory-promotion projection behind promote-to-memory (moved from
//!   `magician-api/src/notes_api.rs`, which re-imports it).
//!
//! What did not move: the provider registry, provider selection/fallback,
//! settings, storage, and the process-wide write lock stay in `notes.rs` —
//! the multi-writer platform store is Layer 1, and every write above it
//! (a capture included) still goes through the same `append_note` /
//! `write_note_markdown` paths, which is what keeps daily-page naming,
//! provider selection and fallback from drifting between a capture and an
//! append. `NotesSettingsStore` keeps orchestration — scope checks,
//! envelope loads, provider writes, index commits — and calls the decision
//! functions here, mirroring how the chat lane seam (1.2) extracted
//! decisions while the service kept orchestration. Handlers, routes, and
//! wire contracts are unchanged.

use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};

use crate::magician_v2::artifact_v2::models::{
    ExecutionIndexEntry, ExecutionScheduleStep, OutputRef, TaskRecord,
};
use crate::magician_v2::learning::{
    CreateLearningCandidateRequest, LearningCandidateState, LearningCandidateType,
    LearningEvidenceRef, LearningRiskLevel,
};
use crate::magician_v2::notes::{
    path_to_string, slugify, CaptureSelectionRequest, NoteLocation, NoteRef, TaskNoteAssetRef,
    TaskNoteIndexEntry, TaskNotePublishMode,
};

/// Schema id stamped into every task page's frontmatter and index entry.
pub(crate) const TASK_NOTE_SCHEMA: &str = "magician.task-note.v1";
/// How many executions a standard page's timeline carries: the newest twenty.
const STANDARD_TIMELINE_EXECUTIONS: usize = 20;
/// How many steps per execution a standard page's timeline spells out.
const STANDARD_TIMELINE_STEPS: usize = 12;

// --- Capture projection ----------------------------------------------------

/// The page a capture lands on when the caller names none.
///
/// Shared with the provider's own append default so the retry check and the
/// write cannot disagree about which file today's captures live in.
pub(crate) fn default_daily_page_path() -> PathBuf {
    PathBuf::from("Inbox").join(format!("{}.md", Utc::now().format("%Y-%m-%d")))
}

/// Yesterday's default page, in the same spelling as `default_daily_page_path`.
///
/// Dedupe-only: a capture sent just before midnight and retried just after
/// resolves a new "today", so the retry read must also check the page the
/// original wrote to. No write ever targets this — an append still lands on the
/// current default page — and the look-back spans exactly one day, not an
/// arbitrary history.
pub(crate) fn previous_daily_page_path() -> PathBuf {
    PathBuf::from("Inbox").join(format!(
        "{}.md",
        (Utc::now() - chrono::Duration::days(1)).format("%Y-%m-%d")
    ))
}

/// The write a capture resolves to before any provider I/O: the Markdown
/// block, the page it lands on, and the id that makes a resend land once.
/// The store owns everything around it — the retry read, provider selection,
/// and the append itself — so a capture stays an append with provenance
/// rather than a second write path.
#[derive(Debug)]
pub(crate) struct CaptureAppendPlan {
    pub(crate) body: String,
    pub(crate) capture_id: Option<String>,
    pub(crate) target_path: String,
}

/// Decide a capture: validate the selection, validate the id, name the page,
/// render the block. This is the entire decision half of
/// `NotesSettingsStore::capture_selection`; it touches no provider files, so
/// an error here leaves nothing behind.
pub(crate) fn capture_append_plan(
    request: &CaptureSelectionRequest,
) -> std::io::Result<CaptureAppendPlan> {
    let text = request.text.trim().to_string();
    if text.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a capture needs selected text",
        ));
    }
    let capture_id = request
        .capture_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if let Some(capture_id) = capture_id.as_deref() {
        if !capture_id_is_safe(capture_id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "capture_id must be short and alphanumeric with `-_.:`",
            ));
        }
    }

    // The page a capture lands on when the caller names none. Resolved here
    // so the retry check reads the same file the append would write.
    let target_path = match request.target_path.as_deref() {
        Some(path) => path.to_string(),
        None => path_to_string(&default_daily_page_path()),
    };

    Ok(CaptureAppendPlan {
        body: render_capture_markdown(&text, request, capture_id.as_deref()),
        capture_id,
        target_path,
    })
}

/// The answer to a resend whose capture already landed. Nothing was written,
/// so `requested` equals `provider` because no selection happened, and no
/// fallback could have occurred.
pub(crate) fn already_captured_note_ref(existing: NoteLocation) -> NoteRef {
    NoteRef {
        requested_provider: existing.provider.clone(),
        provider: existing.provider,
        used_fallback: false,
        fallback_reason: None,
        path: existing.path,
        absolute_path: existing.absolute_path,
        open_url: existing.open_url,
    }
}

/// Escape the characters that would end a Markdown link's text early.
///
/// Replacing them would silently alter the owner's title; escaping keeps the
/// title readable and the link intact.
fn escape_link_text(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

/// Whether a capture id is safe to write into a note.
///
/// The marker is an HTML comment, so an id containing `-->` would close it early
/// and everything after would render as content the owner never captured. Ids
/// come from callers over HTTP, so this is validated rather than trusted.
fn capture_id_is_safe(capture_id: &str) -> bool {
    !capture_id.is_empty()
        && capture_id.len() <= 128
        && capture_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

/// Marker that records which capture produced a block.
///
/// An HTML comment so it renders as nothing in any Markdown viewer — the owner
/// reads their note, not our bookkeeping — while staying greppable in the file
/// that is the source of truth. One spelling, used by both the write (render)
/// and the retry read, so the two cannot drift.
pub(crate) fn capture_marker(capture_id: &str) -> String {
    format!("<!-- magician-capture:{capture_id} -->")
}

/// Render a capture: the selection, then where it came from.
///
/// The text is quoted so it stays visibly the source's words rather than the
/// owner's. Provenance is emitted only for the fields that exist — a native-app
/// selection has no URL, and printing "Source: (none)" would be worse than
/// printing nothing.
fn render_capture_markdown(
    text: &str,
    request: &CaptureSelectionRequest,
    capture_id: Option<&str>,
) -> String {
    let mut out = String::new();
    for line in text.lines() {
        out.push_str("> ");
        out.push_str(line);
        out.push('\n');
    }

    let title = request
        .source_title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let url = request
        .source_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let app = request
        .source_app
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let source = match (title, url) {
        // Angle-bracket destination: a URL containing `)` — every Wikipedia
        // disambiguation link, among others — otherwise terminates the link at
        // the first one and leaves the rest as loose text.
        (Some(title), Some(url)) => Some(format!("[{}](<{}>)", escape_link_text(title), url)),
        (None, Some(url)) => Some(format!("<{url}>")),
        (Some(title), None) => Some(title.to_string()),
        (None, None) => app.map(str::to_string),
    };
    if let Some(source) = source {
        out.push('\n');
        match app {
            Some(app) if title.is_some() || url.is_some() => {
                out.push_str(&format!("— {source} ({app})\n"));
            },
            _ => out.push_str(&format!("— {source}\n")),
        }
    }
    if let Some(capture_id) = capture_id {
        out.push_str(&capture_marker(capture_id));
        out.push('\n');
    }
    out
}

// --- Task-page projection --------------------------------------------------

/// One execution as a page renders it: the index entry, the schedule steps
/// spelled out, and how many steps the mode left out.
#[derive(Debug, Clone)]
pub(crate) struct TaskNoteTimelineEntry {
    pub(crate) execution: ExecutionIndexEntry,
    pub(crate) steps: Vec<ExecutionScheduleStep>,
    pub(crate) omitted_steps: usize,
}

/// Parse the projection mode every surface names as text. `None` is the
/// fail-closed answer for an unrecognized spelling; the caller owns the error
/// its own transport deserves.
pub(crate) fn publish_mode_from_str(value: &str) -> Option<TaskNotePublishMode> {
    match value {
        "compact" => Some(TaskNotePublishMode::Compact),
        "standard" => Some(TaskNotePublishMode::Standard),
        "diagnostic" => Some(TaskNotePublishMode::Diagnostic),
        _ => None,
    }
}

/// Order a task's executions as the timeline renders them and bound them by
/// mode: ascending start, and in standard mode the newest
/// [`STANDARD_TIMELINE_EXECUTIONS`] entries. Diagnostic keeps every
/// execution; compact never builds a timeline at all.
pub(crate) fn bound_timeline_executions(
    mode: TaskNotePublishMode,
    mut executions: Vec<ExecutionIndexEntry>,
) -> Vec<ExecutionIndexEntry> {
    executions.sort_by(|left, right| left.started_at.cmp(&right.started_at));
    if mode == TaskNotePublishMode::Standard && executions.len() > STANDARD_TIMELINE_EXECUTIONS {
        executions = executions.split_off(executions.len() - STANDARD_TIMELINE_EXECUTIONS);
    }
    executions
}

/// Bound one execution's schedule steps the way the page renders them: in
/// standard mode the first [`STANDARD_TIMELINE_STEPS`] with the count left
/// out reported, so the omission is visible rather than silent. Returns the
/// kept steps and the omitted count.
pub(crate) fn bound_timeline_steps(
    mode: TaskNotePublishMode,
    mut steps: Vec<ExecutionScheduleStep>,
) -> (Vec<ExecutionScheduleStep>, usize) {
    if mode == TaskNotePublishMode::Standard && steps.len() > STANDARD_TIMELINE_STEPS {
        let omitted = steps.len() - STANDARD_TIMELINE_STEPS;
        steps.truncate(STANDARD_TIMELINE_STEPS);
        (steps, omitted)
    } else {
        (steps, 0)
    }
}

pub(crate) fn task_note_date(completed_at: Option<&str>, updated_at: &str) -> String {
    completed_at
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .or_else(|| DateTime::parse_from_rfc3339(updated_at).ok())
        .map(|value| value.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string())
}

pub(crate) fn task_note_sort_instant(note: &TaskNoteIndexEntry) -> i64 {
    note.task_completed_at
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .or_else(|| DateTime::parse_from_rfc3339(&note.source_updated_at).ok())
        .map(|value| value.timestamp_millis())
        .unwrap_or(i64::MIN)
}

pub(crate) fn task_note_path(task: &TaskRecord, date: &str) -> String {
    let slug = truncate_slug(&slugify(&task.manifest.title), 72);
    let suffix = if slug.is_empty() {
        task.manifest.task_id.clone()
    } else {
        format!("{}-{slug}", task.manifest.task_id)
    };
    format!("Tasks/{date}/{suffix}.md")
}

pub(crate) fn task_note_tags(task: &TaskRecord, date: &str) -> Vec<String> {
    let mut tags = vec![
        "magician".to_string(),
        "magician/task".to_string(),
        format!("date/{date}"),
    ];
    let status = slugify(&task.state.status);
    if !status.is_empty() {
        tags.push(format!("task/{status}"));
    }
    let agent = slugify(&task.manifest.agent_id);
    if !agent.is_empty() {
        tags.push(format!("agent/{agent}"));
    }
    tags.push(format!(
        "lifecycle/{}",
        match task.manifest.lifecycle {
            crate::magician_v2::artifact_v2::models::TaskLifecycle::Persistent => "persistent",
            crate::magician_v2::artifact_v2::models::TaskLifecycle::Internal => "internal",
        }
    ));
    if let Some(priority) = task.manifest.priority.as_deref() {
        let priority = slugify(priority);
        if !priority.is_empty() {
            tags.push(format!("priority/{priority}"));
        }
    }
    if let Some(due_date) = task.manifest.due_date.as_deref().and_then(iso_date_prefix) {
        tags.push(format!("due/{due_date}"));
    }
    for tag in &task.manifest.tags {
        let normalized = slugify(&tag.name);
        if !normalized.is_empty() {
            tags.push(format!("task-tag/{normalized}"));
        }
    }
    tags.sort();
    tags.dedup();
    tags
}

fn iso_date_prefix(value: &str) -> Option<&str> {
    let prefix = value.get(..10)?;
    NaiveDate::parse_from_str(prefix, "%Y-%m-%d")
        .ok()
        .map(|_| prefix)
}

pub(crate) fn output_media_is_embeddable(media_type: &str) -> bool {
    matches!(
        media_type.split(';').next().unwrap_or("").trim(),
        "text/markdown"
            | "text/plain"
            | "text/html"
            | "application/json"
            | "application/xml"
            | "text/xml"
    )
}

pub(crate) fn task_note_asset_file_name(output: &OutputRef) -> String {
    let stem = truncate_slug(&slugify(&output.output_id), 64);
    let stem = if stem.is_empty() { "artifact" } else { &stem };
    let digest = blake3::hash(output.output_id.as_bytes()).to_hex();
    let extension = Path::new(&output.relative_path)
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 12
                && value.chars().all(|ch| ch.is_ascii_alphanumeric())
        })
        .map(|value| format!(".{}", value.to_ascii_lowercase()))
        .unwrap_or_default();
    format!("{stem}-{}{extension}", &digest.as_str()[..8])
}

fn truncate_slug(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect::<String>()
}

pub(crate) fn markdown_code_block(language: &str, body: &str) -> String {
    let longest_backtick_run = body
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let fence = "`".repeat(longest_backtick_run.saturating_add(1).max(3));
    format!("{fence}{language}\n{}\n{fence}", body.trim_end())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_task_note_markdown(
    task: &TaskRecord,
    mode: TaskNotePublishMode,
    date: &str,
    completed_at: Option<&str>,
    published_at: &str,
    tags: &[String],
    primary_body: Option<&str>,
    intermediate_outputs: &[(OutputRef, String)],
    timeline: &[TaskNoteTimelineEntry],
    assets: &[TaskNoteAssetRef],
) -> String {
    let yaml_string =
        |value: &str| serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string());
    let mut markdown = format!(
        "---\nmagician_kind: task\nmagician_schema: {}\ntask_id: {}\nprincipal: {}\nworkspace: {}\nthread_id: {}\nagent_id: {}\nstatus: {}\nlifecycle: {}\nmode: {}\ndate: {}\ncreated_at: {}\ndue_date: {}\ncompleted_at: {}\nsource_updated_at: {}\npublished_at: {}\ntags: {}\n---\n\n# {}\n\n",
        yaml_string(TASK_NOTE_SCHEMA),
        yaml_string(&task.manifest.task_id),
        yaml_string(&task.manifest.principal),
        yaml_string(&task.manifest.workspace),
        yaml_string(&task.manifest.ui_thread_id),
        yaml_string(&task.manifest.agent_id),
        yaml_string(&task.state.status),
        yaml_string(match task.manifest.lifecycle {
            crate::magician_v2::artifact_v2::models::TaskLifecycle::Persistent => "persistent",
            crate::magician_v2::artifact_v2::models::TaskLifecycle::Internal => "internal",
        }),
        yaml_string(mode.as_str()),
        yaml_string(date),
        yaml_string(&task.manifest.created_at),
        task.manifest
            .due_date
            .as_deref()
            .map(yaml_string)
            .unwrap_or_else(|| "null".to_string()),
        completed_at
            .map(yaml_string)
            .unwrap_or_else(|| "null".to_string()),
        yaml_string(&task.state.updated_at),
        yaml_string(published_at),
        serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string()),
        task.manifest
            .title
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;"),
    );
    markdown.push_str(&format!(
        "> {} · {} · published {}\n\n",
        task.state.status,
        completed_at.unwrap_or(&task.state.updated_at),
        published_at
    ));
    if !task.manifest.description.trim().is_empty() {
        markdown.push_str("## Goal\n\n");
        markdown.push_str(task.manifest.description.trim());
        markdown.push_str("\n\n");
    }
    markdown.push_str("## Final answer\n\n");
    markdown.push_str(
        primary_body
            .unwrap_or("No user-facing final output was available when this page was published."),
    );
    markdown.push_str("\n\n");

    if mode != TaskNotePublishMode::Compact && !timeline.is_empty() {
        markdown.push_str("## Run timeline\n\n");
        for entry in timeline {
            let execution = &entry.execution;
            markdown.push_str(&format!(
                "- **{}** · `{}` · {} → {}{}\n",
                execution.status,
                execution.agent_id,
                execution.started_at,
                execution
                    .completed_at
                    .as_deref()
                    .unwrap_or(&execution.updated_at),
                execution
                    .plan_id
                    .as_deref()
                    .map(|plan| format!(" · plan `{plan}`"))
                    .unwrap_or_default(),
            ));
            for step in &entry.steps {
                markdown.push_str(&format!(
                    "  - **{}** {} · `{}`{}\n",
                    step.taskplan_status,
                    step.title,
                    step.step_id,
                    if step.taskplan_progress.trim().is_empty() {
                        String::new()
                    } else {
                        format!(" · {}", step.taskplan_progress.trim())
                    },
                ));
            }
            if entry.omitted_steps > 0 {
                markdown.push_str(&format!(
                    "  - _{} additional steps omitted in standard mode_\n",
                    entry.omitted_steps
                ));
            }
        }
        markdown.push('\n');
    }
    if mode != TaskNotePublishMode::Compact && !intermediate_outputs.is_empty() {
        markdown.push_str("## Important outputs\n\n");
        for (output, body) in intermediate_outputs {
            markdown.push_str(&format!(
                "### {}\n\n_Source: `{}` · `{}`_\n\n{}\n\n",
                readable_output_label(output),
                output.output_id,
                output.relative_path,
                body,
            ));
        }
    }
    if !assets.is_empty() {
        markdown.push_str("## Assets\n\n");
        for asset in assets {
            let path = Path::new(&asset.path);
            let relative = match (path.parent().and_then(Path::file_name), path.file_name()) {
                (Some(parent), Some(file)) => {
                    format!("./{}/{}", parent.to_string_lossy(), file.to_string_lossy())
                },
                _ => asset.path.clone(),
            };
            markdown.push_str(&format!(
                "- [{}]({}) · `{}` · {} bytes · `{}`\n",
                Path::new(&asset.path)
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("artifact"),
                relative,
                asset.media_type,
                asset.bytes,
                asset.source_output_id,
            ));
        }
        markdown.push('\n');
    }
    if mode == TaskNotePublishMode::Diagnostic {
        markdown.push_str(
            "> [!warning] Diagnostic projection explicitly requested. Raw task metadata and additional outputs may be present in the asset bundle.\n\n",
        );
    }
    markdown.push_str("## Provenance\n\n");
    markdown.push_str(&format!(
        "- Task: `{}`\n- Thread: `{}`\n- Agent: `{}`\n- Source updated: `{}`\n- Projection mode: `{}`\n",
        task.manifest.task_id,
        task.manifest.ui_thread_id,
        task.manifest.agent_id,
        task.state.updated_at,
        mode.as_str(),
    ));
    markdown
}

fn readable_output_label(output: &OutputRef) -> String {
    let name = Path::new(&output.relative_path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(&output.output_id);
    name.replace(['_', '-'], " ")
}

// --- Memory-promotion projection -------------------------------------------

/// The memory-facing body of a published note: frontmatter and the volatile
/// publication banner removed, bounded to the caller's char budget. Public
/// because the promote handler in `magician-api` calls it.
pub fn bounded_note_memory_value(markdown: &str, max_chars: usize) -> String {
    let without_frontmatter = markdown
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n").map(|(_, body)| body))
        .unwrap_or(markdown);
    // The generated publication banner contains `published_at`, which changes
    // on an otherwise identical idempotent re-projection. It is provenance,
    // not memory content, so remove it before candidate fingerprinting.
    let stable_body = without_frontmatter
        .lines()
        .filter(|line| !(line.starts_with("> ") && line.contains(" · published ")))
        .collect::<Vec<_>>()
        .join("\n");
    stable_body
        .trim()
        .chars()
        .take(max_chars)
        .collect::<String>()
}

/// Fingerprint id for a promoted note's candidate: stable across
/// re-publications of the same source content, distinct for different
/// content. Public because the promote handler in `magician-api` calls it.
pub fn task_note_memory_candidate_id(
    task_id: &str,
    source_updated_at: &str,
    value: &str,
) -> String {
    let fingerprint =
        blake3::hash(format!("{task_id}\0{source_updated_at}\0{value}").as_bytes()).to_hex();
    format!("lc_note_{}", &fingerprint.as_str()[..40])
}

/// The review-gated memory candidate an explicit promotion creates. It never
/// writes a memory tier: existing learning review and promotion policy stays
/// the only bridge. Public because the promote handler in `magician-api`
/// calls it.
pub fn build_task_note_memory_candidate_request(
    note: &TaskNoteIndexEntry,
    task_id: &str,
    memory_value: String,
) -> CreateLearningCandidateRequest {
    CreateLearningCandidateRequest {
        principal: None,
        workspace: None,
        candidate_type: LearningCandidateType::MemoryFact,
        state: LearningCandidateState::Proposed,
        title: format!("Remember from note: {}", note.title),
        summary: memory_value.clone(),
        rationale: "User explicitly promoted a published task note for memory review.".to_string(),
        proposed_change: serde_json::json!({
            "memory": {
                "scope": "user",
                "target_tier": "knowledge",
                "operation": "upsert",
                "key": format!("task_note_{task_id}"),
                "value": memory_value,
                "explicit_user_request": true,
                "source_type": "task_note",
                "source_note_path": note.note_path.clone(),
                "source_task_id": task_id,
                "source_updated_at": note.source_updated_at.clone(),
                "facets": note.tags.clone(),
            }
        }),
        proposed_target: Some("user.knowledge".to_string()),
        confidence: Some(0.8),
        source_agent_id: Some(note.agent_id.clone()),
        source_task_id: Some(note.task_id.clone()),
        source_execution_id: None,
        source_chat_session_id: None,
        event_refs: Vec::new(),
        evidence_refs: vec![LearningEvidenceRef {
            kind: "task_note".to_string(),
            id: Some(note.projection_id.clone()),
            path: Some(note.note_path.clone()),
            uri: note.open_url.clone(),
            summary: Some(format!(
                "Published {} task note from {}",
                note.mode.as_str(),
                note.published_at
            )),
        }],
        risk_level: LearningRiskLevel::Low,
        review_required: true,
        review_reason: Some(
            "Confirm the exact durable fact before it enters user memory.".to_string(),
        ),
        review_policy: serde_json::json!({
            "source": "task_note_explicit_promotion",
            "requires_human_confirmation": true,
        }),
        promotion_target: Some("user.knowledge".to_string()),
        promotion_policy: serde_json::json!({ "bridge": "memory" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::artifact_v2::models::{
        TaskLifecycle, TaskManifest, TaskOutputMode, TaskRefs, TaskState, TaskSyncMode,
        TaskTagRecord,
    };

    fn capture_request(text: &str) -> CaptureSelectionRequest {
        CaptureSelectionRequest {
            text: text.to_string(),
            source_url: None,
            source_title: None,
            source_app: None,
            target_path: None,
            provider: None,
            capture_id: None,
        }
    }

    /// The midnight look-back page must use the dated Inbox spelling the
    /// default page uses, or the retry read would look for a file that can
    /// never hold the marker.
    #[test]
    fn previous_daily_page_matches_the_default_page_naming() {
        let previous = previous_daily_page_path();
        assert_eq!(previous.parent(), default_daily_page_path().parent());
        let name = previous.file_name().unwrap().to_string_lossy();
        assert_eq!(name.len(), "YYYY-MM-DD.md".len());
        assert!(name.ends_with(".md"));
        assert_ne!(previous, default_daily_page_path());
    }

    fn projection_sample_task() -> TaskRecord {
        TaskRecord {
            manifest: TaskManifest {
                task_id: "task-t1".to_string(),
                principal: "anonymous".to_string(),
                workspace: "default".to_string(),
                title: "Research & launch plan".to_string(),
                description: "Build the brief.".to_string(),
                agent_id: "research-agent".to_string(),
                goal_id: None,
                ui_thread_id: "thread-9".to_string(),
                priority: Some("p2".to_string()),
                due_date: Some("2026-08-03T17:30:00+05:30".to_string()),
                tags: vec![TaskTagRecord {
                    id: "tag-1".to_string(),
                    name: "Launch Plan".to_string(),
                    color: None,
                }],
                created_by: "user".to_string(),
                depends_on: Vec::new(),
                approved: true,
                schedule: None,
                output_mode: TaskOutputMode::Accumulate,
                chat_session_id: None,
                lifecycle: TaskLifecycle::Persistent,
                sync_mode: TaskSyncMode::Deferred,
                monitor_spec: None,
                monitor_revision: 0,
                created_at: "2026-08-01T09:00:00Z".to_string(),
                updated_at: "2026-08-01T10:17:00Z".to_string(),
            },
            state: TaskState {
                task_id: "task-t1".to_string(),
                status: "completed".to_string(),
                completion_kind: None,
                open_items: Vec::new(),
                active_root_execution_id: None,
                latest_root_execution_id: Some("exec-1".to_string()),
                last_completed_root_execution_id: Some("exec-1".to_string()),
                default_task_agent_output_id: None,
                primary_user_output_id: None,
                schedule_fire_count: 0,
                synthesis_pending_executions: Vec::new(),
                synthesis_failed_execution_id: None,
                monitor_cursor: None,
                last_progress_at: None,
                updated_at: "2026-08-01T10:17:00Z".to_string(),
            },
            refs: TaskRefs {
                task_id: "task-t1".to_string(),
                outputs: Vec::new(),
                default_task_agent_output_id: None,
                primary_user_output_id: None,
                updated_at: Some("2026-08-01T10:17:00Z".to_string()),
            },
        }
    }

    fn timeline_entry(execution_id: &str, omitted_steps: usize) -> TaskNoteTimelineEntry {
        TaskNoteTimelineEntry {
            execution: ExecutionIndexEntry {
                execution_id: execution_id.to_string(),
                task_id: "task-t1".to_string(),
                root_execution_id: Some(execution_id.to_string()),
                parent_execution_id: None,
                agent_id: "research-agent".to_string(),
                relationship_type: "root".to_string(),
                status: "completed".to_string(),
                completion_kind: None,
                open_items: Vec::new(),
                plan_id: Some("plan-1".to_string()),
                primary_execution_output_id: None,
                active_child_execution_ids: Vec::new(),
                started_at: "2026-08-01T10:00:00Z".to_string(),
                completed_at: Some("2026-08-01T10:16:59Z".to_string()),
                updated_at: "2026-08-01T10:16:59Z".to_string(),
            },
            steps: vec![ExecutionScheduleStep {
                step_id: "step-research".to_string(),
                title: "Validate launch channels".to_string(),
                order: 0,
                depends_on_step_ids: Vec::new(),
                capability: Some("research".to_string()),
                delegate_agent_id: None,
                taskplan_status: "completed".to_string(),
                taskplan_progress: "3 channels validated".to_string(),
                sub_step_labels: Vec::new(),
                recipe: None,
            }],
            omitted_steps,
        }
    }

    /// The whole capture decision in one pass: the passage quoted, the
    /// provenance linked, the marker written, and the page named.
    #[test]
    fn a_capture_plan_renders_the_quote_its_source_and_its_marker() {
        let request = CaptureSelectionRequest {
            source_url: Some("https://example.test/post".to_string()),
            source_title: Some("A Post".to_string()),
            source_app: Some("Google Chrome".to_string()),
            capture_id: Some("capture-1".to_string()),
            ..capture_request("first line\nsecond line")
        };
        let plan = capture_append_plan(&request).unwrap();
        assert!(plan.body.contains("> first line\n"));
        assert!(plan.body.contains("> second line\n"));
        assert!(plan.body.contains("[A Post](<https://example.test/post>)"));
        assert!(plan.body.contains("(Google Chrome)"));
        assert!(plan.body.contains(&capture_marker("capture-1")));
        // A caller-named page is honored verbatim.
        let request = CaptureSelectionRequest {
            target_path: Some("Inbox/captures.md".to_string()),
            ..request
        };
        assert_eq!(
            capture_append_plan(&request).unwrap().target_path,
            "Inbox/captures.md"
        );
        // Without an id there is no marker to find on a retry — and no false
        // idempotency claim for a capture that never asked for one.
        let plan = capture_append_plan(&capture_request("a passage")).unwrap();
        assert_eq!(plan.capture_id, None);
        assert!(!plan.body.contains("magician-capture:"));
    }

    /// The default page is today's dated Inbox page, and it is the same page
    /// the provider's plain append default names — the retry check and the
    /// write cannot disagree about where today's captures live.
    #[test]
    fn the_default_capture_page_is_today_s_dated_inbox_page() {
        let path = path_to_string(&default_daily_page_path());
        let (folder, file) = path.split_once('/').expect("dated inbox page");
        assert_eq!(folder, "Inbox");
        let date = file.strip_suffix(".md").expect("markdown page");
        assert_eq!(
            NaiveDate::parse_from_str(date, "%Y-%m-%d").ok(),
            Some(Utc::now().date_naive()),
            "the default page is named for today's UTC date"
        );
        assert_eq!(
            capture_append_plan(&capture_request("x"))
                .unwrap()
                .target_path,
            path
        );
    }

    /// Everything the plan refuses must be refused before a provider file is
    /// touched, and the refusal is the same one the store surfaced inline
    /// before the seam existed.
    #[test]
    fn an_empty_selection_or_an_unsafe_id_never_reaches_a_plan() {
        let error = capture_append_plan(&capture_request("   \n  "))
            .expect_err("whitespace is not a selection");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);

        for hostile in ["a --> <img src=x>", "id with spaces", "<!--nested"] {
            let request = CaptureSelectionRequest {
                capture_id: Some(hostile.to_string()),
                ..capture_request("a passage")
            };
            let error = capture_append_plan(&request)
                .expect_err("an unsafe capture id must not reach a note");
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }

        // A UUID, which is what the surfaces actually send, is accepted.
        let request = CaptureSelectionRequest {
            capture_id: Some("3f2504e0-4f89-11d3-9a0c-0305e82c3301".to_string()),
            ..capture_request("a passage")
        };
        assert!(capture_append_plan(&request).is_ok());
    }

    /// The resend-once answer claims nothing that did not happen: no write,
    /// so no selection, so no fallback.
    #[test]
    fn a_resend_answer_points_at_the_original_page_and_claims_no_fallback() {
        let note_ref = already_captured_note_ref(NoteLocation {
            provider: "local_markdown".to_string(),
            path: "Inbox/2026-08-26.md".to_string(),
            absolute_path: "/tmp/notes/Inbox/2026-08-26.md".to_string(),
            open_url: Some("file:///tmp/notes/Inbox/2026-08-26.md".to_string()),
        });
        assert_eq!(note_ref.requested_provider, note_ref.provider);
        assert!(!note_ref.used_fallback);
        assert!(note_ref.fallback_reason.is_none());
        assert_eq!(note_ref.path, "Inbox/2026-08-26.md");
        assert_eq!(
            note_ref.open_url.as_deref(),
            Some("file:///tmp/notes/Inbox/2026-08-26.md")
        );
    }

    /// The tool surface names modes as text; every other spelling is
    /// refused rather than guessed.
    #[test]
    fn publish_modes_parse_the_three_spellings_and_refuse_all_others() {
        assert_eq!(
            publish_mode_from_str("compact"),
            Some(TaskNotePublishMode::Compact)
        );
        assert_eq!(
            publish_mode_from_str("standard"),
            Some(TaskNotePublishMode::Standard)
        );
        assert_eq!(
            publish_mode_from_str("diagnostic"),
            Some(TaskNotePublishMode::Diagnostic)
        );
        for other in ["", "COMPACT", "verbose", "standard "] {
            assert_eq!(publish_mode_from_str(other), None);
        }
    }

    fn execution_at(index: usize) -> ExecutionIndexEntry {
        ExecutionIndexEntry {
            execution_id: format!("exec-{index:02}"),
            task_id: "task-t1".to_string(),
            root_execution_id: None,
            parent_execution_id: None,
            agent_id: "research-agent".to_string(),
            relationship_type: "root".to_string(),
            status: "completed".to_string(),
            completion_kind: None,
            open_items: Vec::new(),
            plan_id: None,
            primary_execution_output_id: None,
            active_child_execution_ids: Vec::new(),
            started_at: format!("2026-08-01T09:{index:02}:00Z"),
            completed_at: None,
            updated_at: format!("2026-08-01T09:{index:02}:00Z"),
        }
    }

    /// Standard pages carry the newest twenty executions in start order;
    /// diagnostic carries all of them. The bound lives here so the renderer
    /// and the projection cannot disagree about what a timeline is.
    #[test]
    fn standard_timelines_keep_the_newest_twenty_executions() {
        let executions: Vec<_> = (0..25).rev().map(execution_at).collect();
        let standard = bound_timeline_executions(TaskNotePublishMode::Standard, executions.clone());
        assert_eq!(standard.len(), 20);
        assert_eq!(standard.first().unwrap().execution_id, "exec-05");
        assert_eq!(standard.last().unwrap().execution_id, "exec-24");
        let diagnostic = bound_timeline_executions(TaskNotePublishMode::Diagnostic, executions);
        assert_eq!(diagnostic.len(), 25);
        assert_eq!(diagnostic.first().unwrap().execution_id, "exec-00");
    }

    fn step_at(index: usize) -> ExecutionScheduleStep {
        ExecutionScheduleStep {
            step_id: format!("step-{index:02}"),
            title: format!("Step {index}"),
            order: index,
            depends_on_step_ids: Vec::new(),
            capability: None,
            delegate_agent_id: None,
            taskplan_status: "completed".to_string(),
            taskplan_progress: String::new(),
            sub_step_labels: Vec::new(),
            recipe: None,
        }
    }

    /// Standard pages spell out twelve steps per execution and say how many
    /// were omitted; diagnostic spells out every one.
    #[test]
    fn standard_pages_spell_out_twelve_steps_and_report_the_rest() {
        let steps: Vec<_> = (0..15).map(step_at).collect();
        let (standard, omitted) =
            bound_timeline_steps(TaskNotePublishMode::Standard, steps.clone());
        assert_eq!(standard.len(), 12);
        assert_eq!(omitted, 3);
        let (diagnostic, omitted) = bound_timeline_steps(TaskNotePublishMode::Diagnostic, steps);
        assert_eq!(diagnostic.len(), 15);
        assert_eq!(omitted, 0);
    }

    /// The page path, the projection date, and the tags are all derived from
    /// the canonical task — not from the later publish time.
    #[test]
    fn task_pages_are_dated_slugged_and_tagged_from_the_task() {
        let task = projection_sample_task();
        assert_eq!(
            task_note_path(&task, "2026-08-01"),
            "Tasks/2026-08-01/task-t1-research-launch-plan.md"
        );
        let mut untitled = task.clone();
        untitled.manifest.title = "???".to_string();
        assert_eq!(
            task_note_path(&untitled, "2026-08-01"),
            "Tasks/2026-08-01/task-t1.md"
        );

        // Completion time wins, then source-updated, never the publish time.
        assert_eq!(
            task_note_date(Some("2026-08-01T10:16:59Z"), "2026-07-01T10:17:00Z"),
            "2026-08-01"
        );
        assert_eq!(task_note_date(None, "2026-07-01T10:17:00Z"), "2026-07-01");
        let today = task_note_date(None, "not-a-timestamp");
        assert_eq!(
            NaiveDate::parse_from_str(&today, "%Y-%m-%d").ok(),
            Some(Utc::now().date_naive())
        );

        let tags = task_note_tags(&task, "2026-08-01");
        for expected in [
            "magician",
            "magician/task",
            "date/2026-08-01",
            "task/completed",
            "agent/research-agent",
            "lifecycle/persistent",
            "priority/p2",
            "due/2026-08-03",
            "task-tag/launch-plan",
        ] {
            assert!(
                tags.contains(&expected.to_string()),
                "missing tag {expected}"
            );
        }
        let mut sorted = tags.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(tags, sorted, "tags are sorted and deduped");
    }

    /// The rendered page keeps the structure every reader relies on:
    /// frontmatter, banner, final answer, timeline with visible omissions,
    /// and provenance — with compact dropping the timeline entirely.
    #[test]
    fn a_standard_page_renders_its_frontmatter_answer_timeline_and_provenance() {
        let task = projection_sample_task();
        let tags = task_note_tags(&task, "2026-08-01");
        let timeline = vec![timeline_entry("exec-1", 2)];

        let standard = render_task_note_markdown(
            &task,
            TaskNotePublishMode::Standard,
            "2026-08-01",
            Some("2026-08-01T10:16:59Z"),
            "2026-08-02T08:00:00Z",
            &tags,
            Some("Three validated channels."),
            &[],
            &timeline,
            &[],
        );
        assert!(standard.contains("magician_schema: \"magician.task-note.v1\""));
        assert!(standard.contains("task_id: \"task-t1\""));
        assert!(standard.contains("mode: \"standard\""));
        assert!(standard.contains("date: \"2026-08-01\""));
        // The heading is escaped, not rewritten.
        assert!(standard.contains("# Research &amp; launch plan"));
        assert!(standard
            .contains("> completed · 2026-08-01T10:16:59Z · published 2026-08-02T08:00:00Z"));
        assert!(standard.contains("## Goal\n\nBuild the brief."));
        assert!(standard.contains("## Final answer\n\nThree validated channels."));
        assert!(standard.contains("## Run timeline"));
        assert!(standard.contains("`research-agent`"));
        assert!(standard.contains("2026-08-01T10:00:00Z"));
        assert!(standard.contains("Validate launch channels"));
        assert!(standard.contains("_2 additional steps omitted in standard mode_"));
        assert!(standard.contains("## Provenance"));

        let compact = render_task_note_markdown(
            &task,
            TaskNotePublishMode::Compact,
            "2026-08-01",
            Some("2026-08-01T10:16:59Z"),
            "2026-08-02T08:00:00Z",
            &tags,
            Some("Three validated channels."),
            &[],
            &timeline,
            &[],
        );
        assert!(!compact.contains("## Run timeline"));
        assert!(!compact.contains("additional steps omitted"));
        assert!(compact.contains("## Final answer"));
    }

    /// Copied asset names carry a slug of their output id plus a short
    /// digest, and only text a page can embed counts as embeddable.
    #[test]
    fn asset_names_slug_and_digest_their_output_and_only_text_embeds() {
        let output = OutputRef {
            output_id: "Out Final".to_string(),
            scope: "task".to_string(),
            audience: "user".to_string(),
            role: "user_media".to_string(),
            relative_path: "outputs/Result.PNG".to_string(),
            media_type: "image/png".to_string(),
            created_at: "2026-08-01T10:16:00Z".to_string(),
            source_execution_id: None,
            source_plan_id: None,
            source_output_ids: Vec::new(),
        };
        let name = task_note_asset_file_name(&output);
        assert!(name.starts_with("out-final-"), "got {name}");
        assert!(name.ends_with(".png"), "got {name}");
        assert_eq!(name.len(), "out-final-".len() + 8 + ".png".len());

        assert!(output_media_is_embeddable("text/markdown; charset=utf-8"));
        assert!(output_media_is_embeddable("application/json"));
        assert!(!output_media_is_embeddable("image/png"));
    }

    /// The promotion body drops frontmatter and the volatile publication
    /// banner, so an idempotent re-projection promotes the same memory, and
    /// the candidate id follows that stable body rather than the publication.
    #[test]
    fn promotion_values_ignore_volatile_publication_metadata_and_bound_length() {
        let first = "---\npublished_at: one\n---\n\n# Note\n\n> completed · source · published one\n\n## Final answer\n\nDurable fact";
        let second = "---\npublished_at: two\n---\n\n# Note\n\n> completed · source · published two\n\n## Final answer\n\nDurable fact";
        let first_value = bounded_note_memory_value(first, 10_000);
        let second_value = bounded_note_memory_value(second, 10_000);
        assert_eq!(first_value, second_value);
        assert!(!first_value.contains("published one"));
        assert_eq!(bounded_note_memory_value("abcdef", 3), "abc");

        assert_eq!(
            task_note_memory_candidate_id("task-1", "source-1", &first_value),
            task_note_memory_candidate_id("task-1", "source-1", &second_value)
        );
        let id = task_note_memory_candidate_id("task-1", "source-1", "different");
        assert_ne!(
            task_note_memory_candidate_id("task-1", "source-1", &first_value),
            id
        );
        assert!(id.starts_with("lc_note_"));
        assert_eq!(id.len(), "lc_note_".len() + 40);
    }
}

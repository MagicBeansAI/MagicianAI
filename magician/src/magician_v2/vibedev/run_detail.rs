//! A run, read the way a phone reads it: turns, each turn's timeline, and
//! changes one file at a time.
//!
//! The desktop cockpit folds the raw `coding.*` stream in the browser
//! (`spineModel.ts`). Two native apps would each need their own port of that
//! parser, and they would drift; so the fold lives here once and the mobile
//! endpoints serve its result (`docs/archive/plans/2026-10-13-vibedev-mobile-companion.md`):
//!
//! * [`run_turns`] — the run's root and threaded follow-ups, each with its
//!   request, mode, engine, status, cost, summary and (trusted) commit and
//!   changed-file count.
//! * [`turn_timeline`] — one turn's durable `coding.*` log folded into compact
//!   cards: messages, reasoning, one line per tool step, errors, the end.
//! * [`run_changes`] — the files a run (or one turn) changed, with counts,
//!   and one file's patch on request: from the trusted branch, or from the
//!   stored diff proposals of a sandboxed run.
//! * [`suggest_repos`] — repositories a new project could point at.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;

use super::projects::{parse_vibedev_project_id, read_vibedev_projects, VibeDevProjectRecord};
use super::run_pages::fold_into_runs;
use super::run_service::VIBEDEV_RUN_PLAN_TAG;
use super::trusted_workspace::{self, TrustedRunWorkspace};
use crate::magician_v2::agents::runtime::{VIBEDEV_USER_PROMPT_BEGIN, VIBEDEV_USER_PROMPT_END};
use crate::magician_v2::archived_filter::ArchivedFilter;
use crate::magician_v2::artifact_v2::models::TaskListItemV3;
use crate::magician_v2::artifact_v2::{ArtifactV2Service, ScopeRef};

/// Most timeline cards one read returns.
pub const MAX_TIMELINE_CARDS: usize = 400;
/// Coding events read to build one turn's timeline.
const TIMELINE_EVENT_WINDOW: usize = 4_000;
const CARD_TEXT_CHARS: usize = 2_000;
const TOOL_LINE_CHARS: usize = 160;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunDetailError {
    NotFound(String),
    Failed(String),
}

impl std::fmt::Display for RunDetailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(message) | Self::Failed(message) => f.write_str(message),
        }
    }
}

// ─── Pure folds ─────────────────────────────────────────────────────────────

/// The owner's own words: the text inside the request fence of a run's
/// description, else the description's first non-empty line.
pub fn request_from_description(description: &str) -> String {
    if let Some((_, rest)) = description.split_once(VIBEDEV_USER_PROMPT_BEGIN) {
        let rest = rest.split_once('\n').map(|(_, body)| body).unwrap_or(rest);
        let body = match rest.rfind(VIBEDEV_USER_PROMPT_END) {
            Some(end) => &rest[..end],
            None => rest,
        };
        let body = body.trim_end_matches(|c: char| c == '>' || c.is_whitespace());
        let body = body.trim();
        if !body.is_empty() {
            return body.to_string();
        }
    }
    description
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineCardKind {
    /// What the agent said.
    Message,
    /// What it was thinking; shown collapsed.
    Reasoning,
    /// One tool step.
    Action,
    Error,
    /// The turn ended.
    Completed,
    /// Something about the turn (queued, handed to another engine, budget).
    Notice,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineCard {
    /// Position in the turn's timeline (stable for a given log).
    pub index: usize,
    pub kind: TimelineCardKind,
    /// `running | ok | error` for an action; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// One line: the tool and its target, or the card's headline.
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub at_ms: i64,
}

fn clip(text: &str, chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= chars {
        return text.to_string();
    }
    let cut: String = text.chars().take(chars).collect();
    format!("{cut}…")
}

/// Cap without trimming, so joined fragments keep their spaces.
fn cap(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_string();
    }
    let cut: String = text.chars().take(chars).collect();
    format!("{cut}…")
}

fn one_line(text: &str, chars: usize) -> String {
    clip(
        &text.split_whitespace().collect::<Vec<_>>().join(" "),
        chars,
    )
}

/// `read src/app.ts`, `bash npm test`, … from a tool call's arguments.
pub fn tool_line(tool: &str, args: &Value) -> String {
    let target = [
        "path",
        "file_path",
        "filePath",
        "command",
        "cmd",
        "pattern",
        "query",
        "url",
        "description",
    ]
    .iter()
    .find_map(|key| args.get(*key).and_then(Value::as_str))
    .map(str::to_string)
    .or_else(|| {
        args.as_object()
            .and_then(|map| map.values().find_map(Value::as_str))
            .map(str::to_string)
    })
    .unwrap_or_default();
    let tool = if tool.trim().is_empty() {
        "tool"
    } else {
        tool.trim()
    };
    if target.trim().is_empty() {
        tool.to_string()
    } else {
        one_line(&format!("{tool} {target}"), TOOL_LINE_CHARS)
    }
}

fn event_parts(event: &Value) -> (&str, &Value, i64) {
    let kind = event
        .get("event_type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let data = event.get("data").unwrap_or(&Value::Null);
    let at = event
        .get("timestamp_ms")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    (kind, data, at)
}

fn text_of(data: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| data.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// Fold a turn's durable `coding.*` events (`{event_type, data, timestamp_ms}`,
/// deltas already coalesced) into timeline cards, oldest first.
pub fn fold_timeline(events: &[Value]) -> Vec<TimelineCard> {
    let mut cards: Vec<TimelineCard> = Vec::new();
    // tool_call_id → card index, so a finish updates its start.
    let mut open_tools: BTreeMap<String, usize> = BTreeMap::new();
    let push = |cards: &mut Vec<TimelineCard>,
                kind: TimelineCardKind,
                status: Option<&str>,
                title: String,
                text: Option<String>,
                at: i64| {
        let index = cards.len();
        cards.push(TimelineCard {
            index,
            kind,
            status: status.map(str::to_string),
            title,
            text,
            at_ms: at,
        });
        index
    };
    for event in events {
        let (kind, data, at) = event_parts(event);
        match kind {
            "coding.message" => {
                // Raw, not trimmed: a fragment's leading space is the word
                // boundary with the one before it.
                let raw = data
                    .get("delta")
                    .and_then(Value::as_str)
                    .filter(|text| !text.trim().is_empty())
                    .map(str::to_string);
                if let Some(text) = raw {
                    // Consecutive message records continue one message.
                    let continues = cards
                        .last()
                        .is_some_and(|card| card.kind == TimelineCardKind::Message);
                    if continues {
                        let last = cards.last_mut().expect("checked above");
                        let joined = format!("{}{}", last.text.clone().unwrap_or_default(), text);
                        last.title = one_line(&joined, 120);
                        last.text = Some(cap(&joined, CARD_TEXT_CHARS));
                    } else {
                        push(
                            &mut cards,
                            TimelineCardKind::Message,
                            None,
                            one_line(&text, 120),
                            Some(cap(&text, CARD_TEXT_CHARS)),
                            at,
                        );
                    }
                }
            },
            "coding.thinking" => {
                if let Some(text) = text_of(data, &["delta"]) {
                    push(
                        &mut cards,
                        TimelineCardKind::Reasoning,
                        None,
                        one_line(&text, 120),
                        Some(clip(&text, CARD_TEXT_CHARS)),
                        at,
                    );
                }
            },
            "coding.tool.started" => {
                let tool = data
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let args = data.get("args").unwrap_or(&Value::Null);
                let index = push(
                    &mut cards,
                    TimelineCardKind::Action,
                    Some("running"),
                    tool_line(tool, args),
                    None,
                    at,
                );
                if let Some(id) = data.get("tool_call_id").and_then(Value::as_str) {
                    open_tools.insert(id.to_string(), index);
                }
            },
            "coding.tool.finished" => {
                let failed = data
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let result = data.get("result").map(|result| match result {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                });
                let opened = data
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .and_then(|id| open_tools.remove(id));
                match opened.and_then(|index| cards.get_mut(index)) {
                    Some(card) => {
                        card.status = Some(if failed { "error" } else { "ok" }.to_string());
                        if failed {
                            card.text = result.map(|text| clip(&text, 400));
                        }
                    },
                    None => {
                        let tool = data
                            .get("tool_name")
                            .and_then(Value::as_str)
                            .unwrap_or("tool");
                        push(
                            &mut cards,
                            TimelineCardKind::Action,
                            Some(if failed { "error" } else { "ok" }),
                            tool.to_string(),
                            None,
                            at,
                        );
                    },
                }
            },
            "coding.failed" | "coding.session_too_large" => {
                let text = text_of(data, &["error", "error_message", "message", "reason"])
                    .unwrap_or_else(|| "The turn failed.".to_string());
                push(
                    &mut cards,
                    TimelineCardKind::Error,
                    None,
                    one_line(&text, 120),
                    Some(clip(&text, CARD_TEXT_CHARS)),
                    at,
                );
            },
            "coding.completed" => {
                let text = text_of(data, &["assistant_text", "summary"]);
                let title = if data.get("pending_approval").and_then(Value::as_bool) == Some(true) {
                    "Finished; changes wait for your approval"
                } else if data.get("no_change").and_then(Value::as_bool) == Some(true) {
                    "Finished without changes"
                } else {
                    "Finished"
                };
                push(
                    &mut cards,
                    TimelineCardKind::Completed,
                    None,
                    title.to_string(),
                    text.map(|text| clip(&text, CARD_TEXT_CHARS)),
                    at,
                );
            },
            "coding.queued" => {
                push(
                    &mut cards,
                    TimelineCardKind::Notice,
                    None,
                    "Waiting for a free slot".to_string(),
                    None,
                    at,
                );
            },
            "coding.handoff" => {
                let engine = text_of(data, &["to_engine", "engine"]).unwrap_or_default();
                push(
                    &mut cards,
                    TimelineCardKind::Notice,
                    None,
                    one_line(&format!("Handed to {engine}"), 120),
                    None,
                    at,
                );
            },
            "coding.budget_exhausted" => {
                push(
                    &mut cards,
                    TimelineCardKind::Notice,
                    None,
                    "Stopped at its budget".to_string(),
                    None,
                    at,
                );
            },
            _ => {},
        }
    }
    cards
}

/// The engine that ran a turn, from its events.
pub fn engine_of(events: &[Value]) -> Option<String> {
    events.iter().find_map(|event| {
        event
            .get("data")
            .and_then(|data| data.get("engine"))
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

/// The diff proposals a sandboxed turn staged, from its events.
pub fn proposal_ids(events: &[Value]) -> Vec<String> {
    let mut seen = HashSet::new();
    events
        .iter()
        .filter_map(|event| event.get("data")?.get("proposal_id")?.as_str())
        .filter(|id| seen.insert(id.to_string()))
        .map(str::to_string)
        .collect()
}

// ─── Run turns ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunTurn {
    pub task_id: String,
    /// The owner's request for this turn.
    pub prompt: String,
    /// `build | discuss`.
    pub mode: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// Generated catalog route used by this turn, for follow-up settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coding_route_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Trusted runs: the commit the turn ended on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Trusted runs: files the turn changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_changed: Option<usize>,
    /// `partial` when the turn finished with requirements open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_kind: Option<crate::magician_v2::artifact_v2::models::CompletionKind>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub open_items: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunTurns {
    pub run_id: String,
    pub project_id: String,
    pub project_name: String,
    pub title: String,
    pub status: String,
    pub trusted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crew_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    /// Root first, then follow-ups in the order they were made.
    pub turns: Vec<RunTurn>,
}

fn scope_root(service: &ArtifactV2Service, scope: &ScopeRef) -> PathBuf {
    service
        .workspace()
        .scope_root(&scope.principal(), &scope.workspace())
}

/// A run's project, its root and its turns (root first), found from any of
/// its task ids.
async fn locate_run(
    service: &ArtifactV2Service,
    scope: &ScopeRef,
    task_id: &str,
) -> Result<(VibeDevProjectRecord, TaskListItemV3, Vec<TaskListItemV3>), RunDetailError> {
    let item = service
        .get_task_list_item(scope, task_id)
        .await
        .map_err(|error| RunDetailError::NotFound(format!("no run {task_id}: {error}")))?;
    let project_id = parse_vibedev_project_id(&item.description)
        .ok_or_else(|| RunDetailError::NotFound(format!("{task_id} is not a VibeDev run")))?;
    let project = read_vibedev_projects(&scope_root(service, scope))
        .into_iter()
        .find(|project| project.project_id == project_id)
        .ok_or_else(|| RunDetailError::NotFound(format!("no VibeDev project {project_id}")))?;
    let mut ids = project.run_task_ids.clone();
    if !ids.iter().any(|id| id == task_id) {
        ids.push(task_id.to_string());
    }
    let items = service
        .list_task_items_by_ids_archive_filtered(scope, &ids, ArchivedFilter::Include)
        .await
        .map_err(|error| RunDetailError::Failed(error.to_string()))?;
    let (root, turns) = {
        let (roots, turns) = fold_into_runs(&items, &project.run_links);
        let root = roots
            .into_iter()
            .find(|root| {
                root.id == task_id
                    || turns
                        .get(&root.id)
                        .is_some_and(|folded| folded.iter().any(|turn| turn.id == task_id))
            })
            .cloned()
            .ok_or_else(|| RunDetailError::NotFound(format!("no run holds {task_id}")))?;
        let mut folded: Vec<TaskListItemV3> = turns
            .get(&root.id)
            .map(|folded| folded.iter().map(|turn| (*turn).clone()).collect())
            .unwrap_or_default();
        folded.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        (root, folded)
    };
    Ok((project, root, turns))
}

fn task_cost_usd(service: &ArtifactV2Service, scope: &ScopeRef, task_id: &str) -> Option<f64> {
    use crate::magician_v2::execution::coding_engine::cost_budget::CodingCostLedger;
    let dir = service
        .workspace()
        .task_dir(&scope.principal(), &scope.workspace(), task_id);
    let micro = CodingCostLedger::open(&dir).spent_micro_usd();
    (micro > 0).then(|| micro as f64 / 1_000_000.0)
}

/// The commit range a turn of a trusted chain covers: from the previous
/// turn's last commit (or the chain's base) to this turn's last commit.
fn turn_range(workspace: &TrustedRunWorkspace, turn_task_id: &str) -> Option<(String, String)> {
    let last = workspace
        .turns
        .iter()
        .rposition(|turn| turn.task_id == turn_task_id)?;
    let to = workspace.turns[last].commit.clone();
    let first = workspace
        .turns
        .iter()
        .position(|turn| turn.task_id == turn_task_id)?;
    let from = if first == 0 {
        workspace.base_sha.clone()
    } else {
        workspace.turns[first - 1].commit.clone()
    };
    Some((from, to))
}

/// A run's turns, from any of its task ids.
pub async fn run_turns(
    service: &Arc<ArtifactV2Service>,
    scope: &ScopeRef,
    task_id: &str,
) -> Result<RunTurns, RunDetailError> {
    let (project, root, followups) = locate_run(service, scope, task_id).await?;
    let root_path = scope_root(service, scope);
    let workspace = trusted_workspace::load_run_workspace(&root_path, &root.id);
    let link = project.run_links.get(&root.id).cloned().unwrap_or_default();
    let handoff = crate::magician_v2::execution::coding_engine::run_handoff::load(
        &scope_root(service, scope),
        &root.id,
    );
    let mut turns = Vec::new();
    for item in std::iter::once(&root).chain(followups.iter()) {
        let events = service
            .read_run_coding_events(scope, &item.id, 8)
            .await
            .unwrap_or_default();
        let range = workspace
            .as_ref()
            .and_then(|workspace| turn_range(workspace, &item.id));
        let files_changed = match (workspace.clone(), range.clone()) {
            (Some(workspace), Some((from, to))) => tokio::task::spawn_blocking(move || {
                trusted_workspace::changed_files_between(&workspace, &from, &to)
                    .map(|files| files.len())
                    .ok()
            })
            .await
            .ok()
            .flatten(),
            _ => None,
        };
        let discuss = item.tags.iter().any(|tag| tag.name == VIBEDEV_RUN_PLAN_TAG);
        turns.push(RunTurn {
            task_id: item.id.clone(),
            prompt: request_from_description(&item.description),
            mode: if discuss { "discuss" } else { "build" }.to_string(),
            status: item.status.clone(),
            created_at: item.created_at.clone(),
            updated_at: item.updated_at.clone(),
            summary: item
                .completion_summary
                .as_deref()
                .map(str::trim)
                .filter(|summary| !summary.is_empty())
                .map(str::to_string),
            engine: engine_of(&events),
            coding_route_id: handoff
                .turns
                .iter()
                .rev()
                .find(|turn| turn.task_id == item.id)
                .and_then(|turn| turn.profile_id.clone()),
            cost_usd: task_cost_usd(service, scope, &item.id),
            commit: range.map(|(_, to)| to),
            files_changed,
            completion_kind: item.completion_kind.clone(),
            open_items: item.open_items.clone(),
        });
    }
    let latest = turns
        .last()
        .map(|turn| turn.status.clone())
        .unwrap_or_default();
    Ok(RunTurns {
        run_id: root.id.clone(),
        project_id: project.project_id.clone(),
        project_name: project.name.clone(),
        title: root.title.clone(),
        status: latest,
        work_location: if super::original_workspace::load(&root_path, &root.id)
            .ok()
            .flatten()
            .is_some()
        {
            Some("original".into())
        } else {
            link.work_location.as_ref().map(|location| {
                match location.mode {
                    super::projects::VibeDevProjectTrust::Original => "original",
                    super::projects::VibeDevProjectTrust::Trusted => "trusted",
                    super::projects::VibeDevProjectTrust::Sandboxed => "sandboxed",
                }
                .into()
            })
        },
        trusted: workspace.is_some(),
        branch: workspace.map(|workspace| workspace.branch),
        crew_id: link.crew_id,
        lane: link.lane,
        turns,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TurnTimeline {
    pub task_id: String,
    pub cards: Vec<TimelineCard>,
    /// Older cards exist before the first one returned.
    pub truncated: bool,
}

/// One turn's timeline: its newest `limit` cards.
pub async fn turn_timeline(
    service: &Arc<ArtifactV2Service>,
    scope: &ScopeRef,
    task_id: &str,
    limit: usize,
) -> Result<TurnTimeline, RunDetailError> {
    let events = service
        .read_run_coding_events(scope, task_id, TIMELINE_EVENT_WINDOW)
        .await
        .map_err(|error| RunDetailError::Failed(error.to_string()))?;
    let mut cards = fold_timeline(&events);
    let limit = limit.clamp(1, MAX_TIMELINE_CARDS);
    let truncated = cards.len() > limit || events.len() >= TIMELINE_EVENT_WINDOW;
    if cards.len() > limit {
        cards.drain(..cards.len() - limit);
    }
    Ok(TurnTimeline {
        task_id: task_id.to_string(),
        cards,
        truncated,
    })
}

// ─── Changes ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangedFile {
    pub path: String,
    /// `A | M | D | R…` (git name-status letters).
    pub status: String,
    pub additions: usize,
    pub deletions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FilePatch {
    pub path: String,
    pub unified_diff: String,
    /// Cut at the review size cap; open it on the desktop for all of it.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunChanges {
    pub run_id: String,
    /// The turn the changes are limited to, when one was asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<String>,
    /// `branch` (trusted) or `proposals` (sandboxed).
    pub source: String,
    pub files: Vec<ChangedFile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<FilePatch>,
}

/// What a run (or one of its turns) changed, and one file's patch when
/// `path` is given.
pub async fn run_changes(
    service: &Arc<ArtifactV2Service>,
    scope: &ScopeRef,
    task_id: &str,
    turn: Option<&str>,
    path: Option<&str>,
) -> Result<RunChanges, RunDetailError> {
    let (_project, root, followups) = locate_run(service, scope, task_id).await?;
    let turn_ids: Vec<String> = std::iter::once(&root)
        .chain(followups.iter())
        .map(|item| item.id.clone())
        .collect();
    if let Some(turn) = turn {
        if !turn_ids.iter().any(|id| id == turn) {
            return Err(RunDetailError::NotFound(format!(
                "{turn} is not a turn of this run"
            )));
        }
    }
    let root_path = scope_root(service, scope);
    if super::original_workspace::load(&root_path, &root.id)
        .map_err(RunDetailError::Failed)?
        .is_some()
    {
        let history = crate::magician_v2::execution::coding_engine::original_edits::applied_history(
            &root_path, &root.id,
        );
        let mut changed = std::collections::BTreeMap::<String, ChangedFile>::new();
        let mut diff = String::new();
        for txn in history
            .into_iter()
            .filter(|txn| turn.is_none_or(|id| txn.task_id.as_deref() == Some(id)))
        {
            for file in txn.diff_approval_files() {
                if path == Some(file.path.as_str()) {
                    diff.push_str(&file.unified_diff);
                }
                let row = changed.entry(file.path.clone()).or_insert(ChangedFile {
                    path: file.path,
                    status: file.status,
                    additions: 0,
                    deletions: 0,
                });
                row.additions += file.additions;
                row.deletions += file.deletions;
            }
        }
        let patch = path.map(|path| {
            let truncated = diff.len() > 128 * 1024;
            if truncated {
                let mut end = 128 * 1024;
                while !diff.is_char_boundary(end) {
                    end -= 1;
                }
                diff.truncate(end);
            }
            FilePatch {
                path: path.into(),
                unified_diff: diff,
                truncated,
            }
        });
        return Ok(RunChanges {
            run_id: root.id.clone(),
            turn: turn.map(str::to_string),
            source: "original".into(),
            files: changed.into_values().collect(),
            patch,
        });
    }
    if let Some(workspace) = trusted_workspace::load_run_workspace(&root_path, &root.id) {
        let (from, to) = match turn {
            Some(turn) => turn_range(&workspace, turn).ok_or_else(|| {
                RunDetailError::NotFound(format!("turn {turn} has no commit yet"))
            })?,
            None => (workspace.base_sha.clone(), "HEAD".to_string()),
        };
        let path = path.map(str::to_string);
        let outcome = tokio::task::spawn_blocking(move || {
            let files = trusted_workspace::changed_files_between(&workspace, &from, &to)?;
            let patch = match path {
                Some(path) => {
                    let (unified_diff, truncated) =
                        trusted_workspace::file_patch_between(&workspace, &from, &to, &path)?;
                    Some(FilePatch {
                        path,
                        unified_diff,
                        truncated,
                    })
                },
                None => None,
            };
            Ok::<_, String>((files, patch))
        })
        .await
        .map_err(|error| RunDetailError::Failed(error.to_string()))?
        .map_err(RunDetailError::Failed)?;
        return Ok(RunChanges {
            run_id: root.id.clone(),
            turn: turn.map(str::to_string),
            source: "branch".to_string(),
            files: outcome
                .0
                .into_iter()
                .map(|file| ChangedFile {
                    path: file.path,
                    status: file.status,
                    additions: file.additions,
                    deletions: file.deletions,
                })
                .collect(),
            patch: outcome.1,
        });
    }

    // Sandboxed: the diff proposals the turns staged, newest file version wins.
    let mut ids = Vec::new();
    for id in turn_ids
        .iter()
        .filter(|id| turn.is_none_or(|turn| turn == id.as_str()))
    {
        let events = service
            .read_run_coding_events(scope, id, TIMELINE_EVENT_WINDOW)
            .await
            .unwrap_or_default();
        ids.extend(proposal_ids(&events));
    }
    // Proposal files are read and parsed off the reactor, and only the asked
    // file's diff is kept.
    let wanted = path.map(str::to_string);
    let (files, patch) =
        tokio::task::spawn_blocking(move || proposal_changes(&root_path, &ids, wanted.as_deref()))
            .await
            .map_err(|error| RunDetailError::Failed(error.to_string()))?;
    Ok(RunChanges {
        run_id: root.id.clone(),
        turn: turn.map(str::to_string),
        source: "proposals".to_string(),
        files,
        patch,
    })
}

/// The same review size cap as a trusted branch's per-file patch.
const MAX_PROPOSAL_PATCH_BYTES: usize = 256 * 1024;

/// The files `proposal_ids` change (the newest proposal's version of each
/// file wins) and `path`'s patch, cut at the review size cap.
fn proposal_changes(
    root_path: &Path,
    proposal_ids: &[String],
    path: Option<&str>,
) -> (Vec<ChangedFile>, Option<FilePatch>) {
    use crate::magician_v2::execution::file_edit::proposal::{
        CodeChangeProposalId, CodeChangeProposalStore,
    };
    let store = CodeChangeProposalStore::new(root_path);
    let mut by_path: BTreeMap<String, (ChangedFile, chrono::DateTime<chrono::Utc>)> =
        BTreeMap::new();
    let mut patch: Option<(String, chrono::DateTime<chrono::Utc>)> = None;
    for proposal_id in proposal_ids {
        let Ok(parsed) = CodeChangeProposalId::parse(proposal_id) else {
            continue;
        };
        let Ok(proposal) = store.load(&parsed) else {
            continue;
        };
        for file in proposal.files {
            let newer = by_path
                .get(&file.path)
                .is_none_or(|(_, at)| proposal.created_at >= *at);
            if !newer {
                continue;
            }
            if path == Some(file.path.as_str()) {
                patch = Some((file.unified_diff, proposal.created_at));
            }
            by_path.insert(
                file.path.clone(),
                (
                    ChangedFile {
                        path: file.path,
                        status: file.status,
                        additions: file.additions,
                        deletions: file.deletions,
                    },
                    proposal.created_at,
                ),
            );
        }
    }
    let patch = path.zip(patch).map(|(path, (mut unified_diff, _))| {
        let truncated = unified_diff.len() > MAX_PROPOSAL_PATCH_BYTES;
        if truncated {
            let mut cut = MAX_PROPOSAL_PATCH_BYTES;
            while !unified_diff.is_char_boundary(cut) {
                cut -= 1;
            }
            unified_diff.truncate(cut);
        }
        FilePatch {
            path: path.to_string(),
            unified_diff,
            truncated,
        }
    });
    (by_path.into_values().map(|(file, _)| file).collect(), patch)
}

// ─── Repo suggestions ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoSuggestion {
    /// Absolute path to the repository.
    pub path: String,
    /// The folder name, for the chip.
    pub name: String,
    /// The project already using it, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// Most suggestions one read returns.
pub const MAX_REPO_SUGGESTIONS: usize = 30;

fn resolve_repo(path: Option<&str>, home: &Path) -> PathBuf {
    match path
        .map(str::trim)
        .filter(|path| !path.is_empty() && *path != ".")
    {
        Some(path) if Path::new(path).is_absolute() => PathBuf::from(path),
        Some(path) => home.join(path),
        None => home.to_path_buf(),
    }
}

/// Repositories a new project could use: those existing projects use, then
/// git repositories directly under `home` or one folder below it.
pub fn suggest_repos(projects: &[VibeDevProjectRecord], home: &Path) -> Vec<RepoSuggestion> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut add = |path: PathBuf, project: Option<String>, out: &mut Vec<RepoSuggestion>| {
        let key = path.to_string_lossy().into_owned();
        if !seen.insert(key.clone()) {
            return;
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| key.clone());
        out.push(RepoSuggestion {
            path: key,
            name,
            project,
        });
    };
    for project in projects.iter().filter(|project| !project.archived) {
        add(
            resolve_repo(project.repo_path.as_deref(), home),
            Some(project.name.clone()),
            &mut out,
        );
    }
    let mut found = Vec::new();
    let level = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.is_dir()
                            && !path
                                .file_name()
                                .is_some_and(|name| name.to_string_lossy().starts_with('.'))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    for child in level(home) {
        if child.join(".git").exists() {
            found.push(child);
            continue;
        }
        for grandchild in level(&child) {
            if grandchild.join(".git").exists() {
                found.push(grandchild);
            }
        }
    }
    found.sort();
    for path in found {
        add(path, None, &mut out);
    }
    out.truncate(MAX_REPO_SUGGESTIONS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(kind: &str, data: Value, at: i64) -> Value {
        json!({ "event_type": kind, "data": data, "timestamp_ms": at })
    }

    #[test]
    fn the_request_comes_out_of_its_fence() {
        let description = format!(
            "VibeDev coding request:\nOriginal VibeDev user prompt:\n{VIBEDEV_USER_PROMPT_BEGIN}\nAdd a\nlogin form\n{VIBEDEV_USER_PROMPT_END}\n\nVibeDev project: p"
        );
        assert_eq!(request_from_description(&description), "Add a\nlogin form");
        assert_eq!(
            request_from_description("  \nFix the footer\nmore"),
            "Fix the footer"
        );
    }

    #[test]
    fn a_turn_log_folds_into_cards() {
        let events = vec![
            ev("coding.started", json!({ "engine": "pi" }), 1),
            ev(
                "coding.thinking",
                json!({ "delta": "The footer uses a fixed height." }),
                2,
            ),
            ev(
                "coding.tool.started",
                json!({ "tool_name": "read", "tool_call_id": "t1", "args": { "path": "src/footer.css" } }),
                3,
            ),
            ev(
                "coding.tool.finished",
                json!({ "tool_name": "read", "tool_call_id": "t1", "is_error": false, "result": "…" }),
                4,
            ),
            ev(
                "coding.tool.started",
                json!({ "tool_name": "bash", "tool_call_id": "t2", "args": { "command": "npm   test\n --silent" } }),
                5,
            ),
            ev(
                "coding.tool.finished",
                json!({ "tool_name": "bash", "tool_call_id": "t2", "is_error": true, "result": "1 failing" }),
                6,
            ),
            ev("coding.message", json!({ "delta": "Fixed the " }), 7),
            ev("coding.message", json!({ "delta": "footer spacing." }), 8),
            ev("coding.stats", json!({ "tokens": 10 }), 9),
            ev(
                "coding.completed",
                json!({ "assistant_text": "Done.", "pending_approval": true }),
                10,
            ),
        ];
        let cards = fold_timeline(&events);
        let kinds: Vec<TimelineCardKind> = cards.iter().map(|card| card.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TimelineCardKind::Reasoning,
                TimelineCardKind::Action,
                TimelineCardKind::Action,
                TimelineCardKind::Message,
                TimelineCardKind::Completed,
            ]
        );
        assert_eq!(cards[1].title, "read src/footer.css");
        assert_eq!(cards[1].status.as_deref(), Some("ok"));
        assert_eq!(cards[2].title, "bash npm test --silent");
        assert_eq!(cards[2].status.as_deref(), Some("error"));
        assert_eq!(cards[2].text.as_deref(), Some("1 failing"));
        assert_eq!(cards[3].text.as_deref(), Some("Fixed the footer spacing."));
        assert_eq!(cards[4].title, "Finished; changes wait for your approval");
        assert_eq!(
            cards.iter().map(|card| card.index).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4]
        );
        assert_eq!(engine_of(&events).as_deref(), Some("pi"));
    }

    #[test]
    fn a_tool_without_a_start_still_shows_and_failures_say_why() {
        let events = vec![
            ev(
                "coding.tool.finished",
                json!({ "tool_name": "edit", "is_error": false }),
                1,
            ),
            ev("coding.failed", json!({ "error": "the engine exited" }), 2),
            ev(
                "coding.completed",
                json!({ "proposal_id": "ccp-1", "no_change": true }),
                3,
            ),
            ev(
                "coding.approval_requested",
                json!({ "proposal_id": "ccp-1" }),
                4,
            ),
        ];
        let cards = fold_timeline(&events);
        assert_eq!(cards[0].title, "edit");
        assert_eq!(cards[1].kind, TimelineCardKind::Error);
        assert_eq!(cards[1].text.as_deref(), Some("the engine exited"));
        assert_eq!(cards[2].title, "Finished without changes");
        assert_eq!(proposal_ids(&events), vec!["ccp-1".to_string()]);
        assert_eq!(
            tool_line("grep", &json!({ "pattern": "TODO" })),
            "grep TODO"
        );
        assert_eq!(tool_line("", &json!({})), "tool");
    }

    #[test]
    fn repo_suggestions_list_projects_then_nearby_repositories() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join("shop/.git")).unwrap();
        std::fs::create_dir_all(home.join("code/billing/.git")).unwrap();
        std::fs::create_dir_all(home.join("notes")).unwrap();
        std::fs::create_dir_all(home.join(".hidden/.git")).unwrap();
        let project: VibeDevProjectRecord = serde_json::from_value(json!({
            "project_id": "p", "name": "Shop", "chat_thread_id": "t", "chat_session_id": "s",
            "repo_path": "shop", "created_at_ms": 0, "updated_at_ms": 0
        }))
        .unwrap();
        let suggestions = suggest_repos(&[project], home);
        let names: Vec<(&str, Option<&str>)> = suggestions
            .iter()
            .map(|s| (s.name.as_str(), s.project.as_deref()))
            .collect();
        assert_eq!(names, vec![("shop", Some("Shop")), ("billing", None)]);
    }
}

//! Magician at work: the owner's long-running work, one list for every surface.
//!
//! The phone summary (iOS Live Activity, Android ongoing notification) and the
//! desktop notch ear show the same items in the same order: VibeDev coding
//! runs, VibeDev crews (one item each; their lanes stay inside) and the
//! owner's Work tasks. One item is featured, chosen by tiers — needs the
//! owner (longest waiting first), followed or started from a phone, just
//! failed, working (owner-started before scheduled or background work), just
//! done — and held for [`FEATURE_HOLD_MS`] unless something needs the owner or
//! fails, so the summary does not flicker. Plan:
//! `docs/plans/2026-10-15-magician-at-work-summary.md`.
//!
//! Like `vibedev::live_runs`, every read derives the items from stored rows,
//! never from events, so a restart reports exactly what the previous process
//! did. Two small files sit beside the task rows: a task's `origin.json` (who
//! started it, from which surface) and the scope's followed items.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::DateTime;
use serde::{Deserialize, Serialize};

use crate::magician_v2::archived_filter::ArchivedFilter;
use crate::magician_v2::artifact_v2::models::TaskListItemV3;
use crate::magician_v2::artifact_v2::service::PendingDiffApprovals;
use crate::magician_v2::artifact_v2::{ArtifactV2Service, ScopeRef, V3ReadApi};
use crate::magician_v2::vibedev::crews::{CrewStatus, VibeDevCrew};
use crate::magician_v2::vibedev::live_runs::{self, LiveRun, LiveRunState};
use crate::magician_v2::vibedev::projects::{
    parse_vibedev_project_id, read_vibedev_projects, VibeDevProjectRecord,
};

/// Most items one read returns.
pub const MAX_ITEMS: usize = 50;
/// Most items the owner can follow at once.
pub const MAX_FOLLOWED: usize = 3;
/// A Work task shows once it has run this long (or needs the owner), so a
/// quick task does not flash on the lock screen.
pub const LONG_RUNNING_FLOOR_MS: i64 = 60 * 1_000;
/// How long a just-failed or just-done item ranks as such.
pub const JUST_ENDED_MS: i64 = 2 * 60 * 1_000;
/// How long the featured item is held against a lower-urgency newcomer.
pub const FEATURE_HOLD_MS: i64 = 5 * 60 * 1_000;
/// How long an ended item stays listed (and counted).
pub const RECENT_WINDOW_MS: i64 = 30 * 60 * 1_000;
/// How long a followed item that ended stays followed.
pub const FOLLOW_AFTER_END_MS: i64 = 10 * 60 * 1_000;
/// Items listed after the featured one in the summary.
pub const NEXT_ITEMS: usize = 2;

const SETTLED: [&str; 4] = ["completed", "failed", "cancelled", "archived"];
/// Task statuses that are not running yet (queued, scheduled or waiting for
/// a timer): a task in one shows only when it needs the owner.
const NOT_STARTED: [&str; 7] = [
    "pending",
    "ready",
    "requested",
    "draft",
    "scheduled",
    "deferred",
    "sleeping",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkKind {
    CodingRun,
    Crew,
    WorkTask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    NeedsYou,
    Working,
    Paused,
    Failed,
    Stopped,
    Done,
}

impl WorkState {
    pub fn is_ended(self) -> bool {
        matches!(self, Self::Failed | Self::Stopped | Self::Done)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeedsYou => "needs_you",
            Self::Working => "working",
            Self::Paused => "paused",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
            Self::Done => "done",
        }
    }
}

impl From<LiveRunState> for WorkState {
    fn from(state: LiveRunState) -> Self {
        match state {
            LiveRunState::NeedsYou => Self::NeedsYou,
            LiveRunState::Working => Self::Working,
            LiveRunState::Paused => Self::Paused,
            LiveRunState::Failed => Self::Failed,
            LiveRunState::Stopped => Self::Stopped,
            LiveRunState::Done => Self::Done,
        }
    }
}

impl WorkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CodingRun => "coding_run",
            Self::Crew => "crew",
            Self::WorkTask => "work_task",
        }
    }
}

/// What the owner can do about an item without opening it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkAction {
    /// `approve_diff` (answer with `POST /hitl/{id}/respond`) or `answer`
    /// (a pending user request).
    pub kind: String,
    pub id: String,
}

/// One item of long-running work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkItem {
    /// `run:<run id>`, `crew:<crew id>` or `task:<task id>`.
    pub id: String,
    pub kind: WorkKind,
    pub title: String,
    /// The project's name; empty for a Work task.
    pub context: String,
    pub state: WorkState,
    #[serde(default)]
    pub step: Option<String>,
    /// The owner started it (not a schedule, a monitor or an agent).
    pub by_owner: bool,
    /// Started from a phone (iOS, Android or Siri).
    pub from_phone: bool,
    pub followed: bool,
    pub started_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub waiting_since: Option<String>,
    #[serde(default)]
    pub spent_usd: Option<f64>,
    #[serde(default)]
    pub budget_usd: Option<f64>,
    #[serde(default)]
    pub action: Option<WorkAction>,
    pub deep_link: String,
    pub web_path: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub live_task_id: Option<String>,
    #[serde(default)]
    pub crew_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
}

impl WorkItem {
    fn updated_ms(&self) -> i64 {
        rfc3339_ms(&self.updated_at)
    }

    fn waiting_ms(&self) -> i64 {
        self.waiting_since
            .as_deref()
            .map(rfc3339_ms)
            .unwrap_or_else(|| self.updated_ms())
    }

    /// One line for a lock screen: "title · project".
    pub fn headline(&self) -> String {
        if self.context.is_empty() {
            self.title.clone()
        } else {
            format!("{} · {}", self.title, self.context)
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct WorkCounts {
    pub needs_you: u32,
    /// Working and paused.
    pub working: u32,
    pub failed: u32,
    pub done: u32,
}

/// `GET /work/live`.
#[derive(Debug, Clone, Serialize)]
pub struct WorkLive {
    pub featured: Option<WorkItem>,
    pub next: Vec<WorkItem>,
    pub counts: WorkCounts,
    pub items: Vec<WorkItem>,
    pub followed: Vec<String>,
    pub generated_at: String,
}

impl WorkLive {
    /// Items ranked after `featured` and `next`.
    pub fn more(&self) -> u32 {
        let shown = usize::from(self.featured.is_some()) + self.next.len();
        let active = self
            .items
            .iter()
            .filter(|item| !item.state.is_ended())
            .count();
        active.saturating_sub(shown) as u32
    }
}

fn rfc3339_ms(value: &str) -> i64 {
    DateTime::parse_from_rfc3339(value)
        .map(|at| at.timestamp_millis())
        .unwrap_or(0)
}

fn ms_rfc3339(ms: i64) -> String {
    DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn one_line(text: &str, limit: usize) -> String {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
    line.chars().take(limit).collect()
}

/// A run's title without the `VibeDev · ` / `VibeDev follow-up · ` prefix its
/// task carries (the web's `stripRunTitlePrefix`): a lock screen line is short.
pub fn strip_run_title_prefix(title: &str) -> &str {
    title
        .strip_prefix("VibeDev follow-up · ")
        .or_else(|| title.strip_prefix("VibeDev · "))
        .unwrap_or(title)
}

fn non_empty(value: Option<&String>) -> Option<String> {
    value
        .map(|text| text.trim())
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Origin: who started a task, and from where.
// ---------------------------------------------------------------------------

/// Who started a piece of work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginBy {
    Owner,
    Schedule,
    Monitor,
    Agent,
    App,
    System,
}

/// `origin.json` in a task's folder, written where the owner starts work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkOrigin {
    pub by: OriginBy,
    /// `web`, `desktop`, `ios`, `android` or `siri`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    pub at_ms: i64,
}

impl WorkOrigin {
    pub fn from_phone(&self) -> bool {
        matches!(self.surface.as_deref(), Some("ios" | "android" | "siri"))
    }
}

pub const ORIGIN_FILE: &str = "origin.json";

/// The surface a client's `source` names (`ios_siri_intent` → `siri`).
pub fn surface_from_source(source: &str) -> Option<String> {
    let source = source.trim().to_ascii_lowercase();
    let surface = if source.contains("siri") {
        "siri"
    } else if source.contains("ios") || source.contains("iphone") {
        "ios"
    } else if source.contains("android") {
        "android"
    } else if source.contains("desktop") || source.contains("tray") {
        "desktop"
    } else if source.contains("web") {
        "web"
    } else {
        return None;
    };
    Some(surface.to_string())
}

/// Record who started the task in `task_dir`. Best effort for callers: a
/// missing origin falls back to [`infer_origin`].
pub fn write_task_origin(task_dir: &Path, origin: &WorkOrigin) -> std::io::Result<()> {
    std::fs::create_dir_all(task_dir)?;
    let bytes = serde_json::to_vec(origin).map_err(std::io::Error::other)?;
    crate::magician_v2::artifact_v2::io::write_bytes_atomic_sync(&task_dir.join(ORIGIN_FILE), &bytes)
        .map_err(std::io::Error::other)
}

pub fn read_task_origin(task_dir: &Path) -> Option<WorkOrigin> {
    let bytes = std::fs::read(task_dir.join(ORIGIN_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Who started a task without an `origin.json`, from what its row says.
pub fn infer_origin(item: &TaskListItemV3) -> OriginBy {
    if item.monitor_revision > 0 {
        return OriginBy::Monitor;
    }
    if item.schedule.as_ref().is_some_and(|schedule| !schedule.is_null()) {
        return OriginBy::Schedule;
    }
    origin_by_created_by(&item.created_by)
}

/// Who started a task, from its free-text `created_by`.
pub fn origin_by_created_by(created_by: &str) -> OriginBy {
    let created_by = created_by.trim().to_ascii_lowercase();
    match created_by.as_str() {
        "" | "user" | "owner" | "chat" | "chat_delegate" | "chat_vibedev_rail"
        | "vibedev_crew" | "ambient" => OriginBy::Owner,
        "app_action" => OriginBy::App,
        other if other.starts_with("system") => OriginBy::System,
        _ => OriginBy::Agent,
    }
}

// ---------------------------------------------------------------------------
// Follow: at most `MAX_FOLLOWED` items per scope.
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Serialize, Deserialize)]
struct FollowFile {
    #[serde(default)]
    items: Vec<FollowedEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FollowedEntry {
    id: String,
    at_ms: i64,
    /// When the item was first seen ended (or gone).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ended_at_ms: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum FollowError {
    #[error("at most 3 items can be followed")]
    Limit,
    #[error("not a work item id")]
    Invalid,
    #[error("follow store: {0}")]
    Io(#[from] std::io::Error),
}

/// Serializes follow-file writes in this process (one server owns a data root).
static FOLLOW_LOCK: Mutex<()> = Mutex::new(());

fn follow_path(scope_root: &Path) -> PathBuf {
    scope_root.join("work").join("followed.json")
}

fn read_follow(scope_root: &Path) -> FollowFile {
    std::fs::read(follow_path(scope_root))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_follow(scope_root: &Path, file: &FollowFile) -> std::io::Result<()> {
    let path = follow_path(scope_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(file).map_err(std::io::Error::other)?;
    crate::magician_v2::artifact_v2::io::write_bytes_atomic_sync(&path, &bytes).map_err(std::io::Error::other)
}

/// Whether `id` is a work item id (`run:`, `crew:` or `task:` and a plain id).
pub fn is_item_id(id: &str) -> bool {
    let Some((kind, rest)) = id.split_once(':') else {
        return false;
    };
    matches!(kind, "run" | "crew" | "task")
        && !rest.is_empty()
        && rest.len() <= 200
        && rest
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

pub fn followed_ids(scope_root: &Path) -> Vec<String> {
    read_follow(scope_root)
        .items
        .into_iter()
        .map(|entry| entry.id)
        .collect()
}

pub fn follow(scope_root: &Path, item_id: &str, now_ms: i64) -> Result<Vec<String>, FollowError> {
    if !is_item_id(item_id) {
        return Err(FollowError::Invalid);
    }
    let _guard = FOLLOW_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut file = read_follow(scope_root);
    if !file.items.iter().any(|entry| entry.id == item_id) {
        if file.items.len() >= MAX_FOLLOWED {
            return Err(FollowError::Limit);
        }
        file.items.push(FollowedEntry {
            id: item_id.to_string(),
            at_ms: now_ms,
            ended_at_ms: None,
        });
        write_follow(scope_root, &file)?;
    }
    Ok(file.items.into_iter().map(|entry| entry.id).collect())
}

pub fn unfollow(scope_root: &Path, item_id: &str) -> Result<Vec<String>, FollowError> {
    let _guard = FOLLOW_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut file = read_follow(scope_root);
    let before = file.items.len();
    file.items.retain(|entry| entry.id != item_id);
    if file.items.len() != before {
        write_follow(scope_root, &file)?;
    }
    Ok(file.items.into_iter().map(|entry| entry.id).collect())
}

/// Note which followed items ended (or left the list) and drop the ones that
/// ended more than [`FOLLOW_AFTER_END_MS`] ago. Returns the followed ids.
fn settle_followed(scope_root: &Path, items: &[WorkItem], now_ms: i64) -> Vec<String> {
    let _guard = FOLLOW_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut file = read_follow(scope_root);
    if file.items.is_empty() {
        return Vec::new();
    }
    let by_id: HashMap<&str, &WorkItem> = items.iter().map(|item| (item.id.as_str(), item)).collect();
    let mut changed = false;
    for entry in &mut file.items {
        let ended = by_id
            .get(entry.id.as_str())
            .is_none_or(|item| item.state.is_ended());
        match (ended, entry.ended_at_ms) {
            (true, None) => {
                entry.ended_at_ms = Some(now_ms);
                changed = true;
            },
            (false, Some(_)) => {
                entry.ended_at_ms = None;
                changed = true;
            },
            _ => {},
        }
    }
    let before = file.items.len();
    file.items.retain(|entry| {
        entry
            .ended_at_ms
            .is_none_or(|ended| now_ms.saturating_sub(ended) <= FOLLOW_AFTER_END_MS)
    });
    changed |= file.items.len() != before;
    if changed {
        let _ = write_follow(scope_root, &file);
    }
    file.items.into_iter().map(|entry| entry.id).collect()
}

// ---------------------------------------------------------------------------
// Items.
// ---------------------------------------------------------------------------

/// A pending user request: its id and when it was raised.
#[derive(Debug, Clone)]
pub struct WaitingRequest {
    pub id: String,
    pub created_at_ms: i64,
}

/// What [`build_items`] reads, gathered by [`scope_work_live`] (or a test).
pub struct WorkInputs<'a> {
    pub projects: &'a [VibeDevProjectRecord],
    /// The coding runs of `projects` (`vibedev::live_runs`), costs and
    /// approvals filled.
    pub runs: &'a [LiveRun],
    /// The owner's visible tasks (`ArtifactV2Service::list_tasks`).
    pub tasks: &'a [TaskListItemV3],
    /// Pending user requests by task id, oldest first.
    pub requests: &'a HashMap<String, Vec<WaitingRequest>>,
    /// Pending user requests by request id.
    pub requests_by_id: &'a HashMap<String, WaitingRequest>,
    pub approvals: Option<&'a PendingDiffApprovals>,
    /// A task's origin, when recorded.
    pub origin: &'a dyn Fn(&str) -> Option<WorkOrigin>,
    pub followed: &'a HashSet<String>,
    pub now_ms: i64,
}

fn oldest_request<'a>(
    requests: &'a HashMap<String, Vec<WaitingRequest>>,
    task_ids: &[&str],
) -> Option<&'a WaitingRequest> {
    task_ids
        .iter()
        .filter_map(|task_id| requests.get(*task_id))
        .flatten()
        .min_by_key(|request| request.created_at_ms)
}

fn run_item(run: &LiveRun, inputs: &WorkInputs<'_>, roots: &HashMap<&str, &TaskListItemV3>) -> WorkItem {
    let state = WorkState::from(run.state);
    let task_ids = [run.run_id.as_str(), run.live_task_id.as_str()];
    let request = oldest_request(inputs.requests, &task_ids);
    let origin = (inputs.origin)(run.run_id.as_str());
    let by_owner = match (&origin, roots.get(run.run_id.as_str())) {
        (Some(origin), _) => origin.by == OriginBy::Owner,
        (None, Some(root)) => infer_origin(root) == OriginBy::Owner,
        (None, None) => true,
    };
    let action = run
        .approval_id
        .as_ref()
        .map(|id| WorkAction {
            kind: "approve_diff".to_string(),
            id: id.clone(),
        })
        .or_else(|| {
            request.map(|request| WorkAction {
                kind: "answer".to_string(),
                id: request.id.clone(),
            })
        });
    let id = format!("run:{}", run.run_id);
    WorkItem {
        followed: inputs.followed.contains(&id),
        id,
        kind: WorkKind::CodingRun,
        title: one_line(
            strip_run_title_prefix(run.latest_turn_title.as_deref().unwrap_or(&run.title)),
            120,
        ),
        context: run.project_name.clone(),
        state,
        step: if state.is_ended() {
            run.summary.clone()
        } else {
            run.step.clone()
        },
        by_owner,
        from_phone: origin.as_ref().is_some_and(WorkOrigin::from_phone),
        started_at: run.started_at.clone(),
        updated_at: run.updated_at.clone(),
        waiting_since: (state == WorkState::NeedsYou).then(|| {
            request
                .map(|request| ms_rfc3339(request.created_at_ms))
                .unwrap_or_else(|| run.updated_at.clone())
        }),
        spent_usd: run.cost_usd,
        budget_usd: None,
        action,
        deep_link: format!(
            "magican://vibe?project={}&run={}",
            run.project_id, run.run_id
        ),
        web_path: format!("/coding-runs?run={}", run.run_id),
        project_id: Some(run.project_id.clone()),
        run_id: Some(run.run_id.clone()),
        live_task_id: Some(run.live_task_id.clone()),
        crew_id: None,
        task_id: None,
    }
}

fn crew_item(
    project: &VibeDevProjectRecord,
    crew: &VibeDevCrew,
    lanes: &[&LiveRun],
    inputs: &WorkInputs<'_>,
) -> Option<WorkItem> {
    let open_request = [
        crew.start_request_id.as_ref(),
        crew.choose_request_id.as_ref(),
        crew.resolve_request_id.as_ref(),
        crew.pull_request_request_id.as_ref(),
    ]
    .into_iter()
    .flatten()
    .next();
    let lane_waiting = lanes.iter().find(|run| run.state == LiveRunState::NeedsYou);
    let state = match crew.status {
        CrewStatus::Proposed if open_request.is_some() => WorkState::NeedsYou,
        CrewStatus::Running if lane_waiting.is_some() => WorkState::NeedsYou,
        CrewStatus::Proposed | CrewStatus::Running | CrewStatus::Ready => WorkState::Working,
        CrewStatus::Choosing | CrewStatus::Conflict => WorkState::NeedsYou,
        CrewStatus::Integrated if open_request.is_some() => WorkState::NeedsYou,
        CrewStatus::Integrated => WorkState::Done,
        CrewStatus::Failed => WorkState::Failed,
        CrewStatus::Cancelled => WorkState::Stopped,
    };
    if state.is_ended()
        && inputs.now_ms.saturating_sub(crew.updated_at_ms) > RECENT_WINDOW_MS
    {
        return None;
    }
    let done_lanes = crew.lanes.iter().filter(|lane| lane.status.is_ended()).count();
    let step = non_empty(crew.note.as_ref()).or_else(|| {
        (!crew.lanes.is_empty()).then(|| format!("{done_lanes} of {} lanes done", crew.lanes.len()))
    });
    let request = open_request.and_then(|id| inputs.requests_by_id.get(id));
    let action = lane_waiting
        .and_then(|run| run.approval_id.as_ref())
        .map(|id| WorkAction {
            kind: "approve_diff".to_string(),
            id: id.clone(),
        })
        .or_else(|| {
            open_request.map(|id| WorkAction {
                kind: "answer".to_string(),
                id: id.clone(),
            })
        });
    let waiting_since = (state == WorkState::NeedsYou).then(|| {
        request
            .map(|request| request.created_at_ms)
            .or_else(|| lane_waiting.map(|run| rfc3339_ms(&run.updated_at)))
            .map(ms_rfc3339)
            .unwrap_or_else(|| ms_rfc3339(crew.updated_at_ms))
    });
    let id = format!("crew:{}", crew.crew_id);
    Some(WorkItem {
        followed: inputs.followed.contains(&id),
        id,
        kind: WorkKind::Crew,
        title: one_line(&crew.goal, 120),
        context: project.name.clone(),
        state,
        step,
        // Crews are created by the owner (the rail, a phone or the API).
        by_owner: true,
        from_phone: false,
        started_at: ms_rfc3339(crew.created_at_ms),
        updated_at: ms_rfc3339(crew.updated_at_ms),
        waiting_since,
        spent_usd: crew.spent_usd,
        budget_usd: crew.budget_usd,
        action,
        deep_link: format!(
            "magican://vibe?project={}&crew={}",
            project.project_id, crew.crew_id
        ),
        web_path: format!("/vibe?project={}&crew={}", project.project_id, crew.crew_id),
        project_id: Some(project.project_id.clone()),
        run_id: None,
        live_task_id: None,
        crew_id: Some(crew.crew_id.clone()),
        task_id: None,
    })
}

fn task_item(task: &TaskListItemV3, inputs: &WorkInputs<'_>) -> Option<WorkItem> {
    let settled = SETTLED.contains(&task.status.as_str());
    let request = oldest_request(inputs.requests, &[task.id.as_str()]);
    let waits = task.awaiting_diff_approval
        || request.is_some()
        || (!settled && (task.pending_question.is_some() || !task.pending_questions.is_empty()));
    let state = if waits {
        WorkState::NeedsYou
    } else if settled {
        match task.status.as_str() {
            "failed" => WorkState::Failed,
            "cancelled" | "archived" => WorkState::Stopped,
            _ => WorkState::Done,
        }
    } else if NOT_STARTED.contains(&task.status.as_str()) {
        return None;
    } else if task.status == "paused" {
        WorkState::Paused
    } else {
        WorkState::Working
    };
    let updated_ms = rfc3339_ms(&task.updated_at);
    if state.is_ended() && inputs.now_ms.saturating_sub(updated_ms) > RECENT_WINDOW_MS {
        return None;
    }
    // The long-running floor: a task shows once it has run for a minute.
    if matches!(state, WorkState::Working | WorkState::Paused)
        && inputs.now_ms.saturating_sub(rfc3339_ms(&task.created_at)) < LONG_RUNNING_FLOOR_MS
    {
        return None;
    }
    let origin = (inputs.origin)(task.id.as_str());
    let by_owner = origin
        .as_ref()
        .map(|origin| origin.by)
        .unwrap_or_else(|| infer_origin(task))
        == OriginBy::Owner;
    let approval = task
        .awaiting_diff_approval
        .then(|| {
            inputs.approvals.and_then(|approvals| {
                let unclaimed: HashSet<String> = approvals
                    .unclaimed_pending()
                    .iter()
                    .map(|proposal| proposal.id.to_string())
                    .collect();
                approvals
                    .proposal_ids_for_task(&task.id)
                    .iter()
                    .find(|id| unclaimed.contains(*id))
                    .cloned()
            })
        })
        .flatten();
    let action = approval
        .map(|id| WorkAction {
            kind: "approve_diff".to_string(),
            id,
        })
        .or_else(|| {
            request.map(|request| WorkAction {
                kind: "answer".to_string(),
                id: request.id.clone(),
            })
        });
    let id = format!("task:{}", task.id);
    Some(WorkItem {
        followed: inputs.followed.contains(&id),
        id,
        kind: WorkKind::WorkTask,
        title: one_line(&task.title, 120),
        context: String::new(),
        state,
        step: if state.is_ended() {
            non_empty(task.completion_summary.as_ref())
        } else {
            non_empty(task.current_step_title.as_ref())
        },
        by_owner,
        from_phone: origin.as_ref().is_some_and(WorkOrigin::from_phone),
        started_at: task.created_at.clone(),
        updated_at: task.updated_at.clone(),
        waiting_since: (state == WorkState::NeedsYou).then(|| {
            request
                .map(|request| ms_rfc3339(request.created_at_ms))
                .unwrap_or_else(|| task.updated_at.clone())
        }),
        spent_usd: None,
        budget_usd: None,
        action,
        // The task link both phone apps already open.
        deep_link: format!("magican://task/{}", task.id),
        web_path: format!("/work?task={}", task.id),
        project_id: None,
        run_id: None,
        live_task_id: None,
        crew_id: None,
        task_id: Some(task.id.clone()),
    })
}

/// Every item of `inputs`, unranked: coding runs that are not crew lanes,
/// one item per crew, and Work tasks that are not VibeDev runs.
pub fn build_items(inputs: &WorkInputs<'_>) -> Vec<WorkItem> {
    let roots: HashMap<&str, &TaskListItemV3> =
        inputs.tasks.iter().map(|task| (task.id.as_str(), task)).collect();
    let mut items = Vec::new();
    let mut lanes_by_crew: HashMap<&str, Vec<&LiveRun>> = HashMap::new();
    for run in inputs.runs {
        match run.crew_id.as_deref() {
            Some(crew_id) => lanes_by_crew.entry(crew_id).or_default().push(run),
            None => items.push(run_item(run, inputs, &roots)),
        }
    }
    let mut vibedev_task_ids: HashSet<&str> = HashSet::new();
    for project in inputs.projects.iter().filter(|project| !project.archived) {
        vibedev_task_ids.extend(project.run_task_ids.iter().map(String::as_str));
        if let Some(active) = project.active_root_task_id.as_deref() {
            vibedev_task_ids.insert(active);
        }
        for crew in &project.crews {
            vibedev_task_ids.extend(crew.lanes.iter().filter_map(|lane| lane.run_task_id.as_deref()));
            let lanes = lanes_by_crew
                .get(crew.crew_id.as_str())
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            if let Some(item) = crew_item(project, crew, lanes, inputs) {
                items.push(item);
            }
        }
    }
    for task in inputs.tasks {
        if vibedev_task_ids.contains(task.id.as_str())
            || parse_vibedev_project_id(&task.description).is_some()
        {
            continue;
        }
        if let Some(item) = task_item(task, inputs) {
            items.push(item);
        }
    }
    items
}

// ---------------------------------------------------------------------------
// Ranking.
// ---------------------------------------------------------------------------

/// The item's tier: 1 needs you, 2 followed or from a phone, 3 just failed,
/// 4 working, 5 just done, 6 the rest (ended earlier, or stopped).
pub fn tier(item: &WorkItem, now_ms: i64) -> u8 {
    let recent = now_ms.saturating_sub(item.updated_ms()) <= JUST_ENDED_MS;
    match item.state {
        WorkState::NeedsYou => 1,
        WorkState::Working | WorkState::Paused if item.followed || item.from_phone => 2,
        WorkState::Failed if recent => 3,
        WorkState::Working | WorkState::Paused => 4,
        WorkState::Done if recent => 5,
        _ => 6,
    }
}

fn kind_order(kind: WorkKind) -> u8 {
    match kind {
        WorkKind::CodingRun | WorkKind::Crew => 0,
        WorkKind::WorkTask => 1,
    }
}

/// `items` in rank order.
pub fn rank(mut items: Vec<WorkItem>, now_ms: i64) -> Vec<WorkItem> {
    items.sort_by(|a, b| {
        let (ta, tb) = (tier(a, now_ms), tier(b, now_ms));
        ta.cmp(&tb)
            .then_with(|| match ta {
                // Longest waiting first.
                1 => a.waiting_ms().cmp(&b.waiting_ms()),
                // Owner-started first, then coding runs and crews, then the
                // most recent progress.
                4 => b
                    .by_owner
                    .cmp(&a.by_owner)
                    .then_with(|| kind_order(a.kind).cmp(&kind_order(b.kind)))
                    .then_with(|| b.updated_ms().cmp(&a.updated_ms())),
                _ => b.updated_ms().cmp(&a.updated_ms()),
            })
            .then_with(|| a.id.cmp(&b.id))
    });
    items
}

/// The featured item per scope, and since when, for [`FEATURE_HOLD_MS`].
static FEATURED: Mutex<Option<HashMap<String, (String, i64)>>> = Mutex::new(None);

/// Pick the featured item from `ranked`, holding the scope's previous one
/// against a newcomer for [`FEATURE_HOLD_MS`] unless the newcomer needs the
/// owner or just failed.
fn pick_featured(scope_key: &str, ranked: &[WorkItem], now_ms: i64) -> Option<usize> {
    let top = ranked.iter().position(|item| tier(item, now_ms) <= 5)?;
    let mut guard = FEATURED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let held = guard.get_or_insert_with(HashMap::new);
    let top_tier = tier(&ranked[top], now_ms);
    let chosen = match held.get(scope_key) {
        Some((previous, since)) if *previous != ranked[top].id => {
            let keep = ranked.iter().position(|item| {
                item.id == *previous && matches!(tier(item, now_ms), 2 | 4 | 5)
            });
            match keep {
                Some(index)
                    if !matches!(top_tier, 1 | 3)
                        && now_ms.saturating_sub(*since) < FEATURE_HOLD_MS =>
                {
                    return Some(index);
                },
                _ => top,
            }
        },
        Some(_) => return Some(top),
        None => top,
    };
    held.insert(scope_key.to_string(), (ranked[chosen].id.clone(), now_ms));
    Some(chosen)
}

/// Rank `items` and shape the summary.
pub fn summarize(scope_key: &str, items: Vec<WorkItem>, followed: Vec<String>, now_ms: i64) -> WorkLive {
    let mut ranked = rank(items, now_ms);
    let featured_index = pick_featured(scope_key, &ranked, now_ms);
    let featured = featured_index.map(|index| ranked[index].clone());
    let next: Vec<WorkItem> = ranked
        .iter()
        .enumerate()
        .filter(|(index, item)| Some(*index) != featured_index && tier(item, now_ms) <= 5)
        .map(|(_, item)| item.clone())
        .take(NEXT_ITEMS)
        .collect();
    let mut counts = WorkCounts::default();
    for item in &ranked {
        match item.state {
            WorkState::NeedsYou => counts.needs_you += 1,
            WorkState::Working | WorkState::Paused => counts.working += 1,
            WorkState::Failed => counts.failed += 1,
            WorkState::Done => counts.done += 1,
            WorkState::Stopped => {},
        }
    }
    ranked.truncate(MAX_ITEMS);
    WorkLive {
        featured,
        next,
        counts,
        items: ranked,
        followed,
        generated_at: ms_rfc3339(now_ms),
    }
}

// ---------------------------------------------------------------------------
// One scope, read from storage.
// ---------------------------------------------------------------------------

fn scope_key(scope: &ScopeRef) -> String {
    format!("{}\u{1f}{}", scope.principal(), scope.workspace())
}

/// The scope's work summary, read from its stored rows.
pub async fn scope_work_live(service: &ArtifactV2Service, scope: &ScopeRef) -> Result<WorkLive, String> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let scope_root = service
        .workspace()
        .scope_root(&scope.principal(), &scope.workspace());
    let projects = read_vibedev_projects(&scope_root);
    let mut pending: HashMap<String, Vec<WaitingRequest>> = HashMap::new();
    for (task_id, id, created_at_ms) in service.pending_user_request_stamps(scope).await {
        pending.entry(task_id).or_default().push(WaitingRequest { id, created_at_ms });
    }
    for requests in pending.values_mut() {
        requests.sort_by_key(|request| request.created_at_ms);
    }
    let waiting: HashSet<String> = pending.keys().cloned().collect();
    let run_items = service
        .list_task_items_by_ids_archive_filtered(
            scope,
            &live_runs::project_task_ids(&projects, &waiting),
            ArchivedFilter::Exclude,
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut runs = live_runs::live_runs(
        &projects,
        &run_items,
        &waiting,
        now_ms,
        live_runs::DEFAULT_RECENT_WINDOW_MS,
    );
    live_runs::fill_costs(&mut runs, service, scope);
    let approvals = service.pending_diff_approvals_off_reactor(scope).await;
    live_runs::fill_approvals(&mut runs, &approvals);
    let tasks = service.list_tasks(scope).await.map_err(|error| error.to_string())?;
    let requests_by_id: HashMap<String, WaitingRequest> = pending
        .values()
        .flatten()
        .map(|request| (request.id.clone(), request.clone()))
        .collect();
    let workspace = service.workspace();
    let (principal, scope_workspace) = (scope.principal(), scope.workspace());
    let origin = |task_id: &str| -> Option<WorkOrigin> {
        read_task_origin(&workspace.task_dir(&principal, &scope_workspace, task_id))
    };
    let followed_before: HashSet<String> = followed_ids(&scope_root).into_iter().collect();
    let inputs = WorkInputs {
        projects: &projects,
        runs: &runs,
        tasks: &tasks,
        requests: &pending,
        requests_by_id: &requests_by_id,
        approvals: Some(&approvals),
        origin: &origin,
        followed: &followed_before,
        now_ms,
    };
    let items = build_items(&inputs);
    let followed = settle_followed(&scope_root, &items, now_ms);
    Ok(summarize(&scope_key(scope), items, followed, now_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;

    #[test]
    fn ready_tasks_only_appear_when_the_owner_is_needed() {
        let mut task: TaskListItemV3 = serde_json::from_value(serde_json::json!({
            "id": "ready", "title": "Ready task", "description": "", "status": "ready",
            "agent_id": "assistant", "created_at": ms_rfc3339(NOW - 600_000),
            "updated_at": ms_rfc3339(NOW - 300_000)
        }))
        .unwrap();
        let requests = HashMap::new();
        let followed = HashSet::new();
        let inputs = WorkInputs {
            projects: &[],
            runs: &[],
            tasks: &[],
            requests: &requests,
            requests_by_id: &HashMap::new(),
            approvals: None,
            origin: &|_| None,
            followed: &followed,
            now_ms: NOW,
        };
        assert!(task_item(&task, &inputs).is_none());
        task.awaiting_diff_approval = true;
        assert_eq!(
            task_item(&task, &inputs).unwrap().state,
            WorkState::NeedsYou
        );
        task.awaiting_diff_approval = false;
        task.status = "running".into();
        assert_eq!(task_item(&task, &inputs).unwrap().state, WorkState::Working);
    }

    fn item(id: &str, kind: WorkKind, state: WorkState, updated_ago_ms: i64) -> WorkItem {
        WorkItem {
            id: id.to_string(),
            kind,
            title: id.to_string(),
            context: String::new(),
            state,
            step: None,
            by_owner: true,
            from_phone: false,
            followed: false,
            started_at: ms_rfc3339(NOW - 60 * 60 * 1_000),
            updated_at: ms_rfc3339(NOW - updated_ago_ms),
            waiting_since: None,
            spent_usd: None,
            budget_usd: None,
            action: None,
            deep_link: String::new(),
            web_path: String::new(),
            project_id: None,
            run_id: None,
            live_task_id: None,
            crew_id: None,
            task_id: None,
        }
    }

    #[test]
    fn needs_you_ranks_first_longest_waiting_first() {
        let mut older = item("task:a", WorkKind::WorkTask, WorkState::NeedsYou, 0);
        older.waiting_since = Some(ms_rfc3339(NOW - 10 * 60 * 1_000));
        let mut newer = item("run:b", WorkKind::CodingRun, WorkState::NeedsYou, 0);
        newer.waiting_since = Some(ms_rfc3339(NOW - 60 * 1_000));
        let working = item("crew:c", WorkKind::Crew, WorkState::Working, 0);
        let ranked = rank(vec![working, newer, older], NOW);
        let ids: Vec<&str> = ranked.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["task:a", "run:b", "crew:c"]);
    }

    #[test]
    fn working_items_rank_owner_started_first_then_runs_and_crews() {
        let mut scheduled_run = item("run:s", WorkKind::CodingRun, WorkState::Working, 0);
        scheduled_run.by_owner = false;
        let owner_task = item("task:o", WorkKind::WorkTask, WorkState::Working, 0);
        let owner_crew = item("crew:o", WorkKind::Crew, WorkState::Working, 60 * 1_000);
        let ranked = rank(vec![scheduled_run, owner_task, owner_crew], NOW);
        let ids: Vec<&str> = ranked.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["crew:o", "task:o", "run:s"]);
    }

    #[test]
    fn tiers_follow_the_plan() {
        let mut followed = item("run:f", WorkKind::CodingRun, WorkState::Working, 0);
        followed.followed = true;
        assert_eq!(tier(&followed, NOW), 2);
        assert_eq!(tier(&item("run:x", WorkKind::CodingRun, WorkState::Failed, 60 * 1_000), NOW), 3);
        assert_eq!(tier(&item("run:x", WorkKind::CodingRun, WorkState::Failed, 10 * 60 * 1_000), NOW), 6);
        assert_eq!(tier(&item("run:x", WorkKind::CodingRun, WorkState::Done, 60 * 1_000), NOW), 5);
        assert_eq!(tier(&item("run:x", WorkKind::CodingRun, WorkState::Stopped, 0), NOW), 6);
    }

    #[test]
    fn the_featured_item_is_held_unless_something_needs_the_owner() {
        let scope = "test-hold";
        let first = item("run:first", WorkKind::CodingRun, WorkState::Working, 0);
        let live = summarize(scope, vec![first.clone()], Vec::new(), NOW);
        assert_eq!(live.featured.as_ref().map(|item| item.id.as_str()), Some("run:first"));

        // A newer working run ranks first, but the featured one is held.
        let newer = item("run:newer", WorkKind::CodingRun, WorkState::Working, 0);
        let mut first_later = first.clone();
        first_later.updated_at = ms_rfc3339(NOW - 30 * 1_000);
        let live = summarize(scope, vec![first_later.clone(), newer.clone()], Vec::new(), NOW + 60 * 1_000);
        assert_eq!(live.featured.as_ref().map(|item| item.id.as_str()), Some("run:first"));
        assert_eq!(live.next.first().map(|item| item.id.as_str()), Some("run:newer"));

        // Something that needs the owner takes over at once.
        let waiting = item("task:ask", WorkKind::WorkTask, WorkState::NeedsYou, 0);
        let live = summarize(scope, vec![first_later, newer, waiting], Vec::new(), NOW + 2 * 60 * 1_000);
        assert_eq!(live.featured.as_ref().map(|item| item.id.as_str()), Some("task:ask"));
        assert_eq!(live.counts.needs_you, 1);
        assert_eq!(live.counts.working, 2);
    }

    #[test]
    fn run_titles_lose_the_vibedev_prefix() {
        assert_eq!(strip_run_title_prefix("VibeDev · Fix the footer"), "Fix the footer");
        assert_eq!(strip_run_title_prefix("VibeDev follow-up · Add tests"), "Add tests");
        assert_eq!(strip_run_title_prefix("Fix the footer"), "Fix the footer");
    }

    #[test]
    fn origin_is_inferred_from_the_row_when_not_recorded() {
        assert_eq!(surface_from_source("ios_siri_intent").as_deref(), Some("siri"));
        assert_eq!(surface_from_source("android_app").as_deref(), Some("android"));
        assert_eq!(surface_from_source("unknown"), None);
        let origin = WorkOrigin {
            by: OriginBy::Owner,
            surface: Some("siri".to_string()),
            device_id: None,
            at_ms: NOW,
        };
        assert!(origin.from_phone());
    }

    #[test]
    fn follow_is_capped_and_ids_are_checked() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        assert!(matches!(follow(root, "nope", NOW), Err(FollowError::Invalid)));
        assert!(matches!(follow(root, "run:../x", NOW), Err(FollowError::Invalid)));
        for id in ["run:a", "crew:b", "task:c"] {
            follow(root, id, NOW).unwrap();
        }
        // Following again is a no-op; a fourth item is refused.
        assert_eq!(follow(root, "run:a", NOW).unwrap().len(), 3);
        assert!(matches!(follow(root, "task:d", NOW), Err(FollowError::Limit)));
        assert_eq!(unfollow(root, "crew:b").unwrap(), ["run:a", "task:c"]);

        // An item that ended stays followed for a while, then is dropped.
        let ended = item("run:a", WorkKind::CodingRun, WorkState::Done, 0);
        let working = item("task:c", WorkKind::WorkTask, WorkState::Working, 0);
        let items = vec![ended, working];
        assert_eq!(settle_followed(root, &items, NOW), ["run:a", "task:c"]);
        assert_eq!(
            settle_followed(root, &items, NOW + FOLLOW_AFTER_END_MS + 1),
            ["task:c"]
        );
    }
}

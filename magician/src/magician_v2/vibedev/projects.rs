//! VibeDev project store — the scoped `projects.json` substrate shared by the
//! HTTP surface (`api::vibedev_api`) and the lib-side rail/run service.
//! Extracted from `vibedev_api.rs` so lib modules stop importing from `api`
//! (the api-crate extraction prerequisite). `api::vibedev_api` re-exports
//! everything here, so historical paths keep resolving.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::magician_v2::agents::EpisodeProjectResolver;
use crate::magician_v2::artifact_v2::io::write_bytes_durably;
use crate::magician_v2::artifact_v2::{ArtifactV2Service, ScopeRef, V3ReadApi};
use crate::magician_v2::chat::models::{ChatSession, ChatSessionStatus};

/// The cockpit's own UI thread. Also the `ui_thread_id` a chat-started `@vibedev`
/// run is stamped with, so it lands in the same run history the cockpit reads
/// and satisfies `is_vibedev_coding_build_run` on the same signal the cockpit
/// does.
pub const VIBEDEV_THREAD_ID: &str = "vibedev";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VibeDevDeploymentRecord {
    pub deployment_id: String,
    pub target_id: String,
    pub target_label: String,
    pub provider: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_deployment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_command: Option<String>,
    pub output_dir: String,
    pub artifact_files: usize,
    pub artifact_bytes: u64,
    pub created_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs_tail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VibeDevProjectRecord {
    pub project_id: String,
    pub name: String,
    pub chat_thread_id: String,
    pub chat_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_root_task_id: Option<String>,
    /// Durable set of VibeDev root task ids that belong to this project. The
    /// cockpit re-pins `active_root_task_id` for each run, so this list is the
    /// project-lifetime attribution source for LLM/coding cost rollups.
    #[serde(default)]
    pub run_task_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy_url: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default)]
    pub archived: bool,
    /// Provenance (§13.3 #20): the meeting thread / chat session this project's build was first
    /// seeded from, for a durable "view originating meeting/chat" reverse link. First-source-wins
    /// — once set it is never overwritten (a project hosts many runs; the origin is fixed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_meeting_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_chat_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_url: Option<String>,
    #[serde(default)]
    pub deployments: Vec<VibeDevDeploymentRecord>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct VibeDevProjectStore {
    #[serde(default)]
    pub projects: Vec<VibeDevProjectRecord>,
}

/// Parse the `VibeDev project: <uuid>` line the cockpit embeds in EVERY coding
/// task's description — root and follow-up turns alike (`projectContextBlock` /
/// `buildCodingTaskDescription` in the UI). This is a stable, turn-INVARIANT
/// project identity, unlike `active_root_task_id` which the cockpit re-pins to
/// the latest turn on every submit. Both the write resolver and the read path
/// (`resolve_citizen_project`) recover the project UUID from this same line, so
/// they always agree.
///
/// This is a *control* line, and the description it is read from also carries
/// the user's own request verbatim. It therefore delegates to the one reader
/// every VibeDev control line shares
/// ([`extract_vibedev_line_value`](crate::magician_v2::agents::runtime::extract_vibedev_line_value)),
/// which cuts the request region out before scanning — otherwise a request
/// containing a line shaped like this one would win by being first, and would
/// bind a run, a citizen call, or a `contribute_to_project` write to a project
/// the user never selected. It also uses the shared prefix constant rather than
/// its own copy of the literal, so the two cannot drift apart.
pub fn parse_vibedev_project_id(description: &str) -> Option<String> {
    crate::magician_v2::agents::runtime::extract_vibedev_line_value(
        description,
        crate::magician_v2::agents::runtime::VIBEDEV_PROJECT_LINE_PREFIX,
    )
}

/// Write-side `EpisodeProjectResolver` for the code-knowledge distillation sweep.
///
/// Maps a coding-run `task_id` to its VibeDev project UUID by reading the task's
/// manifest and parsing the stable `VibeDev project: <uuid>` line every coding
/// task carries. This is the SAME UUID `resolve_citizen_project` recovers at read
/// time, so the read-side `project_id` filter is an exact (write == read) match —
/// and being turn-invariant, follow-up turns resolve correctly with no dependence
/// on the mutable `active_root_task_id`.
pub struct VibeDevEpisodeProjectResolver {
    pub task_service: Arc<ArtifactV2Service>,
    pub scope: ScopeRef,
    /// `task_id` -> resolved `project_id` (memoizes the lookup per sweep).
    pub cache: tokio::sync::Mutex<HashMap<String, Option<String>>>,
}

pub fn append_vibedev_run_task_id(project: &mut VibeDevProjectRecord, task_id: &str) -> bool {
    let task_id = task_id.trim();
    if task_id.is_empty() {
        return false;
    }
    if project
        .run_task_ids
        .iter()
        .any(|existing| existing == task_id)
    {
        return false;
    }
    project.run_task_ids.push(task_id.to_string());
    true
}

pub fn normalize_vibedev_project_run_task_ids(project: &mut VibeDevProjectRecord) -> bool {
    let original = project.run_task_ids.clone();
    let original_active_root_task_id = project.active_root_task_id.clone();
    let mut seen = HashSet::new();
    project.run_task_ids = original
        .iter()
        .filter_map(|task_id| {
            let trimmed = task_id.trim();
            if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .collect();
    project.active_root_task_id =
        normalized_optional_string(original_active_root_task_id.as_deref());
    let mut changed = project.run_task_ids != original
        || project.active_root_task_id != original_active_root_task_id;
    if let Some(task_id) = project.active_root_task_id.clone() {
        changed |= append_vibedev_run_task_id(project, &task_id);
    }
    changed
}

pub fn normalize_vibedev_project_store(store: &mut VibeDevProjectStore) -> bool {
    let mut changed = false;
    for project in &mut store.projects {
        changed |= normalize_vibedev_project_run_task_ids(project);
    }
    changed
}

/// Every writer of a scope's `projects.json` queues here.
///
/// The store is one file rewritten whole: load it, change one field, write it
/// back. Seven writers do that — the four project endpoints (create, activate,
/// update, delete), [`persist_vibedev_deployment`], the listing (which mints and
/// persists a record for any cockpit session lacking one, so a GET is a writer),
/// and [`set_vibedev_project_active_root_task_id`] off the run service. None of
/// them shared any state to hang a lock on: `VibeDevApi` is a per-request handle
/// and the run service has only a scope root. So nothing ordered load → mutate →
/// write, and a concurrent update was lost **wholesale** — a rename landing
/// between a pin's load and its save was simply written over, and the pin's
/// whole-store write put the old name back.
///
/// The unique temp name and the fsyncs that landed on 2026-08-11 removed the
/// *torn-file* failure, which is a different one; they cannot help here, because
/// each writer's file is individually perfect and merely stale.
///
/// Keyed by the store's own path, so two scopes never wait on each other, and a
/// `tokio::sync::Mutex` because the guarded section awaits — the endpoints talk
/// to the chat store between their load and their save, and the lock has to span
/// that or it is not spanning the read-modify-write. An in-process lock is the
/// whole fix because one process owns a data root (decided 2026-08-12).
static PROJECT_STORE_LOCKS: std::sync::OnceLock<
    std::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
> = std::sync::OnceLock::new();

pub fn project_store_lock(path: &Path) -> Arc<tokio::sync::Mutex<()>> {
    let mut registry = PROJECT_STORE_LOCKS
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(registry.entry(path.to_path_buf()).or_default())
}

/// The one place the scoped project store's location is spelled.
pub fn vibedev_project_store_path(scope_root: &Path) -> PathBuf {
    scope_root.join("vibedev").join("projects.json")
}

/// What [`set_vibedev_project_active_root_task_id`] should do to the pointer.
///
/// A typed pair rather than `Option<&str>`, because the unwind needs to say
/// *which* run it is unwinding and an `Option` had nowhere to put it. With only
/// `None` to go on, the unwind cleared whatever the pointer happened to hold —
/// see [`Self::ClearIfNames`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VibeDevProjectPointer<'a> {
    /// Make this run the project's `active_root_task_id`, and remember it in
    /// `run_task_ids`.
    PinTo(&'a str),
    /// Clear the pointer, but **only if it still names this run**.
    ///
    /// The guard is the whole point. Run A pins the pointer and crashes; the
    /// user starts run B, which pins successfully; the reconciler then unwinds
    /// A. An unconditional clear takes B's pointer with it, and the cockpit
    /// loses the run it is actually looking at over the cleanup of one that
    /// never started. A pointer that has moved on is already not naming a
    /// deleted task, which is the only property the unwind exists to keep.
    ClearIfNames(&'a str),
}

/// Point a project at the run the cockpit is looking at — or unwind a run that
/// never started.
///
/// The write half of [`read_vibedev_projects`], for the same kind of caller:
/// `VibeDevRunService`, which owns starting a cockpit run and therefore owns
/// pinning its `active_root_task_id`, has no `VibeDevApi` and nowhere to put an
/// `HttpResponse` error. It does exactly what
/// [`update_vibedev_project_handler`]'s `active_root_task_id` branch does — set
/// the pointer, remember the run in `run_task_ids` when there is one, bump
/// `updated_at_ms` — so a run pinned by the service and one pinned by the
/// endpoint leave the store in the same state.
///
/// [`VibeDevProjectPointer::ClearIfNames`] is the **unwind**, and it is the half
/// a rollback runs first: the pointer must never name a task that has just been
/// deleted. See `rollback_undispatched_vibedev_run`.
///
/// Durable the way [`save_project_store`] is — a sibling temp under a name that
/// is unique per write, fsynced, renamed over, then the directory fsynced so the
/// rename survives the crash too — and serialised against every other writer of
/// the scope's store by [`mutate_project_store_at`]. A caller that names a
/// project this scope does not have gets an error rather than a silently-created
/// record: the rail never creates projects and neither does this.
///
/// `async`, where it used to be synchronous `std::fs`, because the lock is: a
/// `tokio::sync::Mutex` cannot be taken from a synchronous function running on a
/// runtime thread — `blocking_lock` panics there — and every caller of this one
/// was already inside an `async fn`.
pub async fn set_vibedev_project_active_root_task_id(
    scope_root: &Path,
    project_id: &str,
    pointer: VibeDevProjectPointer<'_>,
) -> Result<(), String> {
    mutate_project_store_at(&vibedev_project_store_path(scope_root), |store| {
        let Some(index) = store
            .projects
            .iter()
            .position(|project| project.project_id == project_id)
        else {
            return Err(format!("no VibeDev project {project_id} in this scope"));
        };
        match pointer {
            VibeDevProjectPointer::PinTo(task_id) => {
                append_vibedev_run_task_id(&mut store.projects[index], task_id);
                store.projects[index].active_root_task_id = Some(task_id.to_string());
            },
            VibeDevProjectPointer::ClearIfNames(task_id) => {
                if store.projects[index].active_root_task_id.as_deref() != Some(task_id) {
                    // Someone else's run owns the pointer now. Leaving it is not
                    // a partial unwind — the pointer is not naming the task about
                    // to be deleted, which is the entire invariant.
                    return Ok(ProjectStoreEdit::Skip);
                }
                store.projects[index].active_root_task_id = None;
            },
        }
        store.projects[index].updated_at_ms = chrono::Utc::now().timestamp_millis();
        Ok(ProjectStoreEdit::Write)
    })
    .await
}

/// Whether a mutation handed to [`mutate_project_store_at`] changed anything.
///
/// `Skip` exists because the pointer unwind is conditional: when the pointer has
/// already moved on there is nothing to unwind, and republishing an identical
/// store would rewrite the file for no reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectStoreEdit {
    Write,
    Skip,
}

/// Read-modify-write a scope's **existing** `projects.json`, holding
/// [`project_store_lock`] across all three steps.
///
/// The guarded writer for callers that have a scope root and no `VibeDevApi` —
/// today [`set_vibedev_project_active_root_task_id`], off the run service. The
/// HTTP handlers cannot take this shape because they await the chat store
/// between their load and their mutation, so they take the same lock directly
/// and hold it around [`load_project_store`] and [`save_project_store`]. One
/// registry, so the two kinds of writer still exclude each other.
///
/// A missing store is an error, not an empty default: everything that reaches
/// here edits a project that must already exist, and inventing a store would
/// publish a one-project file over whatever a concurrent create is writing.
pub fn normalized_optional_string(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub async fn mutate_project_store_at(
    path: &Path,
    mutate: impl FnOnce(&mut VibeDevProjectStore) -> Result<ProjectStoreEdit, String>,
) -> Result<(), String> {
    let lock = project_store_lock(path);
    let _guard = lock.lock().await;

    let mut store = match fs::read(path).await {
        Ok(bytes) => serde_json::from_slice::<VibeDevProjectStore>(&bytes)
            .map_err(|error| format!("{}: {error}", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("no VibeDev project store at {}", path.display()));
        },
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    normalize_vibedev_project_store(&mut store);
    if mutate(&mut store)? == ProjectStoreEdit::Skip {
        return Ok(());
    }

    let payload = serde_json::to_vec_pretty(&store).map_err(|error| error.to_string())?;
    write_bytes_durably(path, &payload)
        .await
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(())
}

#[async_trait::async_trait]
impl EpisodeProjectResolver for VibeDevEpisodeProjectResolver {
    async fn resolve_project_id(&self, task_id: &str) -> Option<String> {
        let task_id = task_id.trim();
        if task_id.is_empty() {
            return None;
        }
        {
            let cache = self.cache.lock().await;
            if let Some(cached) = cache.get(task_id) {
                return cached.clone();
            }
        }
        let resolved = self
            .task_service
            .get_task(&self.scope, task_id)
            .await
            .ok()
            .and_then(|task| parse_vibedev_project_id(&task.manifest.description));
        let mut cache = self.cache.lock().await;
        cache.insert(task_id.to_string(), resolved.clone());
        resolved
    }
}

/// What a build started from `chat_session_id` should land in — or why the
/// caller must not decide on its own.
#[derive(Debug, Clone)]
pub enum VibeDevProjectResolution {
    /// Exactly one project this can mean. Start there, silently.
    Project(VibeDevProjectRecord),
    /// Several live projects, and nothing says which. The caller must **ask**
    /// and start nothing. Carries every live project, in the cockpit's own
    /// display order, so the question can name what it is choosing between.
    Ambiguous(Vec<VibeDevProjectRecord>),
    /// The scope has no live project at all.
    NoProject,
}

/// Read a scope's projects for read-only callers outside the HTTP layer — tool
/// handlers and the `@vibedev` chat rail, which have neither a `VibeDevApi` nor
/// anywhere to put `load_project_store`'s `HttpResponse` errors.
///
/// A missing or unparseable store yields no projects: "this scope has no VibeDev
/// project" is the answer those callers act on, and none of them can repair a
/// broken store. Skipping `load_project_store`'s normalization is deliberate too —
/// it only rewrites run-id bookkeeping, which is a write-path concern.
pub fn read_vibedev_projects(scope_root: &Path) -> Vec<VibeDevProjectRecord> {
    let Ok(bytes) = std::fs::read(vibedev_project_store_path(scope_root)) else {
        return Vec::new();
    };
    serde_json::from_slice::<VibeDevProjectStore>(&bytes)
        .map(|store| store.projects)
        .unwrap_or_default()
}

/// Which project does a build started from `chat_session_id` belong to?
///
/// Tier 1 — the project this very chat session drives: the user is looking at
/// it, so it outranks everything. Tier 2 — the scope's active project, but only
/// where accepting it is not a guess. Tier 3 — several live projects and no
/// pointer at any of them, so the caller **asks**. Tier 4 — nothing live, and
/// the caller refuses. Archived projects are never resolved, never counted
/// toward ambiguity and never offered.
///
/// ### What "not a guess" means, and why the line is here
///
/// [`active_vibedev_project`] answers in two branches: the first live project
/// whose cockpit session is **open**, else — when no session is open at all —
/// the first live project in display order, which is just "whichever row was
/// touched last". The first branch is a pointer the user set (picking a project
/// in the cockpit activates its session); the second is an accident of
/// timestamps.
///
/// So tier 2 accepts the active project when there is only one live project to
/// choose from (there is nothing to be ambiguous about), or when the cockpit's
/// pointer exists. It declines only in the case that made this function need an
/// answer at all: several live projects and no open session anywhere, where
/// "active" degenerates into the most recently updated row.
///
/// **This does not compute a second definition of the active project.** The
/// project returned at tier 2 is always exactly what `active_vibedev_project`
/// returned, so the rail can only agree with the cockpit's highlight or decline
/// to act — never disagree with it.
///
/// `sessions` is a parameter rather than something loaded here because tier 2 is
/// defined in terms of chat-session status, and the callers that need this (the
/// chat service) already hold the session list.
pub fn select_vibedev_project_for_session(
    mut projects: Vec<VibeDevProjectRecord>,
    sessions: &[ChatSession],
    chat_session_id: &str,
) -> VibeDevProjectResolution {
    sort_vibedev_projects_for_display(&mut projects);
    let sessions_by_id = vibedev_sessions_by_id(sessions);
    let ordered = projects.as_slice();
    if let Some(owned) = ordered
        .iter()
        .find(|project| !project.archived && project.chat_session_id == chat_session_id)
    {
        return VibeDevProjectResolution::Project(owned.clone());
    }
    let Some(active) = active_vibedev_project(ordered, &sessions_by_id) else {
        // `active_vibedev_project` is `None` only when nothing live remains.
        return VibeDevProjectResolution::NoProject;
    };
    let live = ordered
        .iter()
        .filter(|project| !project.archived)
        .cloned()
        .collect::<Vec<_>>();
    if live.len() > 1 && !vibedev_project_session_is_open(active, &sessions_by_id) {
        return VibeDevProjectResolution::Ambiguous(live);
    }
    VibeDevProjectResolution::Project(active.clone())
}

/// `select_vibedev_project_for_session` against the scope's on-disk project store.
///
/// The store as written, with no adoption: `load_projects_with_adopted_sessions`
/// mints and persists a project for any unclaimed session in the cockpit's own
/// thread, and a chat turn must not do that — it may only find a project, never
/// create one. So a cockpit session that has never been listed resolves to nothing
/// here while the cockpit would show it; the two agree from its first list call on.
///
/// Caller: the `@vibedev` chat rail (`magician_v2::vibedev::rail`).
pub fn resolve_vibedev_project_for_session(
    scope_root: &Path,
    sessions: &[ChatSession],
    chat_session_id: &str,
) -> VibeDevProjectResolution {
    select_vibedev_project_for_session(read_vibedev_projects(scope_root), sessions, chat_session_id)
}

/// Canonical order for a scope's projects: most recently touched first, then most
/// recently created, then by name. `active_vibedev_project` takes the first match
/// off this order, so the two must be applied together or "the active project"
/// changes meaning.
pub fn sort_vibedev_projects_for_display(projects: &mut [VibeDevProjectRecord]) {
    projects.sort_by(|left, right| {
        right
            .updated_at_ms
            .cmp(&left.updated_at_ms)
            .then_with(|| right.created_at_ms.cmp(&left.created_at_ms))
            .then_with(|| left.name.cmp(&right.name))
    });
}

pub fn vibedev_sessions_by_id(sessions: &[ChatSession]) -> HashMap<&str, &ChatSession> {
    sessions
        .iter()
        .map(|session| (session.id.as_str(), session))
        .collect()
}

/// Is this project's cockpit chat session still open?
///
/// The signal `active_vibedev_project` prefers on, and the only thing in the
/// store that says "the cockpit is pointed here" rather than merely "this row
/// was touched most recently". Activating a project in the cockpit is what sets
/// it (`activate_vibedev_project_handler`).
///
/// Extracted rather than restated so a caller can ask *which branch* of
/// `active_vibedev_project` answered without writing that branch's condition a
/// second time. Two spellings of "its session is open" that drifted apart would
/// surface as a chat-started build landing in a repo the cockpit is not showing.
pub fn vibedev_project_session_is_open(
    project: &VibeDevProjectRecord,
    sessions_by_id: &HashMap<&str, &ChatSession>,
) -> bool {
    sessions_by_id
        .get(project.chat_session_id.as_str())
        .is_some_and(|session| session.status == ChatSessionStatus::Active)
}

/// The scope's active project, over projects already in
/// `sort_vibedev_projects_for_display` order: the first live project whose chat
/// session is still open, else the first live project at all — a scope whose
/// sessions have all been archived still has somewhere to build.
///
/// Single-sourced deliberately. A second, plausible-looking definition elsewhere
/// (say, "most recently updated") would disagree with this one only sometimes,
/// and would surface as a build landing in the wrong repo. The cockpit's
/// `active_project_id` is this function and nothing else; the `@vibedev` rail
/// narrows *when it is willing to accept the answer* (see
/// [`select_vibedev_project_for_session`]) but never computes a different one.
pub fn active_vibedev_project<'a>(
    projects: &'a [VibeDevProjectRecord],
    sessions_by_id: &HashMap<&str, &ChatSession>,
) -> Option<&'a VibeDevProjectRecord> {
    projects
        .iter()
        .find(|project| {
            !project.archived && vibedev_project_session_is_open(project, sessions_by_id)
        })
        .or_else(|| projects.iter().find(|project| !project.archived))
}

impl VibeDevEpisodeProjectResolver {
    pub fn new(task_service: Arc<ArtifactV2Service>, scope: ScopeRef) -> Self {
        Self {
            task_service,
            scope,
            cache: tokio::sync::Mutex::new(HashMap::new()),
        }
    }
}

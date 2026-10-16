//! Scoped, read-only Apps binder for the meeting rails: live capture state,
//! the dated meeting thread index, transcripts, takeaways, upcoming calendar
//! context, and keyword retrieval across all of them.
//!
//! Pattern: the `evidence_data` / `thinking_maps_data` host-read binders. A
//! closed action set, fail-closed argument proofs, executor-owned runtime
//! scope, fixed scan budgets, and a serialized-result ceiling. There is no
//! mutation path here at all — starting, pausing and stopping capture is the
//! separately reviewed control action class, never a read.
//!
//! Two scope notes, because the meeting rails are not uniform:
//!
//! * **The session registries are process-global and in-memory.** They are the
//!   authority for "is capture running right now", which is meaningless per
//!   scope and must never be hidden from the operator — the same reason
//!   `GET /meetings/active` lists every live session. `active_session`
//!   therefore projects the same rows. It deliberately projects LESS than the
//!   first-party endpoint: no `latest_summary`. Rolling summaries are meeting
//!   *content*, and content is reachable here only through the scoped thread,
//!   transcript and takeaway reads below.
//! * **Everything else is scope-owned.** Threads and transcripts resolve
//!   through the published chat store under `__principal`/`__workspace`;
//!   takeaways resolve through the scoped memory service; upcoming meetings
//!   read the scope's own capability auth root.
//!
//! This binder invents no durable session entity. The registries stay
//! in-memory; a durable meeting-session directory remains the scale gate the
//! singular-primitives record owns.

use std::cmp::{Ordering, Reverse};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use magicllm::LlmScope;
use serde::Serialize;
use serde_json::{json, Map, Value};
use tokio::time::timeout;

use super::actions::{ActionResult, ExecutableAction};
use super::capability::{CapabilityPackDefinition, CapabilityProvider, ImplementationType};
use super::error::ExecutionError;
use crate::magician_v2::agents::memory::AgentMemoryResolver;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::artifact_v2::CapabilityWorkspaceManager;
use crate::magician_v2::chat::models::{ChatMessage, ChatMessageContent, ChatSessionStatus};
use crate::magician_v2::chat::storage::{global_chat_store, ChatStore, ChatThreadSessionSummary};
// One import path for the whole meeting engine — the same `media_seam::meeting`
// module the first-party `/meetings` API consumes, so the binder and the API can
// never bind different registries.
use crate::magician_v2::media_seam::meeting::{
    meeting_manager, passive_meeting_manager, upcoming_meetings_cached, MeetingStatus,
    PassiveStatus, ATTENDEE_ENDED_RETAIN_SECS, PASSIVE_ENDED_RETAIN_SECS,
};
use crate::magician_v2::resource_authority::gated_action::MaybeGatedAction;
use crate::magician_v2::resource_authority::scoped_authority::is_safe_scope_id;
use crate::magician_v2::strategy::plan::PlanStep;

pub const MEETINGS_DATA_TOOL_NAME: &str = "meetings_data";

pub(crate) const APP_BOUND_MEETINGS_DATA_INPUT_CEILING: u64 = 8 * 1024;
pub(crate) const APP_BOUND_MEETINGS_DATA_RESULT_CEILING: u64 = 512 * 1024;

/// The dated per-meeting chat threads this binder may see. Nothing outside the
/// prefix is reachable through any action.
pub(crate) const MEETING_THREAD_PREFIX: &str = "meeting-";
/// Meeting takeaways live in one user memory tier, written by
/// `ScopedMeetingMemoryWriter`, keyed `meeting:<thread-id>`.
///
/// The writer names the tier `user.research_findings` but persists it through
/// `normalized_user_memory_tier_name`, which strips the `user.` prefix — so the
/// entries land at the knowledge document's TOP level, not nested under
/// `user`. The key is derived from that same normalizer rather than spelled
/// out, so a reader can never drift from the writer again.
const MEETING_MEMORY_TIER: &str = "user.research_findings";
const MEETING_MEMORY_KEY_PREFIX: &str = "meeting:";

const DEFAULT_LIST_LIMIT: usize = 20;
const MAX_LIST_LIMIT: u64 = 50;
const MAX_ID_BYTES: usize = 255;
const MIN_TEXT_FILTER_BYTES: usize = 2;
const MAX_TEXT_FILTER_BYTES: usize = 512;

/// Live-capture rows are few by construction (one operator, a handful of
/// retained sessions), but the projection is still bounded so a registry leak
/// can never produce an unbounded result.
const ACTIVE_SESSION_ROW_CEILING: usize = 64;
/// Meeting threads scanned before a page reports `scan_truncated`.
const THREAD_SCAN_BUDGET: usize = 2_000;
/// Newest transcript messages a single `read_thread` page may walk.
const MAX_TRANSCRIPT_PAGE: usize = 50;
/// Per-message projected text ceiling. Transcript turns are utterances; a
/// pathological one must not consume the whole result budget.
const MAX_MESSAGE_TEXT_BYTES: usize = 4 * 1024;
const MAX_TAKEAWAY_TEXT_BYTES: usize = 8 * 1024;
const MAX_TAKEAWAY_LIST_ITEMS: usize = 32;
/// Calendar rows projected from the shared cache.
const MAX_UPCOMING_EVENTS: usize = 50;
/// Keyword retrieval budgets. Keyword-only by design — semantic retrieval is
/// recorded platform debt, not a silent upgrade.
const SEARCH_MAX_SESSIONS: usize = 25;
const SEARCH_MESSAGES_PER_SESSION: usize = 200;
const SEARCH_MESSAGE_BUDGET: usize = 2_000;
const SEARCH_EXCERPT_BYTES: usize = 320;

const ACTIONS: &[&str] = &[
    "active_session",
    "list_threads",
    "read_thread",
    "read_takeaways",
    "upcoming_meetings",
    "search_meeting_memory",
];
const THREAD_STATUSES: &[&str] = &["active", "archived", "all"];

#[derive(Clone)]
pub struct MeetingsDataProvider {
    workspace_layout: ArtifactV2Workspace,
    pack_def: Option<CapabilityPackDefinition>,
}

impl std::fmt::Debug for MeetingsDataProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeetingsDataProvider")
            .field("workspace_layout", &self.workspace_layout)
            .field("pack_def", &self.pack_def.as_ref().map(|pack| &pack.name))
            .finish()
    }
}

impl MeetingsDataProvider {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            workspace_layout,
            pack_def: None,
        }
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }
}

#[async_trait]
impl CapabilityProvider for MeetingsDataProvider {
    fn tool_name(&self) -> &str {
        MEETINGS_DATA_TOOL_NAME
    }

    fn prove_app_tool_args(&self, parameters: &HashMap<String, Value>) -> bool {
        prove_app_meetings_data_args(parameters)
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let resolved_params = if let Some(pack_def) = &self.pack_def {
            pack_def.resolve_params(&step.parameters)?
        } else {
            step.parameters.clone()
        };
        let action = ExecutableAction::Pack {
            capability_name: MEETINGS_DATA_TOOL_NAME.to_owned(),
            implementation: ImplementationType::Compiled {
                provider_name: MEETINGS_DATA_TOOL_NAME.to_owned(),
            },
            resolved_params: resolved_params.clone(),
        };
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &resolved_params,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        let params = match action {
            ExecutableAction::Pack {
                resolved_params, ..
            } => resolved_params.clone(),
            _ => {
                return Err(ExecutionError::Step(
                    "meetings_data: unexpected action type".to_owned(),
                ))
            },
        };
        let action_name = string_param(&params, "__action_name")
            .or_else(|| string_param(&params, "action"))
            .unwrap_or_else(|| "active_session".to_owned());
        let mut params = authorize_runtime_scope(params)?;
        params.insert(
            "__action_name".to_owned(),
            Value::String(action_name.clone()),
        );
        if !prove_app_meetings_data_args(&params) {
            return Err(ExecutionError::Step(
                "meetings_data arguments are outside the closed action schema".to_owned(),
            ));
        }
        let input_bytes = serde_json::to_vec(&params).map_err(|error| {
            ExecutionError::Step(format!(
                "meetings_data argument serialization failed: {error}"
            ))
        })?;
        if input_bytes.len() as u64 > APP_BOUND_MEETINGS_DATA_INPUT_CEILING {
            return Err(ExecutionError::Step(format!(
                "meetings_data arguments exceeded the {} byte ceiling",
                APP_BOUND_MEETINGS_DATA_INPUT_CEILING
            )));
        }
        let effective_timeout = timeout_secs.max(1);
        let value = timeout(
            Duration::from_secs(effective_timeout),
            execute_meetings_data_action(&self.workspace_layout, &action_name, &params),
        )
        .await
        .map_err(|_| {
            ExecutionError::Step(format!(
                "meetings_data action `{action_name}` timed out after {effective_timeout}s"
            ))
        })??;
        let rendered = serde_json::to_string_pretty(&value).map_err(|error| {
            ExecutionError::Step(format!(
                "meetings_data result serialization failed: {error}"
            ))
        })?;
        if rendered.len() as u64 > APP_BOUND_MEETINGS_DATA_RESULT_CEILING {
            return Err(ExecutionError::Step(format!(
                "meetings_data result exceeded the {} byte ceiling",
                APP_BOUND_MEETINGS_DATA_RESULT_CEILING
            )));
        }
        Ok(ActionResult::text(rendered))
    }

    fn default_timeout_secs(&self) -> u64 {
        self.pack_def
            .as_ref()
            .and_then(|pack| pack.execution.as_ref())
            .and_then(|execution| execution.default_timeout_secs)
            .unwrap_or(30)
    }
}

// ---------------------------------------------------------------------------
// Closed argument proof
// ---------------------------------------------------------------------------

fn prove_app_meetings_data_args(parameters: &HashMap<String, Value>) -> bool {
    let Some(operation) = parameters.get("__action_name").and_then(Value::as_str) else {
        return false;
    };
    if !ACTIONS.contains(&operation) {
        return false;
    }

    for (key, value) in parameters {
        match key.as_str() {
            "__action_name" => {},
            "operation" | "action" | "method" => {
                let agrees = value.as_str().is_some_and(|alias| {
                    crate::magician_v2::apps::app_tool_bind::normalize_app_action_selector(
                        MEETINGS_DATA_TOOL_NAME,
                        alias,
                    )
                    .as_deref()
                        == Some(operation)
                });
                if !agrees {
                    return false;
                }
            },
            "principal" | "workspace" => {
                if !bounded_nonblank_string(value, MAX_ID_BYTES) {
                    return false;
                }
            },
            "status" if operation == "list_threads" => {
                if value
                    .as_str()
                    .is_none_or(|status| !THREAD_STATUSES.contains(&status))
                {
                    return false;
                }
            },
            "text" if matches!(operation, "list_threads" | "search_meeting_memory") => {
                if !bounded_filter_text(value) {
                    return false;
                }
            },
            "limit"
                if matches!(
                    operation,
                    "list_threads" | "read_thread" | "read_takeaways" | "search_meeting_memory"
                ) =>
            {
                if value
                    .as_u64()
                    .is_none_or(|limit| !(1..=MAX_LIST_LIMIT).contains(&limit))
                {
                    return false;
                }
            },
            "after_thread_id" if operation == "list_threads" => {
                if !bounded_thread_cursor(value) {
                    return false;
                }
            },
            "thread_id" if matches!(operation, "read_thread" | "read_takeaways") => {
                if !bounded_thread_id(value) {
                    return false;
                }
            },
            "session_id" if operation == "read_thread" => {
                if !bounded_safe_id(value) {
                    return false;
                }
            },
            "before_message_id" if operation == "read_thread" => {
                if !bounded_safe_id(value) {
                    return false;
                }
            },
            hidden if hidden.starts_with("__") => {},
            _ => return false,
        }
    }

    match operation {
        // The transcript read is always about one exact named thread: an
        // unscoped "read the transcript" has no meaning and must not default to
        // whichever thread happens to be newest.
        "read_thread" => parameters.contains_key("thread_id"),
        // Keyword retrieval without a keyword would be an unbounded dump of
        // every meeting the scope holds.
        "search_meeting_memory" => parameters.contains_key("text"),
        _ => true,
    }
}

fn bounded_nonblank_string(value: &Value, max_bytes: usize) -> bool {
    value
        .as_str()
        .is_some_and(|text| !text.trim().is_empty() && text.len() <= max_bytes)
}

fn bounded_filter_text(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        let trimmed = text.trim();
        trimmed.len() >= MIN_TEXT_FILTER_BYTES && text.len() <= MAX_TEXT_FILTER_BYTES
    })
}

fn bounded_safe_id(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|id| id.trim() == id && id.len() <= MAX_ID_BYTES && is_safe_scope_id(id))
}

/// A thread id must additionally live under the meeting prefix. Without this
/// the binder would be a general chat reader wearing a meetings name.
fn bounded_thread_id(value: &Value) -> bool {
    bounded_safe_id(value)
        && value.as_str().is_some_and(|id| {
            id.starts_with(MEETING_THREAD_PREFIX) && id.len() > MEETING_THREAD_PREFIX.len()
        })
}

/// A `list_threads` page boundary: either the encoded `(created_at, thread_id)`
/// tuple this read emits as `next_cursor`, or a bare thread id.
///
/// The proof MUST admit the cursor the read hands back, or page two is
/// unreachable — the tuple form does not start with the meeting prefix, so the
/// thread-id validator alone rejects it.
fn bounded_thread_cursor(value: &Value) -> bool {
    if bounded_thread_id(value) {
        return true;
    }
    let Some(raw) = value.as_str() else {
        return false;
    };
    if !(raw.trim() == raw && raw.len() <= MAX_ID_BYTES && is_safe_scope_id(raw)) {
        return false;
    }
    let Some((created_at, thread_id)) = raw.split_once('~') else {
        return false;
    };
    !created_at.is_empty()
        && created_at.len() <= 19
        && created_at.bytes().all(|byte| byte.is_ascii_digit())
        && thread_id.starts_with(MEETING_THREAD_PREFIX)
        && thread_id.len() > MEETING_THREAD_PREFIX.len()
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

async fn execute_meetings_data_action(
    workspace_layout: &ArtifactV2Workspace,
    action: &str,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    match action {
        "active_session" => active_session(params).await,
        "list_threads" => list_threads(params).await,
        "read_thread" => read_thread(params).await,
        "read_takeaways" => read_takeaways(workspace_layout, params).await,
        "upcoming_meetings" => upcoming_meetings(workspace_layout, params).await,
        "search_meeting_memory" => search_meeting_memory(workspace_layout, params).await,
        other => Err(ExecutionError::Step(format!(
            "meetings_data: unknown action `{other}`"
        ))),
    }
}

// ---------------------------------------------------------------------------
// active_session
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct VisibleSessionProjection {
    session_id: String,
    mode: &'static str,
    status: String,
    live: bool,
    /// True when this session's thread belongs to the reading scope. The
    /// operational fields are registry-wide; the descriptive ones below are
    /// present only when this is true.
    in_scope: bool,
    thread_id: Option<String>,
    title: Option<String>,
    url: Option<String>,
    paused: bool,
    capture_mic: Option<bool>,
    started_seconds_ago: u64,
    ended_seconds_ago: Option<u64>,
    retained_for_seconds: u64,
}

/// Both registries as one bounded read.
///
/// **Two visibilities, deliberately.** That capture is RUNNING is registry-wide
/// and must never be hidden from the operator — the same reason
/// `GET /meetings/active` lists every live session. WHAT is being captured is
/// content: a meeting title and a joinable conference link identify a room as
/// surely as a summary does. So a row whose thread does not belong to the
/// reading scope keeps `session_id`, `mode`, `status`, `live`, `paused` and its
/// durations, and drops `thread_id`, `title` and `url`. A cross-scope row is
/// therefore visible and useless, which is exactly the intent.
///
/// Rows are ordered live-first, then by session id — a total order over values
/// that do not change between two identical reads, so a polling surface never
/// sees rows shuffle. Duration is deliberately NOT a sort key: it is whole
/// seconds off a monotonic clock, so two sessions started under a second apart
/// would swap places as the clock advanced.
async fn active_session(params: &HashMap<String, Value>) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params)?;
    let mut rows: Vec<VisibleSessionProjection> = Vec::new();
    // Ownership comes from the scope the registry recorded at spawn — NOT from
    // whether the scope's chat index happens to hold the thread. That index test
    // was wrong in both directions: a meeting's chat session is created lazily
    // by the transcript sink's warm-up, so the owner's own capture would read as
    // foreign for the first seconds (and permanently where the sink cannot
    // authenticate); and thread ids are `meeting-<slug(title)>-<date>`, derived
    // from title and date alone, so two principals in same-titled meetings on
    // the same day collide and each would see the other's join URL.
    let owned_by_scope = |session_scope: Option<&(String, String)>| {
        session_scope.is_some_and(|(principal, workspace)| {
            *principal == scope.principal && *workspace == scope.workspace
        })
    };

    for view in passive_meeting_manager().list().await {
        let live = matches!(view.status, PassiveStatus::Listening);
        let in_scope = owned_by_scope(view.scope.as_ref());
        rows.push(VisibleSessionProjection {
            session_id: view.session_id,
            mode: "passive",
            status: format!("{:?}", view.status),
            live,
            in_scope,
            thread_id: in_scope.then_some(view.thread),
            title: in_scope.then_some(view.title).flatten(),
            url: in_scope.then_some(view.url).flatten(),
            paused: view.paused,
            capture_mic: Some(view.capture_mic),
            started_seconds_ago: view.started_seconds_ago,
            ended_seconds_ago: view.ended_seconds_ago,
            retained_for_seconds: PASSIVE_ENDED_RETAIN_SECS,
        });
    }
    for row in meeting_manager().list().await {
        // The attendee rail retains ended sessions too; `Left`/`Failed` are the
        // terminal states the capture dot and the Active grid already key off.
        let live = !matches!(row.status, MeetingStatus::Left | MeetingStatus::Failed);
        let in_scope = owned_by_scope(row.scope.as_ref());
        rows.push(VisibleSessionProjection {
            session_id: row.session_id,
            mode: "attendee",
            status: format!("{:?}", row.status),
            live,
            in_scope,
            thread_id: in_scope.then_some(row.thread).flatten(),
            title: in_scope.then_some(row.title).flatten(),
            url: in_scope.then_some(row.url),
            paused: row.paused,
            capture_mic: None,
            started_seconds_ago: row.started_seconds_ago,
            ended_seconds_ago: row.ended_seconds_ago,
            retained_for_seconds: ATTENDEE_ENDED_RETAIN_SECS,
        });
    }

    rows.sort_by(|left, right| {
        right
            .live
            .cmp(&left.live)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    let truncated = rows.len() > ACTIVE_SESSION_ROW_CEILING;
    rows.truncate(ACTIVE_SESSION_ROW_CEILING);
    let capture_live = rows.iter().any(|row| row.live);

    Ok(json!({
        "scope": scope.as_json(),
        "sessions": rows,
        "capture_live": capture_live,
        "row_ceiling": ACTIVE_SESSION_ROW_CEILING,
        "truncated": truncated,
        // The registries are in-memory: an ended row disappears once its rail's
        // retention window lapses. A surface that renders session state owes
        // the operator that window, not just a status.
        "attendee_retained_for_seconds": ATTENDEE_ENDED_RETAIN_SECS,
        "passive_retained_for_seconds": PASSIVE_ENDED_RETAIN_SECS,
    }))
}

// ---------------------------------------------------------------------------
// list_threads
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct VisibleThreadProjection {
    thread_id: String,
    session_id: String,
    title: Option<String>,
    agent_id: String,
    status: &'static str,
    created_at: String,
    updated_at: String,
    /// The tuple half the public cursor is built from. Not projected: the
    /// package's `meeting_thread` entity has no such field, and a caller reads
    /// the boundary from `next_cursor`, never from a row.
    #[serde(skip)]
    created_at_ms: i64,
}

async fn list_threads(params: &HashMap<String, Value>) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params)?;
    let store = require_chat_store()?;
    let wanted_status = string_param(params, "status").unwrap_or_else(|| "all".to_owned());
    let text = string_param(params, "text").map(|text| text.to_lowercase());

    let sessions = meeting_sessions(store.as_ref(), &scope).await?;
    let scan_truncated = sessions.len() > THREAD_SCAN_BUDGET;
    let scanned = &sessions[..sessions.len().min(THREAD_SCAN_BUDGET)];

    let mut threads = latest_session_per_thread(scanned);
    threads.retain(|session| session_status_matches(session, &wanted_status));
    if let Some(text) = text.as_deref() {
        threads.retain(|session| {
            session.ui_thread_id.to_lowercase().contains(text)
                || session
                    .title
                    .as_deref()
                    .is_some_and(|title| title.to_lowercase().contains(text))
        });
    }
    threads.sort_by(thread_order);

    // Keyset, not identity lookup. Resolving the cursor by finding its row
    // would strand a pager whose cursor thread was archived, retitled out of
    // the text filter, or re-represented by a rotated session between two
    // pages — the exact "restart at page one" failure the cursor exists to
    // prevent. Comparing the tuple positionally keeps a vanished cursor a
    // valid boundary.
    let after = thread_cursor(&threads, params)?;
    let remaining = match after {
        Some(boundary) => {
            let start =
                threads.partition_point(|session| thread_sort_key(session) <= boundary.as_key());
            &threads[start..]
        },
        None => &threads[..],
    };
    let limit = bounded_limit(params);
    let has_more = remaining.len() > limit;
    let page = &remaining[..remaining.len().min(limit)];
    let rows = page
        .iter()
        .map(|session| VisibleThreadProjection {
            thread_id: session.ui_thread_id.clone(),
            session_id: session.session_id.clone(),
            title: session.title.clone(),
            agent_id: session.agent_id.clone(),
            status: session_status_label(session),
            created_at: rfc3339(session.created_at),
            updated_at: rfc3339(session.updated_at),
            created_at_ms: session.created_at,
        })
        .collect::<Vec<_>>();
    let next_cursor = has_more
        .then(|| {
            rows.last()
                .map(|row| format!("{}~{}", row.created_at_ms, row.thread_id))
        })
        .flatten();

    Ok(json!({
        "scope": scope.as_json(),
        "threads": rows,
        "next_cursor": next_cursor,
        "scan_truncated": scan_truncated,
        "scan_budget": THREAD_SCAN_BUDGET,
    }))
}

/// The decoded public cursor: the `(created_at, thread_id)` tuple the previous
/// page ended on.
#[derive(Debug, Clone)]
struct ThreadCursor {
    created_at: i64,
    thread_id: String,
}

impl ThreadCursor {
    fn as_key(&self) -> (Reverse<i64>, Reverse<&str>) {
        (Reverse(self.created_at), Reverse(self.thread_id.as_str()))
    }
}

/// Sort key matching [`thread_order`]: newest first, id-descending on ties.
fn thread_sort_key<'a>(session: &&'a ChatThreadSessionSummary) -> (Reverse<i64>, Reverse<&'a str>) {
    (
        Reverse(session.created_at),
        Reverse(session.ui_thread_id.as_str()),
    )
}

fn thread_cursor(
    threads: &[&ChatThreadSessionSummary],
    params: &HashMap<String, Value>,
) -> Result<Option<ThreadCursor>, ExecutionError> {
    let Some(raw) = string_param(params, "after_thread_id") else {
        return Ok(None);
    };
    // Back-compatible: a bare thread id is still accepted and resolved against
    // the current page, but the encoded tuple is what this read emits.
    if let Some((created_at, thread_id)) = raw.split_once('~') {
        let created_at = created_at.parse::<i64>().map_err(|_| {
            ExecutionError::Step(
                "meetings_data: `after_thread_id` is not a valid page boundary".to_owned(),
            )
        })?;
        return Ok(Some(ThreadCursor {
            created_at,
            thread_id: thread_id.to_owned(),
        }));
    }
    threads
        .iter()
        .find(|session| session.ui_thread_id == raw)
        .map(|session| ThreadCursor {
            created_at: session.created_at,
            thread_id: session.ui_thread_id.clone(),
        })
        .map(Some)
        .ok_or_else(|| {
            ExecutionError::Step(
                "meetings_data: `after_thread_id` does not name a row in the current scoped ordering"
                    .to_owned(),
            )
        })
}

/// Newest-first over a total, stable `(created_at, thread_id)` ordering. Thread
/// ids are names (`meeting-<slug>-<date>`), not a monotonic sequence, so the id
/// is the tie-break rather than the ordering.
fn thread_order(left: &&ChatThreadSessionSummary, right: &&ChatThreadSessionSummary) -> Ordering {
    right
        .created_at
        .cmp(&left.created_at)
        .then_with(|| right.ui_thread_id.cmp(&left.ui_thread_id))
}

fn session_status_label(session: &ChatThreadSessionSummary) -> &'static str {
    match session.status {
        ChatSessionStatus::Active => "active",
        ChatSessionStatus::Archived => "archived",
    }
}

fn session_status_matches(session: &ChatThreadSessionSummary, wanted: &str) -> bool {
    wanted == "all" || session_status_label(session) == wanted
}

/// A meeting thread can carry rotated sessions; the console reads the newest
/// one. Selection is `(updated_at, id)` so two sessions updated in the same
/// millisecond still resolve deterministically.
fn latest_session_per_thread(
    sessions: &[ChatThreadSessionSummary],
) -> Vec<&ChatThreadSessionSummary> {
    let mut newest: HashMap<&str, &ChatThreadSessionSummary> = HashMap::new();
    for session in sessions {
        newest
            .entry(session.ui_thread_id.as_str())
            .and_modify(|current| {
                let replace = session
                    .updated_at
                    .cmp(&current.updated_at)
                    .then_with(|| session.session_id.cmp(&current.session_id))
                    == Ordering::Greater;
                if replace {
                    *current = session;
                }
            })
            .or_insert(session);
    }
    newest.into_values().collect()
}

/// Index-only. The store answers this from its in-memory session index and
/// opens no document, so a surface polling every few seconds does not re-read
/// and re-parse the scope's whole meeting history on each tick.
async fn meeting_sessions(
    store: &dyn ChatStore,
    scope: &Scope,
) -> Result<Vec<ChatThreadSessionSummary>, ExecutionError> {
    store
        .list_thread_summaries_for_prefix(&scope.principal, &scope.workspace, MEETING_THREAD_PREFIX)
        .await
        .map_err(|error| store_error("listing the scope's meeting threads", error))
}

/// Epoch millis as RFC3339 — the wire form the package's `timestamp` fields
/// declare. An out-of-range stamp becomes the epoch rather than failing the
/// whole page; the store never writes one.
fn rfc3339(millis: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(millis)
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
        .to_rfc3339()
}

// ---------------------------------------------------------------------------
// read_thread
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct VisibleTranscriptLine {
    message_id: String,
    created_at: String,
    speaker: Option<String>,
    text: String,
    transcript: bool,
    #[serde(skip)]
    created_at_ms: i64,
}

#[derive(Debug, Serialize)]
struct VisibleThreadSession {
    session_id: String,
    title: Option<String>,
    created_at: String,
    updated_at: String,
}

async fn read_thread(params: &HashMap<String, Value>) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params)?;
    let store = require_chat_store()?;
    let thread_id = required_string(params, "thread_id")?;

    let sessions = meeting_sessions(store.as_ref(), &scope).await?;
    let mut thread_sessions = sessions
        .iter()
        .filter(|session| session.ui_thread_id == thread_id)
        .collect::<Vec<_>>();
    if thread_sessions.is_empty() {
        return Err(ExecutionError::Step(format!(
            "meetings_data read_thread: `{thread_id}` names no meeting thread in this scope"
        )));
    }
    // Newest first, deterministic on ties.
    thread_sessions.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.session_id.cmp(&left.session_id))
    });

    // An explicit `session_id` must belong to THIS thread. Without the check a
    // caller could name any session id it had ever seen and read it through a
    // meetings-shaped action.
    let selected = match string_param(params, "session_id") {
        Some(requested) => thread_sessions
            .iter()
            .copied()
            .find(|session| session.session_id == requested)
            .ok_or_else(|| {
                ExecutionError::Step(format!(
                    "meetings_data read_thread: `session_id` does not belong to `{thread_id}`"
                ))
            })?,
        None => thread_sessions[0],
    };

    let limit = bounded_limit(params).min(MAX_TRANSCRIPT_PAGE);
    let before = string_param(params, "before_message_id");
    let page = store
        .get_messages_before_exact(&selected.session_id, limit, before.as_deref())
        .await
        .map_err(|error| store_error("reading a meeting transcript page", error))?;
    let (messages, has_older) = page.ok_or_else(|| {
        ExecutionError::Step(
            "meetings_data read_thread: `before_message_id` does not name a message in this session"
                .to_owned(),
        )
    })?;

    let mut lines = Vec::with_capacity(messages.len());
    let mut skipped_non_text = 0usize;
    for message in &messages {
        match visible_transcript_line(message) {
            Some(line) => lines.push(line),
            None => skipped_non_text += 1,
        }
    }
    // Paging is by MESSAGE, not by projected line: the cursor must advance past
    // everything the store returned, otherwise a page of only-skipped rows
    // would stall the pager forever on the same boundary.
    let next_before_message_id = has_older
        .then(|| messages.first().map(|message| message.id.clone()))
        .flatten();

    Ok(json!({
        "scope": scope.as_json(),
        "thread_id": thread_id,
        "session_id": selected.session_id,
        "sessions": thread_sessions
            .iter()
            .map(|session| VisibleThreadSession {
                session_id: session.session_id.clone(),
                title: session.title.clone(),
                created_at: rfc3339(session.created_at),
                updated_at: rfc3339(session.updated_at),
            })
            .collect::<Vec<_>>(),
        "messages": lines,
        "skipped_non_text": skipped_non_text,
        "has_older": has_older,
        "next_before_message_id": next_before_message_id,
    }))
}

/// Only plain-text turns cross this seam. A meeting thread also carries tool
/// cards, attachments and task-status rows; those are runtime internals, not
/// what was said in the room, and none of their payloads are projected.
fn visible_transcript_line(message: &ChatMessage) -> Option<VisibleTranscriptLine> {
    let ChatMessageContent::Text { text, .. } = &message.content else {
        return None;
    };
    let transcript = message.source_surface.as_deref() == Some("meeting-transcript");
    // The transcript writer stores `"<speaker>: <utterance>"`; split it back so
    // a surface never has to parse prose to attribute a line. Only for rows the
    // transcript lane actually wrote — an ordinary chat message that happens to
    // contain a colon is not an attribution.
    let (speaker, body) = match transcript.then(|| split_speaker(text)).flatten() {
        Some((speaker, body)) => (Some(speaker), body),
        None => (None, text.as_str()),
    };
    Some(VisibleTranscriptLine {
        message_id: message.id.clone(),
        created_at: rfc3339(message.created_at),
        speaker: speaker.map(|value| bounded_text(value, MAX_ID_BYTES)),
        text: bounded_text(body, MAX_MESSAGE_TEXT_BYTES),
        transcript,
        created_at_ms: message.created_at,
    })
}

fn split_speaker(text: &str) -> Option<(&str, &str)> {
    let (speaker, body) = text.split_once(": ")?;
    let speaker = speaker.trim();
    if speaker.is_empty() || speaker.len() > MAX_ID_BYTES || speaker.contains('\n') {
        return None;
    }
    Some((speaker, body))
}

// ---------------------------------------------------------------------------
// read_takeaways
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct VisibleTakeawayProjection {
    key: String,
    thread_id: Option<String>,
    title: Option<String>,
    url: Option<String>,
    date: Option<String>,
    mode: Option<String>,
    summary: Option<String>,
    decisions: Vec<String>,
    action_items: Vec<String>,
    updated_at: Option<String>,
}

async fn read_takeaways(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params)?;
    let entries = meeting_takeaway_entries(workspace_layout, &scope).await?;
    let wanted_thread = string_param(params, "thread_id");

    let mut rows = entries
        .iter()
        .filter_map(visible_takeaway)
        .filter(|row| {
            wanted_thread
                .as_deref()
                .is_none_or(|thread| row.thread_id.as_deref() == Some(thread))
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        takeaway_instant(right.updated_at.as_deref())
            .cmp(&takeaway_instant(left.updated_at.as_deref()))
            .then_with(|| left.key.cmp(&right.key))
    });
    let matched = rows.len();
    let limit = bounded_limit(params);
    let truncated = rows.len() > limit;
    rows.truncate(limit);

    Ok(json!({
        "scope": scope.as_json(),
        "tier": meeting_takeaway_tier_key()?,
        "takeaways": rows,
        // How many rows the tier actually held for this filter. The writer
        // prunes to its own newest-N bound, which is env-tunable and therefore
        // NOT mirrored here: telling the operator "retention is 20" while the
        // writer keeps 50 would report present rows as pruned.
        "matched": matched,
        "truncated": truncated,
        "limit": limit,
    }))
}

/// The knowledge-document path the meeting takeaway tier actually occupies,
/// resolved through the writer's own normalizer. Fails closed rather than
/// guessing a path, because guessing wrong reads as "no meetings recorded".
fn meeting_takeaway_tier_key() -> Result<String, ExecutionError> {
    crate::magician_v2::chat::service::normalized_user_memory_tier_name(MEETING_MEMORY_TIER)
        .filter(|tier| !tier.is_empty())
        .ok_or_else(|| {
            ExecutionError::Step(format!(
                "meetings_data: `{MEETING_MEMORY_TIER}` is not an accepted user memory tier"
            ))
        })
}

async fn meeting_takeaway_entries(
    workspace_layout: &ArtifactV2Workspace,
    scope: &Scope,
) -> Result<Vec<Map<String, Value>>, ExecutionError> {
    let memory = AgentMemoryResolver::with_workspace_layout(workspace_layout.clone())
        .resolve_for_scope(&scope.principal, &scope.workspace)
        .map_err(|error| {
            ExecutionError::Step(format!(
                "meetings_data could not resolve the scope's memory service: {error}"
            ))
        })?;
    let knowledge = memory.load_user_knowledge().await.map_err(|error| {
        ExecutionError::Step(format!(
            "meetings_data reading the meeting takeaway tier failed: {error}"
        ))
    })?;
    let tier_key = meeting_takeaway_tier_key()?;
    let tier = tier_key
        .split('.')
        .try_fold(&knowledge, |current, segment| current.get(segment))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(tier
        .into_iter()
        .filter_map(|entry| match entry {
            Value::Object(fields) => fields
                .get("key")
                .and_then(Value::as_str)
                .is_some_and(|key| key.starts_with(MEETING_MEMORY_KEY_PREFIX))
                .then_some(fields),
            _ => None,
        })
        .collect())
}

fn visible_takeaway(entry: &Map<String, Value>) -> Option<VisibleTakeawayProjection> {
    let key = entry.get("key").and_then(Value::as_str)?;
    // The writer stamps `source_type` on every meeting entry. A tier row that
    // merely borrowed the `meeting:` key prefix is not a meeting takeaway.
    if entry.get("source_type").and_then(Value::as_str) != Some("meeting_capture") {
        return None;
    }
    Some(VisibleTakeawayProjection {
        key: bounded_text(key, MAX_ID_BYTES),
        thread_id: bounded_field(entry, "thread_id", MAX_ID_BYTES),
        title: bounded_field(entry, "title", MAX_ID_BYTES),
        url: bounded_field(entry, "url", MAX_ID_BYTES),
        date: bounded_field(entry, "date", 32),
        mode: bounded_field(entry, "mode", 32),
        summary: bounded_field(entry, "summary", MAX_TAKEAWAY_TEXT_BYTES),
        decisions: bounded_string_list(entry, "decisions"),
        action_items: bounded_string_list(entry, "action_items"),
        updated_at: bounded_field(entry, "updated_at", 64),
    })
}

fn bounded_field(entry: &Map<String, Value>, key: &str, max_bytes: usize) -> Option<String> {
    entry
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| bounded_text(value, max_bytes))
}

fn bounded_string_list(entry: &Map<String, Value>, key: &str) -> Vec<String> {
    entry
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .take(MAX_TAKEAWAY_LIST_ITEMS)
                .map(|item| bounded_text(item, MAX_TAKEAWAY_TEXT_BYTES))
                .collect()
        })
        .unwrap_or_default()
}

/// Sort key for a takeaway's `updated_at`. RFC3339 strings with mixed offsets
/// do not sort chronologically as text, so they are parsed; an unparseable or
/// absent stamp sorts oldest rather than winning the page.
fn takeaway_instant(updated_at: Option<&str>) -> i64 {
    updated_at
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc).timestamp_millis())
        .unwrap_or(i64::MIN)
}

// ---------------------------------------------------------------------------
// upcoming_meetings
// ---------------------------------------------------------------------------

async fn upcoming_meetings(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params)?;
    let auth_root = workspace_layout.capability_auth_root(&scope.principal, &scope.workspace);
    // Same repo-root assumption the first-party handler makes: the process CWD
    // is the repo root for every shipped launcher.
    let repo_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let scope_paths = CapabilityWorkspaceManager::new(workspace_layout.clone(), repo_root)
        .scope_paths(&scope.principal, &scope.workspace);
    // The binder never forces a refresh: an app read must not be able to drive
    // the gws CLI once per poll. It serves whatever the shared 60s cache holds,
    // refreshing only when that has lapsed.
    let payload = upcoming_meetings_cached(false, &auth_root, Some(scope_paths)).await;

    let events = payload
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let truncated = events.len() > MAX_UPCOMING_EVENTS;
    let events = events
        .into_iter()
        .take(MAX_UPCOMING_EVENTS)
        .collect::<Vec<_>>();
    Ok(json!({
        "scope": scope.as_json(),
        "accounts": payload.get("accounts").cloned().unwrap_or(Value::Array(Vec::new())),
        "events": events,
        // Per-account failures are surfaced, never hidden: one expired token
        // must not silently blank the section.
        "errors": payload.get("errors").cloned().unwrap_or(Value::Array(Vec::new())),
        "truncated": truncated,
        "event_ceiling": MAX_UPCOMING_EVENTS,
    }))
}

// ---------------------------------------------------------------------------
// search_meeting_memory
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct VisibleSearchHit {
    kind: &'static str,
    thread_id: String,
    session_id: Option<String>,
    message_id: Option<String>,
    created_at: Option<String>,
    speaker: Option<String>,
    excerpt: String,
    #[serde(skip)]
    created_at_ms: Option<i64>,
}

/// Keyword-only, and honest about it. Meeting takeaways, thread titles AND
/// transcript bodies are scanned, because "what was said about X" is false
/// advertising without the bodies; every lane is bounded by its own budget and
/// the result reports whether a budget was hit.
async fn search_meeting_memory(
    workspace_layout: &ArtifactV2Workspace,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let scope = scope_from_params(params)?;
    let needle = required_string(params, "text")?.to_lowercase();
    let limit = bounded_limit(params);
    let store = require_chat_store()?;

    let mut hits: Vec<VisibleSearchHit> = Vec::new();
    let mut scan_truncated = false;

    // 1. Takeaways — smallest lane, highest signal.
    for entry in meeting_takeaway_entries(workspace_layout, &scope).await? {
        let Some(row) = visible_takeaway(&entry) else {
            continue;
        };
        let Some(thread_id) = row.thread_id.clone() else {
            continue;
        };
        let haystacks = row
            .title
            .iter()
            .chain(row.summary.iter())
            .map(String::as_str)
            .chain(row.decisions.iter().map(String::as_str))
            .chain(row.action_items.iter().map(String::as_str));
        for haystack in haystacks {
            if let Some(excerpt) = keyword_excerpt(haystack, &needle) {
                hits.push(VisibleSearchHit {
                    kind: "takeaway",
                    thread_id: thread_id.clone(),
                    session_id: None,
                    message_id: None,
                    created_at: None,
                    speaker: None,
                    excerpt,
                    created_at_ms: None,
                });
                break;
            }
        }
    }

    // 2. Thread titles and 3. transcript bodies, over the newest meeting
    // sessions only. A meeting console asks about recent rooms; an unbounded
    // historical sweep is the retrieval-platform work this deliberately defers.
    let sessions = meeting_sessions(store.as_ref(), &scope).await?;
    let mut newest = latest_session_per_thread(&sessions);
    newest.sort_by(thread_order);
    if newest.len() > SEARCH_MAX_SESSIONS {
        scan_truncated = true;
        newest.truncate(SEARCH_MAX_SESSIONS);
    }

    let mut message_budget = SEARCH_MESSAGE_BUDGET;
    for session in newest {
        if let Some(excerpt) = session
            .title
            .as_deref()
            .and_then(|title| keyword_excerpt(title, &needle))
        {
            hits.push(VisibleSearchHit {
                kind: "thread",
                thread_id: session.ui_thread_id.clone(),
                session_id: Some(session.session_id.clone()),
                message_id: None,
                created_at: Some(rfc3339(session.created_at)),
                speaker: None,
                excerpt,
                created_at_ms: Some(session.created_at),
            });
        }
        if message_budget == 0 {
            scan_truncated = true;
            break;
        }
        let window = SEARCH_MESSAGES_PER_SESSION.min(message_budget);
        // Ask for one more than the budget allows: a session holding exactly
        // `window` messages was scanned IN FULL, and reporting that as
        // truncated would tell the operator a complete scan was partial.
        let messages = store
            .get_messages(&session.session_id, window + 1)
            .await
            .map_err(|error| store_error("scanning a meeting transcript", error))?;
        let scanned = messages.len().min(window);
        if messages.len() > window {
            scan_truncated = true;
        }
        // `get_messages` returns the newest `n` in CHRONOLOGICAL order, so the
        // budgeted window is the TAIL of what came back. Taking the head would
        // drop the newest turn of every over-budget session — the one most
        // likely to answer "what was just said about X".
        let skip = messages.len().saturating_sub(scanned);
        message_budget = message_budget.saturating_sub(scanned.max(1));
        for message in messages.iter().skip(skip) {
            let Some(line) = visible_transcript_line(message) else {
                continue;
            };
            if let Some(excerpt) = keyword_excerpt(&line.text, &needle) {
                hits.push(VisibleSearchHit {
                    kind: "transcript",
                    thread_id: session.ui_thread_id.clone(),
                    session_id: Some(session.session_id.clone()),
                    message_id: Some(line.message_id),
                    created_at: Some(line.created_at),
                    speaker: line.speaker,
                    excerpt,
                    created_at_ms: Some(line.created_at_ms),
                });
            }
        }
    }

    // Newest first over a total ordering; rows without a timestamp (takeaways)
    // sort after the dated ones rather than jostling among them.
    hits.sort_by(|left, right| {
        right
            .created_at_ms
            .unwrap_or(i64::MIN)
            .cmp(&left.created_at_ms.unwrap_or(i64::MIN))
            .then_with(|| left.thread_id.cmp(&right.thread_id))
            .then_with(|| left.message_id.cmp(&right.message_id))
    });
    let truncated_page = hits.len() > limit;
    hits.truncate(limit);

    Ok(json!({
        "scope": scope.as_json(),
        "query": needle,
        "matching": "keyword_substring",
        "hits": hits,
        "truncated_page": truncated_page,
        "scan_truncated": scan_truncated || truncated_page,
        "session_scan_budget": SEARCH_MAX_SESSIONS,
        "message_scan_budget": SEARCH_MESSAGE_BUDGET,
    }))
}

/// A bounded window AROUND the first case-insensitive match, or `None`.
///
/// Returning the head of the haystack instead would show the operator a hit
/// whose evidence is missing whenever the match sits past the excerpt bound —
/// which for a multi-kilobyte takeaway summary or transcript line is the
/// common case, not the edge case.
///
/// Every slice index is snapped to a character boundary; a multi-byte
/// transcript would otherwise panic on a mid-codepoint cut. The match is
/// located in a lowercased copy, so the returned window is taken from the
/// ORIGINAL text at the same byte offsets — `to_lowercase` can change byte
/// length for some scripts, so the located offset is treated as a hint and
/// re-snapped rather than trusted as an exact index into the original.
fn keyword_excerpt(haystack: &str, needle_lowercase: &str) -> Option<String> {
    let lowered = haystack.to_lowercase();
    let hit = lowered.find(needle_lowercase)?;
    if haystack.len() <= SEARCH_EXCERPT_BYTES {
        return Some(haystack.to_owned());
    }
    // Centre the window on the hit, clamped into the haystack.
    let lead = SEARCH_EXCERPT_BYTES / 3;
    let hit = hit.min(haystack.len());
    let mut start = hit.saturating_sub(lead);
    while start > 0 && !haystack.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = start
        .saturating_add(SEARCH_EXCERPT_BYTES)
        .min(haystack.len());
    while end > start && !haystack.is_char_boundary(end) {
        end -= 1;
    }
    let mut window = String::with_capacity(end - start + 6);
    if start > 0 {
        window.push('…');
    }
    window.push_str(&haystack[start..end]);
    if end < haystack.len() {
        window.push('…');
    }
    Some(window)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn require_chat_store() -> Result<Arc<dyn ChatStore>, ExecutionError> {
    global_chat_store().ok_or_else(|| {
        ExecutionError::Step(
            "meetings_data requires the running server's chat store; this process published none"
                .to_owned(),
        )
    })
}

/// Truncate on a character boundary and mark the cut. Never slices bytes.
fn bounded_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes.saturating_sub(3);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[derive(Debug, Clone)]
struct Scope {
    principal: String,
    workspace: String,
}

impl Scope {
    fn as_json(&self) -> Value {
        json!({"principal": self.principal, "workspace": self.workspace})
    }
}

fn scope_from_params(params: &HashMap<String, Value>) -> Result<Scope, ExecutionError> {
    Ok(Scope {
        principal: required_runtime_scope_value(params, "__principal")?,
        workspace: required_runtime_scope_value(params, "__workspace")?,
    })
}

fn authorize_runtime_scope(
    mut params: HashMap<String, Value>,
) -> Result<HashMap<String, Value>, ExecutionError> {
    let principal = required_runtime_scope_value(&params, "__principal")?;
    let workspace = required_runtime_scope_value(&params, "__workspace")?;
    if !LlmScope::new(&principal, &workspace).is_valid() {
        return Err(ExecutionError::Step(
            "meetings_data runtime scope contains an unsafe principal or workspace component"
                .to_owned(),
        ));
    }
    for (public_key, trusted_value) in [
        ("principal", principal.as_str()),
        ("workspace", workspace.as_str()),
    ] {
        if let Some(value) = params.get(public_key) {
            let Value::String(value) = value else {
                return Err(ExecutionError::Step(format!(
                    "meetings_data: `{public_key}` is an optional scope assertion and must be a string"
                )));
            };
            if !value.is_empty() && value != trusted_value {
                return Err(ExecutionError::Step(format!(
                    "meetings_data: model-supplied `{public_key}` does not match the runtime-authorized scope"
                )));
            }
        }
    }
    params.insert("principal".to_owned(), Value::String(principal));
    params.insert("workspace".to_owned(), Value::String(workspace));
    Ok(params)
}

fn required_runtime_scope_value(
    params: &HashMap<String, Value>,
    key: &str,
) -> Result<String, ExecutionError> {
    let value = params.get(key).and_then(Value::as_str).ok_or_else(|| {
        ExecutionError::Step(format!(
            "meetings_data requires runtime-owned scope `{key}`; unscoped execution is denied"
        ))
    })?;
    if value.is_empty() || value.trim() != value {
        return Err(ExecutionError::Step(format!(
            "meetings_data runtime-owned scope `{key}` must be a nonblank canonical component"
        )));
    }
    Ok(value.to_owned())
}

fn bounded_limit(params: &HashMap<String, Value>) -> usize {
    params
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_LIST_LIMIT as u64)
        .clamp(1, MAX_LIST_LIMIT) as usize
}

fn string_param(params: &HashMap<String, Value>, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn required_string(params: &HashMap<String, Value>, key: &str) -> Result<String, ExecutionError> {
    string_param(params, key).ok_or_else(|| {
        ExecutionError::Step(format!("meetings_data: `{key}` must be a non-empty string"))
    })
}

fn store_error(context: &str, error: anyhow::Error) -> ExecutionError {
    ExecutionError::Step(format!("meetings_data {context} failed: {error:#}"))
}

/// Compile-time reminder that the read binder never grows a write verb. The
/// control class starts, pauses and stops capture through its own reviewed
/// destination; a mutation reachable from here would make the binder-family
/// read-only invariant false.
const _: () = {
    let mut index = 0;
    while index < ACTIONS.len() {
        let action = ACTIONS[index].as_bytes();
        assert!(
            !starts_with(action, b"start")
                && !starts_with(action, b"stop")
                && !starts_with(action, b"join")
                && !starts_with(action, b"listen")
                && !starts_with(action, b"pause")
                && !starts_with(action, b"resume"),
            "meetings_data is a read binder; capture control belongs to the reviewed action class"
        );
        index += 1;
    }
};

const fn starts_with(value: &[u8], prefix: &[u8]) -> bool {
    if value.len() < prefix.len() {
        return false;
    }
    let mut index = 0;
    while index < prefix.len() {
        if value[index] != prefix[index] {
            return false;
        }
        index += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::chat::models::ChatMessageDirection;

    fn params(action: &str) -> HashMap<String, Value> {
        HashMap::from([
            ("__action_name".to_owned(), json!(action)),
            ("__principal".to_owned(), json!("owner")),
            ("__workspace".to_owned(), json!("default")),
        ])
    }

    fn session(
        thread: &str,
        id: &str,
        created_at: i64,
        updated_at: i64,
    ) -> ChatThreadSessionSummary {
        ChatThreadSessionSummary {
            ui_thread_id: thread.to_owned(),
            session_id: id.to_owned(),
            status: ChatSessionStatus::Active,
            title: None,
            agent_id: "agent".to_owned(),
            created_at,
            updated_at,
        }
    }

    fn text_message(id: &str, created_at: i64, text: &str, surface: Option<&str>) -> ChatMessage {
        ChatMessage::new(
            id.to_owned(),
            "session".to_owned(),
            ChatMessageDirection::System,
            ChatMessageContent::Text {
                text: text.to_owned(),
                plan_reply: None,
            },
            created_at,
        )
        .with_source_surface(surface.map(str::to_owned))
    }

    #[test]
    fn the_action_surface_is_closed_and_exact_targets_are_mandatory() {
        // Every action but the two that require an exact target proves with no
        // arguments at all; those two are asserted separately below.
        for action in ACTIONS {
            let argument_free = !matches!(*action, "read_thread" | "search_meeting_memory");
            assert_eq!(
                prove_app_meetings_data_args(&params(action)),
                argument_free,
                "`{action}` disagreed with its declared target requirement"
            );
        }
        assert!(!prove_app_meetings_data_args(&params("stop_meeting")));
        assert!(!prove_app_meetings_data_args(&params("read_map")));

        // read_thread has no default target and search has no default keyword.
        assert!(!prove_app_meetings_data_args(&params("read_thread")));
        assert!(!prove_app_meetings_data_args(&params(
            "search_meeting_memory"
        )));

        let mut read = params("read_thread");
        read.insert("thread_id".to_owned(), json!("meeting-acme-2026-09-02"));
        assert!(prove_app_meetings_data_args(&read));

        let mut search = params("search_meeting_memory");
        search.insert("text".to_owned(), json!("pricing"));
        assert!(prove_app_meetings_data_args(&search));

        // A missing __action_name is not an implicit action.
        let mut headless = params("list_threads");
        headless.remove("__action_name");
        assert!(!prove_app_meetings_data_args(&headless));
    }

    #[test]
    fn thread_ids_must_live_under_the_meeting_prefix() {
        let mut read = params("read_thread");
        read.insert("thread_id".to_owned(), json!("general"));
        assert!(
            !prove_app_meetings_data_args(&read),
            "an arbitrary chat thread is not reachable through a meetings read"
        );
        read.insert("thread_id".to_owned(), json!("meeting-"));
        assert!(
            !prove_app_meetings_data_args(&read),
            "the bare prefix names no thread"
        );
        read.insert("thread_id".to_owned(), json!("meeting-../general"));
        assert!(
            !prove_app_meetings_data_args(&read),
            "a traversal-shaped id fails the scope-id safety check"
        );
        read.insert("thread_id".to_owned(), json!("meeting-acme-2026-09-02"));
        assert!(prove_app_meetings_data_args(&read));
    }

    #[test]
    fn filters_limits_and_unknown_parameters_are_bounded_and_closed() {
        let mut listing = params("list_threads");
        listing.insert("limit".to_owned(), json!(0));
        assert!(!prove_app_meetings_data_args(&listing));
        listing.insert("limit".to_owned(), json!(MAX_LIST_LIMIT + 1));
        assert!(!prove_app_meetings_data_args(&listing));
        listing.insert("limit".to_owned(), json!(20));
        assert!(prove_app_meetings_data_args(&listing));

        listing.insert("status".to_owned(), json!("deleted"));
        assert!(!prove_app_meetings_data_args(&listing));
        listing.insert("status".to_owned(), json!("archived"));
        assert!(prove_app_meetings_data_args(&listing));

        listing.insert("text".to_owned(), json!("a"));
        assert!(
            !prove_app_meetings_data_args(&listing),
            "a one-character filter is a scan, not a filter"
        );
        listing.insert("text".to_owned(), json!("acme"));
        assert!(prove_app_meetings_data_args(&listing));

        listing.insert("thread_id".to_owned(), json!("meeting-acme-2026-09-02"));
        assert!(
            !prove_app_meetings_data_args(&listing),
            "list_threads has no thread target; an unknown key for this action is refused"
        );
        listing.remove("thread_id");

        listing.insert("sql".to_owned(), json!("select 1"));
        assert!(!prove_app_meetings_data_args(&listing));
    }

    #[test]
    fn routing_aliases_must_agree_with_the_resolved_action() {
        let mut listing = params("list_threads");
        listing.insert("operation".to_owned(), json!("read_thread"));
        assert!(!prove_app_meetings_data_args(&listing));
        listing.insert("operation".to_owned(), json!("list_threads"));
        assert!(prove_app_meetings_data_args(&listing));
    }

    #[test]
    fn runtime_scope_is_required_and_a_public_assertion_cannot_switch_it() {
        let mut unscoped = params("active_session");
        unscoped.remove("__principal");
        assert!(authorize_runtime_scope(unscoped).is_err());

        let mut mismatched = params("active_session");
        mismatched.insert("principal".to_owned(), json!("someone-else"));
        assert!(authorize_runtime_scope(mismatched).is_err());

        let authorized = authorize_runtime_scope(params("active_session")).expect("scoped");
        assert_eq!(authorized.get("principal"), Some(&json!("owner")));
        assert_eq!(authorized.get("workspace"), Some(&json!("default")));
    }

    #[test]
    fn thread_selection_is_newest_per_thread_and_totally_ordered() {
        let sessions = vec![
            session("meeting-acme-2026-09-01", "s1", 100, 100),
            session("meeting-acme-2026-09-01", "s2", 150, 250),
            session("meeting-zeta-2026-09-01", "s3", 150, 160),
        ];
        let mut latest = latest_session_per_thread(&sessions);
        latest.sort_by(thread_order);
        assert_eq!(latest.len(), 2);
        // Equal created_at (150) ties break on the thread id, descending.
        assert_eq!(latest[0].ui_thread_id, "meeting-zeta-2026-09-01");
        assert_eq!(latest[1].ui_thread_id, "meeting-acme-2026-09-01");
        // The rotated session with the newest updated_at represents its thread.
        assert_eq!(latest[1].session_id, "s2");
    }

    #[test]
    fn the_thread_cursor_is_a_keyset_boundary_a_vanished_row_cannot_strand() {
        let sessions = vec![
            session("meeting-c-2026-09-03", "s3", 300, 300),
            session("meeting-b-2026-09-02", "s2", 200, 200),
            session("meeting-a-2026-09-01", "s1", 100, 100),
        ];
        let mut rows = sessions.iter().collect::<Vec<_>>();
        rows.sort_by(thread_order);

        // The encoded tuple resumes strictly after its boundary.
        let mut listing = params("list_threads");
        listing.insert(
            "after_thread_id".to_owned(),
            json!("300~meeting-c-2026-09-03"),
        );
        let boundary = thread_cursor(&rows, &listing)
            .expect("a well-formed tuple decodes")
            .expect("a cursor");
        let start = rows.partition_point(|row| thread_sort_key(row) <= boundary.as_key());
        assert_eq!(start, 1);

        // And it still resumes when the row it named has left the set — the
        // failure the identity lookup could not survive.
        let without_c = rows[1..].to_vec();
        let start = without_c.partition_point(|row| thread_sort_key(row) <= boundary.as_key());
        assert_eq!(
            start, 0,
            "a vanished cursor row still defines a boundary the pager can resume from"
        );

        // A malformed tuple is refused rather than silently restarting page one.
        listing.insert("after_thread_id".to_owned(), json!("not-a-time~meeting-x"));
        assert!(thread_cursor(&rows, &listing).is_err());

        // A bare id that names nothing is still refused.
        listing.insert(
            "after_thread_id".to_owned(),
            json!("meeting-gone-2026-01-01"),
        );
        let error = thread_cursor(&rows, &listing)
            .expect_err("an unknown bare cursor must not silently restart the page");
        assert!(format!("{error}").contains("does not name a row"));
    }

    #[test]
    fn the_emitted_page_cursor_is_admitted_by_the_binder_s_own_proof() {
        // The read hands back `"<millis>~<thread-id>"`. If the closed proof
        // only admitted a bare thread id, page two would be unreachable — the
        // read would reject its own cursor.
        let mut listing = params("list_threads");
        for admitted in [
            "meeting-acme-2026-09-02",
            "1788325200000~meeting-acme-2026-09-02",
            "0~meeting-a",
        ] {
            listing.insert("after_thread_id".to_owned(), json!(admitted));
            assert!(
                prove_app_meetings_data_args(&listing),
                "`{admitted}` must be an admissible page boundary"
            );
        }
        for refused in [
            "~meeting-acme-2026-09-02",
            "notatime~meeting-acme-2026-09-02",
            "1788325200000~general",
            "1788325200000~meeting-",
            "12345678901234567890~meeting-acme",
            "1788325200000~meeting-../escape",
        ] {
            listing.insert("after_thread_id".to_owned(), json!(refused));
            assert!(
                !prove_app_meetings_data_args(&listing),
                "`{refused}` must not be an admissible page boundary"
            );
        }
    }

    #[test]
    fn only_text_turns_project_and_speakers_split_for_transcript_rows_only() {
        let transcript = text_message(
            "m1",
            10,
            "Priya: we ship on Friday",
            Some("meeting-transcript"),
        );
        let projected = visible_transcript_line(&transcript).expect("transcript line");
        assert_eq!(projected.speaker.as_deref(), Some("Priya"));
        assert_eq!(projected.text, "we ship on Friday");
        assert!(projected.transcript);

        // An ordinary chat turn keeps its colon; it is not an attribution.
        let chat = text_message("m2", 11, "note: check the invoice", None);
        let projected = visible_transcript_line(&chat).expect("text line");
        assert!(projected.speaker.is_none());
        assert_eq!(projected.text, "note: check the invoice");
        assert!(!projected.transcript);

        let card = ChatMessage::new(
            "m3".to_owned(),
            "session".to_owned(),
            ChatMessageDirection::Assistant,
            ChatMessageContent::ToolCallExecuted {
                tool_name: "meeting".to_owned(),
                summary: "joined".to_owned(),
                tool_call_id: None,
            },
            12,
        );
        assert!(
            visible_transcript_line(&card).is_none(),
            "runtime cards are not what was said in the room"
        );
    }

    #[test]
    fn projected_text_truncates_on_character_boundaries() {
        let wide = "é".repeat(64);
        let bounded = bounded_text(&wide, 16);
        assert!(
            bounded.len() <= 16,
            "the ellipsis is budgeted inside max_bytes, not added to it"
        );
        assert!(bounded.ends_with('…'));
        // Round-trips as valid UTF-8 by construction: slicing mid-codepoint
        // would have panicked above.
        assert!(bounded
            .chars()
            .all(|character| character == 'é' || character == '…'));
    }

    #[test]
    fn takeaway_projection_requires_the_writers_source_type() {
        let mut entry = Map::new();
        entry.insert("key".to_owned(), json!("meeting:meeting-acme-2026-09-02"));
        entry.insert("summary".to_owned(), json!("Agreed the Friday cut."));
        assert!(
            visible_takeaway(&entry).is_none(),
            "a tier row that merely borrows the key prefix is not a meeting takeaway"
        );
        entry.insert("source_type".to_owned(), json!("meeting_capture"));
        entry.insert("thread_id".to_owned(), json!("meeting-acme-2026-09-02"));
        entry.insert("decisions".to_owned(), json!(["Ship Friday", "", "  "]));
        let row = visible_takeaway(&entry).expect("meeting takeaway");
        assert_eq!(row.decisions, vec!["Ship Friday".to_owned()]);
        assert_eq!(row.summary.as_deref(), Some("Agreed the Friday cut."));
    }

    #[test]
    fn takeaway_ordering_parses_offsets_instead_of_comparing_strings() {
        // `2026-09-02T10:30:00+05:30` is 05:00Z — an hour EARLIER than
        // `2026-09-02T06:00:00Z` — yet it sorts later as a string. Parsing is
        // what makes the page order true.
        let earlier = takeaway_instant(Some("2026-09-02T10:30:00+05:30"));
        let later = takeaway_instant(Some("2026-09-02T06:00:00Z"));
        assert!(later > earlier);
        assert_eq!(takeaway_instant(None), i64::MIN);
        assert_eq!(takeaway_instant(Some("not-a-time")), i64::MIN);
    }

    #[test]
    fn keyword_excerpts_are_case_insensitive_and_bounded() {
        assert!(keyword_excerpt("We agreed on PRICING", "pricing").is_some());
        assert!(keyword_excerpt("nothing relevant", "pricing").is_none());
        let long = "pricing ".repeat(200);
        let excerpt = keyword_excerpt(&long, "pricing").expect("hit");
        assert!(excerpt.len() <= SEARCH_EXCERPT_BYTES + 2 * "…".len());

        // The whole point: a match past the excerpt bound must still appear IN
        // the excerpt. Returning the head of the haystack would show a hit with
        // its evidence missing.
        let buried = format!("{}NEEDLE tail", "x".repeat(5_000));
        let excerpt = keyword_excerpt(&buried, "needle").expect("buried hit");
        assert!(
            excerpt.contains("NEEDLE"),
            "the excerpt must window around the match, not start at byte zero"
        );
        assert!(excerpt.len() <= SEARCH_EXCERPT_BYTES + 2 * "…".len());

        // Multi-byte text either side of the match never slices a codepoint.
        let wide = format!("{}needle{}", "é".repeat(400), "é".repeat(400));
        let excerpt = keyword_excerpt(&wide, "needle").expect("wide hit");
        assert!(excerpt.contains("needle"));
    }
}

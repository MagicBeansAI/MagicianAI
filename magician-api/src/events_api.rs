//! Unified event-stream HTTP endpoint.
//!
//! `GET /api/magician/v3/events` — operator-facing live tail of the
//! `RuntimeTransportBroadcaster` channel, optionally backfilled from a
//! per-execution `events.jsonl` slice. Returns NDJSON over chunked HTTP
//! so the UI can render a "tail -f events.jsonl"-style live feed without
//! the operator dropping into a terminal.
//!
//! See `docs/archive/plans/2026-05-10-execution-panel-canvas-redesign.md` for the
//! full design + filter contract. This module covers Phase A1 of that
//! plan: live-tail + optional per-execution_id backfill. Cross-scope
//! backfill (24h paginated across all executions) is a follow-up.

use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, warn};

use crate::scope::resolve_required_scope_ref;
use crate::websocket_handler::event_visible_to_scope;
use magician::magician_v2::artifact_v2::{ArtifactV2Service, ScopeRef, V3ReadApi};
use magician::magician_v2::realtime_events::{
    EventCategory, EventSeverity, EventTaxonomy, RuntimeTransportBroadcaster, RuntimeTransportEvent,
};

/// Shared state for the `/events` endpoint. One instance per process,
/// wired in `bin/magician.rs::HttpServer::new` via `app_data`.
#[derive(Clone)]
pub struct EventsApi {
    broadcaster: Arc<RuntimeTransportBroadcaster>,
    artifact_v2_service: Arc<ArtifactV2Service>,
    /// Per-(principal, workspace) transport event log. Optional during
    /// the migration window — wired in startup; tests can leave it
    /// `None` and only see per-execution canonical events.
    workspace_event_log_registry:
        Option<magician::magician_v2::transport_log::WorkspaceEventLogRegistry>,
}

impl EventsApi {
    pub fn new(
        broadcaster: Arc<RuntimeTransportBroadcaster>,
        artifact_v2_service: Arc<ArtifactV2Service>,
    ) -> Self {
        Self {
            broadcaster,
            artifact_v2_service,
            workspace_event_log_registry: None,
        }
    }

    pub fn with_workspace_event_log_registry(
        mut self,
        registry: magician::magician_v2::transport_log::WorkspaceEventLogRegistry,
    ) -> Self {
        self.workspace_event_log_registry = Some(registry);
        self
    }
}

/// Query parameters for the `/events` endpoint. All optional; an empty
/// query returns the live tail across the resolved scope.
#[derive(Debug, Default, Deserialize)]
pub struct EventsQuery {
    /// Scope override (header takes precedence). See `scope::resolve_required_scope_ref`.
    pub workspace: Option<String>,

    /// When set, the endpoint backfills the per-execution `events.jsonl`
    /// before streaming live events. Without this, only live events flow.
    pub execution_id: Option<String>,
    /// Required alongside `execution_id` for the disk path; if absent
    /// we'll attempt to look it up via the artifact service.
    pub task_id: Option<String>,

    /// Filter by agent_id (matches against AgentEvent envelopes + lifecycle events).
    pub agent_id: Option<String>,

    /// Earliest timestamp (Unix ms) to include from backfill. When
    /// omitted, cross-scope backfill defaults to "last 24h"; per-execution
    /// backfill applies no implicit floor.
    pub since: Option<i64>,
    /// Pagination cursor — only include backfill rows with
    /// `timestamp_ms < before`. Live tail is unaffected. Used by the UI to
    /// fetch the previous page (older events) without rescanning the
    /// already-shown rows.
    pub before: Option<i64>,
    /// Backfill row cap. Default 1000.
    pub limit: Option<usize>,

    /// Taxonomy filters (matched against `RuntimeTransportEvent::taxonomy()`).
    /// Multiple values comma-separated (`category=execution,agentic`).
    pub category: Option<String>,
    pub severity: Option<String>,
    /// `true` → only `user_relevant=true` events. `false` → only
    /// `user_relevant=false`. Omitted → all.
    pub user_relevant: Option<bool>,

    /// Substring match against event_type.
    pub event_type: Option<String>,
    /// Free-text search; matched against the serialized event JSON.
    pub search: Option<String>,

    /// When `true`, the response includes Phase 1 backfill ONLY and
    /// the connection closes as soon as backfill drains. The default
    /// (`false` / omitted) keeps the live-tail subscription open
    /// until the client disconnects — the right shape for live
    /// monitoring UIs (`/events`), but it traps fetch-style callers
    /// that use `await response.text()` because the body never ends.
    /// Set this from history-snapshot consumers (the Attention page's
    /// "recently resolved" pull, etc.) so `timedFetch` resolves on
    /// backfill completion instead of timing out at the default 30s.
    pub backfill_only: Option<bool>,
}

/// Request body for `POST /api/magician/v3/events/debug-emit` — a local
/// dev/test injector that pushes a synthetic `RuntimeTransportEvent`
/// onto the live broadcaster so the desktop notify-overlay can be tested
/// end-to-end, decoupled from the agent / HITL execution path. See
/// `debug_emit_event_handler` for the kind → event mapping (the endpoint is
/// always enabled — no env flag).
#[derive(Debug, Deserialize)]
pub struct DebugEmitRequest {
    /// Which synthetic card to inject. One of:
    /// `"approval"` (confirmation → inline Approve/Reject),
    /// `"approval_input"` (text input → Open-in-app),
    /// `"error"` (`ExecutionFailed` → error toast),
    /// `"completion"` (`ExecutionCompleted` success → success toast).
    pub kind: String,
    /// Prompt / error message / title body, depending on `kind`.
    #[serde(default)]
    pub text: Option<String>,
    /// Supplementary hint (approval kinds only — preview/subtext).
    #[serde(default)]
    pub hint: Option<String>,
    /// HITL `source` override (approval kinds). Defaults to `"approval"`.
    #[serde(default)]
    pub source: Option<String>,
    /// Scope the synthetic event is stamped with. Must match the
    /// overlay's `/events` subscription scope (default anonymous/default)
    /// for the card to pass the live-tail `event_visible_to_scope` filter.
    #[serde(default)]
    pub principal: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Clone, Default)]
struct CompiledFilters {
    categories: Option<Vec<EventCategory>>,
    severities: Option<Vec<EventSeverity>>,
    user_relevant: Option<bool>,
    event_type_substring: Option<String>,
    search_substring: Option<String>,
    agent_id: Option<String>,
}

impl CompiledFilters {
    fn from_query(query: &EventsQuery) -> Self {
        Self {
            categories: query.category.as_deref().map(parse_categories),
            severities: query.severity.as_deref().map(parse_severities),
            user_relevant: query.user_relevant,
            event_type_substring: query
                .event_type
                .as_ref()
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty()),
            search_substring: query
                .search
                .as_ref()
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty()),
            agent_id: query
                .agent_id
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        }
    }

    fn passes_taxonomy(&self, taxonomy: &EventTaxonomy) -> bool {
        if let Some(ref cats) = self.categories {
            if !cats.contains(&taxonomy.category) {
                return false;
            }
        }
        if let Some(ref sevs) = self.severities {
            if !sevs.contains(&taxonomy.severity) {
                return false;
            }
        }
        if let Some(ur) = self.user_relevant {
            if taxonomy.user_relevant != ur {
                return false;
            }
        }
        true
    }

    fn passes_event_type(&self, event_type: &str) -> bool {
        match self.event_type_substring.as_deref() {
            None => true,
            Some(needle) => event_type.to_lowercase().contains(needle),
        }
    }

    fn passes_search(&self, serialized: &str) -> bool {
        match self.search_substring.as_deref() {
            None => true,
            Some(needle) => serialized.to_lowercase().contains(needle),
        }
    }

    fn passes_agent(&self, agent_id_in_event: Option<&str>) -> bool {
        match (self.agent_id.as_deref(), agent_id_in_event) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(want), Some(got)) => want == got,
        }
    }
}

fn parse_categories(input: &str) -> Vec<EventCategory> {
    input
        .split(',')
        .filter_map(|raw| match raw.trim().to_lowercase().as_str() {
            "pipeline" => Some(EventCategory::Pipeline),
            "plan" => Some(EventCategory::Plan),
            "tool" => Some(EventCategory::Tool),
            "slot" => Some(EventCategory::Slot),
            "clarification" => Some(EventCategory::Clarification),
            "execution" => Some(EventCategory::Execution),
            "llm" => Some(EventCategory::Llm),
            "agentic" => Some(EventCategory::Agentic),
            "hitl" => Some(EventCategory::Hitl),
            "agent" => Some(EventCategory::Agent),
            "task" => Some(EventCategory::Task),
            "feed" => Some(EventCategory::Feed),
            "observability" => Some(EventCategory::Observability),
            "media" => Some(EventCategory::Media),
            "activity" => Some(EventCategory::Activity),
            // Unknown tokens are dropped rather than rejected, so a typo
            // in `?category=` narrows the result set instead of erroring.
            //
            // Nothing in the type system requires this ladder to cover
            // every `EventCategory` — it is a string match, not a match on
            // the enum, so a new variant compiles fine while silently
            // matching no events. `media` sat missing that way until the
            // activity family was added. The gate is now
            // `every_event_category_parses_from_its_wire_token`, whose
            // exhaustive match over `EventCategory` fails to compile when a
            // variant is added and then fails the assertion until the
            // variant is added here too.
            _ => None,
        })
        .collect()
}

fn parse_severities(input: &str) -> Vec<EventSeverity> {
    input
        .split(',')
        .filter_map(|raw| match raw.trim().to_lowercase().as_str() {
            "info" => Some(EventSeverity::Info),
            "warn" => Some(EventSeverity::Warn),
            "error" => Some(EventSeverity::Error),
            "decision" => Some(EventSeverity::Decision),
            "attention" => Some(EventSeverity::Attention),
            _ => None,
        })
        .collect()
}

/// Maximum buffered NDJSON lines before back-pressuring the producers.
/// Each line is small (a few KB max), so 1024 keeps memory bounded while
/// giving plenty of headroom for bursty live tails.
const NDJSON_CHANNEL_CAPACITY: usize = 1024;
/// Default backfill row cap. Aligned with
/// `transport_log::EVENTS_RETENTION_MIN_COUNT` so a quiet scope whose
/// per-scope log has been filled to the retention floor is returned in
/// full — no read-time truncation tighter than the on-disk retention.
const DEFAULT_BACKFILL_LIMIT: usize =
    magician::magician_v2::transport_log::EVENTS_RETENTION_MIN_COUNT;

/// Default backfill window for the global `/events` view (no
/// `execution_id` scope). 24 hours, in milliseconds. Large enough to
/// cover an overnight triage session; small enough that scanning the
/// matching events.jsonl files stays cheap.
const DEFAULT_CROSS_SCOPE_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;

/// Hard cap on the number of events scanned during cross-scope backfill,
/// regardless of how many match the filters. Bounds CPU/memory on
/// scopes with very large execution histories. Crossing this cap is
/// signalled to the operator via a synthetic `__events_partial__`
/// row at the end of the backfill so the UI can show a
/// "scan limit reached, refine filters" hint.
const CROSS_SCOPE_HARD_SCAN_CAP: usize = 50_000;

/// `GET /api/magician/v3/events`
pub async fn list_events_v3_handler(
    api: web::Data<EventsApi>,
    http_req: HttpRequest,
    query: web::Query<EventsQuery>,
) -> actix_web::Result<HttpResponse> {
    let scope = match resolve_required_scope_ref(http_req.headers(), query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };

    let filters = CompiledFilters::from_query(&query);
    let backfill_limit = query.limit.unwrap_or(DEFAULT_BACKFILL_LIMIT);
    let since_ms = query.since;
    let before_ms = query.before;
    let task_filter = query
        .task_id
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let backfill_only = query.backfill_only.unwrap_or(false);

    let (tx, rx) = mpsc::channel::<Result<web::Bytes, std::io::Error>>(NDJSON_CHANNEL_CAPACITY);

    // Flush an immediate keepalive byte so the response HEAD reaches the client
    // even when the scope is quiet (no backfill/live events yet). A dev proxy
    // (Vite) buffers a streaming response's head until the first body chunk, so
    // without this a quiet stream never delivers headers — the cockpit's 12s
    // connect watchdog then fires and loops "connecting…/error — retrying"
    // forever (esp. on a terminal task with no live events). A blank line is
    // skipped by every NDJSON consumer (they ignore empty lines), and is harmless
    // for `backfill_only` history pulls (`response.text()` splits + skips it).
    let _ = tx.send(Ok(web::Bytes::from_static(b"\n"))).await;

    // Subscribe to the LIVE broadcaster BEFORE spawning the backfill task.
    //
    // Ordering matters: `broadcaster` is a `tokio::sync::broadcast`, which
    // only delivers to receivers that already exist at broadcast time. If we
    // spawned backfill first and subscribed afterwards, any event broadcast
    // during the backfill window would be delivered to NEITHER side — too late
    // to land in the disk snapshot the backfill reads, too early for a live
    // receiver that doesn't exist yet. Subscribing first closes that gap: the
    // bounded broadcast buffer captures every event from this point, and the
    // live loop below drains whatever buffered during backfill before going
    // fully live.
    //
    // The cost of the reorder is a benign OVERLAP: an event can now be sent
    // both via backfill (read from disk) and via the live tail (buffered since
    // this subscribe). That is intentional and safe — the frontend already
    // dedups overlapping backfill+live rows by `correlation_id` / event
    // identity (see the `cross_scope_backfill` Step-0 note re: frontend dedup,
    // ~line 793; and `applyEvent`'s coalesce-by-id in the notify overlay). No
    // server-side dedup is added here. In the `backfill_only` early-return
    // path below this receiver is simply dropped unused (its buffer freed).
    let mut live_rx = api.broadcaster.subscribe();

    // Phase 1 — backfill. Two paths:
    //   (a) per-execution: when both execution_id and task_id are
    //       provided, read the single events.jsonl directly.
    //   (b) cross-scope: when execution_id is absent, enumerate every
    //       task→execution in the active scope, pre-filter executions
    //       by `updated_at` against the effective `since` floor, and
    //       walk each matching events.jsonl applying the same filters.
    //       Default window is 24h; bounded by `CROSS_SCOPE_HARD_SCAN_CAP`
    //       events scanned to keep CPU/memory bounded.
    if let (Some(execution_id), Some(task_id)) =
        (query.execution_id.as_deref(), query.task_id.as_deref())
    {
        let backfill_tx = tx.clone();
        let workspace = api.artifact_v2_service.workspace().clone();
        let workspace_path = workspace.execution_events_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
            execution_id,
        );
        let commit_path = workspace.execution_events_commit_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
            execution_id,
        );
        let filters_for_backfill = filters.clone();
        tokio::spawn(async move {
            backfill_from_jsonl(
                workspace,
                workspace_path,
                commit_path,
                backfill_tx,
                filters_for_backfill,
                since_ms,
                before_ms,
                backfill_limit,
            )
            .await;
        });
    } else {
        let backfill_tx = tx.clone();
        let artifact_service = Arc::clone(&api.artifact_v2_service);
        let scope_for_backfill = scope.clone();
        let filters_for_backfill = filters.clone();
        let registry_for_backfill = api.workspace_event_log_registry.clone();
        tokio::spawn(async move {
            cross_scope_backfill(
                artifact_service,
                scope_for_backfill,
                backfill_tx,
                filters_for_backfill,
                since_ms,
                before_ms,
                backfill_limit,
                task_filter,
                registry_for_backfill,
            )
            .await;
        });
    }

    // Phase 2 — live tail. Forward filtered events from the already-subscribed
    // `live_rx` (created before Phase 1) until the client disconnects (mpsc
    // send error or `live_tx.closed()`) or the broadcaster shuts down. The
    // `tokio::select!` is load-bearing:
    // without it, the task blocks on `live_rx.recv()` forever in scopes
    // that aren't currently emitting events, and the broadcaster
    // subscription + mpsc sender + TCP socket fd all leak when the
    // client tab closes or filters change. Race the recv against
    // `live_tx.closed()` so the task wakes immediately when the
    // response-stream receiver is dropped (i.e. actix tore down the
    // request because the client disconnected).
    //
    // `backfill_only=true` skips this entire phase — `tx` is dropped
    // immediately after the Phase 1 spawn, so the channel closes as
    // soon as backfill drains. Required for history-snapshot callers
    // that use `await response.text()` (e.g. the Attention page's
    // "recently resolved" pull): without the early-close, the live
    // tail holds the body open forever and the caller's `timedFetch`
    // aborts at the 30s default. (`live_rx` was subscribed above but is
    // never read on this path — it is dropped here, freeing its buffer.)
    if backfill_only {
        drop(tx);
        let stream = ReceiverStream::new(rx);
        return Ok(HttpResponse::Ok()
            .content_type("application/x-ndjson")
            .insert_header(("Cache-Control", "no-cache"))
            .insert_header(("X-Accel-Buffering", "no"))
            .streaming(stream));
    }
    // `live_rx` was subscribed BEFORE the backfill spawn (see the note above
    // the Phase 1 block) so it has been buffering every broadcast since then —
    // this loop first drains that buffer (covering the backfill-window gap),
    // then continues live.
    let live_scope = scope.clone();
    let live_filters = filters;
    let live_tx = tx;
    tokio::spawn(async move {
        // Periodic server-side keepalive so an idle scope (no broadcast
        // traffic) never yields a truly silent SSE body. A blank NDJSON line
        // is ignored by every consumer (`ingestLine` skips empty lines) and
        // burns NO broadcast capacity, so it keeps intermediaries with an idle
        // timeout from tearing the stream down and lets the client tell a
        // healthy-but-quiet connection from a dead one. Skip the immediate
        // first tick so we don't emit a redundant byte right after the connect
        // keepalive above.
        let mut keepalive_interval = tokio::time::interval(std::time::Duration::from_secs(15));
        keepalive_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        keepalive_interval.tick().await; // consume the immediate first tick
        loop {
            tokio::select! {
                // Branch 1 — broadcaster delivered an event.
                event_result = live_rx.recv() => match event_result {
                    Ok(event) => {
                        if !event_visible_to_scope(
                            &event,
                            &live_scope.principal(),
                            &live_scope.workspace(),
                        ) {
                            continue;
                        }
                        let Some(line) = serialize_event_if_passes(&event, &live_filters) else {
                            continue;
                        };
                        if live_tx.send(Ok(web::Bytes::from(line))).await.is_err() {
                            // Client disconnected mid-send.
                            break;
                        }
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        debug!(
                            skipped = skipped,
                            "[/events] live tail lagged; dropping {} events", skipped
                        );
                        // Surface the gap to the UI as a synthetic NDJSON
                        // sentinel — the broadcast channel just dropped
                        // `skipped` events because the client (or this
                        // forwarding task) couldn't drain fast enough.
                        // Without this, the operator silently sees an
                        // incomplete event stream with no indication
                        // anything was missed. Mirrors the
                        // `__events_partial__` sentinel from the cross-
                        // scope backfill scan-cap branch.
                        let sentinel = serde_json::json!({
                            "event_type": "__events_lagged__",
                            "timestamp_ms": chrono::Utc::now().timestamp_millis(),
                            "skipped": skipped,
                            "message": format!(
                                "Live event stream dropped {skipped} events because the channel \
                                 buffer was exhausted. Refresh the page to backfill recent activity."
                            ),
                        });
                        if let Ok(out) = serde_json::to_string(&sentinel) {
                            let mut payload = out;
                            payload.push('\n');
                            if live_tx.send(Ok(web::Bytes::from(payload))).await.is_err() {
                                break;
                            }
                        }
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break;
                    },
                },

                // Branch 2 — client disconnected. Wakes immediately when
                // the response-stream `rx` half is dropped. Without this
                // branch, an idle scope (no broadcast traffic) keeps this
                // task pinned in `recv()` forever, leaking the
                // subscription + sender + TCP socket fd.
                _ = live_tx.closed() => {
                    break;
                },

                // Branch 3 — periodic keepalive. Emits a blank NDJSON line so
                // an idle stream never goes silent for >15s; a send error
                // means the client is gone, so tear down like Branch 2.
                _ = keepalive_interval.tick() => {
                    if live_tx.send(Ok(web::Bytes::from_static(b"\n"))).await.is_err() {
                        break;
                    }
                },
            }
        }
    });

    let stream = ReceiverStream::new(rx);
    Ok(HttpResponse::Ok()
        .content_type("application/x-ndjson")
        .insert_header(("Cache-Control", "no-cache"))
        .insert_header(("X-Accel-Buffering", "no"))
        .streaming(stream))
}

/// Correlation-id prefix stamped on every synthetic HITL event minted by the
/// debug event-emit endpoint below. The HITL resolve handler keys on this
/// prefix to recognise a test event that has NO backing pause/approval record
/// and acknowledge it as a pure `HitlResolved` broadcast instead of 404ing.
/// Mint (here) and detect (`respond_hitl_handler`) must stay in sync, so both
/// go through this const / [`is_debug_hitl_correlation`].
pub const DEBUG_HITL_CORRELATION_PREFIX: &str = "debug-";

/// True when `correlation_id` was minted by the debug event-emit endpoint
/// (see [`DEBUG_HITL_CORRELATION_PREFIX`]) and therefore backs no real pause or
/// approval record — the resolve handler pure-acks these instead of 404ing.
pub fn is_debug_hitl_correlation(correlation_id: &str) -> bool {
    correlation_id.starts_with(DEBUG_HITL_CORRELATION_PREFIX)
}

/// `POST /api/magician/v3/events/debug-emit`
///
/// DEBUG-only: inject a synthetic `RuntimeTransportEvent` onto the LIVE
/// broadcaster so the desktop notify-overlay can be exercised end-to-end
/// without driving the real agent / HITL path. Lets the operator bisect
/// "is the overlay broken?" from "is the upstream emitter broken?".
///
/// Emits via `broadcaster.emit_transport_only` — a live-broadcast-only
/// send (no canonical `events.jsonl` write) so the synthetic event reaches
/// a connected overlay (the V3 `/events` live tail subscribes to
/// `broadcaster.subscribe()`) without polluting durable backfill. The
/// `principal` / `workspace` are stamped onto the event so the live tail's
/// `event_visible_to_scope` check passes for an overlay subscribed at the
/// same scope (default anonymous/default).
///
/// Intentionally always enabled — this is a local dev/test event injector that
/// the owner relies on with no env flag (no `MAGICIAN_DEBUG_EVENTS` gate, no
/// `cfg!(debug_assertions)` build gate).
pub async fn debug_emit_event_handler(
    api: web::Data<EventsApi>,
    body: web::Json<DebugEmitRequest>,
) -> HttpResponse {
    // No gate: the debug-emit endpoint is always on (local dev/test injector, per owner request).
    let kind = body.kind.trim().to_string();
    let principal = body
        .principal
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("anonymous")
        .to_string();
    let workspace = body
        .workspace
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("default")
        .to_string();
    let hint = body
        .hint
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let text = body
        .text
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    // Match the millisecond timestamp unit every real emit site for these
    // variants uses (e.g. `bots/auth_hitl_broker.rs`,
    // `gaui/emitter.rs`) — NOT `.timestamp()` (seconds).
    let timestamp = chrono::Utc::now().timestamp_millis();
    let id = format!(
        "{DEBUG_HITL_CORRELATION_PREFIX}{kind}-{}",
        uuid::Uuid::new_v4()
    );

    let event = match kind.as_str() {
        "approval" | "approval_input" => {
            let input_type = if kind == "approval" {
                "confirmation"
            } else {
                "text"
            };
            RuntimeTransportEvent::HitlRequested {
                correlation_id: id.clone(),
                source: body
                    .source
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("approval")
                    .to_string(),
                input_type: input_type.to_string(),
                prompt: text.unwrap_or_else(|| "Debug approval — Approve or Reject?".to_string()),
                hint,
                input_schema: None,
                task_id: None,
                execution_id: None,
                agent_id: None,
                principal: Some(principal.clone()),
                workspace: Some(workspace.clone()),
                timestamp,
            }
        },
        "error" => RuntimeTransportEvent::ExecutionFailed {
            execution_id: id.clone(),
            principal: Some(principal.clone()),
            workspace: Some(workspace.clone()),
            plan_id: "debug".to_string(),
            step_index: 0,
            step_id: "debug".to_string(),
            error: text.unwrap_or_else(|| "Debug error".to_string()),
            timestamp,
        },
        "completion" => RuntimeTransportEvent::ExecutionCompleted {
            execution_id: id.clone(),
            principal: Some(principal.clone()),
            workspace: Some(workspace.clone()),
            plan_id: "debug".to_string(),
            steps_total: 1,
            success: true,
            timestamp,
        },
        _ => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("unknown kind '{kind}'"),
                "valid_kinds": ["approval", "approval_input", "error", "completion"],
            }));
        },
    };

    // Live-broadcast only — reaches the connected overlay's `/events`
    // subscriber without writing the canonical event log.
    api.broadcaster.emit_transport_only(event);

    HttpResponse::Ok().json(serde_json::json!({
        "ok": true,
        "kind": kind,
        "id": id,
        "principal": principal,
        "workspace": workspace,
    }))
}

// Chat-turn coupling has moved out of this endpoint entirely. The
// activity card now consumes the per-chat-turn projection that
// `ChatTurnEventSink` writes, served from
// `GET /api/magician/v2/chat/sessions/{sid}/turns/{cid}/events` for
// backfill and the matching `/events/stream` SSE for live tail. The
// sink applies the "is this chat-bound?" filter exactly once; this
// endpoint stays focused on debug / observability surfaces.

/// Reads a per-execution `events.jsonl` line by line, applies filters,
/// and forwards matching rows as NDJSON. On read failure we just stop —
/// the live tail still runs.
async fn backfill_from_jsonl(
    workspace: magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
    path: std::path::PathBuf,
    commit_path: std::path::PathBuf,
    tx: mpsc::Sender<Result<web::Bytes, std::io::Error>>,
    filters: CompiledFilters,
    since_ms: Option<i64>,
    before_ms: Option<i64>,
    limit: usize,
) {
    use tokio::fs::File;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

    let committed_len = match workspace
        .committed_jsonl_len_path(&path, &commit_path)
        .await
    {
        Ok(committed_len) => committed_len,
        Err(error) => {
            warn!(path = %path.display(), error = %error, "[/events] invalid events.jsonl commit authority");
            return;
        },
    };

    let file = match File::open(&path).await {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            warn!(path = %path.display(), error = %err, "[/events] failed to open events.jsonl");
            return;
        },
    };
    let reader = BufReader::new(file.take(committed_len));
    let mut lines = reader.lines();
    let mut emitted = 0usize;
    let mut scanned = 0usize;
    while let Ok(Some(raw_line)) = lines.next_line().await {
        if emitted >= limit {
            break;
        }
        scanned += 1;
        // Sampled disconnect check so a tightly-filtered scan over a
        // large events.jsonl releases the file fd promptly when the
        // client has already gone away. Without this the loop only
        // notices on `tx.send` — which never runs if every line is
        // filtered out — and the fd stays open until end-of-file.
        if scanned.is_multiple_of(256) && tx.is_closed() {
            return;
        }
        let Some((_, line)) =
            filter_canonical_event_line(&raw_line, &filters, since_ms, before_ms, None)
        else {
            continue;
        };
        if tx.send(Ok(web::Bytes::from(line))).await.is_err() {
            return;
        }
        emitted += 1;
    }
}

/// Applies all filter predicates to a single raw `events.jsonl` line.
/// Returns the parsed timestamp (or `0` when absent) and the line +
/// trailing newline ready to write to the NDJSON stream, or `None` when
/// the line should be skipped. Returning the timestamp lets
/// `cross_scope_backfill` sort rows desc-by-timestamp without re-parsing.
///
/// Shared between per-execution backfill (`backfill_from_jsonl`) and
/// cross-scope backfill (`cross_scope_backfill`) so both paths apply
/// the same predicates and produce byte-identical wire output.
///
/// `task_filter` is applied here, on the value this function has ALREADY
/// parsed, rather than by the caller re-running `serde_json::from_str` over
/// the same line. Only the workspace-log leg passes one: it reads a
/// workspace-scoped file that mixes every task, whereas the per-execution
/// legs have already selected their lines by path. Semantics are unchanged —
/// a row is dropped only when a `task_id` is positively extracted and
/// differs; rows carrying no extractable id (chat events, most agentic
/// envelopes) still pass, which is the `unwrap_or(true)` the caller relied on.
///
/// Note on `since_ms` semantics: per-execution backfill passes through
/// the caller's `since` (possibly `None` → no floor), while cross-scope
/// backfill always passes `Some(effective_since)` (defaulting to
/// `now - 24h` when caller is silent, per the `/events` design — see
/// `EventsQuery::since_ms` doc). To widen the cross-scope window beyond
/// 24h, callers must explicitly set `since=0` (or any value older than
/// 24h ago).
fn filter_canonical_event_line(
    raw_line: &str,
    filters: &CompiledFilters,
    since_ms: Option<i64>,
    before_ms: Option<i64>,
    task_filter: Option<&str>,
) -> Option<(i64, String)> {
    let trimmed = raw_line.trim();
    if trimmed.is_empty() {
        return None;
    }
    // App owner notifications are exclusively owned by the expiry-aware
    // UserRequest/Attention surfaces. Refuse legacy workspace-log copies on
    // every backfill read even if the asynchronous physical scrub has not yet
    // reached this scope; their absolute TTL must not be bypassed by replay.
    if !magician::magician_v2::transport_log::serialized_event_is_safe_for_generic_backfill(trimmed)
    {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    // Unwrap `AgentEvent` envelopes to the real nested type (e.g.
    // `data.event.event_type = "coding.thinking"`) BEFORE filtering — exactly
    // like the live-tail path (`serialize_event_if_passes`). Without this, every
    // enveloped event's type read as the top-level `"AgentEvent"`, so an
    // `event_type=coding.` backfill matched nothing and the cockpit's spine /
    // chat history came back EMPTY on refresh (only live events ever rendered).
    let event_type_owned = unwrap_event_type(&value);
    let event_type = event_type_owned.as_str();
    // Canonical events on disk carry their time as:
    //   - `timestamp` at top level: RFC3339 string (`"2026-05-09T21:32:15Z"`)
    //   - `payload.timestamp_ms`: i64 epoch millis (auto-stamped by the
    //     filesystem-event sink and the broadcaster's
    //     `emit_scoped_or_unscoped`).
    // Earlier code did `value.get("timestamp_ms").or(value.get("timestamp")).as_i64()`
    // — top-level `timestamp_ms` doesn't exist and top-level `timestamp` is
    // a string (returns None from `as_i64()`), so every event ended up with
    // `ts=0` and the cross-scope-backfill global sort degenerated into
    // task-by-task ordering. Walk both real locations to recover the
    // canonical millis.
    let timestamp_ms = extract_event_timestamp_ms(&value);
    if let (Some(ts), Some(since)) = (timestamp_ms, since_ms) {
        if ts < since {
            return None;
        }
    }
    if let (Some(ts), Some(before)) = (timestamp_ms, before_ms) {
        if ts >= before {
            return None;
        }
    }
    if !filters.passes_event_type(event_type) {
        return None;
    }
    // Soft-pass: events whose `event_type` isn't in either taxonomy
    // table sneak past the taxonomy filter so a new emit site that
    // hasn't been registered yet doesn't silently drop out of category-
    // filtered views (mirrors `serialize_event_if_passes`).
    if let Some(taxonomy) = approximate_taxonomy_from_event_type(event_type) {
        if !filters.passes_taxonomy(&taxonomy) {
            return None;
        }
    }
    let agent_id = unwrap_agent_id(&value);
    if !filters.passes_agent(agent_id.as_deref()) {
        return None;
    }
    if !filters.passes_search(trimmed) {
        return None;
    }
    if let Some(filter_task) = task_filter {
        if let Some(found_task) = extract_task_id_from_event_value(&value) {
            if found_task != filter_task {
                return None;
            }
        }
    }
    let mut out = trimmed.to_string();
    out.push('\n');
    // Fall back to `i64::MIN` for truly timestamp-less events so the
    // global desc-sort puts them at the end of the page (treated as
    // "oldest known"), not interleaved with present-day events at
    // ts=0 (1970). After the timestamp-standardization in
    // `RuntimeTransportBroadcaster::emit_scoped_or_unscoped` and the
    // canonical-event sink's auto-stamp, this fallback should rarely
    // fire — it's a safety net for legacy rows already on disk.
    Some((timestamp_ms.unwrap_or(i64::MIN), out))
}

/// One candidate backfill row, ordered so that `BinaryHeap::pop` yields the
/// row to drop first: lowest timestamp, and among rows sharing a timestamp
/// the highest `seq` — i.e. the last row the previous stable
/// `sort_by(desc ts)` + `truncate(limit)` would have kept.
struct BackfillRow {
    ts: i64,
    seq: usize,
    line: String,
}

impl Ord for BackfillRow {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .ts
            .cmp(&self.ts)
            .then_with(|| self.seq.cmp(&other.seq))
    }
}

impl PartialOrd for BackfillRow {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for BackfillRow {
    fn eq(&self, other: &Self) -> bool {
        self.ts == other.ts && self.seq == other.seq
    }
}

impl Eq for BackfillRow {}

/// Bounded top-`limit` accumulator for cross-scope backfill rows.
///
/// The scan used to push every matching row into a `Vec` and truncate to
/// `limit` only once it had finished, so peak memory was set by
/// `CROSS_SCOPE_HARD_SCAN_CAP` (50k rows) instead of by `limit` (2k by
/// default) — the ">100MB on a populated scope" the old comment conceded.
/// Bounding as we go makes the peak `limit` rows, and the emitted page is
/// identical: a row outside the top `limit` at any point can never re-enter
/// it, because rows are only ever added.
struct BoundedTopRows {
    limit: usize,
    heap: std::collections::BinaryHeap<BackfillRow>,
}

impl BoundedTopRows {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            heap: std::collections::BinaryHeap::new(),
        }
    }

    fn push(&mut self, ts: i64, seq: usize, line: String) {
        if self.limit == 0 {
            return;
        }
        self.heap.push(BackfillRow { ts, seq, line });
        if self.heap.len() > self.limit {
            self.heap.pop();
        }
    }

    /// Rows in wire order: oldest → newest, because the UI prepends each
    /// arriving row. Descending on `BackfillRow`'s "worst first" ordering is
    /// exactly the old `sort desc by ts` + `reverse`, tie order included.
    fn into_wire_order(self) -> Vec<String> {
        let mut rows = self.heap.into_vec();
        rows.sort_by(|a, b| b.cmp(a));
        rows.into_iter().map(|row| row.line).collect()
    }
}

/// Sequence base for workspace-log rows. The per-scope transport log is read
/// FIRST but must merge as though it were appended LAST, matching the old
/// `matched.extend(workspace_log_matched)` — so its rows sort after every
/// per-execution row that shares their timestamp. Any base above the
/// per-execution scan's ceiling does that; half of `usize` needs no reasoning
/// about the cap.
const WORKSPACE_LOG_SEQ_BASE: usize = usize::MAX / 2;

/// Cross-scope backfill — enumerates every task → execution in the
/// active scope and walks their `events.jsonl` files. Pre-filters
/// executions whose `updated_at` is older than `effective_since` so we
/// don't open files that can't possibly contain matching rows.
///
/// Implementation notes:
///   * default `since_ms` is `now - 24h` per the design (`/events` open
///     questions Q8), passed through if the caller provided an explicit
///     `since` query param.
///   * a global desc-by-timestamp page can only be emitted once every
///     source has been read, so rows are held rather than streamed —
///     but only the top `limit` of them at a time
///     ([`BoundedTopRows`]), so the peak is set by `limit` and not by
///     `CROSS_SCOPE_HARD_SCAN_CAP`.
///   * a `__events_partial__` synthetic NDJSON row is appended when the
///     hard-scan cap is hit so the UI can render a "scan limit reached,
///     refine filters" hint.
#[allow(clippy::too_many_arguments)]
async fn cross_scope_backfill(
    artifact_service: Arc<ArtifactV2Service>,
    scope: ScopeRef,
    tx: mpsc::Sender<Result<web::Bytes, std::io::Error>>,
    filters: CompiledFilters,
    since_ms: Option<i64>,
    before_ms: Option<i64>,
    limit: usize,
    task_filter: Option<String>,
    workspace_event_log_registry: Option<
        magician::magician_v2::transport_log::WorkspaceEventLogRegistry,
    >,
) {
    use tokio::fs::File;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

    let now_ms = chrono::Utc::now().timestamp_millis();
    let effective_since = since_ms.unwrap_or(now_ms - DEFAULT_CROSS_SCOPE_WINDOW_MS);

    // Every source below feeds this one accumulator, which never holds more
    // than `limit` rows.
    let mut matched = BoundedTopRows::new(limit);

    // ─── Step 0 — read the per-scope transport event log if available.
    // This file holds every `RuntimeTransportEvent` that flowed through
    // the broadcaster, including the broadcaster-only variants
    // (FeedItemCreated, ChatMessageReceived, ExecutionPanelDelta, AGUI
    // envelope events) that aren't written to per-execution
    // events.jsonl by the canonical sink. Reading it makes refresh
    // stable for those event categories. Frontend dedup (v0.0.292)
    // catches any overlap with per-execution backfill below.
    //
    // Task filter: the workspace log is workspace-scoped, not
    // task-scoped, so it mixes events from every task in the workspace.
    // When `task_filter` is set, each row is filtered by its extractable
    // `task_id` and rows whose id doesn't match are dropped (rows that
    // carry none still pass) — mirrors how Step 3 only opens events.jsonl
    // for executions belonging to the filtered task.
    if let Some(registry) = workspace_event_log_registry.as_ref() {
        // The per-scope log is already retention-bounded by
        // `max(events_in_24h, EVENTS_RETENTION_MIN_COUNT)`. Re-applying
        // the cross-scope `since = now - 24h` floor here would
        // double-bound: a quiet workspace whose 24h-window count is
        // < 2000 keeps older entries on disk (retention floor), but
        // those would still be hidden from the read because of the
        // `since` filter. Pass through only the caller-supplied
        // `since_ms` (None when omitted by the client) so the read
        // returns whatever the retention layer is keeping.
        let path = registry.path_for(&scope.principal(), &scope.workspace());
        match File::open(&path).await {
            Ok(file) => {
                // Snapshot the length at open and read no further. The log is
                // appended to (and periodically compacted) underneath us, so
                // an unbounded read walks whatever a concurrent writer adds
                // while we scan and can trail off into a half-written line.
                // Same shape as `backfill_from_jsonl`'s `committed_len` bound
                // — this file carries no commit-authority sidecar, so its
                // length at open is the available snapshot boundary.
                let snapshot_len = match file.metadata().await {
                    Ok(metadata) => metadata.len(),
                    Err(err) => {
                        debug!(
                            path = %path.display(),
                            error = %err,
                            "[/events] cross-scope backfill: skipping unmeasurable workspace events.jsonl"
                        );
                        0
                    },
                };
                let mut lines = BufReader::new(file.take(snapshot_len)).lines();
                let mut seq = WORKSPACE_LOG_SEQ_BASE;
                let mut read = 0usize;
                while let Ok(Some(raw_line)) = lines.next_line().await {
                    read += 1;
                    // Sampled rather than per-line: a tightly-filtered scan
                    // sends nothing, so `tx.send` never reports the
                    // disconnect, but checking every line costs an atomic
                    // load per line for no extra promptness.
                    if read.is_multiple_of(256) && tx.is_closed() {
                        return;
                    }
                    // The task filter is applied inside
                    // `filter_canonical_event_line`, on the value it already
                    // parsed — the second `serde_json::from_str` over the
                    // same line that used to live here doubled the parse
                    // cost of every row a `?task=…` query matched.
                    if let Some((ts, line)) = filter_canonical_event_line(
                        &raw_line,
                        &filters,
                        since_ms,
                        before_ms,
                        task_filter.as_deref(),
                    ) {
                        matched.push(ts, seq, line.trim_end_matches('\n').to_string());
                        seq += 1;
                    }
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {},
            Err(err) => debug!(
                path = %path.display(),
                error = %err,
                "[/events] cross-scope backfill: skipping unreadable workspace events.jsonl"
            ),
        }
    }

    // ─── Step 1 — enumerate tasks ─────────────────────────────────
    // A `?task=…` query already names the only task it can read, so it goes
    // straight to that task's executions. `list_tasks` is not a cheap id
    // listing — it reads every task's manifest + state + refs and walks the
    // proposal store — and this leg used all of it to keep one `task.id`.
    let task_ids: Vec<String> = match task_filter.as_deref() {
        Some(task_id) => vec![task_id.to_string()],
        None => match artifact_service.list_tasks(&scope).await {
            Ok(tasks) => tasks.into_iter().map(|task| task.id).collect(),
            Err(error) => {
                warn!(
                    principal = %scope.principal(),
                    workspace = %scope.workspace(),
                    error = %error,
                    "[/events] cross-scope backfill: failed to list tasks"
                );
                return;
            },
        },
    };

    // ─── Step 2 — collect candidate (task, execution) pairs that
    // could contain events newer than `effective_since`. The
    // `ExecutionIndexEntry::updated_at` field is RFC3339; we compare
    // its parsed millis. Entries that fail to parse are kept (better
    // to be lax than to silently drop).
    let mut candidates: Vec<(String, String)> = Vec::new();
    for task_id in task_ids {
        if tx.is_closed() {
            return;
        }
        let executions = match artifact_service.list_executions(&scope, &task_id).await {
            Ok(e) => e,
            Err(error) => {
                warn!(
                    task_id = %task_id,
                    error = %error,
                    "[/events] cross-scope backfill: failed to list executions"
                );
                continue;
            },
        };
        for execution in executions {
            if let Some(ts) = parse_rfc3339_ms(&execution.updated_at) {
                if ts < effective_since {
                    continue;
                }
            }
            candidates.push((task_id.clone(), execution.execution_id));
        }
    }

    // ─── Step 3 — read each candidate's events.jsonl and apply
    // filters. Bounded by CROSS_SCOPE_HARD_SCAN_CAP across the whole
    // scan to keep memory bounded.
    let mut scanned: usize = 0;
    let mut seq: usize = 0;
    let mut hit_scan_cap = false;
    'outer: for (task_id, execution_id) in candidates {
        let path = artifact_service.workspace().execution_events_path(
            &scope.principal(),
            &scope.workspace(),
            &task_id,
            &execution_id,
        );
        let commit_path = artifact_service.workspace().execution_events_commit_path(
            &scope.principal(),
            &scope.workspace(),
            &task_id,
            &execution_id,
        );
        let committed_len = match artifact_service
            .workspace()
            .committed_jsonl_len_path(&path, &commit_path)
            .await
        {
            Ok(committed_len) => committed_len,
            Err(error) => {
                debug!(
                    path = %path.display(),
                    error = %error,
                    "[/events] cross-scope backfill: skipping invalid events.jsonl commit authority"
                );
                continue;
            },
        };
        let file = match File::open(&path).await {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                debug!(
                    path = %path.display(),
                    error = %err,
                    "[/events] cross-scope backfill: skipping unreadable events.jsonl"
                );
                continue;
            },
        };
        // If the client disconnected (operator reloaded /events,
        // navigated away, etc.) the mpsc receiver has been dropped and
        // every buffered line we'd build below would be discarded
        // anyway. Bail before opening another execution's events.jsonl
        // so we don't waste up to CROSS_SCOPE_HARD_SCAN_CAP × per-line
        // work. Cheap check; no cost on the happy path.
        if tx.is_closed() {
            return;
        }
        let mut lines = BufReader::new(file.take(committed_len)).lines();
        while let Ok(Some(raw_line)) = lines.next_line().await {
            scanned += 1;
            if scanned > CROSS_SCOPE_HARD_SCAN_CAP {
                hit_scan_cap = true;
                break 'outer;
            }
            // Same disconnect bail inside the line-loop so a single
            // huge events.jsonl doesn't keep scanning all the way to
            // the cap when the consumer is already gone. Sampled every
            // 256 lines to keep the cost negligible.
            if scanned.is_multiple_of(256) && tx.is_closed() {
                return;
            }
            // Use the same predicate as per-execution backfill so wire
            // output stays consistent across both paths. The timestamp
            // returned alongside the serialized line is the same one
            // already extracted during the `since` / `before` check —
            // no re-parse needed. No task filter here: this leg's lines
            // were already selected by the execution's path.
            let Some((ts, serialized)) = filter_canonical_event_line(
                &raw_line,
                &filters,
                Some(effective_since),
                before_ms,
                None,
            ) else {
                continue;
            };
            // Strip the trailing newline that `filter_canonical_event_line`
            // already appended; we re-add it at emit time.
            matched.push(ts, seq, serialized.trim_end_matches('\n').to_string());
            seq += 1;
        }
    }

    // ─── Step 4 — order the retained rows oldest → newest. The UI
    //              prepends each arriving row to its display list
    //              (`rows = [row, ...rows]`), so emitting newest-first
    //              would invert the backfill block on screen (oldest at
    //              top of backfill section, newest at bottom). Streaming
    //              oldest-first means the UI's prepend produces the
    //              expected newest-first ordering. Selection to `limit`
    //              already happened during the scan.
    let page = matched.into_wire_order();

    // ─── Step 5 — emit rows + optional "partial" sentinel.
    for line in page {
        let mut out = line;
        out.push('\n');
        if tx.send(Ok(web::Bytes::from(out))).await.is_err() {
            return;
        }
    }

    if hit_scan_cap {
        let sentinel = serde_json::json!({
            "event_type": "__events_partial__",
            "timestamp_ms": now_ms,
            "scanned": scanned,
            "scan_cap": CROSS_SCOPE_HARD_SCAN_CAP,
            "message": "Cross-scope backfill hit the scan cap; refine filters or shorten the time window for older results.",
        });
        let mut out = serde_json::to_string(&sentinel).unwrap_or_default();
        out.push('\n');
        let _ = tx.send(Ok(web::Bytes::from(out))).await;
    }
}

/// Extract the canonical-event millisecond timestamp from a parsed
/// `events.jsonl` row. Walks the locations the filesystem-event sink
/// actually populates (and the live-broadcaster mirror), in order:
///   1. Top-level `timestamp_ms` — for events the live broadcaster's
///      `emit_scoped_or_unscoped` wrote directly.
///   2. Top-level `timestamp` (RFC3339 string) — the canonical
///      `CanonicalEvent.timestamp` written by `FilesystemRuntimeEventSink`.
///   3. `payload.timestamp_ms` — auto-injected by the canonical-event
///      sink (`service.rs:8878`).
///   4. `payload.started_at` / `payload.finished_at` / `payload.ended_at`
///      — semantic time fields some emitters carry.
/// Returns None when none of the above are present or parseable.
fn extract_event_timestamp_ms(value: &serde_json::Value) -> Option<i64> {
    // Top-level `timestamp_ms` (i64) — disk-format wrapper.
    if let Some(v) = value.get("timestamp_ms").and_then(|v| v.as_i64()) {
        return Some(v);
    }
    // Top-level `timestamp` — could be either RFC3339 string (disk
    // wrapper) or i64 ms (canonical wire). Try both.
    if let Some(t) = value.get("timestamp") {
        if let Some(ms) = t.as_i64() {
            return Some(ms);
        }
        if let Some(s) = t.as_str() {
            if let Some(ms) = parse_rfc3339_ms(s) {
                return Some(ms);
            }
        }
    }
    // `payload.timestamp_ms` / `payload.timestamp` — disk wrapper.
    if let Some(p) = value.get("payload") {
        for key in ["timestamp_ms", "timestamp"] {
            if let Some(v) = p.get(key) {
                if let Some(ms) = v.as_i64() {
                    return Some(ms);
                }
                if let Some(s) = v.as_str() {
                    if let Some(ms) = parse_rfc3339_ms(s) {
                        return Some(ms);
                    }
                }
            }
        }
        for key in ["started_at", "finished_at", "ended_at"] {
            if let Some(v) = p.get(key).and_then(|v| v.as_i64()) {
                return Some(v);
            }
        }
    }
    // `data.timestamp` / `data.timestamp_ms` — `#[serde(tag = "event_type",
    // content = "data")]` wire format. Every RuntimeTransportEvent
    // variant carries `timestamp: i64` at the variant level which lands
    // here on serialize.
    if let Some(d) = value.get("data") {
        for key in ["timestamp", "timestamp_ms"] {
            if let Some(v) = d.get(key) {
                if let Some(ms) = v.as_i64() {
                    return Some(ms);
                }
                if let Some(s) = v.as_str() {
                    if let Some(ms) = parse_rfc3339_ms(s) {
                        return Some(ms);
                    }
                }
            }
        }
        // `AgentEvent` envelope nests its time fields under
        // `data.event.*` (the inner `AgentEventEnvelope`). Without
        // walking here the extractor returns `None` for every
        // `tool.call.*` / `reasoning.*` / `llm.*` row on disk, which
        // (a) bypasses the `since`/`before` backfill filters at lines
        // ~508-517 (both gates require a non-None timestamp), and
        // (b) makes the global desc sort fall back to `i64::MIN`, pinning
        // every envelope row to the bottom. Walk the inner envelope so
        // those rows participate correctly in filtering and ordering.
        if let Some(event) = d.get("event") {
            for key in ["timestamp", "timestamp_ms"] {
                if let Some(v) = event.get(key) {
                    if let Some(ms) = v.as_i64() {
                        return Some(ms);
                    }
                    if let Some(s) = v.as_str() {
                        if let Some(ms) = parse_rfc3339_ms(s) {
                            return Some(ms);
                        }
                    }
                }
            }
            if let Some(payload) = event.get("payload") {
                for key in ["timestamp_ms", "timestamp"] {
                    if let Some(v) = payload.get(key) {
                        if let Some(ms) = v.as_i64() {
                            return Some(ms);
                        }
                        if let Some(s) = v.as_str() {
                            if let Some(ms) = parse_rfc3339_ms(s) {
                                return Some(ms);
                            }
                        }
                    }
                }
                for key in ["started_at", "finished_at", "ended_at"] {
                    if let Some(v) = payload.get(key).and_then(|v| v.as_i64()) {
                        return Some(v);
                    }
                }
            }
        }
    }
    None
}

/// Pull a `task_id` out of a parsed canonical-event row, walking the
/// locations various variants stamp it at:
///   - top-level `task_id` (canonical-event sink format)
///   - `data.task_id` (`#[serde(tag, content)]` transport-event wire)
///   - `data.item.task_id` (FeedItem-shaped variants)
///   - `payload.task_id` (disk-wrapper format)
/// Returns `None` for variants that don't ship a task id at all
/// (heartbeats, chat-only events, etc.) so the caller can decide
/// whether "no task id" should be treated as a drop or a pass.
fn extract_task_id_from_event_value(value: &serde_json::Value) -> Option<String> {
    if let Some(s) = value.get("task_id").and_then(|v| v.as_str()) {
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    if let Some(d) = value.get("data") {
        if let Some(s) = d.get("task_id").and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
        if let Some(item) = d.get("item") {
            if let Some(s) = item.get("task_id").and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
        }
        // AgentEvent envelope: the chat path + inner-loop mirror wrap
        // every `emit_named` callsite as `{event_type:"AgentEvent",
        // data:{event:{agent_id, event_type, payload:{...}}}}`. The
        // inner-loop runner stamps `task_id` into the mirrored
        // payload (`runner.rs::append_primitive_event_best_effort`),
        // so it arrives over the wire at `data.event.payload.task_id`.
        // Without this layer in the ladder the `?task=…` filter
        // treated every agentic envelope row as "no task id" and
        // those rows passed the filter regardless of task scope,
        // surfacing as "/events?task=X shows agentic activity from
        // unrelated tasks" to the operator.
        if let Some(event) = d.get("event") {
            if let Some(s) = event.get("task_id").and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
            if let Some(p) = event.get("payload") {
                if let Some(s) = p.get("task_id").and_then(|v| v.as_str()) {
                    if !s.is_empty() {
                        return Some(s.to_string());
                    }
                }
            }
        }
    }
    if let Some(p) = value.get("payload") {
        if let Some(s) = p.get("task_id").and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// Parses an RFC3339 timestamp into Unix milliseconds. Returns `None`
/// for unparseable inputs (treat as "unknown" — caller decides whether
/// to keep or drop the entry; current callers keep them).
fn parse_rfc3339_ms(input: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(input)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

/// Best-effort mapping from a canonical-event `event_type` string back to
/// its operator taxonomy. Used during backfill where we don't have the
/// strongly-typed `RuntimeTransportEvent` instance handy.
///
/// Two lookup paths:
///   1. The `RuntimeTransportEvent` enum-variant table (`MessageProcessingStarted`,
///      `ExecutionStarted`, …) — covers events that were *actual* enum variants.
///   2. The AGUI envelope event_type table (`tool.call.started`,
///      `reasoning.content`, `plan.snapshot`, `agent.cycle.*`, …) — covers
///      every event emitted via `RuntimeTransportEvent::AgentEvent` envelope,
///      which all serialize with outer `event_type: "AgentEvent"` and the
///      real type buried under `data.event.event_type`. Without this table
///      every envelope event would taxonomy-lookup as the catch-all `Agent
///      / Info / user_relevant=false` (the `AgentEvent =>` row in the
///      `taxonomies!` macro), losing the granular category/severity/
///      user-relevant signal those events actually carry.
fn approximate_taxonomy_from_event_type(event_type: &str) -> Option<EventTaxonomy> {
    // Delegates to the single shared lookup in `realtime_events` so the
    // events API filter and the per-surface routing predicates in
    // `progress_channels::surface_routing` agree on classification.
    // Avoids two independent `HashMap` instances drifting in row counts
    // / precedence as events are added.
    magician::magician_v2::realtime_events::lookup_agent_event_taxonomy(event_type)
}

/// Serializes a live `RuntimeTransportEvent` to one NDJSON line if it
/// passes the compiled filters. Returns `None` when the event should be
/// dropped.
fn serialize_event_if_passes(
    event: &RuntimeTransportEvent,
    filters: &CompiledFilters,
) -> Option<String> {
    if magician::magician_v2::realtime_events::is_app_owner_notification_transport_event(event) {
        // UserRequest/Attention is the sole notification content surface.
        // Refuse both live requests and their bodyless special-source
        // resolutions from the generic `/events` tail, matching the durable
        // workspace-log writer and backfill filters.
        return None;
    }
    // Serialize once and read fields off the resulting JSON. For
    // `RuntimeTransportEvent::AgentEvent`, the variant taxonomy from the
    // `taxonomies!` macro returns the catch-all `Agent / Info / false`
    // — we override with a granular AGUI-event lookup keyed on the
    // *inner* envelope event_type so the operator sees the right
    // category/severity/user_relevant on tool.call.* / reasoning.* /
    // plan.* / agent.* events.
    let value = match serde_json::to_value(event) {
        Ok(v) => v,
        Err(_) => return None,
    };
    let unwrapped_event_type = unwrap_event_type(&value);
    let unwrapped_agent_id = unwrap_agent_id(&value);
    // Taxonomy lookup ladder:
    //   1. AGUI envelope event_type → granular table (tool.call.*, etc.)
    //   2. Typed enum variant → `taxonomies!` macro
    //   3. Unknown event_type → soft-pass the taxonomy filter so a new
    //      emit site that hasn't been added to `GAUI_EVENT_TAXONOMY`
    //      yet doesn't get *silently dropped* under a category filter
    //      (e.g. `category=tool` would exclude a new `tool.something`
    //      via the catch-all `Agent / Info / false` of `event.taxonomy()`).
    //      The event still appears in the unfiltered live tail; operators
    //      can refine their filters.
    let taxonomy_lookup = approximate_taxonomy_from_event_type(&unwrapped_event_type);
    let recognized = taxonomy_lookup.is_some();
    let taxonomy = taxonomy_lookup.unwrap_or_else(|| event.taxonomy());
    if recognized && !filters.passes_taxonomy(&taxonomy) {
        return None;
    }
    if !filters.passes_event_type(&unwrapped_event_type) {
        return None;
    }
    if !filters.passes_agent(unwrapped_agent_id.as_deref()) {
        return None;
    }
    // Serialize the *event*, not the `Value` that was built for filtering.
    //
    // Round-tripping through `serde_json::Value` reorders keys: serde_json
    // is built without `preserve_order`, so `Value::Object` is a `BTreeMap`
    // and re-serializing emits keys alphabetically (`data` before
    // `event_type`). The backfill leg replays the raw persisted line, which
    // `transport_log` wrote straight off the enum in declaration order
    // (`event_type` before `data`). The two forms describe the same event
    // and never compare equal.
    //
    // Backfill and the live tail overlap by design, and a consumer is meant
    // to drop the duplicate by comparing the serialized line — that is the
    // documented contract the runtime activity view is built on. With the
    // key order differing, that comparison matched nothing: every event in
    // the overlap window was processed twice, which for `ActivityProgress`
    // (no id of its own) rendered every log line in the window twice.
    // Serializing the event directly makes the two legs byte-identical.
    let serialized = serde_json::to_string(event).ok()?;
    if !filters.passes_search(&serialized) {
        return None;
    }
    let mut out = serialized;
    out.push('\n');
    Some(out)
}

/// Read the canonical `event_type` from a serialized
/// `RuntimeTransportEvent`. For `AgentEvent` envelope-wrapped events the
/// outer tag is always `"AgentEvent"`; the meaningful event_type lives at
/// `data.event.event_type`. Other variants carry the canonical type at
/// the outer level (matches the serde tag).
fn unwrap_event_type(value: &serde_json::Value) -> String {
    let outer = value
        .get("event_type")
        .and_then(|t| t.as_str())
        .unwrap_or_default();
    if outer == "AgentEvent" {
        if let Some(inner) = value
            .pointer("/data/event/event_type")
            .and_then(|t| t.as_str())
        {
            return inner.to_string();
        }
    }
    outer.to_string()
}

/// Read the canonical `agent_id` from a serialized
/// `RuntimeTransportEvent`. For `AgentEvent` envelopes the
/// `agent_id` lives under `data.event.agent_id`; for other variants it's
/// at the outer level (when present at all).
fn unwrap_agent_id(value: &serde_json::Value) -> Option<String> {
    if let Some(inner) = value
        .pointer("/data/event/agent_id")
        .and_then(|a| a.as_str())
    {
        return Some(inner.to_string());
    }
    value
        .get("agent_id")
        .and_then(|a| a.as_str())
        .map(ToOwned::to_owned)
}

// Re-export so downstream `bin/magician.rs` can build the shared API.
pub use list_events_v3_handler as events_handler;

#[allow(dead_code)] // Suppress unused warnings during early integration.
fn _scope_unused_warning_suppressor(_: &ScopeRef) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn per_execution_backfill_stops_at_the_durable_commit_authority() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let workspace =
            magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(temp.path());
        let path = temp.path().join("events.jsonl");
        let commit_path = temp.path().join("events.jsonl.commit");
        let committed = serde_json::json!({
            "event_type": "ExecutionStarted",
            "timestamp_ms": 1,
            "payload": {"marker": "committed"}
        })
        .to_string()
            + "\n";
        let uncertain = serde_json::json!({
            "event_type": "ExecutionCompleted",
            "timestamp_ms": 2,
            "payload": {"marker": "uncertain"}
        })
        .to_string()
            + "\n";
        std::fs::write(&path, format!("{committed}{uncertain}")).expect("physical event log");
        std::fs::write(&commit_path, committed.len().to_string()).expect("commit authority");
        let (tx, mut rx) = mpsc::channel(4);

        backfill_from_jsonl(
            workspace,
            path,
            commit_path,
            tx,
            CompiledFilters::default(),
            None,
            None,
            4,
        )
        .await;

        let row = rx.recv().await.expect("one committed row").expect("bytes");
        let row = std::str::from_utf8(&row).expect("utf8 event row");
        assert!(row.contains("committed"));
        assert!(!row.contains("uncertain"));
        assert!(rx.recv().await.is_none());
    }

    #[test]
    fn parse_categories_handles_csv_and_unknowns() {
        let parsed = parse_categories("execution , agentic, bogus, hitl");
        assert_eq!(
            parsed,
            vec![
                EventCategory::Execution,
                EventCategory::Agentic,
                EventCategory::Hitl
            ]
        );
    }

    /// Every `EventCategory` must be reachable through `?category=`.
    ///
    /// `parse_categories` matches on strings, so rustc cannot tell that it
    /// has fallen behind the enum: a new variant compiles fine and just
    /// never matches, which reads to a caller as "there are no events in
    /// that category" rather than "that filter isn't wired up".
    /// `EventCategory::Media` was unreachable that way.
    ///
    /// The `match` below is the enforcement: it is exhaustive over the
    /// enum, so adding a variant breaks *compilation* of this test, and
    /// the assertion then fails until the ladder in `parse_categories`
    /// learns the same token. The tokens are the serde `snake_case` wire
    /// names, which is what the TS `EVENT_CATEGORIES` mirror ships.
    #[test]
    fn every_event_category_parses_from_its_wire_token() {
        fn wire_token(category: EventCategory) -> &'static str {
            match category {
                EventCategory::Pipeline => "pipeline",
                EventCategory::Plan => "plan",
                EventCategory::Tool => "tool",
                EventCategory::Slot => "slot",
                EventCategory::Clarification => "clarification",
                EventCategory::Execution => "execution",
                EventCategory::Llm => "llm",
                EventCategory::Agentic => "agentic",
                EventCategory::Hitl => "hitl",
                EventCategory::Agent => "agent",
                EventCategory::Task => "task",
                EventCategory::Feed => "feed",
                EventCategory::Observability => "observability",
                EventCategory::Media => "media",
                EventCategory::Activity => "activity",
            }
        }

        for category in [
            EventCategory::Pipeline,
            EventCategory::Plan,
            EventCategory::Tool,
            EventCategory::Slot,
            EventCategory::Clarification,
            EventCategory::Execution,
            EventCategory::Llm,
            EventCategory::Agentic,
            EventCategory::Hitl,
            EventCategory::Agent,
            EventCategory::Task,
            EventCategory::Feed,
            EventCategory::Observability,
            EventCategory::Media,
            EventCategory::Activity,
        ] {
            let token = wire_token(category);
            assert_eq!(
                parse_categories(token),
                vec![category],
                "`?category={token}` does not resolve to {category:?} — add the \
                 arm to `parse_categories`"
            );
        }
    }

    #[test]
    fn parse_severities_handles_csv_and_unknowns() {
        let parsed = parse_severities("info,error,foo");
        assert_eq!(parsed, vec![EventSeverity::Info, EventSeverity::Error]);
    }

    #[test]
    fn compiled_filters_default_passes_everything() {
        let f = CompiledFilters::default();
        let taxonomy = EventTaxonomy::new(EventCategory::Execution, EventSeverity::Info, true);
        assert!(f.passes_taxonomy(&taxonomy));
        assert!(f.passes_event_type("ExecutionStarted"));
        assert!(f.passes_search("anything"));
        assert!(f.passes_agent(None));
    }

    #[test]
    fn compiled_filters_category_match() {
        let f = CompiledFilters {
            categories: Some(vec![EventCategory::Execution, EventCategory::Hitl]),
            ..Default::default()
        };
        assert!(f.passes_taxonomy(&EventTaxonomy::new(
            EventCategory::Execution,
            EventSeverity::Info,
            true
        )));
        assert!(!f.passes_taxonomy(&EventTaxonomy::new(
            EventCategory::Llm,
            EventSeverity::Info,
            true
        )));
    }

    #[test]
    fn compiled_filters_user_relevant_only_keeps_true() {
        let f = CompiledFilters {
            user_relevant: Some(true),
            ..Default::default()
        };
        assert!(f.passes_taxonomy(&EventTaxonomy::new(
            EventCategory::Execution,
            EventSeverity::Info,
            true
        )));
        assert!(!f.passes_taxonomy(&EventTaxonomy::new(
            EventCategory::Execution,
            EventSeverity::Info,
            false
        )));
    }

    /// The bounded accumulator must return the SAME page the previous
    /// "collect everything, `sort_by(desc ts)`, `truncate(limit)`, `reverse`"
    /// produced — including tie order, which that stable sort preserved as
    /// insertion order.
    #[test]
    fn bounded_top_rows_reproduces_the_unbounded_sort_and_truncate() {
        const LIMIT: usize = 4;
        // Deliberately unsorted, with three rows sharing ts=20 and two
        // sharing ts=10 so the tie path is exercised in both the kept and
        // the dropped region.
        let feed: Vec<(i64, &str)> = vec![
            (20, "a"),
            (10, "b"),
            (30, "c"),
            (20, "d"),
            (10, "e"),
            (20, "f"),
            (40, "g"),
        ];

        let mut bounded = BoundedTopRows::new(LIMIT);
        for (seq, (ts, line)) in feed.iter().enumerate() {
            bounded.push(*ts, seq, (*line).to_string());
        }

        // The behaviour being preserved, spelled out longhand.
        let mut reference: Vec<(i64, String)> = feed
            .iter()
            .map(|(ts, line)| (*ts, (*line).to_string()))
            .collect();
        reference.sort_by(|a, b| b.0.cmp(&a.0));
        reference.truncate(LIMIT);
        reference.reverse();
        let reference: Vec<String> = reference.into_iter().map(|(_, line)| line).collect();

        assert_eq!(bounded.into_wire_order(), reference);
        // …and concretely: newest four are g(40), c(30), then a and d — the
        // first two of the ts=20 block — emitted oldest-first.
        assert_eq!(reference, vec!["d", "a", "c", "g"]);
    }

    /// Workspace-log rows are read first but must merge as though appended
    /// last, which is what the old `matched.extend(workspace_log_matched)`
    /// did. At an equal timestamp the per-execution row therefore still comes
    /// first in the desc-ordered page.
    #[test]
    fn workspace_log_rows_merge_after_per_execution_rows_at_the_same_timestamp() {
        let mut bounded = BoundedTopRows::new(8);
        // Step 0 pushes first, from the high base.
        bounded.push(100, WORKSPACE_LOG_SEQ_BASE, "workspace".to_string());
        // Step 3 pushes second, from zero.
        bounded.push(100, 0, "execution".to_string());
        // Desc page is [execution, workspace]; wire order is that reversed.
        assert_eq!(bounded.into_wire_order(), vec!["workspace", "execution"]);
    }

    #[test]
    fn bounded_top_rows_holds_only_limit_rows_while_scanning() {
        let mut bounded = BoundedTopRows::new(3);
        for seq in 0..10_000 {
            bounded.push(seq as i64, seq, format!("row-{seq}"));
        }
        assert_eq!(bounded.heap.len(), 3);
        assert_eq!(
            bounded.into_wire_order(),
            vec!["row-9997", "row-9998", "row-9999"]
        );
    }

    #[test]
    fn task_filter_drops_only_rows_that_positively_name_another_task() {
        let filters = CompiledFilters::default();
        let with_task = serde_json::json!({
            "event_type": "ExecutionStarted",
            "timestamp_ms": 5,
            "task_id": "task_wanted",
        })
        .to_string();
        let other_task = serde_json::json!({
            "event_type": "ExecutionStarted",
            "timestamp_ms": 5,
            "task_id": "task_other",
        })
        .to_string();
        // No extractable task id — chat rows and most agentic envelopes look
        // like this, and they must keep passing.
        let no_task = serde_json::json!({
            "event_type": "ExecutionStarted",
            "timestamp_ms": 5,
        })
        .to_string();

        for (line, expected) in [(&with_task, true), (&other_task, false), (&no_task, true)] {
            let kept = filter_canonical_event_line(line, &filters, None, None, Some("task_wanted"))
                .is_some();
            assert_eq!(kept, expected, "line: {line}");
            // Without a filter every row passes, as on the per-execution leg.
            assert!(filter_canonical_event_line(line, &filters, None, None, None).is_some());
        }
    }

    /// The per-execution backfill's byte bound and early exit, asserted
    /// together: the scan reads only committed bytes and stops the moment it
    /// has `limit` rows rather than draining the file.
    #[tokio::test]
    async fn per_execution_backfill_stops_emitting_at_limit() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let workspace =
            magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(temp.path());
        let path = temp.path().join("events.jsonl");
        let commit_path = temp.path().join("events.jsonl.commit");
        let body: String = (0..50)
            .map(|index| {
                serde_json::json!({
                    "event_type": "ExecutionStarted",
                    "timestamp_ms": index,
                    "payload": {"marker": format!("row-{index}")}
                })
                .to_string()
                    + "\n"
            })
            .collect();
        std::fs::write(&path, &body).expect("physical event log");
        std::fs::write(&commit_path, body.len().to_string()).expect("commit authority");
        let (tx, mut rx) = mpsc::channel(64);

        backfill_from_jsonl(
            workspace,
            path,
            commit_path,
            tx,
            CompiledFilters::default(),
            None,
            None,
            3,
        )
        .await;

        let mut rows = Vec::new();
        while let Some(row) = rx.recv().await {
            rows.push(String::from_utf8(row.expect("bytes").to_vec()).expect("utf8"));
        }
        assert_eq!(rows.len(), 3);
        assert!(rows[0].contains("row-0"));
        assert!(rows[2].contains("row-2"));
    }
}

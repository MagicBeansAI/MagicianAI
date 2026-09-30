//! Live Thinking Map — REST surface.
//!
//! Makes the [`ThinkingMapStore`] reachable over HTTP. The routes are
//! unconditionally mounted (the feature is GA): there is no enable check, so
//! every handler runs directly.
//!
//! ## Owner-authority surface
//! This is the OWNER surface: the operations endpoint always stamps
//! `actor = OperationActor::Owner { principal }` from the resolved request
//! scope and forces `map_id` to the path parameter. A client CANNOT spoof the
//! actor or target a different map by embedding those in the body.
//!
//! ## Scope
//! Every handler resolves an explicit `(principal, workspace)` scope via
//! [`resolve_required_scope`] (internal scope engraved from the bearer or
//! principal/workspace body fields). A missing scope is `400 missing_scope`.

use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse, Result};
use serde::Deserialize;
use serde_json::json;

use crate::scope::resolve_required_scope;
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::learning::{
    CreateLearningCandidateRequest, LearningCandidateState, LearningCandidateType,
    LearningEvidenceRef, LearningRiskLevel, LearningScope, LearningStore,
};
use magician::magician_v2::query_analysis::operation_llm_router::global_operation_router;
use magician::magician_v2::realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent};
use magician_surfaces::thinking_map::{
    build_tutor_map_context, consolidate, interpret, render_markdown, tutor_map_context_registry,
    ApplyOutcome, AssertionOrigin, EpistemicState, ExportOptions, InterpretIntent,
    InterpretProgressSink, InterpretStage, MapLifecycle, MapOperation, MapOperationEnvelope,
    MapSummary, OperationActor, PromotedRef, PromotionKind, RouterInterpreterLlm, ThinkingMap,
    ThinkingMapError, ThinkingMapSessionCoordinator, ThinkingMapSource, ThinkingMapStore,
    ThinkingMapStoreError, TutorMapContextBinding, Utterance,
    DEFAULT_TUTOR_MAP_CONTEXT_BUDGET_CHARS, THINKING_MAP_SCHEMA_VERSION, TUTOR_MAP_CONTEXT_TTL_MS,
};

/// Shared state for the Thinking Maps REST API. Holds a clone-cheap workspace
/// handle; a fresh [`ThinkingMapStore`] is constructed per request.
pub struct ThinkingMapsApi {
    workspace: ArtifactV2Workspace,
}

impl ThinkingMapsApi {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        Self { workspace }
    }

    fn store(&self) -> ThinkingMapStore {
        ThinkingMapStore::new(self.workspace.clone())
    }

    fn workspace(&self) -> &ArtifactV2Workspace {
        &self.workspace
    }
}

/// Current UTC time as an RFC3339 string (the store owns no clock; callers
/// inject timestamps).
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Emit a `ThinkingMapUpdated` change notice onto the transport bus after a
/// successfully applied envelope (owner ops / patch / interpret / consolidate /
/// proposal decision). Best-effort by design: the broadcaster is optional
/// app_data (absent in minimal test apps), and clients treat the push as a
/// poll accelerator — no notice just means the next poll picks the change up.
fn emit_map_updated(
    broadcaster: Option<&web::Data<Arc<RuntimeTransportBroadcaster>>>,
    map_id: &str,
    principal: &str,
    workspace: &str,
    revision: u64,
) {
    if let Some(broadcaster) = broadcaster {
        broadcaster.emit_transport_only(RuntimeTransportEvent::ThinkingMapUpdated {
            map_id: map_id.to_string(),
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            revision,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }
}

/// Narrates one owner-triggered interpretation onto the transport bus, as
/// `ThinkingMapInterpretProgress` events beside the `ThinkingMapUpdated`
/// notice. One instance per interpretation: the scope and `utterance_id` are
/// fixed at construction, so every stage of a run tells the same story about
/// which run it is.
///
/// Best-effort exactly like [`emit_map_updated`]: the broadcaster is optional
/// app_data (absent in minimal test apps), and a missed stage costs a client
/// nothing — its strip keeps the line it had.
struct TransportInterpretProgress {
    broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    map_id: String,
    principal: String,
    workspace: String,
    utterance_id: String,
}

impl TransportInterpretProgress {
    fn new(
        broadcaster: Option<&web::Data<Arc<RuntimeTransportBroadcaster>>>,
        map_id: &str,
        principal: &str,
        workspace: &str,
        utterance_id: &str,
    ) -> Self {
        Self {
            broadcaster: broadcaster.map(|b| b.get_ref().clone()),
            map_id: map_id.to_string(),
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            utterance_id: utterance_id.to_string(),
        }
    }

    /// The terminal stage, emitted by the handler once the interpretation has
    /// settled — applied, zero-move, or failed. Separate from the trait method
    /// so the settle sites read as what they are.
    fn idle(&self) {
        self.stage(InterpretStage::Idle, None);
    }
}

impl InterpretProgressSink for TransportInterpretProgress {
    fn stage(&self, stage: InterpretStage, node_count: Option<usize>) {
        if let Some(broadcaster) = &self.broadcaster {
            broadcaster.emit_transport_only(RuntimeTransportEvent::ThinkingMapInterpretProgress {
                map_id: self.map_id.clone(),
                principal: self.principal.clone(),
                workspace: self.workspace.clone(),
                utterance_id: self.utterance_id.clone(),
                stage: stage.wire_name().to_string(),
                detail: None,
                node_count,
                timestamp: chrono::Utc::now().timestamp_millis(),
            });
        }
    }
}

/// Map a [`ThinkingMapStoreError`] to an HTTP response. Never leaks internal
/// detail beyond `to_string()`. `pub(crate)` so the Today source-action
/// execution path (feed API) maps store failures identically.
pub(crate) fn thinking_map_store_error(error: ThinkingMapStoreError) -> HttpResponse {
    match error {
        ThinkingMapStoreError::NotFound(id) => {
            HttpResponse::NotFound().json(json!({ "error": "not_found", "map_id": id }))
        },
        ThinkingMapStoreError::AlreadyExists(id) => {
            HttpResponse::Conflict().json(json!({ "error": "already_exists", "map_id": id }))
        },
        ThinkingMapStoreError::PurgeRequiresDeleted(id) => HttpResponse::Conflict().json(json!({
            "error": "purge_requires_deleted",
            "map_id": id,
        })),
        // A stale base_revision surfaces as a distinct 409 so offline clients can
        // rebase + retry rather than treating it as a permanent validation failure.
        ThinkingMapStoreError::Validation(ThinkingMapError::RevisionConflict {
            expected,
            actual,
        }) => HttpResponse::Conflict().json(json!({
            "error": "revision_conflict",
            "expected": expected,
            "actual": actual,
        })),
        ThinkingMapStoreError::Validation(error) => HttpResponse::BadRequest().json(json!({
            "error": "validation_failed",
            "details": error.to_string(),
        })),
        ThinkingMapStoreError::InvalidId(detail) => HttpResponse::BadRequest().json(json!({
            "error": "invalid_id",
            "details": detail,
        })),
        ThinkingMapStoreError::Corrupt(detail) => HttpResponse::InternalServerError().json(json!({
            "error": "corrupt",
            "details": detail,
        })),
        ThinkingMapStoreError::Io(error) => HttpResponse::InternalServerError().json(json!({
            "error": "io_error",
            "details": error.to_string(),
        })),
    }
}

// ── Request bodies ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateMapRequest {
    pub title: String,
    #[serde(default)]
    pub source: Option<ThinkingMapSource>,
    #[serde(default)]
    pub map_id: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Client operations envelope. The API is the OWNER surface, so `actor` and
/// `map_id` are NEVER read from the body — the server forces them.
#[derive(Debug, Deserialize)]
pub struct ApplyOperationsRequest {
    #[serde(default)]
    pub operations: Vec<MapOperation>,
    pub idempotency_key: String,
    pub base_revision: u64,
    #[serde(default)]
    pub envelope_id: Option<String>,
    #[serde(default)]
    pub utterance_id: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListMapsQuery {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    /// Exact lifecycle filter. Absent preserves the default visible-list
    /// contract (active + paused + archived; deleted excluded).
    pub lifecycle: Option<MapLifecycle>,
}

#[derive(Debug, Deserialize)]
pub struct PermanentDeleteMapQuery {
    /// Defense-in-depth for an irreversible endpoint. Must be exactly
    /// `permanent`; the web UI additionally uses a themed confirmation dialog.
    pub confirm: Option<String>,
}

/// Hard cap on one list page; a `limit` above this is clamped, `0` becomes `1`.
const MAX_LIST_PAGE: usize = 200;

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    #[serde(default)]
    pub after_seq: u64,
}

#[derive(Debug, Deserialize)]
pub struct ReplayQuery {
    #[serde(default)]
    pub at_seq: u64,
}

/// Query for `GET /{map_id}/export/markdown`. The flags arrive as raw strings
/// so the documented empty-value form (`?include_provisional=&include_superseded=`)
/// falls back to the defaults instead of failing bool deserialization.
#[derive(Debug, Deserialize)]
pub struct ExportMarkdownQuery {
    #[serde(default)]
    pub include_provisional: Option<String>,
    #[serde(default)]
    pub include_superseded: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RestoreRequest {
    pub at_sequence: u64,
    pub new_map_id: String,
    pub new_title: String,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Owner-triggered interpretation of one finalized utterance. The produced ops
/// are model-authored (`actor = Model`), so the model can only express the safe
/// operation subset (the interpreter's schema forbids owner-only ops).
#[derive(Debug, Deserialize)]
pub struct InterpretRequest {
    #[serde(default)]
    pub utterance_id: Option<String>,
    pub text: String,
    #[serde(default)]
    pub thread_id: Option<String>,
    /// Steering intent: `"continue_thinking"` (default) or `"break_open"`.
    /// Unknown/absent values fall back to `continue_thinking`.
    #[serde(default)]
    pub intent: Option<String>,
    /// Request-scoped node to continue from. Clients send this explicitly so
    /// interpretation never depends on a preceding `set_shared_view` mutation
    /// winning a race. The override shapes interpreter context only; it does not
    /// persist another collaborator's UI selection.
    #[serde(default)]
    pub focus_node_id: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Owner-only PATCH of a map's metadata (title and/or lifecycle). Applied as an
/// owner-authored envelope through the reducer, so the change is event-sourced
/// and replays consistently. At least one of `title`/`lifecycle` must be set.
#[derive(Debug, Deserialize)]
pub struct PatchMapRequest {
    #[serde(default)]
    pub title: Option<String>,
    /// serde enum, snake_case: `"active"`/`"paused"`/`"archived"`/`"deleted"`.
    #[serde(default)]
    pub lifecycle: Option<MapLifecycle>,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Owner-triggered board consolidation. No input beyond scope — the model reads
/// the whole-board digest and proposes a reorganization staged as a PENDING
/// proposal (nothing changes until the owner confirms via the decision endpoint).
#[derive(Debug, Deserialize)]
pub struct ConsolidateRequest {
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Owner confirm/reject of a pending restructure proposal. `decision` is
/// `"confirm"` or `"reject"`; the `proposal_id` comes from the path.
#[derive(Debug, Deserialize)]
pub struct ProposalDecisionRequest {
    pub decision: String,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Attach a live source (chat/voice/meeting) session to a map so its finalized
/// user utterances auto-map (interpret → apply) via the ambient
/// [`ThinkingMapSessionCoordinator`]. `source_session_id` is the chat session id
/// whose `ChatMessageReceived` events should feed the map.
#[derive(Debug, Deserialize)]
pub struct AttachSessionRequest {
    pub source_session_id: String,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Lenient query-bool parse for the export flags: absent/empty/unknown ⇒
/// `default`; `true`/`1`/`yes` ⇒ true; `false`/`0`/`no` ⇒ false.
fn parse_query_bool(raw: Option<&str>, default: bool) -> bool {
    let Some(raw) = raw else { return default };
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => true,
        "false" | "0" | "no" => false,
        _ => default,
    }
}

/// Parse the request `intent` string into an [`InterpretIntent`]. Unknown or
/// absent ⇒ `ContinueThinking` (the safe default).
fn parse_intent(raw: Option<&str>) -> InterpretIntent {
    match raw {
        Some("break_open") => InterpretIntent::BreakOpen,
        _ => InterpretIntent::ContinueThinking,
    }
}

/// Apply an explicit request-scoped interpretation focus to the map snapshot.
/// Empty/absent focus keeps the canonical shared view for older clients. A
/// supplied focus must name a live node; silently falling back to the root would
/// make the user's selected branch look accepted while doing different work.
fn apply_interpret_focus(
    map: &mut ThinkingMap,
    raw_focus_node_id: Option<&str>,
) -> std::result::Result<(), String> {
    let Some(focus_node_id) = raw_focus_node_id
        .map(str::trim)
        .filter(|focus_node_id| !focus_node_id.is_empty())
    else {
        return Ok(());
    };
    // Prefer the exact canonical string key. For UUID-shaped legacy client
    // values, accept an equivalent casing only when it identifies exactly one
    // live node, then place the canonical stored key into interpreter context.
    // This is UUID parsing/normalization, not a fuzzy identifier heuristic.
    let canonical_focus = map
        .nodes
        .get(focus_node_id)
        .filter(|node| !node.tombstoned)
        .map(|_| focus_node_id.to_string())
        .or_else(|| {
            let requested = uuid::Uuid::parse_str(focus_node_id).ok()?;
            let mut matches = map.nodes.iter().filter_map(|(node_id, node)| {
                (!node.tombstoned
                    && uuid::Uuid::parse_str(node_id).ok().as_ref() == Some(&requested))
                .then_some(node_id)
            });
            let matched = matches.next()?;
            matches.next().is_none().then(|| matched.clone())
        })
        .ok_or_else(|| focus_node_id.to_string())?;
    map.view_state.active_node = Some(canonical_focus);
    Ok(())
}

// ── Handlers ──────────────────────────────────────────────────────────────────

pub async fn create_map_handler(
    api: web::Data<Arc<ThinkingMapsApi>>,
    req: HttpRequest,
    body: web::Json<CreateMapRequest>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };

    let map_id = body
        .map_id
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let source = body.source.unwrap_or(ThinkingMapSource::Solo);
    let created_at = now_rfc3339();

    let map = ThinkingMap::new(map_id, principal, workspace, body.title, source, created_at);

    match api.store().create_map(&map).await {
        Ok(()) => Ok(HttpResponse::Created().json(map)),
        Err(error) => Ok(thinking_map_store_error(error)),
    }
}

/// `GET /thinking-maps` — most-recently-updated first (store order).
/// Deleted maps are durable tombstones and are excluded from the default rows
/// and total; `?lifecycle=deleted` selects them explicitly for recovery/admin
/// surfaces. Without `limit` this returns the legacy bare array (all maps in the
/// selected view). With `?limit=N[&offset=M]` it returns a page envelope
/// `{maps, total, offset, limit}` so clients can render incrementally and know
/// when they have everything.
///
/// Each summary carries an OPTIONAL `node_preview` (`{nodes, edges}`) — a
/// bounded thumbnail of the map's first few live nodes + their branch edges —
/// so a client can draw a library-card mini-graph without fetching the full
/// map. The field is omitted for empty maps and safely ignored by older clients.
pub async fn list_maps_handler(
    api: web::Data<Arc<ThinkingMapsApi>>,
    req: HttpRequest,
    query: web::Query<ListMapsQuery>,
) -> Result<HttpResponse> {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let summaries_result = match query.lifecycle {
        Some(lifecycle) => {
            api.store()
                .list_maps_by_lifecycle(&principal, &workspace, lifecycle)
                .await
        },
        None => api.store().list_maps(&principal, &workspace).await,
    };
    let summaries = match summaries_result {
        Ok(summaries) => summaries,
        Err(error) => return Ok(thinking_map_store_error(error)),
    };
    let Some(limit) = query.limit else {
        return Ok(HttpResponse::Ok().json(summaries));
    };
    let limit = limit.clamp(1, MAX_LIST_PAGE);
    let offset = query.offset.unwrap_or(0);
    let total = summaries.len();
    let maps: Vec<MapSummary> = summaries.into_iter().skip(offset).take(limit).collect();
    Ok(HttpResponse::Ok().json(json!({
        "maps": maps,
        "total": total,
        "offset": offset,
        "limit": limit,
    })))
}

pub async fn get_map_handler(
    api: web::Data<Arc<ThinkingMapsApi>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> Result<HttpResponse> {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let map_id = path.into_inner();
    match api.store().load_map(&principal, &workspace, &map_id).await {
        Ok(Some(map)) => Ok(HttpResponse::Ok().json(map)),
        Ok(None) => Ok(HttpResponse::NotFound().json(json!({
            "error": "not_found",
            "map_id": map_id,
        }))),
        Err(error) => Ok(thinking_map_store_error(error)),
    }
}

/// `DELETE /thinking-maps/{map_id}?confirm=permanent` — irreversibly remove a
/// previously soft-deleted map and its complete durable history. Active,
/// paused, and archived maps fail closed with 409; missing confirmation is 400.
pub async fn permanently_delete_map_handler(
    api: web::Data<Arc<ThinkingMapsApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PermanentDeleteMapQuery>,
    coordinator: Option<web::Data<Arc<ThinkingMapSessionCoordinator>>>,
) -> Result<HttpResponse> {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let map_id = path.into_inner();
    if query.confirm.as_deref() != Some("permanent") {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "permanent_delete_confirmation_required",
            "map_id": map_id,
        })));
    }

    match api
        .store()
        .permanently_delete_map(&principal, &workspace, &map_id)
        .await
    {
        Ok(()) => {
            // Durable removal succeeded: now clear ephemeral references. Doing
            // this after the lifecycle-gated store call prevents a rejected
            // purge of an active map from detaching valid sessions.
            let detached_sessions = coordinator
                .as_ref()
                .map(|value| value.unregister_map(&principal, &workspace, &map_id))
                .unwrap_or(0);
            let cleared_tutor_contexts =
                tutor_map_context_registry().clear_map(&principal, &workspace, &map_id);
            Ok(HttpResponse::Ok().json(json!({
                "deleted": true,
                "map_id": map_id,
                "detached_sessions": detached_sessions,
                "cleared_tutor_contexts": cleared_tutor_contexts,
            })))
        },
        Err(error) => Ok(thinking_map_store_error(error)),
    }
}

pub async fn apply_operations_handler(
    api: web::Data<Arc<ThinkingMapsApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ApplyOperationsRequest>,
    broadcaster: Option<web::Data<Arc<RuntimeTransportBroadcaster>>>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let map_id = path.into_inner();

    // OWNER surface: force map_id = path param and actor = Owner { principal }.
    // A client CANNOT spoof the actor or target a different map.
    let envelope_id = body
        .envelope_id
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut envelope = MapOperationEnvelope::new(
        envelope_id,
        map_id.clone(),
        body.base_revision,
        OperationActor::Owner {
            principal: principal.clone(),
        },
        body.idempotency_key,
        body.operations,
        now_rfc3339(),
    );
    envelope.utterance_id = body.utterance_id;
    envelope.schema_version = THINKING_MAP_SCHEMA_VERSION;

    let applied_at = now_rfc3339();
    match api
        .store()
        .apply_and_persist(&principal, &workspace, &map_id, &envelope, &applied_at)
        .await
    {
        Ok(ApplyOutcome::Applied {
            map,
            resulting_revision,
            semantic_hash,
        }) => {
            emit_map_updated(
                broadcaster.as_ref(),
                &map_id,
                &principal,
                &workspace,
                resulting_revision,
            );
            Ok(HttpResponse::Ok().json(json!({
                "outcome": "applied",
                "resulting_revision": resulting_revision,
                "semantic_hash": semantic_hash,
                "map": map,
            })))
        },
        Ok(ApplyOutcome::IdempotentReplay { resulting_revision }) => {
            Ok(HttpResponse::Ok().json(json!({
                "outcome": "idempotent_replay",
                "resulting_revision": resulting_revision,
            })))
        },
        Err(error) => Ok(thinking_map_store_error(error)),
    }
}

pub async fn get_events_handler(
    api: web::Data<Arc<ThinkingMapsApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<EventsQuery>,
) -> Result<HttpResponse> {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let map_id = path.into_inner();
    match api
        .store()
        .events_after(&principal, &workspace, &map_id, query.after_seq)
        .await
    {
        Ok(events) => Ok(HttpResponse::Ok().json(events)),
        Err(error) => Ok(thinking_map_store_error(error)),
    }
}

pub async fn replay_handler(
    api: web::Data<Arc<ThinkingMapsApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ReplayQuery>,
) -> Result<HttpResponse> {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let map_id = path.into_inner();
    match api
        .store()
        .replay_to_sequence(&principal, &workspace, &map_id, query.at_seq)
        .await
    {
        Ok(map) => Ok(HttpResponse::Ok().json(map)),
        Err(error) => Ok(thinking_map_store_error(error)),
    }
}

/// `GET /thinking-maps/{id}/export/markdown` — deterministic Markdown export
/// of the current map (plan Phase 9 item 2). Pure read: loads the map and
/// renders it via [`render_markdown`] — no clock/randomness, so the same map
/// state always exports byte-identically. Query flags `include_provisional`
/// (default true) and `include_superseded` (default false) parse leniently
/// (absent/empty ⇒ default); tombstoned content is never exported. Responds
/// `text/markdown; charset=utf-8`.
pub async fn export_markdown_handler(
    api: web::Data<Arc<ThinkingMapsApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ExportMarkdownQuery>,
) -> Result<HttpResponse> {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let map_id = path.into_inner();
    let map = match api.store().load_map(&principal, &workspace, &map_id).await {
        Ok(Some(map)) => map,
        Ok(None) => {
            return Ok(HttpResponse::NotFound().json(json!({
                "error": "not_found",
                "map_id": map_id,
            })))
        },
        Err(error) => return Ok(thinking_map_store_error(error)),
    };

    let opts = ExportOptions {
        include_provisional: parse_query_bool(query.include_provisional.as_deref(), true),
        include_superseded: parse_query_bool(query.include_superseded.as_deref(), false),
    };
    let markdown = render_markdown(&map, &opts);
    Ok(HttpResponse::Ok()
        .content_type("text/markdown; charset=utf-8")
        .body(markdown))
}

pub async fn restore_handler(
    api: web::Data<Arc<ThinkingMapsApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RestoreRequest>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let source_map_id = path.into_inner();
    let restored_at = now_rfc3339();
    match api
        .store()
        .restore_as_branch(
            &principal,
            &workspace,
            &source_map_id,
            body.at_sequence,
            &body.new_map_id,
            &body.new_title,
            &restored_at,
        )
        .await
    {
        Ok(branch) => Ok(HttpResponse::Created().json(branch)),
        Err(error) => Ok(thinking_map_store_error(error)),
    }
}

/// Owner-only PATCH of a map's metadata (title and/or lifecycle). Builds an
/// owner-authored envelope with `SetTitle`/`SetLifecycle` ops and applies it
/// through the reducer, so the change is event-sourced + replay-consistent.
pub async fn patch_map_handler(
    api: web::Data<Arc<ThinkingMapsApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PatchMapRequest>,
    broadcaster: Option<web::Data<Arc<RuntimeTransportBroadcaster>>>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let map_id = path.into_inner();

    // Nothing to patch ⇒ 400 (avoids minting an empty envelope the reducer
    // would reject as `empty_envelope`).
    if body.title.is_none() && body.lifecycle.is_none() {
        return Ok(HttpResponse::BadRequest().json(json!({ "error": "nothing_to_patch" })));
    }

    let store = api.store();

    // Load the map (404 if absent) to read its current revision for the
    // optimistic-concurrency base.
    let map = match store.load_map(&principal, &workspace, &map_id).await {
        Ok(Some(map)) => map,
        Ok(None) => {
            return Ok(HttpResponse::NotFound().json(json!({
                "error": "not_found",
                "map_id": map_id,
            })))
        },
        Err(error) => return Ok(thinking_map_store_error(error)),
    };
    let base_revision = map.revision;

    // Build the owner metadata ops (order: title then lifecycle).
    let mut operations: Vec<MapOperation> = Vec::with_capacity(2);
    if let Some(title) = body.title {
        operations.push(MapOperation::SetTitle { title });
    }
    if let Some(lifecycle) = body.lifecycle {
        operations.push(MapOperation::SetLifecycle { lifecycle });
    }

    let applied_at = now_rfc3339();
    let idempotency_key = format!("patch:{}:{}", map_id, uuid::Uuid::new_v4());
    let mut envelope = MapOperationEnvelope::new(
        uuid::Uuid::new_v4().to_string(),
        map_id.clone(),
        base_revision,
        OperationActor::Owner {
            principal: principal.clone(),
        },
        idempotency_key,
        operations,
        applied_at.clone(),
    );
    envelope.schema_version = THINKING_MAP_SCHEMA_VERSION;

    match store
        .apply_and_persist(&principal, &workspace, &map_id, &envelope, &applied_at)
        .await
    {
        Ok(ApplyOutcome::Applied {
            map,
            resulting_revision,
            semantic_hash,
        }) => {
            emit_map_updated(
                broadcaster.as_ref(),
                &map_id,
                &principal,
                &workspace,
                resulting_revision,
            );
            Ok(HttpResponse::Ok().json(json!({
                "outcome": "applied",
                "resulting_revision": resulting_revision,
                "semantic_hash": semantic_hash,
                "map": map,
            })))
        },
        Ok(ApplyOutcome::IdempotentReplay { resulting_revision }) => {
            Ok(HttpResponse::Ok().json(json!({
                "outcome": "idempotent_replay",
                "resulting_revision": resulting_revision,
            })))
        },
        Err(error) => Ok(thinking_map_store_error(error)),
    }
}

/// Interpret one finalized utterance against the map via the LLM, applying the
/// resulting model-authored envelope. Owner-triggered but the ops are stamped
/// `actor = Model` by the interpreter (the model can only express the safe
/// subset), so this cannot forge owner-only operations.
pub async fn interpret_handler(
    api: web::Data<Arc<ThinkingMapsApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<InterpretRequest>,
    // Registered as actix app_data by the bin; `Option` so tests (which don't
    // register one) still exercise the scope/not-found paths — telemetry is
    // best-effort and simply not emitted when absent.
    broadcaster: Option<web::Data<Arc<RuntimeTransportBroadcaster>>>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let map_id = path.into_inner();
    let store = api.store();

    // Load the map first (404 if absent) — interpretation needs its context +
    // current revision.
    let mut map = match store.load_map(&principal, &workspace, &map_id).await {
        Ok(Some(map)) => map,
        Ok(None) => {
            return Ok(HttpResponse::NotFound().json(json!({
                "error": "not_found",
                "map_id": map_id,
            })))
        },
        Err(error) => return Ok(thinking_map_store_error(error)),
    };

    if let Err(focus_node_id) = apply_interpret_focus(&mut map, body.focus_node_id.as_deref()) {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "invalid_focus_node",
            "focus_node_id": focus_node_id,
        })));
    }

    // The interpreter needs a live LLM; the router is a process-global set at
    // startup. Absent (tests, pre-startup) ⇒ 503.
    let Some(router) = global_operation_router() else {
        return Ok(HttpResponse::ServiceUnavailable().json(json!({
            "error": "llm_unavailable",
        })));
    };
    let adapter = RouterInterpreterLlm::new(
        router,
        broadcaster.as_ref().map(|b| b.get_ref().clone()),
        principal.clone(),
        workspace.clone(),
    );

    let intent = parse_intent(body.intent.as_deref());
    let utterance_id = body
        .utterance_id
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // Narration for THIS interpretation. Built after the utterance id is
    // settled so every stage names the run the response will name — that id
    // is how a client ignores progress for a run it did not start.
    let progress = TransportInterpretProgress::new(
        broadcaster.as_ref(),
        &map_id,
        &principal,
        &workspace,
        &utterance_id,
    );

    let utterance = Utterance {
        utterance_id,
        text: body.text,
        thread_id: body.thread_id,
        timestamp: Some(now_rfc3339()),
    };

    let applied_at = now_rfc3339();
    let envelope = match interpret(
        &map,
        &utterance,
        intent,
        &adapter,
        &applied_at,
        Some("thinking_map_interpret".to_string()),
        &progress,
    )
    .await
    {
        Ok(Some(envelope)) => envelope,
        // Valid zero-move interpretation (chit-chat / no structural change).
        Ok(None) => {
            progress.idle();
            return Ok(HttpResponse::Ok().json(json!({ "outcome": "no_operations" })));
        },
        // Model/transport/parse failure. Idle is emitted on failure too —
        // otherwise a client's strip stays on "facilitating" for a run that
        // died, which is the same lie the spinner used to tell.
        Err(error) => {
            progress.idle();
            return Ok(HttpResponse::BadGateway().json(json!({
                "error": "interpretation_failed",
                "details": error.to_string(),
            })));
        },
    };

    let outcome = store
        .apply_and_persist(&principal, &workspace, &map_id, &envelope, &applied_at)
        .await;
    // Every settle path is idle: applied, replayed, or refused by the store.
    progress.idle();
    match outcome {
        Ok(ApplyOutcome::Applied {
            map,
            resulting_revision,
            semantic_hash,
        }) => {
            emit_map_updated(
                broadcaster.as_ref(),
                &map_id,
                &principal,
                &workspace,
                resulting_revision,
            );
            Ok(HttpResponse::Ok().json(json!({
                "outcome": "applied",
                "resulting_revision": resulting_revision,
                "semantic_hash": semantic_hash,
                "map": map,
            })))
        },
        Ok(ApplyOutcome::IdempotentReplay { resulting_revision }) => {
            Ok(HttpResponse::Ok().json(json!({
                "outcome": "idempotent_replay",
                "resulting_revision": resulting_revision,
            })))
        },
        Err(error) => Ok(thinking_map_store_error(error)),
    }
}

/// Consolidate the board via the LLM, staging a model-authored restructure
/// proposal. Owner-triggered but the proposal is authored `actor = Model` (the
/// model can only express the safe subset), so nothing on the board changes until
/// the owner confirms it via the decision endpoint. On success the map is
/// returned with the new PENDING proposal in `proposals` (its inner ops are NOT
/// yet materialized).
pub async fn consolidate_handler(
    api: web::Data<Arc<ThinkingMapsApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ConsolidateRequest>,
    // Registered as actix app_data by the bin; `Option` so tests (which don't
    // register one) still exercise the scope/not-found/no-router paths.
    broadcaster: Option<web::Data<Arc<RuntimeTransportBroadcaster>>>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let map_id = path.into_inner();
    let store = api.store();

    // Load the map first (404 if absent) — consolidation needs its board digest +
    // current revision.
    let map = match store.load_map(&principal, &workspace, &map_id).await {
        Ok(Some(map)) => map,
        Ok(None) => {
            return Ok(HttpResponse::NotFound().json(json!({
                "error": "not_found",
                "map_id": map_id,
            })))
        },
        Err(error) => return Ok(thinking_map_store_error(error)),
    };

    // Consolidation needs a live LLM; the router is a process-global set at
    // startup. Absent (tests, pre-startup) ⇒ 503.
    let Some(router) = global_operation_router() else {
        return Ok(HttpResponse::ServiceUnavailable().json(json!({
            "error": "llm_unavailable",
        })));
    };
    // Reuse the interpreter's router adapter (SAME `thinking_map_interpret`
    // operation/profile — no new config mapping; usage metering for consolidation
    // is lumped with interpret under that label, which is acceptable for v1).
    let adapter = RouterInterpreterLlm::new(
        router,
        broadcaster.as_ref().map(|b| b.get_ref().clone()),
        principal.clone(),
        workspace.clone(),
    );

    let applied_at = now_rfc3339();
    let envelope = match consolidate(
        &map,
        &adapter,
        &applied_at,
        Some("thinking_map_consolidate".to_string()),
    )
    .await
    {
        Ok(Some(envelope)) => envelope,
        // Valid no-op consolidation (board already well-organized).
        Ok(None) => return Ok(HttpResponse::Ok().json(json!({ "outcome": "no_operations" }))),
        // Model/transport/parse failure.
        Err(error) => {
            return Ok(HttpResponse::BadGateway().json(json!({
                "error": "consolidation_failed",
                "details": error.to_string(),
            })))
        },
    };

    match store
        .apply_and_persist(&principal, &workspace, &map_id, &envelope, &applied_at)
        .await
    {
        Ok(ApplyOutcome::Applied {
            map,
            resulting_revision,
            semantic_hash,
        }) => {
            emit_map_updated(
                broadcaster.as_ref(),
                &map_id,
                &principal,
                &workspace,
                resulting_revision,
            );
            Ok(HttpResponse::Ok().json(json!({
                "outcome": "applied",
                "resulting_revision": resulting_revision,
                "semantic_hash": semantic_hash,
                "map": map,
            })))
        },
        Ok(ApplyOutcome::IdempotentReplay { resulting_revision }) => {
            Ok(HttpResponse::Ok().json(json!({
                "outcome": "idempotent_replay",
                "resulting_revision": resulting_revision,
            })))
        },
        Err(error) => Ok(thinking_map_store_error(error)),
    }
}

/// Owner confirm/reject of a pending restructure proposal. Builds an
/// owner-authored envelope carrying `ConfirmRestructure`/`RejectRestructure` for
/// the path `proposal_id`. On confirm, the reducer re-applies the proposal's
/// inner ops under the STORED proposer's authority (never blanket owner), so a
/// model-proposed reorganization materializes as model-authored content.
pub async fn proposal_decision_handler(
    api: web::Data<Arc<ThinkingMapsApi>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<ProposalDecisionRequest>,
    broadcaster: Option<web::Data<Arc<RuntimeTransportBroadcaster>>>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let (map_id, proposal_id) = path.into_inner();

    // Map the decision string to the owner-only op (unknown ⇒ 400).
    let op = match body.decision.as_str() {
        "confirm" => MapOperation::ConfirmRestructure {
            proposal_id: proposal_id.clone(),
        },
        "reject" => MapOperation::RejectRestructure {
            proposal_id: proposal_id.clone(),
        },
        _ => return Ok(HttpResponse::BadRequest().json(json!({ "error": "invalid_decision" }))),
    };

    let store = api.store();

    // Load the map (404 if absent) to read its current revision for the
    // optimistic-concurrency base.
    let map = match store.load_map(&principal, &workspace, &map_id).await {
        Ok(Some(map)) => map,
        Ok(None) => {
            return Ok(HttpResponse::NotFound().json(json!({
                "error": "not_found",
                "map_id": map_id,
            })))
        },
        Err(error) => return Ok(thinking_map_store_error(error)),
    };
    let base_revision = map.revision;

    let applied_at = now_rfc3339();
    let idempotency_key = format!("proposal-decision:{}:{}", proposal_id, uuid::Uuid::new_v4());
    let mut envelope = MapOperationEnvelope::new(
        uuid::Uuid::new_v4().to_string(),
        map_id.clone(),
        base_revision,
        OperationActor::Owner {
            principal: principal.clone(),
        },
        idempotency_key,
        vec![op],
        applied_at.clone(),
    );
    envelope.schema_version = THINKING_MAP_SCHEMA_VERSION;

    match store
        .apply_and_persist(&principal, &workspace, &map_id, &envelope, &applied_at)
        .await
    {
        Ok(ApplyOutcome::Applied {
            map,
            resulting_revision,
            semantic_hash,
        }) => {
            emit_map_updated(
                broadcaster.as_ref(),
                &map_id,
                &principal,
                &workspace,
                resulting_revision,
            );
            Ok(HttpResponse::Ok().json(json!({
                "outcome": "applied",
                "resulting_revision": resulting_revision,
                "semantic_hash": semantic_hash,
                "map": map,
            })))
        },
        Ok(ApplyOutcome::IdempotentReplay { resulting_revision }) => {
            Ok(HttpResponse::Ok().json(json!({
                "outcome": "idempotent_replay",
                "resulting_revision": resulting_revision,
            })))
        },
        Err(error) => Ok(thinking_map_store_error(error)),
    }
}

/// `POST /thinking-maps/{id}/nodes/{node_id}/promote` — GOVERNED promotion of
/// a node into a durable Magician object (plan Phase 8). Rules:
/// - tombstoned / rejected / superseded / contradicted nodes NEVER promote;
/// - owner-asserted content promotes by default; anything else (model_inferred,
///   provisional, participant/imported) requires `confirm: true`, and the
///   confirmation is recorded as an owner assertion (`set_epistemic_state →
///   asserted`) in the SAME envelope — explicit confirmation for model
///   inference, participant/imported never auto-create;
/// - IDEMPOTENT: an existing promotion link of the same kind returns the
///   existing object (`promoted: false`) — retry creates no duplicate;
/// - provenance travels: the created object carries map/node/origin metadata,
///   and the map records the link via `link_promoted_object` (the reducer
///   enforces global promotion-link uniqueness).
///
/// Targets: `task` (via the v3 task service, absent in minimal test apps →
/// 503 `promotion_unavailable`) and `memory` (a review-gated learning
/// candidate targeting `user.knowledge` — conflict review flows through the
/// canonical Memory handling, nothing is written to memory directly).
#[derive(Debug, Deserialize)]
pub struct PromoteNodeRequest {
    pub target: String,
    #[serde(default)]
    pub confirm: bool,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Outcome of the shared governed node-promotion flow
/// ([`promote_node_to_target`]). One variant per distinct wire response of the
/// promote endpoint, so every caller (REST handler, Today source-action
/// execution) shares identical semantics.
#[derive(Debug)]
pub(crate) enum NodePromotionOutcome {
    MapNotFound,
    NodeNotFound,
    NotPromotable {
        epistemic_state: EpistemicState,
        tombstoned: bool,
    },
    /// Idempotent short-circuit: the node already carries a promotion link of
    /// this kind — the existing object wins, nothing new is created.
    AlreadyLinked {
        object_id: String,
    },
    ConfirmationRequired {
        assertion_origin: AssertionOrigin,
        epistemic_state: EpistemicState,
    },
    /// The destination service (v3 tasks) is not available in this process.
    TargetUnavailable,
    CreateFailed {
        detail: String,
    },
    Promoted {
        object_id: String,
        /// `None` when the link commit replayed idempotently.
        resulting_revision: Option<u64>,
    },
    Store(ThinkingMapStoreError),
}

/// GOVERNED promotion of a node into a durable Magician object (plan Phase 8).
/// This is THE single implementation of the promote semantics — the REST
/// handler and the Today `thinking_map_action` source-action both call it, so
/// there is exactly one task-creation path. Rules:
/// - tombstoned / rejected / superseded / contradicted nodes NEVER promote;
/// - owner-asserted content promotes by default; anything else (model_inferred,
///   provisional, participant/imported) requires `confirm`, and the
///   confirmation is recorded as an owner assertion (`set_epistemic_state →
///   asserted`) in the SAME envelope;
/// - IDEMPOTENT: an existing promotion link of the same kind returns the
///   existing object — retry creates no duplicate;
/// - provenance travels: the created object carries map/node/origin metadata
///   (including the machine-readable `Thinking map node source: <map>:<node>`
///   marker line used by Today reconciliation), and the map records the link
///   via `link_promoted_object`.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) async fn promote_node_to_target(
    workspace: &ArtifactV2Workspace,
    task_service: Option<Arc<magician::magician_v2::artifact_v2::ArtifactV2Service>>,
    broadcaster: Option<&Arc<RuntimeTransportBroadcaster>>,
    principal: &str,
    scope_workspace: &str,
    map_id: &str,
    node_id: &str,
    kind: PromotionKind,
    confirm: bool,
) -> NodePromotionOutcome {
    let store = ThinkingMapStore::new(workspace.clone());
    let map = match store.load_map(principal, scope_workspace, map_id).await {
        Ok(Some(map)) => map,
        Ok(None) => return NodePromotionOutcome::MapNotFound,
        Err(error) => return NodePromotionOutcome::Store(error),
    };
    let Some(node) = map.nodes.get(node_id) else {
        return NodePromotionOutcome::NodeNotFound;
    };

    // Dead or discredited content is never promotable — rejected/superseded
    // history must not become current truth.
    if node.tombstoned
        || matches!(
            node.epistemic_state,
            EpistemicState::Rejected | EpistemicState::Superseded | EpistemicState::Contradicted
        )
    {
        return NodePromotionOutcome::NotPromotable {
            epistemic_state: node.epistemic_state,
            tombstoned: node.tombstoned,
        };
    }

    // Idempotency: an existing link of this kind wins — no duplicate objects.
    if let Some(existing) = node
        .promoted_refs
        .iter()
        .find(|r| r.destination_kind == kind)
    {
        return NodePromotionOutcome::AlreadyLinked {
            object_id: existing.object_id.clone(),
        };
    }

    // Authority: owner-asserted promotes by default; everything else needs an
    // explicit owner confirmation, which becomes an owner assertion.
    let owner_asserted = matches!(
        node.assertion_origin,
        AssertionOrigin::OwnerSpoken | AssertionOrigin::OwnerEdited
    ) && matches!(
        node.epistemic_state,
        EpistemicState::Asserted | EpistemicState::Confirmed
    );
    if !owner_asserted && !confirm {
        return NodePromotionOutcome::ConfirmationRequired {
            assertion_origin: node.assertion_origin,
            epistemic_state: node.epistemic_state,
        };
    }

    let provenance = format!(
        "Promoted from Thinking Map `{}` node `{}` (origin: {:?}, state: {:?}).\n{}",
        map.title,
        node.label,
        node.assertion_origin,
        node.epistemic_state,
        magician::magician_v2::feed::thinking_map_node_source_marker(map_id, node_id),
    );

    // Create the durable object FIRST; the map link commits after, keyed
    // idempotently so a crash between the two is retried safely (the retry
    // short-circuits on the existing link, or re-links the same object id via
    // the idempotency ledger).
    let object_id = match kind {
        PromotionKind::Task => {
            let Some(task_service) = task_service else {
                return NodePromotionOutcome::TargetUnavailable;
            };
            let input = magician::magician_v2::artifact_v2::CreateTaskInput {
                principal: principal.to_string(),
                workspace: scope_workspace.to_string(),
                title: node.label.chars().take(120).collect(),
                description: format!(
                    "{}\n\n{}",
                    node.detail_markdown.clone().unwrap_or_default(),
                    provenance
                )
                .trim()
                .to_string(),
                agent_id: "personal-assistant".to_string(),
                goal_id: None,
                ui_thread_id: format!("thinking-map-{map_id}"),
                priority: None,
                due_date: None,
                tags: Vec::new(),
                created_by: "thinking_map_promotion".to_string(),
                depends_on: Vec::new(),
                approved: false,
                schedule: None,
                output_mode: Default::default(),
                chat_session_id: None,
                lifecycle: Default::default(),
                sync_mode: Default::default(),
            };
            match task_service.create_task(input).await {
                Ok(task) => task.manifest.task_id.clone(),
                Err(error) => {
                    return NodePromotionOutcome::CreateFailed {
                        detail: error.to_string(),
                    }
                },
            }
        },
        PromotionKind::Memory => {
            let learning = LearningStore::new(workspace.clone());
            let request = CreateLearningCandidateRequest {
                principal: None,
                workspace: None,
                candidate_type: LearningCandidateType::MemoryFact,
                state: LearningCandidateState::Proposed,
                title: node.label.chars().take(120).collect(),
                summary: node
                    .detail_markdown
                    .clone()
                    .unwrap_or_else(|| node.label.clone()),
                rationale: provenance.clone(),
                proposed_change: json!({
                    "memory": {
                        "scope": "user",
                        "target_tier": "knowledge",
                        "operation": "upsert",
                        "key": format!("thinking-map:{map_id}:{node_id}"),
                        "value": node.detail_markdown.clone().unwrap_or_else(|| node.label.clone()),
                    }
                }),
                proposed_target: Some("user.knowledge".to_string()),
                confidence: Some(f64::from(node.confidence)),
                source_agent_id: None,
                source_task_id: None,
                source_execution_id: None,
                source_chat_session_id: None,
                event_refs: Vec::new(),
                evidence_refs: vec![LearningEvidenceRef {
                    kind: "thinking_map_node".to_string(),
                    id: Some(format!("{map_id}:{node_id}")),
                    path: None,
                    uri: None,
                    summary: Some(node.label.clone()),
                }],
                risk_level: LearningRiskLevel::Medium,
                review_required: true,
                review_reason: Some("thinking-map promotion".to_string()),
                review_policy: serde_json::Value::Null,
                promotion_target: None,
                promotion_policy: serde_json::Value::Null,
            };
            let scope = LearningScope::new(principal, scope_workspace);
            match learning.create_candidate(scope, request) {
                Ok(candidate) => candidate.id,
                Err(error) => {
                    return NodePromotionOutcome::CreateFailed {
                        detail: error.to_string(),
                    }
                },
            }
        },
        // `today` is not a creatable destination — Today candidates are a
        // read-side projection of unlinked action nodes, never durable objects.
        PromotionKind::Today => return NodePromotionOutcome::TargetUnavailable,
    };

    // Commit the link (plus the confirmation-as-assertion when needed).
    let applied_at = now_rfc3339();
    let mut operations = Vec::with_capacity(2);
    if !owner_asserted {
        operations.push(MapOperation::SetEpistemicState {
            node_id: node_id.to_string(),
            state: EpistemicState::Asserted,
        });
    }
    operations.push(MapOperation::LinkPromotedObject {
        node_id: node_id.to_string(),
        promoted: PromotedRef {
            destination_kind: kind,
            object_id: object_id.clone(),
            linked_at: applied_at.clone(),
        },
    });
    let kind_token = match kind {
        PromotionKind::Task => "task",
        PromotionKind::Memory => "memory",
        PromotionKind::Today => "today",
    };
    let mut envelope = MapOperationEnvelope::new(
        uuid::Uuid::new_v4().to_string(),
        map_id.to_string(),
        map.revision,
        OperationActor::Owner {
            principal: principal.to_string(),
        },
        format!("promote:{node_id}:{kind_token}:{object_id}"),
        operations,
        applied_at.clone(),
    );
    envelope.schema_version = THINKING_MAP_SCHEMA_VERSION;
    match store
        .apply_and_persist(principal, scope_workspace, map_id, &envelope, &applied_at)
        .await
    {
        Ok(ApplyOutcome::Applied {
            resulting_revision, ..
        }) => {
            if let Some(broadcaster) = broadcaster {
                broadcaster.emit_transport_only(RuntimeTransportEvent::ThinkingMapUpdated {
                    map_id: map_id.to_string(),
                    principal: principal.to_string(),
                    workspace: scope_workspace.to_string(),
                    revision: resulting_revision,
                    timestamp: chrono::Utc::now().timestamp_millis(),
                });
            }
            NodePromotionOutcome::Promoted {
                object_id,
                resulting_revision: Some(resulting_revision),
            }
        },
        Ok(ApplyOutcome::IdempotentReplay { .. }) => NodePromotionOutcome::Promoted {
            object_id,
            resulting_revision: None,
        },
        Err(error) => NodePromotionOutcome::Store(error),
    }
}

/// Thin wire adapter for `POST /thinking-maps/{id}/nodes/{node_id}/promote`.
/// All semantics live in [`promote_node_to_target`]; this maps the outcome to
/// the endpoint's stable HTTP contract.
pub async fn promote_node_handler(
    api: web::Data<Arc<ThinkingMapsApi>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<PromoteNodeRequest>,
    task_api: Option<web::Data<crate::task_api_v3::TaskApiV3>>,
    broadcaster: Option<web::Data<Arc<RuntimeTransportBroadcaster>>>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let (map_id, node_id) = path.into_inner();

    let kind = match body.target.as_str() {
        "task" => PromotionKind::Task,
        "memory" => PromotionKind::Memory,
        _ => {
            return Ok(HttpResponse::BadRequest()
                .json(json!({ "error": "invalid_target", "allowed": ["task", "memory"] })))
        },
    };

    let outcome = promote_node_to_target(
        api.workspace(),
        task_api.map(|task_api| task_api.service()),
        broadcaster.as_ref().map(|data| data.get_ref()),
        &principal,
        &workspace,
        &map_id,
        &node_id,
        kind,
        body.confirm,
    )
    .await;

    Ok(match outcome {
        NodePromotionOutcome::MapNotFound => {
            HttpResponse::NotFound().json(json!({ "error": "not_found", "map_id": map_id }))
        },
        NodePromotionOutcome::NodeNotFound => {
            HttpResponse::NotFound().json(json!({ "error": "node_not_found", "node_id": node_id }))
        },
        NodePromotionOutcome::NotPromotable {
            epistemic_state,
            tombstoned,
        } => HttpResponse::Conflict().json(json!({
            "error": "not_promotable",
            "epistemic_state": epistemic_state,
            "tombstoned": tombstoned,
        })),
        NodePromotionOutcome::AlreadyLinked { object_id } => HttpResponse::Ok().json(json!({
            "promoted": false,
            "object_kind": kind,
            "object_id": object_id,
        })),
        NodePromotionOutcome::ConfirmationRequired {
            assertion_origin,
            epistemic_state,
        } => HttpResponse::Conflict().json(json!({
            "error": "confirmation_required",
            "assertion_origin": assertion_origin,
            "epistemic_state": epistemic_state,
        })),
        NodePromotionOutcome::TargetUnavailable => {
            HttpResponse::ServiceUnavailable().json(json!({ "error": "promotion_unavailable" }))
        },
        NodePromotionOutcome::CreateFailed { detail } => {
            let error = match kind {
                PromotionKind::Task => "task_create_failed",
                _ => "memory_candidate_failed",
            };
            HttpResponse::BadGateway().json(json!({ "error": error, "detail": detail }))
        },
        NodePromotionOutcome::Promoted {
            object_id,
            resulting_revision: Some(resulting_revision),
        } => HttpResponse::Ok().json(json!({
            "promoted": true,
            "object_kind": kind,
            "object_id": object_id,
            "resulting_revision": resulting_revision,
        })),
        NodePromotionOutcome::Promoted {
            object_id,
            resulting_revision: None,
        } => HttpResponse::Ok().json(json!({
            "promoted": true,
            "object_kind": kind,
            "object_id": object_id,
        })),
        NodePromotionOutcome::Store(error) => thinking_map_store_error(error),
    })
}

/// Attach a live source session to a map. Every subsequent finalized USER
/// utterance in that session auto-maps (interpret → apply) via the ambient
/// [`ThinkingMapSessionCoordinator`]. Owner-authed via the resolved scope; the
/// map must exist (404 otherwise).
///
/// The coordinator is a separate `web::Data` extractor so this handler can be
/// added without restructuring [`ThinkingMapsApi`]. Registered by the bin as
/// app_data; when absent (e.g. a test app that doesn't register one) the route
/// returns `503 coordinator_unavailable`.
pub async fn attach_session_handler(
    api: web::Data<Arc<ThinkingMapsApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AttachSessionRequest>,
    coordinator: Option<web::Data<Arc<ThinkingMapSessionCoordinator>>>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let map_id = path.into_inner();

    let Some(coordinator) = coordinator else {
        return Ok(HttpResponse::ServiceUnavailable().json(json!({
            "error": "coordinator_unavailable",
        })));
    };

    // The map must exist before we start streaming utterances into it.
    match api.store().load_map(&principal, &workspace, &map_id).await {
        Ok(Some(_)) => {},
        Ok(None) => {
            return Ok(HttpResponse::NotFound().json(json!({
                "error": "not_found",
                "map_id": map_id,
            })))
        },
        Err(error) => return Ok(thinking_map_store_error(error)),
    }

    let source_session_id = body.source_session_id;
    coordinator.register(source_session_id.clone(), map_id, principal, workspace);

    Ok(HttpResponse::Ok().json(json!({
        "attached": true,
        "source_session_id": source_session_id,
    })))
}

/// Register a Thinking Map → Tutor grounding binding for a chat session
/// (plan Phase 8 item 8). Body for `POST /thinking-maps/{id}/tutor-context`.
///
/// `session_id` is the chat session whose NEXT tutor run should receive the
/// bounded map digest; `node_id` optionally selects a node so the digest
/// covers that node's neighborhood instead of a whole-map overview.
#[derive(Debug, Deserialize)]
pub struct TutorContextRequest {
    pub session_id: String,
    #[serde(default)]
    pub node_id: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Register (or refresh) the map→tutor binding for a chat session. Loads the
/// map (404 when absent), validates an explicit `node_id` (404
/// `node_not_found`), builds the bounded origin-annotated digest as a
/// SNAPSHOT, and stores it in the process-local TTL registry consumed by
/// `TutorRunStore::start_run`. Re-registration overwrites; the digest is
/// reference context only — Tutor narration/storyboard ownership is untouched.
pub async fn register_tutor_context_handler(
    api: web::Data<Arc<ThinkingMapsApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TutorContextRequest>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let map_id = path.into_inner();

    let session_id = body.session_id.trim().to_string();
    if session_id.is_empty() {
        return Ok(HttpResponse::BadRequest().json(json!({ "error": "missing_session_id" })));
    }
    let node_id = body
        .node_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned);

    let map = match api.store().load_map(&principal, &workspace, &map_id).await {
        Ok(Some(map)) => map,
        Ok(None) => {
            return Ok(HttpResponse::NotFound().json(json!({
                "error": "not_found",
                "map_id": map_id,
            })))
        },
        Err(error) => return Ok(thinking_map_store_error(error)),
    };
    if let Some(node_id) = node_id.as_deref() {
        if !map.nodes.contains_key(node_id) {
            return Ok(HttpResponse::NotFound()
                .json(json!({ "error": "node_not_found", "node_id": node_id })));
        }
    }

    let context = build_tutor_map_context(
        &map,
        node_id.as_deref(),
        DEFAULT_TUTOR_MAP_CONTEXT_BUDGET_CHARS,
    );
    tutor_map_context_registry().register(
        &principal,
        &workspace,
        &session_id,
        TutorMapContextBinding {
            map_id: map_id.clone(),
            node_id: node_id.clone(),
            context: context.clone(),
            registered_at_ms: chrono::Utc::now().timestamp_millis(),
        },
    );

    Ok(HttpResponse::Ok().json(json!({
        "registered": true,
        "map_id": map_id,
        "node_id": node_id,
        "session_id": session_id,
        "context": context,
        "expires_in_ms": TUTOR_MAP_CONTEXT_TTL_MS,
    })))
}

/// Clear the map→tutor binding for a chat session. Idempotent: returns
/// `cleared: false` when no binding existed. When another map now owns the
/// session's binding, it is left untouched (`cleared: false`).
pub async fn clear_tutor_context_handler(
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> Result<HttpResponse> {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let (map_id, session_id) = path.into_inner();
    let registry = tutor_map_context_registry();
    let cleared = match registry.current(&principal, &workspace, &session_id) {
        Some(binding) if binding.map_id == map_id => registry
            .clear(&principal, &workspace, &session_id)
            .is_some(),
        _ => false,
    };
    Ok(HttpResponse::Ok().json(json!({ "cleared": cleared })))
}

/// Detach a live source session from ambient mapping. Idempotent: returns
/// `detached: false` when the session was not registered.
pub async fn detach_session_handler(
    _req: HttpRequest,
    path: web::Path<(String, String)>,
    coordinator: Option<web::Data<Arc<ThinkingMapSessionCoordinator>>>,
) -> Result<HttpResponse> {
    let (_map_id, source_session_id) = path.into_inner();
    let Some(coordinator) = coordinator else {
        return Ok(HttpResponse::ServiceUnavailable().json(json!({
            "error": "coordinator_unavailable",
        })));
    };
    let detached = coordinator.unregister(&source_session_id);
    Ok(HttpResponse::Ok().json(json!({ "detached": detached })))
}

/// Register all Thinking Maps routes under `/thinking-maps`.
pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/thinking-maps")
            .route("", web::post().to(create_map_handler))
            .route("", web::get().to(list_maps_handler))
            .route("/{map_id}", web::get().to(get_map_handler))
            .route("/{map_id}", web::patch().to(patch_map_handler))
            .route(
                "/{map_id}",
                web::delete().to(permanently_delete_map_handler),
            )
            .route(
                "/{map_id}/operations",
                web::post().to(apply_operations_handler),
            )
            .route("/{map_id}/interpret", web::post().to(interpret_handler))
            .route("/{map_id}/consolidate", web::post().to(consolidate_handler))
            .route(
                "/{map_id}/proposals/{proposal_id}/decision",
                web::post().to(proposal_decision_handler),
            )
            .route("/{map_id}/events", web::get().to(get_events_handler))
            .route("/{map_id}/replay", web::get().to(replay_handler))
            .route(
                "/{map_id}/export/markdown",
                web::get().to(export_markdown_handler),
            )
            .route("/{map_id}/restore", web::post().to(restore_handler))
            .route(
                "/{map_id}/nodes/{node_id}/promote",
                web::post().to(promote_node_handler),
            )
            .route("/{map_id}/sessions", web::post().to(attach_session_handler))
            .route(
                "/{map_id}/sessions/{source_session_id}",
                web::delete().to(detach_session_handler),
            )
            .route(
                "/{map_id}/tutor-context",
                web::post().to(register_tutor_context_handler),
            )
            .route(
                "/{map_id}/tutor-context/{session_id}",
                web::delete().to(clear_tutor_context_handler),
            ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    use actix_web::{http::StatusCode, test, App};
    use magician_surfaces::thinking_map::{
        AssertionOrigin, EpistemicState, MapEvent, MapSummary, NodeKind, ThinkingNode,
    };
    use serde_json::json;
    use serde_json::Value;
    use tempfile::TempDir;

    /// Build a fully-mounted test app for the given `ThinkingMapsApi`. Returned
    /// as `impl ...Service<...>`; the concrete request type is provided by the
    /// `App`'s `IntoServiceFactory`, so callers never name it.
    macro_rules! build_app {
        ($api:expr) => {
            test::init_service(
                App::new()
                    .app_data(web::Data::new($api))
                    .configure(configure),
            )
            .await
        };
    }

    fn api() -> (TempDir, Arc<ThinkingMapsApi>) {
        let tmp = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        (tmp, Arc::new(ThinkingMapsApi::new(workspace)))
    }

    fn add_node_op(node_id: &str) -> Value {
        json!({
            "op": "add_node",
            "node": {
                "node_id": node_id,
                "kind": "idea",
                "label": format!("label-{node_id}"),
                "epistemic_state": "provisional",
                "assertion_origin": "owner_spoken",
                "confidence": 0.5,
                "created_at": "2026-07-19T00:00:00Z",
                "updated_at": "2026-07-19T00:00:00Z"
            }
        })
    }

    fn scoped_post(uri: &str, body: Value) -> test::TestRequest {
        test::TestRequest::post()
            .uri(uri)
            .insert_header(("X-Principal", "anonymous"))
            .insert_header(("X-Workspace", "default"))
            .set_json(body)
    }

    fn scoped_get(uri: &str) -> test::TestRequest {
        test::TestRequest::get()
            .uri(uri)
            .insert_header(("X-Principal", "anonymous"))
            .insert_header(("X-Workspace", "default"))
    }

    fn scoped_patch(uri: &str, body: Value) -> test::TestRequest {
        test::TestRequest::patch()
            .uri(uri)
            .insert_header(("X-Principal", "anonymous"))
            .insert_header(("X-Workspace", "default"))
            .set_json(body)
    }

    fn scoped_delete(uri: &str) -> test::TestRequest {
        test::TestRequest::delete()
            .uri(uri)
            .insert_header(("X-Principal", "anonymous"))
            .insert_header(("X-Workspace", "default"))
    }

    /// POST a create request and assert 201, returning the created map JSON.
    macro_rules! create_map {
        ($service:expr, $title:expr, $map_id:expr) => {{
            let mut body = json!({ "title": $title });
            let map_id: Option<&str> = $map_id;
            if let Some(id) = map_id {
                body["map_id"] = json!(id);
            }
            let req = scoped_post("/thinking-maps", body).to_request();
            let resp = test::call_service(&$service, req).await;
            assert_eq!(resp.status(), StatusCode::CREATED, "create should be 201");
            let created: Value = test::read_body_json(resp).await;
            created
        }};
    }

    #[actix_web::test]
    async fn create_get_round_trip() {
        let (_tmp, api) = api();
        let service = build_app!(api);

        let created = create_map!(service, "My map", Some("m1"));
        assert_eq!(created["map_id"], "m1");
        assert_eq!(created["title"], "My map");
        assert_eq!(created["revision"], 0);

        let req = scoped_get("/thinking-maps/m1").to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let fetched: Value = test::read_body_json(resp).await;
        assert_eq!(fetched, created);
    }

    #[actix_web::test]
    async fn apply_operations_adds_node() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Ops map", Some("m1"));

        let body = json!({
            "operations": [add_node_op("n1")],
            "idempotency_key": "idem-1",
            "base_revision": 0
        });
        let req = scoped_post("/thinking-maps/m1/operations", body).to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let outcome: Value = test::read_body_json(resp).await;
        assert_eq!(outcome["outcome"], "applied");
        assert_eq!(outcome["resulting_revision"], 1);

        // The returned map + a fresh GET both reflect the node.
        assert!(outcome["map"]["nodes"].get("n1").is_some());
        let req = scoped_get("/thinking-maps/m1").to_request();
        let resp = test::call_service(&service, req).await;
        let map: Value = test::read_body_json(resp).await;
        assert_eq!(map["revision"], 1);
        assert!(map["nodes"].get("n1").is_some());
    }

    #[actix_web::test]
    async fn operations_stamp_owner_actor_not_client_supplied() {
        // Even if the client tries to embed a different actor/map_id in the
        // body, the server forces Owner{principal} + the path map_id. The
        // envelope surfaces through the events log, so we assert on it.
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Owner map", Some("m1"));

        let body = json!({
            "operations": [add_node_op("n1")],
            "idempotency_key": "idem-1",
            "base_revision": 0,
            // Hostile fields that must be ignored:
            "actor": "model",
            "map_id": "some-other-map",
            "principal": "anonymous",
            "workspace": "default"
        });
        let req = scoped_post("/thinking-maps/m1/operations", body).to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let req = scoped_get("/thinking-maps/m1/events?after_seq=0").to_request();
        let resp = test::call_service(&service, req).await;
        let events: Vec<MapEvent> = test::read_body_json(resp).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].envelope.map_id, "m1");
        match &events[0].envelope.actor {
            OperationActor::Owner { principal } => assert_eq!(principal, "anonymous"),
            other => panic!("expected Owner actor, got {other:?}"),
        }
    }

    #[actix_web::test]
    async fn events_and_replay() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Replay map", Some("m1"));

        for (rev, (idem, node_id)) in [("i1", "n1"), ("i2", "n2")].into_iter().enumerate() {
            let body = json!({
                "operations": [add_node_op(node_id)],
                "idempotency_key": idem,
                "base_revision": rev
            });
            let req = scoped_post("/thinking-maps/m1/operations", body).to_request();
            let resp = test::call_service(&service, req).await;
            assert_eq!(resp.status(), StatusCode::OK);
        }

        // events?after_seq=0 returns both events.
        let req = scoped_get("/thinking-maps/m1/events?after_seq=0").to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let events: Vec<MapEvent> = test::read_body_json(resp).await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].sequence, 1);
        assert_eq!(events[1].sequence, 2);

        // replay?at_seq=1 returns the intermediate map (only n1).
        let req = scoped_get("/thinking-maps/m1/replay?at_seq=1").to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let intermediate: Value = test::read_body_json(resp).await;
        assert_eq!(intermediate["revision"], 1);
        assert!(intermediate["nodes"].get("n1").is_some());
        assert!(intermediate["nodes"].get("n2").is_none());
    }

    #[actix_web::test]
    async fn idempotent_replay_on_same_key() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Idem map", Some("m1"));

        let body = json!({
            "operations": [add_node_op("n1")],
            "idempotency_key": "idem-1",
            "base_revision": 0
        });
        let req = scoped_post("/thinking-maps/m1/operations", body.clone()).to_request();
        let resp = test::call_service(&service, req).await;
        let first: Value = test::read_body_json(resp).await;
        assert_eq!(first["outcome"], "applied");

        // Same idempotency key + base_revision ⇒ idempotent_replay.
        let req = scoped_post("/thinking-maps/m1/operations", body).to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let second: Value = test::read_body_json(resp).await;
        assert_eq!(second["outcome"], "idempotent_replay");
    }

    #[actix_web::test]
    async fn validation_error_is_bad_request() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Bad map", Some("m1"));

        // set_epistemic_state on an unknown node ⇒ reducer UnknownNode.
        let body = json!({
            "operations": [{
                "op": "set_epistemic_state",
                "node_id": "ghost",
                "state": "asserted"
            }],
            "idempotency_key": "bad",
            "base_revision": 0
        });
        let req = scoped_post("/thinking-maps/m1/operations", body).to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let err: Value = test::read_body_json(resp).await;
        assert_eq!(err["error"], "validation_failed");
    }

    #[actix_web::test]
    async fn stale_base_revision_is_revision_conflict_409() {
        // A stale `base_revision` must surface as a distinct 409 `revision_conflict`
        // (NOT a 400 `validation_failed`) so offline clients can rebase + retry.
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Conflict map", Some("m1"));

        // First op at base_revision 0 succeeds → head is now revision 1.
        let body = json!({
            "operations": [add_node_op("n1")],
            "idempotency_key": "idem-1",
            "base_revision": 0
        });
        let req = scoped_post("/thinking-maps/m1/operations", body).to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let outcome: Value = test::read_body_json(resp).await;
        assert_eq!(outcome["outcome"], "applied");
        assert_eq!(outcome["resulting_revision"], 1);

        // A SECOND op with the now-stale base_revision 0 (fresh idempotency key)
        // ⇒ 409 revision_conflict, expected=1 (head), actual=0 (submitted).
        let body = json!({
            "operations": [add_node_op("n2")],
            "idempotency_key": "idem-2",
            "base_revision": 0
        });
        let req = scoped_post("/thinking-maps/m1/operations", body).to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let err: Value = test::read_body_json(resp).await;
        assert_eq!(err["error"], "revision_conflict");
        assert_eq!(err["expected"], 1);
        assert_eq!(err["actual"], 0);
    }

    #[actix_web::test]
    async fn list_excludes_deleted_maps_but_direct_get_keeps_tombstone_available() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Alpha", Some("alpha"));
        create_map!(service, "Bravo", Some("bravo"));
        create_map!(service, "Deleted", Some("deleted"));

        let req =
            scoped_patch("/thinking-maps/deleted", json!({ "lifecycle": "deleted" })).to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let req = scoped_get("/thinking-maps").to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let summaries: Vec<MapSummary> = test::read_body_json(resp).await;
        assert_eq!(summaries.len(), 2);
        let ids: Vec<&str> = summaries.iter().map(|s| s.map_id.as_str()).collect();
        assert!(ids.contains(&"alpha"));
        assert!(ids.contains(&"bravo"));
        assert!(!ids.contains(&"deleted"));

        let req = scoped_get("/thinking-maps/deleted").to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let tombstone: Value = test::read_body_json(resp).await;
        assert_eq!(tombstone["lifecycle"], "deleted");

        let req = scoped_get("/thinking-maps?lifecycle=deleted&limit=25").to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let deleted_page: Value = test::read_body_json(resp).await;
        assert_eq!(deleted_page["total"], 1);
        assert_eq!(deleted_page["maps"][0]["map_id"], "deleted");
    }

    #[actix_web::test]
    async fn list_paginates_visible_maps_and_excludes_deleted_before_counting() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Alpha", Some("alpha"));
        create_map!(service, "Bravo", Some("bravo"));
        create_map!(service, "Charlie", Some("charlie"));
        create_map!(service, "Deleted", Some("deleted"));
        let req =
            scoped_patch("/thinking-maps/deleted", json!({ "lifecycle": "deleted" })).to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);

        // Page 1: envelope shape, window size, and visible total. The deleted
        // map was updated most recently, so this also proves filtering occurs
        // before the offset/limit window is sliced.
        let req = scoped_get("/thinking-maps?limit=2").to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let page1: Value = test::read_body_json(resp).await;
        assert_eq!(page1["total"], 3);
        assert_eq!(page1["offset"], 0);
        assert_eq!(page1["limit"], 2);
        assert_eq!(page1["maps"].as_array().unwrap().len(), 2);

        // Page 2 continues where page 1 ended; pages are disjoint and cover all.
        let req = scoped_get("/thinking-maps?limit=2&offset=2").to_request();
        let resp = test::call_service(&service, req).await;
        let page2: Value = test::read_body_json(resp).await;
        assert_eq!(page2["maps"].as_array().unwrap().len(), 1);
        let mut ids: Vec<String> = page1["maps"]
            .as_array()
            .unwrap()
            .iter()
            .chain(page2["maps"].as_array().unwrap())
            .map(|m| m["map_id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        assert_eq!(ids, ["alpha", "bravo", "charlie"]);

        // Past-the-end offset → empty page, same total.
        let req = scoped_get("/thinking-maps?limit=2&offset=9").to_request();
        let resp = test::call_service(&service, req).await;
        let past: Value = test::read_body_json(resp).await;
        assert_eq!(past["maps"].as_array().unwrap().len(), 0);
        assert_eq!(past["total"], 3);

        // limit=0 clamps to 1 rather than erroring.
        let req = scoped_get("/thinking-maps?limit=0").to_request();
        let resp = test::call_service(&service, req).await;
        let clamped: Value = test::read_body_json(resp).await;
        assert_eq!(clamped["limit"], 1);
        assert_eq!(clamped["maps"].as_array().unwrap().len(), 1);

        // No limit → legacy bare array (back-compat for existing clients).
        let req = scoped_get("/thinking-maps").to_request();
        let resp = test::call_service(&service, req).await;
        let legacy: Vec<MapSummary> = test::read_body_json(resp).await;
        assert_eq!(legacy.len(), 3);
    }

    #[actix_web::test]
    async fn restore_creates_branch_leaving_source_unchanged() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Source", Some("m1"));
        let body = json!({
            "operations": [add_node_op("n1")],
            "idempotency_key": "i1",
            "base_revision": 0
        });
        let req = scoped_post("/thinking-maps/m1/operations", body).to_request();
        test::call_service(&service, req).await;

        // Fetch source before restore for later comparison.
        let req = scoped_get("/thinking-maps/m1").to_request();
        let resp = test::call_service(&service, req).await;
        let source_before: Value = test::read_body_json(resp).await;

        let restore_body = json!({
            "at_sequence": 1,
            "new_map_id": "branch1",
            "new_title": "Forked"
        });
        let req = scoped_post("/thinking-maps/m1/restore", restore_body).to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let branch: Value = test::read_body_json(resp).await;
        assert_eq!(branch["map_id"], "branch1");
        assert_eq!(branch["title"], "Forked");
        assert!(branch["nodes"].get("n1").is_some());

        // Source unchanged.
        let req = scoped_get("/thinking-maps/m1").to_request();
        let resp = test::call_service(&service, req).await;
        let source_after: Value = test::read_body_json(resp).await;
        assert_eq!(source_before, source_after);
    }

    // ── PATCH /{map_id} (owner metadata) ────────────────────────────────────────

    #[actix_web::test]
    async fn patch_title_updates_map() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Old title", Some("m1"));

        let req = scoped_patch("/thinking-maps/m1", json!({ "title": "New title" })).to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let outcome: Value = test::read_body_json(resp).await;
        assert_eq!(outcome["outcome"], "applied");
        assert_eq!(outcome["resulting_revision"], 1);
        assert_eq!(outcome["map"]["title"], "New title");

        // A fresh GET reflects the rename.
        let req = scoped_get("/thinking-maps/m1").to_request();
        let resp = test::call_service(&service, req).await;
        let map: Value = test::read_body_json(resp).await;
        assert_eq!(map["title"], "New title");
        assert_eq!(map["revision"], 1);
    }

    #[actix_web::test]
    async fn patch_lifecycle_archives_map() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Live map", Some("m1"));

        let req =
            scoped_patch("/thinking-maps/m1", json!({ "lifecycle": "archived" })).to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let outcome: Value = test::read_body_json(resp).await;
        assert_eq!(outcome["outcome"], "applied");
        assert_eq!(outcome["map"]["lifecycle"], "archived");

        let req = scoped_get("/thinking-maps/m1").to_request();
        let resp = test::call_service(&service, req).await;
        let map: Value = test::read_body_json(resp).await;
        assert_eq!(map["lifecycle"], "archived");
    }

    #[actix_web::test]
    async fn patch_with_neither_is_bad_request() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Map", Some("m1"));

        let req = scoped_patch("/thinking-maps/m1", json!({})).to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let err: Value = test::read_body_json(resp).await;
        assert_eq!(err["error"], "nothing_to_patch");
    }

    #[actix_web::test]
    async fn patch_unknown_map_is_404() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        let req = scoped_patch("/thinking-maps/ghost", json!({ "title": "x" })).to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let err: Value = test::read_body_json(resp).await;
        assert_eq!(err["error"], "not_found");
        assert_eq!(err["map_id"], "ghost");
    }

    #[actix_web::test]
    async fn permanent_delete_requires_confirmation_and_deleted_lifecycle_then_purges_map() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Map", Some("purge-map"));

        let req = scoped_delete("/thinking-maps/purge-map").to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let error: Value = test::read_body_json(resp).await;
        assert_eq!(error["error"], "permanent_delete_confirmation_required");

        let req = scoped_delete("/thinking-maps/purge-map?confirm=permanent").to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let error: Value = test::read_body_json(resp).await;
        assert_eq!(error["error"], "purge_requires_deleted");

        let req = scoped_patch(
            "/thinking-maps/purge-map",
            json!({ "lifecycle": "deleted" }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let req = scoped_delete("/thinking-maps/purge-map?confirm=permanent").to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let deleted: Value = test::read_body_json(resp).await;
        assert_eq!(deleted["deleted"], true);
        assert_eq!(deleted["map_id"], "purge-map");

        let req = scoped_get("/thinking-maps/purge-map").to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let req = scoped_get("/thinking-maps?lifecycle=deleted&limit=25").to_request();
        let resp = test::call_service(&service, req).await;
        let page: Value = test::read_body_json(resp).await;
        assert_eq!(page["total"], 0);
        assert!(page["maps"].as_array().unwrap().is_empty());

        let req = scoped_delete("/thinking-maps/purge-map?confirm=permanent").to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn missing_scope_is_bad_request() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        // No X-Principal/X-Workspace headers, no body scope.
        let req = test::TestRequest::post()
            .uri("/thinking-maps")
            .set_json(json!({ "title": "no scope" }))
            .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let err: Value = test::read_body_json(resp).await;
        assert_eq!(err["error"], "missing_scope");
    }

    // ── /interpret ────────────────────────────────────────────────────────────

    #[actix_web::test]
    async fn interpret_missing_scope_is_bad_request() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        // No X-Principal/X-Workspace headers, no body scope.
        let req = test::TestRequest::post()
            .uri("/thinking-maps/m1/interpret")
            .set_json(json!({ "text": "ship v1" }))
            .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let err: Value = test::read_body_json(resp).await;
        assert_eq!(err["error"], "missing_scope");
    }

    #[actix_web::test]
    async fn interpret_map_not_found_is_404() {
        // Enabled + valid scope, but the map does not exist. The 404 is returned
        // BEFORE the LLM router is consulted, so this is deterministic even with
        // no global router set in the test harness.
        let (_tmp, api) = api();
        let service = build_app!(api);
        let req = scoped_post(
            "/thinking-maps/ghost/interpret",
            json!({ "text": "ship v1" }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let err: Value = test::read_body_json(resp).await;
        assert_eq!(err["error"], "not_found");
        assert_eq!(err["map_id"], "ghost");
    }

    /// The progress sink emits real, correctly-scoped transport events — and
    /// nothing at all when no broadcaster is registered (minimal test apps).
    #[actix_web::test]
    async fn interpret_progress_sink_emits_scoped_stage_events() {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut rx = broadcaster.subscribe();
        let data = web::Data::new(broadcaster);

        let sink =
            TransportInterpretProgress::new(Some(&data), "map-7", "alice", "default", "utt-42");
        sink.stage(InterpretStage::Preparing, Some(34));
        sink.idle();

        // First event: preparing, with the count.
        match rx.try_recv().expect("a stage event") {
            RuntimeTransportEvent::ThinkingMapInterpretProgress {
                map_id,
                principal,
                workspace,
                utterance_id,
                stage,
                detail,
                node_count,
                ..
            } => {
                assert_eq!(map_id, "map-7");
                assert_eq!(principal, "alice");
                assert_eq!(workspace, "default");
                assert_eq!(utterance_id, "utt-42");
                assert_eq!(stage, "preparing");
                assert_eq!(detail, None);
                assert_eq!(node_count, Some(34));
            },
            other => panic!("expected ThinkingMapInterpretProgress, got {other:?}"),
        }
        // Second: idle, no count — the terminal stage carries no number.
        match rx.try_recv().expect("the idle event") {
            RuntimeTransportEvent::ThinkingMapInterpretProgress {
                stage, node_count, ..
            } => {
                assert_eq!(stage, "idle");
                assert_eq!(node_count, None);
            },
            other => panic!("expected ThinkingMapInterpretProgress, got {other:?}"),
        }

        // No broadcaster registered ⇒ silently nothing, never an error.
        let silent = TransportInterpretProgress::new(None, "m", "p", "w", "u");
        silent.stage(InterpretStage::Facilitating, None);
        silent.idle();
    }

    /// The progress event serializes to the wire shape the plan specifies —
    /// `event_type` + `data`, snake_case stage, optional fields absent when
    /// unset. Three clients parse this by hand; the shape is the contract.
    /// (`actix_web::test` rather than the built-in `#[test]`: this module
    /// imports `actix_web::test` as a name, which shadows the attribute.)
    #[actix_web::test]
    async fn interpret_progress_wire_shape() {
        let event = RuntimeTransportEvent::ThinkingMapInterpretProgress {
            map_id: "m1".into(),
            principal: "p".into(),
            workspace: "w".into(),
            utterance_id: "u1".into(),
            stage: InterpretStage::LoadingContext.wire_name().into(),
            detail: None,
            node_count: None,
            timestamp: 1_700_000_000_000,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["event_type"], "ThinkingMapInterpretProgress");
        assert_eq!(value["data"]["stage"], "loading_context");
        assert_eq!(value["data"]["utterance_id"], "u1");
        assert!(
            value["data"].get("detail").is_none(),
            "unset detail must be absent from the wire, not null"
        );
        assert!(value["data"].get("node_count").is_none());
    }

    #[actix_web::test]
    async fn interpret_accepts_break_open_intent() {
        // A body carrying `"intent":"break_open"` must parse cleanly. We exercise
        // the map-not-found path (map absent, so the 404 is returned BEFORE the
        // LLM router is consulted) to assert the intent field doesn't break
        // deserialization — the request still reaches the handler and 404s.
        let (_tmp, api) = api();
        let service = build_app!(api);
        let req = scoped_post(
            "/thinking-maps/ghost/interpret",
            json!({ "text": "ship v1", "intent": "break_open" }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let err: Value = test::read_body_json(resp).await;
        assert_eq!(err["error"], "not_found");
        assert_eq!(err["map_id"], "ghost");
    }

    #[actix_web::test]
    async fn interpret_focus_override_selects_live_node_and_rejects_missing() {
        let (_tmp, api) = api();
        let service = build_app!(api.clone());
        create_map!(service, "Focused map", Some("m1"));

        let req = scoped_post(
            "/thinking-maps/m1/operations",
            json!({
                "operations": [
                    add_node_op("n1"),
                    add_node_op("12620746-a25c-4e29-80ca-e112c8d32aa7")
                ],
                "idempotency_key": "focus-node-seed",
                "base_revision": 0
            }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let mut map = api
            .store()
            .load_map("anonymous", "default", "m1")
            .await
            .expect("load focused map")
            .expect("focused map exists");
        apply_interpret_focus(&mut map, Some("n1")).expect("live focus accepted");
        assert_eq!(map.view_state.active_node.as_deref(), Some("n1"));

        apply_interpret_focus(&mut map, Some("12620746-A25C-4E29-80CA-E112C8D32AA7"))
            .expect("equivalent UUID casing accepted");
        assert_eq!(
            map.view_state.active_node.as_deref(),
            Some("12620746-a25c-4e29-80ca-e112c8d32aa7")
        );

        let missing = apply_interpret_focus(&mut map, Some("missing"));
        assert_eq!(missing, Err("missing".to_string()));
        assert_eq!(
            map.view_state.active_node.as_deref(),
            Some("12620746-a25c-4e29-80ca-e112c8d32aa7")
        );
    }

    #[actix_web::test]
    async fn parse_intent_maps_values() {
        assert_eq!(parse_intent(Some("break_open")), InterpretIntent::BreakOpen);
        assert_eq!(
            parse_intent(Some("continue_thinking")),
            InterpretIntent::ContinueThinking
        );
        // Unknown / absent ⇒ ContinueThinking.
        assert_eq!(
            parse_intent(Some("nonsense")),
            InterpretIntent::ContinueThinking
        );
        assert_eq!(parse_intent(None), InterpretIntent::ContinueThinking);
    }

    // ── /consolidate ────────────────────────────────────────────────────────────

    #[actix_web::test]
    async fn consolidate_missing_scope_is_bad_request() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        // No X-Principal/X-Workspace headers, no body scope.
        let req = test::TestRequest::post()
            .uri("/thinking-maps/m1/consolidate")
            .set_json(json!({}))
            .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let err: Value = test::read_body_json(resp).await;
        assert_eq!(err["error"], "missing_scope");
    }

    #[actix_web::test]
    async fn consolidate_map_not_found_is_404() {
        // Valid scope but the map does not exist. The 404 is returned BEFORE the
        // LLM router is consulted, so this is deterministic with no global router
        // set in the test harness.
        let (_tmp, api) = api();
        let service = build_app!(api);
        let req = scoped_post("/thinking-maps/ghost/consolidate", json!({})).to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let err: Value = test::read_body_json(resp).await;
        assert_eq!(err["error"], "not_found");
        assert_eq!(err["map_id"], "ghost");
    }

    #[actix_web::test]
    async fn consolidate_without_router_is_503() {
        // An existing map + valid scope, but no global operation router is set in
        // the test harness ⇒ 503 llm_unavailable (the live LLM path can't be
        // unit-tested; this asserts the graceful fallback).
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Consolidate map", Some("m1"));

        let req = scoped_post("/thinking-maps/m1/consolidate", json!({})).to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let err: Value = test::read_body_json(resp).await;
        assert_eq!(err["error"], "llm_unavailable");
    }

    // ── /proposals/{proposal_id}/decision ───────────────────────────────────────

    /// Stage a pending proposal on `m1` by applying an owner ProposeRestructure
    /// (with a trivial inner add_node) via the /operations endpoint.
    macro_rules! stage_pending_proposal {
        ($service:expr, $proposal_id:expr) => {{
            let body = json!({
                "operations": [{
                    "op": "propose_restructure",
                    "proposal": {
                        "proposal_id": $proposal_id,
                        "rationale": "stage a node",
                        "operations": [add_node_op("inner")],
                        "state": "proposed",
                        "affected_node_ids": [],
                        "created_at": "2026-07-19T00:00:00Z"
                    }
                }],
                "idempotency_key": "stage-1",
                "base_revision": 0
            });
            let req = scoped_post("/thinking-maps/m1/operations", body).to_request();
            let resp = test::call_service(&$service, req).await;
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "proposal staging should apply"
            );
        }};
    }

    #[actix_web::test]
    async fn decision_confirm_materializes_inner_ops() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Decision map", Some("m1"));
        stage_pending_proposal!(service, "p1");

        // The proposal is pending; its inner node is NOT yet materialized.
        let req = scoped_get("/thinking-maps/m1").to_request();
        let resp = test::call_service(&service, req).await;
        let map: Value = test::read_body_json(resp).await;
        assert!(map["proposals"].get("p1").is_some());
        assert_eq!(map["proposals"]["p1"]["state"], "proposed");
        assert!(map["nodes"].get("inner").is_none());

        // Owner confirms → 200 applied, proposal Confirmed, inner node materialized.
        let req = scoped_post(
            "/thinking-maps/m1/proposals/p1/decision",
            json!({ "decision": "confirm" }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let outcome: Value = test::read_body_json(resp).await;
        assert_eq!(outcome["outcome"], "applied");
        assert_eq!(outcome["map"]["proposals"]["p1"]["state"], "confirmed");
        assert!(outcome["map"]["nodes"].get("inner").is_some());
    }

    #[actix_web::test]
    async fn decision_reject_marks_proposal_rejected() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Reject map", Some("m1"));
        stage_pending_proposal!(service, "p1");

        let req = scoped_post(
            "/thinking-maps/m1/proposals/p1/decision",
            json!({ "decision": "reject" }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let outcome: Value = test::read_body_json(resp).await;
        assert_eq!(outcome["outcome"], "applied");
        assert_eq!(outcome["map"]["proposals"]["p1"]["state"], "rejected");
        // The inner node was NOT materialized on reject.
        assert!(outcome["map"]["nodes"].get("inner").is_none());
    }

    #[actix_web::test]
    async fn decision_invalid_string_is_bad_request() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Bad decision map", Some("m1"));
        stage_pending_proposal!(service, "p1");

        let req = scoped_post(
            "/thinking-maps/m1/proposals/p1/decision",
            json!({ "decision": "maybe" }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let err: Value = test::read_body_json(resp).await;
        assert_eq!(err["error"], "invalid_decision");
    }

    #[actix_web::test]
    async fn decision_unknown_proposal_is_validation_failed_400() {
        // An unknown proposal_id ⇒ the reducer returns UnknownProposal, which maps
        // to a 400 validation_failed (acceptable — a decision on a nonexistent
        // proposal is a client error).
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "No proposal map", Some("m1"));

        let req = scoped_post(
            "/thinking-maps/m1/proposals/ghost/decision",
            json!({ "decision": "confirm" }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let err: Value = test::read_body_json(resp).await;
        assert_eq!(err["error"], "validation_failed");
    }

    // ── Governed promotion ─────────────────────────────────────────────────────

    /// Seed one node via /operations (expands inline to dodge the actix
    /// Service generics); yields the new node id.
    macro_rules! seed_node {
        ($service:expr, $map:expr, $label:expr, $origin:expr, $state:expr) => {
            seed_node!($service, $map, $label, $origin, $state, 0)
        };
        ($service:expr, $map:expr, $label:expr, $origin:expr, $state:expr, $rev:expr) => {{
            let node_id = uuid::Uuid::new_v4().to_string();
            let req = scoped_post(
                &format!("/thinking-maps/{}/operations", $map),
                json!({
                    "operations": [{"op": "add_node", "node": {
                        "node_id": node_id, "kind": "action", "label": $label,
                        "epistemic_state": $state, "assertion_origin": $origin,
                        "confidence": 0.9,
                        "created_at": "2026-07-21T00:00:00Z",
                        "updated_at": "2026-07-21T00:00:00Z"
                    }}],
                    "idempotency_key": format!("seed-{node_id}"),
                    "base_revision": $rev
                }),
            )
            .to_request();
            let resp = test::call_service(&$service, req).await;
            assert_eq!(resp.status(), StatusCode::OK, "seed node");
            node_id
        }};
    }

    #[actix_web::test]
    async fn promote_owner_node_to_memory_links_and_is_idempotent() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Promo map", Some("m1"));
        let node = seed_node!(
            service,
            "m1",
            "Ship pricing page",
            "owner_spoken",
            "asserted"
        );

        let req = scoped_post(
            &format!("/thinking-maps/m1/nodes/{node}/promote"),
            json!({ "target": "memory" }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["promoted"], true);
        assert_eq!(body["object_kind"], "memory");
        let object_id = body["object_id"].as_str().unwrap().to_string();
        assert!(!object_id.is_empty());

        // Retry = idempotent: same object, no duplicate candidate.
        let req = scoped_post(
            &format!("/thinking-maps/m1/nodes/{node}/promote"),
            json!({ "target": "memory" }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["promoted"], false);
        assert_eq!(body["object_id"], object_id.as_str());
    }

    #[actix_web::test]
    async fn promote_model_inferred_requires_confirmation_then_asserts() {
        let (_tmp, api) = api();
        let store_api = Arc::clone(&api);
        let service = build_app!(api);
        create_map!(service, "Promo map", Some("m1"));
        // A model-authored node must be seeded the way the interpreter does it
        // (Model-actor envelope via the store) — the owner /operations surface
        // correctly refuses forged model_inferred origins.
        let node = uuid::Uuid::new_v4().to_string();
        {
            use magician_surfaces::thinking_map::{
                AssertionOrigin, EpistemicState, NodeKind, ThinkingNode,
            };
            let n = ThinkingNode {
                node_id: node.clone(),
                kind: NodeKind::Idea,
                label: "Model idea".to_string(),
                detail_markdown: None,
                epistemic_state: EpistemicState::Provisional,
                assertion_origin: AssertionOrigin::ModelInferred,
                confidence: 0.6,
                speaker: None,
                source_refs: vec![],
                parent_id: None,
                position: None,
                position_locked: false,
                promoted_refs: vec![],
                tombstoned: false,
                created_at: "2026-07-21T00:00:00Z".to_string(),
                updated_at: "2026-07-21T00:00:00Z".to_string(),
            };
            let mut envelope = MapOperationEnvelope::new(
                uuid::Uuid::new_v4().to_string(),
                "m1".to_string(),
                0,
                OperationActor::Model {
                    trace_id: Some("test".to_string()),
                },
                "seed-model".to_string(),
                vec![MapOperation::AddNode { node: n }],
                "2026-07-21T00:00:00Z".to_string(),
            );
            envelope.schema_version = THINKING_MAP_SCHEMA_VERSION;
            store_api
                .store()
                .apply_and_persist(
                    "anonymous",
                    "default",
                    "m1",
                    &envelope,
                    "2026-07-21T00:00:00Z",
                )
                .await
                .expect("seed model node");
        }

        // Without confirm ⇒ 409 confirmation_required, nothing created.
        let req = scoped_post(
            &format!("/thinking-maps/m1/nodes/{node}/promote"),
            json!({ "target": "memory" }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);

        // With confirm ⇒ promoted AND the node becomes owner-asserted.
        let req = scoped_post(
            &format!("/thinking-maps/m1/nodes/{node}/promote"),
            json!({ "target": "memory", "confirm": true }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let get = scoped_get("/thinking-maps/m1").to_request();
        let map: Value = test::read_body_json(test::call_service(&service, get).await).await;
        assert_eq!(map["nodes"][&node]["epistemic_state"], "asserted");
        assert_eq!(
            map["nodes"][&node]["promoted_refs"][0]["destination_kind"],
            "memory"
        );
    }

    #[actix_web::test]
    async fn promote_rejected_node_is_refused_and_task_needs_service() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Promo map", Some("m1"));
        let node = seed_node!(service, "m1", "Bad idea", "owner_spoken", "asserted");
        // Owner rejects it after the fact (direct creation-as-rejected is not a
        // valid owner op — the state flip is the real path).
        let req = scoped_post(
            "/thinking-maps/m1/operations",
            json!({
                "operations": [{"op": "set_epistemic_state", "node_id": node, "state": "rejected"}],
                "idempotency_key": "reject-1", "base_revision": 1
            }),
        )
        .to_request();
        assert_eq!(
            test::call_service(&service, req).await.status(),
            StatusCode::OK
        );

        // Rejected/superseded history never promotes — even with confirm.
        let req = scoped_post(
            &format!("/thinking-maps/m1/nodes/{node}/promote"),
            json!({ "target": "memory", "confirm": true }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["error"], "not_promotable");

        // Task target without the v3 task service app_data ⇒ 503 (feature
        // degrades explicitly, never half-creates).
        let ok = seed_node!(service, "m1", "Real action", "owner_spoken", "asserted", 2);
        let req = scoped_post(
            &format!("/thinking-maps/m1/nodes/{ok}/promote"),
            json!({ "target": "task" }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── Ambient session attach/detach ─────────────────────────────────────────

    /// Build a test app + a coordinator (registered as app_data) over the SAME
    /// tempdir workspace as the api, so an attach's map-existence check and the
    /// coordinator's resulting registry state agree.
    fn api_with_coordinator() -> (
        TempDir,
        Arc<ThinkingMapsApi>,
        Arc<ThinkingMapSessionCoordinator>,
    ) {
        let tmp = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let api = Arc::new(ThinkingMapsApi::new(workspace.clone()));
        let broadcaster =
            Arc::new(magician::magician_v2::realtime_events::RuntimeTransportBroadcaster::new(64));
        let coordinator = ThinkingMapSessionCoordinator::new(broadcaster, workspace);
        (tmp, api, coordinator)
    }

    macro_rules! build_app_with_coordinator {
        ($api:expr, $coordinator:expr) => {
            test::init_service(
                App::new()
                    .app_data(web::Data::new($api))
                    .app_data(web::Data::new($coordinator))
                    .configure(configure),
            )
            .await
        };
    }

    #[actix_web::test]
    async fn attach_session_registers_and_detach_removes() {
        let (_tmp, api, coordinator) = api_with_coordinator();
        let service = build_app_with_coordinator!(api, Arc::clone(&coordinator));
        create_map!(service, "Live map", Some("m1"));

        // Attach a source session → 200 { attached: true }.
        let req = scoped_post(
            "/thinking-maps/m1/sessions",
            json!({ "source_session_id": "sess-1" }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["attached"], true);
        assert_eq!(body["source_session_id"], "sess-1");

        // The coordinator now carries the binding (same Arc we handed the app).
        let binding = coordinator.bound_map("sess-1").expect("bound");
        assert_eq!(binding.map_id, "m1");
        assert_eq!(binding.principal, "anonymous");
        assert_eq!(binding.workspace, "default");

        // Detach → 200 { detached: true }, then the binding is gone.
        let req = test::TestRequest::delete()
            .uri("/thinking-maps/m1/sessions/sess-1")
            .insert_header(("X-Principal", "anonymous"))
            .insert_header(("X-Workspace", "default"))
            .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["detached"], true);
        assert!(coordinator.bound_map("sess-1").is_none());

        // Second detach is idempotent → detached: false.
        let req = test::TestRequest::delete()
            .uri("/thinking-maps/m1/sessions/sess-1")
            .insert_header(("X-Principal", "anonymous"))
            .insert_header(("X-Workspace", "default"))
            .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["detached"], false);
    }

    #[actix_web::test]
    async fn attach_to_missing_map_is_404() {
        let (_tmp, api, coordinator) = api_with_coordinator();
        let service = build_app_with_coordinator!(api, Arc::clone(&coordinator));
        // No map created.
        let req = scoped_post(
            "/thinking-maps/ghost/sessions",
            json!({ "source_session_id": "sess-1" }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        // Nothing registered on a failed attach.
        assert!(coordinator.bound_map("sess-1").is_none());
    }

    #[actix_web::test]
    async fn attach_missing_scope_is_bad_request() {
        let (_tmp, api, coordinator) = api_with_coordinator();
        let service = build_app_with_coordinator!(api, coordinator);
        // No X-Principal/X-Workspace headers and no scope in body.
        let req = test::TestRequest::post()
            .uri("/thinking-maps/m1/sessions")
            .set_json(json!({ "source_session_id": "sess-1" }))
            .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn attach_without_coordinator_is_503() {
        // App built WITHOUT the coordinator app_data (via the plain build_app!).
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Live map", Some("m1"));
        let req = scoped_post(
            "/thinking-maps/m1/sessions",
            json!({ "source_session_id": "sess-1" }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── Tutor adapter (map→tutor grounding context) ────────────────────────────

    #[actix_web::test]
    async fn tutor_context_register_builds_annotated_digest_and_binds_registry() {
        let (_tmp, api) = api();
        let store_api = Arc::clone(&api);
        let service = build_app!(api);
        create_map!(service, "Tutor map", Some("m1"));
        let owner_node = seed_node!(
            service,
            "m1",
            "Ship pricing page",
            "owner_spoken",
            "asserted"
        );
        // Model-authored node (Model-actor envelope, like the interpreter).
        {
            use magician_surfaces::thinking_map::NodeKind;
            let n = ThinkingNode {
                node_id: "model-node".to_string(),
                kind: NodeKind::Risk,
                label: "Churn risk from pricing".to_string(),
                detail_markdown: None,
                epistemic_state: EpistemicState::Provisional,
                assertion_origin: AssertionOrigin::ModelInferred,
                confidence: 0.6,
                speaker: None,
                source_refs: vec![],
                parent_id: None,
                position: None,
                position_locked: false,
                promoted_refs: vec![],
                tombstoned: false,
                created_at: "2026-07-22T00:00:00Z".to_string(),
                updated_at: "2026-07-22T00:00:00Z".to_string(),
            };
            let mut envelope = MapOperationEnvelope::new(
                uuid::Uuid::new_v4().to_string(),
                "m1".to_string(),
                1,
                OperationActor::Model {
                    trace_id: Some("test".to_string()),
                },
                "seed-model-tutor".to_string(),
                vec![MapOperation::AddNode { node: n }],
                "2026-07-22T00:00:00Z".to_string(),
            );
            envelope.schema_version = THINKING_MAP_SCHEMA_VERSION;
            store_api
                .store()
                .apply_and_persist(
                    "anonymous",
                    "default",
                    "m1",
                    &envelope,
                    "2026-07-22T00:00:00Z",
                )
                .await
                .expect("seed model node");
        }

        // Unique chat session id — the registry is process-global.
        let session_id = format!("tutor-ctx-{}", uuid::Uuid::new_v4().simple());
        let req = scoped_post(
            "/thinking-maps/m1/tutor-context",
            json!({ "session_id": session_id, "node_id": owner_node }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["registered"], true);
        assert_eq!(body["map_id"], "m1");
        assert_eq!(body["session_id"], session_id.as_str());
        let context = body["context"].as_str().expect("digest");
        assert!(context.contains("Ship pricing page"));
        assert!(context.contains("Selected node"));
        // The digest exposes origin: no [AI-suggested] on the owner node line;
        // (the model node is outside the selected node's neighborhood here, so
        // the whole-map registration below covers the annotation).
        assert!(!context.contains("\"Ship pricing page\" [AI-suggested"));

        // Registry now carries the SNAPSHOT binding for that scope.
        let binding = tutor_map_context_registry()
            .current("anonymous", "default", &session_id)
            .expect("binding");
        assert_eq!(binding.map_id, "m1");
        assert_eq!(binding.node_id.as_deref(), Some(owner_node.as_str()));

        // Re-register WITHOUT a node → whole-map overview, overwriting; the
        // model-inferred node must be attributed.
        let req = scoped_post(
            "/thinking-maps/m1/tutor-context",
            json!({ "session_id": session_id }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = test::read_body_json(resp).await;
        let context = body["context"].as_str().expect("digest");
        assert!(context.contains("\"Churn risk from pricing\" [AI-suggested, provisional]"));
        let binding = tutor_map_context_registry()
            .current("anonymous", "default", &session_id)
            .expect("binding");
        assert_eq!(binding.node_id, None);

        // DELETE clears; second DELETE is idempotent.
        let req = test::TestRequest::delete()
            .uri(&format!("/thinking-maps/m1/tutor-context/{session_id}"))
            .insert_header(("X-Principal", "anonymous"))
            .insert_header(("X-Workspace", "default"))
            .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["cleared"], true);
        assert!(tutor_map_context_registry()
            .current("anonymous", "default", &session_id)
            .is_none());
        let req = test::TestRequest::delete()
            .uri(&format!("/thinking-maps/m1/tutor-context/{session_id}"))
            .insert_header(("X-Principal", "anonymous"))
            .insert_header(("X-Workspace", "default"))
            .to_request();
        let resp = test::call_service(&service, req).await;
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["cleared"], false);
    }

    #[actix_web::test]
    async fn tutor_context_missing_map_is_404_and_unknown_node_is_404() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        let session_id = format!("tutor-ctx-{}", uuid::Uuid::new_v4().simple());

        // Unknown map.
        let req = scoped_post(
            "/thinking-maps/ghost/tutor-context",
            json!({ "session_id": session_id }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // Known map, unknown node.
        create_map!(service, "Tutor map", Some("m1"));
        let req = scoped_post(
            "/thinking-maps/m1/tutor-context",
            json!({ "session_id": session_id, "node_id": "ghost-node" }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["error"], "node_not_found");

        // Neither failure bound anything.
        assert!(tutor_map_context_registry()
            .current("anonymous", "default", &session_id)
            .is_none());

        // Empty session id is a 400.
        let req = scoped_post(
            "/thinking-maps/m1/tutor-context",
            json!({ "session_id": "  " }),
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // ── Markdown export ────────────────────────────────────────────────────────

    #[actix_web::test]
    async fn export_markdown_renders_and_respects_inclusion_toggles() {
        let (_tmp, api) = api();
        let service = build_app!(api);
        create_map!(service, "Export map", Some("m1"));
        // add_node_op seeds n1 as `provisional` — included by the default
        // export, excluded when include_provisional=false.
        let body = json!({
            "operations": [add_node_op("n1")],
            "idempotency_key": "exp-1",
            "base_revision": 0
        });
        let req = scoped_post("/thinking-maps/m1/operations", body).to_request();
        assert_eq!(
            test::call_service(&service, req).await.status(),
            StatusCode::OK
        );

        // Default export: text/markdown, title header, tagged node bullet.
        let req = scoped_get("/thinking-maps/m1/export/markdown").to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let content_type = resp
            .headers()
            .get(actix_web::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(
            content_type.starts_with("text/markdown"),
            "content-type was {content_type}"
        );
        let body = String::from_utf8(test::read_body(resp).await.to_vec()).expect("utf8 body");
        assert!(body.starts_with("# Export map\n"), "body was:\n{body}");
        assert!(body.contains("- **label-n1** `idea`"), "body was:\n{body}");
        assert!(body.contains("_provisional_"), "body was:\n{body}");

        // include_provisional=false drops the provisional node entirely.
        let req =
            scoped_get("/thinking-maps/m1/export/markdown?include_provisional=false").to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = String::from_utf8(test::read_body(resp).await.to_vec()).expect("utf8 body");
        assert!(!body.contains("label-n1"), "body was:\n{body}");
        assert!(body.contains("_No visible nodes._"), "body was:\n{body}");

        // The documented empty-value form falls back to the defaults instead
        // of failing bool deserialization with a 400.
        let req = scoped_get(
            "/thinking-maps/m1/export/markdown?include_provisional=&include_superseded=",
        )
        .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = String::from_utf8(test::read_body(resp).await.to_vec()).expect("utf8 body");
        assert!(body.contains("label-n1"), "body was:\n{body}");
    }

    #[actix_web::test]
    async fn export_markdown_unknown_map_is_404_and_missing_scope_is_400() {
        let (_tmp, api) = api();
        let service = build_app!(api);

        // Valid scope, unknown map ⇒ 404 not_found (mirrors the GET handler).
        let req = scoped_get("/thinking-maps/ghost/export/markdown").to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let err: Value = test::read_body_json(resp).await;
        assert_eq!(err["error"], "not_found");
        assert_eq!(err["map_id"], "ghost");

        // No X-Principal/X-Workspace headers ⇒ 400 missing_scope.
        let req = test::TestRequest::get()
            .uri("/thinking-maps/ghost/export/markdown")
            .to_request();
        let resp = test::call_service(&service, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let err: Value = test::read_body_json(resp).await;
        assert_eq!(err["error"], "missing_scope");
    }

    #[actix_web::test]
    async fn parse_query_bool_is_lenient() {
        assert!(parse_query_bool(None, true));
        assert!(!parse_query_bool(None, false));
        assert!(parse_query_bool(Some(""), true));
        assert!(!parse_query_bool(Some(""), false));
        assert!(parse_query_bool(Some("true"), false));
        assert!(parse_query_bool(Some("1"), false));
        assert!(!parse_query_bool(Some("false"), true));
        assert!(!parse_query_bool(Some("0"), true));
        // Unknown values fall back to the caller's default.
        assert!(parse_query_bool(Some("garbage"), true));
        assert!(!parse_query_bool(Some("garbage"), false));
    }

    // Silence unused-import warnings for types referenced only to document the
    // wire shape in tests above.
    #[allow(dead_code)]
    fn _type_anchors(_n: ThinkingNode, _k: NodeKind, _e: EpistemicState, _a: AssertionOrigin) {}
}

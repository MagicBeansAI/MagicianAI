//! Loopback MCP endpoint for the Magician plane.
//!
//! Task 6 of `docs/plans/2026-08-23-magician-plane-vertical-slice-plan.md`.
//! Auth is a `plt_` bearer resolved against the live grant registry. `tools/call`
//! honours `tool_search` (grant-local load) and routes every other name through
//! `execute_action` when the grant carries live executors. A grant without
//! executors (T3 until Task 11) advertises an empty wire catalog; after a valid
//! `initialize`, a guessed `tools/call` still returns an honest unwired error.
//! Mutating calls require the server-issued MCP session from that initialize,
//! so a caller cannot invent replay namespaces beneath one bearer.

use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};

use actix_web::http::header::{self, HeaderValue};
use actix_web::web::{Bytes, Json};
use actix_web::{HttpRequest, HttpResponse};
use futures_util::Stream;
use futures_util::{FutureExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};
use tokio_stream::wrappers::ReceiverStream;

mod decision_routing;
pub use decision_routing::{plane_decision_routing_get_handler, plane_decision_routing_put_handler};
mod decision_mode;
pub use decision_mode::plane_decision_mode_put_handler;
mod elicitation;
mod run_input;
use elicitation::{CallState, ClientCapabilities, MAX_ACTIVE_CALLS};
use uuid::Uuid;

use magician::config::{load_magician_config_from_path, MagicianChatTurnSettings, PlaneConfig};
use magician::magician_v2::auth::middleware::AuthRuntime;
use magician::magician_v2::auth::sessions::SESSION_TOKEN_PREFIX;
use magician::magician_v2::auth::workspace_registry;
use magician::magician_v2::execution::agentic::AgenticContext;
use magician::magician_v2::execution::flat_loop::ToolIndex;
use magician::magician_v2::execution::plane::{
    chat_harness_snapshot, install_chat_harness_snapshot, install_plane_runtime_forget,
    plane_grant_registry, plane_runnable_tools_list_configured, plane_tools_call_configured,
    PlaneCatalogProfile, PlaneGrant,
};
use magician::magician_v2::query_analysis::parent_engine::{
    engine_family_from_client_name, normalize_parent_engine,
};
use magician::magician_v2::runtime_settings::{
    runtime_settings_paths, write_top_level_yaml_block, yaml_string,
};

// Optional GET notifications have one receiver per authenticated MCP session.
// Elicitation requests and results always use their original POST stream.
struct NotificationSender {
    id: String,
    sender: mpsc::Sender<String>,
}

type NotificationStreams = HashMap<String, HashMap<String, NotificationSender>>;

fn sse_by_grant() -> &'static Mutex<NotificationStreams> {
    static STREAMS: OnceLock<Mutex<NotificationStreams>> = OnceLock::new();
    STREAMS.get_or_init(Default::default)
}

const MAX_RPC_REPLAYS_PER_GRANT: usize = 256;
const MAX_REPLAY_GRANTS: usize = 1024;
const MAX_MCP_SESSIONS_PER_GRANT: usize = 256;

#[derive(Clone)]
struct CachedRpcResponse {
    body: Value,
    mcp_session_id: Option<String>,
}

struct CachedRpcEntry {
    fingerprint: String,
    /// `None` while the first delivery of this id is still dispatching.
    response: Option<CachedRpcResponse>,
}

/// What the door keeps of a client session from its `initialize`, for the
/// life of the session: the capabilities its calls may use, the family of
/// the CLI behind it, and the parent engine its calls take.
#[derive(Clone, Default)]
struct McpSession {
    capabilities: ClientCapabilities,
    /// The connected client's engine family (`engine_family_from_client_name`
    /// over `clientInfo.name`), or none for a client the roster does not
    /// know.
    client_family: Option<&'static str>,
    /// The parent engine of the operations this session's calls and approved
    /// captures trigger, decided once at `initialize`
    /// (`terminal_parent_engine`); none on a run or conversation grant, for
    /// an unknown client, or for a CLI that is not installed here.
    parent_engine: Option<String>,
}

impl McpSession {
    /// The session record for a grant's `initialize`. `installed` is the
    /// on-this-machine CLI probe, a parameter so the decision is testable
    /// away from this machine's PATH.
    fn from_initialize(body: &Value, grant: &PlaneGrant, installed: &dyn Fn(&str) -> bool) -> Self {
        let client_family =
            client_name_from_initialize(body).and_then(engine_family_from_client_name);
        Self {
            capabilities: ClientCapabilities::from_initialize(body),
            client_family,
            parent_engine: terminal_parent_engine(grant, client_family, installed),
        }
    }
}

fn client_name_from_initialize(body: &Value) -> Option<&str> {
    body.pointer("/params/clientInfo/name")
        .and_then(Value::as_str)
}

/// A terminal grant: an operator's own CLI, which Magician did not spawn and
/// whose engine only its `initialize` names. Run and conversation mints
/// (`SpawnedBare`) already carry the engine Magician chose for them as their
/// parent; a conversation mint keeps its lane surface besides.
fn is_terminal_grant(grant: &PlaneGrant) -> bool {
    grant.catalog_profile == PlaneCatalogProfile::Terminal
        && grant.surface() == magician::magician_v2::agents::InvocationSurface::Plane
}

/// The harness a durable grant's operator engraved at mint, when it names
/// one: the native pin names none.
fn engraved_harness_engine(grant: &PlaneGrant) -> Option<String> {
    grant
        .run_authority
        .as_ref()
        .and_then(|authority| normalize_parent_engine(Some(&authority.harness_engine)))
}

/// The parent engine a terminal grant's calls take for a session: the
/// operator-engraved harness when the grant names one — the operator's
/// choice outranks whichever CLI connected — else the connected client's
/// family; and either only when that CLI is installed on this machine, the
/// rule `PUT /plane/engine` applies before it lets an engine drive. An
/// engraved harness that is not installed names no parent rather than
/// yielding to the client: the operator's choice stands, and the
/// `initialize` log says why nothing was adopted. A run or conversation
/// grant names its own parent at mint and takes none here.
fn terminal_parent_engine(
    grant: &PlaneGrant,
    client_family: Option<&str>,
    installed: &dyn Fn(&str) -> bool,
) -> Option<String> {
    if !is_terminal_grant(grant) {
        return None;
    }
    let engine = engraved_harness_engine(grant).or_else(|| client_family.map(str::to_string))?;
    installed(&engine).then_some(engine)
}

/// Whether a roster engine's CLI is on this machine's PATH — the probe
/// `PUT /plane/engine` and `PUT /plane/chat-engine` apply.
fn harness_cli_installed(engine: &str) -> bool {
    magician::magician_v2::execution::plane::roster_with_install_status()
        .into_iter()
        .any(|(name, installed)| name == engine && installed)
}

/// Record a minted session on the grant's window from its `initialize`, and
/// say what its calls will run under. With no sessions status surface, this
/// line is the only visibility into the decision.
fn record_session(
    replay: &mut GrantReplayState,
    session_id: &str,
    body: &Value,
    grant: &PlaneGrant,
) {
    let session = McpSession::from_initialize(body, grant, &harness_cli_installed);
    tracing::info!(
        session = session_id,
        client = ?client_name_from_initialize(body),
        family = ?session.client_family,
        engraved = ?engraved_harness_engine(grant),
        terminal = is_terminal_grant(grant),
        parent = ?session.parent_engine,
        "plane MCP session initialized"
    );
    replay.sessions.insert(session_id.to_string(), session);
}

/// Make the session's parent engine the parent of everything this request's
/// call reaches, on a terminal grant: named on the per-request grant's
/// routing overrides, which the door's `grant_parent_engine` scopes the
/// call under and the executors' scoped router carries explicitly. A run or
/// conversation grant keeps the parent it was minted with. The parent is a
/// fact about the session, so it rides each request's own copy of the grant
/// — the call's, and the fresh projection an approved capture executes on —
/// and is never written back to the registry's projection.
fn adopt_session_parent(grant: &mut PlaneGrant, session: &McpSession) {
    let Some(parent) = session
        .parent_engine
        .as_deref()
        .filter(|_| is_terminal_grant(grant))
    else {
        return;
    };
    grant.ctx.llm_routing_overrides = Some(
        grant
            .ctx
            .llm_routing_overrides
            .take()
            .unwrap_or_default()
            .with_parent_engine(Some(parent.to_string())),
    );
}

#[derive(Default)]
struct GrantReplayState {
    authority_key: String,
    entries: HashMap<String, CachedRpcEntry>,
    order: VecDeque<String>,
    sessions: HashMap<String, McpSession>,
    active: HashMap<String, Arc<CallState>>,
    run_origins: HashMap<String, run_input::RunOrigin>,
}

fn replay_by_grant() -> &'static Mutex<HashMap<String, Arc<Mutex<GrantReplayState>>>> {
    static REPLAY_BY_GRANT: OnceLock<Mutex<HashMap<String, Arc<Mutex<GrantReplayState>>>>> =
        OnceLock::new();
    REPLAY_BY_GRANT.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn replay_state_for(token: &str) -> Option<Arc<Mutex<GrantReplayState>>> {
    let mut replay = replay_by_grant().lock().await;
    if let Some(state) = replay.get(token) {
        return Some(Arc::clone(state));
    }
    // Valid bearer tokens gate entry creation. Never evict an existing window
    // merely because no request holds its Arc right now: the bearer may still
    // be live, and a later retransmission would recreate an empty window and
    // repeat a non-idempotent tools/call. At capacity, preserve every existing
    // replay guarantee and refuse only a previously unseen grant.
    if replay.len() >= MAX_REPLAY_GRANTS {
        return None;
    }
    let state = Arc::new(Mutex::new(GrantReplayState::default()));
    replay.insert(token.to_string(), Arc::clone(&state));
    Some(state)
}

async fn forget_grant_runtime_state(token: &str) {
    sse_by_grant().lock().await.remove(token);
    if let Some(replay) = replay_by_grant().lock().await.remove(token) {
        for call in replay.lock().await.active.values() {
            call.cancelled.cancel();
        }
    }
}

fn ensure_runtime_forget_installed() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        install_plane_runtime_forget(Arc::new(|token: &str| {
            let token = token.to_string();
            tokio::spawn(async move {
                forget_grant_runtime_state(&token).await;
            });
        }));
    });
}

fn bearer_token(req: &HttpRequest) -> Option<String> {
    let value = req.headers().get(header::AUTHORIZATION)?;
    value
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
}

fn jsonrpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    })
}

fn jsonrpc_result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn sse_event(payload: &Value) -> String {
    format!("event: message\ndata: {payload}\n\n")
}

async fn notify_list_changed(grant_token: &str) {
    notify_other_sessions(grant_token, None).await;
}

async fn notify_other_sessions(grant_token: &str, except: Option<&str>) {
    let payload = sse_event(
        &json!({"jsonrpc":"2.0", "method":"notifications/tools/list_changed", "params":{}}),
    );
    let mut streams = sse_by_grant().lock().await;
    if let Some(sessions) = streams.get_mut(grant_token) {
        sessions.retain(|_, tx| !tx.sender.is_closed());
        for (session, tx) in sessions
            .iter()
            .filter(|(session, _)| Some(session.as_str()) != except)
        {
            let _ = session;
            let _ = tx.sender.try_send(payload.clone());
        }
    }
}

struct PlaneSseStream {
    inner: ReceiverStream<String>,
    grant_token: String,
    session: String,
    stream_id: String,
}

fn remove_notification_stream(
    streams: &mut NotificationStreams,
    token: &str,
    session: &str,
    id: &str,
) {
    if let Some(sessions) = streams.get_mut(token) {
        if sessions.get(session).is_some_and(|s| s.id == id) {
            sessions.remove(session);
        }
        if sessions.is_empty() {
            streams.remove(token);
        }
    }
}

impl Drop for PlaneSseStream {
    fn drop(&mut self) {
        if let Ok(mut streams) = sse_by_grant().try_lock() {
            remove_notification_stream(
                &mut streams,
                &self.grant_token,
                &self.session,
                &self.stream_id,
            );
        } else if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let token = self.grant_token.clone();
            let session = self.session.clone();
            let id = self.stream_id.clone();
            runtime.spawn(async move {
                remove_notification_stream(
                    &mut *sse_by_grant().lock().await,
                    &token,
                    &session,
                    &id,
                );
            });
        }
    }
}

impl Stream for PlaneSseStream {
    type Item = Result<Bytes, actix_web::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner)
            .poll_next(cx)
            .map(|value| value.map(|data| Ok(Bytes::from(data))))
    }
}

fn replay_request_key(id: &Value) -> Option<String> {
    match id {
        Value::String(value) => Some(format!("s:{value}")),
        Value::Number(value) => Some(format!("n:{value}")),
        _ => None,
    }
}

fn scoped_replay_request_key(req: &HttpRequest, body: &Value) -> Option<String> {
    let id = body.get("id").and_then(replay_request_key)?;
    let method = body.get("method").and_then(Value::as_str).unwrap_or("");
    let namespace = if method == "initialize" {
        // Replaying initialize for the same bearer/id returns the same
        // server-minted session instead of minting an unbounded series.
        "initialize".to_string()
    } else {
        req.headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| value.starts_with("pltsess_") && value.len() <= 128)
            // Keep even a validated server-issued session opaque in the replay
            // key; the raw value is retained only in the bounded validation set.
            .map(|value| format!("session:{}", blake3::hash(value.as_bytes()).to_hex()))
            .unwrap_or_else(|| "unbound".to_string())
    };
    Some(format!("{namespace}:{id}"))
}

fn presented_mcp_session(req: &HttpRequest) -> Option<&str> {
    req.headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| value.starts_with("pltsess_") && value.len() <= 128)
}

fn grant_authority_key(grant: &PlaneGrant) -> String {
    let mut allowed_tools = grant.allowed_tools.clone();
    allowed_tools.sort_unstable();
    serde_json::to_string(&json!({
        "principal": grant.ctx.principal.as_deref(),
        "workspace": grant.ctx.workspace.as_deref(),
        "agent": grant.ctx.agent_id.as_deref(),
        "execution": grant.ctx.execution_id.as_deref(),
        "session": grant.session_id.as_str(),
        "allowedTools": allowed_tools,
        "surface": format!("{:?}", grant.surface()),
        "catalog": format!("{:?}", grant.catalog_profile),
        "liveHarnessTurn": grant.live_harness_turn,
    }))
    .expect("plane authority key is JSON serializable")
}

fn rpc_fingerprint(body: &Value) -> String {
    // serde_json maps are deterministic unless preserve_order is selected; in
    // that mode an order-only change safely fails closed as conflicting reuse.
    serde_json::to_string(body).expect("JSON-RPC request is JSON serializable")
}

fn cached_rpc_response(response: CachedRpcResponse) -> HttpResponse {
    let mut http = HttpResponse::Ok().json(response.body);
    if let Some(session_id) = response.mcp_session_id {
        if let Ok(value) = HeaderValue::from_str(&session_id) {
            http.headers_mut()
                .insert(header::HeaderName::from_static("mcp-session-id"), value);
        }
    }
    http
}

fn terminal_catalog_unavailable() -> HttpResponse {
    HttpResponse::ServiceUnavailable().json(json!({
        "error": "tool_catalog_unavailable",
        "message": "The runtime tool catalog is unavailable; retry when the runtime is ready.",
    }))
}

// The door admits only a successfully resolved, populated catalog. The two
// halves are kept separate from resolver I/O so both failure modes can be
// exercised without corrupting a workspace or replacing the process-global
// runtime.

/// Resolver I/O failed: refuse without leaking the resolver's diagnostic.
fn terminal_catalog_resolution_failed(error: anyhow::Error) -> HttpResponse {
    tracing::warn!(%error, "terminal plane catalog resolution failed");
    terminal_catalog_unavailable()
}

/// An empty index is not a catalog a terminal can act from.
fn admit_terminal_catalog(index: Arc<ToolIndex>) -> Result<Arc<ToolIndex>, HttpResponse> {
    if index.is_empty() {
        return Err(terminal_catalog_unavailable());
    }
    Ok(index)
}

/// Resolve either plane credential without letting the two identical `plt_`
/// prefixes weaken each other. Durable terminal grants are revalidated against
/// `AuthStore` on every request, then cached only for process-local tool-search
/// state. A cache miss falls back to a process-local run or chat grant.
async fn resolve_plane_grant(
    token: &str,
    auth: Option<&AuthRuntime>,
) -> Result<Option<PlaneGrant>, HttpResponse> {
    let registry = plane_grant_registry();
    if let Some(auth) = auth {
        let terminal = auth.store.resolve_grant(token).map_err(|error| {
            HttpResponse::InternalServerError().json(json!({
                "error": "grant_store_failure",
                "message": error.to_string(),
            }))
        })?;
        if let Some(terminal) = terminal {
            let identity = auth
                .store
                .find_identity(&terminal.identity)
                .map_err(|error| {
                    HttpResponse::InternalServerError().json(json!({
                        "error": "grant_identity_failure",
                        "message": error.to_string(),
                    }))
                })?;
            let Some(identity) = identity else {
                registry.revoke_cached_durable(token).await;
                return Ok(None);
            };
            let scopes_root = auth.store.runtime_root().join("scopes");
            let owns_workspace =
                workspace_registry::owns(&scopes_root, &identity, terminal.workspace.as_str())
                    .map_err(|error| {
                        HttpResponse::InternalServerError().json(json!({
                            "error": "grant_workspace_failure",
                            "message": error.to_string(),
                        }))
                    })?;
            if !owns_workspace {
                registry.revoke_cached_durable(token).await;
                return Ok(None);
            }

            // Resolve the same scope catalog used by live runs. AgentResources'
            // process index is installed lazily by the first registry build;
            // reading only that slot would strand a cold terminal until a chat
            // or run happened to warm it (and could use another scope's index).
            let executors = magician::magician_v2::execution::plane::runless_executors()
                .ok_or_else(terminal_catalog_unavailable)?;
            let resolver = executors
                .capability_scope_resolver
                .as_ref()
                .ok_or_else(terminal_catalog_unavailable)?;
            let snapshot = resolver
                .capability_snapshot_for_scope(&identity.scope_root, &terminal.workspace)
                .map_err(terminal_catalog_resolution_failed)?;
            let tool_index = admit_terminal_catalog(Arc::clone(&snapshot.tool_index))?;

            let mut ctx = AgenticContext::default();
            ctx.principal = Some(identity.scope_root);
            ctx.workspace = Some(terminal.workspace);
            ctx.agent_id = Some(terminal.agent_identity);
            ctx.execution_id = Some(format!("plane-terminal-{}", terminal.id));
            let mut projected = PlaneGrant::for_terminal(
                ctx,
                format!("plane-terminal-{}", terminal.id),
                terminal.allowed_tools,
                tool_index,
            );
            // The grant's own copy of the runless executors: the scope's
            // registry and this identity seeded on fresh slots, so scope-bound
            // compiled tools run instead of failing for a missing principal.
            // Re-projected on every revalidation, so a refreshed grant carries
            // freshly scoped executors along with its refreshed index.
            projected.executors = Some(Arc::new(executors.for_runless_scope(
                &projected.ctx,
                &snapshot,
                &projected.allowed_tools,
            )));
            // Durable terminal grants may delegate runs (plane Tasks 6b/10):
            // the engraved harness is the engine a started run inherits —
            // `magician` is the deliberate pin — with the minted ceilings.
            // The allowlist still gates calling `run_task` at all.
            projected = projected.with_run_authority(
                magician::magician_v2::execution::plane::PlaneRunAuthority {
                    allowed_agents: Vec::new(),
                    harness_engine: terminal.harness_engine.clone(),
                    max_usd: terminal.max_usd,
                    max_wall_clock: terminal
                        .max_wall_clock_secs
                        .map(std::time::Duration::from_secs),
                    max_concurrent_runs: terminal
                        .max_concurrent_runs
                        .unwrap_or(DEFAULT_TERMINAL_GRANT_CONCURRENT_RUNS),
                },
            );
            return Ok(Some(registry.cache_durable(token, projected).await));
        }
        registry.revoke_cached_durable(token).await;
    }
    Ok(registry.resolve_run_scoped(token).await)
}

async fn dispatch_plane_rpc(
    body: &Value,
    token: &str,
    grant: &PlaneGrant,
    presented_session: Option<&str>,
    plane_config: Option<&PlaneConfig>,
) -> CachedRpcResponse {
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    if grant.is_revoked() {
        return CachedRpcResponse {
            body: jsonrpc_error(&id, -32001, "plane grant was revoked"),
            mcp_session_id: None,
        };
    }
    let method = body.get("method").and_then(Value::as_str).unwrap_or("");
    let (response_body, mcp_session_id) = match method {
        "initialize" => {
            let result = json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {"listChanged": true}},
                "serverInfo": {"name": "magician-plane", "version": env!("CARGO_PKG_VERSION")}
            });
            // Always server-mint this value. Reflecting either the Authorization
            // bearer or a client-provided lookalike leaks the plane credential
            // into session-id logs and downstream middleware.
            let session_id = format!("pltsess_{}", Uuid::new_v4().simple());
            (jsonrpc_result(&id, result), Some(session_id))
        },
        "ping" => (jsonrpc_result(&id, json!({})), None),
        "tools/list" => {
            let tools = plane_runnable_tools_list_configured(grant, plane_config);
            (jsonrpc_result(&id, json!({"tools": tools})), None)
        },
        "tools/call" => {
            let params = body.get("params").cloned().unwrap_or(json!({}));
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            if name.is_empty() {
                (jsonrpc_error(&id, -32602, "missing tool name"), None)
            } else {
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
                let result = plane_tools_call_configured(
                    grant,
                    plane_config,
                    presented_session,
                    name,
                    &arguments,
                )
                .await;
                if result
                    .pointer("/_meta/toolsListChanged")
                    .and_then(Value::as_bool)
                    == Some(true)
                {
                    notify_list_changed(token).await;
                }
                (jsonrpc_result(&id, result), None)
            }
        },
        "" => (jsonrpc_error(&id, -32600, "missing method"), None),
        other => (
            jsonrpc_error(&id, -32601, &format!("method not found: {other}")),
            None,
        ),
    };
    CachedRpcResponse {
        body: response_body,
        mcp_session_id,
    }
}

/// POST `/api/magician/v2/plane/mcp` — JSON-RPC MCP over streamable HTTP.
/// Default concurrent-run ceiling for durable terminal grants that did not
/// pin one at mint. Small on purpose: a terminal is one human's seat.
pub const DEFAULT_TERMINAL_GRANT_CONCURRENT_RUNS: usize = 4;

/// Resolve the owner identity for grant management from a session bearer.
/// A PAT or `plt_` bearer cannot manage grants, and there is deliberately no
/// principal-header fallback: an engraved internal scope proves request scope,
/// but only a login session proves this is an interactive owner-management
/// action.
fn owner_identity(req: &HttpRequest, auth: Option<&AuthRuntime>) -> Result<String, HttpResponse> {
    if let Some(token) = bearer_token(req) {
        if token.starts_with(SESSION_TOKEN_PREFIX) {
            let Some(auth) = auth else {
                return Err(HttpResponse::ServiceUnavailable()
                    .json(json!({"error": "auth_store_unavailable"})));
            };
            let session = auth.store.resolve_session(&token).map_err(|error| {
                HttpResponse::InternalServerError().json(json!({
                    "error": "session_store_failure",
                    "message": error.to_string()
                }))
            })?;
            let Some(session) = session else {
                return Err(HttpResponse::Unauthorized().json(json!({"error": "invalid_session"})));
            };
            return Ok(session.identity);
        }
        // Any other bearer shape (a plt_ grant) is refused outright: grants
        // manage nothing.
        return Err(HttpResponse::Unauthorized().json(json!({"error": "grant_cannot_manage"})));
    }
    Err(HttpResponse::Unauthorized().json(json!({"error": "session_bearer_required"})))
}

/// `GET /api/magician/v2/plane/engines` — the harness roster with
/// on-this-machine install status, so surfaces offer only what the operator
/// can actually run. `magician` (the forced pin) is always available.
pub async fn plane_engines_handler() -> HttpResponse {
    let mut engines: Vec<Value> =
        magician::magician_v2::execution::plane::roster_with_install_status()
            .into_iter()
            .map(|(name, installed)| {
                let posture = magician::magician_v2::execution::plane::harness_engine_for(name)
                    .map(|engine| engine.capabilities().native_tool_posture.as_str())
                    .unwrap_or("live");
                json!({
                    "name": name,
                    "installed": installed,
                    "native_tool_posture": posture,
                })
            })
            .collect();
    engines.push(json!({
        "name": "magician",
        "installed": true,
        "native_tool_posture": "stripped",
    }));
    // The model axis: what each harness offers when a surface picks
    // harness+model. Curated from live probes (2026-08-31): claude aliases
    // from --help; agy enumerated by `agy models`; codex PINNED by
    // ChatGPT accounts (alternates 400 — default only); grok ids from
    // `grok models` (refreshed 2026-09-26). `default` is always the CLI's own choice.
    let models_for = |name: &str| -> Vec<&'static str> {
        match name {
            "pi" => vec!["default"],
            "claude_code" => vec!["default", "haiku", "sonnet", "opus", "fable"],
            "codex" => vec!["default"],
            "codex_app_server" => vec!["default"],
            "grok" => vec![
                "default",
                "grok-4.7",
                "grok-4.7-build-fast",
                "grok-4.6",
                "grok-4.5",
            ],
            "agy" => vec![
                "default",
                "gemini-3.7-flash-low",
                "gemini-3.7-flash-medium",
                "gemini-3.7-flash-high",
            ],
            _ => vec!["default"],
        }
    };
    for engine in engines.iter_mut() {
        let name = engine["name"].as_str().unwrap_or_default().to_string();
        engine["models"] = json!(models_for(&name));
    }
    let snapshot = magician::magician_v2::execution::plane::harness_engine_snapshot();
    let chat = magician::magician_v2::execution::plane::chat_harness_snapshot();
    HttpResponse::Ok().json(json!({
        "decision_mode": magician::magician_v2::decision_host::decision_mode(),
        "engines": engines,
        "current": snapshot.engine,
        "chat_current": magician::magician_v2::execution::plane::coerce_chat_mouth_engine(&chat.engine),
        "chat_model": chat.harness_model,
        "run_model": snapshot.harness_model,
        "run_pi_profile": snapshot.pi_profile,
        "turn_max_seconds": snapshot.turn_max_seconds,
        "turn_max_tool_calls": snapshot.turn_max_tool_calls,
        "chat_turn_max_seconds": chat.turn_max_seconds,
        "chat_turn_max_tool_calls": chat.turn_max_tool_calls,
    }))
}

/// Serialize the execution settings as the `execution:` block. Round-trips
/// the typed struct so no field can be dropped by a hand-rendered subset
/// (the section carries sandboxes and failure modes beyond the engine
/// keys); comments in the block are surrendered, as with the chat writer.
fn render_execution_config_block(settings: &magician::config::MagicianExecutionSettings) -> String {
    let body = serde_yaml::to_string(settings).unwrap_or_default();
    let indented: Vec<String> = body
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| format!("  {line}"))
        .collect();
    format!("execution:\n{}", indented.join("\n"))
}

fn render_chat_config_block(settings: &MagicianChatTurnSettings) -> String {
    [
        "chat:".to_string(),
        "  # Who thinks a chat turn. `magician` is today's LLM mouth.".to_string(),
        "  # Roster names (pi, claude_code, codex, grok, agy, codex_app_server) replace"
            .to_string(),
        "  # the mouth; native-tool strip is per-engine (stripped or sandboxed).".to_string(),
        format!(
            "  harness_engine: {}",
            yaml_string(&settings.harness_engine)
        ),
        format!("  harness_model: {}", yaml_string(&settings.harness_model)),
        format!(
            "  harness_turn_max_tool_calls: {}",
            settings.harness_turn_max_tool_calls
        ),
        format!(
            "  harness_turn_max_seconds: {}",
            settings.harness_turn_max_seconds
        ),
    ]
    .join("\n")
}

fn launchable_chat_engine(name: &str) -> bool {
    name == "magician"
        || matches!(
            magician::magician_v2::execution::plane::resolve_turn_engine(Some(name)),
            magician::magician_v2::execution::plane::TurnEngine::Harness(_)
        )
}

/// `PUT /api/magician/v2/plane/engine` — persist `execution.harness_engine`
/// (+ `harness_model`) and install the live snapshot. Session bearer only.
/// Mirrors the chat-engine switch for the RUN engine, and is an affinity
/// driver: runs driven by an external harness carry the system with them.
pub async fn plane_engine_put_handler(
    req: HttpRequest,
    body: Json<Value>,
    auth: Option<actix_web::web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = owner_identity(&req, auth.as_ref().map(|data| data.get_ref())) {
        return response;
    }
    let harness_engine = body
        .get("harness_engine")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if harness_engine.is_empty() {
        return HttpResponse::BadRequest().json(json!({
            "error": "harness_engine_required",
            "message": "name who thinks each loop iteration; `magician` is the built-in path"
        }));
    }
    if !launchable_chat_engine(harness_engine) {
        return HttpResponse::BadRequest().json(json!({
            "error": "harness_engine_not_launchable",
            "message": format!(
                "`{harness_engine}` cannot drive the loop on this build; name a launchable engine or pin `magician` deliberately"
            ),
        }));
    }
    if harness_engine != "magician" {
        let installed = magician::magician_v2::execution::plane::roster_with_install_status()
            .into_iter()
            .any(|(name, installed)| name == harness_engine && installed);
        if !installed {
            return HttpResponse::BadRequest().json(json!({
                "error": "harness_engine_not_installed",
                "message": format!(
                    "`{harness_engine}` is not installed on this machine; install the CLI or pin `magician`"
                ),
            }));
        }
    }
    let harness_model = body
        .get("harness_model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or("default");
    let pi_profile_update = body.get("pi_profile").map(|value| {
        if value.is_null() {
            Ok(None)
        } else {
            value
                .as_str()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(|name| Some(name.to_string()))
                .ok_or("pi_profile must be a non-empty profile name or null")
        }
    });
    let pi_profile_update = match pi_profile_update.transpose() {
        Ok(update) => update,
        Err(message) => {
            return HttpResponse::BadRequest().json(json!({
                "error": "invalid_pi_profile", "message": message,
            }))
        },
    };

    let paths = runtime_settings_paths();
    let mut config = match load_magician_config_from_path(&paths.config_path) {
        Ok(config) => config,
        Err(error) => {
            return HttpResponse::InternalServerError().json(json!({
                "error": "config_unreadable",
                "message": format!("failed to load {}: {}", paths.config_path.display(), error),
            }))
        },
    };
    if let Some(update) = pi_profile_update {
        config.execution.pi_profile = update;
    }
    if let Some(name) = config
        .execution
        .pi_profile
        .as_deref()
        .filter(|_| harness_engine == "pi")
    {
        let resolved = config
            .router_config()
            .ok_or_else(|| anyhow::anyhow!("Magician has no configured LLM profiles"))
            .and_then(|router| {
                magician::magician_v2::query_analysis::multi_llm_service::resolve_pi_profile_config(
                    router, name,
                )
            });
        if let Err(error) = resolved {
            return HttpResponse::BadRequest().json(json!({
                "error": "invalid_pi_profile", "message": error.to_string(),
            }));
        }
    }
    config.execution.harness_engine = harness_engine.to_string();
    config.execution.harness_model = harness_model.to_string();
    if let Err(error) = write_top_level_yaml_block(
        &paths.config_path,
        "execution",
        &render_execution_config_block(&config.execution),
    ) {
        return HttpResponse::InternalServerError().json(json!({
            "error": "config_unwritable",
            "message": format!("failed to update {}: {}", paths.config_path.display(), error),
        }));
    }

    let mut snapshot = magician::magician_v2::execution::plane::harness_engine_snapshot();
    snapshot.engine = config.execution.harness_engine.clone();
    snapshot.harness_model = config.execution.harness_model.clone();
    snapshot.pi_profile = config.execution.pi_profile.clone();
    magician::magician_v2::execution::plane::install_harness_engine_snapshot(snapshot);
    // The run engine is the primary affinity driver (owner rule 2026-08-31).
    magician::magician_v2::query_analysis::operation_llm_router::set_harness_affinity(Some(
        harness_engine,
    ));
    HttpResponse::Ok().json(json!({
        "engine": config.execution.harness_engine,
        "harness_model": config.execution.harness_model,
        "pi_profile": config.execution.pi_profile,
    }))
}

/// `PUT /api/magician/v2/plane/chat-engine` — persist `chat.harness_engine`
/// and install the live snapshot. Session bearer only; a `plt_` grant
/// manages nothing.
pub async fn plane_chat_engine_put_handler(
    req: HttpRequest,
    body: Json<Value>,
    auth: Option<actix_web::web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = owner_identity(&req, auth.as_ref().map(|data| data.get_ref())) {
        return response;
    }
    let harness_engine = body
        .get("harness_engine")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if harness_engine.is_empty() {
        return HttpResponse::BadRequest().json(json!({
            "error": "harness_engine_required",
            "message": "name who thinks a chat turn; `magician` is today's mouth"
        }));
    }
    if !launchable_chat_engine(harness_engine) {
        return HttpResponse::BadRequest().json(json!({
            "error": "harness_engine_not_launchable",
            "message": format!(
                "`{harness_engine}` cannot think a chat turn on this build; name a launchable \
                 engine or pin `magician` deliberately"
            ),
        }));
    }
    if harness_engine != "magician" {
        let installed = magician::magician_v2::execution::plane::roster_with_install_status()
            .into_iter()
            .any(|(name, installed)| name == harness_engine && installed);
        if !installed {
            return HttpResponse::BadRequest().json(json!({
                "error": "harness_engine_not_installed",
                "message": format!(
                    "`{harness_engine}` is not installed on this machine; install the CLI \
                     or pin `magician`"
                ),
            }));
        }
    }

    let paths = runtime_settings_paths();
    let mut config = match load_magician_config_from_path(&paths.config_path) {
        Ok(config) => config,
        Err(error) => {
            return HttpResponse::InternalServerError().json(json!({
                "error": "config_unreadable",
                "message": format!("failed to load {}: {error}", paths.config_path.display()),
            }))
        },
    };
    config.chat.harness_engine = harness_engine.to_string();
    let harness_model = body
        .get("harness_model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or("default");
    config.chat.harness_model = harness_model.to_string();
    if let Err(error) = write_top_level_yaml_block(
        &paths.config_path,
        "chat",
        &render_chat_config_block(&config.chat),
    ) {
        return HttpResponse::InternalServerError().json(json!({
            "error": "config_unwritable",
            "message": format!("failed to update {}: {error}", paths.config_path.display()),
        }));
    }

    let mut snapshot = chat_harness_snapshot();
    snapshot.engine = config.chat.harness_engine.clone();
    snapshot.harness_model = config.chat.harness_model.clone();
    snapshot.turn_max_tool_calls = config.chat.harness_turn_max_tool_calls;
    snapshot.turn_max_seconds = config.chat.harness_turn_max_seconds;
    install_chat_harness_snapshot(snapshot);
    // The driving harness is an affinity driver too (owner rule 2026-08-31):
    // when the chat mouth runs on an external harness, the system's
    // non-local calls follow it.
    magician::magician_v2::query_analysis::operation_llm_router::set_harness_affinity(Some(
        harness_engine,
    ));

    HttpResponse::Ok().json(json!({
        "chat_current": config.chat.harness_engine,
        "chat_model": config.chat.harness_model,
        "persisted": true,
    }))
}

/// `GET /api/magician/v2/plane/grants` — list this owner's terminal grants.
/// Never returns token values; the store keeps only hashes.
pub async fn plane_grants_list_handler(
    req: HttpRequest,
    auth: Option<actix_web::web::Data<AuthRuntime>>,
) -> HttpResponse {
    let identity = match owner_identity(&req, auth.as_ref().map(|data| data.get_ref())) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    let Some(auth) = auth.as_ref().map(|data| data.get_ref()) else {
        return HttpResponse::ServiceUnavailable().json(json!({"error": "auth_store_unavailable"}));
    };
    match auth.store.list_grants(&identity) {
        Ok(grants) => HttpResponse::Ok().json(json!({
            "grants": grants.iter().map(|grant| json!({
                "id": grant.id,
                "label": grant.label,
                "workspace": grant.workspace,
                "agent_identity": grant.agent_identity,
                "harness_engine": grant.harness_engine,
                "allowed_tools": grant.allowed_tools,
                "max_usd": grant.max_usd,
                "max_wall_clock_secs": grant.max_wall_clock_secs,
                "max_concurrent_runs": grant.max_concurrent_runs
                    .unwrap_or(DEFAULT_TERMINAL_GRANT_CONCURRENT_RUNS),
                "created_at": grant.created_at,
                "expires_at": grant.expires_at,
            })).collect::<Vec<_>>(),
        })),
        Err(error) => HttpResponse::InternalServerError()
            .json(json!({"error": "grant_store_failure", "message": error.to_string()})),
    }
}

/// `POST /api/magician/v2/plane/grants` — mint a terminal grant. The token
/// value is returned exactly once, here; the store keeps only its hash.
pub async fn plane_grants_mint_handler(
    req: HttpRequest,
    body: Json<Value>,
    auth: Option<actix_web::web::Data<AuthRuntime>>,
) -> HttpResponse {
    let identity = match owner_identity(&req, auth.as_ref().map(|data| data.get_ref())) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    let Some(auth) = auth.as_ref().map(|data| data.get_ref()) else {
        return HttpResponse::ServiceUnavailable().json(json!({"error": "auth_store_unavailable"}));
    };
    let harness_engine = body
        .get("harness_engine")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if harness_engine.is_empty() {
        return HttpResponse::BadRequest()
            .json(json!({"error": "harness_engine_required",
                "message": "name the harness a started run thinks with; `magician` is the deliberate pin"}));
    }
    // Validate at the door: an unlaunchable name must not be engraved into a
    // durable credential that would only fail later, at run_task. Same rule
    // the delegated-run door applies.
    if harness_engine != "magician"
        && !matches!(
            magician::magician_v2::execution::plane::resolve_turn_engine(Some(harness_engine)),
            magician::magician_v2::execution::plane::TurnEngine::Harness(_)
        )
    {
        return HttpResponse::BadRequest().json(json!({
            "error": "harness_engine_not_launchable",
            "message": format!(
                "`{harness_engine}` cannot be launched on this build; name a launchable \
                 engine or pin `magician` deliberately"
            ),
        }));
    }
    let requested_tools: Vec<String> = body
        .get("allowed_tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|tool| !tool.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    for field in ["label", "workspace", "agent_identity"] {
        let present = body
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .is_some_and(|value| !value.is_empty());
        if !present {
            return HttpResponse::BadRequest()
                .json(json!({"error": "missing_field", "field": field}));
        }
    }
    let ttl_hours = body.get("ttl_hours").and_then(Value::as_u64).unwrap_or(0);
    let text_field = |name: &str| {
        body.get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    };
    let spec = magician::magician_v2::auth::sessions::MintGrantSpec {
        label: text_field("label"),
        workspace: text_field("workspace"),
        agent_identity: text_field("agent_identity"),
        harness_engine: harness_engine.to_string(),
        allowed_tools: requested_tools.clone(),
        ttl_hours,
        max_usd: body.get("max_usd").and_then(Value::as_f64),
        max_wall_clock_secs: body.get("max_wall_clock_secs").and_then(Value::as_u64),
        max_concurrent_runs: body
            .get("max_concurrent_runs")
            .and_then(Value::as_u64)
            .map(|runs| runs as usize),
    };
    match auth.store.mint_grant(&identity, spec) {
        Ok((grant, value)) => {
            let dropped: Vec<&str> = requested_tools
                .iter()
                .map(String::as_str)
                .filter(|tool| !grant.allowed_tools.iter().any(|kept| kept == tool))
                .collect();
            HttpResponse::Created().json(json!({
                "grant": {
                    "id": grant.id,
                    "label": grant.label,
                    "workspace": grant.workspace,
                    "agent_identity": grant.agent_identity,
                    "harness_engine": grant.harness_engine,
                    "allowed_tools": grant.allowed_tools,
                    "max_usd": grant.max_usd,
                    "max_wall_clock_secs": grant.max_wall_clock_secs,
                    "max_concurrent_runs": grant.max_concurrent_runs
                        .unwrap_or(DEFAULT_TERMINAL_GRANT_CONCURRENT_RUNS),
                    "created_at": grant.created_at,
                    "expires_at": grant.expires_at,
                },
                // The only time the full value exists outside the client.
                "token": value,
                "dropped_tools": dropped,
            }))
        },
        Err(error) => HttpResponse::BadRequest()
            .json(json!({"error": "grant_mint_failed", "message": error.to_string()})),
    }
}

/// `DELETE /api/magician/v2/plane/grants/{id}` — revoke a terminal grant.
/// The next `tools/call` under its bearer revalidates against the store,
/// finds nothing, and fails closed (the door revalidates durable grants on
/// every request, so no cached projection outlives the row).
pub async fn plane_grants_revoke_handler(
    req: HttpRequest,
    id: actix_web::web::Path<uuid::Uuid>,
    auth: Option<actix_web::web::Data<AuthRuntime>>,
) -> HttpResponse {
    let identity = match owner_identity(&req, auth.as_ref().map(|data| data.get_ref())) {
        Ok(identity) => identity,
        Err(response) => return response,
    };
    let Some(auth) = auth.as_ref().map(|data| data.get_ref()) else {
        return HttpResponse::ServiceUnavailable().json(json!({"error": "auth_store_unavailable"}));
    };
    match auth.store.revoke_grant(&identity, id.into_inner()) {
        Ok(true) => HttpResponse::Ok().json(json!({"revoked": true})),
        Ok(false) => HttpResponse::NotFound().json(json!({"revoked": false})),
        Err(error) => HttpResponse::InternalServerError()
            .json(json!({"error": "grant_store_failure", "message": error.to_string()})),
    }
}

pub async fn plane_mcp_handler(
    req: HttpRequest,
    body: Json<Value>,
    auth: Option<actix_web::web::Data<AuthRuntime>>,
    plane_config: Option<actix_web::web::Data<PlaneConfig>>,
) -> HttpResponse {
    ensure_runtime_forget_installed();
    let token = match bearer_token(&req) {
        Some(token) => token,
        None => return HttpResponse::Unauthorized().json(json!({"error": "missing_grant"})),
    };
    let mut grant = match resolve_plane_grant(&token, auth.as_ref().map(|d| d.get_ref())).await {
        Ok(Some(grant)) => grant,
        Ok(None) => {
            forget_grant_runtime_state(&token).await;
            return HttpResponse::Unauthorized().json(json!({"error": "invalid_grant"}));
        },
        Err(response) => return response,
    };
    let plane_config = plane_config.as_ref().map(|d| d.get_ref());
    let method = body.get("method").and_then(Value::as_str).unwrap_or("");
    // JSON-RPC ids are scoped to an MCP session, not to the bearer forever.
    // Without this namespace, two legitimate sessions reusing id 1 for
    // different calls conflict; worse, an old response could be replayed into
    // a new session. Read-only calls that omit the session header retain one
    // conservative grant-local namespace; `tools/call` requires a session the
    // server issued and recorded below.
    let replayable_id = body.get("id").and_then(replay_request_key);
    if matches!(method, "initialize" | "tools/call") && replayable_id.is_none() {
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        return HttpResponse::BadRequest().json(jsonrpc_error(
            &id,
            -32600,
            "initialize and tools/call require a string or number request id",
        ));
    }
    let fingerprint = rpc_fingerprint(&body);
    let authority_key = grant_authority_key(&grant);
    let Some(replay_state) = replay_state_for(&token).await else {
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        return HttpResponse::ServiceUnavailable().json(jsonrpc_error(
            &id,
            -32002,
            "plane replay capacity is busy; request was not dispatched",
        ));
    };
    // Hold this lock only for admission: cache lookup, session checks, and the
    // in-flight sentinel. Dispatch (including `execute_action`) runs after
    // the guard is dropped so a long tools/call cannot stall every other id.
    let mut replay = replay_state.lock().await;
    if grant.is_revoked() {
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        return HttpResponse::Unauthorized().json(jsonrpc_error(
            &id,
            -32001,
            "plane grant was revoked",
        ));
    }
    if replay.authority_key != authority_key {
        for call in replay.active.values() {
            call.cancelled.cancel();
        }
        *replay = GrantReplayState {
            authority_key,
            ..GrantReplayState::default()
        };
    }
    // initialize has no established session namespace. Independent clients
    // commonly both use id 0: replaying it by bearer/id would merge their
    // sessions. Each initialize therefore creates a fresh bounded session.
    if method == "initialize" {
        if replay.sessions.len() >= MAX_MCP_SESSIONS_PER_GRANT {
            return HttpResponse::ServiceUnavailable().json(jsonrpc_error(
                &body["id"],
                -32002,
                "plane MCP session capacity is busy",
            ));
        }
        let response = dispatch_plane_rpc(&body, &token, &grant, None, plane_config).await;
        if let Some(session) = response.mcp_session_id.as_ref() {
            record_session(&mut replay, session, &body, &grant);
        }
        return cached_rpc_response(response);
    }
    let session_header_present = req.headers().contains_key("mcp-session-id");
    let presented_session = presented_mcp_session(&req);
    if method != "initialize" {
        let invalid_presented_session = session_header_present
            && presented_session
                .map(|session| !replay.sessions.contains_key(session))
                .unwrap_or(true);
        let missing_call_session =
            (method == "tools/call" || method.is_empty() || method.starts_with("notifications/"))
                && presented_session.is_none();
        if invalid_presented_session || missing_call_session {
            let id = body.get("id").cloned().unwrap_or(Value::Null);
            return HttpResponse::BadRequest().json(jsonrpc_error(
                &id,
                -32003,
                "a valid server-issued mcp-session-id is required",
            ));
        }
    }
    let session = presented_session
        .and_then(|id| replay.sessions.get(id))
        .cloned()
        .unwrap_or_default();
    grant.elicitation_enabled = session.capabilities.supports("form")
        && grant.surface() == magician::magician_v2::agents::InvocationSurface::Plane
        && !grant.live_harness_turn;
    // Only a call dispatches, so only a call takes the session's parent — on
    // this request's own copy of the grant, ahead of both dispatch paths
    // below (the plain call and the interactive one).
    if method == "tools/call" {
        adopt_session_parent(&mut grant, &session);
    }
    if method.is_empty() {
        if let Some(session) = presented_session {
            if replay
                .active
                .values()
                .any(|call| call.answer(session, &body))
            {
                return HttpResponse::Accepted().finish();
            }
        }
        return HttpResponse::BadRequest().json(json!({"error":"unknown_elicitation_response"}));
    }
    if method.starts_with("notifications/") {
        if method == "notifications/cancelled" {
            if let Some(id) = body.pointer("/params/requestId") {
                for call in replay.active.values() {
                    if Some(call.session.as_str()) == presented_session && &call.request_id == id {
                        call.cancelled.cancel();
                    }
                }
            }
        }
        return HttpResponse::Accepted().finish();
    }
    if replayable_id.is_none() {
        return HttpResponse::BadRequest().json(jsonrpc_error(
            &Value::Null,
            -32600,
            "request id must be a string or number",
        ));
    }
    let Some(request_id) = scoped_replay_request_key(&req, &body) else {
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        return HttpResponse::BadRequest().json(jsonrpc_error(
            &id,
            -32600,
            "request id must be a string or number",
        ));
    };
    if let Some(cached) = replay.entries.get(&request_id) {
        if cached.fingerprint != fingerprint {
            let id = body.get("id").cloned().unwrap_or(Value::Null);
            return HttpResponse::Ok().json(jsonrpc_error(
                &id,
                -32600,
                "request id was already used for a different request on this grant",
            ));
        }
        return match &cached.response {
            Some(response) => cached_rpc_response(response.clone()),
            None => {
                let id = body.get("id").cloned().unwrap_or(Value::Null);
                HttpResponse::ServiceUnavailable().json(jsonrpc_error(
                    &id,
                    -32002,
                    "this request id is already in flight",
                ))
            },
        };
    }

    if method == "tools/call"
        && grant.elicitation_enabled
        && replay.active.len() >= MAX_ACTIVE_CALLS
    {
        return HttpResponse::ServiceUnavailable().json(jsonrpc_error(
            &body["id"],
            -32002,
            "too many active interactive calls",
        ));
    }
    if body.pointer("/params/name").and_then(Value::as_str) == Some("run_task")
        && replay.run_origins.len() + replay.active.len() >= MAX_RPC_REPLAYS_PER_GRANT
    {
        return HttpResponse::ServiceUnavailable().json(jsonrpc_error(
            &body["id"],
            -32002,
            "plane run-origin capacity is busy; run was not launched",
        ));
    }
    replay.entries.insert(
        request_id.clone(),
        CachedRpcEntry {
            fingerprint: fingerprint.clone(),
            response: None,
        },
    );
    if method == "tools/call" && grant.elicitation_enabled {
        let session = presented_session
            .expect("tools/call session validated")
            .to_string();
        let (call, receiver) = CallState::new(
            session,
            body["id"].clone(),
            body.pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
        );
        replay.active.insert(request_id.clone(), call.clone());
        drop(replay);
        let body = body.into_inner();
        let config = plane_config.cloned();
        let runtime = req
            .app_data::<actix_web::web::Data<crate::web_api::MagicianV2Api>>()
            .cloned();
        let auth = auth.clone();
        actix_web::rt::spawn(async move {
            let route = magician::magician_v2::execution::plane::input::InputRoute {
                principal: grant.ctx.principal.clone().unwrap_or_default(),
                workspace: grant.ctx.workspace.clone().unwrap_or_default(),
                channel: Arc::new(run_input::AuthorizedInputChannel {
                    call: call.clone(),
                    token: token.clone(),
                    grant: grant.clone(),
                    auth: auth.clone(),
                }),
            };
            let result = std::panic::AssertUnwindSafe(magician::magician_v2::execution::plane::input::INPUT_ROUTE.scope(route, run_input::dispatch_interactive(
                &body, &token, &grant, config.as_ref(), &call, &replay_state, runtime.as_ref(), auth.as_ref().map(|a| a.get_ref()),
            ))).catch_unwind().await.unwrap_or_else(|_| json!({"isError":true,"content":[{"type":"text","text":"the tool call failed; its outcome must be reviewed before starting a new request"}]}));
            let response = CachedRpcResponse {
                body: jsonrpc_result(&body["id"], result),
                mcp_session_id: None,
            };
            {
                let mut replay = replay_state.lock().await;
                replay.active.remove(&request_id);
                finish_replay(&mut replay, request_id, fingerprint, response.clone());
            }
            call.send(&response.body).await;
        });
        return HttpResponse::Ok()
            .insert_header((header::CONTENT_TYPE, "text/event-stream"))
            .insert_header((header::CACHE_CONTROL, "no-cache"))
            .streaming(
                ReceiverStream::new(receiver)
                    .map(|data| Ok::<_, actix_web::Error>(Bytes::from(data))),
            );
    }
    drop(replay);

    let response = dispatch_plane_rpc(&body, &token, &grant, presented_session, plane_config).await;

    let mut replay = replay_state.lock().await;
    if let Some(session_id) = response.mcp_session_id.as_ref() {
        record_session(&mut replay, session_id, &body, &grant);
    }
    finish_replay(&mut replay, request_id, fingerprint, response.clone());
    cached_rpc_response(response)
}

fn finish_replay(
    replay: &mut GrantReplayState,
    key: String,
    fingerprint: String,
    response: CachedRpcResponse,
) {
    replay.order.push_back(key.clone());
    replay.entries.insert(
        key,
        CachedRpcEntry {
            fingerprint,
            response: Some(response),
        },
    );
    while replay.order.len() > MAX_RPC_REPLAYS_PER_GRANT {
        if let Some(oldest) = replay.order.pop_front() {
            replay.entries.remove(&oldest);
        }
    }
}

/// GET `/api/magician/v2/plane/mcp` — optional session-scoped notifications.
pub async fn plane_mcp_sse_handler(
    req: HttpRequest,
    auth: Option<actix_web::web::Data<AuthRuntime>>,
) -> HttpResponse {
    ensure_runtime_forget_installed();
    let token = match bearer_token(&req) {
        Some(token) => token,
        None => return HttpResponse::Unauthorized().json(json!({"error": "missing_grant"})),
    };
    match resolve_plane_grant(&token, auth.as_ref().map(|d| d.get_ref())).await {
        Ok(Some(_)) => {},
        Ok(None) => {
            forget_grant_runtime_state(&token).await;
            return HttpResponse::Unauthorized().json(json!({"error": "invalid_grant"}));
        },
        Err(response) => return response,
    }

    let Some(session) = presented_mcp_session(&req) else {
        return HttpResponse::BadRequest().json(json!({"error":"missing_mcp_session"}));
    };
    let Some(replay) = replay_state_for(&token).await else {
        return HttpResponse::ServiceUnavailable().finish();
    };
    if !replay.lock().await.sessions.contains_key(session) {
        return HttpResponse::NotFound().json(json!({"error":"invalid_mcp_session"}));
    }
    let (sender, rx) = mpsc::channel(32);
    let stream_id = Uuid::new_v4().to_string();
    sse_by_grant()
        .lock()
        .await
        .entry(token.clone())
        .or_default()
        .insert(
            session.into(),
            NotificationSender {
                id: stream_id.clone(),
                sender,
            },
        );
    let stream = PlaneSseStream {
        inner: ReceiverStream::new(rx),
        grant_token: token,
        session: session.into(),
        stream_id,
    };

    HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, "text/event-stream"))
        .insert_header((header::CACHE_CONTROL, "no-cache"))
        .streaming(stream)
}

/// DELETE `/api/magician/v2/plane/mcp` — streamable-HTTP session termination.
/// Ends the presented server-issued session: its id stops being accepted,
/// pending interactive calls bound to it are cancelled (as revocation cancels
/// them), its notification stream is dropped, and its terminal ledger is
/// released (the ledger is keyed by a session id nothing can present again
/// once the session has ended). The grant is untouched — a durable terminal
/// grant outlives every session it opens. Termination is idempotent, so a
/// retransmitted DELETE for an already-ended id is still 204.
pub async fn plane_mcp_delete_handler(
    req: HttpRequest,
    auth: Option<actix_web::web::Data<AuthRuntime>>,
) -> HttpResponse {
    ensure_runtime_forget_installed();
    let token = match bearer_token(&req) {
        Some(token) => token,
        None => return HttpResponse::Unauthorized().json(json!({"error": "missing_grant"})),
    };
    match resolve_plane_grant(&token, auth.as_ref().map(|d| d.get_ref())).await {
        Ok(Some(_)) => {},
        Ok(None) => {
            forget_grant_runtime_state(&token).await;
            return HttpResponse::Unauthorized().json(json!({"error": "invalid_grant"}));
        },
        Err(response) => return response,
    }
    let Some(session) = presented_mcp_session(&req) else {
        return HttpResponse::BadRequest().json(json!({"error":"missing_mcp_session"}));
    };
    // Look the window up without creating one: a DELETE for a grant that
    // never initialized has nothing to end.
    let replay = replay_by_grant().lock().await.get(&token).cloned();
    let mut ended = false;
    if let Some(replay) = replay {
        let mut replay = replay.lock().await;
        ended = replay.sessions.remove(session).is_some();
        for call in replay
            .active
            .values()
            .filter(|call| call.session == session)
        {
            call.cancelled.cancel();
        }
    }
    {
        let mut streams = sse_by_grant().lock().await;
        if let Some(sessions) = streams.get_mut(&token) {
            sessions.remove(session);
            if sessions.is_empty() {
                streams.remove(&token);
            }
        }
    }
    // Only a session this grant's window knew can have a ledger this grant
    // may release: ledger entries are recorded solely for sessions the POST
    // door validated against the window, and a presented id the window never
    // held is not this grant's to clear.
    if ended {
        let _ = magician::magician_v2::execution::plane::terminal_ledger::take(session);
    }
    HttpResponse::NoContent().finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test as actix_test;
    use actix_web::{http::StatusCode, web, App};
    use magician::magician_v2::execution::plane::PlaneGrant;

    /// One JSON-RPC POST at the door: `initialize` (the session id comes back
    /// in the header) or a call on an established session. A macro: the actix
    /// test service's type is an unnameable `impl Service<…>`.
    macro_rules! rpc {
        ($app:expr, $bearer:expr, $session:expr, $id:expr, $method:expr, $params:expr) => {{
            let mut req = actix_test::TestRequest::post()
                .uri("/plane/mcp")
                .insert_header(("Authorization", format!("Bearer {}", $bearer)));
            if let Some(session) = $session {
                req = req.insert_header(("mcp-session-id", session));
            }
            let response = actix_test::call_service(
                &$app,
                req.set_json(json!({
                    "jsonrpc": "2.0", "id": $id, "method": $method, "params": $params
                }))
                .to_request(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            response
        }};
    }

    /// Tests that install the process-global runless executors run one at a
    /// time and put back whatever the slot held before them: a durable door
    /// test asserts what the door does before any are installed, which a
    /// concurrent or leaked install from another test would falsify.
    static RUNLESS_INSTALL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct RunlessInstallScope {
        _serialized: std::sync::MutexGuard<'static, ()>,
        previous: Option<Arc<magician::magician_v2::execution::agentic::ActionExecutors>>,
    }

    impl RunlessInstallScope {
        fn enter() -> Self {
            let serialized = RUNLESS_INSTALL
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let previous = magician::magician_v2::execution::plane::take_runless_executors();
            Self {
                _serialized: serialized,
                previous,
            }
        }
    }

    impl Drop for RunlessInstallScope {
        fn drop(&mut self) {
            magician::magician_v2::execution::plane::take_runless_executors();
            if let Some(previous) = self.previous.take() {
                magician::magician_v2::execution::plane::install_runless_executors(previous);
            }
        }
    }

    /// The door's two catalog halves applied in sequence, as the door does.
    fn require_terminal_catalog(
        result: anyhow::Result<Arc<ToolIndex>>,
    ) -> Result<Arc<ToolIndex>, HttpResponse> {
        result
            .map_err(terminal_catalog_resolution_failed)
            .and_then(admit_terminal_catalog)
    }

    async fn assert_terminal_catalog_unavailable(response: HttpResponse) {
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body,
            json!({
                "error": "tool_catalog_unavailable",
                "message": "The runtime tool catalog is unavailable; retry when the runtime is ready.",
            })
        );
    }

    #[actix_web::test]
    async fn terminal_catalog_resolver_failure_returns_503_without_internal_error_details() {
        let result = require_terminal_catalog(Err(anyhow::anyhow!(
            "failed reading /private/workspace/capabilities: internal resolver diagnostic"
        )));
        assert_terminal_catalog_unavailable(result.expect_err("failed resolution must refuse"))
            .await;
    }

    #[actix_web::test]
    async fn terminal_catalog_empty_index_returns_503_instead_of_an_inert_catalog() {
        let result = require_terminal_catalog(Ok(Arc::new(ToolIndex::default())));
        assert_terminal_catalog_unavailable(result.expect_err("empty catalog must refuse")).await;
    }

    #[test]
    fn terminal_catalog_accepts_the_next_healthy_snapshot_after_failure() {
        use magician::magician_v2::execution::compiled_providers::embedded_compiled_pack_defs_ref;
        use magician::magician_v2::execution::flat_loop::{build_tool_index, ToolIndex};

        assert!(require_terminal_catalog(Err(anyhow::anyhow!("temporarily unavailable"))).is_err());
        assert!(require_terminal_catalog(Ok(Arc::new(ToolIndex::default()))).is_err());
        let index = Arc::new(build_tool_index(embedded_compiled_pack_defs_ref()));
        let admitted = require_terminal_catalog(Ok(Arc::clone(&index))).expect("catalog recovered");
        assert!(
            Arc::ptr_eq(&admitted, &index),
            "retain the canonical snapshot and schemas"
        );
    }

    #[actix_web::test]
    async fn durable_terminal_catalog_has_schemas_and_keeps_deferred_loading_across_requests() {
        use magician::magician_v2::agents::{
            AgentDefinitionStore, AgentMemoryResolver, AgentStorage,
        };
        use magician::magician_v2::artifact_v2::{
            workspace::ArtifactV2Workspace, CapabilityWorkspaceManager,
        };
        use magician::magician_v2::auth::{AuthStore, CredentialKind, MintGrantSpec};
        use magician::magician_v2::execution::agent_resources::AgentResources;
        use magician::magician_v2::execution::agentic::ActionExecutors;
        use magician::magician_v2::execution::compiled_providers::embedded_compiled_pack_defs_ref;
        use magician::magician_v2::execution::flat_loop::build_tool_index;
        use magician::magician_v2::execution::plane::catalog::PLANE_CONTROL_HOT;
        use magician::magician_v2::execution::{
            ExecutionConfig, MagicutorClient, ScopedCapabilityResolver,
        };
        use magician::magician_v2::prompts::json_storage::JsonStorageConfig;
        use magician::magician_v2::prompts::{JsonPromptStorage, PromptManager};
        use magician::magician_v2::test_utils::ConfigurableMockLlm;
        use std::sync::RwLock;

        let _installer = RunlessInstallScope::enter();
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(AuthStore::open(dir.path()).unwrap());
        store
            .create_identity(
                "owner",
                "Owner",
                CredentialKind::Password {
                    hash: "fixture".into(),
                },
            )
            .unwrap();
        let auth = crate::auth_api::auth_runtime(store.clone(), Default::default());
        let spec = MintGrantSpec {
            label: "terminal catalog regression".into(),
            workspace: "default".into(),
            agent_identity: "personal-assistant".into(),
            harness_engine: "magician".into(),
            allowed_tools: Vec::new(),
            ttl_hours: 1,
            max_usd: None,
            max_wall_clock_secs: None,
            max_concurrent_runs: None,
        };
        let (row, token) = store.mint_grant("owner", spec.clone()).unwrap();
        let (_, other_token) = store.mint_grant("owner", spec).unwrap();

        // A valid durable bearer must not silently become an empty-index grant.
        let unavailable = resolve_plane_grant(&token, Some(auth.get_ref())).await;
        assert_terminal_catalog_unavailable(unavailable.err().unwrap()).await;
        let index = Arc::new(build_tool_index(embedded_compiled_pack_defs_ref()));
        let resources = Arc::new(AgentResources {
            magician_config: Arc::new(RwLock::new(Default::default())),
            memory_resolver: Arc::new(AgentMemoryResolver::new(dir.path())),
            agent_definition_store: Arc::new(AgentDefinitionStore::new(AgentStorage::new(
                dir.path(),
            ))),
            artifact_workspace: ArtifactV2Workspace::new(dir.path()),
            artifact_v2_service: None,
            event_broadcaster: None,
            operation_llm_router: None,
            secret_store_resolver: None,
            content_acquisition_resolver: Arc::new(RwLock::new(None)),
            file_sandbox: Default::default(),
            tool_index: Arc::new(OnceLock::new()),
            user_request_service: None,
            agent_runtime: None,
        });
        let resolver = Arc::new(ScopedCapabilityResolver::new(
            Arc::new(CapabilityWorkspaceManager::new(
                ArtifactV2Workspace::new(dir.path()),
                dir.path(),
            )),
            Arc::new(MagicutorClient::new(ExecutionConfig::default()).unwrap()),
            Default::default(),
            Default::default(),
            None,
            None,
            None,
        ));
        resolver.set_agent_resources(Arc::clone(&resources));
        resolver.set_compiled_handlers(Arc::new(
            magician::magician_v2::execution::compiled_providers::default_compiled_handler_registry(
            ),
        ));
        assert!(
            resources.tool_index().is_none(),
            "first terminal connection must warm its own catalog"
        );
        let prompt_storage = JsonPromptStorage::new(JsonStorageConfig {
            storage_dir: dir.path().join("prompts"),
            ..Default::default()
        })
        .unwrap();
        let mut executors = ActionExecutors::new(
            Arc::new(ConfigurableMockLlm::with_response("{}")),
            Arc::new(PromptManager::new(Arc::new(prompt_storage))),
        );
        executors.capability_scope_resolver = Some(resolver);
        magician::magician_v2::execution::plane::install_runless_executors(Arc::new(executors));
        let app = actix_test::init_service(
            App::new().app_data(auth.clone()).service(
                web::resource("/plane/mcp")
                    .route(web::post().to(plane_mcp_handler))
                    .route(web::get().to(plane_mcp_sse_handler))
                    .route(web::delete().to(plane_mcp_delete_handler)),
            ),
        )
        .await;
        let init = rpc!(app, &token, None::<&str>, 1, "initialize", json!({}));
        let session = init
            .headers()
            .get("mcp-session-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let before: Value = actix_test::read_body_json(rpc!(
            app,
            &token,
            Some(session.as_str()),
            2,
            "tools/list",
            json!({})
        ))
        .await;
        let tools = before["result"]["tools"].as_array().unwrap();
        assert_eq!(
            tools.len(),
            PLANE_CONTROL_HOT.len() - 2,
            "input tools require the form capability"
        );
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            if matches!(name, "tool_search" | "session_ledger") {
                assert!(tool["inputSchema"]["properties"].is_object(), "{tool}");
            } else {
                let canonical = index
                    .get(name)
                    .expect("every hot tool has a canonical definition");
                assert_eq!(tool["inputSchema"], canonical.parameters_schema, "{name}");
                assert_eq!(tool["description"], canonical.description, "{name}");
            }
        }
        assert!(tools.iter().all(|tool| tool["name"] != "read_file"));
        let search: Value = actix_test::read_body_json(rpc!(
            app,
            &token,
            Some(session.as_str()),
            3,
            "tools/call",
            json!({
                "name": "tool_search", "arguments": {"query": "+read_file"}
            })
        ))
        .await;
        let hits: Value =
            serde_json::from_str(search["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert!(hits["matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "read_file"));
        assert_eq!(search["result"]["_meta"]["toolsListChanged"], false);
        let unloaded = resolve_plane_grant(&token, Some(auth.get_ref()))
            .await
            .unwrap()
            .unwrap();
        assert!(unloaded.loaded_tool_names().is_empty());

        let selected: Value = actix_test::read_body_json(rpc!(
            app,
            &token,
            Some(session.as_str()),
            4,
            "tools/call",
            json!({
                "name": "tool_search", "arguments": {"query": "select:read_file"}
            })
        ))
        .await;
        assert_eq!(selected["result"]["_meta"]["toolsListChanged"], true);
        // GET also revalidates the durable grant; it must retain the same index.
        let stream = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri("/plane/mcp")
                .insert_header(("Authorization", format!("Bearer {token}")))
                .insert_header(("mcp-session-id", session.as_str()))
                .to_request(),
        )
        .await;
        assert_eq!(stream.status(), StatusCode::OK);
        drop(stream);
        let after: Value = actix_test::read_body_json(rpc!(
            app,
            &token,
            Some(session.as_str()),
            5,
            "tools/list",
            json!({})
        ))
        .await;
        let loaded = after["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "read_file")
            .unwrap();
        assert_eq!(
            loaded["inputSchema"],
            index.get("read_file").unwrap().parameters_schema
        );
        let other: Value = actix_test::read_body_json(rpc!(
            app,
            &other_token,
            None::<&str>,
            1,
            "tools/list",
            json!({})
        ))
        .await;
        assert!(other["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| tool["name"] != "read_file"));

        store.revoke_grant("owner", row.id).unwrap();
        assert!(resolve_plane_grant(&token, Some(auth.get_ref()))
            .await
            .unwrap()
            .is_none());
        assert!(unloaded.is_revoked());
        plane_grant_registry().revoke(&other_token).await;
    }

    /// A runless durable grant must reach a scope-bound compiled handler with
    /// its scope injected. `get_task_details` needs only `__principal` and
    /// `__workspace`, checks them before anything else, and then reports the
    /// fixture's missing task service — so that report is the proof the
    /// handler ran past its scope check from a grant that owns no run.
    #[actix_web::test]
    async fn durable_terminal_grant_dispatches_a_scope_bound_compiled_tool() {
        use magician::magician_v2::agents::{
            AgentDefinitionStore, AgentMemoryResolver, AgentStorage,
        };
        use magician::magician_v2::artifact_v2::{
            workspace::ArtifactV2Workspace, CapabilityWorkspaceManager,
        };
        use magician::magician_v2::auth::{AuthStore, CredentialKind, MintGrantSpec};
        use magician::magician_v2::execution::agent_resources::AgentResources;
        use magician::magician_v2::execution::agentic::ActionExecutors;
        use magician::magician_v2::execution::{
            ExecutionConfig, MagicutorClient, ScopedCapabilityResolver,
        };
        use magician::magician_v2::prompts::json_storage::JsonStorageConfig;
        use magician::magician_v2::prompts::{JsonPromptStorage, PromptManager};
        use magician::magician_v2::test_utils::ConfigurableMockLlm;
        use std::sync::RwLock;

        let _installer = RunlessInstallScope::enter();
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(AuthStore::open(dir.path()).unwrap());
        store
            .create_identity(
                "owner",
                "Owner",
                CredentialKind::Password {
                    hash: "fixture".into(),
                },
            )
            .unwrap();
        let auth = crate::auth_api::auth_runtime(store.clone(), Default::default());
        let (row, token) = store
            .mint_grant(
                "owner",
                MintGrantSpec {
                    label: "scoped runless dispatch".into(),
                    workspace: "default".into(),
                    agent_identity: "personal-assistant".into(),
                    harness_engine: "magician".into(),
                    allowed_tools: vec!["get_task_details".into()],
                    ttl_hours: 1,
                    max_usd: None,
                    max_wall_clock_secs: None,
                    max_concurrent_runs: None,
                },
            )
            .unwrap();
        let resources = Arc::new(AgentResources {
            magician_config: Arc::new(RwLock::new(Default::default())),
            memory_resolver: Arc::new(AgentMemoryResolver::new(dir.path())),
            agent_definition_store: Arc::new(AgentDefinitionStore::new(AgentStorage::new(
                dir.path(),
            ))),
            artifact_workspace: ArtifactV2Workspace::new(dir.path()),
            artifact_v2_service: None,
            event_broadcaster: None,
            operation_llm_router: None,
            secret_store_resolver: None,
            content_acquisition_resolver: Arc::new(RwLock::new(None)),
            file_sandbox: Default::default(),
            tool_index: Arc::new(OnceLock::new()),
            user_request_service: None,
            agent_runtime: None,
        });
        let resolver = Arc::new(ScopedCapabilityResolver::new(
            Arc::new(CapabilityWorkspaceManager::new(
                ArtifactV2Workspace::new(dir.path()),
                dir.path(),
            )),
            Arc::new(MagicutorClient::new(ExecutionConfig::default()).unwrap()),
            Default::default(),
            Default::default(),
            None,
            None,
            None,
        ));
        resolver.set_agent_resources(Arc::clone(&resources));
        resolver.set_compiled_handlers(Arc::new(
            magician::magician_v2::execution::compiled_providers::default_compiled_handler_registry(
            ),
        ));
        let prompt_storage = JsonPromptStorage::new(JsonStorageConfig {
            storage_dir: dir.path().join("prompts"),
            ..Default::default()
        })
        .unwrap();
        let mut executors = ActionExecutors::new(
            Arc::new(ConfigurableMockLlm::with_response("{}")),
            Arc::new(PromptManager::new(Arc::new(prompt_storage))),
        );
        executors.capability_scope_resolver = Some(resolver);
        executors.artifact_v2_workspace = ArtifactV2Workspace::new(dir.path());
        executors.taskplan_base_path = dir.path().to_path_buf();
        executors.api_mining_base_path = dir.path().join("api_mining");
        magician::magician_v2::execution::plane::install_runless_executors(Arc::new(executors));

        // The door hands the grant its own scoped copy: the allowlist narrows
        // the registry, and the shared boot executors stay unscoped.
        let grant = resolve_plane_grant(&token, Some(auth.get_ref()))
            .await
            .unwrap()
            .unwrap();
        let scoped = grant
            .executors
            .as_ref()
            .expect("the door attaches executors");
        let registry = scoped
            .effective_capability_registry_snapshot()
            .expect("the grant's executors carry the scope's registry");
        assert!(registry.get("get_task_details").is_some());
        assert!(
            registry.get("read_file").is_none(),
            "the allowlist narrows the grant's registry"
        );
        let shared = magician::magician_v2::execution::plane::runless_executors().unwrap();
        assert!(!Arc::ptr_eq(scoped, &shared));
        assert!(shared.effective_capability_registry_snapshot().is_none());
        let identity = scoped.run_identity.get();
        assert!(grant.ctx.principal.is_some());
        assert_eq!(
            (identity.principal.clone(), identity.workspace.clone()),
            (grant.ctx.principal.clone(), grant.ctx.workspace.clone()),
            "the grant's identity is seeded for the flat compiled path"
        );
        assert_eq!(identity.agent_id.as_deref(), Some("personal-assistant"));

        let app = actix_test::init_service(
            App::new().app_data(auth.clone()).service(
                web::resource("/plane/mcp")
                    .route(web::post().to(plane_mcp_handler))
                    .route(web::get().to(plane_mcp_sse_handler))
                    .route(web::delete().to(plane_mcp_delete_handler)),
            ),
        )
        .await;
        let init = rpc!(app, &token, None::<&str>, 1, "initialize", json!({}));
        let session = init
            .headers()
            .get("mcp-session-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let called: Value = actix_test::read_body_json(rpc!(
            app,
            &token,
            Some(session.as_str()),
            2,
            "tools/call",
            json!({
                "name": "get_task_details",
                "arguments": {"task_id": "task_does_not_exist"}
            })
        ))
        .await;
        let text = called["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("a text result, got {called}"));
        assert!(
            !text.contains("scoped principal")
                && !text.contains("__principal")
                && !text.contains("missing scope param"),
            "scope must reach the handler: {text}"
        );
        assert!(
            text.contains("ArtifactV2Service is not configured"),
            "the handler ran past its scope check: {text}"
        );

        store.revoke_grant("owner", row.id).unwrap();
        assert!(resolve_plane_grant(&token, Some(auth.get_ref()))
            .await
            .unwrap()
            .is_none());
    }

    #[test]
    fn render_chat_config_block_names_the_engine() {
        let block = render_chat_config_block(&MagicianChatTurnSettings {
            harness_engine: "claude_code".to_string(),
            harness_model: "default".to_string(),
            harness_turn_max_tool_calls: 4000,
            harness_turn_max_seconds: 2400,
        });
        assert!(block.starts_with("chat:"));
        assert!(block.contains("harness_engine: claude_code"));
        assert!(block.contains("harness_turn_max_tool_calls: 4000"));
    }

    #[test]
    fn launchable_chat_engine_accepts_roster_and_magician() {
        assert!(launchable_chat_engine("magician"));
        assert!(launchable_chat_engine("pi"));
        assert!(launchable_chat_engine("claude_code"));
        assert!(launchable_chat_engine("grok"));
        assert!(launchable_chat_engine("codex"));
        assert!(launchable_chat_engine("codex_app_server"));
        assert!(launchable_chat_engine("agy"));
        assert!(!launchable_chat_engine("not-an-engine"));
        assert!(!launchable_chat_engine(""));
    }

    #[test]
    fn initialize_has_no_bearer_or_client_header_fallback_for_session_id() {
        let source = include_str!("plane_api.rs");
        let implementation = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(implementation.contains("format!(\"pltsess_{}\""));
        assert!(
            !implementation.contains("unwrap_or(&token)"),
            "the bearer must never become mcp-session-id"
        );
        let initialize = implementation
            .split("\"initialize\" =>")
            .nth(1)
            .and_then(|tail| tail.split("\"tools/list\" =>").next())
            .expect("initialize branch");
        assert!(
            !initialize.contains("headers()"),
            "initialize must not reflect a client-supplied session id"
        );
    }

    #[test]
    fn json_rpc_ids_are_scoped_to_the_server_session_header() {
        let body = json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}});
        let first = actix_test::TestRequest::post()
            .insert_header(("mcp-session-id", "pltsess_first"))
            .to_http_request();
        let second = actix_test::TestRequest::post()
            .insert_header(("mcp-session-id", "pltsess_second"))
            .to_http_request();
        assert_ne!(
            scoped_replay_request_key(&first, &body),
            scoped_replay_request_key(&second, &body),
            "JSON-RPC ids are reusable in independent MCP sessions"
        );
    }

    #[actix_web::test]
    async fn the_endpoint_refuses_a_request_without_a_valid_grant() {
        let app = actix_test::init_service(
            App::new()
                .service(web::resource("/plane/mcp").route(web::post().to(plane_mcp_handler))),
        )
        .await;
        let req = actix_test::TestRequest::post()
            .uri("/plane/mcp")
            .set_json(json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}))
            .to_request();
        let response = actix_test::call_service(&app, req).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn a_valid_grant_without_executors_advertises_no_unrunnable_tools() {
        let token = plane_grant_registry()
            .mint(PlaneGrant::for_test("exec-list"))
            .await;
        let app = actix_test::init_service(
            App::new()
                .service(web::resource("/plane/mcp").route(web::post().to(plane_mcp_handler))),
        )
        .await;
        let req = actix_test::TestRequest::post()
            .uri("/plane/mcp")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}))
            .to_request();
        let response = actix_test::call_service(&app, req).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = actix_test::read_body_json(response).await;
        assert_eq!(body["result"]["tools"], json!([]));
        plane_grant_registry().revoke(&token).await;
    }

    #[actix_web::test]
    async fn chat_turn_conversation_grant_resolves_at_the_mcp_door() {
        use magician::magician_v2::agents::{
            AgentInvocationContext, FeatureMode, InvocationSourceKind, InvocationSurface,
        };
        use tokio_util::sync::CancellationToken;

        let invocation = AgentInvocationContext {
            principal: "owner".to_string(),
            workspace: "home".to_string(),
            source_agent_id: None,
            target_agent_id: "personal-assistant".to_string(),
            surface: InvocationSurface::Chat,
            feature_mode: FeatureMode::None,
            source_kind: InvocationSourceKind::Direct,
            chat_session_id: Some("sess-chat-mcp".to_string()),
            chat_turn_id: Some("turn-1".to_string()),
        };
        let mut ctx = AgenticContext::default();
        ctx.invocation_context_override = Some(invocation.clone());
        ctx.principal = Some(invocation.principal);
        ctx.workspace = Some(invocation.workspace);
        ctx.agent_id = Some(invocation.target_agent_id);
        ctx.chat_session_id = invocation.chat_session_id;
        let token = plane_grant_registry()
            .mint_chat(PlaneGrant::for_conversation(
                ctx,
                "sess-chat-mcp".to_string(),
                CancellationToken::new(),
            ))
            .await;
        let app = actix_test::init_service(
            App::new()
                .service(web::resource("/plane/mcp").route(web::post().to(plane_mcp_handler))),
        )
        .await;
        let req = actix_test::TestRequest::post()
            .uri("/plane/mcp")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}))
            .to_request();
        let response = actix_test::call_service(&app, req).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "ChatScoped grants must resolve like process-local run grants"
        );
        plane_grant_registry().revoke(&token).await;
    }

    #[actix_web::test]
    async fn initialize_advertises_list_changed() {
        let token = plane_grant_registry()
            .mint(PlaneGrant::for_test("exec-init"))
            .await;
        let app = actix_test::init_service(
            App::new()
                .service(web::resource("/plane/mcp").route(web::post().to(plane_mcp_handler))),
        )
        .await;
        let req = actix_test::TestRequest::post()
            .uri("/plane/mcp")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .insert_header(("mcp-session-id", token.clone()))
            .set_json(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
            .to_request();
        let response = actix_test::call_service(&app, req).await;
        assert_eq!(response.status(), StatusCode::OK);
        let session_id = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .expect("server session id")
            .to_string();
        assert!(session_id.starts_with("pltsess_"));
        assert_ne!(session_id, token, "initialize reflected the bearer token");
        let body: Value = actix_test::read_body_json(response).await;
        assert_eq!(
            body["result"]["capabilities"]["tools"]["listChanged"],
            json!(true)
        );
        plane_grant_registry().revoke(&token).await;
    }

    #[actix_web::test]
    async fn same_grant_request_id_replays_once_and_conflicting_reuse_fails_closed() {
        let token = plane_grant_registry()
            .mint(PlaneGrant::for_test("exec-replay"))
            .await;
        let app = actix_test::init_service(
            App::new()
                .service(web::resource("/plane/mcp").route(web::post().to(plane_mcp_handler))),
        )
        .await;
        let request = || {
            actix_test::TestRequest::post()
                .uri("/plane/mcp")
                .insert_header(("Authorization", format!("Bearer {token}")))
                .set_json(json!({"jsonrpc":"2.0","id":"same","method":"initialize","params":{}}))
                .to_request()
        };
        let first = actix_test::call_service(&app, request()).await;
        let first_session = first
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .expect("first session")
            .to_string();
        let _: Value = actix_test::read_body_json(first).await;
        let replay = actix_test::call_service(&app, request()).await;
        let replay_session = replay
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .expect("replayed session")
            .to_string();
        assert_ne!(
            replay_session, first_session,
            "independent initializes must not share a session"
        );

        let session_request = actix_test::TestRequest::post()
            .uri("/plane/mcp")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .insert_header(("mcp-session-id", first_session.clone()))
            .set_json(json!({"jsonrpc":"2.0","id":"call","method":"tools/list","params":{}}))
            .to_request();
        let session_response = actix_test::call_service(&app, session_request).await;
        let _: Value = actix_test::read_body_json(session_response).await;

        let conflicting = actix_test::TestRequest::post()
            .uri("/plane/mcp")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .insert_header(("mcp-session-id", first_session))
            .set_json(json!({
                "jsonrpc":"2.0",
                "id":"call",
                "method":"tools/call",
                "params":{"name":"create_task","arguments":{}}
            }))
            .to_request();
        let conflict = actix_test::call_service(&app, conflicting).await;
        let conflict_body: Value = actix_test::read_body_json(conflict).await;
        assert_eq!(conflict_body["error"]["code"], json!(-32600));
        plane_grant_registry().revoke(&token).await;
        forget_grant_runtime_state(&token).await;
    }

    #[actix_web::test]
    async fn tools_call_without_executors_is_an_error_not_a_success() {
        let token = plane_grant_registry()
            .mint(PlaneGrant::for_test("exec-call"))
            .await;
        let app = actix_test::init_service(
            App::new()
                .service(web::resource("/plane/mcp").route(web::post().to(plane_mcp_handler))),
        )
        .await;
        let initialize = actix_test::TestRequest::post()
            .uri("/plane/mcp")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(json!({"jsonrpc":"2.0","id":"init","method":"initialize","params":{}}))
            .to_request();
        let initialized = actix_test::call_service(&app, initialize).await;
        let session_id = initialized
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .expect("server session id")
            .to_string();
        let _: Value = actix_test::read_body_json(initialized).await;
        let req = actix_test::TestRequest::post()
            .uri("/plane/mcp")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .insert_header(("mcp-session-id", session_id))
            .set_json(json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"tools/call",
                "params":{"name":"create_task","arguments":{}}
            }))
            .to_request();
        let response = actix_test::call_service(&app, req).await;
        let body: Value = actix_test::read_body_json(response).await;
        assert_eq!(body["result"]["isError"], json!(true));
        assert_eq!(body["result"]["_meta"]["planeDispatch"], json!("unwired"));
        plane_grant_registry().revoke(&token).await;
    }

    #[actix_web::test]
    async fn tools_call_refuses_a_client_invented_session_namespace() {
        let token = plane_grant_registry()
            .mint(PlaneGrant::for_test("exec-forged-session"))
            .await;
        let app = actix_test::init_service(
            App::new()
                .service(web::resource("/plane/mcp").route(web::post().to(plane_mcp_handler))),
        )
        .await;
        let req = actix_test::TestRequest::post()
            .uri("/plane/mcp")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .insert_header(("mcp-session-id", "pltsess_client_invented"))
            .set_json(json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"tools/call",
                "params":{"name":"create_task","arguments":{}}
            }))
            .to_request();
        let response = actix_test::call_service(&app, req).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value = actix_test::read_body_json(response).await;
        assert_eq!(body["error"]["code"], json!(-32003));
        plane_grant_registry().revoke(&token).await;
        forget_grant_runtime_state(&token).await;
    }

    #[actix_web::test]
    async fn tools_call_cannot_bypass_replay_by_omitting_the_request_id() {
        let token = plane_grant_registry()
            .mint(PlaneGrant::for_test("exec-notification-call"))
            .await;
        let app = actix_test::init_service(
            App::new()
                .service(web::resource("/plane/mcp").route(web::post().to(plane_mcp_handler))),
        )
        .await;
        let req = actix_test::TestRequest::post()
            .uri("/plane/mcp")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(json!({
                "jsonrpc":"2.0",
                "method":"tools/call",
                "params":{"name":"create_task","arguments":{}}
            }))
            .to_request();
        let response = actix_test::call_service(&app, req).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value = actix_test::read_body_json(response).await;
        assert_eq!(body["error"]["code"], json!(-32600));
        plane_grant_registry().revoke(&token).await;
        forget_grant_runtime_state(&token).await;
    }

    /// Streamable-HTTP clients end a session with DELETE. The door ends the
    /// named session (its id stops being valid, its bound pending calls are
    /// cancelled, its notification stream is dropped) and answers 204; the
    /// grant itself outlives the session.
    #[actix_web::test]
    async fn delete_ends_the_named_session_and_keeps_the_grant() {
        let token = plane_grant_registry()
            .mint(PlaneGrant::for_test("exec-delete-session"))
            .await;
        let app = actix_test::init_service(
            App::new().service(
                web::resource("/plane/mcp")
                    .route(web::post().to(plane_mcp_handler))
                    .route(web::delete().to(plane_mcp_delete_handler)),
            ),
        )
        .await;
        let initialize = || {
            actix_test::TestRequest::post()
                .uri("/plane/mcp")
                .insert_header(("Authorization", format!("Bearer {token}")))
                .set_json(json!({"jsonrpc":"2.0","id":"init","method":"initialize","params":{}}))
                .to_request()
        };
        let initialized = actix_test::call_service(&app, initialize()).await;
        let session_id = initialized
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .expect("server session id")
            .to_string();
        let _: Value = actix_test::read_body_json(initialized).await;
        let other = actix_test::call_service(&app, initialize()).await;
        let other_session = other
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .expect("second session id")
            .to_string();
        let _: Value = actix_test::read_body_json(other).await;

        // One governed call on each session so each has a terminal ledger
        // entry (the test grant has no executors, so the call settles as an
        // honest-unwired error — still a recorded outcome).
        for (rpc_id, session) in [("call-ended", &session_id), ("call-kept", &other_session)] {
            let call = actix_test::TestRequest::post()
                .uri("/plane/mcp")
                .insert_header(("Authorization", format!("Bearer {token}")))
                .insert_header(("mcp-session-id", session.clone()))
                .set_json(json!({
                    "jsonrpc":"2.0",
                    "id":rpc_id,
                    "method":"tools/call",
                    "params":{"name":"create_task","arguments":{}}
                }))
                .to_request();
            let response = actix_test::call_service(&app, call).await;
            assert_eq!(response.status(), StatusCode::OK);
            let _: Value = actix_test::read_body_json(response).await;
        }
        use magician::magician_v2::execution::plane::terminal_ledger;
        assert_eq!(
            terminal_ledger::entries(&session_id).len(),
            1,
            "the session being ended has a ledger entry to release"
        );
        assert_eq!(terminal_ledger::entries(&other_session).len(), 1);

        // Pending interactive calls bound to each session, plus a
        // notification stream on the session being ended.
        let (ended_call, _ended_rx) =
            CallState::new(session_id.clone(), json!(1), "request_user_input".into());
        let (kept_call, _kept_rx) =
            CallState::new(other_session.clone(), json!(2), "request_user_input".into());
        {
            let replay = replay_state_for(&token).await.expect("replay window");
            let mut replay = replay.lock().await;
            replay.active.insert("ended".into(), ended_call.clone());
            replay.active.insert("kept".into(), kept_call.clone());
        }
        let (sender, _rx) = mpsc::channel(4);
        sse_by_grant()
            .lock()
            .await
            .entry(token.clone())
            .or_default()
            .insert(
                session_id.clone(),
                NotificationSender {
                    id: "stream".into(),
                    sender,
                },
            );

        let unauthorized = actix_test::TestRequest::delete()
            .uri("/plane/mcp")
            .insert_header(("mcp-session-id", session_id.clone()))
            .to_request();
        let response = actix_test::call_service(&app, unauthorized).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let no_session = actix_test::TestRequest::delete()
            .uri("/plane/mcp")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let response = actix_test::call_service(&app, no_session).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let ended = actix_test::TestRequest::delete()
            .uri("/plane/mcp")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .insert_header(("mcp-session-id", session_id.clone()))
            .to_request();
        let response = actix_test::call_service(&app, ended).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(ended_call.cancelled.is_cancelled());
        assert!(
            !kept_call.cancelled.is_cancelled(),
            "ending one session must not cancel another session's call"
        );
        assert!(
            !sse_by_grant()
                .lock()
                .await
                .get(&token)
                .is_some_and(|sessions| sessions.contains_key(&session_id)),
            "the ended session's notification stream must be dropped"
        );
        // The ended session's terminal ledger is released (a close-of-session
        // take finds nothing), while the live session's ledger is untouched.
        assert!(
            terminal_ledger::take(&session_id).is_empty(),
            "the ended session's ledger must be released"
        );
        assert_eq!(
            terminal_ledger::entries(&other_session).len(),
            1,
            "ending one session must not release another session's ledger"
        );

        // The ended id is now unknown: the door refuses it exactly as it
        // refuses any id it did not issue.
        let stale = actix_test::TestRequest::post()
            .uri("/plane/mcp")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .insert_header(("mcp-session-id", session_id.clone()))
            .set_json(json!({"jsonrpc":"2.0","id":"after","method":"tools/list","params":{}}))
            .to_request();
        let response = actix_test::call_service(&app, stale).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value = actix_test::read_body_json(response).await;
        assert_eq!(body["error"]["code"], json!(-32003));

        // The other session and the grant are untouched.
        let live = actix_test::TestRequest::post()
            .uri("/plane/mcp")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .insert_header(("mcp-session-id", other_session.clone()))
            .set_json(json!({"jsonrpc":"2.0","id":"live","method":"tools/list","params":{}}))
            .to_request();
        let response = actix_test::call_service(&app, live).await;
        assert_eq!(response.status(), StatusCode::OK);
        let _: Value = actix_test::read_body_json(response).await;
        assert!(
            plane_grant_registry().resolve(&token).await.is_some(),
            "session termination must not revoke the grant"
        );

        // Termination is idempotent: a retransmitted DELETE is still 204.
        let again = actix_test::TestRequest::delete()
            .uri("/plane/mcp")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .insert_header(("mcp-session-id", session_id))
            .to_request();
        let response = actix_test::call_service(&app, again).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Leave the process-global ledger as this test found it.
        let _ = terminal_ledger::take(&other_session);
        plane_grant_registry().revoke(&token).await;
        forget_grant_runtime_state(&token).await;
    }

    #[actix_web::test]
    async fn sse_without_a_grant_is_unauthorized() {
        let app = actix_test::init_service(
            App::new()
                .service(web::resource("/plane/mcp").route(web::get().to(plane_mcp_sse_handler))),
        )
        .await;
        let req = actix_test::TestRequest::get()
            .uri("/plane/mcp")
            .to_request();
        let response = actix_test::call_service(&app, req).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn normal_sse_disconnect_cleans_sender_without_revoking_grant() {
        let token = plane_grant_registry()
            .mint(PlaneGrant::for_test("exec-sse-drop"))
            .await;
        let (sender, rx) = mpsc::channel(4);
        sse_by_grant()
            .lock()
            .await
            .entry(token.clone())
            .or_default()
            .insert(
                "session".into(),
                NotificationSender {
                    id: "stream".into(),
                    sender,
                },
            );
        let stream = PlaneSseStream {
            inner: ReceiverStream::new(rx),
            grant_token: token.clone(),
            session: "session".into(),
            stream_id: "stream".into(),
        };
        drop(stream);
        tokio::task::yield_now().await;

        assert!(!sse_by_grant().lock().await.contains_key(&token));
        assert!(
            plane_grant_registry().resolve(&token).await.is_some(),
            "transport disconnect must not revoke authorized execution authority"
        );
        plane_grant_registry().revoke(&token).await;
    }

    /// The bridged name whose bridge answers with the parent engine its job
    /// observed — what the native dispatcher behind a bridged name would
    /// route under — so a door test can see a session's family reach a call.
    const PARENT_PROBE_TOOL: &str = "create_chat_thread";

    fn with_parent_probe(grant: PlaneGrant) -> PlaneGrant {
        use futures_util::future::BoxFuture;
        use magician::magician_v2::execution::plane::ChatMouthBridge;
        use magician::magician_v2::query_analysis::parent_engine::current_parent_engine;
        use tokio_util::sync::CancellationToken;

        let bridge: ChatMouthBridge = Arc::new(
            |_call_id: String,
             _name: String,
             _arguments: Value,
             _cancel: CancellationToken|
             -> BoxFuture<'static, Value> {
                Box::pin(async move {
                    json!({"status": "ok", "parent_engine": current_parent_engine()})
                })
            },
        );
        let specs = std::iter::once((
            PARENT_PROBE_TOOL.to_string(),
            magicllm::types::LLMToolSpec {
                name: PARENT_PROBE_TOOL.to_string(),
                description: "reports the parent engine the call ran under".to_string(),
                parameters: json!({"type": "object"}),
            },
        ))
        .collect();
        grant.with_bridged_tools(specs, bridge)
    }

    /// The parent a grant names on its routing overrides, as the door reads
    /// it for every call.
    fn grant_parent(grant: &PlaneGrant) -> Option<String> {
        grant
            .ctx
            .llm_routing_overrides
            .as_ref()
            .and_then(|overrides| overrides.parent_engine.clone())
    }

    /// A conversation grant on the given chat mouth, as the chat service
    /// mints one for a harness turn.
    fn conversation_grant_on(engine: &str, session: &str) -> PlaneGrant {
        use magician::magician_v2::agents::{
            AgentInvocationContext, FeatureMode, InvocationSourceKind, InvocationSurface,
        };
        use tokio_util::sync::CancellationToken;

        let invocation = AgentInvocationContext {
            principal: "owner".to_string(),
            workspace: "home".to_string(),
            source_agent_id: None,
            target_agent_id: "personal-assistant".to_string(),
            surface: InvocationSurface::Chat,
            feature_mode: FeatureMode::None,
            source_kind: InvocationSourceKind::Direct,
            chat_session_id: Some(session.to_string()),
            chat_turn_id: Some("turn-1".to_string()),
        };
        let mut ctx = AgenticContext::default();
        ctx.invocation_context_override = Some(invocation.clone());
        ctx.principal = Some(invocation.principal);
        ctx.workspace = Some(invocation.workspace);
        ctx.agent_id = Some(invocation.target_agent_id);
        ctx.chat_session_id = invocation.chat_session_id;
        ctx.harness_engine = Some(engine.to_string());
        PlaneGrant::for_conversation(ctx, session.to_string(), CancellationToken::new())
    }

    /// `initialize` as a client of the given name; the minted session id.
    macro_rules! initialize_as {
        ($app:expr, $bearer:expr, $client_name:expr) => {{
            let client_name: &str = $client_name;
            let initialized = rpc!(
                $app,
                $bearer,
                None::<&str>,
                format!("init-{client_name}"),
                "initialize",
                json!({"clientInfo": {"name": client_name, "version": "1.0.0"}})
            );
            let session = initialized
                .headers()
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok())
                .expect("server session id")
                .to_string();
            let _: Value = actix_test::read_body_json(initialized).await;
            session
        }};
    }

    /// The parent engine the probe saw on one call of the session.
    macro_rules! probed_parent {
        ($app:expr, $bearer:expr, $session:expr, $id:expr) => {{
            let session: &str = $session;
            let called: Value = actix_test::read_body_json(rpc!(
                $app,
                $bearer,
                Some(session),
                $id,
                "tools/call",
                json!({"name": PARENT_PROBE_TOOL, "arguments": {}})
            ))
            .await;
            assert_eq!(called["result"]["isError"], json!(false), "{called}");
            let text = called["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_else(|| panic!("a text result, got {called}"));
            let value: Value = serde_json::from_str(text).expect("compact JSON");
            value["parent_engine"].clone()
        }};
    }

    /// An external MCP harness is the parent of the operations its calls
    /// trigger: `initialize` records the client's CLI family against the
    /// minted session and decides the parent on this machine's PATH, and
    /// every `tools/call` of that session on a terminal grant runs under
    /// what the record decided — seen from the job a bridged call spawns,
    /// where the native dispatcher would route. A client the roster does not
    /// know leaves the session without a parent, on the same grant.
    #[actix_web::test]
    async fn an_external_harness_session_is_the_parent_of_a_terminal_grants_calls() {
        use magician::magician_v2::execution::plane::terminal_ledger;

        let grant = with_parent_probe(PlaneGrant::for_test("exec-client-family"));
        assert!(is_terminal_grant(&grant));
        assert_eq!(
            grant_parent(&grant),
            None,
            "a terminal grant names no parent at mint"
        );
        let token = plane_grant_registry().mint(grant).await;
        let app = actix_test::init_service(
            App::new()
                .service(web::resource("/plane/mcp").route(web::post().to(plane_mcp_handler))),
        )
        .await;

        let claude = initialize_as!(app, &token, "claude-code");
        let unknown = initialize_as!(app, &token, "cursor");
        {
            let replay = replay_state_for(&token).await.expect("replay window");
            let mut replay = replay.lock().await;
            let recorded = replay
                .sessions
                .get(&claude)
                .expect("the session is recorded");
            assert_eq!(
                recorded.client_family,
                Some("claude_code"),
                "initialize records the client's family against its session"
            );
            // The door decided on this machine's PATH (the decision itself is
            // pinned with a fake probe below); the record is then set to what
            // a machine with the CLI decides, so the rest of this test is the
            // same everywhere.
            let decided_here =
                harness_cli_installed("claude_code").then(|| "claude_code".to_string());
            assert_eq!(recorded.parent_engine, decided_here);
            replay
                .sessions
                .get_mut(&claude)
                .expect("the session is recorded")
                .parent_engine = Some("claude_code".to_string());
            let unknown_record = replay
                .sessions
                .get(&unknown)
                .expect("the session is recorded");
            assert_eq!(unknown_record.client_family, None);
            assert_eq!(unknown_record.parent_engine, None);
        }

        assert_eq!(
            probed_parent!(app, &token, &claude, "call-claude"),
            json!("claude_code"),
            "the session's call runs under the client's family"
        );
        assert_eq!(
            probed_parent!(app, &token, &unknown, "call-unknown"),
            Value::Null,
            "an unknown client is no parent"
        );
        assert_eq!(
            probed_parent!(app, &token, &claude, "call-claude-again"),
            json!("claude_code"),
            "the family is a fact about the session, not the last request"
        );
        let projected = plane_grant_registry()
            .resolve(&token)
            .await
            .expect("the grant is still registered");
        assert_eq!(
            grant_parent(&projected),
            None,
            "the family rides the request's grant, never the registry's projection"
        );

        let _ = terminal_ledger::take(&claude);
        let _ = terminal_ledger::take(&unknown);
        plane_grant_registry().revoke(&token).await;
        forget_grant_runtime_state(&token).await;
    }

    /// A conversation grant already carries the chat mouth as its parent,
    /// and the grant kind decides: the session's client family neither
    /// replaces a harness mouth nor gives the native mouth one.
    #[actix_web::test]
    async fn a_conversation_grant_keeps_its_own_parent_over_the_session_family() {
        use magician::magician_v2::execution::plane::terminal_ledger;

        let app = actix_test::init_service(
            App::new()
                .service(web::resource("/plane/mcp").route(web::post().to(plane_mcp_handler))),
        )
        .await;
        for (mouth, expected) in [("grok", json!("grok")), ("magician", Value::Null)] {
            let grant = with_parent_probe(conversation_grant_on(mouth, "sess-chat-family"));
            assert!(!is_terminal_grant(&grant));
            let token = plane_grant_registry().mint_chat(grant).await;
            let session = initialize_as!(app, &token, "codex");
            assert_eq!(
                probed_parent!(app, &token, &session, "call"),
                expected,
                "the {mouth} mouth's grant ignores the session's family"
            );
            let _ = terminal_ledger::take(&session);
            plane_grant_registry().revoke(&token).await;
            forget_grant_runtime_state(&token).await;
        }
    }

    /// An `initialize` body naming the given client.
    fn initialize_body(client_name: &str) -> Value {
        json!({"params": {"clientInfo": {"name": client_name, "version": "2.0.0"}}})
    }

    /// A terminal grant engraved with the given harness at mint, as the
    /// durable door projects one.
    fn engraved_terminal_grant(execution_id: &str, harness_engine: &str) -> PlaneGrant {
        PlaneGrant::for_test(execution_id).with_run_authority(
            magician::magician_v2::execution::plane::PlaneRunAuthority {
                allowed_agents: Vec::new(),
                harness_engine: harness_engine.to_string(),
                max_usd: None,
                max_wall_clock: None,
                max_concurrent_runs: DEFAULT_TERMINAL_GRANT_CONCURRENT_RUNS,
            },
        )
    }

    /// The session's parent is decided at `initialize`: the operator's
    /// engraved harness outranks the connected client, the client's family
    /// stands in when the grant engraves the native pin, and either counts
    /// only when its CLI is installed here — an uninstalled engraved harness
    /// names no parent rather than yielding to the client. A run or
    /// conversation grant takes none, and the record reads the family off
    /// `clientInfo.name` and nothing else.
    #[test]
    fn a_session_parent_is_the_installed_engraved_harness_else_the_installed_client() {
        let everything: &dyn Fn(&str) -> bool = &|_| true;
        let nothing: &dyn Fn(&str) -> bool = &|_| false;
        let only_codex: &dyn Fn(&str) -> bool = &|engine| engine == "codex";
        let terminal = PlaneGrant::for_test("exec-decide");

        let claude =
            McpSession::from_initialize(&initialize_body("Claude Code"), &terminal, everything);
        assert_eq!(claude.client_family, Some("claude_code"));
        assert_eq!(claude.parent_engine.as_deref(), Some("claude_code"));
        assert_eq!(
            McpSession::from_initialize(&initialize_body("claude-code"), &terminal, nothing)
                .parent_engine,
            None,
            "a family whose CLI is not installed here is no parent"
        );
        assert_eq!(
            McpSession::from_initialize(&initialize_body("cursor"), &terminal, everything)
                .parent_engine,
            None,
            "an unknown client is no parent"
        );
        let no_client = McpSession::from_initialize(&json!({"params": {}}), &terminal, everything);
        assert_eq!(no_client.client_family, None, "no clientInfo is no family");
        assert_eq!(no_client.parent_engine, None);
        assert_eq!(
            McpSession::from_initialize(
                &json!({"params": {"clientInfo": {"name": 7}}}),
                &terminal,
                everything
            )
            .client_family,
            None,
            "a non-string name is no family"
        );
        assert!(
            McpSession::from_initialize(
                &json!({"params": {"capabilities": {"elicitation": {}}, "clientInfo": {"name": "codex"}}}),
                &terminal,
                everything
            )
            .capabilities
            .supports("form"),
            "the family rides beside the capabilities, not instead of them"
        );

        let engraved = engraved_terminal_grant("exec-engraved", "codex");
        assert_eq!(engraved_harness_engine(&engraved).as_deref(), Some("codex"));
        let operator =
            McpSession::from_initialize(&initialize_body("claude-code"), &engraved, everything);
        assert_eq!(
            operator.client_family,
            Some("claude_code"),
            "the client is still recorded"
        );
        assert_eq!(
            operator.parent_engine.as_deref(),
            Some("codex"),
            "the operator's engraved harness outranks the connected client"
        );
        assert_eq!(
            McpSession::from_initialize(&initialize_body("claude-code"), &engraved, only_codex)
                .parent_engine
                .as_deref(),
            Some("codex"),
            "the gate is on the engraved harness, not the client"
        );
        let uninstalled_codex: &dyn Fn(&str) -> bool = &|engine| engine == "claude_code";
        assert_eq!(
            McpSession::from_initialize(
                &initialize_body("claude-code"),
                &engraved,
                uninstalled_codex
            )
            .parent_engine,
            None,
            "an uninstalled engraved harness names no parent rather than yielding to the client"
        );
        let native_pin = engraved_terminal_grant("exec-native-pin", "magician");
        assert_eq!(
            engraved_harness_engine(&native_pin),
            None,
            "the native pin names no harness"
        );
        assert_eq!(
            McpSession::from_initialize(&initialize_body("codex"), &native_pin, everything)
                .parent_engine
                .as_deref(),
            Some("codex"),
            "under the native pin the client's family stands"
        );

        let conversation = conversation_grant_on("grok", "sess-decide");
        assert_eq!(
            McpSession::from_initialize(&initialize_body("codex"), &conversation, everything)
                .parent_engine,
            None,
            "a conversation grant takes no session parent"
        );
    }

    /// Only a terminal grant adopts a session's parent, and it keeps the
    /// endpoints its overrides already carried; a run grant keeps the run
    /// engine, a conversation grant the chat mouth.
    #[test]
    fn only_a_terminal_grant_adopts_the_session_family() {
        use magician::magician_v2::execution::agentic::ActionExecutors;
        use magician::magician_v2::prompts::json_storage::JsonStorageConfig;
        use magician::magician_v2::prompts::{JsonPromptStorage, PromptManager};
        use magician::magician_v2::query_analysis::operation_llm_router::{
            OperationRoutingEndpoint, OperationRoutingOverrides,
        };
        use magician::magician_v2::test_utils::ConfigurableMockLlm;

        let session = McpSession {
            capabilities: Default::default(),
            client_family: Some("claude_code"),
            parent_engine: Some("claude_code".to_string()),
        };

        let mut terminal = PlaneGrant::for_test("exec-adopt");
        terminal.ctx.llm_routing_overrides = Some(OperationRoutingOverrides {
            planning: OperationRoutingEndpoint::for_profile("eval-luna"),
            ..Default::default()
        });
        adopt_session_parent(&mut terminal, &session);
        let overrides = terminal
            .ctx
            .llm_routing_overrides
            .as_ref()
            .expect("the overrides survive");
        assert_eq!(overrides.parent_engine.as_deref(), Some("claude_code"));
        assert_eq!(
            overrides
                .planning
                .as_ref()
                .and_then(|endpoint| endpoint.profile_name()),
            Some("eval-luna"),
            "the endpoints the grant carried survive beside the parent"
        );
        let mut unknown = PlaneGrant::for_test("exec-unknown-client");
        adopt_session_parent(&mut unknown, &McpSession::default());
        assert_eq!(grant_parent(&unknown), None, "no family, no parent");

        let mut conversation = conversation_grant_on("grok", "sess-adopt");
        adopt_session_parent(&mut conversation, &session);
        assert_eq!(grant_parent(&conversation).as_deref(), Some("grok"));

        let dir = tempfile::tempdir().unwrap();
        let prompt_storage = JsonPromptStorage::new(JsonStorageConfig {
            storage_dir: dir.path().join("prompts"),
            ..Default::default()
        })
        .unwrap();
        let executors = Arc::new(ActionExecutors::new(
            Arc::new(ConfigurableMockLlm::with_response("{}")),
            Arc::new(PromptManager::new(Arc::new(prompt_storage))),
        ));
        let mut ctx = AgenticContext::default();
        ctx.harness_engine = Some("codex".to_string());
        let mut run = PlaneGrant::for_run(ctx, executors, "exec-run-adopt".to_string());
        assert!(!is_terminal_grant(&run));
        assert_eq!(grant_parent(&run).as_deref(), Some("codex"));
        adopt_session_parent(&mut run, &session);
        assert_eq!(
            grant_parent(&run).as_deref(),
            Some("codex"),
            "a run grant keeps the run engine"
        );
    }
}

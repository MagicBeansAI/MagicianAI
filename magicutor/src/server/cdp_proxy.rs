/// Thin CDP-compatible proxy that sits on top of the magicutor bridge.
///
/// Exposes just enough of the Chrome DevTools Protocol for agent-browser
/// (and other CDP clients) to connect with `--connect` / connect to
/// `ws://127.0.0.1:3003/devtools/browser/<id>`.
///
/// All actual browser commands are forwarded through the existing bridge to
/// the Chrome extension, which calls `chrome.debugger.sendCommand` under the
/// hood.  No changes are required to the extension or to the existing bridge
/// protocol.
///
/// Supported entry-points
/// ----------------------
/// GET /json/version          → version JSON (tells clients where to connect)
/// GET /json                  → alias for /json/list
/// GET /json/list             → list of open tabs as CDP TargetInfo objects
/// GET /devtools/browser/{id} → browser-level CDP WebSocket
/// GET /devtools/page/{id}    → page-level CDP WebSocket (shortcut for a tab)
use actix::{Actor, AsyncContext, Handler, Message, StreamHandler};
use actix_web::{web, Error as ActixError, HttpRequest, HttpResponse};
use actix_web_actors::ws;
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::bridge_protocol::ExtensionRequest;
use crate::server::bridge::{
    self, send_over_bridge_tracked, send_over_bridge_via, subscribe_cdp_events, BridgeRoute,
    BrowserSelectError, CdpEventPayload, CdpSubscriberHandle,
};
use crate::server::{page_signals, trace_capture};
use crate::types::{AmbientCaptureStatus, AmbientPageSignal};

// Default browser-level path component used when no per-thread id is
// supplied (legacy / generic clients). When the path is anything else,
// it's treated as a magician thread id and the proxy creates a single
// dedicated tab for that thread.
const BROWSER_ID: &str = "magicutor-proxy";

/// Thread -> dedicated Chrome window, reused across CDP connections. Only
/// explicitly created tabs and their opener descendants are owned; a user tab
/// added to the same window does not become part of the automation.
static THREAD_WINDOWS: Lazy<Mutex<HashMap<String, i32>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// `bridge.automation_surface`, sent with every `create_window`.
static AUTOMATION_SURFACE: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new("auto".to_string()));

/// Thread -> the surface the extension chose for its tabs: `in_window` (a
/// tab group in the owner's window), `own_window_group` or `own_window`. An
/// in-window thread's cached window is the owner's own, so it counts as live
/// only while one of the thread's tabs does.
static THREAD_SURFACES: Lazy<Mutex<HashMap<String, String>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Thread -> the name its tab group shows, from the CDP URL's `?label=`.
static THREAD_LABELS: Lazy<Mutex<HashMap<String, String>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
const MAX_LABEL_CHARS: usize = 80;

/// Set `bridge.automation_surface` at startup; an unknown value falls back
/// to `auto`.
pub fn set_automation_surface(value: &str) {
    let value = value.trim().to_ascii_lowercase();
    let surface = match value.as_str() {
        "auto" | "in_window" | "own_window" => value,
        other => {
            warn!(
                "[cdp-proxy] unknown bridge.automation_surface {:?}; using auto",
                other
            );
            "auto".to_string()
        },
    };
    info!("[cdp-proxy] automation surface: {}", surface);
    if let Ok(mut current) = AUTOMATION_SURFACE.lock() {
        *current = surface;
    }
}

fn automation_surface() -> String {
    AUTOMATION_SURFACE
        .lock()
        .map(|surface| surface.clone())
        .unwrap_or_else(|_| "auto".to_string())
}

fn thread_label_from_query(query: &str) -> Option<String> {
    web::Query::<HashMap<String, String>>::from_query(query)
        .ok()
        .and_then(|query| query.into_inner().remove("label"))
        .map(|label| label.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|label| !label.is_empty())
        .map(|label| label.chars().take(MAX_LABEL_CHARS).collect())
}

/// Process-wide map of thread_id -> existing Chrome tab id for extension-
/// initiated "use this tab" runs. These threads intentionally do not get a
/// dedicated window; Target.getTargets exposes only the bound tab plus tabs
/// later claimed through the opener chain.
static THREAD_BOUND_TABS: Lazy<Mutex<HashMap<String, i32>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Process-wide bootstrap fallback of CDP TargetInfo values for tabs a thread
/// already owns. Normal `Target.getTargets` discovery should come from the
/// extension bridge; this cache is consumed only when that bridge discovery
/// fails immediately after bind/create.
static THREAD_TARGET_CACHE: Lazy<Mutex<HashMap<String, Vec<Value>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Accessible task tabs: the initial created/bound root, explicit new targets,
/// and opener descendants. The extension's browser-session ledger recovers
/// created tabs missed while CDP was detached, including cross-window popups.
/// Cleanup uses that ledger, excluding bound owner tabs.
static THREAD_OWNED_TABS: Lazy<Mutex<HashMap<String, HashSet<i32>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// thread id -> the browser its window, bound tab, owned tabs and target
/// cache were built in. Tab and window ids are only meaningful in that
/// browser: when a thread's work moves to another browser (its pin moved, or
/// a legacy default browser was replaced), that state is dropped, never
/// reused.
static THREAD_STATE_BROWSER: Lazy<Mutex<HashMap<String, String>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

async fn thread_state_guard(thread_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
    type Locks = HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>;
    static LOCKS: Lazy<Mutex<Locks>> = Lazy::new(|| Mutex::new(HashMap::new()));
    let lock = {
        let mut locks = LOCKS.lock().unwrap_or_else(|error| error.into_inner());
        locks.retain(|_, lock| lock.strong_count() > 0);
        match locks.get(thread_id).and_then(std::sync::Weak::upgrade) {
            Some(lock) => lock,
            None => {
                let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
                locks.insert(thread_id.to_string(), std::sync::Arc::downgrade(&lock));
                lock
            },
        }
    };
    lock.lock_owned().await
}

/// Record that `thread_id`'s state now belongs to `browser_id`. Returns true
/// when state built in a different browser was dropped.
fn note_thread_state_browser(thread_id: &str, browser_id: &str) -> bool {
    let previous = {
        let mut map = match THREAD_STATE_BROWSER.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        map.insert(thread_id.to_string(), browser_id.to_string())
    };
    match previous {
        Some(previous) if previous != browser_id => {
            let (window_id, bound_tab, owned_tabs) = clear_thread_state(thread_id);
            trace_capture::forget_thread_tabs(thread_id);
            info!(
                "[cdp-proxy] thread={} moved to browser [{}]; dropped window={:?} bound_tab={:?} owned_tabs={:?} from browser [{}]",
                thread_id,
                bridge::browser_fingerprint(browser_id),
                window_id,
                bound_tab,
                owned_tabs,
                bridge::browser_fingerprint(&previous)
            );
            // clear_thread_state forgot the owner too; record the new one.
            if let Ok(mut map) = THREAD_STATE_BROWSER.lock() {
                map.insert(thread_id.to_string(), browser_id.to_string());
            }
            true
        },
        _ => false,
    }
}

/// Before a thread's state is used, make sure it belongs to the browser the
/// thread's next request will reach.
async fn sync_thread_state_browser(thread_id: &str) {
    if let Some(browser_id) = bridge::predicted_thread_browser(thread_id).await {
        note_thread_state_browser(thread_id, &browser_id);
    }
}

/// Record explicitly created, bound, or opener-claimed tabs. Idempotent.
fn seed_owned_tabs(thread_id: &str, tab_ids: impl IntoIterator<Item = i32>) {
    let mut map = match THREAD_OWNED_TABS.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let entry = map
        .entry(thread_id.to_string())
        .or_insert_with(HashSet::new);
    for id in tab_ids {
        entry.insert(id);
    }
}

fn cache_thread_targets(thread_id: &str, targets: Vec<Value>) {
    if targets.is_empty() {
        return;
    }
    if let Ok(mut cache) = THREAD_TARGET_CACHE.lock() {
        cache.insert(thread_id.to_string(), targets);
    }
}

fn take_cached_thread_targets(thread_id: &str) -> Vec<Value> {
    THREAD_TARGET_CACHE
        .lock()
        .ok()
        .and_then(|mut cache| cache.remove(thread_id))
        .unwrap_or_default()
}

fn set_bound_tab(thread_id: &str, tab_id: i32, target_info: Option<Value>) {
    if let Ok(mut bound) = THREAD_BOUND_TABS.lock() {
        bound.insert(thread_id.to_string(), tab_id);
    }
    if let Ok(mut windows) = THREAD_WINDOWS.lock() {
        windows.remove(thread_id);
    }
    if let Some(target_info) = target_info {
        cache_thread_targets(thread_id, vec![target_info]);
    }
    seed_owned_tabs(thread_id, [tab_id]);
}

fn bound_tab(thread_id: &str) -> Option<i32> {
    THREAD_BOUND_TABS
        .lock()
        .ok()
        .and_then(|bound| bound.get(thread_id).copied())
}

fn clear_thread_state(thread_id: &str) -> (Option<i32>, Option<i32>, Vec<i32>) {
    let window_id = THREAD_WINDOWS
        .lock()
        .ok()
        .and_then(|mut windows| windows.remove(thread_id));
    let bound_tab = THREAD_BOUND_TABS
        .lock()
        .ok()
        .and_then(|mut bound| bound.remove(thread_id));
    let owned_tabs = THREAD_OWNED_TABS
        .lock()
        .ok()
        .and_then(|mut owned| owned.remove(thread_id))
        .map(|tabs| tabs.into_iter().collect())
        .unwrap_or_default();
    if let Ok(mut cache) = THREAD_TARGET_CACHE.lock() {
        cache.remove(thread_id);
    }
    if let Ok(mut owners) = THREAD_STATE_BROWSER.lock() {
        owners.remove(thread_id);
    }
    if let Ok(mut surfaces) = THREAD_SURFACES.lock() {
        surfaces.remove(thread_id);
    }
    if let Ok(mut labels) = THREAD_LABELS.lock() {
        labels.remove(thread_id);
    }
    (window_id, bound_tab, owned_tabs)
}

/// If `opener_tab_id` is in `thread_id`'s owned set, claim
/// `new_tab_id` as Tier-2 and return `true`. Otherwise return `false`
/// without modifying state. Called from the inbound CDP event handler
/// when a `Target.attachedToTarget` arrives for a non-iframe target.
fn claim_owned_tab_if_chained(thread_id: &str, new_tab_id: i32, opener_tab_id: i32) -> bool {
    let mut map = match THREAD_OWNED_TABS.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let Some(entry) = map.get_mut(thread_id) else {
        return false;
    };
    if !entry.contains(&opener_tab_id) {
        return false;
    }
    entry.insert(new_tab_id)
}

/// Remove `tab_id` from `thread_id`'s owned set. Called on
/// `Target.targetDestroyed`. Detaching a debugger does not close a tab.
fn drop_owned_tab(thread_id: &str, tab_id: i32) {
    let mut map = match THREAD_OWNED_TABS.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(entry) = map.get_mut(thread_id) {
        entry.remove(&tab_id);
    }
}

/// Return a snapshot of the tabs claimed by `thread_id`. Order is
/// unspecified (callers sort if needed). Empty when the thread has no
/// owned set yet.
pub fn owned_tabs(thread_id: &str) -> Vec<i32> {
    let map = match THREAD_OWNED_TABS.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    map.get(thread_id)
        .map(|set| set.iter().copied().collect())
        .unwrap_or_default()
}

/// Extract every `tabId` (`tab-N` targetId) from a CDP target list and
/// hand them to [`seed_owned_tabs`]. Today the only production caller
/// is `fetch_owned_targets_for_thread`, which filters tabs by their
/// `windowId` field — but this generic helper is kept available
/// (under tests and for future callers that just want bulk-seeding
/// from a TargetInfo array).
#[allow(dead_code)]
fn seed_owned_tabs_from_targets(thread_id: &str, targets: &[Value]) {
    let tab_ids = targets.iter().filter_map(|target| {
        target
            .get("targetId")
            .and_then(Value::as_str)
            .and_then(|tid| tid.strip_prefix("tab-"))
            .and_then(|n| n.parse::<i32>().ok())
    });
    seed_owned_tabs(thread_id, tab_ids);
}

// ─── Access ──────────────────────────────────────────────────────────────────

/// Every route that drives or steers the owner's browsers (devtools, target
/// lists, CDP threads) requires no website `Origin` and either the configured
/// token or a trusted peer (loopback, or `bridge.trusted_peers` /
/// `MAGICUTOR_TRUSTED_PEERS`). See `bridge::owner_request_ok`.
fn automation_request_ok(
    req: &HttpRequest,
    cfg: Option<&web::Data<crate::MagicutorConfig>>,
) -> bool {
    let allowed = bridge::owner_request_ok(req, cfg.map(|cfg| cfg.get_ref()));
    if !allowed {
        warn!(
            "[cdp-proxy] refused {} {} from {:?} (untrusted peer, missing token or website Origin)",
            req.method(),
            req.path(),
            req.peer_addr()
        );
    }
    allowed
}

// ─── Browser selection ───────────────────────────────────────────────────────

/// Optional `browser` selector (install id or label, e.g. `?browser=comet`)
/// from a proxy URL's query string. Several browsers can share the bridge;
/// without a selector work goes to the default browser.
fn browser_selector_from_query(query: &str) -> Option<String> {
    web::Query::<HashMap<String, String>>::from_query(query)
        .ok()
        .and_then(|query| query.into_inner().remove("browser"))
        .map(|selector| selector.trim().to_string())
        .filter(|selector| !selector.is_empty())
}

/// Longest wait a URL may ask for while the owner picks a browser.
const MAX_BROWSER_CHOICE_TIMEOUT_SECS: u64 = 120;
const MAX_CHOICE_TASK_CHARS: usize = 120;

/// `?choose=ask[&timeout=<secs>][&task=<title>]`: ask the owner which
/// browser this run should use. Magician sends it only for attended runs;
/// without it nothing is ever asked.
fn browser_choice_from_query(
    query: &str,
    default_timeout_secs: u64,
) -> Option<(std::time::Duration, String)> {
    let params = web::Query::<HashMap<String, String>>::from_query(query)
        .ok()?
        .into_inner();
    if params.get("choose").map(|value| value.trim()) != Some("ask") {
        return None;
    }
    let timeout_secs = params
        .get("timeout")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default_timeout_secs)
        .clamp(1, MAX_BROWSER_CHOICE_TIMEOUT_SECS);
    let task: String = params
        .get("task")
        .map(|value| {
            value
                .chars()
                .filter(|c| !c.is_control())
                .collect::<String>()
        })
        .map(|value| value.trim().chars().take(MAX_CHOICE_TASK_CHARS).collect())
        .filter(|value: &String| !value.is_empty())
        .unwrap_or_else(|| "this task".to_string());
    Some((std::time::Duration::from_secs(timeout_secs), task))
}

fn browser_choice_timeout_secs(cfg: Option<&web::Data<crate::MagicutorConfig>>) -> u64 {
    cfg.map(|cfg| cfg.bridge.browser_choice_timeout_secs)
        .unwrap_or(20)
}

fn browser_select_error_response(error: &BrowserSelectError) -> HttpResponse {
    let body = json!({
        "error": error.code(),
        "message": error.to_string(),
    });
    match error {
        BrowserSelectError::NoBrowsers => HttpResponse::ServiceUnavailable().json(body),
        BrowserSelectError::NotFound { .. } => HttpResponse::NotFound().json(body),
        BrowserSelectError::Ambiguous { .. } | BrowserSelectError::PinnedElsewhere { .. } => {
            HttpResponse::Conflict().json(body)
        },
    }
}

/// Route for a session that is not thread-scoped: the selected browser, else
/// the default browser at connect time, so the session's tab ids all come
/// from one browser even if the default changes later.
async fn unscoped_route(
    selector: Option<&str>,
) -> std::result::Result<BridgeRoute, BrowserSelectError> {
    Ok(match bridge::select_browser(selector).await? {
        Some(browser_id) => BridgeRoute::Browser(browser_id),
        None => BridgeRoute::Default,
    })
}

/// Cleanup requests go to one resolved browser, never through a thread
/// route: by the time cleanup runs the thread may be unpinned, and a thread
/// route would lazily pin it to the default browser and act on that
/// browser's tab of the same id. `None` means there is nothing to clean up.
async fn cleanup_route(route: &BridgeRoute) -> Option<BridgeRoute> {
    match route {
        BridgeRoute::Thread(thread_id) => bridge::thread_browser(thread_id)
            .await
            .map(BridgeRoute::Browser),
        _ => Some(route.clone()),
    }
}

// ─── HTTP handlers ───────────────────────────────────────────────────────────

pub async fn cdp_version(req: HttpRequest) -> HttpResponse {
    let host = host_from(&req);
    // Carry a browser selector into the advertised URL so a client that
    // discovers the endpoint here connects to the same browser.
    let query = if browser_selector_from_query(req.query_string()).is_some() {
        format!("?{}", req.query_string())
    } else {
        String::new()
    };
    HttpResponse::Ok().json(json!({
        "Browser": "Chrome/magicutor-bridge",
        "Protocol-Version": "1.3",
        "webSocketDebuggerUrl": format!("ws://{}/devtools/browser/{}{}", host, BROWSER_ID, query)
    }))
}

pub async fn cdp_list(
    req: HttpRequest,
    cfg: Option<web::Data<crate::MagicutorConfig>>,
) -> HttpResponse {
    if !automation_request_ok(&req, cfg.as_ref()) {
        return HttpResponse::Forbidden().finish();
    }
    let host = host_from(&req);
    let selector = browser_selector_from_query(req.query_string());
    let route = match unscoped_route(selector.as_deref()).await {
        Ok(route) => route,
        Err(error) => return browser_select_error_response(&error),
    };
    let targets = fetch_targets(&host, &route).await;
    HttpResponse::Ok().json(targets)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BindThreadTabRequest {
    #[serde(alias = "tab_id")]
    pub tab_id: i32,
    #[serde(default, alias = "window_id")]
    pub window_id: Option<i32>,
    #[serde(default, alias = "default_download_path")]
    pub default_download_path: Option<String>,
    /// Browser that owns the tab (install id or label). The extension sends
    /// its own install id, and the thread is pinned to that browser.
    #[serde(default, alias = "browser_id", alias = "browser")]
    pub browser_id: Option<String>,
}

/// Bind an existing Chrome tab to an agent-browser CDP thread.
///
/// This is the agent-browser-era replacement for the old Magicutor
/// `/sessions/bind-tab` endpoint. It does not create a Magicutor session or
/// route browser primitives through Rust executors; it only asks the extension
/// to attach the debugger to the tab, registers the extension-side session
/// mapping, and seeds the CDP proxy's owned-tab set for this thread.
pub async fn cdp_bind_thread_tab(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<BindThreadTabRequest>,
    cfg: Option<web::Data<crate::MagicutorConfig>>,
) -> HttpResponse {
    if !automation_request_ok(&req, cfg.as_ref()) {
        return HttpResponse::Forbidden().finish();
    }
    let thread_id = path.into_inner();
    let _guard = thread_state_guard(&thread_id).await;
    let mut params = json!({
        "tabId": body.tab_id,
        "sessionId": thread_id.clone(),
    });
    if let Some(window_id) = body.window_id {
        params["windowId"] = json!(window_id);
    }
    if let Some(default_download_path) = body.default_download_path.as_deref() {
        params["defaultDownloadPath"] = json!(default_download_path);
    }

    let req = ExtensionRequest {
        request_id: Uuid::new_v4().to_string(),
        action: "attach_tab".to_string(),
        params,
    };

    // Tab ids are only unique within one browser: settle the thread on the
    // browser that owns the tab, and drop state from any other browser,
    // before anything is sent for it.
    let selector = body
        .browser_id
        .as_deref()
        .map(str::trim)
        .filter(|selector| !selector.is_empty());
    if let Err(error) = bridge::claim_thread(&thread_id, selector).await {
        warn!(
            "[cdp-proxy] bind-tab thread={} browser={:?} refused: {}",
            thread_id, selector, error
        );
        return browser_select_error_response(&error);
    }
    sync_thread_state_browser(&thread_id).await;

    match send_over_bridge_via(&BridgeRoute::Thread(thread_id.clone()), req).await {
        Ok(response) if response.success => {
            let target_info =
                target_info_from_bridge_tab(body.tab_id, body.window_id, response.result.as_ref());
            set_bound_tab(&thread_id, body.tab_id, Some(target_info));
            let browser = bridge::thread_browser(&thread_id)
                .await
                .map(|browser_id| bridge::browser_fingerprint(&browser_id));
            HttpResponse::Ok().json(json!({
                "thread_id": thread_id,
                "tab_id": body.tab_id,
                "browser": browser,
                "status": "bound",
            }))
        },
        Ok(response) => HttpResponse::BadGateway().json(json!({
            "thread_id": thread_id,
            "error": response.error.unwrap_or_else(|| "extension attach_tab failed".to_string()),
            "status": "failed",
        })),
        Err(error) => HttpResponse::BadGateway().json(json!({
            "thread_id": thread_id,
            "error": error.to_string(),
            "status": "failed",
        })),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinThreadBrowserRequest {
    /// Install id, fingerprint or label of a connected browser.
    #[serde(alias = "browser_id", alias = "browser")]
    pub browser_id: String,
}

/// Pin a CDP thread to one connected browser before its first request, so a
/// run started from a browser's panel opens its window in that browser and
/// not in the default one. Re-pinning to the same browser is a no-op; a
/// thread already in another browser is refused (409).
pub async fn cdp_pin_thread_browser(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PinThreadBrowserRequest>,
    cfg: Option<web::Data<crate::MagicutorConfig>>,
) -> HttpResponse {
    if !automation_request_ok(&req, cfg.as_ref()) {
        return HttpResponse::Forbidden().finish();
    }
    let thread_id = path.into_inner();
    // `default` settles a waiting choice on the default browser.
    let selector =
        Some(body.browser_id.trim()).filter(|selector| !selector.eq_ignore_ascii_case("default"));
    match bridge::claim_thread(&thread_id, selector).await {
        Ok(browser_id) => {
            sync_thread_state_browser(&thread_id).await;
            // An owner answer from Magician's card releases a connect that is
            // waiting on the browser cards.
            let released = bridge::release_browser_choice(&thread_id).await;
            if released {
                info!(
                    "[cdp-proxy] thread={} browser choice settled by the owner",
                    thread_id
                );
            }
            HttpResponse::Ok().json(json!({
                "thread_id": thread_id,
                "browser": browser_id.map(|browser_id| bridge::browser_fingerprint(&browser_id)),
                "status": "pinned",
            }))
        },
        Err(error) => {
            warn!(
                "[cdp-proxy] pin thread={} browser={} refused: {}",
                thread_id, body.browser_id, error
            );
            browser_select_error_response(&error)
        },
    }
}

/// `GET /cdp/threads/{thread_id}/browser` — the thread's pin, whether a
/// `?choose=ask` connect would ask the owner, the connected browsers (label,
/// fingerprint, state) and the choice timeout. Magician reads it before a run
/// opens its CDP session.
pub async fn cdp_thread_browser_info(
    req: HttpRequest,
    path: web::Path<String>,
    cfg: Option<web::Data<crate::MagicutorConfig>>,
) -> HttpResponse {
    if !automation_request_ok(&req, cfg.as_ref()) {
        return HttpResponse::Forbidden().finish();
    }
    let thread_id = path.into_inner();
    let info = bridge::thread_browser_info(&thread_id).await;
    HttpResponse::Ok().json(json!({
        "thread_id": thread_id,
        "pinned": info.pinned,
        "would_ask": info.would_ask,
        "browsers": info.browsers,
        "choice_timeout_secs": browser_choice_timeout_secs(cfg.as_ref()),
    }))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartBrowserChoiceRequest {
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default, alias = "timeout_secs")]
    pub timeout_secs: Option<u64>,
}

/// `POST /cdp/threads/{thread_id}/choose` — ask the owner which browser the
/// thread should use, without waiting: every accepted browser gets a card,
/// and the call returns at once (`asked`, `open` when a choice is already
/// open, `not_asked`). The choice closes on the first card answer, on any pin
/// of the thread (`PUT .../browser`), or at the timeout, which pins the
/// default browser. Magician polls `GET .../browser` for the pin and then
/// connects normally.
pub async fn cdp_start_browser_choice(
    req: HttpRequest,
    path: web::Path<String>,
    body: Option<web::Json<StartBrowserChoiceRequest>>,
    cfg: Option<web::Data<crate::MagicutorConfig>>,
) -> HttpResponse {
    if !automation_request_ok(&req, cfg.as_ref()) {
        return HttpResponse::Forbidden().finish();
    }
    let thread_id = path.into_inner();
    let body = body.map(|body| body.into_inner()).unwrap_or_default();
    let timeout_secs = body
        .timeout_secs
        .unwrap_or_else(|| browser_choice_timeout_secs(cfg.as_ref()))
        .clamp(1, MAX_BROWSER_CHOICE_TIMEOUT_SECS);
    let task: String = body
        .task
        .as_deref()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_CHOICE_TASK_CHARS)
        .collect();
    let task = if task.is_empty() {
        "this task".to_string()
    } else {
        task
    };
    let started = bridge::start_browser_choice(
        &thread_id,
        &task,
        std::time::Duration::from_secs(timeout_secs),
    )
    .await;
    info!(
        "[cdp-proxy] thread={} browser choice start: {}",
        thread_id,
        started.as_str()
    );
    HttpResponse::Ok().json(json!({
        "thread_id": thread_id,
        "status": started.as_str(),
        "timeout_secs": timeout_secs,
    }))
}

/// Close only automation-created tabs, preserving an owner-bound existing tab.
/// Retain routing/ownership on failure so terminal cleanup can retry safely.
pub async fn cdp_clear_thread(
    req: HttpRequest,
    path: web::Path<String>,
    cfg: Option<web::Data<crate::MagicutorConfig>>,
) -> HttpResponse {
    if !automation_request_ok(&req, cfg.as_ref()) {
        return HttpResponse::Forbidden().finish();
    }
    let thread_id = path.into_inner();
    let _guard = thread_state_guard(&thread_id).await;
    let state_browser = THREAD_STATE_BROWSER
        .lock()
        .ok()
        .and_then(|owners| owners.get(&thread_id).cloned());
    let browser = match state_browser {
        Some(browser) => Some(browser),
        None => bridge::thread_browser(&thread_id).await,
    };
    // With no local state (e.g. Magicutor restarted), the extension's
    // browser-session ledger is authoritative. Never send numeric close IDs
    // to a different/default browser.
    let route = browser
        .clone()
        .map(BridgeRoute::Browser)
        .unwrap_or(BridgeRoute::Default);
    let preserve_tabs = bound_tab(&thread_id).into_iter().collect::<Vec<_>>();
    let keep_tabs = web::Query::<HashMap<String, String>>::from_query(req.query_string())
        .ok()
        .is_some_and(|query| query.get("keep_tabs").is_some_and(|value| value == "true"));
    let request = ExtensionRequest {
        request_id: Uuid::new_v4().to_string(),
        action: "close_session_tabs".to_string(),
        params: json!({"sessionId": thread_id, "preserveTabs": preserve_tabs, "keepTabs": keep_tabs}),
    };
    match send_over_bridge_via(&route, request).await {
        Ok(response)
            if response.success
                && response
                    .result
                    .as_ref()
                    .and_then(|r| r.get("cleared"))
                    .and_then(Value::as_bool)
                    == Some(true) => {},
        response => {
            let error = match response {
                Ok(response) => response
                    .error
                    .unwrap_or_else(|| "extension did not confirm tab cleanup".into()),
                Err(error) => error.to_string(),
            };
            warn!(thread_id, %error, "CDP tab cleanup incomplete; retaining ownership for retry");
            return HttpResponse::ServiceUnavailable()
                .json(json!({"cleared": false, "error": error}));
        },
    }
    let (_, bound_tab, owned_tabs) = clear_thread_state(&thread_id);
    trace_capture::forget_thread_tabs(&thread_id);
    let browser_released = if bridge::thread_has_live_session(&thread_id) {
        false
    } else {
        bridge::unpin_thread(&thread_id).await.is_some()
    };
    HttpResponse::Ok().json(json!({
        "thread_id": thread_id,
        "cleared": true,
        "bound_tab_id": bound_tab,
        "owned_tabs": owned_tabs,
        "browser": browser.map(|id| bridge::browser_fingerprint(&id)),
        "browser_released": browser_released,
    }))
}

// ─── WebSocket handlers ───────────────────────────────────────────────────────

/// Browser-level CDP WebSocket.  Handles Target.* and routes everything else
/// through the bridge using the sessionId → tabId mapping it maintains.
///
/// The `{id}` path segment is interpreted as a magician thread id, used to
/// own a dedicated tab in the user's Chrome. Connections at the legacy
/// `/devtools/browser/magicutor-proxy` URL retain whatever the
/// fall-through path does (no auto-tab creation, list_tabs returns all
/// tabs). New magician runs should connect via
/// `/devtools/browser/<thread-id>`.
///
/// An optional `?browser=<id-fingerprint-or-label>` picks which connected
/// browser the thread runs in; without it the thread keeps its pin, or is
/// pinned to the default browser. The thread is settled here, before the
/// session counts as live: an idle thread whose browser is gone moves now,
/// and the session then stays in that browser.
pub async fn cdp_browser_ws(
    req: HttpRequest,
    stream: web::Payload,
    path: web::Path<String>,
    cfg: Option<web::Data<crate::MagicutorConfig>>,
) -> std::result::Result<HttpResponse, ActixError> {
    // Drives the owner's tabs: never for a website or an untrusted peer.
    if !automation_request_ok(&req, cfg.as_ref()) {
        return Ok(HttpResponse::Forbidden().finish());
    }
    let id = path.into_inner();
    let id = match super::cdp_scope_alias::decode(&id) {
        Some(Some(scope)) => scope,
        Some(None) => return Ok(HttpResponse::BadRequest().finish()),
        None => id,
    };
    let thread_id = if id == BROWSER_ID { None } else { Some(id) };
    let selector = browser_selector_from_query(req.query_string());
    if let (Some(tid), Some(label)) = (
        thread_id.as_deref(),
        thread_label_from_query(req.query_string()),
    ) {
        if let Ok(mut labels) = THREAD_LABELS.lock() {
            labels.insert(tid.to_string(), label);
        }
    }
    let route = match thread_id.as_deref() {
        Some(tid) => {
            // An attended run with several accepted browsers and no pin asks
            // the owner which one to use, holding this connect until the
            // first answer or the timeout (then the default browser).
            if selector.is_none() {
                let default_timeout = browser_choice_timeout_secs(cfg.as_ref());
                if let Some((timeout, task)) =
                    browser_choice_from_query(req.query_string(), default_timeout)
                {
                    let outcome = bridge::choose_browser_for_thread(tid, &task, timeout).await;
                    info!("[cdp-proxy] thread={} browser choice: {:?}", tid, outcome);
                }
            }
            if let Err(error) = bridge::claim_thread(tid, selector.as_deref()).await {
                warn!(
                    "[cdp-proxy] thread={} browser={:?} refused: {}",
                    tid, selector, error
                );
                return Ok(browser_select_error_response(&error));
            }
            sync_thread_state_browser(tid).await;
            BridgeRoute::Thread(tid.to_string())
        },
        None => match unscoped_route(selector.as_deref()).await {
            Ok(route) => route,
            Err(error) => return Ok(browser_select_error_response(&error)),
        },
    };
    ws::WsResponseBuilder::new(CdpSession::browser_level(thread_id, route), &req, stream)
        .frame_size(16 * 1024 * 1024)
        .start()
}

/// Page-level CDP WebSocket.  All commands are routed to the tab given in the
/// URL without requiring a Target.attachToTarget handshake. Tab ids are per
/// browser: `?browser=` names the browser, else the default browser.
pub async fn cdp_page_ws(
    req: HttpRequest,
    stream: web::Payload,
    path: web::Path<i32>,
    cfg: Option<web::Data<crate::MagicutorConfig>>,
) -> std::result::Result<HttpResponse, ActixError> {
    if !automation_request_ok(&req, cfg.as_ref()) {
        return Ok(HttpResponse::Forbidden().finish());
    }
    let tab_id = path.into_inner();
    let selector = browser_selector_from_query(req.query_string());
    let route = match unscoped_route(selector.as_deref()).await {
        Ok(route) => route,
        Err(error) => return Ok(browser_select_error_response(&error)),
    };
    ws::WsResponseBuilder::new(CdpSession::page_level(tab_id, route), &req, stream)
        .frame_size(16 * 1024 * 1024)
        .start()
}

// ─── Actor ───────────────────────────────────────────────────────────────────

struct CdpSession {
    /// sessionId → tabId. Populated by:
    ///   1. `Target.attachToTarget` — proxy-minted uuid for the main
    ///      tab attach.
    ///   2. Chrome auto-attach `Target.attachedToTarget` events for
    ///      OOPIF iframes — Chrome-generated uppercase-hex sessionId.
    /// Both kinds are tab-resolvable here. Distinguishing them
    /// matters when deciding whether to forward `sessionId` to
    /// `chrome.debugger.sendCommand`: Chrome's debugger only knows
    /// the iframe sessionIds (it issued them); it does *not* know
    /// the proxy-minted main-session uuid and replies "Session with
    /// given id not found" if we forward that one. See
    /// `iframe_session_ids` below for the discriminator.
    sessions: HashMap<String, i32>,
    /// Subset of `sessions` keys that came from Chrome auto-attach
    /// events (OOPIF iframes). Only sessionIds in this set are safe
    /// to forward via `chrome.debugger.sendCommand`'s `sessionId`
    /// field — those are the ones Chrome's debugger actually
    /// generated. The main-tab session id was minted locally by
    /// `Target.attachToTarget` at attach time and Chrome wouldn't
    /// recognize it. Without this distinction, every CDP command
    /// (Page.enable, DOM.enable, Runtime.evaluate, …) targeted at
    /// the main tab failed with `-32001 "Session with given id not
    /// found"` from Chrome, breaking every agent-browser run that
    /// went through `Target.attachToTarget` rather than the
    /// page-level URL shortcut.
    iframe_session_ids: std::collections::HashSet<String>,
    /// Set for page-level connections; all commands go here when no sessionId
    default_tab_id: Option<i32>,
    /// Magician thread id from the WS URL path. When set, the proxy owns
    /// exactly one tab in the user's Chrome for this thread, returns only
    /// that tab from Target.getTargets, and rejects attaches to anything
    /// else. When `None` (legacy `magicutor-proxy` URL), no scoping is
    /// applied — all of the user's tabs are exposed.
    thread_id: Option<String>,
    /// tabId → most-recent targetInfo seen via Target.getTargets. Used to
    /// populate Target.attachedToTarget events without an extra bridge
    /// round-trip. Real Chrome emits these events after attachToTarget;
    /// agent-browser and other CDP clients wait for them, so
    /// without this the connect command times out.
    target_info: HashMap<i32, Value>,
    /// Active CDP event subscriptions, one per tab we've attached to.
    /// Dropped when the actor stops, which auto-unregisters from the
    /// global registry.
    event_subscriptions: HashMap<i32, CdpSubscriberHandle>,
    /// Which browser this session's bridge requests go to. Thread-scoped
    /// sessions follow the thread's pin; unscoped sessions are fixed to one
    /// browser at connect time, so tab ids never mix across browsers.
    route: BridgeRoute,
    /// The browser that answered this session's first attach. Teardown
    /// (`session_ended`, `detach_debugger`) goes there and nowhere else.
    served_by: Option<String>,
}

impl CdpSession {
    /// Where teardown for this session goes: the browser that served its
    /// attaches, else its route (resolved without pinning by `cleanup_route`).
    fn teardown_route(&self) -> BridgeRoute {
        self.served_by
            .clone()
            .map(BridgeRoute::Browser)
            .unwrap_or_else(|| self.route.clone())
    }
}

impl CdpSession {
    fn browser_level(thread_id: Option<String>, route: BridgeRoute) -> Self {
        Self {
            sessions: HashMap::new(),
            iframe_session_ids: std::collections::HashSet::new(),
            default_tab_id: None,
            thread_id,
            target_info: HashMap::new(),
            event_subscriptions: HashMap::new(),
            route,
            served_by: None,
        }
    }

    fn page_level(tab_id: i32, route: BridgeRoute) -> Self {
        Self {
            sessions: HashMap::new(),
            iframe_session_ids: std::collections::HashSet::new(),
            default_tab_id: Some(tab_id),
            thread_id: None,
            target_info: HashMap::new(),
            event_subscriptions: HashMap::new(),
            route,
            served_by: None,
        }
    }

    /// The CDP sessionId an incoming event goes out under. An event from a
    /// child target carries Chrome's own session id (`chrome_session`) and
    /// keeps it; the tab's own events go to the main session we issued for
    /// the tab, never to an iframe session on the same tab. Picking "any
    /// session of the tab" made the page's `Page.loadEventFired` go out
    /// under a cross-site iframe's session whenever hash order put that one
    /// first, and agent-browser's `open` then timed out on a loaded page.
    fn event_session_id(&self, tab_id: i32, chrome_session: Option<&str>) -> Option<String> {
        if let Some(sid) = chrome_session {
            return Some(sid.to_string());
        }
        self.sessions
            .iter()
            .filter(|(sid, &t)| t == tab_id && !self.iframe_session_ids.contains(*sid))
            .map(|(sid, _)| sid)
            .min()
            .cloned()
    }

    /// Register / unregister OOPIF iframe sessions based on inbound
    /// `Target.attachedToTarget` and `Target.detachedFromTarget`
    /// events. Chrome auto-attaches OOPIF (out-of-process iframe)
    /// targets when the parent debugger has
    /// `Target.setAutoAttach({autoAttach: true, flatten: true,
    /// filter: [{type: 'iframe'}]})` enabled — the extension does
    /// this in `debugger_actions.js::attachDebugger`. Each attach
    /// event carries a Chrome-generated sessionId; without registering
    /// them, follow-up iframe-targeted commands hit `self.sessions`
    /// lookup miss and return "no tab attached for this session", and
    /// iframe content stays unreadable.
    ///
    /// Filtered to `targetInfo.type == "iframe"` per the plan's
    /// non-goal #3 — workers and service workers are out of scope
    /// because the proxy has no reliable way to drive them.
    /// Worker / SW attaches still hit the unknown-session path in the
    /// command handler, which is fine: the warn was demoted to debug
    /// in the same change so it doesn't pollute the log.
    fn update_iframe_sessions_from_event(&mut self, method: &str, params: &Value, tab_id: i32) {
        match method {
            "Target.attachedToTarget" => {
                let target_type = params
                    .get("targetInfo")
                    .and_then(|t| t.get("type"))
                    .and_then(Value::as_str);
                if target_type != Some("iframe") {
                    return;
                }
                if let Some(sid) = params.get("sessionId").and_then(Value::as_str) {
                    self.sessions.insert(sid.to_string(), tab_id);
                    // Tag this session as iframe-origin so the forward
                    // path knows it's safe to thread the sessionId
                    // through `chrome.debugger.sendCommand` — Chrome
                    // issued it, Chrome will recognize it.
                    self.iframe_session_ids.insert(sid.to_string());
                    debug!(
                        "[cdp-proxy] registered iframe session {} -> tab {}",
                        sid, tab_id
                    );
                }
            },
            "Target.detachedFromTarget" => {
                if let Some(sid) = params.get("sessionId").and_then(Value::as_str) {
                    if self.sessions.remove(sid).is_some() {
                        self.iframe_session_ids.remove(sid);
                        debug!("[cdp-proxy] unregistered iframe session {}", sid);
                    }
                }
            },
            _ => {},
        }
    }
}

impl Actor for CdpSession {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, _ctx: &mut Self::Context) {
        if let Some(thread_id) = self.thread_id.as_deref() {
            bridge::note_thread_session(thread_id, true);
        }
        debug!(
            "CDP proxy session started (default_tab={:?}, route={:?})",
            self.default_tab_id, self.route
        );
    }

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        // The client is gone — the run's cleanup closed the agent-browser
        // session, or the CLI died. Every top-level tab this session
        // attached still carries the extension's `chrome.debugger`
        // attachment (Chrome's "is being debugged" bar) until the
        // extension is told to detach; attach was proxied on the way in,
        // and nothing proxied a detach on the way out.
        let tabs = tabs_to_detach(&self.sessions, &self.iframe_session_ids);
        if let Some(thread_id) = self.thread_id.as_deref() {
            bridge::note_thread_session(thread_id, false);
            // Another connection of this thread is still open (the agent's
            // own session while a secure fill's side connection closes, or
            // the reverse): the automation UI and the tabs' debugger
            // attachments are the thread's, so they stay.
            if bridge::thread_has_live_session(thread_id) {
                info!(
                    "[cdp-proxy] session stopped thread={}; another session of the thread is open, keeping the automation UI and {} tab(s) {:?} attached",
                    thread_id,
                    tabs.len(),
                    tabs
                );
                return;
            }
        }
        // Info, not debug: this is the one line that says the run's
        // browser attachment was released, and the question "why is the
        // debugging bar still up?" is answered from the service log.
        info!(
            "[cdp-proxy] session stopped thread={:?}; ending the automation UI and detaching debugger from {} tab(s) {:?}",
            self.thread_id,
            tabs.len(),
            tabs
        );
        spawn_session_end(self.thread_id.clone(), self.teardown_route(), tabs);
        debug!("CDP proxy session stopped");
    }
}

/// End a thread's run on the extension side, in order: `session_ended`
/// first — the page's status panel and aurora take the execution's
/// terminal state (or hide), the execution is untracked, the session's
/// tab registry is cleared — then `detach_debugger` per top-level tab.
/// The order matters: `detach_debugger` clears the session's tab list as
/// a side effect, and the terminal overlay has to reach those tabs
/// before that. Until 0.1.91 only the detaches were sent, and the panel
/// + aurora stayed on the page until the user pressed Stop, whose
/// `stop_automation` also cancels an execution that had already finished.
fn spawn_session_end(thread_id: Option<String>, route: BridgeRoute, tabs: Vec<i32>) {
    actix_web::rt::spawn(async move {
        let Some(route) = cleanup_route(&route).await else {
            return;
        };
        if let Some(tid) = thread_id.as_deref() {
            let req = ExtensionRequest {
                request_id: Uuid::new_v4().to_string(),
                action: "session_ended".to_string(),
                params: json!({ "sessionId": tid }),
            };
            match send_over_bridge_via(&route, req).await {
                Ok(r) if r.success => info!(
                    "[cdp-proxy] automation UI ended thread={} result={}",
                    tid,
                    r.result.unwrap_or(json!({}))
                ),
                Ok(r) => warn!(
                    "[cdp-proxy] session_ended thread={} failed: {}",
                    tid,
                    r.error.unwrap_or_default()
                ),
                Err(e) => warn!("[cdp-proxy] session_ended thread={} failed: {}", tid, e),
            }
        }
        for tab_id in tabs {
            detach_debugger(thread_id.clone(), &route, tab_id).await;
        }
    });
}

/// The sessions a `Target.detachFromTarget` releases. CDP names them in
/// `params` — `sessionId`, or `targetId` for every session on that target —
/// and clients such as MagicVault's fill adapter send it there; a
/// message-level `sessionId` is the older spelling this proxy first read.
/// Reading only that one left every params-named detach a no-op, so a
/// tab's events kept going out under a stale session id.
fn sessions_to_detach(
    params: &Value,
    message_session: Option<&str>,
    sessions: &HashMap<String, i32>,
) -> Vec<String> {
    if let Some(sid) = params.get("sessionId").and_then(Value::as_str) {
        return vec![sid.to_string()];
    }
    if let Some(tab_id) = params
        .get("targetId")
        .and_then(Value::as_str)
        .and_then(|target| target.strip_prefix("tab-"))
        .and_then(|tab| tab.parse::<i32>().ok())
    {
        let mut sids: Vec<String> = sessions
            .iter()
            .filter(|(_, tab)| **tab == tab_id)
            .map(|(sid, _)| sid.clone())
            .collect();
        sids.sort_unstable();
        return sids;
    }
    message_session.map(str::to_string).into_iter().collect()
}

/// The top-level tabs a session attached, each once: iframe sessions
/// (Chrome auto-attach children) ride their parent's attachment and are
/// released with it.
fn tabs_to_detach(
    sessions: &HashMap<String, i32>,
    iframe_session_ids: &HashSet<String>,
) -> Vec<i32> {
    let mut tabs: Vec<i32> = sessions
        .iter()
        .filter(|(sid, _)| !iframe_session_ids.contains(*sid))
        .map(|(_, tab)| *tab)
        .collect();
    tabs.sort_unstable();
    tabs.dedup();
    tabs
}

/// Ask the extension to `chrome.debugger.detach` from `tab_id` — the
/// mirror of the `attach_debugger` request proxied for
/// `Target.attachToTarget`. Best effort: a tab that is already gone or
/// already detached answers success on the extension side.
fn spawn_detach_debugger(thread_id: Option<String>, route: BridgeRoute, tab_id: i32) {
    actix_web::rt::spawn(async move {
        if let Some(route) = cleanup_route(&route).await {
            detach_debugger(thread_id, &route, tab_id).await;
        }
    });
}

async fn detach_debugger(thread_id: Option<String>, route: &BridgeRoute, tab_id: i32) {
    let mut params = json!({ "tabId": tab_id });
    if let Some(tid) = thread_id.as_deref() {
        params["sessionId"] = json!(tid);
    }
    let req = ExtensionRequest {
        request_id: Uuid::new_v4().to_string(),
        action: "detach_debugger".to_string(),
        params,
    };
    match send_over_bridge_via(route, req).await {
        Ok(r) if r.success => info!("[cdp-proxy] detached debugger tab={}", tab_id),
        Ok(r) => warn!(
            "[cdp-proxy] detach_debugger tab={} failed: {}",
            tab_id,
            r.error.unwrap_or_default()
        ),
        Err(e) => warn!("[cdp-proxy] detach_debugger tab={} failed: {}", tab_id, e),
    }
}

// ─── Actor messages ───────────────────────────────────────────────────────────

struct SendText(String);
impl Message for SendText {
    type Result = ();
}
impl Handler<SendText> for CdpSession {
    type Result = ();
    fn handle(&mut self, msg: SendText, ctx: &mut Self::Context) {
        ctx.text(msg.0);
    }
}

struct RegisterSession {
    session_id: String,
    tab_id: i32,
}
impl Message for RegisterSession {
    type Result = ();
}
impl Handler<RegisterSession> for CdpSession {
    type Result = ();
    fn handle(&mut self, msg: RegisterSession, _ctx: &mut Self::Context) {
        self.sessions.insert(msg.session_id, msg.tab_id);
    }
}

struct CacheTargets(Vec<Value>);
impl Message for CacheTargets {
    type Result = ();
}
impl Handler<CacheTargets> for CdpSession {
    type Result = ();
    fn handle(&mut self, msg: CacheTargets, _ctx: &mut Self::Context) {
        for t in msg.0 {
            if let Some(target_id) = t.get("targetId").and_then(|v| v.as_str()) {
                if let Some(tab_id) = target_id
                    .strip_prefix("tab-")
                    .and_then(|s| s.parse::<i32>().ok())
                {
                    self.target_info.insert(tab_id, t);
                }
            }
        }
    }
}

/// Register this actor as a subscriber for chrome.debugger events on a
/// tab. Idempotent — if we've already subscribed for the tab, the second
/// call is a no-op. Subscriptions auto-unregister when the actor stops.
struct SubscribeTabEvents {
    /// Browser the tab belongs to: the one that answered the attach.
    browser_id: String,
    tab_id: i32,
}
impl Message for SubscribeTabEvents {
    type Result = ();
}
impl Handler<SubscribeTabEvents> for CdpSession {
    type Result = ();
    fn handle(&mut self, msg: SubscribeTabEvents, ctx: &mut Self::Context) {
        if self.route == BridgeRoute::Default {
            // An unscoped session opened before any browser connected stays
            // in the browser that answered its first attach.
            self.route = BridgeRoute::Browser(msg.browser_id.clone());
        }
        if self.served_by.is_none() {
            self.served_by = Some(msg.browser_id.clone());
        }
        if self.event_subscriptions.contains_key(&msg.tab_id) {
            return;
        }
        let recipient = ctx.address().recipient::<CdpEventInbound>();
        let handle = subscribe_cdp_events(&msg.browser_id, msg.tab_id, move |payload| {
            // try_send returns Err if the mailbox is full or the actor
            // is gone; in either case we keep the subscription — actor
            // stop drops the handle (RAII unregister) and a full mailbox
            // is transient backpressure that should resolve.
            let _ = recipient.try_send(CdpEventInbound(payload));
            true
        });
        self.event_subscriptions.insert(msg.tab_id, handle);
    }
}

/// CDP event delivered from the bridge subscriber. Carries the tab and
/// the raw `{method, params}` from chrome.debugger; the actor looks up
/// its sessionId for the tab and emits the canonical CDP event shape on
/// the WS so agent-browser sees `Page.loadEventFired`, `Target.attached
/// ToTarget`, etc.
struct CdpEventInbound(CdpEventPayload);
impl Message for CdpEventInbound {
    type Result = ();
}
impl Handler<CdpEventInbound> for CdpSession {
    type Result = ();
    fn handle(&mut self, msg: CdpEventInbound, ctx: &mut Self::Context) {
        let CdpEventPayload {
            tab_id,
            session_id: chrome_session,
            method,
            params,
            ..
        } = msg.0;
        // Owner-set bookkeeping for thread-scoped sessions. Runs alongside
        // the existing event forwarding — pure additive side-effect, no
        // change to what agent-browser receives. See the module-level
        // notes on `THREAD_OWNED_TABS` and the design plan for the
        // tier rules.
        if let Some(thread_id) = self.thread_id.as_deref() {
            update_owned_tabs_from_event(thread_id, &method, &params);
        }
        // Register / unregister OOPIF iframe sessions so subsequent
        // commands targeting iframes route via `debugger_command` with
        // the session_id forwarded to chrome.debugger. Without this,
        // iframe-targeted commands hit `self.sessions` lookup miss
        // and return "no tab attached for this session" — iframe
        // content never reaches agent-browser. Workers / service
        // workers are intentionally not registered (out of scope per
        // `docs/plans/2026-05-03-cdp-proxy-iframe-session-routing.md`).
        self.update_iframe_sessions_from_event(&method, &params, tab_id);
        let session_id = self.event_session_id(tab_id, chrome_session.as_deref());
        if let Some(thread_id) = self.thread_id.as_deref() {
            record_automation_cdp_event(thread_id, tab_id, session_id.as_deref(), &method, &params);
        }
        let mut event = json!({
            "method": method,
            "params": params,
        });
        if let Some(sid) = session_id {
            event["sessionId"] = json!(sid);
        }
        ctx.text(event.to_string());
    }
}

/// Apply owner-set updates triggered by inbound CDP target-lifecycle
/// events. Keeps the event-forward path in `Handler<CdpEventInbound>`
/// readable.
///
/// * `Target.attachedToTarget` for a `page` target — claim it as Tier 2
///   if its `openerId` chains back to an already-owned tab. iframes
///   are intentionally skipped here (they're handled by the separate
///   iframe-session-routing plan).
/// * `Target.targetDestroyed` — drop a closed tab; debugger detaches retain it.
fn update_owned_tabs_from_event(thread_id: &str, method: &str, params: &Value) {
    match method {
        "Target.attachedToTarget" => {
            let Some(target_info) = params.get("targetInfo") else {
                return;
            };
            let target_type = target_info.get("type").and_then(Value::as_str);
            if target_type != Some("page") {
                // Iframes / workers / service workers are not page tabs.
                // Iframe-session routing has its own plan.
                return;
            }
            let Some(new_tab_id) = target_id_to_tab_id(target_info.get("targetId")) else {
                return;
            };
            let Some(opener_tab_id) = target_id_to_tab_id(target_info.get("openerId")) else {
                // No opener (rel="noopener" / window.open(noopener) /
                // direct user nav). Tier 3 — leave it unowned.
                return;
            };
            if claim_owned_tab_if_chained(thread_id, new_tab_id, opener_tab_id) {
                debug!(
                    "[cdp-proxy] thread={} claimed tab {} via opener tab {}",
                    thread_id, new_tab_id, opener_tab_id
                );
            }
        },
        "Target.targetDestroyed" => {
            let Some(tab_id) = target_id_to_tab_id(params.get("targetId")) else {
                return;
            };
            drop_owned_tab(thread_id, tab_id);
        },
        _ => {},
    }
}

/// Pull a numeric `tab_id` out of a CDP `targetId` field if it follows
/// the proxy's `tab-N` shape. Returns `None` for missing fields,
/// non-string values, or shapes the proxy doesn't manage (frame
/// targets, browser targets, …).
fn target_id_to_tab_id(value: Option<&Value>) -> Option<i32> {
    value
        .and_then(Value::as_str)
        .and_then(|tid| tid.strip_prefix("tab-"))
        .and_then(|n| n.parse::<i32>().ok())
}

/// Outcome of an owned-set guard check. Used by `Target.attachToTarget`
/// and `Target.closeTarget` to decide whether to forward a command.
#[derive(Debug, PartialEq, Eq)]
enum OwnedTabCheck {
    /// Forward the command — the requested tab is in the owned set or
    /// the owned set is empty (pre-bootstrap; the caller is responsible
    /// for ensuring `Target.getTargets` runs first in normal flow).
    Allowed,
    /// Reject with the contained user-facing reason. Caller emits a
    /// CDP error response.
    Rejected { reason: String },
}

/// Gate a tab-targeting CDP command on the per-thread owned set.
///
/// Returns `Allowed` when `requested_tab` is in
/// `THREAD_OWNED_TABS[thread_id]`. Empty sets fail closed.
/// Returns `Rejected` with a user-facing message otherwise.
///
/// `op_label` is used in the rejection message for debuggability
/// (e.g. `"attach"`, `"close"`).
fn check_tab_ownership(thread_id: &str, requested_tab: i32, op_label: &str) -> OwnedTabCheck {
    let owned = owned_tabs(thread_id);
    if owned.contains(&requested_tab) {
        OwnedTabCheck::Allowed
    } else {
        OwnedTabCheck::Rejected {
            reason: format!(
                "tab {requested_tab} is not owned by this execution; {op_label} is only allowed on task-created tabs, the bound owner tab, and their popups"
            ),
        }
    }
}

/// Emit `Target.attachedToTarget` event using cached targetInfo. Falls back
/// to a minimal stub if the tab wasn't seen via getTargets — agent-browser
/// already knows the URL/title from its prior enumeration, so the event
/// just needs to confirm the session.
struct EmitAttachedToTarget {
    session_id: String,
    tab_id: i32,
}
impl Message for EmitAttachedToTarget {
    type Result = ();
}
impl Handler<EmitAttachedToTarget> for CdpSession {
    type Result = ();
    fn handle(&mut self, msg: EmitAttachedToTarget, ctx: &mut Self::Context) {
        let target_info = self
            .target_info
            .get(&msg.tab_id)
            .cloned()
            .unwrap_or_else(|| {
                json!({
                    "targetId": format!("tab-{}", msg.tab_id),
                    "type": "page",
                    "title": "",
                    "url": "",
                    "attached": true,
                    "canAccessOpener": false,
                    "browserContextId": "default"
                })
            });
        let event = json!({
            "method": "Target.attachedToTarget",
            "params": {
                "sessionId": msg.session_id,
                "targetInfo": target_info,
                "waitingForDebugger": false
            }
        });
        ctx.text(event.to_string());
    }
}

// ─── Incoming CDP command ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CdpCommand {
    id: u64,
    #[serde(default)]
    session_id: Option<String>,
    method: String,
    #[serde(default)]
    params: Value,
}

impl StreamHandler<std::result::Result<ws::Message, ws::ProtocolError>> for CdpSession {
    fn handle(
        &mut self,
        item: std::result::Result<ws::Message, ws::ProtocolError>,
        ctx: &mut Self::Context,
    ) {
        let text = match item {
            Ok(ws::Message::Text(t)) => t.to_string(),
            Ok(ws::Message::Ping(d)) => {
                ctx.pong(&d);
                return;
            },
            Ok(ws::Message::Close(r)) => {
                ctx.close(r);
                return;
            },
            _ => return,
        };

        let cmd: CdpCommand = match serde_json::from_str(&text) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "CDP proxy: bad command ({}): {}",
                    e,
                    &text[..text.len().min(120)]
                );
                return;
            },
        };

        let id = cmd.id;
        let method = cmd.method.clone();
        let params = cmd.params.clone();
        let session_id = cmd.session_id.clone();
        let addr = ctx.address();

        match method.as_str() {
            // An attached owner's browser is never ours to shut down.
            "Browser.close" => {
                ctx.text(cdp_err(id, session_id.as_deref(), "Close the task's targets or end its session; the owner's browser cannot be closed through this proxy").to_string());
            },
            "Target.createTarget" => {
                let Some(thread_id) = self.thread_id.clone() else {
                    ctx.text(
                        cdp_err(
                            id,
                            session_id.as_deref(),
                            "Creating a tab requires a scoped browser session",
                        )
                        .to_string(),
                    );
                    return;
                };
                let url = params
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or("about:blank")
                    .to_string();
                let active = !params
                    .get("background")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                actix_web::rt::spawn(async move {
                    sync_thread_state_browser(&thread_id).await;
                    let window_id = if let Some(tab_id) = bound_tab(&thread_id) {
                        fetch_bound_targets_for_thread(&thread_id, tab_id)
                            .await
                            .into_iter()
                            .find(|target| {
                                target_id_to_tab_id(target.get("targetId")) == Some(tab_id)
                            })
                            .as_ref()
                            .and_then(extract_window_id)
                    } else {
                        ensure_thread_window(&thread_id).await
                    };
                    let Some(window_id) = window_id else {
                        addr.do_send(SendText(
                            cdp_err(id, None, "The task's browser window is unavailable")
                                .to_string(),
                        ));
                        return;
                    };
                    let _guard = thread_state_guard(&thread_id).await;
                    if !bridge::thread_has_live_session(&thread_id) {
                        addr.do_send(SendText(
                            cdp_err(id, None, "Browser session ended before tab creation")
                                .to_string(),
                        ));
                        return;
                    }
                    let request = ExtensionRequest {
                        request_id: Uuid::new_v4().to_string(),
                        action: "create_tab".into(),
                        params: json!({"sessionId": thread_id, "windowId": window_id, "url": url, "active": active}),
                    };
                    let response = match send_over_bridge_via(
                        &BridgeRoute::Thread(thread_id.clone()),
                        request,
                    )
                    .await
                    {
                        Ok(response) if response.success => {
                            match response
                                .result
                                .as_ref()
                                .and_then(|value| value.get("tabId"))
                                .and_then(Value::as_i64)
                            {
                                Some(tab_id) => {
                                    seed_owned_tabs(&thread_id, [tab_id as i32]);
                                    cdp_ok(id, None, json!({"targetId": format!("tab-{tab_id}")}))
                                },
                                None => {
                                    cdp_err(id, None, "Extension did not return a created tab ID")
                                },
                            }
                        },
                        Ok(response) => cdp_err(id, None, &response.error.unwrap_or_default()),
                        Err(error) => cdp_err(id, None, &error.to_string()),
                    };
                    addr.do_send(SendText(response.to_string()));
                });
            },
            // ── Browser-level static response ─────────────────────────────
            "Browser.getVersion" => {
                ctx.text(
                    cdp_ok(
                        id,
                        session_id.as_deref(),
                        json!({
                            "protocolVersion": "1.3",
                            "product": "Chrome/magicutor-bridge",
                            "revision": "",
                            "userAgent": "magicutor-cdp-proxy",
                            "jsVersion": ""
                        }),
                    )
                    .to_string(),
                );
            },
            // ── Target session-management no-ops ──────────────────────────
            // Discovery/activation are handled by the proxy. Auto-attach is
            // applied per attached tab by `spawn_post_attach_cdp_bootstrap`,
            // because Chrome's extension debugger API needs a tab target.
            "Target.setDiscoverTargets"
            | "Target.setAutoAttach"
            | "Target.activateTarget"
            | "Target.setRemoteLocations" => {
                ctx.text(cdp_ok(id, session_id.as_deref(), json!({})).to_string());
            },

            // ── Target.getTargets ────────────────────────────────────────
            "Target.getTargets" => {
                let echo_sid = session_id.clone();
                let thread_id = self.thread_id.clone();
                let route = self.route.clone();
                actix_web::rt::spawn(async move {
                    let targets = match thread_id.as_deref() {
                        Some(tid) => {
                            // Window and tab ids from another browser are
                            // meaningless here: drop them before use.
                            sync_thread_state_browser(tid).await;
                            if let Some(tab_id) = bound_tab(tid) {
                                fetch_bound_targets_for_thread(tid, tab_id).await
                            } else {
                                match ensure_thread_window(tid).await {
                                    Some(window_id) => {
                                        // The extension ledger includes task-created
                                        // popups even when CDP missed their events.
                                        fetch_owned_targets_for_thread(tid, window_id).await
                                    },
                                    None => {
                                        warn!(
                                            "[cdp-proxy] thread={} ensure_thread_window failed; \
                                                 returning empty target list",
                                            tid
                                        );
                                        Vec::new()
                                    },
                                }
                            }
                        },
                        // Legacy / unscoped path — expose all tabs.
                        None => fetch_targets_via_bridge(&route).await.unwrap_or_default(),
                    };
                    debug!(
                        "[cdp-proxy] Target.getTargets returning {} targets",
                        targets.len()
                    );
                    addr.do_send(CacheTargets(targets.clone()));
                    addr.do_send(SendText(
                        cdp_ok(id, echo_sid.as_deref(), json!({ "targetInfos": targets }))
                            .to_string(),
                    ));
                });
            },

            // ── Target.attachToTarget ────────────────────────────────────
            "Target.attachToTarget" => {
                let target_id = params
                    .get("targetId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let tab_id = target_id
                    .strip_prefix("tab-")
                    .and_then(|s| s.parse::<i32>().ok());

                // Owned-set guard for thread-scoped sessions: refuse to
                // attach to tabs the agent doesn't own. Primary safety
                // rail preventing the model from driving the user's
                // other Chrome tabs even if it learns an unrelated
                // tab id. Legacy unscoped sessions stay permissive.
                if let (Some(tid), Some(requested_tab)) = (self.thread_id.as_deref(), tab_id) {
                    if let OwnedTabCheck::Rejected { reason } =
                        check_tab_ownership(tid, requested_tab, "attach")
                    {
                        warn!(
                            "[cdp-proxy] thread={} rejected Target.attachToTarget for unowned tab {}",
                            tid, requested_tab
                        );
                        ctx.text(cdp_err(id, None, &reason).to_string());
                        return;
                    }
                }

                let thread_id_for_trace = self.thread_id.clone();
                let route = self.route.clone();
                match tab_id {
                    Some(tab_id) => {
                        actix_web::rt::spawn(async move {
                            debug!("[cdp-proxy] Target.attachToTarget tab={}", tab_id);
                            let mut params = json!({ "tabId": tab_id });
                            if let Some(tid) = thread_id_for_trace.as_deref() {
                                params["sessionId"] = json!(tid);
                            }
                            let req = ExtensionRequest {
                                request_id: Uuid::new_v4().to_string(),
                                action: "attach_debugger".to_string(),
                                params,
                            };
                            let res = send_over_bridge_tracked(&route, req).await;
                            // Trust the bridge's success bit. Previously we
                            // treated "Another debugger is already attached"
                            // as success, but that handed back a fake
                            // sessionId — subsequent commands then hung for
                            // 60s because the extension never actually got
                            // a debugger handle. attachDebugger now self-
                            // heals stale-self attachments; anything still
                            // failing is a foreign debugger and we surface
                            // it cleanly so agent-browser can move on.
                            let attached_ok = matches!(&res, Ok(r) if r.response.success);

                            if attached_ok {
                                // The tab lives in the browser that answered;
                                // its events and follow-up commands stay there.
                                let browser_id = res
                                    .as_ref()
                                    .map(|r| r.browser_id.clone())
                                    .unwrap_or_default();
                                let sid = Uuid::new_v4().to_string();
                                debug!("[cdp-proxy] attach OK tab={} session={}", tab_id, sid);
                                // Begin API-mining trace capture for this
                                // (thread, tab) pair on thread-scoped sessions.
                                // Once the backend bootstrap or CDP client
                                // enables Network, events flow through the
                                // bridge; this registers a parallel subscriber
                                // that builds NetworkTraceEvents and stages
                                // them in the per-thread buffer drained by
                                // GET /trace/drain.
                                if let Some(tid) = thread_id_for_trace.as_deref() {
                                    trace_capture::enable_for_thread_tab(tid, &browser_id, tab_id);
                                }
                                addr.do_send(SubscribeTabEvents {
                                    browser_id: browser_id.clone(),
                                    tab_id,
                                });
                                spawn_post_attach_cdp_bootstrap(
                                    BridgeRoute::Browser(browser_id),
                                    tab_id,
                                    thread_id_for_trace.is_some(),
                                );
                                addr.do_send(RegisterSession {
                                    session_id: sid.clone(),
                                    tab_id,
                                });
                                addr.do_send(SendText(
                                    cdp_ok(id, None, json!({ "sessionId": sid })).to_string(),
                                ));
                                // Emit the unsolicited Target.attachedToTarget
                                // event real Chrome sends after attach. Without
                                // it, agent-browser and other CDP clients
                                // wait indefinitely for session confirmation.
                                addr.do_send(EmitAttachedToTarget {
                                    session_id: sid,
                                    tab_id,
                                });
                            } else {
                                let msg = match res {
                                    Ok(r) => r.response.error.unwrap_or_default(),
                                    Err(e) => e.to_string(),
                                };
                                warn!("[cdp-proxy] attach FAILED tab={} error={}", tab_id, msg);
                                addr.do_send(SendText(cdp_err(id, None, &msg).to_string()));
                            }
                        });
                    },
                    None => {
                        ctx.text(
                            cdp_err(id, None, &format!("invalid targetId: {}", target_id))
                                .to_string(),
                        );
                    },
                }
            },

            // ── Target.detachFromTarget ──────────────────────────────────
            "Target.detachFromTarget" => {
                for sid in sessions_to_detach(&params, session_id.as_deref(), &self.sessions) {
                    let released_tab = self.sessions.remove(&sid);
                    // Keep the iframe-discriminator set in sync — a
                    // sessionId released here may have been an iframe
                    // session we registered via auto-attach.
                    let was_iframe = self.iframe_session_ids.remove(&sid);
                    // A top-level tab no other session of ours still
                    // references is released on the extension side too,
                    // so the client's explicit detach clears Chrome's
                    // debugging bar the way real Chrome would — unless
                    // another connection of this thread is open: the
                    // tab's one `chrome.debugger` attachment is shared,
                    // and releasing it would cut the agent's own session
                    // out from under a secure fill's side connection.
                    if let Some(tab_id) = released_tab {
                        let still_attached = self.sessions.values().any(|t| *t == tab_id);
                        let shared = self
                            .thread_id
                            .as_deref()
                            .is_some_and(bridge::thread_has_other_live_session);
                        if !was_iframe && !still_attached && !shared {
                            spawn_detach_debugger(
                                self.thread_id.clone(),
                                self.teardown_route(),
                                tab_id,
                            );
                        }
                    }
                }
                ctx.text(cdp_ok(id, session_id.as_deref(), json!({})).to_string());
            },

            // ── Target.closeTarget ────────────────────────────────────────
            // Owned-set guarded: only accept close requests for tabs the
            // agent actually owns (Tier 1 in our window, or Tier 2
            // popups it spawned). Closing the seed tab is rejected with
            // a tailored message — the agent ends a session through its
            // terminal control tool, not by closing its own primary
            // tab. Legacy unscoped sessions stay permissive.
            "Target.closeTarget" => {
                let target_id = params
                    .get("targetId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let tab_id = target_id
                    .strip_prefix("tab-")
                    .and_then(|s| s.parse::<i32>().ok());
                let echo_sid = session_id.clone();
                match (self.thread_id.as_deref(), tab_id) {
                    (Some(tid), Some(requested_tab)) => {
                        if let OwnedTabCheck::Rejected { reason } =
                            check_tab_ownership(tid, requested_tab, "close")
                        {
                            warn!(
                                "[cdp-proxy] thread={} rejected Target.closeTarget for unowned tab {}",
                                tid, requested_tab
                            );
                            ctx.text(cdp_err(id, echo_sid.as_deref(), &reason).to_string());
                            return;
                        }
                        let thread_id_clone = tid.to_string();
                        let route = self.route.clone();
                        actix_web::rt::spawn(async move {
                            let req = ExtensionRequest {
                                request_id: Uuid::new_v4().to_string(),
                                action: "close_tab".to_string(),
                                params: json!({ "tabId": requested_tab }),
                            };
                            let resp = match send_over_bridge_via(&route, req).await {
                                Ok(r) if r.success => {
                                    debug!(
                                        "[cdp-proxy] thread={} closed tab {}",
                                        thread_id_clone, requested_tab
                                    );
                                    drop_owned_tab(&thread_id_clone, requested_tab);
                                    cdp_ok(id, echo_sid.as_deref(), json!({ "success": true }))
                                },
                                Ok(r) => {
                                    let err = r.error.unwrap_or_default();
                                    warn!(
                                        "[cdp-proxy] thread={} close_tab FAILED tab={} error={}",
                                        thread_id_clone, requested_tab, err
                                    );
                                    cdp_err(id, echo_sid.as_deref(), &err)
                                },
                                Err(e) => {
                                    warn!(
                                        "[cdp-proxy] thread={} close_tab bridge ERR tab={} error={}",
                                        thread_id_clone, requested_tab, e
                                    );
                                    cdp_err(id, echo_sid.as_deref(), &e.to_string())
                                },
                            };
                            addr.do_send(SendText(resp.to_string()));
                        });
                    },
                    (None, _) => {
                        // Legacy unscoped path — fall through to the
                        // generic forward arm by spawning the same
                        // close_tab dispatch without the owned-set
                        // check. Mirrors the rest of the legacy path's
                        // permissive semantics.
                        let echo_sid_for_legacy = echo_sid.clone();
                        let route = self.route.clone();
                        actix_web::rt::spawn(async move {
                            let Some(requested_tab) = tab_id else {
                                addr.do_send(SendText(
                                    cdp_err(
                                        id,
                                        echo_sid_for_legacy.as_deref(),
                                        &format!("invalid targetId: {}", target_id),
                                    )
                                    .to_string(),
                                ));
                                return;
                            };
                            let req = ExtensionRequest {
                                request_id: Uuid::new_v4().to_string(),
                                action: "close_tab".to_string(),
                                params: json!({ "tabId": requested_tab }),
                            };
                            let resp = match send_over_bridge_via(&route, req).await {
                                Ok(r) if r.success => cdp_ok(
                                    id,
                                    echo_sid_for_legacy.as_deref(),
                                    json!({ "success": true }),
                                ),
                                Ok(r) => cdp_err(
                                    id,
                                    echo_sid_for_legacy.as_deref(),
                                    &r.error.unwrap_or_default(),
                                ),
                                Err(e) => {
                                    cdp_err(id, echo_sid_for_legacy.as_deref(), &e.to_string())
                                },
                            };
                            addr.do_send(SendText(resp.to_string()));
                        });
                    },
                    (Some(_), None) => {
                        ctx.text(
                            cdp_err(
                                id,
                                echo_sid.as_deref(),
                                &format!("invalid targetId: {}", target_id),
                            )
                            .to_string(),
                        );
                    },
                }
            },

            // ── Everything else: forward via debugger_command ────────────
            _ => {
                // Page-level connections ignore sessionId and always use the
                // default tab.  Browser-level connections look up by sessionId.
                let tab_id = self.default_tab_id.or_else(|| {
                    session_id
                        .as_ref()
                        .and_then(|s| self.sessions.get(s))
                        .copied()
                });

                match tab_id {
                    Some(tab_id) => {
                        let echo_sid = session_id.clone();
                        let method_for_log = method.clone();
                        let thread_id_for_signal = self.thread_id.clone();
                        let session_id_for_signal = echo_sid.clone();
                        let method_for_signal = method.clone();
                        let params_for_signal = params.clone();
                        // Forward the sessionId to the extension only
                        // when it's an iframe session — those are the
                        // session ids Chrome itself issued via
                        // auto-attach, so its debugger will recognize
                        // them and route the command to the OOPIF.
                        //
                        // The proxy-minted main-session uuid (from
                        // `Target.attachToTarget` above) is also in
                        // `self.sessions`, but Chrome never saw it, so
                        // forwarding it triggers
                        // `-32001 "Session with given id not found"`
                        // and every command on the main tab fails.
                        // Discriminator: `iframe_session_ids`.
                        //
                        // For main-session commands we forward without
                        // a sessionId field — Chrome's
                        // `chrome.debugger.sendCommand({tabId}, …)`
                        // then targets the implicit main session of
                        // that tab, which is what we want.
                        let forward_session_id = session_id
                            .as_ref()
                            .filter(|sid| self.iframe_session_ids.contains(*sid))
                            .cloned();
                        let route = self.route.clone();
                        actix_web::rt::spawn(async move {
                            debug!(
                                "[cdp-proxy] forward tab={} method={} session={:?}",
                                tab_id, method_for_log, forward_session_id
                            );
                            let mut bridge_params = json!({
                                "tabId": tab_id,
                                "method": method,
                                "params": params,
                            });
                            if let Some(sid) = forward_session_id.as_ref() {
                                bridge_params["sessionId"] = json!(sid);
                            }
                            let req = ExtensionRequest {
                                request_id: Uuid::new_v4().to_string(),
                                action: "debugger_command".to_string(),
                                params: bridge_params,
                            };
                            let resp = match send_over_bridge_via(&route, req).await {
                                Ok(r) if r.success => {
                                    debug!(
                                        "[cdp-proxy] forward OK tab={} method={}",
                                        tab_id, method_for_log
                                    );
                                    record_automation_cdp_command(
                                        thread_id_for_signal.as_deref(),
                                        tab_id,
                                        session_id_for_signal.as_deref(),
                                        &method_for_signal,
                                        &params_for_signal,
                                        None,
                                    );
                                    // The extension's sendDebuggerCommand wraps
                                    // the raw CDP result in
                                    // `{ result: <cdp_response>, ... }`.
                                    // Unwrap one layer so agent-browser receives
                                    // the canonical CDP shape
                                    // (e.g. `{frameId, loaderId}` for
                                    // Page.navigate, `{type, value}` for
                                    // Runtime.evaluate). Without this,
                                    // agent-browser fails to deserialize
                                    // every response with "missing field".
                                    let raw = r.result.unwrap_or(json!({}));
                                    let cdp_result = raw.get("result").cloned().unwrap_or(raw);
                                    cdp_ok(id, echo_sid.as_deref(), cdp_result)
                                },
                                Ok(r) => {
                                    let err = r.error.unwrap_or_default();
                                    warn!(
                                        "[cdp-proxy] forward FAILED tab={} method={} error={}",
                                        tab_id, method_for_log, err
                                    );
                                    record_automation_cdp_command(
                                        thread_id_for_signal.as_deref(),
                                        tab_id,
                                        session_id_for_signal.as_deref(),
                                        &method_for_signal,
                                        &params_for_signal,
                                        Some(&err),
                                    );
                                    cdp_err(id, echo_sid.as_deref(), &err)
                                },
                                Err(e) => {
                                    let err = e.to_string();
                                    warn!(
                                        "[cdp-proxy] bridge ERR tab={} method={} error={}",
                                        tab_id, method_for_log, err
                                    );
                                    record_automation_cdp_command(
                                        thread_id_for_signal.as_deref(),
                                        tab_id,
                                        session_id_for_signal.as_deref(),
                                        &method_for_signal,
                                        &params_for_signal,
                                        Some(&err),
                                    );
                                    cdp_err(id, echo_sid.as_deref(), &err)
                                },
                            };
                            addr.do_send(SendText(resp.to_string()));
                        });
                    },
                    None => {
                        // Reaches here only for sessionIds the proxy
                        // doesn't recognise. Iframe sessions are
                        // registered automatically when Chrome's
                        // auto-attach emits `Target.attachedToTarget`
                        // (see `update_iframe_sessions_from_event`),
                        // so iframe-targeted commands route via the
                        // bridge with the session_id forwarded. The
                        // remaining "no tab" cases are workers,
                        // service workers, and pre-attach races —
                        // genuine capability gaps the operator should
                        // see. Logged at WARN per the plan's stance:
                        // logs are signal; replacing the warning with
                        // a no-op response would hide the gap. The
                        // error response back to the client is
                        // preserved verbatim so agent-browser sees
                        // the explicit "no tab" failure.
                        warn!(
                            "CDP proxy: no tab for session {:?}, method={}",
                            session_id, method
                        );
                        ctx.text(
                            cdp_err(
                                id,
                                session_id.as_deref(),
                                "no tab attached for this session",
                            )
                            .to_string(),
                        );
                    },
                }
            },
        }
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Build a CDP success response, including sessionId only when present.
fn cdp_ok(id: u64, session_id: Option<&str>, result: Value) -> Value {
    let mut resp = json!({ "id": id, "result": result });
    if let Some(sid) = session_id {
        resp["sessionId"] = json!(sid);
    }
    resp
}

/// Build a CDP error response, including sessionId only when present.
fn cdp_err(id: u64, session_id: Option<&str>, message: &str) -> Value {
    let mut resp = json!({
        "id": id,
        "error": { "code": -32000, "message": message }
    });
    if let Some(sid) = session_id {
        resp["sessionId"] = json!(sid);
    }
    resp
}

fn record_automation_cdp_command(
    thread_id: Option<&str>,
    tab_id: i32,
    session_id: Option<&str>,
    method: &str,
    params: &Value,
    error: Option<&str>,
) {
    let Some(thread_id) = thread_id.filter(|tid| !tid.trim().is_empty()) else {
        return;
    };
    let (url, origin) = automation_signal_url_for_command(method, params);
    page_signals::record(AmbientPageSignal {
        event_id: Uuid::new_v4().to_string(),
        thread_id: thread_id.to_string(),
        tab_id,
        session_id: session_id.map(ToString::to_string),
        event_kind: "automation_cdp_command".to_string(),
        timestamp: chrono::Utc::now().timestamp_millis(),
        url,
        origin,
        capture: Some(AmbientCaptureStatus {
            degraded: error.is_some(),
            error: error.map(ToString::to_string),
            method: Some(method.to_string()),
        }),
        capture_source: Some("automation_cdp_mirror".to_string()),
        ..Default::default()
    });
}

fn record_automation_cdp_event(
    thread_id: &str,
    tab_id: i32,
    session_id: Option<&str>,
    method: &str,
    params: &Value,
) {
    if !should_mirror_cdp_event(method) {
        return;
    }
    let (url, origin) = automation_signal_url_for_event(method, params);
    page_signals::record(AmbientPageSignal {
        event_id: Uuid::new_v4().to_string(),
        thread_id: thread_id.to_string(),
        tab_id,
        session_id: session_id.map(ToString::to_string),
        event_kind: "automation_cdp_event".to_string(),
        timestamp: chrono::Utc::now().timestamp_millis(),
        url,
        origin,
        capture: Some(AmbientCaptureStatus {
            degraded: true,
            error: None,
            method: Some(method.to_string()),
        }),
        capture_source: Some("automation_cdp_mirror".to_string()),
        ..Default::default()
    });
}

fn should_mirror_cdp_event(method: &str) -> bool {
    matches!(
        method,
        "Page.loadEventFired" | "Page.frameNavigated" | "DOM.documentUpdated"
    )
}

fn automation_signal_url_for_command(
    method: &str,
    params: &Value,
) -> (Option<String>, Option<String>) {
    match method {
        "Page.navigate" => sanitized_url_and_origin(params.get("url").and_then(Value::as_str)),
        _ => (None, None),
    }
}

fn automation_signal_url_for_event(
    method: &str,
    params: &Value,
) -> (Option<String>, Option<String>) {
    match method {
        "Page.frameNavigated" => sanitized_url_and_origin(
            params
                .get("frame")
                .and_then(|frame| frame.get("url"))
                .and_then(Value::as_str),
        ),
        _ => (None, None),
    }
}

fn sanitized_url_and_origin(raw_url: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(raw_url) = raw_url else {
        return (None, None);
    };
    if !(raw_url.starts_with("http://") || raw_url.starts_with("https://")) {
        return (None, None);
    }

    let without_fragment = raw_url.split('#').next().unwrap_or(raw_url);
    let (path, query) = without_fragment
        .split_once('?')
        .map(|(path, query)| (path, Some(query)))
        .unwrap_or((without_fragment, None));
    let url = if let Some(query) = query {
        let redacted = query
            .split('&')
            .map(redact_sensitive_query_pair)
            .collect::<Vec<_>>()
            .join("&");
        format!("{path}?{redacted}")
    } else {
        path.to_string()
    };
    let origin = origin_from_url(&url);
    (Some(url), origin)
}

fn redact_sensitive_query_pair(pair: &str) -> String {
    let key = pair.split_once('=').map(|(key, _)| key).unwrap_or(pair);
    let lower = key.to_ascii_lowercase();
    let sensitive = [
        "token", "secret", "password", "passwd", "auth", "key", "session", "csrf", "xsrf", "otp",
    ];
    if sensitive.iter().any(|needle| lower.contains(needle)) {
        if let Some((key, _)) = pair.split_once('=') {
            format!("{key}=[REDACTED]")
        } else {
            format!("{key}=[REDACTED]")
        }
    } else {
        pair.to_string()
    }
}

fn origin_from_url(url: &str) -> Option<String> {
    let scheme_end = url.find("://")?;
    let rest_start = scheme_end + 3;
    let rest = &url[rest_start..];
    let host_end = rest.find('/').unwrap_or(rest.len());
    Some(url[..rest_start + host_end].to_string())
}

/// `focus_emulation`: a task's tab usually renders in the background (a tab
/// group in the owner's window, or a window behind theirs). Chrome then hides
/// the page, stops animation frames and slows timers to one a second, and
/// load events go missing. Emulated focus keeps it rendering like the front
/// tab; measured on 10-29, a 6-minute background tab stayed visible, drew
/// frames and kept normal timers with it, and went hidden and missed a load
/// without it.
fn spawn_post_attach_cdp_bootstrap(route: BridgeRoute, tab_id: i32, focus_emulation: bool) {
    actix_web::rt::spawn(async move {
        let mut commands = vec![
            ("Network.enable", json!({})),
            ("Runtime.enable", json!({})),
            ("DOM.enable", json!({})),
            ("Page.enable", json!({})),
            (
                "Target.setAutoAttach",
                json!({
                    "autoAttach": true,
                    "waitForDebuggerOnStart": false,
                    "flatten": true,
                    "filter": [{ "type": "iframe", "exclude": false }],
                }),
            ),
        ];
        if focus_emulation {
            commands.push((
                "Emulation.setFocusEmulationEnabled",
                json!({ "enabled": true }),
            ));
            commands.push(("Page.setWebLifecycleState", json!({ "state": "active" })));
        }

        for (method, params) in commands {
            let req = ExtensionRequest {
                request_id: Uuid::new_v4().to_string(),
                action: "debugger_command".to_string(),
                params: json!({
                    "tabId": tab_id,
                    "method": method,
                    "params": params,
                }),
            };

            match send_over_bridge_via(&route, req).await {
                Ok(response) if response.success => {
                    debug!(
                        "[cdp-proxy] post-attach bootstrap OK tab={} method={}",
                        tab_id, method
                    );
                },
                Ok(response) => {
                    warn!(
                        "[cdp-proxy] post-attach bootstrap FAILED tab={} method={} error={}",
                        tab_id,
                        method,
                        response.error.unwrap_or_default()
                    );
                },
                Err(error) => {
                    warn!(
                        "[cdp-proxy] post-attach bootstrap bridge ERR tab={} method={} error={}",
                        tab_id, method, error
                    );
                    break;
                },
            }
        }
    });
}

fn host_from(req: &HttpRequest) -> String {
    req.headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("127.0.0.1:3003")
        .to_owned()
}

/// Look up (or lazily create) the dedicated Chrome **window** for a
/// magician thread. Mirrors the old create-window behavior
/// Window-isolation path so downstream behavior stays consistent — every
/// magician thread gets its own window, and the user's other windows are
/// never visible to the agent.
///
/// On first call for a thread, fires a `create_window` bridge action
/// (which the extension translates to `chrome.windows.create`) at default
/// viewport (1280x800). Subsequent calls return the cached window id.
///
/// Returns `None` if the bridge is unreachable or the create_window
/// action failed; the caller falls back to an empty target list.
/// What the extension's `list_tabs` says about a cached dedicated window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowLiveness {
    /// At least one tab is in the window: it exists.
    Live,
    /// The extension answered and no tab is in the window. Chrome keeps no
    /// empty windows, so it was closed — by the run that used it or by the
    /// user.
    Gone,
    /// No answer to judge by (bridge unreachable / call failed).
    Unknown,
}

/// Judge a cached window by the extension's `list_tabs` result. Tabs held by
/// another debugger still count as present: the window exists even if the
/// agent cannot attach to that tab.
fn window_liveness_from_list_tabs(result: Option<&Value>, window_id: i32) -> WindowLiveness {
    let tabs: &[Value] = match result {
        Some(Value::Array(arr)) => arr,
        Some(Value::Object(obj)) => match obj.get("tabs") {
            Some(Value::Array(arr)) => arr,
            _ => return WindowLiveness::Unknown,
        },
        _ => return WindowLiveness::Unknown,
    };
    if tabs
        .iter()
        .any(|tab| tab.get("windowId").and_then(|v| v.as_i64()) == Some(i64::from(window_id)))
    {
        WindowLiveness::Live
    } else {
        WindowLiveness::Gone
    }
}

/// An in-window thread's "window" is the owner's: it is live while one of
/// the thread's own tabs is, whatever else that window holds.
fn tabs_liveness_from_list_tabs(result: Option<&Value>, owned: &[i32]) -> WindowLiveness {
    let tabs: &[Value] = match result {
        Some(Value::Array(arr)) => arr,
        Some(Value::Object(obj)) => match obj.get("tabs") {
            Some(Value::Array(arr)) => arr,
            _ => return WindowLiveness::Unknown,
        },
        _ => return WindowLiveness::Unknown,
    };
    let listed = |tab_id: i32| {
        tabs.iter()
            .any(|tab| tab.get("tabId").and_then(Value::as_i64) == Some(i64::from(tab_id)))
    };
    if owned.iter().any(|tab_id| listed(*tab_id)) {
        WindowLiveness::Live
    } else {
        WindowLiveness::Gone
    }
}

/// Drop a thread's mapping to a window that no longer exists, and the tabs
/// that lived in it. The thread's next call opens a fresh surface.
fn forget_thread_window(thread_id: &str, window_id: i32) {
    if let Ok(mut map) = THREAD_WINDOWS.lock() {
        if map.get(thread_id) == Some(&window_id) {
            map.remove(thread_id);
        }
    }
    if let Ok(mut surfaces) = THREAD_SURFACES.lock() {
        surfaces.remove(thread_id);
    }
    if let Ok(mut owned) = THREAD_OWNED_TABS.lock() {
        owned.remove(thread_id);
    }
    let _ = take_cached_thread_targets(thread_id);
    info!(
        "[cdp-proxy] forgot closed dedicated window {} for thread={}",
        window_id, thread_id
    );
}

/// Whether the cached dedicated window for `thread_id` still exists in
/// Chrome. A shared thread id (`magician-chat-<thread>`) outlives the window a
/// run closed at its end; before this check the stale id was returned as-is
/// and every `Target.createTarget` on the thread answered "no tab".
async fn cached_window_is_live(thread_id: &str, window_id: i32) -> WindowLiveness {
    let req = ExtensionRequest {
        request_id: Uuid::new_v4().to_string(),
        action: "list_tabs".to_string(),
        params: json!({}),
    };
    let in_window = THREAD_SURFACES
        .lock()
        .ok()
        .is_some_and(|surfaces| surfaces.get(thread_id).map(String::as_str) == Some("in_window"));
    match send_over_bridge_via(&BridgeRoute::Thread(thread_id.to_string()), req).await {
        Ok(r) if r.success && in_window => {
            tabs_liveness_from_list_tabs(r.result.as_ref(), &owned_tabs(thread_id))
        },
        Ok(r) if r.success => window_liveness_from_list_tabs(r.result.as_ref(), window_id),
        Ok(r) => {
            debug!(
                "[cdp-proxy] list_tabs failed while checking window {} for thread={}: {}",
                window_id,
                thread_id,
                r.error.as_deref().unwrap_or("(no error)")
            );
            WindowLiveness::Unknown
        },
        Err(e) => {
            debug!(
                "[cdp-proxy] list_tabs bridge error while checking window {} for thread={}: {}",
                window_id, thread_id, e
            );
            WindowLiveness::Unknown
        },
    }
}

async fn ensure_thread_window(thread_id: &str) -> Option<i32> {
    let _guard = thread_state_guard(thread_id).await;
    let cached = {
        let map = THREAD_WINDOWS.lock().ok()?;
        map.get(thread_id).copied()
    };
    if let Some(window_id) = cached {
        match cached_window_is_live(thread_id, window_id).await {
            WindowLiveness::Live | WindowLiveness::Unknown => return Some(window_id),
            WindowLiveness::Gone => forget_thread_window(thread_id, window_id),
        }
    }
    let label = THREAD_LABELS
        .lock()
        .ok()
        .and_then(|labels| labels.get(thread_id).cloned());
    if !bridge::thread_has_live_session(thread_id) {
        return None;
    }
    let req = ExtensionRequest {
        request_id: Uuid::new_v4().to_string(),
        action: "create_window".to_string(),
        params: json!({
            "url": "about:blank",
            "width": 1280,
            "height": 800,
            "sessionId": thread_id,
            "connect_to_existing": false,
            // The extension opens the task in a tab group or a window of its
            // own per `surface`; neither takes the owner's focus.
            "surface": automation_surface(),
            "focused": false,
            "label": label,
        }),
    };
    let resp = match send_over_bridge_via(&BridgeRoute::Thread(thread_id.to_string()), req).await {
        Ok(r) if r.success => r.result,
        Ok(r) => {
            warn!(
                "[cdp-proxy] create_window failed for thread={}: {}",
                thread_id,
                r.error.as_deref().unwrap_or("(no error)")
            );
            return None;
        },
        Err(e) => {
            warn!(
                "[cdp-proxy] create_window bridge error for thread={}: {}",
                thread_id, e
            );
            return None;
        },
    };
    let window_id = resp
        .as_ref()
        .and_then(|v| v.get("windowId"))
        .and_then(|v| v.as_i64())
        .map(|n| n as i32)?;
    if let Ok(mut map) = THREAD_WINDOWS.lock() {
        map.insert(thread_id.to_string(), window_id);
    }
    // An extension that predates surfaces always opened a window.
    let surface = resp
        .as_ref()
        .and_then(|v| v.get("surface"))
        .and_then(Value::as_str)
        .unwrap_or("own_window")
        .to_string();
    if let Ok(mut surfaces) = THREAD_SURFACES.lock() {
        surfaces.insert(thread_id.to_string(), surface.clone());
    }
    let initial_targets = target_infos_from_create_window_response(resp.as_ref(), window_id);
    if !initial_targets.is_empty() {
        seed_owned_tabs_from_targets(thread_id, &initial_targets);
        cache_thread_targets(thread_id, initial_targets);
    }
    // Outside the macro: tracing's own `Value` trait shadows serde_json's there.
    let note = resp
        .as_ref()
        .and_then(|v| v.get("surfaceNote"))
        .and_then(Value::as_str)
        .map(|note| format!(" ({note})"))
        .unwrap_or_default();
    info!(
        "[cdp-proxy] opened thread={} as {} in window {}{}",
        thread_id, surface, window_id, note
    );
    Some(window_id)
}

/// Enumerate task-owned targets across windows. Window membership alone is
/// never sufficient. Bootstrap cache is used only on failed bridge discovery.
async fn fetch_owned_targets_for_thread(thread_id: &str, seed_window: i32) -> Vec<Value> {
    let req = ExtensionRequest {
        request_id: Uuid::new_v4().to_string(),
        action: "list_tabs".to_string(),
        params: json!({}),
    };
    let raw = match send_over_bridge_via(&BridgeRoute::Thread(thread_id.to_string()), req).await {
        Ok(r) if r.success => result_to_targets(r.result.as_ref()),
        Ok(r) => {
            warn!(
                "[cdp-proxy] list_tabs failed for thread={}; using bootstrap target cache if present: {}",
                thread_id,
                r.error.as_deref().unwrap_or("(no error)")
            );
            return take_cached_thread_targets(thread_id)
                .into_iter()
                .filter(|target| extract_window_id(target) == Some(seed_window))
                .collect();
        },
        Err(e) => {
            warn!(
                "[cdp-proxy] list_tabs bridge error for thread={}; using bootstrap target cache if present: {}",
                thread_id, e
            );
            return take_cached_thread_targets(thread_id)
                .into_iter()
                .filter(|target| extract_window_id(target) == Some(seed_window))
                .collect();
        },
    };
    let _ = take_cached_thread_targets(thread_id);

    filter_owned_targets(thread_id, raw)
}

/// Fetch targets for an extension-bound existing tab. Unlike
/// [`fetch_owned_targets_for_thread`], this does not treat the bound tab's
/// whole Chrome window as owned; only the explicitly bound tab and opener-
/// chained tabs already in the owned set are exposed.
async fn fetch_bound_targets_for_thread(thread_id: &str, bound_tab_id: i32) -> Vec<Value> {
    seed_owned_tabs(thread_id, [bound_tab_id]);
    let targets = match fetch_targets_via_bridge(&BridgeRoute::Thread(thread_id.to_string())).await
    {
        Some(targets) => {
            let _ = take_cached_thread_targets(thread_id);
            targets
        },
        None => {
            warn!(
                "[cdp-proxy] list_tabs bridge error for bound thread={}; using bootstrap target cache if present",
                thread_id
            );
            take_cached_thread_targets(thread_id)
        },
    };

    filter_owned_targets(thread_id, targets)
}

fn filter_owned_targets(thread_id: &str, targets: Vec<Value>) -> Vec<Value> {
    // Window membership is not ownership: the user can open another tab there.
    let created = targets
        .iter()
        .filter(|target| {
            target.get("automationSessionId").and_then(Value::as_str) == Some(thread_id)
        })
        .filter_map(|target| target_id_to_tab_id(target.get("targetId")))
        .collect::<Vec<_>>();
    seed_owned_tabs(thread_id, created);
    let owned: HashSet<i32> = owned_tabs(thread_id).into_iter().collect();
    targets
        .into_iter()
        .filter(|target| {
            target_id_to_tab_id(target.get("targetId")).is_some_and(|id| owned.contains(&id))
        })
        .collect()
}

/// Pull `windowId` from a target produced by `result_to_targets` or the
/// thread target cache. `windowId` is an internal extension to CDP TargetInfo
/// used only for Magicutor's thread/window ownership filter.
fn extract_window_id(tab: &Value) -> Option<i32> {
    tab.get("windowId")
        .and_then(Value::as_i64)
        .map(|n| n as i32)
}

/// Call list_tabs via bridge on the browser `route` names and return CDP
/// TargetInfo JSON objects.
async fn fetch_targets_via_bridge(route: &BridgeRoute) -> Option<Vec<Value>> {
    let req = ExtensionRequest {
        request_id: Uuid::new_v4().to_string(),
        action: "list_tabs".to_string(),
        params: json!({}),
    };
    match send_over_bridge_via(route, req).await {
        Ok(r) if r.success => Some(result_to_targets(r.result.as_ref())),
        _ => None,
    }
}

/// Convenience wrapper used by the HTTP /json/list handler.
async fn fetch_targets(host: &str, route: &BridgeRoute) -> Vec<Value> {
    let mut targets = fetch_targets_via_bridge(route).await.unwrap_or_default();
    // Page URLs name the browser the tab ids came from, by fingerprint.
    let browser_query = match route {
        BridgeRoute::Browser(browser_id) => {
            format!("?browser={}", bridge::browser_fingerprint(browser_id))
        },
        _ => String::new(),
    };
    // Patch in the webSocketDebuggerUrl while we have the host string
    for t in &mut targets {
        if let Some(tab_id) = t.get("targetId").and_then(|v| v.as_str()) {
            let raw_id = tab_id.strip_prefix("tab-").unwrap_or(tab_id);
            t["webSocketDebuggerUrl"] = json!(format!(
                "ws://{}/devtools/page/{}{}",
                host, raw_id, browser_query
            ));
        }
    }
    targets
}

fn target_info_from_fields(
    tab_id: i32,
    url: Option<&str>,
    title: Option<&str>,
    window_id: Option<i32>,
) -> Value {
    let mut target = json!({
        "targetId": format!("tab-{}", tab_id),
        "type": "page",
        "title": title.unwrap_or(""),
        "url": url.unwrap_or("about:blank"),
        "attached": false,
        "canAccessOpener": false,
        "browserContextId": "default",
    });
    if let Some(window_id) = window_id {
        target["windowId"] = json!(window_id);
    }
    target
}

fn target_info_from_bridge_tab(
    fallback_tab_id: i32,
    fallback_window_id: Option<i32>,
    result: Option<&Value>,
) -> Value {
    let tab_id = result
        .and_then(|v| v.get("tabId"))
        .and_then(Value::as_i64)
        .map(|n| n as i32)
        .unwrap_or(fallback_tab_id);
    let window_id = result
        .and_then(|v| v.get("windowId"))
        .and_then(Value::as_i64)
        .map(|n| n as i32)
        .or(fallback_window_id);
    let url = result.and_then(|v| v.get("url")).and_then(Value::as_str);
    let title = result.and_then(|v| v.get("title")).and_then(Value::as_str);
    target_info_from_fields(tab_id, url, title, window_id)
}

fn target_infos_from_create_window_response(result: Option<&Value>, window_id: i32) -> Vec<Value> {
    let Some(Value::Array(tabs)) = result.and_then(|v| v.get("tabs")) else {
        return Vec::new();
    };
    tabs.iter()
        .filter_map(|tab| {
            let tab_id = tab.get("tabId").and_then(Value::as_i64).map(|n| n as i32)?;
            let url = tab.get("url").and_then(Value::as_str);
            let title = tab.get("title").and_then(Value::as_str);
            Some(target_info_from_fields(tab_id, url, title, Some(window_id)))
        })
        .collect()
}

fn result_to_targets(result: Option<&Value>) -> Vec<Value> {
    let tabs: &[Value] = match result {
        Some(Value::Array(arr)) => arr,
        Some(Value::Object(obj)) => match obj.get("tabs") {
            Some(Value::Array(arr)) => arr,
            _ => return vec![],
        },
        _ => return vec![],
    };

    tabs.iter()
        .filter(|tab| {
            // Drop tabs the extension flagged as held by another debugger
            // (DevTools / a different extension). Including them in
            // Target.getTargets just lets agent-browser pick a tab it
            // can't actually attach to — and the extension's attach call
            // will fail. listTabs sets `attachable: !chromeAttached || ownedByUs`.
            // Tabs predating this enrichment lack the field; treat absent
            // as `true` (legacy behaviour) so we don't accidentally hide
            // every tab against an older extension build.
            tab.get("attachable")
                .and_then(|v| v.as_bool())
                .unwrap_or(true)
        })
        .map(|tab| {
            let tab_id = tab.get("tabId").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let url = tab.get("url").and_then(|v| v.as_str());
            let title = tab.get("title").and_then(|v| v.as_str());
            let window_id = tab.get("windowId").and_then(|v| v.as_i64());
            // Always advertise `attached: false` so agent-browser issues
            // Target.attachToTarget. Even tabs we previously attached to
            // need a fresh session for this WebSocket — there's no way to
            // resume a chrome.debugger session through a different proxy
            // connection.
            //
            // `windowId` is preserved so the per-thread owned-set
            // filter in `fetch_owned_targets_for_thread` can spot
            // Tier-1 tabs (those in the seed window) without an extra
            // bridge call. CDP itself doesn't define `windowId` on
            // `TargetInfo`, but agent-browser tolerates extra fields
            // and our internal callers depend on it.
            let mut target =
                target_info_from_fields(tab_id, url, title, window_id.map(|n| n as i32));
            if let Some(owner) = tab.get("automationSessionId").and_then(Value::as_str) {
                target["automationSessionId"] = json!(owner);
            }
            target
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumeration_never_claims_a_user_tab_from_window_membership() {
        let thread = fresh_thread_id("created-only");
        seed_owned_tabs(&thread, [1]);
        let targets = result_to_targets(Some(&json!({"tabs": [
            {"tabId": 1, "windowId": 10},
            {"tabId": 2, "windowId": 10},
            {"tabId": 3, "windowId": 20, "automationSessionId": thread},
            {"tabId": 4, "windowId": 10, "automationSessionId": "another-task"}
        ]})));
        let visible = filter_owned_targets(&thread, targets);
        assert_eq!(
            visible
                .iter()
                .map(|target| target["targetId"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["tab-1", "tab-3"]
        );
        assert!(!owned_tabs(&thread).contains(&2));
        clear_thread_state(&thread);
    }

    #[test]
    fn a_detach_names_its_session_in_params_or_by_target() {
        let mut sessions = HashMap::new();
        sessions.insert("s1".to_string(), 7);
        sessions.insert("s2".to_string(), 7);
        sessions.insert("s3".to_string(), 9);
        // MagicVault's fill adapter: `{"method":"Target.detachFromTarget",
        // "params":{"sessionId":…}}` with no message-level session.
        assert_eq!(
            sessions_to_detach(&json!({"sessionId": "s2"}), None, &sessions),
            vec!["s2"]
        );
        assert_eq!(
            sessions_to_detach(&json!({"targetId": "tab-7"}), None, &sessions),
            vec!["s1", "s2"]
        );
        assert_eq!(
            sessions_to_detach(&json!({}), Some("s3"), &sessions),
            vec!["s3"]
        );
        assert!(sessions_to_detach(&json!({}), None, &sessions).is_empty());
    }

    #[test]
    fn an_in_window_thread_is_live_while_one_of_its_own_tabs_is() {
        // The owner's window always has tabs; only the thread's count.
        let listed = json!({"tabs": [
            {"tabId": 1, "windowId": 10},
            {"tabId": 2, "windowId": 10},
        ]});
        assert!(matches!(
            tabs_liveness_from_list_tabs(Some(&listed), &[2, 7]),
            WindowLiveness::Live
        ));
        assert!(matches!(
            tabs_liveness_from_list_tabs(Some(&listed), &[7]),
            WindowLiveness::Gone
        ));
        assert!(matches!(
            tabs_liveness_from_list_tabs(Some(&listed), &[]),
            WindowLiveness::Gone
        ));
        assert!(matches!(
            tabs_liveness_from_list_tabs(None, &[2]),
            WindowLiveness::Unknown
        ));
    }

    #[test]
    fn a_tab_group_label_comes_from_the_cdp_url() {
        assert_eq!(
            thread_label_from_query("label=Reddit%20launch%20%20research&browser=x").as_deref(),
            Some("Reddit launch research")
        );
        assert_eq!(thread_label_from_query("label=%20%20"), None);
        assert_eq!(thread_label_from_query("browser=x"), None);
        let long = format!("label={}", "a".repeat(200));
        assert_eq!(
            thread_label_from_query(&long).map(|l| l.len()),
            Some(MAX_LABEL_CHARS)
        );
    }

    #[test]
    fn an_unknown_automation_surface_falls_back_to_auto() {
        set_automation_surface(" In_Window ");
        assert_eq!(automation_surface(), "in_window");
        set_automation_surface("tabs-everywhere");
        assert_eq!(automation_surface(), "auto");
    }

    #[test]
    fn a_page_event_never_goes_out_under_an_iframe_session_of_its_tab() {
        let mut session = CdpSession::browser_level(None, BridgeRoute::Default);
        session.sessions.insert("main-uuid".to_string(), 7);
        session.sessions.insert("other-tab".to_string(), 9);
        // Many iframe sessions, so hash order would put one of them before
        // the main session in any map iteration.
        for n in 0..32 {
            session.update_iframe_sessions_from_event(
                "Target.attachedToTarget",
                &json!({"sessionId": format!("IFRAME{n}"), "targetInfo": {"type": "iframe"}}),
                7,
            );
        }
        assert_eq!(
            session.event_session_id(7, None).as_deref(),
            Some("main-uuid")
        );
        assert_eq!(
            session.event_session_id(7, Some("IFRAME3")).as_deref(),
            Some("IFRAME3")
        );
        assert_eq!(session.event_session_id(8, None), None);
        // A page-level connection has no sessions: its events carry none.
        let page = CdpSession::page_level(7, BridgeRoute::Default);
        assert_eq!(page.event_session_id(7, None), None);
    }

    #[test]
    fn the_extension_event_shape_carries_the_child_session() {
        let child: CdpEventPayload = serde_json::from_value(json!({
            "tabId": 7, "sessionId": "IFRAME1", "method": "Page.loadEventFired", "params": {}
        }))
        .unwrap();
        assert_eq!(child.session_id.as_deref(), Some("IFRAME1"));
        let page: CdpEventPayload = serde_json::from_value(json!({
            "tabId": 7, "method": "Page.loadEventFired", "params": {}
        }))
        .unwrap();
        assert_eq!(page.session_id, None);
    }

    #[test]
    fn a_stopped_session_detaches_each_top_level_tab_once() {
        let mut sessions = HashMap::new();
        sessions.insert("s1".to_string(), 7);
        sessions.insert("s2".to_string(), 7); // re-attached to the same tab
        sessions.insert("s3".to_string(), 9);
        sessions.insert("iframe-1".to_string(), 7); // auto-attach child
        let mut iframes = HashSet::new();
        iframes.insert("iframe-1".to_string());
        assert_eq!(tabs_to_detach(&sessions, &iframes), vec![7, 9]);
        assert!(tabs_to_detach(&HashMap::new(), &HashSet::new()).is_empty());
    }

    /// Generate a unique thread id per test so the process-wide
    /// `THREAD_OWNED_TABS` registry doesn't leak state across parallel
    /// test runs.
    fn fresh_thread_id(label: &str) -> String {
        format!("test-{label}-{}", Uuid::new_v4())
    }

    /// A thread's dedicated window is remembered for the life of the
    /// process, but the window itself is closed by the run that used it
    /// (`agent-browser close`) or by the user. A shared thread id
    /// (`magician-chat-<thread>`) then came back to a cached id whose window
    /// was gone, and every `Target.createTarget` answered "no tab". The
    /// decision that tells a live window from a gone one:
    #[test]
    fn a_cached_window_is_live_only_while_the_extension_still_lists_a_tab_in_it() {
        let in_window = json!({"tabs": [
            {"tabId": 7, "windowId": 1328999119, "url": "about:blank"},
            {"tabId": 8, "windowId": 5, "url": "https://example.invalid"},
        ]});
        assert_eq!(
            window_liveness_from_list_tabs(Some(&in_window), 1328999119),
            WindowLiveness::Live
        );
        // The extension answered and no tab lives in that window: Chrome has
        // no empty windows, so it is gone.
        assert_eq!(
            window_liveness_from_list_tabs(Some(&in_window), 42),
            WindowLiveness::Gone
        );
        assert_eq!(
            window_liveness_from_list_tabs(Some(&json!({"tabs": []})), 42),
            WindowLiveness::Gone
        );
        // No answer to judge by keeps the cached id: the bridge is the
        // problem, and recreating would not fix it.
        assert_eq!(
            window_liveness_from_list_tabs(None, 42),
            WindowLiveness::Unknown
        );
        // A window whose only tab another debugger holds still exists.
        let held = json!({"tabs": [
            {"tabId": 9, "windowId": 42, "attachable": false},
        ]});
        assert_eq!(
            window_liveness_from_list_tabs(Some(&held), 42),
            WindowLiveness::Live
        );
    }

    #[test]
    fn forgetting_a_gone_window_drops_only_that_threads_mapping() {
        let thread = fresh_thread_id("gone-window");
        let other = fresh_thread_id("other-window");
        THREAD_WINDOWS.lock().unwrap().insert(thread.clone(), 111);
        THREAD_WINDOWS.lock().unwrap().insert(other.clone(), 222);
        seed_owned_tabs(&thread, [1, 2]);
        forget_thread_window(&thread, 111);
        assert!(THREAD_WINDOWS.lock().unwrap().get(&thread).is_none());
        assert_eq!(THREAD_WINDOWS.lock().unwrap().get(&other), Some(&222));
        assert!(
            owned_tabs(&thread).is_empty(),
            "tabs of a gone window are gone too"
        );
        clear_thread_state(&thread);
        clear_thread_state(&other);
    }

    #[test]
    fn seed_owned_tabs_from_targets_inserts_all_tab_targetids() {
        let thread = fresh_thread_id("seed");
        let targets = vec![
            json!({"targetId": "tab-101", "type": "page"}),
            json!({"targetId": "tab-202", "type": "page"}),
            json!({"targetId": "browser-target", "type": "browser"}),
            json!({"targetId": "frame-abc", "type": "iframe"}),
        ];

        seed_owned_tabs_from_targets(&thread, &targets);

        let mut owned = owned_tabs(&thread);
        owned.sort();
        assert_eq!(owned, vec![101, 202]);
    }

    #[actix_web::test]
    async fn fetch_bound_targets_falls_back_to_bootstrap_cache_once() {
        let thread = fresh_thread_id("bound-cache");
        let target = target_info_from_fields(
            42,
            Some("https://www.linkedin.com/feed/"),
            Some("LinkedIn"),
            Some(9),
        );

        set_bound_tab(&thread, 42, Some(target.clone()));

        let targets = fetch_bound_targets_for_thread(&thread, 42).await;

        assert_eq!(targets, vec![target]);
        assert!(take_cached_thread_targets(&thread).is_empty());
        clear_thread_state(&thread);
    }

    #[test]
    fn automation_cdp_command_mirror_records_redacted_page_signal() {
        let thread = fresh_thread_id("cmd-mirror");
        let _ = page_signals::disable_thread(&thread);

        record_automation_cdp_command(
            Some(&thread),
            42,
            Some("session-1"),
            "Page.navigate",
            &json!({
                "url": "https://example.com/path?token=secret&q=ok#frag"
            }),
            None,
        );

        let signals = page_signals::drain(&thread);
        assert_eq!(signals.len(), 1);
        let signal = &signals[0];
        assert_eq!(signal.thread_id, thread);
        assert_eq!(signal.tab_id, 42);
        assert_eq!(signal.session_id.as_deref(), Some("session-1"));
        assert_eq!(signal.event_kind, "automation_cdp_command");
        assert_eq!(
            signal.capture_source.as_deref(),
            Some("automation_cdp_mirror")
        );
        assert_eq!(
            signal.url.as_deref(),
            Some("https://example.com/path?token=[REDACTED]&q=ok")
        );
        assert_eq!(signal.origin.as_deref(), Some("https://example.com"));
        assert_eq!(
            signal.capture.as_ref().and_then(|c| c.method.as_deref()),
            Some("Page.navigate")
        );
    }

    #[test]
    fn automation_cdp_event_mirror_records_only_lifecycle_events() {
        let thread = fresh_thread_id("event-mirror");
        let _ = page_signals::disable_thread(&thread);

        record_automation_cdp_event(
            &thread,
            7,
            Some("session-2"),
            "Runtime.consoleAPICalled",
            &json!({}),
        );
        assert!(page_signals::drain(&thread).is_empty());

        record_automation_cdp_event(
            &thread,
            7,
            Some("session-2"),
            "Page.frameNavigated",
            &json!({
                "frame": {
                    "url": "https://example.org/next?session_id=abc"
                }
            }),
        );

        let signals = page_signals::drain(&thread);
        assert_eq!(signals.len(), 1);
        let signal = &signals[0];
        assert_eq!(signal.event_kind, "automation_cdp_event");
        assert_eq!(
            signal.url.as_deref(),
            Some("https://example.org/next?session_id=[REDACTED]")
        );
        assert_eq!(
            signal.capture.as_ref().and_then(|c| c.method.as_deref()),
            Some("Page.frameNavigated")
        );
    }

    #[test]
    fn create_window_targets_are_cached_with_window_id() {
        let result = json!({
            "windowId": 77,
            "tabs": [
                { "tabId": 88, "url": "about:blank", "title": "" }
            ]
        });

        let targets = target_infos_from_create_window_response(Some(&result), 77);

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].get("targetId"), Some(&json!("tab-88")));
        assert_eq!(targets[0].get("windowId"), Some(&json!(77)));
        assert_eq!(targets[0].get("attached"), Some(&json!(false)));
    }

    #[test]
    fn seed_owned_tabs_is_idempotent() {
        let thread = fresh_thread_id("idem");
        seed_owned_tabs(&thread, [100]);
        seed_owned_tabs(&thread, [100, 200]);
        seed_owned_tabs(&thread, [200, 300]);

        let mut owned = owned_tabs(&thread);
        owned.sort();
        assert_eq!(owned, vec![100, 200, 300]);
    }

    #[test]
    fn claim_owned_tab_if_chained_claims_when_opener_is_owned() {
        let thread = fresh_thread_id("claim");
        seed_owned_tabs(&thread, [500]);

        let claimed = claim_owned_tab_if_chained(&thread, 600, 500);

        assert!(claimed);
        let mut owned = owned_tabs(&thread);
        owned.sort();
        assert_eq!(owned, vec![500, 600]);
    }

    #[test]
    fn claim_owned_tab_if_chained_ignores_unrelated_opener() {
        let thread = fresh_thread_id("ignore");
        seed_owned_tabs(&thread, [700]);

        // 999 is not in the owned set — popup is Tier-3 and must stay
        // invisible.
        let claimed = claim_owned_tab_if_chained(&thread, 800, 999);

        assert!(!claimed);
        let owned = owned_tabs(&thread);
        assert_eq!(owned, vec![700]);
    }

    #[test]
    fn drop_owned_tab_removes_entry_and_is_idempotent() {
        let thread = fresh_thread_id("drop");
        seed_owned_tabs(&thread, [10, 20, 30]);

        drop_owned_tab(&thread, 20);
        let mut owned = owned_tabs(&thread);
        owned.sort();
        assert_eq!(owned, vec![10, 30]);

        // Dropping again is a no-op, not a panic.
        drop_owned_tab(&thread, 20);
        drop_owned_tab(&thread, 999);
        let mut owned = owned_tabs(&thread);
        owned.sort();
        assert_eq!(owned, vec![10, 30]);
    }

    #[test]
    fn owned_tabs_isolates_threads() {
        let thread_a = fresh_thread_id("a");
        let thread_b = fresh_thread_id("b");
        seed_owned_tabs(&thread_a, [1, 2, 3]);
        seed_owned_tabs(&thread_b, [10, 20, 30]);

        // Tier-2 attempt on thread_b with thread_a's tab as opener: must
        // not claim, since thread_b doesn't own that tab.
        assert!(!claim_owned_tab_if_chained(&thread_b, 99, 1));

        let mut owned_a = owned_tabs(&thread_a);
        let mut owned_b = owned_tabs(&thread_b);
        owned_a.sort();
        owned_b.sort();
        assert_eq!(owned_a, vec![1, 2, 3]);
        assert_eq!(owned_b, vec![10, 20, 30]);
    }

    #[test]
    fn update_owned_tabs_from_event_claims_page_popup_with_owned_opener() {
        let thread = fresh_thread_id("evt-claim");
        seed_owned_tabs(&thread, [50]);

        update_owned_tabs_from_event(
            &thread,
            "Target.attachedToTarget",
            &json!({
                "sessionId": "abc",
                "targetInfo": {
                    "targetId": "tab-60",
                    "type": "page",
                    "openerId": "tab-50",
                },
                "waitingForDebugger": false,
            }),
        );

        let mut owned = owned_tabs(&thread);
        owned.sort();
        assert_eq!(owned, vec![50, 60]);
    }

    #[test]
    fn update_owned_tabs_from_event_skips_iframe_targets() {
        let thread = fresh_thread_id("evt-iframe");
        seed_owned_tabs(&thread, [70]);

        // iframes get their own routing path; the page-tab owner-set
        // must not pick them up even when the opener is one of ours.
        update_owned_tabs_from_event(
            &thread,
            "Target.attachedToTarget",
            &json!({
                "sessionId": "iframe-session",
                "targetInfo": {
                    "targetId": "tab-71",
                    "type": "iframe",
                    "openerId": "tab-70",
                },
            }),
        );

        let owned = owned_tabs(&thread);
        assert_eq!(owned, vec![70]);
    }

    #[test]
    fn update_owned_tabs_from_event_skips_noopener_popups() {
        let thread = fresh_thread_id("evt-noopener");
        seed_owned_tabs(&thread, [80]);

        // No `openerId` on the targetInfo means rel="noopener" /
        // window.open(noopener) — Tier 3, untracked.
        update_owned_tabs_from_event(
            &thread,
            "Target.attachedToTarget",
            &json!({
                "sessionId": "abc",
                "targetInfo": {
                    "targetId": "tab-81",
                    "type": "page",
                },
            }),
        );

        let owned = owned_tabs(&thread);
        assert_eq!(owned, vec![80]);
    }

    #[test]
    fn update_owned_tabs_from_event_keeps_detached_tabs_until_destroyed() {
        let thread = fresh_thread_id("evt-drop");
        seed_owned_tabs(&thread, [90, 91, 92]);

        update_owned_tabs_from_event(
            &thread,
            "Target.detachedFromTarget",
            &json!({"targetId": "tab-91"}),
        );
        update_owned_tabs_from_event(
            &thread,
            "Target.targetDestroyed",
            &json!({"targetId": "tab-92"}),
        );

        let mut owned = owned_tabs(&thread);
        owned.sort();
        assert_eq!(owned, vec![90, 91]);
    }

    #[test]
    fn check_tab_ownership_allows_owned_tab() {
        let thread = fresh_thread_id("guard-allow");
        seed_owned_tabs(&thread, [1000, 1001]);

        assert_eq!(
            check_tab_ownership(&thread, 1000, "attach"),
            OwnedTabCheck::Allowed
        );
        assert_eq!(
            check_tab_ownership(&thread, 1001, "close"),
            OwnedTabCheck::Allowed
        );
    }

    #[test]
    fn check_tab_ownership_rejects_unowned_tab_with_op_label() {
        let thread = fresh_thread_id("guard-reject");
        seed_owned_tabs(&thread, [2000]);

        let outcome = check_tab_ownership(&thread, 9999, "attach");
        match outcome {
            OwnedTabCheck::Rejected { reason } => {
                assert!(reason.contains("9999"));
                assert!(reason.contains("attach"));
                assert!(reason.contains("not owned by this execution"));
            },
            OwnedTabCheck::Allowed => panic!("expected Rejected"),
        }

        let outcome = check_tab_ownership(&thread, 9999, "close");
        match outcome {
            OwnedTabCheck::Rejected { reason } => assert!(reason.contains("close")),
            OwnedTabCheck::Allowed => panic!("expected Rejected"),
        }
    }

    #[test]
    fn check_tab_ownership_rejects_when_owned_set_empty_pre_bootstrap() {
        // A guessed tab ID before discovery must not bypass ownership.
        let thread = fresh_thread_id("guard-empty");
        assert!(matches!(
            check_tab_ownership(&thread, 1234, "attach"),
            OwnedTabCheck::Rejected { .. }
        ));
    }

    #[test]
    fn check_tab_ownership_isolates_owned_sets_across_threads() {
        let thread_a = fresh_thread_id("guard-iso-a");
        let thread_b = fresh_thread_id("guard-iso-b");
        seed_owned_tabs(&thread_a, [555]);
        seed_owned_tabs(&thread_b, [777]);

        // tab 555 is owned by A but not by B — B's guard rejects.
        assert!(matches!(
            check_tab_ownership(&thread_b, 555, "attach"),
            OwnedTabCheck::Rejected { .. }
        ));
        // tab 777 is owned by B but not by A — A's guard rejects.
        assert!(matches!(
            check_tab_ownership(&thread_a, 777, "close"),
            OwnedTabCheck::Rejected { .. }
        ));
        // Each thread accepts its own tab.
        assert_eq!(
            check_tab_ownership(&thread_a, 555, "attach"),
            OwnedTabCheck::Allowed
        );
        assert_eq!(
            check_tab_ownership(&thread_b, 777, "close"),
            OwnedTabCheck::Allowed
        );
    }

    #[test]
    fn target_id_to_tab_id_handles_expected_shapes() {
        assert_eq!(target_id_to_tab_id(Some(&json!("tab-42"))), Some(42));
        assert_eq!(target_id_to_tab_id(Some(&json!("tab-0"))), Some(0));
        assert_eq!(target_id_to_tab_id(Some(&json!("frame-abc"))), None);
        assert_eq!(target_id_to_tab_id(Some(&json!("tab-"))), None);
        assert_eq!(target_id_to_tab_id(Some(&json!(42))), None);
        assert_eq!(target_id_to_tab_id(None), None);
    }

    async fn gated_app(
        config: crate::MagicutorConfig,
    ) -> impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
    > {
        actix_web::test::init_service(
            actix_web::App::new()
                .app_data(web::Data::new(config))
                .route("/devtools/browser/{id}", web::get().to(cdp_browser_ws))
                .route("/devtools/page/{tab_id}", web::get().to(cdp_page_ws))
                .route("/json/list", web::get().to(cdp_list))
                .route(
                    "/cdp/threads/{thread_id}/browser",
                    web::put().to(cdp_pin_thread_browser),
                )
                .route(
                    "/cdp/threads/{thread_id}/browser",
                    web::get().to(cdp_thread_browser_info),
                )
                .route(
                    "/cdp/threads/{thread_id}/choose",
                    web::post().to(cdp_start_browser_choice),
                )
                .route(
                    "/cdp/threads/{thread_id}",
                    web::delete().to(cdp_clear_thread),
                ),
        )
        .await
    }

    fn status_of(response: actix_web::dev::ServiceResponse) -> actix_web::http::StatusCode {
        response.status()
    }

    #[actix_web::test]
    async fn devtools_and_targets_refuse_untrusted_peers_and_websites() {
        use actix_web::http::StatusCode;
        let mut open = crate::MagicutorConfig::default();
        open.bridge.auth_token.clear();
        let app = gated_app(open).await;
        let thread = fresh_thread_id("gate");
        for uri in [
            format!("/devtools/browser/{thread}"),
            "/devtools/page/7".to_string(),
            "/json/list".to_string(),
        ] {
            // A LAN machine.
            let request = actix_web::test::TestRequest::get()
                .uri(&uri)
                .peer_addr("192.168.1.20:5000".parse().unwrap())
                .to_request();
            assert_eq!(
                status_of(actix_web::test::call_service(&app, request).await),
                StatusCode::FORBIDDEN,
                "{uri} from the LAN"
            );
            // A website, through the owner's own browser on loopback.
            let request = actix_web::test::TestRequest::get()
                .uri(&uri)
                .peer_addr("127.0.0.1:5000".parse().unwrap())
                .insert_header(("origin", "https://evil.example"))
                .to_request();
            assert_eq!(
                status_of(actix_web::test::call_service(&app, request).await),
                StatusCode::FORBIDDEN,
                "{uri} from a website"
            );
            // A DNS-rebinding page: same-origin (no Origin), loopback peer,
            // but its own host name.
            let request = actix_web::test::TestRequest::get()
                .uri(&uri)
                .peer_addr("127.0.0.1:5000".parse().unwrap())
                .insert_header(("host", "rebind.evil.example:3003"))
                .to_request();
            assert_eq!(
                status_of(actix_web::test::call_service(&app, request).await),
                StatusCode::FORBIDDEN,
                "{uri} through DNS rebinding"
            );
            // agent-browser on this machine sends no Origin: past the gate
            // (the test request is not a WebSocket upgrade, so the upgrade
            // itself then fails with a client error, never 403).
            let request = actix_web::test::TestRequest::get()
                .uri(&uri)
                .peer_addr("127.0.0.1:5000".parse().unwrap())
                .to_request();
            assert_ne!(
                status_of(actix_web::test::call_service(&app, request).await),
                StatusCode::FORBIDDEN,
                "{uri} from agent-browser"
            );
        }
    }

    #[actix_web::test]
    async fn thread_routes_check_the_configured_token() {
        use actix_web::http::StatusCode;
        let mut tokened = crate::MagicutorConfig::default();
        tokened.bridge.auth_token = "owner-secret".to_string();
        let app = gated_app(tokened).await;
        let thread = fresh_thread_id("token");
        let remote = |method: actix_web::test::TestRequest, uri: &str, token: Option<&str>| {
            let mut request = method
                .uri(uri)
                .peer_addr("192.168.1.20:5000".parse().unwrap());
            if let Some(token) = token {
                request = request.insert_header(("authorization", format!("Bearer {token}")));
            }
            request.to_request()
        };
        let routes = [
            (
                actix_web::test::TestRequest::get(),
                format!("/cdp/threads/{thread}/browser"),
            ),
            (
                actix_web::test::TestRequest::put().set_json(json!({ "browserId": "default" })),
                format!("/cdp/threads/{thread}/browser"),
            ),
            (
                actix_web::test::TestRequest::post().set_json(json!({ "task": "x" })),
                format!("/cdp/threads/{thread}/choose"),
            ),
            (
                actix_web::test::TestRequest::delete(),
                format!("/cdp/threads/{thread}"),
            ),
        ];
        for (method, uri) in routes {
            let request = remote(method, &uri, Some("guess"));
            assert_eq!(
                status_of(actix_web::test::call_service(&app, request).await),
                StatusCode::FORBIDDEN,
                "{uri} with a wrong token"
            );
        }
        let request = remote(
            actix_web::test::TestRequest::get(),
            &format!("/cdp/threads/{thread}/browser"),
            Some("owner-secret"),
        );
        assert_eq!(
            status_of(actix_web::test::call_service(&app, request).await),
            StatusCode::OK
        );
    }

    #[test]
    fn only_choose_ask_asks_and_its_parameters_are_bounded() {
        // Unattended runs send no `choose`: nothing is ever asked.
        assert_eq!(browser_choice_from_query("", 20), None);
        assert_eq!(browser_choice_from_query("browser=comet", 20), None);
        assert_eq!(browser_choice_from_query("choose=never", 20), None);
        let (timeout, task) =
            browser_choice_from_query("choose=ask&task=Book%20a%20table", 20).unwrap();
        assert_eq!(timeout, std::time::Duration::from_secs(20));
        assert_eq!(task, "Book a table");
        let (timeout, task) = browser_choice_from_query("choose=ask&timeout=9999", 20).unwrap();
        assert_eq!(timeout.as_secs(), MAX_BROWSER_CHOICE_TIMEOUT_SECS);
        assert_eq!(task, "this task");
        let (timeout, _) = browser_choice_from_query("choose=ask&timeout=0", 20).unwrap();
        assert_eq!(timeout.as_secs(), 1);
        let long = "x".repeat(500);
        let (_, task) = browser_choice_from_query(&format!("choose=ask&task={long}"), 20).unwrap();
        assert_eq!(task.chars().count(), MAX_CHOICE_TASK_CHARS);
    }

    #[test]
    fn the_browser_selector_comes_from_the_proxy_url_query() {
        assert_eq!(
            browser_selector_from_query("browser=comet"),
            Some("comet".to_string())
        );
        assert_eq!(
            browser_selector_from_query("x=1&browser=Chrome%20Work"),
            Some("Chrome Work".to_string())
        );
        assert_eq!(browser_selector_from_query("browser=%20"), None);
        assert_eq!(browser_selector_from_query(""), None);
    }

    #[test]
    fn browser_selection_errors_map_to_distinct_http_statuses() {
        use actix_web::http::StatusCode;
        let status = |error: BrowserSelectError| browser_select_error_response(&error).status();
        assert_eq!(
            status(BrowserSelectError::NoBrowsers),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            status(BrowserSelectError::NotFound {
                selector: "dia".to_string(),
                connected: vec!["Chrome [a]".to_string()],
            }),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(BrowserSelectError::Ambiguous {
                selector: "chrome".to_string(),
                matches: vec!["Chrome [a]".to_string(), "Chrome [b]".to_string()],
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(BrowserSelectError::PinnedElsewhere {
                thread_id: "t".to_string(),
                pinned: "Comet [c]".to_string(),
            }),
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn thread_state_from_another_browser_is_dropped_never_reused() {
        let thread = fresh_thread_id("state-owner");
        assert!(!note_thread_state_browser(&thread, "chrome"));
        seed_owned_tabs(&thread, [7, 8]);
        if let Ok(mut windows) = THREAD_WINDOWS.lock() {
            windows.insert(thread.clone(), 41);
        }
        // Same browser again (e.g. it reconnected): the state is kept.
        assert!(!note_thread_state_browser(&thread, "chrome"));
        assert_eq!(owned_tabs(&thread).len(), 2);

        // The thread now reaches another browser (moved pin, or a legacy
        // default replaced by a new connection): its window id and tab ids
        // are dropped rather than looked up in the wrong browser.
        assert!(note_thread_state_browser(&thread, "legacy-browser-9"));
        assert!(owned_tabs(&thread).is_empty());
        assert!(THREAD_WINDOWS
            .lock()
            .map(|windows| !windows.contains_key(&thread))
            .unwrap_or(false));
        assert!(!note_thread_state_browser(&thread, "legacy-browser-9"));
        clear_thread_state(&thread);
    }

    #[actix_web::test]
    async fn cleanup_never_routes_through_an_unpinned_thread() {
        // A thread with no pin has nothing to clean up: a thread route here
        // would pin it to the default browser and detach that browser's tab.
        let thread = fresh_thread_id("cleanup");
        assert_eq!(
            cleanup_route(&BridgeRoute::Thread(thread.clone())).await,
            None
        );
        assert_eq!(bridge::thread_browser(&thread).await, None);
        assert_eq!(
            cleanup_route(&BridgeRoute::Browser("chrome".to_string())).await,
            Some(BridgeRoute::Browser("chrome".to_string()))
        );
    }

    #[test]
    fn equal_tab_ids_from_two_browsers_stay_in_their_own_threads() {
        // Chrome and Comet can both have tab 7; each thread runs in one
        // browser, so ownership is per thread and never shared.
        let chrome_thread = fresh_thread_id("chrome-tab7");
        let comet_thread = fresh_thread_id("comet-tab7");
        seed_owned_tabs(&chrome_thread, [7]);
        seed_owned_tabs(&comet_thread, [7, 8]);
        drop_owned_tab(&chrome_thread, 7);
        assert!(owned_tabs(&chrome_thread).is_empty());
        let mut comet = owned_tabs(&comet_thread);
        comet.sort();
        assert_eq!(comet, vec![7, 8]);
        // An opener in one thread cannot claim a tab for the other.
        assert!(!claim_owned_tab_if_chained(&chrome_thread, 9, 8));
        clear_thread_state(&chrome_thread);
        clear_thread_state(&comet_thread);
    }
}

//! Network trace capture for the CDP-proxy route.
//!
//! Subscribes to `Network.*` CDP events that the extension already forwards to
//! the bridge (debugger attach calls `Network.enable` for every tab), assembles
//! `NetworkTraceEvent`s in the same shape the magician API-mining pipeline
//! consumes, and buffers them per magician thread.
//!
//! Magician drains the per-thread buffer through `GET /trace/drain/{thread_id}`
//! and tags the events with `capture_source = "cdp_proxy"` before pushing them
//! into its `TraceManager`.
//!
//! This path runs entirely on the magicutor side; it does not depend on an
//! extension-local trace buffer.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::bridge_protocol::ExtensionRequest;
use crate::server::bridge::{
    send_over_bridge, subscribe_cdp_events, CdpEventPayload, CdpSubscriberHandle,
};

/// Resource types we capture for API mining.
const CAPTURED_RESOURCE_TYPES: &[&str] = &["XHR", "Fetch", "Document", "Other"];

/// Per-thread cap on completed traces. Older entries are evicted when full.
const MAX_TRACES_PER_THREAD: usize = 200;

/// Per-thread cap for transient auth material. These values are drained by
/// Magician into its encrypted captured-secret store and are never included in
/// the durable network trace payload.
const MAX_AUTH_EVENTS_PER_THREAD: usize = 200;

/// Cap on captured response body size before truncation.
const MAX_RESPONSE_BODY_BYTES: usize = 256 * 1024;

/// Cap on captured request body size before truncation.
const MAX_REQUEST_BODY_BYTES: usize = 100 * 1024;

/// Sensitive request/response header names whose values are replaced with
/// `[REDACTED]` before persisting. Mirrors the extension's redaction set.
const SENSITIVE_REQUEST_HEADER_NAMES: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "x-api-key",
    "x-auth-token",
    "x-csrf-token",
    "x-xsrf-token",
    "x-access-token",
    "x-session-token",
    "x-framework-xsrf-token",
    "x-google-btd",
    "x-gmail-btai",
    "x-xsrf-asfe-token",
    "x-amz-security-token",
    "api-key",
    "api_key",
    "bearer",
    "token",
    "secret",
];

const SENSITIVE_RESPONSE_HEADER_NAMES: &[&str] = &[
    "set-cookie",
    "x-csrf-token",
    "x-xsrf-token",
    "www-authenticate",
];

/// Network trace event surfaced to magician. Field names match
/// `magician::api_mining::types::NetworkTraceEvent` so the consumer can
/// deserialize directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkTraceEvent {
    pub request_id: String,
    pub method: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<String>,
    /// Chrome tab the request was issued from, and the magician thread the
    /// capture belongs to. Both are already known at capture time and are the
    /// join keys page signals carry (`tabId` / `threadId`). Without them a
    /// trace can only be matched to a page signal by task-directory plus
    /// timestamp proximity, which is guesswork — and the DOM-effect signal
    /// ("did this request change what the user sees?") depends entirely on
    /// that join being exact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub request_headers: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body: Option<String>,
    pub response_headers: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_body: Option<String>,
    /// Why `response_body` is absent on an otherwise-completed request.
    /// Distinguishes the four separate causes that used to collapse into a
    /// bare `None` (bridge error, no body in the CDP result, binary content,
    /// CDP refusal such as an evicted buffer) so the empty-body rate can be
    /// attributed instead of guessed at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_unavailable_reason: Option<String>,
    /// `Network.loadingFailed` detail. Requests that never completed are
    /// recorded with `status = 0`; without these fields there is no way to
    /// tell an ad-blocked tracker from a navigation-cancelled fetch from a
    /// real network error, which is the difference between "drop as noise"
    /// and "investigate".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_error_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_blocked_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_canceled: Option<bool>,
    pub status: u16,
    pub timing: RequestTiming,
    pub initiator: RequestInitiator,
    pub timestamp: i64,
    pub request_size: u64,
    pub response_size: u64,
    /// Always `"cdp_proxy"` for events captured by this module. Lets the
    /// consumer distinguish from headed/headless captures (planned).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_source: Option<String>,
}

/// Unredacted authentication material observed on one CDP request.
///
/// This type is served only by the one-shot `/auth/drain/{thread_id}` endpoint.
/// It must never be logged or folded into [`NetworkTraceEvent`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturedAuthEvent {
    pub request_id: String,
    pub url: String,
    pub auth_headers: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie_header: Option<String>,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestTiming {
    pub request_time: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns_duration: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_duration: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssl_duration: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttfb: Option<f64>,
    pub total_duration: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestInitiator {
    pub initiator_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<Vec<StackFrame>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackFrame {
    pub function_name: String,
    pub script_id: String,
    pub url: String,
    pub line_number: u32,
    pub column_number: u32,
}

/// Half-built trace waiting for `Network.responseReceived` /
/// `Network.loadingFinished` to complete it.
#[derive(Debug, Clone)]
struct PendingTrace {
    event: NetworkTraceEvent,
    timestamp_ms: i64,
}

/// Per-tab capture state held inside [`ThreadCapture`].
struct TabState {
    pending: HashMap<String, PendingTrace>,
    /// Keeps the bridge subscription alive for as long as the thread is
    /// being captured. Dropped when the thread is removed from the registry.
    _subscriber: CdpSubscriberHandle,
}

/// Per-thread capture state.
struct ThreadCapture {
    thread_id: String,
    tabs: Mutex<HashMap<i32, TabState>>,
    /// Completed traces ready to drain. Bounded by `MAX_TRACES_PER_THREAD`;
    /// oldest entries are evicted when full.
    completed: Mutex<Vec<NetworkTraceEvent>>,
    /// Unredacted auth material waiting for a one-shot drain into Magician's
    /// encrypted secret store. This buffer is process-memory only.
    captured_auth: Mutex<Vec<CapturedAuthEvent>>,
}

impl ThreadCapture {
    fn new(thread_id: String) -> Self {
        Self {
            thread_id,
            tabs: Mutex::new(HashMap::new()),
            completed: Mutex::new(Vec::new()),
            captured_auth: Mutex::new(Vec::new()),
        }
    }

    /// Push a completed trace, evicting the oldest entry once the buffer is
    /// at capacity. Capacity matches the extension default.
    fn push_completed(&self, mut event: NetworkTraceEvent) {
        sanitize_durable_trace(&mut event);
        event.capture_source = Some("cdp_proxy".to_string());
        let mut completed = match self.completed.lock() {
            Ok(g) => g,
            Err(e) => {
                warn!(thread = %self.thread_id, error = %e, "trace_capture: completed buffer poisoned");
                return;
            },
        };
        if completed.len() >= MAX_TRACES_PER_THREAD {
            completed.remove(0);
        }
        completed.push(event);
    }

    fn drain(&self) -> Vec<NetworkTraceEvent> {
        match self.completed.lock() {
            Ok(mut g) => std::mem::take(&mut *g),
            Err(e) => {
                warn!(thread = %self.thread_id, error = %e, "trace_capture: drain failed");
                Vec::new()
            },
        }
    }

    fn push_captured_auth(&self, event: CapturedAuthEvent) {
        let mut captured = match self.captured_auth.lock() {
            Ok(guard) => guard,
            Err(error) => {
                warn!(
                    thread = %self.thread_id,
                    error = %error,
                    "trace_capture: captured-auth buffer poisoned"
                );
                return;
            },
        };
        if captured.len() >= MAX_AUTH_EVENTS_PER_THREAD {
            captured.remove(0);
        }
        captured.push(event);
    }

    fn drain_captured_auth(&self) -> Vec<CapturedAuthEvent> {
        match self.captured_auth.lock() {
            Ok(mut guard) => std::mem::take(&mut *guard),
            Err(error) => {
                warn!(
                    thread = %self.thread_id,
                    error = %error,
                    "trace_capture: captured-auth drain failed"
                );
                Vec::new()
            },
        }
    }
}

/// Process-wide registry of per-thread captures. Singletons are created
/// lazily on first `enable_for_thread_tab` call for each thread; entries
/// are removed via `disable_thread`.
struct CaptureRegistry {
    threads: Mutex<HashMap<String, Arc<ThreadCapture>>>,
}

impl CaptureRegistry {
    fn new() -> Self {
        Self {
            threads: Mutex::new(HashMap::new()),
        }
    }

    fn get_or_create(&self, thread_id: &str) -> Arc<ThreadCapture> {
        let mut threads = match self.threads.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        threads
            .entry(thread_id.to_string())
            .or_insert_with(|| Arc::new(ThreadCapture::new(thread_id.to_string())))
            .clone()
    }

    fn get(&self, thread_id: &str) -> Option<Arc<ThreadCapture>> {
        let threads = match self.threads.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        threads.get(thread_id).cloned()
    }

    fn remove(&self, thread_id: &str) -> Option<Arc<ThreadCapture>> {
        let mut threads = match self.threads.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        threads.remove(thread_id)
    }
}

static REGISTRY: Lazy<CaptureRegistry> = Lazy::new(CaptureRegistry::new);

/// Begin capturing network traces for `tab_id` under `thread_id`. Idempotent:
/// repeat calls with the same `(thread_id, tab_id)` are no-ops.
///
/// Once the backend bootstrap or CDP client enables the `Network` domain,
/// events flow through the bridge automatically. This function only registers a
/// parallel subscriber that builds `NetworkTraceEvent`s from those events and
/// stages them into the per-thread completed buffer.
pub fn enable_for_thread_tab(thread_id: &str, tab_id: i32) {
    let thread = REGISTRY.get_or_create(thread_id);
    {
        let tabs = match thread.tabs.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if tabs.contains_key(&tab_id) {
            return;
        }
    }

    let thread_for_cb = Arc::clone(&thread);
    let subscriber = subscribe_cdp_events(tab_id, move |payload| {
        handle_event(&thread_for_cb, tab_id, payload);
        true
    });

    let mut tabs = match thread.tabs.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    tabs.entry(tab_id).or_insert(TabState {
        pending: HashMap::new(),
        _subscriber: subscriber,
    });
    debug!(
        thread = thread_id,
        tab = tab_id,
        "trace_capture: tab enabled"
    );
}

/// Drain all completed traces for `thread_id`. Returns an empty vec if no
/// thread state exists.
pub fn drain(thread_id: &str) -> Vec<NetworkTraceEvent> {
    REGISTRY
        .get(thread_id)
        .map(|t| t.drain())
        .unwrap_or_default()
}

/// Drain transient, unredacted auth material for `thread_id`.
pub fn drain_captured_auth(thread_id: &str) -> Vec<CapturedAuthEvent> {
    REGISTRY
        .get(thread_id)
        .map(|thread| thread.drain_captured_auth())
        .unwrap_or_default()
}

/// Tear down capture state for `thread_id`. Drops all subscriber handles and
/// discards in-flight pending traces. Completed traces are returned so the
/// caller has one last chance to persist them.
pub fn disable_thread(thread_id: &str) -> Vec<NetworkTraceEvent> {
    REGISTRY
        .remove(thread_id)
        .map(|t| t.drain())
        .unwrap_or_default()
}

fn handle_event(thread: &Arc<ThreadCapture>, tab_id: i32, payload: CdpEventPayload) {
    if payload.tab_id != tab_id {
        return;
    }
    match payload.method.as_str() {
        "Network.requestWillBeSent" => handle_request_will_be_sent(thread, tab_id, payload.params),
        "Network.responseReceived" => handle_response_received(thread, tab_id, &payload.params),
        "Network.loadingFinished" => handle_loading_finished(thread, tab_id, payload.params),
        "Network.loadingFailed" => handle_loading_failed(thread, tab_id, &payload.params),
        _ => {},
    }
}

fn handle_request_will_be_sent(thread: &Arc<ThreadCapture>, tab_id: i32, params: Value) {
    let resource_type = params
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("Other");
    if !CAPTURED_RESOURCE_TYPES.contains(&resource_type) {
        return;
    }
    let request_id = match params.get("requestId").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => return,
    };
    let request = match params.get("request") {
        Some(v) => v,
        None => return,
    };
    let url = request
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_string();
    let raw_headers = request.get("headers").cloned().unwrap_or(json!({}));
    if let Some(auth_event) = captured_auth_event(
        request_id.clone(),
        url.clone(),
        &raw_headers,
        current_time_ms(),
    ) {
        thread.push_captured_auth(auth_event);
    }
    let request_headers = redact_headers(raw_headers, SENSITIVE_REQUEST_HEADER_NAMES);
    let request_body = request.get("postData").and_then(Value::as_str).map(|body| {
        if body.len() > MAX_REQUEST_BODY_BYTES {
            let mut truncated = body[..MAX_REQUEST_BODY_BYTES].to_string();
            truncated.push_str("...[truncated]");
            truncated
        } else {
            body.to_string()
        }
    });
    let request_size = request_body.as_ref().map(|b| b.len() as u64).unwrap_or(0);

    let frame_id = params
        .get("frameId")
        .and_then(Value::as_str)
        .map(str::to_string);

    let initiator = parse_initiator(params.get("initiator"));
    let timestamp_ms = current_time_ms();
    let request_time = params
        .get("wallTime")
        .and_then(Value::as_f64)
        .unwrap_or((timestamp_ms as f64) / 1000.0);

    let event = NetworkTraceEvent {
        request_id: request_id.clone(),
        method,
        url: redact_auth_query(&url),
        resource_type: Some(resource_type.to_string()),
        frame_id,
        // Join keys — both already in scope here, and the same values page
        // signals carry, so the two streams line up exactly.
        tab_id: Some(i64::from(tab_id)),
        thread_id: Some(thread.thread_id.clone()),
        request_headers,
        request_body,
        response_headers: HashMap::new(),
        response_body: None,
        // Populated later: on `loadingFinished` if the body fetch fails, or on
        // `loadingFailed` from CDP's own failure detail.
        body_unavailable_reason: None,
        failure_error_text: None,
        failure_blocked_reason: None,
        failure_canceled: None,
        status: 0,
        timing: RequestTiming {
            request_time,
            dns_duration: None,
            connect_duration: None,
            ssl_duration: None,
            ttfb: None,
            total_duration: 0.0,
        },
        initiator,
        timestamp: timestamp_ms,
        request_size,
        response_size: 0,
        capture_source: None,
    };

    if let Some(state) = thread.tabs.lock().ok().and_then(|mut tabs| {
        tabs.get_mut(&tab_id).map(|s| {
            s.pending.insert(
                request_id.clone(),
                PendingTrace {
                    event,
                    timestamp_ms,
                },
            );
        })
    }) {
        let _ = state;
    }
}

fn handle_response_received(thread: &Arc<ThreadCapture>, tab_id: i32, params: &Value) {
    let request_id = match params.get("requestId").and_then(Value::as_str) {
        Some(s) => s,
        None => return,
    };
    let response = match params.get("response") {
        Some(v) => v,
        None => return,
    };

    let mut tabs = match thread.tabs.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let Some(state) = tabs.get_mut(&tab_id) else {
        return;
    };
    let Some(pending) = state.pending.get_mut(request_id) else {
        return;
    };

    pending.event.status = response.get("status").and_then(Value::as_u64).unwrap_or(0) as u16;
    pending.event.response_headers = redact_headers(
        response.get("headers").cloned().unwrap_or(json!({})),
        SENSITIVE_RESPONSE_HEADER_NAMES,
    );

    if let Some(timing) = response.get("timing") {
        let dns_start = timing.get("dnsStart").and_then(Value::as_f64);
        let dns_end = timing.get("dnsEnd").and_then(Value::as_f64);
        if let (Some(s), Some(e)) = (dns_start, dns_end) {
            if e > 0.0 && s >= 0.0 {
                pending.event.timing.dns_duration = Some(e - s);
            }
        }
        let connect_start = timing.get("connectStart").and_then(Value::as_f64);
        let connect_end = timing.get("connectEnd").and_then(Value::as_f64);
        if let (Some(s), Some(e)) = (connect_start, connect_end) {
            if e > 0.0 && s >= 0.0 {
                pending.event.timing.connect_duration = Some(e - s);
            }
        }
        let ssl_start = timing.get("sslStart").and_then(Value::as_f64);
        let ssl_end = timing.get("sslEnd").and_then(Value::as_f64);
        if let (Some(s), Some(e)) = (ssl_start, ssl_end) {
            if e > 0.0 && s >= 0.0 {
                pending.event.timing.ssl_duration = Some(e - s);
            }
        }
        if let Some(ttfb) = timing.get("receiveHeadersEnd").and_then(Value::as_f64) {
            if ttfb > 0.0 {
                pending.event.timing.ttfb = Some(ttfb);
            }
        }
    }
}

fn handle_loading_finished(thread: &Arc<ThreadCapture>, tab_id: i32, params: Value) {
    let request_id = match params.get("requestId").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => return,
    };

    // Pop the pending trace and finalize timing now; body fetch happens
    // asynchronously via the bridge, which can take several ms and must
    // not block the event hot path.
    let mut pending = {
        let mut tabs = match thread.tabs.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some(state) = tabs.get_mut(&tab_id) else {
            return;
        };
        match state.pending.remove(&request_id) {
            Some(p) => p,
            None => return,
        }
    };
    pending.event.timing.total_duration = (current_time_ms() - pending.timestamp_ms) as f64;

    // Fast path: the extension already fetched the body at the moment
    // `loadingFinished` fired, while Chrome still had it buffered, and attached
    // it here. Taking it avoids the two WebSocket round-trips that the fallback
    // below needs — the window in which Chrome evicts the buffer.
    if let Some(body) = params
        .get("magicutorResponseBody")
        .and_then(Value::as_str)
        .filter(|body| !body.is_empty())
    {
        pending.event.response_size = body.len() as u64;
        pending.event.response_body = Some(body.to_string());
        thread.push_completed(pending.event);
        return;
    }

    let thread_for_body = Arc::clone(thread);
    let request_id_for_body = request_id.clone();
    actix_web::rt::spawn(async move {
        match fetch_response_body(tab_id, &request_id_for_body).await {
            Ok(b) => {
                pending.event.response_size = b.len() as u64;
                pending.event.response_body = Some(b);
            },
            Err(reason) => {
                // Record why rather than silently shipping a body-less event:
                // the empty-body rate is only actionable if it can be split by
                // cause.
                pending.event.body_unavailable_reason = Some(reason);
            },
        }
        thread_for_body.push_completed(pending.event);
    });
}

fn handle_loading_failed(thread: &Arc<ThreadCapture>, tab_id: i32, params: &Value) {
    let request_id = match params.get("requestId").and_then(Value::as_str) {
        Some(s) => s,
        None => return,
    };
    let mut pending = {
        let mut tabs = match thread.tabs.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some(state) = tabs.get_mut(&tab_id) else {
            return;
        };
        match state.pending.remove(request_id) {
            Some(p) => p,
            None => return,
        }
    };
    pending.event.status = 0;
    pending.event.timing.total_duration = (current_time_ms() - pending.timestamp_ms) as f64;
    pending.event.response_body = None;
    pending.event.response_size = 0;
    // Keep CDP's own account of the failure. `status = 0` alone cannot
    // distinguish an ad-blocked tracker from a navigation-cancelled fetch from
    // a real network error — and that distinction decides whether these events
    // are noise to drop or a problem to fix.
    pending.event.failure_error_text = params
        .get("errorText")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(truncate_reason);
    pending.event.failure_blocked_reason = params
        .get("blockedReason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty())
        .map(truncate_reason);
    pending.event.failure_canceled = params.get("canceled").and_then(Value::as_bool);
    thread.push_completed(pending.event);
}

/// Issue `Network.getResponseBody` via the bridge and return the response
/// body as a string. Body is truncated to [`MAX_RESPONSE_BODY_BYTES`].
///
/// Returns `Err(reason)` rather than a bare `None` so the caller can record
/// WHY a body is missing. These four causes need different fixes — a CDP
/// refusal ("No resource with given identifier found") is the buffer-eviction
/// race, binary content is expected and harmless, and a bridge error is an
/// extension/transport problem — and collapsing them made the empty-body rate
/// undiagnosable.
async fn fetch_response_body(tab_id: i32, request_id: &str) -> Result<String, String> {
    let req = ExtensionRequest {
        request_id: Uuid::new_v4().to_string(),
        action: "debugger_command".to_string(),
        params: json!({
            "tabId": tab_id,
            "method": "Network.getResponseBody",
            "params": { "requestId": request_id },
        }),
    };
    let resp = match send_over_bridge(req).await {
        Ok(r) if r.success => match r.result {
            Some(result) => result,
            None => return Err("bridge_ok_but_no_result".to_string()),
        },
        Ok(r) => {
            // The extension reached CDP but Chrome refused. This is where the
            // evicted-buffer case surfaces; keep Chrome's own wording.
            let detail = r
                .error
                .unwrap_or_else(|| "extension reported failure".to_string());
            return Err(format!("cdp_error: {}", truncate_reason(&detail)));
        },
        Err(err) => {
            return Err(format!(
                "bridge_error: {}",
                truncate_reason(&err.to_string())
            ))
        },
    };

    // The extension's `sendDebuggerCommand` wraps the raw CDP result in
    // `{ result: <cdp_response>, ... }`, mirroring the unwrap done in
    // cdp_proxy.rs for forwarded commands. Handle both shapes.
    let cdp_result = resp.get("result").cloned().unwrap_or(resp);
    let body = match cdp_result.get("body").and_then(Value::as_str) {
        Some(body) => body,
        None => return Err("no_body_field_in_cdp_result".to_string()),
    };
    let base64_encoded = cdp_result
        .get("base64Encoded")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let body_str = if base64_encoded {
        // Skip binary content; only decode if the response advertises a
        // textual content type. Without the headers here we can't tell, so
        // we drop binary bodies unconditionally — text bodies arrive
        // un-encoded. Expected and harmless, but still worth attributing so
        // it is not counted against the eviction race.
        return Err("binary_body_skipped".to_string());
    } else {
        body.to_string()
    };

    if body_str.len() > MAX_RESPONSE_BODY_BYTES {
        let mut truncated = body_str[..MAX_RESPONSE_BODY_BYTES].to_string();
        truncated.push_str("...[truncated]");
        Ok(truncated)
    } else {
        Ok(body_str)
    }
}

/// Keep a recorded reason short — these ride on every body-less trace event
/// and are for attribution, not for full error text.
fn truncate_reason(reason: &str) -> String {
    const MAX: usize = 160;
    if reason.len() <= MAX {
        return reason.to_string();
    }
    let mut out: String = reason.chars().take(MAX).collect();
    out.push('…');
    out
}

fn parse_initiator(value: Option<&Value>) -> RequestInitiator {
    let Some(value) = value else {
        return RequestInitiator {
            initiator_type: "other".to_string(),
            stack: None,
            url: None,
        };
    };
    let initiator_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("other")
        .to_string();
    let url = value.get("url").and_then(Value::as_str).map(str::to_string);
    let stack = value
        .get("stack")
        .and_then(|s| s.get("callFrames"))
        .and_then(Value::as_array)
        .map(|frames| {
            frames
                .iter()
                .take(5)
                .map(|frame| StackFrame {
                    function_name: frame
                        .get("functionName")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    script_id: frame
                        .get("scriptId")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    url: frame
                        .get("url")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    line_number: frame.get("lineNumber").and_then(Value::as_u64).unwrap_or(0)
                        as u32,
                    column_number: frame
                        .get("columnNumber")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32,
                })
                .collect()
        });
    RequestInitiator {
        initiator_type,
        stack,
        url,
    }
}

fn redact_headers(value: Value, sensitive: &[&str]) -> HashMap<String, String> {
    let Some(obj) = value.as_object() else {
        return HashMap::new();
    };
    obj.iter()
        .map(|(name, raw)| {
            let lower = name.to_ascii_lowercase();
            let is_sensitive =
                sensitive.iter().any(|s| *s == lower.as_str()) || is_auth_header_name(&lower);
            let value = if is_sensitive {
                "[REDACTED]".to_string()
            } else {
                raw.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| raw.to_string())
            };
            (name.clone(), value)
        })
        .collect()
}

/// Final defense at the durable-trace boundary. Raw authentication material
/// has already branched into `CapturedAuthEvent`; the normal trace must remain
/// safe for persistence, mining, telemetry, and operation-routed LLM inputs.
fn sanitize_durable_trace(event: &mut NetworkTraceEvent) {
    event.url = redact_auth_query(&event.url);
    for (name, value) in &mut event.request_headers {
        if is_auth_header_name(name) || name.eq_ignore_ascii_case("cookie") {
            *value = "[REDACTED]".to_string();
        }
    }
    for (name, value) in &mut event.response_headers {
        let lower = name.to_ascii_lowercase();
        if SENSITIVE_RESPONSE_HEADER_NAMES.contains(&lower.as_str()) || is_auth_header_name(name) {
            *value = "[REDACTED]".to_string();
        }
    }
    if let Some(body) = event.request_body.as_deref() {
        event.request_body = Some(redact_json_body(body));
    }
    if let Some(body) = event.response_body.as_deref() {
        event.response_body = Some(redact_json_body(body));
    }
    if let Some(url) = event.initiator.url.as_deref() {
        event.initiator.url = Some(redact_auth_query(url));
    }
    if let Some(stack) = event.initiator.stack.as_mut() {
        for frame in stack {
            frame.url = redact_auth_query(&frame.url);
        }
    }
}

fn redact_json_body(body: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(body) else {
        return body.to_string();
    };
    redact_json_value(&mut value);
    serde_json::to_string(&value).unwrap_or_else(|_| body.to_string())
}

fn redact_json_value(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (name, value) in object {
                if is_sensitive_body_field(name) {
                    *value = Value::String("[REDACTED]".to_string());
                } else {
                    redact_json_value(value);
                }
            }
        },
        Value::Array(values) => {
            for value in values {
                redact_json_value(value);
            }
        },
        _ => {},
    }
}

fn is_sensitive_body_field(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();
    let compact = normalized
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>();
    normalized.contains("password")
        || normalized.contains("passwd")
        || normalized.contains("authorization")
        || normalized.contains("api_key")
        || normalized.contains("api-key")
        || normalized.contains("access_token")
        || normalized.contains("refresh_token")
        || normalized.contains("client_secret")
        || matches!(compact.as_str(), "apikey" | "clientsecret")
        || compact.ends_with("token")
        || compact.ends_with("secret")
        || compact.ends_with("csrf")
        || compact.ends_with("xsrf")
}

fn captured_auth_event(
    request_id: String,
    url: String,
    headers: &Value,
    timestamp: i64,
) -> Option<CapturedAuthEvent> {
    let object = headers.as_object()?;
    let mut auth_headers = HashMap::new();
    let mut cookie_header = None;

    for (name, raw_value) in object {
        let lower = name.to_ascii_lowercase();
        let value = raw_value
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| raw_value.to_string());
        if value.trim().is_empty() || value == "[REDACTED]" {
            continue;
        }
        if lower == "cookie" {
            cookie_header = Some(value);
        } else if is_auth_header_name(&lower) {
            auth_headers.insert(name.clone(), value);
        }
    }

    if auth_headers.is_empty() && cookie_header.is_none() && !url_has_auth_query(&url) {
        return None;
    }

    Some(CapturedAuthEvent {
        request_id,
        url,
        auth_headers,
        cookie_header,
        timestamp,
    })
}

fn url_has_auth_query(url: &str) -> bool {
    let Some((_, query)) = url.split_once('?') else {
        return false;
    };
    query.split('&').any(|pair| {
        let key = pair.split_once('=').map(|(key, _)| key).unwrap_or(pair);
        is_auth_query_key(key)
    })
}

fn is_auth_query_key(key: &str) -> bool {
    let normalized = percent_decode_ascii(key).to_ascii_lowercase();
    let compact = normalized
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>();
    let has_auth_segment = normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|segment| {
            matches!(
                segment,
                "key" | "auth" | "token" | "secret" | "csrf" | "xsrf"
            )
        });

    has_auth_segment
        || matches!(
            compact.as_str(),
            "apikey"
                | "accesskey"
                | "secretkey"
                | "subscriptionkey"
                | "authorization"
                | "authentication"
        )
        || compact.ends_with("token")
        || compact.ends_with("secret")
        || compact.ends_with("csrf")
        || compact.ends_with("xsrf")
}

fn is_auth_header_name(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();
    if SENSITIVE_REQUEST_HEADER_NAMES.contains(&normalized.as_str()) {
        return true;
    }
    let compact = normalized
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>();
    normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|segment| {
            matches!(
                segment,
                "auth" | "authorization" | "token" | "secret" | "csrf" | "xsrf"
            )
        })
        || normalized.ends_with("-api-key")
        || normalized.ends_with("-apikey")
        || matches!(compact.as_str(), "apikey" | "authorization")
        || compact.ends_with("authtoken")
        || compact.ends_with("accesstoken")
        || compact.ends_with("securitytoken")
        || compact.ends_with("sessiontoken")
        || compact.ends_with("csrftoken")
        || compact.ends_with("xsrftoken")
        || compact.ends_with("clientsecret")
}

fn percent_decode_ascii(value: &str) -> String {
    fn hex_value(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }

    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
            {
                decoded.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn redact_auth_query(url: &str) -> String {
    let Some((base, query_and_fragment)) = url.split_once('?') else {
        return url.to_string();
    };
    let (query, fragment) = query_and_fragment
        .split_once('#')
        .map(|(query, fragment)| (query, Some(fragment)))
        .unwrap_or((query_and_fragment, None));
    let redacted = query
        .split('&')
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            if is_auth_query_key(key) {
                format!("{key}=[REDACTED]")
            } else if pair.contains('=') {
                format!("{key}={value}")
            } else {
                key.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("&");
    match fragment {
        Some(fragment) => format!("{base}?{redacted}#{fragment}"),
        None => format!("{base}?{redacted}"),
    }
}

fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_headers_replaces_sensitive_values() {
        let headers = json!({
            "Authorization": "Bearer abc123",
            "Content-Type": "application/json",
            "X-API-Key": "secret-key",
        });
        let redacted = redact_headers(headers, SENSITIVE_REQUEST_HEADER_NAMES);
        assert_eq!(redacted.get("Authorization").unwrap(), "[REDACTED]");
        assert_eq!(redacted.get("X-API-Key").unwrap(), "[REDACTED]");
        assert_eq!(redacted.get("Content-Type").unwrap(), "application/json");
    }

    #[test]
    fn captured_auth_event_keeps_secrets_out_of_redacted_trace_headers() {
        let headers = json!({
            "Authorization": "Bearer abc123",
            "Cookie": "session=cookie-secret",
            "Content-Type": "application/json",
        });

        let event = captured_auth_event(
            "request-1".to_string(),
            "https://app.example.com/api/me".to_string(),
            &headers,
            42,
        )
        .expect("auth-bearing request should produce an ephemeral event");
        let redacted = redact_headers(headers, SENSITIVE_REQUEST_HEADER_NAMES);

        assert_eq!(
            event.auth_headers.get("Authorization").map(String::as_str),
            Some("Bearer abc123")
        );
        assert_eq!(
            event.cookie_header.as_deref(),
            Some("session=cookie-secret")
        );
        assert_eq!(
            redacted.get("Authorization").map(String::as_str),
            Some("[REDACTED]")
        );
        assert_eq!(
            redacted.get("Cookie").map(String::as_str),
            Some("[REDACTED]")
        );
        assert!(!event.auth_headers.contains_key("Content-Type"));
    }

    #[test]
    fn captured_auth_event_accepts_auth_query_without_headers() {
        let event = captured_auth_event(
            "request-2".to_string(),
            "https://api.example.com/data?api_key=secret".to_string(),
            &json!({}),
            43,
        );
        assert!(event.is_some());
    }

    #[test]
    fn captured_auth_event_accepts_vendor_api_key_headers() {
        let event = captured_auth_event(
            "request-3".to_string(),
            "https://api.example.com/data".to_string(),
            &json!({"x-algolia-api-key": "vendor-secret"}),
            44,
        )
        .expect("vendor API key should produce an ephemeral event");
        assert_eq!(
            event
                .auth_headers
                .get("x-algolia-api-key")
                .map(String::as_str),
            Some("vendor-secret")
        );
        assert_eq!(
            redact_headers(
                json!({"x-algolia-api-key": "vendor-secret"}),
                SENSITIVE_REQUEST_HEADER_NAMES,
            )
            .get("x-algolia-api-key")
            .map(String::as_str),
            Some("[REDACTED]")
        );
    }

    #[test]
    fn auth_query_detection_ignores_words_that_only_contain_key_or_auth() {
        assert!(!url_has_auth_query(
            "https://api.example.com/data?keyboard=compact&author=alice&monkey=capuchin"
        ));
        assert!(url_has_auth_query(
            "https://api.example.com/data?apiKey=secret"
        ));
        assert!(url_has_auth_query(
            "https://api.example.com/data?access_token=secret"
        ));
        assert!(url_has_auth_query(
            "https://api.example.com/data?api%5Fkey=secret"
        ));
    }

    #[test]
    fn durable_trace_url_redacts_auth_query_values_only() {
        assert_eq!(
            redact_auth_query("https://api.example.com/data?q=rust&api_key=secret#results"),
            "https://api.example.com/data?q=rust&api_key=[REDACTED]#results"
        );
        assert_eq!(
            redact_auth_query(
                "https://api.example.com/data?keyboard=compact&author=alice&monkey=capuchin"
            ),
            "https://api.example.com/data?keyboard=compact&author=alice&monkey=capuchin"
        );
        assert_eq!(
            redact_auth_query("https://api.example.com/data?api%5Fkey=secret"),
            "https://api.example.com/data?api%5Fkey=[REDACTED]"
        );
    }

    #[test]
    fn durable_trace_boundary_keeps_raw_auth_only_in_ephemeral_event() {
        let raw_headers = json!({
            "Authorization": "Bearer raw-header-secret",
            "Cookie": "session=raw-cookie-secret",
            "Content-Type": "application/json",
        });
        let ephemeral = captured_auth_event(
            "request-boundary".to_string(),
            "https://api.example.com/data?access_token=raw-query-secret".to_string(),
            &raw_headers,
            45,
        )
        .expect("raw authentication should be captured transiently");
        let mut durable = NetworkTraceEvent {
            request_id: "request-boundary".to_string(),
            method: "POST".to_string(),
            url: ephemeral.url.clone(),
            resource_type: Some("Fetch".to_string()),
            frame_id: None,
            tab_id: None,
            thread_id: None,
            request_headers: raw_headers
                .as_object()
                .unwrap()
                .iter()
                .map(|(name, value)| (name.clone(), value.as_str().unwrap().to_string()))
                .collect(),
            request_body: Some(
                r#"{"username":"alice","access_token":"raw-body-secret"}"#.to_string(),
            ),
            response_headers: HashMap::from([(
                "Set-Cookie".to_string(),
                "session=response-secret".to_string(),
            )]),
            response_body: Some(r#"{"refreshToken":"raw-response-secret"}"#.to_string()),
            body_unavailable_reason: None,
            failure_error_text: None,
            failure_blocked_reason: None,
            failure_canceled: None,
            status: 200,
            timing: RequestTiming {
                request_time: 0.0,
                dns_duration: None,
                connect_duration: None,
                ssl_duration: None,
                ttfb: None,
                total_duration: 0.0,
            },
            initiator: RequestInitiator {
                initiator_type: "script".to_string(),
                stack: None,
                url: None,
            },
            timestamp: 0,
            request_size: 0,
            response_size: 0,
            capture_source: None,
        };

        sanitize_durable_trace(&mut durable);
        let serialized = serde_json::to_string(&durable).unwrap();

        assert!(serialized.contains("[REDACTED]"));
        for secret in [
            "raw-header-secret",
            "raw-cookie-secret",
            "raw-query-secret",
            "raw-body-secret",
            "response-secret",
            "raw-response-secret",
        ] {
            assert!(
                !serialized.contains(secret),
                "durable trace leaked {secret}"
            );
        }
        assert_eq!(
            ephemeral
                .auth_headers
                .get("Authorization")
                .map(String::as_str),
            Some("Bearer raw-header-secret")
        );
    }

    #[test]
    fn parse_initiator_handles_missing_fields() {
        let initiator = parse_initiator(None);
        assert_eq!(initiator.initiator_type, "other");
        assert!(initiator.stack.is_none());
        assert!(initiator.url.is_none());
    }

    #[test]
    fn parse_initiator_extracts_stack_top_5() {
        let value = json!({
            "type": "script",
            "stack": {
                "callFrames": [
                    {"functionName": "f0", "scriptId": "1", "url": "u", "lineNumber": 1, "columnNumber": 0},
                    {"functionName": "f1", "scriptId": "2", "url": "u", "lineNumber": 2, "columnNumber": 0},
                    {"functionName": "f2", "scriptId": "3", "url": "u", "lineNumber": 3, "columnNumber": 0},
                    {"functionName": "f3", "scriptId": "4", "url": "u", "lineNumber": 4, "columnNumber": 0},
                    {"functionName": "f4", "scriptId": "5", "url": "u", "lineNumber": 5, "columnNumber": 0},
                    {"functionName": "f5", "scriptId": "6", "url": "u", "lineNumber": 6, "columnNumber": 0},
                ]
            }
        });
        let initiator = parse_initiator(Some(&value));
        assert_eq!(initiator.initiator_type, "script");
        let stack = initiator.stack.expect("stack present");
        assert_eq!(stack.len(), 5);
        assert_eq!(stack[0].function_name, "f0");
        assert_eq!(stack[4].function_name, "f4");
    }

    #[test]
    fn push_completed_caps_buffer_size() {
        let thread = Arc::new(ThreadCapture::new("t1".to_string()));
        for i in 0..(MAX_TRACES_PER_THREAD + 50) {
            let event = NetworkTraceEvent {
                request_id: format!("req{}", i),
                method: "GET".to_string(),
                url: format!("https://example.com/{}", i),
                resource_type: Some("XHR".to_string()),
                frame_id: None,
                tab_id: None,
                thread_id: None,
                request_headers: HashMap::new(),
                request_body: None,
                response_headers: HashMap::new(),
                response_body: None,
                body_unavailable_reason: None,
                failure_error_text: None,
                failure_blocked_reason: None,
                failure_canceled: None,
                status: 200,
                timing: RequestTiming {
                    request_time: 0.0,
                    dns_duration: None,
                    connect_duration: None,
                    ssl_duration: None,
                    ttfb: None,
                    total_duration: 0.0,
                },
                initiator: RequestInitiator {
                    initiator_type: "script".to_string(),
                    stack: None,
                    url: None,
                },
                timestamp: 0,
                request_size: 0,
                response_size: 0,
                capture_source: None,
            };
            thread.push_completed(event);
        }
        let drained = thread.drain();
        assert_eq!(drained.len(), MAX_TRACES_PER_THREAD);
        // Every drained event must be tagged.
        assert!(drained
            .iter()
            .all(|e| e.capture_source.as_deref() == Some("cdp_proxy")));
        // Oldest entries evicted: first remaining is req50, last is req249.
        assert_eq!(drained[0].request_id, "req50");
        assert_eq!(
            drained.last().unwrap().request_id,
            format!("req{}", MAX_TRACES_PER_THREAD + 49)
        );
    }

    #[test]
    fn drain_returns_empty_for_unknown_thread() {
        assert!(drain("never-registered").is_empty());
        assert!(disable_thread("never-registered").is_empty());
    }
}

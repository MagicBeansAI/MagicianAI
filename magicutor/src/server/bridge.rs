use actix::{Actor, ActorContext, Addr, AsyncContext, Handler, Message, StreamHandler};
use actix_http::ws::Item as WsItem;
use actix_web::{web, Error as ActixError, HttpRequest, HttpResponse};
use actix_web_actors::ws;
use bytes::BytesMut;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::sync::{oneshot, RwLock};
use tracing::{debug, info, trace, warn};

use crate::bridge_protocol::{ExtensionRequest, ExtensionResponse};
use crate::server::page_signals;
use crate::types::{ExecutionError, Result};

/// Shared bridge state (single active connection supported for now).
#[derive(Default)]
struct BridgeHub {
    active_session_id: Option<u64>,
    active_addr: Option<Addr<BridgeSession>>,
    tx: Option<tokio::sync::mpsc::UnboundedSender<BridgeOutbound>>,
    pending: HashMap<String, oneshot::Sender<std::result::Result<ExtensionResponse, String>>>,
}

static HUB: Lazy<Arc<RwLock<BridgeHub>>> =
    Lazy::new(|| Arc::new(RwLock::new(BridgeHub::default())));
static NEXT_BRIDGE_SESSION_ID: AtomicU64 = AtomicU64::new(1);

/// Whether a browser extension currently owns the bridge. Health and Edge
/// capability discovery use this signal so a running proxy without an
/// extension is not advertised as usable browser control.
pub async fn is_connected() -> bool {
    HUB.read().await.tx.is_some()
}

/// Process-wide CDP event subscribers, keyed by Chrome tab id. The cdp_proxy
/// CdpSession actors register here when they attach to a tab and receive
/// chrome.debugger events forwarded by the extension. Multiple subscribers
/// per tab are supported (e.g. browser-level + page-level WS for the same
/// tab) — each gets a clone of every event.
static CDP_EVENT_SUBSCRIBERS: Lazy<std::sync::Mutex<HashMap<i32, Vec<CdpEventSubscriber>>>> =
    Lazy::new(|| std::sync::Mutex::new(HashMap::new()));

/// Subscriber callback. Returns false if the receiver should be dropped
/// (e.g. WS closed); the dispatcher prunes the registry on next event.
type CdpEventSubscriber = Arc<dyn Fn(CdpEventPayload) -> bool + Send + Sync>;

/// Register a closure to receive CDP events for the given tab. Returns a
/// SubscriberHandle that unregisters on drop. The closure is invoked from
/// the bridge dispatcher; it must be cheap and non-blocking — typically
/// `addr.do_send(event)` to an Actix actor.
pub fn subscribe_cdp_events(
    tab_id: i32,
    callback: impl Fn(CdpEventPayload) -> bool + Send + Sync + 'static,
) -> CdpSubscriberHandle {
    let arc: CdpEventSubscriber = Arc::new(callback);
    if let Ok(mut map) = CDP_EVENT_SUBSCRIBERS.lock() {
        map.entry(tab_id).or_default().push(arc.clone());
    }
    CdpSubscriberHandle { tab_id, arc }
}

/// RAII handle that unregisters the subscriber when dropped.
pub struct CdpSubscriberHandle {
    tab_id: i32,
    arc: CdpEventSubscriber,
}

impl Drop for CdpSubscriberHandle {
    fn drop(&mut self) {
        if let Ok(mut map) = CDP_EVENT_SUBSCRIBERS.lock() {
            if let Some(list) = map.get_mut(&self.tab_id) {
                list.retain(|s| !Arc::ptr_eq(s, &self.arc));
                if list.is_empty() {
                    map.remove(&self.tab_id);
                }
            }
        }
    }
}

/// Payload for a CDP event pushed from the extension. Mirrors the shape the
/// extension serialises: `{tabId, method, params}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CdpEventPayload {
    pub tab_id: i32,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BridgeEnvelope {
    Request {
        data: ExtensionRequest,
    },
    Response {
        data: ExtensionResponse,
    },
    Ping {
        timestamp: i64,
    },
    Pong {
        timestamp: i64,
    },
    /// Fire-and-forget chrome.debugger event pushed by the extension.
    /// Routed to subscribers registered via [`subscribe_cdp_events`].
    CdpEvent {
        data: CdpEventPayload,
    },
    /// Fire-and-forget passive page identity/change signal pushed by the
    /// extension. Buffered by thread until Magician drains it.
    AmbientPageSignal {
        data: crate::types::AmbientPageSignal,
    },
}

#[derive(Debug)]
enum BridgeOutbound {
    Request(ExtensionRequest),
}

pub async fn bridge_route(
    req: HttpRequest,
    stream: web::Payload,
    cfg: web::Data<crate::MagicutorConfig>,
) -> std::result::Result<HttpResponse, ActixError> {
    info!("Bridge connection attempt from {:?}", req.peer_addr());

    if !cfg.bridge.enabled {
        warn!("Bridge connection rejected: bridge disabled in config");
        return Ok(HttpResponse::NotFound().finish());
    }

    let token_ok = if cfg.bridge.auth_token.is_empty() {
        true
    } else {
        let header_ok = req
            .headers()
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .map(|v| {
                v.trim_start_matches("Bearer ")
                    .eq(cfg.bridge.auth_token.as_str())
            })
            .unwrap_or(false);
        let query_ok = req
            .query_string()
            .split('&')
            .find_map(|kv| {
                let mut parts = kv.split('=');
                match (parts.next(), parts.next()) {
                    (Some("token"), Some(val)) => Some(val == cfg.bridge.auth_token),
                    _ => None,
                }
            })
            .unwrap_or(false);
        header_ok || query_ok
    };

    if !token_ok {
        warn!("Bridge connection rejected: missing/invalid token");
        return Ok(HttpResponse::Unauthorized().finish());
    }

    // Use WsResponseBuilder to configure larger frame sizes for DOM snapshots
    // Default is 64KB which is too small for large page content
    ws::WsResponseBuilder::new(
        BridgeSession {
            session_id: NEXT_BRIDGE_SESSION_ID.fetch_add(1, Ordering::Relaxed),
            _config: cfg.clone(),
            continuation_buffer: BytesMut::new(),
            continuation_is_text: false,
        },
        &req,
        stream,
    )
    .frame_size(16 * 1024 * 1024) // 16MB max frame size
    .start()
}

struct BridgeSession {
    session_id: u64,
    _config: web::Data<crate::MagicutorConfig>,
    /// Buffer for reassembling continuation frames (large messages)
    continuation_buffer: BytesMut,
    /// Whether we're currently accumulating a text message (vs binary)
    continuation_is_text: bool,
}

impl Actor for BridgeSession {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        info!(session_id = self.session_id, "Bridge session started");

        // Start heartbeat to keep connection alive
        ctx.run_interval(std::time::Duration::from_secs(15), |_act, ctx| {
            ctx.ping(b"");
        });

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BridgeOutbound>();

        // Register sender
        {
            let hub = HUB.clone();
            let session_id = self.session_id;
            let addr = ctx.address();
            actix_web::rt::spawn(async move {
                let mut guard = hub.write().await;
                if guard
                    .active_session_id
                    .is_some_and(|active_id| active_id > session_id)
                {
                    drop(guard);
                    addr.do_send(CloseBridgeSession);
                    debug!(session_id, "discarded stale bridge session registration");
                    return;
                }
                let old_addr = guard.active_addr.take();
                let replaced_session_id = guard.active_session_id.replace(session_id);
                guard.active_addr = Some(addr);
                guard.tx = Some(tx);
                let pending = std::mem::take(&mut guard.pending);
                drop(guard);
                if let Some(old_addr) = old_addr {
                    old_addr.do_send(CloseBridgeSession);
                }
                for (_, sender) in pending {
                    let _ = sender.send(Err("active bridge session was replaced".to_string()));
                }
                info!(
                    session_id,
                    replaced_session_id, "Bridge sender registered in HUB"
                );
            });
        }

        // Outgoing sender loop
        let addr = ctx.address();
        actix_web::rt::spawn(async move {
            while let Some(msg) = rx.recv().await {
                match msg {
                    BridgeOutbound::Request(req) => {
                        let env = BridgeEnvelope::Request { data: req };
                        if let Ok(text) = serde_json::to_string(&env) {
                            addr.do_send(WsText(text));
                        }
                    },
                }
            }
            debug!("Bridge outgoing sender loop ended");
        });
    }

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        info!(session_id = self.session_id, "Bridge session stopped");
        let session_id = self.session_id;
        actix_web::rt::spawn(async move {
            let mut hub = HUB.write().await;
            if hub.active_session_id == Some(session_id) {
                hub.active_session_id = None;
                hub.active_addr = None;
                hub.tx = None;
                let pending = std::mem::take(&mut hub.pending);
                drop(hub);
                for (_, sender) in pending {
                    let _ = sender.send(Err("bridge session disconnected".to_string()));
                }
            }
        });
    }
}

struct CloseBridgeSession;

impl Message for CloseBridgeSession {
    type Result = ();
}

impl Handler<CloseBridgeSession> for BridgeSession {
    type Result = ();

    fn handle(&mut self, _msg: CloseBridgeSession, ctx: &mut Self::Context) {
        ctx.close(None);
        ctx.stop();
    }
}

struct WsText(String);

impl Message for WsText {
    type Result = ();
}

impl Handler<WsText> for BridgeSession {
    type Result = ();
    fn handle(&mut self, msg: WsText, ctx: &mut Self::Context) -> Self::Result {
        ctx.text(msg.0);
    }
}

impl BridgeSession {
    /// Process a complete text message (either received directly or reassembled from continuation frames)
    fn process_text_message(&self, text: &str, ctx: &mut ws::WebsocketContext<Self>) {
        let text_len = text.len();
        trace!("Bridge inbound: {} bytes", text_len);
        match serde_json::from_str::<BridgeEnvelope>(text) {
            Ok(env) => match env {
                BridgeEnvelope::Response { data } => {
                    let request_id = data.request_id.clone();
                    debug!(
                        "Bridge response: id={}, success={}, size={} bytes",
                        request_id, data.success, text_len
                    );
                    actix_web::rt::spawn(async move {
                        let mut hub = HUB.write().await;
                        if let Some(tx) = hub.pending.remove(&data.request_id) {
                            if tx.send(Ok(data)).is_err() {
                                warn!("Bridge response channel closed for {}", request_id);
                            }
                        } else {
                            warn!(
                                "Dropping unmatched bridge response {} (pending: {:?})",
                                data.request_id,
                                hub.pending.keys().collect::<Vec<_>>()
                            );
                        }
                    });
                },
                BridgeEnvelope::Ping { timestamp } => {
                    // Respond with pong to keep extension alive
                    trace!("Bridge received ping, sending pong");
                    let pong = BridgeEnvelope::Pong { timestamp };
                    if let Ok(text) = serde_json::to_string(&pong) {
                        ctx.text(text);
                    }
                },
                BridgeEnvelope::Pong { .. } => {
                    trace!("Bridge received pong");
                },
                BridgeEnvelope::Request { .. } => {
                    // Server shouldn't receive requests from extension in normal flow
                    warn!("Unexpected request from extension");
                },
                BridgeEnvelope::CdpEvent { data } => {
                    // Fan out to subscribers for this tab. Cheap clones —
                    // the closures typically just `do_send` to an actor.
                    let subscribers: Vec<CdpEventSubscriber> = match CDP_EVENT_SUBSCRIBERS.lock() {
                        Ok(map) => map.get(&data.tab_id).cloned().unwrap_or_default(),
                        Err(e) => {
                            warn!("CDP_EVENT_SUBSCRIBERS poisoned: {}", e);
                            return;
                        },
                    };
                    let n = subscribers.len();
                    debug!(
                        "[bridge] cdp_event tab={} method={} subscribers={}",
                        data.tab_id, data.method, n
                    );
                    if n == 0 {
                        return;
                    }
                    for cb in subscribers {
                        let _ = cb(data.clone());
                    }
                },
                BridgeEnvelope::AmbientPageSignal { data } => {
                    page_signals::record(data);
                },
            },
            Err(e) => {
                warn!(
                    "Failed to parse bridge message (error: {}), first 200 chars: {}",
                    e,
                    &text[..std::cmp::min(200, text.len())]
                );
            },
        }
    }
}

impl StreamHandler<std::result::Result<ws::Message, ws::ProtocolError>> for BridgeSession {
    fn handle(
        &mut self,
        item: std::result::Result<ws::Message, ws::ProtocolError>,
        ctx: &mut Self::Context,
    ) {
        match item {
            Ok(msg) => match msg {
                ws::Message::Text(text) => {
                    self.process_text_message(&text, ctx);
                },
                ws::Message::Binary(data) => {
                    info!(
                        "Bridge received binary data: {} bytes (unexpected - should be text)",
                        data.len()
                    );
                },
                ws::Message::Close(reason) => {
                    info!("Bridge received close frame: {:?}", reason);
                    ctx.close(reason);
                },
                ws::Message::Ping(data) => {
                    trace!("Bridge received WS ping, sending pong");
                    ctx.pong(&data);
                },
                ws::Message::Pong(_) => {
                    trace!("Bridge received WS pong");
                },
                ws::Message::Continuation(item) => {
                    // Handle fragmented messages (large payloads are split across frames)
                    match item {
                        WsItem::FirstText(bytes) => {
                            trace!("Bridge continuation: FirstText {} bytes", bytes.len());
                            self.continuation_buffer.clear();
                            self.continuation_buffer.extend_from_slice(&bytes);
                            self.continuation_is_text = true;
                        },
                        WsItem::FirstBinary(bytes) => {
                            trace!("Bridge continuation: FirstBinary {} bytes", bytes.len());
                            self.continuation_buffer.clear();
                            self.continuation_buffer.extend_from_slice(&bytes);
                            self.continuation_is_text = false;
                        },
                        WsItem::Continue(bytes) => {
                            trace!(
                                "Bridge continuation: {} bytes (total: {} bytes)",
                                bytes.len(),
                                self.continuation_buffer.len() + bytes.len()
                            );
                            self.continuation_buffer.extend_from_slice(&bytes);
                        },
                        WsItem::Last(bytes) => {
                            self.continuation_buffer.extend_from_slice(&bytes);
                            let total_len = self.continuation_buffer.len();
                            debug!("Bridge reassembled: {} bytes", total_len);

                            if self.continuation_is_text {
                                match std::str::from_utf8(&self.continuation_buffer) {
                                    Ok(text) => {
                                        self.process_text_message(text, ctx);
                                    },
                                    Err(e) => {
                                        warn!("Bridge continuation: invalid UTF-8: {}", e);
                                    },
                                }
                            } else {
                                warn!(
                                    "Bridge received binary data: {} bytes (unexpected)",
                                    total_len
                                );
                            }

                            self.continuation_buffer.clear();
                        },
                    }
                },
                ws::Message::Nop => {},
            },
            Err(e) => {
                let rendered = format!("{e:?}");
                if rendered.contains("UnexpectedEof")
                    || rendered.contains("ResetWithoutClosingHandshake")
                    || rendered.contains("ConnectionReset")
                {
                    debug!(session_id = self.session_id, error = %rendered, "Bridge WebSocket disconnected");
                } else {
                    warn!(session_id = self.session_id, error = %rendered, "Bridge WebSocket protocol error");
                }
            },
        }
    }
}

/// Send a request over the bridge if connected.
pub async fn send_over_bridge(req: ExtensionRequest) -> Result<ExtensionResponse> {
    let action = req.action.clone();
    let request_id = req.request_id.clone();
    let timeout_secs = bridge_timeout_secs(&req);
    debug!("Bridge request: action={}, id={}", action, request_id);

    let (tx, rx) = oneshot::channel();
    {
        let mut hub = HUB.write().await;
        let Some(sender) = hub.tx.clone() else {
            warn!("Bridge not connected (no WebSocket session active)");
            return Err(ExecutionError::BridgeError(
                "Bridge not connected".to_string(),
            ));
        };
        hub.pending.insert(req.request_id.clone(), tx);
        sender
            .send(BridgeOutbound::Request(req))
            .map_err(|e| ExecutionError::BridgeError(e.to_string()))?;
    }

    match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), rx).await {
        Ok(Ok(Ok(response))) => {
            if !response.success {
                warn!(
                    "Bridge error response: id={}, error={:?}",
                    response.request_id, response.error
                );
            }
            Ok(response)
        },
        Ok(Ok(Err(reason))) => {
            debug!(
                reason,
                "send_over_bridge: bridge session ended before response"
            );
            Err(ExecutionError::BridgeError(reason))
        },
        Ok(Err(e)) => {
            warn!("send_over_bridge: Channel error: {}", e);
            Err(ExecutionError::BridgeError(e.to_string()))
        },
        Err(_) => {
            {
                let mut hub = HUB.write().await;
                hub.pending.remove(&request_id);
            }
            warn!(
                "send_over_bridge: Timeout waiting for response ({}s)",
                timeout_secs
            );
            Err(ExecutionError::BridgeTimeout {
                action,
                timeout_secs,
            })
        },
    }
}

fn bridge_timeout_secs(req: &ExtensionRequest) -> u64 {
    match req.action.as_str() {
        "probe_contextual_assist" => 2,
        "capture_contextual_assist_tab" => 10,
        "list_tabs" => 10,
        "clear_session" | "session_ended" | "close_tab" | "close_window" => 15,
        "create_window" | "attach_tab" | "attach_debugger" => 60,
        "debugger_command" => 75,
        "scroll" => 20,
        "click" | "click_coordinates" | "hover" | "press_key" | "slide" | "drag_and_drop"
        | "drag_path" => 60,
        _ => 600,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bridge_request(action: &str) -> ExtensionRequest {
        ExtensionRequest {
            request_id: "test-request".to_string(),
            action: action.to_string(),
            params: json!({}),
        }
    }

    #[test]
    fn debugger_commands_do_not_wait_for_default_bridge_timeout() {
        assert_eq!(bridge_timeout_secs(&bridge_request("debugger_command")), 75);
    }

    #[test]
    fn non_browser_actions_keep_existing_default_timeout() {
        assert_eq!(bridge_timeout_secs(&bridge_request("api_replay")), 600);
    }

    #[test]
    fn target_discovery_does_not_exceed_agent_browser_connect_timeout() {
        assert_eq!(bridge_timeout_secs(&bridge_request("list_tabs")), 10);
    }
}

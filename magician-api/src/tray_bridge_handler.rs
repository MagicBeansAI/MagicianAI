//! Native presence/tray bridge WebSocket actor (Phase 5).
//!
//! A native tray or mascot app (macOS / Windows / Linux) registers a media
//! session of surface type `tray_macos`, `mascot_macos`, etc., then connects here to
//! stream:
//!
//! * **screen frames** as binary frames (JPEG / WebP),
//! * **ambient audio chunks** as binary frames (PCM16),
//! * **pointer commands** as text frames (JSON),
//! * **transcripts** as text frames (JSON),
//! * **capability updates** as text frames (JSON),
//! * **mascot/presence state** as text frames (JSON).
//!
//! See `docs/components/magician/realtime-media/tray-bridge-protocol.md`
//! for the per-message contract.
//!
//! The actor itself is intentionally simple: it validates session
//! ownership, classifies frames by an embedded `kind` field on text
//! messages, emits `media.tray.*` events through the broadcaster, and
//! exposes a `TrayDownstreamFrame` actix message that upstream code
//! paths can use to push pointer commands / overlay updates back to
//! the tray.

use std::sync::Arc;
use std::time::{Duration, Instant};

use actix::prelude::*;
use actix_web::{web, HttpRequest, HttpResponse, Result as ActixResult};
use actix_web_actors::ws;
use serde_json::{json, Value};
use tracing::{debug, error, info, warn};

use crate::scope::resolve_required_scope;
use crate::websocket_handler::validate_origin;
use magician::magician_v2::realtime_events::RuntimeTransportBroadcaster;
use magician_media::{SurfaceType, MEDIA_SYSTEM_AGENT};

use crate::media_api::MediaApi;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const CLIENT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, serde::Deserialize)]
pub struct TrayBridgeQuery {
    #[serde(default)]
    workspace: Option<String>,
}

pub async fn tray_bridge_ws_handler(
    req: HttpRequest,
    stream: web::Payload,
    path: web::Path<String>,
    query: web::Query<TrayBridgeQuery>,
    media_api: web::Data<Arc<MediaApi>>,
) -> ActixResult<HttpResponse> {
    // CSWSH defense — same gate as the voice bridge. Browsers
    // shouldn't be opening tray bridges from a foreign origin in any
    // case, but the check costs nothing and matches the rest of the
    // WS surface.
    if let Err((origin, host)) = validate_origin(&req) {
        warn!(
            origin = %origin,
            host = %host,
            "[TRAY-BRIDGE] WebSocket Origin mismatch — rejecting"
        );
        return Ok(HttpResponse::Forbidden().json(json!({
            "error": "origin_not_allowed",
        })));
    }
    let media_session_id = path.into_inner();
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };

    let registry = media_api.registry();
    let Some(session) = registry.get(&media_session_id) else {
        return Ok(HttpResponse::NotFound().json(json!({
            "error": "media_session_not_found",
            "session_id": media_session_id,
        })));
    };
    if session.principal != principal || session.workspace != workspace {
        return Ok(HttpResponse::Forbidden().json(json!({
            "error": "media_session_scope_mismatch",
        })));
    }
    if !matches!(
        session.surface_type,
        SurfaceType::TrayMacos
            | SurfaceType::TrayWindows
            | SurfaceType::TrayLinux
            | SurfaceType::MascotMacos
            | SurfaceType::MascotWindows
            | SurfaceType::MascotLinux
    ) {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "native_bridge_requires_native_surface",
            "surface_type": session.surface_type,
        })));
    }

    let actor = TrayBridgeSession {
        media_session_id,
        principal,
        workspace,
        thread_id: session.thread_id.clone(),
        last_heartbeat: Instant::now(),
        broadcaster: registry.broadcaster(),
    };
    ws::start(actor, &req, stream)
}

pub struct TrayBridgeSession {
    media_session_id: String,
    principal: String,
    workspace: String,
    thread_id: Option<String>,
    last_heartbeat: Instant,
    broadcaster: Arc<RuntimeTransportBroadcaster>,
}

impl TrayBridgeSession {
    fn emit(&self, event_type: &str, mut payload: Value) {
        if let Some(obj) = payload.as_object_mut() {
            obj.entry("media_session_id".to_string())
                .or_insert_with(|| Value::String(self.media_session_id.clone()));
            if let Some(thread) = &self.thread_id {
                obj.entry("thread_id".to_string())
                    .or_insert_with(|| Value::String(thread.clone()));
            }
        }
        self.broadcaster.emit_named(
            event_type,
            MEDIA_SYSTEM_AGENT,
            Some(&self.principal),
            Some(&self.workspace),
            payload,
        );
    }

    fn schedule_heartbeat(&self, ctx: &mut <Self as Actor>::Context) {
        ctx.run_interval(HEARTBEAT_INTERVAL, |actor, ctx| {
            if Instant::now().duration_since(actor.last_heartbeat) > CLIENT_TIMEOUT {
                warn!("[TRAY-BRIDGE] {} timed out", actor.media_session_id);
                actor.emit(
                    "media.tray.bridge.error",
                    json!({ "reason": "heartbeat_timeout" }),
                );
                ctx.stop();
                return;
            }
            ctx.ping(b"");
        });
    }

    fn dispatch_text(&self, text: &str) {
        let parsed: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(_) => {
                self.emit(
                    "media.tray.bridge.error",
                    json!({ "reason": "invalid_text_frame_json", "preview": &text[..text.len().min(200)] }),
                );
                return;
            },
        };
        let kind = parsed.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        match kind {
            "pointer" => self.emit("media.tray.pointer.command", parsed),
            "transcript" => self.emit("media.transcript.final", parsed),
            "transcript.delta" => self.emit("media.transcript.delta", parsed),
            "capability" => self.emit("media.capabilities.updated", parsed),
            "mascot" => self.emit("media.mascot.state.changed", parsed),
            "chunk.metadata" => self.emit("media.tray.frame.received", parsed),
            other => {
                // Truncate the client-supplied `kind` before echoing
                // it back onto the broadcaster — downstream consumers
                // shouldn't see arbitrary-length attacker-controlled
                // strings even within a scope-isolated event.
                let safe_kind: String = other.chars().take(64).collect();
                self.emit(
                    "media.tray.bridge.error",
                    json!({
                        "reason": "unknown_text_frame_kind",
                        "kind": safe_kind,
                    }),
                );
            },
        }
    }
}

impl Actor for TrayBridgeSession {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        info!("[TRAY-BRIDGE] connected {}", self.media_session_id);
        self.emit("media.tray.bridge.connected", json!({}));
        self.schedule_heartbeat(ctx);
    }

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        debug!("[TRAY-BRIDGE] stopped {}", self.media_session_id);
        self.emit("media.tray.bridge.disconnected", json!({}));
    }
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for TrayBridgeSession {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        match msg {
            Ok(ws::Message::Ping(payload)) => {
                self.last_heartbeat = Instant::now();
                ctx.pong(&payload);
            },
            Ok(ws::Message::Pong(_)) => {
                self.last_heartbeat = Instant::now();
            },
            Ok(ws::Message::Text(text)) => {
                self.last_heartbeat = Instant::now();
                self.dispatch_text(text.as_ref());
            },
            Ok(ws::Message::Binary(bytes)) => {
                self.last_heartbeat = Instant::now();
                // We can't tell screen frames from audio chunks from
                // bytes alone, so we emit a generic-size event and let
                // the per-frame metadata arrive on the next text
                // frame (which advertises the just-sent binary's
                // `kind`). Tray clients SHOULD send a `{kind: …,
                // chunk_id: …}` text frame just before each binary
                // frame to make this less ambiguous.
                self.emit("media.tray.frame.received", json!({ "bytes": bytes.len() }));
            },
            Ok(ws::Message::Close(reason)) => {
                debug!(
                    "[TRAY-BRIDGE] close {:?} for {}",
                    reason, self.media_session_id
                );
                ctx.stop();
            },
            Ok(ws::Message::Continuation(_)) | Ok(ws::Message::Nop) => {},
            Err(error) => {
                error!("[TRAY-BRIDGE] protocol error: {error}");
                self.emit(
                    "media.tray.bridge.error",
                    json!({ "reason": format!("{error}") }),
                );
                ctx.stop();
            },
        }
    }
}

/// Push a pointer command or overlay update from magician's runtime
/// down to the tray app.
#[derive(Message)]
#[rtype(result = "()")]
pub enum TrayDownstreamFrame {
    Text(String),
    Binary(Vec<u8>),
}

impl Handler<TrayDownstreamFrame> for TrayBridgeSession {
    type Result = ();
    fn handle(&mut self, msg: TrayDownstreamFrame, ctx: &mut Self::Context) -> Self::Result {
        match msg {
            TrayDownstreamFrame::Text(text) => ctx.text(text),
            TrayDownstreamFrame::Binary(bytes) => ctx.binary(bytes),
        }
    }
}

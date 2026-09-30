//! Authenticated outbound WebSocket admission for Magician Edge desktops.
//!
//! A desktop dials this endpoint on its selected Magician engine. The server
//! never opens an inbound connection to the machine. Authentication and scope
//! come from the existing paired-device credential boundary; the first text
//! frame is the shared [`runtime_core::edge::EdgeClientMessage::Hello`]. Socket
//! lifecycle and invocation correlation remain owned by `EdgeSessionRegistry`.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use actix::{Actor, ActorContext, AsyncContext, Handler, Message, StreamHandler};
use actix_web::{web, HttpMessage, HttpRequest, HttpResponse, Result};
use actix_web_actors::ws;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{debug, info, warn};

use magician::magician_v2::auth::middleware::bearer_token;
use magician::magician_v2::cloudflare_access::{
    VerifiedRequestAuthentication, VerifiedRequestIdentity,
};
use magician::magician_v2::device_pairing::{DevicePairingStore, MobileClientKind};
use magician::magician_v2::runtime::edge_sessions::{
    EdgeDispatchRequest, EdgeSessionError, EdgeSessionKey, EdgeSessionRegistry, EdgeSessionSink,
    EDGE_MAX_DISPATCH_TIMEOUT_MS, EDGE_MIN_DISPATCH_TIMEOUT_MS,
};
use runtime_core::edge::{
    EdgeClientMessage, EDGE_ABSOLUTE_MAX_PAYLOAD_BYTES, EDGE_MAX_IDENTIFIER_BYTES,
};

const HEADER_DEVICE_ID: &str = "X-Magician-Device-Id";
const LEGACY_HEADER_DEVICE_ID: &str = "X-Magdroid-Device";
const EDGE_HELLO_TIMEOUT: Duration = Duration::from_secs(15);
const EDGE_SOCKET_TICK: Duration = Duration::from_secs(30);
const EDGE_TRANSPORT_TIMEOUT: Duration = Duration::from_secs(100);
const MAX_PENDING_EDGE_HANDSHAKES: usize = 64;
/// The payload ceiling already counts encoded JSON values. Leave a small,
/// bounded allowance for the typed wire envelope around that value.
const EDGE_MAX_TEXT_FRAME_BYTES: usize = EDGE_ABSOLUTE_MAX_PAYLOAD_BYTES as usize + (64 * 1024);

#[derive(Message)]
#[rtype(result = "()")]
struct SendText(String);

#[derive(Message)]
#[rtype(result = "()")]
struct CloseSocket;

struct ActorSink {
    addr: actix::Addr<EdgeBridgeSession>,
}

impl EdgeSessionSink for ActorSink {
    fn send_text(&self, payload: String) -> bool {
        self.addr.try_send(SendText(payload)).is_ok()
    }

    fn close(&self) {
        // A revocation or replacement must not be dropped behind a full data
        // mailbox. `do_send` permits this one control message to be queued.
        self.addr.do_send(CloseSocket);
    }
}

#[derive(Clone)]
struct BoundSession {
    session_id: String,
    generation: u64,
}

pub struct EdgeBridgeSession {
    key: EdgeSessionKey,
    registry: Arc<EdgeSessionRegistry>,
    bound: Option<BoundSession>,
    last_transport_activity: Instant,
    pending_handshake: Option<OwnedSemaphorePermit>,
}

impl Actor for EdgeBridgeSession {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        info!(
            device_id = %self.key.device_id,
            principal = %self.key.principal,
            workspace = %self.key.workspace,
            "[EDGE] desktop socket awaiting hello"
        );
        ctx.run_later(EDGE_HELLO_TIMEOUT, |actor, ctx| {
            if actor.bound.is_none() {
                warn!(device_id = %actor.key.device_id, "[EDGE] hello timed out");
                ctx.close(None);
                ctx.stop();
            }
        });
        ctx.run_interval(EDGE_SOCKET_TICK, |actor, ctx| {
            actor.registry.expire_leases(Instant::now());
            if Instant::now().duration_since(actor.last_transport_activity) > EDGE_TRANSPORT_TIMEOUT
            {
                warn!(device_id = %actor.key.device_id, "[EDGE] transport went silent");
                ctx.close(None);
                ctx.stop();
                return;
            }
            ctx.ping(b"");
        });
    }

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        if let Some(bound) = &self.bound {
            self.registry
                .disconnect(&self.key, &bound.session_id, bound.generation);
        }
        debug!(device_id = %self.key.device_id, "[EDGE] desktop socket stopped");
    }
}

impl Handler<SendText> for EdgeBridgeSession {
    type Result = ();

    fn handle(&mut self, message: SendText, ctx: &mut Self::Context) {
        ctx.text(message.0);
    }
}

impl Handler<CloseSocket> for EdgeBridgeSession {
    type Result = ();

    fn handle(&mut self, _message: CloseSocket, ctx: &mut Self::Context) {
        ctx.close(None);
        ctx.stop();
    }
}

impl EdgeBridgeSession {
    fn receive_text(&mut self, text: &str, ctx: &mut ws::WebsocketContext<Self>) {
        let message = match serde_json::from_str::<EdgeClientMessage>(text) {
            Ok(message) => message,
            Err(error) => {
                warn!(device_id = %self.key.device_id, %error, "[EDGE] invalid client frame");
                ctx.close(None);
                ctx.stop();
                return;
            },
        };

        if self.bound.is_none() {
            let EdgeClientMessage::Hello(hello) = message else {
                warn!(device_id = %self.key.device_id, "[EDGE] first frame was not hello");
                ctx.close(None);
                ctx.stop();
                return;
            };
            let sink: Arc<dyn EdgeSessionSink> = Arc::new(ActorSink {
                addr: ctx.address(),
            });
            match self.registry.connect(
                self.key.clone(),
                hello,
                sink,
                chrono::Utc::now().timestamp_millis(),
                Instant::now(),
            ) {
                Ok(accepted) => {
                    self.bound = Some(BoundSession {
                        session_id: accepted.session_id,
                        generation: accepted.session_generation,
                    });
                    self.pending_handshake.take();
                    self.last_transport_activity = Instant::now();
                    info!(device_id = %self.key.device_id, "[EDGE] desktop session accepted");
                },
                Err(error) => {
                    warn!(device_id = %self.key.device_id, %error, "[EDGE] hello refused");
                    ctx.close(None);
                    ctx.stop();
                },
            }
            return;
        }

        match self
            .registry
            .receive_client_message(&self.key, message, Instant::now())
        {
            Ok(()) => self.last_transport_activity = Instant::now(),
            Err(error) => {
                warn!(device_id = %self.key.device_id, %error, "[EDGE] client frame refused");
                ctx.close(None);
                ctx.stop();
            },
        }
    }
}

impl StreamHandler<std::result::Result<ws::Message, ws::ProtocolError>> for EdgeBridgeSession {
    fn handle(
        &mut self,
        item: std::result::Result<ws::Message, ws::ProtocolError>,
        ctx: &mut Self::Context,
    ) {
        match item {
            Ok(ws::Message::Text(text)) => self.receive_text(&text, ctx),
            Ok(ws::Message::Ping(payload)) => {
                self.last_transport_activity = Instant::now();
                ctx.pong(&payload);
            },
            Ok(ws::Message::Pong(_)) => self.last_transport_activity = Instant::now(),
            Ok(ws::Message::Close(reason)) => {
                ctx.close(reason);
                ctx.stop();
            },
            Ok(ws::Message::Nop) => {},
            Ok(ws::Message::Binary(_)) | Ok(ws::Message::Continuation(_)) => {
                warn!(device_id = %self.key.device_id, "[EDGE] non-text frame refused");
                ctx.close(None);
                ctx.stop();
            },
            Err(error) => {
                debug!(device_id = %self.key.device_id, %error, "[EDGE] websocket ended");
                ctx.stop();
            },
        }
    }
}

fn device_header(req: &HttpRequest) -> Option<String> {
    req.headers()
        .get(HEADER_DEVICE_ID)
        .or_else(|| req.headers().get(LEGACY_HEADER_DEVICE_ID))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= EDGE_MAX_IDENTIFIER_BYTES
                && !value.chars().any(char::is_control)
        })
        .map(str::to_owned)
}

fn acquire_pending_handshake() -> Option<OwnedSemaphorePermit> {
    static PENDING: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Arc::clone(PENDING.get_or_init(|| Arc::new(Semaphore::new(MAX_PENDING_EDGE_HANDSHAKES))))
        .try_acquire_owned()
        .ok()
}

/// `GET /api/magician/v2/edge/bridge`
///
/// The paired desktop token supplies principal/workspace authority. The
/// device-id header is verified against that exact pairing before upgrade and
/// must also match the first Edge hello.
pub async fn edge_bridge_ws_handler(
    req: HttpRequest,
    stream: web::Payload,
    registry: web::Data<Arc<EdgeSessionRegistry>>,
    pairing: web::Data<Arc<DevicePairingStore>>,
) -> Result<HttpResponse> {
    let Some(device_id) = device_header(&req) else {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "edge_bridge_requires_device_id",
            "detail": format!("{HEADER_DEVICE_ID} is required"),
        })));
    };
    let Some(identity) = req.extensions().get::<VerifiedRequestIdentity>().cloned() else {
        return Ok(HttpResponse::Unauthorized().json(json!({
            "error": "edge_bridge_requires_verified_bearer",
        })));
    };
    if identity.authentication() != VerifiedRequestAuthentication::PairedDevice {
        return Ok(HttpResponse::Unauthorized().json(json!({
            "error": "edge_bridge_requires_paired_desktop",
        })));
    }
    let Some(workspace) = identity.workspace().map(str::to_owned) else {
        return Ok(HttpResponse::Unauthorized().json(json!({
            "error": "edge_bridge_requires_workspace_bound_bearer",
        })));
    };
    let principal = identity.principal().to_owned();
    if pairing
        .client_kind(&principal, &workspace, &device_id)
        .await
        != Some(MobileClientKind::Desktop)
    {
        return Ok(HttpResponse::Unauthorized().json(json!({
            "error": "edge_bridge_requires_desktop_credential",
        })));
    }
    let Some(token) = bearer_token(req.headers()) else {
        return Ok(HttpResponse::Unauthorized().json(json!({
            "error": "edge_bridge_requires_token",
        })));
    };
    let key = EdgeSessionKey::new(principal, workspace, device_id);
    let pairing_key = magician::magician_v2::device_bridge::DeviceKey::new(
        &key.principal,
        &key.workspace,
        &key.device_id,
    );
    if pairing
        .verify(&pairing_key, &token, chrono::Utc::now().timestamp_millis())
        .await
        .is_err()
    {
        return Ok(HttpResponse::Unauthorized().json(json!({
            "error": "edge_device_not_paired_or_token_rejected",
        })));
    }
    let Some(pending_handshake) = acquire_pending_handshake() else {
        return Ok(HttpResponse::TooManyRequests().json(json!({
            "error": "edge_bridge_handshake_capacity_reached",
        })));
    };

    let now = Instant::now();
    let session = EdgeBridgeSession {
        key,
        registry: registry.get_ref().clone(),
        bound: None,
        last_transport_activity: now,
        pending_handshake: Some(pending_handshake),
    };
    ws::WsResponseBuilder::new(session, &req, stream)
        .frame_size(EDGE_MAX_TEXT_FRAME_BYTES)
        .start()
}

fn default_dispatch_timeout_ms() -> u64 {
    30_000
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeInvokeRequest {
    pub execution_id: String,
    pub execution_epoch: u64,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    pub capability: String,
    pub operation: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(default = "default_dispatch_timeout_ms")]
    pub timeout_ms: u64,
}

fn dispatch_error_response(error: EdgeSessionError) -> HttpResponse {
    let (status, code) = match &error {
        EdgeSessionError::InvalidProtocol(_) | EdgeSessionError::GrantScopeMismatch => (
            actix_web::http::StatusCode::BAD_REQUEST,
            "edge_dispatch_invalid",
        ),
        EdgeSessionError::NotConnected(_) | EdgeSessionError::CapabilityUnavailable { .. } => (
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "edge_capability_unavailable",
        ),
        EdgeSessionError::InFlightLimit | EdgeSessionError::CapacityReached => (
            actix_web::http::StatusCode::TOO_MANY_REQUESTS,
            "edge_dispatch_capacity_reached",
        ),
        EdgeSessionError::Timeout(_) => (
            actix_web::http::StatusCode::GATEWAY_TIMEOUT,
            "edge_dispatch_timed_out",
        ),
        _ => (
            actix_web::http::StatusCode::BAD_GATEWAY,
            "edge_dispatch_failed",
        ),
    };
    HttpResponse::build(status).json(json!({
        "error": code,
        "detail": error.to_string(),
    }))
}

fn verified_owner_scope(req: &HttpRequest) -> std::result::Result<(String, String), HttpResponse> {
    let Some(identity) = req.extensions().get::<VerifiedRequestIdentity>().cloned() else {
        return Err(HttpResponse::Unauthorized().json(json!({
            "error": "edge_dispatch_requires_verified_identity",
        })));
    };
    if identity.authentication() == VerifiedRequestAuthentication::PairedDevice {
        return Err(HttpResponse::Forbidden().json(json!({
            "error": "edge_dispatch_refuses_device_credentials",
        })));
    }
    let asserted_principal = req
        .headers()
        .get("X-Principal")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let asserted_workspace = req
        .headers()
        .get("X-Workspace")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if asserted_principal.is_some_and(|principal| principal != identity.principal()) {
        return Err(HttpResponse::Forbidden().json(json!({
            "error": "edge_dispatch_scope_mismatch",
        })));
    }
    let workspace = identity.workspace().or(asserted_workspace);
    let Some(workspace) = workspace else {
        return Err(HttpResponse::BadRequest().json(json!({
            "error": "edge_dispatch_requires_scope",
        })));
    };
    if asserted_workspace.is_some_and(|asserted| asserted != workspace) {
        return Err(HttpResponse::Forbidden().json(json!({
            "error": "edge_dispatch_scope_mismatch",
        })));
    }
    Ok((identity.principal().to_owned(), workspace.to_owned()))
}

/// `GET /api/magician/v2/edge/devices`
///
/// Return only live Edge sessions in the authenticated scope. Capability
/// descriptors contain routing metadata and limits, never host URLs or secrets.
pub async fn edge_devices_handler(
    req: HttpRequest,
    registry: web::Data<Arc<EdgeSessionRegistry>>,
) -> Result<HttpResponse> {
    let (principal, workspace) = match verified_owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let devices = registry
        .snapshots_for_scope(&principal, &workspace, Instant::now())
        .into_iter()
        .map(|snapshot| {
            json!({
                "device_id": snapshot.key.device_id,
                "client_version": snapshot.client_version,
                "capabilities": snapshot.capabilities,
            })
        })
        .collect::<Vec<_>>();
    Ok(HttpResponse::Ok().json(json!({ "devices": devices })))
}

/// `POST /api/magician/v2/edge/devices/{device_id}/invoke`
///
/// Dispatch one typed operation through the current outbound desktop session.
/// Request middleware supplies the owner scope; paired-device credentials are
/// explicitly refused so one companion cannot turn another desktop into an
/// ambient proxy. The server mints all grant generations and byte ceilings.
pub async fn edge_invoke_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<EdgeInvokeRequest>,
    registry: web::Data<Arc<EdgeSessionRegistry>>,
    pairing: web::Data<Arc<DevicePairingStore>>,
) -> Result<HttpResponse> {
    let (principal, workspace) = match verified_owner_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let device_id = path.into_inner();
    if device_id.trim().is_empty()
        || device_id.len() > EDGE_MAX_IDENTIFIER_BYTES
        || device_id.chars().any(char::is_control)
    {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "edge_dispatch_invalid_device_id",
        })));
    }
    if pairing
        .client_kind(&principal, &workspace, &device_id)
        .await
        != Some(MobileClientKind::Desktop)
    {
        return Ok(HttpResponse::NotFound().json(json!({
            "error": "edge_desktop_not_enrolled",
        })));
    }
    if !(EDGE_MIN_DISPATCH_TIMEOUT_MS..=EDGE_MAX_DISPATCH_TIMEOUT_MS).contains(&body.timeout_ms) {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "edge_dispatch_timeout_out_of_range",
            "minimum_ms": EDGE_MIN_DISPATCH_TIMEOUT_MS,
            "maximum_ms": EDGE_MAX_DISPATCH_TIMEOUT_MS,
        })));
    }
    let idempotency_key = body
        .idempotency_key
        .clone()
        .unwrap_or_else(|| format!("{}:{}", body.execution_id, uuid::Uuid::new_v4()));
    let key = EdgeSessionKey::new(principal, workspace, device_id);
    let now_ms = chrono::Utc::now().timestamp_millis();
    info!(
        principal = %key.principal,
        workspace = %key.workspace,
        device_id = %key.device_id,
        execution_id = %body.execution_id,
        execution_epoch = body.execution_epoch,
        capability = %body.capability,
        operation = %body.operation,
        "[EDGE] dispatch admitted"
    );
    match registry
        .dispatch(
            &key,
            EdgeDispatchRequest {
                execution_id: body.execution_id.clone(),
                execution_epoch: body.execution_epoch,
                idempotency_key,
                capability: body.capability.clone(),
                operation: body.operation.clone(),
                payload: body.payload.clone(),
                timeout_ms: body.timeout_ms,
            },
            now_ms,
        )
        .await
    {
        Ok(result) => {
            info!(
                device_id = %key.device_id,
                execution_id = %body.execution_id,
                status = ?result.status,
                "[EDGE] dispatch settled"
            );
            Ok(HttpResponse::Ok().json(result))
        },
        Err(error) => {
            warn!(
                device_id = %key.device_id,
                execution_id = %body.execution_id,
                error = %error,
                "[EDGE] dispatch refused or failed"
            );
            Ok(dispatch_error_response(error))
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_dispatch_scope_comes_from_the_verified_bearer() {
        let request = actix_web::test::TestRequest::default().to_http_request();
        request
            .extensions_mut()
            .insert(VerifiedRequestIdentity::for_test(
                "owner",
                Some("default"),
                VerifiedRequestAuthentication::MagicianBearer,
            ));
        assert_eq!(
            verified_owner_scope(&request).unwrap(),
            ("owner".to_owned(), "default".to_owned())
        );
    }

    #[test]
    fn owner_dispatch_rejects_headers_that_conflict_with_the_verified_bearer() {
        let request = actix_web::test::TestRequest::default()
            .insert_header(("X-Workspace", "other"))
            .to_http_request();
        request
            .extensions_mut()
            .insert(VerifiedRequestIdentity::for_test(
                "owner",
                Some("default"),
                VerifiedRequestAuthentication::MagicianBearer,
            ));
        assert_eq!(
            verified_owner_scope(&request).unwrap_err().status(),
            actix_web::http::StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn edge_frame_ceiling_covers_the_protocol_payload_and_bounded_envelope() {
        assert_eq!(
            EDGE_MAX_TEXT_FRAME_BYTES,
            EDGE_ABSOLUTE_MAX_PAYLOAD_BYTES as usize + 64 * 1024
        );
    }

    #[test]
    fn device_ids_are_bounded_before_pairing_lookup() {
        let valid = actix_web::test::TestRequest::default()
            .insert_header((HEADER_DEVICE_ID, "office-linux"))
            .to_http_request();
        assert_eq!(device_header(&valid).as_deref(), Some("office-linux"));

        let oversized = actix_web::test::TestRequest::default()
            .insert_header((HEADER_DEVICE_ID, "x".repeat(EDGE_MAX_IDENTIFIER_BYTES + 1)))
            .to_http_request();
        assert!(device_header(&oversized).is_none());
    }

    #[test]
    fn hello_deadline_is_shorter_than_transport_and_registry_leases() {
        assert!(EDGE_HELLO_TIMEOUT < EDGE_TRANSPORT_TIMEOUT);
        assert!(EDGE_SOCKET_TICK < EDGE_TRANSPORT_TIMEOUT);
    }

    #[test]
    fn dispatch_errors_preserve_unavailable_and_capacity_classes() {
        assert_eq!(
            dispatch_error_response(EdgeSessionError::NotConnected("desktop-1".into())).status(),
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            dispatch_error_response(EdgeSessionError::InFlightLimit).status(),
            actix_web::http::StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            dispatch_error_response(EdgeSessionError::Timeout(Duration::from_secs(1))).status(),
            actix_web::http::StatusCode::GATEWAY_TIMEOUT
        );
    }

    #[test]
    fn dispatch_default_is_inside_the_shared_timeout_bounds() {
        assert!(
            (EDGE_MIN_DISPATCH_TIMEOUT_MS..=EDGE_MAX_DISPATCH_TIMEOUT_MS)
                .contains(&default_dispatch_timeout_ms())
        );
    }
}

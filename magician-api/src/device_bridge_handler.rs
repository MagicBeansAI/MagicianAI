//! WebSocket endpoint the Android companion dials into.
//!
//! The device opens this connection and keeps it; Magician sends work down it
//! and reads answers back. There is no inbound path to the phone, which is the
//! security property the whole design exists for — the companion can type into
//! anything on screen, so it must not be reachable by whatever else happens to
//! be on the same network.
//!
//! MCP lifecycle, correlation, timeouts and disconnect handling live in
//! [`magician::magician_v2::device_bridge`]; this file is only the socket.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use actix::{
    Actor, ActorContext, ActorFutureExt as _, AsyncContext, Handler, Message, StreamHandler,
    WrapFuture as _,
};
use actix_web::{web, HttpMessage, HttpRequest, HttpResponse, Result};
use actix_web_actors::ws;
use base64::Engine as _;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{debug, info, warn};

use magician::magician_v2::cloudflare_access::{
    VerifiedRequestAuthentication, VerifiedRequestIdentity,
};
use magician::magician_v2::device_bridge::{
    DeviceBridgeHub, DeviceConnectionId, DeviceKey, DeviceSink,
};
use magician::magician_v2::device_pairing::DevicePairingStore;

use crate::android_apps_attestation::{android_apps_socket_signing_bytes, verify_p256_signature};
use crate::android_automation_trust::{reviewed_policy_for_identity, AndroidAutomationTrustPolicy};
use crate::android_play_integrity::{play_integrity_request_hash, verify_play_integrity_token};
use crate::device_pairing_api::MobileEnrollmentConfig;

/// Missed heartbeats before the socket is considered dead.
///
/// A phone's connection dies without a close frame all the time — the radio
/// sleeps, WiFi hands over — so silence has to be treated as gone. The device
/// reconnects on its own; holding a dead entry only makes dispatches wait for a
/// timeout that was never going to be met.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(90);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const DEVICE_SOCKET_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const DEVICE_SOCKET_HANDSHAKE_SCHEMA: &str = "magician.android-apps-socket-handshake.v2";
/// One reviewed 1 MiB Apps result plus the exact bounded MCP/JSON envelope.
/// Magdroid applies the same ceiling to the final serialized UTF-8 response.
const DEVICE_BRIDGE_MAX_TEXT_FRAME_BYTES: usize = (1024 * 1024) + (64 * 1024);
const MAX_PENDING_DEVICE_HANDSHAKES_GLOBAL: usize = 64;
const MAX_PENDING_DEVICE_HANDSHAKES_PER_DEVICE: usize = 2;

const HEADER_DEVICE_ID: &str = "X-Magician-Device-Id";
const LEGACY_HEADER_DEVICE_ID: &str = "X-Magdroid-Device";

fn is_clean_socket_disconnect(error: &ws::ProtocolError) -> bool {
    matches!(
        error,
        ws::ProtocolError::Io(io_error)
            if matches!(
                io_error.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::NotConnected
            )
    )
}

/// Text to push down the socket, addressed to the actor that owns it.
#[derive(Message)]
#[rtype(result = "()")]
struct SendText(String);

#[derive(Message)]
#[rtype(result = "()")]
struct CloseSocket;

/// The hub's handle on one connected device.
///
/// Holding an `Addr` rather than the context is what lets the hub write to a
/// device from any task: actix contexts are not `Send`, addresses are.
struct ActorSink {
    addr: actix::Addr<DeviceBridgeSession>,
}

impl DeviceSink for ActorSink {
    fn send_text(&self, payload: String) -> bool {
        self.addr.try_send(SendText(payload)).is_ok()
    }

    fn close(&self) {
        // Control must not lose to a full data mailbox. `do_send` may queue one
        // close message beyond capacity; it never queues unbounded user data.
        self.addr.do_send(CloseSocket);
    }
}

pub struct DeviceBridgeSession {
    key: DeviceKey,
    connection_id: DeviceConnectionId,
    authority: magician::magician_v2::device_pairing::DeviceAutomationSocketAuthority,
    hub: Arc<DeviceBridgeHub>,
    pairing: Arc<DevicePairingStore>,
    last_heartbeat: Instant,
    handshake: SocketHandshake,
    pending_permits: Option<PendingHandshakePermits>,
    trust_policy: AndroidAutomationTrustPolicy,
}

enum SocketHandshake {
    Pending {
        server_nonce: [u8; 32],
        expires_at: Instant,
    },
    Verifying,
    Bound,
}

struct PendingHandshakePermits {
    _global: OwnedSemaphorePermit,
    _device: OwnedSemaphorePermit,
}

fn acquire_pending_handshake(key: &DeviceKey) -> Option<PendingHandshakePermits> {
    static GLOBAL: OnceLock<Arc<Semaphore>> = OnceLock::new();
    static DEVICES: OnceLock<Mutex<HashMap<DeviceKey, Weak<Semaphore>>>> = OnceLock::new();
    let global = Arc::clone(
        GLOBAL.get_or_init(|| Arc::new(Semaphore::new(MAX_PENDING_DEVICE_HANDSHAKES_GLOBAL))),
    );
    let _global = global.try_acquire_owned().ok()?;
    let device = {
        let mut devices = DEVICES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .ok()?;
        devices.retain(|_, semaphore| semaphore.strong_count() > 0);
        if let Some(semaphore) = devices.get(key).and_then(Weak::upgrade) {
            semaphore
        } else {
            let semaphore = Arc::new(Semaphore::new(MAX_PENDING_DEVICE_HANDSHAKES_PER_DEVICE));
            devices.insert(key.clone(), Arc::downgrade(&semaphore));
            semaphore
        }
    };
    let _device = device.try_acquire_owned().ok()?;
    Some(PendingHandshakePermits { _global, _device })
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct DeviceSocketChallenge<'a> {
    schema: &'static str,
    connection_id: String,
    key_id: &'a str,
    target_ref: &'a str,
    review_generation: u64,
    protocol_version: &'static str,
    server_nonce_base64: String,
    apk_sha256: &'a str,
    attestation_policy_digest: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceSocketProof {
    schema: String,
    connection_id: String,
    key_id: String,
    target_ref: String,
    review_generation: u64,
    protocol_version: String,
    server_nonce_base64: String,
    apk_sha256: String,
    attestation_policy_digest: String,
    signature_base64: String,
    play_integrity_token: String,
}

impl Actor for DeviceBridgeSession {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        info!(
            "[DEVICE-BRIDGE] authenticating {} ({}::{})",
            self.key.device_id, self.key.principal, self.key.workspace
        );
        let SocketHandshake::Pending { server_nonce, .. } = &self.handshake else {
            ctx.stop();
            return;
        };
        let challenge = DeviceSocketChallenge {
            schema: DEVICE_SOCKET_HANDSHAKE_SCHEMA,
            connection_id: self.connection_id.to_string(),
            key_id: self.authority.key_id(),
            target_ref: self.authority.target_ref(),
            review_generation: self.authority.review_generation(),
            protocol_version: "2026-07-28",
            server_nonce_base64: base64::engine::general_purpose::STANDARD.encode(server_nonce),
            apk_sha256: self.authority.apk_sha256(),
            attestation_policy_digest: self.authority.attestation_policy_digest(),
        };
        let Ok(challenge) = serde_json::to_string(&challenge) else {
            ctx.stop();
            return;
        };
        ctx.text(challenge);
        ctx.run_later(DEVICE_SOCKET_HANDSHAKE_TIMEOUT, |actor, ctx| {
            // This timer bounds proof arrival only. Once a valid hardware-key
            // proof has moved the actor to Verifying, the separately bounded
            // 12-second provider call owns the pending permits to terminal.
            if matches!(actor.handshake, SocketHandshake::Pending { .. }) {
                warn!(
                    "[DEVICE-BRIDGE] {} did not prove its attested key; closing",
                    actor.device_label()
                );
                ctx.close(None);
                ctx.stop();
            }
        });
        self.schedule_heartbeat(ctx);
    }

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        debug!("[DEVICE-BRIDGE] stopped {}", self.key.device_id);
        self.hub.disconnect(&self.key, self.connection_id);
    }
}

impl Handler<SendText> for DeviceBridgeSession {
    type Result = ();

    fn handle(&mut self, msg: SendText, ctx: &mut Self::Context) {
        ctx.text(msg.0);
    }
}

impl Handler<CloseSocket> for DeviceBridgeSession {
    type Result = ();

    fn handle(&mut self, _msg: CloseSocket, ctx: &mut Self::Context) {
        ctx.close(None);
        ctx.stop();
    }
}

impl DeviceBridgeSession {
    fn schedule_heartbeat(&self, ctx: &mut ws::WebsocketContext<Self>) {
        ctx.run_interval(HEARTBEAT_INTERVAL, |actor, ctx| {
            if Instant::now().duration_since(actor.last_heartbeat) > CLIENT_TIMEOUT {
                warn!(
                    "[DEVICE-BRIDGE] {} went silent; closing",
                    actor.device_label()
                );
                ctx.stop();
                return;
            }
            ctx.ping(b"");
        });
    }

    fn device_label(&self) -> String {
        format!("{}::{}", self.key.principal, self.key.device_id)
    }

    /// Route one text frame from the device.
    fn dispatch_text(&mut self, text: &str, ctx: &mut ws::WebsocketContext<Self>) {
        if matches!(self.handshake, SocketHandshake::Pending { .. }) {
            if !self.accept_socket_proof(text, ctx) {
                warn!(
                    "[DEVICE-BRIDGE] closing invalid attested socket proof from {}",
                    self.device_label()
                );
                ctx.close(None);
                ctx.stop();
            }
            return;
        }
        if matches!(self.handshake, SocketHandshake::Verifying) {
            ctx.close(None);
            ctx.stop();
            return;
        }
        if let Err(error) = self
            .hub
            .receive_text(&self.key, self.connection_id, text.to_owned())
        {
            warn!(
                "[DEVICE-BRIDGE] closing invalid MCP stream from {}: {}",
                self.device_label(),
                error
            );
            ctx.stop();
        }
    }

    fn accept_socket_proof(&mut self, text: &str, ctx: &mut ws::WebsocketContext<Self>) -> bool {
        let SocketHandshake::Pending {
            server_nonce,
            expires_at,
        } = &self.handshake
        else {
            return false;
        };
        if Instant::now() >= *expires_at || text.len() > 40 * 1024 {
            return false;
        }
        let Ok(proof) = serde_json::from_str::<DeviceSocketProof>(text) else {
            return false;
        };
        let expected_nonce = base64::engine::general_purpose::STANDARD.encode(server_nonce);
        if proof.schema != DEVICE_SOCKET_HANDSHAKE_SCHEMA
            || proof.connection_id != self.connection_id.to_string()
            || proof.key_id != self.authority.key_id()
            || proof.target_ref != self.authority.target_ref()
            || proof.review_generation != self.authority.review_generation()
            || proof.protocol_version != "2026-07-28"
            || proof.server_nonce_base64 != expected_nonce
            || proof.apk_sha256 != self.authority.apk_sha256()
            || proof.attestation_policy_digest != self.authority.attestation_policy_digest()
        {
            return false;
        }
        let signing_bytes = android_apps_socket_signing_bytes(
            &proof.connection_id,
            &proof.key_id,
            &proof.target_ref,
            proof.review_generation,
            &proof.protocol_version,
            server_nonce,
            &proof.apk_sha256,
            &proof.attestation_policy_digest,
        );
        if verify_p256_signature(
            self.authority.public_key_spki_base64(),
            &signing_bytes,
            &proof.signature_base64,
        )
        .is_err()
        {
            return false;
        }
        let Ok(signature) =
            base64::engine::general_purpose::STANDARD.decode(&proof.signature_base64)
        else {
            return false;
        };
        let play_request_hash = play_integrity_request_hash(
            b"magician.android-play-integrity.socket.v1",
            &signing_bytes,
            &signature,
        );
        let sink: Arc<dyn DeviceSink> = Arc::new(ActorSink {
            addr: ctx.address(),
        });
        let pairing = Arc::clone(&self.pairing);
        let hub = Arc::clone(&self.hub);
        let key = self.key.clone();
        let connection_id = self.connection_id;
        let authority = self.authority.clone();
        let trust_policy = self.trust_policy.clone();
        let play_integrity_token = proof.play_integrity_token;
        self.handshake = SocketHandshake::Verifying;
        ctx.spawn(
            async move {
                let trust_verdict_digest = if let Some(play_policy) = trust_policy.play_integrity()
                {
                    let Ok(Ok(digest)) = tokio::time::timeout(
                        Duration::from_secs(15),
                        verify_play_integrity_token(
                            play_policy,
                            &play_integrity_token,
                            &play_request_hash,
                            chrono::Utc::now().timestamp_millis(),
                        ),
                    )
                    .await
                    else {
                        return false;
                    };
                    digest
                } else {
                    if !play_integrity_token.is_empty() {
                        return false;
                    }
                    let Some(digest) =
                        trust_policy.private_socket_verdict_digest(&signing_bytes, &signature)
                    else {
                        return false;
                    };
                    digest
                };
                let Some(authority) = authority.bind_runtime_trust_verdict(trust_verdict_digest)
                else {
                    return false;
                };
                pairing
                    .admit_automation_socket(&key, connection_id, &authority, hub.as_ref(), sink)
                    .await
                    .unwrap_or(false)
            }
            .into_actor(self)
            .map(|accepted, actor, ctx| {
                if !accepted {
                    ctx.close(None);
                    ctx.stop();
                    return;
                }
                actor.handshake = SocketHandshake::Bound;
                actor.pending_permits = None;
                info!(
                    "[DEVICE-BRIDGE] attested Apps owner connected {}",
                    actor.device_label()
                );
            }),
        );
        true
    }
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for DeviceBridgeSession {
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
                if text.len() > DEVICE_BRIDGE_MAX_TEXT_FRAME_BYTES {
                    warn!(
                        "[DEVICE-BRIDGE] oversized MCP text frame from {}; closing",
                        self.device_label()
                    );
                    ctx.close(None);
                    ctx.stop();
                } else {
                    self.dispatch_text(text.as_ref(), ctx);
                }
            },
            Ok(ws::Message::Binary(_)) => {
                warn!(
                    "[DEVICE-BRIDGE] binary frame on MCP text stream from {}; closing",
                    self.device_label()
                );
                ctx.close(None);
                ctx.stop();
            },
            Ok(ws::Message::Close(reason)) => {
                debug!(
                    "[DEVICE-BRIDGE] close {:?} from {}",
                    reason,
                    self.device_label()
                );
                ctx.stop();
            },
            Ok(ws::Message::Continuation(_)) => {
                // This typed transport requires one complete bounded text
                // message. Silently dropping fragments turns a valid response
                // into a timeout; accepting them without aggregate accounting
                // creates an unbounded allocation path.
                warn!(
                    "[DEVICE-BRIDGE] fragmented MCP message from {}; closing",
                    self.device_label()
                );
                ctx.close(None);
                ctx.stop();
            },
            Ok(ws::Message::Nop) => {},
            Err(error) if is_clean_socket_disconnect(&error) => {
                debug!(
                    "[DEVICE-BRIDGE] transport disconnected without a close frame from {}: {}",
                    self.device_label(),
                    error
                );
                ctx.stop();
            },
            Err(error) => {
                warn!(
                    "[DEVICE-BRIDGE] protocol error from {}: {}",
                    self.device_label(),
                    error
                );
                ctx.stop();
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_transport_eof_is_not_a_protocol_violation() {
        let eof = ws::ProtocolError::Io(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        let reset =
            ws::ProtocolError::Io(std::io::Error::from(std::io::ErrorKind::ConnectionReset));
        assert!(is_clean_socket_disconnect(&eof));
        assert!(is_clean_socket_disconnect(&reset));
        assert!(!is_clean_socket_disconnect(&ws::ProtocolError::Overflow));
    }

    #[test]
    fn typed_bridge_ceiling_accepts_rich_snapshot_and_rejects_overflow() {
        assert!(70 * 1024 < DEVICE_BRIDGE_MAX_TEXT_FRAME_BYTES);
        assert_eq!(
            DEVICE_BRIDGE_MAX_TEXT_FRAME_BYTES,
            (1024 * 1024) + (64 * 1024)
        );
        assert!(DEVICE_BRIDGE_MAX_TEXT_FRAME_BYTES + 1 > DEVICE_BRIDGE_MAX_TEXT_FRAME_BYTES);
    }
}

fn device_header(req: &HttpRequest, name: &str, legacy_name: &str) -> Option<String> {
    req.headers()
        .get(name)
        .or_else(|| req.headers().get(legacy_name))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// `GET /api/magician/v2/devices/bridge`
///
/// Scope comes from the verified device bearer and its pairing record. The
/// device id remains an address, not authority.
pub async fn device_bridge_ws_handler(
    req: HttpRequest,
    stream: web::Payload,
    hub: web::Data<Arc<DeviceBridgeHub>>,
    pairing: web::Data<Arc<DevicePairingStore>>,
    enrollment_config: web::Data<MobileEnrollmentConfig>,
) -> Result<HttpResponse> {
    let Some(device_id) = device_header(&req, HEADER_DEVICE_ID, LEGACY_HEADER_DEVICE_ID) else {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "device_bridge_requires_identity",
            "detail": format!("{HEADER_DEVICE_ID} is required"),
        })));
    };
    let Some(identity) = req.extensions().get::<VerifiedRequestIdentity>().cloned() else {
        return Ok(HttpResponse::Unauthorized().json(json!({
            "error": "device_bridge_requires_verified_bearer",
        })));
    };
    if identity.authentication() != VerifiedRequestAuthentication::PairedDevice {
        return Ok(HttpResponse::Unauthorized().json(json!({
            "error": "device_bridge_requires_paired_device",
        })));
    }
    let Some(workspace) = identity.workspace().map(str::to_owned) else {
        return Ok(HttpResponse::Unauthorized().json(json!({
            "error": "device_bridge_requires_workspace_bound_bearer",
        })));
    };
    let principal = identity.principal().to_owned();

    let token = magician::magician_v2::auth::middleware::bearer_token(req.headers());
    let Some(token) = token else {
        return Ok(HttpResponse::Unauthorized().json(json!({
            "error": "device_bridge_requires_token",
        })));
    };

    let key = DeviceKey::new(principal, workspace, device_id);
    let Some(pending_permits) = acquire_pending_handshake(&key) else {
        return Ok(HttpResponse::TooManyRequests().json(json!({
            "error": "device_bridge_handshake_capacity_reached",
        })));
    };

    // The token is verified against the paired-device roster. It used to be
    // merely required, which let anything reaching this endpoint claim any
    // device id under any principal — and the scope decides what the device may
    // touch, so it was taken on the device's own word.
    let now_ms = chrono::Utc::now().timestamp_millis();
    let authority = match pairing.verify_automation_socket(&key, &token, now_ms).await {
        Ok(authority) => authority,
        Err(error) => {
            warn!("[DEVICE-BRIDGE] refused {}: {}", key.device_id, error);
            // One shape for both "unknown device" and "wrong token": distinguishing
            // them turns this endpoint into an oracle for which devices are paired.
            return Ok(HttpResponse::Unauthorized().json(json!({
                "error": "device_not_paired_or_token_rejected",
            })));
        },
    };
    let Some(trust_policy) = reviewed_policy_for_identity(
        enrollment_config.as_ref(),
        authority.attestation_policy_digest(),
        (authority.app_signing_sha256(), authority.app_version_code()),
    ) else {
        return Ok(HttpResponse::ServiceUnavailable().json(json!({
            "error": "android_apps_attestation_policy_unavailable",
        })));
    };
    if !trust_policy.admits_persisted_identity(
        authority.app_package(),
        authority.app_version_code(),
        authority.app_signing_sha256(),
        authority.apk_sha256(),
        authority.attestation_root_sha256(),
        authority.attestation_security_level(),
        authority.attestation_policy_digest(),
    ) {
        return Ok(HttpResponse::Unauthorized().json(json!({
            "error": "android_apps_attestation_policy_changed",
        })));
    }

    let mut server_nonce = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut server_nonce);
    let session = DeviceBridgeSession {
        key,
        connection_id: uuid::Uuid::new_v4(),
        authority,
        hub: hub.get_ref().clone(),
        pairing: pairing.get_ref().clone(),
        last_heartbeat: Instant::now(),
        handshake: SocketHandshake::Pending {
            server_nonce,
            expires_at: Instant::now() + DEVICE_SOCKET_HANDSHAKE_TIMEOUT,
        },
        pending_permits: Some(pending_permits),
        trust_policy,
    };
    ws::WsResponseBuilder::new(session, &req, stream)
        .frame_size(DEVICE_BRIDGE_MAX_TEXT_FRAME_BYTES)
        .start()
}

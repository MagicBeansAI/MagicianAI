//! Outbound Magician Edge connection owned by the desktop process.
//!
//! Enrollment reuses the server's paired-device authority and stores the
//! resulting desktop-only token in the OS credential store. The worker dials
//! only the currently selected remote engine; browser extensions and CUA stay
//! on loopback behind this process.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tokio::sync::{mpsc, Notify, RwLock};
use tokio::task::AbortHandle;
use tokio_tungstenite::tungstenite::{
    http::{HeaderName, HeaderValue},
    Message,
};

use crate::{edge_dispatch, magician_auth, AppState};
use runtime_core::edge::{
    EdgeCallError, EdgeCallResult, EdgeCallStatus, EdgeCapabilityDescriptor, EdgeClientMessage,
    EdgeHeartbeat, EdgeHello, EdgeInvoke, EdgeServerMessage, EdgeSessionAccepted,
    EDGE_PROTOCOL_VERSION,
};

const EDGE_PROFILE_SCHEMA: u8 = 1;
const EDGE_KEYRING_SERVICE: &str = "ai.magicbeans.magican.desktop.edge";
const EDGE_DEVICE_HEADER: &str = "x-magician-device-id";
const CF_ACCESS_CLIENT_ID_HEADER: &str = "cf-access-client-id";
const CF_ACCESS_CLIENT_SECRET_HEADER: &str = "cf-access-client-secret";
const EDGE_RECONNECT_MIN: Duration = Duration::from_secs(2);
const EDGE_RECONNECT_MAX: Duration = Duration::from_secs(60);
const EDGE_IDLE_POLL: Duration = Duration::from_secs(10);
const EDGE_CAPABILITY_REFRESH: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CloudflareCredential {
    client_id: String,
    client_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EdgeCredentialProfile {
    schema_version: u8,
    origin: String,
    device_id: String,
    label: String,
    principal: String,
    workspace: String,
    token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cloudflare_access: Option<CloudflareCredential>,
}

impl EdgeCredentialProfile {
    fn validate(&self, origin: &str) -> Result<(), String> {
        if self.schema_version != EDGE_PROFILE_SCHEMA
            || self.origin != origin
            || self.device_id.trim().is_empty()
            || self.device_id.len() > 160
            || self.label.trim().is_empty()
            || self.principal.trim().is_empty()
            || self.workspace.trim().is_empty()
            || self.token.trim().is_empty()
        {
            return Err("The stored Desktop Edge credential is invalid".into());
        }
        if self.cloudflare_access.as_ref().is_some_and(|credential| {
            credential.client_id.trim().is_empty() || credential.client_secret.trim().is_empty()
        }) {
            return Err("The stored Desktop Edge outer credential is invalid".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EdgeClientStatus {
    available: bool,
    enrolled: bool,
    connected: bool,
    server_origin: Option<String>,
    device_id: Option<String>,
    label: Option<String>,
    principal: Option<String>,
    workspace: Option<String>,
    error: Option<String>,
}

impl Default for EdgeClientStatus {
    fn default() -> Self {
        Self {
            available: false,
            enrolled: false,
            connected: false,
            server_origin: None,
            device_id: None,
            label: None,
            principal: None,
            workspace: None,
            error: None,
        }
    }
}

struct EdgeRuntime {
    status: RwLock<EdgeClientStatus>,
    changed: Notify,
}

struct CompletedCall {
    request_id: String,
    message: EdgeClientMessage,
}

#[derive(Default)]
struct ActiveCalls(HashMap<String, AbortHandle>);

impl ActiveCalls {
    fn insert(&mut self, request_id: String, abort: AbortHandle) {
        self.0.insert(request_id, abort);
    }

    fn remove(&mut self, request_id: &str) -> bool {
        self.0.remove(request_id).is_some()
    }

    fn cancel(&mut self, request_id: &str) {
        if let Some(abort) = self.0.remove(request_id) {
            abort.abort();
        }
    }

    fn abort_all(&mut self) {
        for (_, abort) in self.0.drain() {
            abort.abort();
        }
    }
}

impl Drop for ActiveCalls {
    fn drop(&mut self) {
        self.abort_all();
    }
}

static EDGE_RUNTIME: OnceLock<Arc<EdgeRuntime>> = OnceLock::new();
static OUTER_ACCESS: OnceLock<StdMutex<HashMap<String, CloudflareCredential>>> = OnceLock::new();

fn outer_access() -> &'static StdMutex<HashMap<String, CloudflareCredential>> {
    OUTER_ACCESS.get_or_init(|| StdMutex::new(HashMap::new()))
}

fn remember_outer_access(profile: &EdgeCredentialProfile) {
    let mut cached = outer_access()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match &profile.cloudflare_access {
        Some(credential) => {
            cached.insert(profile.origin.clone(), credential.clone());
        },
        None => {
            cached.remove(&profile.origin);
        },
    }
}

fn forget_outer_access(origin: &str) {
    outer_access()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(origin);
}

pub(crate) fn authorize_outer_http(
    request: reqwest::RequestBuilder,
    url: &reqwest::Url,
) -> reqwest::RequestBuilder {
    let origin = url.origin().ascii_serialization();
    let credential = outer_access()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&origin)
        .cloned();
    apply_cloudflare_http_headers(request, credential.as_ref())
}

pub(crate) fn authorize_outer_websocket(
    request: &mut tokio_tungstenite::tungstenite::http::Request<()>,
    url: &reqwest::Url,
) -> Result<(), String> {
    let mut origin = url.clone();
    match origin.scheme() {
        "ws" => origin
            .set_scheme("http")
            .map_err(|_| "The Magician WebSocket origin is invalid")?,
        "wss" => origin
            .set_scheme("https")
            .map_err(|_| "The Magician WebSocket origin is invalid")?,
        _ => return Err("The Magician WebSocket origin is invalid".to_string()),
    }
    let origin = origin.origin().ascii_serialization();
    let credential = outer_access()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&origin)
        .cloned();
    apply_cloudflare_headers(request, credential.as_ref())
}

fn runtime() -> Result<Arc<EdgeRuntime>, String> {
    EDGE_RUNTIME
        .get()
        .cloned()
        .ok_or_else(|| "Desktop Edge has not started".to_owned())
}

fn normalized_origin(origin: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(origin.trim())
        .map_err(|_| "The selected Magician server address is invalid")?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || !matches!(parsed.path(), "" | "/")
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err("The selected Magician server must be an HTTP(S) origin".into());
    }
    Ok(parsed.origin().ascii_serialization())
}

fn keyring_account(origin: &str) -> String {
    format!("origin:{}", blake3::hash(origin.as_bytes()).to_hex())
}

fn keyring_entry(origin: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(EDGE_KEYRING_SERVICE, &keyring_account(origin))
        .map_err(|_| "The Desktop Edge credential store is unavailable".to_owned())
}

async fn load_profile(origin: &str) -> Result<Option<EdgeCredentialProfile>, String> {
    let requested_origin = origin.to_owned();
    let lookup_origin = requested_origin.clone();
    let profile = tokio::task::spawn_blocking(move || {
        let entry = keyring_entry(&lookup_origin)?;
        let encoded = match entry.get_password() {
            Ok(value) => value,
            Err(keyring::Error::NoEntry) => return Ok(None),
            Err(_) => return Err("The Desktop Edge credential could not be read".to_owned()),
        };
        let profile: EdgeCredentialProfile = serde_json::from_str(&encoded)
            .map_err(|_| "The Desktop Edge credential is unreadable".to_owned())?;
        profile.validate(&lookup_origin)?;
        Ok(Some(profile))
    })
    .await
    .map_err(|_| "The Desktop Edge credential reader stopped".to_owned())??;
    match &profile {
        Some(profile) => remember_outer_access(profile),
        None => forget_outer_access(&requested_origin),
    }
    Ok(profile)
}

pub(crate) async fn hydrate_outer_access(origin: &str) -> Result<bool, String> {
    Ok(load_profile(origin).await?.is_some())
}

async fn save_profile(profile: EdgeCredentialProfile) -> Result<(), String> {
    profile.validate(&profile.origin)?;
    let cached = profile.clone();
    tokio::task::spawn_blocking(move || {
        let encoded = serde_json::to_string(&profile)
            .map_err(|_| "The Desktop Edge credential could not be encoded".to_owned())?;
        keyring_entry(&profile.origin)?
            .set_password(&encoded)
            .map_err(|_| "The Desktop Edge credential could not be saved".to_owned())
    })
    .await
    .map_err(|_| "The Desktop Edge credential writer stopped".to_owned())??;
    remember_outer_access(&cached);
    Ok(())
}

async fn delete_profile(origin: &str) -> Result<(), String> {
    let requested_origin = origin.to_owned();
    let delete_origin = requested_origin.clone();
    tokio::task::spawn_blocking(
        move || match keyring_entry(&delete_origin)?.delete_password() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err("The Desktop Edge credential could not be removed".to_owned()),
        },
    )
    .await
    .map_err(|_| "The Desktop Edge credential remover stopped".to_owned())??;
    forget_outer_access(&requested_origin);
    Ok(())
}

fn api_url(origin: &str, path: &str) -> String {
    format!("{}{}", origin.trim_end_matches('/'), path)
}

fn unix_now_ms() -> Result<i64, String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "The system clock is before the Unix epoch")?
        .as_millis();
    i64::try_from(millis).map_err(|_| "The system clock is outside the Edge range".to_owned())
}

fn edge_ws_url(origin: &str) -> Result<String, String> {
    let mut url = reqwest::Url::parse(&api_url(origin, "/api/magician/v2/edge/bridge"))
        .map_err(|_| "The selected Magician server address is invalid")?;
    match url.scheme() {
        "http" => url
            .set_scheme("ws")
            .map_err(|_| "The selected Magician WebSocket address is invalid")?,
        "https" => url
            .set_scheme("wss")
            .map_err(|_| "The selected Magician WebSocket address is invalid")?,
        _ => return Err("The selected Magician server must use HTTP(S)".into()),
    }
    Ok(url.to_string())
}

fn desktop_label() -> String {
    let host = std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .map(|value| value.trim().chars().take(96).collect::<String>())
        .filter(|value| !value.is_empty());
    host.map(|host| format!("{host} Desktop"))
        .unwrap_or_else(|| format!("Magican Desktop ({})", std::env::consts::OS))
}

#[derive(Serialize)]
struct ExchangeEnrollmentRequest<'a> {
    enrollment_id: &'a str,
    secret: &'a str,
    device_id: &'a str,
    label: &'a str,
}

#[derive(Deserialize)]
struct ExchangeEnrollmentResponse {
    device_id: String,
    label: String,
    token: String,
    principal: String,
    workspace: String,
    client_kind: String,
    cloudflare_access: Option<CloudflareCredential>,
}

fn enrollment_parts(uri: &str, expected_origin: &str) -> Result<(String, String), String> {
    let parsed = reqwest::Url::parse(uri)
        .map_err(|_| "The server returned an invalid Desktop Edge enrollment URI")?;
    if parsed.scheme() != "magican" || parsed.host_str() != Some("connect") {
        return Err("The server returned an invalid Desktop Edge enrollment URI".into());
    }
    let values = parsed
        .query_pairs()
        .collect::<std::collections::HashMap<_, _>>();
    let base = values
        .get("base")
        .map(|value| value.as_ref())
        .ok_or("The Desktop Edge enrollment URI has no server")?;
    if normalized_origin(base)? != expected_origin {
        return Err("The Desktop Edge enrollment server changed during setup".into());
    }
    if values.get("kind").map(|value| value.as_ref()) != Some("desktop") {
        return Err("The server did not mint a desktop enrollment".into());
    }
    let id = values
        .get("id")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or("The Desktop Edge enrollment URI has no id")?;
    let secret = values
        .get("secret")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or("The Desktop Edge enrollment URI has no secret")?;
    Ok((id, secret))
}

async fn enroll_selected_engine(
    app: &AppHandle,
    enrollment_uri: &str,
) -> Result<EdgeCredentialProfile, String> {
    let config = app.state::<AppState>().config.lock().await.clone();
    if !config.is_remote_engine() {
        return Err("Desktop Edge is available only for a selected remote Magician server".into());
    }
    let origin = normalized_origin(&config.engine_base_url())?;
    // An authenticated server page creates this one-time enrollment through
    // its normal browser session (including any outer Access cookie). Native
    // code receives only the server-issued URI and performs the public,
    // origin-bound exchange; it never imports browser cookies or passwords.
    let (enrollment_id, secret) = enrollment_parts(enrollment_uri, &origin)?;
    let device_id = format!("desktop-{}", uuid::Uuid::new_v4());
    let label = desktop_label();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| "Desktop Edge could not create its enrollment client")?;
    let exchange = client
        .post(api_url(
            &origin,
            "/api/magician/v2/devices/enrollment/exchange",
        ))
        .json(&ExchangeEnrollmentRequest {
            enrollment_id: &enrollment_id,
            secret: &secret,
            device_id: &device_id,
            label: &label,
        })
        .send()
        .await
        .map_err(|error| format!("Desktop Edge exchange could not reach the server: {error}"))?;
    if !exchange.status().is_success() {
        return Err(format!(
            "Desktop Edge credential exchange was refused (HTTP {})",
            exchange.status()
        ));
    }
    let exchange: ExchangeEnrollmentResponse = exchange
        .json()
        .await
        .map_err(|_| "The Desktop Edge credential response was invalid")?;
    if exchange.device_id != device_id
        || exchange.client_kind != "desktop"
        || exchange.token.trim().is_empty()
    {
        return Err("The server returned the wrong Desktop Edge credential".into());
    }
    let profile = EdgeCredentialProfile {
        schema_version: EDGE_PROFILE_SCHEMA,
        origin,
        device_id: exchange.device_id,
        label: exchange.label,
        principal: exchange.principal,
        workspace: exchange.workspace,
        token: exchange.token,
        cloudflare_access: exchange.cloudflare_access,
    };
    profile.validate(&profile.origin)?;
    save_profile(profile.clone()).await?;
    Ok(profile)
}

fn apply_cloudflare_headers(
    request: &mut tokio_tungstenite::tungstenite::http::Request<()>,
    credential: Option<&CloudflareCredential>,
) -> Result<(), String> {
    let Some(credential) = credential else {
        return Ok(());
    };
    for (name, value) in [
        (CF_ACCESS_CLIENT_ID_HEADER, credential.client_id.as_str()),
        (
            CF_ACCESS_CLIENT_SECRET_HEADER,
            credential.client_secret.as_str(),
        ),
    ] {
        request.headers_mut().insert(
            HeaderName::from_static(name),
            HeaderValue::from_str(value)
                .map_err(|_| "The Desktop Edge outer credential is invalid")?,
        );
    }
    Ok(())
}

fn apply_cloudflare_http_headers(
    request: reqwest::RequestBuilder,
    credential: Option<&CloudflareCredential>,
) -> reqwest::RequestBuilder {
    let Some(credential) = credential else {
        return request;
    };
    request
        .header(CF_ACCESS_CLIENT_ID_HEADER, &credential.client_id)
        .header(CF_ACCESS_CLIENT_SECRET_HEADER, &credential.client_secret)
}

fn bounded_error_message(message: impl Into<String>) -> String {
    let message = message.into();
    if message.len() <= 4 * 1024 {
        return message;
    }
    let mut end = 4 * 1024;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message[..end].to_owned()
}

fn call_error_result(
    accepted: &EdgeSessionAccepted,
    request_id: String,
    status: EdgeCallStatus,
    code: &str,
    message: impl Into<String>,
    retryable: bool,
) -> EdgeClientMessage {
    EdgeClientMessage::Result(EdgeCallResult {
        session_id: accepted.session_id.clone(),
        session_generation: accepted.session_generation,
        request_id,
        status,
        payload: None,
        error: Some(EdgeCallError {
            code: code.to_owned(),
            message: bounded_error_message(message),
            retryable,
        }),
    })
}

fn validate_invoke_authority(
    invoke: &EdgeInvoke,
    accepted: &EdgeSessionAccepted,
    profile: &EdgeCredentialProfile,
    capabilities: &[EdgeCapabilityDescriptor],
) -> Result<(), String> {
    if invoke.session_id != accepted.session_id
        || invoke.session_generation != accepted.session_generation
    {
        return Err("Desktop Edge invocation names a stale session".into());
    }
    if invoke.grant.workspace_id != profile.workspace || invoke.grant.device_id != profile.device_id
    {
        return Err("Desktop Edge invocation grant names a different device scope".into());
    }
    let descriptor = capabilities
        .iter()
        .find(|descriptor| descriptor.supports(&invoke.capability, &invoke.operation))
        .ok_or("Desktop Edge invocation requests an unadvertised capability")?;
    if descriptor.generation != invoke.grant.capability_generation {
        return Err("Desktop Edge invocation uses a stale capability generation".into());
    }
    if invoke.grant.max_request_bytes > descriptor.max_request_bytes
        || invoke.grant.max_response_bytes > descriptor.max_response_bytes
    {
        return Err("Desktop Edge invocation grant exceeds advertised limits".into());
    }
    Ok(())
}

async fn dispatch_call(
    app: AppHandle,
    accepted: EdgeSessionAccepted,
    invoke: EdgeInvoke,
) -> CompletedCall {
    let request_id = invoke.request_id.clone();
    let now_ms = unix_now_ms().unwrap_or(invoke.deadline_at_ms);
    let wait_ms = invoke.deadline_at_ms.saturating_sub(now_ms).max(1) as u64;
    let capability = invoke.capability.clone();
    let operation = invoke.operation.clone();
    let max_response_bytes = invoke.grant.max_response_bytes;
    let result = tokio::time::timeout(
        Duration::from_millis(wait_ms),
        edge_dispatch::invoke(app, &capability, &operation, invoke.payload),
    )
    .await;
    let message = match result {
        Ok(Ok(payload)) => {
            let candidate = EdgeCallResult {
                session_id: accepted.session_id.clone(),
                session_generation: accepted.session_generation,
                request_id: request_id.clone(),
                status: EdgeCallStatus::Succeeded,
                payload: Some(payload),
                error: None,
            };
            if candidate.validate(max_response_bytes).is_ok() {
                EdgeClientMessage::Result(candidate)
            } else {
                call_error_result(
                    &accepted,
                    request_id.clone(),
                    EdgeCallStatus::Failed,
                    "capability_response_too_large",
                    "Desktop capability result exceeded its execution grant",
                    false,
                )
            }
        },
        Ok(Err(error)) => {
            let status = match error.kind {
                edge_dispatch::EdgeDispatchFailureKind::Failed => EdgeCallStatus::Failed,
                edge_dispatch::EdgeDispatchFailureKind::Unavailable => EdgeCallStatus::Unavailable,
            };
            call_error_result(
                &accepted,
                request_id.clone(),
                status,
                error.code,
                error.message,
                error.retryable,
            )
        },
        Err(_) => call_error_result(
            &accepted,
            request_id.clone(),
            EdgeCallStatus::Cancelled,
            "capability_deadline_expired",
            "Desktop capability call exceeded its deadline",
            true,
        ),
    };
    CompletedCall {
        request_id,
        message,
    }
}

async fn connect_profile(
    app: &AppHandle,
    runtime: &Arc<EdgeRuntime>,
    profile: &EdgeCredentialProfile,
) -> Result<(), String> {
    let url = edge_ws_url(&profile.origin)?;
    let mut capability_generation = 1_u64;
    let mut capabilities = edge_dispatch::discover_capabilities(app, capability_generation).await;
    let mut request = magician_auth::websocket_request_with_bearer(&url, Some(&profile.token))?;
    request.headers_mut().insert(
        HeaderName::from_static(EDGE_DEVICE_HEADER),
        HeaderValue::from_str(&profile.device_id)
            .map_err(|_| "The Desktop Edge device id is invalid")?,
    );
    apply_cloudflare_headers(&mut request, profile.cloudflare_access.as_ref())?;
    let (mut socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|error| format!("Desktop Edge connection failed: {error}"))?;
    let hello = EdgeClientMessage::Hello(EdgeHello {
        protocol_version: EDGE_PROTOCOL_VERSION,
        device_id: profile.device_id.clone(),
        workspace_id: profile.workspace.clone(),
        client_version: env!("CARGO_PKG_VERSION").to_owned(),
        client_instance_id: uuid::Uuid::new_v4().to_string(),
        capabilities: capabilities.clone(),
    });
    socket
        .send(Message::Text(
            serde_json::to_string(&hello).map_err(|_| "Desktop Edge hello could not be encoded")?,
        ))
        .await
        .map_err(|error| format!("Desktop Edge hello could not be sent: {error}"))?;

    let accepted = tokio::time::timeout(Duration::from_secs(20), socket.next())
        .await
        .map_err(|_| "Desktop Edge server did not accept the session")?
        .ok_or("Desktop Edge server closed during admission")?
        .map_err(|error| format!("Desktop Edge admission failed: {error}"))?;
    let Message::Text(accepted) = accepted else {
        return Err("Desktop Edge server returned a non-text admission frame".into());
    };
    let EdgeServerMessage::SessionAccepted(accepted) = serde_json::from_str(&accepted)
        .map_err(|_| "Desktop Edge server returned an invalid admission frame")?
    else {
        return Err("Desktop Edge server invoked a capability before admission".into());
    };
    accepted
        .validate()
        .map_err(|error| format!("Desktop Edge admission was invalid: {error}"))?;

    runtime
        .set_status(EdgeClientStatus {
            available: true,
            enrolled: true,
            connected: true,
            server_origin: Some(profile.origin.clone()),
            device_id: Some(profile.device_id.clone()),
            label: Some(profile.label.clone()),
            principal: Some(profile.principal.clone()),
            workspace: Some(profile.workspace.clone()),
            error: None,
        })
        .await;

    let mut heartbeat =
        tokio::time::interval(Duration::from_millis(accepted.heartbeat_interval_ms));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.tick().await;
    let mut selection_check = tokio::time::interval(Duration::from_secs(5));
    selection_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    selection_check.tick().await;
    let mut capability_check = tokio::time::interval(EDGE_CAPABILITY_REFRESH);
    capability_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    capability_check.tick().await;
    let (completed_tx, mut completed_rx) = mpsc::unbounded_channel::<CompletedCall>();
    let mut active_calls = ActiveCalls::default();

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                let heartbeat = EdgeClientMessage::Heartbeat(EdgeHeartbeat {
                    session_id: accepted.session_id.clone(),
                    session_generation: accepted.session_generation,
                    sent_at_ms: unix_now_ms()?,
                });
                socket.send(Message::Text(serde_json::to_string(&heartbeat)
                    .map_err(|_| "Desktop Edge heartbeat could not be encoded")?))
                    .await
                    .map_err(|error| format!("Desktop Edge heartbeat failed: {error}"))?;
            },
            _ = selection_check.tick() => {
                let config = app.state::<AppState>().config.lock().await.clone();
                let session_present = magician_auth::process_connection_auth()
                    .ok()
                    .and_then(|(_, token, _)| token)
                    .is_some_and(|token| !token.trim().is_empty());
                if !config.is_remote_engine()
                    || normalized_origin(&config.engine_base_url()).ok().as_deref() != Some(profile.origin.as_str())
                    || !session_present
                {
                    let _ = socket.close(None).await;
                    return Ok(());
                }
            },
            _ = capability_check.tick() => {
                let mut probed = edge_dispatch::discover_capabilities(app, capability_generation).await;
                if probed != capabilities {
                    capability_generation = capability_generation.checked_add(1)
                        .ok_or("Desktop Edge capability generation exhausted")?;
                    for descriptor in &mut probed {
                        descriptor.generation = capability_generation;
                    }
                    capabilities = probed;
                    // Every active grant names the previous generation. Stop
                    // local work before publishing the new manifest; the
                    // server invalidates its matching pending calls atomically.
                    active_calls.abort_all();
                    let update = EdgeClientMessage::CapabilityUpdate(
                        runtime_core::edge::EdgeCapabilityUpdate {
                            session_id: accepted.session_id.clone(),
                            session_generation: accepted.session_generation,
                            capabilities: capabilities.clone(),
                        }
                    );
                    socket.send(Message::Text(serde_json::to_string(&update)
                        .map_err(|_| "Desktop Edge capability update could not be encoded")?))
                        .await
                        .map_err(|error| format!("Desktop Edge capability update failed: {error}"))?;
                }
            },
            _ = runtime.changed.notified() => {
                let _ = socket.close(None).await;
                return Ok(());
            },
            completed = completed_rx.recv() => {
                let completed = completed.ok_or("Desktop Edge dispatcher stopped")?;
                // Cancellation and capability rotation remove the abort handle
                // first. A completion already queued in that race is stale and
                // must not make the server reject the now-unknown request id.
                if !active_calls.remove(&completed.request_id) {
                    continue;
                }
                socket.send(Message::Text(serde_json::to_string(&completed.message)
                    .map_err(|_| "Desktop Edge result could not be encoded")?))
                    .await
                    .map_err(|error| format!("Desktop Edge result failed: {error}"))?;
            },
            incoming = socket.next() => {
                let incoming = incoming
                    .ok_or("Desktop Edge server closed the connection")?
                    .map_err(|error| format!("Desktop Edge socket failed: {error}"))?;
                match incoming {
                    Message::Text(text) => {
                        match serde_json::from_str::<EdgeServerMessage>(&text)
                            .map_err(|_| "Desktop Edge received an invalid server frame")?
                        {
                            EdgeServerMessage::SessionAccepted(_) => {
                                return Err("Desktop Edge received a duplicate admission".into());
                            },
                            EdgeServerMessage::Invoke(invoke) => {
                                invoke.validate(unix_now_ms()?)
                                    .map_err(|error| format!("Desktop Edge received an invalid invocation: {error}"))?;
                                validate_invoke_authority(&invoke, &accepted, profile, &capabilities)?;
                                let request_id = invoke.request_id.clone();
                                if active_calls.0.contains_key(&request_id) {
                                    return Err("Desktop Edge received a duplicate active request".into());
                                }
                                let app = app.clone();
                                let accepted = accepted.clone();
                                let completed_tx = completed_tx.clone();
                                let task = tokio::spawn(async move {
                                    let completed = dispatch_call(app, accepted, invoke).await;
                                    let _ = completed_tx.send(completed);
                                });
                                active_calls.insert(request_id, task.abort_handle());
                            },
                            EdgeServerMessage::Cancel(cancel) => {
                                cancel.validate()
                                    .map_err(|error| format!("Desktop Edge received an invalid cancellation: {error}"))?;
                                if cancel.session_id != accepted.session_id
                                    || cancel.session_generation != accepted.session_generation
                                {
                                    return Err("Desktop Edge received a cancellation for a stale session".into());
                                }
                                active_calls.cancel(&cancel.request_id);
                            },
                        }
                    },
                    Message::Ping(payload) => socket.send(Message::Pong(payload)).await
                        .map_err(|error| format!("Desktop Edge pong failed: {error}"))?,
                    Message::Pong(_) => {},
                    Message::Close(_) => return Ok(()),
                    Message::Binary(_) | Message::Frame(_) => {
                        return Err("Desktop Edge received a non-text data frame".into());
                    },
                }
            },
        }
    }
}

impl EdgeRuntime {
    async fn set_status(&self, status: EdgeClientStatus) {
        *self.status.write().await = status;
    }

    async fn current_status(&self) -> EdgeClientStatus {
        self.status.read().await.clone()
    }
}

async fn worker(app: AppHandle, runtime: Arc<EdgeRuntime>) {
    let mut backoff = EDGE_RECONNECT_MIN;
    loop {
        let config = app.state::<AppState>().config.lock().await.clone();
        if !config.is_remote_engine() {
            runtime.set_status(EdgeClientStatus::default()).await;
            tokio::select! {
                _ = tokio::time::sleep(EDGE_IDLE_POLL) => {},
                _ = runtime.changed.notified() => {},
            }
            backoff = EDGE_RECONNECT_MIN;
            continue;
        }
        let origin = match normalized_origin(&config.engine_base_url()) {
            Ok(origin) => origin,
            Err(error) => {
                runtime
                    .set_status(EdgeClientStatus {
                        available: false,
                        server_origin: Some(config.engine_base_url()),
                        error: Some(error),
                        ..EdgeClientStatus::default()
                    })
                    .await;
                tokio::time::sleep(EDGE_IDLE_POLL).await;
                continue;
            },
        };
        let profile = match load_profile(&origin).await {
            Ok(Some(profile)) => profile,
            Ok(None) => {
                runtime
                    .set_status(EdgeClientStatus {
                        available: true,
                        server_origin: Some(origin),
                        ..EdgeClientStatus::default()
                    })
                    .await;
                tokio::select! {
                    _ = tokio::time::sleep(EDGE_IDLE_POLL) => {},
                    _ = runtime.changed.notified() => {},
                }
                backoff = EDGE_RECONNECT_MIN;
                continue;
            },
            Err(error) => {
                runtime
                    .set_status(EdgeClientStatus {
                        available: false,
                        server_origin: Some(origin),
                        error: Some(error),
                        ..EdgeClientStatus::default()
                    })
                    .await;
                tokio::time::sleep(EDGE_IDLE_POLL).await;
                continue;
            },
        };
        runtime
            .set_status(EdgeClientStatus {
                available: true,
                enrolled: true,
                server_origin: Some(profile.origin.clone()),
                device_id: Some(profile.device_id.clone()),
                label: Some(profile.label.clone()),
                principal: Some(profile.principal.clone()),
                workspace: Some(profile.workspace.clone()),
                ..EdgeClientStatus::default()
            })
            .await;
        match connect_profile(&app, &runtime, &profile).await {
            Ok(()) => backoff = EDGE_RECONNECT_MIN,
            Err(error) => {
                runtime
                    .set_status(EdgeClientStatus {
                        available: true,
                        enrolled: true,
                        server_origin: Some(profile.origin.clone()),
                        device_id: Some(profile.device_id.clone()),
                        label: Some(profile.label.clone()),
                        principal: Some(profile.principal.clone()),
                        workspace: Some(profile.workspace.clone()),
                        error: Some(error),
                        ..EdgeClientStatus::default()
                    })
                    .await;
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {},
                    _ = runtime.changed.notified() => {},
                }
                backoff = backoff.saturating_mul(2).min(EDGE_RECONNECT_MAX);
            },
        }
    }
}

pub(crate) fn start(app: AppHandle) {
    let runtime = Arc::new(EdgeRuntime {
        status: RwLock::new(EdgeClientStatus::default()),
        changed: Notify::new(),
    });
    if EDGE_RUNTIME.set(runtime.clone()).is_err() {
        return;
    }
    tauri::async_runtime::spawn(worker(app, runtime));
}

#[tauri::command]
pub(crate) async fn get_edge_client_status() -> Result<EdgeClientStatus, String> {
    Ok(runtime()?.status.read().await.clone())
}

#[tauri::command]
pub(crate) async fn enroll_edge_client(
    app: AppHandle,
    enrollment_uri: String,
) -> Result<EdgeClientStatus, String> {
    enroll_edge_client_uri(&app, &enrollment_uri).await
}

pub(crate) async fn enroll_edge_client_uri(
    app: &AppHandle,
    enrollment_uri: &str,
) -> Result<EdgeClientStatus, String> {
    let profile = enroll_selected_engine(app, enrollment_uri).await?;
    let status = EdgeClientStatus {
        available: true,
        enrolled: true,
        server_origin: Some(profile.origin.clone()),
        device_id: Some(profile.device_id.clone()),
        label: Some(profile.label.clone()),
        principal: Some(profile.principal.clone()),
        workspace: Some(profile.workspace.clone()),
        ..EdgeClientStatus::default()
    };
    // A cold-start deep link is delivered before async_setup starts the Edge
    // worker. The credential is already durable, so let that worker discover
    // it normally instead of rejecting an otherwise valid enrollment.
    let Ok(runtime) = runtime() else {
        return Ok(status);
    };
    runtime.set_status(status).await;
    runtime.changed.notify_one();
    Ok(runtime.current_status().await)
}

#[tauri::command]
pub(crate) async fn revoke_edge_client(app: AppHandle) -> Result<EdgeClientStatus, String> {
    let runtime = runtime()?;
    let config = app.state::<AppState>().config.lock().await.clone();
    let origin = normalized_origin(&config.engine_base_url())?;
    let Some(profile) = load_profile(&origin).await? else {
        runtime.changed.notify_one();
        return Ok(runtime.current_status().await);
    };
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| "Desktop Edge could not create its revocation client")?;
    let mut revoke_url = reqwest::Url::parse(&api_url(&origin, "/api/magician/v2/devices/"))
        .map_err(|_| "The selected Magician server address is invalid")?;
    revoke_url
        .path_segments_mut()
        .map_err(|_| "The selected Magician server address is invalid")?
        .push(&profile.device_id);
    let response = apply_cloudflare_http_headers(
        magician_auth::authorize(client.delete(revoke_url)),
        profile.cloudflare_access.as_ref(),
    )
    .send()
    .await
    .map_err(|error| format!("Desktop Edge revocation could not reach the server: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Desktop Edge revocation was refused (HTTP {})",
            response.status()
        ));
    }
    delete_profile(&origin).await?;
    runtime
        .set_status(EdgeClientStatus {
            available: true,
            server_origin: Some(origin),
            ..EdgeClientStatus::default()
        })
        .await;
    runtime.changed.notify_one();
    Ok(runtime.current_status().await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_websocket_url_tracks_the_selected_http_origin() {
        assert_eq!(
            edge_ws_url("https://connect.example.test").unwrap(),
            "wss://connect.example.test/api/magician/v2/edge/bridge"
        );
        assert_eq!(
            edge_ws_url("http://127.0.0.1:13002").unwrap(),
            "ws://127.0.0.1:13002/api/magician/v2/edge/bridge"
        );
    }

    #[test]
    fn desktop_enrollment_uri_is_origin_and_kind_bound() {
        let uri = "magican://connect?base=https%3A%2F%2Fconnect.example.test&id=enroll-1&secret=secret-1&kind=desktop";
        assert_eq!(
            enrollment_parts(uri, "https://connect.example.test").unwrap(),
            ("enroll-1".to_owned(), "secret-1".to_owned())
        );
        assert!(enrollment_parts(uri, "https://other.example.test").is_err());
        assert!(enrollment_parts(
            "magican://connect?base=https%3A%2F%2Fconnect.example.test&id=e&secret=s&kind=ios",
            "https://connect.example.test"
        )
        .is_err());
    }
}

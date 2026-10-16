//! Local dispatch for capabilities advertised over Magician Edge.
//!
//! The remote engine receives typed operations, never a loopback URL or host
//! credential. Dispatch stays inside the desktop process and reuses the same
//! bounded host adapters used by a local/container Magician runtime.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tokio::process::Command;
use tokio_tungstenite::tungstenite::Message;

use crate::AppState;
use runtime_core::edge::{
    EdgeCapabilityDescriptor, EDGE_ANDROID_OBSERVATION_OPERATIONS, EDGE_BROWSER_OPERATIONS,
    EDGE_CAPABILITY_ANDROID_OBSERVATION, EDGE_CAPABILITY_BROWSER_CDP, EDGE_CAPABILITY_CUA,
    EDGE_CAPABILITY_IMESSAGE, EDGE_IMESSAGE_OPERATIONS,
};

const CUA_MAX_REQUEST_BYTES: u64 = 256 * 1024;
const CUA_MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const BROWSER_MAX_REQUEST_BYTES: u64 = 1024 * 1024;
const BROWSER_MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const IMESSAGE_MAX_REQUEST_BYTES: u64 = 64 * 1024;
const IMESSAGE_MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
const ANDROID_OBSERVATION_MAX_REQUEST_BYTES: u64 = 4 * 1024;
const ANDROID_OBSERVATION_MAX_RESPONSE_BYTES: u64 = 4 * 1024;
const LOCAL_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const LOCAL_BROWSER_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EdgeDispatchFailureKind {
    Failed,
    Unavailable,
}

#[derive(Debug)]
pub(crate) struct EdgeDispatchError {
    pub kind: EdgeDispatchFailureKind,
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl EdgeDispatchError {
    fn failed(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind: EdgeDispatchFailureKind::Failed,
            code,
            message: message.into(),
            retryable: false,
        }
    }

    fn unavailable(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind: EdgeDispatchFailureKind::Unavailable,
            code,
            message: message.into(),
            retryable: true,
        }
    }
}

fn descriptor(
    capability: &str,
    generation: u64,
    operations: Vec<String>,
    max_request_bytes: u64,
    max_response_bytes: u64,
    max_in_flight: u16,
) -> EdgeCapabilityDescriptor {
    EdgeCapabilityDescriptor {
        capability: capability.to_owned(),
        generation,
        operations,
        max_request_bytes,
        max_response_bytes,
        max_in_flight,
    }
}

fn descriptors_from_availability(
    generation: u64,
    cua_operations: Vec<String>,
    browser_available: bool,
    imessage_available: bool,
    android_observation_available: bool,
) -> Vec<EdgeCapabilityDescriptor> {
    let mut capabilities = Vec::new();
    if !cua_operations.is_empty() {
        capabilities.push(descriptor(
            EDGE_CAPABILITY_CUA,
            generation,
            cua_operations,
            CUA_MAX_REQUEST_BYTES,
            CUA_MAX_RESPONSE_BYTES,
            1,
        ));
    }
    if browser_available {
        capabilities.push(descriptor(
            EDGE_CAPABILITY_BROWSER_CDP,
            generation,
            EDGE_BROWSER_OPERATIONS
                .iter()
                .map(|operation| (*operation).to_owned())
                .collect(),
            BROWSER_MAX_REQUEST_BYTES,
            BROWSER_MAX_RESPONSE_BYTES,
            8,
        ));
    }
    if imessage_available {
        capabilities.push(descriptor(
            EDGE_CAPABILITY_IMESSAGE,
            generation,
            EDGE_IMESSAGE_OPERATIONS
                .iter()
                .map(|operation| (*operation).to_owned())
                .collect(),
            IMESSAGE_MAX_REQUEST_BYTES,
            IMESSAGE_MAX_RESPONSE_BYTES,
            2,
        ));
    }
    if android_observation_available {
        capabilities.push(descriptor(
            EDGE_CAPABILITY_ANDROID_OBSERVATION,
            generation,
            EDGE_ANDROID_OBSERVATION_OPERATIONS
                .iter()
                .map(|operation| (*operation).to_owned())
                .collect(),
            ANDROID_OBSERVATION_MAX_REQUEST_BYTES,
            ANDROID_OBSERVATION_MAX_RESPONSE_BYTES,
            1,
        ));
    }
    capabilities
}

async fn cua_operations() -> Vec<String> {
    if !runtime_core::cua::has_desktop_session() {
        return Vec::new();
    }
    let Some(binary) = runtime_core::cua::driver_binary() else {
        return Vec::new();
    };
    let output = match tokio::time::timeout(
        LOCAL_PROBE_TIMEOUT,
        Command::new(binary)
            .arg("list-tools")
            .kill_on_drop(true)
            .output(),
    )
    .await
    {
        Ok(Ok(output)) if output.status.success() => output,
        _ => return Vec::new(),
    };
    let mut operations = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once(':').map(|(name, _)| name.trim()))
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 160
                && name.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'.' | b'_' | b'-')
                })
        })
        .map(str::to_owned)
        .take(64)
        .collect::<Vec<_>>();
    operations.sort();
    operations.dedup();
    operations
}

async fn browser_available(port: u16) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(LOCAL_PROBE_TIMEOUT)
        .build()
    else {
        return false;
    };
    let Ok(response) = client
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .await
    else {
        return false;
    };
    if !response.status().is_success() {
        return false;
    }
    response
        .json::<Value>()
        .await
        .ok()
        .and_then(|health| health.get("extension_connected").and_then(Value::as_bool))
        .unwrap_or(false)
}

fn imessage_available() -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    dirs::home_dir()
        .map(|home| home.join("Library/Messages/chat.db").is_file())
        .unwrap_or(false)
}

pub(crate) async fn discover_capabilities(
    app: &AppHandle,
    generation: u64,
) -> Vec<EdgeCapabilityDescriptor> {
    let port = app
        .state::<AppState>()
        .config
        .lock()
        .await
        .network
        .magicutor_port;
    let (cua, browser) = tokio::join!(cua_operations(), browser_available(port));
    descriptors_from_availability(
        generation,
        cua,
        browser,
        imessage_available(),
        cfg!(target_os = "macos"),
    )
}

pub(crate) async fn invoke(
    app: AppHandle,
    capability: &str,
    operation: &str,
    payload: Value,
) -> Result<Value, EdgeDispatchError> {
    match capability {
        EDGE_CAPABILITY_CUA => invoke_cua(&app, operation, payload).await,
        EDGE_CAPABILITY_BROWSER_CDP => invoke_browser(&app, operation, payload).await,
        EDGE_CAPABILITY_IMESSAGE => invoke_imessage(operation, payload).await,
        EDGE_CAPABILITY_ANDROID_OBSERVATION => {
            invoke_android_observation(&app, operation, payload).await
        },
        _ => Err(EdgeDispatchError::failed(
            "capability_not_supported",
            format!("Unsupported desktop capability `{capability}`"),
        )),
    }
}

async fn invoke_android_observation(
    app: &AppHandle,
    operation: &str,
    payload: Value,
) -> Result<Value, EdgeDispatchError> {
    if operation != "open-settings" || !cfg!(target_os = "macos") {
        return Err(EdgeDispatchError::failed(
            "android_observation_operation_not_supported",
            "This desktop cannot service the requested Android observation operation",
        ));
    }
    let trust_mode = payload.as_object().and_then(|value| {
        (value.len() <= 1)
            .then(|| value.get("trust_mode").and_then(Value::as_str))
            .flatten()
    });
    if !matches!(
        trust_mode,
        None | Some("play_integrity") | Some("owner_pinned_private_build")
    ) || payload.as_object().is_none()
    {
        return Err(EdgeDispatchError::failed(
            "invalid_android_observation_payload",
            "Android observation setup accepts only a reviewed trust mode",
        ));
    }
    crate::tray::open_android_observation_approval(app, trust_mode);
    Ok(json!({ "opened": true }))
}

async fn invoke_cua(
    app: &AppHandle,
    operation: &str,
    payload: Value,
) -> Result<Value, EdgeDispatchError> {
    crate::host_gateway::invoke_edge_ax(app, operation, payload)
        .await
        .map_err(|message| EdgeDispatchError::unavailable("cua_unavailable", message))
}

async fn invoke_imessage(operation: &str, payload: Value) -> Result<Value, EdgeDispatchError> {
    if operation != "query" || !cfg!(target_os = "macos") {
        return Err(EdgeDispatchError::failed(
            "imessage_operation_not_supported",
            "This desktop cannot service the requested iMessage operation",
        ));
    }
    let body = serde_json::to_vec(&payload).map_err(|_| {
        EdgeDispatchError::failed("invalid_imessage_payload", "Invalid iMessage query payload")
    })?;
    crate::host_imessage::handle(&body)
        .await
        .map_err(|message| EdgeDispatchError::unavailable("imessage_unavailable", message))
}

async fn local_browser_port(app: &AppHandle) -> u16 {
    app.state::<AppState>()
        .config
        .lock()
        .await
        .network
        .magicutor_port
}

async fn invoke_browser(
    app: &AppHandle,
    operation: &str,
    payload: Value,
) -> Result<Value, EdgeDispatchError> {
    let port = local_browser_port(app).await;
    match operation {
        "health" => browser_get(port, "/health").await,
        "version" => browser_get(port, "/json/version").await,
        "targets" => browser_get(port, "/json/list").await,
        "command" => browser_command(port, payload).await,
        _ => Err(EdgeDispatchError::failed(
            "browser_operation_not_supported",
            format!("Unsupported browser operation `{operation}`"),
        )),
    }
}

async fn browser_get(port: u16, path: &str) -> Result<Value, EdgeDispatchError> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(LOCAL_BROWSER_TIMEOUT)
        .build()
        .map_err(|_| {
            EdgeDispatchError::unavailable(
                "browser_unavailable",
                "Could not initialize the local browser client",
            )
        })?;
    let response = client
        .get(format!("http://127.0.0.1:{port}{path}"))
        .send()
        .await
        .map_err(|error| {
            EdgeDispatchError::unavailable(
                "browser_unavailable",
                format!("Local Magicutor is unavailable: {error}"),
            )
        })?;
    if !response.status().is_success() {
        return Err(EdgeDispatchError::unavailable(
            "browser_upstream_error",
            format!("Local Magicutor returned HTTP {}", response.status()),
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > BROWSER_MAX_RESPONSE_BYTES)
    {
        return Err(EdgeDispatchError::failed(
            "browser_response_too_large",
            "Local Magicutor response exceeded the Edge limit",
        ));
    }
    let body = response.bytes().await.map_err(|_| {
        EdgeDispatchError::unavailable(
            "browser_response_failed",
            "Could not read the local Magicutor response",
        )
    })?;
    if body.len() as u64 > BROWSER_MAX_RESPONSE_BYTES {
        return Err(EdgeDispatchError::failed(
            "browser_response_too_large",
            "Local Magicutor response exceeded the Edge limit",
        ));
    }
    serde_json::from_slice(&body).map_err(|_| {
        EdgeDispatchError::unavailable(
            "browser_response_invalid",
            "Local Magicutor returned invalid JSON",
        )
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserCommand {
    target: String,
    #[serde(default)]
    target_id: Option<String>,
    method: String,
    #[serde(default = "empty_json_object")]
    params: Value,
}

fn empty_json_object() -> Value {
    json!({})
}

fn browser_command_path(command: &BrowserCommand) -> Result<String, EdgeDispatchError> {
    let method = command.method.trim();
    if method.is_empty()
        || method.len() > 160
        || !method
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(EdgeDispatchError::failed(
            "invalid_browser_method",
            "CDP method must be a bounded method token",
        ));
    }
    match command.target.as_str() {
        "browser" => Ok(format!("/devtools/browser/edge-{}", uuid::Uuid::new_v4())),
        "page" => {
            let target_id = command
                .target_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty() && value.len() <= 160)
                .ok_or_else(|| {
                    EdgeDispatchError::failed(
                        "browser_target_required",
                        "Page CDP commands require target_id",
                    )
                })?;
            if target_id.chars().any(|character| {
                character.is_control() || matches!(character, '/' | '?' | '#' | '\\')
            }) {
                return Err(EdgeDispatchError::failed(
                    "invalid_browser_target",
                    "Browser target id contains invalid characters",
                ));
            }
            Ok(format!("/devtools/page/{target_id}"))
        },
        _ => Err(EdgeDispatchError::failed(
            "invalid_browser_target",
            "Browser target must be `browser` or `page`",
        )),
    }
}

async fn browser_command(port: u16, payload: Value) -> Result<Value, EdgeDispatchError> {
    let command: BrowserCommand = serde_json::from_value(payload).map_err(|_| {
        EdgeDispatchError::failed(
            "invalid_browser_payload",
            "Browser command payload does not match the Edge CDP contract",
        )
    })?;
    if !command.params.is_object() {
        return Err(EdgeDispatchError::failed(
            "invalid_browser_params",
            "Browser command params must be a JSON object",
        ));
    }
    let path = browser_command_path(&command)?;
    let url = format!("ws://127.0.0.1:{port}{path}");
    let (mut socket, _) = tokio_tungstenite::connect_async(&url)
        .await
        .map_err(|error| {
            EdgeDispatchError::unavailable(
                "browser_unavailable",
                format!("Could not connect to local Magicutor CDP: {error}"),
            )
        })?;
    socket
        .send(Message::Text(
            json!({"id": 1, "method": command.method, "params": command.params}).to_string(),
        ))
        .await
        .map_err(|error| {
            EdgeDispatchError::unavailable(
                "browser_send_failed",
                format!("Could not send the local CDP command: {error}"),
            )
        })?;

    while let Some(message) = socket.next().await {
        match message.map_err(|error| {
            EdgeDispatchError::unavailable(
                "browser_receive_failed",
                format!("Local CDP connection failed: {error}"),
            )
        })? {
            Message::Text(text) => {
                if text.len() as u64 > BROWSER_MAX_RESPONSE_BYTES {
                    return Err(EdgeDispatchError::failed(
                        "browser_response_too_large",
                        "Local CDP response exceeded the Edge limit",
                    ));
                }
                let value: Value = serde_json::from_str(&text).map_err(|_| {
                    EdgeDispatchError::unavailable(
                        "browser_response_invalid",
                        "Local CDP returned invalid JSON",
                    )
                })?;
                if value.get("id").and_then(Value::as_u64) == Some(1) {
                    let _ = socket.close(None).await;
                    return Ok(value);
                }
            },
            Message::Ping(payload) => {
                socket.send(Message::Pong(payload)).await.map_err(|_| {
                    EdgeDispatchError::unavailable(
                        "browser_connection_closed",
                        "Local CDP connection closed during pong",
                    )
                })?;
            },
            Message::Close(_) => break,
            Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {},
        }
    }
    Err(EdgeDispatchError::unavailable(
        "browser_connection_closed",
        "Local CDP connection closed before returning a command result",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_manifest_never_adds_imessage_without_explicit_availability() {
        let capabilities =
            descriptors_from_availability(7, vec!["click".into()], true, false, true);
        assert!(capabilities
            .iter()
            .any(|capability| capability.capability == EDGE_CAPABILITY_CUA));
        assert!(capabilities
            .iter()
            .any(|capability| capability.capability == EDGE_CAPABILITY_BROWSER_CDP));
        assert!(!capabilities
            .iter()
            .any(|capability| capability.capability == EDGE_CAPABILITY_IMESSAGE));
        assert!(capabilities.iter().any(|capability| {
            capability.capability == EDGE_CAPABILITY_ANDROID_OBSERVATION
                && capability.operations == ["open-settings"]
        }));
        assert!(capabilities
            .iter()
            .all(|capability| capability.generation == 7 && capability.validate().is_ok()));
    }

    #[test]
    fn browser_commands_are_target_and_method_bounded() {
        let page = BrowserCommand {
            target: "page".into(),
            target_id: Some("tab-42".into()),
            method: "Runtime.evaluate".into(),
            params: json!({}),
        };
        assert_eq!(
            browser_command_path(&page).unwrap(),
            "/devtools/page/tab-42"
        );

        let traversal = BrowserCommand {
            target_id: Some("../bridge/native".into()),
            ..page
        };
        assert!(browser_command_path(&traversal).is_err());

        let decoded: BrowserCommand = serde_json::from_value(json!({
            "target": "browser",
            "method": "Target.getTargets"
        }))
        .unwrap();
        assert_eq!(decoded.params, json!({}));
    }

    #[test]
    fn imessage_capability_is_compile_time_platform_gated() {
        if !cfg!(target_os = "macos") {
            assert!(!imessage_available());
        }
    }
}

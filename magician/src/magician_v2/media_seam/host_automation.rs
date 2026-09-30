//! Host macOS automation relay client.
//!
//! macOS automation — screen capture, AppleScript/JXA, Accessibility (AX)
//! actions — can only run on the user's Mac with the desktop app's TCC grants,
//! never inside a Linux container. This provider POSTs to the Tauri host
//! gateway (`MAGICIAN_HOST_GATEWAY_URL`, default `http://127.0.0.1:3017`),
//! which performs the action and returns the result.
//!
//! The SAME provider is used natively and from a container — one code path, so
//! callers never branch on environment. It mirrors `MacOsSpeechProvider`'s
//! relay-client shape (the speech gateway is the proven prior art).

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::error;

pub const HOST_AUTOMATION_DEFAULT_GATEWAY_URL: &str = "http://127.0.0.1:3017";
const HOST_AUTOMATION_TIMEOUT: Duration = Duration::from_secs(30);
const HOST_AUTOMATION_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Resolve the host gateway base URL from `MAGICIAN_HOST_GATEWAY_URL`, falling
/// back to the default. The single place every relay caller reads the env, so
/// the var is interpreted one way (trimmed; empty treated as unset).
pub fn host_gateway_url_from_env() -> String {
    std::env::var("MAGICIAN_HOST_GATEWAY_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| HOST_AUTOMATION_DEFAULT_GATEWAY_URL.to_string())
}

#[derive(Debug, thiserror::Error)]
pub enum HostAutomationError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("transport failure: {0}")]
    Transport(String),
    #[error("host gateway returned status {status}: {body}")]
    Upstream { status: u16, body: String },
}

/// Relay client for the macOS-automation routes on the Tauri host gateway.
#[derive(Debug, Clone)]
pub struct HostAutomationProvider {
    client: Client,
    gateway_url: String,
}

impl HostAutomationProvider {
    pub fn new(gateway_url: impl Into<String>) -> Self {
        Self {
            client: default_http_client(),
            gateway_url: gateway_url.into(),
        }
    }

    pub fn with_client(client: Client, gateway_url: impl Into<String>) -> Self {
        Self {
            client,
            gateway_url: gateway_url.into(),
        }
    }

    /// Build a gateway URL for a `/host/<path>` route, tolerating trailing
    /// slashes on the base and a leading slash on `path`.
    fn endpoint(&self, path: &str) -> String {
        format!(
            "{}/host/{}",
            self.gateway_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    /// Capture the screen (or a display/region/window) on the host. Returns the
    /// decoded image bytes from the gateway's base64 payload. TCC: Screen
    /// Recording (granted to the desktop app).
    pub async fn capture_screen(
        &self,
        request: ScreenCaptureRequest,
    ) -> Result<ScreenCapture, HostAutomationError> {
        let endpoint = self.endpoint("screen/capture");
        let response = self
            .post(&endpoint, &request, HOST_AUTOMATION_TIMEOUT)
            .await?;
        let parsed: ScreenCaptureResponse = decode_json(response).await?;
        let image = STANDARD
            .decode(parsed.image_b64.as_bytes())
            .map_err(|error| {
                HostAutomationError::Transport(format!("decoding screen capture image: {error}"))
            })?;
        Ok(ScreenCapture {
            image,
            content_type: parsed.content_type,
            width: parsed.width,
            height: parsed.height,
        })
    }

    /// Run an AppleScript/JXA source string on the host. TCC: Automation. This
    /// is the broadest route — the gateway gates it on the skill grant.
    pub async fn run_applescript(
        &self,
        request: AppleScriptRequest,
    ) -> Result<AppleScriptResult, HostAutomationError> {
        if request.source.trim().is_empty() {
            return Err(HostAutomationError::BadRequest(
                "empty applescript source".into(),
            ));
        }
        let endpoint = self.endpoint("applescript");
        // The host clamps the osascript run to 1–120s; give the HTTP client
        // strictly MORE time than that (+ transport headroom) so a slow-but-
        // successful script is never aborted client-side as a Transport error
        // (which would discard a real result and, for sends, risk a double-send
        // on retry). Native and container alike.
        let host_secs = request.timeout_secs.unwrap_or(30).clamp(1, 120);
        let client_timeout = Duration::from_secs(host_secs + 10);
        let response = self.post(&endpoint, &request, client_timeout).await?;
        decode_json(response).await
    }

    /// Proxy an Accessibility (AX) action through the host's `cua-driver`
    /// daemon. TCC: Accessibility. `action` is the route suffix (e.g. `click`);
    /// `body` is the action's JSON payload.
    pub async fn ax(
        &self,
        action: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, HostAutomationError> {
        if action.trim().is_empty() {
            return Err(HostAutomationError::BadRequest("empty ax action".into()));
        }
        let endpoint = self.endpoint(&format!("ax/{}", action.trim_matches('/')));
        let response = self.post(&endpoint, &body, HOST_AUTOMATION_TIMEOUT).await?;
        decode_json(response).await
    }

    /// Lightweight health check: is the host automation gateway reachable and
    /// reporting itself available? Mirrors `probe_host_speech_availability` —
    /// callers advertise the mac skills as unavailable when this is false
    /// (headless server / desktop app not running).
    pub async fn gateway_available(&self) -> bool {
        self.automation_status().await.available
    }

    /// CuaDriver availability is independent of Apple Events permissions.
    /// Older desktop releases reported only the installed `cua_driver` bit.
    pub async fn cua_available(&self) -> bool {
        let status = self.automation_status().await;
        status.cua_available.unwrap_or(status.cua_driver)
    }

    async fn automation_status(&self) -> HostAutomationStatus {
        let endpoint = self.endpoint("automation/status");
        match self
            .client
            .get(&endpoint)
            .timeout(HOST_AUTOMATION_PROBE_TIMEOUT)
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => response
                .json::<HostAutomationStatus>()
                .await
                .unwrap_or_default(),
            _ => HostAutomationStatus::default(),
        }
    }

    async fn post<T: Serialize>(
        &self,
        endpoint: &str,
        payload: &T,
        timeout: Duration,
    ) -> Result<reqwest::Response, HostAutomationError> {
        let response = self
            .client
            .post(endpoint)
            .timeout(timeout)
            .json(payload)
            .send()
            .await
            .map_err(|error| {
                error!("[host-automation] transport failure: {error}");
                HostAutomationError::Transport(error.to_string())
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!(
                "[host-automation] host gateway rejected status={} body={}",
                status, body
            );
            return Err(HostAutomationError::Upstream {
                status: status.as_u16(),
                body,
            });
        }
        Ok(response)
    }
}

async fn decode_json<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, HostAutomationError> {
    response.json::<T>().await.map_err(|error| {
        error!("[host-automation] response decode failure: {error}");
        HostAutomationError::Transport(format!("decoding host gateway response: {error}"))
    })
}

fn default_http_client() -> Client {
    Client::builder()
        .user_agent("magician-media-rails/0.1 host-automation")
        .build()
        .expect("reqwest client")
}

/// Screen-capture options. All fields optional → default is the main display.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ScreenCaptureRequest {
    /// Capture only this display index when set (default: main display).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<u32>,
    /// Capture a specific region, formatted `x,y,w,h`, when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Capture the frontmost window only.
    #[serde(default, skip_serializing_if = "is_false")]
    pub window: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Deserialize)]
struct ScreenCaptureResponse {
    image_b64: String,
    #[serde(default = "png_content_type")]
    content_type: String,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
}

fn png_content_type() -> String {
    "image/png".to_string()
}

/// Decoded screen capture returned to callers.
#[derive(Debug, Clone)]
pub struct ScreenCapture {
    pub image: Vec<u8>,
    pub content_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppleScriptRequest {
    pub source: String,
    /// `applescript` (default) or `javascript` (JXA). `None` → gateway default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppleScriptResult {
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub exit_code: i32,
}

#[derive(Debug, Deserialize, Default)]
struct HostAutomationStatus {
    #[serde(default)]
    available: bool,
    #[serde(default)]
    cua_driver: bool,
    #[serde(default)]
    cua_available: Option<bool>,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use crate::magician_v2::media_seam::*;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use serde_json::{json, Value};
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    #[test]
    fn endpoint_trims_base_slashes_and_leading_path_slash() {
        let provider = HostAutomationProvider::new("http://127.0.0.1:3017///");
        assert_eq!(
            provider.endpoint("/screen/capture"),
            "http://127.0.0.1:3017/host/screen/capture"
        );
        assert_eq!(
            provider.endpoint("applescript"),
            "http://127.0.0.1:3017/host/applescript"
        );
    }

    #[tokio::test]
    async fn capture_screen_posts_and_decodes_base64_image() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/host/screen/capture"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "image_b64": STANDARD.encode(b"PNGDATA"),
                "content_type": "image/png",
                "width": 1920,
                "height": 1080
            })))
            .mount(&server)
            .await;

        let provider = HostAutomationProvider::new(format!("{}/", server.uri()));
        let capture = provider
            .capture_screen(ScreenCaptureRequest {
                window: true,
                ..Default::default()
            })
            .await
            .expect("capture should parse");

        assert_eq!(capture.image, b"PNGDATA");
        assert_eq!(capture.content_type, "image/png");
        assert_eq!(capture.width, Some(1920));

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        // Default fields are omitted; only the explicitly-set `window` is sent.
        assert_eq!(body["window"], json!(true));
        assert!(body.get("display").is_none());
        assert!(body.get("region").is_none());
    }

    #[tokio::test]
    async fn run_applescript_rejects_empty_source_before_calling_gateway() {
        let server = MockServer::start().await;
        let provider = HostAutomationProvider::new(server.uri());
        let err = provider
            .run_applescript(AppleScriptRequest {
                source: "   ".to_string(),
                language: None,
                timeout_secs: None,
            })
            .await
            .expect_err("empty source should fail locally");
        assert!(matches!(err, HostAutomationError::BadRequest(_)));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn run_applescript_posts_source_and_parses_result() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/host/applescript"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "stdout": "Notes",
                "stderr": "",
                "exit_code": 0
            })))
            .mount(&server)
            .await;

        let provider = HostAutomationProvider::new(server.uri());
        let result = provider
            .run_applescript(AppleScriptRequest {
                source: "tell application \"System Events\" to return name of first process"
                    .to_string(),
                language: Some("applescript".to_string()),
                timeout_secs: Some(10),
            })
            .await
            .expect("applescript result should parse");

        assert_eq!(result.stdout, "Notes");
        assert_eq!(result.exit_code, 0);

        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["language"], "applescript");
        assert_eq!(body["timeout_secs"], 10);
        assert!(body["source"].as_str().unwrap().contains("System Events"));
    }

    #[tokio::test]
    async fn ax_action_is_appended_to_the_route() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/host/ax/click"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .mount(&server)
            .await;

        let provider = HostAutomationProvider::new(server.uri());
        let out = provider
            .ax("click", json!({ "element_index": 3 }))
            .await
            .expect("ax proxy should parse");
        assert_eq!(out["ok"], json!(true));
    }

    #[tokio::test]
    async fn upstream_rejection_preserves_status_and_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/host/applescript"))
            .respond_with(ResponseTemplate::new(403).set_body_string("automation not permitted"))
            .mount(&server)
            .await;

        let provider = HostAutomationProvider::new(server.uri());
        let err = provider
            .run_applescript(AppleScriptRequest {
                source: "beep".to_string(),
                language: None,
                timeout_secs: None,
            })
            .await
            .expect_err("gateway rejection should surface");
        match err {
            HostAutomationError::Upstream { status, body } => {
                assert_eq!(status, 403);
                assert_eq!(body, "automation not permitted");
            },
            other => panic!("expected upstream error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn gateway_available_reflects_status_payload() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/host/automation/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "available": true })))
            .mount(&server)
            .await;
        let provider = HostAutomationProvider::new(server.uri());
        assert!(provider.gateway_available().await);
    }

    #[tokio::test]
    async fn gateway_available_is_false_when_unreachable() {
        // Nothing is listening on this port → probe fails fast → unavailable.
        let provider = HostAutomationProvider::new("http://127.0.0.1:1");
        assert!(!provider.gateway_available().await);
        assert!(!provider.cua_available().await);
    }

    #[tokio::test]
    async fn cua_does_not_require_or_enable_apple_events() {
        for payload in [
            json!({"available": false, "cua_available": true}),
            json!({"available": false, "cua_driver": true}),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/host/automation/status"))
                .respond_with(ResponseTemplate::new(200).set_body_json(payload))
                .mount(&server)
                .await;
            let provider = HostAutomationProvider::new(server.uri());
            assert!(provider.cua_available().await);
            assert!(!provider.gateway_available().await);
        }
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/host/automation/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "available": true, "cua_driver": true, "cua_available": false
            })))
            .mount(&server)
            .await;
        let provider = HostAutomationProvider::new(server.uri());
        assert!(provider.gateway_available().await);
        assert!(!provider.cua_available().await);
    }
}

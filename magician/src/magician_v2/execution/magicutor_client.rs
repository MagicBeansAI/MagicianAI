use std::time::Duration;

use magicutor::types::execution::AmbientPageSignal;
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use url::Url;

use crate::magician_v2::api_mining::types::{CapturedAuthEvent, NetworkTraceEvent};

const DELETE_SESSION_TIMEOUT: Duration = Duration::from_secs(20);

/// Configuration needed to connect to Magicutor.
#[derive(Clone, Debug)]
pub struct ExecutionConfig {
    pub base_url: Url,
    pub request_timeout: Duration,
    pub api_key: Option<SecretString>,
}

impl ExecutionConfig {
    pub fn new(base_url: Url) -> Self {
        Self {
            base_url,
            request_timeout: Duration::from_secs(600), // 10 minutes for complex pages
            api_key: None,
        }
    }
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self::new(
            Url::parse("http://127.0.0.1:3003/")
                .expect("valid default Magicutor base URL must parse"),
        )
    }
}

fn cdp_thread_id_from_session_alias(session_alias: &str) -> String {
    let raw = session_alias
        .strip_prefix("agentic-session-")
        .unwrap_or(session_alias);
    if raw.starts_with("magician-") {
        raw.to_string()
    } else {
        format!("magician-{}", sanitize_cdp_thread_component(raw))
    }
}

fn sanitize_cdp_thread_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// Errors emitted by the Magicutor client.
#[derive(Debug, Error)]
pub enum MagicutorClientError {
    #[error("invalid Magicutor endpoint url: {0}")]
    InvalidEndpoint(String),

    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("browser cleanup failed: {0}")]
    Cleanup(String),
}

/// Thin HTTP client that talks directly to Magicutor's REST API.
///
/// The legacy `/execute` browser-action client has been removed. New browser
/// automation must use the browser skill and agent-browser; this client keeps
/// only Magicutor sidecar APIs such as CDP thread cleanup, trace drain,
/// download metadata, and extension health.
#[derive(Clone)]
pub struct MagicutorClient {
    http_client: Client,
    base_url: Url,
    api_key: Option<SecretString>,
}

impl MagicutorClient {
    pub fn new(config: ExecutionConfig) -> Result<Self, MagicutorClientError> {
        let disable_proxy = std::env::var("MAGICIAN_DISABLE_SYSTEM_PROXY")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let mut builder = Client::builder().timeout(config.request_timeout);

        if disable_proxy {
            builder = builder.no_proxy();
        }

        let http_client = builder.build()?;

        Ok(Self {
            http_client,
            base_url: config.base_url,
            api_key: config.api_key,
        })
    }

    /// Base URL used by the CDP proxy. Exposed so portable browser-owned
    /// subsystems can derive the matching WebSocket endpoint without creating
    /// a second client with potentially different runtime configuration.
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Clear a browser CDP thread by its historical session alias or by the
    /// current agent-browser thread id. This is best-effort cleanup for
    /// windows/tabs owned by Magicutor's CDP proxy.
    ///
    /// Note: This is best-effort - returns Ok even if session doesn't exist.
    pub async fn delete_session(&self, session_alias: &str) -> Result<(), MagicutorClientError> {
        let thread_id = cdp_thread_id_from_session_alias(session_alias);
        if let Err(error) = self.delete_session_checked(&thread_id).await {
            tracing::warn!(session_alias, %error, "Browser cleanup incomplete (best effort)");
        }
        Ok(())
    }

    /// Cleanup callers must retain their retry state until Magicutor confirms
    /// that the extension actually removed the task's tabs and session.
    pub async fn delete_session_checked(
        &self,
        session_alias: &str,
    ) -> Result<(), MagicutorClientError> {
        self.release_session_checked(session_alias, false).await
    }

    pub async fn handoff_session_checked(
        &self,
        session_alias: &str,
    ) -> Result<(), MagicutorClientError> {
        self.release_session_checked(session_alias, true).await
    }

    async fn release_session_checked(
        &self,
        session_alias: &str,
        keep_tabs: bool,
    ) -> Result<(), MagicutorClientError> {
        // This is an actual agent-browser ID, including retrieval-/scope-
        // prefixes. Only the historical best-effort API expands old aliases.
        let thread_id = session_alias;
        if thread_id.is_empty()
            || !thread_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(MagicutorClientError::Cleanup(
                "invalid browser session ID".into(),
            ));
        }
        let endpoint = self
            .base_url
            .join(&format!("cdp/threads/{}", thread_id))
            .map_err(|error| MagicutorClientError::InvalidEndpoint(error.to_string()))?;
        let mut request = self
            .http_client
            .delete(endpoint)
            .query(&[("keep_tabs", keep_tabs)]);
        if let Some(secret) = &self.api_key {
            request = request.bearer_auth(secret.expose_secret());
        }
        tokio::time::timeout(DELETE_SESSION_TIMEOUT, async {
            let response = request.send().await?.error_for_status()?;
            let body: serde_json::Value = response.json().await?;
            if body.get("cleared").and_then(serde_json::Value::as_bool) != Some(true) {
                return Err(MagicutorClientError::Cleanup(
                    "Magicutor did not confirm cleanup".into(),
                ));
            }
            Ok(())
        })
        .await
        .map_err(|_| MagicutorClientError::Cleanup("Magicutor cleanup timed out".into()))?
    }

    /// Drain API-mining network traces captured for `thread_id` on the
    /// magicutor CDP proxy. Returns the events with `capture_source =
    /// "cdp_proxy"` already set by the proxy. The buffer is per-thread, so
    /// the caller is expected to drain at inner-loop boundaries (or more
    /// frequently for high-traffic sessions) and feed the events into the
    /// `TraceManager`.
    ///
    /// Returns an empty vec when no thread state exists or no events have
    /// completed since the last drain. Network errors are surfaced; missing
    /// endpoint (older magicutor) returns 404 → empty vec.
    pub async fn drain_network_traces(
        &self,
        thread_id: &str,
    ) -> Result<Vec<NetworkTraceEvent>, MagicutorClientError> {
        let endpoint = self
            .base_url
            .join(&format!("trace/drain/{}", thread_id))
            .map_err(|err| MagicutorClientError::InvalidEndpoint(err.to_string()))?;
        let mut builder = self.http_client.get(endpoint);
        if let Some(secret) = &self.api_key {
            builder = builder.bearer_auth(secret.expose_secret());
        }
        let response = builder.send().await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        let body: DrainTracesResponse = response.error_for_status()?.json().await?;
        Ok(body.traces)
    }

    /// Drain unredacted, process-memory-only auth material captured for a CDP
    /// session. Magicutor keeps this channel separate from redacted network
    /// traces; callers must consume it directly into the encrypted secret store.
    pub async fn drain_captured_auth(
        &self,
        thread_id: &str,
    ) -> Result<Vec<CapturedAuthEvent>, MagicutorClientError> {
        let endpoint = self
            .base_url
            .join(&format!("auth/drain/{}", thread_id))
            .map_err(|err| MagicutorClientError::InvalidEndpoint(err.to_string()))?;
        let mut builder = self.http_client.get(endpoint);
        if let Some(secret) = &self.api_key {
            builder = builder.bearer_auth(secret.expose_secret());
        }
        let response = builder.send().await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        let body: DrainCapturedAuthResponse = response.error_for_status()?.json().await?;
        Ok(body.events)
    }

    /// Drain passive ambient page signals captured for `thread_id` on the
    /// magicutor CDP proxy. Returns an empty vec for no state or an older
    /// Magicutor build without the endpoint.
    pub async fn drain_page_signals(
        &self,
        thread_id: &str,
    ) -> Result<Vec<AmbientPageSignal>, MagicutorClientError> {
        let endpoint = self
            .base_url
            .join(&format!("ambient/page/drain/{}", thread_id))
            .map_err(|err| MagicutorClientError::InvalidEndpoint(err.to_string()))?;
        let mut builder = self.http_client.get(endpoint);
        if let Some(secret) = &self.api_key {
            builder = builder.bearer_auth(secret.expose_secret());
        }
        let response = builder.send().await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        let body: DrainPageSignalsResponse = response.error_for_status()?.json().await?;
        Ok(body.page_signals)
    }
}

#[derive(Debug, serde::Deserialize)]
struct DrainTracesResponse {
    #[serde(default)]
    traces: Vec<NetworkTraceEvent>,
}

#[derive(Debug, serde::Deserialize)]
struct DrainCapturedAuthResponse {
    #[serde(default)]
    events: Vec<CapturedAuthEvent>,
}

#[derive(Debug, serde::Deserialize)]
struct DrainPageSignalsResponse {
    #[serde(default)]
    page_signals: Vec<AmbientPageSignal>,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod cleanup_tests {
    use super::*;
    use wiremock::{
        matchers::{method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };

    #[tokio::test]
    async fn cleanup_uses_exact_current_session_ids_and_explicit_handoff() {
        let server = MockServer::start().await;
        let client =
            MagicutorClient::new(ExecutionConfig::new(Url::parse(&server.uri()).unwrap())).unwrap();
        for session in [
            "retrieval-cdp-example",
            "scope-engagement-demo-magician-chat-main--cdp",
        ] {
            Mock::given(method("DELETE"))
                .and(path(format!("/cdp/threads/{session}")))
                .and(query_param("keep_tabs", "false"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({"cleared": true})),
                )
                .expect(1)
                .mount(&server)
                .await;
            client.delete_session_checked(session).await.unwrap();
        }
        Mock::given(method("DELETE"))
            .and(path("/cdp/threads/magician-owner-handoff"))
            .and(query_param("keep_tabs", "true"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"cleared": true})),
            )
            .expect(1)
            .mount(&server)
            .await;
        client
            .handoff_session_checked("magician-owner-handoff")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn cleanup_cannot_report_success_for_http_failure_or_missing_confirmation() {
        let server = MockServer::start().await;
        let client =
            MagicutorClient::new(ExecutionConfig::new(Url::parse(&server.uri()).unwrap())).unwrap();
        for (session, response) in [
            ("magician-failed", ResponseTemplate::new(503)),
            (
                "magician-unconfirmed",
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"cleared": false})),
            ),
        ] {
            Mock::given(method("DELETE"))
                .and(path(format!("/cdp/threads/{session}")))
                .respond_with(response)
                .mount(&server)
                .await;
            assert!(client.delete_session_checked(session).await.is_err());
        }
    }
}

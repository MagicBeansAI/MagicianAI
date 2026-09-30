use std::time::Duration;

use magicutor::types::execution::AmbientPageSignal;
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use url::Url;

use crate::magician_v2::api_mining::types::{CapturedAuthEvent, NetworkTraceEvent};

const DELETE_SESSION_TIMEOUT: Duration = Duration::from_secs(2);

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
        let session_endpoint = self
            .base_url
            .join(&format!("cdp/threads/{}", thread_id))
            .map_err(|err| MagicutorClientError::InvalidEndpoint(err.to_string()))?;

        let mut builder = self.http_client.delete(session_endpoint);

        if let Some(secret) = &self.api_key {
            builder = builder.bearer_auth(secret.expose_secret());
        }

        // Best-effort: ignore 404 (session doesn't exist) and other errors
        // We just want to ensure the session is gone
        match tokio::time::timeout(DELETE_SESSION_TIMEOUT, builder.send()).await {
            Ok(Ok(_)) => {
                tracing::info!(
                    "[MAGICUTOR-CLIENT] Cleared browser CDP thread: {}",
                    thread_id
                );
                Ok(())
            },
            Ok(Err(e)) => {
                tracing::warn!(
                    "[MAGICUTOR-CLIENT] Failed to clear browser CDP thread {}: {} (continuing anyway)",
                    thread_id,
                    e
                );
                // Return Ok anyway - we don't want to fail cancel_execution if Magicutor is down
                Ok(())
            },
            Err(_) => {
                tracing::warn!(
                    "[MAGICUTOR-CLIENT] Timed out clearing browser CDP thread {} after {:?} (continuing anyway)",
                    thread_id,
                    DELETE_SESSION_TIMEOUT
                );
                Ok(())
            },
        }
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

//! Host-side client for the engine's Unix socket.
//!
//! One HTTP/1.1 connection per call: the socket is local, a connect costs
//! microseconds, and nothing pools state across a restarted engine. Every
//! failure — no socket, a timeout, a bad body, a contract mismatch — is a
//! [`ClientError`]. [`EngineClient::decide_or_unbound`] folds them into the "unbound"
//! answer hosts already treat as "keep the incumbent path".

use std::path::PathBuf;
use std::time::{Duration, Instant};

use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::net::UnixStream;

use crate::action::{ActionRequest, ActionResponse, ACTION_PATH};
use crate::wire::{
    DecideRequest, DecideResponse, DecideStatus, HealthResponse, OperationsResponse,
    CONTRACT_VERSION, DECIDE_PATH, HEALTH_PATH, OPERATIONS_PATH,
};

/// Engine responses are small structured JSON; a larger body is a fault.
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("decision engine unreachable: {0}")]
    Connect(String),
    #[error("decision engine timed out")]
    Timeout,
    #[error("decision engine transport error: {0}")]
    Http(String),
    #[error("decision engine returned status {status}: {body}")]
    Status { status: u16, body: String },
    #[error("decision engine body invalid: {0}")]
    Decode(String),
    #[error("decision engine speaks contract {engine}, host speaks {CONTRACT_VERSION}")]
    ContractMismatch { engine: u32 },
}

#[derive(Debug, Clone)]
pub struct EngineClient {
    socket: PathBuf,
    timeout: Duration,
}

/// Every body the engine returns carries the contract version.
trait Versioned {
    fn contract_version(&self) -> u32;
}

impl Versioned for crate::settings::RoutingSettings {
    fn contract_version(&self) -> u32 {
        self.contract_version
    }
}

impl Versioned for ActionResponse {
    fn contract_version(&self) -> u32 {
        self.contract_version
    }
}

impl Versioned for DecideResponse {
    fn contract_version(&self) -> u32 {
        self.contract_version
    }
}
impl Versioned for OperationsResponse {
    fn contract_version(&self) -> u32 {
        self.contract_version
    }
}
impl Versioned for HealthResponse {
    fn contract_version(&self) -> u32 {
        self.contract_version
    }
}

impl EngineClient {
    pub fn new(socket: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            socket: socket.into(),
            timeout,
        }
    }

    pub async fn health(&self) -> Result<HealthResponse, ClientError> {
        self.call::<(), _>("GET", HEALTH_PATH, None).await
    }

    pub async fn routing_settings(&self) -> Result<crate::settings::RoutingSettings, ClientError> {
        self.call::<(), _>("GET", crate::settings::SETTINGS_PATH, None)
            .await
    }

    pub async fn update_routing(
        &self,
        update: &crate::settings::RoutingUpdate,
    ) -> Result<crate::settings::RoutingSettings, ClientError> {
        self.call("PUT", crate::settings::SETTINGS_PATH, Some(update))
            .await
    }

    pub async fn operations(&self) -> Result<OperationsResponse, ClientError> {
        self.call::<(), _>("GET", OPERATIONS_PATH, None).await
    }

    pub async fn decide(&self, request: &DecideRequest) -> Result<DecideResponse, ClientError> {
        self.decide_with_timeout(request, self.timeout).await
    }

    pub fn with_timeout(&self, timeout: Duration) -> Self {
        Self {
            socket: self.socket.clone(),
            timeout,
        }
    }

    /// Foreground callers reserve time for fallback independently of the
    /// engine's execution budget and bounded receipt collection margin.
    pub async fn decide_with_timeout(
        &self,
        request: &DecideRequest,
        timeout: Duration,
    ) -> Result<DecideResponse, ClientError> {
        request
            .batch
            .validate_identity()
            .map_err(|e| ClientError::Decode(e.into()))?;
        let response: DecideResponse = self
            .with_timeout(timeout)
            .call("POST", DECIDE_PATH, Some(request))
            .await?;
        if !response.batch.matches(&request.batch) {
            return Err(ClientError::Decode(
                "decision batch identity or policy mismatch".into(),
            ));
        }
        let mut call_ids = std::collections::BTreeSet::new();
        for call in &response.model_calls {
            if !call_ids.insert(&call.call_id)
                || (!request.batch.items.is_empty()
                    && (call.batch_id.as_deref() != Some(request.batch.request_id.as_str())
                        || call.item_ids.is_empty()
                        || call
                            .item_ids
                            .iter()
                            .any(|id| !request.batch.items.iter().any(|i| &i.item_id == id))))
            {
                return Err(ClientError::Decode(
                    "decision receipt identity mismatch".into(),
                ));
            }
        }
        Ok(response)
    }

    /// A transport failure is an error, never authority to execute a planner proposal.
    pub async fn action(&self, request: &ActionRequest) -> Result<ActionResponse, ClientError> {
        let response: ActionResponse = self.call("POST", ACTION_PATH, Some(request)).await?;
        if !response.matches(request) {
            return Err(ClientError::Decode(
                "decision response snapshot mismatch".into(),
            ));
        }
        Ok(response)
    }

    /// [`Self::decide`], with every failure read as unbound.
    pub async fn decide_or_unbound(&self, request: &DecideRequest) -> DecideResponse {
        let started = Instant::now();
        self.decide(request)
            .await
            .unwrap_or_else(|error| DecideResponse {
                batch: Default::default(),
                model_calls: Vec::new(),
                contract_version: CONTRACT_VERSION,
                status: DecideStatus::Unbound,
                response: None,
                thresholds: None,
                error: Some(error.to_string()),
                latency_ms: started.elapsed().as_millis() as u64,
            })
    }

    async fn call<Req: Serialize, Resp: DeserializeOwned + Versioned>(
        &self,
        method: &str,
        path: &str,
        body: Option<&Req>,
    ) -> Result<Resp, ClientError> {
        let bytes = match body {
            Some(body) => {
                serde_json::to_vec(body).map_err(|err| ClientError::Decode(err.to_string()))?
            },
            None => Vec::new(),
        };
        let response = tokio::time::timeout(self.timeout, self.exchange(method, path, bytes))
            .await
            .map_err(|_| ClientError::Timeout)??;
        // Check the envelope before decoding the current body: an old engine
        // may not have fields required by this contract.
        let envelope: serde_json::Value = serde_json::from_slice(&response)
            .map_err(|err| ClientError::Decode(err.to_string()))?;
        if let Some(version) = envelope.get("contract_version").and_then(|v| v.as_u64()) {
            if version != u64::from(CONTRACT_VERSION) {
                return Err(ClientError::ContractMismatch {
                    engine: version.try_into().unwrap_or(u32::MAX),
                });
            }
        }
        let decoded: Resp =
            serde_json::from_value(envelope).map_err(|err| ClientError::Decode(err.to_string()))?;
        if decoded.contract_version() != CONTRACT_VERSION {
            return Err(ClientError::ContractMismatch {
                engine: decoded.contract_version(),
            });
        }
        Ok(decoded)
    }

    async fn exchange(
        &self,
        method: &str,
        path: &str,
        body: Vec<u8>,
    ) -> Result<Bytes, ClientError> {
        let stream = UnixStream::connect(&self.socket)
            .await
            .map_err(|err| ClientError::Connect(format!("{}: {err}", self.socket.display())))?;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|err| ClientError::Http(err.to_string()))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let request = hyper::Request::builder()
            .method(method)
            .uri(path)
            .header(hyper::header::HOST, "decision-engine")
            .header(hyper::header::CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(body)))
            .map_err(|err| ClientError::Http(err.to_string()))?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|err| ClientError::Http(err.to_string()))?;
        let status = response.status().as_u16();
        let body = Limited::new(response.into_body(), MAX_RESPONSE_BYTES)
            .collect()
            .await
            .map_err(|err| ClientError::Http(err.to_string()))?
            .to_bytes();
        if !(200..300).contains(&status) {
            return Err(ClientError::Status {
                status,
                body: String::from_utf8_lossy(&body).chars().take(500).collect(),
            });
        }
        Ok(body)
    }
}

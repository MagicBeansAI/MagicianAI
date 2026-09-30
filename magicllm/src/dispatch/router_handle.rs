//! Router abstraction used by the dispatch queue.
//!
//! Both `MultiLLMRouter` (used directly by tests) and `ConfiguredRouter`
//! (used in production wiring) implement this trait. Workers consult the
//! handle to route requests, run streaming, and resolve provider /
//! timeout for each call.

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::bootstrap::ConfiguredRouter;
use crate::capability::LLMProviderKind;
use crate::config::LLMProfile;
use crate::error::LLMResult;
use crate::router::MultiLLMRouter;
use crate::types::{LLMRequest, LLMResponse, StreamDelta};

/// Object-safe router abstraction used by `LlmDispatchQueue`.
#[async_trait]
pub trait DispatchRouter: Send + Sync + 'static {
    /// Route a request synchronously (workers' default path).
    async fn route(&self, request: LLMRequest) -> LLMResult<LLMResponse>;

    /// Route a streaming request.
    async fn route_stream(
        &self,
        request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()>;

    /// Resolve the provider kind for an operation. Used by the per-provider
    /// concurrency / cool-down / circuit-breaker gates.
    fn provider_for_operation(&self, operation: &str) -> Option<LLMProviderKind>;

    /// Resolve the provider for the concrete request, including exact
    /// profile/provider overrides. Custom test routers retain operation-only
    /// behavior unless they opt into request-aware routing.
    fn provider_for_request(&self, request: &LLMRequest) -> Option<LLMProviderKind> {
        let operation = if request.metadata.operation.is_empty() {
            "default"
        } else {
            request.metadata.operation.as_str()
        };
        self.provider_for_operation(operation)
    }

    /// Resolve the per-profile timeout (seconds) for an operation. Used by
    /// the worker watchdog.
    fn timeout_for_operation(&self, operation: &str) -> Option<u64>;

    /// Resolve the timeout for the concrete request, including exact profile
    /// overrides. Defaults to the operation mapping for compatibility.
    fn timeout_for_request(&self, request: &LLMRequest) -> Option<u64> {
        let operation = if request.metadata.operation.is_empty() {
            "default"
        } else {
            request.metadata.operation.as_str()
        };
        self.timeout_for_operation(operation)
    }
}

#[async_trait]
impl DispatchRouter for MultiLLMRouter {
    async fn route(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
        MultiLLMRouter::route(self, request).await
    }

    async fn route_stream(
        &self,
        request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        MultiLLMRouter::route_stream(self, request, tx).await
    }

    fn provider_for_operation(&self, operation: &str) -> Option<LLMProviderKind> {
        self.profile_for_operation(operation)
            .map(|p: &LLMProfile| p.provider.clone())
    }

    fn provider_for_request(&self, request: &LLMRequest) -> Option<LLMProviderKind> {
        self.profile_for_request(request)
            .map(|profile| profile.provider.clone())
    }

    fn timeout_for_operation(&self, operation: &str) -> Option<u64> {
        self.profile_for_operation(operation)
            .and_then(|p| p.timeout_secs)
    }

    fn timeout_for_request(&self, request: &LLMRequest) -> Option<u64> {
        self.profile_for_request(request)
            .and_then(|profile| profile.timeout_secs)
    }
}

#[async_trait]
impl DispatchRouter for ConfiguredRouter {
    async fn route(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
        ConfiguredRouter::route(self, request).await
    }

    async fn route_stream(
        &self,
        request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        ConfiguredRouter::route_stream(self, request, tx).await
    }

    fn provider_for_operation(&self, operation: &str) -> Option<LLMProviderKind> {
        ConfiguredRouter::provider_for_operation(self, operation)
    }

    fn provider_for_request(&self, request: &LLMRequest) -> Option<LLMProviderKind> {
        self.router()
            .profile_for_request(request)
            .map(|profile| profile.provider.clone())
    }

    fn timeout_for_operation(&self, operation: &str) -> Option<u64> {
        self.router()
            .profile_for_operation(operation)
            .and_then(|p| p.timeout_secs)
    }

    fn timeout_for_request(&self, request: &LLMRequest) -> Option<u64> {
        self.router()
            .profile_for_request(request)
            .and_then(|profile| profile.timeout_secs)
    }
}

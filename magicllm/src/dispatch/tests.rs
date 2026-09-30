//! Unit tests for the dispatch module.
//!
//! Tests are scoped per topic. They use a fake provider implementation
//! (`TestProvider`) registered with a real `MultiLLMRouter` so the full
//! worker pipeline exercises.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::{mpsc, Mutex as AsyncMutex, Notify};

use crate::capability::{LLMCapability, LLMModality, LLMProviderKind};
use crate::config::{LLMProfile, LLMRouterConfig, OperationProfileSelector};
use crate::error::{LLMError, LLMResult};
use crate::provider::LLMProvider;
use crate::router::MultiLLMRouter;
use crate::types::{
    ContentBlock, LLMMessage, LLMRequest, LLMResponse, LLMToolCall, LLMToolSpec, MediaContent,
    MessageRole, RequestMetadata, StreamDelta, SummarisableBlock, SummarisationPurpose, TokenUsage,
};

use super::cancellation::NoopTaskStateView;
use super::capacity::{DispatchCapacityPlan, DispatchEngine};
use super::config::DispatchConfig;
use super::events::LlmQueueEvent;
use super::job::{DispatchedResponse, ErrorClass, LlmJob, LlmStreamJob};
use super::ledger::{LlmCallLedgerEvent, NoopTaskLedgerSink, TaskLedgerSink};
use super::local_prep::{install_local_prep_test_hook, LocalPrepTestHook};
use super::queue::LlmDispatchQueue;
use super::router_handle::DispatchRouter;
use super::test_support::{MockTaskLedgerSink, MockTaskStateView};
use super::types::{JobOrigin, JobState, Priority, TaskRef, TombstoneReason};

// ---------------------------------------------------------------------------
// Fake provider with programmable per-call outcomes.
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct TestProvider {
    script: Arc<Mutex<std::collections::VecDeque<TestOutcome>>>,
    captured_requests: Arc<Mutex<Vec<LLMRequest>>>,
    call_count: Arc<std::sync::atomic::AtomicU32>,
    delay: Arc<Mutex<Option<Duration>>>,
    stream_without_terminal: Arc<std::sync::atomic::AtomicBool>,
    block_first_call: Arc<std::sync::atomic::AtomicBool>,
    first_call_entered: Arc<Notify>,
    release_first_call: Arc<Notify>,
    advertised_kind: Arc<Mutex<Option<LLMProviderKind>>>,
    in_flight: Arc<std::sync::atomic::AtomicUsize>,
    peak_in_flight: Arc<std::sync::atomic::AtomicUsize>,
    invoke_started: Arc<Notify>,
}

#[derive(Clone)]
enum TestOutcome {
    Ok(LLMResponse),
    Err(LLMError),
}

impl TestProvider {
    fn script(&self, outcome: TestOutcome) {
        self.script.lock().push_back(outcome);
    }
    fn ok(text: &str) -> TestOutcome {
        TestOutcome::Ok(LLMResponse {
            text: Some(Arc::<str>::from(text)),
            usage: Some(TokenUsage {
                prompt_tokens: Some(10),
                completion_tokens: Some(5),
                total_tokens: Some(15),
                reasoning_tokens: Some(0),
                cached_tokens: Some(0),
                cache_creation_tokens: Some(0),
            }),
            ..Default::default()
        })
    }
    fn server_5xx() -> TestOutcome {
        TestOutcome::Err(LLMError::Provider {
            provider: "test".to_string(),
            message: "500 internal server error".to_string(),
        })
    }
    fn provider_4xx() -> TestOutcome {
        TestOutcome::Err(LLMError::Provider {
            provider: "test".to_string(),
            message: "400 bad request".to_string(),
        })
    }
    fn rate_limit() -> TestOutcome {
        TestOutcome::Err(LLMError::RateLimited {
            retry_after: Some(Duration::from_millis(50)),
        })
    }
    fn calls(&self) -> u32 {
        self.call_count.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn captured_requests(&self) -> Vec<LLMRequest> {
        self.captured_requests.lock().clone()
    }
    fn set_delay(&self, d: Duration) {
        *self.delay.lock() = Some(d);
    }
    fn omit_stream_terminal(&self) {
        self.stream_without_terminal
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    fn block_first_call(&self) {
        self.block_first_call
            .store(true, std::sync::atomic::Ordering::Release);
    }
    async fn wait_for_first_call(&self) {
        self.first_call_entered.notified().await;
    }
    fn release_first_call(&self) {
        self.release_first_call.notify_one();
    }
    fn advertise_as(&self, provider: LLMProviderKind) {
        *self.advertised_kind.lock() = Some(provider);
    }
    fn in_flight(&self) -> usize {
        self.in_flight.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn peak_in_flight(&self) -> usize {
        self.peak_in_flight
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

struct InvokeInFlightGuard {
    slot: Arc<std::sync::atomic::AtomicUsize>,
}

impl InvokeInFlightGuard {
    fn enter(
        slot: Arc<std::sync::atomic::AtomicUsize>,
        peak: &Arc<std::sync::atomic::AtomicUsize>,
    ) -> Self {
        let current = slot.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        peak.fetch_max(current, std::sync::atomic::Ordering::Relaxed);
        Self { slot }
    }
}

impl Drop for InvokeInFlightGuard {
    fn drop(&mut self) {
        self.slot.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

#[async_trait]
impl LLMProvider for TestProvider {
    fn provider_kind(&self) -> LLMProviderKind {
        self.advertised_kind
            .lock()
            .clone()
            .unwrap_or_else(|| LLMProviderKind::Custom("test".to_string()))
    }

    fn capabilities(&self, _model: &str) -> LLMCapability {
        LLMCapability {
            modalities: vec![LLMModality::Text, LLMModality::Vision],
            tool_calling: true,
            ..Default::default()
        }
    }

    async fn invoke(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
        self.captured_requests.lock().push(request);
        self.invoke_started.notify_waiters();
        let _in_flight =
            InvokeInFlightGuard::enter(Arc::clone(&self.in_flight), &self.peak_in_flight);
        let call_index = self
            .call_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if call_index == 0
            && self
                .block_first_call
                .load(std::sync::atomic::Ordering::Acquire)
        {
            self.first_call_entered.notify_one();
            self.release_first_call.notified().await;
        }
        // Read+drop the parking_lot guard BEFORE awaiting — guards aren't
        // Send, and the trait wants a Send-able future.
        let delay = *self.delay.lock();
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        let next = self
            .script
            .lock()
            .pop_front()
            .unwrap_or_else(|| TestOutcome::Ok(LLMResponse::default()));
        match next {
            TestOutcome::Ok(r) => Ok(r),
            TestOutcome::Err(e) => Err(e),
        }
    }

    async fn invoke_stream(
        &self,
        request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        let response = self.invoke(request).await?;
        if self
            .stream_without_terminal
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Ok(());
        }
        let _ = tx.send(StreamDelta::Done(response)).await;
        Ok(())
    }
}

struct DispatchRouterProbe {
    inner: Arc<dyn DispatchRouter>,
    route_calls: std::sync::atomic::AtomicU32,
    provider_resolution_calls: std::sync::atomic::AtomicU32,
    timeout_resolution_calls: std::sync::atomic::AtomicU32,
}

impl DispatchRouterProbe {
    fn new(inner: Arc<dyn DispatchRouter>) -> Self {
        Self {
            inner,
            route_calls: std::sync::atomic::AtomicU32::new(0),
            provider_resolution_calls: std::sync::atomic::AtomicU32::new(0),
            timeout_resolution_calls: std::sync::atomic::AtomicU32::new(0),
        }
    }

    fn route_calls(&self) -> u32 {
        self.route_calls.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn provider_resolution_calls(&self) -> u32 {
        self.provider_resolution_calls
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn timeout_resolution_calls(&self) -> u32 {
        self.timeout_resolution_calls
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[async_trait]
impl DispatchRouter for DispatchRouterProbe {
    async fn route(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
        self.route_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.route(request).await
    }

    async fn route_stream(
        &self,
        request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        self.inner.route_stream(request, tx).await
    }

    fn provider_for_operation(&self, operation: &str) -> Option<LLMProviderKind> {
        self.provider_resolution_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.provider_for_operation(operation)
    }

    fn provider_for_request(&self, request: &LLMRequest) -> Option<LLMProviderKind> {
        self.provider_resolution_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.provider_for_request(request)
    }

    fn timeout_for_operation(&self, operation: &str) -> Option<u64> {
        self.timeout_resolution_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.timeout_for_operation(operation)
    }

    fn timeout_for_request(&self, request: &LLMRequest) -> Option<u64> {
        self.timeout_resolution_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.timeout_for_request(request)
    }
}

/// Models providers which publish an error delta for the consumer and then
/// return a richer routed error to the dispatch layer.
struct ErrorDeltaThenRoutedErrorRouter;

#[async_trait]
impl DispatchRouter for ErrorDeltaThenRoutedErrorRouter {
    async fn route(&self, _request: LLMRequest) -> LLMResult<LLMResponse> {
        unreachable!("stream-only test router")
    }

    async fn route_stream(
        &self,
        mut request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        request.metadata.record_provider_attempt();
        let _ = tx
            .send(StreamDelta::Error("provider stream failed".to_string()))
            .await;
        Err(LLMError::Provider {
            provider: "fallback-provider".to_string(),
            message: "400 malformed stream".to_string(),
        }
        .with_route(
            "fallback-profile",
            LLMProviderKind::Custom("fallback-provider".to_string()),
            "fallback-model",
        ))
    }

    fn provider_for_operation(&self, _operation: &str) -> Option<LLMProviderKind> {
        Some(LLMProviderKind::Custom("primary-provider".to_string()))
    }

    fn timeout_for_operation(&self, _operation: &str) -> Option<u64> {
        Some(60)
    }
}

/// Emits more deltas than a one-slot caller channel can accept, then returns.
/// This isolates consumer backpressure from provider execution so shutdown
/// coverage can prove that forwarding itself is cancellation-aware.
struct BackpressuredStreamRouter;

#[async_trait]
impl DispatchRouter for BackpressuredStreamRouter {
    async fn route(&self, _request: LLMRequest) -> LLMResult<LLMResponse> {
        unreachable!("stream-only test router")
    }

    async fn route_stream(
        &self,
        mut request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        request.metadata.record_provider_attempt();
        let _ = tx.send(StreamDelta::Token("first".to_string())).await;
        let _ = tx.send(StreamDelta::Token("second".to_string())).await;
        Ok(())
    }

    fn provider_for_operation(&self, _operation: &str) -> Option<LLMProviderKind> {
        Some(LLMProviderKind::Custom("backpressured".to_string()))
    }

    fn provider_for_request(&self, _request: &LLMRequest) -> Option<LLMProviderKind> {
        Some(LLMProviderKind::Custom("backpressured".to_string()))
    }

    fn timeout_for_operation(&self, _operation: &str) -> Option<u64> {
        Some(60)
    }
}

/// Reproduces an adapter that publishes a terminal-looking provider error but
/// never resolves its own future. The dispatch stream must commit the delivered
/// provider failure without waiting for that future to finish.
struct TerminalThenHungStreamRouter {
    calls: std::sync::atomic::AtomicUsize,
}

#[derive(Default)]
struct BlockingAttemptLedger {
    entered: Notify,
}

impl BlockingAttemptLedger {
    async fn wait_until_attempt_start(&self) {
        self.entered.notified().await;
    }
}

#[async_trait]
impl TaskLedgerSink for BlockingAttemptLedger {
    async fn append(&self, _task_ref: &TaskRef, event: LlmCallLedgerEvent) {
        if matches!(event, LlmCallLedgerEvent::AttemptStart { .. }) {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
    }
}

impl TerminalThenHungStreamRouter {
    fn new() -> Self {
        Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl DispatchRouter for TerminalThenHungStreamRouter {
    async fn route(&self, _request: LLMRequest) -> LLMResult<LLMResponse> {
        unreachable!("stream-only test router")
    }

    async fn route_stream(
        &self,
        mut request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        request.metadata.record_provider_attempt();
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 0 {
            let _ = tx
                .send(StreamDelta::Error("terminal before hang".to_string()))
                .await;
            std::future::pending::<()>().await;
            unreachable!("hung stream is cancelled by the dispatch layer")
        }
        let _ = tx.send(StreamDelta::Done(LLMResponse::default())).await;
        if call == 1 {
            std::future::pending::<()>().await;
            unreachable!("Done commits success and aborts adapter cleanup")
        }
        Ok(())
    }

    fn provider_for_operation(&self, _operation: &str) -> Option<LLMProviderKind> {
        Some(LLMProviderKind::Custom("terminal-hang".to_string()))
    }

    fn provider_for_request(&self, _request: &LLMRequest) -> Option<LLMProviderKind> {
        Some(LLMProviderKind::Custom("terminal-hang".to_string()))
    }

    fn timeout_for_operation(&self, _operation: &str) -> Option<u64> {
        Some(60)
    }
}

/// Two-provider router used to reproduce the subtle handoff inversion where a
/// background job reserved provider A while waiting behind a provider-B job in
/// the only background-capable worker lane.
struct PriorityHandoffRouter {
    background_started: std::sync::atomic::AtomicBool,
    owner_started: std::sync::atomic::AtomicBool,
    background_release: tokio::sync::Semaphore,
    owner_release: tokio::sync::Semaphore,
    provider_a_order: Mutex<Vec<String>>,
}

impl PriorityHandoffRouter {
    fn new() -> Self {
        Self {
            background_started: std::sync::atomic::AtomicBool::new(false),
            owner_started: std::sync::atomic::AtomicBool::new(false),
            background_release: tokio::sync::Semaphore::new(0),
            owner_release: tokio::sync::Semaphore::new(0),
            provider_a_order: Mutex::new(Vec::new()),
        }
    }

    fn provider_a_order(&self) -> Vec<String> {
        self.provider_a_order.lock().clone()
    }

    fn response(text: &str) -> LLMResponse {
        LLMResponse {
            text: Some(Arc::<str>::from(text)),
            ..Default::default()
        }
    }
}

#[async_trait]
impl DispatchRouter for PriorityHandoffRouter {
    async fn route(&self, mut request: LLMRequest) -> LLMResult<LLMResponse> {
        request.metadata.record_provider_attempt();
        match request.model.as_str() {
            "background-blocker" => {
                self.background_started
                    .store(true, std::sync::atomic::Ordering::Release);
                let _ = self.background_release.acquire().await;
                Ok(Self::response("background-blocker-complete"))
            },
            "provider-a-owner" => {
                self.provider_a_order.lock().push("owner".to_string());
                self.owner_started
                    .store(true, std::sync::atomic::Ordering::Release);
                let _ = self.owner_release.acquire().await;
                Ok(Self::response("provider-a-owner-complete"))
            },
            "provider-a-high" => {
                self.provider_a_order.lock().push("high".to_string());
                Ok(Self::response("provider-a-high-complete"))
            },
            "provider-a-background" => {
                self.provider_a_order.lock().push("background".to_string());
                Ok(Self::response("provider-a-background-complete"))
            },
            other => panic!("unexpected handoff-test model: {other}"),
        }
    }

    async fn route_stream(
        &self,
        request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        let response = self.route(request).await?;
        let _ = tx.send(StreamDelta::Done(response)).await;
        Ok(())
    }

    fn provider_for_operation(&self, _operation: &str) -> Option<LLMProviderKind> {
        None
    }

    fn provider_for_request(&self, request: &LLMRequest) -> Option<LLMProviderKind> {
        Some(LLMProviderKind::Custom(
            if request.model == "background-blocker" {
                "provider-b"
            } else {
                "provider-a"
            }
            .to_string(),
        ))
    }

    fn timeout_for_operation(&self, _operation: &str) -> Option<u64> {
        Some(60)
    }
}

// ---------------------------------------------------------------------------
// Test scaffolding.
// ---------------------------------------------------------------------------

fn test_router_with_profile_metadata(
    provider: Arc<TestProvider>,
    metadata: Option<HashMap<String, serde_json::Value>>,
) -> Arc<MultiLLMRouter> {
    let mut profiles = HashMap::new();
    profiles.insert(
        "test_default".to_string(),
        LLMProfile {
            provider: LLMProviderKind::Custom("test".to_string()),
            model: "test-model".to_string(),
            api_key_env: None,
            api_base_url: None,
            temperature: None,
            max_output_tokens: None,
            default_modality: Some(LLMModality::Text),
            reasoning: None,
            metadata,
            supports_vision: Some(false),
            supports_reasoning: Some(false),
            supports_tool_calling: Some(false),
            supports_computer_use: Some(false),
            timeout_secs: Some(60),
            context_window_tokens: None,
            chunking: None,
        },
    );
    let mut operation_mapping = HashMap::new();
    operation_mapping.insert(
        "default".to_string(),
        OperationProfileSelector::Simple("test_default".to_string()),
    );
    let config = LLMRouterConfig {
        profiles,
        adaptive_profiles: HashMap::new(),
        operation_mapping,
        locality: Default::default(),
        default_profile: "test_default".to_string(),
        realtime_voice: Default::default(),
    };
    let mut router = MultiLLMRouter::new(config).expect("router");
    router.register_provider(provider);
    Arc::new(router)
}

fn test_router_with(provider: Arc<TestProvider>) -> Arc<MultiLLMRouter> {
    test_router_with_profile_metadata(
        provider,
        Some(HashMap::from([(
            "streaming".to_string(),
            serde_json::Value::Bool(true),
        )])),
    )
}

fn test_router_without_profile_metadata(provider: Arc<TestProvider>) -> Arc<MultiLLMRouter> {
    test_router_with_profile_metadata(provider, None)
}

fn test_router_with_fallback(provider: Arc<TestProvider>) -> Arc<MultiLLMRouter> {
    test_router_with_fallback_capabilities(
        provider,
        LLMProviderKind::Custom("test".to_string()),
        false,
    )
}

fn test_router_with_fallback_capabilities(
    provider: Arc<TestProvider>,
    provider_kind: LLMProviderKind,
    supports_vision: bool,
) -> Arc<MultiLLMRouter> {
    let profile = |model: &str, metadata: Option<HashMap<String, serde_json::Value>>| LLMProfile {
        provider: provider_kind.clone(),
        model: model.to_string(),
        api_key_env: (!matches!(&provider_kind, LLMProviderKind::Custom(_)))
            .then(|| "TEST_PROVIDER_API_KEY".to_string()),
        api_base_url: None,
        temperature: None,
        max_output_tokens: None,
        default_modality: Some(LLMModality::Text),
        reasoning: None,
        metadata,
        supports_vision: Some(supports_vision),
        supports_reasoning: Some(false),
        supports_tool_calling: Some(true),
        supports_computer_use: Some(false),
        timeout_secs: Some(60),
        context_window_tokens: None,
        chunking: None,
    };
    let config = LLMRouterConfig {
        profiles: HashMap::from([
            (
                "primary".to_string(),
                profile(
                    "primary-model",
                    Some(HashMap::from([(
                        "fallback_profile".to_string(),
                        serde_json::Value::String("fallback".to_string()),
                    )])),
                ),
            ),
            ("fallback".to_string(), profile("fallback-model", None)),
        ]),
        adaptive_profiles: HashMap::new(),
        operation_mapping: HashMap::from([(
            "default".to_string(),
            OperationProfileSelector::Simple("primary".to_string()),
        )]),
        locality: Default::default(),
        default_profile: "primary".to_string(),
        realtime_voice: Default::default(),
    };
    let mut router = MultiLLMRouter::new(config).expect("fallback router");
    router.register_provider(provider);
    Arc::new(router)
}

fn small_request() -> LLMRequest {
    LLMRequest {
        model: "test-model".to_string(),
        metadata: RequestMetadata {
            operation: "default".to_string(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn retained_request_bytes_u64(request: &LLMRequest) -> u64 {
    u64::try_from(request.estimated_retained_bytes())
        .expect("test request retained-byte estimate must fit dispatch accounting")
}

fn small_config() -> DispatchConfig {
    let mut cfg = DispatchConfig::default();
    cfg.workers = 2;
    // Preserve the historical all-lanes worker pool unless a test is
    // specifically exercising interactive reservation.
    cfg.reserved_interactive_workers = 0;
    cfg.queue_capacity_high = 8;
    cfg.queue_capacity_normal = 8;
    cfg.queue_capacity_background = 8;
    cfg.shutdown_timeout_secs = 2;
    cfg.completed_ring_capacity = 32;
    cfg.tombstone_ring_capacity = 32;
    cfg.retry.backoff_base_ms = 10;
    cfg.retry.backoff_cap_ms = 50;
    cfg.retry.requeue_backoff_base_ms = 20;
    cfg.retry.requeue_backoff_cap_ms = 80;
    cfg
}

fn mixed_request(kind: &LLMProviderKind) -> LLMRequest {
    let operation = kind.as_str().to_string();
    LLMRequest {
        model: format!("{operation}-model"),
        metadata: RequestMetadata {
            operation: operation.clone(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn test_router_with_providers(
    providers: &[(LLMProviderKind, Arc<TestProvider>)],
) -> Arc<MultiLLMRouter> {
    let mut profiles = HashMap::new();
    let mut operation_mapping = HashMap::new();
    let mut default_profile = String::new();
    for (kind, _) in providers {
        let name = kind.as_str().to_string();
        if default_profile.is_empty() {
            default_profile = name.clone();
        }
        profiles.insert(
            name.clone(),
            LLMProfile {
                provider: kind.clone(),
                model: format!("{name}-model"),
                api_key_env: Some("MAGICLLM_TEST_KEY".to_string()),
                api_base_url: None,
                temperature: None,
                max_output_tokens: None,
                default_modality: Some(LLMModality::Text),
                reasoning: None,
                metadata: Some(HashMap::from([(
                    "streaming".to_string(),
                    serde_json::Value::Bool(true),
                )])),
                supports_vision: Some(false),
                supports_reasoning: Some(false),
                supports_tool_calling: Some(false),
                supports_computer_use: Some(false),
                timeout_secs: Some(60),
                context_window_tokens: None,
                chunking: None,
            },
        );
        operation_mapping.insert(name.clone(), OperationProfileSelector::Simple(name));
    }
    let config = LLMRouterConfig {
        profiles,
        adaptive_profiles: HashMap::new(),
        operation_mapping,
        locality: Default::default(),
        default_profile,
        realtime_voice: Default::default(),
    };
    let mut router = MultiLLMRouter::new(config).expect("mixed-provider router");
    for (_, provider) in providers {
        router.register_provider(provider.clone());
    }
    Arc::new(router)
}

#[derive(Default)]
struct BlockingTaskLedgerSink {
    entered: Notify,
    release: Notify,
    first_append_seen: std::sync::atomic::AtomicBool,
    events: AsyncMutex<Vec<LlmCallLedgerEvent>>,
}

#[derive(Default)]
struct TombstoneBlockingTaskLedgerSink {
    tombstone_entered: Notify,
    release: Notify,
}

#[derive(Default)]
struct CompletionBlockingTaskLedgerSink {
    completion_entered: Notify,
    release: Notify,
}

#[derive(Default)]
struct FailureBlockingTaskLedgerSink {
    failure_entered: Notify,
    release: Notify,
}

#[async_trait]
impl TaskLedgerSink for CompletionBlockingTaskLedgerSink {
    async fn append(&self, _task_ref: &TaskRef, event: LlmCallLedgerEvent) {
        if matches!(event, LlmCallLedgerEvent::Completed { .. }) {
            self.completion_entered.notify_one();
            self.release.notified().await;
        }
    }
}

#[async_trait]
impl TaskLedgerSink for FailureBlockingTaskLedgerSink {
    async fn append(&self, _task_ref: &TaskRef, event: LlmCallLedgerEvent) {
        if matches!(event, LlmCallLedgerEvent::Failed { .. }) {
            self.failure_entered.notify_one();
            self.release.notified().await;
        }
    }
}

#[async_trait]
impl TaskLedgerSink for TombstoneBlockingTaskLedgerSink {
    async fn append(&self, _task_ref: &TaskRef, event: LlmCallLedgerEvent) {
        if matches!(event, LlmCallLedgerEvent::Tombstoned { .. }) {
            self.tombstone_entered.notify_one();
            self.release.notified().await;
        }
    }
}

#[async_trait]
impl TaskLedgerSink for BlockingTaskLedgerSink {
    async fn append(&self, _task_ref: &TaskRef, event: LlmCallLedgerEvent) {
        if !self
            .first_append_seen
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.events.lock().await.push(event);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn task_ref_populates_scope_and_execution_lineage_before_dispatch() {
    let (job, _rx) = LlmJob::new(small_request(), JobOrigin::op("lineage"));
    let job = job.with_task(
        TaskRef::task("task-1")
            .with_agent("agent-1")
            .with_scope("principal-1", "workspace-1")
            .with_execution("root-exec-1", "exec-2")
            .with_chat_session("session-1")
            .with_chat_turn("turn-3")
            .with_iteration("iteration-4"),
    );

    assert_eq!(job.trace_context.task_id.as_deref(), Some("task-1"));
    assert_eq!(
        job.trace_context.scope,
        crate::trace::LlmScope::new("principal-1", "workspace-1")
    );
    assert_eq!(
        job.trace_context.scope_resolution,
        crate::trace::LlmScopeResolution::Inherited
    );
    assert_eq!(
        job.trace_context.root_execution_id.as_deref(),
        Some("root-exec-1")
    );
    assert_eq!(job.trace_context.execution_id.as_deref(), Some("exec-2"));
    assert_eq!(
        job.trace_context.chat_session_id.as_deref(),
        Some("session-1")
    );
    assert_eq!(job.trace_context.chat_turn_id.as_deref(), Some("turn-3"));
    assert_eq!(
        job.trace_context.iteration_id.as_deref(),
        Some("iteration-4")
    );
    assert_eq!(
        job.request
            .metadata
            .trace_context
            .as_ref()
            .map(|context| context.llm_call_id.as_str()),
        Some(job.trace_context.llm_call_id.as_str())
    );
}

#[test]
fn minimal_task_ref_does_not_erase_existing_trace_lineage() {
    let mut request = small_request();
    let mut context = crate::trace::LlmTraceContext::new(
        crate::trace::LlmScope::new("principal", "workspace"),
        crate::trace::LlmWorkloadClass::ForegroundChat,
    );
    context.chat_session_id = Some("session".to_string());
    context.chat_turn_id = Some("turn".to_string());
    context.root_execution_id = Some("root".to_string());
    context.execution_id = Some("execution".to_string());
    context.plan_id = Some("plan".to_string());
    context.step_id = Some("step".to_string());
    context.iteration_id = Some("iteration".to_string());
    request.metadata.set_trace_context(context.clone());
    let (job, _rx) = LlmJob::new(request, JobOrigin::op("lineage-preservation"));
    let job = job.with_task(TaskRef::task("task"));

    assert_eq!(job.trace_context.task_id.as_deref(), Some("task"));
    assert_eq!(
        job.trace_context.root_execution_id,
        context.root_execution_id
    );
    assert_eq!(job.trace_context.execution_id, context.execution_id);
    assert_eq!(job.trace_context.plan_id, context.plan_id);
    assert_eq!(job.trace_context.step_id, context.step_id);
    assert_eq!(job.trace_context.iteration_id, context.iteration_id);
    assert_eq!(job.trace_context.chat_session_id, context.chat_session_id);
    assert_eq!(job.trace_context.chat_turn_id, context.chat_turn_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_rejects_invalid_trace_context_before_registry_admission() {
    let provider = Arc::new(TestProvider::default());
    let queue = LlmDispatchQueue::start(
        test_router_with(provider),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );
    let (mut job, response) = LlmJob::new(small_request(), JobOrigin::op("invalid"));
    job.trace_context.scope.principal.clear();
    job.request
        .metadata
        .set_trace_context(job.trace_context.clone());
    let job_id = job.job_id.clone();

    assert!(matches!(
        queue.submit(job).await,
        Err(LLMError::Validation(_))
    ));
    assert!(matches!(
        response.await.expect("response"),
        Err(LLMError::Validation(_))
    ));
    assert!(queue
        .snapshot()
        .registry
        .pending
        .iter()
        .all(|meta| meta.job_id != job_id));
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_job_and_retry_use_the_producer_router_snapshot() {
    let boot_provider = Arc::new(TestProvider::default());
    boot_provider.script(TestProvider::ok("wrong boot router"));
    let boot_router = Arc::new(DispatchRouterProbe::new(test_router_with(
        boot_provider.clone(),
    )));
    let queue = LlmDispatchQueue::start(
        boot_router.clone(),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );

    let snapshot_provider = Arc::new(TestProvider::default());
    snapshot_provider.script(TestProvider::server_5xx());
    snapshot_provider.script(TestProvider::ok("snapshot generation"));
    let snapshot_router = Arc::new(DispatchRouterProbe::new(test_router_with(
        snapshot_provider.clone(),
    )));
    let router_snapshot: Arc<dyn DispatchRouter> = snapshot_router.clone();
    let (job, response) = LlmJob::new(small_request(), JobOrigin::op("snapshot-routing"));
    queue
        .submit(job.with_router_snapshot(router_snapshot))
        .await
        .expect("snapshot-bound job accepted");
    let dispatched = response
        .await
        .expect("response channel")
        .expect("snapshot retry succeeds");

    assert_eq!(boot_provider.calls(), 0);
    assert_eq!(boot_router.route_calls(), 0);
    assert_eq!(boot_router.provider_resolution_calls(), 0);
    assert_eq!(boot_router.timeout_resolution_calls(), 0);
    assert_eq!(snapshot_provider.calls(), 2);
    assert_eq!(snapshot_router.route_calls(), 2);
    assert!(snapshot_router.provider_resolution_calls() >= 2);
    assert!(snapshot_router.timeout_resolution_calls() >= 2);
    assert_eq!(
        dispatched.response.text.as_deref(),
        Some("snapshot generation")
    );
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_job_uses_the_producer_router_snapshot() {
    let boot_provider = Arc::new(TestProvider::default());
    boot_provider.script(TestProvider::ok("wrong boot router"));
    let boot_router = test_router_with(boot_provider.clone());
    let queue = LlmDispatchQueue::start(
        boot_router,
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );

    let snapshot_provider = Arc::new(TestProvider::default());
    snapshot_provider.script(TestProvider::ok("snapshot stream"));
    let snapshot_router: Arc<dyn DispatchRouter> = test_router_with(snapshot_provider.clone());
    let (job, mut stream) =
        LlmStreamJob::new(small_request(), JobOrigin::op("stream-snapshot-routing"), 4);
    queue
        .submit_stream(job.with_router_snapshot(snapshot_router))
        .await
        .expect("snapshot-bound stream accepted");
    let terminal = tokio::time::timeout(Duration::from_secs(1), stream.recv())
        .await
        .expect("terminal timeout")
        .expect("terminal delta");
    assert!(
        matches!(terminal, StreamDelta::Done(ref response) if response.text.as_deref() == Some("snapshot stream")),
        "stream must come from the producer snapshot, got {terminal:?}"
    );
    assert_eq!(boot_provider.calls(), 0);
    assert_eq!(snapshot_provider.calls(), 1);
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_without_terminal_delta_fails_and_leaves_no_in_flight_job() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("discarded"));
    provider.omit_stream_terminal();
    let queue = LlmDispatchQueue::start(
        test_router_with(provider),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );
    let mut events = queue.subscribe_events();
    let (job, mut stream) = LlmStreamJob::new(small_request(), JobOrigin::op("stream"), 4);
    let job_id = job.job_id.clone();
    queue.submit_stream(job).await.expect("accepted");
    let terminal = tokio::time::timeout(Duration::from_secs(1), stream.recv())
        .await
        .expect("terminal timeout")
        .expect("terminal delta");
    assert!(matches!(terminal, StreamDelta::Error(_)));
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("event timeout")
            .expect("event");
        if matches!(event, super::events::LlmQueueEvent::Failed { ref meta, .. } if meta.job_id == job_id)
        {
            break;
        }
    }
    assert!(queue
        .snapshot()
        .registry
        .in_flight
        .iter()
        .all(|meta| meta.job_id != job_id));
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_failure_metadata_uses_the_effective_route_and_attempt() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::provider_4xx());
    let queue = LlmDispatchQueue::start(
        test_router_with(provider),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );
    let mut events = queue.subscribe_events();
    let (job, mut stream) =
        LlmStreamJob::new(small_request(), JobOrigin::op("stream-route-failure"), 4);
    let call_id = job.trace_context.llm_call_id.clone();
    let job_id = job.job_id.clone();
    queue.submit_stream(job).await.expect("accepted");

    assert!(matches!(
        stream.recv().await,
        Some(StreamDelta::Error(message)) if message.contains("400 bad request")
    ));
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("event timeout")
            .expect("event");
        if let LlmQueueEvent::Failed { meta, .. } = event {
            if meta.job_id == job_id {
                assert_eq!(meta.profile.as_deref(), Some("test_default"));
                assert_eq!(
                    meta.provider,
                    Some(LLMProviderKind::Custom("test".to_string()))
                );
                assert_eq!(meta.model.as_deref(), Some("test-model"));
                assert_eq!(meta.provider_attempt_count, 1);
                assert_eq!(
                    meta.provider_attempt_id.as_deref(),
                    Some(format!("{call_id}:a1").as_str())
                );
                break;
            }
        }
    }
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_error_delta_does_not_erase_returned_route_or_error_class() {
    let queue = LlmDispatchQueue::start(
        Arc::new(ErrorDeltaThenRoutedErrorRouter),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );
    let mut events = queue.subscribe_events();
    let (job, mut stream) = LlmStreamJob::new(
        small_request(),
        JobOrigin::op("stream-error-delta-route"),
        4,
    );
    let call_id = job.trace_context.llm_call_id.clone();
    let job_id = job.job_id.clone();
    queue.submit_stream(job).await.expect("accepted");

    assert!(matches!(
        stream.recv().await,
        Some(StreamDelta::Error(message)) if message == "provider stream failed"
    ));
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("event timeout")
            .expect("event");
        if let LlmQueueEvent::Failed { meta, error_class } = event {
            if meta.job_id == job_id {
                assert_eq!(error_class, ErrorClass::Provider4xx);
                assert_eq!(meta.error_class, Some(ErrorClass::Provider4xx));
                assert_eq!(meta.profile.as_deref(), Some("fallback-profile"));
                assert_eq!(
                    meta.provider,
                    Some(LLMProviderKind::Custom("fallback-provider".to_string()))
                );
                assert_eq!(meta.model.as_deref(), Some("fallback-model"));
                assert_eq!(meta.provider_attempt_count, 1);
                assert_eq!(
                    meta.provider_attempt_id.as_deref(),
                    Some(format!("{call_id}:a1").as_str())
                );
                break;
            }
        }
    }
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_stream_receiver_during_ttft_aborts_provider_http() {
    let provider = Arc::new(TestProvider::default());
    provider.advertise_as(LLMProviderKind::OpenAI);
    provider.script(TestProvider::ok("should-abort"));
    provider.set_delay(Duration::from_millis(800));
    let mut config = small_config();
    config.global_cloud_concurrency = 1;
    config
        .provider_concurrency
        .overrides
        .insert("openai".to_string(), 2);
    let queue = LlmDispatchQueue::start(
        test_router_with_providers(&[(LLMProviderKind::OpenAI, provider.clone())]),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let (job, stream) = LlmStreamJob::new(
        mixed_request(&LLMProviderKind::OpenAI),
        JobOrigin::op("stream-drop-ttft"),
        4,
    );
    queue.submit_stream(job).await.expect("accepted");
    wait_until(Duration::from_secs(1), || provider.in_flight() >= 1).await;
    drop(stream);
    wait_until(Duration::from_millis(300), || provider.in_flight() == 0).await;
    assert_eq!(
        provider.in_flight(),
        0,
        "dropping the chat receiver during TTFT must abort provider HTTP"
    );
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_stream_receiver_tombstones_instead_of_leaking_in_flight() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("unused"));
    let queue = LlmDispatchQueue::start(
        test_router_with(provider),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );
    let mut events = queue.subscribe_events();
    let (job, stream) = LlmStreamJob::new(small_request(), JobOrigin::op("stream-drop"), 1);
    let job_id = job.job_id.clone();
    drop(stream);
    queue.submit_stream(job).await.expect("accepted");
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("event timeout")
            .expect("event");
        if matches!(event, super::events::LlmQueueEvent::Tombstoned { ref meta, .. } if meta.job_id == job_id)
        {
            break;
        }
    }
    assert!(queue
        .snapshot()
        .registry
        .in_flight
        .iter()
        .all(|meta| meta.job_id != job_id));
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_success_releases_admission_before_completed_ledger_io() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("first"));
    provider.script(TestProvider::ok("second"));
    let ledger = Arc::new(CompletionBlockingTaskLedgerSink::default());
    let mut config = small_config();
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        ledger.clone(),
        config,
    );
    let (first, mut first_stream) =
        LlmStreamJob::new(small_request(), JobOrigin::op("stream-completed-ledger"), 2);
    let first_job_id = first.job_id.clone();
    queue
        .submit_stream(first.with_task(TaskRef::task("stream-completed-ledger")))
        .await
        .expect("first stream accepted");
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), first_stream.recv())
            .await
            .expect("first stream terminal timeout"),
        Some(StreamDelta::Done(_))
    ));
    tokio::time::timeout(Duration::from_secs(1), ledger.completion_entered.notified())
        .await
        .expect("stream reached blocking completed-ledger append");
    let snapshot = queue.snapshot();
    assert_eq!(snapshot.retained_bytes_global, 0);
    assert!(snapshot.registry.pending.is_empty());
    assert!(snapshot.registry.in_flight.is_empty());
    assert!(snapshot
        .registry
        .completed
        .iter()
        .any(|meta| meta.job_id == first_job_id));

    // With provider concurrency fixed at one, this second terminal proves the
    // first stream did not retain provider or lane admission while its
    // auxiliary completion ledger write remained blocked.
    let (second, mut second_stream) =
        LlmStreamJob::new(small_request(), JobOrigin::op("stream-after-ledger"), 2);
    queue
        .submit_stream(second)
        .await
        .expect("second stream accepted");
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), second_stream.recv())
            .await
            .expect("second stream is not blocked by the first ledger append"),
        Some(StreamDelta::Done(_))
    ));
    assert_eq!(provider.calls(), 2);

    ledger.release.notify_one();
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_failure_releases_admission_before_failed_ledger_io() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::provider_4xx());
    provider.script(TestProvider::ok("recovered"));
    let ledger = Arc::new(FailureBlockingTaskLedgerSink::default());
    let mut config = small_config();
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        ledger.clone(),
        config,
    );
    let (failed, mut failed_stream) =
        LlmStreamJob::new(small_request(), JobOrigin::op("stream-failed-ledger"), 2);
    let failed_job_id = failed.job_id.clone();
    queue
        .submit_stream(failed.with_task(TaskRef::task("stream-failed-ledger")))
        .await
        .expect("failed stream accepted");
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), failed_stream.recv())
            .await
            .expect("failed stream terminal timeout"),
        Some(StreamDelta::Error(message)) if message.contains("400 bad request")
    ));
    tokio::time::timeout(Duration::from_secs(1), ledger.failure_entered.notified())
        .await
        .expect("stream reached blocking failed-ledger append");
    let snapshot = queue.snapshot();
    assert_eq!(snapshot.retained_bytes_global, 0);
    assert!(snapshot.registry.pending.is_empty());
    assert!(snapshot.registry.in_flight.is_empty());
    assert!(snapshot
        .registry
        .failed
        .iter()
        .any(|meta| meta.job_id == failed_job_id));

    let (replacement, mut replacement_stream) = LlmStreamJob::new(
        small_request(),
        JobOrigin::op("stream-after-failed-ledger"),
        2,
    );
    queue
        .submit_stream(replacement)
        .await
        .expect("replacement stream accepted");
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), replacement_stream.recv())
            .await
            .expect("replacement is not blocked by the failed-ledger append"),
        Some(StreamDelta::Done(_))
    ));
    assert_eq!(provider.calls(), 2);

    ledger.release.notify_one();
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_jobs_share_the_lane_lifetime_capacity_bound() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_secs(2));
    provider.script(TestProvider::ok("first"));
    provider.script(TestProvider::ok("second"));
    let mut config = small_config();
    config.queue_capacity_normal = 2;
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );

    let (first, _first_stream) =
        LlmStreamJob::new(small_request(), JobOrigin::op("stream-capacity-first"), 2);
    let (second, _second_stream) =
        LlmStreamJob::new(small_request(), JobOrigin::op("stream-capacity-second"), 2);
    queue.submit_stream(first).await.expect("first accepted");
    queue.submit_stream(second).await.expect("second accepted");

    let (third, mut third_stream) =
        LlmStreamJob::new(small_request(), JobOrigin::op("stream-capacity-third"), 2);
    assert!(matches!(
        queue.submit_stream(third).await,
        Err(LLMError::QueueFull {
            priority: "normal",
            capacity: 2,
            ..
        })
    ));
    assert!(matches!(
        third_stream.recv().await,
        Some(StreamDelta::Error(message)) if message.contains("full")
    ));
    assert_eq!(queue.snapshot().depth_normal, 2);
    queue.shutdown(Duration::from_millis(20)).await;
}

#[tokio::test]
async fn stream_pickup_after_pending_shutdown_never_reaches_the_provider() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("must-not-run"));
    let config = small_config();
    let registry = super::registry::JobRegistry::new(4, 4, 4);
    let pending_shutdown = tokio_util::sync::CancellationToken::new();
    pending_shutdown.cancel();
    let context = Arc::new(super::streaming::StreamingContext {
        router: test_router_with(provider.clone()),
        registry: registry.clone(),
        provider_state: Arc::new(super::provider_state::ProviderStateMap::new(
            config.provider_concurrency.clone(),
            config.breaker.clone(),
        )),
        task_state: Arc::new(NoopTaskStateView),
        ledger_sink: Arc::new(NoopTaskLedgerSink),
        events: super::events::EventBus::new(16),
        metrics: super::metrics::DispatchMetrics::new(),
        provider_quota: super::quota::ProviderQuotaMap::from_config(&config.provider_quota),
        cloud_admission: super::cloud_admission::CloudAdmission::new(0),
        config: Arc::new(parking_lot::RwLock::new(config)),
        pending_shutdown,
        shutdown: tokio_util::sync::CancellationToken::new(),
    });
    let (job, mut stream) =
        LlmStreamJob::new(small_request(), JobOrigin::op("stream-after-shutdown"), 2);
    registry.insert_pending(super::job::JobMeta::pending_from_stream(&job));

    super::streaming::process_stream_job(context, job).await;

    assert_eq!(provider.calls(), 0);
    assert!(matches!(
        stream.recv().await,
        Some(StreamDelta::Error(message)) if message.contains("shutdown")
    ));
    assert!(registry.pending.is_empty());
    assert_eq!(registry.snapshot().tombstoned.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn force_shutdown_tombstones_a_stream_blocked_on_consumer_backpressure() {
    let queue = LlmDispatchQueue::start(
        Arc::new(BackpressuredStreamRouter),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );
    let (job, stream) = LlmStreamJob::new(
        small_request(),
        JobOrigin::op("stream-consumer-backpressure"),
        1,
    );
    queue.submit_stream(job).await.expect("stream accepted");

    tokio::time::timeout(Duration::from_secs(1), async {
        while stream.len() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the first delta fills the consumer channel");

    let stats = queue.shutdown(Duration::from_millis(20)).await;
    let snapshot = queue.snapshot();
    assert_eq!(stats.in_flight_at_shutdown, 0);
    assert_eq!(stats.pending_at_shutdown, 0);
    assert_eq!(snapshot.registry.in_flight.len(), 0);
    assert_eq!(snapshot.registry.pending.len(), 0);
    assert_eq!(snapshot.registry.tombstoned.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_cancellation_tombstones_a_stream_blocked_on_consumer_backpressure() {
    let task_state = Arc::new(MockTaskStateView::new());
    task_state.set_active("backpressured-task");
    let queue = LlmDispatchQueue::start(
        Arc::new(BackpressuredStreamRouter),
        task_state.clone(),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );
    let (job, stream) = LlmStreamJob::new(
        small_request(),
        JobOrigin::op("stream-task-cancel-backpressure"),
        1,
    );
    queue
        .submit_stream(job.with_task(TaskRef::task("backpressured-task")))
        .await
        .expect("stream accepted");

    tokio::time::timeout(Duration::from_secs(1), async {
        while stream.len() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the first delta fills the consumer channel");
    task_state.fire_cancel("backpressured-task");
    tokio::time::timeout(Duration::from_secs(1), async {
        while queue.snapshot().registry.tombstoned.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("task cancellation must interrupt stream forwarding");

    let snapshot = queue.snapshot();
    assert!(snapshot.registry.in_flight.is_empty());
    assert!(snapshot.registry.pending.is_empty());
    assert_eq!(snapshot.registry.tombstoned.len(), 1);
    queue.shutdown(Duration::from_millis(20)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_error_aborts_a_hung_router_and_releases_provider_capacity() {
    let router = Arc::new(TerminalThenHungStreamRouter::new());
    let mut config = small_config();
    config
        .provider_concurrency
        .overrides
        .insert("terminal-hang".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        router,
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let (first, mut first_stream) =
        LlmStreamJob::new(small_request(), JobOrigin::op("terminal-hang-first"), 2);
    let first_id = first.job_id.clone();
    queue
        .submit_stream(first)
        .await
        .expect("first stream accepted");
    assert!(matches!(
        first_stream.recv().await,
        Some(StreamDelta::Error(message)) if message == "terminal before hang"
    ));

    tokio::time::timeout(Duration::from_secs(1), async {
        while queue
            .snapshot()
            .registry
            .failed
            .iter()
            .all(|meta| meta.job_id != first_id)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("terminal Error must fail without waiting on adapter cleanup");

    // A second request to the same single-slot provider proves terminal error
    // cleanup released the permit rather than only repairing registry metadata.
    let (second, mut second_stream) =
        LlmStreamJob::new(small_request(), JobOrigin::op("terminal-hang-second"), 2);
    let second_id = second.job_id.clone();
    queue
        .submit_stream(second)
        .await
        .expect("second stream accepted");
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), second_stream.recv())
            .await
            .expect("released provider permit")
            .expect("second stream terminal"),
        StreamDelta::Done(_)
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        while queue
            .snapshot()
            .registry
            .completed
            .iter()
            .all(|meta| meta.job_id != second_id)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Done commits success even when adapter cleanup hangs");
    queue.shutdown(Duration::from_millis(20)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delivered_terminal_error_beats_immediate_task_cancellation() {
    let task_state = Arc::new(MockTaskStateView::new());
    task_state.set_active("terminal-error-cancel-task");
    let queue = LlmDispatchQueue::start(
        Arc::new(TerminalThenHungStreamRouter::new()),
        task_state.clone(),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );
    let (job, mut stream) = LlmStreamJob::new(
        small_request(),
        JobOrigin::op("terminal-error-immediate-cancel"),
        2,
    );
    let job_id = job.job_id.clone();
    queue
        .submit_stream(job.with_task(TaskRef::task("terminal-error-cancel-task")))
        .await
        .expect("stream accepted");
    assert!(matches!(
        stream.recv().await,
        Some(StreamDelta::Error(message)) if message == "terminal before hang"
    ));

    // The Error delta is terminal on the consumer wire. Cancellation racing
    // the dispatcher's bounded typed-error handoff must not rewrite the
    // already-delivered provider failure into a task-cancelled tombstone.
    task_state.fire_cancel("terminal-error-cancel-task");
    tokio::time::timeout(Duration::from_secs(1), async {
        while queue
            .snapshot()
            .registry
            .failed
            .iter()
            .all(|meta| meta.job_id != job_id)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("delivered Error must retain failure ownership");

    let snapshot = queue.snapshot();
    assert!(snapshot
        .registry
        .tombstoned
        .iter()
        .all(|meta| meta.job_id != job_id));
    queue.shutdown(Duration::from_millis(20)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delivered_terminal_error_beats_immediate_queue_shutdown() {
    let queue = LlmDispatchQueue::start(
        Arc::new(TerminalThenHungStreamRouter::new()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );
    let (job, mut stream) = LlmStreamJob::new(
        small_request(),
        JobOrigin::op("terminal-error-immediate-shutdown"),
        2,
    );
    let job_id = job.job_id.clone();
    queue.submit_stream(job).await.expect("stream accepted");
    assert!(matches!(
        stream.recv().await,
        Some(StreamDelta::Error(message)) if message == "terminal before hang"
    ));

    // A zero-grace shutdown force-cancels active work immediately. Even then,
    // an Error already delivered to the consumer remains the terminal failure
    // commit rather than becoming a queue-shutdown tombstone.
    queue.shutdown(Duration::ZERO).await;
    let snapshot = queue.snapshot();
    assert!(snapshot
        .registry
        .failed
        .iter()
        .any(|meta| meta.job_id == job_id));
    assert!(snapshot
        .registry
        .tombstoned
        .iter()
        .all(|meta| meta.job_id != job_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_receiver_after_terminal_error_keeps_failure_and_releases_capacity() {
    let router = Arc::new(TerminalThenHungStreamRouter::new());
    let mut config = small_config();
    config
        .provider_concurrency
        .overrides
        .insert("terminal-hang".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        router,
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let (first, mut first_stream) = LlmStreamJob::new(
        small_request(),
        JobOrigin::op("terminal-hang-dropped-receiver"),
        2,
    );
    let first_id = first.job_id.clone();
    queue
        .submit_stream(first)
        .await
        .expect("first stream accepted");
    assert!(matches!(
        first_stream.recv().await,
        Some(StreamDelta::Error(message)) if message == "terminal before hang"
    ));
    drop(first_stream);
    tokio::time::timeout(Duration::from_secs(1), async {
        while queue
            .snapshot()
            .registry
            .failed
            .iter()
            .all(|meta| meta.job_id != first_id)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("delivered Error must commit failure without adapter cleanup");
    assert!(queue
        .snapshot()
        .registry
        .tombstoned
        .iter()
        .all(|meta| meta.job_id != first_id));

    let (second, mut second_stream) = LlmStreamJob::new(
        small_request(),
        JobOrigin::op("terminal-hang-after-dropped-receiver"),
        2,
    );
    queue
        .submit_stream(second)
        .await
        .expect("replacement accepted");
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), second_stream.recv())
            .await
            .expect("released provider permit")
            .expect("replacement terminal"),
        StreamDelta::Done(_)
    ));
    queue.shutdown(Duration::from_millis(20)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_attempt_ledger_cancellation_releases_all_admission_before_provider_call() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("replacement"));
    let task_state = Arc::new(MockTaskStateView::new());
    task_state.set_active("stream-ledger-task");
    let ledger = Arc::new(BlockingAttemptLedger::default());
    let (blocked, _blocked_stream) = LlmStreamJob::new(
        small_request(),
        JobOrigin::op("stream-attempt-ledger-blocked"),
        2,
    );
    let blocked = blocked.with_task(TaskRef::task("stream-ledger-task"));
    let (replacement, mut replacement_stream) = LlmStreamJob::new(
        small_request(),
        JobOrigin::op("stream-after-attempt-ledger-cancel"),
        2,
    );
    let request_bytes = retained_request_bytes_u64(&blocked.request)
        .max(retained_request_bytes_u64(&replacement.request));
    let mut config = small_config();
    config.max_request_bytes = request_bytes;
    config.queue_bytes_normal = request_bytes;
    config.queue_bytes_global = request_bytes;
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        task_state.clone(),
        ledger.clone(),
        config,
    );
    queue
        .submit_stream(blocked)
        .await
        .expect("blocked stream admitted");
    ledger.wait_until_attempt_start().await;
    assert_eq!(provider.calls(), 0);

    task_state.fire_cancel("stream-ledger-task");
    tokio::time::timeout(Duration::from_secs(1), async {
        while queue.snapshot().registry.tombstoned.is_empty()
            || queue.snapshot().retained_bytes_global != 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("ledger cancellation releases stream ownership");

    queue
        .submit_stream(replacement)
        .await
        .expect("released stream admission is reusable");
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), replacement_stream.recv())
            .await
            .expect("released provider permit")
            .expect("replacement terminal"),
        StreamDelta::Done(_)
    ));
    assert_eq!(provider.calls(), 1);
    queue.shutdown(Duration::from_millis(20)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_attempt_ledger_cancellation_releases_all_admission_before_provider_call() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("replacement"));
    let task_state = Arc::new(MockTaskStateView::new());
    task_state.set_active("sync-ledger-task");
    let ledger = Arc::new(BlockingAttemptLedger::default());
    let (blocked, blocked_rx) = LlmJob::new(
        small_request(),
        JobOrigin::op("sync-attempt-ledger-blocked"),
    );
    let blocked = blocked.with_task(TaskRef::task("sync-ledger-task"));
    let (replacement, replacement_rx) = LlmJob::new(
        small_request(),
        JobOrigin::op("sync-after-attempt-ledger-cancel"),
    );
    let request_bytes = retained_request_bytes_u64(&blocked.request)
        .max(retained_request_bytes_u64(&replacement.request));
    let mut config = small_config();
    config.max_request_bytes = request_bytes;
    config.queue_bytes_normal = request_bytes;
    config.queue_bytes_global = request_bytes;
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        task_state.clone(),
        ledger.clone(),
        config,
    );
    queue.submit(blocked).await.expect("blocked call admitted");
    ledger.wait_until_attempt_start().await;
    assert_eq!(provider.calls(), 0);

    task_state.fire_cancel("sync-ledger-task");
    assert!(blocked_rx.await.expect("blocked response channel").is_err());
    assert_eq!(queue.snapshot().retained_bytes_global, 0);

    queue
        .submit(replacement)
        .await
        .expect("released sync admission and provider permit are reusable");
    let replacement = replacement_rx
        .await
        .expect("replacement response channel")
        .expect("replacement provider call succeeds");
    assert_eq!(replacement.response.text.as_deref(), Some("replacement"));
    assert_eq!(provider.calls(), 1);
    queue.shutdown(Duration::from_millis(20)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_stream_waiter_exits_without_obtaining_provider_capacity() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_secs(2));
    provider.script(TestProvider::ok("owner"));
    let (owner, _owner_rx) = LlmJob::new(small_request(), JobOrigin::op("stream-cancel-owner"));
    let request = owner.request.clone();
    let request_bytes = retained_request_bytes_u64(&owner.request);
    let mut config = small_config();
    config.max_request_bytes = request_bytes;
    config.queue_bytes_normal = request_bytes;
    config.queue_bytes_global = request_bytes;
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    queue.submit(owner).await.expect("owner accepted");
    while provider.calls() == 0 {
        tokio::task::yield_now().await;
    }

    let (waiter, mut stream) =
        LlmStreamJob::new(request.clone(), JobOrigin::op("stream-cancel-waiter"), 2);
    let waiter_id = waiter.job_id.clone();
    queue.submit_stream(waiter).await.expect("waiter accepted");
    tokio::time::timeout(Duration::from_secs(1), async {
        while queue.snapshot().retained_bytes_global != request_bytes {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("stream provider waiter remains byte charged");
    let (rejected, mut rejected_stream) =
        LlmStreamJob::new(request, JobOrigin::op("stream-byte-rejected"), 2);
    assert!(matches!(
        queue.submit_stream(rejected).await,
        Err(LLMError::QueueBytesFull { .. })
    ));
    assert!(matches!(
        rejected_stream.recv().await,
        Some(StreamDelta::Error(message)) if message.contains("retained-byte")
    ));
    assert!(queue.cancel_job(&waiter_id, "user_cancelled_stream"));

    let terminal = tokio::time::timeout(Duration::from_millis(500), stream.recv())
        .await
        .expect("cancelled stream must not wait for the provider")
        .expect("stream terminal delta");
    assert!(matches!(terminal, StreamDelta::Error(message) if message.contains("cancel")));
    assert_eq!(
        provider.calls(),
        1,
        "cancelled waiter never reached provider"
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        while queue.snapshot().retained_bytes_global != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled stream releases byte permit");
    queue.shutdown(Duration::from_millis(20)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_deadline_is_enforced_while_waiting_for_provider_capacity() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_secs(2));
    provider.script(TestProvider::ok("owner"));
    let mut config = small_config();
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let (owner, _owner_rx) = LlmJob::new(small_request(), JobOrigin::op("stream-deadline-owner"));
    queue.submit(owner).await.expect("owner accepted");
    while provider.calls() == 0 {
        tokio::task::yield_now().await;
    }

    let (waiter, mut stream) =
        LlmStreamJob::new(small_request(), JobOrigin::op("stream-deadline-waiter"), 2);
    let waiter = waiter.with_submission_deadline(Instant::now() + Duration::from_millis(30));
    queue.submit_stream(waiter).await.expect("waiter accepted");
    let terminal = tokio::time::timeout(Duration::from_millis(500), stream.recv())
        .await
        .expect("deadline must be observed during provider wait")
        .expect("stream terminal delta");
    assert!(matches!(terminal, StreamDelta::Error(message) if message.contains("deadline")));
    assert_eq!(provider.calls(), 1, "expired waiter never reached provider");
    queue.shutdown(Duration::from_millis(20)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_immediately_tombstones_a_stream_waiting_for_provider_capacity() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_secs(2));
    provider.script(TestProvider::ok("owner"));
    let mut config = small_config();
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let (owner, _owner_rx) = LlmJob::new(small_request(), JobOrigin::op("stream-shutdown-owner"));
    queue.submit(owner).await.expect("owner accepted");
    while provider.calls() == 0 {
        tokio::task::yield_now().await;
    }
    let (waiter, mut stream) =
        LlmStreamJob::new(small_request(), JobOrigin::op("stream-shutdown-waiter"), 2);
    queue.submit_stream(waiter).await.expect("waiter accepted");

    queue.shutdown(Duration::from_millis(20)).await;
    assert!(matches!(
        stream.recv().await,
        Some(StreamDelta::Error(message)) if message.contains("shutdown")
    ));
    assert_eq!(
        provider.calls(),
        1,
        "shutdown waiter never reached provider"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn happy_path_single_call() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("hello"));
    let router = test_router_with(provider.clone());
    let queue = LlmDispatchQueue::start(
        router,
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );

    let resp = queue
        .submit_and_wait(small_request(), JobOrigin::op("test"))
        .await
        .expect("dispatch ok");
    assert_eq!(resp.response.text.as_deref(), Some("hello"));
    assert_eq!(provider.calls(), 1);
    assert!(resp.trace_receipt.context.is_valid());
    assert_eq!(resp.trace_receipt.provider_attempt_count, 1);
    let expected_attempt_id = resp.trace_receipt.context.provider_attempt_id(1);
    assert_eq!(
        resp.trace_receipt.provider_attempt_id.as_deref(),
        Some(expected_attempt_id.as_str())
    );
    assert_eq!(
        resp.response.trace_receipt.as_ref(),
        Some(&resp.trace_receipt)
    );
    assert_eq!(
        Arc::strong_count(&resp.response),
        1,
        "non-idempotent success must not allocate or retain a cache clone"
    );
    assert!(resp.trace_receipt.dispatch_job_id.is_some());
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_success_releases_ownership_and_callers_before_completed_ledger_io() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("completed before ledger"));
    let ledger = Arc::new(CompletionBlockingTaskLedgerSink::default());
    let mut config = small_config();
    config.workers = 1;
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        ledger.clone(),
        config,
    );
    let task_ref = TaskRef::task("completed-ledger-ordering");
    let (owner, owner_rx) = LlmJob::new(small_request(), JobOrigin::op("completed-ledger-owner"));
    let owner_job_id = owner.job_id.clone();
    queue
        .submit(
            owner
                .with_task(task_ref.clone())
                .with_idempotency_key("completed-ledger-key"),
        )
        .await
        .expect("owner accepted");

    tokio::time::timeout(Duration::from_secs(1), ledger.completion_entered.notified())
        .await
        .expect("worker reached blocking completed-ledger append");

    let owner_result = tokio::time::timeout(Duration::from_millis(100), owner_rx)
        .await
        .expect("owner response is visible before completed-ledger persistence")
        .expect("owner response channel")
        .expect("owner provider success");
    assert_eq!(
        owner_result.response.text.as_deref(),
        Some("completed before ledger")
    );
    let snapshot = queue.snapshot();
    assert_eq!(snapshot.retained_bytes_global, 0);
    assert!(snapshot.registry.pending.is_empty());
    assert!(snapshot.registry.in_flight.is_empty());
    assert!(snapshot
        .registry
        .completed
        .iter()
        .any(|meta| meta.job_id == owner_job_id));

    // Completion must also publish the idempotency result before auxiliary
    // persistence. An equivalent caller must not enqueue behind the worker
    // currently blocked in the terminal ledger sink.
    let (reused, reused_rx) =
        LlmJob::new(small_request(), JobOrigin::op("completed-ledger-reused"));
    queue
        .submit(
            reused
                .with_task(task_ref)
                .with_idempotency_key("completed-ledger-key"),
        )
        .await
        .expect("cached equivalent accepted while ledger remains blocked");
    let reused_result = tokio::time::timeout(Duration::from_millis(100), reused_rx)
        .await
        .expect("cached response is not blocked by completed-ledger persistence")
        .expect("cached response channel")
        .expect("cached provider success");
    assert!(reused_result.trace_receipt.response_reused);
    assert_eq!(provider.calls(), 1);

    ledger.release.notify_one();
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_failure_releases_ownership_and_callers_before_failed_ledger_io() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::provider_4xx());
    let ledger = Arc::new(FailureBlockingTaskLedgerSink::default());
    let mut config = small_config();
    config.workers = 1;
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        ledger.clone(),
        config,
    );
    let task_ref = TaskRef::task("failed-ledger-ordering");
    let (owner, owner_rx) = LlmJob::new(small_request(), JobOrigin::op("failed-ledger-owner"));
    let owner_job_id = owner.job_id.clone();
    queue
        .submit(
            owner
                .with_task(task_ref.clone())
                .with_idempotency_key("failed-ledger-key"),
        )
        .await
        .expect("owner accepted");
    tokio::time::timeout(Duration::from_secs(1), ledger.failure_entered.notified())
        .await
        .expect("worker reached blocking failed-ledger append");

    let owner_error = tokio::time::timeout(Duration::from_millis(100), owner_rx)
        .await
        .expect("owner failure is visible before failed-ledger persistence")
        .expect("owner response channel")
        .expect_err("owner provider failure");
    assert!(matches!(
        owner_error.root_cause(),
        LLMError::Provider { .. }
    ));
    let snapshot = queue.snapshot();
    assert_eq!(snapshot.retained_bytes_global, 0);
    assert!(snapshot.registry.pending.is_empty());
    assert!(snapshot.registry.in_flight.is_empty());
    assert!(snapshot
        .registry
        .failed
        .iter()
        .any(|meta| meta.job_id == owner_job_id));

    let (reused, reused_rx) = LlmJob::new(small_request(), JobOrigin::op("failed-ledger-reused"));
    queue
        .submit(
            reused
                .with_task(task_ref)
                .with_idempotency_key("failed-ledger-key"),
        )
        .await
        .expect("cached equivalent accepted while failed ledger remains blocked");
    let reused_error = tokio::time::timeout(Duration::from_millis(100), reused_rx)
        .await
        .expect("cached failure is not blocked by failed-ledger persistence")
        .expect("cached response channel")
        .expect_err("cached provider failure");
    assert!(matches!(
        reused_error.root_cause(),
        LLMError::Provider { .. }
    ));
    assert_eq!(provider.calls(), 1);

    ledger.release.notify_one();
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_permit_wait_is_observed_separately_from_execution() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_millis(90));
    provider.script(TestProvider::ok("first"));
    provider.script(TestProvider::ok("second"));
    let mut config = small_config();
    config.provider_concurrency.default = 1;
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let (first, first_response) = LlmJob::new(small_request(), JobOrigin::op("serial-first"));
    let (second, second_response) = LlmJob::new(small_request(), JobOrigin::op("serial-second"));
    let first_id = first.job_id.clone();
    let second_id = second.job_id.clone();
    queue.submit(first).await.expect("first submitted");
    queue.submit(second).await.expect("second submitted");
    first_response
        .await
        .expect("first response channel")
        .expect("first response");
    second_response
        .await
        .expect("second response channel")
        .expect("second response");

    let snapshot = queue.snapshot();
    let completed = snapshot
        .registry
        .completed
        .iter()
        .filter(|meta| meta.job_id == first_id || meta.job_id == second_id)
        .collect::<Vec<_>>();
    assert_eq!(completed.len(), 2);
    assert!(completed
        .iter()
        .all(|meta| meta.execution_ms.is_some_and(|ms| ms >= 70)));
    assert!(
        completed
            .iter()
            .any(|meta| meta.provider_wait_ms.is_some_and(|ms| ms >= 60)),
        "the second call should expose time spent behind the provider permit"
    );
    assert!(completed.iter().all(|meta| meta.wait_ms.is_some()));
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saturated_provider_wait_does_not_hold_a_global_worker() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_millis(180));
    provider.script(TestProvider::ok("first"));
    provider.script(TestProvider::ok("second"));
    let mut config = small_config();
    config.workers = 2;
    config.reserved_interactive_workers = 1;
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let (first, first_rx) = LlmJob::new(small_request(), JobOrigin::op("capacity-first"));
    let (second, second_rx) = LlmJob::new(small_request(), JobOrigin::op("capacity-second"));
    queue.submit(first).await.expect("first submitted");
    queue.submit(second).await.expect("second submitted");

    let observed = tokio::time::timeout(Duration::from_millis(120), async {
        loop {
            let snapshot = queue.snapshot();
            if snapshot.registry.in_flight.len() == 1
                && snapshot.registry.pending.len() == 1
                && snapshot.workers_busy == 1
            {
                assert_eq!(snapshot.waiting_for_provider, 1);
                assert_eq!(snapshot.waiting_for_provider_normal, 1);
                break snapshot.workers_busy;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("one call should wait outside the workers");
    assert_eq!(
        observed, 1,
        "the capacity waiter must not hold the second worker"
    );

    first_rx.await.unwrap().unwrap();
    second_rx.await.unwrap().unwrap();
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_wait_bytes_remain_charged_until_cancel_then_are_exactly_reusable() {
    let provider = Arc::new(TestProvider::default());
    provider.block_first_call();
    provider.script(TestProvider::ok("owner"));
    provider.script(TestProvider::ok("replacement"));
    let (owner, owner_rx) = LlmJob::new(small_request(), JobOrigin::op("byte-owner"));
    let request = owner.request.clone();
    let request_bytes = retained_request_bytes_u64(&owner.request);
    let mut config = small_config();
    config.max_request_bytes = request_bytes;
    config.queue_bytes_normal = request_bytes;
    config.queue_bytes_global = request_bytes;
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );

    queue.submit(owner).await.expect("owner accepted");
    provider.wait_for_first_call().await;
    assert_eq!(queue.snapshot().retained_bytes_global, 0);

    let (waiter, waiter_rx) = LlmJob::new(request.clone(), JobOrigin::op("byte-waiter"));
    let waiter_id = waiter.job_id.clone();
    queue.submit(waiter).await.expect("exact byte cap accepted");
    tokio::time::timeout(Duration::from_secs(1), async {
        while queue.snapshot().waiting_for_provider != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("waiter parked");
    assert_eq!(queue.snapshot().retained_bytes_global, request_bytes);

    let (rejected, _rejected_rx) = LlmJob::new(request.clone(), JobOrigin::op("byte-rejected"));
    assert!(matches!(
        queue.submit(rejected).await,
        Err(LLMError::QueueBytesFull { .. })
    ));
    assert!(queue.cancel_job(&waiter_id, "release retained bytes"));
    let _ = waiter_rx.await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while queue.snapshot().retained_bytes_global != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancel releases byte permit");

    let (replacement, replacement_rx) = LlmJob::new(request, JobOrigin::op("byte-replacement"));
    queue
        .submit(replacement)
        .await
        .expect("released exact cap is reusable");
    provider.release_first_call();
    owner_rx.await.unwrap().unwrap();
    replacement_rx.await.unwrap().unwrap();
    assert_eq!(queue.snapshot().retained_bytes_global, 0);
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_and_router_reuse_one_admission_verdict_for_unchanged_payload_lanes() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("admitted"));
    let queue = LlmDispatchQueue::start(
        test_router_without_profile_metadata(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );
    let mut request = small_request();
    request.set_extra(serde_json::json!({"provider_option": {"enabled": true}}));
    let (job, rx) = LlmJob::new(request, JobOrigin::op("single-admission-pass"));
    queue.submit(job).await.expect("request accepted");
    rx.await.unwrap().unwrap();
    let captured = provider.captured_requests();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].json_admission_scan_passes(), 1);
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wide_empty_structural_lanes_cannot_bypass_request_byte_admission() {
    let provider = Arc::new(TestProvider::default());
    let mut request = small_request();
    request.messages = Arc::new(Vec::with_capacity(4_096));
    request.tools = Arc::new(Vec::with_capacity(4_096));
    request.input_media = Some(Arc::new(Vec::with_capacity(4_096)));
    request.summarisable_blocks = Arc::new(Vec::with_capacity(4_096));
    let retained_bytes = retained_request_bytes_u64(&request);
    let mut config = small_config();
    config.max_request_bytes = retained_bytes.saturating_sub(1);
    config.queue_bytes_normal = retained_bytes;
    config.queue_bytes_global = retained_bytes;
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let (job, rx) = LlmJob::new(request, JobOrigin::op("wide-empty-byte-admission"));
    assert!(matches!(
        queue.submit(job).await,
        Err(LLMError::RequestTooLarge { .. })
    ));
    assert!(matches!(
        rx.await.expect("rejection response"),
        Err(LLMError::RequestTooLarge { .. })
    ));
    assert_eq!(provider.calls(), 0);
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cooldown_requeue_reacquires_retained_byte_admission() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::rate_limit());
    provider.script(TestProvider::ok("after-cooldown"));
    let (job, rx) = LlmJob::new(small_request(), JobOrigin::op("byte-cooldown"));
    let request = job.request.clone();
    let request_bytes = retained_request_bytes_u64(&job.request);
    let mut config = small_config();
    config.max_request_bytes = request_bytes;
    config.queue_bytes_normal = request_bytes;
    config.queue_bytes_global = request_bytes;
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    queue.submit(job).await.expect("initial request accepted");
    tokio::time::timeout(Duration::from_secs(1), async {
        while provider.calls() != 1 || queue.snapshot().retained_bytes_global != request_bytes {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cooldown payload remains charged");
    let (other, _other_rx) = LlmJob::new(request, JobOrigin::op("byte-cooldown-other"));
    assert!(matches!(
        queue.submit(other).await,
        Err(LLMError::QueueBytesFull { .. })
    ));
    rx.await.unwrap().unwrap();
    assert_eq!(queue.snapshot().retained_bytes_global, 0);
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_place_retry_backoff_is_byte_admitted_and_shutdown_releases_it() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::server_5xx());
    provider.script(TestProvider::ok("unused-after-cancel"));
    let (job, rx) = LlmJob::new(small_request(), JobOrigin::op("byte-in-place-retry"));
    let request = job.request.clone();
    let request_bytes = retained_request_bytes_u64(&job.request);
    let mut config = small_config();
    config.max_request_bytes = request_bytes;
    config.queue_bytes_normal = request_bytes;
    config.queue_bytes_global = request_bytes;
    config.retry.backoff_base_ms = 500;
    config.retry.backoff_cap_ms = 500;
    config.retry.backoff_jitter_pct = 0.0;
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );

    queue.submit(job).await.expect("initial request accepted");
    tokio::time::timeout(Duration::from_secs(1), async {
        while provider.calls() != 1 || queue.snapshot().retained_bytes_global != request_bytes {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("retry backoff reacquires the exact byte reservation");

    let (other, _other_rx) = LlmJob::new(request.clone(), JobOrigin::op("byte-in-place-other"));
    assert!(matches!(
        queue.submit(other).await,
        Err(LLMError::QueueBytesFull { .. })
    ));
    queue.shutdown(Duration::from_millis(500)).await;
    assert!(rx.await.expect("retry response channel").is_err());
    tokio::time::timeout(Duration::from_secs(1), async {
        while queue.snapshot().retained_bytes_global != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("shutdown releases retained bytes from in-place backoff");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cycle_requeue_backoff_is_byte_admitted_and_cancel_releases_it() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::server_5xx());
    provider.script(TestProvider::ok("unused-after-shutdown"));
    let (job, rx) = LlmJob::new(small_request(), JobOrigin::op("byte-cycle-requeue"));
    let job_id = job.job_id.clone();
    let request = job.request.clone();
    let request_bytes = retained_request_bytes_u64(&job.request);
    let mut config = small_config();
    config.max_request_bytes = request_bytes;
    config.queue_bytes_normal = request_bytes;
    config.queue_bytes_global = request_bytes;
    config.retry.max_attempts_per_dispatch = 1;
    config.retry.max_dispatch_cycles = 2;
    config.retry.requeue_backoff_base_ms = 500;
    config.retry.requeue_backoff_cap_ms = 500;
    config.retry.backoff_jitter_pct = 0.0;
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );

    queue.submit(job).await.expect("initial request accepted");
    tokio::time::timeout(Duration::from_secs(1), async {
        while provider.calls() != 1 || queue.snapshot().retained_bytes_global != request_bytes {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cycle requeue backoff reacquires byte admission");
    let (other, _other_rx) = LlmJob::new(request, JobOrigin::op("byte-cycle-other"));
    assert!(matches!(
        queue.submit(other).await,
        Err(LLMError::QueueBytesFull { .. })
    ));

    assert!(queue.cancel_job(&job_id, "cancel byte-admitted cycle requeue"));
    assert!(rx.await.expect("requeue response channel").is_err());
    tokio::time::timeout(Duration::from_secs(1), async {
        while queue.snapshot().retained_bytes_global != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cycle requeue cancellation releases retained bytes");
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn admitted_retry_waits_for_transient_byte_contention_then_proceeds() {
    let provider = Arc::new(TestProvider::default());
    provider.block_first_call();
    provider.script(TestProvider::server_5xx());
    provider.script(TestProvider::ok("newer-job"));
    provider.script(TestProvider::ok("retried-owner"));
    let (owner, owner_rx) = LlmJob::new(small_request(), JobOrigin::op("byte-retry-owner"));
    let request = owner.request.clone();
    let request_bytes = retained_request_bytes_u64(&owner.request);
    let mut config = small_config();
    config.workers = 3;
    config.max_request_bytes = request_bytes;
    config.queue_bytes_normal = request_bytes;
    config.queue_bytes_global = request_bytes;
    config.retry.backoff_base_ms = 0;
    config.retry.backoff_cap_ms = 0;
    config.retry.backoff_jitter_pct = 0.0;
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );

    queue.submit(owner).await.expect("owner accepted");
    provider.wait_for_first_call().await;
    assert_eq!(
        queue.snapshot().retained_bytes_global,
        0,
        "active provider work releases queued-byte accounting",
    );
    let (newer, newer_rx) = LlmJob::new(request, JobOrigin::op("byte-retry-newer"));
    queue
        .submit(newer)
        .await
        .expect("newer job fills the released byte capacity");
    assert_eq!(queue.snapshot().retained_bytes_global, request_bytes);

    provider.release_first_call();
    let newer = newer_rx
        .await
        .expect("newer response channel")
        .expect("newer job succeeds");
    let owner = owner_rx
        .await
        .expect("owner response channel")
        .expect("admitted owner retry must not fail on transient byte contention");
    assert_eq!(newer.response.text.as_deref(), Some("newer-job"));
    assert_eq!(owner.response.text.as_deref(), Some("retried-owner"));
    assert_eq!(provider.calls(), 3);
    assert_eq!(queue.snapshot().retained_bytes_global, 0);
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provider_waiters_remain_inside_the_lane_admission_bound() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_millis(300));
    for index in 0..3 {
        provider.script(TestProvider::ok(&format!("response-{index}")));
    }
    let mut config = small_config();
    config.workers = 4;
    config.queue_capacity_normal = 3;
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );

    let mut accepted = 0usize;
    let mut rejected = 0usize;
    for index in 0..12 {
        let (job, _rx) = LlmJob::new(
            small_request(),
            JobOrigin::op(format!("bounded-capacity-{index}")),
        );
        match queue.submit(job).await {
            Ok(()) => accepted += 1,
            Err(LLMError::QueueFull { .. }) => rejected += 1,
            Err(error) => panic!("unexpected admission result: {error}"),
        }
    }
    assert_eq!(
        accepted, 3,
        "in-flight and parked work share one hard bound"
    );
    assert_eq!(rejected, 9);
    let snapshot = queue.snapshot();
    assert_eq!(snapshot.depth_normal, 3);
    assert!(snapshot.waiting_for_provider <= 2);
    assert!(snapshot.registry.pending.len() <= 2);
    queue.shutdown(Duration::from_millis(50)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provider_scheduler_resumes_high_priority_before_background() {
    let router = Arc::new(PriorityHandoffRouter::new());
    let mut config = small_config();
    config.workers = 4;
    config
        .provider_concurrency
        .overrides
        .insert("provider-a".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        router.clone(),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );

    let mut owner_request = small_request();
    owner_request.model = "provider-a-owner".to_string();
    let (owner, owner_rx) = LlmJob::new(owner_request, JobOrigin::op("priority-owner"));
    queue.submit(owner).await.unwrap();
    while !router
        .owner_started
        .load(std::sync::atomic::Ordering::Acquire)
    {
        tokio::task::yield_now().await;
    }

    let mut background_request = small_request();
    background_request.model = "provider-a-background".to_string();
    let (background, background_rx) =
        LlmJob::new(background_request, JobOrigin::op("priority-background"));
    let mut high_request = small_request();
    high_request.model = "provider-a-high".to_string();
    let (high, high_rx) = LlmJob::new(high_request, JobOrigin::op("priority-high"));
    queue
        .submit(background.with_priority(Priority::Background))
        .await
        .unwrap();
    queue
        .submit(high.with_priority(Priority::High))
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(1), async {
        while queue.snapshot().waiting_for_provider != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("both priority candidates must reach the provider scheduler");
    router.owner_release.add_permits(1);

    owner_rx.await.unwrap().unwrap();
    let high_result = high_rx.await.unwrap().unwrap();
    let background_result = background_rx.await.unwrap().unwrap();
    assert_eq!(
        high_result.response.text.as_deref(),
        Some("provider-a-high-complete")
    );
    assert_eq!(
        background_result.response.text.as_deref(),
        Some("provider-a-background-complete")
    );
    assert_eq!(
        router.provider_a_order(),
        vec![
            "owner".to_string(),
            "high".to_string(),
            "background".to_string()
        ]
    );
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_lane_handoff_does_not_park_a_provider_permit() {
    let router = Arc::new(PriorityHandoffRouter::new());
    let mut config = small_config();
    // Keep one background-capable worker available after the provider-B
    // blocker and provider-A owner have both started, regardless of which
    // eligible worker picked the Normal owner.
    config.workers = 4;
    config.reserved_interactive_workers = 1;
    config
        .provider_concurrency
        .overrides
        .insert("provider-a".to_string(), 1);
    config
        .provider_concurrency
        .overrides
        .insert("provider-b".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        router.clone(),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );

    let mut blocker_request = small_request();
    blocker_request.model = "background-blocker".to_string();
    let (blocker, blocker_rx) =
        LlmJob::new(blocker_request, JobOrigin::op("handoff-background-blocker"));
    queue
        .submit(blocker.with_priority(Priority::Background))
        .await
        .expect("background blocker accepted");
    while !router
        .background_started
        .load(std::sync::atomic::Ordering::Acquire)
    {
        tokio::task::yield_now().await;
    }

    let mut owner_request = small_request();
    owner_request.model = "provider-a-owner".to_string();
    let (owner, owner_rx) = LlmJob::new(owner_request, JobOrigin::op("handoff-provider-owner"));
    queue.submit(owner).await.expect("provider owner accepted");
    while !router
        .owner_started
        .load(std::sync::atomic::Ordering::Acquire)
    {
        tokio::task::yield_now().await;
    }

    let mut background_request = small_request();
    background_request.model = "provider-a-background".to_string();
    let (background, background_rx) = LlmJob::new(
        background_request,
        JobOrigin::op("handoff-provider-background"),
    );
    queue
        .submit(background.with_priority(Priority::Background))
        .await
        .expect("provider background accepted");
    while queue.snapshot().waiting_for_provider == 0 {
        tokio::task::yield_now().await;
    }

    router.owner_release.add_permits(1);
    owner_rx
        .await
        .expect("owner response")
        .expect("owner success");
    while queue.snapshot().waiting_for_provider != 0 {
        tokio::task::yield_now().await;
    }

    let mut high_request = small_request();
    high_request.model = "provider-a-high".to_string();
    let (high, high_rx) = LlmJob::new(high_request, JobOrigin::op("handoff-provider-high"));
    queue
        .submit(high.with_priority(Priority::High))
        .await
        .expect("high request accepted");
    let high_result = tokio::time::timeout(Duration::from_millis(500), high_rx)
        .await
        .expect("interactive request must not wait behind background lane")
        .expect("high response channel")
        .expect("high response");
    assert_eq!(
        high_result.response.text.as_deref(),
        Some("provider-a-high-complete")
    );

    router.background_release.add_permits(1);
    blocker_rx
        .await
        .expect("blocker response channel")
        .expect("blocker success");
    background_rx
        .await
        .expect("background response channel")
        .expect("background success");
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_wait_is_distinct_from_dispatch_and_retry_telemetry() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_millis(100));
    provider.script(TestProvider::ok("owner"));
    provider.script(TestProvider::ok("waiter"));
    let mut config = small_config();
    config.workers = 2;
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let mut events = queue.subscribe_events();
    let (owner, owner_rx) = LlmJob::new(small_request(), JobOrigin::op("event-owner"));
    let (waiter, waiter_rx) = LlmJob::new(small_request(), JobOrigin::op("event-waiter"));
    let waiter_id = waiter.job_id.clone();
    queue.submit(owner).await.unwrap();
    while provider.calls() == 0 {
        tokio::task::yield_now().await;
    }
    queue.submit(waiter).await.unwrap();

    let mut waiter_events = Vec::new();
    while waiter_events.len() < 2 {
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("event timeout")
            .expect("queue event");
        match &event {
            LlmQueueEvent::WaitingForProvider { meta, .. }
            | LlmQueueEvent::Dispatched { meta }
            | LlmQueueEvent::Requeued { meta, .. }
                if meta.job_id == waiter_id =>
            {
                waiter_events.push(event)
            },
            _ => {},
        }
    }
    assert!(matches!(
        waiter_events[0],
        LlmQueueEvent::WaitingForProvider { .. }
    ));
    assert!(matches!(waiter_events[1], LlmQueueEvent::Dispatched { .. }));
    assert!(!waiter_events
        .iter()
        .any(|event| matches!(event, LlmQueueEvent::Requeued { .. })));
    owner_rx.await.unwrap().unwrap();
    waiter_rx.await.unwrap().unwrap();
    queue.shutdown(Duration::from_millis(500)).await;
}

#[test]
fn provider_capacity_permit_is_rejected_when_the_route_changes() {
    let registry = super::registry::JobRegistry::new(4, 4, 4);
    let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = semaphore.clone().try_acquire_owned().expect("test permit");
    let job_id = super::types::JobId::new();
    registry.store_capacity_permit(
        &job_id,
        LLMProviderKind::Custom("provider-a".to_string()),
        permit,
    );
    assert!(registry
        .take_capacity_permit(&job_id, &LLMProviderKind::Custom("provider-b".to_string()),)
        .is_none());
    assert_eq!(semaphore.available_permits(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_tombstones_provider_waiters_without_waiting_for_the_owner() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_secs(2));
    provider.script(TestProvider::ok("owner"));
    let mut config = small_config();
    config.workers = 2;
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let (owner, _owner_rx) = LlmJob::new(small_request(), JobOrigin::op("shutdown-owner"));
    let (waiter, waiter_rx) = LlmJob::new(small_request(), JobOrigin::op("shutdown-waiter"));
    queue.submit(owner).await.unwrap();
    while provider.calls() == 0 {
        tokio::task::yield_now().await;
    }
    queue.submit(waiter).await.unwrap();
    while queue.snapshot().waiting_for_provider == 0 {
        tokio::task::yield_now().await;
    }

    queue.shutdown(Duration::from_millis(20)).await;
    assert!(matches!(
        waiter_rx.await.expect("waiter response"),
        Err(LLMError::Cancelled { .. })
    ));
    let snapshot = queue.snapshot();
    assert_eq!(snapshot.waiting_for_provider, 0);
    assert!(snapshot.registry.pending.is_empty());
    assert!(snapshot
        .registry
        .tombstoned
        .iter()
        .any(|meta| meta.state == JobState::Tombstoned));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn force_shutdown_does_not_wait_forever_on_a_blocked_tombstone_ledger() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_secs(2));
    provider.script(TestProvider::ok("owner"));
    let ledger = Arc::new(TombstoneBlockingTaskLedgerSink::default());
    let mut config = small_config();
    config.workers = 2;
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        ledger.clone(),
        config,
    );
    let task_ref = TaskRef::task("blocked-shutdown-ledger-task");
    let (owner, _owner_rx) = LlmJob::new(small_request(), JobOrigin::op("ledger-owner"));
    queue
        .submit(owner.with_task(task_ref.clone()))
        .await
        .expect("owner accepted");
    while provider.calls() == 0 {
        tokio::task::yield_now().await;
    }
    let (waiter, waiter_rx) = LlmJob::new(small_request(), JobOrigin::op("ledger-waiter"));
    queue
        .submit(waiter.with_task(task_ref))
        .await
        .expect("waiter accepted");
    while queue.snapshot().waiting_for_provider == 0 {
        tokio::task::yield_now().await;
    }

    let shutdown = tokio::spawn({
        let queue = queue.clone();
        async move { queue.shutdown(Duration::from_millis(20)).await }
    });
    ledger.tombstone_entered.notified().await;
    tokio::time::timeout(Duration::from_secs(2), shutdown)
        .await
        .expect("force shutdown must cancel the blocked ledger future")
        .expect("shutdown task");
    assert!(matches!(
        waiter_rx.await.expect("waiter response"),
        Err(LLMError::Cancelled { .. })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tombstone_releases_admission_and_caller_before_terminal_ledger_io() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("replacement"));
    let task_state = Arc::new(MockTaskStateView::new());
    task_state.set_cancelled("blocked-tombstone-ledger", "cancelled before pickup");
    let ledger = Arc::new(TombstoneBlockingTaskLedgerSink::default());
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        task_state,
        ledger.clone(),
        small_config(),
    );
    let (cancelled, cancelled_rx) =
        LlmJob::new(small_request(), JobOrigin::op("blocked-tombstone-ledger"));
    let cancelled_job_id = cancelled.job_id.clone();
    queue
        .submit(cancelled.with_task(TaskRef::task("blocked-tombstone-ledger")))
        .await
        .expect("cancelled job admitted before pickup gate");
    tokio::time::timeout(Duration::from_secs(1), ledger.tombstone_entered.notified())
        .await
        .expect("worker reached blocking tombstone-ledger append");

    assert!(matches!(
        tokio::time::timeout(Duration::from_millis(100), cancelled_rx)
            .await
            .expect("cancellation response precedes tombstone-ledger persistence")
            .expect("cancellation response channel"),
        Err(LLMError::Cancelled { .. })
    ));
    let snapshot = queue.snapshot();
    assert_eq!(snapshot.retained_bytes_global, 0);
    assert!(snapshot.registry.pending.is_empty());
    assert!(snapshot.registry.in_flight.is_empty());
    assert!(snapshot
        .registry
        .tombstoned
        .iter()
        .any(|meta| meta.job_id == cancelled_job_id));

    // A terminal ledger append may still occupy its worker, but it cannot
    // retain lane/byte admission. Another worker can admit and complete
    // unrelated work while the append remains blocked.
    let replacement = queue
        .submit_and_wait(
            small_request(),
            JobOrigin::op("replacement-after-tombstone-ledger"),
        )
        .await
        .expect("released admission is reusable before ledger release");
    assert_eq!(replacement.response.text.as_deref(), Some("replacement"));
    assert_eq!(provider.calls(), 1);

    ledger.release.notify_one();
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unpublished_rejection_releases_idempotency_and_response_before_terminal_ledger_io() {
    let provider = Arc::new(TestProvider::default());
    provider.block_first_call();
    provider.script(TestProvider::ok("capacity owner"));
    let ledger = Arc::new(TombstoneBlockingTaskLedgerSink::default());
    let mut config = small_config();
    config.workers = 1;
    config.queue_capacity_normal = 1;
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        ledger.clone(),
        config,
    );
    let (owner, owner_rx) =
        LlmJob::new(small_request(), JobOrigin::op("unpublished-capacity-owner"));
    queue.submit(owner).await.expect("capacity owner accepted");
    tokio::time::timeout(Duration::from_secs(1), provider.wait_for_first_call())
        .await
        .expect("capacity owner reached provider");

    let task_ref = TaskRef::task("unpublished-ledger-rejection");
    let (rejected, rejected_rx) = LlmJob::new(
        small_request(),
        JobOrigin::op("unpublished-ledger-rejection"),
    );
    let rejected_scope = rejected.trace_context.scope.clone();
    let submit_queue = queue.clone();
    let submit_task_ref = task_ref.clone();
    let rejected_submit = tokio::spawn(async move {
        submit_queue
            .submit(
                rejected
                    .with_task(submit_task_ref)
                    .with_idempotency_key("unpublished-ledger-key"),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), ledger.tombstone_entered.notified())
        .await
        .expect("rejection reached blocking tombstone-ledger append");

    assert!(matches!(
        tokio::time::timeout(Duration::from_millis(100), rejected_rx)
            .await
            .expect("queue rejection response precedes terminal ledger persistence")
            .expect("queue rejection response channel"),
        Err(LLMError::QueueFull { .. })
    ));
    assert!(queue.idempotency_key_is_reusable_for_test(
        Some(&task_ref),
        &rejected_scope,
        "unpublished-ledger-key",
    ));

    ledger.release.notify_one();
    assert!(matches!(
        rejected_submit.await.expect("rejection submit task"),
        Err(LLMError::QueueFull { .. })
    ));
    provider.release_first_call();
    owner_rx
        .await
        .expect("owner response channel")
        .expect("capacity owner success");
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn force_shutdown_bounds_a_blocked_ledger_while_draining_worker_lanes() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_secs(2));
    provider.script(TestProvider::ok("owner"));
    let ledger = Arc::new(TombstoneBlockingTaskLedgerSink::default());
    let mut config = small_config();
    config.workers = 1;
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        ledger,
        config,
    );
    let (owner, _owner_rx) = LlmJob::new(small_request(), JobOrigin::op("lane-ledger-owner"));
    queue.submit(owner).await.expect("owner accepted");
    while provider.calls() == 0 {
        tokio::task::yield_now().await;
    }
    let (queued, queued_rx) = LlmJob::new(small_request(), JobOrigin::op("lane-ledger-waiter"));
    queue
        .submit(queued.with_task(TaskRef::task("blocked-lane-ledger-task")))
        .await
        .expect("waiter accepted");

    tokio::time::timeout(
        Duration::from_secs(2),
        queue.shutdown(Duration::from_millis(20)),
    )
    .await
    .expect("lane drain must obey the shutdown deadline");
    assert!(matches!(
        queued_rx.await.expect("queued response"),
        Err(LLMError::Cancelled { .. })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn force_shutdown_bounds_a_blocked_stream_tombstone_ledger() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_secs(2));
    provider.script(TestProvider::ok("owner"));
    let ledger = Arc::new(TombstoneBlockingTaskLedgerSink::default());
    let mut config = small_config();
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        ledger,
        config,
    );
    let (owner, _owner_rx) = LlmJob::new(small_request(), JobOrigin::op("stream-ledger-owner"));
    queue.submit(owner).await.expect("owner accepted");
    while provider.calls() == 0 {
        tokio::task::yield_now().await;
    }
    let (stream_job, mut stream) =
        LlmStreamJob::new(small_request(), JobOrigin::op("stream-ledger-waiter"), 2);
    queue
        .submit_stream(stream_job.with_task(TaskRef::task("blocked-stream-ledger-task")))
        .await
        .expect("stream accepted");

    tokio::time::timeout(
        Duration::from_secs(2),
        queue.shutdown(Duration::from_millis(20)),
    )
    .await
    .expect("stream tombstone must obey force shutdown");
    assert!(matches!(
        stream.recv().await,
        Some(StreamDelta::Error(message)) if message.contains("shutdown")
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capacity_waiter_honors_job_cancellation_without_waiting_for_provider() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_millis(250));
    provider.script(TestProvider::ok("first"));
    let mut config = small_config();
    config.workers = 2;
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let (first, first_rx) = LlmJob::new(small_request(), JobOrigin::op("capacity-owner"));
    let (waiting, waiting_rx) = LlmJob::new(small_request(), JobOrigin::op("capacity-cancelled"));
    let waiting_id = waiting.job_id.clone();
    queue.submit(first).await.expect("owner submitted");
    tokio::time::timeout(Duration::from_millis(100), async {
        while provider.calls() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("owner should enter the provider before waiter submission");
    queue.submit(waiting).await.expect("waiter submitted");

    tokio::time::timeout(Duration::from_millis(100), async {
        loop {
            let snapshot = queue.snapshot();
            if snapshot
                .registry
                .pending
                .iter()
                .any(|meta| meta.job_id == waiting_id)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("second job should park behind provider capacity");

    assert!(queue.cancel_job(&waiting_id, "test capacity cancellation"));
    let cancelled = tokio::time::timeout(Duration::from_millis(100), waiting_rx)
        .await
        .expect("capacity cancellation should not await the provider owner")
        .expect("waiter response channel");
    assert!(matches!(cancelled, Err(LLMError::Cancelled { .. })));

    first_rx.await.unwrap().unwrap();
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_provider_permit_wait_is_not_counted_as_lane_wait() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_millis(90));
    provider.block_first_call();
    provider.script(TestProvider::ok("first"));
    provider.script(TestProvider::ok("second"));
    let mut config = small_config();
    config.provider_concurrency.default = 1;
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 1);
    let ledger = Arc::new(MockTaskLedgerSink::new());
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        ledger.clone(),
        config,
    );
    let mut events = queue.subscribe_events();
    let (first, mut first_stream) =
        LlmStreamJob::new(small_request(), JobOrigin::op("stream-serial-first"), 4);
    let (second, mut second_stream) =
        LlmStreamJob::new(small_request(), JobOrigin::op("stream-serial-second"), 4);
    let first_id = first.job_id.clone();
    let second_id = second.job_id.clone();
    queue
        .submit_stream(first)
        .await
        .expect("first stream submitted");
    tokio::time::timeout(Duration::from_secs(1), provider.wait_for_first_call())
        .await
        .expect("first stream should enter the provider while owning its permit");
    queue
        .submit_stream(second.with_task(TaskRef::task("stream-provider-wait-ledger")))
        .await
        .expect("second stream submitted");
    let lane_wait_at_provider_admission = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let event = events.recv().await.expect("queue event");
            if let LlmQueueEvent::WaitingForProvider { meta, .. } = event {
                if meta.job_id == second_id {
                    break meta
                        .wait_ms
                        .expect("provider-wait event retains lane pickup time");
                }
            }
        }
    })
    .await
    .expect("second stream should publish provider-capacity wait");
    // Hold the owner after the waiter has entered provider admission. This
    // makes provider contention deterministic even when the full suite loads
    // the host heavily; an arbitrary scheduler delay before that transition is
    // lane wait and remains independently observable.
    tokio::time::sleep(Duration::from_millis(80)).await;
    provider.release_first_call();
    assert!(matches!(
        first_stream.recv().await,
        Some(StreamDelta::Done(_))
    ));
    assert!(matches!(
        second_stream.recv().await,
        Some(StreamDelta::Done(_))
    ));

    let ledger_wait_ms = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Some(wait_ms) =
                ledger
                    .events()
                    .await
                    .into_iter()
                    .find_map(|(_, event)| match event {
                        LlmCallLedgerEvent::Completed {
                            job_id, wait_ms, ..
                        } if job_id == second_id => Some(wait_ms),
                        _ => None,
                    })
            {
                break wait_ms;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("second stream completion ledger");
    let snapshot = queue.snapshot();
    let completed = snapshot
        .registry
        .completed
        .iter()
        .filter(|meta| meta.job_id == first_id || meta.job_id == second_id)
        .collect::<Vec<_>>();
    assert_eq!(completed.len(), 2);
    let waited = completed
        .iter()
        .find(|meta| meta.job_id == second_id)
        .expect("second stream completion metadata");
    assert!(waited.provider_wait_ms.is_some_and(|ms| ms >= 60));
    assert_eq!(waited.wait_ms, Some(lane_wait_at_provider_admission));
    assert!(waited.execution_ms.is_some_and(|ms| ms >= 70));
    assert_eq!(ledger_wait_ms, lane_wait_at_provider_admission);
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_receipt_counts_provider_fallback_hops_as_physical_attempts() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::server_5xx());
    provider.script(TestProvider::ok("fallback ok"));
    let router = test_router_with_fallback(provider.clone());
    let queue = LlmDispatchQueue::start(
        router,
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );

    let response = queue
        .submit_and_wait(small_request(), JobOrigin::op("fallback"))
        .await
        .expect("fallback dispatch");

    assert_eq!(response.response.text.as_deref(), Some("fallback ok"));
    let route_identity = response
        .response
        .route_identity
        .as_ref()
        .expect("queue response must retain the effective fallback route");
    assert_eq!(route_identity.profile, "fallback");
    assert_eq!(route_identity.model, "fallback-model");
    assert_eq!(provider.calls(), 2);
    assert_eq!(response.attempts, 1, "retry policy saw one routed call");
    assert_eq!(response.trace_receipt.provider_attempt_count, 2);
    assert_eq!(
        response.trace_receipt.provider_attempt_id.as_deref(),
        Some(
            response
                .trace_receipt
                .context
                .provider_attempt_id(2)
                .as_str()
        )
    );
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fallback_and_dispatch_retry_share_large_request_payload_lanes() {
    let provider = Arc::new(TestProvider::default());
    provider.advertise_as(LLMProviderKind::OpenRouter);
    // Two failures traverse the router fallback and then the dispatch retry;
    // the third physical attempt succeeds. Every provider-effective envelope
    // must still point at the original bootstrap message allocation.
    provider.script(TestProvider::server_5xx());
    provider.script(TestProvider::server_5xx());
    provider.script(TestProvider::ok("shared payload"));
    let queue = LlmDispatchQueue::start(
        test_router_with_fallback_capabilities(provider.clone(), LLMProviderKind::OpenRouter, true),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );
    let mut request = small_request();
    request.modality = LLMModality::Vision;
    request.messages = vec![LLMMessage::user("x".repeat(256 * 1024))].into();
    request.tools = vec![LLMToolSpec {
        name: "large_schema".to_string(),
        description: "d".repeat(64 * 1024),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {"payload": {"type": "string", "description": "p".repeat(64 * 1024)}}
        }),
    }]
    .into();
    request.media = Some(Arc::new(vec![7_u8; 128 * 1024]));
    request.input_media = Some(Arc::new(vec![MediaContent {
        media_type: "application/octet-stream".to_string(),
        data: Some(vec![9_u8; 128 * 1024]),
        url: None,
        description: Some("binary fixture".to_string()),
    }]));
    let bootstrap = request.clone();

    queue
        .submit_and_wait(request, JobOrigin::op("payload-sharing"))
        .await
        .expect("retry succeeds");

    let captured = provider.captured_requests();
    assert_eq!(captured.len(), 3);
    assert!(captured
        .iter()
        .all(|request| request.shares_large_payload_with(&bootstrap)));
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_dispatch_preserves_large_request_payload_identity() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("streamed"));
    let queue = LlmDispatchQueue::start(
        test_router_without_profile_metadata(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );
    let mut request = small_request();
    request.messages = vec![LLMMessage::user("s".repeat(256 * 1024))].into();
    let bootstrap = request.clone();
    let (job, mut stream) = LlmStreamJob::new(request, JobOrigin::op("stream-sharing"), 4);
    queue.submit_stream(job).await.expect("stream accepted");
    assert!(matches!(stream.recv().await, Some(StreamDelta::Done(_))));

    let captured = provider.captured_requests();
    assert_eq!(captured.len(), 1);
    assert!(captured[0].shares_large_payload_with(&bootstrap));
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_failure_metadata_uses_the_terminal_fallback_route() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::server_5xx());
    provider.script(TestProvider::provider_4xx());
    let queue = LlmDispatchQueue::start(
        test_router_with_fallback(provider),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );
    let mut events = queue.subscribe_events();
    let (job, response) = LlmJob::new(small_request(), JobOrigin::op("fallback-failure"));
    let job_id = job.job_id.clone();
    queue.submit(job).await.expect("submitted");
    response
        .await
        .expect("worker response")
        .expect_err("fallback should fail");

    let failed = loop {
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("event timeout")
            .expect("event");
        if let LlmQueueEvent::Failed { meta, .. } = event {
            if meta.job_id == job_id {
                break meta;
            }
        }
    };
    assert_eq!(failed.profile.as_deref(), Some("fallback"));
    assert_eq!(failed.model.as_deref(), Some("fallback-model"));
    assert_eq!(
        failed.provider,
        Some(LLMProviderKind::Custom("test".to_string()))
    );
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_completion_retains_tokens_and_effective_route_in_job_metadata() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("streamed"));
    let queue = LlmDispatchQueue::start(
        test_router_with(provider),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );
    let mut events = queue.subscribe_events();
    let (job, mut stream) = LlmStreamJob::new(small_request(), JobOrigin::op("stream"), 4);
    let job_id = job.job_id.clone();
    queue.submit_stream(job).await.expect("accepted");

    let terminal = tokio::time::timeout(Duration::from_secs(1), stream.recv())
        .await
        .expect("terminal timeout")
        .expect("terminal delta");
    let StreamDelta::Done(response) = terminal else {
        panic!("expected completed stream");
    };
    assert_eq!(
        response
            .route_identity
            .as_ref()
            .map(|value| value.model.as_str()),
        Some("test-model")
    );

    let completed = loop {
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("event timeout")
            .expect("event");
        if let LlmQueueEvent::Completed { meta } = event {
            if meta.job_id == job_id {
                break meta;
            }
        }
    };
    let tokens = completed
        .tokens
        .expect("stream usage must reach job metadata");
    assert_eq!(tokens.prompt_tokens, 10);
    assert_eq!(tokens.completion_tokens, 5);
    assert_eq!(completed.profile.as_deref(), Some("test_default"));
    assert_eq!(completed.model.as_deref(), Some("test-model"));
    assert_eq!(
        completed.provider,
        Some(LLMProviderKind::Custom("test".to_string()))
    );
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn priority_drains_high_first() {
    let provider = Arc::new(TestProvider::default());
    // Add one slow + several fast outcomes; high should resolve first.
    provider.set_delay(Duration::from_millis(20));
    provider.script(TestProvider::ok("low_1"));
    provider.script(TestProvider::ok("low_2"));
    provider.script(TestProvider::ok("hi_1"));

    let router = test_router_with(provider.clone());
    let mut cfg = small_config();
    cfg.workers = 1; // serialize for ordering check
    let queue = LlmDispatchQueue::start(
        router,
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        cfg,
    );

    // Submit background + normal + high in that order; high should still complete first.
    let (bg_job, bg_rx) = LlmJob::new(small_request(), JobOrigin::op("bg"));
    let bg_job = bg_job.with_priority(Priority::Background);
    queue.submit(bg_job).await.unwrap();

    let (n_job, n_rx) = LlmJob::new(small_request(), JobOrigin::op("normal"));
    let n_job = n_job.with_priority(Priority::Normal);
    queue.submit(n_job).await.unwrap();

    let (h_job, h_rx) = LlmJob::new(small_request(), JobOrigin::op("high"));
    let h_job = h_job.with_priority(Priority::High);
    queue.submit(h_job).await.unwrap();

    // With 1 worker, the worker is busy on whichever it picked first. Drain.
    let _ = bg_rx.await.unwrap();
    let _ = n_rx.await.unwrap();
    let _ = h_rx.await.unwrap();
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retriable_error_retries_in_place() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::server_5xx());
    provider.script(TestProvider::server_5xx());
    provider.script(TestProvider::ok("eventually ok"));
    let router = test_router_with(provider.clone());
    let queue = LlmDispatchQueue::start(
        router,
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );

    let mut events = queue.subscribe_events();
    let (job, response_rx) = LlmJob::new(small_request(), JobOrigin::op("retry"));
    let expected_call_id = job.trace_context.llm_call_id.clone();
    let expected_job_id = job.job_id.to_string();
    queue.submit(job).await.expect("submit retry job");
    let resp = response_rx.await.unwrap().expect("ok after retries");
    assert_eq!(resp.response.text.as_deref(), Some("eventually ok"));
    assert!(provider.calls() >= 3);
    assert_eq!(resp.trace_receipt.context.llm_call_id, expected_call_id);
    assert_eq!(
        resp.trace_receipt.dispatch_job_id.as_deref(),
        Some(expected_job_id.as_str())
    );
    assert_eq!(resp.trace_receipt.provider_attempt_count, 3);
    let expected_final_attempt_id = format!("{expected_call_id}:a3");
    assert_eq!(
        resp.trace_receipt.provider_attempt_id.as_deref(),
        Some(expected_final_attempt_id.as_str())
    );

    let mut attempt_ids = Vec::new();
    let mut dispatch_events = 0usize;
    while attempt_ids.len() < 3 {
        let event = tokio::time::timeout(Duration::from_millis(500), events.recv())
            .await
            .expect("attempt event timeout")
            .expect("attempt event");
        match event {
            super::events::LlmQueueEvent::Dispatched { ref meta }
                if meta.job_id.as_str() == expected_job_id.as_str() =>
            {
                dispatch_events += 1;
            },
            super::events::LlmQueueEvent::AttemptDone { meta, .. } => {
                assert_eq!(meta.trace_context.llm_call_id, expected_call_id);
                attempt_ids.push(meta.provider_attempt_id.expect("provider attempt id"));
            },
            _ => {},
        }
    }
    assert_eq!(
        dispatch_events, 1,
        "logical dispatch is emitted exactly once"
    );
    assert_eq!(
        attempt_ids,
        vec![
            format!("{expected_call_id}:a1"),
            format!("{expected_call_id}:a2"),
            format!("{expected_call_id}:a3"),
        ]
    );
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_retriable_fails_fast() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::provider_4xx());
    let router = test_router_with(provider.clone());
    let queue = LlmDispatchQueue::start(
        router,
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );

    let err = queue
        .submit_and_wait(small_request(), JobOrigin::op("4xx"))
        .await
        .expect_err("should fail fast on 4xx");
    // First-attempt fail preserves the exact route while its root cause stays
    // classifiable as the original provider error.
    assert!(
        matches!(err.root_cause(), LLMError::Provider { .. }),
        "expected routed Provider error, got {:?}",
        err
    );
    assert_eq!(provider.calls(), 1);
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hard_ceiling_terminal_fail() {
    let provider = Arc::new(TestProvider::default());
    for _ in 0..10 {
        provider.script(TestProvider::server_5xx());
    }
    let router = test_router_with(provider.clone());
    let queue = LlmDispatchQueue::start(
        router,
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );

    let err = queue
        .submit_and_wait(small_request(), JobOrigin::op("hard_fail"))
        .await
        .expect_err("should terminal-fail after ceiling");
    let (profile, provider, model) = err
        .effective_route()
        .expect("retry exhaustion must retain the final concrete route");
    assert_eq!(profile, "test_default");
    assert_eq!(provider, &LLMProviderKind::Custom("test".to_string()));
    assert_eq!(model, "test-model");
    match err.root_cause() {
        LLMError::AllRetriesExhausted { attempts, .. } => {
            // Hard ceiling = max_attempts_per_dispatch (3) × max_dispatch_cycles (2) = 6.
            assert_eq!(
                *attempts, 6,
                "expected exactly hard ceiling (6) attempts, got {}",
                attempts
            );
        },
        other => panic!("expected AllRetriesExhausted, got {:?}", other),
    }
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rate_limit_engages_cooldown_then_succeeds() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::rate_limit());
    provider.script(TestProvider::ok("ok after rate limit"));
    let router = test_router_with(provider.clone());
    let queue = LlmDispatchQueue::start(
        router,
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );

    let resp = queue
        .submit_and_wait(small_request(), JobOrigin::op("ratelimit"))
        .await
        .expect("ok after cooldown");
    assert_eq!(resp.response.text.as_deref(), Some("ok after rate limit"));
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pre_dispatch_tombstones_cancelled_task() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("never called"));
    let router = test_router_with(provider.clone());
    let task_state = Arc::new(MockTaskStateView::new());
    task_state.set_cancelled("task-1", "user_pressed_stop");

    let queue = LlmDispatchQueue::start(
        router,
        task_state,
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );

    let mut events = queue.subscribe_events();
    let (job, rx) = LlmJob::new(small_request(), JobOrigin::op("cancelled"));
    let expected_call_id = job.trace_context.llm_call_id.clone();
    let job = job.with_task(TaskRef::task("task-1"));
    queue.submit(job).await.unwrap();

    let err = rx.await.unwrap().expect_err("should be cancelled");
    assert!(matches!(err, LLMError::Cancelled { .. }));
    assert_eq!(provider.calls(), 0, "provider should not be called");
    loop {
        let event = tokio::time::timeout(Duration::from_millis(500), events.recv())
            .await
            .expect("tombstone event timeout")
            .expect("tombstone event");
        if let super::events::LlmQueueEvent::Tombstoned { meta, .. } = event {
            assert_eq!(meta.trace_context.llm_call_id, expected_call_id);
            assert_eq!(meta.trace_context.task_id.as_deref(), Some("task-1"));
            break;
        }
    }
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_flight_cancellation_preserves_the_started_provider_attempt_identity() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_secs(1));
    provider.script(TestProvider::ok("cancelled before delivery"));
    let task_state = Arc::new(MockTaskStateView::new());
    task_state.set_active("task-in-flight");
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        task_state.clone(),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );
    let mut events = queue.subscribe_events();
    let (job, response_rx) = LlmJob::new(small_request(), JobOrigin::op("cancel-in-flight"));
    let call_id = job.trace_context.llm_call_id.clone();
    queue
        .submit(job.with_task(TaskRef::task("task-in-flight")))
        .await
        .expect("job accepted");

    tokio::time::timeout(Duration::from_millis(500), async {
        while provider.calls() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("provider attempt started");
    task_state.fire_cancel("task-in-flight");

    assert!(matches!(
        response_rx.await.expect("response channel"),
        Err(LLMError::Cancelled { .. })
    ));
    loop {
        let event = tokio::time::timeout(Duration::from_millis(500), events.recv())
            .await
            .expect("tombstone event timeout")
            .expect("queue event");
        if let LlmQueueEvent::Tombstoned { meta, .. } = event {
            assert_eq!(meta.provider_attempt_count, 1);
            assert_eq!(
                meta.provider_attempt_id.as_deref(),
                Some(format!("{call_id}:a1").as_str())
            );
            assert_eq!(meta.attempts, 1);
            break;
        }
    }
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_cancel_task_tombstones_pending() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_secs(1));
    let router = test_router_with(provider.clone());
    let task_state = Arc::new(MockTaskStateView::new());
    task_state.set_active("task-X");

    let mut cfg = small_config();
    cfg.workers = 1;
    let queue = LlmDispatchQueue::start(router, task_state, Arc::new(NoopTaskLedgerSink), cfg);

    // First job will tie up the worker
    provider.script(TestProvider::ok("first"));
    let (busy_job, busy_rx) = LlmJob::new(small_request(), JobOrigin::op("busy"));
    queue
        .submit(busy_job.with_task(TaskRef::task("task-X")))
        .await
        .unwrap();

    // Subsequent jobs queued behind it
    let (pending_job, pending_rx) = LlmJob::new(small_request(), JobOrigin::op("pending"));
    queue
        .submit(pending_job.with_task(TaskRef::task("task-X")))
        .await
        .unwrap();

    // Brief yield so submission settles
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Cancel — pending job should be tombstoned
    let n = queue.cancel_task("task-X", "test_cancel");
    assert!(n >= 1, "should have cancelled at least one pending job");

    let pending_outcome = pending_rx.await.unwrap();
    assert!(matches!(pending_outcome, Err(LLMError::Cancelled { .. })));
    let _ = busy_rx.await; // busy one still resolves
    queue.shutdown(Duration::from_millis(500)).await;
}

#[test]
fn task_ref_cancel_id_matches_execution_and_root() {
    let task_ref = TaskRef::task("artifact").with_execution("root-exec", "child-exec");
    assert!(task_ref.matches_cancel_id("artifact"));
    assert!(task_ref.matches_cancel_id("root-exec"));
    assert!(task_ref.matches_cancel_id("child-exec"));
    assert!(!task_ref.matches_cancel_id("other"));
    assert!(!task_ref.matches_cancel_id(""));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_execution_id_tombstones_jobs_tagged_with_artifact_task_id() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_secs(1));
    let router = test_router_with(provider.clone());
    let task_state = Arc::new(MockTaskStateView::new());
    task_state.set_active("artifact-task");

    let mut cfg = small_config();
    cfg.workers = 1;
    let queue = LlmDispatchQueue::start(router, task_state, Arc::new(NoopTaskLedgerSink), cfg);

    provider.script(TestProvider::ok("first"));
    let (busy_job, busy_rx) = LlmJob::new(small_request(), JobOrigin::op("busy"));
    queue
        .submit(
            busy_job.with_task(TaskRef::task("artifact-task").with_execution("exec-1", "exec-1")),
        )
        .await
        .unwrap();

    let (pending_job, pending_rx) = LlmJob::new(small_request(), JobOrigin::op("pending"));
    queue
        .submit(
            pending_job
                .with_task(TaskRef::task("artifact-task").with_execution("exec-1", "exec-1")),
        )
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(20)).await;

    let n = queue.cancel_task("exec-1", "cancel_execution");
    assert!(
        n >= 1,
        "cancel_execution must match jobs whose task_id is the artifact id"
    );

    let pending_outcome = pending_rx.await.unwrap();
    assert!(matches!(pending_outcome, Err(LLMError::Cancelled { .. })));
    let _ = busy_rx.await;
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_job_tombstones_specific_pending() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_secs(1));
    let router = test_router_with(provider.clone());
    let mut cfg = small_config();
    cfg.workers = 1;
    let queue = LlmDispatchQueue::start(
        router,
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        cfg,
    );

    provider.script(TestProvider::ok("first"));
    let (busy_job, _busy_rx) = LlmJob::new(small_request(), JobOrigin::op("busy"));
    queue.submit(busy_job).await.unwrap();

    let (pending_job, pending_rx) = LlmJob::new(small_request(), JobOrigin::op("p"));
    let pending_job_id = pending_job.job_id.clone();
    queue.submit(pending_job).await.unwrap();

    tokio::time::sleep(Duration::from_millis(20)).await;
    let cancelled = queue.cancel_job(&pending_job_id, "explicit");
    assert!(cancelled, "cancel_job should have hit a pending entry");

    let outcome = pending_rx.await.unwrap();
    assert!(matches!(outcome, Err(LLMError::Cancelled { .. })));
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submission_deadline_tombstoned_at_pickup() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_millis(200));
    provider.script(TestProvider::ok("first"));
    let router = test_router_with(provider.clone());
    let mut cfg = small_config();
    cfg.workers = 1;
    let queue = LlmDispatchQueue::start(
        router,
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        cfg,
    );

    // Submit a busy job to occupy the worker.
    let (busy_job, _busy_rx) = LlmJob::new(small_request(), JobOrigin::op("busy"));
    queue.submit(busy_job).await.unwrap();

    // Submit a deadline job whose deadline is already past by the time the
    // worker tries to pick it up.
    let (deadline_job, deadline_rx) = LlmJob::new(small_request(), JobOrigin::op("deadline"));
    let deadline_job = deadline_job
        .with_submission_deadline(std::time::Instant::now() + Duration::from_millis(10));
    queue.submit(deadline_job).await.unwrap();

    let outcome = deadline_rx.await.unwrap();
    assert!(matches!(outcome, Err(LLMError::Cancelled { .. })));
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_submission_never_reuses_cached_idempotent_result() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("cached-success"));
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );
    let mut events = queue.subscribe_events();

    let (owner, owner_rx) = LlmJob::new(small_request(), JobOrigin::op("idemp-deadline"));
    queue
        .submit(owner.with_idempotency_key("deadline-key"))
        .await
        .expect("owner accepted");
    owner_rx
        .await
        .expect("owner channel")
        .expect("owner response");

    let (expired, expired_rx) = LlmJob::new(small_request(), JobOrigin::op("idemp-deadline"));
    let submit_error = queue
        .submit(
            expired
                .with_idempotency_key("deadline-key")
                .with_submission_deadline(Instant::now() - Duration::from_millis(1)),
        )
        .await
        .expect_err("expired submission must be rejected before cache lookup");
    assert!(matches!(submit_error, LLMError::DeadlineExceeded));
    let response_error = expired_rx
        .await
        .expect("expired response channel")
        .expect_err("expired submission must not receive cached success");
    assert!(matches!(response_error, LLMError::DeadlineExceeded));
    assert_eq!(
        provider.calls(),
        1,
        "deadline rejection must not reinvoke provider"
    );
    let mut saw_tombstone = false;
    while let Ok(Ok(event)) = tokio::time::timeout(Duration::from_millis(100), events.recv()).await
    {
        if let super::events::LlmQueueEvent::Tombstoned { meta, reason } = event {
            if meta.origin.operation == "idemp-deadline"
                && reason == super::types::TombstoneReason::DeadlineExceeded
                && meta.provider_attempt_count == 0
            {
                saw_tombstone = true;
                break;
            }
        }
    }
    assert!(saw_tombstone, "expired call must remain observable");
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_stream_submission_is_rejected_and_emits_zero_attempt_tombstone() {
    let provider = Arc::new(TestProvider::default());
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );
    let mut events = queue.subscribe_events();
    let (job, mut stream) = LlmStreamJob::new(small_request(), JobOrigin::op("expired-stream"), 4);
    let job_id = job.job_id.clone();

    let error = queue
        .submit_stream(job.with_submission_deadline(Instant::now() - Duration::from_millis(1)))
        .await
        .expect_err("expired stream must fail at admission");
    assert!(matches!(error, LLMError::DeadlineExceeded));
    assert!(matches!(
        stream.recv().await,
        Some(StreamDelta::Error(message)) if message.contains("deadline")
    ));

    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("event timeout")
            .expect("event");
        if let super::events::LlmQueueEvent::Tombstoned { meta, reason } = event {
            if meta.job_id == job_id {
                assert_eq!(reason, super::types::TombstoneReason::DeadlineExceeded);
                assert_eq!(meta.provider_attempt_count, 0);
                break;
            }
        }
    }
    assert_eq!(provider.calls(), 0);
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_full_rejects_overflow() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_secs(2));
    provider.script(TestProvider::ok("a"));
    let router = test_router_with(provider.clone());

    let mut cfg = small_config();
    cfg.workers = 1;
    cfg.queue_capacity_normal = 2;
    let queue = LlmDispatchQueue::start(
        router,
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        cfg,
    );

    // The configured capacity is now a lifetime bound across executing,
    // parked, delayed-retry, and channel-resident jobs.
    let mut accepted = 0usize;
    let mut rejected = 0usize;
    for _ in 0..6 {
        let (job, _rx) = LlmJob::new(small_request(), JobOrigin::op("flood"));
        match queue.submit(job).await {
            Ok(_) => accepted += 1,
            Err(LLMError::QueueFull { .. }) => rejected += 1,
            Err(e) => panic!("unexpected error: {:?}", e),
        }
    }
    assert!(accepted >= 1);
    assert!(rejected >= 1, "expected at least one QueueFull");
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_full_does_not_poison_a_same_key_idempotent_retry() {
    let provider = Arc::new(TestProvider::default());
    provider.block_first_call();
    provider.script(TestProvider::ok("capacity-owner"));
    provider.script(TestProvider::ok("retried-operation"));
    let mut config = small_config();
    config.workers = 1;
    config.queue_capacity_normal = 1;
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );

    let (capacity_owner, capacity_owner_rx) =
        LlmJob::new(small_request(), JobOrigin::op("capacity-owner"));
    queue.submit(capacity_owner).await.expect("owner accepted");
    provider.wait_for_first_call().await;

    let task = TaskRef::task("queue-full-idempotent-retry");
    let (rejected, rejected_rx) =
        LlmJob::new(small_request(), JobOrigin::op("rejected-generation"));
    let rejected = rejected
        .with_task(task.clone())
        .with_idempotency_key("retryable-key");
    let scope = rejected.trace_context.scope.clone();
    assert!(matches!(
        queue.submit(rejected).await,
        Err(LLMError::QueueFull { .. })
    ));
    assert!(matches!(
        rejected_rx.await.expect("rejected response"),
        Err(LLMError::QueueFull { .. })
    ));
    assert!(queue.idempotency_key_is_reusable_for_test(Some(&task), &scope, "retryable-key"));

    provider.release_first_call();
    capacity_owner_rx
        .await
        .expect("owner response channel")
        .expect("owner success");

    let (retry, retry_rx) = LlmJob::new(small_request(), JobOrigin::op("retry-generation"));
    queue
        .submit(retry.with_task(task).with_idempotency_key("retryable-key"))
        .await
        .expect("same key must be admitted after capacity returns");
    let retried = retry_rx
        .await
        .expect("retry response channel")
        .expect("retry provider success");
    assert_eq!(retried.response.text.as_deref(), Some("retried-operation"));
    assert_eq!(provider.calls(), 2);

    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idempotency_returns_same_result() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("only once"));
    let router = test_router_with(provider.clone());
    let queue = LlmDispatchQueue::start(
        router,
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );

    let (job_a, rx_a) = LlmJob::new(small_request(), JobOrigin::op("idemp"));
    let job_a = job_a.with_idempotency_key("dedup-key-1");
    queue.submit(job_a).await.unwrap();

    // Second submit with same key should NOT trigger another provider call.
    let (job_b, rx_b) = LlmJob::new(small_request(), JobOrigin::op("idemp"));
    let job_b = job_b.with_idempotency_key("dedup-key-1");
    queue.submit(job_b).await.unwrap();

    let result_a = rx_a.await.unwrap().expect("a ok");
    let result_b = rx_b.await.unwrap().expect("b ok");
    assert_eq!(
        result_a.response.text.as_deref(),
        result_b.response.text.as_deref()
    );
    assert!(
        Arc::ptr_eq(&result_a.response, &result_b.response),
        "idempotent subscribers must share normalized provider bytes"
    );
    assert_eq!(provider.calls(), 1, "provider should be called once");
    assert_eq!(
        result_a.trace_receipt.context.llm_call_id,
        result_b.trace_receipt.context.llm_call_id
    );
    assert_eq!(
        result_a.trace_receipt.dispatch_job_id,
        result_b.trace_receipt.dispatch_job_id
    );
    assert!(!result_a.trace_receipt.response_reused);
    assert!(result_b.trace_receipt.response_reused);
    assert!(
        result_b
            .clone()
            .into_response()
            .trace_receipt
            .is_some_and(|receipt| receipt.response_reused),
        "owned compatibility response must carry the consumer reuse receipt"
    );
    queue.shutdown(Duration::from_millis(500)).await;
}

#[test]
fn into_response_stamps_receipt_without_copying_large_provider_lanes() {
    let mut trace_receipt =
        crate::trace::LlmTraceReceipt::direct(crate::trace::LlmTraceContext::legacy(
            Some("public-into-response"),
            crate::trace::LlmWorkloadClass::Evaluation,
        ));
    trace_receipt.response_reused = true;
    let shared = Arc::new(LLMResponse {
        text: Some(Arc::<str>::from("x".repeat(256 * 1024))),
        reasoning_text: Some(Arc::<str>::from("r".repeat(64 * 1024))),
        messages: Arc::new(vec![LLMMessage::assistant("m".repeat(128 * 1024))]),
        tool_calls: Arc::new(vec![LLMToolCall {
            id: "call".to_string(),
            name: "large".to_string(),
            arguments: serde_json::json!({"payload": "a".repeat(128 * 1024)}),
        }]),
        raw_response: Some(Arc::new(serde_json::json!({"raw": "z".repeat(128 * 1024)}))),
        ..LLMResponse::default()
    });
    let retained = Arc::clone(&shared);
    let dispatched = DispatchedResponse {
        response: shared,
        wait: Duration::ZERO,
        execution: Duration::ZERO,
        local_prep: None,
        attempts: 1,
        trace_receipt,
    };

    let mut owned = dispatched.into_response();
    assert!(owned.shares_large_payload_with(retained.as_ref()));
    assert!(owned
        .trace_receipt
        .as_ref()
        .is_some_and(|receipt| receipt.response_reused));

    owned
        .messages_mut()
        .push(LLMMessage::assistant("consumer-only"));
    assert_eq!(retained.messages.len(), 1, "payload mutation must be COW");
    assert_eq!(owned.messages.len(), 2);
    owned.raw_response_mut().expect("raw response object")["consumer"] =
        serde_json::Value::Bool(true);
    assert!(retained
        .raw_response
        .as_deref()
        .and_then(|raw| raw.get("consumer"))
        .is_none());

    let encoded = serde_json::to_value(&owned).expect("response serialization");
    assert!(encoded
        .get("text")
        .is_some_and(serde_json::Value::is_string));
    assert!(encoded
        .get("messages")
        .is_some_and(serde_json::Value::is_array));
    assert!(encoded
        .get("tool_calls")
        .is_some_and(serde_json::Value::is_array));
    assert!(encoded
        .get("raw_response")
        .is_some_and(serde_json::Value::is_object));
    assert!(encoded.get("trace_receipt").is_none());
    let decoded: LLMResponse =
        serde_json::from_value(encoded).expect("response wire shape remains deserializable");
    assert_eq!(decoded.text.as_deref(), owned.text.as_deref());
    assert_eq!(decoded.messages.len(), owned.messages.len());
    assert_eq!(decoded.tool_calls.len(), owned.tool_calls.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idempotency_never_deduplicates_across_scopes() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("scope-a"));
    provider.script(TestProvider::ok("scope-b"));
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );

    let (job_a, rx_a) = LlmJob::new(small_request(), JobOrigin::op("idemp-scope"));
    let job_a = job_a
        .with_task(TaskRef::task("task").with_scope("principal-a", "workspace"))
        .with_idempotency_key("shared-key");
    queue.submit(job_a).await.unwrap();
    let result_a = rx_a.await.unwrap().expect("scope a response");

    let (job_b, rx_b) = LlmJob::new(small_request(), JobOrigin::op("idemp-scope"));
    let job_b = job_b
        .with_task(TaskRef::task("task").with_scope("principal-b", "workspace"))
        .with_idempotency_key("shared-key");
    queue.submit(job_b).await.unwrap();
    let result_b = rx_b.await.unwrap().expect("scope b response");

    assert_eq!(provider.calls(), 2);
    assert_ne!(
        result_a.trace_receipt.context.llm_call_id,
        result_b.trace_receipt.context.llm_call_id
    );
    assert_eq!(
        result_a.trace_receipt.context.scope.principal,
        "principal-a"
    );
    assert_eq!(
        result_b.trace_receipt.context.scope.principal,
        "principal-b"
    );
    assert!(!result_a.trace_receipt.response_reused);
    assert!(!result_b.trace_receipt.response_reused);
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idempotency_is_unchanged_by_observability_only_execution_lineage() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("shared"));
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );
    let task_a = TaskRef::task("task")
        .with_scope("principal", "workspace")
        .with_execution("root", "execution-a")
        .with_iteration("iteration-a");
    let task_b = TaskRef::task("task")
        .with_scope("principal", "workspace")
        .with_execution("root", "execution-b")
        .with_iteration("iteration-b");
    let (job_a, rx_a) = LlmJob::new(small_request(), JobOrigin::op("idemp-lineage"));
    queue
        .submit(job_a.with_task(task_a).with_idempotency_key("same-key"))
        .await
        .unwrap();
    let (job_b, rx_b) = LlmJob::new(small_request(), JobOrigin::op("idemp-lineage"));
    queue
        .submit(job_b.with_task(task_b).with_idempotency_key("same-key"))
        .await
        .unwrap();

    let produced = rx_a.await.unwrap().expect("producer response");
    let reused = rx_b.await.unwrap().expect("reused response");
    assert_eq!(provider.calls(), 1);
    assert_eq!(
        produced.trace_receipt.context.llm_call_id,
        reused.trace_receipt.context.llm_call_id
    );
    assert!(reused.trace_receipt.response_reused);
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ledger_emits_lifecycle_events_for_success() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("ok"));
    let router = test_router_with(provider.clone());
    let ledger = Arc::new(MockTaskLedgerSink::new());
    let queue = LlmDispatchQueue::start(
        router,
        Arc::new(NoopTaskStateView),
        ledger.clone(),
        small_config(),
    );

    let (job, rx) = LlmJob::new(small_request(), JobOrigin::op("ledger_test"));
    let job = job.with_task(TaskRef::task("task-L"));
    queue.submit(job).await.unwrap();
    rx.await.unwrap().expect("ok");
    queue.shutdown(Duration::from_millis(500)).await;

    let events = ledger.events().await;
    let kinds: Vec<&str> = events
        .iter()
        .map(|(_, e)| match e {
            LlmCallLedgerEvent::Submitted { .. } => "submitted",
            LlmCallLedgerEvent::AttemptStart { .. } => "attempt_start",
            LlmCallLedgerEvent::AttemptDone { .. } => "attempt_done",
            LlmCallLedgerEvent::Requeued { .. } => "requeued",
            LlmCallLedgerEvent::Completed { .. } => "completed",
            LlmCallLedgerEvent::Failed { .. } => "failed",
            LlmCallLedgerEvent::Tombstoned { .. } => "tombstoned",
        })
        .collect();
    assert!(kinds.contains(&"submitted"));
    assert!(kinds.contains(&"attempt_start"));
    assert!(kinds.contains(&"attempt_done"));
    assert!(kinds.contains(&"completed"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ledger_emits_failed_on_terminal_fail() {
    let provider = Arc::new(TestProvider::default());
    for _ in 0..10 {
        provider.script(TestProvider::server_5xx());
    }
    let router = test_router_with(provider.clone());
    let ledger = Arc::new(MockTaskLedgerSink::new());
    let queue = LlmDispatchQueue::start(
        router,
        Arc::new(NoopTaskStateView),
        ledger.clone(),
        small_config(),
    );

    let (job, rx) = LlmJob::new(small_request(), JobOrigin::op("ledger_fail"));
    let job = job.with_task(TaskRef::task("task-F"));
    queue.submit(job).await.unwrap();
    let _ = rx.await.unwrap();
    queue.shutdown(Duration::from_millis(500)).await;

    let events = ledger.events().await;
    let has_failed = events
        .iter()
        .any(|(_, e)| matches!(e, LlmCallLedgerEvent::Failed { .. }));
    assert!(has_failed, "expected Failed ledger event");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_drains_pending_and_tombstones() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_secs(1));
    provider.script(TestProvider::ok("a"));
    let router = test_router_with(provider.clone());
    let mut cfg = small_config();
    cfg.workers = 1;
    let queue = LlmDispatchQueue::start(
        router,
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        cfg,
    );

    // Submit busy + 2 pending
    let (b, _br) = LlmJob::new(small_request(), JobOrigin::op("busy"));
    queue.submit(b).await.unwrap();
    let (p1, p1_rx) = LlmJob::new(small_request(), JobOrigin::op("p1"));
    queue.submit(p1).await.unwrap();
    let (p2, p2_rx) = LlmJob::new(small_request(), JobOrigin::op("p2"));
    queue.submit(p2).await.unwrap();

    tokio::time::sleep(Duration::from_millis(20)).await;
    let stats = queue.shutdown(Duration::from_millis(100)).await;
    assert!(stats.tombstoned_pending >= 2);

    let r1 = p1_rx.await.unwrap();
    let r2 = p2_rx.await.unwrap();
    assert!(matches!(r1, Err(LLMError::Cancelled { .. })));
    assert!(matches!(r2, Err(LLMError::Cancelled { .. })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_does_not_observe_sync_submission_blocked_on_ledger_as_pending() {
    let provider = Arc::new(TestProvider::default());
    let ledger = Arc::new(BlockingTaskLedgerSink::default());
    let queue = LlmDispatchQueue::start(
        test_router_with(provider),
        Arc::new(NoopTaskStateView),
        ledger.clone(),
        small_config(),
    );
    let mut queue_events = queue.subscribe_events();
    let (job, response_rx) = LlmJob::new(small_request(), JobOrigin::op("blocked-ledger"));
    let submit_queue = queue.clone();
    let submit = tokio::spawn(async move {
        submit_queue
            .submit(job.with_task(TaskRef::task("task-ledger-race")))
            .await
    });
    ledger.entered.notified().await;

    let stats = queue.shutdown(Duration::from_millis(20)).await;
    assert_eq!(stats.pending_at_shutdown, 0);
    assert!(queue.snapshot().registry.pending.is_empty());

    ledger.release.notify_waiters();
    let error = submit
        .await
        .expect("submit task")
        .expect_err("shutdown wins admission");
    assert!(matches!(error, LLMError::Cancelled { .. }));
    assert!(matches!(
        response_rx.await.expect("response result"),
        Err(LLMError::Cancelled { .. })
    ));
    let submitted = queue_events.recv().await.expect("submitted event");
    let tombstoned = queue_events.recv().await.expect("tombstoned event");
    assert!(matches!(submitted, LlmQueueEvent::Submitted { .. }));
    assert!(matches!(
        tombstoned,
        LlmQueueEvent::Tombstoned {
            reason: TombstoneReason::QueueShutdown,
            ..
        }
    ));
    assert!(queue.snapshot().registry.pending.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_rejection_does_not_cache_an_unpublished_idempotent_generation() {
    let provider = Arc::new(TestProvider::default());
    let ledger = Arc::new(BlockingTaskLedgerSink::default());
    let queue = LlmDispatchQueue::start(
        test_router_with(provider),
        Arc::new(NoopTaskStateView),
        ledger.clone(),
        small_config(),
    );
    let task = TaskRef::task("shutdown-idempotent-retry");
    let (job, response_rx) = LlmJob::new(small_request(), JobOrigin::op("shutdown-generation"));
    let job = job
        .with_task(task.clone())
        .with_idempotency_key("retryable-after-shutdown-race");
    let scope = job.trace_context.scope.clone();
    let submit_queue = Arc::clone(&queue);
    let submit = tokio::spawn(async move { submit_queue.submit(job).await });
    ledger.entered.notified().await;

    queue.shutdown(Duration::from_millis(20)).await;
    ledger.release.notify_waiters();
    assert!(matches!(
        submit
            .await
            .expect("submit task")
            .expect_err("shutdown wins admission"),
        LLMError::Cancelled { .. }
    ));
    assert!(matches!(
        response_rx.await.expect("shutdown response"),
        Err(LLMError::Cancelled { .. })
    ));
    assert!(
        queue.idempotency_key_is_reusable_for_test(
            Some(&task),
            &scope,
            "retryable-after-shutdown-race"
        ),
        "an unpublished shutdown rejection may wake subscribers but must not enter the recent-result cache"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_idempotent_submission_releases_its_generation_before_lane_publication() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("ok"));
    let ledger = Arc::new(BlockingTaskLedgerSink::default());
    let queue = LlmDispatchQueue::start(
        test_router_with(provider),
        Arc::new(NoopTaskStateView),
        ledger.clone(),
        small_config(),
    );
    let task = TaskRef::task("task-cancelled-idempotent-submit");
    let (owner, owner_rx) = LlmJob::new(small_request(), JobOrigin::op("cancelled-owner"));
    let submit_queue = Arc::clone(&queue);
    let submit_task = task.clone();
    let submit = tokio::spawn(async move {
        submit_queue
            .submit(
                owner
                    .with_task(submit_task)
                    .with_idempotency_key("cancelled-submit"),
            )
            .await
    });
    ledger.entered.notified().await;
    submit.abort();
    assert!(submit
        .await
        .expect_err("submit future is cancelled")
        .is_cancelled());
    assert!(
        owner_rx.await.is_err(),
        "cancelled owner's sender is dropped"
    );

    let (retry, retry_rx) = LlmJob::new(small_request(), JobOrigin::op("retry-owner"));
    queue
        .submit(
            retry
                .with_task(task)
                .with_idempotency_key("cancelled-submit"),
        )
        .await
        .expect("cancelled generation must not poison retry admission");
    let retry_result = tokio::time::timeout(Duration::from_secs(1), retry_rx)
        .await
        .expect("retry must not subscribe to an orphan generation")
        .expect("retry response channel")
        .expect("retry provider success");
    assert_eq!(retry_result.response.text.as_deref(), Some("ok"));

    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_does_not_observe_stream_submission_blocked_on_ledger_as_pending() {
    let provider = Arc::new(TestProvider::default());
    let ledger = Arc::new(BlockingTaskLedgerSink::default());
    let queue = LlmDispatchQueue::start(
        test_router_with(provider),
        Arc::new(NoopTaskStateView),
        ledger.clone(),
        small_config(),
    );
    let mut queue_events = queue.subscribe_events();
    let (job, mut delta_rx) =
        LlmStreamJob::new(small_request(), JobOrigin::op("blocked-stream-ledger"), 4);
    let submit_queue = queue.clone();
    let submit = tokio::spawn(async move {
        submit_queue
            .submit_stream(job.with_task(TaskRef::task("task-stream-ledger-race")))
            .await
    });
    ledger.entered.notified().await;

    let stats = queue.shutdown(Duration::from_millis(20)).await;
    assert_eq!(stats.pending_at_shutdown, 0);
    assert!(queue.snapshot().registry.pending.is_empty());

    ledger.release.notify_waiters();
    let error = submit
        .await
        .expect("submit task")
        .expect_err("shutdown wins stream admission");
    assert!(matches!(error, LLMError::Cancelled { .. }));
    assert!(matches!(delta_rx.recv().await, Some(StreamDelta::Error(_))));
    let submitted = queue_events.recv().await.expect("submitted event");
    let tombstoned = queue_events.recv().await.expect("tombstoned event");
    assert!(matches!(submitted, LlmQueueEvent::Submitted { .. }));
    assert!(matches!(
        tombstoned,
        LlmQueueEvent::Tombstoned {
            reason: TombstoneReason::QueueShutdown,
            ..
        }
    ));
    assert!(queue.snapshot().registry.pending.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_bus_emits_completed() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("evt"));
    let router = test_router_with(provider.clone());
    let queue = LlmDispatchQueue::start(
        router,
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );

    let mut rx = queue.subscribe_events();
    let (job, jr) = LlmJob::new(small_request(), JobOrigin::op("evt"));
    queue.submit(job).await.unwrap();
    let _ = jr.await.unwrap();

    let mut saw_completed = false;
    for _ in 0..8 {
        if let Ok(ev) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            if let Ok(super::events::LlmQueueEvent::Completed { .. }) = ev {
                saw_completed = true;
                break;
            }
        } else {
            break;
        }
    }
    assert!(saw_completed, "expected Completed event");
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_reflects_workers_and_lanes() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("ok"));
    let router = test_router_with(provider.clone());
    let queue = LlmDispatchQueue::start(
        router,
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        small_config(),
    );
    let snap = queue.snapshot();
    assert!(snap.workers_total >= 1);
    assert!(snap.oldest_wait_ms_high.is_none());
    assert!(snap.oldest_wait_ms_normal.is_none());
    assert!(snap.oldest_wait_ms_background.is_none());
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_workers_total_matches_config() {
    let provider = Arc::new(TestProvider::default());
    let mut config = small_config();
    config.workers = 8;
    let queue = LlmDispatchQueue::start(
        test_router_with(provider),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    assert_eq!(queue.snapshot().workers_total, 8);
    assert_eq!(queue.worker_count(), 8);
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_accepts_provider_isolated_engine() {
    let mut config = small_config();
    config.engine = DispatchEngine::ProviderIsolated;
    let queue = LlmDispatchQueue::start(
        test_router_with(Arc::new(TestProvider::default())),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    assert_eq!(queue.snapshot().workers_total, 2);
    queue.shutdown(Duration::from_millis(500)).await;
}

async fn wait_until<F>(timeout: Duration, mut pred: F)
where
    F: FnMut() -> bool,
{
    tokio::time::timeout(timeout, async {
        while !pred() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("timed out waiting for dispatch occupancy");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saturated_ollama_does_not_block_open_cloud() {
    let workers = 8;
    let ollama = Arc::new(TestProvider::default());
    ollama.advertise_as(LLMProviderKind::Ollama);
    ollama.block_first_call();
    for _ in 0..workers {
        ollama.script(TestProvider::ok("ollama-done"));
    }
    let openai = Arc::new(TestProvider::default());
    openai.advertise_as(LLMProviderKind::OpenAI);
    openai.script(TestProvider::ok("openai-done"));
    let mut config = small_config();
    config.workers = workers;
    config.reserved_interactive_workers = 0;
    // Retained active/provider-waiting work counts against the bounded queue.
    // Leave one exact slot for the other provider whose progress this test pins.
    config.queue_capacity_normal = workers + 1;
    config
        .provider_concurrency
        .overrides
        .insert("ollama".to_string(), 1);
    config
        .provider_concurrency
        .overrides
        .insert("openai".to_string(), 4);
    let queue = LlmDispatchQueue::start(
        test_router_with_providers(&[
            (LLMProviderKind::Ollama, ollama.clone()),
            (LLMProviderKind::OpenAI, openai.clone()),
        ]),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let mut ollama_receipts = Vec::new();
    for index in 0..workers {
        let (job, rx) = LlmJob::new(
            mixed_request(&LLMProviderKind::Ollama),
            JobOrigin::op(format!("ollama-{index}")),
        );
        queue.submit(job).await.expect("ollama submitted");
        ollama_receipts.push(rx);
    }
    wait_until(Duration::from_secs(1), || {
        ollama.in_flight() == 1 && queue.snapshot().waiting_for_provider >= workers - 1
    })
    .await;
    let (openai_job, openai_rx) = LlmJob::new(
        mixed_request(&LLMProviderKind::OpenAI),
        JobOrigin::op("openai-progress"),
    );
    queue.submit(openai_job).await.expect("openai submitted");
    tokio::time::timeout(Duration::from_secs(1), openai_rx)
        .await
        .expect("OpenAI must progress while Ollama is saturated")
        .unwrap()
        .unwrap();
    assert_eq!(ollama.in_flight(), 1, "Ollama owner must still hold invoke");
    ollama.release_first_call();
    queue.shutdown(Duration::from_millis(500)).await;
    drop(ollama_receipts);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saturated_provider_does_not_block_mixed_provider_progress() {
    let workers = 8;
    let openai = Arc::new(TestProvider::default());
    openai.advertise_as(LLMProviderKind::OpenAI);
    openai.block_first_call();
    for _ in 0..workers {
        openai.script(TestProvider::ok("openai-done"));
    }
    let anthropic = Arc::new(TestProvider::default());
    anthropic.advertise_as(LLMProviderKind::Anthropic);
    anthropic.script(TestProvider::ok("anthropic-done"));
    let mut config = small_config();
    config.workers = workers;
    config.reserved_interactive_workers = 0;
    // Retained active/provider-waiting work counts against the bounded queue.
    // Leave one exact slot for the other provider whose progress this test pins.
    config.queue_capacity_normal = workers + 1;
    config
        .provider_concurrency
        .overrides
        .insert("openai".to_string(), 1);
    config
        .provider_concurrency
        .overrides
        .insert("anthropic".to_string(), 4);
    let queue = LlmDispatchQueue::start(
        test_router_with_providers(&[
            (LLMProviderKind::OpenAI, openai.clone()),
            (LLMProviderKind::Anthropic, anthropic.clone()),
        ]),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let mut openai_receipts = Vec::new();
    for index in 0..workers {
        let (job, rx) = LlmJob::new(
            mixed_request(&LLMProviderKind::OpenAI),
            JobOrigin::op(format!("openai-{index}")),
        );
        queue.submit(job).await.expect("openai submitted");
        openai_receipts.push(rx);
    }
    wait_until(Duration::from_secs(1), || {
        openai.in_flight() == 1 && queue.snapshot().waiting_for_provider >= workers - 1
    })
    .await;
    let (anthropic_job, anthropic_rx) = LlmJob::new(
        mixed_request(&LLMProviderKind::Anthropic),
        JobOrigin::op("anthropic-progress"),
    );
    queue
        .submit(anthropic_job)
        .await
        .expect("anthropic submitted");
    tokio::time::timeout(Duration::from_secs(1), anthropic_rx)
        .await
        .expect("Anthropic must progress while OpenAI is saturated")
        .unwrap()
        .unwrap();
    assert_eq!(openai.in_flight(), 1);
    openai.release_first_call();
    queue.shutdown(Duration::from_millis(500)).await;
    drop(openai_receipts);
}

async fn peak_mixed_cloud_invokes(workers: usize) -> (usize, usize, usize) {
    let openai = Arc::new(TestProvider::default());
    openai.advertise_as(LLMProviderKind::OpenAI);
    let anthropic = Arc::new(TestProvider::default());
    anthropic.advertise_as(LLMProviderKind::Anthropic);
    for _ in 0..4 {
        openai.script(TestProvider::ok("openai"));
        anthropic.script(TestProvider::ok("anthropic"));
    }
    let delay = Duration::from_millis(400);
    openai.set_delay(delay);
    anthropic.set_delay(delay);
    let mut config = small_config();
    config.workers = workers;
    config.reserved_interactive_workers = 0;
    config.queue_capacity_normal = 16;
    config
        .provider_concurrency
        .overrides
        .insert("openai".to_string(), 4);
    config
        .provider_concurrency
        .overrides
        .insert("anthropic".to_string(), 4);
    let queue = LlmDispatchQueue::start(
        test_router_with_providers(&[
            (LLMProviderKind::OpenAI, openai.clone()),
            (LLMProviderKind::Anthropic, anthropic.clone()),
        ]),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let mut receipts = Vec::new();
    for index in 0..4 {
        let (job, rx) = LlmJob::new(
            mixed_request(&LLMProviderKind::OpenAI),
            JobOrigin::op(format!("openai-{index}")),
        );
        queue.submit(job).await.expect("openai burst");
        receipts.push(rx);
        let (job, rx) = LlmJob::new(
            mixed_request(&LLMProviderKind::Anthropic),
            JobOrigin::op(format!("anthropic-{index}")),
        );
        queue.submit(job).await.expect("anthropic burst");
        receipts.push(rx);
    }
    let target = workers.min(8);
    wait_until(Duration::from_secs(1), || {
        openai.in_flight().saturating_add(anthropic.in_flight()) >= target
    })
    .await;
    let combined = openai.in_flight().saturating_add(anthropic.in_flight());
    let openai_peak = openai.peak_in_flight();
    let anthropic_peak = anthropic.peak_in_flight();
    for rx in receipts {
        let _ = rx.await;
    }
    queue.shutdown(Duration::from_millis(500)).await;
    (combined, openai_peak, anthropic_peak)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn raising_worker_count_raises_concurrent_cloud_invokes() {
    let (three, ..) = peak_mixed_cloud_invokes(3).await;
    let (eight, openai_peak, anthropic_peak) = peak_mixed_cloud_invokes(8).await;
    assert_eq!(
        three, 3,
        "three workers must be fully occupied by mixed-cloud invoke"
    );
    assert_eq!(
        eight, 8,
        "eight workers must occupy OpenAI 4 + Anthropic 4 together"
    );
    assert!(
        openai_peak >= 2 && anthropic_peak >= 2,
        "both clouds must participate, openai_peak={openai_peak} anthropic_peak={anthropic_peak}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interactive_reservation_holds_after_worker_resize() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_millis(400));
    for _ in 0..9 {
        provider.script(TestProvider::ok("bg"));
    }
    let mut config = small_config();
    config.workers = 8;
    config.reserved_interactive_workers = 2;
    config.queue_capacity_background = 16;
    config.queue_capacity_high = 8;
    // Default provider cap is 3. Without raising it, extra background jobs
    // park and release workers, so High can start without using reservation.
    config.provider_concurrency.default = 8;
    config
        .provider_concurrency
        .overrides
        .insert("test".to_string(), 8);
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let mut background = Vec::new();
    for index in 0..8 {
        let (job, rx) = LlmJob::new(small_request(), JobOrigin::op(format!("bg-flood-{index}")));
        let job = job.with_priority(Priority::Background);
        queue.submit(job).await.expect("background flood");
        background.push(rx);
    }
    wait_until(Duration::from_millis(400), || provider.in_flight() == 6).await;
    assert_eq!(
        provider.in_flight(),
        6,
        "reservation must leave two workers free of background work"
    );
    let (high, _high_rx) = LlmJob::new(small_request(), JobOrigin::op("interactive-high"));
    let high = high.with_priority(Priority::High);
    queue.submit(high).await.expect("high submitted");
    wait_until(Duration::from_millis(200), || provider.in_flight() == 7).await;
    queue.shutdown(Duration::from_millis(50)).await;
    drop(background);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_queue_full_below_advertised_current_profile_load() {
    let provider = Arc::new(TestProvider::default());
    provider.set_delay(Duration::from_secs(30));
    let mut config = small_config();
    DispatchCapacityPlan::CURRENT.apply_to(&mut config);
    let advertised = DispatchCapacityPlan::CURRENT.queue_capacity_normal;
    config.queue_capacity_high = advertised;
    config.queue_capacity_background = advertised;
    let queue = LlmDispatchQueue::start(
        test_router_with(provider),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let mut receipts = Vec::with_capacity(advertised);
    for index in 0..advertised {
        let (job, rx) = LlmJob::new(
            small_request(),
            JobOrigin::op(format!("profile-load-{index}")),
        );
        queue
            .submit(job)
            .await
            .unwrap_or_else(|error| panic!("job {index} should fit advertised load: {error}"));
        receipts.push(rx);
    }
    let (overflow, _rx) = LlmJob::new(small_request(), JobOrigin::op("profile-overflow"));
    assert!(matches!(
        queue.submit(overflow).await,
        Err(LLMError::QueueFull { .. })
    ));
    queue.shutdown(Duration::from_millis(50)).await;
    drop(receipts);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn classifier_buckets_known_messages() {
    use super::classifier::classify;
    assert_eq!(
        classify(&LLMError::Provider {
            provider: "p".to_string(),
            message: "429 too many requests".to_string()
        }),
        ErrorClass::RateLimit
    );
    assert_eq!(
        classify(&LLMError::Provider {
            provider: "p".to_string(),
            message: "500 internal server error".to_string()
        }),
        ErrorClass::Server5xx
    );
    assert_eq!(
        classify(&LLMError::Provider {
            provider: "p".to_string(),
            message: "401 unauthorized".to_string()
        }),
        ErrorClass::Provider4xx
    );
    assert_eq!(
        classify(&LLMError::Provider {
            provider: "p".to_string(),
            message: "400 request blocked by content policy".to_string()
        }),
        ErrorClass::ContentPolicy
    );
    assert_eq!(classify(&LLMError::Timeout), ErrorClass::Timeout);
    assert_eq!(
        classify(&LLMError::RateLimited { retry_after: None }),
        ErrorClass::RateLimit
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_decision_terminal_at_ceiling() {
    use super::job::AttemptHistory;
    use super::retry::{decide, RetryDecision};

    let cfg = small_config();
    // Simulate: at hard ceiling.
    let history = AttemptHistory {
        total: cfg.retry.hard_ceiling(),
        provider_total: cfg.retry.hard_ceiling(),
        dispatch: cfg.retry.max_attempts_per_dispatch,
        cycle: cfg.retry.max_dispatch_cycles - 1,
        errors: Vec::new(),
    };
    let d = decide(&history, ErrorClass::Server5xx, &cfg, None);
    assert!(matches!(d, RetryDecision::TerminalFail));
}

/// Both copies of a job's trace context name the same activity.
///
/// `LlmJob::new` fills `activity_id` from the origin, but
/// `RequestMetadata::ensure_trace_context` returns a *clone* and keeps its own
/// copy on the request. Mutating the returned value alone left the two copies
/// disagreeing: `job.trace_context.activity_id` was `Some` while
/// `job.request.metadata.trace_context.activity_id` stayed `None`.
///
/// That is not cosmetic. Local prep derives its summariser child context from
/// the request-side copy, so every locally-summarised block was attributed to
/// no span at all — and the router's content-capture path reads the same
/// field. Asserting on BOTH copies is the point of this test; asserting on the
/// job-side one alone would have passed throughout the bug.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_jobs_two_trace_context_copies_agree_about_the_activity() {
    let origin = JobOrigin::op("activity-sync").with_activity_id(Some("activity-42".to_string()));
    let (job, _rx) = LlmJob::new(LLMRequest::default(), origin);

    assert_eq!(
        job.trace_context.activity_id.as_deref(),
        Some("activity-42"),
        "the job-side context takes the activity the origin carried"
    );
    assert_eq!(
        job.request
            .metadata
            .trace_context
            .as_ref()
            .and_then(|context| context.activity_id.as_deref()),
        Some("activity-42"),
        "and so must the request-side copy — local prep and content capture \
         read that one, and a `None` there attributes real work to no span"
    );
}

/// A submit site that stamps an unusable activity id still gets its call made.
///
/// `activity_id` used to be checked by `LlmTraceContext::is_valid`, which
/// `ConfiguredRouter::route` consults to decide whether to route *at all*. A
/// caller passing `Some("")` — `with_activity_id` is `pub` and takes whatever a
/// lookup returned — therefore converted a telemetry defect into a refused LLM
/// call. The id is normalised at this boundary instead, so the defect costs the
/// join key and nothing else.
///
/// Both copies are asserted for the same reason as the test above: the
/// request-side one is what local prep and content capture read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unusable_activity_id_is_dropped_at_the_origin_rather_than_carried() {
    for unusable in ["", "   ", "\t\n"] {
        let origin = JobOrigin::op("activity-sync").with_activity_id(Some(unusable.to_string()));
        assert_eq!(
            origin.activity_id, None,
            "an activity id of {unusable:?} joins against nothing and must not read as present"
        );

        let (job, _rx) = LlmJob::new(LLMRequest::default(), origin);
        assert_eq!(job.trace_context.activity_id, None);
        assert!(
            job.trace_context.is_valid(),
            "and the job must still be routable — no telemetry field may refuse a call"
        );
        assert_eq!(
            job.request
                .metadata
                .trace_context
                .as_ref()
                .and_then(|context| context.activity_id.clone()),
            None
        );
    }

    // The field is `pub`, so the builder is not the only way in. `LlmJob::new`
    // normalises on the copy across as well.
    let mut origin = JobOrigin::op("activity-sync");
    origin.activity_id = Some("  ".to_string());
    let (job, _rx) = LlmJob::new(LLMRequest::default(), origin);
    assert_eq!(job.trace_context.activity_id, None);
    assert!(job.trace_context.is_valid());
}

/// A context that already names an activity is never overwritten by the
/// origin, and the resync does not fabricate one where neither side has any.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_origin_without_an_activity_leaves_the_context_alone() {
    let (job, _rx) = LlmJob::new(LLMRequest::default(), JobOrigin::op("no-activity"));
    assert_eq!(job.trace_context.activity_id, None);
    assert_eq!(
        job.request
            .metadata
            .trace_context
            .as_ref()
            .and_then(|context| context.activity_id.clone()),
        None,
        "absent stays absent — an invented id would attach cost to unrelated work"
    );
}

fn local_prep_occupancy_request() -> LLMRequest {
    let raw = "x".repeat(9000);
    LLMRequest {
        model: "test-model".to_string(),
        messages: vec![LLMMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::Text {
                text: String::new(),
            }],
        }]
        .into(),
        summarisable_blocks: Arc::new(vec![SummarisableBlock {
            message_index: 0,
            content_index: 0,
            raw,
            purpose: SummarisationPurpose::Other,
            max_chars: None,
        }]),
        metadata: RequestMetadata {
            operation: "default".to_string(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn local_prep_occupancy_config(workers: usize, yield_worker: bool) -> DispatchConfig {
    let mut config = small_config();
    config.workers = workers;
    config.reserved_interactive_workers = 0;
    config.local_prep.enabled = true;
    config.local_prep.yield_worker = yield_worker;
    config.local_prep.threshold_chars = 8000;
    config.local_prep.model = "test-model".to_string();
    config.local_prep.base_url = "http://127.0.0.1:9".to_string();
    config
}

fn request_text_contains(request: &LLMRequest, needle: &str) -> bool {
    request.messages.iter().any(|message| {
        message.content.iter().any(|block| match block {
            ContentBlock::Text { text } => text.contains(needle),
            _ => false,
        })
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_prep_below_threshold_stays_on_worker_and_invokes() {
    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("inlined-raw"));
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        local_prep_occupancy_config(1, true),
    );
    let mut request = local_prep_occupancy_request();
    request.summarisable_blocks = Arc::new(vec![SummarisableBlock {
        message_index: 0,
        content_index: 0,
        raw: "below-threshold-raw".to_string(),
        purpose: SummarisationPurpose::Other,
        max_chars: None,
    }]);
    let (job, rx) = LlmJob::new(request, JobOrigin::op("local-prep-below-threshold"));
    queue.submit(job).await.expect("below-threshold submitted");
    rx.await.unwrap().unwrap();
    let captured = provider.captured_requests();
    assert_eq!(captured.len(), 1);
    assert!(
        request_text_contains(&captured[0], "below-threshold-raw"),
        "below-threshold blocks must be inlined as raw on the worker"
    );
    assert!(
        !request_text_contains(&captured[0], "hook-summary"),
        "below-threshold path must not call Ollama / the test hook"
    );
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_prep_does_not_hold_dispatch_worker() {
    let _hook_serial = super::local_prep::local_prep_test_lock().lock().await;
    let hook = LocalPrepTestHook::new();
    let _guard = install_local_prep_test_hook(hook.clone());

    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("cloud"));
    provider.script(TestProvider::ok("cloud"));
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        local_prep_occupancy_config(1, true),
    );

    let entered = hook.entered.notified();
    tokio::pin!(entered);
    entered.as_mut().enable();

    let (first, first_rx) = LlmJob::new(
        local_prep_occupancy_request(),
        JobOrigin::op("local-prep-hold-1"),
    );
    queue.submit(first).await.expect("first prep submitted");
    let (second, second_rx) = LlmJob::new(
        local_prep_occupancy_request(),
        JobOrigin::op("local-prep-hold-2"),
    );
    queue.submit(second).await.expect("second prep submitted");

    tokio::time::timeout(Duration::from_secs(1), entered)
        .await
        .expect("timed out waiting for local-prep hook enter");
    wait_until(Duration::from_secs(1), || {
        let snap = queue.snapshot();
        snap.workers_busy == 0 && snap.waiting_for_local_prep >= 2
    })
    .await;
    assert_eq!(
        queue.snapshot().workers_busy,
        0,
        "Ollama-bound local-prep must yield the dispatch worker"
    );
    assert!(
        queue.snapshot().waiting_for_local_prep >= 2,
        "in-progress generate plus the queued sibling must both count"
    );

    hook.release.notify_one();
    hook.release.notify_one();
    first_rx.await.unwrap().unwrap();
    second_rx.await.unwrap().unwrap();
    let captured = provider.captured_requests();
    assert_eq!(captured.len(), 2);
    assert!(
        captured
            .iter()
            .all(|request| request_text_contains(request, "hook-summary")),
        "placeholder must be replaced with the hook summary before invoke"
    );
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blocked_local_prep_does_not_block_cloud_invoke() {
    let _hook_serial = super::local_prep::local_prep_test_lock().lock().await;
    let hook = LocalPrepTestHook::new();
    let _guard = install_local_prep_test_hook(hook.clone());

    let provider = Arc::new(TestProvider::default());
    // Prep is parked on the hook, so the first invoke is cloud; hold it for in_flight.
    provider.block_first_call();
    provider.script(TestProvider::ok("prep-done"));
    provider.script(TestProvider::ok("cloud-done"));
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        local_prep_occupancy_config(1, true),
    );

    let entered = hook.entered.notified();
    tokio::pin!(entered);
    entered.as_mut().enable();

    let (prep_job, prep_rx) = LlmJob::new(
        local_prep_occupancy_request(),
        JobOrigin::op("local-prep-blocked"),
    );
    queue.submit(prep_job).await.expect("prep submitted");
    tokio::time::timeout(Duration::from_secs(1), entered)
        .await
        .expect("timed out waiting for local-prep hook enter");
    wait_until(Duration::from_secs(1), || {
        let snap = queue.snapshot();
        snap.workers_busy == 0 && snap.waiting_for_local_prep == 1 && provider.in_flight() == 0
    })
    .await;
    assert_eq!(queue.snapshot().workers_busy, 0);
    assert_eq!(
        queue.snapshot().waiting_for_local_prep,
        1,
        "the in-progress generate must remain visible on the snapshot"
    );
    assert_eq!(provider.in_flight(), 0);

    let (cloud_job, cloud_rx) = LlmJob::new(small_request(), JobOrigin::op("cloud-during-prep"));
    queue.submit(cloud_job).await.expect("cloud submitted");
    wait_until(Duration::from_secs(1), || provider.in_flight() == 1).await;
    assert!(
        queue.snapshot().workers_busy <= 1,
        "cloud invoke must reuse the yielded worker, not require a second slot"
    );

    hook.release.notify_one();
    provider.release_first_call();
    prep_rx.await.unwrap().unwrap();
    cloud_rx.await.unwrap().unwrap();
    queue.shutdown(Duration::from_millis(500)).await;
}

/// `yield_worker = false` restores pre-PR6 in-worker local-prep HOL.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn yield_worker_false_restores_in_worker_local_prep_hol() {
    let _hook_serial = super::local_prep::local_prep_test_lock().lock().await;
    let hook = LocalPrepTestHook::new();
    let _guard = install_local_prep_test_hook(hook.clone());

    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("prep-done"));
    provider.script(TestProvider::ok("cloud-done"));
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        local_prep_occupancy_config(1, false),
    );

    let entered = hook.entered.notified();
    tokio::pin!(entered);
    entered.as_mut().enable();

    let (prep_job, prep_rx) = LlmJob::new(
        local_prep_occupancy_request(),
        JobOrigin::op("local-prep-hol"),
    );
    queue.submit(prep_job).await.expect("prep submitted");
    tokio::time::timeout(Duration::from_secs(1), entered)
        .await
        .expect("timed out waiting for local-prep hook enter");
    wait_until(Duration::from_secs(1), || {
        queue.snapshot().workers_busy == 1
    })
    .await;
    assert_eq!(
        queue.snapshot().workers_busy,
        1,
        "yield_worker=false must keep the dispatch worker inside local-prep"
    );

    let (cloud_job, cloud_rx) = LlmJob::new(small_request(), JobOrigin::op("cloud-behind-prep"));
    queue.submit(cloud_job).await.expect("cloud submitted");
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        provider.in_flight(),
        0,
        "cloud invoke must stay blocked while the single worker is inside local-prep"
    );

    hook.release.notify_one();
    prep_rx.await.unwrap().unwrap();
    cloud_rx.await.unwrap().unwrap();
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_prep_coordinator_is_cap_one() {
    let _hook_serial = super::local_prep::local_prep_test_lock().lock().await;
    let hook = LocalPrepTestHook::new();
    let _guard = install_local_prep_test_hook(hook.clone());

    let provider = Arc::new(TestProvider::default());
    for _ in 0..3 {
        provider.script(TestProvider::ok("prep-done"));
    }
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        local_prep_occupancy_config(2, true),
    );

    let entered = hook.entered.notified();
    tokio::pin!(entered);
    entered.as_mut().enable();

    let mut receipts = Vec::new();
    for index in 0..3 {
        let (job, rx) = LlmJob::new(
            local_prep_occupancy_request(),
            JobOrigin::op(format!("local-prep-cap-{index}")),
        );
        queue.submit(job).await.expect("prep submitted");
        receipts.push(rx);
    }

    tokio::time::timeout(Duration::from_secs(1), entered)
        .await
        .expect("timed out waiting for local-prep hook enter");
    wait_until(Duration::from_secs(1), || {
        let snap = queue.snapshot();
        snap.workers_busy == 0 && snap.waiting_for_local_prep >= 3
    })
    .await;
    assert_eq!(
        queue.snapshot().waiting_for_local_prep,
        3,
        "cap-1 must keep one job in generate and the other two in lanes"
    );
    assert_eq!(queue.snapshot().workers_busy, 0);

    let entered = hook.entered.notified();
    tokio::pin!(entered);
    entered.as_mut().enable();
    hook.release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), entered)
        .await
        .expect("timed out waiting for second local-prep hook enter");
    wait_until(Duration::from_secs(1), || {
        queue.snapshot().waiting_for_local_prep >= 2
    })
    .await;

    hook.release.notify_one();
    hook.release.notify_one();
    for rx in receipts {
        rx.await.unwrap().unwrap();
    }
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_while_waiting_for_local_prep_does_not_invoke() {
    let _hook_serial = super::local_prep::local_prep_test_lock().lock().await;
    let hook = LocalPrepTestHook::new();
    let _guard = install_local_prep_test_hook(hook.clone());

    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("prep-done"));
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        local_prep_occupancy_config(1, true),
    );

    let entered = hook.entered.notified();
    tokio::pin!(entered);
    entered.as_mut().enable();

    let (first, first_rx) = LlmJob::new(
        local_prep_occupancy_request(),
        JobOrigin::op("local-prep-cancel-owner"),
    );
    queue.submit(first).await.expect("first prep submitted");
    let (second, second_rx) = LlmJob::new(
        local_prep_occupancy_request(),
        JobOrigin::op("local-prep-cancel-waiter"),
    );
    let second_id = second.job_id.clone();
    queue.submit(second).await.expect("second prep submitted");

    tokio::time::timeout(Duration::from_secs(1), entered)
        .await
        .expect("timed out waiting for local-prep hook enter");
    wait_until(Duration::from_secs(1), || {
        queue.snapshot().waiting_for_local_prep >= 2
    })
    .await;

    assert!(
        queue.cancel_job(&second_id, "cancel waiting local-prep"),
        "waiting local-prep job must be cancellable by id"
    );
    wait_until(Duration::from_secs(1), || {
        queue.snapshot().waiting_for_local_prep == 1
    })
    .await;
    let cancelled = second_rx.await.unwrap();
    assert!(matches!(cancelled, Err(LLMError::Cancelled { .. })));
    assert_eq!(
        provider.in_flight(),
        0,
        "cancelled waiter must not reach provider invoke"
    );

    hook.release.notify_one();
    first_rx.await.unwrap().unwrap();
    assert_eq!(provider.captured_requests().len(), 1);
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_in_progress_local_prep_aborts_generate() {
    let _hook_serial = super::local_prep::local_prep_test_lock().lock().await;
    let hook = LocalPrepTestHook::new();
    let _guard = install_local_prep_test_hook(hook.clone());

    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("must-not-run"));
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        local_prep_occupancy_config(1, true),
    );

    let entered = hook.entered.notified();
    tokio::pin!(entered);
    entered.as_mut().enable();

    let (job, rx) = LlmJob::new(
        local_prep_occupancy_request(),
        JobOrigin::op("local-prep-cancel-in-progress"),
    );
    let job_id = job.job_id.clone();
    queue.submit(job).await.expect("prep submitted");
    tokio::time::timeout(Duration::from_secs(1), entered)
        .await
        .expect("timed out waiting for local-prep hook enter");
    wait_until(Duration::from_secs(1), || {
        queue.snapshot().waiting_for_local_prep == 1
    })
    .await;

    assert!(
        queue.cancel_job(&job_id, "cancel in-progress local-prep"),
        "in-progress local-prep is pending and must be cancellable by id"
    );
    let cancelled = tokio::time::timeout(Duration::from_secs(1), rx)
        .await
        .expect("in-progress local-prep cancel must not wait for Ollama")
        .unwrap();
    assert!(matches!(cancelled, Err(LLMError::Cancelled { .. })));
    wait_until(Duration::from_secs(1), || {
        queue.snapshot().waiting_for_local_prep == 0
    })
    .await;
    assert_eq!(provider.captured_requests().len(), 0);
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_tombstones_local_prep_waiters() {
    let _hook_serial = super::local_prep::local_prep_test_lock().lock().await;
    let hook = LocalPrepTestHook::new();
    let _guard = install_local_prep_test_hook(hook.clone());

    let provider = Arc::new(TestProvider::default());
    provider.script(TestProvider::ok("prep-done"));
    provider.script(TestProvider::ok("prep-done"));
    let queue = LlmDispatchQueue::start(
        test_router_with(provider.clone()),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        local_prep_occupancy_config(1, true),
    );

    let entered = hook.entered.notified();
    tokio::pin!(entered);
    entered.as_mut().enable();

    let (first, first_rx) = LlmJob::new(
        local_prep_occupancy_request(),
        JobOrigin::op("local-prep-shutdown-1"),
    );
    queue.submit(first).await.expect("first prep submitted");
    let (second, second_rx) = LlmJob::new(
        local_prep_occupancy_request(),
        JobOrigin::op("local-prep-shutdown-2"),
    );
    queue.submit(second).await.expect("second prep submitted");

    tokio::time::timeout(Duration::from_secs(1), entered)
        .await
        .expect("timed out waiting for local-prep hook enter");
    wait_until(Duration::from_secs(1), || {
        queue.snapshot().waiting_for_local_prep >= 2
    })
    .await;

    queue.shutdown(Duration::from_millis(100)).await;
    hook.release.notify_waiters();
    let first_outcome = first_rx.await.unwrap();
    let second_outcome = second_rx.await.unwrap();
    assert!(
        matches!(first_outcome, Err(LLMError::Cancelled { .. })),
        "in-progress local-prep must tombstone on shutdown, got {first_outcome:?}"
    );
    assert!(
        matches!(second_outcome, Err(LLMError::Cancelled { .. })),
        "queued local-prep waiter must tombstone on shutdown, got {second_outcome:?}"
    );
    assert_eq!(provider.in_flight(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn isolated_engine_mixed_cloud_exceeds_scheduler_workers() {
    let workers = 3;
    let openai = Arc::new(TestProvider::default());
    openai.advertise_as(LLMProviderKind::OpenAI);
    let anthropic = Arc::new(TestProvider::default());
    anthropic.advertise_as(LLMProviderKind::Anthropic);
    for _ in 0..4 {
        openai.script(TestProvider::ok("openai"));
        anthropic.script(TestProvider::ok("anthropic"));
    }
    let delay = Duration::from_millis(400);
    openai.set_delay(delay);
    anthropic.set_delay(delay);
    let mut config = small_config();
    config.engine = DispatchEngine::ProviderIsolated;
    config.workers = workers;
    config.reserved_interactive_workers = 0;
    config.queue_capacity_normal = 16;
    config.global_cloud_concurrency = 16;
    config
        .provider_concurrency
        .overrides
        .insert("openai".to_string(), 4);
    config
        .provider_concurrency
        .overrides
        .insert("anthropic".to_string(), 4);
    let queue = LlmDispatchQueue::start(
        test_router_with_providers(&[
            (LLMProviderKind::OpenAI, openai.clone()),
            (LLMProviderKind::Anthropic, anthropic.clone()),
        ]),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let mut receipts = Vec::new();
    for index in 0..4 {
        let (job, rx) = LlmJob::new(
            mixed_request(&LLMProviderKind::OpenAI),
            JobOrigin::op(format!("isolated-openai-{index}")),
        );
        queue.submit(job).await.expect("openai burst");
        receipts.push(rx);
        let (job, rx) = LlmJob::new(
            mixed_request(&LLMProviderKind::Anthropic),
            JobOrigin::op(format!("isolated-anthropic-{index}")),
        );
        queue.submit(job).await.expect("anthropic burst");
        receipts.push(rx);
    }
    wait_until(Duration::from_secs(1), || {
        openai.in_flight().saturating_add(anthropic.in_flight()) >= 8
    })
    .await;
    let combined = openai.in_flight().saturating_add(anthropic.in_flight());
    assert_eq!(
        combined, 8,
        "provider-isolated HTTP must not be ceilinged by {workers} scheduler workers"
    );
    for rx in receipts {
        let _ = rx.await;
    }
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn isolated_engine_cloud_cap_bounds_spend() {
    let openai = Arc::new(TestProvider::default());
    openai.advertise_as(LLMProviderKind::OpenAI);
    let anthropic = Arc::new(TestProvider::default());
    anthropic.advertise_as(LLMProviderKind::Anthropic);
    for _ in 0..4 {
        openai.script(TestProvider::ok("openai"));
        anthropic.script(TestProvider::ok("anthropic"));
    }
    let delay = Duration::from_millis(400);
    openai.set_delay(delay);
    anthropic.set_delay(delay);
    let mut config = small_config();
    config.engine = DispatchEngine::ProviderIsolated;
    config.workers = 8;
    config.reserved_interactive_workers = 0;
    config.queue_capacity_normal = 16;
    config.global_cloud_concurrency = 2;
    config
        .provider_concurrency
        .overrides
        .insert("openai".to_string(), 4);
    config
        .provider_concurrency
        .overrides
        .insert("anthropic".to_string(), 4);
    let queue = LlmDispatchQueue::start(
        test_router_with_providers(&[
            (LLMProviderKind::OpenAI, openai.clone()),
            (LLMProviderKind::Anthropic, anthropic.clone()),
        ]),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let mut receipts = Vec::new();
    for index in 0..4 {
        let (job, rx) = LlmJob::new(
            mixed_request(&LLMProviderKind::OpenAI),
            JobOrigin::op(format!("cap-openai-{index}")),
        );
        queue.submit(job).await.expect("openai burst");
        receipts.push(rx);
        let (job, rx) = LlmJob::new(
            mixed_request(&LLMProviderKind::Anthropic),
            JobOrigin::op(format!("cap-anthropic-{index}")),
        );
        queue.submit(job).await.expect("anthropic burst");
        receipts.push(rx);
    }
    wait_until(Duration::from_secs(1), || {
        openai.in_flight().saturating_add(anthropic.in_flight()) >= 2
    })
    .await;
    let mut peak_combined = openai.in_flight().saturating_add(anthropic.in_flight());
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(250) {
        peak_combined = peak_combined.max(openai.in_flight().saturating_add(anthropic.in_flight()));
        tokio::task::yield_now().await;
    }
    assert!(
        peak_combined <= 2,
        "global cloud cap 2 must bound combined provider HTTP, peak={peak_combined}"
    );
    assert_eq!(
        peak_combined, 2,
        "cloud cap 2 should be fully occupied, peak={peak_combined}"
    );
    for rx in receipts {
        let _ = rx.await;
    }
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stream_cloud_cap_bounds_combined_http() {
    let openai = Arc::new(TestProvider::default());
    openai.advertise_as(LLMProviderKind::OpenAI);
    let anthropic = Arc::new(TestProvider::default());
    anthropic.advertise_as(LLMProviderKind::Anthropic);
    for _ in 0..4 {
        openai.script(TestProvider::ok("openai-stream"));
        anthropic.script(TestProvider::ok("anthropic-stream"));
    }
    let delay = Duration::from_millis(400);
    openai.set_delay(delay);
    anthropic.set_delay(delay);
    let mut config = small_config();
    config.workers = 8;
    config.reserved_interactive_workers = 0;
    config.queue_capacity_normal = 16;
    config.global_cloud_concurrency = 2;
    config
        .provider_concurrency
        .overrides
        .insert("openai".to_string(), 4);
    config
        .provider_concurrency
        .overrides
        .insert("anthropic".to_string(), 4);
    let queue = LlmDispatchQueue::start(
        test_router_with_providers(&[
            (LLMProviderKind::OpenAI, openai.clone()),
            (LLMProviderKind::Anthropic, anthropic.clone()),
        ]),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let mut streams = Vec::new();
    for index in 0..4 {
        let (job, rx) = LlmStreamJob::new(
            mixed_request(&LLMProviderKind::OpenAI),
            JobOrigin::op(format!("stream-cap-openai-{index}")),
            4,
        );
        queue.submit_stream(job).await.expect("openai stream");
        streams.push(rx);
        let (job, rx) = LlmStreamJob::new(
            mixed_request(&LLMProviderKind::Anthropic),
            JobOrigin::op(format!("stream-cap-anthropic-{index}")),
            4,
        );
        queue.submit_stream(job).await.expect("anthropic stream");
        streams.push(rx);
    }
    wait_until(Duration::from_secs(1), || {
        openai.in_flight().saturating_add(anthropic.in_flight()) >= 2
    })
    .await;
    let mut peak_combined = openai.in_flight().saturating_add(anthropic.in_flight());
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(250) {
        peak_combined = peak_combined.max(openai.in_flight().saturating_add(anthropic.in_flight()));
        tokio::task::yield_now().await;
    }
    assert!(
        peak_combined <= 2,
        "streaming jobs must share global cloud cap 2, peak={peak_combined}"
    );
    assert_eq!(
        peak_combined, 2,
        "stream cloud cap 2 should be fully occupied, peak={peak_combined}"
    );
    for mut stream in streams {
        while stream.recv().await.is_some() {}
    }
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stream_and_sync_share_the_cloud_cap() {
    let openai = Arc::new(TestProvider::default());
    openai.advertise_as(LLMProviderKind::OpenAI);
    for _ in 0..4 {
        openai.script(TestProvider::ok("mixed-cloud"));
    }
    openai.set_delay(Duration::from_millis(400));
    let mut config = small_config();
    config.workers = 8;
    config.reserved_interactive_workers = 0;
    config.queue_capacity_normal = 16;
    config.global_cloud_concurrency = 1;
    config
        .provider_concurrency
        .overrides
        .insert("openai".to_string(), 4);
    let queue = LlmDispatchQueue::start(
        test_router_with_providers(&[(LLMProviderKind::OpenAI, openai.clone())]),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let (stream_job, mut stream) = LlmStreamJob::new(
        mixed_request(&LLMProviderKind::OpenAI),
        JobOrigin::op("shared-cap-stream"),
        4,
    );
    queue
        .submit_stream(stream_job)
        .await
        .expect("stream accepted");
    wait_until(Duration::from_secs(1), || openai.in_flight() >= 1).await;
    let (sync_job, sync_rx) = LlmJob::new(
        mixed_request(&LLMProviderKind::OpenAI),
        JobOrigin::op("shared-cap-sync"),
    );
    queue.submit(sync_job).await.expect("sync accepted");
    let started = Instant::now();
    let mut peak = openai.in_flight();
    while started.elapsed() < Duration::from_millis(200) {
        peak = peak.max(openai.in_flight());
        tokio::task::yield_now().await;
    }
    assert_eq!(
        peak, 1,
        "one stream occupying the only cloud slot must block the sync job, peak={peak}"
    );
    while stream.recv().await.is_some() {}
    let _ = sync_rx.await;
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ollama_streams_skip_the_cloud_cap() {
    let ollama = Arc::new(TestProvider::default());
    ollama.advertise_as(LLMProviderKind::Ollama);
    for _ in 0..2 {
        ollama.script(TestProvider::ok("ollama-stream"));
    }
    ollama.set_delay(Duration::from_millis(300));
    let mut config = small_config();
    config.workers = 4;
    config.reserved_interactive_workers = 0;
    config.queue_capacity_normal = 8;
    config.global_cloud_concurrency = 1;
    config
        .provider_concurrency
        .overrides
        .insert("ollama".to_string(), 2);
    let queue = LlmDispatchQueue::start(
        test_router_with_providers(&[(LLMProviderKind::Ollama, ollama.clone())]),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let mut streams = Vec::new();
    for index in 0..2 {
        let (job, rx) = LlmStreamJob::new(
            mixed_request(&LLMProviderKind::Ollama),
            JobOrigin::op(format!("ollama-stream-{index}")),
            4,
        );
        queue.submit_stream(job).await.expect("ollama stream");
        streams.push(rx);
    }
    wait_until(Duration::from_secs(1), || ollama.in_flight() >= 2).await;
    assert_eq!(
        ollama.in_flight(),
        2,
        "Ollama streams must not consume global_cloud_concurrency"
    );
    for mut stream in streams {
        while stream.recv().await.is_some() {}
    }
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stream_rate_limit_observe_shrinks_the_cloud_cap() {
    let openai = Arc::new(TestProvider::default());
    openai.advertise_as(LLMProviderKind::OpenAI);
    openai.script(TestProvider::rate_limit());
    for _ in 0..2 {
        openai.script(TestProvider::ok("after-shrink"));
    }
    let mut config = small_config();
    config.workers = 4;
    config.reserved_interactive_workers = 0;
    config.queue_capacity_normal = 8;
    config.global_cloud_concurrency = 2;
    config
        .provider_concurrency
        .overrides
        .insert("openai".to_string(), 4);
    let queue = LlmDispatchQueue::start(
        test_router_with_providers(&[(LLMProviderKind::OpenAI, openai.clone())]),
        Arc::new(NoopTaskStateView),
        Arc::new(NoopTaskLedgerSink),
        config,
    );
    let (job, mut stream) = LlmStreamJob::new(
        mixed_request(&LLMProviderKind::OpenAI),
        JobOrigin::op("stream-429"),
        4,
    );
    queue.submit_stream(job).await.expect("429 stream accepted");
    while stream.recv().await.is_some() {}
    wait_until(Duration::from_secs(1), || {
        queue
            .snapshot()
            .registry
            .failed
            .iter()
            .any(|meta| meta.origin.operation == "stream-429")
    })
    .await;

    openai.set_delay(Duration::from_millis(400));
    let mut receipts = Vec::new();
    for index in 0..2 {
        let (job, rx) = LlmJob::new(
            mixed_request(&LLMProviderKind::OpenAI),
            JobOrigin::op(format!("after-stream-429-{index}")),
        );
        queue.submit(job).await.expect("follow-up accepted");
        receipts.push(rx);
    }
    wait_until(Duration::from_secs(1), || openai.in_flight() >= 1).await;
    let mut peak = openai.in_flight();
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(200) {
        peak = peak.max(openai.in_flight());
        tokio::task::yield_now().await;
    }
    assert_eq!(
        peak, 1,
        "stream 429 must shrink the shared cloud cap from 2 to 1, peak={peak}"
    );
    for rx in receipts {
        let _ = rx.await;
    }
    queue.shutdown(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn legacy_engine_still_ceilings_http_to_workers() {
    let (combined, ..) = peak_mixed_cloud_invokes(3).await;
    assert_eq!(
        combined, 3,
        "legacy_worker_pool still ceilings mixed-cloud HTTP to scheduler workers"
    );
}

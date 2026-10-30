//! Provider-free tests of the generic resource carrier at real dispatch seams.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Notify;

use crate::dispatch::local_prep::{local_prep_needs_ollama, maybe_local_prep_direct};
use crate::dispatch::test_support::MockTaskStateView;
use crate::types::*;
use crate::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    Reserve,
    Dispatch,
    Start,
}

#[derive(Debug, Default)]
struct Record {
    plans: Vec<LlmPhysicalAttemptPlan>,
    phases: Vec<&'static str>,
    observations: Vec<LlmPhysicalAttemptObservation>,
}

#[derive(Debug)]
struct Authority {
    record: Arc<Mutex<Record>>,
    fault: Fault,
    profile: String,
    model: String,
    cohort: String,
    task: String,
    root: String,
    execution: String,
}

impl Authority {
    fn new(profile: &LLMProfile) -> Self {
        Self {
            record: Arc::new(Mutex::new(Record::default())),
            fault: Fault::None,
            profile: "bounded".into(),
            model: profile.model.clone(),
            cohort: transport_cohort_fingerprint(
                &profile.provider,
                &profile.model,
                profile.api_base_url.as_deref(),
                profile.metadata.as_ref(),
            ),
            task: "task-bounded".into(),
            root: "root-bounded".into(),
            execution: "exec-bounded".into(),
        }
    }
}

#[async_trait]
impl LlmPhysicalResourceAuthorizer for Authority {
    async fn reserve(
        &self,
        plan: LlmPhysicalAttemptPlan,
    ) -> Result<Box<dyn LlmPhysicalAttemptPermit>, String> {
        {
            let mut record = self.record.lock().unwrap();
            record.phases.push("reserve");
            record.plans.push(plan.clone());
        }
        if self.fault == Fault::Reserve
            || plan.profile() != self.profile
            || plan.model() != self.model
            || plan.transport_cohort() != self.cohort
            || plan.task_id() != self.task
            || plan.root_execution_id() != self.root
            || plan.execution_id() != self.execution
        {
            return Err("unadmitted physical route or identity".into());
        }
        Ok(Box::new(Permit {
            record: self.record.clone(),
            fault: self.fault,
        }))
    }
}

#[derive(Debug)]
struct Permit {
    record: Arc<Mutex<Record>>,
    fault: Fault,
}

#[async_trait]
impl LlmPhysicalAttemptPermit for Permit {
    async fn mark_dispatched(&mut self) -> Result<(), String> {
        self.record.lock().unwrap().phases.push("dispatch");
        if self.fault == Fault::Dispatch {
            Err("dispatch denied".into())
        } else {
            Ok(())
        }
    }

    fn mark_physical_started(&mut self) -> Result<LlmPhysicalAttemptStart, String> {
        self.record.lock().unwrap().phases.push("start");
        if self.fault == Fault::Start {
            return Err("expired physical window".into());
        }
        LlmPhysicalAttemptStart::from_resource_permit(chrono::Utc::now().timestamp_millis(), 5_000)
    }

    async fn release_pre_io(self: Box<Self>, _: i64) -> Result<(), String> {
        self.record.lock().unwrap().phases.push("release");
        Ok(())
    }

    async fn settle(
        self: Box<Self>,
        observation: LlmPhysicalAttemptObservation,
    ) -> Result<(), String> {
        let mut record = self.record.lock().unwrap();
        record.phases.push("settle");
        record.observations.push(observation);
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Reply {
    Usage,
    MissingUsage,
    RateLimited,
    Status(u16),
    Timeout,
}

struct Provider {
    requests: Arc<Mutex<Vec<LLMRequest>>>,
    reply: Reply,
    started: Notify,
    release: Option<Arc<Notify>>,
}

#[async_trait]
impl LLMProvider for Provider {
    fn provider_kind(&self) -> LLMProviderKind {
        LLMProviderKind::OpenAI
    }

    fn capabilities(&self, _: &str) -> LLMCapability {
        LLMCapability::default()
    }

    async fn invoke(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
        self.requests.lock().unwrap().push(request);
        self.started.notify_one();
        if let Some(release) = &self.release {
            release.notified().await;
        }
        if matches!(self.reply, Reply::RateLimited) {
            return Err(LLMError::RateLimited {
                retry_after: Some(Duration::from_millis(1)),
            });
        }
        if let Reply::Status(status) = self.reply {
            return Err(LLMError::ProviderStatus {
                provider: "openai".into(),
                status,
                body: "rejected".into(),
            });
        }
        if matches!(self.reply, Reply::Timeout) {
            return Err(LLMError::Timeout);
        }
        Ok(LLMResponse {
            text: Some(Arc::from("bounded result")),
            usage: matches!(self.reply, Reply::Usage).then_some(TokenUsage {
                prompt_tokens: Some(40),
                completion_tokens: Some(5),
                total_tokens: Some(45),
                ..Default::default()
            }),
            ..Default::default()
        })
    }
}

fn profile() -> LLMProfile {
    serde_json::from_value(json!({
        "provider": "openai", "model": "gpt-6.1-sol",
        "api_key_env": "__RESOURCE_GUARD_TEST_KEY__",
        "max_output_tokens": 128, "context_window_tokens": 4096,
        "timeout_secs": 5
    }))
    .unwrap()
}

fn request() -> LLMRequest {
    let mut request = LLMRequest {
        model: "placeholder".into(),
        messages: vec![LLMMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::text("perform bounded work")],
        }]
        .into(),
        ..Default::default()
    };
    request.metadata.operation = "bounded-test".into();
    let mut trace = LlmTraceContext::new(
        LlmScope::new("test", "bounded"),
        LlmWorkloadClass::AutonomousTask,
    );
    trace.task_id = Some("task-bounded".into());
    trace.root_execution_id = Some("root-bounded".into());
    trace.execution_id = Some("exec-bounded".into());
    request.metadata.set_trace_context(trace);
    request
}

fn router(profile: LLMProfile, reply: Reply) -> (MultiLLMRouter, Arc<Provider>) {
    let mut config = LLMRouterConfig::default();
    config.default_profile = "bounded".into();
    let mut fallback = profile.clone();
    if let Some(metadata) = fallback.metadata.as_mut() {
        metadata.remove("fallback_profile");
    }
    config.profiles.insert("fallback-bounded".into(), fallback);
    config.profiles.insert("bounded".into(), profile);
    let provider = Arc::new(Provider {
        requests: Arc::new(Mutex::new(Vec::new())),
        reply,
        started: Notify::new(),
        release: None,
    });
    let mut router = MultiLLMRouter::new(config).unwrap();
    router.register_provider(provider.clone());
    (router, provider)
}

#[tokio::test]
async fn resource_guard_settles_one_exact_physical_attempt() {
    let profile = profile();
    let authority = Arc::new(Authority::new(&profile));
    let (router, provider) = router(profile, Reply::Usage);
    with_physical_resource_guard(
        Some(LlmPhysicalResourceGuard::new(authority.clone())),
        async {
            router.route(request()).await.unwrap();
        },
    )
    .await;
    let record = authority.record.lock().unwrap();
    assert_eq!(record.phases, ["reserve", "dispatch", "start", "settle"]);
    assert_eq!(record.plans.len(), 1);
    assert_eq!(record.plans[0].attempt_index(), 1);
    assert_eq!(record.plans[0].input_token_upper(), 4096);
    assert_eq!(record.plans[0].output_token_upper(), 128);
    let observation = &record.observations[0];
    assert_eq!(observation.outcome(), LlmPhysicalAttemptOutcome::Committed);
    assert_eq!(
        (observation.input_tokens(), observation.output_tokens()),
        (40, 5)
    );
    assert!(observation.cost_microusd() > 0);
    assert!(observation.cost_microusd() <= record.plans[0].cost_upper_microusd());
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].metadata.single_physical_attempt);
    assert!(requests[0].context_reuse.is_none());
    assert_eq!(requests[0].prompt_cache, Some(PromptCacheConfig::Disabled));
}

#[tokio::test]
async fn resource_guard_missing_usage_and_rate_limits_remain_uncertain() {
    for reply in [Reply::MissingUsage, Reply::RateLimited] {
        let profile = profile();
        let authority = Arc::new(Authority::new(&profile));
        let (router, provider) = router(profile, reply);
        let mut request = request();
        request
            .metadata
            .set_physical_resource_guard(LlmPhysicalResourceGuard::new(authority.clone()));
        assert!(router.route(request).await.is_err());
        let record = authority.record.lock().unwrap();
        assert_eq!(record.observations.len(), 1);
        assert_eq!(
            record.observations[0].outcome(),
            LlmPhysicalAttemptOutcome::OutcomeUncertain
        );
        assert!(!record.phases.contains(&"release"));
        assert_eq!(provider.requests.lock().unwrap().len(), 1);
    }
}

/// A provider that answered with a definitive client rejection generated
/// nothing billable. The attempt still settles `OutcomeUncertain` for every
/// generic consumer, but carries the typed rejection so a resource owner may
/// release the reservation. Timeouts, 408, 429 and 5xx stay plain uncertain.
#[tokio::test]
async fn resource_guard_marks_only_definitive_client_rejections() {
    for (reply, definitive) in [
        (Reply::Status(400), true),
        (Reply::Status(401), true),
        (Reply::Status(403), true),
        (Reply::Status(422), true),
        (Reply::Status(408), false),
        (Reply::Status(429), false),
        (Reply::Status(500), false),
        (Reply::Status(503), false),
        (Reply::Timeout, false),
        (Reply::RateLimited, false),
        (Reply::MissingUsage, false),
    ] {
        let profile = profile();
        let authority = Arc::new(Authority::new(&profile));
        let (router, _provider) = router(profile, reply);
        let mut request = request();
        request
            .metadata
            .set_physical_resource_guard(LlmPhysicalResourceGuard::new(authority.clone()));
        assert!(router.route(request).await.is_err());
        let record = authority.record.lock().unwrap();
        assert!(!record.observations.is_empty());
        for observation in &record.observations {
            assert_eq!(
                observation.outcome(),
                LlmPhysicalAttemptOutcome::OutcomeUncertain
            );
            assert_eq!(observation.is_definitive_rejection(), definitive);
            assert_eq!(observation.input_tokens(), 0);
            assert_eq!(observation.output_tokens(), 0);
            assert_eq!(observation.cost_microusd(), 0);
        }
        assert!(!record.phases.contains(&"release"));
    }
}

#[tokio::test]
async fn resource_guard_pre_io_failure_never_invokes_provider() {
    for fault in [Fault::Reserve, Fault::Dispatch, Fault::Start] {
        let profile = profile();
        let mut authority = Authority::new(&profile);
        authority.fault = fault;
        let authority = Arc::new(authority);
        let (router, provider) = router(profile, Reply::Usage);
        let mut request = request();
        request
            .metadata
            .set_physical_resource_guard(LlmPhysicalResourceGuard::new(authority.clone()));
        assert!(router.route(request).await.is_err());
        assert!(provider.requests.lock().unwrap().is_empty());
        let record = authority.record.lock().unwrap();
        assert!(record.observations.is_empty());
        assert_eq!(record.phases.contains(&"release"), fault != Fault::Reserve);
    }
}

#[tokio::test]
async fn resource_guard_checks_resolved_route_and_all_execution_identities() {
    for drift in ["profile", "model", "cohort", "task", "root", "execution"] {
        let profile = profile();
        let mut authority = Authority::new(&profile);
        match drift {
            "profile" => authority.profile = "different".into(),
            "model" => authority.model = "different".into(),
            "cohort" => authority.cohort = "0".repeat(64),
            "task" => authority.task = "different".into(),
            "root" => authority.root = "different".into(),
            "execution" => authority.execution = "different".into(),
            _ => unreachable!(),
        }
        let authority = Arc::new(authority);
        let (router, provider) = router(profile, Reply::Usage);
        let mut request = request();
        request
            .metadata
            .set_physical_resource_guard(LlmPhysicalResourceGuard::new(authority.clone()));
        assert!(router.route(request).await.is_err(), "drift: {drift}");
        assert!(
            provider.requests.lock().unwrap().is_empty(),
            "drift: {drift}"
        );
        assert_eq!(authority.record.lock().unwrap().plans.len(), 1);
    }
}

#[tokio::test]
async fn resource_guard_refuses_unqualified_fallback_stream_search_and_continuation() {
    for mode in ["fallback", "stream", "search", "continuation"] {
        let mut profile = profile();
        let authority = Arc::new(Authority::new(&profile));
        if mode == "fallback" {
            profile.metadata =
                Some([(String::from("fallback_profile"), json!("fallback-bounded"))].into());
        }
        let (router, provider) = router(profile, Reply::Usage);
        let mut request = request();
        request
            .metadata
            .set_physical_resource_guard(LlmPhysicalResourceGuard::new(authority.clone()));
        match mode {
            "stream" => request.stream = true,
            "search" => request.set_extra(json!({"server_web_search": true})),
            "continuation" => {
                request.context_reuse = Some(Arc::new(ContextReuseConfig::new(
                    ContextReuseStrategy::ServerContinuation,
                )))
            },
            _ => {},
        }
        assert!(router.route(request).await.is_err(), "mode: {mode}");
        assert!(provider.requests.lock().unwrap().is_empty());
        assert!(authority.record.lock().unwrap().plans.is_empty());
    }
}

#[tokio::test]
async fn resource_guard_stream_and_embedding_entrypoints_refuse_before_io() {
    let profile = profile();
    let authority = Arc::new(Authority::new(&profile));
    let (router, provider) = router(profile, Reply::Usage);
    with_physical_resource_guard(
        Some(LlmPhysicalResourceGuard::new(authority.clone())),
        async {
            let (tx, _) = tokio::sync::mpsc::channel(1);
            assert!(router.route_stream(request(), tx).await.is_err());
            let embedding = EmbeddingRequest {
                model: "placeholder".into(),
                inputs: vec!["bounded input".into()],
                truncate: false,
                keep_alive: None,
                options: None,
                timeout_ms: None,
                metadata: RequestMetadata::default(),
            };
            let error = router
                .embed_for_operation("bounded-test", embedding)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("resource-bound embeddings"));
        },
    )
    .await;
    assert!(provider.requests.lock().unwrap().is_empty());
    assert!(authority.record.lock().unwrap().plans.is_empty());
}

#[tokio::test]
async fn resource_guard_nested_scope_cannot_replace_or_drop_authority() {
    let profile = profile();
    let authority = Arc::new(Authority::new(&profile));
    let other = Arc::new(Authority::new(&profile));
    let guard = LlmPhysicalResourceGuard::new(authority.clone());
    let (router, provider) = router(profile, Reply::Usage);
    with_physical_resource_guard(Some(guard.clone()), async {
        with_physical_resource_guard(None, async {
            with_physical_resource_guard(Some(guard.clone()), async {
                router.route(request()).await.unwrap();
            })
            .await;
        })
        .await;
        with_physical_resource_guard(Some(LlmPhysicalResourceGuard::new(other.clone())), async {
            assert!(router.route(request()).await.is_err());
        })
        .await;
        let mut conflicting = request();
        conflicting
            .metadata
            .set_physical_resource_guard(LlmPhysicalResourceGuard::new(other.clone()));
        assert!(router.route(conflicting).await.is_err());
    })
    .await;
    assert!(current_physical_resource_guard().is_none());
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    assert_eq!(authority.record.lock().unwrap().observations.len(), 1);
    assert!(other.record.lock().unwrap().plans.is_empty());
}

#[test]
fn resource_guard_serialization_cannot_mint_or_restore_authority() {
    let mut metadata = RequestMetadata::default();
    metadata.set_physical_resource_guard(LlmPhysicalResourceGuard::new(Arc::new(Authority::new(
        &profile(),
    ))));
    let encoded = serde_json::to_value(&metadata).unwrap();
    assert!(encoded.get("physical_resource_guard").is_none());
    assert!(encoded.get("single_physical_attempt").is_none());
    let restored: RequestMetadata = serde_json::from_value(json!({
        "physical_resource_guard": {"authorizer": "caller-data"}, "single_physical_attempt": true
    }))
    .unwrap();
    assert!(!restored.has_physical_resource_authority());
    assert!(!restored.single_physical_attempt);
}

#[derive(Debug)]
struct Disclosure;

#[async_trait]
impl LlmDisclosureAuthorizer for Disclosure {
    async fn revalidate(
        &self,
        _: &str,
        _: &LLMProviderKind,
        _: &str,
        _: Option<&str>,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[tokio::test]
async fn resource_guard_app_disclosure_preserves_fence_and_denies_dual_metering() {
    let profile = profile();
    let authority = Arc::new(Authority::new(&profile));
    let guard = LlmPhysicalResourceGuard::new(authority.clone());
    let disclosure = LlmDisclosureGuard::new(
        "bounded",
        authority.cohort.clone(),
        "partition",
        "policy",
        LlmDisclosureCapturePolicy::MetadataOnly,
        Arc::new(Disclosure),
    )
    .unwrap();
    let mut request = request();
    request.metadata.set_physical_resource_guard(guard);
    request.metadata.set_disclosure_guard(disclosure);
    assert!(request.metadata.single_physical_attempt);
    assert!(request
        .metadata
        .validate_physical_resource_authority()
        .is_ok());
    let dual = LlmDisclosureGuard::new_with_physical_resource_authorizer(
        "bounded",
        authority.cohort.clone(),
        "partition",
        "policy",
        LlmDisclosureCapturePolicy::MetadataOnly,
        Arc::new(Disclosure),
        authority.clone(),
    )
    .unwrap();
    request.metadata.set_disclosure_guard(dual);
    let (router, provider) = router(profile, Reply::Usage);
    assert!(router.route(request).await.is_err());
    assert!(provider.requests.lock().unwrap().is_empty());
    assert!(authority.record.lock().unwrap().plans.is_empty());
}

#[tokio::test]
async fn resource_guard_local_prep_inherits_ambient_authority_and_keeps_raw_content() {
    let authority = Arc::new(Authority::new(&profile()));
    let mut request = request();
    request.summarisable_blocks = vec![SummarisableBlock {
        message_index: 0,
        content_index: 0,
        raw: "raw bounded content".into(),
        purpose: SummarisationPurpose::Other,
        max_chars: None,
    }]
    .into();
    let config = LocalPrepConfig {
        enabled: true,
        yield_worker: true,
        threshold_chars: 0,
        base_url: "http://127.0.0.1:1".into(),
        ..Default::default()
    };
    with_physical_resource_guard(Some(LlmPhysicalResourceGuard::new(authority.clone())), async {
        assert!(!local_prep_needs_ollama(&request, &config));
        let stat = maybe_local_prep_direct(&mut request, &config).await.unwrap();
        assert_eq!(stat.blocks_processed, 0);
        assert!(stat.calls.is_empty());
        assert!(request.metadata.has_physical_resource_authority());
        assert!(matches!(&request.messages[0].content[0], ContentBlock::Text {text} if text == "raw bounded content"));
    }).await;
    assert!(authority.record.lock().unwrap().plans.is_empty());
}

fn queue(router: MultiLLMRouter, state: Arc<dyn TaskStateView>) -> Arc<LlmDispatchQueue> {
    LlmDispatchQueue::start(
        Arc::new(router),
        state,
        Arc::new(NoopTaskLedgerSink),
        DispatchConfig {
            workers: 1,
            local_prep: LocalPrepConfig {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        },
    )
}

#[tokio::test]
async fn resource_guard_queue_hop_carries_authority_and_refuses_response_reuse() {
    let profile = profile();
    let authority = Arc::new(Authority::new(&profile));
    let (router, provider) = router(profile, Reply::Usage);
    let queue = queue(router, Arc::new(NoopTaskStateView));
    with_physical_resource_guard(
        Some(LlmPhysicalResourceGuard::new(authority.clone())),
        async {
            queue
                .submit_and_wait(request(), JobOrigin::op("bounded-test"))
                .await
                .unwrap();
            let (job, _) = LlmJob::new(request(), JobOrigin::op("bounded-test"));
            assert!(queue
                .submit(job.with_idempotency_key("same-response"))
                .await
                .is_err());
        },
    )
    .await;
    queue.shutdown(Duration::from_secs(2)).await;
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    assert_eq!(authority.record.lock().unwrap().observations.len(), 1);
}

#[tokio::test]
async fn resource_guard_cancel_drains_the_physical_permit_before_returning() {
    let profile = profile();
    let authority = Arc::new(Authority::new(&profile));
    let (mut router, _) = router(profile, Reply::Usage);
    let release = Arc::new(Notify::new());
    let provider = Arc::new(Provider {
        requests: Arc::new(Mutex::new(Vec::new())),
        reply: Reply::Usage,
        started: Notify::new(),
        release: Some(release.clone()),
    });
    router.register_provider(provider.clone());
    let state = Arc::new(MockTaskStateView::new());
    state.set_active("task-bounded");
    let queue = queue(router, state.clone());
    let (job, mut rx) = LlmJob::new(request(), JobOrigin::op("bounded-test"));
    let job = job.with_task(TaskRef::task("task-bounded"));
    with_physical_resource_guard(
        Some(LlmPhysicalResourceGuard::new(authority.clone())),
        async {
            queue.submit(job).await.unwrap();
        },
    )
    .await;
    tokio::time::timeout(Duration::from_secs(2), provider.started.notified())
        .await
        .unwrap();
    state.fire_cancel("task-bounded");
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    assert!(authority.record.lock().unwrap().observations.is_empty());
    release.notify_one();
    assert!(tokio::time::timeout(Duration::from_secs(2), rx)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    queue.shutdown(Duration::from_secs(2)).await;
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    let record = authority.record.lock().unwrap();
    assert_eq!(record.observations.len(), 1);
    assert_eq!(
        record.observations[0].outcome(),
        LlmPhysicalAttemptOutcome::Committed
    );
}

#[tokio::test]
async fn resource_guard_queue_rate_limit_does_not_retry_an_uncertain_attempt() {
    let profile = profile();
    let authority = Arc::new(Authority::new(&profile));
    let (router, provider) = router(profile, Reply::RateLimited);
    let queue = queue(router, Arc::new(NoopTaskStateView));
    let result = with_physical_resource_guard(
        Some(LlmPhysicalResourceGuard::new(authority.clone())),
        queue.submit_and_wait(request(), JobOrigin::op("bounded-test")),
    )
    .await;
    assert!(result.is_err());
    queue.shutdown(Duration::from_secs(2)).await;
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    let record = authority.record.lock().unwrap();
    assert_eq!(record.observations.len(), 1);
    assert_eq!(
        record.observations[0].outcome(),
        LlmPhysicalAttemptOutcome::OutcomeUncertain
    );
}

use super::*;
use decision_engine_contract::{
    batch::{BatchResponse, DecisionItemResult, ItemStatus},
    *,
};
use std::{path::PathBuf, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
};

static TEST_LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));
struct Socket(PathBuf);
impl Drop for Socket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        decision_host::configure(&crate::config::DecisionHostConfig {
            mode: crate::config::DecisionMode::Off,
            ..Default::default()
        });
    }
}
async fn fixture(delay: Duration, receipt: bool) -> (Participation, Socket, Input) {
    fixture_with_answer(delay, receipt, false).await
}
async fn fixture_with_answer(
    delay: Duration,
    receipt: bool,
    eligible: bool,
) -> (Participation, Socket, Input) {
    fixture_with_limits(delay, receipt, eligible, None, None).await
}

async fn fixture_with_limits(
    delay: Duration,
    receipt: bool,
    eligible: bool,
    decision_budget_ms: Option<u64>,
    queue_budget_ms: Option<u64>,
) -> (Participation, Socket, Input) {
    fixture_with_limits_and_notify(
        delay,
        receipt,
        eligible,
        decision_budget_ms,
        queue_budget_ms,
        None,
    )
    .await
}

async fn fixture_with_limits_and_notify(
    delay: Duration,
    receipt: bool,
    eligible: bool,
    decision_budget_ms: Option<u64>,
    queue_budget_ms: Option<u64>,
    dispatched: Option<Arc<tokio::sync::Notify>>,
) -> (Participation, Socket, Input) {
    let socket = Socket(PathBuf::from(format!(
        "/tmp/mem-dec-{}.sock",
        ulid::Ulid::new()
    )));
    let listener = UnixListener::bind(&socket.0).unwrap();
    let mut operations = OperationsResponse {
        contract_version: CONTRACT_VERSION,
        action_contract_version: Some(CONTRACT_VERSION),
        engine_instance: "test-boot".into(),
        policy_revision: "policy".into(),
        operations: vec![OperationPolicy {
            name: "memory_test".into(),
            shadow: true,
            gate: eligible,
            max_consecutive_steps: 3,
            sees_body: true,
            route_local: vec![],
            route_cloud: vec!["jev".into()],
            classification: Default::default(),
        }],
    };
    if let Some(value) = decision_budget_ms {
        operations.operations[0]
            .classification
            .limits
            .decision_budget_ms = value;
    }
    if let Some(value) = queue_budget_ms {
        operations.operations[0]
            .classification
            .limits
            .queue_budget_ms = value;
    }
    let served_operations = operations.clone();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = Vec::new();
        let request: DecideRequest = loop {
            let mut chunk = [0u8; 4096];
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            buffer.extend_from_slice(&chunk[..n]);
            if let Some(split) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&buffer[..split]).to_lowercase();
                let length: usize = header
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:").map(str::trim))
                    .unwrap()
                    .parse()
                    .unwrap();
                if buffer.len() >= split + 4 + length {
                    break serde_json::from_slice(&buffer[split + 4..split + 4 + length]).unwrap();
                }
            }
        };
        if let Some(dispatched) = dispatched {
            dispatched.notify_one();
        }
        tokio::time::sleep(delay).await;
        let mut calls = vec![];
        if receipt {
            let mut call = crate::magician_v2::analytics::decision_model_telemetry::fixture_call();
            call.batch_id = Some(request.batch.request_id.clone());
            call.item_ids = vec!["0".into()];
            calls.push(call);
        }
        let other_items = request
            .batch
            .items
            .iter()
            .skip(1)
            .map(|item| DecisionItemResult {
                item_id: item.item_id.clone(),
                status: ItemStatus::NoFittingModel,
                response: None,
                thresholds: None,
                eligible_answers: Default::default(),
                error: None,
                latency_ms: 0,
            })
            .collect::<Vec<_>>();
        let mut reply = DecideResponse {
            batch: BatchResponse {
                request_id: request.batch.request_id,
                engine_instance: "test-boot".into(),
                policy_revision: "policy".into(),
                items: vec![DecisionItemResult {
                    item_id: "0".into(),
                    status: if eligible { ItemStatus::Answered } else { ItemStatus::NoFittingModel },
                    response: eligible.then(|| serde_json::from_value(serde_json::json!({
                        "model":{"adapter":"typesafe","model":"jev-1.13.0"},"pack_id":"fixture","pack_version":"1.0.0",
                        "answers":{"applicable":{"type":"noul","noul":0.99}},"usage":{"input_tokens":1,"output_tokens":0}})).unwrap()),
                    thresholds: eligible.then(|| BTreeMap::from([("applicable".into(),0.9)])),
                    eligible_answers: if eligible { BTreeMap::from([("applicable".into(),"fixture-qualified".into())]) } else { Default::default() },
                    error: None,
                    latency_ms: delay.as_millis() as u64,
                }],
                model_health: Default::default(),
            },
            model_calls: calls,
            contract_version: CONTRACT_VERSION,
            status: if eligible { DecideStatus::Answered } else { DecideStatus::NoFittingModel },
            response: None,
            thresholds: None,
            error: None,
            latency_ms: delay.as_millis() as u64,
        };
        reply.batch.items.extend(other_items);
        let body = serde_json::to_vec(&reply).unwrap();
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(header.as_bytes()).await;
        let _ = stream.write_all(&body).await;
        drop(stream);
        // A slow answer can cross the policy TTL. Serve real discovery refresh
        // so the runner must check the same engine/revision before applying it.
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut request = [0u8; 4096];
            let n = stream.read(&mut request).await.unwrap();
            assert!(String::from_utf8_lossy(&request[..n]).starts_with("GET /v1/operations"));
            let body = serde_json::to_vec(&served_operations).unwrap();
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes()).await;
            let _ = stream.write_all(&body).await;
        }
    });
    let participation = decision_host::classification::test_participation(&socket.0, operations);
    let input = Input {
        operation: "memory_test".into(),
        projection_version: "p1".into(),
        reference_version: "r1".into(),
        case_id: "case".into(),
        context: None,
        items: vec![DecisionItem {
            item_id: "0".into(),
            state: DecisionState::from_text("bounded"),
            choice_candidates: Default::default(),
        }],
        required_questions: vec!["applicable".into()],
        scope: LlmTraceContext::new(
            magicllm::LlmScope::new("test", "test"),
            magicllm::LlmWorkloadClass::Ambient,
        ),
        agent: None,
        requires_completion: false,
        replay: None,
    };
    (participation, socket, input)
}

#[tokio::test]
async fn memory_decision_saved_policy_refreshes_without_retiring_current_authority() {
    let _serial = TEST_LOCK.lock().await;
    use decision_host::classification::{policy, policy_for_reconciliation, test_expire_policy};
    let socket = Socket(PathBuf::from(format!(
        "/tmp/mem-policy-{}.sock",
        ulid::Ulid::new()
    )));
    let listener = UnixListener::bind(&socket.0).unwrap();
    let operations = OperationsResponse {
        contract_version: CONTRACT_VERSION,
        action_contract_version: Some(CONTRACT_VERSION),
        engine_instance: "saved-boot".into(),
        policy_revision: "saved-policy".into(),
        operations: vec![OperationPolicy {
            name: "memory_test".into(),
            shadow: false,
            gate: true,
            max_consecutive_steps: 3,
            sees_body: true,
            route_local: vec![],
            route_cloud: vec!["jev".into()],
            classification: Default::default(),
        }],
    };
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 4096];
            let n = stream.read(&mut buffer).await.unwrap();
            assert!(String::from_utf8_lossy(&buffer[..n]).starts_with("GET /v1/operations"));
            tokio::time::sleep(Duration::from_millis(15)).await;
            let body = serde_json::to_vec(&operations).unwrap();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            stream.write_all(&body).await.unwrap();
        }
    });
    decision_host::configure(&crate::config::DecisionHostConfig {
        mode: crate::config::DecisionMode::AllEngines,
        socket: Some(socket.0.display().to_string()),
        ..Default::default()
    });
    let first = policy_for_reconciliation("memory_test", "test", "test").await;
    let PolicyLookup::Participating(first) = first else {
        panic!("cold audit must await discovery")
    };
    assert!(first.discovery_latency_ms >= 15);
    test_expire_policy();
    assert!(
        matches!(
            policy("memory_test", "test", "test"),
            PolicyLookup::Unavailable(_)
        ),
        "new inference still falls back immediately on expiry"
    );
    let second = policy_for_reconciliation("memory_test", "test", "test").await;
    let PolicyLookup::Participating(second) = second else {
        panic!("saved audit must await refresh")
    };
    assert_eq!(first.engine_instance, second.engine_instance);
    assert_eq!(first.revision, second.revision);
    server.await.unwrap();
    test_expire_policy();
    assert!(
        matches!(
            policy_for_reconciliation("memory_test", "test", "test").await,
            PolicyLookup::Unavailable(_)
        ),
        "outage cannot impersonate disablement"
    );
    decision_host::configure(&crate::config::DecisionHostConfig {
        mode: crate::config::DecisionMode::Off,
        ..Default::default()
    });
    assert!(matches!(
        policy_for_reconciliation("memory_test", "test", "test").await,
        PolicyLookup::Disabled
    ));
}

#[tokio::test]
async fn memory_decision_shadow_never_discards_or_delays_fast_incumbent() {
    let _serial = TEST_LOCK.lock().await;
    let (policy, _socket, input) = fixture(Duration::from_secs(5), false).await;
    let start = Instant::now();
    let result = run(
        input,
        PolicyLookup::Participating(policy),
        Duration::from_secs(3),
        Duration::from_secs(2),
        |_, _| async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Some("incumbent output")
        },
        |_| Reference::from(crate::magician_v2::decisions::telemetry::Labels::new()),
    )
    .await;
    assert_eq!(result.incumbent, Some("incumbent output"));
    assert!(start.elapsed() < Duration::from_millis(500));
    assert!(result.answers.is_empty());
}

#[tokio::test]
async fn memory_decision_off_invalidates_captured_authority_immediately() {
    let _serial = TEST_LOCK.lock().await;
    let (policy, _socket, _) = fixture(Duration::ZERO, false).await;
    assert!(policy.is_current());
    decision_host::configure(&crate::config::DecisionHostConfig {
        mode: crate::config::DecisionMode::Off,
        ..Default::default()
    });
    assert!(!policy.is_current());
    assert!(matches!(
        decision_host::classification::policy("memory_test", "test", "test"),
        PolicyLookup::Disabled
    ));
}

#[tokio::test]
async fn memory_decision_inflight_authority_revoked_before_answer_cannot_apply() {
    let _serial = TEST_LOCK.lock().await;
    let dispatched = Arc::new(tokio::sync::Notify::new());
    let (policy, _socket, input) = fixture_with_limits_and_notify(
        Duration::from_millis(100),
        false,
        true,
        None,
        None,
        Some(dispatched.clone()),
    )
    .await;
    let result = tokio::spawn(async move {
        run(
            input,
            PolicyLookup::Participating(policy),
            Duration::from_secs(2),
            Duration::ZERO,
            |_, _| async { None::<()> },
            |_| Reference::from(crate::magician_v2::decisions::telemetry::Labels::new()),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(1), dispatched.notified())
        .await
        .expect("fixture decision was not dispatched");
    decision_host::configure(&crate::config::DecisionHostConfig {
        mode: crate::config::DecisionMode::Off,
        ..Default::default()
    });
    let outcome = result.await.unwrap();
    assert!(outcome.answers.is_empty());
    assert!(outcome.origins.is_empty());
}

#[tokio::test]
async fn memory_decision_revalidation_observes_reload_inside_cache_ttl() {
    let _serial = TEST_LOCK.lock().await;
    let socket = Socket(PathBuf::from(format!(
        "/tmp/mem-policy-reload-{}.sock",
        ulid::Ulid::new()
    )));
    let listener = UnixListener::bind(&socket.0).unwrap();
    let mut operations = OperationsResponse {
        contract_version: CONTRACT_VERSION,
        action_contract_version: Some(CONTRACT_VERSION),
        engine_instance: "boot".into(),
        policy_revision: "before".into(),
        operations: vec![OperationPolicy {
            name: "memory_test".into(),
            shadow: false,
            gate: true,
            max_consecutive_steps: 3,
            sees_body: true,
            route_local: vec![],
            route_cloud: vec!["jev".into()],
            classification: Default::default(),
        }],
    };
    let participation =
        decision_host::classification::test_participation(&socket.0, operations.clone());
    assert!(participation.is_current(), "the lookup cache is still warm");
    operations.policy_revision = "after".into();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let n = stream.read(&mut request).await.unwrap();
        assert!(String::from_utf8_lossy(&request[..n]).starts_with("GET /v1/operations"));
        let body = serde_json::to_vec(&operations).unwrap();
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes()).await.unwrap();
        stream.write_all(&body).await.unwrap();
    });
    assert!(!participation.revalidate().await);
    server.await.unwrap();
    assert!(!participation.is_current());
}

#[tokio::test]
async fn memory_decision_revalidation_outage_revokes_cached_authority() {
    let _serial = TEST_LOCK.lock().await;
    let socket = Socket(PathBuf::from(format!(
        "/tmp/mem-policy-outage-{}.sock",
        ulid::Ulid::new()
    )));
    let listener = UnixListener::bind(&socket.0).unwrap();
    let operations = OperationsResponse {
        contract_version: CONTRACT_VERSION,
        action_contract_version: Some(CONTRACT_VERSION),
        engine_instance: "boot".into(),
        policy_revision: "before".into(),
        operations: vec![OperationPolicy {
            name: "memory_test".into(),
            shadow: false,
            gate: true,
            max_consecutive_steps: 3,
            sees_body: true,
            route_local: vec![],
            route_cloud: vec!["jev".into()],
            classification: Default::default(),
        }],
    };
    let participation = decision_host::classification::test_participation(&socket.0, operations);
    assert!(participation.is_current());
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        drop(stream);
    });
    assert!(!participation.revalidate().await);
    server.await.unwrap();
    assert!(!participation.is_current());
}

#[tokio::test]
async fn memory_decision_foreground_cancellation_keeps_receipt_accounting() {
    let _serial = TEST_LOCK.lock().await;
    let (policy, _socket, input) = fixture(Duration::from_millis(100), true).await;
    let bus = Arc::new(crate::magician_v2::realtime_events::RuntimeTransportBroadcaster::new(64));
    decision_host::set_health_broadcaster(&bus);
    let mut events = bus.subscribe();
    let req = request(&input, ClassificationMode::Shadow);
    let foreground = tokio::spawn(async move {
        policy
            .decide(req, input.scope, None, Duration::from_secs(1), None)
            .await
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    foreground.abort();
    let observed = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let crate::magician_v2::realtime_events::RuntimeTransportEvent::LLMResponseReceived { .. } = events.recv().await.unwrap() { return true; }
        }
    }).await.unwrap();
    assert!(observed);
}

#[tokio::test]
async fn memory_decision_pure_gate_uses_budget_previously_reserved_for_llm() {
    let _serial = TEST_LOCK.lock().await;
    let (policy, _socket, mut input) =
        fixture_with_limits(Duration::from_millis(2200), true, true, Some(3000), None).await;
    input.case_id = "pure-gate-budget".into();
    let output = run(
        input,
        PolicyLookup::Participating(policy),
        Duration::from_secs(3),
        Duration::from_secs(2),
        |_, _| async {
            panic!("no LLM fallback");
            #[allow(unreachable_code)]
            Some(())
        },
        |_| Reference::from(BTreeMap::new()),
    )
    .await;
    assert!(output.incumbent.is_none());
    assert!(
        output.current_answers().contains_key("0"),
        "a valid answer beyond the 2s policy TTL must refresh authority without LLM fallback"
    );
}

#[tokio::test]
async fn memory_decision_mixed_gate_honors_longer_engine_budget_and_reserves_text() {
    let _serial = TEST_LOCK.lock().await;
    let (policy, _socket, mut input) =
        fixture_with_limits(Duration::from_millis(3200), true, true, Some(6000), None).await;
    input.case_id = "mixed-gate-configured-budget".into();
    input.requires_completion = true;
    let total = Duration::from_secs(10);
    let started = std::time::Instant::now();
    let output = run(
        input,
        PolicyLookup::Participating(policy),
        total,
        text_reserve(total),
        |_, _| async {
            panic!("mixed gate must never fall back to LLM classification");
            #[allow(unreachable_code)]
            Some(())
        },
        |_| Reference::from(BTreeMap::new()),
    )
    .await;
    assert!(output.incumbent.is_none());
    assert!(output.current_answers().contains_key("0"));
    assert!(started.elapsed() < total);
}

#[tokio::test]
async fn memory_decision_slow_gate_never_calls_incumbent() {
    let _serial = TEST_LOCK.lock().await;
    let (policy, _socket, mut input) =
        fixture_with_answer(Duration::from_secs(5), false, true).await;
    input.case_id = "slow-gate-without-fallback".into();
    let started = Instant::now();
    let output = run(
        input,
        PolicyLookup::Participating(policy),
        Duration::from_secs(3),
        Duration::from_secs(2),
        |_, _| async move {
            panic!("gated timeout must not call the LLM");
            #[allow(unreachable_code)]
            Some("fallback")
        },
        |_| Reference::from(BTreeMap::new()),
    )
    .await;
    assert_eq!(output.incumbent, None);
    assert!(output.answers.is_empty());
    assert!(started.elapsed() >= Duration::from_millis(500));
    assert!(started.elapsed() < Duration::from_millis(1600));
}

#[tokio::test]
async fn memory_decision_unavailable_bounds_and_unanswered_never_call_incumbent() {
    let _serial = TEST_LOCK.lock().await;
    for variant in 0..3 {
        let (mut policy, _socket, mut input) = fixture(Duration::ZERO, false).await;
        policy.policy.gate = true;
        input.case_id = format!("no-llm-{variant}");
        if variant == 1 {
            policy.policy.classification.limits.max_request_bytes = 1;
        }
        let lookup = if variant == 0 {
            PolicyLookup::Unavailable(decision_host::classification::PolicyFailure::Transport)
        } else {
            PolicyLookup::Participating(policy)
        };
        let output = run(
            input,
            lookup,
            Duration::from_secs(3),
            Duration::from_secs(2),
            |_, _| async {
                panic!("no LLM fallback");
                #[allow(unreachable_code)]
                Some(())
            },
            |_| Reference::from(BTreeMap::new()),
        )
        .await;
        assert!(output.incumbent.is_none());
        assert!(output.answers.is_empty());
    }
}

#[tokio::test]
async fn memory_decision_partial_heads_cannot_generate_unapproved_decisions() {
    let _serial = TEST_LOCK.lock().await;
    let (policy, _socket, mut input) = fixture_with_answer(Duration::ZERO, true, true).await;
    let mut second = input.items[0].clone();
    second.item_id = "1".into();
    input.items.push(second);
    let router = text_router();
    let result = super::super::text::review(
        input,
        PolicyLookup::Participating(policy),
        &router,
        "memory_test",
        "p1",
        Duration::from_secs(3),
        |_| async { anyhow::bail!("generator must never be called for partial decisions") },
        |raw| Ok(raw.to_owned()),
        |_| BTreeMap::new(),
        |_, _| Ok(()),
    )
    .await;
    assert!(result
        .err()
        .unwrap()
        .to_string()
        .contains("incomplete engine answers"));
}

#[tokio::test]
async fn memory_decision_admission_coalesces_and_caps_scopes_until_last_owner_drops() {
    let _serial = TEST_LOCK.lock().await;
    let (_, _socket, mut input) = fixture(Duration::ZERO, false).await;
    let first = Arc::new(Job::admit(&input).unwrap());
    assert!(
        Job::admit(&input).is_none(),
        "same scoped case must coalesce"
    );
    let accounting_owner = first.clone();
    drop(first);
    assert!(
        Job::admit(&input).is_none(),
        "accounting retains admission after foreground cancellation"
    );
    let mut held = Vec::new();
    for index in 1..4 {
        input.case_id = format!("case-{index}");
        held.push(Job::admit(&input).unwrap());
    }
    input.case_id = "fifth".into();
    assert!(Job::admit(&input).is_none(), "per-scope cap is four");
    drop(accounting_owner);
    assert!(Job::admit(&input).is_some());
    for scope in 1..4 {
        input.scope.scope.workspace = format!("scope-{scope}");
        for index in 0..4 {
            input.case_id = format!("case-{index}");
            held.push(Job::admit(&input).unwrap());
        }
    }
    input.scope.scope.workspace = "scope-four".into();
    held.push(Job::admit(&input).unwrap());
    input.case_id = "extra".into();
    assert!(Job::admit(&input).is_none(), "global cap is sixteen");
    drop(held);
    assert!(Job::admit(&input).is_some());
}

fn text_router() -> crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter {
    let mut config = magicllm::config::LLMRouterConfig::default();
    config.profiles.insert(
        "base".into(),
        serde_json::from_value(serde_json::json!({"provider":"ollama","model":"fixture"})).unwrap(),
    );
    config.default_profile = "base".into();
    config.operation_mapping.insert(
        "memory_test".into(),
        serde_json::from_value(serde_json::json!("base")).unwrap(),
    );
    crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter::new(Some(config))
}

#[tokio::test]
async fn memory_decision_required_text_is_generated_once_and_invalid_prose_never_applies() {
    let _serial = TEST_LOCK.lock().await;
    for valid in [true, false] {
        let (policy, _socket, mut input) = fixture_with_answer(Duration::ZERO, true, true).await;
        let router = text_router();
        input.reference_version =
            super::super::reference::version(&router, "memory_test", "p1").unwrap();
        input.requires_completion = true;
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let result=super::super::text::review(input,PolicyLookup::Participating(policy),&router,"memory_test","p1",Duration::from_secs(3),
            |heads| { calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst); assert!(!heads.is_empty()); async move {
                Ok(crate::magician_v2::query_analysis::operation_llm_router::SimplifiedLLMResponse{content:if valid {"grounded text"}else{""}.into(),..Default::default()})
            }}, |raw| Ok(raw.to_owned()), |_| BTreeMap::new(), |text,heads| {
                anyhow::ensure!(!text.trim().is_empty(),"missing required text");
                assert!(heads.contains_key("0"));Ok(())
            }).await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        if valid {
            let reviewed = result.unwrap();
            assert!(reviewed.current());
            assert!(!reviewed.origins.is_empty());
        } else {
            assert!(result.is_err());
        }
    }
}

#[tokio::test]
async fn memory_decision_required_text_reuses_incumbent_and_rollback_rejects_generated_output() {
    let _serial = TEST_LOCK.lock().await;
    for gate in [false, true] {
        let (policy, _socket, mut input) = fixture_with_answer(Duration::ZERO, true, gate).await;
        // The preceding shadow intentionally outlives its foreground. These
        // independent cases must not coalesce with that still-accounting job.
        input.case_id = format!("text-rollback-{gate}");
        let router = text_router();
        input.reference_version =
            super::super::reference::version(&router, "memory_test", "p1").unwrap();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let result=super::super::text::review(input,PolicyLookup::Participating(policy),&router,"memory_test","p1",Duration::from_secs(3),
            |heads| {calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst);assert_eq!(!heads.is_empty(),gate);async move {
                if gate { decision_host::configure(&crate::config::DecisionHostConfig{mode:crate::config::DecisionMode::Off,..Default::default()}); }
                Ok(crate::magician_v2::query_analysis::operation_llm_router::SimplifiedLLMResponse{content:"incumbent or text".into(),..Default::default()})
            }}, |raw| Ok(raw.to_owned()), |_| BTreeMap::new(), |_,_| Ok(())).await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(result.is_err(), gate);
        if !gate {
            assert!(result.unwrap().guard.is_none());
        }
    }
}

#[tokio::test]
async fn memory_decision_background_wait_allowance_is_included_in_host_timeout() {
    let _serial = TEST_LOCK.lock().await;
    let (policy, _socket, mut input) = fixture_with_limits(
        Duration::from_millis(800),
        true,
        true,
        Some(500),
        Some(1000),
    )
    .await;
    input.case_id = "background-wait-budget".into();
    let output = run(
        input,
        PolicyLookup::Participating(policy),
        Duration::from_secs(2),
        Duration::ZERO,
        |_, _| async {
            panic!("no generative fallback");
            #[allow(unreachable_code)]
            Some(())
        },
        |_| Reference::from(BTreeMap::new()),
    )
    .await;
    assert!(output.incumbent.is_none());
    assert!(output.current_answers().contains_key("0"));
}

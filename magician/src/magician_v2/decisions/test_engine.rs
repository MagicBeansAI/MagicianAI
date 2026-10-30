//! A scripted decision engine on a Unix socket, for adapter wiring tests.
//!
//! Serves `GET /v1/operations` with one operation's policy and answers
//! `POST /v1/decide` with fixed Choice answers for every item, marking an
//! answer eligible when its confidence reaches the threshold. Every decide
//! request is captured so a test can check the adapter's projection.
//!
//! The decision host is process-global: hold [`serial`] for the whole test.
use decision_engine_contract::{
    batch::{BatchResponse, DecisionItemResult, ItemStatus},
    DecideRequest, DecideResponse, DecideStatus, OperationPolicy, OperationsResponse,
    CONTRACT_VERSION,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
};

use super::runner::tests::TEST_LOCK;
use crate::magician_v2::decision_host;

/// Serializes every test that configures the global decision host.
pub(crate) async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    TEST_LOCK.lock().await
}

pub(crate) const THRESHOLD: f64 = 0.8;

pub(crate) struct FakeEngine {
    socket: PathBuf,
    requests: Arc<Mutex<Vec<DecideRequest>>>,
}

impl FakeEngine {
    /// Bind an engine for `operation` (`gate` selects gate vs shadow) that
    /// answers each listed question with `(option, confidence)`, and point the
    /// decision host at it.
    pub(crate) async fn start(operation: &str, gate: bool, answers: &[(&str, &str, f64)]) -> Self {
        let socket = PathBuf::from(format!("/tmp/dec-adapter-{}.sock", ulid::Ulid::new()));
        let listener = UnixListener::bind(&socket).expect("bind fake engine socket");
        let operations = OperationsResponse {
            contract_version: CONTRACT_VERSION,
            action_contract_version: Some(CONTRACT_VERSION),
            engine_instance: "fake-boot".into(),
            policy_revision: "fake-policy".into(),
            operations: vec![OperationPolicy {
                name: operation.into(),
                shadow: !gate,
                gate,
                max_consecutive_steps: 3,
                sees_body: true,
                route_local: vec![],
                route_cloud: vec!["jev".into()],
                classification: Default::default(),
                images_local: false,
                images_cloud: false,
            }],
        };
        let answers: Vec<(String, String, f64)> = answers
            .iter()
            .map(|(q, o, c)| ((*q).to_owned(), (*o).to_owned(), *c))
            .collect();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let served = operations.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let Some((head, body)) = read_request(&mut stream).await else {
                    continue;
                };
                let reply = if head.starts_with("GET /v1/operations") {
                    serde_json::to_vec(&served).expect("operations json")
                } else {
                    let request: DecideRequest =
                        serde_json::from_slice(&body).expect("decide request json");
                    let reply = respond(&request, &answers);
                    captured.lock().expect("requests").push(request);
                    serde_json::to_vec(&reply).expect("decide reply json")
                };
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    reply.len()
                );
                let _ = stream.write_all(header.as_bytes()).await;
                let _ = stream.write_all(&reply).await;
            }
        });
        let _ = decision_host::classification::test_participation(&socket, operations);
        Self { socket, requests }
    }

    /// The decide requests seen so far.
    pub(crate) fn requests(&self) -> Vec<DecideRequest> {
        self.requests.lock().expect("requests").clone()
    }

    /// Wait (bounded) for a shadow decide request, which runs detached.
    pub(crate) async fn wait_for_request(&self) -> DecideRequest {
        for _ in 0..200 {
            if let Some(request) = self.requests().into_iter().next() {
                return request;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the engine never received a decide request");
    }
}

impl Drop for FakeEngine {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
        decision_host::configure(&crate::config::DecisionHostConfig {
            mode: crate::config::DecisionMode::Off,
            ..Default::default()
        });
    }
}

async fn read_request(stream: &mut tokio::net::UnixStream) -> Option<(String, Vec<u8>)> {
    let mut buffer = Vec::new();
    loop {
        let mut chunk = [0u8; 8192];
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..n]);
        let Some(split) = buffer.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&buffer[..split]).to_string();
        let length: usize = head
            .to_lowercase()
            .lines()
            .find_map(|l| l.strip_prefix("content-length:").map(str::trim))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if buffer.len() >= split + 4 + length {
            return Some((head, buffer[split + 4..split + 4 + length].to_vec()));
        }
    }
}

fn respond(request: &DecideRequest, answers: &[(String, String, f64)]) -> DecideResponse {
    let items = request
        .batch
        .items
        .iter()
        .map(|item| {
            let response = serde_json::from_value(serde_json::json!({
                "model": {"adapter": "typesafe", "model": "jev-fake"},
                "pack_id": request.operation,
                "pack_version": "1.0.0",
                "answers": answers.iter().map(|(q, o, c)| (q.clone(), serde_json::json!({
                    "type": "choice", "choice": o, "probabilities": {}, "confidence": c
                }))).collect::<serde_json::Map<_, _>>(),
                "usage": {"input_tokens": 1, "output_tokens": 0},
            }))
            .expect("fake decision response");
            DecisionItemResult {
                item_id: item.item_id.clone(),
                status: ItemStatus::Answered,
                response: Some(response),
                thresholds: Some(
                    answers
                        .iter()
                        .map(|(q, _, _)| (q.clone(), THRESHOLD))
                        .collect(),
                ),
                eligible_answers: answers
                    .iter()
                    .filter(|(_, _, c)| *c >= THRESHOLD)
                    .map(|(q, _, _)| (q.clone(), "fake-qualified".to_owned()))
                    .collect::<BTreeMap<_, _>>(),
                error: None,
                latency_ms: 1,
            }
        })
        .collect();
    DecideResponse {
        batch: BatchResponse {
            request_id: request.batch.request_id.clone(),
            engine_instance: "fake-boot".into(),
            policy_revision: "fake-policy".into(),
            items,
            model_health: Default::default(),
        },
        model_calls: vec![],
        contract_version: CONTRACT_VERSION,
        status: DecideStatus::Answered,
        response: None,
        thresholds: None,
        error: None,
        latency_ms: 1,
    }
}

/// A router whose only profile is a local fixture, with `operation` mapped to
/// it, so `reference::version` binds. Never dispatched by these tests.
pub(crate) fn fixture_router(
    operation: &str,
) -> crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter {
    let mut config = magicllm::config::LLMRouterConfig::default();
    config.profiles.insert(
        "base".into(),
        serde_json::from_value(serde_json::json!({"provider": "ollama", "model": "fixture"}))
            .expect("fixture profile"),
    );
    config.default_profile = "base".into();
    config.operation_mapping.insert(
        operation.into(),
        serde_json::from_value(serde_json::json!("base")).expect("fixture mapping"),
    );
    crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter::new(Some(config))
}

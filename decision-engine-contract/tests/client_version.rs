#![cfg(feature = "client")]
use decision_engine_contract::{
    action::*,
    client::{ClientError, EngineClient},
    *,
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
};
async fn old_engine() -> (EngineClient, PathBuf) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = PathBuf::from(format!(
        "/tmp/dec-v4-{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let listener = UnixListener::bind(&path).unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        let _ = stream.read(&mut request).await.unwrap();
        // Intentionally lacks current fields: version failure must remain explicit.
        let body = r#"{"contract_version":4,"status":"unbound"}"#;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    (EngineClient::new(&path, Duration::from_secs(1)), path)
}
#[tokio::test]
async fn v5_client_rejects_v4_decide_and_action_before_decoding_body() {
    let (client, path) = old_engine().await;
    let request = DecideRequest {
        contract_version: CONTRACT_VERSION,
        batch: Default::default(),
        operation: "test".into(),
        state: request::DecisionState::from_text(""),
        choice_candidates: Default::default(),
        locality: Default::default(),
    };
    assert!(matches!(
        client.decide(&request).await,
        Err(ClientError::ContractMismatch { engine: 4 })
    ));
    std::fs::remove_file(path).unwrap();
    let (client, path) = old_engine().await;
    let request = ActionRequest {
        contract_version: CONTRACT_VERSION,
        snapshot: "s".into(),
        locality: Default::default(),
        context: Default::default(),
        tools: vec![],
        plan: Default::default(),
        phase: Default::default(),
        consecutive_gated_steps: 0,
    };
    assert!(matches!(
        client.action(&request).await,
        Err(ClientError::ContractMismatch { engine: 4 })
    ));
    std::fs::remove_file(path).unwrap();
}

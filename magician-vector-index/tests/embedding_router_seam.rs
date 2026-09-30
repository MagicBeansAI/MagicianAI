//! Routed embedding seam: provider identity comes from router config.
//!
//! One sequential test: the embedding router is a process-global, so the
//! routed / fallback / error-classification phases must not interleave with
//! each other (parallel #[tokio::test]s in this binary would race the global).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use magician_vector_index::vector_toolkit::{OllamaEmbedder, OllamaEmbedderConfig};
use magician_vector_index::{install_embedding_router, uninstall_embedding_router};
use magicllm::config::{LLMProfile, LLMRouterConfig, OperationProfileSelector};
use magicllm::ConfiguredRouter;
use serde_json::{json, Value};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn embedder(base_url: &str) -> OllamaEmbedder {
    OllamaEmbedder::new(OllamaEmbedderConfig {
        base_url: base_url.to_string(),
        model: "pplx-embed".to_string(),
        dims: 2,
        timeout: Duration::from_secs(10),
        query_timeout: Duration::from_secs(5),
        health_timeout: Duration::from_secs(5),
        batch_size: 8,
        num_parallel: 1,
        max_loaded_models: 1,
        keep_alive: Some("-1".to_string()),
        context_tokens: Some(8192),
        batch_tokens: Some(512),
    })
}

fn embedding_router_config(daemon_root: &str) -> LLMRouterConfig {
    let mut profiles = HashMap::new();
    profiles.insert(
        "op-embedding-local".to_string(),
        LLMProfile {
            provider: magicllm::LLMProviderKind::Ollama,
            model: "pplx-embed".to_string(),
            api_key_env: None,
            api_base_url: Some(daemon_root.to_string()),
            temperature: None,
            max_output_tokens: None,
            context_window_tokens: None,
            chunking: None,
            default_modality: None,
            reasoning: None,
            metadata: None,
            supports_vision: None,
            supports_reasoning: None,
            supports_tool_calling: None,
            supports_computer_use: None,
            timeout_secs: Some(30),
        },
    );
    let mut operation_mapping = HashMap::new();
    operation_mapping.insert(
        "embed_documents".to_string(),
        OperationProfileSelector::Simple("op-embedding-local".to_string()),
    );
    operation_mapping.insert(
        "embed_query".to_string(),
        OperationProfileSelector::Simple("op-embedding-local".to_string()),
    );
    LLMRouterConfig {
        profiles,
        operation_mapping,
        default_profile: "op-embedding-local".to_string(),
        ..LLMRouterConfig::default()
    }
}

/// One embedding per POST: write-priority work posts one input per request
/// (background requests are deliberately single-sequence).
async fn mount_embed_single(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/embed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "embeddings": [[0.25, 0.75]]
        })))
        .mount(server)
        .await;
}

async fn first_embed_body(server: &MockServer) -> Value {
    let requests = server.received_requests().await.expect("recorded requests");
    let post = requests
        .iter()
        .find(|request| request.url.path() == "/api/embed")
        .expect("an /api/embed POST reached the daemon");
    serde_json::from_slice(&post.body).expect("wire body is JSON")
}

#[tokio::test]
async fn embedding_seam_routed_fallback_and_error_classification() {
    // ── Phase 1: routed profile serves the call (direct base is dead). ──
    let server = MockServer::start().await;
    mount_embed_single(&server).await;
    let router = ConfiguredRouter::from_router_config(embedding_router_config(&server.uri()))
        .expect("embedding router config");
    install_embedding_router(Arc::new(router));

    let vectors = embedder("http://127.0.0.1:1")
        .embed_documents(&["first entry".to_string(), "second entry".to_string()])
        .await
        .expect("routed embed succeeds");

    assert_eq!(
        vectors.len(),
        2,
        "one vector per input, pooled from fragments"
    );
    assert_eq!(vectors[0], vec![0.25_f32, 0.75]);
    let body = first_embed_body(&server).await;
    // Wire parity with the pre-migration direct body.
    assert_eq!(body["model"], "pplx-embed");
    assert_eq!(body["input"], json!(["first entry"]));
    assert_eq!(body["truncate"], false);
    assert_eq!(body["keep_alive"], json!(-1));
    assert_eq!(body["options"]["num_ctx"], 8192);
    assert_eq!(body["options"]["num_batch"], 512);

    // ── Phase 2: no router installed ⇒ ad-hoc provider fallback. ──
    uninstall_embedding_router();
    let fallback_server = MockServer::start().await;
    mount_embed_single(&fallback_server).await;
    let vectors = embedder(&fallback_server.uri())
        .embed_documents(&["direct entry".to_string()])
        .await
        .expect("direct fallback embed succeeds");
    assert_eq!(vectors, vec![vec![0.25_f32, 0.75]]);
    let body = first_embed_body(&fallback_server).await;
    assert_eq!(body["model"], "pplx-embed");
    assert_eq!(body["input"], json!(["direct entry"]));
    assert_eq!(body["truncate"], false);
    assert_eq!(body["keep_alive"], json!(-1));
    assert_eq!(body["options"]["num_ctx"], 8192);
    assert_eq!(body["options"]["num_batch"], 512);

    // ── Phase 3: typed status errors preserve body + classification. ──
    let error_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/embed"))
        .respond_with(ResponseTemplate::new(503).set_body_string("{\"error\":\"runner busy\"}"))
        .mount(&error_server)
        .await;
    let router = ConfiguredRouter::from_router_config(embedding_router_config(&error_server.uri()))
        .expect("embedding router config");
    install_embedding_router(Arc::new(router));

    let error = embedder("http://127.0.0.1:1")
        .embed_documents(&["one".to_string()])
        .await
        .expect_err("503 must surface as an error");

    uninstall_embedding_router();

    let chain = format!("{error:#}");
    assert!(chain.contains("runner busy"), "body intact in: {chain}");
    assert!(chain.contains("503"), "status intact in: {chain}");
}

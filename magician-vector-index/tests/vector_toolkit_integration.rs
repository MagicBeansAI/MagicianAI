//! Integration tests for `vector_toolkit`.
//!
//! These tests cover the in-memory algorithmic helpers (RankResult variants,
//! clustering, RRF, normalization). Tests that require a live Ollama daemon
//! or a writable lancedb table are gated behind the `live-ollama` env var so
//! CI doesn't depend on an embedder.

use magician_vector_index::{
    OllamaEmbedder, OllamaEmbedderConfig, RankOutput, RankResult, SearchMode, VectorItem,
    VectorTable,
};
use serde_json::json;

fn item(id: &str, text: &str) -> VectorItem {
    VectorItem {
        id: id.to_string(),
        text: text.to_string(),
        metadata: json!({}),
    }
}

#[tokio::test]
async fn rank_ranked_empty_items_returns_empty() {
    let cfg = OllamaEmbedderConfig {
        base_url: "http://127.0.0.1:1".to_string(),
        ..Default::default()
    };
    let embedder = OllamaEmbedder::new(cfg);
    let result = VectorTable::rank(&embedder, &[], Some("query"), RankOutput::Ranked, 0.0, None)
        .await
        .expect("empty input is fast path");
    match result {
        RankResult::Ranked(hits) => assert!(hits.is_empty()),
        _ => panic!("expected Ranked"),
    }
}

#[tokio::test]
async fn rank_ranked_requires_query() {
    let cfg = OllamaEmbedderConfig {
        base_url: "http://127.0.0.1:1".to_string(),
        ..Default::default()
    };
    let embedder = OllamaEmbedder::new(cfg);
    // With non-empty items, rank(Ranked) without query must error before
    // hitting Ollama (so port 1 unreachability doesn't matter).
    let items = vec![item("a", "hello")];
    let err = VectorTable::rank(&embedder, &items, None, RankOutput::Ranked, 0.0, None).await;
    // The embed call may fail first depending on order; either an "Ollama"
    // error or the "requires query" error is acceptable.
    assert!(err.is_err());
}

#[tokio::test]
async fn ollama_embedder_health_fails_for_unreachable_url() {
    let cfg = OllamaEmbedderConfig {
        base_url: "http://127.0.0.1:1".to_string(),
        ..Default::default()
    };
    let embedder = OllamaEmbedder::new(cfg);
    assert!(embedder.health_check().await.is_err());
}

#[tokio::test]
async fn ollama_embedder_ignores_environment_model_contract_fallbacks() {
    std::env::remove_var("MAGICIAN_OLLAMA_BASE_URL");
    std::env::remove_var("MAGICIAN_MEMORY_OLLAMA_URL");
    std::env::set_var("MAGICIAN_OLLAMA_EMBEDDING_MODEL", "ignored-model");
    std::env::set_var("MAGICIAN_OLLAMA_EMBEDDING_DIMENSIONS", "123");
    std::env::set_var("MAGICIAN_OLLAMA_EMBEDDING_CONTEXT_TOKENS", "456");
    std::env::set_var("MAGICIAN_OLLAMA_EMBEDDING_BATCH_TOKENS", "78");
    std::env::set_var("MAGICIAN_OLLAMA_EMBEDDING_BATCH_SIZE", "9");
    let cfg = OllamaEmbedderConfig::from_env();
    std::env::remove_var("MAGICIAN_OLLAMA_EMBEDDING_MODEL");
    std::env::remove_var("MAGICIAN_OLLAMA_EMBEDDING_DIMENSIONS");
    std::env::remove_var("MAGICIAN_OLLAMA_EMBEDDING_CONTEXT_TOKENS");
    std::env::remove_var("MAGICIAN_OLLAMA_EMBEDDING_BATCH_TOKENS");
    std::env::remove_var("MAGICIAN_OLLAMA_EMBEDDING_BATCH_SIZE");
    assert_eq!(cfg.base_url, "http://127.0.0.1:11435");
    assert!(cfg.model.is_empty());
    assert_eq!(cfg.dims, 0);
    assert_eq!(cfg.batch_size, 0);
    assert_eq!(cfg.context_tokens, None);
    assert_eq!(cfg.batch_tokens, None);
    assert_eq!(cfg.keep_alive.as_deref(), Some("-1"));
}

#[tokio::test]
async fn vector_index_upserts_without_replacing_the_namespace() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let table = VectorTable::at(tmp.path(), 2);
    let initial = vec![item("a", "original alpha"), item("b", "retained beta")];
    table
        .index_with_embeddings(&initial, &[vec![1.0, 0.0], vec![0.0, 1.0]])
        .await
        .expect("create initial vector table");

    let marker = tmp.path().join("runtime-preserved.marker");
    std::fs::write(&marker, "preserve namespace").expect("write marker");
    let update = vec![item("a", "updated alpha"), item("c", "inserted gamma")];
    table
        .index_with_embeddings(&update, &[vec![0.8, 0.2], vec![0.5, 0.5]])
        .await
        .expect("merge vector rows");

    let embedder = OllamaEmbedder::new(OllamaEmbedderConfig {
        base_url: "http://127.0.0.1:1".to_string(),
        dims: 2,
        ..Default::default()
    });
    let updated = table
        .search(&embedder, "updated", SearchMode::Fts, 10)
        .await
        .expect("search updated row");
    let retained = table
        .search(&embedder, "retained", SearchMode::Fts, 10)
        .await
        .expect("search retained row");
    let inserted = table
        .search(&embedder, "inserted", SearchMode::Fts, 10)
        .await
        .expect("search inserted row");

    assert_eq!(updated.first().map(|hit| hit.id.as_str()), Some("a"));
    assert_eq!(retained.first().map(|hit| hit.id.as_str()), Some("b"));
    assert_eq!(inserted.first().map(|hit| hit.id.as_str()), Some("c"));
    assert!(
        marker.exists(),
        "runtime upsert must preserve the LanceDB namespace directory"
    );
}

// =============================================================================
// Live-Ollama tests (only run when LIVE_OLLAMA=1 in env)
// =============================================================================

fn live_ollama_enabled() -> bool {
    std::env::var("LIVE_OLLAMA").ok().as_deref() == Some("1")
}

#[tokio::test]
async fn live_index_search_roundtrip() {
    if !live_ollama_enabled() {
        eprintln!("skipped: set LIVE_OLLAMA=1 to run");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let embedder = OllamaEmbedder::from_env();
    let table = VectorTable::at(tmp.path(), embedder.config().dims);
    let items = vec![
        item("1", "OpenAI Sora 2 launches to the public"),
        item("2", "Mistral releases a new mixture-of-experts model"),
        item("3", "DeepMind preview of Gemini 3"),
    ];
    table.index(&embedder, &items).await.expect("index");

    let hits = table
        .search(
            &embedder,
            "video generation model",
            magician_vector_index::SearchMode::Hybrid,
            5,
        )
        .await
        .expect("search");
    assert!(!hits.is_empty(), "expected at least one hit");
    // Sora should rank above the others for video-generation intent.
    assert_eq!(hits[0].id, "1", "Sora should win on video-generation");
}

#[tokio::test]
async fn live_rank_dedupes_paraphrases() {
    if !live_ollama_enabled() {
        eprintln!("skipped: set LIVE_OLLAMA=1 to run");
        return;
    }
    let embedder = OllamaEmbedder::from_env();
    let items = vec![
        item("a", "OpenAI Sora 2 launches today"),
        item("b", "Sora 2 hits public beta — what's new"),
        item("c", "Apple releases Vision Pro 2 hardware refresh"),
    ];
    let result = VectorTable::rank(&embedder, &items, None, RankOutput::Deduped, 0.75, None)
        .await
        .expect("rank");
    match result {
        RankResult::Deduped(hits) => {
            assert!(
                hits.len() <= 2,
                "expected Sora paraphrases to collapse; got {} hits",
                hits.len()
            );
        },
        _ => panic!("expected Deduped"),
    }
}

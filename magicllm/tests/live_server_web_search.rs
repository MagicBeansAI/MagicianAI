//! Live-provider verification for server-side web search.
//!
//! These tests make REAL, BILLABLE API calls. They are `#[ignore]`d so the
//! normal suite never touches the network; run them explicitly with the
//! relevant keys exported:
//!
//! ```sh
//! set -a; source "$MAGICIAN_ROOT_DIR/.env.development"; set +a
//! CARGO_TARGET_DIR=/Volumes/build/magician/builds/wt/magicllm-web-search \
//!   cargo test -p magicllm --test live_server_web_search -- --ignored --nocapture
//! ```
//!
//! Each test exercises the full production path: `LLMRequest` with the
//! `server_web_search` extra flag → provider `invoke()` → wire call →
//! response parsing, then asserts the provider ACTUALLY searched by
//! counting search calls / citations from the retained raw response.
//! A missing key skips that provider (exit 0) so the harness degrades
//! gracefully; Anthropic is skipped until the account has credits and
//! OpenRouter until an `OPENROUTER_API_KEY` exists in the env files.

use magicllm::capability::LLMProviderKind;
use magicllm::provider::LLMProvider;
use magicllm::providers::{GeminiProvider, OpenAIResponsesProvider};
use magicllm::server_web_search::{extract_citations, web_search_call_count};
use magicllm::types::{ContentBlock, LLMMessage, LLMRequest, MessageRole};
use serde_json::json;

/// A question whose answer changes often enough that a model without
/// search cannot fake the citations, and any grounding shows up in metadata.
const FRESHNESS_QUERY: &str =
    "Search the web: what is the latest stable version of the Rust programming \
     language, and when was it released? Answer in one sentence.";

fn search_request(model: &str) -> LLMRequest {
    LLMRequest {
        model: model.to_string(),
        messages: vec![LLMMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::text(FRESHNESS_QUERY)],
        }]
        .into(),
        extra: Some(json!({"server_web_search": true}).into()),
        ..Default::default()
    }
}

fn report(provider: &str, response: &magicllm::types::LLMResponse, kind: &LLMProviderKind) {
    let raw = response
        .raw_response
        .as_ref()
        .expect("raw response retained");
    let citations = extract_citations(kind, raw);
    let calls = web_search_call_count(kind, raw);
    println!("--- {provider} ---");
    println!(
        "answer: {}",
        response
            .text
            .as_deref()
            .map(|text| text.chars().take(220).collect::<String>())
            .unwrap_or_else(|| "<empty>".to_string())
    );
    println!("server-side searches executed: {calls}");
    for citation in citations.iter().take(5) {
        println!(
            "  cited: {} ({})",
            citation.url,
            citation.title.as_deref().unwrap_or("untitled")
        );
    }
}

#[tokio::test]
#[ignore = "makes a real billable OpenAI API call"]
async fn live_openai_responses_web_search() {
    let Ok(api_key) = std::env::var("OPENAI_API_KEY") else {
        eprintln!("SKIP: OPENAI_API_KEY not set");
        return;
    };
    let provider = OpenAIResponsesProvider::new(api_key);
    let response = provider
        .invoke(search_request("gpt-6-luna"))
        .await
        .expect("OpenAI Responses invoke must succeed");
    let raw = response
        .raw_response
        .as_ref()
        .expect("raw response retained");
    let calls = web_search_call_count(&LLMProviderKind::OpenAI, raw);
    let citations = extract_citations(&LLMProviderKind::OpenAI, raw);
    report("openai_responses", &response, &LLMProviderKind::OpenAI);
    assert!(
        calls >= 1,
        "expected at least one web_search_call item; got {calls} — the tool was not injected or did not run"
    );
    assert!(
        !citations.is_empty(),
        "expected url_citation annotations in the response"
    );
}

#[tokio::test]
#[ignore = "makes a real billable Gemini API call"]
async fn live_gemini_google_search_grounding() {
    let Ok(api_key) = std::env::var("GEMINI_API_KEY") else {
        eprintln!("SKIP: GEMINI_API_KEY not set");
        return;
    };
    let provider = GeminiProvider::new(api_key);
    let response = provider
        .invoke(search_request("gemini-3.1-pro-preview"))
        .await
        .expect("Gemini invoke must succeed");
    let raw = response
        .raw_response
        .as_ref()
        .expect("raw response retained");
    let calls = web_search_call_count(&LLMProviderKind::Gemini, raw);
    let citations = extract_citations(&LLMProviderKind::Gemini, raw);
    report("gemini", &response, &LLMProviderKind::Gemini);
    assert!(
        calls >= 1,
        "expected grounding webSearchQueries; got {calls} — googleSearch tool was not injected or did not ground"
    );
    assert!(
        !citations.is_empty(),
        "expected groundingChunks with web URIs in the response"
    );
}

/// End-to-end config proof: a router built from an `op-web-answer`-shaped
/// profile (metadata `server_web_search`, pinned Responses mode) must inject
/// the flag via profile-defaults merging — the request itself carries NO
/// extra flag — and the transport must actually search. This is the exact
/// chain the magician `web_answer` tool drives.
#[tokio::test]
#[ignore = "makes a real billable OpenAI API call through a configured router"]
async fn live_web_answer_operation_profile_injects_the_flag_from_config() {
    let Ok(_api_key) = std::env::var("OPENAI_API_KEY") else {
        eprintln!("SKIP: OPENAI_API_KEY not set");
        return;
    };
    use magicllm::bootstrap::ConfiguredRouter;
    use magicllm::config::{LLMProfile, LLMRouterConfig, OperationProfileSelector};

    let profile: LLMProfile = serde_json::from_value(json!({
        "provider": "openai",
        "model": "gpt-6-luna",
        "api_key_env": "OPENAI_API_KEY",
        "supports_tool_calling": true,
        "max_output_tokens": 2000,
        "timeout_secs": 60,
        "metadata": {
            "openai_api_mode": "responses",
            "server_web_search": true
        }
    }))
    .expect("profile shape");

    let mut config = LLMRouterConfig::default();
    config.default_profile = "op-web-answer".to_string();
    config.profiles.insert("op-web-answer".to_string(), profile);
    config.operation_mapping.insert(
        "web_answer".to_string(),
        OperationProfileSelector::Simple("op-web-answer".to_string()),
    );
    // Profile validation must accept the server_web_search metadata.
    let router = ConfiguredRouter::from_router_config(config)
        .expect("configured router with server_web_search profile");

    // No `server_web_search` in extra — the profile must supply it. The
    // extra lane is cleared explicitly so this test cannot pass through the
    // caller-side flag by accident.
    let mut request = search_request("gpt-6-luna");
    request.extra = None;
    request.metadata.operation = "web_answer".to_string();
    let response = router
        .route(request)
        .await
        .expect("routed web_answer call succeeds");
    let raw = response
        .raw_response
        .as_ref()
        .expect("raw response retained");
    let calls = web_search_call_count(&LLMProviderKind::OpenAI, raw);
    let citations = extract_citations(&LLMProviderKind::OpenAI, raw);
    println!("--- configured-router web_answer ---");
    println!(
        "answer: {}",
        response
            .text
            .as_deref()
            .map(|text| text.chars().take(220).collect::<String>())
            .unwrap_or_else(|| "<empty>".to_string())
    );
    println!("server-side searches executed: {calls}");
    for citation in citations.iter().take(5) {
        println!("  cited: {}", citation.url);
    }
    assert!(
        calls >= 1,
        "profile metadata must inject server_web_search; got {calls} searches"
    );
    assert!(!citations.is_empty(), "expected citations from the search");
}

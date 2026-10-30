//! Argon profile/wire contracts, plus an opt-in live text and tool smoke.
//! Run with `make cargo ARGS="test -p magicllm --test gemini4_argon"`.
//! With GEMINI_API_KEY exported, append `-- --ignored --nocapture` for live calls.

use std::{collections::HashMap, sync::Arc, time::Instant};

use magicllm::{
    config::{LLMProfile, LLMRouterConfig},
    providers::GeminiProvider,
    router::MultiLLMRouter,
    types::{ContentBlock, LLMMessage, LLMRequest, LLMToolSpec, MessageRole, PromptCacheConfig},
    LLMModality, LLMProvider, LLMReasoning,
};
use serde_json::{json, Value};
use wiremock::{
    matchers::{method, path, query_param},
    Mock, MockServer, ResponseTemplate,
};

const MODEL: &str = "gemini-4-argon";
const MODEL_ID: &str = "gemini/gemini-4-argon";

fn route(thinking: &str) -> String {
    magicllm::catalog::choice_id(MODEL_ID, thinking)
}

fn profiles() -> HashMap<String, LLMProfile> {
    let text = include_str!("../../magician-config.yaml").replacen(
        "  router:\n",
        &format!("  router:\n{}\n", include_str!("../../llm-router.yaml")),
        1,
    );
    let root: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
    let config: LLMRouterConfig = serde_yaml::from_value(root["llm"]["router"].clone()).unwrap();
    config
        .catalog
        .choices()
        .into_iter()
        .find(|choice| choice.id == MODEL_ID)
        .expect("qualified Argon model")
        .choices
        .into_values()
        .map(|name| (name.clone(), config.profiles[&name].clone()))
        .collect()
}

fn router(provider: GeminiProvider) -> MultiLLMRouter {
    let mut router = MultiLLMRouter::new(LLMRouterConfig {
        profiles: profiles(),
        default_profile: route("auto"),
        ..Default::default()
    })
    .unwrap();
    router.register_provider(Arc::new(provider));
    router
}

fn request(profile: &str, prompt: &str) -> LLMRequest {
    LLMRequest {
        messages: vec![LLMMessage::user(prompt)].into(),
        extra: Some(json!({"router_profile_override": profile}).into()),
        ..Default::default()
    }
}

#[test]
fn gemini4_argon_profiles_have_supported_limits_and_capabilities() {
    let provider = GeminiProvider::new("unused");
    let capabilities = provider.capabilities(MODEL);
    assert!(capabilities.modalities.contains(&LLMModality::Vision));
    assert_eq!(capabilities.reasoning, LLMReasoning::Standard);
    assert!(capabilities.tool_calling);
    let profiles = profiles();
    assert_eq!(profiles.len(), 4);
    for profile in profiles.values() {
        assert_eq!(profile.model, MODEL);
        assert_eq!(profile.max_output_tokens, Some(32_768));
        assert_eq!(profile.context_window_tokens, Some(1_048_576));
        assert_eq!(profile.timeout_secs, Some(600));
        assert_eq!(profile.api_key_env.as_deref(), Some("GEMINI_API_KEY"));
        assert!(profile.temperature.is_none());
    }
}

#[tokio::test]
async fn gemini4_argon_routes_thinking_omits_sampling_and_explicit_cache() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/models/gemini-4-argon:streamGenerateContent"))
        .and(query_param("alt", "sse"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!(
                "data: {}\n\n",
                json!({
                    "candidates": [{"content": {"role":"model", "parts":[{"text":"ARGON_OK"}]}, "finishReason":"STOP"}],
                    "usageMetadata": {"promptTokenCount":10,"candidatesTokenCount":3,"thoughtsTokenCount":7,"totalTokenCount":20}
                })
            ),
            "text/event-stream",
        ))
        .expect(3)
        .mount(&server).await;
    let router = router(GeminiProvider::with_client(
        reqwest::Client::new(),
        "unused",
        server.uri(),
    ));
    for (index, effort) in ["low", "medium", "high"].iter().enumerate() {
        let profile = route(effort);
        let mut request = request(&profile, "Reply ARGON_OK");
        request.temperature = Some(0.4);
        request.top_p = Some(0.8);
        request.prompt_cache = Some(PromptCacheConfig::enabled_with_cached_content(
            "cachedContents/old",
        ));
        if index == 1 {
            request.set_extra(
                json!({"router_profile_override": profile,"cachedContent":"cachedContents/extra"}),
            );
        }
        let response = router.route(request).await.unwrap();
        let usage = response.usage.unwrap();
        assert_eq!(
            usage.completion_tokens,
            Some(10),
            "thinking is billable output"
        );
        assert_eq!(usage.reasoning_tokens, Some(7));
    }
    for (captured, effort) in server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .zip(["low", "medium", "high"])
    {
        let body: Value = serde_json::from_slice(&captured.body).unwrap();
        let generation = &body["generationConfig"];
        assert_eq!(generation["thinkingConfig"]["thinkingLevel"], effort);
        assert_eq!(generation["maxOutputTokens"], 32768);
        assert!(generation.get("temperature").is_none());
        assert!(generation.get("topP").is_none());
        assert!(generation["thinkingConfig"].get("thinkingBudget").is_none());
        assert!(body.get("cachedContent").is_none());
        assert!(body.get("router_profile_override").is_none());
    }
}

#[tokio::test]
#[ignore = "makes five billable Gemini API calls; requires GEMINI_API_KEY"]
async fn live_gemini4_argon_profiles_text_and_tool_replay() {
    let key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY required for live test");
    let router = router(GeminiProvider::new(key));
    for effort in ["low", "medium", "high"] {
        let start = Instant::now();
        let response = router
            .route(request(&route(effort), "Respond with only ARGON_OK."))
            .await
            .expect("Argon text request succeeds");
        assert_eq!(response.text.as_deref().map(str::trim), Some("ARGON_OK"));
        assert_eq!(response.finish_reason.as_deref(), Some("STOP"));
        assert!(
            response
                .usage
                .as_ref()
                .and_then(|u| u.completion_tokens)
                .unwrap_or(0)
                > 0
        );
        println!(
            "Argon {effort}: text passed, {}ms, usage={:?}",
            start.elapsed().as_millis(),
            response.usage
        );
    }

    let mut tool_request = request(
        &route("high"),
        "Call lookup_code exactly once, then reply with only the code it returns.",
    );
    tool_request.set_extra(json!({"router_profile_override":route("high"), "tool_choice":"any"}));
    tool_request.tools = vec![LLMToolSpec {
        name: "lookup_code".into(),
        description: "Return the secret code".into(),
        parameters: json!({"type":"object","properties":{}}),
    }]
    .into();
    let response = router
        .route(tool_request.clone())
        .await
        .expect("Argon tool call succeeds");
    assert_eq!(response.tool_calls.len(), 1);
    let call = &response.tool_calls[0];
    assert_eq!(call.name, "lookup_code");
    // Preserve thought signatures and provider call IDs exactly as rich chat does.
    let raw = response.raw_response.as_ref().unwrap();
    let parts = raw["candidates"][0]["content"]["parts"].clone();
    let mut messages = tool_request.messages.as_ref().clone();
    messages.push(LLMMessage {
        role: MessageRole::Assistant,
        content: vec![ContentBlock::Json {
            value: json!({"_magicllm_gemini_raw_parts":parts}),
        }],
    });
    messages.push(LLMMessage {
        role: MessageRole::Tool,
        content: vec![ContentBlock::ToolResult {
            tool_call_id: call.id.clone(),
            content: json!({"code":"cobalt-7429"}),
        }],
    });
    tool_request.messages = messages.into();
    tool_request.set_extra(json!({"router_profile_override":route("high"), "tool_choice":"none"}));
    let answer = router
        .route(tool_request)
        .await
        .expect("Argon tool result replay succeeds");
    assert!(answer
        .text
        .as_deref()
        .unwrap_or_default()
        .contains("cobalt-7429"));
    assert!(answer.tool_calls.is_empty());
    println!(
        "Argon high: tool call and replay passed, usage={:?}",
        answer.usage
    );
}

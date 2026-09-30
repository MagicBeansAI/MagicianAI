use base64::Engine;
use magicllm::{
    error::LLMError,
    providers::{
        AnthropicMessagesProvider, DeepSeekProvider, GeminiProvider, MinimaxProvider,
        OllamaProvider, OpenAIChatProvider, OpenAIResponsesProvider, OpenRouterProvider,
        YutoriN1Provider,
    },
    types::{
        ContentBlock, LLMMessage, LLMRequest, LLMResponseFormat, LLMToolSpec, MessageRole,
        PromptCacheConfig, ReasoningConfig, RequestMetadata,
    },
    ContextReuseConfig, ContextReuseStrategy, LLMModality, LLMProvider,
};
use serde_json::{json, Value};
use std::{str::FromStr, time::Duration};
use wiremock::{
    http::HeaderName,
    matchers::{method, path},
    Mock, MockServer, Request, ResponseTemplate,
};

fn sample_tool() -> LLMToolSpec {
    LLMToolSpec {
        name: "extract_data".to_string(),
        description: "Extracts structured data".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "foo": { "type": "string" }
            },
            "required": ["foo"]
        }),
    }
}

fn build_text_tool_request() -> LLMRequest {
    let mut request = LLMRequest::default();
    request.model = "gpt-5.6-terra".to_string();
    request.messages = vec![
        LLMMessage::system("You are helpful."),
        LLMMessage::user("Check current status."),
    ]
    .into();
    request.tools = vec![sample_tool()].into();
    request.set_response_format(LLMResponseFormat::JsonObject);
    request.metadata = RequestMetadata {
        operation: "status_check".to_string(),
        trace_id: Some("trace-parity".to_string()),
        timeout_secs: Some(20),
        tags: None,
        trace_context: None,
        provider_attempt_counter: None,
        ..RequestMetadata::default()
    };
    request.temperature = Some(0.2);
    request.top_p = Some(0.7);
    request.max_output_tokens = Some(128);
    request.set_extra(json!({ "seed": 101 }));
    request
}

fn header_value(request: &Request, name: &str) -> Option<String> {
    HeaderName::from_str(name).ok().and_then(|header| {
        request
            .headers
            .get(&header)
            .and_then(|values| values.get(0))
            .map(|value| value.as_str().to_string())
    })
}

#[tokio::test]
async fn openai_responses_invocation_sends_expected_payload_and_parses_response(
) -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;

    let response_payload = json!({
        "output": [
            {
                "type": "message",
                "role": "assistant",
                "content": [
                    { "type": "output_text", "text": "Hello structured world!" },
                    {
                        "type": "tool_call",
                        "tool_call": {
                            "id": "tool_1",
                            "function": {
                                "name": "extract_data",
                                "arguments": "{\"foo\":\"bar\"}"
                            }
                        }
                    }
                ]
            }
        ],
        "usage": {
            "prompt_tokens": 12,
            "completion_tokens": 7,
            "total_tokens": 19
        },
        "status": "completed"
    });

    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_payload.clone()))
        .mount(&server)
        .await;

    let provider = OpenAIResponsesProvider::with_client(
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap(),
        "test-key",
        format!("{}/v1/responses", server.uri()),
    );

    let schema = json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" }
        },
        "required": ["summary"]
    });

    let mut request = LLMRequest::default();
    request.model = "gpt-5.6-terra".to_string();
    request.messages = vec![
        LLMMessage::system("You are a structured assistant."),
        LLMMessage {
            role: magicllm::types::MessageRole::User,
            content: vec![
                ContentBlock::Text {
                    text: "Summarise this data.".to_string(),
                },
                ContentBlock::ImageUrl {
                    url: "https://example.com/screenshot.png".to_string(),
                    prompt: None,
                },
            ],
        },
    ]
    .into();
    request.modality = LLMModality::Vision;
    request.tools = vec![sample_tool()].into();
    request.set_response_format(LLMResponseFormat::JsonSchema {
        schema: schema.clone(),
    });
    request.reasoning = Some(ReasoningConfig {
        effort: Some("medium".to_string()),
        max_reasoning_tokens: Some(256),
        strategy: Some("default".to_string()),
        summary: None,
    });
    request.metadata = RequestMetadata {
        operation: "analysis".to_string(),
        trace_id: Some("trace-123".to_string()),
        timeout_secs: Some(42),
        tags: Some(vec!["experiment:responses".to_string()]),
        trace_context: None,
        provider_attempt_counter: None,
        ..RequestMetadata::default()
    };
    request.temperature = Some(0.3);
    request.top_p = Some(0.8);
    request.max_output_tokens = Some(200);
    request.set_extra(json!({ "seed": 99 }));

    let response = provider.invoke(request).await?;
    assert_eq!(response.text.as_deref(), Some("Hello structured world!"));
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].name, "extract_data");
    assert_eq!(response.tool_calls[0].arguments, json!({"foo": "bar"}));
    assert_eq!(
        response.usage.as_ref().and_then(|u| u.total_tokens),
        Some(19)
    );
    assert_eq!(response.finish_reason.as_deref(), Some("completed"));

    let requests = server
        .received_requests()
        .await
        .ok_or("no requests captured for OpenAI Responses")?;
    assert_eq!(requests.len(), 1);
    let captured = &requests[0];
    assert_eq!(
        header_value(captured, "authorization"),
        Some("Bearer test-key".to_string())
    );

    let body: Value = serde_json::from_slice(&captured.body)?;
    assert_eq!(body["model"], "gpt-5.6-terra");
    assert_eq!(body["input"].as_array().unwrap().len(), 2);
    // Note: modalities parameter is NOT supported in Responses API (only Realtime API).
    // Vision/media is handled via content block types, not a modalities array.
    assert!(
        body["modalities"].is_null(),
        "Responses API should NOT include modalities"
    );
    // Responses API uses flat tool format: {"type":"function","name":...}
    // NOT the Chat Completions nested format: {"type":"function","function":{"name":...}}
    assert_eq!(body["tools"].as_array().unwrap()[0]["name"], "extract_data");
    // Responses API structured output belongs at `text.format` and uses a
    // flat JSON Schema shape. The top-level `response_format.json_schema`
    // envelope is the Chat Completions wire contract.
    assert!(body["response_format"].is_null());
    assert_eq!(
        body["text"]["format"],
        json!({
            "type": "json_schema",
            "name": "response",
            "schema": schema
        })
    );
    assert_eq!(body["reasoning"]["effort"], "medium");
    assert_eq!(body["reasoning"]["strategy"], "default");
    assert_eq!(body["metadata"]["operation"], "analysis");
    assert_eq!(body["metadata"]["trace_id"], "trace-123");
    // Note: temperature/top_p are NOT included when reasoning is set
    // (OpenAI reasoning models like o1, gpt-5, etc. don't support these params)
    assert!(
        body["temperature"].is_null(),
        "temperature should NOT be included when reasoning is set"
    );
    assert!(
        body["top_p"].is_null(),
        "top_p should NOT be included when reasoning is set"
    );
    assert_eq!(body["max_output_tokens"], 200);
    assert_eq!(body["seed"], 99);

    Ok(())
}

#[tokio::test]
async fn openrouter_invocation_sends_expected_headers_and_parses_response(
) -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;

    let response_payload = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "All systems nominal.",
                "tool_calls": [{
                    "id": "call_1",
                    "function": {
                        "name": "verify_state",
                        "arguments": "{\"status\":\"ok\"}"
                    }
                }]
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 15,
            "prompt_tokens_details": {
                "cached_tokens": 9,
                "cache_write_tokens": 0
            },
            "completion_tokens": 6,
            "total_tokens": 21
        }
    });

    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_payload.clone()))
        .mount(&server)
        .await;

    let provider = OpenRouterProvider::with_client(
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap(),
        "router-key",
        format!("{}/api/v1/chat/completions", server.uri()),
    );

    let mut request = LLMRequest::default();
    request.model = "openrouter/anthropic/claude-sonnet-4.5".to_string();
    request.messages = vec![LLMMessage::user("Check current state.")].into();
    request.modality = LLMModality::Text;
    request.tools = vec![sample_tool()].into();
    request.set_response_format(LLMResponseFormat::JsonObject);
    request.reasoning = Some(ReasoningConfig {
        effort: Some("low".to_string()),
        max_reasoning_tokens: None,
        strategy: None,
        summary: None,
    });
    request.metadata = RequestMetadata {
        operation: "action_validation".to_string(),
        trace_id: Some("trace-openrouter".to_string()),
        timeout_secs: Some(15),
        tags: Some(vec![
            "referer:https://magician.local".to_string(),
            "title:Magician Test".to_string(),
        ]),
        trace_context: None,
        provider_attempt_counter: None,
        ..RequestMetadata::default()
    };
    request.temperature = Some(0.4);
    request.max_output_tokens = Some(512);
    request.prompt_cache = Some(PromptCacheConfig::enabled_with_ttl("1h"));
    request.set_extra(json!({
        "seed": 123,
        "headers": {
            "X-Custom-Header": "value"
        }
    }));

    let response = provider.invoke(request).await?;
    assert_eq!(response.text.as_deref(), Some("All systems nominal."));
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].name, "verify_state");
    assert_eq!(response.tool_calls[0].arguments, json!({"status": "ok"}));
    assert_eq!(response.finish_reason.as_deref(), Some("stop"));
    assert_eq!(
        response.usage.as_ref().and_then(|u| u.total_tokens),
        Some(21)
    );
    assert_eq!(
        response.usage.as_ref().and_then(|u| u.cached_tokens),
        Some(9)
    );

    let requests = server
        .received_requests()
        .await
        .ok_or("no requests captured for OpenRouter")?;
    assert_eq!(requests.len(), 1);
    let captured = &requests[0];
    assert_eq!(
        header_value(captured, "authorization"),
        Some("Bearer router-key".to_string())
    );
    assert_eq!(
        header_value(captured, "x-custom-header"),
        Some("value".to_string())
    );
    assert_eq!(
        header_value(captured, "http-referer"),
        Some("https://magician.local".to_string())
    );
    assert_eq!(
        header_value(captured, "x-title"),
        Some("Magician Test".to_string())
    );

    let body: Value = serde_json::from_slice(&captured.body)?;
    assert_eq!(body["model"], "openrouter/anthropic/claude-sonnet-4.5");
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        body["messages"].as_array().unwrap()[0]["content"],
        "Check current state."
    );
    assert_eq!(
        body["tools"].as_array().unwrap()[0]["function"]["name"],
        "extract_data"
    );
    assert_eq!(body["response_format"], json!({ "type": "json_object" }));
    assert_eq!(body["reasoning"]["effort"], "low");
    assert_eq!(body["cache_control"]["type"], "ephemeral");
    assert_eq!(body["cache_control"]["ttl"], "1h");
    assert_eq!(body["seed"], 123);
    assert_eq!(body["headers"]["X-Custom-Header"], "value");

    Ok(())
}

#[tokio::test]
async fn gemini_invocation_supports_cached_content_reference(
) -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/models/gemini-2.5-flash:generateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{
                "content": {
                    "parts": [{ "text": "Gemini cache hit." }],
                    "role": "model"
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 12,
                "cachedContentTokenCount": 100,
                "candidatesTokenCount": 3,
                "totalTokenCount": 15
            }
        })))
        .mount(&server)
        .await;

    let provider = GeminiProvider::with_client(reqwest::Client::new(), "gemini-key", server.uri());

    let mut request = LLMRequest::default();
    request.model = "gemini-2.5-flash".to_string();
    request.messages = vec![LLMMessage::user("Summarize the cached report.")].into();
    request.prompt_cache = Some(PromptCacheConfig::enabled_with_cached_content(
        "cachedContents/research-brief",
    ));

    let response = provider.invoke(request).await?;
    assert_eq!(response.text.as_deref(), Some("Gemini cache hit."));
    assert_eq!(
        response.usage.as_ref().and_then(|u| u.cached_tokens),
        Some(100)
    );

    let requests = server
        .received_requests()
        .await
        .ok_or("no requests captured for Gemini")?;
    let body: Value = serde_json::from_slice(&requests[0].body)?;
    assert_eq!(body["cachedContent"], "cachedContents/research-brief");

    Ok(())
}

#[tokio::test]
async fn openai_chat_and_responses_parity_for_text_and_tools(
) -> Result<(), Box<dyn std::error::Error>> {
    let chat_server = MockServer::start().await;
    let responses_server = MockServer::start().await;

    let chat_payload = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "All systems nominal.",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "extract_data",
                        "arguments": "{\"foo\":\"bar\"}"
                    }
                }]
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 9,
            "prompt_tokens_details": {
                "cached_tokens": 7
            },
            "completion_tokens": 4,
            "total_tokens": 13
        }
    });

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_payload))
        .mount(&chat_server)
        .await;

    let responses_payload = json!({
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [
                { "type": "output_text", "text": "All systems nominal." },
                {
                    "type": "tool_call",
                    "tool_call": {
                        "id": "call_1",
                        "function": {
                            "name": "extract_data",
                            "arguments": "{\"foo\":\"bar\"}"
                        }
                    }
                }
            ]
        }],
        "usage": {
            "prompt_tokens": 9,
            "prompt_tokens_details": {
                "cached_tokens": 7
            },
            "completion_tokens": 4,
            "total_tokens": 13
        },
        "status": "stop"
    });

    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(responses_payload))
        .mount(&responses_server)
        .await;

    let chat_provider = OpenAIChatProvider::with_client(
        reqwest::Client::new(),
        "chat-key",
        format!("{}/v1/chat/completions", chat_server.uri()),
    );
    let responses_provider = OpenAIResponsesProvider::with_client(
        reqwest::Client::new(),
        "responses-key",
        format!("{}/v1/responses", responses_server.uri()),
    );

    let chat_response = chat_provider.invoke(build_text_tool_request()).await?;
    let responses_response = responses_provider.invoke(build_text_tool_request()).await?;

    assert_eq!(chat_response.text, responses_response.text);
    assert_eq!(
        chat_response.tool_calls.len(),
        responses_response.tool_calls.len()
    );
    if let (Some(chat_call), Some(resp_call)) = (
        chat_response.tool_calls.get(0),
        responses_response.tool_calls.get(0),
    ) {
        assert_eq!(chat_call.name, resp_call.name);
        assert_eq!(chat_call.arguments, resp_call.arguments);
    }
    assert_eq!(
        chat_response.finish_reason,
        responses_response.finish_reason
    );
    assert_eq!(
        chat_response
            .usage
            .as_ref()
            .and_then(|usage| usage.total_tokens),
        responses_response
            .usage
            .as_ref()
            .and_then(|usage| usage.total_tokens)
    );
    assert_eq!(
        chat_response
            .usage
            .as_ref()
            .and_then(|usage| usage.cached_tokens),
        responses_response
            .usage
            .as_ref()
            .and_then(|usage| usage.cached_tokens)
    );
    assert_eq!(
        chat_response
            .usage
            .as_ref()
            .and_then(|usage| usage.cached_tokens),
        Some(7)
    );

    Ok(())
}

#[tokio::test]
async fn openai_responses_returns_provider_error_on_429() -> Result<(), Box<dyn std::error::Error>>
{
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": { "message": "Rate limit exceeded" }
        })))
        .mount(&server)
        .await;

    let provider = OpenAIResponsesProvider::with_client(
        reqwest::Client::new(),
        "test-key",
        format!("{}/v1/responses", server.uri()),
    );

    let mut request = LLMRequest::default();
    request.model = "gpt-5.6-terra".to_string();
    request.messages = vec![LLMMessage::user("hello")].into();

    let err = provider
        .invoke(request)
        .await
        .expect_err("expected provider error");
    match err {
        LLMError::Provider { provider, message } => {
            assert_eq!(provider, "openai");
            assert!(message.contains("Rate limit"));
        },
        other => panic!("unexpected error variant: {:?}", other),
    }

    Ok(())
}

#[tokio::test]
async fn openrouter_returns_provider_error_on_503() -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "error": { "message": "Service unavailable" }
        })))
        .mount(&server)
        .await;

    let provider = OpenRouterProvider::with_client(
        reqwest::Client::new(),
        "router-key",
        format!("{}/api/v1/chat/completions", server.uri()),
    );

    let mut request = LLMRequest::default();
    request.model = "openrouter/openai/gpt-5.6-terra".to_string();
    request.messages = vec![LLMMessage::user("status?")].into();

    let err = provider
        .invoke(request)
        .await
        .expect_err("expected provider error");
    assert!(matches!(err, LLMError::Provider { provider, .. } if provider == "openrouter"));

    Ok(())
}

#[tokio::test]
async fn openai_responses_streaming_accumulates_chunks() -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;

    let stream_body = b"data: {\"type\":\"response.output_text.delta\",\"delta\":{\"text\":\"Hello \"}}\n\n\
data: {\"type\":\"response.output_text.delta\",\"delta\":{\"text\":\"world\"}}\n\n\
data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello world\"}]}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}}\n\n\
data: [DONE]\n\n";

    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "text/event-stream")
                .set_body_raw(stream_body.as_ref(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let provider = OpenAIResponsesProvider::with_client(
        reqwest::Client::new(),
        "test-key",
        format!("{}/v1/responses", server.uri()),
    );

    let mut request = LLMRequest::default();
    request.model = "gpt-5.6-terra".to_string();
    request.messages = vec![LLMMessage::user("say hi")].into();
    request.stream = true;

    let response = provider.invoke(request).await?;
    assert_eq!(response.text.as_deref(), Some("Hello world"));
    assert_eq!(
        response.usage.as_ref().and_then(|u| u.total_tokens),
        Some(15)
    );
    assert_eq!(response.finish_reason.as_deref(), Some("completed"));

    Ok(())
}

#[tokio::test]
async fn openai_responses_encodes_image_content_as_base64() -> Result<(), Box<dyn std::error::Error>>
{
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": "done"
                }]
            }]
        })))
        .mount(&server)
        .await;

    let provider = OpenAIResponsesProvider::with_client(
        reqwest::Client::new(),
        "test-key",
        format!("{}/v1/responses", server.uri()),
    );

    let mut request = LLMRequest::default();
    request.model = "gpt-5.6-terra".to_string();
    request.messages = vec![LLMMessage {
        role: magicllm::types::MessageRole::User,
        content: vec![
            ContentBlock::Text {
                text: "Analyze image".to_string(),
            },
            ContentBlock::Image {
                data: vec![0x01, 0x02, 0x03],
                media_type: "image/png".to_string(),
                caption: None,
            },
        ],
    }]
    .into();
    request.modality = LLMModality::Vision;

    provider.invoke(request).await?;

    let requests = server
        .received_requests()
        .await
        .ok_or("no requests captured for base64 test")?;
    let body: Value = serde_json::from_slice(&requests[0].body)?;
    // Image is now sent as input_image type with image_url property containing data URL
    let image_block = &body["input"].as_array().unwrap()[0]["content"]
        .as_array()
        .unwrap()[1];
    assert_eq!(image_block["type"].as_str().unwrap(), "input_image");
    let image_url = image_block["image_url"].as_str().unwrap().to_string();
    let expected_data = base64::engine::general_purpose::STANDARD.encode([0x01, 0x02, 0x03]);
    assert_eq!(
        image_url,
        format!("data:image/png;base64,{}", expected_data)
    );

    Ok(())
}

#[tokio::test]
async fn anthropic_messages_invocation_sends_expected_payload_and_headers(
) -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{
                "type": "text",
                "text": "ack"
            }],
            "usage": {
                "input_tokens": 20,
                "cache_read_input_tokens": 40,
                "cache_creation_input_tokens": 15,
                "output_tokens": 5
            }
        })))
        .mount(&server)
        .await;

    let provider = AnthropicMessagesProvider::with_client(
        reqwest::Client::new(),
        "anthropic-key",
        format!("{}/v1/messages", server.uri()),
    );

    let mut request = LLMRequest::default();
    request.model = "claude-sonnet-4.5".to_string();
    request.messages = vec![
        LLMMessage::system("You are helpful."),
        LLMMessage {
            role: magicllm::types::MessageRole::User,
            content: vec![
                ContentBlock::Text {
                    text: "Describe the image.".to_string(),
                },
                ContentBlock::Image {
                    data: vec![0xFF, 0xD8, 0xFF],
                    media_type: "image/jpeg".to_string(),
                    caption: Some("example".to_string()),
                },
            ],
        },
    ]
    .into();
    request.tools = vec![sample_tool()].into();
    request.reasoning = Some(ReasoningConfig {
        effort: None,
        max_reasoning_tokens: None,
        strategy: Some("concise".to_string()),
        summary: None,
    });
    request.metadata = RequestMetadata {
        operation: "vision_analysis".to_string(),
        trace_id: Some("trace-anthropic".to_string()),
        timeout_secs: Some(30),
        tags: None,
        trace_context: None,
        provider_attempt_counter: None,
        ..RequestMetadata::default()
    };

    let response = provider.invoke(request).await?;
    assert_eq!(response.text.as_deref(), Some("ack"));
    let usage = response.usage.expect("usage should exist");
    assert_eq!(usage.prompt_tokens, Some(75));
    assert_eq!(usage.completion_tokens, Some(5));
    assert_eq!(usage.total_tokens, Some(80));
    assert_eq!(usage.cached_tokens, Some(40));
    assert_eq!(usage.cache_creation_tokens, Some(15));

    let requests = server
        .received_requests()
        .await
        .ok_or("no requests captured for Anthropic")?;
    let captured = &requests[0];
    assert_eq!(
        header_value(captured, "x-api-key"),
        Some("anthropic-key".to_string())
    );
    assert_eq!(
        header_value(captured, "anthropic-version"),
        Some("2023-06-01".to_string())
    );

    let body: Value = serde_json::from_slice(&captured.body)?;
    assert_eq!(body["model"], "claude-sonnet-4.5");
    // System is emitted as an array of content blocks with an explicit
    // cache_control breakpoint on the last block. This anchors the
    // prompt cache at the stable (tools + system) prefix so volatile user
    // messages do not invalidate the cached prefill.
    let system_blocks = body["system"]
        .as_array()
        .expect("system should be emitted as an array of content blocks");
    assert_eq!(system_blocks.len(), 1);
    assert_eq!(system_blocks[0]["type"], "text");
    assert_eq!(system_blocks[0]["text"], "You are helpful.");
    assert_eq!(system_blocks[0]["cache_control"]["type"], "ephemeral");
    // Top-level cache_control must not be set when the system block already
    // carries a breakpoint; duplicating would either land a second cache
    // write or tag a volatile end-of-prompt block.
    assert!(
        body.get("cache_control").is_none(),
        "top-level cache_control should be absent when the system block carries an inline breakpoint"
    );
    let image_block = body["messages"]
        .as_array()
        .unwrap()
        .first()
        .and_then(|entry| entry.get("content"))
        .and_then(Value::as_array)
        .and_then(|content| {
            content
                .iter()
                .find(|item| item.get("type").and_then(Value::as_str) == Some("image"))
        })
        .expect("image block present");
    assert_eq!(
        image_block["source"]["data"],
        Value::String(base64::engine::general_purpose::STANDARD.encode([0xFF, 0xD8, 0xFF]))
    );
    assert_eq!(body["tools"].as_array().unwrap()[0]["name"], "extract_data");
    // Note: Anthropic API only accepts `user_id` in metadata.
    // Custom fields (operation, trace_id, tags) are NOT sent to Anthropic API.
    // They are logged locally via tracing but not included in the API request.
    assert!(
        body["metadata"].is_null(),
        "Anthropic API should NOT include custom metadata fields"
    );

    Ok(())
}

#[tokio::test]
async fn openrouter_anthropic_family_anchors_cache_control_on_system_message(
) -> Result<(), Box<dyn std::error::Error>> {
    // Regression: when the OpenRouter model is an Anthropic-family model AND
    // the request has a system message, the cache_control breakpoint must be
    // attached to the system message's content block so the upstream
    // Anthropic provider caches the stable `tools + system` prefix. A
    // top-level cache_control would be auto-applied by Anthropic to the
    // LAST cacheable block — the volatile user message — which never hits.
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "ok"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 1,
                "total_tokens": 6
            }
        })))
        .mount(&server)
        .await;

    let provider = magicllm::OpenRouterProvider::with_client(
        reqwest::Client::new(),
        "router-key",
        format!("{}/api/v1/chat/completions", server.uri()),
    );

    let mut request = magicllm::LLMRequest::default();
    request.model = "openrouter/anthropic/claude-sonnet-4-6".to_string();
    request.messages = vec![
        magicllm::LLMMessage::system("You are helpful."),
        magicllm::LLMMessage::user("hi"),
    ]
    .into();
    request.prompt_cache = Some(magicllm::types::PromptCacheConfig::enabled());

    provider.invoke(request).await?;

    let requests = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body)?;

    // cache_control must live on the system message's content block, not at
    // top level.
    let system_msg = &body["messages"][0];
    assert_eq!(system_msg["role"], "system");
    let content = system_msg["content"]
        .as_array()
        .expect("system content must be an array when cache anchor is attached");
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "You are helpful.");
    assert_eq!(content[0]["cache_control"]["type"], "ephemeral");
    assert!(
        body.get("cache_control").is_none(),
        "top-level cache_control must be absent when cache is anchored on the system block"
    );

    // The user message stays as the legacy string form.
    assert_eq!(body["messages"][1]["role"], "user");
    assert_eq!(body["messages"][1]["content"], "hi");

    Ok(())
}

#[tokio::test]
async fn openrouter_anthropic_family_splits_user_message_on_cache_sentinel(
) -> Result<(), Box<dyn std::error::Error>> {
    // OpenRouter routing to an Anthropic-family model forwards the request
    // body to Anthropic, so the same two-content-block shape is required.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "index": 0, "message": {"role":"assistant","content":"ok"}, "finish_reason":"stop" }],
            "usage": {"prompt_tokens": 2, "completion_tokens": 1, "total_tokens": 3}
        })))
        .mount(&server)
        .await;

    let provider = magicllm::OpenRouterProvider::with_client(
        reqwest::Client::new(),
        "router-key",
        format!("{}/api/v1/chat/completions", server.uri()),
    );

    let mut request = magicllm::LLMRequest::default();
    request.model = "openrouter/anthropic/claude-sonnet-4-6".to_string();
    request.messages = vec![magicllm::LLMMessage::user(format!(
        "stable-part\n{}\nvolatile-part",
        magicllm::CACHE_BREAKPOINT_SENTINEL
    ))]
    .into();
    request.prompt_cache = Some(magicllm::types::PromptCacheConfig::enabled());

    provider.invoke(request).await?;

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    let user_content = body["messages"][0]["content"]
        .as_array()
        .expect("user content must be an array when splitting on sentinel");
    assert_eq!(user_content.len(), 2);
    assert_eq!(user_content[0]["text"], "stable-part");
    assert_eq!(user_content[0]["cache_control"]["type"], "ephemeral");
    assert_eq!(user_content[1]["text"], "volatile-part");
    assert!(user_content[1].get("cache_control").is_none());

    let body_str = serde_json::to_string(&body)?;
    assert!(!body_str.contains(magicllm::CACHE_BREAKPOINT_SENTINEL));

    Ok(())
}

#[tokio::test]
async fn openrouter_openai_family_strips_sentinel_without_splitting(
) -> Result<(), Box<dyn std::error::Error>> {
    // Non-Anthropic-family OpenRouter models don't get a split — OpenAI's
    // upstream automatic caching handles the reordered-prefix case on its
    // own — but the sentinel must still be stripped.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "index": 0, "message": {"role":"assistant","content":"ok"}, "finish_reason":"stop" }],
            "usage": {"prompt_tokens": 2, "completion_tokens": 1, "total_tokens": 3}
        })))
        .mount(&server)
        .await;

    let provider = magicllm::OpenRouterProvider::with_client(
        reqwest::Client::new(),
        "router-key",
        format!("{}/api/v1/chat/completions", server.uri()),
    );

    let mut request = magicllm::LLMRequest::default();
    request.model = "openrouter/openai/gpt-4o".to_string();
    request.messages = vec![magicllm::LLMMessage::user(format!(
        "stable-part\n{}\nvolatile-part",
        magicllm::CACHE_BREAKPOINT_SENTINEL
    ))]
    .into();
    request.prompt_cache = Some(magicllm::types::PromptCacheConfig::enabled());

    provider.invoke(request).await?;

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    assert_eq!(body["messages"][0]["content"], "stable-part\nvolatile-part");
    let body_str = serde_json::to_string(&body)?;
    assert!(!body_str.contains(magicllm::CACHE_BREAKPOINT_SENTINEL));

    Ok(())
}

#[tokio::test]
async fn anthropic_messages_disables_prompt_cache_leaves_system_as_plain_string(
) -> Result<(), Box<dyn std::error::Error>> {
    // Regression: when the caller explicitly disables prompt caching, neither
    // the system block nor the top-level field should carry a cache_control
    // breakpoint. The system must also stay as a plain string (the pre-cache
    // format) so downstream tooling that expects the legacy shape continues
    // to work.
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{
                "type": "text",
                "text": "ack"
            }],
            "usage": {
                "input_tokens": 1,
                "output_tokens": 1
            }
        })))
        .mount(&server)
        .await;

    let provider = magicllm::AnthropicMessagesProvider::with_client(
        reqwest::Client::new(),
        "anthropic-key",
        format!("{}/v1/messages", server.uri()),
    );

    let mut request = magicllm::LLMRequest::default();
    request.model = "claude-sonnet-4.5".to_string();
    request.messages = vec![
        magicllm::LLMMessage::system("You are helpful."),
        magicllm::LLMMessage {
            role: magicllm::MessageRole::User,
            content: vec![magicllm::ContentBlock::Text {
                text: "hi".to_string(),
            }],
        },
    ]
    .into();
    request.prompt_cache = Some(magicllm::types::PromptCacheConfig::Disabled);

    provider.invoke(request).await?;

    let requests = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body)?;
    assert!(
        body.get("cache_control").is_none(),
        "top-level cache_control must be absent when prompt cache is disabled"
    );
    assert_eq!(
        body["system"], "You are helpful.",
        "system must remain a plain string when prompt cache is disabled"
    );

    Ok(())
}

#[tokio::test]
async fn anthropic_messages_splits_user_message_on_cache_sentinel(
) -> Result<(), Box<dyn std::error::Error>> {
    // Regression: when the rendered user prompt contains the
    // CACHE_BREAKPOINT_SENTINEL and prompt caching is enabled, the Anthropic
    // provider must emit the final user message as two content blocks with
    // `cache_control: {"type":"ephemeral"}` on the first block. The sentinel
    // itself must not appear in either block — Anthropic would otherwise see
    // our internal marker in the prompt.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{ "type": "text", "text": "ack" }],
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        })))
        .mount(&server)
        .await;

    let provider = magicllm::AnthropicMessagesProvider::with_client(
        reqwest::Client::new(),
        "anthropic-key",
        format!("{}/v1/messages", server.uri()),
    );

    let stable = "stable-prefix-tokens-here";
    let volatile = "volatile-suffix-tokens-here";
    let user_text = format!(
        "{stable}\n{}\n{volatile}",
        magicllm::CACHE_BREAKPOINT_SENTINEL
    );

    let mut request = magicllm::LLMRequest::default();
    request.model = "claude-sonnet-4-6".to_string();
    request.messages = vec![magicllm::LLMMessage::user(user_text)].into();
    request.prompt_cache = Some(magicllm::types::PromptCacheConfig::enabled());

    provider.invoke(request).await?;

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    let user_content = body["messages"][0]["content"]
        .as_array()
        .expect("user content must be an array when sentinel splits into two blocks");
    assert_eq!(user_content.len(), 2, "expected exactly two content blocks");
    assert_eq!(user_content[0]["type"], "text");
    assert_eq!(user_content[0]["text"], stable);
    assert_eq!(user_content[0]["cache_control"]["type"], "ephemeral");
    assert_eq!(user_content[1]["type"], "text");
    assert_eq!(user_content[1]["text"], volatile);
    assert!(
        user_content[1].get("cache_control").is_none(),
        "only the first block should carry cache_control"
    );

    // Sentinel must not appear anywhere in the serialised body.
    let body_str = serde_json::to_string(&body)?;
    assert!(
        !body_str.contains(magicllm::CACHE_BREAKPOINT_SENTINEL),
        "sentinel must be stripped from the request body"
    );

    Ok(())
}

#[tokio::test]
async fn anthropic_messages_without_sentinel_keeps_single_user_block(
) -> Result<(), Box<dyn std::error::Error>> {
    // Regression: prompts without the sentinel must pass through unchanged
    // as a single text block (preserves today's wire shape for anything
    // that doesn't opt into the Phase 2 cache breakpoint).
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{ "type": "text", "text": "ack" }],
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        })))
        .mount(&server)
        .await;

    let provider = magicllm::AnthropicMessagesProvider::with_client(
        reqwest::Client::new(),
        "anthropic-key",
        format!("{}/v1/messages", server.uri()),
    );

    let mut request = magicllm::LLMRequest::default();
    request.model = "claude-sonnet-4-6".to_string();
    request.messages = vec![magicllm::LLMMessage::user("plain prompt without sentinel")].into();
    request.prompt_cache = Some(magicllm::types::PromptCacheConfig::enabled());

    provider.invoke(request).await?;

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    // Legacy shape: content is a string, not an array.
    assert_eq!(
        body["messages"][0]["content"],
        "plain prompt without sentinel"
    );

    Ok(())
}

#[tokio::test]
async fn minimax_strips_cache_sentinel_into_single_string() -> Result<(), Box<dyn std::error::Error>>
{
    // Pin the decoupled contract: Minimax now talks to MiniMax's
    // OpenAI-compatible Chat Completions API (`/v1/chat/completions`), NOT the
    // Anthropic Messages wrapper. Like the other OpenAI-style providers it has
    // no explicit cache-breakpoint concept, so a user message carrying the
    // sentinel must collapse into a SINGLE string content (halves rejoined on
    // the original newline) with no `cache_control` and no sentinel leak.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": { "role": "assistant", "content": "ack" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
        })))
        .mount(&server)
        .await;

    let provider = MinimaxProvider::with_base_url(
        "minimax-key",
        format!("{}/v1/chat/completions", server.uri()),
    );

    let stable = "stable-prefix-tokens-here";
    let volatile = "volatile-suffix-tokens-here";
    let user_text = format!(
        "{stable}\n{}\n{volatile}",
        magicllm::CACHE_BREAKPOINT_SENTINEL
    );

    let mut request = LLMRequest::default();
    request.model = "MiniMax-M2.7".to_string();
    request.messages = vec![LLMMessage::user(user_text)].into();
    request.prompt_cache = Some(PromptCacheConfig::enabled());

    provider.invoke(request).await?;

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    // OpenAI-compatible shape: content is a single string with the sentinel
    // stripped and the two halves reconnected on the newline that separated
    // them — not an Anthropic-style content array with cache_control.
    assert_eq!(
        body["messages"][0]["content"],
        json!(format!("{stable}\n{volatile}")),
        "sentinel must collapse to a single newline-joined string"
    );
    assert!(
        body["messages"][0].get("cache_control").is_none(),
        "OpenAI-style providers must not emit cache_control"
    );

    // Sentinel must not leak anywhere in the serialised body.
    let body_str = serde_json::to_string(&body)?;
    assert!(
        !body_str.contains(magicllm::CACHE_BREAKPOINT_SENTINEL),
        "sentinel must be stripped from the Minimax request body"
    );

    Ok(())
}

#[tokio::test]
async fn minimax_fans_out_multi_tool_results_into_separate_tool_messages(
) -> Result<(), Box<dyn std::error::Error>> {
    // Regression for MiniMax error 2013 ("tool call result does not follow tool
    // call"): magician packs a parallel/multi-tool turn into ONE user
    // LLMMessage with N ToolResult blocks. The OpenAI-compatible surface
    // requires every assistant tool_call to be answered by its OWN adjacent
    // role:tool message, so the provider must fan a multi-result user message
    // out into N separate tool messages — in order, each keyed by its own
    // tool_call_id — rather than collapsing them into a single message (which
    // left the other tool_calls unanswered and triggered the 400).
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": { "role": "assistant", "content": "done" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
        })))
        .mount(&server)
        .await;

    let provider = MinimaxProvider::with_base_url(
        "minimax-key",
        format!("{}/v1/chat/completions", server.uri()),
    );

    let mut request = LLMRequest::default();
    request.model = "MiniMax-M3".to_string();
    request.messages = vec![
        LLMMessage::user("do two things".to_string()),
        LLMMessage {
            role: MessageRole::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "I'll click then type".to_string(),
                },
                ContentBlock::ToolCall {
                    id: "call_a".to_string(),
                    name: "click".to_string(),
                    arguments: json!({ "x": 1 }),
                },
                ContentBlock::ToolCall {
                    id: "call_b".to_string(),
                    name: "type".to_string(),
                    arguments: json!({ "text": "hi" }),
                },
            ],
        },
        LLMMessage {
            role: MessageRole::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_call_id: "call_a".to_string(),
                    content: json!("clicked"),
                },
                ContentBlock::ToolResult {
                    tool_call_id: "call_b".to_string(),
                    content: json!("typed"),
                },
            ],
        },
    ]
    .into();

    provider.invoke(request).await?;

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    let msgs = body["messages"].as_array().expect("messages array");

    // Both tool_calls live on one assistant message.
    let asst = msgs
        .iter()
        .position(|m| m["role"] == "assistant" && m.get("tool_calls").is_some())
        .expect("assistant message with tool_calls");
    let tool_calls = msgs[asst]["tool_calls"]
        .as_array()
        .expect("tool_calls array");
    assert_eq!(
        tool_calls.len(),
        2,
        "both tool_calls stay on one assistant message"
    );
    assert_eq!(tool_calls[0]["id"], "call_a");
    assert_eq!(tool_calls[1]["id"], "call_b");

    // It is immediately followed by exactly two role:tool messages, one per id,
    // in order — nothing interleaved.
    assert_eq!(msgs[asst + 1]["role"], "tool");
    assert_eq!(msgs[asst + 1]["tool_call_id"], "call_a");
    assert_eq!(msgs[asst + 1]["content"], "clicked");
    assert_eq!(msgs[asst + 2]["role"], "tool");
    assert_eq!(msgs[asst + 2]["tool_call_id"], "call_b");
    assert_eq!(msgs[asst + 2]["content"], "typed");

    Ok(())
}

#[tokio::test]
async fn minimax_hoists_observation_image_out_of_tool_message(
) -> Result<(), Box<dyn std::error::Error>> {
    // On a vision turn the decision layer appends the screenshot Image to the
    // SAME user message that carries the tool result. A role:tool message can
    // only hold string content, so the provider must keep the result a string
    // role:tool message and hoist the screenshot into a SEPARATE role:user
    // multimodal message after it — never dropping the image, never embedding
    // it in the tool message, and never interleaving it before the result.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": { "role": "assistant", "content": "ok" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
        })))
        .mount(&server)
        .await;

    let provider = MinimaxProvider::with_base_url(
        "minimax-key",
        format!("{}/v1/chat/completions", server.uri()),
    );

    let mut request = LLMRequest::default();
    request.model = "MiniMax-M3".to_string();
    request.messages = vec![
        LLMMessage {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolCall {
                id: "call_a".to_string(),
                name: "screenshot".to_string(),
                arguments: json!({}),
            }],
        },
        LLMMessage {
            role: MessageRole::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_call_id: "call_a".to_string(),
                    content: json!("page text snapshot"),
                },
                ContentBlock::Text {
                    text: "current observation".to_string(),
                },
                ContentBlock::Image {
                    data: vec![0x89, 0x50, 0x4e, 0x47],
                    media_type: "image/png".to_string(),
                    caption: None,
                },
            ],
        },
    ]
    .into();

    provider.invoke(request).await?;

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    let msgs = body["messages"].as_array().expect("messages array");

    let tool_idx = msgs
        .iter()
        .position(|m| m["role"] == "tool")
        .expect("a role:tool message");
    // Tool message holds the result as a STRING and carries no image.
    assert_eq!(msgs[tool_idx]["tool_call_id"], "call_a");
    assert!(
        msgs[tool_idx]["content"].is_string(),
        "tool content must be a plain string"
    );
    let tool_str = serde_json::to_string(&msgs[tool_idx])?;
    assert!(
        !tool_str.contains("image_url"),
        "tool message must not carry an image"
    );

    // The screenshot rides a SEPARATE role:user multimodal message after it.
    let obs = &msgs[tool_idx + 1];
    assert_eq!(
        obs["role"], "user",
        "observation image must ride a user message"
    );
    let parts = obs["content"].as_array().expect("multimodal content array");
    assert!(
        parts.iter().any(|p| p["type"] == "image_url"),
        "screenshot image must be preserved, not dropped"
    );
    assert!(
        parts
            .iter()
            .any(|p| p["type"] == "text" && p["text"] == "current observation"),
        "observation text rides with the image"
    );

    Ok(())
}

#[tokio::test]
async fn minimax_forwards_reasoning_as_native_thinking_toggle(
) -> Result<(), Box<dyn std::error::Error>> {
    // Regression: the profile's `reasoning.effort` must reach M3 as MiniMax's
    // native `thinking: {type: enabled}` switch — the effective lever on this
    // surface (a live probe showed `reasoning_effort` is a no-op for M3). The
    // earlier provider dropped it entirely, so M3 ran at its shallow adaptive
    // default and frequently produced zero reasoning before deciding.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": { "role": "assistant", "content": "ok" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
        })))
        .expect(2)
        .mount(&server)
        .await;

    let provider = MinimaxProvider::with_base_url(
        "minimax-key",
        format!("{}/v1/chat/completions", server.uri()),
    );

    // effort=high (or any non-disabled effort) → thinking enabled.
    let mut enabled = LLMRequest::default();
    enabled.model = "MiniMax-M3".to_string();
    enabled.messages = vec![LLMMessage::user("decide".to_string())].into();
    enabled.reasoning = Some(ReasoningConfig {
        effort: Some("high".to_string()),
        ..Default::default()
    });
    provider.invoke(enabled).await?;

    // effort=none → thinking explicitly disabled.
    let mut disabled = LLMRequest::default();
    disabled.model = "MiniMax-M3".to_string();
    disabled.messages = vec![LLMMessage::user("decide".to_string())].into();
    disabled.reasoning = Some(ReasoningConfig {
        effort: Some("none".to_string()),
        ..Default::default()
    });
    provider.invoke(disabled).await?;

    let reqs = server.received_requests().await.unwrap();
    let body0: Value = serde_json::from_slice(&reqs[0].body)?;
    let body1: Value = serde_json::from_slice(&reqs[1].body)?;
    assert_eq!(
        body0["thinking"],
        json!({ "type": "enabled" }),
        "non-disabled effort must enable M3 thinking"
    );
    assert_eq!(
        body1["thinking"],
        json!({ "type": "disabled" }),
        "effort=none must disable thinking, not drop the signal"
    );
    // `reasoning_effort` must NOT be sent — it is a no-op on M3 and combining it
    // with `thinking` reduces reasoning.
    assert!(body0.get("reasoning_effort").is_none());

    Ok(())
}

#[tokio::test]
async fn anthropic_messages_strips_sentinel_preserves_newline_boundary_when_cache_disabled(
) -> Result<(), Box<dyn std::error::Error>> {
    // Regression: when the sentinel is present but caching is disabled, the
    // Anthropic provider reconnects the halves. The line boundary that
    // separated them in the user's original prompt must be preserved — a
    // naive concat (`format!("{prefix}{suffix}")`) would glue unrelated
    // sections together as a single word, silently corrupting the prompt.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{ "type": "text", "text": "ack" }],
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        })))
        .mount(&server)
        .await;

    let provider = magicllm::AnthropicMessagesProvider::with_client(
        reqwest::Client::new(),
        "anthropic-key",
        format!("{}/v1/messages", server.uri()),
    );

    let mut request = magicllm::LLMRequest::default();
    request.model = "claude-sonnet-4-6".to_string();
    request.messages = vec![magicllm::LLMMessage::user(format!(
        "stable-part\n{}\nvolatile-part",
        magicllm::CACHE_BREAKPOINT_SENTINEL
    ))]
    .into();
    request.prompt_cache = Some(magicllm::types::PromptCacheConfig::Disabled);

    provider.invoke(request).await?;

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    // Single string content (no cache_control), halves reconnected with the
    // preserved newline boundary.
    assert_eq!(body["messages"][0]["content"], "stable-part\nvolatile-part");
    let body_str = serde_json::to_string(&body)?;
    assert!(!body_str.contains(magicllm::CACHE_BREAKPOINT_SENTINEL));
    Ok(())
}

#[tokio::test]
async fn openai_chat_includes_json_schema_in_request() -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "ok"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "total_tokens": 2
            }
        })))
        .mount(&server)
        .await;

    let provider = OpenAIChatProvider::with_client(
        reqwest::Client::new(),
        "chat-key",
        format!("{}/v1/chat/completions", server.uri()),
    );

    let mut request = build_text_tool_request();
    request.set_response_format(LLMResponseFormat::JsonSchema {
        schema: json!({ "type": "object", "properties": { "foo": { "type": "string" }}}),
    });

    provider.invoke(request).await?;

    let requests = server
        .received_requests()
        .await
        .ok_or("no requests captured for chat json schema")?;
    let body: Value = serde_json::from_slice(&requests[0].body)?;
    assert_eq!(body["response_format"]["type"], "json_schema");

    Ok(())
}

#[tokio::test]
async fn openai_chat_returns_provider_error_on_500() -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": { "message": "internal error" }
        })))
        .mount(&server)
        .await;

    let provider = OpenAIChatProvider::with_client(
        reqwest::Client::new(),
        "chat-key",
        format!("{}/v1/chat/completions", server.uri()),
    );

    let err = provider
        .invoke(build_text_tool_request())
        .await
        .expect_err("expected provider error");
    assert!(matches!(err, LLMError::Provider { provider, .. } if provider == "openai"));

    Ok(())
}

#[tokio::test]
async fn openai_chat_strips_cache_sentinel_from_user_message(
) -> Result<(), Box<dyn std::error::Error>> {
    // The OpenAI Chat Completions API has no breakpoint concept (it uses
    // automatic prefix-based caching upstream). When our user prompt carries
    // the Magician cache sentinel, we must strip it before sending so our
    // internal marker never reaches the model, and reconnect the halves with
    // a single `\n` to preserve the line boundary.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": "stop",
                "index": 0
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })))
        .mount(&server)
        .await;

    let provider = OpenAIChatProvider::with_client(
        reqwest::Client::new(),
        "openai-key",
        format!("{}/v1/chat/completions", server.uri()),
    );
    let mut request = LLMRequest::default();
    request.model = "gpt-4o".to_string();
    request.messages = vec![LLMMessage::user(format!(
        "a\n{}\nb",
        magicllm::CACHE_BREAKPOINT_SENTINEL
    ))]
    .into();
    request.set_context_reuse(ContextReuseConfig {
        strategy: ContextReuseStrategy::BoundedReplay,
        continuation_id: Some("must-not-leak".to_string()),
        transport_cohort_fingerprint: Some("transport-must-not-leak".to_string()),
        stable_prefix_fingerprint: Some("fingerprint-must-not-leak".to_string()),
        disclosure_partition_fingerprint: Some("disclosure-must-not-leak".to_string()),
        session_key: Some("session-must-not-leak".to_string()),
        rolling_prefix: false,
    });
    provider.invoke(request).await?;

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    assert_eq!(body["messages"][0]["content"], "a\nb");
    let body_str = serde_json::to_string(&body)?;
    assert!(
        !body_str.contains(magicllm::CACHE_BREAKPOINT_SENTINEL),
        "sentinel must be stripped from the serialised OpenAI Chat request body"
    );
    for local_control in [
        "must-not-leak",
        "transport-must-not-leak",
        "fingerprint-must-not-leak",
        "disclosure-must-not-leak",
        "session-must-not-leak",
    ] {
        assert!(
            !body_str.contains(local_control),
            "typed context control leaked upstream: {local_control}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn openai_responses_strips_cache_sentinel_from_user_message(
) -> Result<(), Box<dyn std::error::Error>> {
    // The OpenAI Responses API also uses upstream automatic caching — no
    // breakpoint concept — but the sentinel must still be stripped so our
    // internal marker never reaches the model. The Responses API wraps user
    // text in `input_text` content blocks inside `input[]`.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "ok" }]
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
            "status": "completed"
        })))
        .mount(&server)
        .await;

    let provider = OpenAIResponsesProvider::with_client(
        reqwest::Client::new(),
        "openai-key",
        format!("{}/v1/responses", server.uri()),
    );
    let mut request = LLMRequest::default();
    request.model = "gpt-5".to_string();
    request.messages = vec![LLMMessage::user(format!(
        "a\n{}\nb",
        magicllm::CACHE_BREAKPOINT_SENTINEL
    ))]
    .into();
    provider.invoke(request).await?;

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    // User message lives in `input[0]`. With the sentinel in the middle, the
    // provider's text-extraction path should have produced a single cleaned
    // input_text block containing `"a\nb"`.
    let user_content = body["input"][0]["content"]
        .as_array()
        .expect("user content is an array of input_text items");
    let text_block = user_content
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("input_text"))
        .expect("at least one input_text block on the user input");
    assert_eq!(text_block["text"], "a\nb");
    let body_str = serde_json::to_string(&body)?;
    assert!(
        !body_str.contains(magicllm::CACHE_BREAKPOINT_SENTINEL),
        "sentinel must be stripped from the serialised OpenAI Responses request body"
    );
    Ok(())
}

#[tokio::test]
async fn gemini_strips_cache_sentinel_from_user_message() -> Result<(), Box<dyn std::error::Error>>
{
    // Gemini uses implicit upstream caching (`cachedContent` references).
    // There's no breakpoint concept on regular `contents[]`, so we just strip
    // the sentinel from user text and reconnect halves with `\n`.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/models/gemini-2.5-flash:generateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{
                "content": {"parts": [{"text": "ok"}], "role": "model"},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1, "totalTokenCount": 2}
        })))
        .mount(&server)
        .await;

    let provider = GeminiProvider::with_client(reqwest::Client::new(), "gemini-key", server.uri());
    let mut request = LLMRequest::default();
    request.model = "gemini-2.5-flash".to_string();
    request.messages = vec![LLMMessage::user(format!(
        "a\n{}\nb",
        magicllm::CACHE_BREAKPOINT_SENTINEL
    ))]
    .into();
    provider.invoke(request).await?;

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    assert_eq!(body["contents"][0]["parts"][0]["text"], "a\nb");
    let body_str = serde_json::to_string(&body)?;
    assert!(
        !body_str.contains(magicllm::CACHE_BREAKPOINT_SENTINEL),
        "sentinel must be stripped from the serialised Gemini request body"
    );
    Ok(())
}

#[tokio::test]
async fn gemini_strips_cache_sentinel_from_tool_response_text(
) -> Result<(), Box<dyn std::error::Error>> {
    // Regression: the tool-role ContentBlock::Text branch in
    // map_tool_response_parts previously bypassed strip_cache_sentinel.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{
                "content": {
                    "parts": [{ "text": "ok" }],
                    "role": "model"
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 1,
                "candidatesTokenCount": 1,
                "totalTokenCount": 2
            }
        })))
        .mount(&server)
        .await;

    let provider = GeminiProvider::with_client(reqwest::Client::new(), "gemini-key", server.uri());

    let mut request = LLMRequest::default();
    request.model = "gemini-2.5-flash".to_string();
    request.messages = vec![
        LLMMessage::user("kick off"),
        LLMMessage {
            role: magicllm::types::MessageRole::Tool,
            content: vec![ContentBlock::Text {
                text: format!("a\n{}\nb", magicllm::CACHE_BREAKPOINT_SENTINEL),
            }],
        },
    ]
    .into();

    provider.invoke(request).await?;

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    let body_str = serde_json::to_string(&body)?;
    assert!(
        !body_str.contains(magicllm::CACHE_BREAKPOINT_SENTINEL),
        "sentinel must be stripped from tool-role text in Gemini request"
    );
    Ok(())
}

#[tokio::test]
async fn ollama_strips_cache_sentinel_from_user_message() -> Result<(), Box<dyn std::error::Error>>
{
    // Ollama doesn't cache prompts at all, but the sentinel must still be
    // stripped so our internal marker never leaks into the flat prompt we
    // send. Ollama's provider joins messages into a single `prompt` string
    // posted to `/api/generate`, so we assert the sentinel is absent from
    // the full serialised body and the concatenated prompt contains the
    // reconnected halves with the newline boundary preserved.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/generate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "response": "ok",
            "prompt_eval_count": 1,
            "eval_count": 1
        })))
        .mount(&server)
        .await;

    let provider = OllamaProvider::with_base_url(format!("{}/api/generate", server.uri()));
    let mut request = LLMRequest::default();
    request.model = "llama3".to_string();
    request.messages = vec![LLMMessage::user(format!(
        "a\n{}\nb",
        magicllm::CACHE_BREAKPOINT_SENTINEL
    ))]
    .into();
    provider.invoke(request).await?;

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    // Ollama joins messages into a single flat `prompt` string with role
    // prefixes ("[user] …"). The cleaned prompt must contain the reconnected
    // "a\nb" halves and must NOT contain the sentinel literal anywhere.
    let prompt = body["prompt"].as_str().expect("prompt is a string");
    assert!(
        prompt.contains("a\nb"),
        "expected reconnected halves in Ollama prompt, got: {prompt:?}"
    );
    let body_str = serde_json::to_string(&body)?;
    assert!(
        !body_str.contains(magicllm::CACHE_BREAKPOINT_SENTINEL),
        "sentinel must be stripped from the serialised Ollama request body"
    );
    assert!(!body_str.contains("must-not-leak"));
    assert!(!body_str.contains("fingerprint-must-not-leak"));
    assert!(!body_str.contains("session-must-not-leak"));
    Ok(())
}

#[tokio::test]
async fn ollama_sends_configured_keep_alive() -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/generate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "response": "ok",
            "prompt_eval_count": 1,
            "eval_count": 1
        })))
        .mount(&server)
        .await;

    let provider = OllamaProvider::with_base_url(format!("{}/api/generate", server.uri()))
        .with_keep_alive(Some("42s".to_string()));
    let mut request = LLMRequest::default();
    request.model = "llama3".to_string();
    request.messages = vec![LLMMessage::user("hello")].into();
    provider.invoke(request).await?;

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    assert_eq!(body["keep_alive"], "42s");
    Ok(())
}

#[tokio::test]
async fn ollama_maps_generation_controls_to_options() -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/generate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "response": "ok",
            "prompt_eval_count": 1,
            "eval_count": 1
        })))
        .mount(&server)
        .await;

    let provider = OllamaProvider::with_base_url(format!("{}/api/generate", server.uri()));
    let mut request = LLMRequest::default();
    request.model = "llama3".to_string();
    request.messages = vec![LLMMessage::user("hello")].into();
    request.max_output_tokens = Some(128);
    request.temperature = Some(0.2);
    provider.invoke(request).await?;

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    assert_eq!(body["options"]["num_predict"], 128);
    let temperature = body["options"]["temperature"].as_f64().unwrap();
    assert!(
        (temperature - 0.2).abs() < 1e-6,
        "temperature should be forwarded via Ollama options, got {temperature}"
    );
    Ok(())
}

#[tokio::test]
async fn ollama_forwards_configured_context_with_generation_controls(
) -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/generate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "response": "ok",
            "prompt_eval_count": 1,
            "eval_count": 1
        })))
        .mount(&server)
        .await;

    let provider = OllamaProvider::with_base_url(format!("{}/api/generate", server.uri()));
    let mut request = LLMRequest::default();
    request.model = "test-model".to_string();
    request.messages = vec![LLMMessage::user("hello")].into();
    request.max_output_tokens = Some(64);
    request.set_extra(json!({ "options": { "num_ctx": 4096 } }));
    provider.invoke(request).await?;

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    assert_eq!(body["options"]["num_ctx"], 4096);
    assert_eq!(body["options"]["num_predict"], 64);
    Ok(())
}

#[tokio::test]
async fn ollama_maps_json_response_format() -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/generate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "response": "{\"ok\":true}",
            "prompt_eval_count": 1,
            "eval_count": 1
        })))
        .mount(&server)
        .await;

    let provider = OllamaProvider::with_base_url(format!("{}/api/generate", server.uri()));
    let mut request = LLMRequest::default();
    request.model = "llama3".to_string();
    request.messages = vec![LLMMessage::user("hello")].into();
    request.set_response_format(LLMResponseFormat::JsonObject);
    provider.invoke(request).await?;

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    assert_eq!(body["format"], "json");
    Ok(())
}

#[tokio::test]
async fn ollama_json_schema_overrides_generic_extra_format(
) -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/generate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "response": "{\"ok\":true}",
            "prompt_eval_count": 1,
            "eval_count": 1
        })))
        .mount(&server)
        .await;

    let provider = OllamaProvider::with_base_url(format!("{}/api/generate", server.uri()));
    let schema = json!({
        "type": "object",
        "properties": {"ok": {"type": "boolean"}},
        "required": ["ok"]
    });
    let mut request = LLMRequest::default();
    request.model = "llama3".to_string();
    request.messages = vec![LLMMessage::user("hello")].into();
    request.set_extra(json!({"format": "json"}));
    request.set_response_format(LLMResponseFormat::JsonSchema {
        schema: schema.clone(),
    });
    provider.invoke(request).await?;

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    assert_eq!(body["format"], schema);
    Ok(())
}

#[tokio::test]
async fn yutori_strips_cache_sentinel_from_user_message() -> Result<(), Box<dyn std::error::Error>>
{
    // Yutori N1 doesn't cache prompts, but the sentinel must still be stripped
    // so our internal marker never leaks into the browser-action model's
    // context. N1 uses the OpenAI Chat Completions wire format.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": "stop",
                "index": 0
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })))
        .mount(&server)
        .await;

    let provider = YutoriN1Provider::with_client(
        reqwest::Client::new(),
        "yutori-key",
        format!("{}/v1/chat/completions", server.uri()),
    );
    let mut request = LLMRequest::default();
    request.model = "n1-latest".to_string();
    request.messages = vec![LLMMessage::user(format!(
        "a\n{}\nb",
        magicllm::CACHE_BREAKPOINT_SENTINEL
    ))]
    .into();
    provider.invoke(request).await?;

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    assert_eq!(body["messages"][0]["content"], "a\nb");
    let body_str = serde_json::to_string(&body)?;
    assert!(
        !body_str.contains(magicllm::CACHE_BREAKPOINT_SENTINEL),
        "sentinel must be stripped from the serialised Yutori N1 request body"
    );
    Ok(())
}

#[tokio::test]
async fn anthropic_messages_splits_multimodal_user_message_on_cache_sentinel(
) -> Result<(), Box<dyn std::error::Error>> {
    // Regression: when the user message has mixed content (text + image,
    // typical for SoM / vision decision calls) AND the text block contains
    // the sentinel AND caching is enabled, the text block must be split
    // into two content blocks with cache_control on the prefix. The image
    // block stays in place, untouched.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{ "type": "text", "text": "ack" }],
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        })))
        .mount(&server)
        .await;

    let provider = magicllm::AnthropicMessagesProvider::with_client(
        reqwest::Client::new(),
        "anthropic-key",
        format!("{}/v1/messages", server.uri()),
    );

    let stable = "stable-prefix";
    let volatile = "volatile-suffix";
    let user_text = format!(
        "{stable}\n{}\n{volatile}",
        magicllm::CACHE_BREAKPOINT_SENTINEL
    );

    let mut request = magicllm::LLMRequest::default();
    request.model = "claude-sonnet-4-6".to_string();
    request.messages = vec![magicllm::LLMMessage {
        role: magicllm::types::MessageRole::User,
        content: vec![
            magicllm::types::ContentBlock::Text { text: user_text },
            magicllm::types::ContentBlock::Image {
                data: vec![0xFF, 0xD8, 0xFF],
                media_type: "image/jpeg".to_string(),
                caption: None,
            },
        ],
    }]
    .into();
    request.prompt_cache = Some(magicllm::types::PromptCacheConfig::enabled());

    provider.invoke(request).await?;

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    let user_content = body["messages"][0]["content"]
        .as_array()
        .expect("user content must be an array");
    // [prefix_text(cache_control), suffix_text, image] — 3 blocks
    assert_eq!(user_content.len(), 3);
    assert_eq!(user_content[0]["type"], "text");
    assert_eq!(user_content[0]["text"], stable);
    assert_eq!(user_content[0]["cache_control"]["type"], "ephemeral");
    assert_eq!(user_content[1]["type"], "text");
    assert_eq!(user_content[1]["text"], volatile);
    assert!(
        user_content[1].get("cache_control").is_none(),
        "only the prefix carries cache_control"
    );
    assert_eq!(user_content[2]["type"], "image");
    // Sentinel must not appear anywhere in the serialised body.
    let body_str = serde_json::to_string(&body)?;
    assert!(
        !body_str.contains(magicllm::CACHE_BREAKPOINT_SENTINEL),
        "sentinel must be stripped from the request body"
    );
    Ok(())
}

/// The old "one moving breakpoint" (a top-level `cache_control` and nothing
/// else) only pays off when every request is the previous request plus
/// appended turns. An agentic decision loop is not that: each iteration ends
/// with a re-rendered observation prompt that replaces the previous one, so
/// the entry cached at the end of request N is never a prefix of request N+1.
/// A Fable 5.1 run wrote 30–50k tokens into the cache on every one of nine
/// decisions and read zero back — $5 in eight iterations. The breakpoints
/// have to sit on the parts that ARE stable: the tools, the system prompt,
/// and the conversation up to the last completed turn.
#[tokio::test]
async fn anthropic_agentic_rolling_prefix_anchors_the_stable_prefix_and_the_last_completed_turn(
) -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{ "type": "text", "text": "ack" }],
            "usage": { "input_tokens": 10, "output_tokens": 1 }
        })))
        .mount(&server)
        .await;

    let provider = AnthropicMessagesProvider::with_client(
        reqwest::Client::new(),
        "anthropic-key",
        format!("{}/v1/messages", server.uri()),
    );
    let mut request = LLMRequest::default();
    request.model = "claude-fable-5-1".to_string();
    request.messages = vec![
        LLMMessage::system("stable system"),
        LLMMessage::user("iteration 1 observation"),
        LLMMessage {
            role: MessageRole::Assistant,
            content: vec![
                ContentBlock::text("looking"),
                ContentBlock::ToolCall {
                    id: "call-1".to_string(),
                    name: "sample_tool".to_string(),
                    arguments: json!({"q": "x"}),
                },
            ],
        },
        LLMMessage {
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_call_id: "call-1".to_string(),
                content: json!({"ok": true}),
            }],
        },
        LLMMessage::user(format!(
            "iteration 2 observation\n{}\nfresh state",
            magicllm::CACHE_BREAKPOINT_SENTINEL
        )),
    ]
    .into();
    request.tools = vec![sample_tool()].into();
    request.prompt_cache = Some(PromptCacheConfig::enabled());
    request.set_context_reuse(ContextReuseConfig {
        strategy: ContextReuseStrategy::PrefixCache,
        continuation_id: None,
        transport_cohort_fingerprint: None,
        stable_prefix_fingerprint: Some("stable-fingerprint".to_string()),
        disclosure_partition_fingerprint: None,
        session_key: Some("execution-session".to_string()),
        rolling_prefix: true,
    });

    provider.invoke(request).await?;
    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;

    // The stable prefix is anchored where it is stable.
    assert!(
        body.get("cache_control").is_none(),
        "no top-level breakpoint: it only ever cached a prefix ending in the volatile prompt"
    );
    assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");

    // The conversation up to the last completed turn is anchored on that turn's
    // last block, so the next iteration — which appends to it — reads it back.
    let messages = body["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 4);
    let boundary = messages[2]["content"]
        .as_array()
        .expect("tool result blocks");
    assert_eq!(
        boundary.last().unwrap()["cache_control"]["type"],
        "ephemeral",
        "the last completed turn carries the moving breakpoint: {}",
        body["messages"]
    );
    for message in &messages[..2] {
        let blocks = message["content"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        assert!(
            blocks
                .iter()
                .all(|block| block.get("cache_control").is_none()),
            "earlier turns carry no breakpoint of their own: {}",
            message
        );
    }
    // The volatile prompt itself is never a cache anchor; its sentinel is
    // stripped, not turned into a fourth breakpoint.
    let last = &messages[3];
    let last_blocks = last["content"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    assert!(
        last["content"].is_string()
            || last_blocks
                .iter()
                .all(|block| block.get("cache_control").is_none()),
        "{last}"
    );
    let encoded = serde_json::to_string(&body)?;
    assert!(!encoded.contains(magicllm::CACHE_BREAKPOINT_SENTINEL));
    assert!(!encoded.contains("stable-fingerprint"));
    assert!(!encoded.contains("execution-session"));
    Ok(())
}

#[tokio::test]
async fn openrouter_agentic_prefix_uses_sticky_session_and_rolling_breakpoint(
) -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": { "role": "assistant", "content": "ack" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 1, "total_tokens": 11 }
        })))
        .mount(&server)
        .await;
    let provider = OpenRouterProvider::with_client(
        reqwest::Client::new(),
        "router-key",
        format!("{}/api/v1/chat/completions", server.uri()),
    );
    let mut request = LLMRequest::default();
    request.model = "anthropic/claude-sonnet-4-6".to_string();
    request.messages = vec![
        LLMMessage::system("stable system"),
        LLMMessage::user(format!(
            "stable task\n{}\nnew observation",
            magicllm::CACHE_BREAKPOINT_SENTINEL
        )),
    ]
    .into();
    request.prompt_cache = Some(PromptCacheConfig::enabled());
    request.set_context_reuse(ContextReuseConfig {
        strategy: ContextReuseStrategy::PrefixCache,
        continuation_id: None,
        transport_cohort_fingerprint: None,
        stable_prefix_fingerprint: Some("local-only".to_string()),
        disclosure_partition_fingerprint: None,
        session_key: Some("magician:test-session".to_string()),
        rolling_prefix: true,
    });

    provider.invoke(request).await?;
    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    assert_eq!(body["session_id"], "magician:test-session");
    assert_eq!(body["cache_control"]["type"], "ephemeral");
    assert_eq!(body["messages"][0]["content"], "stable system");
    assert_eq!(
        body["messages"][1]["content"],
        "stable task\nnew observation"
    );
    let encoded = serde_json::to_string(&body)?;
    assert!(!encoded.contains("local-only"));
    assert!(!encoded.contains(magicllm::CACHE_BREAKPOINT_SENTINEL));
    Ok(())
}

#[tokio::test]
async fn deepseek_uses_automatic_cache_and_reports_hit_miss_usage(
) -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/anthropic/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{ "type": "text", "text": "ack" }],
            "usage": {
                "input_tokens": 999,
                "output_tokens": 7,
                "prompt_cache_hit_tokens": 80,
                "prompt_cache_miss_tokens": 20
            }
        })))
        .mount(&server)
        .await;
    let provider = DeepSeekProvider::with_base_url(
        "deepseek-key",
        format!("{}/anthropic/v1/messages", server.uri()),
    );
    let mut request = LLMRequest::default();
    request.model = "deepseek-v4-pro".to_string();
    request.messages = vec![
        LLMMessage::system("stable system"),
        LLMMessage::user("new observation"),
    ]
    .into();
    request.prompt_cache = Some(PromptCacheConfig::enabled());
    request.set_context_reuse(ContextReuseConfig {
        strategy: ContextReuseStrategy::PrefixCache,
        continuation_id: None,
        transport_cohort_fingerprint: None,
        stable_prefix_fingerprint: Some("local-only".to_string()),
        disclosure_partition_fingerprint: None,
        session_key: None,
        rolling_prefix: true,
    });

    let response = provider.invoke(request).await?;
    let usage = response.usage.expect("DeepSeek usage");
    assert_eq!(usage.prompt_tokens, Some(100));
    assert_eq!(usage.completion_tokens, Some(7));
    assert_eq!(usage.total_tokens, Some(107));
    assert_eq!(usage.cached_tokens, Some(80));
    assert_eq!(usage.cache_creation_tokens, Some(0));
    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    let encoded = serde_json::to_string(&body)?;
    assert!(!encoded.contains("cache_control"));
    assert!(!encoded.contains("local-only"));
    Ok(())
}

#[tokio::test]
async fn gemini_interactions_bootstrap_maps_state_tools_and_usage(
) -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/interactions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "int_first",
            "status": "requires_action",
            "steps": [{
                "type": "function_call",
                "id": "call_lookup_1",
                "name": "extract_data",
                "arguments": { "foo": "bar" }
            }],
            "usage": {
                "total_input_tokens": 25,
                "total_output_tokens": 4,
                "total_thought_tokens": 3,
                "total_cached_tokens": 10,
                "total_tokens": 32
            }
        })))
        .mount(&server)
        .await;
    let provider = GeminiProvider::with_client(
        reqwest::Client::new(),
        "gemini-key",
        format!("{}/v1beta", server.uri()),
    );
    let mut request = LLMRequest::default();
    request.model = "gemini-3.5-flash".to_string();
    request.messages = vec![
        LLMMessage::system("stable system"),
        LLMMessage::user("find the value"),
    ]
    .into();
    request.tools = vec![sample_tool()].into();
    request.temperature = Some(0.2);
    request.max_output_tokens = Some(256);
    request.set_context_reuse(ContextReuseConfig {
        strategy: ContextReuseStrategy::ServerContinuation,
        continuation_id: None,
        transport_cohort_fingerprint: None,
        stable_prefix_fingerprint: Some("local-only".to_string()),
        disclosure_partition_fingerprint: None,
        session_key: None,
        rolling_prefix: false,
    });

    let response = provider.invoke(request).await?;
    assert_eq!(response.response_id.as_deref(), Some("int_first"));
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].name, "extract_data");
    assert_eq!(
        response.tool_calls[0].id,
        "gemini-call-extract_data-0::call_lookup_1"
    );
    let usage = response.usage.expect("Interactions usage");
    assert_eq!(usage.prompt_tokens, Some(25));
    assert_eq!(usage.completion_tokens, Some(4));
    assert_eq!(usage.reasoning_tokens, Some(3));
    assert_eq!(usage.cached_tokens, Some(10));

    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    assert_eq!(body["model"], "gemini-3.5-flash");
    assert_eq!(body["store"], true);
    assert!(body.get("previous_interaction_id").is_none());
    assert_eq!(body["system_instruction"], "stable system");
    assert_eq!(
        body["input"][0],
        json!({ "type": "text", "text": "find the value" })
    );
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["name"], "extract_data");
    let temperature = body["generation_config"]["temperature"]
        .as_f64()
        .expect("temperature");
    assert!((temperature - 0.2).abs() < 1e-6);
    assert_eq!(body["generation_config"]["max_output_tokens"], 256);
    assert_eq!(body["generation_config"]["tool_choice"], "auto");
    assert_eq!(body["generation_config"]["thinking_level"], "low");
    assert!(!serde_json::to_string(&body)?.contains("local-only"));
    Ok(())
}

#[tokio::test]
async fn gemini_interactions_continuation_sends_only_delta_and_resends_controls(
) -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/interactions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "int_second",
            "status": "completed",
            "steps": [{
                "type": "model_output",
                "content": [{ "type": "text", "text": "done" }]
            }],
            "usage": {
                "total_input_tokens": 6,
                "total_output_tokens": 2,
                "total_tokens": 8
            }
        })))
        .mount(&server)
        .await;
    let provider = GeminiProvider::with_client(
        reqwest::Client::new(),
        "gemini-key",
        format!("{}/v1beta", server.uri()),
    );
    let mut request = LLMRequest::default();
    request.model = "gemini-3.6-flash".to_string();
    request.messages = vec![
        LLMMessage::system("stable system"),
        LLMMessage::user("historical question that must not be resent"),
        LLMMessage {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::ToolCall {
                id: "gemini-call-extract_data-0::call_lookup_1".to_string(),
                name: "extract_data".to_string(),
                arguments: json!({ "foo": "bar" }),
            }],
        },
        LLMMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::ToolResult {
                tool_call_id: "gemini-call-extract_data-0::call_lookup_1".to_string(),
                content: json!({ "value": 42 }),
            }],
        },
    ]
    .into();
    request.tools = vec![sample_tool()].into();
    request.max_output_tokens = Some(256);
    request.set_context_reuse(ContextReuseConfig {
        strategy: ContextReuseStrategy::ServerContinuation,
        continuation_id: Some("int_first".to_string()),
        transport_cohort_fingerprint: None,
        stable_prefix_fingerprint: Some("local-only".to_string()),
        disclosure_partition_fingerprint: None,
        session_key: None,
        rolling_prefix: false,
    });

    let response = provider.invoke(request).await?;
    assert_eq!(response.response_id.as_deref(), Some("int_second"));
    assert_eq!(response.text.as_deref(), Some("done"));
    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    assert_eq!(body["previous_interaction_id"], "int_first");
    assert_eq!(body["system_instruction"], "stable system");
    assert_eq!(body["tools"][0]["name"], "extract_data");
    assert_eq!(body["generation_config"]["max_output_tokens"], 256);
    assert_eq!(body["input"].as_array().unwrap().len(), 1);
    assert_eq!(body["input"][0]["type"], "function_result");
    assert_eq!(body["input"][0]["call_id"], "call_lookup_1");
    assert_eq!(body["input"][0]["name"], "extract_data");
    let encoded = serde_json::to_string(&body)?;
    assert!(!encoded.contains("historical question that must not be resent"));
    assert!(!encoded.contains("local-only"));
    Ok(())
}

#[tokio::test]
async fn gemini_interactions_drops_unsafe_continuation_after_history_compaction(
) -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/interactions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "int_rebootstrap",
            "status": "completed",
            "steps": [{
                "type": "model_output",
                "content": [{ "type": "text", "text": "safe" }]
            }]
        })))
        .mount(&server)
        .await;
    let provider = GeminiProvider::with_client(
        reqwest::Client::new(),
        "gemini-key",
        format!("{}/v1beta", server.uri()),
    );
    let mut request = LLMRequest::default();
    request.model = "gemini-3.6-flash".to_string();
    request.messages = vec![
        LLMMessage::system("stable system"),
        LLMMessage::user("compacted state without an assistant boundary"),
        LLMMessage {
            role: MessageRole::User,
            content: vec![
                ContentBlock::text("latest visual state"),
                ContentBlock::Image {
                    data: vec![1, 2, 3],
                    media_type: "image/png".to_string(),
                    caption: None,
                },
            ],
        },
    ]
    .into();
    request.set_context_reuse(ContextReuseConfig {
        strategy: ContextReuseStrategy::ServerContinuation,
        continuation_id: Some("stale_interaction".to_string()),
        transport_cohort_fingerprint: None,
        stable_prefix_fingerprint: None,
        disclosure_partition_fingerprint: None,
        session_key: None,
        rolling_prefix: false,
    });

    provider.invoke(request).await?;
    let body: Value = serde_json::from_slice(&server.received_requests().await.unwrap()[0].body)?;
    assert!(body.get("previous_interaction_id").is_none());
    assert!(body["input"][0]["text"]
        .as_str()
        .unwrap()
        .contains("compacted state without an assistant boundary"));
    assert!(body["input"].as_array().unwrap().iter().any(|block| {
        block.get("type").and_then(Value::as_str) == Some("image")
            && block.get("data").and_then(Value::as_str) == Some("AQID")
    }));
    assert!(!serde_json::to_string(&body)?.contains("stale_interaction"));
    Ok(())
}

#[tokio::test]
async fn gemini_interactions_rejects_nonterminal_synchronous_status(
) -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/interactions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "int_still_running",
            "status": "in_progress",
            "steps": []
        })))
        .mount(&server)
        .await;
    let provider = GeminiProvider::with_client(
        reqwest::Client::new(),
        "gemini-key",
        format!("{}/v1beta", server.uri()),
    );
    let mut request = LLMRequest::default();
    request.model = "gemini-3.6-flash".to_string();
    request.messages = vec![LLMMessage::user("hello")].into();
    request.set_context_reuse(ContextReuseConfig::new(
        ContextReuseStrategy::ServerContinuation,
    ));

    let error = provider
        .invoke(request)
        .await
        .expect_err("synchronous invocation must not accept an in-progress resource");
    assert!(matches!(error, LLMError::Provider { .. }));
    assert!(error.to_string().contains("in_progress"));
    Ok(())
}

// ── Ollama embedding seam ─────────────────────────────────────────────────────

fn embed_request(operation: &str) -> magicllm::types::EmbeddingRequest {
    magicllm::types::EmbeddingRequest {
        model: "pplx-embed".to_string(),
        inputs: vec!["first".to_string(), "second".to_string()],
        truncate: false,
        keep_alive: Some(magicllm::request_keep_alive_value("-1")),
        options: Some(magicllm::types::EmbeddingOptions {
            context_tokens: Some(8192),
            batch_tokens: Some(512),
        }),
        timeout_ms: Some(5_000),
        metadata: RequestMetadata {
            operation: operation.to_string(),
            ..RequestMetadata::default()
        },
    }
}

#[tokio::test]
async fn ollama_embed_sends_expected_body_and_parses_response(
) -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/embed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "embeddings": [[0.1, 0.2], [0.3, 0.4]]
        })))
        .mount(&server)
        .await;

    let provider = OllamaProvider::with_base_url(format!("{}/api/embed", server.uri()));
    let response = provider.embed(embed_request("embed_documents")).await?;

    assert_eq!(response.embeddings.len(), 2);
    assert_eq!(response.embeddings[0], vec![0.1_f32, 0.2_f32]);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body)?;
    // Byte-parity with the pre-migration direct-HTTP callers in
    // magician-vector-index: numeric keep_alive sentinel, fail-closed truncate,
    // options passthrough.
    assert_eq!(body["model"], "pplx-embed");
    assert_eq!(body["input"], json!(["first", "second"]));
    assert_eq!(body["truncate"], false);
    assert_eq!(body["keep_alive"], json!(-1));
    assert_eq!(body["options"]["num_ctx"], 8192);
    assert_eq!(body["options"]["num_batch"], 512);
    Ok(())
}

#[tokio::test]
async fn ollama_embed_endpoint_resolves_from_daemon_root_url_forms(
) -> Result<(), Box<dyn std::error::Error>> {
    // An embedding profile may carry a bare daemon root or a URL already
    // carrying a generation suffix; every form must address {root}/api/embed.
    for suffix in ["", "/api/generate", "/api/embed"] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/embed"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "embeddings": [[1.0]] })),
            )
            .mount(&server)
            .await;
        let provider = OllamaProvider::with_base_url(format!("{}{}", server.uri(), suffix));
        let request = magicllm::types::EmbeddingRequest {
            model: "m".to_string(),
            inputs: vec!["x".to_string()],
            truncate: false,
            keep_alive: None,
            options: None,
            timeout_ms: None,
            metadata: RequestMetadata {
                operation: "embed_query".to_string(),
                ..RequestMetadata::default()
            },
        };
        let response = provider.embed(request).await?;
        assert_eq!(
            response.embeddings,
            vec![vec![1.0_f32]],
            "suffix form {suffix:?} must resolve"
        );
    }
    Ok(())
}

#[tokio::test]
async fn ollama_embed_surfaces_typed_provider_status_for_503_and_400(
) -> Result<(), Box<dyn std::error::Error>> {
    for (http_status, body_text) in [
        (503_u16, "daemon offline".to_string()),
        (400, "{\"error\":\"input too large\"}".to_string()),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/embed"))
            .respond_with(ResponseTemplate::new(http_status).set_body_string(body_text.clone()))
            .mount(&server)
            .await;
        let provider = OllamaProvider::with_base_url(format!("{}/api/embed", server.uri()));
        let error = provider
            .embed(embed_request("embed_documents"))
            .await
            .expect_err("non-2xx must surface as ProviderStatus");
        match &error {
            LLMError::ProviderStatus {
                provider,
                status,
                body,
            } => {
                assert_eq!(provider, "ollama");
                assert_eq!(*status, http_status);
                assert!(
                    body.contains(&body_text),
                    "body must be intact, got: {body}"
                );
            },
            other => panic!("expected ProviderStatus, got: {other:?}"),
        }
        // Retry classification must key off the typed status.
        let class = magicllm::dispatch::classifier::classify(&error);
        match http_status {
            503 => assert!(matches!(class, magicllm::dispatch::ErrorClass::Server5xx)),
            400 => assert!(matches!(class, magicllm::dispatch::ErrorClass::Provider4xx)),
            _ => unreachable!(),
        }
    }
    Ok(())
}

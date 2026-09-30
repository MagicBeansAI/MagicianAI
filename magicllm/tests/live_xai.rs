//! Live check of the xAI provider against `api.x.ai`.
//!
//! These tests make REAL, BILLABLE Grok calls (a few cents in total) and are
//! `#[ignore]`d. Run them on purpose:
//!
//! ```bash
//! set -a; source "$MAGICIAN_ROOT_DIR/.env.development"; set +a
//! cargo test -p magicllm --test live_xai -- --ignored --nocapture
//! ```
//!
//! A missing `XAI_API_KEY` skips (exit 0). Every call goes through the
//! production `XaiProvider`, so what passes here is what a run uses:
//!
//! - xAI accepts the body the xAI dialect builds (it refuses `metadata` and
//!   `effort: none`, which the OpenAI dialect would send);
//! - a forced tool call comes back as a `function_call` Magician can lower;
//! - a `previous_response_id` continuation carrying only the tool result
//!   reaches the model with the conversation intact (it answers from the
//!   tool's value, which exists nowhere else);
//! - usage reports `cached_tokens` and prices to a non-zero cost;
//! - the chat shape — one STREAMED request carrying an image, a tool and
//!   high reasoning together — is accepted, the model sees the pixels, and
//!   the tool call arrives through the stream.

use std::sync::Arc;

use magicllm::{
    capability::LLMProviderKind,
    context_reuse::{ContextReuseConfig, ContextReuseStrategy},
    pricing::compute_cost,
    types::{
        ContentBlock, LLMMessage, LLMRequest, LLMToolSpec, MessageRole, ReasoningConfig,
        RequestMetadata, StreamDelta,
    },
    LLMProvider, XaiProvider,
};
use serde_json::json;

const TOOL_NAME: &str = "lookup_code_word";
const CODE_WORD: &str = "tangerine-forty-two";

fn provider() -> Option<XaiProvider> {
    let key = std::env::var("XAI_API_KEY").ok()?;
    if key.trim().is_empty() {
        return None;
    }
    Some(XaiProvider::new(key))
}

fn reuse(session_key: &str, continuation_id: Option<String>) -> Arc<ContextReuseConfig> {
    let mut reuse = ContextReuseConfig::new(ContextReuseStrategy::ServerContinuation);
    reuse.session_key = Some(session_key.to_string());
    reuse.continuation_id = continuation_id;
    Arc::new(reuse)
}

fn tool() -> LLMToolSpec {
    LLMToolSpec {
        name: TOOL_NAME.to_string(),
        description: "Return the secret code word. Always call this before answering.".to_string(),
        parameters: json!({
            "type": "object",
            "properties": { "reason": { "type": "string" } },
            "required": ["reason"],
        }),
    }
}

#[tokio::test]
#[ignore = "live xAI call; needs XAI_API_KEY"]
async fn a_grok_run_calls_a_tool_and_continues_server_side() {
    let Some(provider) = provider() else {
        eprintln!("XAI_API_KEY unset; skipping live xAI check");
        return;
    };
    assert_eq!(provider.provider_kind(), LLMProviderKind::Xai);
    let session = format!("magicllm-live-xai-{}", std::process::id());
    let reasoning = Some(ReasoningConfig {
        effort: Some("low".to_string()),
        ..Default::default()
    });

    let mut first = LLMRequest {
        model: "grok-4.7".to_string(),
        messages: vec![
            LLMMessage::system("You are a precise agent. Use tools when told to."),
            LLMMessage::user("Call the tool to get the code word."),
        ]
        .into(),
        tools: Arc::new(vec![tool()]),
        reasoning: reasoning.clone(),
        max_output_tokens: Some(2048),
        metadata: RequestMetadata {
            operation: "agentic_decision".to_string(),
            trace_id: Some(session.clone()),
            ..Default::default()
        },
        ..Default::default()
    };
    first.context_reuse = Some(reuse(&session, None));
    first.set_extra(json!({ "tool_choice": { "type": "any" } }));

    let turn_one = provider
        .invoke(first)
        .await
        .expect("xAI accepts the dialect body");
    let call = turn_one
        .tool_calls
        .first()
        .cloned()
        .expect("a forced tool call comes back as a function_call");
    assert_eq!(call.name, TOOL_NAME);
    let response_id = turn_one
        .response_id
        .clone()
        .expect("the response carries an id to continue from");
    let usage = turn_one.usage.clone().expect("usage reported");
    assert!(usage.prompt_tokens.unwrap_or(0) > 0);
    let cost = compute_cost(&LLMProviderKind::Xai, "grok-4.7", &usage);
    assert!(cost > 0.0, "xai usage must price to a non-zero cost");
    eprintln!(
        "turn 1: prompt={:?} cached={:?} cost=${cost:.6}",
        usage.prompt_tokens, usage.cached_tokens
    );

    let mut second = LLMRequest {
        model: "grok-4.7".to_string(),
        messages: vec![
            LLMMessage::system("You are a precise agent. Use tools when told to."),
            LLMMessage::user("Call the tool to get the code word."),
            LLMMessage {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                }],
            },
            LLMMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::ToolResult {
                    tool_call_id: call.id.clone(),
                    content: json!(format!("The code word is {CODE_WORD}.")),
                }],
            },
        ]
        .into(),
        tools: Arc::new(vec![tool()]),
        reasoning,
        max_output_tokens: Some(2048),
        metadata: RequestMetadata {
            operation: "agentic_decision".to_string(),
            trace_id: Some(session.clone()),
            ..Default::default()
        },
        ..Default::default()
    };
    second.context_reuse = Some(reuse(&session, Some(response_id)));
    second.set_extra(json!({ "tool_choice": "auto" }));

    let turn_two = provider
        .invoke(second)
        .await
        .expect("the continuation is accepted");
    let text = turn_two.text.as_deref().unwrap_or_default().to_string();
    eprintln!(
        "turn 2: text={text:?} usage={:?}",
        turn_two
            .usage
            .as_ref()
            .map(|u| (u.prompt_tokens, u.cached_tokens))
    );
    assert!(
        text.contains(CODE_WORD),
        "the model answered from the tool result carried by the continuation: {text:?}"
    );
}

/// 64×64 solid red PNG (RGB 220,30,30).
const RED_SQUARE_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAT0lEQVR42u3PQQkAAAgEsEty/UMZxgi+hcEKLNO+FgEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQGBywLPLIEA68ZURwAAAABJRU5ErkJggg==";

#[tokio::test]
#[ignore = "live xAI call; needs XAI_API_KEY"]
async fn a_streamed_grok_turn_sees_an_image_and_calls_a_tool_with_high_reasoning() {
    use base64::Engine as _;
    let Some(provider) = provider() else {
        eprintln!("XAI_API_KEY unset; skipping live xAI check");
        return;
    };
    let png = base64::engine::general_purpose::STANDARD
        .decode(RED_SQUARE_PNG_BASE64)
        .expect("fixture decodes");
    let colour_tool = LLMToolSpec {
        name: "record_colour".to_string(),
        description: "Record the single dominant colour of the attached image.".to_string(),
        parameters: json!({
            "type": "object",
            "properties": { "colour": { "type": "string", "description": "one lowercase word" } },
            "required": ["colour"],
        }),
    };
    let mut request = LLMRequest {
        model: "grok-4.7".to_string(),
        messages: vec![
            LLMMessage::system("You are a precise assistant. Use the tool to report findings."),
            LLMMessage {
                role: MessageRole::User,
                content: vec![
                    ContentBlock::Text {
                        text: "What colour is this image? Report it with record_colour.".to_string(),
                    },
                    ContentBlock::Image {
                        data: png,
                        media_type: "image/png".to_string(),
                        caption: None,
                    },
                ],
            },
        ]
        .into(),
        tools: Arc::new(vec![colour_tool]),
        reasoning: Some(ReasoningConfig {
            effort: Some("high".to_string()),
            ..Default::default()
        }),
        max_output_tokens: Some(4096),
        metadata: RequestMetadata {
            operation: "chat".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    request.set_extra(json!({ "tool_choice": "auto" }));

    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let stream = tokio::spawn(async move { provider.invoke_stream(request, tx).await });
    let mut streamed_tool_name = None;
    let mut deltas = 0usize;
    let mut done = None;
    while let Some(delta) = rx.recv().await {
        deltas += 1;
        match delta {
            StreamDelta::ToolCallDelta { name: Some(name), .. } => streamed_tool_name = Some(name),
            StreamDelta::Done(response) => done = Some(response),
            _ => {},
        }
    }
    stream
        .await
        .expect("stream task joins")
        .expect("xAI accepts image + tool + high reasoning in one streamed request");
    let response = done.expect("the stream ends with Done");
    let call = response
        .tool_calls
        .first()
        .cloned()
        .expect("the model reports through the tool");
    eprintln!(
        "stream: deltas={deltas} streamed_tool={streamed_tool_name:?} call={} usage={:?}",
        call.arguments,
        response.usage.as_ref().map(|u| (u.prompt_tokens, u.completion_tokens, u.reasoning_tokens))
    );
    assert_eq!(call.name, "record_colour");
    let colour = call.arguments["colour"].as_str().unwrap_or_default().to_ascii_lowercase();
    assert!(colour.contains("red"), "the model saw the pixels: {colour:?}");
}

#[tokio::test]
#[ignore = "live xAI call; needs XAI_API_KEY"]
async fn a_grok_text_answer_streams_as_tokens() {
    let Some(provider) = provider() else {
        eprintln!("XAI_API_KEY unset; skipping live xAI check");
        return;
    };
    let request = LLMRequest {
        model: "grok-4.7".to_string(),
        messages: vec![LLMMessage::user(
            "Count from one to twenty in words, separated by commas.",
        )]
        .into(),
        reasoning: Some(ReasoningConfig {
            effort: Some("low".to_string()),
            ..Default::default()
        }),
        max_output_tokens: Some(1024),
        metadata: RequestMetadata {
            operation: "chat".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(512);
    let stream = tokio::spawn(async move { provider.invoke_stream(request, tx).await });
    let mut tokens = 0usize;
    let mut streamed = String::new();
    while let Some(delta) = rx.recv().await {
        if let StreamDelta::Token(text) = delta {
            tokens += 1;
            streamed.push_str(&text);
        }
    }
    stream.await.expect("stream task joins").expect("the stream completes");
    eprintln!("text stream: {tokens} token deltas, {} chars", streamed.len());
    assert!(tokens > 1, "the answer arrives incrementally, not in one piece");
    assert!(streamed.to_ascii_lowercase().contains("twenty"));
}

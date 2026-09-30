//! Live check of the Sarvam provider against `api.sarvam.ai`.
//!
//! These tests make REAL, BILLABLE Sarvam calls (well under a rupee each) and
//! are `#[ignore]`d. Run them on purpose:
//!
//! ```bash
//! set -a; source "$MAGICIAN_ROOT_DIR/.env.development"; set +a
//! cargo test -p magicllm --test live_sarvam -- --ignored --nocapture
//! ```
//!
//! A missing `SARVAM_API_KEY` skips (exit 0). Every call goes through the
//! production `SarvamProvider`, so what passes here is what a run uses:
//!
//! - Sarvam accepts the body the provider builds (string content,
//!   `max_tokens`, a `low|medium|high` effort, no OpenAI-only fields);
//! - a Hindi prompt forced to call a tool comes back as a tool call, and the
//!   tool-result turn answers from the tool's value, which exists nowhere
//!   else — without echoing prior reasoning back;
//! - usage prices to a non-zero cost;
//! - the chat shape — one STREAMED request with a tool on offer — streams
//!   reasoning deltas before the answer and closes the thinking block.

use std::sync::Arc;

use magicllm::{
    capability::LLMProviderKind,
    pricing::compute_cost,
    types::{
        ContentBlock, LLMMessage, LLMRequest, LLMToolSpec, MessageRole, ReasoningConfig,
        RequestMetadata, StreamDelta,
    },
    LLMProvider, SarvamProvider,
};
use serde_json::json;

const MODEL: &str = "sarvam-105b";
const TOOL_NAME: &str = "lookup_code_word";
const CODE_WORD: &str = "tangerine-forty-two";

fn provider() -> Option<SarvamProvider> {
    let key = std::env::var("SARVAM_API_KEY").ok()?;
    if key.trim().is_empty() {
        return None;
    }
    Some(SarvamProvider::new(key))
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

fn low_effort() -> Option<ReasoningConfig> {
    Some(ReasoningConfig {
        effort: Some("low".to_string()),
        ..Default::default()
    })
}

fn metadata(operation: &str) -> RequestMetadata {
    RequestMetadata {
        operation: operation.to_string(),
        trace_id: Some(format!("magicllm-live-sarvam-{}", std::process::id())),
        ..Default::default()
    }
}

#[tokio::test]
#[ignore = "live Sarvam call; needs SARVAM_API_KEY"]
async fn a_hindi_turn_calls_a_tool_and_answers_from_its_result() {
    let Some(provider) = provider() else {
        eprintln!("SARVAM_API_KEY unset; skipping live Sarvam check");
        return;
    };
    assert_eq!(provider.provider_kind(), LLMProviderKind::Sarvam);
    let system = LLMMessage::system("You are a precise agent. Use tools when told to.");
    let user = LLMMessage::user("कोड वर्ड पाने के लिए टूल को कॉल करें, फिर कोड वर्ड बताएं।");

    let mut first = LLMRequest {
        model: MODEL.to_string(),
        messages: vec![system.clone(), user.clone()].into(),
        tools: Arc::new(vec![tool()]),
        reasoning: low_effort(),
        metadata: metadata("agentic_decision"),
        ..Default::default()
    };
    first.set_extra(json!({ "tool_choice": { "type": "any" } }));

    let turn_one = provider
        .invoke(first)
        .await
        .expect("Sarvam accepts the provider body");
    let call = turn_one
        .tool_calls
        .first()
        .cloned()
        .expect("a forced tool call comes back");
    assert_eq!(call.name, TOOL_NAME);
    assert!(turn_one.reasoning_text.is_some(), "Sarvam always reasons");
    let usage = turn_one.usage.clone().expect("usage reported");
    assert!(usage.prompt_tokens.unwrap_or(0) > 0);
    let cost = compute_cost(&LLMProviderKind::Sarvam, MODEL, &usage);
    assert!(cost > 0.0, "sarvam usage must price to a non-zero cost");
    eprintln!(
        "turn 1: prompt={:?} completion={:?} reasoning={:?} cost=${cost:.6}",
        usage.prompt_tokens, usage.completion_tokens, usage.reasoning_tokens
    );

    let second = LLMRequest {
        model: MODEL.to_string(),
        messages: vec![
            system,
            user,
            LLMMessage {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                }],
            },
            LLMMessage {
                role: MessageRole::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_call_id: call.id.clone(),
                    content: json!({ "code_word": CODE_WORD }),
                }],
            },
        ]
        .into(),
        tools: Arc::new(vec![tool()]),
        reasoning: low_effort(),
        metadata: metadata("agentic_decision"),
        ..Default::default()
    };
    let turn_two = provider
        .invoke(second)
        .await
        .expect("the tool-result turn is accepted");
    let text = turn_two.text.as_deref().unwrap_or_default();
    eprintln!("turn 2: {text}");
    assert!(
        text.contains(CODE_WORD),
        "the answer comes from the tool result: {text}"
    );
}

#[tokio::test]
#[ignore = "live Sarvam call; needs SARVAM_API_KEY"]
async fn a_streamed_chat_turn_thinks_then_answers_in_tamil() {
    let Some(provider) = provider() else {
        eprintln!("SARVAM_API_KEY unset; skipping live Sarvam check");
        return;
    };
    let request = LLMRequest {
        model: MODEL.to_string(),
        messages: vec![LLMMessage::user(
            "Reply with one short Tamil sentence greeting a new customer.",
        )]
        .into(),
        tools: Arc::new(vec![tool()]),
        reasoning: low_effort(),
        metadata: metadata("chat"),
        ..Default::default()
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(4096);
    let stream = tokio::spawn(async move { provider.invoke_stream(request, tx).await });

    let (mut reasoning_started, mut reasoning_ended, mut tokens) = (false, false, 0usize);
    let mut done = None;
    while let Some(delta) = rx.recv().await {
        match delta {
            StreamDelta::ReasoningStart { .. } => reasoning_started = true,
            StreamDelta::ReasoningEnd { .. } => reasoning_ended = true,
            StreamDelta::Token(_) => {
                assert!(reasoning_ended, "the thinking block closes before the answer");
                tokens += 1;
            },
            StreamDelta::Done(response) => done = Some(response),
            _ => {},
        }
    }
    stream
        .await
        .expect("stream task joins")
        .expect("Sarvam accepts the streamed body");

    let response = done.expect("the stream finishes with a Done");
    assert!(reasoning_started && reasoning_ended);
    assert!(tokens > 0, "answer tokens stream");
    let text = response.text.as_deref().unwrap_or_default();
    eprintln!("streamed: {text}");
    assert!(
        text.chars().any(|c| ('\u{0B80}'..='\u{0BFF}').contains(&c)),
        "the answer is in Tamil script: {text}"
    );
    assert_eq!(text, text.trim_start(), "leading blank lines are dropped");
    assert!(response.reasoning_text.is_some());
    assert!(response.usage.is_some(), "the final chunk carries usage");
}

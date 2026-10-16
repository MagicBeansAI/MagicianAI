//! Live eval for every Gemini Live model Magician ships as a realtime engine.
//!
//! These tests open REAL, BILLABLE Gemini Live sessions (a few seconds of
//! audio each) and are `#[ignore]`d. Run them on purpose:
//!
//! ```bash
//! set -a; source "$MAGICIAN_ROOT_DIR/.env.development"; set +a
//! make test-gemini-live-models-live
//! # or
//! cargo test -p magicllm --test live_gemini_live_models -- --ignored --nocapture
//! ```
//!
//! A missing `GEMINI_API_KEY` skips (exit 0). Each model runs through the
//! production `GeminiLiveProvider` — `create_session`, `open_proxied_audio`,
//! `ConfigureSession`, a text turn, a tool round-trip — never a hand-built
//! socket, so what passes here is what a voice call uses. Per model it proves:
//!
//! - Google accepts the setup payload for that model family (a wrong field
//!   such as `thinkingConfig` on 3.8 Live, or `NON_BLOCKING` on 3.1, closes the
//!   socket 1007 before `setupComplete`);
//! - the model calls Magician's tool and then *speaks the tool's answer* — the
//!   code word only the tool knows shows up in the assistant transcript, so
//!   the `functionResponse` shape (with `scheduling` on 3.8) round-tripped;
//! - the extended-thinking model reports `interactionStatus` — `IN_PROGRESS`
//!   while the non-blocking call is open, `IDLE` after the answer; any model
//!   that reports a status ends idle;
//! - usage arrives with a modality split (on the `turnComplete` message, one
//!   step after `generationComplete`) and prices to a non-zero cost.
//!
//! `GEMINI_LIVE_MODELS_EVAL_REPORT=/path/report.json` appends one JSON record
//! per model for the eval report lane.

use std::{
    collections::BTreeMap,
    io::Write as _,
    time::{Duration, Instant},
};

use magicllm::{
    pricing::compute_realtime_cost,
    realtime::{
        gemini_live_model_contract, GeminiLiveProvider, GeminiThinkingLevel,
        GeminiToolResultScheduling, RealtimeAudioControl, RealtimeProvider, RealtimeProviderEvent,
    },
    types::{LLMToolSpec, RealtimeUsage},
};
use serde_json::json;

const CODE_WORD: &str = "tangerine forty-two";
const TOOL_NAME: &str = "lookup_code_word";
const SETUP_DEADLINE: Duration = Duration::from_secs(20);
const TURN_DEADLINE: Duration = Duration::from_secs(60);
const TOOL_LATENCY: Duration = Duration::from_millis(1500);
/// After the answer is heard, Google still sends the answer generation's
/// `turnComplete` + `usageMetadata` (measured ~2 s after its
/// `generationComplete`) and, on extended thinking, the `IDLE` status. Keep
/// listening until the line has been quiet this long so the report carries
/// every generation the provider billed.
const TRAILING_QUIET: Duration = Duration::from_secs(4);
const TRAILING_CAP: Duration = Duration::from_secs(12);

#[derive(Debug, Default, serde::Serialize)]
struct ModelReport {
    model: String,
    thinking_level: Option<String>,
    setup_accepted: bool,
    setup_ms: Option<u64>,
    function_call_seen: bool,
    function_call_ms: Option<u64>,
    /// Every `interaction_status` value in arrival order.
    interaction_statuses: Vec<String>,
    /// Assistant transcripts finalized per generation, in order.
    assistant_transcripts: Vec<String>,
    code_word_spoken: bool,
    audio_bytes: usize,
    response_done_count: usize,
    usage: Option<RealtimeUsage>,
    cost_usd: f64,
    errors: Vec<String>,
    closed: Option<String>,
}

fn code_word_tool() -> LLMToolSpec {
    LLMToolSpec {
        name: TOOL_NAME.to_string(),
        description: "Returns today's secret code word. The code word changes every day and \
                      is only available through this tool; never guess it."
            .to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "reason": { "type": "string", "description": "Why the code word is needed." }
            },
            "required": ["reason"]
        }),
    }
}

async fn drive_model(
    api_key: &str,
    model: &str,
    thinking_level: Option<GeminiThinkingLevel>,
) -> ModelReport {
    let mut report = ModelReport {
        model: model.to_string(),
        thinking_level: thinking_level.map(|level| format!("{level:?}").to_ascii_lowercase()),
        ..ModelReport::default()
    };
    let contract = gemini_live_model_contract(model);

    // Automatic activity detection so a text turn produces a response
    // without Magician's PTT `activityStart`/`activityEnd` envelope.
    let provider = GeminiLiveProvider::new(api_key)
        .with_defaults(model, Some("Kore"))
        .with_profile_overrides(Some("server_vad".to_string()), None, None)
        .with_live_options(thinking_level, GeminiToolResultScheduling::WhenIdle);
    let voice_session_id = format!("live-eval-{}", ulid_like());
    let descriptor = provider
        .create_session("live-eval", "live-eval", &voice_session_id, None, None)
        .await
        .expect("descriptor");
    let mut channel = provider
        .open_proxied_audio(&descriptor)
        .await
        .expect("open Gemini Live socket");

    channel
        .control_tx
        .send(RealtimeAudioControl::ConfigureSession {
            instructions: "You are a terse test assistant. Whenever the user asks for the \
                           code word you MUST call the lookup_code_word tool first and then \
                           say the exact code word it returns, in one short sentence."
                .to_string(),
            tools: vec![code_word_tool()],
            input_transcription_model: None,
            update_id: None,
            defer_response_until_context: false,
        })
        .await
        .expect("configure");

    let started = Instant::now();
    // Phase 1: setup acknowledged. A 1007 close arrives as TransportClosed.
    let setup_deadline = tokio::time::sleep(SETUP_DEADLINE);
    tokio::pin!(setup_deadline);
    loop {
        tokio::select! {
            event = channel.events_rx.recv() => match event {
                Some(RealtimeProviderEvent::SessionConfigured { .. }) => {
                    report.setup_accepted = true;
                    report.setup_ms = Some(started.elapsed().as_millis() as u64);
                    break;
                },
                Some(RealtimeProviderEvent::TransportClosed { message }) => {
                    report.closed = Some(message);
                    return report;
                },
                Some(RealtimeProviderEvent::Error { message, .. }) => {
                    report.errors.push(message);
                },
                Some(_) => {},
                None => {
                    report.closed = Some("event channel closed before setup".to_string());
                    return report;
                },
            },
            _ = &mut setup_deadline => {
                report.errors.push("setupComplete not received within deadline".to_string());
                return report;
            },
        }
    }

    channel
        .control_tx
        .send(RealtimeAudioControl::InjectSystemMessage {
            text: "What is today's code word? Use the tool, then tell me.".to_string(),
            request_response: true,
        })
        .await
        .expect("text turn");

    // Phase 2: tool round-trip and the spoken answer.
    let turn_deadline = tokio::time::sleep(TURN_DEADLINE);
    tokio::pin!(turn_deadline);
    let mut tool_result_sent = false;
    let mut done_after_tool = false;
    loop {
        // The turn is over when the tool result has been spoken back and
        // that generation has closed (its `ResponseDone` carries the usage).
        // On the extended-thinking model the same message carries the
        // `IDLE` status, so waiting for the close covers it too.
        if tool_result_sent && report.code_word_spoken && done_after_tool {
            break;
        }
        tokio::select! {
            frame = channel.downstream_rx.recv() => {
                if let Some(frame) = frame {
                    report.audio_bytes += frame.len();
                }
            },
            event = channel.events_rx.recv() => match event {
                Some(RealtimeProviderEvent::FunctionCall { call_id, name, .. }) => {
                    report.function_call_seen = true;
                    report.function_call_ms = Some(started.elapsed().as_millis() as u64);
                    assert_eq!(name, TOOL_NAME, "{model}: unexpected tool {name}");
                    // A Magician tool takes seconds; give a non-blocking model
                    // the same window so it can finish "let me check…" and
                    // report the interaction still in progress before the
                    // result lands.
                    if contract.async_function_calling {
                        tokio::time::sleep(TOOL_LATENCY).await;
                    }
                    channel
                        .control_tx
                        .send(RealtimeAudioControl::ToolResult {
                            call_id,
                            output: json!({ "code_word": CODE_WORD }).to_string(),
                        })
                        .await
                        .expect("tool result");
                    tool_result_sent = true;
                },
                Some(RealtimeProviderEvent::InteractionStatus { in_progress }) => {
                    report.interaction_statuses.push(
                        if in_progress { "in_progress" } else { "idle" }.to_string(),
                    );
                },
                Some(RealtimeProviderEvent::AssistantTranscriptFinal { text, .. }) => {
                    if text.to_ascii_lowercase().contains("tangerine") {
                        report.code_word_spoken = true;
                    }
                    report.assistant_transcripts.push(text);
                },
                Some(RealtimeProviderEvent::ResponseDone { usage, .. }) => {
                    report.response_done_count += 1;
                    if tool_result_sent {
                        done_after_tool = true;
                    }
                    if let Some(usage) = usage {
                        report.cost_usd += compute_realtime_cost(model, &usage);
                        report.usage = Some(add_usage(report.usage.take(), usage));
                    }
                },
                Some(RealtimeProviderEvent::Error { message, .. }) => {
                    report.errors.push(message);
                },
                Some(RealtimeProviderEvent::TransportClosed { message }) => {
                    report.closed = Some(message);
                    break;
                },
                Some(_) => {},
                None => {
                    report.closed = Some("event channel closed mid-turn".to_string());
                    break;
                },
            },
            _ = &mut turn_deadline => {
                report.errors.push("turn did not finish within deadline".to_string());
                break;
            },
        }
    }

    // Trailing generations: the answer's own close (with its usage) and the
    // idle status arrive after the transcript that satisfied the loop above.
    let trailing_started = Instant::now();
    loop {
        if trailing_started.elapsed() > TRAILING_CAP {
            break;
        }
        tokio::select! {
            frame = channel.downstream_rx.recv() => {
                if let Some(frame) = frame {
                    report.audio_bytes += frame.len();
                }
            },
            event = channel.events_rx.recv() => match event {
                Some(RealtimeProviderEvent::InteractionStatus { in_progress }) => {
                    report.interaction_statuses.push(
                        if in_progress { "in_progress" } else { "idle" }.to_string(),
                    );
                },
                Some(RealtimeProviderEvent::AssistantTranscriptFinal { text, .. }) => {
                    report.assistant_transcripts.push(text);
                },
                Some(RealtimeProviderEvent::ResponseDone { usage, .. }) => {
                    report.response_done_count += 1;
                    if let Some(usage) = usage {
                        report.cost_usd += compute_realtime_cost(model, &usage);
                        report.usage = Some(add_usage(report.usage.take(), usage));
                    }
                },
                Some(RealtimeProviderEvent::Error { message, .. }) => {
                    report.errors.push(message);
                },
                Some(RealtimeProviderEvent::TransportClosed { message }) => {
                    report.closed = Some(message);
                    break;
                },
                Some(_) => {},
                None => break,
            },
            _ = tokio::time::sleep(TRAILING_QUIET) => break,
        }
    }

    let _ = channel.control_tx.send(RealtimeAudioControl::End).await;
    let _ = provider.close_session(&descriptor).await;
    report
}

fn add_usage(sum: Option<RealtimeUsage>, usage: RealtimeUsage) -> RealtimeUsage {
    match sum {
        None => usage,
        Some(sum) => RealtimeUsage {
            text_input_tokens: sum.text_input_tokens + usage.text_input_tokens,
            text_cached_input_tokens: sum.text_cached_input_tokens + usage.text_cached_input_tokens,
            text_output_tokens: sum.text_output_tokens + usage.text_output_tokens,
            audio_input_tokens: sum.audio_input_tokens + usage.audio_input_tokens,
            audio_cached_input_tokens: sum.audio_cached_input_tokens
                + usage.audio_cached_input_tokens,
            audio_output_tokens: sum.audio_output_tokens + usage.audio_output_tokens,
            // Gemini bills by tokens; a clock-billed provider is the only one
            // that reports seconds, and this summer runs per Gemini model.
            billed_seconds: sum.billed_seconds + usage.billed_seconds,
        },
    }
}

fn ulid_like() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{nanos:x}")
}

fn publish(report: &ModelReport) {
    let line = serde_json::to_string(report).expect("report json");
    eprintln!("GEMINI_LIVE_EVAL {line}");
    if let Ok(path) = std::env::var("GEMINI_LIVE_MODELS_EVAL_REPORT") {
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(file, "{line}");
        }
    }
}

fn assert_model_passed(report: &ModelReport) {
    let contract = gemini_live_model_contract(&report.model);
    assert!(
        report.setup_accepted,
        "{}: setup was not accepted — closed={:?} errors={:?}",
        report.model, report.closed, report.errors
    );
    assert!(
        report.function_call_seen,
        "{}: the model never called {TOOL_NAME}; transcripts={:?}",
        report.model, report.assistant_transcripts
    );
    assert!(
        report.code_word_spoken,
        "{}: the tool result was not spoken back; transcripts={:?} errors={:?}",
        report.model, report.assistant_transcripts, report.errors
    );
    assert!(
        report.audio_bytes > 0,
        "{}: no audio came back",
        report.model
    );
    let usage = report
        .usage
        .as_ref()
        .unwrap_or_else(|| panic!("{}: no usage split reported", report.model));
    assert!(
        usage.audio_output_tokens > 0,
        "{}: audio output tokens missing from usage {usage:?}",
        report.model
    );
    assert!(
        report.cost_usd > 0.0,
        "{}: priced to $0 — a pricing row is missing",
        report.model
    );
    if contract.interaction_status {
        // The extended-thinking model acknowledges, runs the call with the
        // interaction open, then answers and goes idle — the sequence the
        // "Working…" client state depends on.
        assert_eq!(
            report.interaction_statuses,
            vec!["in_progress".to_string(), "idle".to_string()],
            "{}: expected one open/close pair across the non-blocking call",
            report.model
        );
    }
    // Any model that reports status at all must end the interaction idle;
    // a model that reports none is fine (3.1 has no field, 3.8 Live sent
    // none as of 2026-09-17 — if it starts, this still holds it to `IDLE`).
    if let Some(last) = report.interaction_statuses.last() {
        assert_eq!(
            last, "idle",
            "{}: interaction did not return to idle: {:?}",
            report.model, report.interaction_statuses
        );
    }
    let thoughts_note = if usage.text_output_tokens > 0 {
        ""
    } else {
        " (no text/thought output tokens)"
    };
    eprintln!(
        "{}: cost ${:.5} for {} audio-in / {} audio-out / {} text-in / {} text+thought-out tokens{thoughts_note}",
        report.model,
        report.cost_usd,
        usage.audio_input_tokens,
        usage.audio_output_tokens,
        usage.text_input_tokens,
        usage.text_output_tokens
    );
}

async fn run(model: &str, thinking_level: Option<GeminiThinkingLevel>) {
    let Ok(api_key) = std::env::var("GEMINI_API_KEY") else {
        eprintln!("SKIP: GEMINI_API_KEY not set");
        return;
    };
    let report = drive_model(&api_key, model, thinking_level).await;
    publish(&report);
    assert_model_passed(&report);
}

#[tokio::test]
#[ignore = "opens a real billable Gemini Live session"]
async fn live_gemini_3_8_live_round_trips_a_non_blocking_tool() {
    run("gemini-3.8-live", None).await;
}

#[tokio::test]
#[ignore = "opens a real billable Gemini Live session"]
async fn live_gemini_3_8_live_extended_thinking_accepts_a_level_and_round_trips_a_tool() {
    run(
        "gemini-3.8-live-extended-thinking",
        Some(GeminiThinkingLevel::Medium),
    )
    .await;
}

#[tokio::test]
#[ignore = "opens a real billable Gemini Live session"]
async fn live_gemini_3_1_flash_live_still_round_trips_a_blocking_tool() {
    run("gemini-3.1-flash-live-preview", None).await;
}

/// One record per model family so the report lane can diff generations.
#[tokio::test]
#[ignore = "opens three real billable Gemini Live sessions"]
async fn live_gemini_live_models_summary() {
    let Ok(api_key) = std::env::var("GEMINI_API_KEY") else {
        eprintln!("SKIP: GEMINI_API_KEY not set");
        return;
    };
    let mut summary = BTreeMap::new();
    for (model, level) in [
        ("gemini-3.8-live", None),
        (
            "gemini-3.8-live-extended-thinking",
            Some(GeminiThinkingLevel::Medium),
        ),
        ("gemini-3.1-flash-live-preview", None),
    ] {
        let report = drive_model(&api_key, model, level).await;
        publish(&report);
        summary.insert(model.to_string(), report);
    }
    let failures = summary
        .values()
        .filter(|report| {
            !(report.setup_accepted && report.function_call_seen && report.code_word_spoken)
        })
        .map(|report| report.model.clone())
        .collect::<Vec<_>>();
    assert!(
        failures.is_empty(),
        "models that did not pass: {failures:?}"
    );
}

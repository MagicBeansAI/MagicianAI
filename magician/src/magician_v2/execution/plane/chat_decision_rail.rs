//! Managed direct-chat loop for every foreign harness. The CLI only proposes;
//! selected work always enters the same grant-scoped dispatcher as normal MCP.
use super::{
    chat_turn::{ChatHarnessSnapshot, ChatHarnessTurnOutcome, ChatHarnessTurnRequest},
    engine::{HarnessStopReason, HarnessTurnSettled, HarnessUsage},
    grant::{
        drain_turn_ledger, plane_grant_registry, PlaneGrant, PlaneTurnStopReason, RevokeGrantOnDrop,
    },
};
use crate::magician_v2::chat::{
    decision_rail::{ChatDecision, ChatDecisionRail},
    models::{ChatLlmTranscriptEntry, StoredToolCall},
};
use anyhow::{anyhow, Result};
use decision_engine_contract::{
    action::{ActionPlan, PlannerMode},
    client::EngineClient,
};
use magicllm::LLMToolSpec;
use serde_json::Value;

pub(crate) async fn run(
    request: &ChatHarnessTurnRequest<'_>,
    snapshot: &ChatHarnessSnapshot,
    engine: &str,
    grant: PlaneGrant,
    client: EngineClient,
) -> ChatHarnessTurnOutcome {
    run_with_planner(
        request,
        snapshot,
        grant,
        client,
        &HarnessPlanner {
            request,
            snapshot,
            engine,
        },
    )
    .await
}

#[async_trait::async_trait]
trait ChatPlanner: Send + Sync {
    async fn propose(
        &self,
        ctx: &crate::magician_v2::execution::agentic::AgenticContext,
        system: &str,
        prompt: &str,
        images: Vec<Value>,
        remaining_seconds: u64,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(Option<ActionPlan>, HarnessTurnSettled)>;
}

struct HarnessPlanner<'a, 'b> {
    request: &'a ChatHarnessTurnRequest<'b>,
    snapshot: &'a ChatHarnessSnapshot,
    engine: &'a str,
}

#[async_trait::async_trait]
impl ChatPlanner for HarnessPlanner<'_, '_> {
    async fn propose(
        &self,
        ctx: &crate::magician_v2::execution::agentic::AgenticContext,
        system: &str,
        prompt: &str,
        images: Vec<Value>,
        remaining_seconds: u64,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(Option<ActionPlan>, HarnessTurnSettled)> {
        let mut remaining = self.snapshot.clone();
        remaining.turn_max_seconds = remaining_seconds;
        super::decision_planner_harness::propose_chat(
            ctx,
            self.engine,
            &remaining,
            self.request.pi_profile.clone(),
            images,
            system,
            prompt,
            cancel,
        )
        .await
    }
}

async fn run_with_planner(
    request: &ChatHarnessTurnRequest<'_>,
    snapshot: &ChatHarnessSnapshot,
    mut grant: PlaneGrant,
    client: EngineClient,
    planner: &dyn ChatPlanner,
) -> ChatHarnessTurnOutcome {
    let turn_cancel = request.cancel.child_token();
    grant.cancellation_token = Some(turn_cancel.child_token());
    let _deadline = TurnDeadline::new(snapshot.turn_max_seconds, turn_cancel.clone());
    let ledger = grant.turn_ledger.clone();
    let token = plane_grant_registry().mint_chat(grant).await;
    let mut lease = RevokeGrantOnDrop::new(token.clone());
    let mut usage = None;
    let mut usage_complete = true;
    let mut trace = magicllm::LlmTraceContext::new(
        magicllm::LlmScope::new(&request.invocation.principal, &request.invocation.workspace),
        magicllm::LlmWorkloadClass::ForegroundChat,
    );
    trace.chat_session_id = request.invocation.chat_session_id.clone();
    trace.chat_turn_id = request.invocation.chat_turn_id.clone();
    trace.execution_id = trace.chat_turn_id.clone();
    if let Some(turn) = &trace.chat_turn_id {
        trace.trace_id = turn.clone();
    }
    let mut rail =
        ChatDecisionRail::for_scope(&request.invocation.principal, &request.invocation.workspace)
            .with_trace(trace)
            .with_agent(request.invocation.target_agent_id.clone());
    let result = run_inner(
        request,
        snapshot,
        &client,
        planner,
        &turn_cancel,
        &token,
        &mut usage,
        &mut usage_complete,
        &mut rail,
    )
    .await;
    plane_grant_registry().revoke(&token).await;
    lease.disarm();
    if result.is_err() {
        usage_complete = false;
    }
    let (assistant_text, stop_reason) = match result {
        Ok(result) => result,
        Err(error) => (
            if turn_cancel.is_cancelled() && !request.cancel.is_cancelled() {
                "The chat turn reached its time limit.".into()
            } else {
                format!("The chat turn stopped: {error}")
            },
            if request.cancel.is_cancelled() {
                HarnessStopReason::Cancelled
            } else if turn_cancel.is_cancelled() {
                HarnessStopReason::TurnBudgetSpent
            } else {
                HarnessStopReason::Refused
            },
        ),
    };
    // Planner JSON and intermediate planner prose are never streamed to users.
    if !request.cancel.is_cancelled() {
        if let Some(tx) = &request.token_sink {
            let _ = tx.try_send(magicllm::StreamDelta::Token(assistant_text.clone()));
        }
    }
    ChatHarnessTurnOutcome {
        decision_model_calls: rail.take_model_calls(),
        settled: HarnessTurnSettled {
            assistant_text,
            stop_reason,
            usage: if usage_complete { usage } else { None },
            native_session_id: None,
        },
        tool_calls: drain_turn_ledger(&ledger),
    }
}

async fn run_inner(
    request: &ChatHarnessTurnRequest<'_>,
    snapshot: &ChatHarnessSnapshot,
    client: &EngineClient,
    planner: &dyn ChatPlanner,
    cancel: &tokio_util::sync::CancellationToken,
    token: &str,
    usage: &mut Option<HarnessUsage>,
    usage_complete: &mut bool,
    rail: &mut ChatDecisionRail,
) -> Result<(String, HarnessStopReason)> {
    let mut history = request.history.to_vec();
    let mut captured_images = Vec::new();
    let started = std::time::Instant::now();
    // The plane also enforces the exact configured work-call budget. This
    // outer bound covers an unlimited grant and allows one final prose round.
    let rounds = if snapshot.turn_max_tool_calls == 0 {
        64
    } else {
        snapshot.turn_max_tool_calls.saturating_add(1)
    };
    for _ in 0..rounds {
        if cancel.is_cancelled() {
            return Err(anyhow!("Chat cancelled"));
        }
        if snapshot.turn_max_seconds > 0 && started.elapsed().as_secs() >= snapshot.turn_max_seconds
        {
            return Ok((
                "The chat turn reached its time limit.".into(),
                HarnessStopReason::TurnBudgetSpent,
            ));
        }
        let grant = plane_grant_registry()
            .resolve(token)
            .await
            .ok_or_else(|| anyhow!("Chat authority was revoked"))?;
        let tools = super::catalog::plane_tools_list(&grant)
            .into_iter()
            .map(|tool| {
                Ok(LLMToolSpec {
                    name: tool["name"]
                        .as_str()
                        .ok_or_else(|| anyhow!("Invalid tool name"))?
                        .into(),
                    description: tool["description"].as_str().unwrap_or("").into(),
                    parameters: tool["inputSchema"].clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let decision = rail
            .prepare_with_client(
                client.clone(),
                request.user_text,
                request.system_prompt,
                &history,
                &tools,
                PlannerMode::ChatHarness,
                cancel,
            )
            .await?;
        let call = match decision {
            ChatDecision::Execute(call) => call,
            ChatDecision::Planner { system, prompt } => {
                let remaining = if snapshot.turn_max_seconds > 0 {
                    snapshot.turn_max_seconds.saturating_sub(started.elapsed().as_secs()).max(1)
                } else { 0 };
                let images = request.pi_images.iter().chain(captured_images.iter()).take(8).cloned().collect();
                let (plan, settled) = planner.propose(&grant.ctx, &system, &prompt, images, remaining, cancel).await?;
                accumulate_usage(usage, usage_complete, settled.usage);
                if !matches!(settled.stop_reason, HarnessStopReason::Settled | HarnessStopReason::TurnBudgetSpent) {
                    let text = if settled.assistant_text.trim().is_empty() {
                        format!("The selected chat harness stopped without a reply ({:?}).", settled.stop_reason)
                    } else { settled.assistant_text };
                    return Ok((text, settled.stop_reason));
                }
                match plan {
                    Some(plan) => rail.resolve(plan, cancel).await?,
                    None => return Ok((if settled.assistant_text.trim().is_empty() { "The chat planner ended without a reply.".into() } else { settled.assistant_text }, settled.stop_reason)),
                }
            },
            // The operation may be disabled by a reload after this loop took
            // ownership. Never replay a partial turn through a second engine.
            ChatDecision::Bypass => return Err(anyhow!("Decision Engine was disabled during this chat turn; send a new message to continue")),
        };
        if cancel.is_cancelled() {
            return Err(anyhow!("Chat cancelled"));
        }
        let result = super::dispatch::plane_tools_call(&grant, &call.tool, &call.arguments).await;
        let call_id = format!("decision-{}", uuid::Uuid::new_v4());
        history.push(ChatLlmTranscriptEntry::AssistantTurn {
            text: None,
            tool_calls: vec![StoredToolCall {
                id: call_id.clone(),
                name: call.tool.clone(),
                arguments: call.arguments,
            }],
            provider_state: None,
        });
        let (content, images) = result_evidence(&result);
        captured_images = images; // Never reuse a screenshot after another work call.
        history.push(ChatLlmTranscriptEntry::ToolResult {
            tool_call_id: call_id,
            tool_name: Some(call.tool),
            content,
        });
        if let Some(stop) = grant.turn_stop_reason() {
            return Ok(match stop {
                PlaneTurnStopReason::NeedsApproval => (
                    "This action needs your approval before it can continue.".into(),
                    HarnessStopReason::NeedsApproval,
                ),
                PlaneTurnStopReason::TurnBudgetSpent => (
                    "The chat turn reached its tool-call limit.".into(),
                    HarnessStopReason::TurnBudgetSpent,
                ),
                PlaneTurnStopReason::Delegate => (
                    "The chat turn requested delegation.".into(),
                    HarnessStopReason::Delegate,
                ),
            });
        }
    }
    Ok((
        "The chat turn reached its tool-call limit.".into(),
        HarnessStopReason::TurnBudgetSpent,
    ))
}

struct TurnDeadline(Option<tokio::task::JoinHandle<()>>);
impl TurnDeadline {
    fn new(seconds: u64, cancel: tokio_util::sync::CancellationToken) -> Self {
        Self((seconds > 0).then(|| {
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
                cancel.cancel();
            })
        }))
    }
}
impl Drop for TurnDeadline {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

fn result_evidence(result: &Value) -> (String, Vec<Value>) {
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| {
                    (block["type"] == "text")
                        .then(|| block["text"].as_str())
                        .flatten()
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| result.to_string());
    let mut value =
        serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"text":text}));
    let mut images = Vec::new();
    let mut image_bytes = 0;
    capture_images(&mut value, &mut images, &mut image_bytes, 0);
    if let Some(blocks) = result.get("content").and_then(Value::as_array) {
        for block in blocks.iter().filter(|block| block["type"] == "image") {
            capture_images(&mut block.clone(), &mut images, &mut image_bytes, 0);
        }
    }
    let value = crate::magician_v2::secrets::sanitize_json_for_provider(&value);
    let mut value = if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > 64 * 1024) {
        serde_json::json!({"evidence_unavailable":"Result exceeds decision bound; read a bounded result before continuing"})
    } else {
        value
    };
    if result["isError"] == true {
        if let Some(fields) = value.as_object_mut() {
            fields.insert("isError".into(), Value::Bool(true));
        } else {
            value = serde_json::json!({"isError":true,"result":value});
        }
    }
    (value.to_string(), images)
}

// Standard MCP image blocks are transport data, independent of the work tool.
// Never follow arbitrary file paths or URLs returned by a tool.
fn capture_images(value: &mut Value, images: &mut Vec<Value>, bytes: &mut usize, depth: usize) {
    if depth > 24 {
        return;
    }
    if value["type"] == "image" && value.get("data").is_some() {
        if let Some(data) = value["data"].as_str() {
            if images.len() < 8
                && bytes.saturating_add(data.len()) <= 12 * 1024 * 1024
                && matches!(
                    value["mimeType"].as_str(),
                    Some("image/png" | "image/jpeg" | "image/webp" | "image/gif")
                )
            {
                *bytes += data.len();
                images.push(value.clone());
            }
        }
        if let Some(fields) = value.as_object_mut() {
            fields.remove("data");
            fields.insert(
                "note".into(),
                Value::String(
                    "Captured image is available to the planner through its image channel".into(),
                ),
            );
        }
        return;
    }
    match value {
        Value::Object(fields) => {
            for child in fields.values_mut() {
                capture_images(child, images, bytes, depth + 1);
            }
        },
        Value::Array(values) => {
            for child in values {
                capture_images(child, images, bytes, depth + 1);
            }
        },
        _ => {},
    }
}

fn accumulate_usage(
    total: &mut Option<HarnessUsage>,
    complete: &mut bool,
    usage: Option<HarnessUsage>,
) {
    let Some(usage) = usage else {
        *complete = false;
        return;
    };
    match total {
        Some(total) => total.accumulate(usage),
        None => *total = Some(usage),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chat_decision_rail_preserves_mcp_errors_and_bounded_evidence() {
        let result =
            serde_json::json!({"isError":true,"content":[{"type":"text","text":"denied"}]});
        let value: Value = serde_json::from_str(&result_evidence(&result).0).unwrap();
        assert_eq!(value["isError"], true);
        let result = serde_json::json!({"isError":false,"content":[{"type":"text","text":"x".repeat(70 * 1024)}]});
        assert!(result_evidence(&result).0.contains("evidence_unavailable"));
    }
    #[test]
    fn chat_decision_rail_does_not_invent_missing_planner_usage() {
        let mut total = None;
        let mut complete = true;
        accumulate_usage(
            &mut total,
            &mut complete,
            Some(HarnessUsage {
                input_tokens: 5,
                output_tokens: 3,
                cached_input_tokens: 2,
                ..Default::default()
            }),
        );
        accumulate_usage(&mut total, &mut complete, None);
        accumulate_usage(
            &mut total,
            &mut complete,
            Some(HarnessUsage {
                input_tokens: 7,
                output_tokens: 2,
                cached_input_tokens: 1,
                ..Default::default()
            }),
        );
        assert!(!complete);
        assert_eq!(total.unwrap().input_tokens, 12);
    }
}

#[cfg(test)]
mod image_tests {
    use super::*;
    #[test]
    fn chat_decision_rail_moves_generic_mcp_images_out_of_decision_text() {
        let image = serde_json::json!({"type":"image","mimeType":"image/png","data":"aGVsbG8="});
        let result = serde_json::json!({"isError":false,"content":[{"type":"text","text":serde_json::json!({"result":{"content":[image.clone()]}}).to_string()}]});
        let (text, images) = result_evidence(&result);
        assert_eq!(images, vec![image]);
        assert!(!text.contains("aGVsbG8="));
        assert!(text.contains("image channel"));
    }
}

#[cfg(test)]
#[path = "chat_decision_rail_tests.rs"]
mod loop_tests;

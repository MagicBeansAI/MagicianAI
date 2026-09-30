//! Restricted harness planner: the only Magician tool is a proposal collector.
//! The grant has no executors. Every proposed work call must be selected by the
//! Decision Engine and executed later through the ordinary host loop.

use anyhow::{anyhow, Result};
use decision_engine_contract::action::{
    planner_schema, ActionPlan, PLANNER_IMAGE_TOOL, PLANNER_TOOL,
};
use magicllm::LLMToolSpec;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use super::decision_planner::PlannerOutput;
use super::engine::{
    HarnessSessionRequest, HarnessStreamSink, HarnessTurnInput, NativeToolPosture, PlaneEndpoint,
};
use super::grant::{plane_grant_registry, PlaneGrant, RevokeGrantOnDrop};
use super::mouth_bridge::ChatMouthBridge;
use super::turn_engine::{harness_engine_snapshot, run_engine_for, settings_pin};
use crate::magician_v2::execution::agentic::types::{account_execution_tokens, AgenticContext};
use crate::magician_v2::execution::agentic::ActionExecutors;

fn collect_bridge(
    plan: Arc<Mutex<Option<ActionPlan>>>,
    images: Arc<Vec<Value>>,
) -> ChatMouthBridge {
    Arc::new(move |_id, name, arguments, cancel| {
        let plan = Arc::clone(&plan);
        let images = Arc::clone(&images);
        Box::pin(async move {
            if !cancel.is_cancelled() && name == PLANNER_IMAGE_TOOL {
                return json!({"status":"ok","planner_images": &*images});
            }
            if cancel.is_cancelled() || name != PLANNER_TOOL {
                return json!({"status":"error","error":"proposal unavailable"});
            }
            let parsed: ActionPlan = match parse_proposal(arguments) {
                Ok(plan) => plan,
                Err(_) => return json!({"status":"error","error":"invalid action plan"}),
            };
            if parsed.steps.is_empty() || parsed.steps.len() > 32 {
                return json!({"status":"error","error":"a proposal needs 1 to 32 steps"});
            }
            let Ok(mut slot) = plan.lock() else {
                return json!({"status":"error","error":"proposal collector unavailable"});
            };
            if slot.is_some() {
                return json!({"status":"error","error":"a proposal has already been submitted"});
            }
            *slot = Some(parsed);
            json!({"status":"ok","message":"Proposal recorded. No work call was executed. End this planner turn."})
        })
    })
}

pub(crate) fn planner_grant(
    ctx: &AgenticContext,
    captured: Arc<Mutex<Option<ActionPlan>>>,
    images: Vec<Value>,
) -> PlaneGrant {
    let mut context = ctx.clone();
    // This grant adds one internal data-return channel, with no work authority.
    let mut names = vec![PLANNER_TOOL.to_string()];
    if !images.is_empty() {
        names.push(PLANNER_IMAGE_TOOL.into());
    }
    context.plane_allowed_capability_names = Some(names.clone());
    let mut specs = BTreeMap::from([(PLANNER_TOOL.into(), LLMToolSpec {
        name: PLANNER_TOOL.into(),
        description: "Submit a proposed action plan; no tool call in it is executed. End the turn after submission.".into(),
        parameters: planner_schema(),
    })]);
    if !images.is_empty() {
        specs.insert(PLANNER_IMAGE_TOOL.into(), LLMToolSpec {
            name: PLANNER_IMAGE_TOOL.into(),
            description: "Read the already captured images for this decision. This returns existing evidence and executes no work tool.".into(),
            parameters: json!({"type":"object","properties":{},"additionalProperties":false}),
        });
    }
    let budget = names.len() as u32;
    let grant = PlaneGrant::for_terminal(
        context,
        format!("decision-planner-{}", uuid::Uuid::new_v4()),
        names,
        Arc::new(crate::magician_v2::execution::flat_loop::ToolIndex::from_entries(Vec::new())),
    )
    .with_bridged_tools(specs, collect_bridge(captured, Arc::new(images)))
    .with_turn_tool_budget(budget, 0);
    debug_assert!(grant.executors.is_none());
    grant
}

/// Only the internal, grant-scoped evidence bridge may return raw MCP images.
/// Arbitrary work-tool JSON cannot opt itself into this transport.
pub(crate) fn planner_image_content(tool: &str, value: &Value) -> Option<Value> {
    if tool != PLANNER_IMAGE_TOOL {
        return None;
    }
    let invalid = || json!({"content":[{"type":"text","text":"Captured images are unavailable or exceed the transport limit."}],"isError":true});
    let Some(images) = value.get("planner_images").and_then(Value::as_array) else {
        return Some(invalid());
    };
    if value["status"] != "ok" || images.len() > 8 {
        return Some(invalid());
    }
    let mut bytes = 0usize;
    for image in images {
        if image["type"] != "image"
            || !matches!(
                image["mimeType"].as_str(),
                Some("image/png" | "image/jpeg" | "image/webp" | "image/gif")
            )
        {
            return Some(invalid());
        }
        let Some(data) = image["data"].as_str() else {
            return Some(invalid());
        };
        bytes = bytes.saturating_add(data.len());
        if bytes > 12 * 1024 * 1024 {
            return Some(invalid());
        }
    }
    Some(json!({"content":images,"isError":false}))
}

fn parse_proposal(mut value: Value) -> Result<ActionPlan> {
    // CLI replies can add presentation labels or a top-level explanation.
    // Ignore only string metadata, never fields inside proposed work calls.
    if let Some(fields) = value.as_object_mut() {
        for key in ["toolAction", "toolSummary", "reason"] {
            if fields.remove(key).is_some_and(|value| !value.is_string()) {
                return Err(anyhow!("invalid planner presentation metadata"));
            }
        }
    }
    serde_json::from_value(value).map_err(|_| anyhow!("planner did not return an action plan"))
}

fn parse_text(text: &str) -> Result<ActionPlan> {
    let text = text.trim();
    let text = if let Some(fenced) = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
    {
        fenced
            .trim()
            .strip_suffix("```")
            .ok_or_else(|| anyhow!("unterminated planner JSON fence"))?
            .trim()
    } else {
        text
    };
    let value =
        serde_json::from_str(text).map_err(|_| anyhow!("planner did not return an action plan"))?;
    parse_proposal(value)
}

pub async fn propose(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    system: &str,
    prompt: &str,
    images: Vec<crate::magician_v2::slot_graph::extraction::ImageData>,
    cancel: Option<&CancellationToken>,
) -> Result<PlannerOutput> {
    let started_at_ms = chrono::Utc::now().timestamp_millis();
    let name = run_engine_for(ctx);
    let engine = super::engines::harness_engine_for(&name)
        .ok_or_else(|| anyhow!("unknown planner engine {name}"))?;
    if engine.capabilities().native_tool_posture == NativeToolPosture::Live {
        return Err(anyhow!(
            "planner engine {name} does not constrain native tools"
        ));
    }
    let snapshot = harness_engine_snapshot();
    let pin = ctx
        .run_engine_pin
        .as_ref()
        .filter(|pin| pin.engine == name)
        .cloned()
        .unwrap_or_else(|| {
            super::engine_pin::RunEnginePin::for_engine(&name, &settings_pin(&snapshot))
        });
    let pi_profile = if name == "pi" {
        if let Some(profile) = pin.pi_profile.as_deref() {
            let router = executors.operation_llm_router.as_ref()
                .and_then(|router|router.router_config_snapshot())
                .or_else(||crate::magician_v2::query_analysis::operation_llm_router::global_operation_router()
                    .and_then(|router|router.router_config_snapshot()))
                .ok_or_else(||anyhow!("Pi planner profile requires a configured router"))?;
            Some(
                crate::magician_v2::query_analysis::multi_llm_service::resolve_pi_profile_config(
                    &router, profile,
                )?,
            )
        } else {
            None
        }
    } else {
        None
    };
    let captured = Arc::new(Mutex::new(None));
    let images: Vec<Value> = images
        .into_iter()
        .map(|image| {
            json!({
                "type":"image", "data":image.base64, "mimeType":image.media_type
            })
        })
        .collect();
    let has_images = !images.is_empty();
    let mut grant = planner_grant(ctx, Arc::clone(&captured), images.clone());
    if let Some(cancel) = cancel {
        grant.cancellation_token = Some(cancel.child_token());
    }
    let token = plane_grant_registry().mint(grant).await;
    let mut lease = RevokeGrantOnDrop::new(token.clone());
    let bound = if snapshot.turn_max_seconds == 0 {
        300
    } else {
        snapshot.turn_max_seconds.min(300)
    };
    let request = HarnessSessionRequest {
        planning_only: true,
        endpoint: PlaneEndpoint {
            url: snapshot.plane_endpoint,
        },
        grant: token.clone(),
        system_prompt: system.into(),
        model: Some(pin.harness_model.clone()).filter(|name| name != "default"),
        pi_profile,
        pi_images: if name == "pi" { images } else { Vec::new() },
        cwd: std::env::current_dir()?,
        env_allowlist: Vec::new(),
        cancel: cancel.cloned(),
        resume_session_id: None,
        turn_timeout: Duration::from_secs(bound),
        turn_idle_timeout: Some(Duration::from_secs(60)),
        native_home: None,
    };
    let mut session = engine.start(&request).await.map_err(|error| {
        report_harness_health(
            ctx,
            &request,
            &name,
            crate::magician_v2::realtime_events::ServiceFailure::from_error(&error.to_string())
                .map(Err),
        );
        error
    })?;
    let image_hint = if has_images && name != "pi" {
        format!("\nThe host captured images for this decision. Call {PLANNER_IMAGE_TOOL} to see them before proposing a call that depends on vision.")
    } else {
        String::new()
    };
    let input = HarnessTurnInput {
        text:format!("{prompt}\n\nSubmit the plan through {PLANNER_TOOL}, or return only its JSON. No work tools are available on this planner grant.{image_hint}"),
        operator_steer:Vec::new(),
    };
    let outcome = session.turn(&input, &HarnessStreamSink::drain()).await;
    session.shutdown().await;
    plane_grant_registry().revoke(&token).await;
    lease.disarm();
    let outcome = outcome.map_err(|error| {
        report_harness_health(
            ctx,
            &request,
            &name,
            crate::magician_v2::realtime_events::ServiceFailure::from_error(&error.to_string())
                .map(Err),
        );
        error
    })?;
    report_harness_health(ctx, &request, &name, session.service_health(&outcome));
    // Preserve CLI-reported buckets and prices, while keeping unreported
    // fields and the number of physical provider attempts explicitly unknown.
    let mut metadata = crate::magician_v2::execution::agentic::native_integration::NativeDecisionMetadata::synthetic(
        "agentic_decision_planner_harness", "Harness returned an action-plan proposal",
    );
    let mut trace = magicllm::LlmTraceContext::new(
        magicllm::LlmScope::new(
            ctx.principal.clone().unwrap_or_else(|| "anonymous".into()),
            ctx.workspace.clone().unwrap_or_else(|| "default".into()),
        ),
        magicllm::LlmWorkloadClass::AutonomousTask,
    );
    trace.execution_id = ctx.execution_id.clone();
    trace.root_execution_id = ctx.root_execution_id.clone();
    trace.task_id = ctx.task_id.clone();
    trace.plan_id = ctx.plan_id.clone();
    trace.step_id = ctx.step_id.clone();
    let input_tokens = outcome
        .usage
        .as_ref()
        .map(|u| u.input_tokens.min(u32::MAX as u64) as u32);
    let output_tokens = outcome
        .usage
        .as_ref()
        .map(|u| u.output_tokens.min(u32::MAX as u64) as u32);
    if let Some(turn) = metadata.assistant_turn.as_mut() {
        turn.prompt_tokens = input_tokens;
        turn.completion_tokens = output_tokens;
        turn.llm_trace_context = Some(trace.clone());
    }
    metadata.telemetry = Some(
        crate::magician_v2::slot_graph::extraction::LlmCallTelemetry {
            provider: format!("harness:{name}"),
            model: outcome
                .usage
                .as_ref()
                .and_then(|u| u.model.clone())
                .unwrap_or(pin.harness_model),
            usage_reported: outcome.usage.is_some(),
            usage_availability: Some(
                outcome
                    .usage
                    .as_ref()
                    .map(|u| u.availability())
                    .unwrap_or_default(),
            ),
            cache_read_tokens: outcome
                .usage
                .as_ref()
                .map(|u| u.cached_input_tokens.min(u32::MAX as u64) as u32)
                .unwrap_or(0),
            cache_creation_tokens: outcome
                .usage
                .as_ref()
                .and_then(|u| u.cache_creation_tokens)
                .map(|n| n.min(u32::MAX as u64) as u32)
                .unwrap_or(0),
            cost_usd: outcome
                .usage
                .as_ref()
                .and_then(|u| u.cost_usd)
                .unwrap_or(0.0),
            input_tokens: input_tokens.unwrap_or(0),
            output_tokens: output_tokens.unwrap_or(0),
            started_at_ms,
            operation: Some("agentic_decision".into()),
            profile: (name == "pi").then_some(pin.pi_profile).flatten(),
            trace_receipt: Some(magicllm::LlmTraceReceipt::direct_with_attempt_count(
                trace, 0,
            )),
            ..Default::default()
        },
    );
    if let Some(usage) = outcome.usage.as_ref() {
        account_execution_tokens(usage.input_tokens.saturating_add(usage.output_tokens)).map_err(
            |error| {
                super::decision_planner::response_error(
                    error.into(),
                    "planner_token_budget",
                    true,
                    metadata.telemetry.clone(),
                )
            },
        )?;
    } else {
        crate::magician_v2::execution::agentic::types::exhaust_execution_token_budget_for_missing_usage()
            .map_err(|error| super::decision_planner::response_error(error.into(), "planner_usage_missing", true, metadata.telemetry.clone()))?;
    }
    if matches!(
        outcome.stop_reason,
        super::engine::HarnessStopReason::Cancelled
            | super::engine::HarnessStopReason::Refused
            | super::engine::HarnessStopReason::Stalled
            | super::engine::HarnessStopReason::TurnBudgetSpent
    ) || cancel.is_some_and(CancellationToken::is_cancelled)
    {
        return Err(super::decision_planner::response_error(
            anyhow!("planner {name} stopped: {:?}", outcome.stop_reason),
            "planner_stopped",
            false,
            metadata.telemetry.clone(),
        ));
    }
    let plan = captured
        .lock()
        .map_err(|_| anyhow!("planner collector poisoned"))?
        .take();
    let plan = match plan {
        Some(plan) => plan,
        None => parse_text(&outcome.assistant_text).map_err(|error| {
            let sanitized =
                crate::magician_v2::secrets::sanitize_text_for_provider(&outcome.assistant_text);
            let excerpt: String = sanitized.chars().take(512).collect();
            super::decision_planner::response_error(
                anyhow!(
                    "planner {name}: {error}; stop={:?}; reply={excerpt:?}",
                    outcome.stop_reason
                ),
                "invalid_action_plan",
                true,
                metadata.telemetry.clone(),
            )
        })?,
    };
    tracing::info!(engine=%name,steps=plan.steps.len(),
        input_tokens=outcome.usage.as_ref().map(|usage|usage.input_tokens),
        output_tokens=outcome.usage.as_ref().map(|usage|usage.output_tokens),
        "Harness returned a proposal to the Decision Engine");
    Ok(PlannerOutput {
        plan,
        metadata: Some(metadata),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn collector_is_data_only_and_cannot_dispatch_work() {
        let captured = Arc::new(Mutex::new(None));
        let grant = planner_grant(
            &AgenticContext::default(),
            Arc::clone(&captured),
            Vec::new(),
        );
        assert!(grant.executors.is_none());
        for tool in [
            "browser__click",
            "android_act",
            "macos-ui-automation__call",
            "run_task",
            "tool_search",
            "http",
        ] {
            assert!(!grant.permits(tool));
        }
        assert!(grant.permits(PLANNER_TOOL));
        let bridge = grant.mouth_bridge.as_ref().unwrap();
        let payload = json!({"steps":[{"id":"one","call":{"tool":"browser__click","arguments":{"target":"x"}}}],"toolAction":"Submit plan","toolSummary":"Proposal only"});
        let answer = bridge(
            "call-1".into(),
            PLANNER_TOOL.into(),
            payload,
            CancellationToken::new(),
        )
        .await;
        assert_eq!(answer["status"], "ok");
        assert_eq!(
            captured.lock().unwrap().as_ref().unwrap().steps[0]
                .call
                .tool,
            "browser__click"
        );
        let tools = super::super::catalog::plane_tools_list(&grant);
        assert_eq!(tools.len(), 1);
    }

    #[tokio::test]
    async fn planner_images_cross_the_real_plane_as_images_and_work_calls_are_denied() {
        let captured = Arc::new(Mutex::new(None));
        let image = json!({"type":"image","data":"aGVsbG8=","mimeType":"image/png"});
        let mut grant = planner_grant(
            &AgenticContext::default(),
            Arc::clone(&captured),
            vec![image.clone()],
        );
        grant.turn_ledger = Some(Arc::new(Mutex::new(
            super::super::grant::PlaneTurnLedger::default(),
        )));
        let result =
            super::super::dispatch::plane_tools_call(&grant, PLANNER_IMAGE_TOOL, &json!({})).await;
        assert_eq!(result["isError"], false, "{result}");
        assert_eq!(result["content"][0], image);
        let payload = json!({"steps":[{"id":"one","call":{"tool":"browser__click","arguments":{"args":["@e2"]}}}]});
        let result = super::super::dispatch::plane_tools_call(&grant, PLANNER_TOOL, &payload).await;
        assert_eq!(result["isError"], false, "{result}");
        assert_eq!(captured.lock().unwrap().as_ref().unwrap().steps.len(), 1);
        let denied = super::super::dispatch::plane_tools_call(
            &grant,
            "browser__click",
            &json!({"args":["@e2"]}),
        )
        .await;
        assert_eq!(denied["isError"], true, "{denied}");
        assert!(grant.executors.is_none());
        let ledger = grant.turn_ledger.as_ref().unwrap().lock().unwrap();
        let debug = format!("{ledger:?}");
        assert!(
            !debug.contains("aGVsbG8="),
            "binary evidence must not fill the ledger"
        );
    }

    #[test]
    fn text_plans_are_strict_json_not_prose_or_shell() {
        assert!(parse_text("I executed everything already").is_err());
        assert!(parse_text("rm -rf anything").is_err());
        assert!(parse_text("{\"steps\":[]}").is_ok());
        assert!(parse_text(
            r#"{"steps":[],"toolAction":"Submit plan","toolSummary":"Proposal only"}"#
        )
        .is_ok());
        assert!(parse_text(r#"{"steps":[],"toolAction":{"execute":"work"}}"#).is_err());
        assert!(parse_text(r#"{"steps":[],"execute":"work"}"#).is_err());
        let plan = parse_text(r#"{"reason":"Load the read schema first","steps":[{"id":"read","call":{"tool":"tool_search","arguments":{"query":"select:internal_data__read_task_state"}}}]}"#).unwrap();
        assert_eq!(plan.steps[0].call.tool, "tool_search");
        assert!(parse_text(r#"{"steps":[],"reason":{"execute":"work"}}"#).is_err());
        assert!(parse_text(r#"{"steps":[{"id":"read","call":{"tool":"read","arguments":{},"reason":"invalid call field"}}]}"#).is_err());
    }
}

/// Chat uses the same proposal-only grant as agentic execution. Its ordinary
/// final prose is returned separately and never interpreted as a work call.
pub(crate) async fn propose_chat(
    ctx: &AgenticContext,
    engine_name: &str,
    snapshot: &super::chat_turn::ChatHarnessSnapshot,
    pi_profile: Option<magicllm::config::LlmConfig>,
    images: Vec<Value>,
    system: &str,
    prompt: &str,
    cancel: &CancellationToken,
) -> Result<(Option<ActionPlan>, super::engine::HarnessTurnSettled)> {
    let engine = super::engines::harness_engine_for(engine_name)
        .ok_or_else(|| anyhow!("Unknown chat planner engine {engine_name}"))?;
    if engine.capabilities().native_tool_posture == NativeToolPosture::Live {
        return Err(anyhow!(
            "Chat planner {engine_name} cannot constrain native tools"
        ));
    }
    let captured = Arc::new(Mutex::new(None));
    let mut grant = planner_grant(ctx, captured.clone(), images.clone());
    grant.cancellation_token = Some(cancel.child_token());
    let token = plane_grant_registry().mint(grant).await;
    let mut lease = RevokeGrantOnDrop::new(token.clone());
    let request = HarnessSessionRequest {
        planning_only: true,
        endpoint: PlaneEndpoint {
            url: snapshot.plane_endpoint.clone(),
        },
        grant: token.clone(),
        system_prompt: system.into(),
        model: Some(snapshot.harness_model.clone()).filter(|model| model != "default"),
        pi_profile,
        pi_images: if engine_name == "pi" {
            images.clone()
        } else {
            Vec::new()
        },
        cwd: std::env::current_dir()?,
        env_allowlist: Vec::new(),
        cancel: Some(cancel.clone()),
        resume_session_id: None,
        native_home: None,
        turn_timeout: Duration::from_secs(if snapshot.turn_max_seconds == 0 {
            300
        } else {
            snapshot.turn_max_seconds.min(300)
        }),
        turn_idle_timeout: Some(Duration::from_secs(60)),
    };
    let mut session = engine.start(&request).await.map_err(|error| {
        report_harness_health(
            ctx,
            &request,
            engine_name,
            crate::magician_v2::realtime_events::ServiceFailure::from_error(&error.to_string())
                .map(Err),
        );
        error
    })?;
    let image_hint = if !images.is_empty() && engine_name != "pi" {
        format!("\nCaptured images are available through {PLANNER_IMAGE_TOOL}.")
    } else {
        String::new()
    };
    let outcome = session
        .turn(
            &HarnessTurnInput {
                text: format!("{prompt}{image_hint}"),
                operator_steer: Vec::new(),
            },
            &HarnessStreamSink::drain(),
        )
        .await;
    session.shutdown().await;
    plane_grant_registry().revoke(&token).await;
    lease.disarm();
    let mut outcome = outcome.map_err(|error| {
        report_harness_health(
            ctx,
            &request,
            engine_name,
            crate::magician_v2::realtime_events::ServiceFailure::from_error(&error.to_string())
                .map(Err),
        );
        error
    })?;
    report_harness_health(ctx, &request, engine_name, session.service_health(&outcome));
    if cancel.is_cancelled() {
        return Err(anyhow!("Chat cancelled"));
    }
    let captured = captured
        .lock()
        .map_err(|_| anyhow!("Chat proposal collector poisoned"))?
        .take();
    let plan = match captured {
        Some(plan) => Some(plan),
        None => parse_chat_reply(&mut outcome.assistant_text)?,
    };
    Ok((plan, outcome))
}

fn parse_chat_reply(text: &mut String) -> Result<Option<ActionPlan>> {
    if let Ok(plan) = parse_text(text) {
        return Ok(Some(plan));
    }
    let trimmed = text.trim();
    let json_text = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|body| body.trim().strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    if let Ok(Value::Object(mut value)) = serde_json::from_str::<Value>(json_text) {
        for label in ["toolAction", "toolSummary"] {
            if value.get(label).is_some_and(Value::is_string) {
                value.remove(label);
            }
        }
        if value.contains_key("steps") {
            return Err(anyhow!("Chat planner returned an invalid action plan"));
        }
        if value.len() == 1 {
            if let Some(answer) = value.get("answer").and_then(Value::as_str) {
                *text = answer.to_string();
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod chat_reply_tests {
    use super::*;
    #[test]
    fn decision_planner_chat_distinguishes_final_prose_from_proposals() {
        let mut text = "Hello!".to_string();
        assert!(parse_chat_reply(&mut text).unwrap().is_none());
        assert_eq!(text, "Hello!");
        let mut text = r#"{"answer":"All done."}"#.to_string();
        assert!(parse_chat_reply(&mut text).unwrap().is_none());
        assert_eq!(text, "All done.");
        let mut text =
            r#"{"answer":"All done.","toolAction":"Finish","toolSummary":"Reply"}"#.to_string();
        assert!(parse_chat_reply(&mut text).unwrap().is_none());
        assert_eq!(text, "All done.");
        let mut text =
            r#"{"steps":[{"id":"one","call":{"tool":"read","arguments":{}}}]}"#.to_string();
        assert_eq!(parse_chat_reply(&mut text).unwrap().unwrap().steps.len(), 1);
        let mut text = r#"{"steps":"invalid"}"#.to_string();
        assert!(parse_chat_reply(&mut text).is_err());
    }
}

fn report_harness_health(
    ctx: &AgenticContext,
    request: &HarnessSessionRequest,
    engine: &str,
    result: Option<Result<(), crate::magician_v2::realtime_events::ServiceFailure>>,
) {
    if request
        .cancel
        .as_ref()
        .is_some_and(CancellationToken::is_cancelled)
    {
        return;
    }
    let Some(result) = result else {
        return;
    };
    crate::magician_v2::decision_host::report_health(
        ctx.principal.as_deref().unwrap_or("anonymous"),
        ctx.workspace.as_deref().unwrap_or("default"),
        &request.health_service(engine),
        result,
    );
}

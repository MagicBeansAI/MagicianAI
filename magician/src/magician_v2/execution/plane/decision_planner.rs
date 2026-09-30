//! Generative proposal transport for the engine-owned action rail.
//! A returned plan has no execution authority until /v1/action accepts it.

use anyhow::{anyhow, Result};
use decision_engine_contract::action::{planner_schema, ActionPlan, PLANNER_TOOL};
use tokio_util::sync::CancellationToken;

use super::turn_engine::run_engine_for;
use crate::magician_v2::execution::agentic::native_integration::decision_rail_adapter;
use crate::magician_v2::execution::agentic::native_integration::NativeDecisionMetadata;
use crate::magician_v2::execution::agentic::native_types::NativeExecutionTool;
use crate::magician_v2::execution::agentic::types::{
    preflight_execution_token_budget, AgenticContext,
};
use crate::magician_v2::execution::agentic::ActionExecutors;

/// A received proposal can fail parsing or settling and still have real usage.
#[derive(Debug, thiserror::Error)]
#[error("{source}")]
pub(crate) struct PlannerResponseError {
    #[source]
    source: anyhow::Error,
    class: &'static str,
    transport_success: bool,
    telemetry: Option<crate::magician_v2::slot_graph::extraction::LlmCallTelemetry>,
}

pub(crate) fn response_error(
    source: anyhow::Error,
    class: &'static str,
    transport_success: bool,
    telemetry: Option<crate::magician_v2::slot_graph::extraction::LlmCallTelemetry>,
) -> anyhow::Error {
    anyhow::Error::new(PlannerResponseError {
        source,
        class,
        transport_success,
        telemetry,
    })
}

pub(crate) fn response_failure(
    error: &anyhow::Error,
) -> Option<(
    String,
    Option<crate::magician_v2::slot_graph::extraction::LlmCallTelemetry>,
    bool,
)> {
    if let Some(failure) = error.downcast_ref::<PlannerResponseError>() {
        return Some((
            failure.class.into(),
            failure.telemetry.clone(),
            failure.transport_success,
        ));
    }
    crate::magician_v2::execution::multi_llm_agent_adapter::MultiLlmAgentAdapter::validation_failure_from_error(error)
        .map(|(class, telemetry)| (class, telemetry, true))
}

fn plan_from_response(
    response: &crate::magician_v2::execution::agentic::native_types::ExecutionNativeResponse,
) -> Result<ActionPlan> {
    match response.tool_calls() {
        [] => serde_json::from_str(response.text.as_deref().unwrap_or_default())
            .map_err(|_| anyhow!("planner text was not an action plan")),
        [call] if call.name == PLANNER_TOOL => serde_json::from_value(call.arguments.clone())
            .map_err(|_| anyhow!("planner returned an invalid action plan")),
        _ => Err(anyhow!(
            "planner must return one {PLANNER_TOOL} proposal or a JSON action plan"
        )),
    }
}

pub struct PlannerOutput {
    pub plan: ActionPlan,
    pub metadata: Option<NativeDecisionMetadata>,
}

pub async fn propose(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    system: &str,
    prompt: &str,
    images: Vec<crate::magician_v2::slot_graph::extraction::ImageData>,
    cancel: Option<&CancellationToken>,
    iteration: usize,
) -> Result<PlannerOutput> {
    use crate::magician_v2::execution::agentic::types::{
        spawn_with_execution_token_meter_in_set, CapturedRunTaskLocals,
    };
    // Poll provider/harness work from a fresh task root, preserving the exact
    // shared run budget and secret/parent scopes. The task set aborts on drop.
    let locals = CapturedRunTaskLocals::for_context(ctx);
    let (ctx, executors) = (Box::new(ctx.clone()), Box::new(executors.clone()));
    let (system, prompt, cancel) = (system.to_owned(), prompt.to_owned(), cancel.cloned());
    let mut tasks = tokio::task::JoinSet::new();
    spawn_with_execution_token_meter_in_set(&mut tasks, async move {
        locals
            .scope(propose_inner(
                &ctx,
                &executors,
                &system,
                &prompt,
                images,
                cancel.as_ref(),
                iteration,
            ))
            .await
    });
    tasks
        .join_next()
        .await
        .ok_or_else(|| anyhow!("planner task disappeared"))?
        .map_err(|error| anyhow!("planner task failed: {error}"))?
}

// Task-local wrappers otherwise embed this entire provider/harness state
// machine repeatedly. Even a fresh scheduler root then overflows an ordinary
// debug worker stack. Keep the boundary at the definition so all callers carry
// only a pointer through those scopes; cancellation still drops the same work.
fn propose_inner<'a>(
    ctx: &'a AgenticContext,
    executors: &'a ActionExecutors,
    system: &'a str,
    prompt: &'a str,
    images: Vec<crate::magician_v2::slot_graph::extraction::ImageData>,
    cancel: Option<&'a CancellationToken>,
    iteration: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<PlannerOutput>> + Send + 'a>> {
    Box::pin(async move {
        preflight_execution_token_budget()?;
        let engine = run_engine_for(ctx);
        if engine != "magician" {
            return super::decision_planner_harness::propose(
                ctx, executors, system, prompt, images, cancel,
            )
            .await;
        }
        let adapter = decision_rail_adapter(ctx, executors, iteration);
        let tools = vec![NativeExecutionTool {
        name: PLANNER_TOOL.into(),
        description: "Return a proposed action plan to the Decision Engine. This does not execute any of its calls.".into(),
        parameters: planner_schema(),
        is_control_tool: false,
    }];
        let request = adapter.call_execution_native(
            "agentic_decision",
            system,
            prompt,
            tools,
            (!images.is_empty()).then_some(images),
            // Profiles may use auto tool choice (including reasoning providers).
            // Both forms remain proposals; the engine validates before execution.
            None,
            true,
            None,
        );
        let (envelope, response) = if let Some(cancel) = cancel {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(anyhow!("execution cancelled")),
                result = request => result?,
            }
        } else {
            request.await?
        };
        let plan = plan_from_response(&response).map_err(|error| {
            response_error(
                error,
                "invalid_action_plan",
                true,
                response.telemetry.clone(),
            )
        })?;
        Ok(PlannerOutput {
            plan,
            metadata: Some(NativeDecisionMetadata::from_envelope_and_response(
                "agentic_decision",
                &envelope,
                &response,
            )),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::agentic::native_types::ExecutionNativeResponse;

    #[test]
    fn decision_planner_dispatches_on_an_ordinary_worker_with_scopes_and_budget() {
        use crate::magician_v2::execution::agentic::types::{
            execution_token_budget_snapshot, with_execution_token_meter,
        };
        use crate::magician_v2::execution::multi_llm_agent_adapter::MultiLlmAgentAdapter;
        use crate::magician_v2::prompts::{JsonPromptStorage, PromptManager};
        use crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        struct Provider(Arc<AtomicUsize>);
        #[async_trait::async_trait]
        impl magicllm::dispatch::DispatchRouter for Provider {
            async fn route(
                &self,
                mut request: magicllm::LLMRequest,
            ) -> magicllm::LLMResult<magicllm::LLMResponse> {
                assert_eq!(request.tools[0].name, PLANNER_TOOL);
                let trace = request.metadata.trace_context.as_ref().unwrap();
                assert_eq!(trace.scope, magicllm::LlmScope::new("alice", "work"));
                assert_eq!(trace.execution_id.as_deref(), Some("planner-stack-fixture"));
                request.metadata.record_provider_attempt();
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(magicllm::LLMResponse {
                    text: Some(r#"{"steps":[{"id":"read","call":{"tool":"read_file","arguments":{"path":"fixture"}}}]}"#.into()),
                    usage: Some(magicllm::TokenUsage {
                        prompt_tokens: Some(7), completion_tokens: Some(3), total_tokens: Some(10),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
            }
            async fn route_stream(
                &self,
                _: magicllm::LLMRequest,
                _: tokio::sync::mpsc::Sender<magicllm::types::StreamDelta>,
            ) -> magicllm::LLMResult<()> {
                unreachable!()
            }
            fn provider_for_operation(&self, _: &str) -> Option<magicllm::LLMProviderKind> {
                Some(magicllm::LLMProviderKind::Ollama)
            }
            fn timeout_for_operation(&self, _: &str) -> Option<u64> {
                Some(5)
            }
        }

        // Explicitly use Tokio's ordinary 2 MiB stack, independent of a test
        // runner's RUST_MIN_STACK. The scripted native-response shortcut misses
        // the real adapter -> router -> queue poll chain that crashed at runtime.
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_stack_size(2 * 1024 * 1024)
            .enable_all()
            .build().unwrap()
            .block_on(async {
                let calls = Arc::new(AtomicUsize::new(0));
                let queue = magicllm::LlmDispatchQueue::start(
                    Arc::new(Provider(calls.clone())),
                    Arc::new(magicllm::dispatch::NoopTaskStateView),
                    Arc::new(magicllm::dispatch::NoopTaskLedgerSink),
                    magicllm::dispatch::DispatchConfig::default(),
                );
                let config = serde_json::from_value(serde_json::json!({
                    "default_profile": "fixture",
                    "profiles": {"fixture": {"provider": "ollama", "model": "fixture", "supports_tool_calling": true}}
                })).unwrap();
                let router = Arc::new(OperationLlmRouter::new(Some(config)));
                router.set_dispatch_queue(queue.clone());
                let executors = ActionExecutors::new(
                    Arc::new(crate::magician_v2::test_utils::ConfigurableMockLlm::with_response("{}")),
                    Arc::new(PromptManager::new(Arc::new(JsonPromptStorage::with_default_config().unwrap()))),
                ).with_native_adapter(Arc::new(MultiLlmAgentAdapter::new(router)));
                let mut ctx = AgenticContext::new("Read a fixture", "Record read");
                ctx.execution_id = Some("planner-stack-fixture".into());
                ctx.principal = Some("alice".into());
                ctx.workspace = Some("work".into());
                ctx.run_engine_pin = Some(super::super::RunEnginePin {
                    engine: "magician".into(), harness_model: "default".into(), pi_profile: None,
                });
                with_execution_token_meter(5, 100, async {
                    let cancel = CancellationToken::new();
                    let output = propose(&ctx, &executors, "system", "prompt", vec![], Some(&cancel), 1).await.unwrap();
                    assert_eq!(output.plan.steps[0].call.tool, "read_file");
                    assert_eq!(execution_token_budget_snapshot(), Some((15, 100)));
                    cancel.cancel();
                    assert!(propose(&ctx, &executors, "system", "prompt", vec![], Some(&cancel), 2).await.err().expect("cancelled planner must fail").to_string().contains("execution cancelled"));
                    assert_eq!(execution_token_budget_snapshot(), Some((15, 100)));
                }).await;
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                queue.shutdown(std::time::Duration::from_secs(1)).await;
            });
    }

    #[test]
    fn decision_planner_native_text_is_strict_proposal_data() {
        let mut response = ExecutionNativeResponse::unadmitted(Vec::new());
        response.text = Some(r#"{"steps":[{"id":"read","call":{"tool":"read_file","arguments":{"path":"fixture"}}}]}"#.into());
        assert_eq!(
            plan_from_response(&response).unwrap().steps[0].call.tool,
            "read_file"
        );
        for text in [
            "I completed everything",
            r#"{"steps":[],"unexpected":true}"#,
            r#"{"steps":[{"id":"bad","call":{"tool":"read_file","arguments":{},"extra":true}}]}"#,
        ] {
            response.text = Some(text.into());
            assert!(plan_from_response(&response).is_err());
        }
    }

    #[test]
    fn terminal_usage_rejected_planner_preserves_received_metering() {
        let telemetry = crate::magician_v2::slot_graph::extraction::LlmCallTelemetry {
            input_tokens: 123,
            cost_usd: 0.25,
            ..Default::default()
        };
        let error = response_error(
            anyhow!("private model reply"),
            "invalid_action_plan",
            true,
            Some(telemetry),
        )
        .context("outer context");
        let (class, telemetry, succeeded) = response_failure(&error).unwrap();
        assert_eq!(class, "invalid_action_plan");
        assert!(succeeded);
        let telemetry = telemetry.unwrap();
        assert_eq!(telemetry.input_tokens, 123);
        assert_eq!(telemetry.cost_usd, 0.25);
        assert!(response_failure(&anyhow!("pre-call failure")).is_none());
    }
}

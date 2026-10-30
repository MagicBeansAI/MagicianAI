//! The provider-outage branch of `resolve_decision_error`.
use super::*;
use crate::magician_v2::execution::agentic::decision::history_is_stuck;
use crate::magician_v2::prompts::{JsonPromptStorage, PromptManager};
use crate::magician_v2::slot_graph::extraction::{
    LlmFunctionCallRequest, LlmFunctionCallResponse, LlmService,
};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

/// The resolve phase never calls a model; this only lets `ActionExecutors` be built.
struct UnusedLlm;

#[async_trait]
impl LlmService for UnusedLlm {
    async fn call_function(
        &self,
        _request: LlmFunctionCallRequest,
    ) -> Result<LlmFunctionCallResponse> {
        Err(anyhow!("the resolve phase does not call the model"))
    }
}

fn executors() -> ActionExecutors {
    let storage = JsonPromptStorage::with_default_config().unwrap();
    ActionExecutors::new(
        Arc::new(UnusedLlm),
        Arc::new(PromptManager::new(Arc::new(storage))),
    )
}

fn outage() -> anyhow::Error {
    anyhow::Error::new(magicllm::LLMError::ProviderUnavailable).context("decision call failed")
}

struct Run {
    ctx: AgenticContext,
    executors: ActionExecutors,
    history: ExecutionHistory,
    protective: LoopProtectiveState,
    /// The iteration the next failure lands on. Every boundary, a retry
    /// included, advances it, as both drivers do.
    iteration: usize,
    /// Keeps an owner-backed run's storage root alive for the test.
    _storage: Option<tempfile::TempDir>,
}

impl Run {
    fn new() -> Self {
        Self {
            ctx: AgenticContext::new("goal", "done"),
            executors: executors(),
            history: ExecutionHistory::new(),
            protective: LoopProtectiveState::default(),
            iteration: 1,
            _storage: None,
        }
    }

    /// An ordinary task-backed root run: the kind that pauses for its owner
    /// when the provider retries run out.
    fn owner_backed() -> Self {
        let storage = tempfile::tempdir().expect("temp storage root");
        let mut run = Self::new();
        run.ctx.storage_base_path = storage.path().to_owned();
        run.ctx.principal = Some("owner".into());
        run.ctx.workspace = Some("workspace".into());
        run.ctx.task_id = Some("task-1".into());
        // Unique: a pause publishes the run's budget to a process-wide registry.
        run.ctx.execution_id = Some(format!("provider-failure-{}", uuid::Uuid::new_v4()));
        run._storage = Some(storage);
        assert!(
            crate::magician_v2::execution::agentic::run_budget::pauses_on_provider_failure(
                &run.ctx
            )
        );
        run
    }

    async fn fail_with(&mut self, error: anyhow::Error) -> PhaseStep<ResolveOutput> {
        let mut approved = Vec::new();
        let iteration = self.iteration;
        self.iteration += 1;
        resolve_decision_error(
            &mut self.ctx,
            &self.executors,
            &mut self.history,
            &mut self.protective,
            &mut approved,
            EnvironmentState::Uninitialized,
            &None,
            &Instant::now(),
            iteration,
            error,
        )
        .heap_boxed()
        .await
        .expect("the resolve phase handles a decision error")
    }

    async fn retry_wait(&mut self, error: anyhow::Error) -> Duration {
        match self.fail_with(error).await {
            PhaseStep::Exit(BoundaryOutcome::Retry(wait)) => wait,
            PhaseStep::Exit(_) => panic!("expected a retry, got another boundary exit"),
            PhaseStep::Continue(_) => panic!("expected a retry, the phase continued"),
            PhaseStep::Return(_) => panic!("expected a retry, the run ended"),
        }
    }
}

#[actix_rt::test]
async fn six_outages_retry_with_capped_backoff_and_the_seventh_fails() {
    let mut run = Run::new();
    let bases = [15, 30, 60, 120, 240, 300];
    assert_eq!(bases.len(), OUTAGE_MAX_RETRIES);
    for (index, base) in bases.into_iter().enumerate() {
        let wait = run.retry_wait(outage()).await;
        assert!(
            wait >= Duration::from_secs(base) && wait < Duration::from_secs(base + 3),
            "retry {} waited {wait:?}, expected about {base}s",
            index + 1
        );
        // Journal clamps a retry at one hour; every outage wait is far inside it.
        assert!(wait < Duration::from_secs(3600));
    }
    assert_eq!(run.protective.outage_retry_count, OUTAGE_MAX_RETRIES);
    // A run with no task owner fails, and the retries it rode out were refunded.
    assert_eq!(run.protective.parked_iteration_refunds, OUTAGE_MAX_RETRIES);

    match run.fail_with(outage()).await {
        PhaseStep::Return(outcome) => match *outcome {
            AgenticOutcome::Failed { reason, .. } => {
                assert!(
                    reason.contains("provider outage persisted after 7 tries over ~13 min"),
                    "{reason}"
                );
            },
            _ => panic!("expected the run to fail"),
        },
        _ => panic!("the seventh outage must end the run"),
    }
}

#[actix_rt::test]
async fn outage_waits_push_no_failed_iteration_record() {
    let mut run = Run::new();
    for _ in 0..OUTAGE_MAX_RETRIES {
        run.retry_wait(outage()).await;
        assert!(
            run.history.iterations.is_empty(),
            "five identical failed records would trip the repeat-failure detector"
        );
        assert!(!history_is_stuck(&run.history));
    }
}

#[actix_rt::test]
async fn a_transient_error_still_records_its_failed_iteration() {
    let mut run = Run::new();
    run.retry_wait(anyhow!("rate limit exceeded")).await;
    assert_eq!(run.history.iterations.len(), 1);
    assert!(!run.history.iterations[0].result.success);
}

#[actix_rt::test]
async fn the_outage_counter_survives_a_pause_and_resume() {
    let mut run = Run::new();
    for _ in 0..3 {
        run.retry_wait(outage()).await;
    }
    // A pause persists the protective bundle; the resumed run decodes it.
    let encoded = serde_json::to_value(&run.protective).expect("encode the bundle");
    assert_eq!(encoded["outage_retry_count"], 3);
    run.protective = serde_json::from_value(encoded).expect("decode the bundle");
    assert_eq!(run.protective.outage_retry_count, 3);
    let wait = run.retry_wait(outage()).await;
    assert!(
        wait >= Duration::from_secs(120) && wait < Duration::from_secs(123),
        "the fourth outage waits 120s, got {wait:?}"
    );

    // Each failed call handed its iteration back.
    assert_eq!(run.protective.parked_iteration_refunds, 4);

    // Once the provider answers, the counter returns to the pristine state the
    // pause authorization hash was computed over (the refunds aside, which
    // stay with the segment until a pause folds them into its limit).
    run.protective.outage_retry_count = 0;
    run.protective.parked_iteration_refunds = 0;
    assert!(run.protective.is_pristine());
    assert_eq!(
        serde_json::to_value(&run.protective).unwrap(),
        serde_json::to_value(LoopProtectiveState::default()).unwrap()
    );
}

/// Throttling and an outage are separate budgets: five 429s do not push a
/// later outage straight to its longest wait, and an outage does not make the
/// 429 that follows it abort the run.
#[actix_rt::test]
async fn throttling_and_an_outage_do_not_share_a_retry_budget() {
    let throttled = || anyhow!("429 Too Many Requests: rate limit exceeded");
    let mut run = Run::new();
    for _ in 0..3 {
        run.retry_wait(throttled()).await;
    }
    let wait = run.retry_wait(outage()).await;
    assert!(
        wait >= Duration::from_secs(15) && wait < Duration::from_secs(18),
        "the first outage after throttling waits 15s, got {wait:?}"
    );
    for _ in 0..4 {
        run.retry_wait(outage()).await;
    }
    assert_eq!(run.protective.outage_retry_count, 5);
    // The outage did not use up the throttling budget: a fourth 429 still retries.
    run.retry_wait(throttled()).await;
    assert_eq!(run.protective.transient_retry_count, 4);
}

fn spent_envelope() -> anyhow::Error {
    anyhow::Error::new(magicllm::LLMError::SpendEnvelopeExhausted {
        resets_at_ms: chrono::Utc::now().timestamp_millis() + 3_600_000,
    })
    .context("decision call failed")
}

/// A run parked on a spent envelope waits without spending its allowance: each
/// parked wake hands its iteration back, and no retry counter moves, however
/// long the window is. The refund is counted on the protective bundle, which
/// every pause and continuation checkpoint persists; the limit the run was
/// given is left alone.
#[actix_rt::test]
async fn a_parked_wake_hands_its_iteration_back() {
    let mut run = Run::new();
    let allowance = run.ctx.max_iterations;
    for _ in 0..20 {
        run.retry_wait(spent_envelope()).await;
    }
    assert_eq!(run.ctx.max_iterations, allowance);
    assert_eq!(run.protective.parked_iteration_refunds, 20);
    assert_eq!(
        crate::magician_v2::execution::agentic::run_budget::iteration_ceiling(
            &run.ctx,
            &run.protective,
        ),
        allowance + 20
    );
    assert_eq!(run.protective.transient_retry_count, 0);
    assert_eq!(run.protective.outage_retry_count, 0);

    // The refund survives the bundle's round trip (a pause, or the Retry
    // boundary's continuation checkpoint read back after a restart).
    let encoded = serde_json::to_value(&run.protective).expect("encode the bundle");
    assert_eq!(encoded["parked_iteration_refunds"], 20);
    let decoded: LoopProtectiveState = serde_json::from_value(encoded).expect("decode the bundle");
    assert_eq!(decoded.parked_iteration_refunds, 20);
    assert!(!decoded.is_pristine());
}

/// An app contract's disclosure authority. Never consulted: the resolve phase
/// makes no provider call.
#[derive(Debug)]
struct DenyAppDisclosure;

#[async_trait]
impl magicllm::LlmDisclosureAuthorizer for DenyAppDisclosure {
    async fn revalidate(
        &self,
        _profile: &str,
        _provider: &magicllm::capability::LLMProviderKind,
        _model: &str,
        _api_base_url: Option<&str>,
    ) -> std::result::Result<(), String> {
        Err("the resolve phase makes no provider call".to_string())
    }
}

/// A sealed controller attempt and an app contract keep the limit they were
/// admitted with: a parked wake there still waits, but hands nothing back.
#[actix_rt::test]
async fn a_controller_attempt_or_app_contract_gets_no_parked_refund() {
    let mut controller = Run::new();
    controller.ctx.controller_attempt_hash = Some("a".repeat(64));
    let allowance = controller.ctx.max_iterations;
    for _ in 0..3 {
        controller.retry_wait(spent_envelope()).await;
    }
    assert_eq!(controller.ctx.max_iterations, allowance);
    assert_eq!(controller.protective.parked_iteration_refunds, 0);
    assert!(controller.protective.is_pristine());
    assert_eq!(
        crate::magician_v2::execution::agentic::run_budget::iteration_ceiling(
            &controller.ctx,
            &controller.protective,
        ),
        allowance
    );

    let mut app = AgenticContext::new("goal", "done");
    assert!(crate::magician_v2::execution::agentic::run_budget::refunds_parked_iterations(&app));
    app.app_disclosure_guard = Some(
        magicllm::LlmDisclosureGuard::new(
            "app-local",
            "a".repeat(64),
            "app-continuation-v1",
            "app-policy-v1",
            magicllm::LlmDisclosureCapturePolicy::MetadataOnly,
            Arc::new(DenyAppDisclosure),
        )
        .expect("valid test disclosure guard"),
    );
    assert!(!crate::magician_v2::execution::agentic::run_budget::refunds_parked_iterations(&app));
}

/// An outage on a call the router attributed to a provider and model.
fn routed_outage() -> anyhow::Error {
    anyhow::Error::new(magicllm::LLMError::Routed {
        profile: "default".to_string(),
        provider: magicllm::capability::LLMProviderKind::OpenAI,
        model: "gpt-test".to_string(),
        source: Box::new(magicllm::LLMError::ProviderUnavailable),
    })
    .context("decision call failed")
}

/// The pause an exhausted provider budget raised, with the bundle it persisted.
fn provider_failure_pause(
    step: PhaseStep<ResolveOutput>,
) -> (
    BudgetDimension,
    crate::magician_v2::execution::agentic::types::AgenticPauseState,
) {
    match step {
        PhaseStep::Return(outcome) => match *outcome {
            AgenticOutcome::BudgetExhausted {
                dimension: dimension @ BudgetDimension::ProviderFailure { .. },
                pause_state: Some(pause),
                ..
            } => (dimension, pause),
            AgenticOutcome::Failed { reason, .. } => {
                panic!("an owner-backed run must pause, not fail: {reason}")
            },
            _ => panic!("expected a provider-failure pause"),
        },
        _ => panic!("the exhausted provider budget must end the segment"),
    }
}

/// No fallback model: when the outage outlasts its budget on an owner-backed
/// run, the run pauses for its owner to retry or cancel instead of failing.
#[actix_rt::test]
async fn an_outage_that_outlasts_its_budget_pauses_an_owner_run_for_retry_or_cancel() {
    let mut run = Run::owner_backed();
    for _ in 0..OUTAGE_MAX_RETRIES {
        run.retry_wait(routed_outage()).await;
    }
    let (dimension, pause) = provider_failure_pause(run.fail_with(routed_outage()).await);

    let BudgetDimension::ProviderFailure {
        ref route,
        attempts,
        waited_secs,
        ..
    } = dimension
    else {
        unreachable!()
    };
    assert_eq!(route, "openai/gpt-test");
    assert_eq!(attempts, OUTAGE_MAX_RETRIES + 1);
    assert_eq!(
        waited_secs, 765,
        "15+30+60+120+240+300 s of scheduled waits"
    );
    assert_eq!(dimension.confirm_label(), "Retry");
    let prompt = dimension.continue_prompt();
    assert!(prompt.contains("openai/gpt-test"), "{prompt}");
    assert!(prompt.contains("for about 13 minutes"), "{prompt}");
    assert!(prompt.contains("Retry this run"), "{prompt}");

    // Checkpointed before the failed call, which did no work; the six retried
    // calls were refunded, so Retry gets the run's whole allowance back.
    assert_eq!(pause.iteration, OUTAGE_MAX_RETRIES);
    let persisted = pause
        .loop_protective_state
        .clone()
        .expect("the pause persists the protective bundle");
    assert!(persisted.provider_failure_pause);
    assert_eq!(persisted.parked_iteration_refunds, OUTAGE_MAX_RETRIES);
    assert!(crate::magician_v2::execution::agentic::run_budget::is_provider_failure_pause(&pause));
    // The card itself: `executor::tests::a_provider_failure_pause_asks_the_owner_to_retry_or_cancel`.
}

/// Retry resumes from the saved progress with fresh provider budgets: the
/// persisted bundle carries no spent outage or transient retries, and the
/// resumed segment drops the provider-failure marker, so a later pause's
/// Cancel is not read as this one's.
#[actix_rt::test]
async fn retry_resumes_with_reset_provider_retry_budgets() {
    let mut run = Run::owner_backed();
    for _ in 0..2 {
        run.retry_wait(anyhow!("429 Too Many Requests: rate limit exceeded"))
            .await;
    }
    for _ in 0..OUTAGE_MAX_RETRIES {
        run.retry_wait(outage()).await;
    }
    let (_, pause) = provider_failure_pause(run.fail_with(outage()).await);
    let persisted = pause
        .loop_protective_state
        .clone()
        .expect("persisted bundle");
    assert_eq!(persisted.outage_retry_count, 0);
    assert_eq!(persisted.transient_retry_count, 0);

    let resumed = crate::magician_v2::execution::agentic::executor::restore_context_from_pause(
        &pause,
        &pause.goal,
        &pause.success_criteria,
        pause.max_iterations,
    );
    let carried = resumed
        .resume_loop_protective
        .expect("the resumed segment carries the bundle");
    assert_eq!(carried.outage_retry_count, 0);
    assert_eq!(carried.transient_retry_count, 0);
    assert!(!carried.provider_failure_pause);
    assert_eq!(
        carried.parked_iteration_refunds, 0,
        "folded into the resumed limit"
    );
}

/// The ordinary transient budget (about 16 s of quick retries) pauses an
/// owner run the same way once it is spent.
#[actix_rt::test]
async fn spent_transient_retries_pause_an_owner_run_too() {
    let throttled = || anyhow!("429 Too Many Requests: rate limit exceeded");
    let mut run = Run::owner_backed();
    for _ in 0..MAX_TRANSIENT_RETRIES {
        run.retry_wait(throttled()).await;
    }
    assert_eq!(
        run.protective.parked_iteration_refunds,
        MAX_TRANSIENT_RETRIES
    );
    let (dimension, _) = provider_failure_pause(run.fail_with(throttled()).await);
    let BudgetDimension::ProviderFailure {
        attempts,
        waited_secs,
        ..
    } = dimension
    else {
        unreachable!()
    };
    assert_eq!(attempts, MAX_TRANSIENT_RETRIES + 1);
    assert!(waited_secs < 90);
    assert!(
        dimension.continue_prompt().contains("tries in a row"),
        "{}",
        dimension.continue_prompt()
    );
}

/// A delegated child keeps failing, so its parent or root decides; it is not
/// the child's place to ask the owner.
#[actix_rt::test]
async fn a_delegated_child_still_fails_when_the_outage_persists() {
    let mut run = Run::owner_backed();
    run.ctx.depth = 1;
    assert!(
        !crate::magician_v2::execution::agentic::run_budget::pauses_on_provider_failure(&run.ctx)
    );
    for _ in 0..OUTAGE_MAX_RETRIES {
        run.retry_wait(outage()).await;
    }
    match run.fail_with(outage()).await {
        PhaseStep::Return(outcome) => match *outcome {
            AgenticOutcome::Failed { reason, .. } => {
                assert!(reason.contains("provider outage persisted"), "{reason}");
            },
            _ => panic!("a delegated child must fail, not pause"),
        },
        _ => panic!("the exhausted outage budget must end the child"),
    }
}

/// A sealed controller attempt keeps its fixed limit: a retried provider
/// failure hands no iteration back there, and the exhausted budget fails it.
#[actix_rt::test]
async fn a_controller_attempt_gets_no_provider_retry_refund_and_still_fails() {
    let mut run = Run::owner_backed();
    run.ctx.controller_attempt_hash = Some("a".repeat(64));
    for _ in 0..OUTAGE_MAX_RETRIES {
        run.retry_wait(outage()).await;
    }
    assert_eq!(run.protective.parked_iteration_refunds, 0);
    match run.fail_with(outage()).await {
        PhaseStep::Return(outcome) => {
            assert!(matches!(*outcome, AgenticOutcome::Failed { .. }));
        },
        _ => panic!("the exhausted outage budget must end the attempt"),
    }
}

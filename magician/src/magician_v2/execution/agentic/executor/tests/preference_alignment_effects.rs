//! Preference alignment through the production seams, counting effects
//! (plan §13.3, P4.9).
//!
//! Every test here drives `execute_agentically` on the worker driver the
//! product uses, with a scripted model and a scripted judge, against a real
//! owner preference saved through the chat save path. The act is a
//! commitment (`swiggy-mcp` `place_food_order`) served by a counting provider
//! registered in the capability registry, so the count is the number of
//! times the act actually reached its transport — not a prompt string, not a
//! helper's return value.
//!
//! Mutation guide: bypassing the gate, ignoring the ledger, admitting on a
//! receipt for a different act, or treating free text or a late reply as
//! consent each makes one of these counts wrong.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::magician_v2::preference_alignment::{
    ask::GENERIC_ASK_TEXT,
    contract::{
        AlignmentDecision, AlignmentInput, Assessment, Continuation, Intervention,
        ResolutionChoice, ALIGNMENT_DECISION_SCHEMA_VERSION,
    },
    judge::ScriptedJudge,
    owner_turn::{with_owner_turn, OwnerTurn},
    service::AlignmentOverride,
};

const PACK: &str = "swiggy-mcp";

/// A judge that asks to hold on every act it sees, citing everything it was
/// shown, and counts its calls.
struct HoldsEverything {
    calls: AtomicUsize,
}

impl ScriptedJudge for HoldsEverything {
    fn respond(&self, input: &AlignmentInput) -> std::result::Result<String, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        serde_json::to_string(&AlignmentDecision {
            schema_version: ALIGNMENT_DECISION_SCHEMA_VERSION.to_string(),
            assessment: Assessment::Conflict,
            intervention: Intervention::Confirm,
            continuation: Continuation::Wait,
            supporting_intentions: input.intentions.iter().map(|i| i.id.clone()).collect(),
            holding_intentions: input.intentions.iter().map(|i| i.id.clone()).collect(),
            cited_owner_spans: Vec::new(),
            explanation: "This order includes meat, and you asked to be checked first.".to_string(),
            unresolved_facts: Vec::new(),
            resolution_choices: vec![
                ResolutionChoice::AllowOnce,
                ResolutionChoice::UseAlternative,
                ResolutionChoice::ChangePreference,
                ResolutionChoice::Cancel,
            ],
            remember_offer: None,
        })
        .map_err(|error| error.to_string())
    }
}

/// The transport: counts every order that reaches it.
#[derive(Debug)]
struct CountingOrderProvider {
    executions: Arc<AtomicUsize>,
}

#[async_trait]
impl crate::magician_v2::execution::capability::CapabilityProvider for CountingOrderProvider {
    fn tool_name(&self) -> &str {
        PACK
    }

    fn lower(
        &self,
        step: &crate::magician_v2::strategy::plan::PlanStep,
    ) -> std::result::Result<
        crate::magician_v2::resource_authority::gated_action::MaybeGatedAction,
        crate::magician_v2::execution::error::ExecutionError,
    > {
        Ok(
            crate::magician_v2::resource_authority::gated_action::MaybeGatedAction::Bare(
                ExecutableAction::Pack {
                    capability_name: PACK.to_string(),
                    implementation:
                        crate::magician_v2::execution::capability::ImplementationType::Compiled {
                            provider_name: PACK.to_string(),
                        },
                    resolved_params: step.parameters.clone(),
                },
            ),
        )
    }

    async fn execute(
        &self,
        _action: &ExecutableAction,
        _session_id: Option<String>,
        _timeout_secs: u64,
    ) -> std::result::Result<ActionResult, crate::magician_v2::execution::error::ExecutionError>
    {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(ActionResult::Text {
            content: "order placed".to_string(),
        })
    }
}

fn order_pack() -> crate::magician_v2::execution::capability::CapabilityPackDefinition {
    serde_yaml::from_str(&format!(
        r#"
name: {PACK}
description: Counting food-order provider for the alignment effect matrix.
version: "1.0.0"
parameters:
  - name: action
    required: true
    param_type: string
    description: The order action.
  - name: items
    required: true
    param_type: string
    description: What to order.
implementation:
  type: compiled
  provider_name: {PACK}
execution:
  requires_browser_session: false
  default_timeout_secs: 5
  categories: [test]
  composition_category: action
  sandbox: none
"#
    ))
    .expect("order pack")
}

fn order_tool() -> runtime_core::ToolInfo {
    let parameter = |name: &str, description: &str| runtime_core::ParameterDefinition {
        name: name.to_string(),
        param_type: "string".to_string(),
        required: true,
        description: description.to_string(),
        validation_rules: Vec::new(),
        default_value: None,
        enum_values: None,
        schema: serde_json::json!({"type": "string"}),
    };
    runtime_core::ToolInfo {
        name: PACK.to_string(),
        description: "Order food".to_string(),
        category: "test".to_string(),
        categories: vec!["test".to_string()],
        parameters: vec![
            parameter("action", "The order action"),
            parameter("items", "What to order"),
        ],
        enhanced_description: None,
        keywords: Vec::new(),
        use_cases: Vec::new(),
        composition_category: Some("action".to_string()),
        providing_agent_id: None,
    }
}

fn order_decision(item: &str) -> String {
    format!(
        r#"{{"decision":"execute","action_type":"tool","capability_name":"{PACK}","action":"place_food_order","items":"{item}"}}"#
    )
}

const DONE: &str =
    r#"{"decision":"goal_reached","evidence":"the order step is settled","artifacts":[]}"#;

struct AlignmentRun {
    run: ScopedLoopRun,
    judge: Arc<HoldsEverything>,
    executions: Arc<AtomicUsize>,
}

/// One scoped run on the worker driver, with the owner's explicit preference
/// saved through the chat save path and alignment in `mode`.
async fn alignment_run(mode: &str, fixtures: Vec<String>) -> AlignmentRun {
    // Diagnostics: `PA_TEST_TRACE=1 RUST_LOG=info` prints the run's logs.
    if std::env::var_os("PA_TEST_TRACE").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_test_writer()
            .try_init();
    }
    let mut run = scoped_loop_run_owned_by_with_direct_tools(
        "Order dinner for the owner",
        "Dinner is ordered or the owner decided",
        fixtures,
        SCOPED_LOOP_OWNER,
        Vec::new(),
        vec![order_tool()],
    )
    .heap_boxed()
    .await;

    // The owner said it, in their own turn: an explicit preference that may
    // hold an act.
    let resolver = crate::magician_v2::agents::AgentMemoryResolver::new(
        run.executors.artifact_v2_workspace.base_root(),
    );
    let saved = with_owner_turn(
        OwnerTurn::new("turn-1", "Check with me before ordering meat for me."),
        crate::magician_v2::chat::service::merge_user_preference_with_event(
            &resolver,
            SCOPED_LOOP_PRINCIPAL,
            SCOPED_LOOP_WORKSPACE,
            "preferences",
            "meat_orders",
            serde_json::json!("Check with me before ordering meat for me."),
            None,
        ),
    )
    .await;
    assert_eq!(saved["status"], "ok", "{saved}");

    let executions = Arc::new(AtomicUsize::new(0));
    let registry = Arc::new(crate::magician_v2::execution::capability::CapabilityRegistry::new());
    registry.register(Arc::new(CountingOrderProvider {
        executions: Arc::clone(&executions),
    }));
    let pack = order_pack();
    registry.set_pack_definition(PACK, pack.clone());
    run.executors.capability_registry = Some(Arc::clone(&registry));
    run.ctx.tool_index = Some(Arc::new(
        crate::magician_v2::execution::flat_loop::build_tool_index(&[pack]),
    ));
    run.ctx
        .scratch
        .loaded_tools
        .lock()
        .expect("loaded tools")
        .insert(PACK.to_string());
    run.ctx.merged_agent_tools = vec![order_tool()];
    run.ctx.max_iterations = 6;

    let judge = Arc::new(HoldsEverything {
        calls: AtomicUsize::new(0),
    });
    run.executors = run
        .executors
        .clone()
        .with_preference_alignment_override(AlignmentOverride {
            config: crate::config::PreferenceAlignmentConfig {
                mode: mode.to_string(),
                ..crate::config::PreferenceAlignmentConfig::default()
            },
            judge: Some(judge.clone() as Arc<dyn ScriptedJudge>),
        });
    AlignmentRun {
        run,
        judge,
        executions,
    }
}

async fn run_to_outcome(alignment: &AlignmentRun) -> AgenticOutcome {
    execute_agentically(
        &alignment.run.ctx,
        EnvironmentState::Shell(ShellState::default()),
        &alignment.run.executors,
        None,
    )
    .await
    .expect("the run reaches a terminal")
}

/// The pause the HITL respond path would hand to resume, mapped the way the
/// orchestrator maps a `WaitingForConfirmation`.
fn pause_data_for(outcome: &AgenticOutcome, executors: &ActionExecutors) -> FullPauseData {
    let AgenticOutcome::WaitingForConfirmation {
        action_summary,
        reason,
        action_type,
        action_json,
        pause_state,
        ..
    } = outcome
    else {
        panic!("expected an alignment hold, got {outcome:?}");
    };
    FullPauseData::new(
        pause_state.clone(),
        UserInputType::Confirmation {
            confirm_label: Some("Proceed".to_string()),
            deny_label: Some("Cancel".to_string()),
            destructive: false,
        },
        executors.clone(),
        Some(format!("{action_summary}: {reason}")),
        Some(format!("Action: {action_json}")),
    )
    .with_confirmation_action_json(Some(action_json.clone()))
    .with_confirmation_event_details(action_summary.clone(), reason.clone(), action_type.clone())
    .with_pause_kind(ExecutionPauseKind::Confirmation)
}

async fn resume(pause_data: FullPauseData, value: UserInputValue) -> ResumeResult {
    let input_type = pause_data.input_type.clone();
    execute_agentically_resume_with_validation(
        pause_data,
        UserInputResponse::new(input_type, value),
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("the resume settles")
}

/// The same answer delivered again to a pause the first resume consumed (a
/// lost response, a client retry). On the stateless driver the resumed
/// segment has already ended under a settlement receipt, so the executor
/// refuses to advance it — the control signal production callers hand to the
/// terminal lifecycle — and nothing runs: `None`. A driver that does run it
/// again must hold the act: `Some(result)`.
async fn resume_retry(pause_data: FullPauseData, value: UserInputValue) -> Option<ResumeResult> {
    let input_type = pause_data.input_type.clone();
    match execute_agentically_resume_with_validation(
        pause_data,
        UserInputResponse::new(input_type, value),
        None,
        None,
        None,
        None,
        None,
    )
    .await
    {
        Ok(result) => Some(result),
        Err(error)
            if crate::magician_v2::execution::agentic::run_loop::is_terminal_settlement_pending_error(
                &error.to_string(),
            ) =>
        {
            None
        },
        Err(error) => panic!("a retried answer is refused or held, never an error: {error}"),
    }
}

fn describe(result: &ResumeResult) -> String {
    match result {
        ResumeResult::Continue(outcome) => format!("{outcome:?}"),
        ResumeResult::ReaskRequired { reason, .. } => format!("re-ask: {reason}"),
    }
}

/// Run to the hold and check it is an alignment hold that fired nothing.
async fn hold(alignment: &AlignmentRun) -> FullPauseData {
    let outcome = run_to_outcome(alignment).await;
    let AgenticOutcome::WaitingForConfirmation {
        action_json,
        pause_state,
        ..
    } = &outcome
    else {
        panic!("an explicit preference holds the order: {outcome:?}");
    };
    let ask = pause_state
        .alignment_ask
        .as_ref()
        .expect("the hold carries its typed ask");
    assert!(
        action_json.starts_with(
            crate::magician_v2::preference_alignment::receipts::ALIGNMENT_EXCEPTION_PREFIX
        ),
        "the pause token is an alignment receipt, never a bare approval"
    );
    assert_eq!(pause_state.ask_id.as_deref(), Some(ask.ask_id.as_str()));
    assert!(ask.deadline_at_ms > ask.created_at_ms);
    assert_eq!(
        alignment.executions.load(Ordering::SeqCst),
        0,
        "a held order never fires"
    );
    pause_data_for(&outcome, &alignment.run.executors)
}

/// A judge that finds every act aligned, and counts its calls.
struct AllowsEverything {
    calls: AtomicUsize,
}

impl ScriptedJudge for AllowsEverything {
    fn respond(&self, _input: &AlignmentInput) -> std::result::Result<String, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        serde_json::to_string(&AlignmentDecision {
            schema_version: ALIGNMENT_DECISION_SCHEMA_VERSION.to_string(),
            assessment: Assessment::Aligned,
            intervention: Intervention::None,
            continuation: Continuation::Proceed,
            supporting_intentions: Vec::new(),
            holding_intentions: Vec::new(),
            cited_owner_spans: Vec::new(),
            explanation: String::new(),
            unresolved_facts: Vec::new(),
            resolution_choices: Vec::new(),
            remember_offer: None,
        })
        .map_err(|error| error.to_string())
    }
}

/// A judge whose answer is not a decision at all, and counts its calls.
struct AnswersGarbage {
    calls: AtomicUsize,
}

impl ScriptedJudge for AnswersGarbage {
    fn respond(&self, _input: &AlignmentInput) -> std::result::Result<String, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok("Sure! The order looks fine to me, go ahead.".to_string())
    }
}

/// Swap the run's judge and configuration (alignment always enforcing).
fn rejudge(
    alignment: &mut AlignmentRun,
    judge: Arc<dyn ScriptedJudge>,
    configure: impl FnOnce(&mut crate::config::PreferenceAlignmentConfig),
) {
    let mut config = crate::config::PreferenceAlignmentConfig {
        mode: "enforce".to_string(),
        ..crate::config::PreferenceAlignmentConfig::default()
    };
    configure(&mut config);
    alignment.run.executors = alignment
        .run
        .executors
        .clone()
        .with_preference_alignment_override(AlignmentOverride {
            config,
            judge: Some(judge),
        });
}

/// Plan §13.3 item 25: a task that has spent its judge-call budget holds its
/// next act; the budget never turns into "proceed unchecked".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spent_judge_budget_holds_the_next_act_instead_of_letting_it_through() {
    let mut alignment = alignment_run(
        "enforce",
        vec![
            order_decision("paneer biryani"),
            order_decision("chicken biryani"),
        ],
    )
    .await;
    let allows = Arc::new(AllowsEverything {
        calls: AtomicUsize::new(0),
    });
    rejudge(&mut alignment, allows.clone(), |config| {
        config.budgets.max_judge_calls_per_task = 1;
    });
    let outcome = run_to_outcome(&alignment).await;
    assert!(
        matches!(outcome, AgenticOutcome::WaitingForConfirmation { .. }),
        "the unchecked second order is held: {outcome:?}"
    );
    assert_eq!(
        alignment.executions.load(Ordering::SeqCst),
        1,
        "only the checked first order fires"
    );
    assert_eq!(
        allows.calls.load(Ordering::SeqCst),
        1,
        "the spent budget asks nothing more"
    );
}

/// Plan §13.3 item 10: an answer that is not a decision is unresolved, and an
/// unresolved check holds the act; it never becomes "proceed".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_judgment_holds_the_act_and_never_fires_it() {
    let mut alignment = alignment_run("enforce", vec![order_decision("chicken biryani")]).await;
    let garbage = Arc::new(AnswersGarbage {
        calls: AtomicUsize::new(0),
    });
    rejudge(&mut alignment, garbage.clone(), |_| {});
    let outcome = run_to_outcome(&alignment).await;
    assert!(
        matches!(outcome, AgenticOutcome::WaitingForConfirmation { .. }),
        "a malformed answer holds: {outcome:?}"
    );
    assert_eq!(alignment.executions.load(Ordering::SeqCst), 0);
    assert_eq!(garbage.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_order_never_reaches_its_transport() {
    let alignment = alignment_run("enforce", vec![order_decision("chicken biryani")]).await;
    let _ = hold(&alignment).await;
    assert_eq!(alignment.judge.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shadow_and_off_never_hold_and_the_order_fires_once() {
    for (mode, judged) in [("shadow", 1), ("off", 0)] {
        let alignment = alignment_run(
            mode,
            vec![order_decision("chicken biryani"), DONE.to_string()],
        )
        .await;
        let outcome = run_to_outcome(&alignment).await;
        assert!(
            matches!(outcome, AgenticOutcome::Success { .. }),
            "{mode}: the run completes, got {outcome:?}"
        );
        assert_eq!(alignment.executions.load(Ordering::SeqCst), 1, "{mode}");
        assert_eq!(
            alignment.judge.calls.load(Ordering::SeqCst),
            judged,
            "{mode}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allow_once_fires_exactly_once_and_a_lost_response_retry_fires_nothing() {
    let alignment = alignment_run(
        "enforce",
        vec![
            order_decision("chicken biryani"),
            // Resumed: the agent proposes the same order again.
            order_decision("chicken biryani"),
            DONE.to_string(),
            // The retried resume: the same order once more.
            order_decision("chicken biryani"),
        ],
    )
    .await;
    let pause_data = hold(&alignment).await;
    let retry = pause_data.clone();

    let resumed = resume(pause_data, UserInputValue::Confirmation { confirmed: true }).await;
    assert!(
        matches!(
            resumed,
            ResumeResult::Continue(AgenticOutcome::Success { .. })
        ),
        "allow once completes the run: {}",
        describe(&resumed)
    );
    assert_eq!(alignment.executions.load(Ordering::SeqCst), 1);
    assert_eq!(
        alignment.judge.calls.load(Ordering::SeqCst),
        1,
        "the owner's answer admits the act without asking the judge again"
    );

    // The same answer delivered again (a lost response, a client retry): the
    // run it resumed has ended, so the retry is refused; were it run again,
    // the used-up receipt would leave the act judged and held. Either way it
    // does not fire a second time.
    let again = resume_retry(retry, UserInputValue::Confirmation { confirmed: true }).await;
    assert!(
        matches!(
            again,
            None | Some(ResumeResult::Continue(
                AgenticOutcome::WaitingForConfirmation { .. }
            ))
        ),
        "a reused answer is refused or holds again: {:?}",
        again.as_ref().map(describe)
    );
    assert_eq!(alignment.executions.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answer_for_one_order_does_not_cover_a_changed_order() {
    let alignment = alignment_run(
        "enforce",
        vec![
            order_decision("chicken biryani"),
            // Resumed: the agent widens the act.
            order_decision("mutton biryani"),
        ],
    )
    .await;
    let pause_data = hold(&alignment).await;
    let resumed = resume(pause_data, UserInputValue::Confirmation { confirmed: true }).await;
    assert!(
        matches!(
            resumed,
            ResumeResult::Continue(AgenticOutcome::WaitingForConfirmation { .. })
        ),
        "the changed order is held again: {}",
        describe(&resumed)
    );
    assert_eq!(alignment.executions.load(Ordering::SeqCst), 0);
    assert_eq!(alignment.judge.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_and_a_late_answer_fire_nothing() {
    let alignment = alignment_run("enforce", vec![order_decision("chicken biryani")]).await;
    let pause_data = hold(&alignment).await;
    let mut late = pause_data.clone();

    let cancelled = resume(
        pause_data,
        UserInputValue::Confirmation { confirmed: false },
    )
    .await;
    assert!(
        matches!(
            cancelled,
            ResumeResult::Continue(AgenticOutcome::Failed { .. })
        ),
        "cancel ends the run: {}",
        describe(&cancelled)
    );

    // Past the deadline, even an approval is not consent.
    let ask = late.pause_state.alignment_ask.as_mut().expect("ask");
    ask.deadline_at_ms = Utc::now().timestamp_millis() - 1;
    let expired = resume(late, UserInputValue::Confirmation { confirmed: true }).await;
    assert!(
        matches!(
            expired,
            ResumeResult::Continue(AgenticOutcome::Failed { .. })
        ),
        "a late approval runs nothing: {}",
        describe(&expired)
    );
    assert_eq!(alignment.executions.load(Ordering::SeqCst), 0);
}

/// The owner's approval rule for every order, beside their preference. Set on
/// the owner's profile, where a real owner's `requires_approval` lives: run
/// setup replaces the context's rules with the profile's.
fn with_order_approval_rule(alignment: &mut AlignmentRun) {
    let rules = vec![crate::magician_v2::agents::types::ApprovalRule {
        tool: pack_tool_for_approval(PACK),
        action: crate::magician_v2::agents::types::ActionPattern::Single("*".to_string()),
        when: None,
        ttl_secs: None,
    }];
    alignment.run.executors = alignment
        .run
        .executors
        .clone()
        .with_ownership_runtime(Arc::new(ScopedLoopOwnershipRuntime {
            owner_agent_id: SCOPED_LOOP_OWNER.to_owned(),
            delegation_targets: alignment.run.ctx.delegation_targets.clone(),
            direct_tools: vec![order_tool()],
            approval_rules: rules.clone(),
        }));
    alignment.run.ctx.approval_rules = rules;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_order_needing_approval_and_review_is_asked_once_and_one_answer_settles_both() {
    let mut alignment = alignment_run(
        "enforce",
        vec![
            order_decision("chicken biryani"),
            // Resumed: the agent proposes the same order again.
            order_decision("chicken biryani"),
            DONE.to_string(),
        ],
    )
    .await;
    with_order_approval_rule(&mut alignment);
    let pause_data = hold(&alignment).await;
    let ask = pause_data
        .pause_state
        .alignment_ask
        .as_ref()
        .expect("the combined ask");
    assert_eq!(
        ask.also_covers
            .iter()
            .map(|covered| covered.gate)
            .collect::<Vec<_>>(),
        vec![crate::magician_v2::preference_alignment::ask::CoveredGate::ApprovalRule],
        "the one ask names the approval it also settles"
    );

    let resumed = resume(pause_data, UserInputValue::Confirmation { confirmed: true }).await;
    assert!(
        matches!(
            resumed,
            ResumeResult::Continue(AgenticOutcome::Success { .. })
        ),
        "one answer settles both, with no second ask: {}",
        describe(&resumed)
    );
    assert_eq!(alignment.executions.load(Ordering::SeqCst), 1);
    assert_eq!(alignment.judge.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn words_in_reply_to_a_combined_ask_approve_nothing() {
    let mut alignment = alignment_run(
        "enforce",
        vec![
            order_decision("chicken biryani"),
            // Resumed with the owner's words: the agent tries the order again.
            order_decision("chicken biryani"),
        ],
    )
    .await;
    with_order_approval_rule(&mut alignment);
    let pause_data = hold(&alignment).await;
    let resumed = resume(
        pause_data,
        UserInputValue::Text {
            value: "sure, whatever you think".to_string(),
        },
    )
    .await;
    match &resumed {
        ResumeResult::Continue(AgenticOutcome::WaitingForConfirmation { pause_state, .. }) => {
            assert!(
                pause_state
                    .alignment_ask
                    .as_ref()
                    .is_some_and(|ask| !ask.also_covers.is_empty()),
                "the re-proposed order is put to the owner as one combined ask again"
            );
        },
        other => panic!(
            "words are neither consent nor approval: {}",
            describe(other)
        ),
    }
    assert_eq!(alignment.executions.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shadow_check_never_joins_the_approval_ask() {
    let mut alignment = alignment_run(
        "shadow",
        vec![
            order_decision("chicken biryani"),
            order_decision("chicken biryani"),
            DONE.to_string(),
        ],
    )
    .await;
    with_order_approval_rule(&mut alignment);
    let outcome = run_to_outcome(&alignment).await;
    let AgenticOutcome::WaitingForConfirmation { pause_state, .. } = &outcome else {
        panic!("the approval rule pauses the order: {outcome:?}");
    };
    assert!(
        pause_state.alignment_ask.is_none(),
        "an ordinary approval pause"
    );
    assert_eq!(
        alignment.judge.calls.load(Ordering::SeqCst),
        0,
        "not judged before approval"
    );

    let resumed = resume(
        pause_data_for(&outcome, &alignment.run.executors),
        UserInputValue::Confirmation { confirmed: true },
    )
    .await;
    assert!(
        matches!(
            resumed,
            ResumeResult::Continue(AgenticOutcome::Success { .. })
        ),
        "{}",
        describe(&resumed)
    );
    assert_eq!(alignment.executions.load(Ordering::SeqCst), 1);
    assert_eq!(
        alignment.judge.calls.load(Ordering::SeqCst),
        1,
        "recorded once, at dispatch"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn free_text_issues_no_receipt_and_becomes_the_owner_turn() {
    let alignment = alignment_run(
        "enforce",
        vec![
            order_decision("chicken biryani"),
            // Resumed with the owner's words: the agent tries the order again.
            order_decision("chicken biryani"),
        ],
    )
    .await;
    let pause_data = hold(&alignment).await;
    let resumed = resume(
        pause_data,
        UserInputValue::Text {
            value: "yes go ahead".to_string(),
        },
    )
    .await;
    let pause_state = match &resumed {
        ResumeResult::Continue(AgenticOutcome::WaitingForConfirmation { pause_state, .. }) => {
            pause_state.clone()
        },
        other => panic!(
            "free text is not consent; the order is checked and held again: {}",
            describe(other)
        ),
    };
    assert_eq!(alignment.executions.load(Ordering::SeqCst), 0);
    assert_eq!(alignment.judge.calls.load(Ordering::SeqCst), 2);
    let turn = pause_state
        .alignment_owner_turn
        .expect("the owner's reply is carried as their turn");
    assert_eq!(turn.excerpt, "yes go ahead");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_choice_the_ask_did_not_offer_keeps_the_hold() {
    let alignment = alignment_run("enforce", vec![order_decision("chicken biryani")]).await;
    let pause_data = hold(&alignment).await;
    let resumed = resume(
        pause_data,
        UserInputValue::Choice {
            selected_id: "proceed_without_check".to_string(),
            other_value: None,
        },
    )
    .await;
    assert!(
        matches!(resumed, ResumeResult::ReaskRequired { .. }),
        "an unoffered choice re-asks: {}",
        describe(&resumed)
    );
    assert_eq!(alignment.executions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_shared_hitl_event_carries_no_preference_detail() {
    let broadcaster =
        Arc::new(crate::magician_v2::realtime_events::RuntimeTransportBroadcaster::new(16));
    let mut events = broadcaster.subscribe();
    let mut executors =
        ActionExecutors::new(Arc::new(MockLlmService::new(vec![])), test_prompt_manager());
    executors.event_broadcaster = Some(Arc::clone(&broadcaster));
    let mut ctx = AgenticContext::new("Order dinner", "done");
    ctx.principal = Some("owner".to_string());
    ctx.workspace = Some("default".to_string());
    ctx.execution_id = Some("exec-shared".to_string());
    let ask = crate::magician_v2::preference_alignment::ask::AlignmentAsk::for_hold(
        "ask-1".to_string(),
        "alignment_binding.v1:x".to_string(),
        Some("exec-shared".to_string()),
        vec!["pref-meat".to_string()],
        &[ResolutionChoice::UseAlternative],
        0,
        60,
        true,
        // The owner-private detail rides with the ask and must not leak.
        Some(crate::magician_v2::preference_alignment::ask::AskDetail {
            explanation: "This order includes meat, and you asked to be checked first.".to_string(),
            act_summary: "place a meat order".to_string(),
            intentions: Vec::new(),
            remember: None,
        }),
    );
    emit_hitl_requested_for_agentic_confirmation(
        &ctx,
        &executors,
        None,
        "pause~ask-1",
        "Check with you first: pack",
        "This order includes meat, and you asked to be checked first.",
        "pack",
        Some(&ask),
    );
    let mut seen = false;
    while let Ok(event) = events.try_recv() {
        if let RuntimeTransportEvent::HitlRequested {
            prompt,
            input_schema,
            ..
        } = event
        {
            seen = true;
            assert_eq!(prompt, GENERIC_ASK_TEXT);
            let schema = input_schema.expect("schema");
            let text = schema.to_string();
            assert!(!text.contains("meat"), "{text}");
            assert!(!text.contains("pref-meat"), "{text}");
            assert_eq!(schema["escalation_trigger"], "preference_alignment");
            assert_eq!(schema["alignment"]["ask_id"], "ask-1");
            // Background work is time-bound for attention delivery.
            assert!(schema.get("collection_deadline_ms").is_some());
        }
    }
    assert!(seen, "the ask is published");
}

/// P7.1 through the production resume: "change my preference" writes nothing
/// until the exact wording is confirmed with the private readback's token,
/// the write supersedes the saved preference with the owner's words, and the
/// held order still never fires — saving a preference is not consent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_preference_is_saved_only_from_the_confirmed_readback() {
    use crate::magician_v2::preference_alignment::ask::{AskStage, LastingWriteKind};

    let alignment = alignment_run(
        "enforce",
        vec![
            order_decision("chicken biryani"),
            // After the change is saved the agent proposes the order again.
            order_decision("chicken biryani"),
        ],
    )
    .await;
    let pause_data = hold(&alignment).await;
    let ask = pause_data.pause_state.alignment_ask.clone().expect("ask");
    let detail = ask
        .detail
        .clone()
        .expect("the hold keeps its private detail");
    assert!(detail.explanation.contains("meat"));
    assert!(
        detail.intentions.iter().any(|intention| intention
            .statement
            .contains("Check with me before ordering meat")),
        "the cited preference is in the private detail: {detail:?}"
    );
    assert!(ask
        .choices
        .iter()
        .any(|choice| choice.id == "change_preference"));

    let new_words = "Only check with me before ordering meat on weekdays.";
    let staged = match resume(
        pause_data,
        UserInputValue::Choice {
            selected_id: "change_preference".to_string(),
            other_value: Some(new_words.to_string()),
        },
    )
    .await
    {
        ResumeResult::ReaskRequired { pause_data, .. } => pause_data,
        other => panic!("a change opens the wording stage: {}", describe(&other)),
    };
    let staged_ask = staged
        .pause_state
        .alignment_ask
        .clone()
        .expect("staged ask");
    assert_eq!(staged_ask.stage, AskStage::ConfirmWording);
    let write = staged_ask.lasting_write.clone().expect("the readback");
    assert_eq!(write.kind, LastingWriteKind::Replace);
    assert_eq!(write.wording, new_words);

    let resolver = crate::magician_v2::agents::AgentMemoryResolver::new(
        alignment.run.executors.artifact_v2_workspace.base_root(),
    );
    let resolver = &resolver;
    let saved_entries = || async move {
        let memory = resolver
            .resolve_for_scope(SCOPED_LOOP_PRINCIPAL, SCOPED_LOOP_WORKSPACE)
            .expect("owner memory");
        let knowledge = memory.load_user_knowledge().await.expect("knowledge");
        knowledge["preferences"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| entry["key"] == "meat_orders")
            .collect::<Vec<_>>()
    };
    assert_eq!(saved_entries().await.len(), 1);

    // A bare approve on the wording stage — what a shared surface can send —
    // carries no token and saves nothing.
    let bare = resume(
        staged.clone(),
        UserInputValue::Confirmation { confirmed: true },
    )
    .await;
    assert!(
        matches!(bare, ResumeResult::ReaskRequired { .. }),
        "a bare approve saves nothing: {}",
        describe(&bare)
    );
    assert_eq!(saved_entries().await.len(), 1);

    let confirmed = resume(
        staged,
        UserInputValue::Choice {
            selected_id: "confirm_wording".to_string(),
            other_value: Some(write.token.clone()),
        },
    )
    .await;
    assert!(
        matches!(
            confirmed,
            ResumeResult::Continue(AgenticOutcome::WaitingForConfirmation { .. })
        ),
        "the order is checked again against what was saved: {}",
        describe(&confirmed)
    );
    assert_eq!(
        alignment.executions.load(Ordering::SeqCst),
        0,
        "saving a preference never runs the held order"
    );
    let entries = saved_entries().await;
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert!(entries
        .iter()
        .any(|entry| entry["memory_lifecycle"] == "superseded"));
    let successor = entries
        .iter()
        .find(|entry| entry["value"] == new_words)
        .expect("the owner's words are saved");
    assert_eq!(
        crate::magician_v2::preference_alignment::sources::native::assertion_status(successor),
        crate::magician_v2::preference_alignment::intention::AssertionStatus::DirectOwnerStatement
    );
}

/// A pending-pause listing reaches plane `wait` callers and every client, so
/// for a hold it carries the generic prompt, never the private reason.
#[test]
fn a_pending_listing_of_a_hold_carries_only_the_generic_prompt() {
    let mut info = PendingPauseInfo::for_test(
        "exec-1:plan:step",
        "exec-1",
        UserInputType::Confirmation {
            confirm_label: Some("Allow this once".to_string()),
            deny_label: Some("Cancel".to_string()),
            destructive: false,
        },
    );
    let private = "This order includes meat, and you asked to be checked first.";
    info.question = Some(private.to_string());
    info.confirmation_reason = Some(private.to_string());
    info.hint = Some(private.to_string());
    info.previous_answer = Some(private.to_string());
    let ordinary = info.clone().into_user_facing();
    assert_eq!(
        ordinary.question.as_deref(),
        Some(private),
        "only holds are scrubbed"
    );

    info.alignment_hold = true;
    let listed = info.into_user_facing();
    assert_eq!(listed.question.as_deref(), Some(GENERIC_ASK_TEXT));
    assert_eq!(
        listed.confirmation_reason.as_deref(),
        Some(GENERIC_ASK_TEXT)
    );
    assert_eq!(listed.hint, None);
    assert_eq!(listed.previous_answer, None);
}

/// P5.5: a hold survives a restart. The persisted pause index carries the
/// typed ask, so after a new process loads it the hold is still readable for
/// the owner's private surfaces, found by the deadline sweep, listed
/// generically, and its body still carries the ask a resume resolves against.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hold_survives_a_restart_readable_expirable_and_answerable() {
    let alignment = alignment_run("enforce", vec![order_decision("chicken biryani")]).await;
    let pause_data = hold(&alignment).await;
    let ask = pause_data.pause_state.alignment_ask.clone().expect("ask");
    let execution_id = pause_data
        .pause_state
        .execution_id
        .clone()
        .expect("the hold names its run");

    let tmp = tempfile::tempdir().expect("tempdir");
    let writer = FullPauseStore::with_scoped_v3_persistence_root(tmp.path());
    let key = writer
        .store_durable(pause_data)
        .expect("the hold is durable");
    drop(writer);

    // A new process: nothing in memory, only what was persisted.
    let restarted = FullPauseStore::with_scoped_v3_persistence_root(tmp.path());
    restarted
        .load_pending_index_from_disk()
        .expect("the index loads");

    let snapshot = restarted
        .alignment_ask_snapshot(&key)
        .expect("the hold is readable after a restart");
    assert_eq!(
        snapshot.ask, ask,
        "the typed ask, detail included, survived"
    );
    assert_eq!(snapshot.principal.as_deref(), Some(SCOPED_LOOP_PRINCIPAL));
    assert_eq!(
        snapshot.execution_id.as_deref(),
        Some(execution_id.as_str())
    );

    let holds = restarted.alignment_hold_snapshots();
    assert_eq!(
        holds.len(),
        1,
        "the deadline sweep finds the recovered hold"
    );
    assert_eq!(holds[0].ask.deadline_at_ms, ask.deadline_at_ms);

    // The listing every client and plane `wait` reads stays generic.
    let listed = restarted.get_pending_for_execution(&execution_id);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].question.as_deref(), Some(GENERIC_ASK_TEXT));
    assert!(listed[0].alignment_ask.is_none(), "the ask is internal");

    // The body a resume takes still carries the ask.
    let body = restarted
        .take_serializable_by_key_from_disk_scoped(
            &key,
            Some((SCOPED_LOOP_PRINCIPAL, SCOPED_LOOP_WORKSPACE)),
        )
        .expect("readable")
        .expect("the body is on disk");
    assert_eq!(body.pause_state.alignment_ask, Some(ask));
}

// ── Holds inside a harness turn, through the real plane `tools/call` ──

fn order_arguments(item: &str) -> serde_json::Value {
    serde_json::json!({"action": "place_food_order", "items": item})
}

/// A live harness turn's grant over this run: the run's context and
/// executors (scripted judge, counting provider), its tool index and loaded
/// pack, and optionally the owner's approval rule for every order.
fn harness_turn_grant(
    alignment: &AlignmentRun,
    approval_rule: bool,
) -> crate::magician_v2::execution::plane::grant::PlaneGrant {
    let execution_id = alignment
        .run
        .ctx
        .execution_id
        .clone()
        .unwrap_or_else(|| "exec-plane-hold".to_string());
    let mut grant =
        crate::magician_v2::execution::plane::grant::PlaneGrant::for_test(&execution_id)
            .with_live_harness_turn();
    grant.ctx = alignment.run.ctx.clone();
    // Minted onto the plane surface, as every run grant is.
    if let Some(invocation) = grant.ctx.invocation_context_override.as_mut() {
        invocation.surface = crate::magician_v2::agents::InvocationSurface::Plane;
    }
    grant.executors = Some(Arc::new(alignment.run.executors.clone()));
    if let Some(index) = alignment.run.ctx.tool_index.clone() {
        grant = grant.with_tool_index(index);
    }
    if approval_rule {
        grant = grant.with_approval_rule_for(PACK);
    }
    grant
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_harness_order_is_held_in_its_turn_and_never_reaches_its_transport() {
    let alignment = alignment_run("enforce", Vec::new()).await;
    let grant = harness_turn_grant(&alignment, false);
    let result = crate::magician_v2::execution::plane::plane_tools_call(
        &grant,
        PACK,
        &order_arguments("chicken biryani"),
    )
    .await;
    assert_eq!(result["isError"], serde_json::json!(true), "{result}");
    assert_eq!(
        result["_meta"]["planeTurnStop"],
        serde_json::json!("needs_approval"),
        "the turn ends so the owner can answer: {result}"
    );
    assert!(
        !result.to_string().contains("meat"),
        "the harness is told only that a review is needed: {result}"
    );
    let pending = grant
        .take_pending_approval()
        .expect("the held order is captured for execute-on-resume");
    let seed = pending.alignment.expect("the capture carries the hold");
    assert!(seed.also_covers.is_empty());
    assert_eq!(alignment.executions.load(Ordering::SeqCst), 0);
    assert_eq!(alignment.judge.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_harness_order_needing_approval_and_review_is_captured_as_one_ask() {
    let alignment = alignment_run("enforce", Vec::new()).await;
    let grant = harness_turn_grant(&alignment, true);
    let result = crate::magician_v2::execution::plane::plane_tools_call(
        &grant,
        PACK,
        &order_arguments("chicken biryani"),
    )
    .await;
    assert_eq!(
        result["_meta"]["planeTurnStop"],
        serde_json::json!("needs_approval"),
        "{result}"
    );
    let pending = grant.take_pending_approval().expect("captured");
    let seed = pending
        .alignment
        .expect("the approval capture carries the hold too");
    assert_eq!(
        seed.also_covers
            .iter()
            .map(|covered| covered.gate)
            .collect::<Vec<_>>(),
        vec![crate::magician_v2::preference_alignment::ask::CoveredGate::PlaneApproval]
    );
    assert_eq!(alignment.executions.load(Ordering::SeqCst), 0);
    assert_eq!(alignment.judge.calls.load(Ordering::SeqCst), 1);
}

/// The owner allows the captured order once: resume consumes the recorded
/// answer, admits exactly that act and runs it once; the same answer again
/// runs nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_harness_hold_allowed_once_runs_the_captured_order_exactly_once() {
    use crate::magician_v2::preference_alignment::receipts::receipt_token;

    let alignment = alignment_run(
        "enforce",
        vec![
            // The run's own pause, which the harness capture is put into.
            order_decision("chicken biryani"),
            DONE.to_string(),
            DONE.to_string(),
        ],
    )
    .await;
    let grant = harness_turn_grant(&alignment, false);
    let _ = crate::magician_v2::execution::plane::plane_tools_call(
        &grant,
        PACK,
        &order_arguments("paneer biryani"),
    )
    .await;
    let pending = grant.take_pending_approval().expect("captured");
    let seed = pending.alignment.clone().expect("held");

    // The pause the loop raises for the capture, as the apply phase builds
    // it for a harness hold: the seed's typed ask, the alignment receipt over
    // the captured act, and the harness continuation.
    let mut pause_data = hold(&alignment).await;
    let mut ask = seed.ask(
        pause_data.pause_state.execution_id.clone(),
        Utc::now().timestamp_millis(),
        3_600,
        false,
    );
    ask.ask_id = pause_data.pause_state.ask_id.clone().expect("ask id");
    pause_data.pause_state.alignment_ask = Some(ask);
    let token = receipt_token(seed.purpose, &pending.action_json).expect("token");
    let pause_data = pause_data
        .with_confirmation_action_json(Some(token))
        .with_harness_pause(Some(
            crate::magician_v2::execution::plane::turn_engine::HarnessPauseContinuation {
                action_json: Some(pending.action_json.clone()),
                action_summary: Some(pending.action_summary.clone()),
                action_type: Some(pending.action_type.clone()),
                native_session_id: None,
                session_fingerprint: None,
                turn_tool_calls_spent: 0,
                turn_wall_clock_spent_ms: 0,
                ..Default::default()
            },
        ));
    let retry = pause_data.clone();

    let resumed = resume(pause_data, UserInputValue::Confirmation { confirmed: true }).await;
    assert_eq!(
        alignment.executions.load(Ordering::SeqCst),
        1,
        "the captured order runs once: {}",
        describe(&resumed)
    );

    let again = resume_retry(retry, UserInputValue::Confirmation { confirmed: true }).await;
    assert_eq!(
        alignment.executions.load(Ordering::SeqCst),
        1,
        "a reused answer runs nothing: {:?}",
        again.as_ref().map(describe)
    );
}

//! Clarification-answer interpretation on the decision engine.
//!
//! The answer-interpretation LLM call decides a reply's `answer_type` (six
//! closed labels) and `requires_replan`, and generates the corrected value for
//! a correction. Its readers: the ask-loop clarifier (slot value and
//! confidence, resume mode, confidence trigger) and the planning pipeline
//! (router rules 2/3 on `requires_replan`). Nothing reads its free-text
//! `clarification_text`, `rejection_reason` or `reasoning`; `slots` is always
//! empty (the prompt leaves it to the caller).
//!
//! The LLM stays the incumbent. In shadow (or with the engine off) it decides
//! every answer and the engine's answer is recorded beside it. When the
//! operation is gated and the engine answers both heads eligibly, the LLM call
//! is skipped, except for a correction, which still needs the LLM's corrected
//! value. Every other outcome (no scope, no reference binding, an engine
//! outage, an unanswered head) runs the LLM exactly as before. The engine never
//! sees or selects the LLM.

use std::{collections::BTreeMap, future::Future, sync::Arc, time::Duration};

use decision_engine_contract::{
    batch::DecisionItem,
    request::{Answer, DecisionState},
};
use serde_json::json;

use super::answer_interpreter::{AnswerType, InterpretedAnswer, InterpreterError};
use crate::magician_v2::{
    analytics::operation_llm_telemetry::OperationLlmTelemetryScope,
    decision_host::classification::{self, PolicyLookup},
    decisions::{reference, runner, telemetry::Reference},
    prompts::constants::versions,
    query_analysis::operation_llm_router::OperationLlmRouter,
};

pub const DECISION_ID: &str = "answer_interpretation";
pub const PROJECTION: &str = "answer_interpretation_v1";
const ITEM: &str = "answer";
/// Never cut the incumbent: the LLM operation has its own timeout. A gated
/// engine is capped by the operation's decision and queue budgets.
const OUTER: Duration = Duration::from_secs(600);
const QUESTION_CHARS: usize = 1_000;
const ANSWER_CHARS: usize = 2_000;
const CONTEXT_CHARS: usize = 1_500;

/// Which incumbent the engine is compared against: the router and the LLM
/// operation that actually serves this interpreter's calls.
#[derive(Clone)]
pub struct AnswerDecisionBinding {
    pub router: Arc<OperationLlmRouter>,
    pub reference_operation: &'static str,
}

/// Whose answer this is, for the engine's policy and attribution.
#[derive(Debug, Clone, Default)]
pub struct AnswerDecisionScope {
    pub principal: String,
    pub workspace: String,
    pub execution_id: Option<String>,
    pub task_id: Option<String>,
    pub agent_id: Option<String>,
}

impl AnswerDecisionScope {
    pub fn from_telemetry(scope: &OperationLlmTelemetryScope) -> Self {
        Self {
            principal: scope.principal.clone(),
            workspace: scope.workspace.clone(),
            execution_id: scope.attribution.execution_id.clone(),
            task_id: scope.attribution.task_id.clone(),
            agent_id: scope.attribution.agent_id.clone(),
        }
    }
}

/// The LLM's interpretation, whether it parsed as the prompt's JSON (a text
/// fallback is not a label), and how long the call took.
pub struct LlmInterpretation {
    pub answer: InterpretedAnswer,
    pub structured: bool,
    pub latency_ms: u64,
}

type LlmResult = Result<LlmInterpretation, InterpreterError>;

/// Interpret one reply: engine first when gated, the LLM otherwise.
pub async fn interpret<F, Fut>(
    binding: Option<&AnswerDecisionBinding>,
    scope: Option<&AnswerDecisionScope>,
    question: &str,
    answer: &str,
    context: Option<&str>,
    llm: F,
) -> Result<InterpretedAnswer, InterpreterError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = LlmResult>,
{
    let engine = match (binding, scope) {
        (Some(binding), Some(scope)) => {
            engine_input(binding, scope, question, answer, context).await
        },
        _ => None,
    };
    let Some((input, lookup)) = engine else {
        return llm().await.map(|call| call.answer);
    };
    decide(input, lookup, llm).await
}

/// Run the incumbent and the engine under one policy lookup.
async fn decide<F, Fut>(
    input: runner::Input,
    lookup: PolicyLookup,
    llm: F,
) -> Result<InterpretedAnswer, InterpreterError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = LlmResult>,
{
    if matches!(&lookup, PolicyLookup::Participating(p) if p.policy.gate) {
        let outcome = runner::run(
            input,
            lookup,
            OUTER,
            Duration::ZERO,
            |_, _| async { None::<LlmResult> },
            labels,
        )
        .await;
        let answers = outcome.current_answers().remove(ITEM).unwrap_or_default();
        return match resolve_without_llm(&answers) {
            Ok(resolved) => Ok(resolved),
            Err(reason) => {
                tracing::debug!(
                    reason,
                    "[ANSWER-INTERPRETER] decision engine left the answer to the LLM"
                );
                llm().await.map(|call| call.answer)
            },
        };
    }
    // An engine outage never leaves a clarification answer uninterpreted.
    if !lookup.allows_incumbent() {
        return llm().await.map(|call| call.answer);
    }
    let outcome = runner::run(
        input,
        lookup,
        OUTER,
        Duration::ZERO,
        |_, _| async move { Some(llm().await) },
        labels,
    )
    .await;
    match outcome.incumbent {
        Some(call) => call.map(|call| call.answer),
        None => Err(InterpreterError::LLMFailed(
            "answer interpretation did not complete".to_owned(),
        )),
    }
}

async fn engine_input(
    binding: &AnswerDecisionBinding,
    scope: &AnswerDecisionScope,
    question: &str,
    answer: &str,
    context: Option<&str>,
) -> Option<(runner::Input, PolicyLookup)> {
    let principal = scope.principal.trim();
    let workspace = scope.workspace.trim();
    if principal.is_empty() || workspace.is_empty() {
        return None;
    }
    let reference_version = reference::version(
        &binding.router,
        binding.reference_operation,
        versions::ANSWER_INTERPRETATION,
    )?;
    let lookup = classification::ready_policy(DECISION_ID, principal, workspace).await;
    let item = project(question, answer, context);
    let case_id = blake3::hash(&serde_json::to_vec(&item).ok()?)
        .to_hex()
        .to_string();
    let mut trace = binding
        .router
        .classification_trace_context(magicllm::LlmScope::new(
            principal.to_owned(),
            workspace.to_owned(),
        ));
    trace.execution_id = scope.execution_id.clone().or(trace.execution_id);
    trace.task_id = scope.task_id.clone().or(trace.task_id);
    Some((
        runner::Input {
            operation: DECISION_ID.into(),
            projection_version: PROJECTION.into(),
            reference_version,
            case_id,
            context: None,
            items: vec![item],
            required_questions: vec!["answer_type".into(), "requires_replan".into()],
            scope: trace,
            agent: scope.agent_id.clone(),
            requires_completion: false,
            replay: None,
        },
        lookup,
    ))
}

fn clip(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

pub fn project(question: &str, answer: &str, context: Option<&str>) -> DecisionItem {
    DecisionItem {
        item_id: ITEM.into(),
        state: DecisionState::from_json(json!({
            "question": clip(question, QUESTION_CHARS),
            "answer": clip(answer, ANSWER_CHARS),
            "context": clip(context.unwrap_or(""), CONTEXT_CHARS),
        })),
        choice_candidates: BTreeMap::new(),
    }
}

/// The interpretation from the engine alone. Both heads must be eligible, and
/// a correction still needs the LLM's corrected value.
pub fn resolve_without_llm(
    answers: &BTreeMap<String, Answer>,
) -> Result<InterpretedAnswer, &'static str> {
    let (answer_type, confidence) = match answers.get("answer_type") {
        Some(Answer::Choice {
            choice, confidence, ..
        }) => (
            answer_type_from_id(choice.as_str()).ok_or("answer_type_unresolved")?,
            *confidence,
        ),
        _ => return Err("answer_type_unresolved"),
    };
    if answer_type == AnswerType::Correction {
        return Err("correction_needs_llm");
    }
    let requires_replan = match answers.get("requires_replan") {
        Some(Answer::Choice { choice, .. }) => {
            replan_from_id(choice.as_str()).ok_or("requires_replan_unresolved")?
        },
        _ => return Err("requires_replan_unresolved"),
    };
    Ok(InterpretedAnswer {
        answer_type,
        slots: Vec::new(),
        correction_target: None,
        correction_value: None,
        clarification_text: None,
        rejection_reason: None,
        confidence: confidence.clamp(0.0, 1.0),
        requires_replan,
        reasoning: "Interpreted by the decision engine (answer_interpretation).".to_owned(),
    })
}

/// The incumbent's labels in the pack's option ids. A reply that did not
/// parse as the prompt's JSON is a safe default, not a label.
fn labels(call: &LlmResult) -> Reference {
    let Ok(call) = call else {
        return BTreeMap::new().into();
    };
    let mut reference: Reference = if call.structured {
        BTreeMap::from([(
            ITEM.to_owned(),
            BTreeMap::from([
                (
                    "answer_type".to_owned(),
                    json!(answer_type_id(call.answer.answer_type)),
                ),
                (
                    "requires_replan".to_owned(),
                    json!(replan_id(call.answer.requires_replan)),
                ),
            ]),
        )])
        .into()
    } else {
        BTreeMap::new().into()
    };
    reference.latency_ms = call.latency_ms;
    reference
}

pub fn answer_type_id(value: AnswerType) -> &'static str {
    match value {
        AnswerType::DirectAnswer => "direct_answer",
        AnswerType::Correction => "correction",
        AnswerType::Clarification => "clarification",
        AnswerType::Rejection => "rejection",
        AnswerType::MultiValue => "multi_value",
        AnswerType::Ambiguous => "ambiguous",
    }
}
fn answer_type_from_id(id: &str) -> Option<AnswerType> {
    Some(match id {
        "direct_answer" => AnswerType::DirectAnswer,
        "correction" => AnswerType::Correction,
        "clarification" => AnswerType::Clarification,
        "rejection" => AnswerType::Rejection,
        "multi_value" => AnswerType::MultiValue,
        "ambiguous" => AnswerType::Ambiguous,
        _ => return None,
    })
}
pub fn replan_id(value: bool) -> &'static str {
    if value {
        "replan"
    } else {
        "keep_plan"
    }
}
fn replan_from_id(id: &str) -> Option<bool> {
    match id {
        "replan" => Some(true),
        "keep_plan" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::decisions::test_engine::{self, FakeEngine};
    use decision_engine_contract::primitives::OptionId;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn choice(id: &str, confidence: f64) -> Answer {
        Answer::Choice {
            choice: OptionId::new(id),
            probabilities: BTreeMap::new(),
            confidence,
        }
    }

    fn llm_answer(answer_type: AnswerType, requires_replan: bool) -> LlmInterpretation {
        LlmInterpretation {
            answer: InterpretedAnswer {
                answer_type,
                slots: Vec::new(),
                correction_target: None,
                correction_value: Some("WhatsApp".into()),
                clarification_text: None,
                rejection_reason: None,
                confidence: 0.9,
                requires_replan,
                reasoning: "llm".into(),
            },
            structured: true,
            latency_ms: 42,
        }
    }

    #[test]
    fn option_ids_round_trip() {
        for value in [
            AnswerType::DirectAnswer,
            AnswerType::Correction,
            AnswerType::Clarification,
            AnswerType::Rejection,
            AnswerType::MultiValue,
            AnswerType::Ambiguous,
        ] {
            assert_eq!(answer_type_from_id(answer_type_id(value)), Some(value));
        }
        for value in [true, false] {
            assert_eq!(replan_from_id(replan_id(value)), Some(value));
        }
    }

    #[test]
    fn engine_resolves_both_heads_except_a_correction() {
        let answers = BTreeMap::from([
            ("answer_type".to_owned(), choice("multi_value", 0.91)),
            ("requires_replan".to_owned(), choice("keep_plan", 0.88)),
        ]);
        let resolved = resolve_without_llm(&answers).expect("resolves");
        assert_eq!(resolved.answer_type, AnswerType::MultiValue);
        assert!(!resolved.requires_replan);
        assert_eq!(resolved.confidence, 0.91);
        assert!(resolved.correction_value.is_none() && resolved.slots.is_empty());

        let correction = BTreeMap::from([
            ("answer_type".to_owned(), choice("correction", 0.99)),
            ("requires_replan".to_owned(), choice("replan", 0.99)),
        ]);
        assert_eq!(
            resolve_without_llm(&correction).err(),
            Some("correction_needs_llm")
        );
        assert_eq!(
            resolve_without_llm(&BTreeMap::from([(
                "answer_type".to_owned(),
                choice("rejection", 0.9)
            )]))
            .err(),
            Some("requires_replan_unresolved")
        );
        assert_eq!(
            resolve_without_llm(&BTreeMap::new()).err(),
            Some("answer_type_unresolved")
        );
    }

    #[test]
    fn labels_skip_text_fallback_replies() {
        let reference = labels(&Ok(llm_answer(AnswerType::Clarification, true)));
        assert_eq!(reference.labels[ITEM]["answer_type"], "clarification");
        assert_eq!(reference.labels[ITEM]["requires_replan"], "replan");
        assert_eq!(reference.latency_ms, 42);
        let mut fallback = llm_answer(AnswerType::DirectAnswer, false);
        fallback.structured = false;
        assert!(labels(&Ok(fallback)).labels.is_empty());
    }

    #[test]
    fn projection_is_bounded() {
        let item = project(&"q".repeat(5_000), "Slack", Some(&"c".repeat(9_000)));
        assert_eq!(
            item.state.0["question"].as_str().unwrap().len(),
            QUESTION_CHARS
        );
        assert_eq!(item.state.0["answer"], "Slack");
        assert_eq!(
            item.state.0["context"].as_str().unwrap().len(),
            CONTEXT_CHARS
        );
    }

    fn binding() -> AnswerDecisionBinding {
        AnswerDecisionBinding {
            router: Arc::new(test_engine::fixture_router(DECISION_ID)),
            reference_operation: DECISION_ID,
        }
    }

    fn scope() -> AnswerDecisionScope {
        AnswerDecisionScope {
            principal: "p".into(),
            workspace: "w".into(),
            ..Default::default()
        }
    }

    async fn run(
        calls: &AtomicUsize,
        llm_type: AnswerType,
    ) -> Result<InterpretedAnswer, InterpreterError> {
        interpret(
            Some(&binding()),
            Some(&scope()),
            "Which messaging app?",
            "Slack",
            Some("the team chat"),
            || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(llm_answer(llm_type, false))
            },
        )
        .await
    }

    #[tokio::test]
    async fn without_scope_or_engine_the_llm_decides() {
        let _serial = test_engine::serial().await;
        let calls = AtomicUsize::new(0);
        let answer = interpret(None, None, "q", "a", None, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(llm_answer(AnswerType::Rejection, false))
        })
        .await
        .expect("llm answers");
        assert_eq!(answer.answer_type, AnswerType::Rejection);
        // Engine off: the policy is Disabled and the LLM still decides.
        let answer = run(&calls, AnswerType::Ambiguous)
            .await
            .expect("llm answers");
        assert_eq!(answer.answer_type, AnswerType::Ambiguous);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn shadow_keeps_the_llm_answer_and_asks_the_engine_beside_it() {
        let _serial = test_engine::serial().await;
        let engine = FakeEngine::start(
            DECISION_ID,
            false,
            &[
                ("answer_type", "direct_answer", 0.95),
                ("requires_replan", "keep_plan", 0.95),
            ],
        )
        .await;
        let calls = AtomicUsize::new(0);
        let answer = run(&calls, AnswerType::Clarification)
            .await
            .expect("answers");
        assert_eq!(
            answer.answer_type,
            AnswerType::Clarification,
            "the LLM decides"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let request = engine.wait_for_request().await;
        assert_eq!(request.operation, DECISION_ID);
        assert_eq!(request.batch.projection_version, PROJECTION);
        assert_eq!(request.batch.items[0].state.0["answer"], "Slack");
    }

    #[tokio::test]
    async fn gate_skips_the_llm_on_eligible_heads_and_falls_back_otherwise() {
        let _serial = test_engine::serial().await;
        let calls = AtomicUsize::new(0);
        {
            let _engine = FakeEngine::start(
                DECISION_ID,
                true,
                &[
                    ("answer_type", "direct_answer", 0.95),
                    ("requires_replan", "keep_plan", 0.9),
                ],
            )
            .await;
            let answer = run(&calls, AnswerType::Ambiguous).await.expect("answers");
            assert_eq!(answer.answer_type, AnswerType::DirectAnswer);
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "engine resolved: no LLM call"
            );
        }
        {
            let _engine = FakeEngine::start(
                DECISION_ID,
                true,
                &[
                    ("answer_type", "direct_answer", 0.95),
                    ("requires_replan", "keep_plan", 0.5),
                ],
            )
            .await;
            let answer = run(&calls, AnswerType::Ambiguous).await.expect("answers");
            assert_eq!(
                answer.answer_type,
                AnswerType::Ambiguous,
                "below threshold: LLM"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
        {
            let _engine = FakeEngine::start(
                DECISION_ID,
                true,
                &[
                    ("answer_type", "correction", 0.99),
                    ("requires_replan", "replan", 0.99),
                ],
            )
            .await;
            let answer = run(&calls, AnswerType::Correction).await.expect("answers");
            assert_eq!(answer.correction_value.as_deref(), Some("WhatsApp"));
            assert_eq!(
                calls.load(Ordering::SeqCst),
                2,
                "a correction needs the LLM's value"
            );
        }
    }
}

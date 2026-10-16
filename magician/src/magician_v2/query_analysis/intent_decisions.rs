//! Per-turn intent, task clarity and complexity bucket on the decision engine.
//!
//! On the `legacy` analysis path the unified query-analysis LLM call stays the
//! incumbent. In shadow (or with the engine off) it answers everything, as
//! before. When `query_intent` is gated, the engine and the LLM run
//! concurrently: the LLM still supplies extraction and every other field, and
//! only the engine-resolved fields are taken from the engine.
//!
//! On the `new` path the gated engine decides first while the LLM call starts
//! beside it. When the engine answers all three heads eligibly and the
//! message is not a slot answer, the LLM call is dropped (cancelled) and the
//! turn's analysis is built from those answers; extraction is left
//! to the slot extractor (`ExtractionSource::Deferred`). Otherwise the LLM
//! runs exactly as on the legacy path and the engine's eligible answers are
//! applied to it, so a fallback turn equals a legacy turn.
//!
//! The `shadow` path returns the legacy result and records what `new` would
//! have returned next to it. The engine never sees or selects the LLM.

use std::{
    collections::BTreeMap,
    future::Future,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use decision_engine_contract::{
    batch::DecisionItem,
    request::{Answer, DecisionState},
};
use runtime_core::ExecutionContext;
use serde::Serialize;
use serde_json::json;

use super::{
    CategoryAnalysis, ComplexityAnalysis, ConversationContext, DependencyAnalysis,
    ExtractedEntities, ExtractionSource, TaskClarity, UnifiedAnalysisResult,
};
use crate::magician_v2::{
    decision_host::classification::{self, PolicyLookup},
    decisions::{reference, runner, telemetry::Reference},
    prompts::constants::versions,
    query_analysis::{
        analysis_path::{self, AnalysisPath, CallCost},
        intent::QueryIntent,
        operation_llm_router::{LLMOperation, OperationLlmRouter, SimplifiedLLMResponse},
    },
    storage::TurnDirection,
};

const DECISION_ID: &str = "query_intent";
const PROJECTION: &str = "query_intent_v1";
const ITEM: &str = "turn";
/// Never cut the incumbent: the LLM operation has its own timeout. A gated
/// engine is capped by the operation's decision and queue budgets.
const OUTER: Duration = Duration::from_secs(600);
const TURN_TEXT_CHARS: usize = 600;

type Call = (UnifiedAnalysisResult, SimplifiedLLMResponse, u64);

/// The three closed-set heads a planning turn's analysis feeds downstream,
/// plus whether a slot match came with them.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Heads {
    pub intent: &'static str,
    pub task_clarity: &'static str,
    pub complexity_bucket: &'static str,
    pub complexity_score: f32,
    pub slot_match: bool,
}

impl Heads {
    fn of(result: &UnifiedAnalysisResult) -> Self {
        Self {
            intent: intent_id(result.intent.as_ref().unwrap_or(&QueryIntent::NewTask)),
            task_clarity: clarity_id(result.task_clarity),
            complexity_bucket: bucket_id(result.complexity.score),
            complexity_score: result.complexity.score,
            slot_match: result.slot_match.is_some(),
        }
    }
}

/// The unified LLM call's own answers, before any engine answer is applied.
#[derive(Debug, Clone, Serialize)]
pub struct LlmAnswers {
    pub heads: Heads,
    pub extraction_confidence: f32,
    pub entity_count: usize,
    pub cost: CallCost,
}

/// Whether the new path can answer this turn without the LLM.
#[derive(Debug, Clone, Serialize)]
pub struct NewPathResolution {
    pub resolved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heads: Option<Heads>,
}

/// What one turn's analysis did, for the shadow record and offline evals.
#[derive(Debug, Clone, Serialize)]
pub struct AnalysisTrace {
    pub path: AnalysisPath,
    /// `context_free`, `no_scope`, `not_participating`, `shadow_policy` or `gated`.
    pub engine: &'static str,
    /// Eligible engine answers: question -> (choice, confidence).
    pub engine_answers: BTreeMap<String, (String, f64)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm: Option<LlmAnswers>,
    pub new_path: NewPathResolution,
    /// The heads this turn actually used.
    pub result: Heads,
}

impl AnalysisTrace {
    fn new(path: AnalysisPath, engine: &'static str) -> Self {
        Self {
            path,
            engine,
            engine_answers: BTreeMap::new(),
            engine_latency_ms: None,
            llm: None,
            new_path: NewPathResolution {
                resolved: false,
                fallback_reason: Some(engine_fallback_reason(engine)),
                heads: None,
            },
            result: Heads {
                intent: "new_task",
                task_clarity: "needs_elicitation",
                complexity_bucket: "simple",
                complexity_score: 0.0,
                slot_match: false,
            },
        }
    }

    fn llm_called(&mut self, call: &Call) {
        let (result, response, latency_ms) = call;
        self.llm = Some(LlmAnswers {
            heads: Heads::of(result),
            extraction_confidence: result.extracted_entities.extraction_confidence,
            entity_count: result.extracted_entities.entities.len(),
            cost: CallCost::from_telemetry(*latency_ms, response.telemetry.as_ref()),
        });
    }

    fn engine_answered(&mut self, answers: &BTreeMap<String, Answer>, latency: Duration) {
        self.engine_latency_ms = Some(latency.as_millis() as u64);
        self.engine_answers = answers
            .iter()
            .filter_map(|(question, answer)| match answer {
                Answer::Choice {
                    choice, confidence, ..
                } => Some((question.clone(), (choice.as_str().to_owned(), *confidence))),
                _ => None,
            })
            .collect();
        self.new_path = match resolve_without_llm(answers) {
            Ok(result) => NewPathResolution {
                resolved: true,
                fallback_reason: None,
                heads: Some(Heads::of(&result)),
            },
            Err(reason) => NewPathResolution {
                resolved: false,
                fallback_reason: Some(reason),
                heads: None,
            },
        };
    }
}

fn engine_fallback_reason(engine: &'static str) -> &'static str {
    match engine {
        "gated" => "engine_unanswered",
        "shadow_policy" => "engine_not_gated",
        "context_free" => "no_conversation",
        "no_scope" => "no_scope",
        _ => "engine_unavailable",
    }
}

pub(super) async fn analyze<F, Fut>(
    path: AnalysisPath,
    router: Option<&OperationLlmRouter>,
    execution_context: &ExecutionContext,
    execution_id: Option<&str>,
    query: &str,
    conversation: Option<&ConversationContext>,
    llm: F,
) -> Result<(UnifiedAnalysisResult, AnalysisTrace)>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Call>>,
{
    // Intent is only meaningful against a thread; the context-free analysis
    // (decomposer, debug entry points) stays LLM-only.
    let (Some(router), Some(conversation)) = (router, conversation) else {
        return llm_only(path, "context_free", llm).await;
    };
    let Some((input, lookup)) =
        engine_input(router, execution_context, execution_id, query, conversation).await
    else {
        return llm_only(path, "no_scope", llm).await;
    };

    if matches!(&lookup, PolicyLookup::Participating(p) if p.policy.gate) {
        let mut trace = AnalysisTrace::new(path, "gated");
        let engine = async {
            let started = Instant::now();
            let outcome = runner::run(
                input,
                lookup,
                OUTER,
                Duration::ZERO,
                |_, _| async { None::<Result<Call>> },
                labels,
            )
            .await;
            let answers = outcome.current_answers().remove(ITEM).unwrap_or_default();
            (answers, started.elapsed())
        };
        if path == AnalysisPath::New {
            // Start the LLM beside the engine so a fallback turn is never
            // slower than a legacy one; a resolved turn drops (cancels) it.
            let mut llm_call = Box::pin(llm());
            let mut engine = Box::pin(engine);
            let mut early: Option<Result<Call>> = None;
            let (answers, latency) = loop {
                tokio::select! {
                    engine_done = &mut engine => break engine_done,
                    call = &mut llm_call, if early.is_none() => early = Some(call),
                }
            };
            trace.engine_answered(&answers, latency);
            if let Ok(result) = resolve_without_llm(&answers) {
                trace.result = Heads::of(&result);
                return Ok((result, trace));
            }
            let call = match early {
                Some(call) => call?,
                None => llm_call.await?,
            };
            trace.llm_called(&call);
            let mut result = call.0;
            apply(&mut result, &answers);
            trace.result = Heads::of(&result);
            return Ok((result, trace));
        }
        let ((answers, latency), call) = tokio::join!(engine, llm());
        let call = call?;
        trace.engine_answered(&answers, latency);
        trace.llm_called(&call);
        let mut result = call.0;
        apply(&mut result, &answers);
        trace.result = Heads::of(&result);
        return Ok((result, trace));
    }
    if !lookup.allows_incumbent() {
        return llm_only(path, "not_participating", llm).await;
    }
    let engine = if matches!(lookup, PolicyLookup::Participating(_)) {
        "shadow_policy"
    } else {
        "not_participating"
    };
    let mut trace = AnalysisTrace::new(path, engine);
    let outcome = runner::run(
        input,
        lookup,
        OUTER,
        Duration::ZERO,
        |_, _| async move { Some(llm().await) },
        labels,
    )
    .await;
    let call = match outcome.incumbent {
        Some(call) => call?,
        None => return Err(anyhow!("query analysis did not complete")),
    };
    trace.llm_called(&call);
    trace.result = Heads::of(&call.0);
    Ok((call.0, trace))
}

async fn llm_only<F, Fut>(
    path: AnalysisPath,
    engine: &'static str,
    llm: F,
) -> Result<(UnifiedAnalysisResult, AnalysisTrace)>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Call>>,
{
    let call = llm().await?;
    let mut trace = AnalysisTrace::new(path, engine);
    trace.llm_called(&call);
    trace.result = Heads::of(&call.0);
    Ok((call.0, trace))
}

/// The new path's analysis from engine answers alone: all three heads must be
/// eligible, and a slot answer still needs the LLM's slot match and value.
fn resolve_without_llm(
    answers: &BTreeMap<String, Answer>,
) -> std::result::Result<UnifiedAnalysisResult, &'static str> {
    let choice = |question: &str| match answers.get(question) {
        Some(Answer::Choice { choice, .. }) => Some(choice.as_str().to_owned()),
        _ => None,
    };
    let intent = choice("intent")
        .as_deref()
        .and_then(intent_from_id)
        .ok_or("intent_unresolved")?;
    if intent == QueryIntent::AnswerElicitation {
        return Err("slot_answer_needs_llm");
    }
    let task_clarity = choice("task_clarity")
        .as_deref()
        .and_then(clarity_from_id)
        .ok_or("task_clarity_unresolved")?;
    let score = choice("complexity")
        .as_deref()
        .and_then(bucket_score)
        .ok_or("complexity_unresolved")?;
    Ok(UnifiedAnalysisResult {
        complexity: ComplexityAnalysis {
            score,
            factors: Vec::new(),
            reasoning: String::new(),
        },
        categories: Some(CategoryAnalysis {
            categories: Vec::new(),
            reasoning: String::new(),
        }),
        dependencies: DependencyAnalysis {
            is_multi_step: false,
            dependencies: Vec::new(),
            workflow_steps: Vec::new(),
            reasoning: String::new(),
            required_capabilities: Vec::new(),
        },
        extracted_entities: ExtractedEntities {
            source: Some(ExtractionSource::Deferred),
            ..ExtractedEntities::default()
        },
        intent: Some(intent),
        slot_match: None,
        task_clarity,
    })
}

/// Record a shadow comparison for one analysis stage.
pub(super) async fn record_shadow(
    execution_context: &ExecutionContext,
    execution_id: Option<&str>,
    correlation_id: Option<&str>,
    query: &str,
    trace: &AnalysisTrace,
) {
    let agree = |field: fn(&Heads) -> &'static str| {
        trace
            .new_path
            .heads
            .as_ref()
            .map(|heads| field(heads) == field(&trace.result))
    };
    let record = json!({
        "stage": "analysis",
        "recorded_at": chrono::Utc::now().to_rfc3339(),
        "execution_id": execution_id,
        "correlation_id": correlation_id,
        "task_id": execution_context.metadata.get("task_id"),
        "query_head": analysis_path::query_head(query),
        "trace": trace,
        "agreement": {
            "intent": agree(|h| h.intent),
            "task_clarity": agree(|h| h.task_clarity),
            "complexity_bucket": agree(|h| h.complexity_bucket),
        },
    });
    analysis_path::record_shadow(
        &execution_context.principal,
        &execution_context.workspace,
        record,
    )
    .await;
}

async fn engine_input(
    router: &OperationLlmRouter,
    execution_context: &ExecutionContext,
    execution_id: Option<&str>,
    query: &str,
    conversation: &ConversationContext,
) -> Option<(runner::Input, PolicyLookup)> {
    let principal = execution_context.principal.trim();
    let workspace = execution_context.workspace.trim();
    if principal.is_empty() || workspace.is_empty() {
        return None;
    }
    let reference_version = reference::version(
        router,
        LLMOperation::QueryAnalysis.as_str(),
        versions::UNIFIED_ANALYSIS,
    )?;
    let lookup = classification::ready_policy(DECISION_ID, principal, workspace).await;
    let item = project(query, conversation);
    let case_id = blake3::hash(&serde_json::to_vec(&item).ok()?)
        .to_hex()
        .to_string();
    let mut scope = router.classification_trace_context(magicllm::LlmScope::new(
        principal.to_owned(),
        workspace.to_owned(),
    ));
    let metadata = &execution_context.metadata;
    scope.execution_id = execution_id.map(str::to_owned).or(scope.execution_id);
    scope.task_id = metadata.get("task_id").cloned().or(scope.task_id);
    scope.chat_session_id = metadata
        .get("chat_session_id")
        .cloned()
        .or(scope.chat_session_id);
    Some((
        runner::Input {
            operation: DECISION_ID.into(),
            projection_version: PROJECTION.into(),
            reference_version,
            case_id,
            context: None,
            items: vec![item],
            required_questions: vec!["intent".into()],
            scope,
            agent: metadata.get("agent_id").cloned(),
            requires_completion: false,
            replay: None,
        },
        lookup,
    ))
}

fn project(query: &str, conversation: &ConversationContext) -> DecisionItem {
    let recent_turns: Vec<_> = conversation
        .recent_turns
        .iter()
        .rev()
        .take(3)
        .map(|turn| {
            json!({
                "from": match turn.direction {
                    TurnDirection::Inbound => "user",
                    TurnDirection::Outbound => "assistant",
                },
                "text": turn.text.chars().take(TURN_TEXT_CHARS).collect::<String>(),
            })
        })
        .collect();
    DecisionItem {
        item_id: ITEM.into(),
        state: DecisionState::from_json(json!({
            "message": query,
            "execution_status": format!("{:?}", conversation.execution_status),
            "pending_slot_count": conversation.pending_slots.len(),
            "recent_turns": recent_turns,
        })),
        choice_candidates: BTreeMap::new(),
    }
}

/// Take the engine's eligible answers. Complexity keeps the LLM's finer
/// score when both fall in the same bucket.
fn apply(result: &mut UnifiedAnalysisResult, answers: &BTreeMap<String, Answer>) {
    let choice = |question: &str| match answers.get(question) {
        Some(Answer::Choice { choice, .. }) => Some(choice.as_str().to_owned()),
        _ => None,
    };
    if let Some(intent) = choice("intent").as_deref().and_then(intent_from_id) {
        result.intent = Some(intent);
    }
    if let Some(clarity) = choice("task_clarity").as_deref().and_then(clarity_from_id) {
        result.task_clarity = clarity;
    }
    if let Some(bucket) = choice("complexity") {
        if bucket_id(result.complexity.score) != bucket {
            if let Some(score) = bucket_score(&bucket) {
                result.complexity.score = score;
            }
        }
    }
}

fn labels(call: &Result<Call>) -> Reference {
    let Ok((result, response, latency_ms)) = call else {
        return BTreeMap::new().into();
    };
    let row = BTreeMap::from([
        (
            "intent".to_owned(),
            json!(intent_id(
                result.intent.as_ref().unwrap_or(&QueryIntent::NewTask)
            )),
        ),
        (
            "task_clarity".to_owned(),
            json!(clarity_id(result.task_clarity)),
        ),
        (
            "complexity".to_owned(),
            json!(bucket_id(result.complexity.score)),
        ),
    ]);
    Reference::from_response(
        BTreeMap::from([(ITEM.to_owned(), row)]),
        response,
        *latency_ms,
    )
}

fn intent_id(intent: &QueryIntent) -> &'static str {
    match intent {
        QueryIntent::NewTask => "new_task",
        QueryIntent::AnswerElicitation => "answer_elicitation",
        QueryIntent::StatusQuery => "status_query",
        QueryIntent::Cancellation => "cancellation",
        QueryIntent::Clarification => "clarification",
        QueryIntent::ContinueWorkflow => "continue_workflow",
    }
}
fn intent_from_id(id: &str) -> Option<QueryIntent> {
    Some(match id {
        "new_task" => QueryIntent::NewTask,
        "answer_elicitation" => QueryIntent::AnswerElicitation,
        "status_query" => QueryIntent::StatusQuery,
        "cancellation" => QueryIntent::Cancellation,
        "clarification" => QueryIntent::Clarification,
        "continue_workflow" => QueryIntent::ContinueWorkflow,
        _ => return None,
    })
}
fn clarity_id(clarity: TaskClarity) -> &'static str {
    match clarity {
        TaskClarity::Plannable => "plannable",
        TaskClarity::NeedsElicitation => "needs_elicitation",
    }
}
fn clarity_from_id(id: &str) -> Option<TaskClarity> {
    Some(match id {
        "plannable" => TaskClarity::Plannable,
        "needs_elicitation" => TaskClarity::NeedsElicitation,
        _ => return None,
    })
}
/// The outline prompt's own goal-count buckets: < 0.3, 0.3–0.7, > 0.7.
fn bucket_id(score: f32) -> &'static str {
    if score < 0.3 {
        "simple"
    } else if score <= 0.7 {
        "medium"
    } else {
        "complex"
    }
}
fn bucket_score(id: &str) -> Option<f32> {
    Some(match id {
        "simple" => 0.2,
        "medium" => 0.5,
        "complex" => 0.8,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::storage::WaitingState;
    use decision_engine_contract::primitives::OptionId;

    fn choice(id: &str) -> Answer {
        Answer::Choice {
            choice: OptionId::new(id),
            probabilities: BTreeMap::new(),
            confidence: 0.9,
        }
    }

    fn parsed() -> UnifiedAnalysisResult {
        serde_json::from_value(json!({
            "complexity": {"score": 0.45, "factors": []},
            "dependencies": {
                "is_multi_step": false,
                "dependencies": [],
                "workflow_steps": [],
                "required_capabilities": [],
                "reasoning": ""
            },
            "intent": "new_task",
            "task_clarity": "plannable"
        }))
        .expect("minimal analysis parses")
    }

    #[test]
    fn option_ids_round_trip() {
        for intent in [
            QueryIntent::NewTask,
            QueryIntent::AnswerElicitation,
            QueryIntent::StatusQuery,
            QueryIntent::Cancellation,
            QueryIntent::Clarification,
            QueryIntent::ContinueWorkflow,
        ] {
            assert_eq!(intent_from_id(intent_id(&intent)), Some(intent));
        }
        for clarity in [TaskClarity::Plannable, TaskClarity::NeedsElicitation] {
            assert_eq!(clarity_from_id(clarity_id(clarity)), Some(clarity));
        }
        for bucket in ["simple", "medium", "complex"] {
            assert_eq!(bucket_id(bucket_score(bucket).unwrap()), bucket);
        }
    }

    #[test]
    fn engine_answers_replace_only_their_fields() {
        let mut result = parsed();
        apply(
            &mut result,
            &BTreeMap::from([
                ("intent".to_owned(), choice("cancellation")),
                ("complexity".to_owned(), choice("medium")),
            ]),
        );
        assert_eq!(result.intent, Some(QueryIntent::Cancellation));
        assert_eq!(
            result.task_clarity,
            TaskClarity::Plannable,
            "unanswered field kept"
        );
        assert_eq!(
            result.complexity.score, 0.45,
            "same bucket keeps the finer score"
        );

        apply(
            &mut result,
            &BTreeMap::from([
                ("complexity".to_owned(), choice("complex")),
                ("intent".to_owned(), choice("bogus")),
            ]),
        );
        assert_eq!(result.complexity.score, 0.8);
        assert_eq!(
            result.intent,
            Some(QueryIntent::Cancellation),
            "unknown label ignored"
        );
    }

    fn all_heads(intent: &str) -> BTreeMap<String, Answer> {
        BTreeMap::from([
            ("intent".to_owned(), choice(intent)),
            ("task_clarity".to_owned(), choice("plannable")),
            ("complexity".to_owned(), choice("medium")),
        ])
    }

    #[test]
    fn new_path_resolves_from_three_eligible_heads_and_defers_extraction() {
        let result = resolve_without_llm(&all_heads("new_task")).expect("resolves");
        assert_eq!(result.intent, Some(QueryIntent::NewTask));
        assert_eq!(result.task_clarity, TaskClarity::Plannable);
        assert_eq!(result.complexity.score, 0.5);
        assert!(result.slot_match.is_none());
        assert_eq!(
            result.extracted_entities.source,
            Some(ExtractionSource::Deferred)
        );
        assert!(!result.extracted_entities.is_assessed());
        assert!(result
            .categories
            .as_ref()
            .is_some_and(|c| c.categories.is_empty()));
        assert!(!result.dependencies.is_multi_step);
    }

    #[test]
    fn new_path_falls_back_when_a_head_is_unresolved_or_a_slot_is_answered() {
        let mut answers = all_heads("new_task");
        answers.remove("complexity");
        assert_eq!(
            resolve_without_llm(&answers).err(),
            Some("complexity_unresolved")
        );
        let mut answers = all_heads("new_task");
        answers.remove("task_clarity");
        assert_eq!(
            resolve_without_llm(&answers).err(),
            Some("task_clarity_unresolved")
        );
        assert_eq!(
            resolve_without_llm(&BTreeMap::new()).err(),
            Some("intent_unresolved")
        );
        assert_eq!(
            resolve_without_llm(&all_heads("answer_elicitation")).err(),
            Some("slot_answer_needs_llm"),
            "a slot answer needs the LLM's slot match and typed value"
        );
        assert_eq!(
            resolve_without_llm(&all_heads("bogus")).err(),
            Some("intent_unresolved")
        );
    }

    #[test]
    fn fallback_turn_equals_a_legacy_turn() {
        // Both paths apply the same eligible answers to the same LLM result.
        let mut answers = all_heads("cancellation");
        answers.remove("complexity");
        let mut legacy = parsed();
        apply(&mut legacy, &answers);
        let mut fallback = parsed();
        assert!(resolve_without_llm(&answers).is_err());
        apply(&mut fallback, &answers);
        assert_eq!(Heads::of(&legacy), Heads::of(&fallback));
        assert_eq!(Heads::of(&fallback).complexity_score, 0.45);
    }

    #[test]
    fn trace_records_engine_answers_and_new_path_heads() {
        let mut trace = AnalysisTrace::new(AnalysisPath::Shadow, "gated");
        assert_eq!(trace.new_path.fallback_reason, Some("engine_unanswered"));
        trace.engine_answered(&all_heads("status_query"), Duration::from_millis(7));
        assert_eq!(trace.engine_latency_ms, Some(7));
        assert!(trace.new_path.resolved);
        assert_eq!(
            trace.new_path.heads.as_ref().map(|h| h.intent),
            Some("status_query")
        );
        assert_eq!(trace.engine_answers["complexity"].0, "medium");
        let value = serde_json::to_value(&trace).expect("serializes");
        assert_eq!(value["path"], "shadow");
        assert_eq!(value["new_path"]["heads"]["complexity_bucket"], "medium");
    }

    fn llm_call() -> Result<Call> {
        Ok((
            parsed(),
            SimplifiedLLMResponse::content_only("{}".to_owned()),
            11,
        ))
    }

    #[tokio::test]
    async fn without_a_thread_every_path_is_the_llm_alone() {
        for path in [
            AnalysisPath::Legacy,
            AnalysisPath::Shadow,
            AnalysisPath::New,
        ] {
            let (result, trace) = analyze(
                path,
                None,
                &ExecutionContext::default(),
                None,
                "ping example.org",
                None,
                || async { llm_call() },
            )
            .await
            .expect("llm answers");
            assert_eq!(result.intent, Some(QueryIntent::NewTask));
            assert_eq!(trace.engine, "context_free");
            assert!(!trace.new_path.resolved);
            assert_eq!(trace.new_path.fallback_reason, Some("no_conversation"));
            let llm = trace.llm.expect("llm answered");
            assert_eq!(llm.cost.latency_ms, 11);
            assert_eq!(trace.result, llm.heads);
        }
    }

    #[test]
    fn projection_is_bounded_and_content_ordered() {
        let conversation = ConversationContext {
            execution_status: WaitingState::default(),
            pending_slots: Vec::new(),
            recent_turns: Vec::new(),
        };
        let item = project("cancel that", &conversation);
        assert_eq!(item.item_id, ITEM);
        assert_eq!(item.state.0["message"], "cancel that");
        assert_eq!(item.state.0["pending_slot_count"], 0);
    }
}

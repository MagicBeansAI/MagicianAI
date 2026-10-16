//! Planner parameter priority on the decision engine.
//!
//! One decision batch covers every parameter of a plan. The planner's own
//! per-parameter LLM classification stays the incumbent: it runs for every
//! parameter in shadow (or with the engine off) and, when the operation is
//! gated, only for the parameters the engine left unresolved. The engine never
//! sees or selects the LLM; this module alone owns the fallback.

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

use decision_engine_contract::{
    batch::DecisionItem,
    request::{Answer, DecisionState},
};
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use tracing::debug;

use super::{
    plan::{AskTiming, DiscoveryTiming, QuestionPriority},
    types::StrategyContext,
};
use crate::magician_v2::{
    decision_host::classification::{self, PolicyLookup},
    decisions::{reference, runner, telemetry::Reference},
    prompts::{
        constants::{names, versions},
        PromptManager,
    },
    query_analysis::operation_llm_router::{
        LLMOperation, OperationLlmRouter, SimplifiedLLMResponse,
    },
};

const DECISION_ID: &str = "plan_parameter_priority";
const PROJECTION: &str = "plan_parameter_priority_v2";
/// Pack 1.1.0 asks priority only; ask and discovery timing follow from it.
const QUESTIONS: [&str; 1] = ["priority"];
/// Outer deadline for the whole classification. In shadow the runner cuts the
/// incumbent LLM at this deadline, so it must cover the LLM calls; a gated
/// engine is capped separately by the operation's `decision_budget_ms` and
/// `queue_budget_ms`, after which unresolved parameters fall through to the LLM.
const BUDGET: Duration = Duration::from_secs(120);
const LLM_CONCURRENCY: usize = 4;

/// One parameter the planner needs a priority for.
#[derive(Debug, Clone)]
pub(super) struct PriorityCase {
    pub parameter: String,
    pub step_tool: String,
    pub step_description: String,
}

/// A classification. `None` fields mean the source said nothing about that
/// field, so the caller keeps its own value.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Priority {
    pub priority: Option<QuestionPriority>,
    pub ask_timing: Option<AskTiming>,
    pub discovery_timing: Option<DiscoveryTiming>,
    /// Free-text hints only the LLM produces.
    pub hints: Vec<String>,
}

struct LlmClassified {
    priority: Priority,
    response: SimplifiedLLMResponse,
    elapsed_ms: u64,
}
type LlmResults = BTreeMap<String, Result<LlmClassified, String>>;

/// Classify every case. The result has one entry per case, in order; `None`
/// means neither the engine nor the LLM produced a classification and the
/// caller applies its safe defaults.
pub(super) async fn classify(
    router: Option<&Arc<OperationLlmRouter>>,
    prompt_manager: Option<&Arc<PromptManager>>,
    context: &StrategyContext,
    user_request: &str,
    cases: &[PriorityCase],
) -> Vec<Option<Priority>> {
    if cases.is_empty() {
        return Vec::new();
    }
    let ids: Vec<String> = (0..cases.len()).map(|i| i.to_string()).collect();
    let llm_fn = |selected: Vec<String>| {
        llm_classify(
            router,
            prompt_manager,
            context,
            user_request,
            cases,
            selected,
        )
    };
    let llm = &llm_fn;

    let engine = match router {
        Some(router) => engine_input(router, context, user_request, cases).await,
        None => None,
    };
    let Some((input, lookup)) = engine else {
        let results = llm(ids.clone()).await;
        return assemble(&ids, &BTreeMap::new(), &results);
    };

    let outcome = runner::run(
        input,
        lookup,
        BUDGET,
        Duration::ZERO,
        |selected, _| async move { Some(llm(selected).await) },
        labels,
    )
    .await;

    let engine_answers: BTreeMap<String, Priority> = outcome
        .current_answers()
        .into_iter()
        .filter_map(|(id, answers)| from_answers(&answers).map(|p| (id, p)))
        .collect();
    let mut results = outcome.incumbent.unwrap_or_default();
    // The caller owns unresolved parameters: anything neither resolved by the
    // engine nor already attempted by the incumbent goes to the LLM now.
    let missing: Vec<String> = ids
        .iter()
        .filter(|id| !engine_answers.contains_key(*id) && !results.contains_key(*id))
        .cloned()
        .collect();
    if !missing.is_empty() {
        results.extend(llm(missing).await);
    }
    if !engine_answers.is_empty() {
        debug!(
            resolved = engine_answers.len(),
            total = cases.len(),
            "[PRIORITY-CLASSIFICATION] decision engine resolved parameter priorities"
        );
    }
    assemble(&ids, &engine_answers, &results)
}

fn assemble(
    ids: &[String],
    engine: &BTreeMap<String, Priority>,
    llm: &LlmResults,
) -> Vec<Option<Priority>> {
    ids.iter()
        .map(|id| {
            engine.get(id).cloned().or_else(|| {
                llm.get(id)
                    .and_then(|r| r.as_ref().ok())
                    .map(|c| c.priority.clone())
            })
        })
        .collect()
}

/// Build the runner input, or `None` when the engine cannot take part (no
/// scope, no bound reference prompt): the LLM path then runs as before.
async fn engine_input(
    router: &OperationLlmRouter,
    context: &StrategyContext,
    user_request: &str,
    cases: &[PriorityCase],
) -> Option<(runner::Input, PolicyLookup)> {
    let principal = context.execution_context.principal.trim();
    let workspace = context.execution_context.workspace.trim();
    if principal.is_empty() || workspace.is_empty() {
        return None;
    }
    let reference_version = reference::version(
        router,
        LLMOperation::ParameterExtraction.as_str(),
        versions::PARAM_PRIORITY_CLASSIFICATION,
    )?;
    let lookup = classification::ready_policy(DECISION_ID, principal, workspace).await;
    let (shared, items) = project(user_request, cases);
    let case_id = blake3::hash(&serde_json::to_vec(&(&shared, &items)).ok()?)
        .to_hex()
        .to_string();
    let mut scope = router.classification_trace_context(magicllm::LlmScope::new(
        principal.to_owned(),
        workspace.to_owned(),
    ));
    let metadata = &context.execution_context.metadata;
    scope.execution_id = context.execution_id.clone().or(scope.execution_id);
    scope.task_id = metadata.get("task_id").cloned().or(scope.task_id);
    scope.chat_session_id = metadata
        .get("chat_session_id")
        .cloned()
        .or(scope.chat_session_id);
    let agent = metadata
        .get("agent_id")
        .or_else(|| metadata.get("source_agent_id"))
        .cloned();
    Some((
        runner::Input {
            operation: DECISION_ID.into(),
            projection_version: PROJECTION.into(),
            reference_version,
            case_id,
            context: Some(shared),
            items,
            required_questions: QUESTIONS.iter().map(|q| (*q).to_owned()).collect(),
            scope,
            agent,
            requires_completion: false,
            replay: None,
        },
        lookup,
    ))
}

fn project(user_request: &str, cases: &[PriorityCase]) -> (DecisionState, Vec<DecisionItem>) {
    let shared = DecisionState::from_json(json!({ "user_request": user_request }));
    let items = cases
        .iter()
        .enumerate()
        .map(|(index, case)| DecisionItem {
            item_id: index.to_string(),
            state: {
                let (kind, meaning) = parameter_kind(&case.parameter);
                DecisionState::from_json(json!({
                    "parameter": case.parameter,
                    "kind": kind,
                    "kind_meaning": meaning,
                    "step_tool": case.step_tool,
                    "step_description": case.step_description,
                }))
            },
            choice_candidates: BTreeMap::new(),
        })
        .collect();
    (shared, items)
}

/// The planner's parameter-name prefixes, spelled out for the engine: the
/// name alone (`tool_selection:email_access`) does not say who decides it.
fn parameter_kind(parameter: &str) -> (&'static str, &'static str) {
    match parameter.split_once(':').map(|(prefix, _)| prefix) {
        Some("tool_selection") => (
            "tool_selection",
            "which connected tool, account or service carries out the step",
        ),
        Some("slot") => ("slot", "a value in the user's world the step needs"),
        Some("consent") => ("consent", "a permission the user must grant"),
        _ => ("value", "a value the step needs"),
    }
}

/// Engine answer → a classification. Only priority is asked; ask timing is
/// derived the way the planner prompt pairs them (upfront for values the run
/// cannot start without, when needed for tuning values, never for what can
/// be inferred or defaulted). Discovery timing has no production reader and
/// stays `Auto`.
fn from_answers(answers: &BTreeMap<String, Answer>) -> Option<Priority> {
    let priority = match answers.get("priority") {
        Some(Answer::Choice { choice, .. }) => priority_from_id(choice.as_str())?,
        _ => return None,
    };
    Some(Priority {
        priority: Some(priority),
        ask_timing: Some(ask_timing_for(&priority)),
        discovery_timing: Some(DiscoveryTiming::Auto),
        hints: Vec::new(),
    })
}

fn ask_timing_for(priority: &QuestionPriority) -> AskTiming {
    match priority {
        QuestionPriority::Critical | QuestionPriority::PreExecution => AskTiming::PreExecution,
        QuestionPriority::JustInTime => AskTiming::JustInTime,
        QuestionPriority::Optional | QuestionPriority::Inferrable => AskTiming::Never,
    }
}

fn priority_from_id(id: &str) -> Option<QuestionPriority> {
    Some(match id {
        "critical" => QuestionPriority::Critical,
        "pre_execution" => QuestionPriority::PreExecution,
        "just_in_time" => QuestionPriority::JustInTime,
        "optional" => QuestionPriority::Optional,
        "inferrable" => QuestionPriority::Inferrable,
        _ => return None,
    })
}
fn priority_id(value: &QuestionPriority) -> &'static str {
    match value {
        QuestionPriority::Critical => "critical",
        QuestionPriority::PreExecution => "pre_execution",
        QuestionPriority::JustInTime => "just_in_time",
        QuestionPriority::Optional => "optional",
        QuestionPriority::Inferrable => "inferrable",
    }
}

/// Incumbent labels in the pack's option ids, with every physical call's receipt.
fn labels(results: &LlmResults) -> Reference {
    let mut labels = BTreeMap::new();
    let mut responses = Vec::new();
    for (id, result) in results {
        let Ok(classified) = result else { continue };
        let mut row = BTreeMap::new();
        if let Some(value) = &classified.priority.priority {
            row.insert("priority".to_owned(), json!(priority_id(value)));
        }
        labels.insert(id.clone(), row);
        responses.push(classified);
    }
    let Some((first, rest)) = responses.split_first() else {
        return labels.into();
    };
    let mut reference = Reference::from_response(labels, &first.response, first.elapsed_ms);
    reference.calls = rest
        .iter()
        .filter_map(|c| Reference::from_response(BTreeMap::new(), &c.response, c.elapsed_ms).call)
        .collect();
    // Calls run concurrently; the slowest one is the incumbent's latency.
    reference.latency_ms = responses.iter().map(|c| c.elapsed_ms).max().unwrap_or(0);
    reference
}

/// The planner's LLM classification for the selected cases, bounded-concurrent.
async fn llm_classify(
    router: Option<&Arc<OperationLlmRouter>>,
    prompt_manager: Option<&Arc<PromptManager>>,
    context: &StrategyContext,
    user_request: &str,
    cases: &[PriorityCase],
    selected: Vec<String>,
) -> LlmResults {
    let (Some(router), Some(prompt_manager)) = (router, prompt_manager) else {
        return selected
            .into_iter()
            .map(|id| (id, Err("LLM or prompt manager unavailable".to_owned())))
            .collect();
    };
    stream::iter(selected)
        .map(|id: String| async move {
            let result = match id.parse::<usize>().ok().and_then(|i| cases.get(i)) {
                Some(case) => llm_one(router, prompt_manager, context, user_request, case)
                    .await
                    .map_err(|e| e.to_string()),
                None => Err("unknown item".to_owned()),
            };
            (id, result)
        })
        .buffer_unordered(LLM_CONCURRENCY)
        .collect()
        .await
}

async fn llm_one(
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
    context: &StrategyContext,
    user_request: &str,
    case: &PriorityCase,
) -> anyhow::Result<LlmClassified> {
    let variables = std::collections::HashMap::from([
        ("parameter_name".to_string(), case.parameter.clone()),
        ("step_tool".to_string(), case.step_tool.clone()),
        (
            "step_description".to_string(),
            case.step_description.clone(),
        ),
        ("user_request".to_string(), user_request.to_string()),
    ]);
    let prompt = prompt_manager
        .get_rendered_prompt(
            names::PARAM_PRIORITY_CLASSIFICATION,
            versions::PARAM_PRIORITY_CLASSIFICATION,
            variables,
        )
        .await
        .map_err(|e| anyhow::anyhow!("Failed to load param_priority_classification prompt: {e}"))?;
    let started = Instant::now();
    let response = router
        .generate_for_operation(&LLMOperation::ParameterExtraction, &prompt)
        .await?;
    let elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    context.emit_operation_llm_telemetry(
        LLMOperation::ParameterExtraction.as_str(),
        &response,
        elapsed_ms,
    );
    let json: Value = serde_json::from_str(&response.content)
        .map_err(|e| anyhow::anyhow!("Failed to parse LLM response: {e}"))?;
    Ok(LlmClassified {
        priority: parse_llm(&json),
        response,
        elapsed_ms,
    })
}

/// The prompt's labels. A present but unknown label means the planner's safe
/// default; an absent field leaves the caller's value alone.
fn parse_llm(json: &Value) -> Priority {
    Priority {
        priority: json["priority"].as_str().map(|s| match s {
            "Critical" => QuestionPriority::Critical,
            "JustInTime" => QuestionPriority::JustInTime,
            "Optional" => QuestionPriority::Optional,
            "Inferrable" => QuestionPriority::Inferrable,
            _ => QuestionPriority::PreExecution,
        }),
        ask_timing: json["ask_timing"].as_str().map(|s| match s {
            "JustInTime" => AskTiming::JustInTime,
            "Never" => AskTiming::Never,
            _ => AskTiming::PreExecution,
        }),
        discovery_timing: json["discovery_timing"].as_str().map(|s| match s {
            "PreExecution" => DiscoveryTiming::PreExecution,
            "JustInTime" => DiscoveryTiming::JustInTime,
            _ => DiscoveryTiming::Auto,
        }),
        hints: json["inference_hints"]
            .as_array()
            .map(|hints| {
                hints
                    .iter()
                    .filter_map(|h| h.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use decision_engine_contract::primitives::OptionId;

    fn choice(id: &str) -> Answer {
        Answer::Choice {
            choice: OptionId::new(id),
            probabilities: BTreeMap::new(),
            confidence: 0.9,
        }
    }

    #[test]
    fn priority_alone_resolves_and_timing_is_derived() {
        let answers = BTreeMap::from([("priority".to_owned(), choice("inferrable"))]);
        assert_eq!(
            from_answers(&answers),
            Some(Priority {
                priority: Some(QuestionPriority::Inferrable),
                ask_timing: Some(AskTiming::Never),
                discovery_timing: Some(DiscoveryTiming::Auto),
                hints: Vec::new(),
            })
        );
        let upfront = BTreeMap::from([("priority".to_owned(), choice("pre_execution"))]);
        assert_eq!(
            from_answers(&upfront).unwrap().ask_timing,
            Some(AskTiming::PreExecution),
            "a value the run cannot start without is asked before it starts"
        );
        let unknown = BTreeMap::from([("priority".to_owned(), choice("urgent"))]);
        assert_eq!(from_answers(&unknown), None);
        assert_eq!(from_answers(&BTreeMap::new()), None);
    }

    #[test]
    fn derived_ask_timing_matches_the_planner_prompt_pairs() {
        for (priority, timing) in [
            (QuestionPriority::Critical, AskTiming::PreExecution),
            (QuestionPriority::PreExecution, AskTiming::PreExecution),
            (QuestionPriority::JustInTime, AskTiming::JustInTime),
            (QuestionPriority::Optional, AskTiming::Never),
            (QuestionPriority::Inferrable, AskTiming::Never),
        ] {
            assert_eq!(ask_timing_for(&priority), timing);
            assert_eq!(priority_from_id(priority_id(&priority)), Some(priority));
        }
    }

    #[test]
    fn parameter_kind_explains_planner_prefixes() {
        assert_eq!(
            parameter_kind("tool_selection:email_access").0,
            "tool_selection"
        );
        assert_eq!(parameter_kind("slot:landlord_email").0, "slot");
        assert_eq!(parameter_kind("consent:send_email").0, "consent");
        assert_eq!(parameter_kind("email_service_url").0, "value");
    }

    #[test]
    fn llm_labels_keep_the_prompt_defaults() {
        let parsed = parse_llm(&json!({
            "priority": "Bogus",
            "discovery_timing": "JustInTime",
            "inference_hints": ["current_user"]
        }));
        assert_eq!(parsed.priority, Some(QuestionPriority::PreExecution));
        assert_eq!(
            parsed.ask_timing, None,
            "absent field keeps the caller's value"
        );
        assert_eq!(parsed.discovery_timing, Some(DiscoveryTiming::JustInTime));
        assert_eq!(parsed.hints, ["current_user"]);
    }

    #[test]
    fn engine_answers_win_and_the_llm_fills_the_rest() {
        let ids: Vec<String> = (0..3).map(|i| i.to_string()).collect();
        let engine = BTreeMap::from([(
            "0".to_owned(),
            Priority {
                priority: Some(QuestionPriority::Optional),
                ..Default::default()
            },
        )]);
        let llm: LlmResults = BTreeMap::from([("2".to_owned(), Err("timeout".to_owned()))]);
        let out = assemble(&ids, &engine, &llm);
        assert_eq!(
            out[0].as_ref().unwrap().priority,
            Some(QuestionPriority::Optional)
        );
        assert!(out[1].is_none());
        assert!(
            out[2].is_none(),
            "a failed LLM call leaves the caller's defaults"
        );
    }

    #[test]
    fn projection_uses_ordinal_ids_and_shares_the_request() {
        let (shared, items) = project(
            "ping example.org",
            &[PriorityCase {
                parameter: "target_host".into(),
                step_tool: "network_diagnostics".into(),
                step_description: "Check connectivity".into(),
            }],
        );
        assert_eq!(
            shared,
            DecisionState::from_json(json!({"user_request": "ping example.org"}))
        );
        assert_eq!(items[0].item_id, "0");
        assert!(items[0].choice_candidates.is_empty());
        assert_eq!(items[0].state.0["kind"], "value");
    }
}

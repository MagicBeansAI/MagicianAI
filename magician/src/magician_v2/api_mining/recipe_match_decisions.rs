//! Task Recipe fuzzy match on the decision engine.
//!
//! When neither the task id nor a recipe template matches a new task
//! deterministically, the matcher asks the `recipe_match` LLM whether the task
//! is the same shape as one of up to three candidate recipes and, if so, to
//! extract the recipe's inputs. The pack asks only the closed half — which
//! candidate, or none — over the same shape-only payload (templates and input
//! schemas, never traces, values or answers).
//!
//! The LLM stays the incumbent: in shadow (or with the engine off) it decides
//! and the engine's answer is recorded beside it. When the operation is gated
//! and the engine eligibly answers `none`, the LLM call is skipped and the
//! lookup misses (the browser rail runs, as on any miss). Any other answer
//! still needs the LLM for the inputs, so it falls back to the LLM, as does
//! every case the engine cannot take part in. The engine never sees or selects
//! the LLM.

use std::{collections::BTreeMap, future::Future, time::Duration};

use decision_engine_contract::{
    batch::DecisionItem,
    request::{Answer, DecisionState},
};
use serde_json::{json, Value};

use super::{recipe::TaskRecipe, recipe_matcher::serialize_match_payload};
use crate::magician_v2::{
    decision_host::classification::{self, PolicyLookup},
    decisions::{reference, runner, telemetry::Reference},
    prompts::constants::versions,
    query_analysis::operation_llm_router::{OperationLlmRouter, SimplifiedLLMResponse},
};

pub const DECISION_ID: &str = "recipe_match";
pub const PROJECTION: &str = "recipe_match_v1";
/// The LLM operation the pack is compared against.
pub const OPERATION: &str = "recipe_match";
pub const NONE: &str = "none";
const ITEM: &str = "task";
/// Never cut the incumbent: the LLM operation has its own timeout. A gated
/// engine is capped by the operation's decision and queue budgets.
const OUTER: Duration = Duration::from_secs(600);
const TEXT_CHARS: usize = 2_000;

/// The recipe_match LLM's reply: its JSON (when it parsed), with the
/// response and latency for the shadow record.
pub struct LlmMatch {
    pub reply: Option<Value>,
    pub response: SimplifiedLLMResponse,
    pub latency_ms: u64,
}

impl LlmMatch {
    /// The LLM's pick as a pack option id: the candidate it named with
    /// confidence at or above the matcher's threshold, else `none`. `None`
    /// when the reply did not parse.
    pub fn choice(&self, candidates: &[&TaskRecipe], threshold: f32) -> Option<String> {
        let reply = self.reply.as_ref()?;
        let recipe_id = reply.get("recipe_id").and_then(Value::as_str);
        let confidence = reply
            .get("confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.0) as f32;
        Some(match recipe_id {
            Some(id)
                if confidence.is_finite()
                    && confidence >= threshold
                    && confidence <= 1.0
                    && candidates.iter().any(|recipe| recipe.id == id) =>
            {
                id.to_owned()
            },
            _ => NONE.to_owned(),
        })
    }
}

type LlmResult = Option<LlmMatch>;

/// The new task, and whose it is.
pub struct MatchTask<'a> {
    pub principal: &'a str,
    pub workspace: &'a str,
    pub title: &'a str,
    pub description: &'a str,
}

/// Ask the engine and/or the LLM. `None` from the LLM means it failed or was
/// skipped: the lookup misses. Returns the LLM's reply when one ran.
pub async fn confirm<F, Fut>(
    router: &OperationLlmRouter,
    task: &MatchTask<'_>,
    candidates: &[&TaskRecipe],
    threshold: f32,
    llm: F,
) -> Option<LlmMatch>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = LlmResult>,
{
    let Some((input, lookup)) = engine_input(router, task, candidates).await else {
        return llm().await;
    };
    let labels = |call: &LlmResult| labels(call, candidates, threshold);
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
        if engine_says_none(&answers) {
            tracing::debug!("[API_MINING] decision engine ruled out every candidate recipe");
            return None;
        }
        return llm().await;
    }
    if !lookup.allows_incumbent() {
        return llm().await;
    }
    runner::run(
        input,
        lookup,
        OUTER,
        Duration::ZERO,
        |_, _| async move { Some(llm().await) },
        labels,
    )
    .await
    .incumbent
    .flatten()
}

async fn engine_input(
    router: &OperationLlmRouter,
    task: &MatchTask<'_>,
    candidates: &[&TaskRecipe],
) -> Option<(runner::Input, PolicyLookup)> {
    let principal = task.principal.trim();
    let workspace = task.workspace.trim();
    if principal.is_empty() || workspace.is_empty() || candidates.is_empty() {
        return None;
    }
    let reference_version = reference::version(router, OPERATION, versions::RECIPE_MATCH_SYSTEM)?;
    let lookup = classification::ready_policy(DECISION_ID, principal, workspace).await;
    let item = project(task.title, task.description, candidates);
    let case_id = blake3::hash(&serde_json::to_vec(&item).ok()?)
        .to_hex()
        .to_string();
    let scope = router.classification_trace_context(magicllm::LlmScope::new(
        principal.to_owned(),
        workspace.to_owned(),
    ));
    Some((
        runner::Input {
            operation: DECISION_ID.into(),
            projection_version: PROJECTION.into(),
            reference_version,
            case_id,
            context: None,
            items: vec![item],
            required_questions: vec!["recipe".into()],
            scope,
            agent: None,
            requires_completion: false,
            replay: None,
        },
        lookup,
    ))
}

/// The LLM's own shape-only payload, with the candidates offered as the
/// `recipe` question's options (each labelled by its template) plus `none`.
pub fn project(title: &str, description: &str, candidates: &[&TaskRecipe]) -> DecisionItem {
    let clip = |text: &str| text.chars().take(TEXT_CHARS).collect::<String>();
    let payload: Value = serde_json::from_str(&serialize_match_payload(
        &clip(title),
        &clip(description),
        candidates,
    ))
    .unwrap_or(Value::Null);
    let mut options: Vec<(String, String)> = candidates
        .iter()
        .map(|recipe| {
            let label = match recipe.shape.description_template.as_deref() {
                Some(body) => format!("{} — {}", recipe.shape.template, body),
                None => recipe.shape.template.clone(),
            };
            (recipe.id.clone(), label)
        })
        .collect();
    options.push((
        NONE.to_owned(),
        "None of the candidates is the same task shape as the new task.".to_owned(),
    ));
    DecisionItem {
        item_id: ITEM.into(),
        state: DecisionState::from_json(payload),
        choice_candidates: BTreeMap::from([("recipe".to_owned(), options)]),
    }
}

pub fn engine_says_none(answers: &BTreeMap<String, Answer>) -> bool {
    matches!(answers.get("recipe"), Some(Answer::Choice { choice, .. }) if choice.as_str() == NONE)
}

fn labels(call: &LlmResult, candidates: &[&TaskRecipe], threshold: f32) -> Reference {
    let Some(call) = call else {
        return BTreeMap::new().into();
    };
    let labels = match call.choice(candidates, threshold) {
        Some(choice) => BTreeMap::from([(
            ITEM.to_owned(),
            BTreeMap::from([("recipe".to_owned(), json!(choice))]),
        )]),
        None => BTreeMap::new(),
    };
    Reference::from_response(labels, &call.response, call.latency_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::recipe::{
        RecipeShape, TaskInput, TaskInputSchema, TaskInputSource,
    };
    use crate::magician_v2::decisions::test_engine::{self, FakeEngine};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn recipe(id: &str, template: &str) -> TaskRecipe {
        TaskRecipe {
            id: id.into(),
            scope_principal: "p".into(),
            scope_workspace: "w".into(),
            agent_id: "assistant".into(),
            shape: RecipeShape {
                description_template: None,
                template: template.into(),
                fingerprint: "shape".into(),
                inputs: vec![TaskInput {
                    name: "city".into(),
                    schema: TaskInputSchema::String,
                    example_value: "Paris".into(),
                    source: TaskInputSource::TaskText,
                }],
            },
            current_version: 1,
            versions: Vec::new(),
        }
    }

    fn reply(raw: Value) -> LlmMatch {
        LlmMatch {
            reply: Some(raw),
            response: SimplifiedLLMResponse::content_only(String::new()),
            latency_ms: 5,
        }
    }

    #[test]
    fn llm_choice_applies_the_matcher_threshold() {
        let a = recipe("rcp_a", "weather in {city}");
        let candidates = [&a];
        let pick = |raw| reply(raw).choice(&candidates, 0.85);
        assert_eq!(
            pick(json!({"recipe_id": "rcp_a", "confidence": 0.9})).as_deref(),
            Some("rcp_a")
        );
        assert_eq!(
            pick(json!({"recipe_id": "rcp_a", "confidence": 0.6})).as_deref(),
            Some(NONE)
        );
        assert_eq!(
            pick(json!({"recipe_id": "rcp_other", "confidence": 0.99})).as_deref(),
            Some(NONE)
        );
        assert_eq!(
            pick(json!({"recipe_id": null, "confidence": 0.9})).as_deref(),
            Some(NONE)
        );
        let unparsed = LlmMatch {
            reply: None,
            response: SimplifiedLLMResponse::content_only(String::new()),
            latency_ms: 1,
        };
        assert_eq!(unparsed.choice(&candidates, 0.85), None);
    }

    #[test]
    fn projection_offers_each_candidate_and_none() {
        let a = recipe("rcp_a", "weather in {city}");
        let b = recipe("rcp_b", "flights to {city}");
        let item = project("Weather in Lyon", "today", &[&a, &b]);
        let ids: Vec<_> = item.choice_candidates["recipe"]
            .iter()
            .map(|(id, _)| id.as_str())
            .collect();
        assert_eq!(ids, ["rcp_a", "rcp_b", NONE]);
        assert_eq!(item.state.0["task"]["title"], "Weather in Lyon");
        assert_eq!(
            item.state.0["candidates"][1]["template"],
            "flights to {city}"
        );
        assert!(item.state.0["candidates"][0].get("example_value").is_none());
    }

    async fn run(calls: &AtomicUsize, engine: Option<&FakeEngine>) -> Option<LlmMatch> {
        let _ = engine;
        let router = test_engine::fixture_router(OPERATION);
        let a = recipe("rcp_a", "weather in {city}");
        let task = MatchTask {
            principal: "p",
            workspace: "w",
            title: "Weather in Lyon",
            description: "",
        };
        confirm(&router, &task, &[&a], 0.85, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Some(reply(
                json!({"recipe_id": "rcp_a", "confidence": 0.95, "inputs": {"city": "Lyon"}}),
            ))
        })
        .await
    }

    #[tokio::test]
    async fn shadow_keeps_the_llm_match_and_offers_the_engine_the_candidates() {
        let _serial = test_engine::serial().await;
        let engine = FakeEngine::start(DECISION_ID, false, &[("recipe", NONE, 0.95)]).await;
        let calls = AtomicUsize::new(0);
        let matched = run(&calls, Some(&engine)).await.expect("the LLM's match");
        assert_eq!(matched.reply.unwrap()["recipe_id"], "rcp_a");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let request = engine.wait_for_request().await;
        assert_eq!(request.batch.projection_version, PROJECTION);
        let options = &request.batch.items[0].choice_candidates["recipe"];
        assert_eq!(options.last().map(|(id, _)| id.as_str()), Some(NONE));
    }

    #[tokio::test]
    async fn gate_skips_the_llm_only_on_a_confident_none() {
        let _serial = test_engine::serial().await;
        let calls = AtomicUsize::new(0);
        {
            let engine = FakeEngine::start(DECISION_ID, true, &[("recipe", NONE, 0.95)]).await;
            assert!(run(&calls, Some(&engine)).await.is_none(), "lookup misses");
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
        {
            let engine = FakeEngine::start(DECISION_ID, true, &[("recipe", "rcp_a", 0.95)]).await;
            assert!(
                run(&calls, Some(&engine)).await.is_some(),
                "inputs need the LLM"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
        {
            let engine = FakeEngine::start(DECISION_ID, true, &[("recipe", NONE, 0.5)]).await;
            assert!(
                run(&calls, Some(&engine)).await.is_some(),
                "below threshold: LLM"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 2);
        }
    }

    #[tokio::test]
    async fn engine_off_runs_the_llm() {
        let _serial = test_engine::serial().await;
        let calls = AtomicUsize::new(0);
        assert!(run(&calls, None).await.is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

//! The adversarial progress reviewer, and its `progress_review` decision pack.
//!
//! When a task-backed run's read-only streak reaches a reviewer cadence point
//! (`decision.rs`, `ReviewerGate::Fire`), one LLM pass judges how close the
//! run is to its success criteria and picks the next step: `continue`,
//! `converge`, `redirect` (each injects its steer text into the next turn) or
//! `escalate` (concludes through the synthesized stuck yield). `closeness`,
//! `assessment` and `missing` are only logged. See
//! `docs/archive/plans/2026-07-11-adversarial-progress-reviewer-design.md`.
//!
//! The decision pack asks only the closed `decision`. The LLM stays the
//! incumbent: in shadow (or with the engine off) it decides and the engine's
//! answer is recorded beside it. When the operation is gated and the engine
//! answers eligibly, `escalate` needs no text and `converge` takes the
//! runtime's own synthesis directive (the same one used when the reviewer is
//! unavailable), so the LLM call is skipped; `continue` and `redirect` need the
//! LLM's specific steer, so they fall back to it. The engine never sees or
//! selects the LLM.

use std::{collections::BTreeMap, collections::HashMap, future::Future, time::Duration};

use anyhow::{anyhow, Result};
use decision_engine_contract::{
    batch::DecisionItem,
    request::{Answer, DecisionState},
};
use serde_json::json;

use crate::magician_v2::{
    decision_host::classification::{self, PolicyLookup},
    decisions::{reference, runner, telemetry::Reference},
    prompts::{
        constants::{names, versions},
        rendered_prompt_or,
    },
    query_analysis::operation_llm_router::{
        LLMOperation, OperationLlmRouter, SimplifiedLLMResponse,
    },
};

/// Operation key for the reviewer's LLM call. Dispatched via
/// `LLMOperation::Other`, so it can be bound to a model in the
/// operation-routing config without a Rust routing branch.
pub const OPERATION: &str = "progress_review";
pub const DECISION_ID: &str = "progress_review";
pub const PROJECTION: &str = "progress_review_v1";
const ITEM: &str = "run";
/// Never cut the incumbent: the LLM operation has its own timeout. A gated
/// engine is capped by the operation's decision and queue budgets.
const OUTER: Duration = Duration::from_secs(600);
const GOAL_CHARS: usize = 2_000;
const CRITERIA_CHARS: usize = 2_000;
const SUMMARY_CHARS: usize = 3_000;

/// The reviewer's structured verdict.
#[derive(serde::Deserialize, Debug, Clone)]
pub struct ReviewerVerdict {
    /// 0-100: how close the work so far is to satisfying `success_criteria`.
    #[serde(default)]
    pub closeness: u8,
    /// Short adversarial rationale (why it is / isn't converging).
    #[serde(default)]
    pub assessment: String,
    /// What to do next.
    pub decision: ReviewerDecision,
    /// Directive folded into the current turn's steer for
    /// continue/converge/redirect (ignored for escalate).
    #[serde(default)]
    pub steer_message: String,
    /// What still blocks the deliverable (continue), or why it is escalating.
    #[serde(default)]
    pub missing: Vec<String>,
}

/// The reviewer's next-step decision. `continue`/`converge`/`redirect` inject a
/// steer and let the run proceed; `escalate` routes to the existing yield.
#[derive(serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewerDecision {
    Continue,
    Converge,
    Redirect,
    Escalate,
}

/// What the reviewer judges: the run's requirement and recent work.
#[derive(Debug, Clone)]
pub struct ReviewCase {
    pub goal: String,
    pub success_criteria: String,
    pub iterations: usize,
    pub streak: usize,
    pub cap: usize,
    pub work_summary: String,
}

/// Whose run this is, for the engine's policy and attribution.
#[derive(Debug, Clone, Default)]
pub struct ReviewScope {
    pub principal: String,
    pub workspace: String,
    pub execution_id: Option<String>,
    pub task_id: Option<String>,
    pub agent_id: Option<String>,
}

/// One reviewer LLM call: the parsed verdict (or why it did not parse), with
/// the response and latency for telemetry.
pub struct ReviewCall {
    pub verdict: Result<ReviewerVerdict>,
    pub response: SimplifiedLLMResponse,
    pub latency_ms: u64,
}

/// Compiled fallback for the reviewer SYSTEM prompt when the store copy
/// (`progress_review_system`) is unavailable. Prompts live in the store
/// (`data/magician_v2/prompts/`); this is only the robustness net.
const REVIEWER_SYSTEM_FALLBACK: &str = "You are an adversarial progress reviewer. A task-backed agentic run has spent many consecutive read-only / inspection steps without producing its deliverable. Judge — skeptically — how close it actually is to satisfying the success criteria, and decide the next step. Default to assuming it is spinning unless the evidence shows real convergence. Return STRICT JSON only, no prose, no code fences.";

/// Compiled fallback for the reviewer USER prompt (store key `progress_review`).
fn reviewer_user_fallback(case: &ReviewCase) -> String {
    format!(
        "REQUIREMENT (success criteria):\n{}\n\nGOAL:\n{}\n\nPROGRESS: {} consecutive read-only / inspection steps (of {} before this run is force-concluded).\n\nWORK SO FAR:\n{}\n\nDecide the next step and return STRICT JSON ONLY in this exact shape:\n{{\"closeness\": <0-100 integer of how close the work is to the requirement>, \"assessment\": \"<one skeptical sentence>\", \"decision\": \"continue|converge|redirect|escalate\", \"steer_message\": \"<a specific directive; for converge tell it to STOP researching and synthesise + emit the deliverable now; for continue name exactly what is still missing; for redirect give the better approach; empty for escalate>\", \"missing\": [\"<what still blocks the deliverable>\"]}}\n\nGuidance: prefer converge if enough evidence exists to produce the deliverable; continue only if genuinely closer than before; escalate if blocked or the requirement cannot be met from here.",
        case.success_criteria, case.goal, case.streak, case.cap, case.work_summary
    )
}

/// Parse the reviewer's LLM output into a `ReviewerVerdict`, tolerating markdown
/// code fences and surrounding prose.
pub fn parse_reviewer_verdict(raw: &str) -> Result<ReviewerVerdict> {
    let trimmed = raw.trim();
    let body = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(str::trim_start)
        .unwrap_or(trimmed);
    let body = body.strip_suffix("```").unwrap_or(body).trim();
    let json = match (body.find('{'), body.rfind('}')) {
        (Some(start), Some(end)) if end > start => &body[start..=end],
        _ => body,
    };
    serde_json::from_str::<ReviewerVerdict>(json).map_err(|error| {
        anyhow!(
            "progress review output was not valid JSON ({}); raw: {}",
            error,
            raw.chars().take(200).collect::<String>()
        )
    })
}

/// One reviewer LLM pass with the store prompts (the decision pack's
/// incumbent). A transport failure is an error; an unparseable reply is a
/// `ReviewCall` whose verdict is the parse error.
pub async fn llm_review(router: &OperationLlmRouter, case: &ReviewCase) -> Result<ReviewCall> {
    let variables = HashMap::from([
        ("goal".to_string(), case.goal.clone()),
        (
            "success_criteria".to_string(),
            case.success_criteria.clone(),
        ),
        ("iterations".to_string(), case.iterations.to_string()),
        ("streak".to_string(), case.streak.to_string()),
        ("cap".to_string(), case.cap.to_string()),
        ("work_summary".to_string(), case.work_summary.clone()),
    ]);
    let system_prompt = rendered_prompt_or(
        names::PROGRESS_REVIEW_SYSTEM,
        versions::PROGRESS_REVIEW_SYSTEM,
        HashMap::new(),
        REVIEWER_SYSTEM_FALLBACK,
    )
    .await;
    let user_prompt = rendered_prompt_or(
        names::PROGRESS_REVIEW,
        versions::PROGRESS_REVIEW,
        variables,
        &reviewer_user_fallback(case),
    )
    .await;
    let started = std::time::Instant::now();
    let response = router
        .generate_for_operation_with_system(
            &LLMOperation::Other(OPERATION.to_string()),
            Some(&system_prompt),
            &user_prompt,
        )
        .await
        .map_err(|error| anyhow!("progress review LLM call failed: {}", error))?;
    let latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    Ok(ReviewCall {
        verdict: parse_reviewer_verdict(&response.content),
        response,
        latency_ms,
    })
}

/// Review one run: engine first when gated, the LLM otherwise. An error means
/// neither produced a verdict; the caller applies its synthesis fallback.
pub async fn review<F, Fut>(
    router: &OperationLlmRouter,
    scope: &ReviewScope,
    case: &ReviewCase,
    synthesis_steer: String,
    llm: F,
) -> Result<ReviewerVerdict>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<ReviewCall>>,
{
    let verdict_of = |call: Result<ReviewCall>| call.and_then(|call| call.verdict);
    let Some((input, lookup)) = engine_input(router, scope, case).await else {
        return verdict_of(llm().await);
    };
    if matches!(&lookup, PolicyLookup::Participating(p) if p.policy.gate) {
        let outcome = runner::run(
            input,
            lookup,
            OUTER,
            Duration::ZERO,
            |_, _| async { None::<Result<ReviewCall>> },
            labels,
        )
        .await;
        let answers = outcome.current_answers().remove(ITEM).unwrap_or_default();
        return match resolve_without_llm(&answers, synthesis_steer) {
            Ok(verdict) => Ok(verdict),
            Err(reason) => {
                tracing::debug!(
                    target: "agentic.reviewer",
                    reason,
                    "[PROGRESS-REVIEW] decision engine left the verdict to the LLM"
                );
                verdict_of(llm().await)
            },
        };
    }
    // An engine outage never skips the review.
    if !lookup.allows_incumbent() {
        return verdict_of(llm().await);
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
        Some(call) => verdict_of(call),
        None => Err(anyhow!("progress review did not complete")),
    }
}

async fn engine_input(
    router: &OperationLlmRouter,
    scope: &ReviewScope,
    case: &ReviewCase,
) -> Option<(runner::Input, PolicyLookup)> {
    let principal = scope.principal.trim();
    let workspace = scope.workspace.trim();
    if principal.is_empty() || workspace.is_empty() {
        return None;
    }
    let reference_version = reference::version(router, OPERATION, versions::PROGRESS_REVIEW)?;
    let lookup = classification::ready_policy(DECISION_ID, principal, workspace).await;
    let item = project(case);
    let case_id = blake3::hash(&serde_json::to_vec(&item).ok()?)
        .to_hex()
        .to_string();
    let mut trace = router.classification_trace_context(magicllm::LlmScope::new(
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
            required_questions: vec!["decision".into()],
            scope: trace,
            agent: scope.agent_id.clone(),
            requires_completion: false,
            replay: None,
        },
        lookup,
    ))
}

/// The engine's view of a run: the same fields the reviewer prompt sees.
pub fn project(case: &ReviewCase) -> DecisionItem {
    let clip = |text: &str, chars: usize| text.chars().take(chars).collect::<String>();
    DecisionItem {
        item_id: ITEM.into(),
        state: DecisionState::from_json(json!({
            "success_criteria": clip(&case.success_criteria, CRITERIA_CHARS),
            "goal": clip(&case.goal, GOAL_CHARS),
            "read_only_streak": case.streak,
            "force_conclude_at": case.cap,
            "iterations": case.iterations,
            "work_so_far": clip(&case.work_summary, SUMMARY_CHARS),
        })),
        choice_candidates: BTreeMap::new(),
    }
}

/// A verdict from the engine alone: `escalate` needs no text and `converge`
/// takes the runtime's synthesis directive; `continue` and `redirect` need
/// the LLM's specific steer.
pub fn resolve_without_llm(
    answers: &BTreeMap<String, Answer>,
    synthesis_steer: String,
) -> std::result::Result<ReviewerVerdict, &'static str> {
    let decision = match answers.get("decision") {
        Some(Answer::Choice { choice, .. }) => {
            decision_from_id(choice.as_str()).ok_or("decision_unresolved")?
        },
        _ => return Err("decision_unresolved"),
    };
    let steer_message = match decision {
        ReviewerDecision::Escalate => String::new(),
        ReviewerDecision::Converge => synthesis_steer,
        ReviewerDecision::Continue | ReviewerDecision::Redirect => return Err("steer_needs_llm"),
    };
    Ok(ReviewerVerdict {
        closeness: 0,
        assessment: "decided by the decision engine (progress_review)".to_owned(),
        decision,
        steer_message,
        missing: Vec::new(),
    })
}

fn labels(call: &Result<ReviewCall>) -> Reference {
    let Ok(call) = call else {
        return BTreeMap::new().into();
    };
    let labels = match &call.verdict {
        Ok(verdict) => BTreeMap::from([(
            ITEM.to_owned(),
            BTreeMap::from([("decision".to_owned(), json!(decision_id(verdict.decision)))]),
        )]),
        Err(_) => BTreeMap::new(),
    };
    Reference::from_response(labels, &call.response, call.latency_ms)
}

pub fn decision_id(value: ReviewerDecision) -> &'static str {
    match value {
        ReviewerDecision::Continue => "continue",
        ReviewerDecision::Converge => "converge",
        ReviewerDecision::Redirect => "redirect",
        ReviewerDecision::Escalate => "escalate",
    }
}
fn decision_from_id(id: &str) -> Option<ReviewerDecision> {
    Some(match id {
        "continue" => ReviewerDecision::Continue,
        "converge" => ReviewerDecision::Converge,
        "redirect" => ReviewerDecision::Redirect,
        "escalate" => ReviewerDecision::Escalate,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::decisions::test_engine::{self, FakeEngine};
    use decision_engine_contract::primitives::OptionId;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn case() -> ReviewCase {
        ReviewCase {
            goal: "Compare three laptops".into(),
            success_criteria: "A table comparing price, weight and battery".into(),
            iterations: 14,
            streak: 13,
            cap: 20,
            work_summary: "- iter 13: browser__open shop.example [ok]".into(),
        }
    }

    fn scope() -> ReviewScope {
        ReviewScope {
            principal: "p".into(),
            workspace: "w".into(),
            ..Default::default()
        }
    }

    fn llm_call(raw: &str) -> Result<ReviewCall> {
        Ok(ReviewCall {
            verdict: parse_reviewer_verdict(raw),
            response: SimplifiedLLMResponse::content_only(raw.to_owned()),
            latency_ms: 7,
        })
    }

    fn choice(id: &str) -> Answer {
        Answer::Choice {
            choice: OptionId::new(id),
            probabilities: BTreeMap::new(),
            confidence: 0.9,
        }
    }

    #[test]
    fn option_ids_round_trip() {
        for value in [
            ReviewerDecision::Continue,
            ReviewerDecision::Converge,
            ReviewerDecision::Redirect,
            ReviewerDecision::Escalate,
        ] {
            assert_eq!(decision_from_id(decision_id(value)), Some(value));
        }
    }

    #[test]
    fn engine_resolves_escalate_and_converge_only() {
        let answers = |id| BTreeMap::from([("decision".to_owned(), choice(id))]);
        let escalate = resolve_without_llm(&answers("escalate"), "S".into()).unwrap();
        assert_eq!(escalate.decision, ReviewerDecision::Escalate);
        assert!(escalate.steer_message.is_empty());
        let converge = resolve_without_llm(&answers("converge"), "S".into()).unwrap();
        assert_eq!(
            converge.steer_message, "S",
            "the runtime's synthesis directive"
        );
        for id in ["continue", "redirect"] {
            assert_eq!(
                resolve_without_llm(&answers(id), "S".into()).err(),
                Some("steer_needs_llm")
            );
        }
        assert_eq!(
            resolve_without_llm(&BTreeMap::new(), "S".into()).err(),
            Some("decision_unresolved")
        );
    }

    #[test]
    fn labels_carry_the_llm_decision_and_skip_unparsed_replies() {
        let reference = labels(&llm_call(r#"{"decision":"redirect"}"#));
        assert_eq!(reference.labels[ITEM]["decision"], "redirect");
        assert_eq!(reference.latency_ms, 7);
        assert!(labels(&llm_call("not json")).labels.is_empty());
    }

    #[test]
    fn projection_is_bounded() {
        let mut long = case();
        long.work_summary = "x".repeat(10_000);
        let item = project(&long);
        assert_eq!(
            item.state.0["work_so_far"].as_str().unwrap().len(),
            SUMMARY_CHARS
        );
        assert_eq!(item.state.0["read_only_streak"], 13);
    }

    async fn run(calls: &AtomicUsize, raw: &'static str) -> Result<ReviewerVerdict> {
        let router = test_engine::fixture_router(OPERATION);
        review(&router, &scope(), &case(), "SYNTH".into(), || async {
            calls.fetch_add(1, Ordering::SeqCst);
            llm_call(raw)
        })
        .await
    }

    #[tokio::test]
    async fn shadow_keeps_the_llm_verdict_and_asks_the_engine_beside_it() {
        let _serial = test_engine::serial().await;
        let engine = FakeEngine::start(DECISION_ID, false, &[("decision", "escalate", 0.95)]).await;
        let calls = AtomicUsize::new(0);
        let verdict = run(
            &calls,
            r#"{"decision":"continue","steer_message":"find prices"}"#,
        )
        .await
        .expect("verdict");
        assert_eq!(
            verdict.decision,
            ReviewerDecision::Continue,
            "the LLM decides"
        );
        assert_eq!(verdict.steer_message, "find prices");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let request = engine.wait_for_request().await;
        assert_eq!(request.batch.projection_version, PROJECTION);
        assert_eq!(
            request.batch.items[0].state.0["goal"],
            "Compare three laptops"
        );
    }

    #[tokio::test]
    async fn gate_skips_the_llm_for_escalate_and_falls_back_for_continue() {
        let _serial = test_engine::serial().await;
        let calls = AtomicUsize::new(0);
        {
            let _engine =
                FakeEngine::start(DECISION_ID, true, &[("decision", "converge", 0.95)]).await;
            let verdict = run(&calls, r#"{"decision":"escalate"}"#)
                .await
                .expect("verdict");
            assert_eq!(verdict.decision, ReviewerDecision::Converge);
            assert_eq!(verdict.steer_message, "SYNTH");
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
        {
            let _engine =
                FakeEngine::start(DECISION_ID, true, &[("decision", "continue", 0.95)]).await;
            let verdict = run(&calls, r#"{"decision":"continue","steer_message":"x"}"#)
                .await
                .expect("verdict");
            assert_eq!(verdict.steer_message, "x", "continue needs the LLM's steer");
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
        {
            let _engine =
                FakeEngine::start(DECISION_ID, true, &[("decision", "escalate", 0.4)]).await;
            let verdict = run(&calls, r#"{"decision":"redirect","steer_message":"y"}"#)
                .await
                .expect("verdict");
            assert_eq!(
                verdict.decision,
                ReviewerDecision::Redirect,
                "below threshold"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 2);
        }
    }

    #[tokio::test]
    async fn without_a_scope_the_llm_decides_and_its_errors_surface() {
        let _serial = test_engine::serial().await;
        let router = test_engine::fixture_router(OPERATION);
        let verdict = review(
            &router,
            &ReviewScope::default(),
            &case(),
            "S".into(),
            || async { llm_call(r#"{"decision":"escalate"}"#) },
        )
        .await
        .expect("verdict");
        assert_eq!(verdict.decision, ReviewerDecision::Escalate);
        let error = review(&router, &scope(), &case(), "S".into(), || async {
            llm_call("garbage")
        })
        .await;
        assert!(
            error.is_err(),
            "an unparseable reply is the caller's fallback"
        );
    }
}

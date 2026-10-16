//! The typed path for a preference check: Jev answers the
//! `action_alignment` pack through the Decision Engine, one item per offered
//! intention; the host composer (`magician_alignment::composer`) turns the
//! answers into an `alignment_decision.v2`; and the same host validator every
//! judge's answer goes through accepts it.
//!
//! The engine call goes through the shared decisions runner in gate mode, so
//! it carries the engine's policy revision, budget accounting and telemetry
//! like every other decision operation. The engine is configured to mark every
//! `action_alignment` answer eligible (operator-enabled thresholds that every
//! answer meets): uncertainty is the composer's to judge, against the owner's
//! bars, not the engine's.
//!
//! When the typed path leaves the check unresolved (the engine is unavailable,
//! an item went unanswered, or the composer could not settle it) and at least
//! [`FALLBACK_MIN_REMAINING`] of the check's budget is left, the generative
//! judge answers instead (owner decision, 2026-10-11). Otherwise the check is
//! unresolved and the act is held, as for any unresolved check.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use decision_engine_contract::batch::DecisionItem;
use decision_engine_contract::primitives::QuestionId;
use decision_engine_contract::request::{Answer, DecisionState};

use magician_alignment::composer::{
    compose, heads_from_answers, project, ComposerBars, CANCEL_QUESTION, DECIDES_QUESTION,
    ENGINE_OPERATION, ENGINE_PACK_VERSION, FIRM_QUESTION, MUTED_QUESTION, PROJECTION_VERSION,
    RELATION_QUESTION, SPECIFIED_QUESTION, SUBJECT_QUESTION,
};
use magician_alignment::contract::{validate_decision, AlignmentInput, ValidatedDecision};
use magician_alignment::intention::CoverageStatus;

use super::judge::{parse_and_validate, reference_call, JudgeOutcome, UnresolvedCause};
use crate::magician_v2::decision_host::classification::{ready_policy, PolicyLookup};
use crate::magician_v2::decisions::runner;
use crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;

/// Least time left for the generative fallback to be worth starting; below
/// it the check is unresolved at once rather than timing out later.
pub const FALLBACK_MIN_REMAINING: Duration = Duration::from_millis(1_500);

/// Every question the composer reads; an item missing any is unanswered.
const QUESTIONS: [&str; 7] = [
    RELATION_QUESTION,
    CANCEL_QUESTION,
    SPECIFIED_QUESTION,
    FIRM_QUESTION,
    SUBJECT_QUESTION,
    MUTED_QUESTION,
    DECIDES_QUESTION,
];

/// The reference identity of a typed decision, for cache keys and logs.
pub fn typed_reference_version() -> String {
    format!("typed:{ENGINE_OPERATION}@{ENGINE_PACK_VERSION}/{PROJECTION_VERSION}")
}

/// One check through the typed path, with the generative fallback.
pub async fn judge_typed(
    input: &AlignmentInput,
    fallback: Option<&OperationLlmRouter>,
    bars: &ComposerBars,
    budget: Duration,
) -> JudgeOutcome {
    let started = Instant::now();
    let elapsed = |started: Instant| started.elapsed().as_millis() as u64;
    let coverage_complete = input.coverage.status == CoverageStatus::Complete;
    let reason = match typed_decision(input, fallback, bars, budget).await {
        Ok(decision) => {
            return JudgeOutcome::Decided {
                decision,
                reference_version: Some(typed_reference_version()),
                coverage_complete,
                latency_ms: elapsed(started),
            }
        },
        Err(reason) => reason,
    };
    let remaining = budget.saturating_sub(started.elapsed());
    let Some(router) = fallback.filter(|_| remaining >= FALLBACK_MIN_REMAINING) else {
        tracing::warn!(
            reason = %reason,
            "[PREFERENCE-ALIGNMENT] typed path unresolved and no time for the generative fallback"
        );
        return JudgeOutcome::Unresolved {
            cause: UnresolvedCause::Provider(format!("typed path: {reason}")),
            latency_ms: elapsed(started),
        };
    };
    tracing::info!(
        reason = %reason,
        remaining_ms = remaining.as_millis() as u64,
        "[PREFERENCE-ALIGNMENT] typed path unresolved; asking the generative judge"
    );
    match reference_call(router, input, remaining).await {
        Err(cause) => JudgeOutcome::Unresolved {
            cause,
            latency_ms: elapsed(started),
        },
        Ok((reference_version, response)) => match parse_and_validate(input, &response.content) {
            Ok(decision) => JudgeOutcome::Decided {
                decision,
                reference_version: Some(reference_version),
                coverage_complete,
                latency_ms: elapsed(started),
            },
            Err(cause) => JudgeOutcome::Unresolved {
                cause,
                latency_ms: elapsed(started),
            },
        },
    }
}

/// The typed decision, or why there is none.
async fn typed_decision(
    input: &AlignmentInput,
    fallback: Option<&OperationLlmRouter>,
    bars: &ComposerBars,
    budget: Duration,
) -> Result<ValidatedDecision, String> {
    let lookup = ready_policy(ENGINE_OPERATION, &input.principal, &input.workspace).await;
    match &lookup {
        PolicyLookup::Participating(participation) if participation.policy.gate => {},
        PolicyLookup::Participating(_) => return Err("the engine's action_alignment gate is off".into()),
        PolicyLookup::Disabled => return Err("the decision engine is off".into()),
        PolicyLookup::Unavailable(failure) => {
            return Err(format!("the decision engine policy is unavailable ({failure:?})"))
        },
    }
    let (context, items) = project(input);
    let case_id = blake3::hash(
        &serde_json::to_vec(&serde_json::json!([&context, &items])).unwrap_or_default(),
    )
    .to_hex()
    .to_string();
    let scope = magicllm::LlmScope::new(input.principal.clone(), input.workspace.clone());
    let trace = match fallback {
        Some(router) => router.classification_trace_context(scope),
        None => {
            let mut trace = magicllm::LlmTraceContext::new(scope, magicllm::LlmWorkloadClass::Ambient);
            trace.call_role = magicllm::LlmCallRole::Judge;
            trace
        },
    };
    let request = runner::Input {
        operation: ENGINE_OPERATION.to_string(),
        projection_version: PROJECTION_VERSION.to_string(),
        reference_version: typed_reference_version(),
        case_id,
        context: Some(DecisionState::from_json(context)),
        items: items
            .into_iter()
            .map(|(item_id, state)| DecisionItem {
                item_id,
                state: DecisionState::from_json(state),
                choice_candidates: BTreeMap::new(),
            })
            .collect(),
        required_questions: QUESTIONS.iter().map(|question| question.to_string()).collect(),
        scope: trace,
        agent: None,
        requires_completion: false,
        replay: None,
    };
    let outcome = runner::run::<(), _, _, _>(
        request,
        lookup,
        budget,
        Duration::ZERO,
        |_, _| async { None },
        |_: &()| BTreeMap::new().into(),
    )
    .await;
    if let Some(origin) = outcome
        .origins
        .values()
        .find(|origin| origin.pack_version != ENGINE_PACK_VERSION)
    {
        return Err(format!(
            "the engine answered with pack {}, not {ENGINE_PACK_VERSION}",
            origin.pack_version
        ));
    }
    let answers = outcome.current_answers();
    let mut heads = BTreeMap::new();
    for intention in &input.intentions {
        let item = answers
            .get(&intention.id)
            .ok_or_else(|| format!("no answer for intention `{}`", intention.id))?;
        let typed: BTreeMap<QuestionId, Answer> = item
            .iter()
            .map(|(question, answer)| (QuestionId::new(question.as_str()), answer.clone()))
            .collect();
        let parsed = heads_from_answers(&typed)
            .ok_or_else(|| format!("malformed answer for intention `{}`", intention.id))?;
        heads.insert(intention.id.clone(), parsed);
    }
    let decision = compose(input, &heads, bars).map_err(|unresolved| unresolved.to_string())?;
    validate_decision(input, decision).map_err(|rejection| format!("{rejection:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::preference_alignment::contract::{
        AlignmentRequest, EffectivePolicy, ALIGNMENT_INPUT_SCHEMA_VERSION,
    };
    use crate::magician_v2::preference_alignment::intention::{
        AssertionStatus, Coverage, Intention, IntentionKind, IntentionLifecycle, IntentionSource,
        StandingGeneration, INTENTION_SCHEMA_VERSION,
    };

    fn input() -> AlignmentInput {
        AlignmentInput {
            schema_version: ALIGNMENT_INPUT_SCHEMA_VERSION.to_string(),
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            execution_id: None,
            request: AlignmentRequest {
                revision: 1,
                text: "buy the headphones".to_string(),
                owner_spans: Vec::new(),
            },
            intentions: vec![Intention {
                schema_version: INTENTION_SCHEMA_VERSION.to_string(),
                id: "i-budget".to_string(),
                revision: "r1".to_string(),
                source: IntentionSource::NativeTier {
                    tier: "preferences".to_string(),
                },
                kind: IntentionKind::StandingInstruction,
                assertion_status: AssertionStatus::DirectOwnerStatement,
                statement: "Check with me before purchases over 5,000 rupees.".to_string(),
                conditions: None,
                scope: None,
                lifecycle: IntentionLifecycle::Active,
                valid_until: None,
                evidence_refs: Vec::new(),
                intervention_policy: None,
            }],
            coverage: Coverage {
                status: CoverageStatus::Complete,
                omitted_ids: Vec::new(),
                unavailable_sources: Vec::new(),
            },
            host_matched_exceptions: Vec::new(),
            action: None,
            pending_gates: Vec::new(),
            policy: EffectivePolicy {
                global: None,
                version: "policy.v1".to_string(),
            },
            generation: StandingGeneration::default(),
        }
    }

    /// With no engine to answer and no generative fallback, the check is
    /// unresolved (the act holds); it never decides on its own.
    #[tokio::test]
    async fn no_engine_and_no_fallback_leaves_the_check_unresolved() {
        let outcome = judge_typed(
            &input(),
            None,
            &ComposerBars::default(),
            Duration::from_secs(4),
        )
        .await;
        match outcome {
            JudgeOutcome::Unresolved {
                cause: UnresolvedCause::Provider(reason),
                ..
            } => assert!(reason.starts_with("typed path: "), "{reason}"),
            other => panic!("expected an unresolved check, got {other:?}"),
        }
    }

    /// The engine backend goes through the shared judge entry, which keeps the
    /// verified-empty fast path ahead of any engine call.
    #[tokio::test]
    async fn the_engine_backend_keeps_the_empty_inventory_fast_path() {
        let mut empty = input();
        empty.intentions.clear();
        let outcome = super::super::judge::judge_with(
            Some(super::super::judge::JudgeBackend::Engine {
                fallback: None,
                bars: ComposerBars::default(),
            }),
            &empty,
            Duration::from_secs(4),
        )
        .await;
        assert!(matches!(outcome, JudgeOutcome::NothingToCompare), "{outcome:?}");
    }

    #[test]
    fn the_typed_reference_names_the_pack_and_projection() {
        assert_eq!(
            typed_reference_version(),
            "typed:action_alignment@1.3.0/action_alignment_projection.v2"
        );
        assert_eq!(QUESTIONS.len(), 7);
    }
}

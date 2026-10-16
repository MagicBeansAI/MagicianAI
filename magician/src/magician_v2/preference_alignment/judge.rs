//! The generative reference judge for `action_alignment` (plan §7.3).
//!
//! One bounded call through the router's bound `action_alignment_judge`
//! profile, its answer parsed strictly and passed through
//! [`validate_decision`]. **Every failure is unresolved** — no router, an
//! unbound route, a missing prompt, a timeout, a provider error, an
//! unparseable or rejected answer. None of them becomes `proceed`.
//!
//! That is the opposite of `memory_applicability`, whose every failure serves
//! the incoming order: a ranking judge failing costs a slightly worse prompt,
//! while an alignment judge failing open would release an act the owner asked
//! to be consulted about.
//!
//! A complete, structurally empty inventory never reaches the model: it is
//! host-origin `nothing_to_compare` evidence (plan §6.1), not an `aligned`
//! verdict.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::contract::{
    validate_decision, AlignmentDecision, AlignmentInput, Continuation, DecisionRejection,
    Intervention, ValidatedDecision,
};
use super::intention::CoverageStatus;
use crate::config::PREFERENCE_ALIGNMENT_JUDGE_OPERATION;
use crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;

/// Why a check could not produce a usable decision. Each is a distinct,
/// metered failure stage (plan §12.1); all of them leave the act held when
/// enforcement requires it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cause", content = "detail", rename_all = "snake_case")]
pub enum UnresolvedCause {
    NoRouter,
    /// `action_alignment_judge` is not bound. Under enforce this is a
    /// rejected configuration; reaching it at runtime means the config
    /// changed underneath a running check.
    Unbound,
    PromptUnavailable(String),
    Timeout,
    Provider(String),
    Malformed(String),
    Rejected(String),
    /// The task spent `budgets.max_judge_calls_per_task`. Nothing was asked.
    BudgetSpent,
}

/// The result of one alignment check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum JudgeOutcome {
    /// Complete coverage and no structurally eligible intention: nothing was
    /// compared and no model was called.
    NothingToCompare,
    Decided {
        decision: ValidatedDecision,
        /// Reference identity of the bound profile, for cache keys and
        /// shadow comparison.
        reference_version: Option<String>,
        /// Whether the input's coverage was complete. A proceed over partial
        /// coverage is not a clear-alignment receipt.
        coverage_complete: bool,
        latency_ms: u64,
    },
    Unresolved {
        cause: UnresolvedCause,
        latency_ms: u64,
    },
}

/// What admission may do with an outcome, before any other gate is composed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlignmentDisposition {
    /// No alignment objection. Every other gate still applies.
    Proceed,
    /// Proceed, and show the owner the advisory nudge.
    ProceedWithNudge,
    /// Hold the act and publish the interaction.
    Hold,
    /// The current owner turn cancelled the work.
    Cancel,
    /// The check is unresolved: hold the act and offer the owner recovery
    /// (retry, cancel, or an explicit `proceed_without_check`).
    Unresolved,
}

impl JudgeOutcome {
    pub fn disposition(&self) -> AlignmentDisposition {
        match self {
            Self::NothingToCompare => AlignmentDisposition::Proceed,
            Self::Unresolved { .. } => AlignmentDisposition::Unresolved,
            Self::Decided {
                decision,
                coverage_complete,
                ..
            } => match decision.decision.continuation {
                Continuation::Cancel => AlignmentDisposition::Cancel,
                Continuation::Wait => AlignmentDisposition::Hold,
                // A proceed found no conflict in what was offered; with
                // partial coverage that is not evidence about what was not.
                Continuation::Proceed if !coverage_complete => AlignmentDisposition::Unresolved,
                Continuation::Proceed if decision.decision.intervention == Intervention::Nudge => {
                    AlignmentDisposition::ProceedWithNudge
                },
                Continuation::Proceed => AlignmentDisposition::Proceed,
            },
        }
    }
}

/// A provider-free judge: returns the raw model text for one input, exactly
/// as a model would. Its answer still goes through the strict parse and the
/// host validator, so a scripted judge exercises every host rule a real one
/// does. Used by the journey eval lane and tests; production configures only
/// the router.
pub trait ScriptedJudge: Send + Sync {
    /// `Ok(raw)` is the model's text; `Err(reason)` is a provider failure.
    fn respond(&self, input: &AlignmentInput) -> Result<String, String>;
}

/// Where judge calls go.
#[derive(Clone, Copy)]
pub enum JudgeBackend<'a> {
    /// The governed `action_alignment_judge` route.
    Router(&'a OperationLlmRouter),
    /// A scripted responder (journey lane, tests).
    Scripted(&'a dyn ScriptedJudge),
    /// The typed path: Jev through the Decision Engine, composed by the host
    /// (`engine_judge.rs`), falling back to `fallback` when it leaves the
    /// check unresolved and time allows.
    Engine {
        fallback: Option<&'a OperationLlmRouter>,
        bars: magician_alignment::composer::ComposerBars,
    },
}

/// The reference version a scripted judge reports.
pub const SCRIPTED_JUDGE_REFERENCE: &str = "scripted";

/// Run one check. `budget` bounds the whole judge call; past it the check is
/// unresolved.
pub async fn judge(
    router: Option<&OperationLlmRouter>,
    input: &AlignmentInput,
    budget: Duration,
) -> JudgeOutcome {
    judge_with(router.map(JudgeBackend::Router), input, budget).await
}

/// [`judge`] against an explicit backend.
pub async fn judge_with(
    backend: Option<JudgeBackend<'_>>,
    input: &AlignmentInput,
    budget: Duration,
) -> JudgeOutcome {
    let started = Instant::now();
    let elapsed = |started: Instant| started.elapsed().as_millis() as u64;

    if input.coverage.status == CoverageStatus::Complete && input.intentions.is_empty() {
        return JudgeOutcome::NothingToCompare;
    }
    let router = match backend {
        Some(JudgeBackend::Engine { fallback, bars }) => {
            return super::engine_judge::judge_typed(input, fallback, &bars, budget).await;
        },
        Some(JudgeBackend::Scripted(scripted)) => {
            return match scripted.respond(input) {
                Err(error) => JudgeOutcome::Unresolved {
                    cause: UnresolvedCause::Provider(bounded(&error)),
                    latency_ms: elapsed(started),
                },
                Ok(raw) => match parse_and_validate(input, &raw) {
                    Ok(decision) => JudgeOutcome::Decided {
                        decision,
                        reference_version: Some(SCRIPTED_JUDGE_REFERENCE.to_string()),
                        coverage_complete: input.coverage.status == CoverageStatus::Complete,
                        latency_ms: elapsed(started),
                    },
                    Err(cause) => JudgeOutcome::Unresolved {
                        cause,
                        latency_ms: elapsed(started),
                    },
                },
            };
        },
        Some(JudgeBackend::Router(router)) => Some(router),
        None => None,
    };
    let Some(router) = router else {
        return JudgeOutcome::Unresolved {
            cause: UnresolvedCause::NoRouter,
            latency_ms: elapsed(started),
        };
    };
    let (reference_version, response) = match reference_call(router, input, budget).await {
        Ok(answer) => answer,
        Err(cause) => {
            return JudgeOutcome::Unresolved {
                cause,
                latency_ms: elapsed(started),
            }
        },
    };

    match parse_and_validate(input, &response.content) {
        Ok(decision) => JudgeOutcome::Decided {
            decision,
            reference_version: Some(reference_version),
            coverage_complete: input.coverage.status == CoverageStatus::Complete,
            latency_ms: elapsed(started),
        },
        Err(cause) => JudgeOutcome::Unresolved {
            cause,
            latency_ms: elapsed(started),
        },
    }
}

/// One call to the bound reference judge: the pinned route, the versioned
/// prompt and the time budget, returning the raw answer with the reference
/// version it came from. Nothing is parsed or validated here — production
/// does that next ([`parse_and_validate`]); the live eval lane records the raw
/// answer so the same call scores both the governed judge and the prompt on
/// its own.
///
/// Boxed and type-erased: inlined into every caller's future, its body pushed
/// the auto-trait check of the API's handler futures past the compiler's
/// recursion limit.
pub fn reference_call<'a>(
    router: &'a OperationLlmRouter,
    input: &'a AlignmentInput,
    budget: Duration,
) -> futures_util::future::BoxFuture<
    'a,
    Result<
        (
            String,
            crate::magician_v2::query_analysis::operation_llm_router::SimplifiedLLMResponse,
        ),
        UnresolvedCause,
    >,
> {
    Box::pin(async move {
        let prompt_version =
            crate::magician_v2::prompts::constants::versions::ACTION_ALIGNMENT_JUDGE;
        let reference_version = crate::magician_v2::decisions::reference::version(
            router,
            PREFERENCE_ALIGNMENT_JUDGE_OPERATION,
            prompt_version,
        )
        .ok_or(UnresolvedCause::Unbound)?;
        let system = judge_prompt(input).await?;
        let call = crate::magician_v2::decisions::reference::pinned_json(
            router,
            PREFERENCE_ALIGNMENT_JUDGE_OPERATION,
            &input.principal,
            &input.workspace,
            &system,
            JUDGE_USER_MESSAGE,
        );
        match tokio::time::timeout(budget, call).await {
            Err(_) => Err(UnresolvedCause::Timeout),
            Ok(Err(error)) => Err(UnresolvedCause::Provider(bounded(&error.to_string()))),
            Ok(Ok(response)) => Ok((reference_version, response)),
        }
    })
}

/// Run one check over batched inputs (see `collect::build_inputs`) in
/// parallel and combine them: the most restrictive outcome wins. Any
/// unresolved batch makes the whole check unresolved — a batch that could not
/// be judged may hold exactly the instruction that matters.
pub async fn judge_batches(
    router: Option<&OperationLlmRouter>,
    inputs: &[AlignmentInput],
    budget: Duration,
) -> JudgeOutcome {
    judge_batches_with(router.map(JudgeBackend::Router), inputs, budget).await
}

/// [`judge_batches`] against an explicit backend.
pub async fn judge_batches_with(
    backend: Option<JudgeBackend<'_>>,
    inputs: &[AlignmentInput],
    budget: Duration,
) -> JudgeOutcome {
    let outcomes = futures_util::future::join_all(
        inputs
            .iter()
            .map(|input| judge_with(backend, input, budget)),
    )
    .await;
    combine(outcomes)
}

/// Fold batch outcomes. Precedence: cancel, a confirmation hold, unresolved,
/// a clarification hold, a choice hold, a nudge, proceed. Within a rank the
/// first batch wins, and batches lead with explicit standing instructions.
///
/// The owner's cancellation is never hidden behind another batch's failure,
/// and a known conflict with an explicit instruction is never reduced to a
/// "could not be checked" waiver the owner could wave through. A hold chosen
/// over a batch that could not be judged says so in its explanation.
pub fn combine(outcomes: Vec<JudgeOutcome>) -> JudgeOutcome {
    fn rank(outcome: &JudgeOutcome) -> u8 {
        match outcome {
            JudgeOutcome::Unresolved { .. } => 2,
            JudgeOutcome::Decided { decision, .. } => {
                match (
                    decision.decision.continuation,
                    decision.decision.intervention,
                ) {
                    (Continuation::Cancel, _) => 0,
                    (Continuation::Wait, Intervention::Confirm) => 1,
                    (Continuation::Wait, Intervention::Clarify) => 3,
                    (Continuation::Wait, _) => 4,
                    (Continuation::Proceed, Intervention::None) => 6,
                    (Continuation::Proceed, _) => 5,
                }
            },
            JudgeOutcome::NothingToCompare => 7,
        }
    }
    let any_unresolved = outcomes
        .iter()
        .any(|outcome| matches!(outcome, JudgeOutcome::Unresolved { .. }));
    let mut best: Option<JudgeOutcome> = None;
    for outcome in outcomes {
        let replace = match &best {
            None => true,
            Some(current) => rank(&outcome) < rank(current),
        };
        if replace {
            best = Some(outcome);
        }
    }
    if any_unresolved {
        if let Some(JudgeOutcome::Decided { decision, .. }) = best.as_mut() {
            if decision.decision.continuation == Continuation::Wait {
                decision.decision.explanation.push_str(
                    " Part of this action could not be checked against your other saved \
                     preferences.",
                );
            }
        }
    }
    best.unwrap_or(JudgeOutcome::Unresolved {
        cause: UnresolvedCause::Malformed("no batch was judged".to_string()),
        latency_ms: 0,
    })
}

/// The user turn of the judge call. The input travels in the system prompt's
/// `{alignment_input}` slot, where the prompt marks it as data.
const JUDGE_USER_MESSAGE: &str =
    "Judge the proposed act in the input. Return only the alignment_decision.v2 JSON object.";

async fn judge_prompt(input: &AlignmentInput) -> Result<String, UnresolvedCause> {
    let rendered_input = serde_json::to_string_pretty(input)
        .map_err(|error| UnresolvedCause::PromptUnavailable(bounded(&error.to_string())))?;
    let mut variables = HashMap::new();
    variables.insert("alignment_input".to_string(), rendered_input);
    crate::magician_v2::prompts::rendered_prompt(
        crate::magician_v2::prompts::constants::names::ACTION_ALIGNMENT_JUDGE,
        crate::magician_v2::prompts::constants::versions::ACTION_ALIGNMENT_JUDGE,
        variables,
    )
    .await
    .map_err(|error| UnresolvedCause::PromptUnavailable(bounded(&error.to_string())))
}

/// Strict parse: one JSON object, nothing else. A fenced or chatty answer is
/// malformed rather than repaired — repairing is how a permissive reading
/// sneaks in.
pub fn parse_and_validate(
    input: &AlignmentInput,
    raw: &str,
) -> Result<ValidatedDecision, UnresolvedCause> {
    let decision: AlignmentDecision = serde_json::from_str(raw.trim())
        .map_err(|error| UnresolvedCause::Malformed(bounded(&error.to_string())))?;
    validate_decision(input, decision)
        .map_err(|rejection: DecisionRejection| UnresolvedCause::Rejected(rejection.to_string()))
}

fn bounded(text: &str) -> String {
    const LIMIT: usize = 300;
    if text.chars().count() <= LIMIT {
        text.to_string()
    } else {
        let mut cut: String = text.chars().take(LIMIT).collect();
        cut.push('…');
        cut
    }
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

    fn input(intentions: Vec<Intention>, coverage: CoverageStatus) -> AlignmentInput {
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
            intentions,
            coverage: Coverage {
                status: coverage,
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

    fn budget_rule() -> Intention {
        Intention {
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
        }
    }

    #[tokio::test]
    async fn a_verified_empty_inventory_never_calls_the_model() {
        let outcome = judge(
            None,
            &input(Vec::new(), CoverageStatus::Complete),
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(outcome, JudgeOutcome::NothingToCompare);
        assert_eq!(outcome.disposition(), AlignmentDisposition::Proceed);
    }

    #[tokio::test]
    async fn an_empty_but_failed_collection_is_not_nothing_to_compare() {
        let outcome = judge(
            None,
            &input(Vec::new(), CoverageStatus::Unavailable),
            Duration::from_secs(1),
        )
        .await;
        assert!(matches!(
            outcome,
            JudgeOutcome::Unresolved {
                cause: UnresolvedCause::NoRouter,
                ..
            }
        ));
        assert_eq!(outcome.disposition(), AlignmentDisposition::Unresolved);
    }

    #[test]
    fn a_fenced_or_chatty_answer_is_malformed_not_repaired() {
        let input = input(vec![budget_rule()], CoverageStatus::Complete);
        let fenced = "```json\n{\"schema_version\":\"alignment_decision.v2\"}\n```";
        assert!(matches!(
            parse_and_validate(&input, fenced),
            Err(UnresolvedCause::Malformed(_))
        ));
    }

    /// A v1 answer has no holding list, so its force cannot be capped by
    /// what it rests on: it is rejected, never read as v2.
    #[test]
    fn a_v1_answer_is_rejected() {
        let input = input(vec![budget_rule()], CoverageStatus::Complete);
        let raw = r#"{"schema_version":"alignment_decision.v1","assessment":"conflict",
            "intervention":"confirm","continuation":"wait","supporting_intentions":["i-budget"],
            "explanation":"Over budget.","resolution_choices":["allow_once"]}"#;
        assert!(matches!(
            parse_and_validate(&input, raw),
            Err(UnresolvedCause::Rejected(_))
        ));
    }

    #[test]
    fn an_invented_intention_is_rejected() {
        let input = input(vec![budget_rule()], CoverageStatus::Complete);
        let raw = r#"{"schema_version":"alignment_decision.v2","assessment":"conflict",
            "intervention":"confirm","continuation":"wait","supporting_intentions":["i-made-up"],"holding_intentions":["i-made-up"],
            "explanation":"Over budget.","resolution_choices":["allow_once"]}"#;
        assert!(matches!(
            parse_and_validate(&input, raw),
            Err(UnresolvedCause::Rejected(_))
        ));
    }

    #[test]
    fn the_most_restrictive_batch_wins() {
        let input = input(vec![budget_rule()], CoverageStatus::Complete);
        let hold = r#"{"schema_version":"alignment_decision.v2","assessment":"conflict",
            "intervention":"confirm","continuation":"wait","supporting_intentions":["i-budget"],"holding_intentions":["i-budget"],
            "explanation":"Over budget.","resolution_choices":["allow_once"]}"#;
        let aligned = r#"{"schema_version":"alignment_decision.v2","assessment":"aligned",
            "intervention":"none","continuation":"proceed"}"#;
        let decided = |raw: &str| JudgeOutcome::Decided {
            decision: parse_and_validate(&input, raw).expect("valid"),
            reference_version: None,
            coverage_complete: true,
            latency_ms: 1,
        };
        let combined = combine(vec![decided(aligned), decided(hold)]);
        assert_eq!(combined.disposition(), AlignmentDisposition::Hold);

        let with_failure = combine(vec![
            decided(hold),
            JudgeOutcome::Unresolved {
                cause: UnresolvedCause::Timeout,
                latency_ms: 1,
            },
        ]);
        // A known conflict with an explicit instruction is never reduced to
        // a waiver; the hold says part of the act was not checked.
        assert_eq!(with_failure.disposition(), AlignmentDisposition::Hold);
        match &with_failure {
            JudgeOutcome::Decided { decision, .. } => {
                assert!(decision.decision.explanation.contains("could not be checked"))
            },
            other => panic!("expected the hold, got {other:?}"),
        }
        let choice = r#"{"schema_version":"alignment_decision.v2","assessment":"conflict",
            "intervention":"clarify","continuation":"wait","supporting_intentions":["i-budget"],"holding_intentions":["i-budget"],
            "explanation":"Over budget.","resolution_choices":["allow_once"]}"#;
        if let Ok(choice) = parse_and_validate(&input, choice) {
            let unresolved_first = combine(vec![
                JudgeOutcome::Decided {
                    decision: choice,
                    reference_version: None,
                    coverage_complete: true,
                    latency_ms: 1,
                },
                JudgeOutcome::Unresolved {
                    cause: UnresolvedCause::Timeout,
                    latency_ms: 1,
                },
            ]);
            assert_eq!(unresolved_first.disposition(), AlignmentDisposition::Unresolved);
        }
        assert_eq!(
            combine(Vec::new()).disposition(),
            AlignmentDisposition::Unresolved
        );
    }

    #[test]
    fn dispositions_follow_the_validated_decision() {
        let input_complete = input(vec![budget_rule()], CoverageStatus::Complete);
        let hold = r#"{"schema_version":"alignment_decision.v2","assessment":"conflict",
            "intervention":"confirm","continuation":"wait","supporting_intentions":["i-budget"],"holding_intentions":["i-budget"],
            "explanation":"This is over your budget.","resolution_choices":["allow_once","cancel"]}"#;
        let decision = parse_and_validate(&input_complete, hold).expect("valid");
        let outcome = JudgeOutcome::Decided {
            decision,
            reference_version: None,
            coverage_complete: true,
            latency_ms: 1,
        };
        assert_eq!(outcome.disposition(), AlignmentDisposition::Hold);

        let aligned = r#"{"schema_version":"alignment_decision.v2","assessment":"aligned",
            "intervention":"none","continuation":"proceed"}"#;
        let decision = parse_and_validate(&input_complete, aligned).expect("valid");
        let partial = JudgeOutcome::Decided {
            decision: decision.clone(),
            reference_version: None,
            coverage_complete: false,
            latency_ms: 1,
        };
        assert_eq!(partial.disposition(), AlignmentDisposition::Unresolved);
        let complete = JudgeOutcome::Decided {
            decision,
            reference_version: None,
            coverage_complete: true,
            latency_ms: 1,
        };
        assert_eq!(complete.disposition(), AlignmentDisposition::Proceed);
    }
}

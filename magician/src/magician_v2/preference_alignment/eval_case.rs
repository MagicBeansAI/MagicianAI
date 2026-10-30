//! The portable eval case (`preference_alignment_case.v1`): the case types
//! and host adapter live in [`magician_alignment::eval_case`]; this module
//! adds the scripted judge that answers each case through `magician`'s judge.

use std::collections::HashMap;

pub use magician_alignment::eval_case::*;

use super::contract::{
    AlignmentDecision, AlignmentInput, Assessment, Continuation, Intervention,
    ALIGNMENT_DECISION_SCHEMA_VERSION,
};
use super::judge::ScriptedJudge;

/// A scripted judge that answers each case with a fixed raw response, keyed
/// by case id. A case with no entry answers a provider error.
pub struct CaseScript {
    responses: HashMap<String, String>,
}

impl CaseScript {
    pub fn new(responses: HashMap<String, String>) -> Self {
        Self { responses }
    }

    /// The oracle: every case answered with its first reachable outcome.
    pub fn oracle(cases: &[EvalCase]) -> Self {
        let responses = cases
            .iter()
            .filter_map(|case| {
                let index = *case.reachable_outcomes().first()?;
                let decision = case.oracle_decision(&case.acceptable[index])?;
                Some((case.id.clone(), serde_json::to_string(&decision).ok()?))
            })
            .collect();
        Self { responses }
    }

    /// Always proceed, never cite: today's behavior with no alignment check.
    pub fn always_proceed(cases: &[EvalCase]) -> Self {
        let decision = AlignmentDecision {
            schema_version: ALIGNMENT_DECISION_SCHEMA_VERSION.to_string(),
            assessment: Assessment::Aligned,
            intervention: Intervention::None,
            continuation: Continuation::Proceed,
            supporting_intentions: Vec::new(),
            holding_intentions: Vec::new(),
            cited_owner_spans: Vec::new(),
            explanation: "Proceeding.".to_string(),
            unresolved_facts: Vec::new(),
            resolution_choices: Vec::new(),
            remember_offer: None,
        };
        let raw = serde_json::to_string(&decision).unwrap_or_default();
        Self {
            responses: cases
                .iter()
                .map(|case| (case.id.clone(), raw.clone()))
                .collect(),
        }
    }
}

impl ScriptedJudge for CaseScript {
    fn respond(&self, input: &AlignmentInput) -> Result<String, String> {
        let id = input
            .execution_id
            .as_deref()
            .and_then(|id| id.strip_prefix(CASE_EXECUTION_PREFIX))
            .ok_or_else(|| "not an eval case input".to_string())?;
        self.responses
            .get(id)
            .cloned()
            .ok_or_else(|| format!("no scripted answer for case {id}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::preference_alignment::judge::{judge_with, JudgeBackend, JudgeOutcome};

    fn corpus() -> Vec<EvalCase> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../data/magician_v2/evals/preference_alignment/dev.jsonl");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        parse_cases(&text).expect("the dev corpus parses as preference_alignment_case.v1")
    }

    /// Provider-free judgment lane: the oracle scores perfectly through the
    /// real parse and validator, and the "always proceed" baseline misses
    /// every case that needs an intervention.
    #[tokio::test]
    async fn the_oracle_passes_and_always_proceed_misses_every_intervention() {
        let cases = corpus();
        let oracle = CaseScript::oracle(&cases);
        let baseline = CaseScript::always_proceed(&cases);
        let budget = std::time::Duration::from_secs(5);
        let mut baseline_misses_where_intervention_required = 0;
        let mut intervention_required = 0;
        for case in &cases {
            let input = case.to_alignment_input();
            match judge_with(Some(JudgeBackend::Scripted(&oracle)), &input, budget).await {
                JudgeOutcome::Decided { decision, .. } => {
                    assert!(
                        case.accepts(&decision.decision),
                        "{}: oracle rejected",
                        case.id
                    )
                },
                JudgeOutcome::NothingToCompare => assert!(
                    case.acceptable
                        .iter()
                        .any(|o| o.continuation.contains(&Continuation::Proceed)),
                    "{}: nothing to compare but proceeding is not acceptable",
                    case.id
                ),
                other => panic!("{}: oracle unresolved: {other:?}", case.id),
            }
            let needs_intervention = case
                .acceptable
                .iter()
                .all(|o| !o.intervention.contains(&Intervention::None));
            if needs_intervention {
                intervention_required += 1;
                // Nothing to compare proceeds too, so it misses as well.
                let missed =
                    match judge_with(Some(JudgeBackend::Scripted(&baseline)), &input, budget).await
                    {
                        JudgeOutcome::Decided { decision, .. } => !case.accepts(&decision.decision),
                        _ => true,
                    };
                if missed {
                    baseline_misses_where_intervention_required += 1;
                }
            }
        }
        assert_eq!(
            baseline_misses_where_intervention_required, intervention_required,
            "always-proceed should miss every case that requires an intervention"
        );
    }
}

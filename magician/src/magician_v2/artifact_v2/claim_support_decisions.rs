//! Chat-reply claim checks on the decision engine.
//!
//! The owner-text check (`synthesis::check_owner_text`) splits a reply into
//! claims and finds each claim's evidence lines in the turn's tool results;
//! figures are checked in code. This adapter asks the `claim_support` pack the
//! two closed questions the LLM claim judge otherwise answers — is the claim
//! supported, contradicted, absent or not a claim (`verdict`), and is it part
//! of the answer or a side remark (`core`) — one item per claim, with the
//! owner's request as shared context.
//!
//! There is no incumbent: the engine replaces the LLM judge on this path, so
//! it only runs gated. A claim the engine does not answer eligibly (low
//! confidence, no route, an outage, past the operation's item or byte limits)
//! is returned unjudged and the caller keeps the figures-only rule for it.

use std::{collections::BTreeMap, time::Duration};

use decision_engine_contract::{
    batch::DecisionItem,
    request::{Answer, DecisionState},
};
use serde_json::json;

use crate::magician_v2::{
    decision_host::classification::{self, PolicyLookup},
    decisions::{reference, runner, telemetry::Reference},
    prompts::constants::versions,
    query_analysis::operation_llm_router::OperationLlmRouter,
};

pub const DECISION_ID: &str = "claim_support";
pub const PROJECTION: &str = "claim_support_v1";
/// The LLM operation this pack stands in for (reference identity only).
const REFERENCE_OPERATION: &str = "evidence_precision_judge";
/// The caller's whole wait. A gated engine is also capped by the operation's
/// decision and queue budgets.
const OUTER: Duration = Duration::from_secs(6);
const REQUEST_CHARS: usize = 1_000;
const CLAIM_CHARS: usize = 1_200;
const CONTEXT_CHARS: usize = 400;
/// Room for the request envelope around the items.
const REQUEST_OVERHEAD_BYTES: usize = 4_096;
/// Evidence is never cut below this: a claim that does not fit with it is
/// left unjudged rather than judged against a stub.
const MIN_EVIDENCE_CHARS: usize = 300;

/// Whose reply this is, and the router that names the reference judge.
pub struct ClaimSupportEngine<'a> {
    pub router: &'a OperationLlmRouter,
    pub principal: &'a str,
    pub workspace: &'a str,
}

/// One claim and the evidence lines retrieved for it.
pub struct EngineClaim<'a> {
    pub id: &'a str,
    pub claim: &'a str,
    pub context: Option<&'a str>,
    pub evidence: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineVerdict {
    Supported,
    Contradicted,
    Absent,
    NotAClaim,
}

/// The engine's eligible answers for one claim. `core` is `None` when that
/// head was not answered eligibly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineJudgement {
    pub verdict: EngineVerdict,
    pub core: Option<bool>,
}

/// Judge `claims` on the engine. `Err` says why nothing was judged; claims
/// missing from an `Ok` map were not answered eligibly.
pub async fn judge(
    engine: &ClaimSupportEngine<'_>,
    request: &str,
    claims: &[EngineClaim<'_>],
) -> Result<BTreeMap<String, EngineJudgement>, String> {
    let principal = engine.principal.trim();
    let workspace = engine.workspace.trim();
    if principal.is_empty() || workspace.is_empty() {
        return Err("claim_support: no owner scope".into());
    }
    if claims.is_empty() {
        return Ok(BTreeMap::new());
    }
    let reference_version = reference::version(
        engine.router,
        REFERENCE_OPERATION,
        versions::TASK_USER_CLAIM_JUDGE,
    )
    .ok_or("claim_support: the reference judge is not bound")?;
    let lookup = classification::ready_policy(DECISION_ID, principal, workspace).await;
    let limits = match &lookup {
        PolicyLookup::Participating(p) if p.policy.gate => p.policy.classification.limits.clone(),
        PolicyLookup::Participating(_) => return Err("claim_support: not gated".into()),
        PolicyLookup::Disabled => return Err("claim_support: decision engine off".into()),
        PolicyLookup::Unavailable(failure) => {
            return Err(format!("claim_support: policy unavailable ({failure:?})"))
        },
    };
    let context = DecisionState::from_json(json!({ "request": clip(request, REQUEST_CHARS) }));
    let context = serde_json::to_vec(&context)
        .is_ok_and(|bytes| bytes.len() <= limits.max_context_bytes)
        .then_some(context);
    let mut items = Vec::new();
    let mut bytes = REQUEST_OVERHEAD_BYTES
        + context
            .as_ref()
            .and_then(|c| serde_json::to_vec(c).ok())
            .map_or(0, |c| c.len());
    for claim in claims {
        if items.len() >= limits.max_items {
            break;
        }
        let Some((item, size)) = project(claim, limits.max_item_bytes) else {
            continue;
        };
        if bytes + size > limits.max_request_bytes {
            break;
        }
        bytes += size;
        items.push(item);
    }
    if items.is_empty() {
        return Err("claim_support: no claim fits the operation's limits".into());
    }
    let case_id = blake3::hash(&serde_json::to_vec(&(&context, &items)).unwrap_or_default())
        .to_hex()
        .to_string();
    let scope = engine
        .router
        .classification_trace_context(magicllm::LlmScope::new(
            principal.to_owned(),
            workspace.to_owned(),
        ));
    let input = runner::Input {
        operation: DECISION_ID.into(),
        projection_version: PROJECTION.into(),
        reference_version,
        case_id,
        context,
        items,
        required_questions: vec!["verdict".into()],
        scope,
        agent: None,
        requires_completion: false,
        replay: None,
    };
    let outcome = runner::run(
        input,
        lookup,
        OUTER,
        Duration::ZERO,
        |_, _| async { None::<()> },
        |_: &()| Reference::from(BTreeMap::new()),
    )
    .await;
    Ok(outcome
        .current_answers()
        .into_iter()
        .filter_map(|(id, answers)| Some((id, judgement(&answers)?)))
        .collect())
}

fn clip(text: &str, chars: usize) -> String {
    text.trim().chars().take(chars).collect()
}

/// One claim as an item, with its evidence cut until the item fits the
/// operation's item limit. Returns the item and its size.
fn project(claim: &EngineClaim<'_>, max_item_bytes: usize) -> Option<(DecisionItem, usize)> {
    let mut evidence_chars = claim.evidence.chars().count();
    loop {
        let item = DecisionItem {
            item_id: claim.id.to_owned(),
            state: DecisionState::from_json(json!({
                "claim": clip(claim.claim, CLAIM_CHARS),
                "context": clip(claim.context.unwrap_or(""), CONTEXT_CHARS),
                "evidence": claim.evidence.chars().take(evidence_chars).collect::<String>(),
            })),
            choice_candidates: BTreeMap::new(),
        };
        let size = serde_json::to_vec(&item).ok()?.len();
        if size <= max_item_bytes {
            return Some((item, size));
        }
        if evidence_chars <= MIN_EVIDENCE_CHARS {
            return None;
        }
        evidence_chars = (evidence_chars / 2).max(MIN_EVIDENCE_CHARS);
    }
}

fn choice<'a>(answers: &'a BTreeMap<String, Answer>, question: &str) -> Option<&'a str> {
    match answers.get(question) {
        Some(Answer::Choice { choice, .. }) => Some(choice.as_str()),
        _ => None,
    }
}

fn judgement(answers: &BTreeMap<String, Answer>) -> Option<EngineJudgement> {
    let verdict = match choice(answers, "verdict")? {
        "supported" => EngineVerdict::Supported,
        "contradicted" => EngineVerdict::Contradicted,
        "absent" => EngineVerdict::Absent,
        "not_a_claim" => EngineVerdict::NotAClaim,
        _ => return None,
    };
    let core = match choice(answers, "core") {
        Some("core") => Some(true),
        Some("supporting") => Some(false),
        _ => None,
    };
    Some(EngineJudgement { verdict, core })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::decisions::test_engine::{self, FakeEngine};

    fn claims() -> Vec<EngineClaim<'static>> {
        vec![
            EngineClaim {
                id: "c1",
                claim: "GMV for September was ₹1,58,92,276.59.",
                context: None,
                evidence: "  │ rows[0][1] = 15892276.59",
            },
            EngineClaim {
                id: "c2",
                claim: "Orders grew 12% month on month.",
                context: Some("Summary"),
                evidence: "  │ rows[0][2] = 4821",
            },
        ]
    }

    async fn run() -> Result<BTreeMap<String, EngineJudgement>, String> {
        let router = test_engine::fixture_router(REFERENCE_OPERATION);
        let engine = ClaimSupportEngine {
            router: &router,
            principal: "p",
            workspace: "w",
        };
        judge(&engine, "What was GMV last month?", &claims()).await
    }

    #[tokio::test]
    async fn gated_engine_judges_every_claim_with_the_request_as_context() {
        let _serial = test_engine::serial().await;
        let engine = FakeEngine::start(
            DECISION_ID,
            true,
            &[
                ("verdict", "contradicted", 0.95),
                ("core", "supporting", 0.95),
            ],
        )
        .await;
        let judged = run().await.expect("engine verdicts");
        assert_eq!(judged.len(), 2);
        assert_eq!(
            judged["c2"],
            EngineJudgement {
                verdict: EngineVerdict::Contradicted,
                core: Some(false),
            }
        );
        let request = engine.requests().pop().expect("one decide request");
        assert_eq!(request.batch.projection_version, PROJECTION);
        let context = request.batch.context.expect("shared context");
        assert_eq!(context.0["request"], "What was GMV last month?");
        assert_eq!(request.batch.items[1].state.0["context"], "Summary");
    }

    #[tokio::test]
    async fn an_unconfident_verdict_leaves_the_claim_unjudged() {
        let _serial = test_engine::serial().await;
        let _engine = FakeEngine::start(
            DECISION_ID,
            true,
            &[("verdict", "absent", 0.3), ("core", "core", 0.95)],
        )
        .await;
        assert!(run().await.expect("engine ran").is_empty());
    }

    #[tokio::test]
    async fn an_unconfident_core_keeps_the_verdict_without_it() {
        let _serial = test_engine::serial().await;
        let _engine = FakeEngine::start(
            DECISION_ID,
            true,
            &[("verdict", "supported", 0.95), ("core", "core", 0.3)],
        )
        .await;
        let judged = run().await.expect("engine verdicts");
        assert_eq!(judged["c1"].core, None);
        assert_eq!(judged["c1"].verdict, EngineVerdict::Supported);
    }

    #[tokio::test]
    async fn a_shadow_only_operation_judges_nothing() {
        let _serial = test_engine::serial().await;
        let engine = FakeEngine::start(DECISION_ID, false, &[("verdict", "supported", 0.95)]).await;
        assert!(run().await.is_err());
        assert!(engine.requests().is_empty());
    }

    #[test]
    fn evidence_is_cut_to_fit_the_item_limit() {
        let evidence = "│ x = 1\n".repeat(2_000);
        let claim = EngineClaim {
            id: "c1",
            claim: "x is 1",
            context: None,
            evidence: &evidence,
        };
        let (_, size) = project(&claim, 4_096).expect("fits after cutting");
        assert!(size <= 4_096);
        assert!(project(&claim, 200).is_none());
    }
}

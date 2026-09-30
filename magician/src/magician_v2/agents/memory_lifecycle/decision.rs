//! Pair decisions retain lifecycle's original source hashes and storage owner.
use super::{Review, Source, OPERATION};
use crate::magician_v2::{
    agents::AgentMemoryService,
    decision_host::classification::{self},
    decisions::{observation, reference, runner, telemetry::Reference, text},
    query_analysis::operation_llm_router::{OperationLlmRouter, SimplifiedLLMResponse},
};
use decision_engine_contract::{batch::DecisionItem, request::DecisionState};
use serde_json::json;
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
const ENGINE: &str = "memory_lifecycle_relation";
const PROJECTION: &str = "memory_lifecycle_pair_v1";
const PROMPT: &str = "memory_lifecycle_review_v1";
const QUESTIONS: &[&str] = &[
    "kind",
    "relation",
    "incoming_coverage",
    "same_subject_and_aspect",
    "same_context",
    "explicit_correction",
];

pub(super) async fn review<F, Fut>(
    service: &AgentMemoryService,
    router: Option<&OperationLlmRouter>,
    incoming: &Source,
    offered: &[Source],
    prompt: String,
    generate: F,
) -> anyhow::Result<text::Reviewed<Review>>
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = anyhow::Result<SimplifiedLLMResponse>>,
{
    let parse = |raw: &str| {
        super::review_wire::parse(raw)
            .and_then(|r| super::runtime::bind_review_references(r, offered))
            .map_err(|error| anyhow::anyhow!("invalid memory review: {error}"))
    };
    let scope = router.and_then(OperationLlmRouter::authoritative_trace_scope);
    let lookup = match scope.as_ref() {
        Some(s) => classification::ready_policy(ENGINE, &s.principal, &s.workspace).await,
        None => classification::unscoped_policy(),
    };
    let revision = router.and_then(|r| reference::version(r, OPERATION, PROMPT));
    let Some((router, scope, revision)) = router
        .zip(scope)
        .zip(revision)
        .map(|((r, s), v)| (r, s, v))
        .filter(|_| !offered.is_empty() && prompt.len() <= 32768)
    else {
        anyhow::ensure!(
            lookup.allows_incumbent(),
            "lifecycle decision deferred: missing scope/reference or input bounds"
        );
        let response = generate(prompt).await?;
        return Ok(text::Reviewed {
            value: parse(&response.content)?,
            response,
            guard: None,
            origins: BTreeMap::new(),
        });
    };
    let projection = |source: &Source| {
        json!({"claim": super::text(&source.item),
        "source_type": source.item.get("source_type"), "scope": super::applicability(&source.item),
        "lifecycle": super::state(&source.item), "independent_observations": super::independent_observations(&source.item),
        "observation_span_days": super::observation_span_days(&source.item), "evidence": source.item.get("memory_evidence").and_then(serde_json::Value::as_array).map(|rows|
                rows.iter().rev().take(6).map(|e| json!({"at":e.get("at"),"source_type":e.get("source_type"),
                    "quote":e.get("quote").and_then(serde_json::Value::as_str).map(|q|q.chars().take(256).collect::<String>())})).collect::<Vec<_>>())})
    };
    let items = offered
        .iter()
        .enumerate()
        .map(|(i, s)| DecisionItem {
            item_id: format!("m{}", i + 1),
            state: DecisionState::from_json(projection(s)),
            choice_candidates: Default::default(),
        })
        .collect();
    let mut input = runner::Input {
        operation: ENGINE.into(),
        projection_version: PROJECTION.into(),
        reference_version: revision.clone(),
        case_id: super::digest(&json!([
            incoming.revision,
            offered.iter().map(|s| &s.revision).collect::<Vec<_>>()
        ])),
        context: Some(DecisionState::from_json(projection(incoming))),
        items,
        required_questions: QUESTIONS.iter().map(|s| (*s).into()).collect(),
        scope: router.classification_trace_context(scope),
        agent: None,
        requires_completion: true,
        replay: None,
    };
    if let classification::PolicyLookup::Participating(participation) = &lookup {
        if observation::selected(&input, participation) {
            type Snapshot = (Source, Vec<Source>, String);
            if let Some((snapshot, bytes)) =
                observation::snapshot_bounded::<_, Snapshot>(&(incoming, offered, &prompt))
            {
                let snapshot = Arc::new(snapshot);
                let check_snapshot = snapshot.clone();
                let check_service = service.clone();
                let check_router = router.clone();
                let check_revision = revision.clone();
                let check_scope = input.scope.scope.clone();
                let run_snapshot = snapshot.clone();
                let run_router = router.clone();
                let run_scope = input.scope.scope.clone();
                input.replay = Some(Ok(runner::ReferenceReplay {
                    bytes,
                    cost_reservation_microusd: reference::observation_cost_upper_microusd(
                        router, OPERATION, bytes,
                    ),
                    current: Box::new(move || {
                        let snapshot = check_snapshot.clone();
                        let service = check_service.clone();
                        let router = check_router.clone();
                        let revision = check_revision.clone();
                        let scope = check_scope.clone();
                        Box::pin(async move {
                            if service.scoped_memory_scope()
                                != Some((scope.principal.as_str(), scope.workspace.as_str()))
                                || !router.observation_dispatch_available()
                                || router.authoritative_trace_scope().as_ref() != Some(&scope)
                                || reference::version(&router, OPERATION, PROMPT).as_ref()
                                    != Some(&revision)
                            {
                                return false;
                            }
                            let Ok(document) = service.load_user_knowledge().await else {
                                return false;
                            };
                            super::current(&document, &snapshot.0)
                                && snapshot
                                    .1
                                    .iter()
                                    .all(|source| super::current(&document, source))
                        })
                    }),
                    access_current: None,
                    run: Box::new(move || {
                        Box::pin(async move {
                            let started = Instant::now();
                            let response = reference::pinned_json_observation(
                                &run_router,
                                OPERATION,
                                &run_scope.principal,
                                &run_scope.workspace,
                                super::runtime::SYSTEM_PROMPT,
                                &run_snapshot.2,
                            )
                            .await;
                            let Ok(response) = response else {
                                return runner::ReplayResult {
                                    status: "failed",
                                    attempted: None,
                                    reference: None,
                                };
                            };
                            let parsed =
                                super::review_wire::parse(&response.content).and_then(|review| {
                                    super::runtime::bind_review_references(review, &run_snapshot.1)
                                });
                            let labels = parsed
                                .as_ref()
                                .map(|review| reference_labels(review, &run_snapshot.1))
                                .unwrap_or_default();
                            runner::ReplayResult {
                                status: if parsed.is_ok() {
                                    "completed"
                                } else {
                                    "failed"
                                },
                                attempted: Some(true),
                                reference: Some(Reference::from_response(
                                    labels,
                                    &response,
                                    started.elapsed().as_millis() as u64,
                                )),
                            }
                        })
                    }),
                }));
            } else {
                input.replay = Some(Err("snapshot_oversize"));
            }
        }
    }
    let generate = Mutex::new(Some(generate));
    text::review(
        input,
        lookup,
        router,
        OPERATION,
        PROMPT,
        Duration::from_secs(40),
        |heads| {
            let make = generate
                .lock()
                .unwrap()
                .take()
                .expect("text generator invoked once");
            make(text::locked_prompt(&prompt, &heads))
        },
        parse,
        |r| reference_labels(r, offered),
        |r, heads| apply(r, heads, offered),
    )
    .await
}

fn reference_labels(
    review: &Review,
    offered: &[Source],
) -> crate::magician_v2::decisions::telemetry::Labels {
    offered
        .iter()
        .enumerate()
        .filter_map(|(i, source)| {
            let pair = review
                .relationships
                .iter()
                .find(|p| p.existing_id == source.id)?;
            Some((
                format!("m{}", i + 1),
                BTreeMap::from([
                    ("kind".into(), json!(review.kind)),
                    ("relation".into(), json!(pair.relation)),
                    ("incoming_coverage".into(), json!(pair.incoming_coverage)),
                    (
                        "same_subject_and_aspect".into(),
                        json!(pair.same_subject_and_aspect),
                    ),
                    ("same_context".into(), json!(pair.same_context)),
                    (
                        "explicit_correction".into(),
                        json!(pair.explicit_correction),
                    ),
                ]),
            ))
        })
        .collect()
}

fn apply(review: &mut Review, heads: &text::Answers, offered: &[Source]) -> anyhow::Result<()> {
    let mut accepted_kind = None;
    for (id, answers) in heads {
        let index: usize = id
            .strip_prefix('m')
            .ok_or_else(|| anyhow::anyhow!("foreign pair"))?
            .parse()?;
        let source = index
            .checked_sub(1)
            .and_then(|i| offered.get(i))
            .ok_or_else(|| anyhow::anyhow!("foreign pair"))?;
        let (kind, _) =
            text::choice(answers, "kind").ok_or_else(|| anyhow::anyhow!("missing kind"))?;
        anyhow::ensure!(
            accepted_kind.as_ref().is_none_or(|k| k == &kind),
            "inconsistent incoming kinds"
        );
        accepted_kind = Some(kind.clone());
        review.kind = serde_json::from_value(json!(kind))?;
        let chosen_relation = text::choice(answers, "relation")
            .ok_or_else(|| anyhow::anyhow!("missing relation"))?
            .0;
        // An unrelated/compatible pair has no relationship mutation and needs
        // no invented prose. Incoming subject/aspect/context still come from
        // the required full review and pass the lifecycle owner's validation.
        if chosen_relation == "coexist"
            && !review
                .relationships
                .iter()
                .any(|p| p.existing_id == source.id)
        {
            continue;
        }
        let pair = review
            .relationships
            .iter_mut()
            .find(|p| p.existing_id == source.id)
            .ok_or_else(|| anyhow::anyhow!("required lifecycle relationship text missing"))?;
        anyhow::ensure!(
            json!(pair.relation) == json!(chosen_relation),
            "generated relationship reversed qualified relation"
        );
        pair.relation = serde_json::from_value(json!(chosen_relation))?;
        pair.incoming_coverage = serde_json::from_value(json!(
            text::choice(answers, "incoming_coverage")
                .ok_or_else(|| anyhow::anyhow!("missing coverage"))?
                .0
        ))?;
        pair.same_subject_and_aspect = text::boolean(answers, "same_subject_and_aspect")
            .ok_or_else(|| anyhow::anyhow!("missing subject agreement"))?
            .0;
        pair.same_context = text::boolean(answers, "same_context")
            .ok_or_else(|| anyhow::anyhow!("missing context agreement"))?
            .0;
        pair.explicit_correction = text::boolean(answers, "explicit_correction")
            .ok_or_else(|| anyhow::anyhow!("missing correction"))?
            .0;
        pair.confidence = QUESTIONS
            .iter()
            .filter_map(|q| {
                text::choice(answers, q)
                    .map(|v| v.1)
                    .or_else(|| text::boolean(answers, q).map(|v| v.1))
            })
            .fold(1.0, f64::min);
        anyhow::ensure!(
            !pair.rationale.trim().is_empty(),
            "missing lifecycle rationale"
        );
        if pair.relation == super::Relation::Clarify {
            anyhow::ensure!(
                pair.question.as_ref().is_some_and(|q| !q.trim().is_empty()),
                "missing clarification question"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use decision_engine_contract::request::Answer;
    fn source() -> Source {
        Source {
            id: "source".into(),
            pointer: "/preferences/0".into(),
            revision: "revision".into(),
            item: json!({"value":"original"}),
        }
    }
    fn review() -> Review {
        serde_json::from_value(json!({"kind":"fact","subject":"owner","aspect":"city","context":"","valid_until":null,"validity_quote":null,
        "relationships":[{"existing_id":"source","relation":"coexist","incoming_coverage":"unknown","same_subject_and_aspect":true,"same_context":true,"explicit_correction":false,"confidence":0.9,"rationale":"Different contexts","question":null}]})).unwrap()
    }
    fn answers(relation: &str) -> text::Answers {
        let choice = |s: &str| Answer::Choice {
            choice: decision_engine_contract::primitives::OptionId::new(s),
            probabilities: Default::default(),
            confidence: 0.99,
        };
        BTreeMap::from([(
            "m1".into(),
            BTreeMap::from([
                ("kind".into(), choice("fact")),
                ("relation".into(), choice(relation)),
                ("incoming_coverage".into(), choice("unknown")),
                (
                    "same_subject_and_aspect".into(),
                    Answer::Noul { noul: 0.99 },
                ),
                ("same_context".into(), Answer::Noul { noul: 0.99 }),
                ("explicit_correction".into(), Answer::Noul { noul: 0.01 }),
            ]),
        )])
    }
    #[test]
    fn memory_decision_lifecycle_rejects_foreign_pairs_and_inconsistent_required_text() {
        assert!(apply(&mut review(), &answers("coexist"), &[source()]).is_ok());
        assert!(apply(&mut review(), &answers("supersede"), &[source()]).is_err());
        let mut a = answers("coexist");
        let fields = a.remove("m1").unwrap();
        a.insert("m2".into(), fields);
        assert!(apply(&mut review(), &a, &[source()]).is_err());
        let mut r = review();
        r.relationships.clear();
        assert!(apply(&mut r, &answers("coexist"), &[source()]).is_ok());
        assert!(apply(&mut r, &answers("supersede"), &[source()]).is_err());
    }
}

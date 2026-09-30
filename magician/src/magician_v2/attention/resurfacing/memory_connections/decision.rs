use super::*;
use crate::magician_v2::{
    decision_host::{
        self,
        classification::{self, PolicyLookup},
    },
    decisions::{observation, reference, runner, telemetry::Reference, text},
    query_analysis::operation_llm_router::{
        LLMOperation, OperationLlmRouter, SimplifiedLLMResponse,
    },
};
use decision_engine_contract::{
    batch::DecisionItem, classification::ClassificationOrigin, request::DecisionState,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
const ENGINE: &str = "memory_connection_gate";
const PROJECTION: &str = "memory_connection_sources_v1";
const PROMPT: &str = "memory_connection_review_v1";
const BUDGET: Duration = Duration::from_secs(20);
/// The source owner re-reads the candidate and all current memory/profile
/// sources. The replay cannot rely on a captured prompt after revocation.
pub type ConnectionSourceCheck =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;
#[derive(Clone)]
pub struct ConnectionDecisionPolicy {
    pub key: String,
    lookup: PolicyLookup,
    reference: Option<String>,
}
impl ConnectionDecisionPolicy {
    pub async fn capture(router: &OperationLlmRouter) -> Self {
        let reference = reference::version(router, OPERATION, PROMPT);
        let lookup = match router.authoritative_trace_scope() {
            Some(s) => classification::ready_policy(ENGINE, &s.principal, &s.workspace).await,
            None => classification::unscoped_policy(),
        };
        let origin = match &lookup {
            PolicyLookup::Participating(p) if p.policy.gate => {
                format!("{}:{}", p.engine_instance, p.revision)
            },
            _ => "incumbent".into(),
        };
        let key = blake3::hash(
            format!(
                "{}:{reference:?}:{origin}:{PROJECTION}",
                decision_host::generation()
            )
            .as_bytes(),
        )
        .to_hex()
        .to_string();
        Self {
            key,
            lookup,
            reference,
        }
    }
    pub async fn capture_for_reconciliation(router: &OperationLlmRouter) -> Self {
        if let Some(scope) = router.authoritative_trace_scope() {
            let _ = classification::policy_for_reconciliation(
                ENGINE,
                &scope.principal,
                &scope.workspace,
            )
            .await;
        }
        Self::capture(router).await
    }
    pub fn unavailable(&self) -> bool {
        matches!(self.lookup, PolicyLookup::Unavailable(_))
    }
    pub fn fingerprint(&self, sources: &[ConnectionSource]) -> String {
        blake3::hash(format!("{}:{}", self.key, super::fingerprint(sources)).as_bytes())
            .to_hex()
            .to_string()
    }
}
pub struct ConnectionDecisionReview {
    pub response: SimplifiedLLMResponse,
    pub connection: Option<Connection>,
    pub origin: Option<ClassificationOrigin>,
    guard: Option<Arc<reference::ApplyGuard>>,
}
impl ConnectionDecisionReview {
    pub fn current(&self) -> bool {
        self.guard.as_ref().is_none_or(|g| g.current())
    }
}

pub async fn review_connection(
    router: &OperationLlmRouter,
    sources: &[ConnectionSource],
    policy: &ConnectionDecisionPolicy,
) -> Result<ConnectionDecisionReview> {
    review_connection_with_source_check(router, sources, policy, None).await
}

pub async fn review_connection_with_source_check(
    router: &OperationLlmRouter,
    sources: &[ConnectionSource],
    policy: &ConnectionDecisionPolicy,
    source_check_factory: Option<&(dyn Fn() -> ConnectionSourceCheck + Send + Sync)>,
) -> Result<ConnectionDecisionReview> {
    let started = Instant::now();
    let prompt = serde_json::to_string(&json!({"sources":sources}))?;
    let invoke = |heads: text::Answers| {
        let prompt = text::locked_prompt(&prompt, &heads);
        async move {
            let at = Instant::now();
            let response = router
                .generate_for_operation_with_system(
                    &LLMOperation::Other(OPERATION.into()),
                    Some(SYSTEM),
                    &prompt,
                )
                .await?;
            if let Some(scope) = router.authoritative_trace_scope() {
                reference::record_response(
                    &response,
                    &scope.principal,
                    &scope.workspace,
                    OPERATION,
                    at.elapsed(),
                );
            }
            let connection = parse_connection(&response.content, sources)?;
            Ok::<_, anyhow::Error>((connection, response, at.elapsed().as_millis() as u64))
        }
    };
    let Some((scope, revision)) = router
        .authoritative_trace_scope()
        .zip(policy.reference.clone())
        .filter(|_| prompt.len() <= 16384)
    else {
        anyhow::ensure!(
            policy.lookup.allows_incumbent(),
            "connection decision deferred: missing scope/reference or input bounds"
        );
        let (connection, response, _) = invoke(BTreeMap::new()).await?;
        return Ok(ConnectionDecisionReview {
            response,
            connection,
            origin: None,
            guard: None,
        });
    };
    let mut input = runner::Input {
        operation: ENGINE.into(),
        projection_version: PROJECTION.into(),
        reference_version: revision.clone(),
        case_id: super::fingerprint(sources),
        context: None,
        items: vec![DecisionItem {
            item_id: "0".into(),
            state: DecisionState::from_json(
                json!({"sources":sources.iter().map(|s| json!({"id":s.id,"text":s.text})).collect::<Vec<_>>()}),
            ),
            choice_candidates: Default::default(),
        }],
        required_questions: vec!["worth_surfacing".into(), "surface".into()],
        scope: router.classification_trace_context(scope.clone()),
        agent: None,
        requires_completion: true,
        replay: None,
    };
    if let (PolicyLookup::Participating(participation), Some(source_check_factory)) =
        (&policy.lookup, source_check_factory)
    {
        if observation::selected(&input, participation) {
            if let Some((snapshot, bytes)) =
                observation::snapshot_bounded::<_, Vec<ConnectionSource>>(&sources)
            {
                let source_check = source_check_factory();
                let snapshot = Arc::new(snapshot);
                let check_router = router.clone();
                let check_scope = scope.clone();
                let check_revision = revision.clone();
                let run_router =
                    router.with_dispatch_priority(magicllm::dispatch::Priority::Background);
                let run_scope = scope.clone();
                input.replay = Some(Ok(runner::ReferenceReplay {
                    bytes,
                    cost_reservation_microusd: reference::observation_cost_upper_microusd(
                        router, OPERATION, bytes,
                    ),
                    current: Box::new(move || {
                        let router = check_router.clone();
                        let scope = check_scope.clone();
                        let revision = check_revision.clone();
                        let source_check = source_check.clone();
                        Box::pin(async move {
                            router.observation_dispatch_available()
                                && router.authoritative_trace_scope().as_ref() == Some(&scope)
                                && reference::version(&router, OPERATION, PROMPT).as_ref()
                                    == Some(&revision)
                                && source_check().await
                        })
                    }),
                    access_current: None,
                    run: Box::new(move || {
                        Box::pin(async move {
                            let started = Instant::now();
                            let user =
                                serde_json::to_string(&json!({"sources": snapshot.as_ref()}))
                                    .unwrap_or_default();
                            let response = run_router
                                .generate_for_operation_with_system(
                                    &LLMOperation::Other(OPERATION.into()),
                                    Some(SYSTEM),
                                    &user,
                                )
                                .await;
                            let Ok(response) = response else {
                                return runner::ReplayResult {
                                    status: "failed",
                                    attempted: None,
                                    reference: None,
                                };
                            };
                            reference::record_response(
                                &response,
                                &run_scope.principal,
                                &run_scope.workspace,
                                OPERATION,
                                started.elapsed(),
                            );
                            let parsed = parse_connection(&response.content, &snapshot);
                            let labels = parsed
                                .as_ref()
                                .map(|connection| {
                                    BTreeMap::from([(
                                        "0".into(),
                                        BTreeMap::from([
                                            ("worth_surfacing".into(), json!(connection.is_some())),
                                            (
                                                "surface".into(),
                                                connection
                                                    .as_ref()
                                                    .map(|value| json!(value.surface))
                                                    .unwrap_or(json!("none")),
                                            ),
                                        ]),
                                    )])
                                })
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
    let outcome = runner::run(
        input,
        policy.lookup.clone(),
        BUDGET,
        runner::text_reserve(BUDGET),
        |_, _| async { Some(invoke(BTreeMap::new()).await) },
        |r| match r {
            Ok((connection, response, ms)) => Reference::from_response(
                BTreeMap::from([(
                    "0".into(),
                    BTreeMap::from([
                        ("worth_surfacing".into(), json!(connection.is_some())),
                        (
                            "surface".into(),
                            connection
                                .as_ref()
                                .map(|c| json!(c.surface))
                                .unwrap_or(json!("none")),
                        ),
                    ]),
                )]),
                response,
                *ms,
            ),
            Err(_) => BTreeMap::new().into(),
        },
    )
    .await;
    let heads = outcome.current_answers();
    let accepted = heads
        .get("0")
        .and_then(|a| text::boolean(a, "worth_surfacing").zip(text::choice(a, "surface")));
    let Some(((worth, confidence), (surface, sc))) = accepted else {
        let (connection, response, _) = outcome.incumbent.ok_or_else(|| {
            anyhow::anyhow!(
                "connection decision deferred: no complete accepted decision or incumbent result"
            )
        })??;
        if let Some(o) = outcome.observation {
            o.complete(None, false, true, started.elapsed());
        }
        return Ok(ConnectionDecisionReview {
            response,
            connection,
            origin: None,
            guard: None,
        });
    };
    anyhow::ensure!(
        worth == (surface != "none"),
        "contradictory connection heads"
    );
    let generated_text = worth && outcome.incumbent.is_none();
    let mut result = match outcome.incumbent {
        Some(r) => Some(r?),
        None if worth => Some(
            tokio::time::timeout(BUDGET.saturating_sub(started.elapsed()), invoke(heads)).await??,
        ),
        None => None,
    };
    let text_reference = generated_text.then(|| {
        let (_, r, ms) = result.as_ref().unwrap();
        Reference::from_response(BTreeMap::new(), r, *ms)
    });
    if let Some(authority) = &outcome.authority {
        authority.revalidate().await;
    }
    let guard = outcome.authority.as_ref().map(|p| {
        Arc::new(reference::ApplyGuard::new(
            p.clone(),
            router,
            OPERATION,
            PROMPT,
            &revision,
        ))
    });
    anyhow::ensure!(
        guard.as_ref().is_some_and(|g| g.current()),
        "connection decision policy changed"
    );
    let connection = if worth {
        let c = result
            .as_mut()
            .and_then(|r| r.0.as_mut())
            .ok_or_else(|| anyhow::anyhow!("accepted connection requires grounded text"))?;
        // Surface changes require matching generated text, especially a HITL question.
        anyhow::ensure!(
            json!(c.surface) == json!(surface),
            "generated connection reversed qualified surface"
        );
        c.confidence = confidence.min(sc) as f32;
        Some(
            parse_connection(&json!({"connection":c}).to_string(), sources)?
                .ok_or_else(|| anyhow::anyhow!("missing connection"))?,
        )
    } else {
        None
    };
    if let Some(o) = outcome.observation {
        o.complete(text_reference, generated_text, true, started.elapsed());
    }
    let response = result
        .map(|r| r.1)
        .unwrap_or_else(|| SimplifiedLLMResponse {
            content: json!({"connection":null}).to_string(),
            ..Default::default()
        });
    Ok(ConnectionDecisionReview {
        response,
        connection,
        origin: outcome.origins.get("0").cloned(),
        guard,
    })
}

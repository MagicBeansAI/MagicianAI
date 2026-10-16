//! Required-text decisions. Reuse one incumbent result, freeze qualified heads,
//! and retain a guard for the owner's final mutation boundary.
use super::{reference, runner, telemetry::Reference};
use crate::magician_v2::{
    decision_host::classification::PolicyLookup,
    query_analysis::operation_llm_router::{OperationLlmRouter, SimplifiedLLMResponse},
};
use decision_engine_contract::{classification::ClassificationOrigin, request::Answer};
use std::{
    collections::BTreeMap,
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};
pub(crate) type Answers = BTreeMap<String, BTreeMap<String, Answer>>;
pub(crate) struct Reviewed<T> {
    pub value: T,
    pub response: SimplifiedLLMResponse,
    pub guard: Option<Arc<reference::ApplyGuard>>,
    pub origins: BTreeMap<String, ClassificationOrigin>,
}
impl<T> Reviewed<T> {
    pub fn current(&self) -> bool {
        self.guard.as_ref().is_none_or(|g| g.current())
    }
}
struct Generated<T> {
    value: T,
    response: SimplifiedLLMResponse,
    elapsed_ms: u64,
}

/// The generator receives locked decision heads only when all items qualified.
/// Partial fallback already produces the whole mixed output and is reused once.
pub(crate) async fn review<T, G, F, P, L, A>(
    input: runner::Input,
    lookup: PolicyLookup,
    router: &OperationLlmRouter,
    mapping: &str,
    prompt_version: &str,
    budget: Duration,
    generate: G,
    parse: P,
    labels: L,
    apply: A,
) -> anyhow::Result<Reviewed<T>>
where
    G: Fn(Answers) -> F,
    F: Future<Output = anyhow::Result<SimplifiedLLMResponse>>,
    P: Fn(&str) -> anyhow::Result<T>,
    L: Fn(&T) -> BTreeMap<String, BTreeMap<String, serde_json::Value>>,
    A: Fn(&mut T, &Answers) -> anyhow::Result<()>,
{
    let started = Instant::now();
    let revision = input.reference_version.clone();
    let expected_items = input.items.len();
    let invoke = |heads| async {
        let at = Instant::now();
        let response = generate(heads).await?;
        let value = parse(&response.content)?;
        Ok::<_, anyhow::Error>(Generated {
            value,
            response,
            elapsed_ms: at.elapsed().as_millis() as u64,
        })
    };
    let outcome = runner::run(
        input,
        lookup,
        budget,
        runner::text_reserve(budget),
        |_, _| async { Some(invoke(Answers::new()).await) },
        |result| match result {
            Ok(g) => Reference::from_response(labels(&g.value), &g.response, g.elapsed_ms),
            Err(_) => BTreeMap::new().into(),
        },
    )
    .await;
    let heads = outcome.current_answers();
    if outcome.incumbent.is_none() && heads.len() != expected_items {
        if let Some(observation) = &outcome.observation {
            observation.complete(None, false, false, started.elapsed());
        }
        anyhow::bail!(
            "memory decision deferred: incomplete engine answers; no LLM classification fallback"
        );
    }
    let generated_text = outcome.incumbent.is_none() && !heads.is_empty();
    let mut generated = match outcome.incumbent {
        Some(result) => result?,
        None if generated_text => {
            tokio::time::timeout(
                budget.saturating_sub(started.elapsed()),
                invoke(heads.clone()),
            )
            .await??
        },
        None => anyhow::bail!("memory decision exhausted its foreground deadline"),
    };
    let text_receipt = generated_text.then(|| {
        Reference::from_response(BTreeMap::new(), &generated.response, generated.elapsed_ms)
    });
    if let Some(authority) = &outcome.authority {
        authority.revalidate().await;
    }
    let guard = outcome.authority.as_ref().map(|p| {
        Arc::new(reference::ApplyGuard::new(
            p.clone(),
            router,
            mapping,
            prompt_version,
            &revision,
        ))
    });
    let mut origins = outcome.origins;
    let guard = if !heads.is_empty() && guard.as_ref().is_some_and(|g| g.current()) {
        if let Err(error) = apply(&mut generated.value, &heads) {
            if let Some(o) = outcome.observation {
                o.complete(text_receipt, generated_text, false, started.elapsed());
            }
            if !generated_text {
                // A partial gate may reuse a complete incumbent output. If its
                // prose cannot support the selected heads, keep the original
                // validated output rather than discarding unrelated work.
                return Ok(Reviewed {
                    value: parse(&generated.response.content)?,
                    response: generated.response,
                    guard: None,
                    origins: BTreeMap::new(),
                });
            }
            return Err(error);
        }
        guard
    } else {
        if generated_text {
            anyhow::bail!("memory decision policy changed during text generation");
        }
        origins.clear();
        None
    };
    if let Some(o) = outcome.observation {
        o.complete(text_receipt, generated_text, true, started.elapsed());
    }
    Ok(Reviewed {
        value: generated.value,
        response: generated.response,
        guard,
        origins,
    })
}

pub(crate) fn locked_prompt(prompt: &str, heads: &Answers) -> String {
    if heads.is_empty() {
        return prompt.to_owned();
    }
    format!("{prompt}\n\nHost-qualified decision heads (immutable): {}\nGenerate the required schema and grounded supporting text for these heads. Do not reverse these decisions. Return the original JSON schema only.", serde_json::to_string(heads).unwrap_or_default())
}
pub(crate) fn choice(answers: &BTreeMap<String, Answer>, key: &str) -> Option<(String, f64)> {
    match answers.get(key)? {
        Answer::Choice {
            choice, confidence, ..
        } => Some((choice.to_string(), *confidence)),
        _ => None,
    }
}
pub(crate) fn boolean(answers: &BTreeMap<String, Answer>, key: &str) -> Option<(bool, f64)> {
    match answers.get(key)? {
        Answer::Noul { noul } => Some((*noul >= 0.5, noul.max(1.0 - noul))),
        _ => None,
    }
}

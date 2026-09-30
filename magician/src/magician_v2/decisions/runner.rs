use super::telemetry::{self, Reference};
use crate::magician_v2::decision_host::{
    self,
    classification::{Participation, PolicyLookup},
};
use decision_engine_contract::{
    batch::{BatchRequest, ClassificationMode, DecisionItem},
    request::{Answer, DecisionState},
    DecideRequest, CONTRACT_VERSION,
};
use magicllm::LlmTraceContext;
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};

pub(crate) struct Input {
    pub operation: String,
    pub projection_version: String,
    pub reference_version: String,
    pub case_id: String,
    pub context: Option<DecisionState>,
    pub items: Vec<DecisionItem>,
    pub required_questions: Vec<String>,
    pub scope: LlmTraceContext,
    pub agent: Option<String>,
    pub requires_completion: bool,
    /// Independent observation work. It may never feed the application result.
    pub replay: Option<Result<ReferenceReplay, &'static str>>,
}

pub(crate) struct ReplayResult {
    pub status: &'static str,
    /// None means dispatch could not be verified (for example, timeout or a
    /// provider error with no receipt). Do not count it as an actual attempt.
    pub attempted: Option<bool>,
    pub reference: Option<Reference>,
}
pub(crate) type ReplayFuture = Pin<Box<dyn Future<Output = ReplayResult> + Send>>;
pub(crate) type ReplayCheckFuture = Pin<Box<dyn Future<Output = bool> + Send>>;
pub(crate) struct ReferenceReplay {
    pub bytes: usize,
    /// Admission reports dispatch and pricing failures separately.
    pub cost_reservation_microusd: Result<u64, &'static str>,
    /// Rechecks source/version/scope/locality before dispatch and export.
    pub current: Box<dyn Fn() -> ReplayCheckFuture + Send + Sync>,
    /// After inference, owners may check current access separately from the
    /// original content revision. Their own production write can legitimately
    /// change that revision while the reference call is still running.
    pub access_current: Option<Box<dyn Fn() -> ReplayCheckFuture + Send + Sync>>,
    pub run: Box<dyn FnOnce() -> ReplayFuture + Send>,
}
pub(crate) struct Outcome<T> {
    /// Complete incumbent output (including any generated text), if called.
    pub incumbent: Option<T>,
    /// Only engine-authorized items; uncertain gated items stay unresolved.
    pub answers: BTreeMap<String, BTreeMap<String, Answer>>,
    pub origins: BTreeMap<String, decision_engine_contract::classification::ClassificationOrigin>,
    pub authority: Option<Participation>,
    pub observation: Option<telemetry::Observation>,
}
impl<T> Outcome<T> {
    pub fn current_answers(&self) -> BTreeMap<String, BTreeMap<String, Answer>> {
        if self
            .authority
            .as_ref()
            .is_some_and(Participation::is_current)
        {
            self.answers.clone()
        } else {
            BTreeMap::new()
        }
    }
}

#[derive(Default)]
struct Jobs {
    active: BTreeSet<(String, String, String, String)>,
    scopes: BTreeMap<(String, String), usize>,
}
static JOBS: LazyLock<Mutex<Jobs>> = LazyLock::new(|| Mutex::new(Jobs::default()));
struct Job((String, String, String, String));
impl Job {
    fn admit(input: &Input) -> Option<Self> {
        let scope = (
            input.scope.scope.principal.clone(),
            input.scope.scope.workspace.clone(),
        );
        let key = (
            scope.0.clone(),
            scope.1.clone(),
            input.operation.clone(),
            input.case_id.clone(),
        );
        let mut jobs = JOBS.lock().unwrap_or_else(|p| p.into_inner());
        if jobs.active.len() >= 16
            || jobs.scopes.get(&scope).copied().unwrap_or(0) >= 4
            || jobs.active.contains(&key)
        {
            return None;
        }
        jobs.active.insert(key.clone());
        *jobs.scopes.entry(scope).or_default() += 1;
        Some(Self(key))
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        let mut jobs = JOBS.lock().unwrap_or_else(|p| p.into_inner());
        jobs.active.remove(&self.0);
        let scope = (self.0 .0.clone(), self.0 .1.clone());
        if let Some(count) = jobs.scopes.get_mut(&scope) {
            *count -= 1;
            if *count == 0 {
                jobs.scopes.remove(&scope);
            }
        }
    }
}
fn request(input: &Input, mode: ClassificationMode) -> DecideRequest {
    DecideRequest {
        contract_version: CONTRACT_VERSION,
        operation: input.operation.clone(),
        state: DecisionState::from_text(""),
        choice_candidates: BTreeMap::new(),
        locality: decision_host::global_decision_locality(),
        batch: BatchRequest {
            mode,
            reference_version: input.reference_version.clone(),
            request_id: ulid::Ulid::new().to_string(),
            expected_policy_revision: String::new(),
            projection_version: input.projection_version.clone(),
            execution_budget_ms: 1,
            context: input.context.clone(),
            items: input.items.clone(),
        },
    }
}
fn within_bounds(input: &Input, participation: &Participation) -> bool {
    let limits = &participation.policy.classification.limits;
    input.items.len() <= limits.max_items
        && input.items.len() <= 256
        && input.context.as_ref().is_none_or(|c| {
            serde_json::to_vec(c).is_ok_and(|v| v.len() <= limits.max_context_bytes)
        })
        && serde_json::to_vec(&request(input, ClassificationMode::Shadow))
            .is_ok_and(|v| v.len() <= limits.max_request_bytes)
}

/// Keep half the caller's total budget for required prose. The engine owns
/// the decision deadline within the other half, rather than a host-side 3s cap.
pub(crate) fn text_reserve(total: Duration) -> Duration {
    total / 2
}

/// Incumbent starts immediately in shadow. Its output never waits for Jev.
/// Gate allocates at most the engine budget and reserves required-text time within
/// one absolute caller deadline. No late task can apply or cache decisions.
pub(crate) async fn run<T, F, Fut, L>(
    mut input: Input,
    lookup: PolicyLookup,
    total: Duration,
    text_reserve: Duration,
    incumbent: F,
    labels: L,
) -> Outcome<T>
where
    F: FnOnce(Vec<String>, Duration) -> Fut,
    Fut: Future<Output = Option<T>>,
    L: Fn(&T) -> Reference,
{
    let started = Instant::now();
    let allow_incumbent = lookup.allows_incumbent();
    let deadline = tokio::time::Instant::now() + total;
    let all: Vec<_> = input.items.iter().map(|i| i.item_id.clone()).collect();
    let mut skipped_reason = "disabled";
    let participation = match lookup {
        PolicyLookup::Participating(p) if within_bounds(&input, &p) => Some(p),
        PolicyLookup::Participating(_) => {
            skipped_reason = "input_bounds";
            None
        },
        PolicyLookup::Unavailable(reason) => {
            skipped_reason = "policy_unavailable";
            tracing::debug!(?reason, operation = %input.operation, "memory decision policy unavailable");
            None
        },
        _ => None,
    };
    if participation.is_some() {
        skipped_reason = "admission_or_coalesced";
    }
    let admitted = participation.as_ref().and_then(|_| Job::admit(&input));
    let Some((participation, job)) = participation.zip(admitted) else {
        let incumbent = if allow_incumbent {
            tokio::time::timeout_at(deadline, Box::pin(incumbent(all, total)))
                .await
                .ok()
                .flatten()
        } else {
            None
        };
        telemetry::invocation(
            &input.scope.scope.principal,
            &input.scope.scope.workspace,
            &input.operation,
            &input.case_id,
            input.items.len(),
            skipped_reason,
            started.elapsed(),
        );
        return Outcome {
            incumbent,
            answers: BTreeMap::new(),
            origins: BTreeMap::new(),
            authority: None,
            observation: None,
        };
    };
    let job = Arc::new(job);
    if !participation.policy.gate {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let budget =
            Duration::from_millis(participation.policy.classification.limits.shadow_budget_ms)
                + decision_host::classification::RECEIPT_MARGIN;
        let request = request(&input, ClassificationMode::Shadow);
        tokio::spawn(async move {
            let _job = job.clone();
            let until = tokio::time::Instant::now() + budget.max(total);
            let result = participation
                .decide(
                    request,
                    input.scope.clone(),
                    input.agent.clone(),
                    budget,
                    Some(job),
                )
                .await;
            let observed = tokio::time::timeout_at(until, rx)
                .await
                .ok()
                .and_then(Result::ok);
            telemetry::record(
                &input,
                &participation,
                result.as_ref().ok(),
                observed
                    .as_ref()
                    .and_then(|(reference, _): &(Option<Reference>, u64)| reference.clone()),
                started.elapsed(),
                "shadow",
                true,
                observed.map(|(_, elapsed)| elapsed),
            );
        });
        let incumbent = tokio::time::timeout_at(deadline, Box::pin(incumbent(all, total)))
            .await
            .ok()
            .flatten();
        let _ = tx.send((
            incumbent.as_ref().map(labels),
            started.elapsed().as_millis() as u64,
        ));
        return Outcome {
            incumbent,
            answers: BTreeMap::new(),
            origins: BTreeMap::new(),
            authority: None,
            observation: None,
        };
    }
    let budget = (if input.requires_completion {
        total.saturating_sub(text_reserve)
    } else {
        total
    })
    .min(Duration::from_millis(
        participation
            .policy
            .classification
            .limits
            .decision_budget_ms
            + participation.policy.classification.limits.queue_budget_ms,
    ));
    let result = participation
        .decide(
            request(&input, ClassificationMode::Gate),
            input.scope.clone(),
            input.agent.clone(),
            budget,
            Some(job.clone()),
        )
        .await;
    let mut answers = BTreeMap::new();
    let mut origins = BTreeMap::new();
    // The model budget may exceed the short discovery TTL. Refresh the same
    // authority before consuming a slow successful reply; never adopt a new one.
    let current = participation.is_current()
        || (result.is_ok() && Box::pin(participation.revalidate()).await);
    if current {
        if let Ok(reply) = &result {
            for item in &reply.batch.items {
                if !input.required_questions.is_empty()
                    && input
                        .required_questions
                        .iter()
                        .all(|q| item.eligible_answers.contains_key(q))
                {
                    if let Some(response) = &item.response {
                        origins.insert(
                            item.item_id.clone(),
                            decision_engine_contract::classification::ClassificationOrigin {
                                model: response.model.clone(),
                                pack: response.pack_id.clone(),
                                pack_version: response.pack_version.clone(),
                                policy_revision: participation.revision.clone(),
                                projection_version: input.projection_version.clone(),
                                reference_version: input.reference_version.clone(),
                                batch_id: reply.batch.request_id.clone(),
                                call_ids: reply
                                    .model_calls
                                    .iter()
                                    .filter(|c| c.item_ids.contains(&item.item_id))
                                    .map(|c| c.call_id.clone())
                                    .collect(),
                                qualifications: item.eligible_answers.clone(),
                            },
                        );
                        answers.insert(
                            item.item_id.clone(),
                            response
                                .answers
                                .iter()
                                .filter(|(q, _)| item.eligible_answers.contains_key(q.as_str()))
                                .map(|(q, a)| (q.as_str().to_owned(), a.clone()))
                                .collect(),
                        );
                    }
                }
            }
        }
    }
    // Gated classification never escalates to the incumbent LLM. Each owner
    // retains its deterministic/no-mutation behavior for unresolved items.
    let reference_attempted = false;
    let incumbent = None;
    if !answers.is_empty() && !participation.is_current() {
        let current = tokio::time::timeout_at(deadline, participation.revalidate())
            .await
            .unwrap_or(false);
        if !current {
            answers.clear();
        }
    }
    let observation = telemetry::record(
        &input,
        &participation,
        result.as_ref().ok(),
        incumbent.as_ref().map(labels),
        started.elapsed(),
        "gate",
        reference_attempted,
        Some(started.elapsed().as_millis() as u64),
    );
    match input.replay.take() {
        Some(Ok(replay)) => {
            super::observation::submit(&input, &participation, &observation, replay)
        },
        Some(Err(reason)) if super::observation::selected(&input, &participation) => {
            observation.reference(reason, Some(false), None);
        },
        None if super::observation::selected(&input, &participation) => {
            observation.reference("adapter_unavailable", Some(false), None);
        },
        _ => {},
    }
    drop(job);
    origins.retain(|id, _| answers.contains_key(id));
    Outcome {
        incumbent,
        answers,
        origins,
        authority: Some(participation),
        observation: Some(observation),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
#[path = "runner_tests.rs"]
mod tests;

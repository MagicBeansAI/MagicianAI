//! Operation → model → pack binding and dispatch.
//!
//! The runtime is deliberately small: it resolves a configured operation to
//! its pack and adapter, dispatches, and re-validates answers against the
//! pack before anyone composes on them. Locality, shadow policy, and
//! thresholds are the host's call — this crate has no idea what "local
//! mode" means, which is what keeps it vendor- and product-neutral.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use tracing::warn;

use crate::error::DecisionError;
use crate::model::{ModelCapabilities, ModelIdentity, StructuredDecisionModel};
use crate::pack::Pack;
use crate::primitives::Question;
use crate::request::{validate_answers, DecisionRequest, DecisionResponse, DecisionState};

/// How long a model that failed at transport level is skipped before the
/// router tries it again.
pub const UNHEALTHY_COOLDOWN: Duration = Duration::from_secs(30);

/// Background scheduling has separate queue/inference caps and still respects
/// the caller's absolute deadline. Foreground tools retain their short budget.
#[derive(Clone, Copy)]
pub struct BackgroundBudget {
    pub queue: Duration,
    pub inference: Duration,
    pub deadline: tokio::time::Instant,
}
tokio::task_local! { static BACKGROUND_BUDGET: Option<BackgroundBudget>; }
pub async fn with_background_budget<T>(
    budget: Option<BackgroundBudget>,
    future: impl std::future::Future<Output = T>,
) -> T {
    BACKGROUND_BUDGET.scope(budget, future).await
}

/// One model in an operation's route, with the thresholds that model owns.
#[derive(Clone)]
pub struct BoundModel {
    pub admission: Arc<crate::admission::ModelAdmission>,
    /// The `decision.models` entry name (or the model id for direct binds).
    pub name: String,
    pub model: Arc<dyn StructuredDecisionModel>,
    /// `None` = this model has no thresholds of its own: its answers may be
    /// shadowed and logged, never gated on (thresholds are never copied
    /// across models).
    pub thresholds: Option<BTreeMap<String, f64>>,
}

/// One bound operation: the pack it runs and the models that may answer it,
/// in preference order.
#[derive(Clone)]
pub struct BoundOperation {
    pub operation: String,
    pub pack: Pack,
    /// The first model in the route (kept for callers that predate routes).
    pub model: Arc<dyn StructuredDecisionModel>,
    pub route: Vec<BoundModel>,
}

pub struct DecisionRuntime {
    operations: HashMap<String, BoundOperation>,
}

#[derive(Default)]
pub struct DecisionRuntimeBuilder {
    operations: HashMap<String, BoundOperation>,
}

impl DecisionRuntimeBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind an operation to one model. Rebinding the same name replaces the
    /// previous binding so a config reload can retarget without rebuilding.
    /// The model owns an empty threshold set, so consumers apply their
    /// built-in defaults — the behavior before routes existed.
    pub fn bind(
        self,
        operation: impl Into<String>,
        pack: Pack,
        model: Arc<dyn StructuredDecisionModel>,
    ) -> Self {
        let name = model.identity().model;
        self.bind_route(
            operation,
            pack,
            vec![BoundModel {
                admission: Default::default(),
                name,
                model,
                thresholds: Some(BTreeMap::new()),
            }],
        )
    }

    /// Bind an operation to an ordered route of models. An empty route
    /// leaves the operation unbound.
    pub fn bind_route(
        mut self,
        operation: impl Into<String>,
        pack: Pack,
        route: Vec<BoundModel>,
    ) -> Self {
        let operation = operation.into();
        let Some(first) = route.first() else {
            self.operations.remove(&operation);
            return self;
        };
        self.operations.insert(
            operation.clone(),
            BoundOperation {
                model: Arc::clone(&first.model),
                operation,
                pack,
                route,
            },
        );
        self
    }

    pub fn build(self) -> DecisionRuntime {
        DecisionRuntime {
            operations: self.operations,
        }
    }
}

/// Why a model cannot take a request, or `None` when it fits. Conservative
/// on purpose: a model that would silently truncate the state or fall off
/// its option-count cliff answers worse than one that is skipped.
pub fn misfit_reason(
    request: &DecisionRequest,
    capabilities: &ModelCapabilities,
) -> Option<String> {
    if let Some(max_questions) = capabilities.max_questions {
        if request.questions.len() > max_questions {
            return Some(format!(
                "request has {} questions, model takes {max_questions}",
                request.questions.len()
            ));
        }
    }
    if let Some(max_options) = capabilities.max_choice_options {
        for question in &request.questions {
            if let Question::Choice(choice) = question {
                if choice.criteria.len() > max_options {
                    return Some(format!(
                        "question '{}' has {} options, model takes {max_options}",
                        choice.id.as_str(),
                        choice.criteria.len()
                    ));
                }
            }
        }
    }
    if !capabilities.supports_score
        && request
            .questions
            .iter()
            .any(|question| matches!(question, Question::Score(_)))
    {
        return Some("request asks a Score question".to_string());
    }
    if let Some(max_tokens) = capabilities.max_state_tokens {
        let estimate = estimate_request_tokens(request);
        if estimate > max_tokens {
            return Some(format!(
                "request is ~{estimate} tokens, model takes {max_tokens}"
            ));
        }
    }
    None
}

/// Upper-bound token estimate for state + questions: serialized bytes / 3.
/// Deliberately pessimistic (JSON punctuation and subword tokenizers both
/// run denser than the usual four characters per token), so a misfit is
/// caught before the model truncates.
pub fn estimate_request_tokens(request: &DecisionRequest) -> u64 {
    let state = serde_json::to_vec(request.state.as_json()).map_or(0, |bytes| bytes.len());
    let questions = serde_json::to_vec(&request.questions).map_or(0, |bytes| bytes.len());
    ((state + questions) as u64).div_ceil(3)
}

/// Errors that allow another route to serve the request. Admission pressure
/// advances the route without putting the provider on cooldown; provider health
/// is tracked separately by ModelPermit.
pub(crate) fn is_availability_error(error: &DecisionError) -> bool {
    matches!(
        error,
        DecisionError::DispatchFull
            | DecisionError::DispatchTimeout
            | DecisionError::Transport(_)
            | DecisionError::Timeout
            | DecisionError::RateLimited { .. }
            | DecisionError::ProviderStatus { .. }
            | DecisionError::InvalidResponse(_)
            | DecisionError::UnknownOption { .. }
    )
}

impl DecisionRuntime {
    /// Unbound is the only "off" state: the caller keeps its incumbent path
    /// and no default model is ever consulted.
    pub fn operation_bound(&self, operation: &str) -> bool {
        self.operations.contains_key(operation)
    }

    pub fn bound_operations(&self) -> Vec<&str> {
        self.operations.keys().map(String::as_str).collect()
    }

    pub fn bound_operation(&self, operation: &str) -> Option<&BoundOperation> {
        self.operations.get(operation)
    }

    /// The thresholds owned by the model that answered, looked up by the
    /// identity echoed on the response. `None` when that model owns none
    /// (or is not on the operation's route): its answer must not gate.
    pub fn thresholds_for(
        &self,
        operation: &str,
        answered_by: &ModelIdentity,
    ) -> Option<BTreeMap<String, f64>> {
        let bound = self.operations.get(operation)?;
        let mut entries = bound.route.iter().filter(|entry| {
            let identity = entry.model.identity();
            identity.adapter == answered_by.adapter && identity.model == answered_by.model
        });
        let entry = entries.next()?;
        // The wire identity does not include the configured profile name.
        // If aliases disagree, never guess which profile supplied the answer.
        if entries.any(|other| other.thresholds != entry.thresholds) {
            return None;
        }
        entry.thresholds.clone()
    }

    /// Build the request for an operation without dispatching (shadow lanes
    /// and eval harnesses use this to run both models off one request).
    pub fn build_request(
        &self,
        operation: &str,
        state: DecisionState,
    ) -> Result<DecisionRequest, DecisionError> {
        let bound = self
            .operations
            .get(operation)
            .ok_or_else(|| DecisionError::OperationUnbound(operation.to_string()))?;
        Ok(bound.pack.to_request(operation, state))
    }

    pub async fn evaluate(
        &self,
        operation: &str,
        state: DecisionState,
    ) -> Result<DecisionResponse, DecisionError> {
        let request = self.build_request(operation, state)?;
        self.evaluate_request(request).await
    }

    /// Evaluate a pre-built request. The dynamic-candidate path: the host
    /// builds the request via [`Self::build_request`], injects Choice
    /// candidates with [`crate::request::set_choice_candidates`], then
    /// dispatches here. Shadow lanes use the same entry point to run both
    /// models off one request.
    ///
    /// Routing: the first model on the route that fits the request and is
    /// not cooling down answers. An availability failure puts that model on
    /// [`UNHEALTHY_COOLDOWN`] and moves to the next. No fitting, healthy
    /// model → [`DecisionError::NoFittingModel`]; the caller keeps its
    /// incumbent path exactly as for an unbound operation.
    pub async fn evaluate_request(
        &self,
        request: DecisionRequest,
    ) -> Result<DecisionResponse, DecisionError> {
        self.evaluate_request_before(request, None).await
    }

    /// Bounded selection fallback: reserve time for remaining models, retry
    /// uncertain answers, and retain the best coherent single-model response.
    /// The caller still owns the absolute deadline and output qualification.
    pub async fn evaluate_request_before(
        &self,
        request: DecisionRequest,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<DecisionResponse, DecisionError> {
        self.evaluate_request_before_mapped(request, deadline, None)
            .await
    }

    /// A shared classification request uses synthetic question IDs. Map each
    /// synthetic ID back to its pack head for model-owned confidence routing.
    /// The returned response still carries synthetic IDs for the engine's
    /// checked demultiplexing; qualification uses canonical per-item heads.
    pub async fn evaluate_request_before_mapped(
        &self,
        request: DecisionRequest,
        deadline: Option<tokio::time::Instant>,
        canonical_heads: Option<&BTreeMap<String, String>>,
    ) -> Result<DecisionResponse, DecisionError> {
        // An uninjected dynamic question is a caller fault, not a provider
        // outage. Reject before admission so it cannot poison shared health.
        if request.questions.iter().any(|question| {
            matches!(question,
            Question::Choice(choice) if choice.criteria.is_empty())
        }) {
            return Err(DecisionError::InvalidResponse(
                "Choice requires candidates".into(),
            ));
        }
        let bound = self
            .operations
            .get(request.operation.as_str())
            .ok_or_else(|| DecisionError::OperationUnbound(request.operation.clone()))?;
        // A pin chooses the model, not permission to discard its input or
        // exceed the caller's deadline. A single-model route is bounded too.
        // Explicit gates apply to unbatched callers too. Keep the historical
        // first-success behavior only for routes with no configured gates.
        let check_confidence = deadline.is_some()
            || bound
                .route
                .iter()
                .any(|entry| entry.thresholds.as_ref().is_some_and(|t| !t.is_empty()));
        let mut best: Option<(DecisionResponse, BTreeMap<String, f64>)> = None;
        let mut skipped: Vec<String> = Vec::new();
        let mut last_availability_error = None;
        let mut budget_exhausted = false;
        let fitting: Vec<_> = bound
            .route
            .iter()
            .filter(|entry| {
                if let Some(reason) = misfit_reason(&request, &entry.model.capabilities()) {
                    skipped.push(format!("{}: {reason}", entry.name));
                    false
                } else {
                    true
                }
            })
            .collect();
        for (index, entry) in fitting.iter().enumerate() {
            let background = BACKGROUND_BUDGET.try_with(|b| *b).ok().flatten();
            let result = if let Some(mut budget) = background {
                let routes_left = (fitting.len() - index) as u32;
                let now = tokio::time::Instant::now();
                let window = budget
                    .deadline
                    .saturating_duration_since(now)
                    .saturating_sub(Duration::from_millis(2))
                    / routes_left;
                if window.is_zero() {
                    budget_exhausted = true;
                    break;
                }
                // An owner may grant less time than the configured queue +
                // inference caps. Reserve its remaining wall time for later
                // fallbacks too; the primary cannot consume the whole deadline.
                budget.deadline = now + window;
                budget.queue /= routes_left;
                budget.inference /= routes_left;
                evaluate_entry(entry, &request, Some(budget.deadline), Some(budget)).await
            } else if let Some(deadline) = deadline {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                // Leave a small return margin for the enclosing batch deadline.
                let budget = remaining.saturating_sub(Duration::from_millis(2))
                    / (fitting.len() - index) as u32;
                if budget.is_zero() {
                    budget_exhausted = true;
                    break;
                }
                evaluate_entry(
                    entry,
                    &request,
                    Some(tokio::time::Instant::now() + budget),
                    None,
                )
                .await
            } else {
                evaluate_entry(entry, &request, None, None).await
            };
            match result {
                Ok(response) => {
                    if !check_confidence {
                        return Ok(response);
                    }
                    let thresholds = entry.thresholds.clone().unwrap_or_default();
                    let effective = mapped_thresholds(&thresholds, canonical_heads);
                    let eligible = confident_answers(&response, &effective);
                    if let Some((previous, previous_thresholds)) = &best {
                        // Never replace an already-confident decision with a
                        // contradictory fallback. No cross-model answer merging.
                        let previous_effective =
                            mapped_thresholds(previous_thresholds, canonical_heads);
                        let previous_eligible = confident_answers(previous, &previous_effective);
                        if previous_eligible.iter().any(|q| {
                            !eligible.contains(q)
                                || !same_decision(&previous.answers[*q], &response.answers[*q])
                        }) {
                            continue;
                        }
                    }
                    let complete = eligible.len() == request.questions.len();
                    best = Some((response, thresholds));
                    if complete {
                        return Ok(best.take().unwrap().0);
                    }
                },
                Err(error) if is_availability_error(&error) => {
                    warn!(operation = %request.operation, model = %entry.name, %error, "decision route: model failed; trying the next");
                    skipped.push(format!("{}: {error}", entry.name));
                    last_availability_error = Some(error);
                },
                Err(error) => return Err(error),
            }
        }
        if let Some((response, _)) = best {
            return Ok(response);
        }
        if let Some(error) = last_availability_error {
            return Err(error);
        }
        if budget_exhausted {
            return Err(DecisionError::Timeout);
        }
        Err(DecisionError::NoFittingModel {
            operation: request.operation.clone(),
            skipped,
        })
    }

    /// Only models on this operation's route are exposed to its caller.
    pub fn health_for(
        &self,
        operation: &str,
    ) -> Vec<decision_engine_contract::action::ActionModelHealth> {
        let Some(bound) = self.operations.get(operation) else {
            return Vec::new();
        };
        bound
            .route
            .iter()
            .filter_map(|entry| {
                entry.admission.health().map(|issue| {
                    decision_engine_contract::action::ActionModelHealth {
                        model: entry.name.clone(),
                        issue,
                    }
                })
            })
            .collect()
    }
}

fn mapped_thresholds(
    canonical: &BTreeMap<String, f64>,
    heads: Option<&BTreeMap<String, String>>,
) -> BTreeMap<String, f64> {
    match heads {
        Some(heads) => heads
            .iter()
            .filter_map(|(synthetic, original)| {
                canonical
                    .get(original)
                    .map(|threshold| (synthetic.clone(), *threshold))
            })
            .collect(),
        None => canonical.clone(),
    }
}

pub fn confident_answers<'a>(
    response: &'a DecisionResponse,
    thresholds: &BTreeMap<String, f64>,
) -> Vec<&'a crate::primitives::QuestionId> {
    response
        .answers
        .iter()
        .filter_map(|(q, answer)| {
            let confidence = match answer {
                crate::request::Answer::Noul { noul } => noul.max(1.0 - noul),
                crate::request::Answer::Choice { confidence, .. }
                | crate::request::Answer::Score { confidence, .. } => *confidence,
            };
            thresholds
                .get(q.as_str())
                // The shared action pack's choice gate predates the per-head schema.
                .or_else(|| {
                    (q.as_str() == "next_action")
                        .then(|| thresholds.get("next_action_confidence"))
                        .flatten()
                })
                .filter(|t| confidence >= **t)
                .map(|_| q)
        })
        .collect()
}

fn same_decision(a: &crate::request::Answer, b: &crate::request::Answer) -> bool {
    use crate::request::Answer;
    match (a, b) {
        (Answer::Noul { noul: a }, Answer::Noul { noul: b }) => (*a >= 0.5) == (*b >= 0.5),
        (Answer::Choice { choice: a, .. }, Answer::Choice { choice: b, .. }) => a == b,
        (Answer::Score { score: a, .. }, Answer::Score { score: b, .. }) => a.round() == b.round(),
        _ => false,
    }
}

async fn evaluate_entry(
    entry: &BoundModel,
    request: &DecisionRequest,
    deadline: Option<tokio::time::Instant>,
    background: Option<BackgroundBudget>,
) -> Result<DecisionResponse, DecisionError> {
    let bytes = serde_json::to_vec(request).map_or(usize::MAX, |v| v.len());
    let admission_deadline = background
        .map(|b| b.deadline.min(tokio::time::Instant::now() + b.queue))
        .or(deadline);
    let permit = match admission_deadline {
        Some(deadline) => tokio::time::timeout_at(
            deadline,
            entry.admission.enter_request(&request.operation, bytes),
        )
        .await
        .map_err(|_| DecisionError::DispatchTimeout)??,
        None => {
            entry
                .admission
                .enter_request(&request.operation, bytes)
                .await?
        },
    };
    let deadline = background
        .map(|b| b.deadline.min(tokio::time::Instant::now() + b.inference))
        .or(deadline);
    crate::admission::with_physical_lease(
        permit.physical_lease(),
        crate::telemetry::with_queue_wait(permit.queue_wait_ms(), async {
            let mut receipt = (!entry.model.records_attempts()).then(|| {
                let identity = entry.model.identity();
                crate::telemetry::Attempt::new(
                    &request.operation,
                    identity.clone(),
                    format!("decision:{}", identity.adapter),
                    crate::host::runs_in_process(&identity.adapter),
                    &crate::telemetry::group_id(),
                    1,
                )
            });
            let (result, budget_expired) = match deadline {
                Some(deadline) => {
                    match tokio::time::timeout_at(deadline, entry.model.evaluate(request.clone()))
                        .await
                    {
                        Ok(result) => (result, false),
                        Err(_) => (Err(DecisionError::Timeout), true),
                    }
                },
                None => (entry.model.evaluate(request.clone()).await, false),
            };
            if let Some(receipt) = &mut receipt {
                receipt.result(&result);
            }
            let result = result.and_then(|response| {
                validate_answers(request, &response)?;
                Ok(response)
            });
            if let (Some(receipt), Err(error)) = (&mut receipt, &result) {
                receipt.failed(error);
            }
            drop(receipt);
            // An allocated route slice expiring does not prove a provider outage;
            // genuine transport/provider errors still drive the shared cooldown.
            if !budget_expired {
                permit.finish(result.as_ref().err());
            }
            result
        }),
    )
    .await
}

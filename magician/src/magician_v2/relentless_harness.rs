//! Resource integration for composite actors on the existing native harness.
//! The controller reserves one parent attempt; each physical call consumes
//! that reservation through the existing ledger and move-only provider permit.

use std::{
    fmt,
    sync::{Arc, Mutex},
};

use anyhow::{ensure, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use magician_relentless::{
    budget::{OperationClass, OperationIntent},
    store::{ActorCheckpoint, AdapterBudget, OperationReceipt, RunStore},
};
use magicllm::{
    LlmPhysicalAttemptObservation, LlmPhysicalAttemptOutcome, LlmPhysicalAttemptPermit,
    LlmPhysicalAttemptPlan, LlmPhysicalAttemptStart, LlmPhysicalResourceAuthorizer,
    LlmPhysicalResourceGuard,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::query_analysis::operation_llm_router::{BoundedOperationRoute, OperationLlmRouter};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessResourceBinding {
    pub task_id: String,
    pub root_execution_id: String,
    pub execution_id: String,
    pub maximum_physical_calls: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub maximum_tool_calls: u32,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub maximum_tool_cost_microusd: u64,
    /// The complete admitted actor recipe, including tools and harness limits.
    pub configuration_hash: String,
}

pub struct HarnessResources {
    pub(super) store: Arc<RunStore>,
    pub(super) run_id: Uuid,
    pub(super) worker_id: String,
    pub(super) parent: OperationIntent,
    route: BoundedOperationRoute,
    pub(super) binding: HarnessResourceBinding,
    binding_hash: String,
    pub(super) cancel: CancellationToken,
    calls: Arc<PhysicalCalls>,
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}
fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

impl HarnessResourceBinding {
    fn maximum_operations(&self) -> Result<u32> {
        self.maximum_physical_calls
            .checked_add(self.maximum_tool_calls)
            .context("harness operation ceiling overflow")
    }
}

#[derive(Default)]
struct PhysicalCalls {
    state: Mutex<PhysicalCallState>,
    drained: tokio::sync::Notify,
}

#[derive(Default)]
struct PhysicalCallState {
    closed: bool,
    active: usize,
}

pub(super) struct PhysicalCall {
    calls: Arc<PhysicalCalls>,
}

impl PhysicalCalls {
    fn enter(self: &Arc<Self>) -> Result<PhysicalCall> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        ensure!(!state.closed, "harness physical dispatch is closed");
        state.active = state
            .active
            .checked_add(1)
            .context("physical call count overflow")?;
        Ok(PhysicalCall {
            calls: self.clone(),
        })
    }

    async fn close_and_drain(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
        loop {
            // Register before inspecting the count: the last permit may drop
            // between the inspection and awaiting its notification.
            let notified = self.drained.notified();
            if self.state.lock().unwrap_or_else(|e| e.into_inner()).active == 0 {
                return;
            }
            notified.await;
        }
    }
}

impl Drop for PhysicalCall {
    fn drop(&mut self) {
        let mut state = self.calls.state.lock().unwrap_or_else(|e| e.into_inner());
        state.active -= 1;
        if state.active == 0 {
            self.calls.drained.notify_waiters();
        }
    }
}

impl fmt::Debug for HarnessResources {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HarnessResources")
            .field("run_id", &self.run_id)
            .field("operation_id", &self.parent.id)
            .finish_non_exhaustive()
    }
}

async fn io<T: Send + 'static>(work: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .context("harness resource storage worker")?
}

fn digest(value: &impl Serialize) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}

impl HarnessResources {
    /// Controller-only construction after its parent operation is reserved.
    /// A dispatch marker is durable before a runtime carrier is returned.
    pub async fn begin(
        store: Arc<RunStore>,
        run_id: Uuid,
        worker_id: String,
        parent: OperationIntent,
        route: BoundedOperationRoute,
        binding: HarnessResourceBinding,
        cancel: CancellationToken,
    ) -> Result<Arc<Self>> {
        let owner = Self::construct(store, run_id, worker_id, parent, route, binding, cancel)?;
        let begin = owner.clone();
        io(move || {
            begin.store.begin_budgeted_adapter_job(
                begin.run_id,
                &begin.worker_id,
                &begin.parent,
                begin.binding_hash.clone(),
                begin.binding.maximum_operations()?,
                Utc::now(),
            )
        })
        .await?;
        Ok(owner)
    }

    /// Restore a new runtime-only carrier after the controller has journaled
    /// the exact answer. The original route/recipe and physical ledger must
    /// match before a continuation claim can be consumed.
    #[allow(clippy::too_many_arguments)]
    pub async fn resume(
        store: Arc<RunStore>,
        run_id: Uuid,
        worker_id: String,
        parent: OperationIntent,
        route: BoundedOperationRoute,
        binding: HarnessResourceBinding,
        checkpoint: ActorCheckpoint,
        cancel: CancellationToken,
    ) -> Result<Arc<Self>> {
        let owner = Self::construct(store, run_id, worker_id, parent, route, binding, cancel)?;
        let resume = owner.clone();
        io(move || {
            let budget = resume.store.adapter_budget(resume.run_id, &resume.parent)?;
            resume.validate_budget(&budget)?;
            let budget = resume.store.resume_budgeted_adapter_job(
                resume.run_id,
                &resume.worker_id,
                &resume.parent,
                &checkpoint,
                Utc::now(),
            )?;
            resume.validate_budget(&budget)
        })
        .await?;
        Ok(owner)
    }

    fn construct(
        store: Arc<RunStore>,
        run_id: Uuid,
        worker_id: String,
        parent: OperationIntent,
        route: BoundedOperationRoute,
        binding: HarnessResourceBinding,
        cancel: CancellationToken,
    ) -> Result<Arc<Self>> {
        ensure!(
            !cancel.is_cancelled() && parent.class == OperationClass::Actor,
            "harness resources require a live actor reservation"
        );
        ensure!(
            binding.maximum_physical_calls > 0
                && binding.configuration_hash.len() == 64
                && binding
                    .configuration_hash
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                && [
                    &binding.task_id,
                    &binding.root_execution_id,
                    &binding.execution_id
                ]
                .into_iter()
                .all(|id| !id.trim().is_empty()
                    && id.len() <= 512
                    && !id.bytes().any(|b| b.is_ascii_control())),
            "invalid harness resource binding"
        );
        let maximum = route
            .maximum_microusd()
            .checked_mul(u64::from(binding.maximum_physical_calls))
            .and_then(|maximum| maximum.checked_add(binding.maximum_tool_cost_microusd))
            .context("harness maximum quote overflow")?;
        let minimum = route
            .maximum_microusd()
            .checked_add(binding.maximum_tool_cost_microusd)
            .context("harness minimum quote overflow")?;
        ensure!(
            parent.maximum_cost_microusd >= minimum && parent.maximum_cost_microusd <= maximum,
            "harness parent quote lies outside admitted resource bounds"
        );
        // Qualify native tool capability before publishing any dispatch marker.
        route.harness_router()?;
        let binding_hash = digest(&serde_json::json!({
            "version": "relentless-harness-resources-v1", "parent": parent,
            "binding": binding, "route": route.model_pin(),
        }))?;
        Ok(Arc::new(Self {
            store,
            run_id,
            worker_id,
            parent,
            route,
            binding,
            binding_hash,
            cancel,
            calls: Arc::new(PhysicalCalls::default()),
        }))
    }

    pub fn guard(self: &Arc<Self>) -> LlmPhysicalResourceGuard {
        LlmPhysicalResourceGuard::new(self.clone())
    }

    pub(super) fn enter_tool(&self) -> Result<PhysicalCall> {
        ensure!(
            !self.cancel.is_cancelled(),
            "harness cancelled before tool dispatch"
        );
        self.calls.enter()
    }

    pub fn router(self: &Arc<Self>) -> Result<OperationLlmRouter> {
        Ok(self
            .route
            .harness_router()?
            .with_physical_resource_guard(Some(self.guard())))
    }

    /// Stop accepting requests and join all permits, including storage work
    /// retained by spawn_blocking after cancellation of its async caller.
    pub async fn close_and_drain(&self) {
        self.calls.close_and_drain().await;
    }

    pub async fn save_checkpoint(
        self: &Arc<Self>,
        attention_id: String,
        bytes: Vec<u8>,
    ) -> Result<ActorCheckpoint> {
        {
            let state = self.calls.state.lock().unwrap_or_else(|e| e.into_inner());
            ensure!(
                state.closed && state.active == 0,
                "harness checkpoint requires closed and drained dispatch"
            );
        }
        let owner = self.clone();
        io(move || {
            owner.store.checkpoint_budgeted_adapter_job(
                owner.run_id,
                &owner.worker_id,
                &owner.parent,
                attention_id,
                &bytes,
                Utc::now(),
            )
        })
        .await
    }

    fn validate_budget(&self, budget: &AdapterBudget) -> Result<()> {
        ensure!(
            budget.binding_hash == self.binding_hash
                && budget.maximum_operations == self.binding.maximum_operations()?,
            "harness resource recipe changed"
        );
        Ok(())
    }

    pub async fn receipt(self: &Arc<Self>) -> Result<OperationReceipt> {
        let owner = self.clone();
        io(move || {
            let budget = owner.store.adapter_budget(owner.run_id, &owner.parent)?;
            owner.validate_budget(&budget)?;
            Ok(OperationReceipt {
                input_hash: owner.parent.input_hash.clone(),
                actual_cost_microusd: budget.actual_cost_microusd()?,
                receipt_hash: digest(&serde_json::json!({
                    "parent": owner.parent, "binding_hash": owner.binding_hash, "budget": budget,
                }))?,
            })
        })
        .await
    }

    /// Only fully reconciled exhaustion may end a failed attempt and release
    /// its unused review allowance. Unknown usage must keep the parent held.
    pub(super) async fn known_call_allowance_exhausted(self: &Arc<Self>) -> Result<bool> {
        let owner = self.clone();
        io(move || {
            let budget = owner.store.adapter_budget(owner.run_id, &owner.parent)?;
            owner.validate_budget(&budget)?;
            let Some(actual) = budget.actual_cost_microusd()? else {
                return Ok(false);
            };
            let calls = budget
                .ledger
                .operations()
                .values()
                .filter(|operation| operation.intent.id.starts_with("physical:"))
                .count();
            Ok(calls >= owner.binding.maximum_physical_calls as usize
                || owner
                    .parent
                    .maximum_cost_microusd
                    .checked_sub(actual)
                    .context("harness settled cost exceeds parent")?
                    < owner.route.maximum_microusd())
        })
        .await
    }

    pub async fn save_result<T: Serialize + Send + Sync>(
        self: &Arc<Self>,
        result: &T,
        receipt: OperationReceipt,
    ) -> Result<()> {
        {
            let state = self.calls.state.lock().unwrap_or_else(|e| e.into_inner());
            ensure!(
                state.closed && state.active == 0,
                "harness result requires closed and drained dispatch"
            );
        }
        let bytes = serde_json::to_vec(result)?;
        let owner = self.clone();
        io(move || {
            owner
                .store
                .finish_budgeted_adapter_job(owner.run_id, &owner.parent, &bytes, &receipt)
        })
        .await
    }
}

#[async_trait]
impl LlmPhysicalResourceAuthorizer for HarnessResources {
    async fn reserve(
        &self,
        plan: LlmPhysicalAttemptPlan,
    ) -> Result<Box<dyn LlmPhysicalAttemptPermit>, String> {
        let call = self.calls.enter().map_err(|error| error.to_string())?;
        let validate = || -> Result<OperationIntent> {
            ensure!(
                !self.cancel.is_cancelled(),
                "harness cancelled before physical reservation"
            );
            self.route.validate_physical_plan(&plan)?;
            ensure!(
                plan.task_id() == self.binding.task_id
                    && plan.root_execution_id() == self.binding.root_execution_id
                    && plan.execution_id() == self.binding.execution_id,
                "physical call changed canonical harness identity"
            );
            Ok(OperationIntent {
                id: format!("physical:{}:{}", plan.llm_call_id(), plan.attempt_index()),
                input_hash: digest(&serde_json::json!({
                    "binding_hash": self.binding_hash, "call_id": plan.llm_call_id(),
                    "request_digest": plan.effective_request_digest(),
                    "profile": plan.profile(), "provider": plan.provider(), "model": plan.model(),
                    "cohort": plan.transport_cohort(), "pricing": plan.pricing_version(),
                    "input_upper": plan.input_token_upper(), "output_upper": plan.output_token_upper(),
                    "cost_upper": plan.cost_upper_microusd(), "timeout_secs": plan.timeout_secs(),
                }))?,
                class: self.parent.class,
                maximum_cost_microusd: plan.cost_upper_microusd(),
            })
        };
        let child = validate().map_err(|error| error.to_string())?;
        let store = self.store.clone();
        let run_id = self.run_id;
        let worker = self.worker_id.clone();
        let parent = self.parent.clone();
        let reserved = child.clone();
        let maximum_calls = self.binding.maximum_physical_calls;
        let call = io(move || {
            store.reserve_adapter_child_bounded(
                run_id,
                &worker,
                &parent,
                reserved,
                Some(("physical:", maximum_calls)),
                Utc::now(),
            )?;
            Ok(call)
        })
        .await
        .map_err(|error| error.to_string())?;
        Ok(Box::new(HarnessPhysicalAttempt {
            store: self.store.clone(),
            run_id,
            worker_id: self.worker_id.clone(),
            parent: self.parent.clone(),
            child,
            plan,
            cancel: self.cancel.clone(),
            _call: call,
            ready: false,
            started_at: None,
        }))
    }
}

struct HarnessPhysicalAttempt {
    store: Arc<RunStore>,
    run_id: Uuid,
    worker_id: String,
    parent: OperationIntent,
    child: OperationIntent,
    plan: LlmPhysicalAttemptPlan,
    cancel: CancellationToken,
    ready: bool,
    started_at: Option<i64>,
    _call: PhysicalCall,
}

impl fmt::Debug for HarnessPhysicalAttempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HarnessPhysicalAttempt")
            .field("operation_id", &self.child.id)
            .field("started", &self.started_at.is_some())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl LlmPhysicalAttemptPermit for HarnessPhysicalAttempt {
    async fn mark_dispatched(&mut self) -> Result<(), String> {
        if self.ready || self.started_at.is_some() || self.cancel.is_cancelled() {
            return Err("harness physical dispatch no longer eligible".into());
        }
        self.ready = true;
        Ok(())
    }

    fn mark_physical_started(&mut self) -> Result<LlmPhysicalAttemptStart, String> {
        let now = Utc::now();
        if !self.ready
            || self.started_at.is_some()
            || self.cancel.is_cancelled()
            || now.timestamp_millis() < self.plan.started_at_unix_ms()
        {
            return Err("harness physical start no longer eligible".into());
        }
        // The physical start trait deliberately has no awaited gap before I/O.
        // This one synchronous, durably synced transition is its final fence.
        let deadline = self
            .store
            .start_adapter_child(self.run_id, &self.worker_id, &self.parent, &self.child, now)
            .map_err(|error| error.to_string())?;
        let remaining = (deadline - Utc::now()).num_milliseconds();
        if remaining <= 0 {
            return Err("harness resource window expired while syncing physical start".into());
        }
        let start = LlmPhysicalAttemptStart::from_resource_permit(
            now.timestamp_millis(),
            remaining as u64,
        )?;
        self.started_at = Some(now.timestamp_millis());
        Ok(start)
    }

    async fn release_pre_io(self: Box<Self>, _: i64) -> Result<(), String> {
        if self.started_at.is_some() {
            return Err("started harness call cannot be refunded".into());
        }
        let evidence =
            digest(&serde_json::json!({"intent": self.child, "physical_start": "absent"}))
                .map_err(|error| error.to_string())?;
        io(move || {
            let result = self.store.release_adapter_child_pre_io(
                self.run_id,
                &self.parent,
                &self.child,
                evidence,
            );
            drop(self);
            result
        })
        .await
        .map_err(|error| error.to_string())
    }

    async fn settle(
        self: Box<Self>,
        observation: LlmPhysicalAttemptObservation,
    ) -> Result<(), String> {
        let admitted_usage = self.started_at == Some(observation.started_at_unix_ms())
            && observation.input_tokens() <= self.plan.input_token_upper()
            && observation.cached_input_tokens() <= self.plan.cached_input_token_upper()
            && observation.output_tokens() <= self.plan.output_token_upper()
            && observation.cost_microusd() <= self.plan.cost_upper_microusd();
        // Invalid usage still follows a real physical start. Persist uncertainty
        // before rejecting it so spare funding cannot authorize another call.
        let cost = (admitted_usage
            && observation.outcome() == LlmPhysicalAttemptOutcome::Committed)
            .then_some(observation.cost_microusd());
        let receipt = digest(&serde_json::json!({
            "intent": self.child, "started_at": observation.started_at_unix_ms(),
            "completed_at": observation.completed_at_unix_ms(), "known_cost": cost,
            "input_tokens": observation.input_tokens(), "output_tokens": observation.output_tokens(),
            "cached_input_tokens": observation.cached_input_tokens(),
        })).map_err(|error| error.to_string())?;
        io(move || {
            let result = self.store.settle_adapter_child(
                self.run_id,
                &self.parent,
                &self.child,
                cost,
                receipt,
            );
            if result.is_err() {
                // An accounting write failure cannot leave this live owner
                // authorizing calls against an unsettled physical start.
                self.cancel.cancel();
            }
            drop(self);
            result
        })
        .await
        .map_err(|error| error.to_string())?;
        if !admitted_usage {
            return Err("harness physical usage changed its admitted start or ceiling".into());
        }
        Ok(())
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::query_analysis::operation_llm_router::{
        LLMOperation, OperationRoutingEndpoint, OperationRoutingOverrides,
    };
    use super::*;
    use magician_relentless::{
        budget::Allowance,
        contract::{
            Budget, Criterion, CriterionKind, CriticPin, DefinitionOfDone, ReviewPolicy,
            UnknownReviewPolicy,
        },
        store::{Admission, RunEvent, RunScope},
    };
    use magicllm::{LLMProfile, LLMRouterConfig, TaskRef};
    use serde_json::json;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    pub(crate) struct Fixture {
        _temp: tempfile::TempDir,
        server: MockServer,
        pub(crate) owner: Arc<HarnessResources>,
        router: Arc<OperationLlmRouter>,
        pub(crate) queue: Arc<magicllm::LlmDispatchQueue>,
    }

    impl Fixture {
        async fn new(calls: u32, usage: bool) -> Self {
            Self::with_delay(calls, usage, std::time::Duration::ZERO).await
        }

        async fn with_delay(calls: u32, usage: bool, delay: std::time::Duration) -> Self {
            Self::with_tools(calls, usage, delay, 0, 0).await
        }

        pub(crate) async fn with_tools(
            calls: u32,
            usage: bool,
            delay: std::time::Duration,
            tool_calls: u32,
            tool_cost: u64,
        ) -> Self {
            Self::with_parent_cap(calls, usage, delay, tool_calls, tool_cost, None).await
        }

        async fn with_parent_cap(
            calls: u32,
            usage: bool,
            delay: std::time::Duration,
            tool_calls: u32,
            tool_cost: u64,
            funded_calls: Option<u32>,
        ) -> Self {
            static KEY: std::sync::Once = std::sync::Once::new();
            KEY.call_once(|| {
                std::env::set_var("RELENTLESS_RESOURCE_FIXTURE_KEY", "local-fixture-only")
            });
            let server = MockServer::start().await;
            let mut response = json!({
                "id": "resp_fixture", "model": "gpt-6.1-sol", "status": "completed",
                "output": [{"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":"bounded candidate"}]}],
            });
            if usage {
                response["usage"] = json!({"input_tokens":40,"output_tokens":5,"total_tokens":45});
            }
            Mock::given(method("POST"))
                .and(path("/v1/responses"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(response)
                        .set_delay(delay),
                )
                .mount(&server)
                .await;
            let profile: LLMProfile = serde_json::from_value(json!({
                "provider":"openai", "model":"gpt-6.1-sol", "api_key_env":"RELENTLESS_RESOURCE_FIXTURE_KEY",
                // Bootstrap accepts a complete endpoint, not an API root.
                "api_base_url":format!("{}/v1/responses", server.uri()), "max_output_tokens":128,
                "context_window_tokens":4096, "timeout_secs":5, "supports_tool_calling":true,
                "metadata":{"model_family":"openai"},
            })).unwrap();
            let mut config = LLMRouterConfig::default();
            config.default_profile = "actor-fixture".into();
            config
                .profiles
                .insert("actor-fixture".into(), profile.clone());
            let mut wrong = profile;
            wrong.model = "gpt-6-luna".into();
            config.profiles.insert("other-profile".into(), wrong);
            let base = OperationLlmRouter::new(Some(config));
            let queue = magicllm::LlmDispatchQueue::start(
                base.live_dispatch_router(),
                Arc::new(magicllm::NoopTaskStateView),
                Arc::new(magicllm::NoopTaskLedgerSink),
                magicllm::DispatchConfig {
                    workers: 1,
                    local_prep: magicllm::LocalPrepConfig {
                        enabled: false,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            base.set_dispatch_queue(queue.clone());
            let route = base.bounded_route("actor-fixture").unwrap();
            let maximum =
                route.maximum_microusd() * u64::from(funded_calls.unwrap_or(calls)) + tool_cost;
            let contract = DefinitionOfDone {
                goal: "perform bounded local work".into(),
                artifact_kind: "text".into(),
                actor: route.model_pin(),
                criteria: vec![Criterion {
                    id: "complete".into(),
                    kind: CriterionKind::Check,
                    specification_hash: digest(&"check").unwrap(),
                    weight: None,
                    must_pass: true,
                    pass_score: None,
                }],
                critics: vec![CriticPin {
                    id: "check".into(),
                    criteria: vec!["complete".into()],
                    model: None,
                    configuration_hash: digest(&"reviewer").unwrap(),
                }],
                sources: vec![],
                collected_source_limits: None,
                score_at_least: None,
                review: ReviewPolicy {
                    panel_size: 1,
                    score_quorum: 1,
                    min_confidence: 0.8,
                    max_score_spread: 0.2,
                    noise_margin: 0.02,
                    max_retries_per_critic: 0,
                    unknown_review: UnknownReviewPolicy::Pause,
                },
                budget: Budget {
                    max_attempts: 2,
                    max_wall_clock_secs: 600,
                    max_cost_microusd: maximum,
                    plateau_attempts: 2,
                },
            }
            .try_into()
            .unwrap();
            let temp = tempfile::tempdir().unwrap();
            let store = Arc::new(RunStore::new(
                temp.path(),
                RunScope {
                    principal: "owner".into(),
                    workspace: "work".into(),
                },
            ));
            let run_id = Uuid::new_v4();
            let source_hash = store.put_blob(run_id, b"frozen harness setup").unwrap();
            let now = Utc::now();
            store
                .admit(
                    run_id,
                    contract,
                    Admission {
                        owner_principal: "owner".into(),
                        source_id: "fixture".into(),
                        source_hash,
                        task_id: "task-fixture".into(),
                        routing_policy: None,
                        handoff_kind: Default::default(),
                        not_after: None,
                    },
                    now,
                )
                .unwrap();
            let state = store.claim(run_id, "worker".into(), now).unwrap();
            let parent = OperationIntent {
                id: "actor".into(),
                input_hash: digest(&"actor envelope").unwrap(),
                class: OperationClass::Actor,
                maximum_cost_microusd: maximum,
            };
            store
                .commit(
                    run_id,
                    state.generation,
                    RunEvent::AttemptStarted {
                        actor: parent.clone(),
                        review: Allowance {
                            id: "review".into(),
                            input_hash: digest(&"review").unwrap(),
                            maximum_microusd: 0,
                            remaining_microusd: 0,
                            closed: false,
                        },
                    },
                    now,
                )
                .unwrap();
            let owner = HarnessResources::begin(
                store,
                run_id,
                "worker".into(),
                parent,
                route,
                HarnessResourceBinding {
                    task_id: "task-fixture".into(),
                    root_execution_id: "root-fixture".into(),
                    execution_id: "exec-fixture".into(),
                    maximum_physical_calls: calls,
                    maximum_tool_calls: tool_calls,
                    maximum_tool_cost_microusd: tool_cost,
                    configuration_hash: digest(&"frozen native harness and tool ceiling").unwrap(),
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
            let mut task = TaskRef::task("task-fixture");
            task.scope = Some(magicllm::LlmScope::new("owner", "work"));
            task.root_execution_id = Some("root-fixture".into());
            task.execution_id = Some("exec-fixture".into());
            let router = Arc::new(owner.router().unwrap().with_task_context(Some(task)));
            Self {
                _temp: temp,
                server,
                owner,
                router,
                queue,
            }
        }

        async fn call(
            &self,
            model: Option<&str>,
        ) -> Result<super::super::query_analysis::operation_llm_router::ExecutionNativeRouterResponse>
        {
            self.router
                .generate_for_execution_native_tools(
                    &LLMOperation::Other("fixture-native-step".into()),
                    Some("perform bounded work"),
                    "produce a candidate",
                    vec![],
                    model,
                    None,
                    None,
                    None,
                )
                .await
        }
    }

    #[tokio::test]
    async fn harness_resources_close_joins_a_live_call_before_sealing_and_refuses_new_dispatch() {
        let f = Fixture::with_delay(2, true, std::time::Duration::from_secs(1)).await;
        let router = f.router.clone();
        let call = tokio::spawn(async move {
            router
                .generate_for_execution_native_tools(
                    &LLMOperation::Other("fixture-live-call".into()),
                    Some("bounded work"),
                    "produce a candidate",
                    vec![],
                    None,
                    None,
                    None,
                    None,
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while f.server.received_requests().await.unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(f.owner.calls.state.lock().unwrap().active, 1);
        let receipt = f.owner.receipt().await.unwrap();
        assert!(f
            .owner
            .save_result(&json!({"candidate":"premature"}), receipt)
            .await
            .is_err());
        assert!(f
            .owner
            .save_checkpoint("premature-question".into(), b"premature pause".to_vec())
            .await
            .is_err());
        let owner = f.owner.clone();
        let drain = tokio::spawn(async move {
            owner.close_and_drain().await;
        });
        tokio::task::yield_now().await;
        assert!(!drain.is_finished());
        call.await.unwrap().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), drain)
            .await
            .unwrap()
            .unwrap();
        let receipt = f.owner.receipt().await.unwrap();
        assert!(receipt.actual_cost_microusd.unwrap() > 0);
        f.owner
            .save_result(&json!({"candidate":"settled"}), receipt)
            .await
            .unwrap();
        assert!(f.call(None).await.is_err());
        assert_eq!(f.server.received_requests().await.unwrap().len(), 1);
        assert_eq!(
            f.owner
                .store
                .adapter_budget(f.owner.run_id, &f.owner.parent)
                .unwrap()
                .physical_starts
                .len(),
            1
        );
        f.queue.shutdown(std::time::Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn harness_resources_native_calls_share_one_parent_and_stop_before_an_extra_request() {
        let f = Fixture::new(2, true).await;
        assert_eq!(
            f.call(None).await.unwrap().text.as_deref(),
            Some("bounded candidate")
        );
        f.call(None).await.unwrap();
        assert!(f.call(None).await.is_err());
        let requests = f.server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests
            .iter()
            .all(|request| request.url.path() == "/v1/responses"));
        assert!(requests
            .iter()
            .all(
                |request| request.body_json::<serde_json::Value>().unwrap()["model"]
                    == "gpt-6.1-sol"
            ));
        let budget = f
            .owner
            .store
            .adapter_budget(f.owner.run_id, &f.owner.parent)
            .unwrap();
        assert_eq!(budget.ledger.operations().len(), 2);
        assert_eq!(budget.physical_starts.len(), 2);
        assert_eq!(budget.ledger.spend().unwrap().reserved_microusd, 0);
        f.owner.close_and_drain().await;
        let receipt = f.owner.receipt().await.unwrap();
        assert!(receipt.actual_cost_microusd.unwrap() > 0);
        assert!(receipt.actual_cost_microusd.unwrap() < f.owner.parent.maximum_cost_microusd);
        f.owner
            .save_result(&json!({"candidate":"exact local result"}), receipt)
            .await
            .unwrap();
        assert!(matches!(
            f.owner
                .store
                .lookup_adapter_job(f.owner.run_id, &f.owner.parent)
                .unwrap(),
            magician_relentless::store::AdapterJobLookup::Completed(_)
        ));
        f.queue.shutdown(std::time::Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn harness_resources_refuse_unfunded_or_overquoted_parent_before_any_dispatch() {
        let f = Fixture::new(2, true).await;
        for maximum in [
            f.owner.route.maximum_microusd() - 1,
            f.owner.parent.maximum_cost_microusd + 1,
        ] {
            let mut parent = f.owner.parent.clone();
            parent.maximum_cost_microusd = maximum;
            assert!(HarnessResources::construct(
                f.owner.store.clone(),
                f.owner.run_id,
                f.owner.worker_id.clone(),
                parent,
                f.owner.route.clone(),
                f.owner.binding.clone(),
                CancellationToken::new()
            )
            .is_err());
        }
        assert_eq!(f.server.received_requests().await.unwrap().len(), 0);
        f.queue.shutdown(std::time::Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn harness_resources_500_calls_use_settled_dollars_and_stop_at_cost_before_dispatch() {
        let f = Fixture::with_parent_cap(500, true, std::time::Duration::ZERO, 0, 0, Some(2)).await;
        let mut completed = 0;
        for _ in 0..=500 {
            if f.call(None).await.is_err() {
                break;
            }
            completed += 1;
        }
        assert!(
            completed > 12 && completed < 500,
            "completed {completed} calls"
        );
        assert_eq!(f.server.received_requests().await.unwrap().len(), completed);
        let budget = f
            .owner
            .store
            .adapter_budget(f.owner.run_id, &f.owner.parent)
            .unwrap();
        assert_eq!(budget.physical_starts.len(), completed);
        let spend = budget.ledger.spend().unwrap();
        assert!(spend.settled_microusd > 0);
        assert!(spend.settled_microusd <= f.owner.parent.maximum_cost_microusd);
        assert_eq!(spend.reserved_microusd, 0);
        f.owner.close_and_drain().await;
        assert!(f.owner.known_call_allowance_exhausted().await.unwrap());
        assert_eq!(
            f.owner.receipt().await.unwrap().actual_cost_microusd,
            Some(spend.settled_microusd)
        );
        f.queue.shutdown(std::time::Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn harness_resources_capped_unknown_call_retains_funded_parent_after_restart() {
        let f =
            Fixture::with_parent_cap(500, false, std::time::Duration::ZERO, 0, 0, Some(2)).await;
        assert!(f.call(None).await.is_err());
        assert_eq!(f.server.received_requests().await.unwrap().len(), 1);
        assert_eq!(f.owner.receipt().await.unwrap().actual_cost_microusd, None);
        assert!(!f.owner.known_call_allowance_exhausted().await.unwrap());
        let restarted = RunStore::new(
            f._temp.path(),
            RunScope {
                principal: "owner".into(),
                workspace: "work".into(),
            },
        );
        let budget = restarted
            .adapter_budget(f.owner.run_id, &f.owner.parent)
            .unwrap();
        assert_eq!(budget.actual_cost_microusd().unwrap(), None);
        assert_eq!(
            budget.ledger.spend().unwrap().reserved_microusd,
            f.owner.route.maximum_microusd()
        );
        assert_eq!(
            restarted
                .load(f.owner.run_id)
                .unwrap()
                .budget
                .spend()
                .unwrap()
                .reserved_microusd,
            f.owner.parent.maximum_cost_microusd,
            "an unknown child retains the whole funded controller parent"
        );
        assert!(f.call(None).await.is_err());
        assert_eq!(f.server.received_requests().await.unwrap().len(), 1);
        f.queue.shutdown(std::time::Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn harness_resources_missing_usage_retains_the_parent_ceiling_after_restart() {
        let f = Fixture::new(1, false).await;
        let error = f.call(None).await.unwrap_err();
        assert!(
            error.to_string().contains("usage"),
            "unexpected error: {error:#}"
        );
        let requests = f.server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url.path(), "/v1/responses");
        assert_eq!(f.owner.receipt().await.unwrap().actual_cost_microusd, None);
        let restarted = RunStore::new(
            f._temp.path(),
            RunScope {
                principal: "owner".into(),
                workspace: "work".into(),
            },
        );
        let budget = restarted
            .adapter_budget(f.owner.run_id, &f.owner.parent)
            .unwrap();
        assert_eq!(budget.actual_cost_microusd().unwrap(), None);
        assert_eq!(
            budget.ledger.spend().unwrap().reserved_microusd,
            f.owner.parent.maximum_cost_microusd
        );
        assert!(f.call(None).await.is_err());
        assert_eq!(f.server.received_requests().await.unwrap().len(), 1);
        f.queue.shutdown(std::time::Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn harness_resources_invalid_usage_blocks_an_affordable_second_dispatch() {
        let f = Fixture::with_parent_cap(500, true, std::time::Duration::ZERO, 0, 0, Some(2)).await;
        f.server.reset().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id":"resp_invalid_usage", "model":"gpt-6.1-sol", "status":"completed",
                "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"bounded candidate"}]}],
                "usage":{"input_tokens":5000,"output_tokens":5,"total_tokens":5005}
            })))
            .mount(&f.server)
            .await;
        assert!(f.call(None).await.is_err());
        assert_eq!(f.server.received_requests().await.unwrap().len(), 1);
        assert_eq!(f.owner.receipt().await.unwrap().actual_cost_microusd, None);
        let reopened = RunStore::new(
            f._temp.path(),
            f.owner.store.load(f.owner.run_id).unwrap().scope,
        );
        let budget = reopened
            .adapter_budget(f.owner.run_id, &f.owner.parent)
            .unwrap();
        assert!(budget
            .ledger
            .operations()
            .values()
            .any(|operation| operation.settlement
                == magician_relentless::budget::Settlement::OutcomeUnknown));
        assert_eq!(
            budget.ledger.spend().unwrap().reserved_microusd,
            f.owner.route.maximum_microusd()
        );
        assert!(f.call(None).await.is_err());
        assert_eq!(f.server.received_requests().await.unwrap().len(), 1);
        f.queue.shutdown(std::time::Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn harness_resources_pin_survives_agent_overlays_and_refuses_model_and_chain_drift() {
        let mut f = Fixture::new(2, true).await;
        let mut overrides = OperationRoutingOverrides::default();
        overrides.operations.insert(
            "fixture-native-step".into(),
            OperationRoutingEndpoint::for_profile("other-profile").unwrap(),
        );
        f.router = Arc::new(f.router.with_routing_overrides(Some(overrides)));
        f.call(None).await.unwrap();
        assert!(f.call(Some("gpt-6-luna")).await.is_err());
        assert!(f
            .router
            .generate_for_execution_native_messages_with_chain(
                &LLMOperation::Other("fixture-native-step".into()),
                vec![magicllm::LLMMessage {
                    role: magicllm::types::MessageRole::User,
                    content: vec![magicllm::ContentBlock::text("continued input")],
                }],
                vec![],
                None,
                None,
                Some("resp_fixture"),
                None,
            )
            .await
            .is_err());
        assert_eq!(f.server.received_requests().await.unwrap().len(), 1);
        assert_eq!(
            f.owner
                .store
                .adapter_budget(f.owner.run_id, &f.owner.parent)
                .unwrap()
                .ledger
                .operations()
                .len(),
            1
        );
        f.queue.shutdown(std::time::Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn harness_resources_resume_the_paid_ledger_without_refilling_calls_or_recipe() {
        let mut f = Fixture::new(2, true).await;
        f.call(None).await.unwrap();
        let checkpoint_bytes = b"sealed native pause and exact work-area binding";
        assert!(f
            .owner
            .save_checkpoint("material-fact".into(), checkpoint_bytes.to_vec())
            .await
            .is_err());
        f.owner.close_and_drain().await;
        let checkpoint = f
            .owner
            .save_checkpoint("material-fact".into(), checkpoint_bytes.to_vec())
            .await
            .unwrap();
        let state = f.owner.store.load(f.owner.run_id).unwrap();
        f.owner
            .store
            .commit(
                f.owner.run_id,
                state.generation,
                RunEvent::ActorCheckpointed {
                    checkpoint: checkpoint.clone(),
                    reason: "missing material owner fact".into(),
                },
                Utc::now(),
            )
            .unwrap();
        f.owner
            .store
            .commit(
                f.owner.run_id,
                state.generation,
                RunEvent::LeaseReleased,
                Utc::now(),
            )
            .unwrap();
        let answer = f
            .owner
            .store
            .put_blob(
                f.owner.run_id,
                b"Question: required fact? Answer: owner fact.",
            )
            .unwrap();
        let answered = f
            .owner
            .store
            .commit(
                f.owner.run_id,
                state.generation,
                RunEvent::OwnerInputRecorded {
                    attention_id: "material-fact".into(),
                    response_hash: answer,
                },
                Utc::now(),
            )
            .unwrap();
        let store = Arc::new(RunStore::new(f._temp.path(), answered.scope));
        store
            .claim(f.owner.run_id, "replacement-worker".into(), Utc::now())
            .unwrap();
        let before = serde_json::to_value(
            store
                .adapter_budget(f.owner.run_id, &f.owner.parent)
                .unwrap(),
        )
        .unwrap();
        let mut wrong = f.owner.binding.clone();
        wrong.configuration_hash = digest(&"a different actor recipe").unwrap();
        assert!(HarnessResources::resume(
            store.clone(),
            f.owner.run_id,
            "replacement-worker".into(),
            f.owner.parent.clone(),
            f.owner.route.clone(),
            wrong,
            checkpoint.clone(),
            CancellationToken::new()
        )
        .await
        .is_err());
        let resumed = HarnessResources::resume(
            store.clone(),
            f.owner.run_id,
            "replacement-worker".into(),
            f.owner.parent.clone(),
            f.owner.route.clone(),
            f.owner.binding.clone(),
            checkpoint.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::to_value(
                store
                    .adapter_budget(f.owner.run_id, &f.owner.parent)
                    .unwrap()
            )
            .unwrap(),
            before
        );
        assert!(HarnessResources::resume(
            store.clone(),
            f.owner.run_id,
            "replacement-worker".into(),
            f.owner.parent.clone(),
            f.owner.route.clone(),
            f.owner.binding.clone(),
            checkpoint,
            CancellationToken::new()
        )
        .await
        .is_err());
        let mut task = TaskRef::task("task-fixture");
        task.scope = Some(magicllm::LlmScope::new("owner", "work"));
        task.root_execution_id = Some("root-fixture".into());
        task.execution_id = Some("exec-fixture".into());
        f.router = Arc::new(resumed.router().unwrap().with_task_context(Some(task)));
        f.call(None).await.unwrap();
        assert!(f.call(None).await.is_err());
        assert_eq!(f.server.received_requests().await.unwrap().len(), 2);
        let budget = store
            .adapter_budget(f.owner.run_id, &f.owner.parent)
            .unwrap();
        assert_eq!(budget.maximum_operations, 2);
        assert_eq!(budget.physical_starts.len(), 2);
        assert!(budget.actual_cost_microusd().unwrap().unwrap() > 0);
        resumed.close_and_drain().await;
        let receipt = resumed.receipt().await.unwrap();
        resumed
            .save_result(&json!({"candidate":"after authenticated answer"}), receipt)
            .await
            .unwrap();
        assert_eq!(store.load(f.owner.run_id).unwrap().attempts.len(), 1);
        f.queue.shutdown(std::time::Duration::from_secs(2)).await;
    }
}

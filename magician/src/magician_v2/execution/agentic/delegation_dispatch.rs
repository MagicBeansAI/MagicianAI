//! Delegation dispatcher trait and supporting types for cross-agent delegation.
//!
//! The executor uses `DelegationDispatcher` to discover available delegation targets
//! and to spawn isolated delegated child executions. This trait abstracts the runtime
//! from the executor, maintaining separation of concerns.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{fmt::Debug, sync::Arc, time::Instant};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::magician_v2::agents::InvocationSurface;
use crate::magician_v2::execution::actions::DelegationTargetRequest;

/// Immutable composition needed to re-enter one delegated child after a
/// stateless `SleepUntil` boundary.
///
/// The exact queue key already carries scope/task/execution/generation.  This
/// record carries the composition that cannot be reconstructed from the task
/// root: the child goal, policy revisions, inherited routing/work authority,
/// and whether the app-owned lifecycle must be reopened.  Artifact V2 stores a
/// scope-keyed integrity-sealed copy before the child's first model call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DelegatedChildRecoveryBinding {
    pub schema_version: u8,
    pub principal: String,
    pub workspace: String,
    pub task_id: String,
    pub execution_id: String,
    pub parent_execution_id: String,
    pub root_execution_id: String,
    pub source_agent_id: String,
    pub source_definition_digest: String,
    pub target_agent_id: String,
    pub target_definition_digest: String,
    /// Ordinary delegation stores the exact LLM-facing goal. Governed app
    /// children keep protected input out of this generic sidecar and rebuild
    /// it from their attached lifecycle instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinary_execution_goal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinary_child_title: Option<String>,
    /// Ordinary-only output contract. A governed app child reconstructs its
    /// result declaration from the protected attached lifecycle, never from
    /// this generic execution sidecar.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ordinary_expected_artifacts:
        Vec<crate::magician_v2::execution::actions::DelegationExpectedArtifact>,
    /// Ordinary delegation spend-authority locators. Governed app children do
    /// not accept caller/model spend tokens and persist an empty list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ordinary_spend_token_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_budget_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_authority: Option<crate::magician_v2::work_context::WorkAuthorityRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_routing_overrides:
        Option<crate::magician_v2::query_analysis::operation_llm_router::OperationRoutingOverrides>,
    pub app_agent_tool: bool,
}

pub const DELEGATED_CHILD_RECOVERY_BINDING_SCHEMA_VERSION: u8 = 1;

// ============================================================================
// Delegation Target Descriptors
// ============================================================================

/// Summary of an agent that the current agent can delegate to.
///
/// Since goals are being removed from agents, this struct exposes
/// tools instead so the LLM knows what the target can do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationTarget {
    pub agent_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub description: String,
    /// What tools this agent has (so LLM knows what it can do).
    pub tools: Vec<String>,
    /// Exact typed owner-transition surfaces admitted by the target's current
    /// scoped definition. Provider schemas project this instead of assuming a
    /// delegation relationship also permits handover.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_invocation_surfaces: Vec<InvocationSurface>,
}

impl DelegationTarget {
    pub fn permits_surface(&self, surface: InvocationSurface) -> bool {
        self.allowed_invocation_surfaces.contains(&surface)
    }
}

// NOTE: DelegationTargetGoal has been removed — goals are no longer part of
// agent definitions. Use tools on DelegationTarget instead.

/// Summary of one spawned delegated child execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpawnedDelegationChild {
    pub execution_id: String,
    pub target_agent_id: String,
}

/// Result of a delegation spawn request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DelegationSpawnResult {
    pub child_executions: Vec<SpawnedDelegationChild>,
    /// Existing completed children that satisfied this admission request.
    ///
    /// These ids are results to consume, not executions to wait on. Keeping
    /// them separate prevents an idempotent retry from parking the parent in
    /// `WaitingChildren` with no live child left to wake it.
    pub completed_reused_execution_ids: Vec<String>,
    /// Time spent queued behind another admission for this same parent. Work
    /// from unrelated parents does not share this lock.
    #[serde(default)]
    pub admission_queue_ms: u64,
    /// Time spent validating the delegation batch before any child shell was
    /// committed. These diagnostics are returned to the caller so the durable
    /// launch event can explain admission latency without scraping logs.
    #[serde(default)]
    pub admission_preflight_ms: u64,
    /// Cumulative time from dispatch entry until child shells and their parent
    /// links were durable.
    #[serde(default)]
    pub child_shells_ready_ms: u64,
    /// Longest local-resource admission wait among children in this batch.
    #[serde(default)]
    pub resource_wait_ms: u64,
    /// Total synchronous dispatch time before all child jobs were launched.
    #[serde(default)]
    pub dispatch_total_ms: u64,
}

// ============================================================================
// Dispatch Error
// ============================================================================

#[derive(Debug, Error)]
pub enum DispatchError {
    #[error("delegation service unavailable")]
    ServiceUnavailable,
    #[error("dispatch failed: {0}")]
    DispatchFailed(String),
    #[error("spawn interrupted: {0}")]
    SpawnInterrupted(String),
    #[error("runtime error: {0}")]
    Runtime(String),
}

// ============================================================================
// Dispatcher Trait
// ============================================================================

/// Abstraction over the runtime's delegation machinery.
///
/// The executor calls `available_targets()` to populate prompt context, and
/// `spawn_children()` to start isolated delegated child executions.
///
/// ## Delegation Model (Unilateral)
///
/// `available_targets()` only checks the source agent's `delegate_to` list.
/// The previous bilateral consent model (`can_receive_from` on the target) has
/// been removed. Target agent definitions are still loaded to build
/// [`DelegationTarget`] structs with metadata (name, description, tools).
#[async_trait]
pub trait DelegationDispatcher: Send + Sync + Debug {
    /// List agents that `source_agent_id` is allowed to delegate to.
    ///
    /// Uses the **unilateral model**: only the source agent's `delegate_to` list
    /// is consulted. Target agent definitions are loaded for metadata but no
    /// `can_receive_from` check is performed on the target.
    async fn available_targets(
        &self,
        source_agent_id: &str,
        principal: Option<&str>,
        workspace: Option<&str>,
    ) -> Vec<DelegationTarget>;

    /// Spawn one delegated child execution per target and return immediately.
    ///
    /// When `cancel` is `Some`, the spawn operation exits early with
    /// `DispatchError::SpawnInterrupted` if the token is cancelled.
    async fn spawn_children(
        &self,
        source_agent_id: &str,
        source_execution_id: &str,
        source_chain_id: Option<&str>,
        targets: Vec<DelegationTargetRequest>,
        cancel: Option<CancellationToken>,
    ) -> Result<DelegationSpawnResult, DispatchError>;

    /// Spawn or adopt the one ordinary delegated child whose deterministic
    /// identity was sealed before an accepted launch was acknowledged.
    ///
    /// The expected identity is part of admission, not a result assertion:
    /// implementations must carry it through the parent-scoped serialization
    /// boundary and must never replace a terminal exact child with a retry id.
    /// Dispatchers that cannot provide this fencing fail closed.
    async fn spawn_exact_child(
        &self,
        _source_agent_id: &str,
        _source_execution_id: &str,
        _source_chain_id: Option<&str>,
        _target: DelegationTargetRequest,
        _expected_child_execution_id: &str,
        _cancel: Option<CancellationToken>,
    ) -> Result<DelegationSpawnResult, DispatchError> {
        Err(DispatchError::ServiceUnavailable)
    }

    /// Admit one install/task-sealed `agent_as_tool` child through the same
    /// canonical delegation runtime. The move-only launch intent has already
    /// been persisted before this call; implementations must create/link and
    /// attach the exact deterministic child, transition the intent to Attached,
    /// and only then schedule it. Ordinary dispatchers fail closed.
    async fn spawn_agent_tool_child(
        &self,
        _source_agent_id: &str,
        _source_execution_id: &str,
        _source_chain_id: Option<&str>,
        _target: DelegationTargetRequest,
        _launch: crate::magician_v2::artifact_v2::app_agent_tool::AppAgentToolReservedLaunch,
        _cancel: Option<CancellationToken>,
    ) -> Result<DelegationSpawnResult, DispatchError> {
        Err(DispatchError::ServiceUnavailable)
    }

    /// Recompose and execute the exact child named by an integrity-verified
    /// Artifact binding. Implementations must still revalidate current source,
    /// target, work and app authority before the model boundary.
    async fn recover_sleeping_child(
        &self,
        _binding: DelegatedChildRecoveryBinding,
        _stateless_source_segment: Option<String>,
        _execution_retry_due_at: chrono::DateTime<chrono::Utc>,
        _execution_retry_claimed_until: chrono::DateTime<chrono::Utc>,
    ) -> Result<crate::magician_v2::execution::AgenticOutcome, DispatchError> {
        Err(DispatchError::ServiceUnavailable)
    }

    /// Recompose an ordinary delegated child whose durable runtime shell was
    /// published but whose scheduled execution job was interrupted before it
    /// reached a resumable timer or terminal outcome. The integrity-verified
    /// binding is the sole source of child composition; implementations must
    /// not fall back to the task-root manifest or a caller-supplied goal.
    async fn recover_interrupted_child(
        &self,
        _binding: DelegatedChildRecoveryBinding,
    ) -> Result<crate::magician_v2::execution::AgenticOutcome, DispatchError> {
        Err(DispatchError::ServiceUnavailable)
    }
}

/// Run a complete delegated-child admission from a fresh execution-runtime
/// task.
///
/// Delegation is not a small transport call: the production dispatcher enters
/// scoped policy resolution, Artifact V2 scheduling, execution persistence,
/// child attachment, resource admission, and child launch. Callers outside an
/// active agentic scheduler lane must use this wrapper rather than polling
/// [`DelegationDispatcher::spawn_children`] at the bottom of an HTTP,
/// verification, or orchestration stack.
///
/// The closure is lazy, the result remains joined, and dropping the waiter
/// aborts the admission task. Task-local authority/budget context is restored
/// explicitly because Tokio does not inherit task locals across spawned tasks.
pub async fn spawn_children_on_execution_runtime(
    dispatcher: Arc<dyn DelegationDispatcher>,
    source_agent_id: String,
    source_execution_id: String,
    source_chain_id: Option<String>,
    targets: Vec<DelegationTargetRequest>,
    cancel: Option<CancellationToken>,
    principal: Option<String>,
    workspace: Option<String>,
) -> Result<DelegationSpawnResult, DispatchError> {
    let submitted_at = Instant::now();
    let execution_token_meter =
        crate::magician_v2::execution::agentic::types::CapturedExecutionTokenMeter::current();
    let task_locals = crate::magician_v2::execution::agentic::types::CapturedRunTaskLocals::current(
        principal, workspace,
    );
    let execution_label = source_execution_id.clone();

    crate::magician_v2::execution::runtime_boundary::run_execution_job(move || async move {
        debug!(
            source_execution_id = %source_execution_id,
            scheduler_queue_ms = submitted_at.elapsed().as_millis() as u64,
            "[DELEGATION-TIMING] entered external delegation scheduler root"
        );
        execution_token_meter
            .scope(async move {
                let dispatch = dispatcher.spawn_children(
                    &source_agent_id,
                    &source_execution_id,
                    source_chain_id.as_deref(),
                    targets,
                    cancel,
                );
                task_locals.scope(dispatch).await
            })
            .await
    })
    .await
    .map_err(|error| {
        DispatchError::Runtime(format!(
            "delegation scheduler-root task failed for '{execution_label}': {error}"
        ))
    })?
}

/// Run one sealed exact-child admission on the execution runtime.
///
/// This mirrors [`spawn_children_on_execution_runtime`] while keeping the
/// accepted child identity in the dispatch contract all the way to the
/// implementation's parent admission lock.
pub async fn spawn_exact_child_on_execution_runtime(
    dispatcher: Arc<dyn DelegationDispatcher>,
    source_agent_id: String,
    source_execution_id: String,
    source_chain_id: Option<String>,
    target: DelegationTargetRequest,
    expected_child_execution_id: String,
    cancel: Option<CancellationToken>,
    principal: Option<String>,
    workspace: Option<String>,
) -> Result<DelegationSpawnResult, DispatchError> {
    let submitted_at = Instant::now();
    let execution_token_meter =
        crate::magician_v2::execution::agentic::types::CapturedExecutionTokenMeter::current();
    let task_locals = crate::magician_v2::execution::agentic::types::CapturedRunTaskLocals::current(
        principal, workspace,
    );
    let execution_label = source_execution_id.clone();

    crate::magician_v2::execution::runtime_boundary::run_execution_job(move || async move {
        debug!(
            source_execution_id = %source_execution_id,
            scheduler_queue_ms = submitted_at.elapsed().as_millis() as u64,
            "[DELEGATION-TIMING] entered exact external delegation scheduler root"
        );
        execution_token_meter
            .scope(async move {
                let dispatch = dispatcher.spawn_exact_child(
                    &source_agent_id,
                    &source_execution_id,
                    source_chain_id.as_deref(),
                    target,
                    &expected_child_execution_id,
                    cancel,
                );
                task_locals.scope(dispatch).await
            })
            .await
    })
    .await
    .map_err(|error| {
        DispatchError::Runtime(format!(
            "exact delegation scheduler-root task failed for '{execution_label}': {error}"
        ))
    })?
}

/// Startup-adopt one already-durable callable-agent launch on the ordinary
/// execution runtime. The sealed reservation is move-only and the dispatcher
/// still owns canonical child creation/linking/scheduling.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn spawn_agent_tool_child_on_execution_runtime(
    dispatcher: Arc<dyn DelegationDispatcher>,
    source_agent_id: String,
    source_execution_id: String,
    source_chain_id: Option<String>,
    target: DelegationTargetRequest,
    launch: crate::magician_v2::artifact_v2::app_agent_tool::AppAgentToolReservedLaunch,
    principal: String,
    workspace: String,
) -> Result<DelegationSpawnResult, DispatchError> {
    let execution_label = source_execution_id.clone();
    let execution_token_meter =
        crate::magician_v2::execution::agentic::types::CapturedExecutionTokenMeter::current();
    let task_locals = crate::magician_v2::execution::agentic::types::CapturedRunTaskLocals::current(
        Some(principal),
        Some(workspace),
    );
    crate::magician_v2::execution::runtime_boundary::run_execution_job(move || async move {
        execution_token_meter
            .scope(async move {
                let dispatch = dispatcher.spawn_agent_tool_child(
                    &source_agent_id,
                    &source_execution_id,
                    source_chain_id.as_deref(),
                    target,
                    launch,
                    None,
                );
                task_locals.scope(dispatch).await
            })
            .await
    })
    .await
    .map_err(|error| {
        DispatchError::Runtime(format!(
            "agent_as_tool recovery task failed for '{execution_label}': {error}"
        ))
    })?
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    };

    use super::*;

    #[test]
    fn governed_app_recovery_binding_serialization_omits_protected_payload() {
        let protected = "DO-NOT-PERSIST-app-secret-input";
        let binding = DelegatedChildRecoveryBinding {
            schema_version: DELEGATED_CHILD_RECOVERY_BINDING_SCHEMA_VERSION,
            principal: "principal".to_owned(),
            workspace: "workspace".to_owned(),
            task_id: "task".to_owned(),
            execution_id: "child".to_owned(),
            parent_execution_id: "parent".to_owned(),
            root_execution_id: "root".to_owned(),
            source_agent_id: "source".to_owned(),
            source_definition_digest: "source-digest".to_owned(),
            target_agent_id: "target".to_owned(),
            target_definition_digest: "target-digest".to_owned(),
            ordinary_execution_goal: None,
            ordinary_child_title: None,
            ordinary_expected_artifacts: Vec::new(),
            ordinary_spend_token_ids: Vec::new(),
            work_budget_secs: Some(60),
            work_authority: None,
            llm_routing_overrides: None,
            app_agent_tool: true,
        };
        let encoded = serde_json::to_string(&binding).expect("serialize app recovery binding");
        assert!(!encoded.contains(protected));
        assert!(!encoded.contains("context"));
        assert!(!encoded.contains("input_data"));
        assert!(!encoded.contains("ordinary_execution_goal"));
        assert!(!encoded.contains("ordinary_child_title"));
    }

    #[derive(Debug)]
    struct RecordingDispatcher {
        task_id: Arc<Mutex<Option<String>>>,
        token_budget: Arc<Mutex<Option<(u64, u64)>>>,
        started: Arc<AtomicBool>,
        dropped: Arc<AtomicBool>,
        coding_context_active: Arc<AtomicBool>,
        parent_engine: Arc<Mutex<Option<String>>>,
        launching_pin: Arc<Mutex<Option<crate::magician_v2::execution::plane::RunEnginePin>>>,
        block: bool,
    }

    #[async_trait]
    impl DelegationDispatcher for RecordingDispatcher {
        async fn available_targets(
            &self,
            _source_agent_id: &str,
            _principal: Option<&str>,
            _workspace: Option<&str>,
        ) -> Vec<DelegationTarget> {
            Vec::new()
        }

        async fn spawn_children(
            &self,
            _source_agent_id: &str,
            _source_execution_id: &str,
            _source_chain_id: Option<&str>,
            _targets: Vec<DelegationTargetRequest>,
            _cancel: Option<CancellationToken>,
        ) -> Result<DelegationSpawnResult, DispatchError> {
            struct DropMarker(Arc<AtomicBool>);
            impl Drop for DropMarker {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::SeqCst);
                }
            }

            *self.task_id.lock().expect("task id lock") =
                tokio::task::try_id().map(|id| format!("{id:?}"));
            *self.token_budget.lock().expect("token budget lock") =
                crate::magician_v2::execution::agentic::types::execution_token_budget_snapshot();
            self.coding_context_active.store(
                crate::magician_v2::execution::coding_engine::coding_context_active(),
                Ordering::SeqCst,
            );
            *self.parent_engine.lock().expect("parent engine lock") =
                crate::magician_v2::query_analysis::parent_engine::current_parent_engine();
            *self.launching_pin.lock().expect("launching pin lock") =
                crate::magician_v2::execution::plane::current_launching_run_engine_pin();
            let _drop_marker = DropMarker(Arc::clone(&self.dropped));
            self.started.store(true, Ordering::SeqCst);
            if self.block {
                std::future::pending::<()>().await;
            }
            Ok(DelegationSpawnResult {
                child_executions: Vec::new(),
                completed_reused_execution_ids: Vec::new(),
                admission_queue_ms: 0,
                admission_preflight_ms: 0,
                child_shells_ready_ms: 0,
                resource_wait_ms: 0,
                dispatch_total_ms: 0,
            })
        }
    }

    fn recording_dispatcher(block: bool) -> (Arc<RecordingDispatcher>, Arc<AtomicBool>) {
        let dropped = Arc::new(AtomicBool::new(false));
        (
            Arc::new(RecordingDispatcher {
                task_id: Arc::new(Mutex::new(None)),
                token_budget: Arc::new(Mutex::new(None)),
                started: Arc::new(AtomicBool::new(false)),
                dropped: Arc::clone(&dropped),
                coding_context_active: Arc::new(AtomicBool::new(false)),
                parent_engine: Arc::new(Mutex::new(None)),
                launching_pin: Arc::new(Mutex::new(None)),
                block,
            }),
            dropped,
        )
    }

    #[tokio::test]
    async fn default_stack_external_delegation_is_constructed_on_a_fresh_scheduler_task() {
        let caller_task_id = tokio::task::try_id().map(|id| format!("{id:?}"));
        let (dispatcher, _dropped) = recording_dispatcher(false);
        crate::magician_v2::execution::agentic::types::with_execution_token_meter(
            11,
            101,
            crate::magician_v2::execution::coding_engine::with_coding_context(
                true,
                crate::magician_v2::query_analysis::parent_engine::with_parent_engine(
                    Some("codex"),
                    crate::magician_v2::execution::plane::with_launching_run_engine_pin(
                        Some(test_launching_pin()),
                        spawn_children_on_execution_runtime(
                            dispatcher.clone(),
                            "source-agent".to_string(),
                            "source-execution".to_string(),
                            None,
                            Vec::new(),
                            None,
                            Some("anonymous".to_string()),
                            Some("default".to_string()),
                        ),
                    ),
                ),
            ),
        )
        .await
        .expect("delegation dispatch");

        let dispatch_task_id = dispatcher
            .task_id
            .lock()
            .expect("task id lock")
            .clone()
            .expect("dispatcher task id");
        assert_ne!(
            caller_task_id.as_deref(),
            Some(dispatch_task_id.as_str()),
            "the external wrapper must reset construction and poll ancestry"
        );
        assert_eq!(
            *dispatcher.token_budget.lock().expect("token budget lock"),
            Some((11, 101)),
            "the exact execution token ledger must cross the task boundary"
        );
        assert!(
            dispatcher.coding_context_active.load(Ordering::SeqCst),
            "coding isolation must cross the task boundary"
        );
        assert_eq!(
            dispatcher
                .parent_engine
                .lock()
                .expect("parent engine lock")
                .as_deref(),
            Some("codex"),
            "the flow's parent engine must cross the task boundary"
        );
        assert_eq!(
            *dispatcher.launching_pin.lock().expect("launching pin lock"),
            Some(test_launching_pin()),
            "the parent run's engine pin must cross the task boundary, or a \
             delegated child would take the Settings engine"
        );
    }

    fn test_launching_pin() -> crate::magician_v2::execution::plane::RunEnginePin {
        crate::magician_v2::execution::plane::RunEnginePin {
            engine: "codex".to_string(),
            harness_model: "gpt-parent".to_string(),
            pi_profile: None,
        }
    }

    #[tokio::test]
    async fn default_stack_dropping_external_delegation_waiter_aborts_dispatch() {
        let (dispatcher, dropped) = recording_dispatcher(true);
        let started = Arc::clone(&dispatcher.started);
        let waiter = tokio::spawn(spawn_children_on_execution_runtime(
            dispatcher,
            "source-agent".to_string(),
            "source-execution".to_string(),
            None,
            Vec::new(),
            None,
            Some("anonymous".to_string()),
            Some("default".to_string()),
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !started.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dispatcher should start");

        waiter.abort();
        let _ = waiter.await;
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dispatch future should be dropped with its waiter");
    }
}

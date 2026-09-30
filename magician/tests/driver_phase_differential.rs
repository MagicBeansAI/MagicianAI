//! The flip gate's phase differential: the same scripted run, driven twice.
//!
//! `docs/archive/plans/2026-08-25-stateless-loop-design.md`, *Testing*, asks for a
//! "phase differential + shadow-run" as the gate for flipping
//! `MAGICIAN_EXECUTION_DRIVER` to `stateless`. This binary is the first half.
//! The second half is **declined** in that document rather than stubbed here —
//! see *Shadow-run isolation* for why, and do not read this file as covering it.
//!
//! # Why this is a separate binary with exactly one test
//!
//! `ExecutionDriver::from_env` reads a **process-global** environment variable,
//! once per execution. A test that set it inside the crate's unit-test binary
//! would flip the driver under every other test running concurrently in that
//! process — the crate has dozens that drive `execute_agentically`. One binary
//! holding one test is the only arrangement in which the variable is safe under
//! `cargo test` as well as under `cargo nextest`, which gives every test its own
//! process. `engagement_authority_matrix.rs` is split from the unit tests for
//! the same class of reason and says so in its own header.
//!
//! # Why the script is THREE turns, and what each shorter one could not say
//!
//! An earlier cut of this file scripted a single `goal_reached` on turn one. It
//! could not discriminate, and it is worth saying exactly how, because the shape
//! is easy to reintroduce:
//!
//! - `iterations_used()` was `1 == 1`. Every driver that terminates at all
//!   agrees on that, including one that re-observed or re-decided the turn.
//! - The journal held one iteration, so nothing checked phase order **across**
//!   an iteration boundary — the one transition where the iteration counter
//!   steps, and where an exit lands on the epilogue instead of on the next
//!   phase.
//! - Its epilogue assertion was **provably dead**. The exact-partition assert
//!   above it already pinned `steps` to a five-element vector containing no
//!   epilogue, so the `any(..)` below it could never fire.
//!
//! Two turns makes all three load-bearing at once, because the epilogue rule has
//! two sides and a two-turn script is the shortest one that reaches both: a
//! non-final turn ends on `PhaseStep::Exit`, whose boundary **runs** the
//! epilogue, and the final turn ends on `PhaseStep::Return`, which **skips** it.
//!
//! **The third turn was added 2026-08-28 and it dispatches.** Two turns proved
//! the boundary and proved nothing about `Apply::Execute`, which is 59% of the
//! iteration body — the design doc named it as the flip gate's largest hole, and
//! it was reachable by no test under the worker driver at all. Turn two now
//! takes an `execute` decision, so a worker that mis-drove a DISPATCHING turn
//! stops being invisible to this file.
//!
//! Its dispatch fails, because no capability is registered in this harness, and
//! that is the design rather than a shortcut: the failing path still runs the
//! effect gate, the pending effect row, the tool lineage and the settle — the
//! machinery both arms must agree about — while a succeeding one would require a
//! registered pack and would test that pack as much as the driver.
//!
//! Turn one is a delegation whose child is already complete — the
//! `completed_reused` path, which continues the run in place instead of parking
//! it on `WaitingForChildren`. It was chosen because it is the one continuing
//! decision that reaches `BoundaryOutcome::NextIteration` without a registered
//! capability, a live browser session, or a real agent definition:
//! `Decision::Execute` would have to clear the trust, confirmation and allowlist
//! gates before the execute path could report `Continue`, and `spawn_sub_goal`
//! or a plain `delegate_to_agent` park the run rather than continuing it.
//!
//! # What this compares, precisely, and what it cannot
//!
//! The two drivers advance **different units**. `driver_inproc::run_iteration`
//! advances one ITERATION on the Rust stack; `driver_worker::advance_once`
//! advances one PHASE and commits it, and `StatelessArm::advance_iteration`
//! calls that in a loop until the turn boundary. So there is no call-for-call
//! comparison to make, and a test claiming one would be comparing a loop against
//! a step.
//!
//! What is comparable is the pair of things a driver is *for*:
//!
//! 1. **The terminal, and the work on the way to it.** The same two-turn script
//!    under both arms must reach the same `AgenticOutcome` in the same number of
//!    iterations, and must have performed the run's one dispatchable act exactly
//!    once under each. Those are the only two quantities both arms produce.
//! 2. **The phase partition.** The worker arm leaves a journal; the resident arm
//!    leaves nothing, because it has no store — that asymmetry IS the refactor.
//!    So the worker's own log is asserted against the partition the design
//!    claims: six phases for a turn that continues, five for the turn that ends
//!    the run, and the epilogue present in the first and absent from the second.
//!
//! What this does **not** cover, stated because an unstated gap reads as
//! coverage:
//!
//! - It does not catch a dropped `break 'iteration_body` exit. Both arms
//!   re-perform whatever a phase returned, so an exit that stopped being
//!   produced is produced by neither, and both agree. That is what the 20-site
//!   enumeration and `run_loop::outcome`'s exhaustiveness test are for.
//! - It does not compare the resident arm's phase order against anything. The
//!   resident arm keeps no journal, so its half of the partition is unobserved,
//!   and the vector at the end of this file is a **conformance** check on the
//!   worker rather than a comparison. Only the two quantities in (1) are
//!   genuinely cross-arm.
//! - It runs one process that never dies, so it says nothing about resume. The
//!   crash-point tests in `run_loop/driver_worker.rs` are that gate.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use magician::magician_v2::{
    execution::agentic::{
        execute_agentically, ActionExecutors, AgenticContext, AgenticOutcome, EnvironmentState,
        ExecutionNativeResponse, ExecutionToolCall, OwnerExecutionProfile, OwnershipRuntime,
        ShellState,
    },
    prompts::{json_storage::JsonStorageConfig, JsonPromptStorage, PromptManager},
    slot_graph::extraction::{LlmFunctionCallRequest, LlmFunctionCallResponse, LlmService},
};

// Spelled in full rather than through the `run_loop` module imported above.
// A `use` whose first segment is another `use`'s binding is legal and is also
// the shape that goes ambiguous the moment a crate of the same name appears in
// the extern prelude; the full path cannot.
use magician::magician_v2::agents::{AgentDefinition, InvocationSurface};
use magician::magician_v2::execution::actions::DelegationTargetRequest;
use magician::magician_v2::execution::agentic::delegation_dispatch::{
    DelegationDispatcher, DelegationSpawnResult, DelegationTarget, DispatchError,
    SpawnedDelegationChild,
};
use magician::magician_v2::execution::agentic::run_loop::journal::{
    JournalBody, RecordedBoundary, RecordedStep, TerminalKind,
};
use magician::magician_v2::execution::agentic::run_loop::outcome::Phase;
use magician::magician_v2::execution::agentic::run_loop::store::fs::FsLoopStateStore;
use magician::magician_v2::execution::agentic::run_loop::store::{ExecutionKey, LoopStateStore};
use magician::magician_v2::execution::agentic::run_loop::{ExecutionDriver, EXECUTION_DRIVER_ENV};

/// The scope both runs are keyed under. Distinct execution ids keep the two
/// stores apart; the scope is shared so nothing about the comparison depends on
/// which principal a run belonged to.
const PRINCIPAL: &str = "driver-differential";
const WORKSPACE: &str = "default";

/// The run's owner, and the agent it delegates one already-finished piece of
/// work to. Both arms use the same pair, so the owner the journal records is not
/// a second variable in the comparison.
const OWNER_AGENT: &str = "differential-owner";
const DELEGATE_AGENT: &str = "differential-delegate";

/// `ActionExecutors::new` requires an `LlmService`, and the modern decide loop
/// does **not** consult it — decisions arrive as native tool calls through the
/// adapter. This satisfies the constructor and asserts as much: a run that
/// reached the legacy seam took a path this differential does not model, and
/// would be comparing two things neither arm does in production.
struct UnusedLlm;

#[async_trait]
impl LlmService for UnusedLlm {
    async fn call_function(
        &self,
        _request: LlmFunctionCallRequest,
    ) -> anyhow::Result<LlmFunctionCallResponse> {
        anyhow::bail!(
            "the legacy LlmService decision seam was consulted; this differential scripts the \
             native tool-call adapter instead"
        )
    }
}

/// A delegation dispatcher whose child is **already complete**.
///
/// This is the whole reason turn one continues rather than parking.
/// `DelegationSpawnResult::completed_reused_execution_ids` names children that
/// are results to consume, not executions to wait on, so the apply arm answers
/// `ExecutePathControl::Continue` and the phase exits
/// `BoundaryOutcome::NextIteration`. A dispatcher that left that list empty
/// would park the run on `WaitingForChildren`, and this file would be comparing
/// two parks.
///
/// It counts its own calls, and that count is one of the only two quantities
/// both arms produce. A driver that re-entered `Apply` after committing it —
/// the failure a per-phase commit makes possible and that the resident loop
/// structurally cannot have — dispatches twice and is caught here.
#[derive(Debug)]
struct CompletedChildDispatcher {
    spawns: AtomicUsize,
}

impl CompletedChildDispatcher {
    fn new() -> Self {
        Self {
            spawns: AtomicUsize::new(0),
        }
    }

    fn spawns(&self) -> usize {
        self.spawns.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl DelegationDispatcher for CompletedChildDispatcher {
    async fn available_targets(
        &self,
        _source_agent_id: &str,
        _principal: Option<&str>,
        _workspace: Option<&str>,
    ) -> Vec<DelegationTarget> {
        vec![delegation_target()]
    }

    async fn spawn_children(
        &self,
        _source_agent_id: &str,
        _source_execution_id: &str,
        _source_chain_id: Option<&str>,
        targets: Vec<DelegationTargetRequest>,
        _cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<DelegationSpawnResult, DispatchError> {
        self.spawns.fetch_add(1, Ordering::SeqCst);
        let child_executions: Vec<SpawnedDelegationChild> = targets
            .iter()
            .map(|target| SpawnedDelegationChild {
                execution_id: format!("child-{}", target.target_agent_id),
                target_agent_id: target.target_agent_id.clone(),
            })
            .collect();
        let completed_reused_execution_ids = child_executions
            .iter()
            .map(|child| child.execution_id.clone())
            .collect();
        Ok(DelegationSpawnResult {
            child_executions,
            completed_reused_execution_ids,
            admission_queue_ms: 0,
            admission_preflight_ms: 0,
            child_shells_ready_ms: 0,
            resource_wait_ms: 0,
            dispatch_total_ms: 0,
        })
    }
}

/// The owner definition the run is hydrated from.
///
/// Built through `AgentDefinition::from_yaml_str` rather than a struct literal
/// so it goes through the same parse-defaults-validate path a real definition
/// file does — a fixture that skipped `validate` could assert against an owner
/// the product would refuse to load.
///
/// Everything is left at its default:
///
/// - `tools` is empty, so `derive_allowed_action_types` answers `None` — *no*
///   action-type restriction, rather than an allowlist this differential would
///   then have to keep in sync with the decisions it scripts.
/// - `invocation_policy` is the permissive default, and a depth-0 run with an
///   empty owner stack is a `Task` invocation, which `is_default_direct_surface`
///   admits. `validate_agent_invocation` runs immediately after the profile
///   loads, so a narrower policy here would fail the run before its first
///   decision.
/// - `kind` is `worker`, which is what this owner is: it holds no memory tiers
///   and runs no autonomous cycle. `apply_defaults` injects the six-tier
///   personal-agent memory config for `personal`, and none of it is exercised.
fn owner_definition() -> AgentDefinition {
    AgentDefinition::from_yaml_str(&format!(
        "agent_id: {OWNER_AGENT}\n\
         name: {OWNER_AGENT}\n\
         kind: worker\n\
         description: Scripted owner of the differential run.\n\
         persona: Runs one scripted delegation and then reports.\n"
    ))
    .expect("the differential's owner definition must parse and validate")
}

/// The minimum `OwnershipRuntime` a scoped run needs to start.
///
/// `execute_agentically_inner` resolves a full owner profile whenever owner AND
/// principal AND workspace are all present, and REFUSES with
/// `owner_profile_missing` when `executors.ownership_runtime` is absent. This
/// differential cannot drop `principal`/`workspace` — the stateless arm needs
/// both to key a run — and cannot drop the owner either, because delegation
/// fails `"Cannot delegate: No agent context"` without one. So the owner has to
/// be resolvable, and in an integration binary that means supplying the runtime
/// rather than a definition store behind it.
///
/// The profile is deliberately the narrowest one that still starts a run:
///
/// - `delegation_targets` carries the one target, because
///   `apply_owner_execution_profile` **replaces** `ctx.delegation_targets` with
///   the profile's copy. A profile that returned none would silently empty the
///   roster this test set on the context and turn one would be refused before
///   dispatch.
/// - `trust_level` is `None`, which is the honest answer for an owner with no
///   scoped policy file. It is also load-bearing:
///   `refresh_trust_dispatch_guard_for_decision` demands a readable
///   `trust_policies_path` whenever a trust level is set on a run that has both
///   an owner definition and an ownership runtime, and this fixture has no such
///   file to point at.
/// - Everything else is empty/`None`: no merged tools, no approval rules, no
///   denies, no memory tiers, no procedure skills. None of them is what this
///   differential compares, and an elaborate one would be a second variable.
#[derive(Debug)]
struct ScriptedOwnershipRuntime;

#[async_trait]
impl OwnershipRuntime for ScriptedOwnershipRuntime {
    /// Both differential arms model the same uninterrupted run. There is no
    /// shared Runtime store in this fixture and therefore no external durable
    /// cancellation to observe at phase boundaries.
    async fn execution_is_durably_cancelled(&self, _execution_id: &str) -> Result<bool, String> {
        Ok(false)
    }

    async fn active_stateless_control_generation(
        &self,
        _execution_id: &str,
    ) -> Result<Option<String>, String> {
        Ok(Some("driver-differential-generation".to_owned()))
    }

    async fn stateless_control_generation(
        &self,
        _execution_id: &str,
    ) -> Result<Option<String>, String> {
        Ok(Some("driver-differential-generation".to_owned()))
    }

    async fn load_owner_execution_profile(
        &self,
        agent_id: &str,
        principal: Option<&str>,
        workspace: Option<&str>,
    ) -> Result<OwnerExecutionProfile, String> {
        // Answering for any id would let a run that resolved the WRONG owner
        // look like a run that resolved the right one.
        if agent_id != OWNER_AGENT {
            return Err(format!(
                "this differential knows one owner ('{OWNER_AGENT}'), and was asked for \
                 '{agent_id}'"
            ));
        }
        if principal != Some(PRINCIPAL) || workspace != Some(WORKSPACE) {
            return Err(format!(
                "owner '{agent_id}' was resolved outside the differential's scope: \
                 {principal:?}/{workspace:?}"
            ));
        }
        let definition = owner_definition();
        Ok(OwnerExecutionProfile {
            agent_id: definition.agent_id.clone(),
            trust_level: None,
            trust_policies_path: None,
            preloaded_trust_enforcer: None,
            approval_rules: Vec::new(),
            llm_model_override: None,
            llm_routing_overrides: None,
            prompt_identity: None,
            delegation_targets: vec![delegation_target()],
            merged_agent_tools: Vec::new(),
            denied_capability_names: Vec::new(),
            denied_tool_params: std::collections::HashMap::new(),
            invocation_policy: definition.invocation_policy.clone(),
            allowed_action_types: None,
            max_delegation_depth: definition.constraints.coordination.max_delegation_depth,
            delegation_timeout_secs: definition.constraints.coordination.delegation_timeout_secs,
            tier_definitions: Vec::new(),
            available_procedure_skills: Vec::new(),
            definition,
        })
    }

    /// There is no conversation store behind this fixture, so there is nothing
    /// to snapshot. The run never hands over, so this is never called; it
    /// answers rather than panicking so a future turn that DOES hand over fails
    /// on the assertion it broke instead of on the fixture.
    async fn update_execution_owner_snapshot(
        &self,
        _thread_id: &str,
        _active_owner_agent_id: &str,
        _owner_stack: &[String],
    ) -> Result<(), String> {
        Ok(())
    }

    /// Not exercised: the script never hands over, and single-target in-context
    /// delegation is off by default (`ctx.delegate_single_in_context`), so the
    /// delegated turn takes the spawn-child path instead. Validated against the
    /// one pair this fixture actually knows rather than waved through, so a
    /// transition it was never built for is refused rather than granted.
    async fn validate_owner_transition(
        &self,
        source_agent_id: &str,
        target_agent_id: &str,
        _surface: InvocationSurface,
        _principal: Option<&str>,
        _workspace: Option<&str>,
    ) -> Result<(), String> {
        if source_agent_id == OWNER_AGENT && target_agent_id == DELEGATE_AGENT {
            return Ok(());
        }
        Err(format!(
            "this differential declares one transition ('{OWNER_AGENT}' -> '{DELEGATE_AGENT}'), \
             and was asked for '{source_agent_id}' -> '{target_agent_id}'"
        ))
    }
}

/// The one target both arms are allowed to delegate to.
///
/// `allowed_invocation_surfaces` carries `Delegation` because the request is
/// validated against the target's own declaration rather than against the
/// roster's mere existence — a target that permits no surface is refused before
/// dispatch, and the run would then take a different first turn under both arms.
fn delegation_target() -> DelegationTarget {
    DelegationTarget {
        agent_id: DELEGATE_AGENT.to_string(),
        name: DELEGATE_AGENT.to_string(),
        aliases: Vec::new(),
        description: "Scripted delegate whose work is already finished.".to_string(),
        tools: vec!["research".to_string()],
        allowed_invocation_surfaces: vec![InvocationSurface::Delegation],
    }
}

/// Decisions scripted as **native tool calls**, mirroring
/// `engagement_authority_matrix.rs::scripted_executors`: the decision kind
/// selects the tool name, an `execute`/`tool` decision uses its
/// `capability_name` as that name, and the remaining keys become the call
/// arguments.
///
/// The real prompt storage is used rather than a stub, so the decision prompt
/// renders exactly as it does in production — and so a prompt-rendering failure
/// cannot silently make BOTH arms fail in the same way and look like agreement.
fn scripted_executors(decisions: Vec<&str>, storage_root: &Path) -> ActionExecutors {
    let storage_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root above the magician crate")
        .join("data")
        .join("magician_v2")
        .join("prompts");
    let storage = JsonPromptStorage::new(JsonStorageConfig {
        storage_dir,
        enable_cache: true,
        max_cache_entries: 128,
    })
    .expect("real prompt storage opens");

    let native_responses = decisions
        .iter()
        .map(|raw| {
            let value: serde_json::Value =
                serde_json::from_str(raw).expect("scripted decision must be valid JSON");
            let mut arguments = value
                .as_object()
                .expect("scripted decision must be an object")
                .clone();
            let decision = arguments
                .remove("decision")
                .and_then(|value| value.as_str().map(str::to_owned))
                .expect("scripted decision kind");
            let tool_name = match decision.as_str() {
                "execute" => {
                    let action_type = arguments
                        .remove("action_type")
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .expect("scripted execute action type");
                    if action_type == "tool" {
                        arguments
                            .remove("capability_name")
                            .and_then(|value| value.as_str().map(str::to_owned))
                            .expect("scripted tool capability")
                    } else {
                        action_type
                    }
                },
                other => other.to_owned(),
            };
            arguments.insert(
                "task_state_action".to_owned(),
                serde_json::json!({
                    "action": "none",
                    "reason": "No durable task-state change is needed for this differential."
                }),
            );
            let mut response = ExecutionNativeResponse::from_tool_calls(vec![ExecutionToolCall {
                id: "driver-differential-tool-call".to_owned(),
                name: tool_name,
                arguments: serde_json::Value::Object(arguments),
            }]);
            response.finish_reason = Some("tool_calls".to_owned());
            response.telemetry = Some(
                magician::magician_v2::slot_graph::extraction::LlmCallTelemetry {
                    trace_receipt: Some(magicllm::LlmTraceReceipt::direct(
                        magicllm::LlmTraceContext::new(
                            magicllm::LlmScope::new(PRINCIPAL, WORKSPACE),
                            magicllm::LlmWorkloadClass::AutonomousTask,
                        ),
                    )),
                    ..Default::default()
                },
            );
            response
        })
        .collect();

    ActionExecutors::new(
        Arc::new(UnusedLlm),
        Arc::new(PromptManager::new(Arc::new(storage))),
    )
    .with_native_adapter(Arc::new(
        magician::magician_v2::execution::MultiLlmAgentAdapter::new_with_test_native_responses(
            native_responses,
        ),
    ))
    // Both the taskplan root and the artifact workspace, which is what
    // `StatelessArm::new` roots its `FsLoopStateStore` at. Without this the
    // worker arm writes its loop state into the process's real storage root —
    // and the assertions below would then be reading somebody else's journal.
    .with_taskplan_base_path(storage_root.to_path_buf())
}

/// What one arm produced: the terminal, and how many times the run's one
/// dispatchable act actually fired.
struct ArmResult {
    outcome: AgenticOutcome,
    spawns: usize,
    execution_id: String,
    loop_root: PathBuf,
}

/// One complete run under one driver.
///
/// The variable is set immediately before and cleared immediately after, and
/// this binary holds exactly one test so nothing else in the process can observe
/// the window. Cleared rather than left set: a leaked `stateless` would make a
/// later addition to this file silently exercise the wrong arm.
async fn run_once(driver: &str, storage_root: &Path, execution_label: &str) -> ArmResult {
    std::env::set_var(EXECUTION_DRIVER_ENV, driver);

    let dispatcher = Arc::new(CompletedChildDispatcher::new());
    // A stateless terminal is projected under the Artifact/runtime owner's
    // lifecycle exclusion. Register the same real task/execution shape in the
    // test owner for both arms so terminal settlement, rather than a
    // missing-owner shortcut, is part of the differential.
    let (artifact_service, orchestrator) =
        magician::magician_v2::test_support::build_test_artifact_v2_harness(storage_root);
    let loop_root = storage_root.join("magician_data_v3");
    let (task, artifact_execution) = artifact_service
        .create_task_with_execution_shell(magician::magician_v2::artifact_v2::CreateTaskInput {
            principal: PRINCIPAL.to_owned(),
            workspace: WORKSPACE.to_owned(),
            title: format!("Driver phase differential {execution_label}"),
            description: "Driver phase differential fixture".to_owned(),
            agent_id: OWNER_AGENT.to_owned(),
            goal_id: None,
            ui_thread_id: "driver-phase-differential".to_owned(),
            priority: None,
            due_date: None,
            tags: Vec::new(),
            created_by: "test".to_owned(),
            depends_on: Vec::new(),
            approved: true,
            schedule: None,
            output_mode: magician::magician_v2::artifact_v2::models::TaskOutputMode::Accumulate,
            chat_session_id: None,
            lifecycle: magician::magician_v2::artifact_v2::models::TaskLifecycle::default(),
            sync_mode: magician::magician_v2::artifact_v2::models::TaskSyncMode::default(),
        })
        .await
        .expect("create the differential's Artifact task and execution shell");
    let task_id = task.manifest.task_id;
    let execution_id = artifact_execution.state.execution_id;
    let scope = magician::magician_v2::artifact_v2::ScopeRef::system_internal_unauthenticated(
        PRINCIPAL, WORKSPACE,
    );
    artifact_service
        .activate_execution(&scope, &task_id, &execution_id)
        .await
        .expect("activate the differential's Artifact execution");
    if driver == ExecutionDriver::STATELESS {
        for root in [storage_root.to_path_buf(), loop_root.clone()] {
            FsLoopStateStore::new(root)
                .seal_legacy_writer_cutover(
                    PRINCIPAL,
                    WORKSPACE,
                    "driver-phase-differential",
                    chrono::Utc::now().timestamp_millis(),
                )
                .await
                .expect("activate the isolated stateless test scope");
        }
    }
    orchestrator
        .create_execution_with_id(
            PRINCIPAL,
            WORKSPACE,
            Some("Driver phase differential".to_owned()),
            OWNER_AGENT,
            &execution_id,
            Some(task_id.clone()),
            Some(execution_id.clone()),
        )
        .await
        .expect("register the differential's runtime execution");
    let executors = scripted_executors(
        vec![
            // Turn one: continue. The child is already complete, so the result
            // is consumed in place rather than parked on — the apply arm answers
            // `Continue` and the phase exits `NextIteration`.
            r#"{"decision":"delegate_to_agent","delegation_targets":[{"target_agent_id":"differential-delegate","context":"Consume the already-finished delegated result."}]}"#,
            // Turn two: DISPATCH. `Apply::Execute` is 59% of the iteration body
            // and no test reached it under the worker driver until this turn
            // existed — the design doc's own words. The capability is not
            // registered in this bare harness, so the dispatch FAILS, and that
            // is the point rather than a compromise: the failure still runs the
            // gate, the effect row, the lineage and the settle, which is the
            // machinery both arms must agree about. A turn that succeeded would
            // need a registered pack and would test the pack as much as the
            // driver.
            r#"{"decision":"execute","action_type":"tool","capability_name":"read_file","path":"differential-does-not-exist.txt"}"#,
            // Turn three: end the run.
            r#"{"decision":"goal_reached","evidence":"The delegated result was consumed and the goal is met.","artifacts":[]}"#,
        ],
        &loop_root,
    )
    .with_delegation_dispatcher(dispatcher.clone())
    .with_ownership_runtime(Arc::new(ScriptedOwnershipRuntime))
    .with_artifact_v2_service(artifact_service);

    if driver == "stateless" {
        magician::magician_v2::execution::agentic::run_loop::steer_inbox::activate_control_generation(
            &executors.artifact_v2_workspace,
            PRINCIPAL,
            WORKSPACE,
            &execution_id,
            "driver-differential-generation",
        )
        .await
        .expect("seed the stateless arm's durable control generation");
    }

    let mut ctx = AgenticContext::new(
        "Consume one already-finished delegated result, then report",
        "The delegated result is consumed and the run reports its terminal",
    );
    // All three are what `StatelessArm::new` needs to key a run, and it REFUSES
    // rather than falling back when any is missing. The resident arm needs none
    // of them, so setting them on both is what keeps the only difference between
    // the two runs the driver itself.
    ctx.principal = Some(PRINCIPAL.to_string());
    ctx.workspace = Some(WORKSPACE.to_string());
    ctx.task_id = Some(task_id);
    ctx.execution_id = Some(execution_id.clone());
    // Delegation refuses outright without an owner
    // ("Cannot delegate: No agent context"), so turn one needs this named. With
    // `principal` and `workspace` also set, naming it commits the run to the
    // scoped path: a full owner profile is resolved before the first decision,
    // and the run is REFUSED with `owner_profile_missing` unless the executors
    // carry an `OwnershipRuntime`. That is why `ScriptedOwnershipRuntime` exists
    // — the three are one constraint, not three choices.
    ctx.agent_id = Some(OWNER_AGENT.to_string());
    // The delegation is validated against the roster on the CONTEXT, not against
    // whatever the dispatcher would advertise, so a target missing here is
    // refused before dispatch and turn one becomes a different turn. Set here
    // AND returned by the owner profile: `apply_owner_execution_profile`
    // replaces this field wholesale with the profile's copy, so the two have to
    // agree or the roster the run decides against is not the one set here.
    ctx.delegation_targets = vec![delegation_target()];

    // Spelled out rather than `ShellState::default()`, whose `working_dir` is
    // the process's current directory. The two arms must start from byte-identical
    // state or the comparison has a second variable in it, and a default that
    // reads an ambient value is exactly that.
    let initial_state = EnvironmentState::Shell(ShellState {
        working_dir: PathBuf::from("/tmp/driver-phase-differential"),
        last_command: None,
        last_stdout: None,
        last_stderr: None,
        last_exit_code: None,
    });

    let outcome = execute_agentically(&ctx, initial_state, &executors, None).await;

    std::env::remove_var(EXECUTION_DRIVER_ENV);

    let outcome = outcome.unwrap_or_else(|error| {
        panic!(
            "the {driver} arm errored out of the harness rather than reaching a terminal: \
             {error:#}"
        )
    });

    ArmResult {
        outcome,
        spawns: dispatcher.spawns(),
        execution_id,
        loop_root,
    }
}

#[test]
fn the_two_drivers_reach_the_same_terminal_and_the_worker_commits_the_phases_it_claims() {
    std::thread::Builder::new()
        .name("driver-phase-differential".to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("driver differential runtime")
                .block_on(driver_phase_differential_inner());
        })
        .expect("spawn driver differential thread")
        .join()
        .expect("driver differential thread");
}

async fn driver_phase_differential_inner() {
    // Executor internals are otherwise invisible from an integration binary, and
    // a disagreement between two arms is exactly the failure that needs its own
    // logs to explain.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("magician=debug")),
        )
        .with_test_writer()
        .try_init();

    let temp = tempfile::tempdir().expect("differential tempdir");
    let resident_root = temp.path().join("resident");
    let worker_root = temp.path().join("worker");

    // The control arm first. If it cannot reach a terminal the comparison below
    // proves nothing about the worker, so its failure has to be attributable.
    let resident = run_once(
        ExecutionDriver::INPROCESS,
        &resident_root,
        "differential-resident",
    )
    .await;
    let worker = run_once(
        ExecutionDriver::STATELESS,
        &worker_root,
        "differential-worker",
    )
    .await;

    // ── 1. The terminal, and the work on the way to it ──────────────────────
    assert!(
        matches!(resident.outcome, AgenticOutcome::Success { .. }),
        "the CONTROL arm did not succeed, so nothing below is evidence about the worker. \
         Resident outcome: {:?}",
        resident.outcome
    );
    assert!(
        matches!(worker.outcome, AgenticOutcome::Success { .. }),
        "the worker arm did not reach the terminal the resident arm reached on the same \
         scripted decisions. Resident: {:?}. Worker: {:?}",
        resident.outcome,
        worker.outcome
    );
    // Discriminating only because the script is two turns. On a one-turn script
    // this reads `1 == 1` and every driver that terminates at all passes it; on
    // two, a driver that re-decided or re-ran a turn reports a different count,
    // and nothing else in the suite would say so. The exact-partition assert at
    // the end of this test is what pins the script to two turns.
    assert_eq!(
        resident.outcome.iterations_used(),
        worker.outcome.iterations_used(),
        "the two arms disagree about how many iterations the same script took. Resident: {:?}. \
         Worker: {:?}",
        resident.outcome,
        worker.outcome
    );
    // The second of the only two quantities both arms produce. The delegation is
    // the run's one dispatchable act; firing it twice is the failure a per-phase
    // commit makes reachable and the resident loop structurally cannot have, so
    // an inequality here names the worker.
    assert_eq!(
        resident.spawns, worker.spawns,
        "the two arms dispatched the run's one delegation a different number of times: \
         resident {}, worker {}",
        resident.spawns, worker.spawns
    );
    assert_eq!(
        worker.spawns, 1,
        "the delegated turn must have dispatched exactly once; {} says the run did not take the \
         turn this differential is built on",
        worker.spawns
    );

    // ── 2. The phase partition, from the worker's own journal ───────────────
    let store = FsLoopStateStore::new(worker.loop_root.clone());
    let key = ExecutionKey::new(PRINCIPAL, WORKSPACE, &worker.execution_id)
        .expect("the differential's execution id must be a safe path segment");
    let records = store
        .read_journal(&key, 0)
        .await
        .expect("the worker arm must leave a readable journal under its own key");
    assert!(
        !records.is_empty(),
        "the worker arm committed no journal records at {}. Either the stateless arm did not \
         run — check that {EXECUTION_DRIVER_ENV} reached it — or it rooted its store somewhere \
         other than the artifact workspace this test set",
        worker.loop_root.display()
    );

    let steps: Vec<(usize, Phase, RecordedStep)> = records
        .iter()
        .filter_map(|record| match &record.body {
            JournalBody::PhaseCompleted { step } => Some((record.iteration, record.phase, *step)),
            // Events and owner transitions are not phase completions and are
            // deliberately not part of the partition being asserted.
            _ => None,
        })
        .collect();

    // The epilogue rule, both sides, asserted BEFORE the exact partition below.
    //
    // Order is the point. The exact-partition assert subsumes these two, so
    // placing them after it would leave them unable to fire — which is precisely
    // the defect this file used to have. Placed first, each one is the assert
    // that reports the failure, and it reports it by name rather than printing
    // two eleven-element vectors and leaving the reader to diff them.
    assert!(
        steps
            .iter()
            .any(|(iteration, phase, _)| *iteration == 1 && *phase == Phase::Epilogue),
        "the epilogue did NOT run for the delegated turn. A turn that continues ends on \
         `PhaseStep::Exit`, and a boundary exit lands ON the epilogue — skipping it drops the \
         no-action counters, the stuck warning and `IterationCompleted` for every turn that is \
         not the last. Committed steps: {steps:?}"
    );
    assert!(
        !steps
            .iter()
            .any(|(iteration, phase, _)| *iteration == 3 && *phase == Phase::Epilogue),
        "the epilogue ran for the turn that ENDED the run. A terminal is a `PhaseStep::Return` \
         and leaves the cursor where it is; conflating it with an exit runs the stuck detector \
         and emits `IterationCompleted` for a finished run. Committed steps: {steps:?}"
    );

    assert_eq!(
        steps,
        vec![
            // Turn one continues, so it is a full six-phase iteration ending on
            // the epilogue — which is the one place the iteration counter steps.
            (1, Phase::Prepare, RecordedStep::Continued),
            (1, Phase::Observe, RecordedStep::Continued),
            (1, Phase::Decide, RecordedStep::Continued),
            (1, Phase::Resolve, RecordedStep::Continued),
            (
                1,
                Phase::Apply,
                RecordedStep::Exited {
                    boundary: RecordedBoundary::NextIteration,
                },
            ),
            (1, Phase::Epilogue, RecordedStep::Continued),
            // Turn two DISPATCHES, and it is the reason this script is three
            // turns rather than two. `Apply::Execute` is 59% of the iteration
            // body and reached it under NO test until this turn existed — so a
            // worker that mis-drove a dispatching turn would have been invisible
            // to every assertion in this file. It continues, so it is a full six
            // phases like turn one.
            //
            // The dispatch FAILS — no capability is registered in this harness —
            // and that is deliberate. The failure still runs the effect gate,
            // the pending row, the tool lineage and the settle, which is the
            // machinery the two arms have to agree about. A turn that SUCCEEDED
            // would need a registered pack and would then be testing that pack
            // as much as the driver.
            (2, Phase::Prepare, RecordedStep::Continued),
            (2, Phase::Observe, RecordedStep::Continued),
            (2, Phase::Decide, RecordedStep::Continued),
            (2, Phase::Resolve, RecordedStep::Continued),
            (
                2,
                Phase::Apply,
                RecordedStep::Exited {
                    boundary: RecordedBoundary::NextIteration,
                },
            ),
            (2, Phase::Epilogue, RecordedStep::Continued),
            // Turn three ends the run, so it stops at `Apply` with no epilogue.
            (3, Phase::Prepare, RecordedStep::Continued),
            (3, Phase::Observe, RecordedStep::Continued),
            (3, Phase::Decide, RecordedStep::Continued),
            (3, Phase::Resolve, RecordedStep::Continued),
            (
                3,
                Phase::Apply,
                RecordedStep::RunEnded {
                    terminal: TerminalKind::Success,
                },
            ),
        ],
        "the worker committed a different phase partition than the design claims across a turn \
         boundary: six phases for a turn that continues, five for a turn that ends the run, each \
         its own commit, in this order"
    );
}
